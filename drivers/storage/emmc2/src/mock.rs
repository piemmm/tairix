//! A register-level model of the EMMC2 controller, the SD card behind it,
//! and the board's supplies, for host tests (QEMU models no EMMC2).
//!
//! The controller decodes every register the engine drives and answers the
//! command set the engine issues. The card keeps its own state — powered,
//! signalling voltage, bus width, access mode — and, like a real one, moves
//! data only when the host's timing matches its own and never faster than
//! its mode allows, so a bring-up that mis-programs the host shows up as a
//! failed transfer or a failed assertion, not a silent pass.

extern crate alloc;

use alloc::rc::Rc;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};

use tairix_abi::driver::CompletionSignal;
use tairix_abi::DriverError;

use crate::bus::BusMode;
use crate::command::{self, BLOCK_SIZE};
use crate::trace::Trace;
use crate::{adma, regs, CardSupply, DmaArea, DmaRegion, SdhciHost, SignalVoltage, DMA_DATA_BYTES};

/// Backing-card size: 16 blocks of 512 bytes.
pub(crate) const STORE_BLOCKS: usize = 16;

/// RCA the model publishes in its `CMD3` (R6) response.
pub(crate) const TEST_RCA: u32 = 0xAAAA;

/// Device-visible bases the DMA-capable model reports for its staging.
pub(crate) const DATA_DEVICE_BASE: u64 = 0x8000_0000;
pub(crate) const TABLE_DEVICE_BASE: u64 = 0x9000_0000;

/// Bytes of the modelled descriptor-table slab: one page, as a DMA carve
/// rounds it.
pub(crate) const TABLE_SLAB_BYTES: usize = 4096;

/// The status a command the card never answers leaves: the error summary
/// plus the command-timeout error bit.
pub(crate) const CMD_TIMEOUT: u32 = regs::INT_ERROR | (1 << 16);

/// The status a data block whose CRC failed leaves.
const DATA_CRC: u32 = regs::INT_ERROR | (1 << 21);

/// `DAT[3:0]` all high in the present-state register.
const DAT_LINES_HIGH: u32 = 0xF << 20;

/// The BCM2711 EMMC2's registers, as Linux dumps them.
pub(crate) const BCM2711_VERSION: u32 = 0x1002_0000;
pub(crate) const BCM2711_CAPS: u32 = 0x45ee_6432;
pub(crate) const BCM2711_CAPS1: u32 = 0x0000_a525;
pub(crate) const BCM2711_MAX_CURRENT: u32 = 0x0008_0008;

/// An SD 3.0x card's SCR: 1- and 4-bit buses, `SD_SPEC3`, `CMD23`.
pub(crate) const SCR_UHS_CARD: [u8; 8] = [0x02, 0x35, 0x80, 0x02, 0, 0, 0, 0];

/// `CURRENT_STATE` values the model reports.
pub(crate) const STATE_STBY: u32 = 3;

/// The command index a `CMDTM` word carries.
pub(crate) fn command_index(cmdtm: u32) -> u8 {
    ((cmdtm >> regs::CMD_INDEX_SHIFT) & 0x3F) as u8
}

/// The board around the controller: the clock the engine's delays advance
/// and the card's two rails, shared with [`MockSupply`].
pub(crate) struct Wiring {
    pub(crate) now_us: Cell<u64>,
    pub(crate) card_powered: Cell<bool>,
    pub(crate) signal_1v8: Cell<bool>,
    /// Completed off-then-on cycles of the card's power.
    pub(crate) power_cycles: Cell<u32>,
    off_since: Cell<Option<u64>>,
    /// The supply refuses every switch.
    pub(crate) refuse: Cell<bool>,
    /// Every switch, with the time it happened.
    pub(crate) events: RefCell<Vec<(u64, RailEvent)>>,
}

impl Default for Wiring {
    fn default() -> Self {
        Self {
            now_us: Cell::new(0),
            card_powered: Cell::new(true),
            signal_1v8: Cell::new(false),
            power_cycles: Cell::new(0),
            off_since: Cell::new(None),
            refuse: Cell::new(false),
            events: RefCell::new(Vec::new()),
        }
    }
}

/// One switch the supply made.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum RailEvent {
    Signal(SignalVoltage),
    Power(bool),
}

/// The board's switchable supplies, over the shared [`Wiring`].
pub(crate) struct MockSupply {
    pub(crate) wiring: Rc<Wiring>,
}

impl CardSupply for MockSupply {
    fn set_signal_voltage(&mut self, voltage: SignalVoltage) -> Result<(), DriverError> {
        if self.wiring.refuse.get() {
            return Err(DriverError::DeviceFault);
        }
        self.wiring.signal_1v8.set(voltage == SignalVoltage::V1_8);
        self.record(RailEvent::Signal(voltage));
        Ok(())
    }

    fn set_card_power(&mut self, on: bool) -> Result<(), DriverError> {
        if self.wiring.refuse.get() {
            return Err(DriverError::DeviceFault);
        }
        let now = self.wiring.now_us.get();
        if on {
            if let Some(off) = self.wiring.off_since.take() {
                assert!(
                    now - off >= 1000,
                    "the card was held off {} us, under 1 ms",
                    now - off
                );
                self.wiring
                    .power_cycles
                    .set(self.wiring.power_cycles.get() + 1);
            }
        } else if self.wiring.card_powered.get() {
            self.wiring.off_since.set(Some(now));
        }
        self.wiring.card_powered.set(on);
        self.record(RailEvent::Power(on));
        Ok(())
    }
}

impl MockSupply {
    fn record(&self, event: RailEvent) {
        self.wiring
            .events
            .borrow_mut()
            .push((self.wiring.now_us.get(), event));
    }
}

/// Where a card's `CMD11` voltage switch stands.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Switch {
    /// No switch under way.
    Idle,
    /// `CMD11` answered; the card holds `DAT` low and waits for the clock to
    /// stop.
    Answered,
    /// The clock stopped at this time; the card switches when it restarts.
    ClockStopped(u64),
    /// The switch failed: the card holds `DAT` low until it loses power.
    Stuck,
}

/// The register-level controller, card and board model.
///
/// The flags model independent hardware and card conditions a test toggles
/// in isolation, so they are separate booleans rather than a state enum.
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct MockSdhci {
    // Controller registers.
    pub(crate) control0: u32,
    pub(crate) control1: u32,
    pub(crate) control2: u32,
    pub(crate) interrupt: u32,
    arg: u32,
    pub(crate) arg2: u32,
    resp: [u32; 4],
    blksizecnt: u32,
    app_cmd: bool,
    pub(crate) version: u32,
    pub(crate) caps: u32,
    pub(crate) caps1: u32,
    pub(crate) max_current: u32,
    /// The clock actually feeding the controller, which its capabilities may
    /// misreport.
    pub(crate) base_clock_hz: u32,
    pub(crate) power_on: bool,
    pub(crate) power_wired: bool,

    // The board.
    pub(crate) wiring: Rc<Wiring>,
    seen_power_cycles: u32,
    pub(crate) delays: Vec<u32>,

    // The card's identity and what it offers.
    pub(crate) c_size: u32,
    pub(crate) high_capacity: bool,
    pub(crate) csd_structure_v2: bool,
    pub(crate) if_cond_echo: bool,
    pub(crate) acmd41_ready_after: u32,
    pub(crate) acmd41_count: u32,
    /// The card offers 1.8 V signalling.
    pub(crate) uhs_card: bool,
    pub(crate) scr: [u8; 8],
    pub(crate) access_modes: u16,
    pub(crate) current_limits: u16,
    /// Access modes (by function) at which the board cannot carry data.
    pub(crate) broken_modes: u16,
    /// The card leaves its switch to 1.8 V unfinished.
    pub(crate) switch_fails: bool,

    // The card's own state.
    card_1v8: bool,
    card_4bit: bool,
    /// The card's access-mode function.
    pub(crate) card_mode: u8,
    card_current_limit: u8,
    s18a_given: bool,
    switch: Switch,
    identified: bool,
    pub(crate) acmd6_arg: Option<u32>,
    pub(crate) acmd41_args: Vec<u32>,
    /// When each `ACMD41` arrived, on the board's clock.
    pub(crate) acmd41_times: Vec<u64>,
    /// The card answers nothing until its power has been cycled once.
    pub(crate) mute_until_cycled: bool,
    pub(crate) card_state: u32,
    /// R1 error bits the next `CMD13` reports, then clears.
    pub(crate) pending_status_errors: u32,
    /// R1 error bits a data command of this index reports.
    pub(crate) r1_errors_on: Option<(u8, u32)>,

    // Backing card.
    pub(crate) store: Vec<u8>,

    // PIO transfer state.
    read_cursor: usize,
    read_end: usize,
    read_start: usize,
    register_block: Vec<u8>,
    register_cursor: usize,
    write_start: usize,
    write_cursor: usize,
    write_end: usize,

    // ADMA2 model.
    pub(crate) dma_capable: bool,
    pub(crate) dma_data: Vec<u8>,
    pub(crate) dma_table: Vec<u8>,
    pub(crate) adma_addr: u32,
    pub(crate) dma_syncs: Vec<(DmaArea, usize, usize)>,
    /// Every counted multi-block transfer, as its `ARG2` block count.
    pub(crate) counted: Vec<u32>,
    /// The engine masters memory other than the staging it was handed.
    pub(crate) dma_misdirected: bool,
    /// Every DMA transfer ends in an ADMA error.
    pub(crate) dma_fails: bool,

    // Fault injection.
    pub(crate) error_on_index: Option<u8>,
    pub(crate) stall: bool,
    pub(crate) line_resets: u32,
    pub(crate) lines_stuck: bool,
    pub(crate) stops: Vec<(u32, u32)>,
    pub(crate) stop_unanswered: bool,
    pub(crate) withheld: Rc<Cell<bool>>,
    pub(crate) ignored_aborts: usize,
    busy_until_irq: bool,
    pub(crate) commands: Vec<u32>,
    /// Every record the engine traced, in order; shared, so a test still
    /// reads it after a failed bring-up drops the host.
    pub(crate) traces: Rc<RefCell<Vec<Trace>>>,

    // Interrupt delivery.
    pub(crate) defer: bool,
    staged: u32,
    pub(crate) await_calls: u32,
    pub(crate) silent: bool,
    pub(crate) irpt_en: u32,
}

impl MockSdhci {
    /// A BCM2711 EMMC2 with a healthy high-capacity UHS-I card whose CSD
    /// reports `c_size`.
    pub(crate) fn healthy(c_size: u32) -> Self {
        Self {
            control0: 0,
            control1: 0,
            control2: 0,
            interrupt: 0,
            arg: 0,
            arg2: 0,
            resp: [0; 4],
            blksizecnt: 0,
            app_cmd: false,
            version: BCM2711_VERSION,
            caps: BCM2711_CAPS,
            caps1: BCM2711_CAPS1,
            max_current: BCM2711_MAX_CURRENT,
            base_clock_hz: 100_000_000,
            power_on: false,
            power_wired: true,
            wiring: Rc::new(Wiring::default()),
            seen_power_cycles: 0,
            delays: Vec::new(),
            c_size,
            high_capacity: true,
            csd_structure_v2: true,
            if_cond_echo: true,
            acmd41_ready_after: 2,
            acmd41_count: 0,
            uhs_card: true,
            scr: SCR_UHS_CARD,
            access_modes: 0x1F,
            current_limits: 0x0F,
            broken_modes: 0,
            switch_fails: false,
            card_1v8: false,
            card_4bit: false,
            card_mode: 0,
            card_current_limit: 0,
            s18a_given: false,
            switch: Switch::Idle,
            identified: false,
            acmd6_arg: None,
            acmd41_args: Vec::new(),
            acmd41_times: Vec::new(),
            mute_until_cycled: false,
            card_state: command::STATE_TRAN,
            pending_status_errors: 0,
            r1_errors_on: None,
            store: vec![0u8; STORE_BLOCKS * BLOCK_SIZE as usize],
            read_cursor: 0,
            read_end: 0,
            read_start: 0,
            register_block: Vec::new(),
            register_cursor: 0,
            write_start: 0,
            write_cursor: 0,
            write_end: 0,
            dma_capable: false,
            dma_data: vec![0u8; DMA_DATA_BYTES],
            dma_table: vec![0u8; TABLE_SLAB_BYTES],
            adma_addr: 0,
            dma_syncs: Vec::new(),
            counted: Vec::new(),
            dma_misdirected: false,
            dma_fails: false,
            error_on_index: None,
            stall: false,
            line_resets: 0,
            lines_stuck: false,
            stops: Vec::new(),
            stop_unanswered: false,
            withheld: Rc::new(Cell::new(false)),
            ignored_aborts: 0,
            busy_until_irq: false,
            commands: Vec::new(),
            traces: Rc::new(RefCell::new(Vec::new())),
            defer: false,
            staged: 0,
            await_calls: 0,
            silent: false,
            irpt_en: 0,
        }
    }

    /// As [`Self::healthy`], delivering every completion only through the
    /// interrupt, so the engine cannot progress without parking.
    pub(crate) fn healthy_deferred(c_size: u32) -> Self {
        Self {
            defer: true,
            ..Self::healthy(c_size)
        }
    }

    /// As [`Self::healthy`], offering DMA staging, over a card of
    /// `store_blocks` blocks.
    pub(crate) fn healthy_dma(c_size: u32, store_blocks: usize) -> Self {
        Self {
            dma_capable: true,
            store: vec![0u8; store_blocks * BLOCK_SIZE as usize],
            ..Self::healthy(c_size)
        }
    }

    /// The supply over this model's board.
    pub(crate) fn supply(&self) -> MockSupply {
        MockSupply {
            wiring: Rc::clone(&self.wiring),
        }
    }

    /// Fill block `lba` with a deterministic per-block pattern.
    pub(crate) fn fill_block(&mut self, lba: usize, seed: u8) {
        let start = lba * BLOCK_SIZE as usize;
        let mut value = seed;
        for byte in &mut self.store[start..start + BLOCK_SIZE as usize] {
            *byte = value;
            value = value.wrapping_add(1);
        }
    }

    /// Fill `count` blocks from `lba`, block `n` seeded [`Self::nth_seed`].
    pub(crate) fn fill_blocks(&mut self, lba: usize, count: usize, seed: u8) {
        for n in 0..count {
            self.fill_block(lba + n, Self::nth_seed(seed, n));
        }
    }

    /// The seed of the `n`th block of a run [`Self::fill_blocks`] seeds
    /// `seed`.
    pub(crate) fn nth_seed(seed: u8, n: usize) -> u8 {
        seed.wrapping_add(u8::try_from(n % 256).unwrap_or(0))
    }

    /// How many commands of `index` were issued.
    pub(crate) fn issued(&self, index: u8) -> usize {
        self.commands
            .iter()
            .filter(|&&word| command_index(word) == index)
            .count()
    }

    /// The block [`Self::fill_block`] writes for `seed`.
    pub(crate) fn expected_block(seed: u8) -> Vec<u8> {
        let mut value = seed;
        (0..BLOCK_SIZE as usize)
            .map(|_| {
                let byte = value;
                value = value.wrapping_add(1);
                byte
            })
            .collect()
    }

    /// The index of every command issued, in order.
    pub(crate) fn command_indices(&self) -> Vec<u8> {
        self.commands.iter().copied().map(command_index).collect()
    }

    /// The SD clock the controller currently drives, in Hz (zero stopped).
    pub(crate) fn sd_clock_hz(&self) -> u32 {
        let running = regs::CONTROL1_CLK_INTLEN | regs::CONTROL1_CLK_EN;
        if self.control1 & running != running {
            return 0;
        }
        let n = ((self.control1 >> 8) & 0xFF) | (((self.control1 >> 6) & 0b11) << 8);
        if n == 0 {
            self.base_clock_hz
        } else {
            self.base_clock_hz / (2 * n)
        }
    }

    /// The mode the card is in, as the bus sees it.
    pub(crate) fn card_bus_mode(&self) -> BusMode {
        match (self.card_1v8, self.card_mode) {
            (false, 0) => BusMode::DefaultSpeed,
            (false, _) => BusMode::HighSpeed,
            (true, 0) => BusMode::Sdr12,
            (true, 1) => BusMode::Sdr25,
            (true, 2) => BusMode::Sdr50,
            (true, _) => BusMode::Ddr50,
        }
    }

    /// Whether the card signals at 1.8 V.
    pub(crate) fn card_at_1v8(&self) -> bool {
        self.card_1v8
    }

    /// Whether the host's timing is the one the card's mode needs: its
    /// signalling, UHS mode select, High Speed output timing and bus width.
    fn host_timing_matches(&self) -> bool {
        let mode = self.card_bus_mode();
        let host_1v8 = self.control2 & regs::CONTROL2_1V8_SIGNALLING != 0;
        let select =
            (self.control2 & regs::CONTROL2_UHS_MODE_MASK) >> regs::CONTROL2_UHS_MODE_SHIFT;
        let high_speed = self.control0 & regs::CONTROL0_HIGH_SPEED != 0;
        let host_4bit = self.control0 & regs::CONTROL0_DATA_WIDTH_4BIT != 0;
        host_1v8 == mode.signals_at_1v8()
            && (!mode.signals_at_1v8() || select == mode.uhs_select())
            && high_speed == mode.high_speed_timing()
            && host_4bit == self.card_4bit
    }

    /// The fastest clock the card takes now: 400 kHz until it has an address,
    /// then its mode's.
    fn clock_ceiling(&self) -> u32 {
        if self.identified {
            self.card_bus_mode().max_clock_hz()
        } else {
            400_000
        }
    }

    /// Raise completion `bits`: at once, or at the next park when deferred.
    fn raise(&mut self, bits: u32) {
        if self.defer {
            self.staged |= bits;
        } else {
            self.interrupt |= bits;
        }
    }

    /// A card that lost its power is back in its power-on state.
    fn follow_power(&mut self) {
        let cycles = self.wiring.power_cycles.get();
        if cycles != self.seen_power_cycles {
            self.seen_power_cycles = cycles;
            self.card_1v8 = false;
            self.card_4bit = false;
            self.card_mode = 0;
            self.card_current_limit = 0;
            self.s18a_given = false;
            self.switch = Switch::Idle;
            self.identified = false;
            self.acmd41_count = 0;
        }
    }

    /// The R2 words of the model's CSD, laid out as the controller presents
    /// `RESP0..3`: the CRC-stripped field right-aligned, `CSD_STRUCTURE` at
    /// `RESP3[23:22]` and `C_SIZE` at `RESP1[29:8]`.
    fn csd_words(&self) -> [u32; 4] {
        let r3 = if self.csd_structure_v2 { 1 << 22 } else { 0 };
        let r1 = (self.c_size & 0x003F_FFFF) << 8;
        [0, r1, 0, r3]
    }

    /// The R1 a `CMD13` answers: busy only while programming, with any
    /// pending error bits, which reading clears.
    fn card_status(&mut self) -> u32 {
        let ready = if self.card_state == command::STATE_PRG {
            0
        } else {
            command::READY_FOR_DATA
        };
        let errors = core::mem::take(&mut self.pending_status_errors);
        (self.card_state << command::STATE_SHIFT) | ready | errors
    }

    /// The 64-byte `CMD6` status for `arg`, switching the card when asked.
    fn switch_status(&mut self, arg: u32) -> Vec<u8> {
        let set = arg & (1 << 31) != 0;
        let access = (arg & 0xF) as u8;
        let limit = ((arg >> 12) & 0xF) as u8;
        let access_ok = access == 0xF
            || (self.access_modes & (1 << access) != 0 && (self.card_1v8 || access <= 1));
        let limit_ok = limit == 0xF || self.current_limits & (1 << limit) != 0;
        let access_result = match access {
            0xF => self.card_mode,
            _ if access_ok => access,
            _ => 0xF,
        };
        let limit_result = match limit {
            0xF => self.card_current_limit,
            _ if limit_ok => limit,
            _ => 0xF,
        };
        if set {
            if access != 0xF && access_ok {
                self.card_mode = access;
            }
            if limit != 0xF && limit_ok {
                self.card_current_limit = limit;
            }
        }
        let mut status = vec![0u8; 64];
        status[6..8].copy_from_slice(&self.current_limits.to_be_bytes());
        status[12..14].copy_from_slice(&self.access_modes.to_be_bytes());
        status[15] = limit_result << 4;
        status[16] = access_result;
        status
    }

    /// Queue `bytes` as one register-sized data block at the data port.
    fn send_register_block(&mut self, bytes: Vec<u8>) {
        assert_eq!(
            (self.blksizecnt & 0xFFFF) as usize,
            bytes.len(),
            "block size programmed for the register read"
        );
        self.register_block = bytes;
        self.register_cursor = 0;
        self.raise(regs::INT_READ_RDY);
    }

    /// Whether a data transfer at the current timing arrives intact.
    fn data_path_intact(&self) -> bool {
        self.host_timing_matches()
            && self.broken_modes & (1 << self.card_bus_mode().function_index()) == 0
    }

    /// Walk the ADMA2 table the engine programmed and move its bytes between
    /// the backing store at block `arg` and the data staging.
    fn process_dma_transfer(&mut self, is_read: bool) {
        assert_eq!(
            self.control0 & regs::CONTROL0_DMA_SELECT_MASK,
            regs::CONTROL0_DMA_SELECT_ADMA2,
            "a DMA command requires ADMA2 selected"
        );
        if self.dma_fails {
            self.raise(regs::INT_ERROR | (1 << 25));
            return;
        }
        let blocks = ((self.blksizecnt >> 16) & 0xFFFF) as usize;
        let expected = blocks * BLOCK_SIZE as usize;
        let mut desc_off =
            usize::try_from(u64::from(self.adma_addr) - TABLE_DEVICE_BASE).expect("table offset");
        let mut card_off = self.arg as usize * BLOCK_SIZE as usize;
        let mut moved = 0;
        loop {
            let desc = &self.dma_table[desc_off..desc_off + adma::DESC_BYTES];
            let attr = u16::from_le_bytes([desc[0], desc[1]]);
            assert_ne!(attr & 0x1, 0, "descriptor Valid");
            assert_eq!(attr & (0b11 << 4), 0b10 << 4, "descriptor Act = Tran");
            let len = match u16::from_le_bytes([desc[2], desc[3]]) {
                0 => adma::MAX_DESC_BYTES,
                n => usize::from(n),
            };
            let addr = u32::from_le_bytes([desc[4], desc[5], desc[6], desc[7]]);
            let data_off =
                usize::try_from(u64::from(addr) - DATA_DEVICE_BASE).expect("data offset");
            assert!(
                data_off + len <= self.dma_data.len(),
                "descriptor inside the staging"
            );
            if self.dma_misdirected {
                // The bytes go to, or come from, memory nobody reads.
            } else if is_read {
                self.dma_data[data_off..data_off + len]
                    .copy_from_slice(&self.store[card_off..card_off + len]);
            } else {
                self.store[card_off..card_off + len]
                    .copy_from_slice(&self.dma_data[data_off..data_off + len]);
            }
            moved += len;
            card_off += len;
            desc_off += adma::DESC_BYTES;
            if attr & 0x2 != 0 {
                break;
            }
        }
        assert_eq!(
            moved, expected,
            "the table moves the programmed block count"
        );
        self.raise(regs::INT_DATA_DONE);
    }

    /// Check an Auto-`CMD23` transfer announced its own length.
    fn note_auto_command(&mut self, cmdtm: u32) {
        if cmdtm & (0b11 << 2) == regs::TM_AUTO_CMD23 {
            let blocks = (self.blksizecnt >> 16) & 0xFFFF;
            assert_eq!(self.arg2, blocks, "ARG2 carries the block count");
            assert!(
                self.scr[3] & 0x02 != 0,
                "CMD23 only for a card that takes it"
            );
            self.counted.push(blocks);
        }
    }

    fn next_data_word(&mut self) -> u32 {
        if self.register_cursor < self.register_block.len() {
            let at = self.register_cursor;
            let word = u32::from_le_bytes([
                self.register_block[at],
                self.register_block[at + 1],
                self.register_block[at + 2],
                self.register_block[at + 3],
            ]);
            self.register_cursor += 4;
            if self.register_cursor == self.register_block.len() {
                self.raise(regs::INT_DATA_DONE);
            }
            return word;
        }
        let off = self.read_cursor;
        let value = u32::from_le_bytes([
            self.store[off],
            self.store[off + 1],
            self.store[off + 2],
            self.store[off + 3],
        ]);
        self.read_cursor += 4;
        if (self.read_cursor - self.read_start).is_multiple_of(BLOCK_SIZE as usize) {
            if self.read_cursor < self.read_end {
                self.raise(regs::INT_READ_RDY);
            } else {
                self.raise(regs::INT_DATA_DONE);
            }
        }
        value
    }

    fn accept_data_word(&mut self, value: u32) {
        if self.write_cursor >= self.write_end {
            return;
        }
        let off = self.write_cursor;
        self.store[off..off + 4].copy_from_slice(&value.to_le_bytes());
        self.write_cursor += 4;
        if (self.write_cursor - self.write_start).is_multiple_of(BLOCK_SIZE as usize) {
            if self.write_cursor < self.write_end {
                self.raise(regs::INT_WRITE_RDY);
            } else {
                self.raise(regs::INT_DATA_DONE);
            }
        }
    }

    /// Advance the voltage switch as the SD clock stops or starts.
    fn clock_changed(&mut self, running_before: bool) {
        let running = self.sd_clock_hz() != 0;
        match (self.switch, running_before, running) {
            (Switch::Answered, true, false) => {
                self.switch = Switch::ClockStopped(self.wiring.now_us.get());
            }
            (Switch::ClockStopped(at), false, true) => {
                let gated = self.wiring.now_us.get() - at;
                let host_1v8 = self.control2 & regs::CONTROL2_1V8_SIGNALLING != 0;
                if !self.switch_fails && host_1v8 && self.wiring.signal_1v8.get() && gated >= 5000 {
                    self.card_1v8 = true;
                    self.switch = Switch::Idle;
                } else {
                    self.switch = Switch::Stuck;
                }
            }
            _ => {}
        }
    }

    fn process_command(&mut self, cmdtm: u32) {
        self.commands.push(cmdtm);
        self.follow_power();
        let index = command_index(cmdtm);

        if !self.power_on || !self.wiring.card_powered.get() || self.stall {
            // A dark bus or a wedged controller never completes a command.
            return;
        }
        if self.mute_until_cycled && self.wiring.power_cycles.get() == 0 && index != 0 {
            self.raise(CMD_TIMEOUT);
            return;
        }
        if matches!(
            self.switch,
            Switch::Answered | Switch::ClockStopped(_) | Switch::Stuck
        ) {
            // Mid-switch or stuck, the card drives CMD low and answers nothing.
            self.raise(CMD_TIMEOUT);
            return;
        }
        let clock = self.sd_clock_hz();
        assert!(clock > 0, "CMD{index} issued with the SD clock stopped");
        assert!(
            clock <= self.clock_ceiling(),
            "CMD{index} at {clock} Hz, over the card's {} Hz",
            self.clock_ceiling()
        );
        if self.error_on_index == Some(index) {
            self.raise(CMD_TIMEOUT);
            return;
        }

        let was_app = self.app_cmd;
        self.app_cmd = false;
        let data = cmdtm & regs::CMD_IS_DATA != 0;
        if data && !self.data_path_intact() {
            self.resp[0] = 0;
            self.raise(regs::INT_CMD_DONE | DATA_CRC);
            return;
        }

        let answered = match index {
            12 | 13 => self.process_stop_or_status(index, cmdtm),
            17 | 18 | 24 | 25 => {
                self.process_block_command(index, cmdtm);
                true
            }
            _ => self.process_card_command(index, was_app),
        };
        if answered {
            self.raise(regs::INT_CMD_DONE);
        } else {
            self.raise(CMD_TIMEOUT);
        }
    }

    /// Answer an identification, negotiation or selection command; `false`
    /// when the card leaves it unanswered.
    fn process_card_command(&mut self, index: u8, was_app: bool) -> bool {
        match index {
            0 => {
                self.identified = false;
                self.card_4bit = false;
                self.card_mode = 0;
                self.acmd41_count = 0;
            }
            8 => self.resp[0] = if self.if_cond_echo { self.arg } else { 0 },
            55 => {
                self.app_cmd = true;
                self.resp[0] = 0;
            }
            6 if was_app => {
                self.acmd6_arg = Some(self.arg);
                self.card_4bit = self.arg == command::BUS_WIDTH_4BIT_ARG;
                self.resp[0] = 0;
            }
            6 => {
                let status = self.switch_status(self.arg);
                self.resp[0] = 0;
                self.send_register_block(status);
            }
            11 if !self.s18a_given => return false,
            11 => {
                self.resp[0] = 0;
                self.switch = Switch::Answered;
            }
            41 if was_app => self.resp[0] = self.op_cond(),
            51 if was_app => {
                self.resp[0] = 0;
                let scr = self.scr.to_vec();
                self.send_register_block(scr);
            }
            2 => self.resp = [0x0102_0304, 0, 0, 0],
            3 => {
                self.resp[0] = TEST_RCA << 16;
                self.identified = true;
            }
            9 => self.resp = self.csd_words(),
            7 => {
                self.resp[0] = 0;
                self.card_state = command::STATE_TRAN;
                self.raise(regs::INT_DATA_DONE);
            }
            16 => self.resp[0] = 0,
            _ => {}
        }
        true
    }

    /// The OCR one `ACMD41` round answers: ready after the configured
    /// rounds, with S18A for a UHS card asked for 1.8 V it is not already at.
    fn op_cond(&mut self) -> u32 {
        self.acmd41_count += 1;
        self.acmd41_args.push(self.arg);
        self.acmd41_times.push(self.wiring.now_us.get());
        let mut ocr = 0x00FF_8000;
        if self.acmd41_count >= self.acmd41_ready_after {
            ocr |= command::OCR_READY;
            if self.high_capacity {
                ocr |= command::OCR_CCS;
            }
            if self.uhs_card && !self.card_1v8 && self.arg & command::OCR_S18 != 0 {
                ocr |= command::OCR_S18;
                self.s18a_given = true;
            }
        }
        ocr
    }

    /// Answer `CMD12` or `CMD13`; `false` when the card leaves it unanswered.
    fn process_stop_or_status(&mut self, index: u8, cmdtm: u32) -> bool {
        if index == 12 {
            self.stops.push((cmdtm, self.line_resets));
            if self.stop_unanswered {
                return false;
            }
            self.resp[0] = 0;
            if self.ignored_aborts > 0 {
                self.ignored_aborts -= 1;
            } else if matches!(self.card_state, command::STATE_DATA | command::STATE_RCV) {
                self.card_state = command::STATE_TRAN;
            }
            self.raise(regs::INT_DATA_DONE);
            return true;
        }
        // A card answers only a status request addressed to its RCA.
        if self.arg != TEST_RCA << 16 {
            return false;
        }
        self.resp[0] = self.card_status();
        if (cmdtm >> regs::CMD_RESP_TYPE_SHIFT) & 0b11 == regs::RESP_48_BUSY {
            if self.card_state == command::STATE_PRG {
                self.busy_until_irq = true;
            } else {
                self.raise(regs::INT_DATA_DONE);
            }
        }
        true
    }

    fn process_block_command(&mut self, index: u8, cmdtm: u32) {
        self.resp[0] = match self.r1_errors_on {
            Some((failing, bits)) if failing == index => bits,
            _ => 0,
        };
        self.note_auto_command(cmdtm);
        self.register_block.clear();
        let read = matches!(index, 17 | 18);
        if cmdtm & regs::TM_DMA_EN != 0 {
            self.process_dma_transfer(read);
            return;
        }
        let block_count = ((self.blksizecnt >> 16) & 0xFFFF) as usize;
        let start = self.arg as usize * BLOCK_SIZE as usize;
        let end = start + block_count * BLOCK_SIZE as usize;
        if read {
            (self.read_start, self.read_cursor, self.read_end) = (start, start, end);
            self.raise(regs::INT_READ_RDY);
        } else {
            (self.write_start, self.write_cursor, self.write_end) = (start, start, end);
            self.raise(regs::INT_WRITE_RDY);
        }
    }
}

impl SdhciHost for MockSdhci {
    fn read32(&mut self, offset: usize) -> Result<u32, DriverError> {
        let value = match offset {
            regs::REG_CONTROL0 => self.control0,
            regs::REG_CONTROL1 => {
                if self.control1 & regs::CONTROL1_CLK_INTLEN != 0 {
                    self.control1 | regs::CONTROL1_CLK_STABLE
                } else {
                    self.control1
                }
            }
            regs::REG_CONTROL2 => self.control2,
            regs::REG_INTERRUPT => self.interrupt,
            regs::REG_RESP0 => self.resp[0],
            regs::REG_RESP1 => self.resp[1],
            regs::REG_RESP2 => self.resp[2],
            regs::REG_RESP3 => self.resp[3],
            regs::REG_DATA => self.next_data_word(),
            regs::REG_STATUS => {
                let held_low = matches!(
                    self.switch,
                    Switch::Answered | Switch::ClockStopped(_) | Switch::Stuck
                );
                if held_low {
                    0
                } else {
                    DAT_LINES_HIGH
                }
            }
            regs::REG_CAPABILITIES => self.caps,
            regs::REG_CAPABILITIES_1 => self.caps1,
            regs::REG_MAX_CURRENT => self.max_current,
            regs::REG_SLOTISR_VER => self.version,
            _ => 0,
        };
        Ok(value)
    }

    fn write32(&mut self, offset: usize, value: u32) -> Result<(), DriverError> {
        match offset {
            regs::REG_CONTROL1 => {
                let running = self.sd_clock_hz() != 0;
                let lines = regs::CONTROL1_SRST_CMD | regs::CONTROL1_SRST_DATA;
                if value & lines != 0 {
                    self.line_resets += 1;
                    self.interrupt = 0;
                }
                if value & regs::CONTROL1_SRST_HC != 0 {
                    // The full reset clears every host register, bus power
                    // and the UHS settings included.
                    self.control0 = 0;
                    self.control2 = 0;
                    self.power_on = false;
                    self.interrupt = 0;
                }
                let settled = if self.lines_stuck {
                    regs::CONTROL1_SRST_HC
                } else {
                    regs::CONTROL1_SRST_HC | lines
                };
                self.control1 = value & !settled;
                self.clock_changed(running);
            }
            regs::REG_CONTROL0 => {
                self.control0 = if self.power_wired {
                    value
                } else {
                    value & !regs::CONTROL0_BUS_POWER
                };
                self.power_on = self.control0 & regs::CONTROL0_BUS_POWER != 0;
            }
            regs::REG_CONTROL2 => self.control2 = value & 0xFFFF_0000,
            regs::REG_INTERRUPT => self.interrupt &= !value,
            regs::REG_ARG1 => self.arg = value,
            regs::REG_ARG2 => self.arg2 = value,
            regs::REG_BLKSIZECNT => self.blksizecnt = value,
            regs::REG_ADMA_ADDR => self.adma_addr = value,
            regs::REG_CMDTM => self.process_command(value),
            regs::REG_DATA => self.accept_data_word(value),
            regs::REG_IRPT_EN => self.irpt_en = value,
            _ => {}
        }
        Ok(())
    }

    fn await_irq(&mut self) -> CompletionSignal {
        self.await_calls += 1;
        if self.silent {
            return CompletionSignal::TimedOut;
        }
        if self.busy_until_irq {
            self.busy_until_irq = false;
            self.card_state = command::STATE_TRAN;
            self.raise(regs::INT_DATA_DONE);
        }
        self.interrupt |= self.staged;
        self.staged = 0;
        CompletionSignal::Fired
    }

    fn delay_us(&mut self, us: u32) {
        self.delays.push(us);
        self.wiring
            .now_us
            .set(self.wiring.now_us.get() + u64::from(us));
    }

    fn dma_region(&mut self) -> Option<DmaRegion<'_>> {
        if !self.dma_capable {
            return None;
        }
        Some(DmaRegion {
            data: &mut self.dma_data,
            data_device: DATA_DEVICE_BASE,
            table: &mut self.dma_table,
            table_device: TABLE_DEVICE_BASE,
        })
    }

    fn sync_dma(&mut self, area: DmaArea, offset: usize, len: usize) {
        self.dma_syncs.push((area, offset, len));
    }

    fn withhold_dma(&mut self) {
        self.withheld.set(true);
    }

    fn trace(&mut self, record: Trace) {
        self.traces.borrow_mut().push(record);
    }
}

impl BusMode {
    /// The access-mode function, as a bit index into a support mask.
    fn function_index(self) -> u32 {
        u32::from(self.function())
    }
}
