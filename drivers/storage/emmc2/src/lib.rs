//! TAIRiX Raspberry Pi 4 (BCM2711) EMMC2 SD-host block driver.
//!
//! The Pi 4's EMMC2 controller is an Arasan / SDHCI 3.00 SD host. This
//! driver brings an SD card up over the standard SDHCI register block at the
//! fastest bus timing the controller, the card and the board's supplies
//! allow, and exposes it through [`tairix_abi::driver::block::Block`].
//!
//! # Bus speed
//!
//! Bring-up ([`Emmc2::open`]) reads the controller's capabilities, divides
//! every SD clock from the base clock actually feeding it, and negotiates
//! down a ladder: UHS-I at 1.8 V signalling — DDR50 on the BCM2711, 50 MB/s —
//! when the [`Board`] can switch the card's I/O rail and power-cycle it, then
//! High Speed at 3.3 V (25 MB/s), then Default Speed. Each rung is verified
//! with a read at its own timing, and a failure steps down, power-cycling the
//! card when it had left 3.3 V. [`Emmc2::link`] reports where it landed.
//!
//! # Transfers
//!
//! ADMA2 through a staging area the host grants ([`SdhciHost::dma_region`]),
//! [`DMA_STAGE_BLOCKS`] per command, with a descriptor table in a separate
//! device-visible area; the buffer data port otherwise. A multi-block
//! command announces its length with `CMD23` when the card takes it and is
//! stopped by `CMD12` when it does not, and every write is followed by the
//! card's status, which is where it reports a write it could not program.
//!
//! # Layered seam
//!
//! The state machine is written against [`SdhciHost`], not a memory mapping:
//! metal drives it over a capability-gated [`RegisterWindow`] ([`IrqSdhci`]),
//! host tests over a register-level mock controller and card.
//!
//! # Capabilities
//!
//! Loading requires [`CapabilityId::DRV_LOAD`]; mapping the discovered
//! register window additionally requires [`CapabilityId::MMIO_MAP`]
//! (checked in [`wiring`]).

#![no_std]
#![forbid(unsafe_op_in_unsafe_fn)]
#![deny(missing_docs)]

use tairix_abi::blkio::{BlkDeviceClass, BlkDeviceName};
use tairix_abi::driver::block::{Block, BlockGeometry};
use tairix_abi::driver::dma::DmaSlab;
use tairix_abi::driver::timing::Delay;
use tairix_abi::driver::{BufferClass, CompletionSignal};
use tairix_abi::{
    CapabilityId, DriverBindKey, DriverError, DriverHandle, DriverHost, HwMatchKey, RegisterBlock,
    RegisterWindow,
};
use tairix_dma_barrier::{dma_rmb, dma_wmb};

pub mod adma;
mod bringup;
pub mod bus;
pub mod card;
pub mod command;
pub mod host;
pub mod regs;
pub mod trace;
pub mod wiring;

#[cfg(test)]
mod bringup_tests;
#[cfg(test)]
mod mock;
#[cfg(test)]
mod tests;

pub use bus::{BusMode, Link};

use command::{CardCondition, ResponseKind, SdCommand, BLOCK_SIZE};
use trace::{Trace, WaitEnd};

/// Per-driver `DriverHandle` marker returned by [`register`].
const REGISTER_HANDLE_MARKER: u64 = 0x5344_5000_0000_0001;

/// The bind priority [`BIND_KEYS`] carries: an exact `compatible` match.
const BIND_PRIORITY: u16 = 10;

/// This driver's hardware bind table: the BCM2711 EMMC2 SD host, matched by
/// the device-tree `compatible` string `brcm,bcm2711-emmc2`. The single
/// source the signed manifest's bind table is authored from.
pub const BIND_KEYS: &[DriverBindKey] = &[DriverBindKey::new(
    BIND_PRIORITY,
    match HwMatchKey::compatible(b"brcm,bcm2711-emmc2") {
        Ok(key) => key,
        // A too-long literal is a compile-time const-eval error here, never a
        // runtime panic.
        Err(_) => panic!("compatible string fits HW_COMPATIBLE_MAX"),
    },
)];

/// Upper bound on register polls, and on completion parks, while waiting
/// for a controller event: a defence against an unresponsive controller,
/// orders of magnitude past any honest completion, not a capacity.
pub const DEFAULT_POLL_BUDGET: u32 = 1_000_000;

/// Largest number of blocks one command may carry: the 16-bit block-count
/// field. A format-fixed bound; a longer transfer is split.
const MAX_BLOCKS_PER_COMMAND: usize = 0xFFFF;

/// Rounds of `SEND_STATUS` and its remedy a card of unknown state is given to
/// reach `tran`; a card still out of `tran` after three is faulty. A defence
/// bound, not a capacity.
const CARD_STATE_ROUNDS: usize = 3;

/// Blocks the ADMA2 path moves per command: 256 KiB, enough to keep a
/// command's fixed latency to a few percent of its time on a 50 MB/s bus. It
/// sizes the staging window, not a transfer: a longer one is split.
pub const DMA_STAGE_BLOCKS: usize = 512;

/// Bytes of the ADMA2 data-staging area the host is asked for.
pub const DMA_DATA_BYTES: usize = DMA_STAGE_BLOCKS * BLOCK_SIZE as usize;

/// Bytes of the ADMA2 descriptor table the host is asked for: enough
/// descriptors for one full staging window.
pub const DMA_TABLE_BYTES: usize = adma::descriptors_for(DMA_DATA_BYTES) * adma::DESC_BYTES;

/// Driver entry point.
///
/// # Errors
///
/// * [`DriverError::PermissionDenied`] if the host did not grant
///   [`CapabilityId::DRV_LOAD`].
///
/// # Capabilities
///
/// Requires [`CapabilityId::DRV_LOAD`].
pub fn register(host: &dyn DriverHost) -> Result<DriverHandle, DriverError> {
    if !host.has_capability(CapabilityId::DRV_LOAD) {
        return Err(DriverError::PermissionDenied);
    }
    DriverHandle::from_raw(REGISTER_HANDLE_MARKER)
}

/// The SDHCI host seam every controller access goes through: the register
/// block, and what the engine needs around it.
pub trait SdhciHost: RegisterBlock {
    /// Park until the controller raises its interrupt line or the wait's
    /// bounded budget elapses; the engine then re-reads `INTERRUPT`.
    ///
    /// [`CompletionSignal::TimedOut`] reports a wait that elapsed with no
    /// interrupt at all, which the engine fails closed on.
    fn await_irq(&mut self) -> CompletionSignal;

    /// Block for at least `us` microseconds, off the CPU: the intervals the
    /// SD specification makes the host wait (supply ramps, a signalling
    /// switch, the pace of power-up polling).
    fn delay_us(&mut self, us: u32);

    /// The device-shared DMA staging this host provides, or `None` for a
    /// programmed-I/O-only host.
    ///
    /// Re-borrowed per call so it never aliases a register access: the
    /// engine stages into it, drops the borrow, then programs the controller.
    fn dma_region(&mut self) -> Option<DmaRegion<'_>> {
        None
    }

    /// Synchronize `len` bytes at `offset` of one DMA staging `area` between
    /// the CPU caches and the controller: after publishing bytes it will
    /// read, and before consuming bytes it wrote. A coherent host keeps this
    /// no-op.
    fn sync_dma(&mut self, _area: DmaArea, _offset: usize, _len: usize) {}

    /// Never return the DMA staging to its pool: the controller may still be
    /// mastering it.
    fn withhold_dma(&mut self) {}

    /// Report a step of the engine's work as a [`Trace`] record; a host that
    /// records nothing keeps this no-op.
    fn trace(&mut self, _record: Trace) {}
}

/// A borrowed view of a host's device-shared DMA staging: the data area a
/// transfer moves through and the table of ADMA2 descriptors describing it,
/// each with the device address of its first byte.
pub struct DmaRegion<'a> {
    /// CPU-accessible data-staging bytes.
    pub data: &'a mut [u8],
    /// Device-visible address of `data[0]`.
    pub data_device: u64,
    /// CPU-accessible descriptor-table bytes.
    pub table: &'a mut [u8],
    /// Device-visible address of `table[0]`.
    pub table_device: u64,
}

/// One area of the DMA staging, for [`SdhciHost::sync_dma`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DmaArea {
    /// The data-staging area.
    Data,
    /// The ADMA2 descriptor table.
    Table,
}

/// The environment the metal [`IrqSdhci`] host runs the engine in: the
/// completion park, the timed wait, and where the engine's trace goes.
///
/// The driver is generic over `lib/abi` only, so it cannot name the kernel's
/// IRQ-wait machinery; the kernel supplies an implementation that blocks the
/// calling task on the controller's bound interrupt line, and sleeps it for
/// the [`Delay`] intervals, while a host test supplies inline ones. The
/// outcome is the driver ABI's [`CompletionSignal`].
pub trait CompletionWait: Delay {
    /// Block until the controller signals on its interrupt line or the
    /// implementation's bounded budget elapses; the caller re-reads
    /// `INTERRUPT` on a fire and fails closed on a timeout.
    fn await_irq(&self) -> CompletionSignal;

    /// Record a step of the engine's work as a [`Trace`] record; an
    /// environment that records nothing keeps this no-op.
    fn trace(&self, _record: Trace) {}
}

/// The signalling voltage of the card's I/O rail.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SignalVoltage {
    /// 3.3 V, every card's power-on signalling.
    V3_3,
    /// 1.8 V, the UHS-I signalling.
    V1_8,
}

impl SignalVoltage {
    /// The rail's voltage in microvolts, as a regulator's states name it.
    #[must_use]
    pub const fn microvolts(self) -> u32 {
        match self {
            Self::V3_3 => 3_300_000,
            Self::V1_8 => 1_800_000,
        }
    }
}

/// The card's switchable supplies, as the board wires them.
///
/// Each switch returns once its rail has settled. A board offers both or
/// neither: switching the card to 1.8 V is only safe where a failed switch
/// can be undone, and only a power cycle undoes it.
pub trait CardSupply {
    /// Switch the card's I/O rail to `voltage`.
    ///
    /// # Errors
    ///
    /// Any [`DriverError`] the board's switch reports; the rail's voltage is
    /// then unknown.
    fn set_signal_voltage(&mut self, voltage: SignalVoltage) -> Result<(), DriverError>;

    /// Switch the card's power rail.
    ///
    /// # Errors
    ///
    /// Any [`DriverError`] the board's switch reports.
    fn set_card_power(&mut self, on: bool) -> Result<(), DriverError>;
}

/// The board wiring a bring-up borrows, for the bring-up only.
#[derive(Default)]
pub struct Board<'a> {
    /// The controller's base clock as the platform programmed it, in Hz;
    /// `None` leaves the capabilities register's figure to be used.
    pub base_clock_hz: Option<u32>,
    /// The card's switchable supplies, where the board has them.
    pub supply: Option<&'a mut dyn CardSupply>,
}

/// The metal SDHCI host: the capability-gated [`RegisterWindow`], a
/// [`CompletionWait`] that parks on the controller's interrupt line, and —
/// on the fast path — the two DMA staging slabs.
pub struct IrqSdhci<W: CompletionWait> {
    window: RegisterWindow,
    waiter: W,
    dma: Option<Staging>,
}

/// The data and descriptor-table slabs of the ADMA2 staging.
struct Staging {
    data: DmaSlab,
    table: DmaSlab,
}

impl<W: CompletionWait> IrqSdhci<W> {
    /// Pair a mapped register `window` with the completion `waiter`, on the
    /// programmed-I/O path.
    #[must_use]
    pub fn new(window: RegisterWindow, waiter: W) -> Self {
        Self {
            window,
            waiter,
            dma: None,
        }
    }

    /// As [`Self::new`], with the ADMA2 staging: a `data` slab of at least a
    /// block and a `table` slab of descriptors for it. On a non-coherent
    /// interconnect each must be uncached or carry the cache maintenance
    /// [`DmaSlab::sync_range`] invokes.
    #[must_use]
    pub fn with_dma(window: RegisterWindow, waiter: W, data: DmaSlab, table: DmaSlab) -> Self {
        Self {
            window,
            waiter,
            dma: Some(Staging { data, table }),
        }
    }
}

impl<W: CompletionWait> RegisterBlock for IrqSdhci<W> {
    fn read32(&self, offset: usize) -> Result<u32, DriverError> {
        self.window.read32(offset)
    }

    fn write32(&self, offset: usize, value: u32) -> Result<(), DriverError> {
        self.window.write32(offset, value)
    }

    fn block_len(&self) -> usize {
        self.window.block_len()
    }
}

impl<W: CompletionWait> SdhciHost for IrqSdhci<W> {
    fn await_irq(&mut self) -> CompletionSignal {
        self.waiter.await_irq()
    }

    fn delay_us(&mut self, us: u32) {
        self.waiter.delay_us(us);
    }

    fn dma_region(&mut self) -> Option<DmaRegion<'_>> {
        let staging = self.dma.as_mut()?;
        let data_device = staging.data.phys();
        let table_device = staging.table.phys();
        Some(DmaRegion {
            data: staging.data.as_bytes_mut(),
            data_device,
            table: staging.table.as_bytes_mut(),
            table_device,
        })
    }

    fn sync_dma(&mut self, area: DmaArea, offset: usize, len: usize) {
        if let Some(staging) = self.dma.as_ref() {
            match area {
                DmaArea::Data => staging.data.sync_range(offset, len),
                DmaArea::Table => staging.table.sync_range(offset, len),
            }
        }
    }

    fn withhold_dma(&mut self) {
        if let Some(staging) = self.dma.as_mut() {
            staging.data.withhold();
            staging.table.withhold();
        }
    }

    fn trace(&mut self, record: Trace) {
        self.waiter.trace(record);
    }
}

/// The step of the bring-up that failed.
///
/// Carried by [`BringUpFault`] so a metal caller can log which step the
/// controller or card stalled at: QEMU models no EMMC2, so on a real Pi 4
/// this is the signal that localises a failure (`plans/PI.md` P8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BringUpStage {
    /// Mapping the discovered register window (the [`wiring`] pre-step).
    MapWindow,
    /// Controller reset, capabilities, and the identification clock.
    ResetClock,
    /// Selecting 3.3 V signalling on the board's supply before the first
    /// command.
    InitialSignalling,
    /// `CMD0` `GO_IDLE_STATE`.
    GoIdle,
    /// `CMD8` `SEND_IF_COND`.
    SendIfCond,
    /// `ACMD41` `SD_SEND_OP_COND` power-up polling.
    OpCond,
    /// `CMD11` `VOLTAGE_SWITCH` and the switch to 1.8 V signalling.
    VoltageSwitch,
    /// `CMD2` `ALL_SEND_CID`.
    AllSendCid,
    /// `CMD3` `SEND_RELATIVE_ADDR`.
    SendRelativeAddr,
    /// `CMD9` `SEND_CSD` and the geometry derived from it.
    SendCsd,
    /// `CMD7` `SELECT_CARD`.
    SelectCard,
    /// `CMD16` `SET_BLOCKLEN`.
    SetBlockLen,
    /// Raising the SD clock from identification to the 25 MHz every card
    /// takes once selected.
    RaiseClock,
    /// `ACMD51` `SEND_SCR`.
    SendScr,
    /// `ACMD6` `SET_BUS_WIDTH` and the controller's 4-bit width.
    SetBusWidth,
    /// `CMD6` `SWITCH_FUNC`: the card's current limit or access mode.
    SwitchFunction,
    /// The controller's timing and clock for the negotiated mode.
    SetBusTiming,
    /// The read that proves the negotiated mode moves data.
    VerifyBus,
    /// Selecting ADMA2.
    SelectDma,
    /// The read that proves ADMA2 lands data where the CPU reads it.
    VerifyDma,
    /// Cycling the card's power to bring it back to 3.3 V.
    PowerCycle,
}

impl BringUpStage {
    /// A stable, terse name for the stage: the `stage=` field of the metal
    /// log line, so treat it as part of the operator-facing contract.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            BringUpStage::MapWindow => "map register window",
            BringUpStage::ResetClock => "reset + SD clock",
            BringUpStage::InitialSignalling => "select 3.3 V signalling",
            BringUpStage::GoIdle => "CMD0 GO_IDLE_STATE",
            BringUpStage::SendIfCond => "CMD8 SEND_IF_COND",
            BringUpStage::OpCond => "ACMD41 SD_SEND_OP_COND",
            BringUpStage::VoltageSwitch => "CMD11 VOLTAGE_SWITCH",
            BringUpStage::AllSendCid => "CMD2 ALL_SEND_CID",
            BringUpStage::SendRelativeAddr => "CMD3 SEND_RELATIVE_ADDR",
            BringUpStage::SendCsd => "CMD9 SEND_CSD",
            BringUpStage::SelectCard => "CMD7 SELECT_CARD",
            BringUpStage::SetBlockLen => "CMD16 SET_BLOCKLEN",
            BringUpStage::RaiseClock => "raise SD clock to 25 MHz",
            BringUpStage::SendScr => "ACMD51 SEND_SCR",
            BringUpStage::SetBusWidth => "ACMD6 SET_BUS_WIDTH",
            BringUpStage::SwitchFunction => "CMD6 SWITCH_FUNC",
            BringUpStage::SetBusTiming => "set bus timing",
            BringUpStage::VerifyBus => "verify read at bus timing",
            BringUpStage::SelectDma => "select ADMA2",
            BringUpStage::VerifyDma => "verify ADMA2 read",
            BringUpStage::PowerCycle => "power-cycle card",
        }
    }
}

/// A bring-up failure: the [`BringUpStage`] reached and the underlying
/// [`DriverError`]. Convert with `DriverError::from` / `?` where only the
/// error matters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BringUpFault {
    /// The step the bring-up reached before failing.
    pub stage: BringUpStage,
    /// The underlying driver error at that step.
    pub error: DriverError,
}

impl BringUpFault {
    /// Pair `stage` with the `error` that ended the step.
    #[must_use]
    const fn new(stage: BringUpStage, error: DriverError) -> Self {
        Self { stage, error }
    }
}

impl From<BringUpFault> for DriverError {
    fn from(fault: BringUpFault) -> Self {
        fault.error
    }
}

/// An SD card brought up over the SDHCI register seam.
///
/// `H` is the register backing: a capability-gated [`RegisterWindow`] on
/// metal, a register-level mock in host tests.
pub struct Emmc2<H: SdhciHost> {
    host: H,
    geometry: BlockGeometry,
    /// The card's Relative Card Address, in bits `[31:16]` as every addressed
    /// command takes it; zero until `CMD3` publishes it.
    rca: u32,
    poll_budget: u32,
    link: Link,
    /// Blocks the ADMA2 path stages per command, or `0` on programmed I/O.
    dma_stage_blocks: usize,
    /// A failed transfer's line reset never confirmed, so the controller may
    /// still be mastering the staging: no transfer may reuse it.
    dma_wedged: bool,
    /// A failed transfer's recovery could not prove the card back in `tran`,
    /// so the next data command asks it first.
    card_state_unknown: bool,
}

impl<H: SdhciHost> Emmc2<H> {
    /// Bring the card up over `host`, borrowing `board` for the bring-up.
    ///
    /// # Errors
    ///
    /// A [`BringUpFault`] naming the [`BringUpStage`] that failed at the
    /// slowest bus timing tried:
    ///
    /// * [`DriverError::Unsupported`] — a card that is not a v2
    ///   high-capacity (block-addressed) SD card, or a controller whose base
    ///   clock neither it nor the board declares.
    /// * [`DriverError::DeviceFault`] — the controller or card never
    ///   completed a step within its bound.
    pub fn open(host: H, board: Board<'_>) -> Result<Self, BringUpFault> {
        Self::open_with_budget(host, board, DEFAULT_POLL_BUDGET)
    }

    /// As [`Self::open`], bounding every controller wait by `poll_budget`
    /// (host tests assert the fail-closed timeout with a small one).
    ///
    /// # Errors
    ///
    /// As [`Self::open`].
    pub fn open_with_budget(
        host: H,
        board: Board<'_>,
        poll_budget: u32,
    ) -> Result<Self, BringUpFault> {
        let mut dev = Self {
            host,
            geometry: BlockGeometry {
                block_size: BLOCK_SIZE,
                block_count: 0,
            },
            rca: 0,
            poll_budget,
            link: Link {
                mode: BusMode::DefaultSpeed,
                clock_hz: 0,
                base_clock_hz: 0,
                counted_transfers: false,
                dma: false,
                fallback: None,
                dma_fallback: None,
            },
            dma_stage_blocks: 0,
            dma_wedged: false,
            card_state_unknown: false,
        };
        dev.link = dev.init(board)?;
        Ok(dev)
    }

    /// The bus the bring-up left the card on.
    #[must_use]
    pub fn link(&self) -> Link {
        self.link
    }

    /// Borrow the underlying register backing (host-test inspection).
    #[must_use]
    pub fn host(&self) -> &H {
        &self.host
    }

    /// Poll `register` until every bit in `mask` clears — a bounded
    /// handshake with the controller's own reset and clock logic.
    fn wait_clear(&mut self, register: usize, mask: u32) -> Result<(), DriverError> {
        self.wait_register(register, mask, 0)
    }

    /// Poll `register` until every bit in `mask` is set, within the budget.
    fn wait_set(&mut self, register: usize, mask: u32) -> Result<(), DriverError> {
        self.wait_register(register, mask, mask)
    }

    /// Poll `register` until its `mask` bits read `expected`, within the
    /// budget, reporting the last value read when they never do.
    fn wait_register(
        &mut self,
        register: usize,
        mask: u32,
        expected: u32,
    ) -> Result<(), DriverError> {
        let mut value = 0;
        for _ in 0..self.poll_budget {
            value = self.host.read32(register)?;
            if value & mask == expected {
                return Ok(());
            }
            core::hint::spin_loop();
        }
        self.host.trace(Trace::WaitFailed {
            register,
            wanted: mask,
            value,
            waits: 0,
            end: WaitEnd::Budget,
        });
        Err(DriverError::DeviceFault)
    }

    /// Wait for `INTERRUPT` to assert `wanted`, parking on the controller's
    /// interrupt between reads and failing closed on any error bit.
    ///
    /// The wanted bits are cleared before returning, which also lowers the
    /// level-sensitive line for the next wait. A wait that elapses with no
    /// interrupt fails at once: the controller signals every started
    /// operation, so a silent one is dead.
    fn wait_interrupt(&mut self, wanted: u32) -> Result<(), DriverError> {
        let mut status = 0;
        let mut waits = 0;
        let end = loop {
            if waits == self.poll_budget {
                break WaitEnd::Budget;
            }
            status = self.host.read32(regs::REG_INTERRUPT)?;
            if status & regs::INT_ERROR_MASK != 0 {
                self.host.write32(regs::REG_INTERRUPT, status)?;
                break WaitEnd::Error;
            }
            if status & wanted == wanted {
                self.host.write32(regs::REG_INTERRUPT, wanted)?;
                return Ok(());
            }
            waits += 1;
            if self.host.await_irq() == CompletionSignal::TimedOut {
                break WaitEnd::Silent;
            }
        };
        self.host.trace(Trace::WaitFailed {
            register: regs::REG_INTERRUPT,
            wanted,
            value: status,
            waits,
            end,
        });
        Err(DriverError::DeviceFault)
    }

    /// Issue `cmd` with `arg` and `transfer_mode`, returning the response
    /// words (only `RESP0` is meaningful for a short response).
    fn issue(
        &mut self,
        cmd: SdCommand,
        arg: u32,
        transfer_mode: u32,
    ) -> Result<[u32; 4], DriverError> {
        let mut inhibit = regs::STATUS_CMD_INHIBIT;
        if cmd.transfers_data || cmd.response == ResponseKind::ShortBusy {
            inhibit |= regs::STATUS_DAT_INHIBIT;
        }
        self.wait_clear(regs::REG_STATUS, inhibit)?;
        self.host.write32(regs::REG_INTERRUPT, regs::INT_ALL)?;
        self.host.write32(regs::REG_ARG1, arg)?;
        self.host.trace(Trace::Command {
            index: cmd.index,
            arg,
        });
        self.host
            .write32(regs::REG_CMDTM, transfer_mode | cmd.cmd_word())?;
        self.wait_interrupt(regs::INT_CMD_DONE)?;

        let r0 = self.host.read32(regs::REG_RESP0)?;
        self.host.trace(Trace::Response {
            index: cmd.index,
            response: r0,
        });
        if cmd.response == ResponseKind::Long {
            Ok([
                r0,
                self.host.read32(regs::REG_RESP1)?,
                self.host.read32(regs::REG_RESP2)?,
                self.host.read32(regs::REG_RESP3)?,
            ])
        } else {
            Ok([r0, 0, 0, 0])
        }
    }

    /// Issue the application command `acmd` behind `CMD55` addressed to the
    /// card's RCA (zero before `CMD3`, as `ACMD41` requires).
    fn issue_app(
        &mut self,
        acmd: SdCommand,
        arg: u32,
        transfer_mode: u32,
    ) -> Result<[u32; 4], DriverError> {
        self.issue(command::APP_CMD, self.rca, 0)?;
        self.issue(acmd, arg, transfer_mode)
    }

    /// Issue the R1b `cmd` and park until the transfer-complete interrupt
    /// reports the card's busy ended.
    fn issue_awaiting_busy(&mut self, cmd: SdCommand, arg: u32) -> Result<(), DriverError> {
        self.issue(cmd, arg, 0)?;
        self.wait_interrupt(regs::INT_DATA_DONE)
    }

    /// Move one data-port block into `block`, a whole number of words.
    fn read_block_pio(&mut self, block: &mut [u8]) -> Result<(), DriverError> {
        self.wait_interrupt(regs::INT_READ_RDY)?;
        for word in block.as_chunks_mut::<4>().0 {
            *word = self.host.read32(regs::REG_DATA)?.to_le_bytes();
        }
        Ok(())
    }

    /// Move one block from `block`, a whole number of words, into the data
    /// port.
    fn write_block_pio(&mut self, block: &[u8]) -> Result<(), DriverError> {
        self.wait_interrupt(regs::INT_WRITE_RDY)?;
        for word in block.as_chunks::<4>().0 {
            self.host
                .write32(regs::REG_DATA, u32::from_le_bytes(*word))?;
        }
        Ok(())
    }

    /// Read one register-sized block the card sends for `cmd` (an
    /// application command when `app`) into `out` over the data port.
    fn read_register(
        &mut self,
        cmd: SdCommand,
        arg: u32,
        app: bool,
        out: &mut [u8],
    ) -> Result<(), DriverError> {
        let len = u32::try_from(out.len()).map_err(|_| DriverError::LengthOutOfRange)?;
        self.host.write32(regs::REG_BLKSIZECNT, (1 << 16) | len)?;
        let issued = if app {
            self.issue_app(cmd, arg, regs::TM_DAT_DIR_READ)
        } else {
            self.issue(cmd, arg, regs::TM_DAT_DIR_READ)
        };
        let moved = issued.and_then(|_| {
            self.read_block_pio(out)?;
            self.wait_interrupt(regs::INT_DATA_DONE)
        });
        if moved.is_err() {
            self.recover_transfer(false);
        }
        moved
    }

    /// The command and `CMDTM` transfer mode moving `blocks` blocks, by DMA
    /// or through the data port. One definition for every path, so the
    /// direction, block-count, auto-command and DMA bits cannot drift apart.
    fn data_command(&self, blocks: u32, write: bool, dma: bool) -> (SdCommand, u32) {
        let mut mode = if write { 0 } else { regs::TM_DAT_DIR_READ };
        if dma {
            mode |= regs::TM_DMA_EN;
        }
        if blocks == 1 {
            let cmd = if write {
                command::WRITE_BLOCK
            } else {
                command::READ_SINGLE_BLOCK
            };
            return (cmd, mode);
        }
        mode |= regs::TM_BLKCNT_EN | regs::TM_MULTI_BLOCK;
        mode |= if self.link.counted_transfers {
            regs::TM_AUTO_CMD23
        } else {
            regs::TM_AUTO_CMD12
        };
        let cmd = if write {
            command::WRITE_MULTIPLE_BLOCK
        } else {
            command::READ_MULTIPLE_BLOCK
        };
        (cmd, mode)
    }

    /// Program the block count and issue a data command at block `addr`,
    /// failing it when the card's own status reports the command failed.
    fn start_transfer(
        &mut self,
        cmd: SdCommand,
        addr: u32,
        blocks: u32,
        transfer_mode: u32,
    ) -> Result<(), DriverError> {
        self.host
            .write32(regs::REG_BLKSIZECNT, (blocks << 16) | BLOCK_SIZE)?;
        if transfer_mode & regs::TM_AUTO_CMD23 == regs::TM_AUTO_CMD23 {
            self.host.write32(regs::REG_ARG2, blocks)?;
        }
        let status = self.issue(cmd, addr, transfer_mode)?[0];
        if status & command::R1_ERRORS != 0 {
            return Err(DriverError::DeviceFault);
        }
        Ok(())
    }

    /// Collect the status a write left, the only place the card reports a
    /// block it could not program, waiting out its programming busy.
    fn confirm_write(&mut self) -> Result<(), DriverError> {
        let confirmed = (|| {
            for _ in 0..CARD_STATE_ROUNDS {
                let status = self.issue(command::SEND_STATUS, self.rca, 0)?[0];
                if status & command::R1_ERRORS != 0 {
                    return Err(DriverError::DeviceFault);
                }
                match command::card_condition(status) {
                    CardCondition::Ready => return Ok(()),
                    CardCondition::Busy => {
                        self.issue_awaiting_busy(command::SEND_STATUS_BUSY, self.rca)?;
                    }
                    CardCondition::Transferring | CardCondition::Unusable => {
                        return Err(DriverError::DeviceFault);
                    }
                }
            }
            Err(DriverError::DeviceFault)
        })();
        if confirmed.is_err() {
            self.card_state_unknown = true;
        }
        confirmed
    }

    /// Read `buf` from block `addr` over the data port, one command per
    /// [`MAX_BLOCKS_PER_COMMAND`] blocks.
    fn read_blocks_pio(&mut self, addr: u32, buf: &mut [u8]) -> Result<(), DriverError> {
        self.ensure_card_in_tran()?;
        let mut at = addr;
        for chunk in buf.chunks_mut(MAX_BLOCKS_PER_COMMAND * BLOCK_SIZE as usize) {
            let blocks = blocks_in(chunk.len())?;
            let (cmd, mode) = self.data_command(blocks, false, false);
            let moved = self.start_transfer(cmd, at, blocks, mode).and_then(|()| {
                for block in chunk.chunks_mut(BLOCK_SIZE as usize) {
                    self.read_block_pio(block)?;
                }
                self.wait_interrupt(regs::INT_DATA_DONE)
            });
            if moved.is_err() {
                self.recover_transfer(blocks != 1);
                return moved;
            }
            at = at
                .checked_add(blocks)
                .ok_or(DriverError::LengthOutOfRange)?;
        }
        Ok(())
    }

    /// Write `buf` to block `addr` over the data port, one command per
    /// [`MAX_BLOCKS_PER_COMMAND`] blocks.
    fn write_blocks_pio(&mut self, addr: u32, buf: &[u8]) -> Result<(), DriverError> {
        self.ensure_card_in_tran()?;
        let mut at = addr;
        for chunk in buf.chunks(MAX_BLOCKS_PER_COMMAND * BLOCK_SIZE as usize) {
            let blocks = blocks_in(chunk.len())?;
            let (cmd, mode) = self.data_command(blocks, true, false);
            let moved = self.start_transfer(cmd, at, blocks, mode).and_then(|()| {
                for block in chunk.chunks(BLOCK_SIZE as usize) {
                    self.write_block_pio(block)?;
                }
                self.wait_interrupt(regs::INT_DATA_DONE)
            });
            if moved.is_err() {
                self.recover_transfer(blocks != 1);
                return moved;
            }
            self.confirm_write()?;
            at = at
                .checked_add(blocks)
                .ok_or(DriverError::LengthOutOfRange)?;
        }
        Ok(())
    }

    /// Stage the descriptor table for `len` bytes of the data area and
    /// publish it, returning the table's device address.
    fn stage_table(&mut self, len: usize) -> Result<u32, DriverError> {
        let region = self.host.dma_region().ok_or(DriverError::DeviceFault)?;
        let data = u32::try_from(region.data_device).map_err(|_| DriverError::DeviceFault)?;
        let table = u32::try_from(region.table_device).map_err(|_| DriverError::DeviceFault)?;
        let used = adma::encode_table(data, len, region.table).ok_or(DriverError::DeviceFault)?;
        self.host.sync_dma(DmaArea::Table, 0, used);
        Ok(table)
    }

    /// Move `len` staged bytes between the data area and block `addr` by
    /// ADMA2, completing on a single transfer-complete interrupt.
    fn dma_chunk(&mut self, addr: u32, len: usize, write: bool) -> Result<(), DriverError> {
        let blocks = blocks_in(len)?;
        let table = self.stage_table(len)?;
        self.host.sync_dma(DmaArea::Data, 0, len);
        // Publish the staged descriptors and data before the doorbell.
        dma_wmb();
        self.host.write32(regs::REG_ADMA_ADDR, table)?;
        let (cmd, mode) = self.data_command(blocks, write, true);
        let moved = self
            .start_transfer(cmd, addr, blocks, mode)
            .and_then(|()| self.wait_interrupt(regs::INT_DATA_DONE));
        if moved.is_err() {
            self.recover_transfer(blocks != 1);
        }
        moved
    }

    /// Read `buf` from block `addr` by ADMA2, one staging window per command.
    fn read_blocks_dma(&mut self, addr: u32, buf: &mut [u8]) -> Result<(), DriverError> {
        self.staging_free()?;
        self.ensure_card_in_tran()?;
        let mut at = addr;
        for chunk in buf.chunks_mut(self.dma_stage_blocks * BLOCK_SIZE as usize) {
            self.dma_chunk(at, chunk.len(), false)?;
            // Order reads of device-written data after the completion and
            // drop any stale cached copy before the bytes are consumed.
            dma_rmb();
            self.host.sync_dma(DmaArea::Data, 0, chunk.len());
            let region = self.host.dma_region().ok_or(DriverError::DeviceFault)?;
            chunk.copy_from_slice(&region.data[..chunk.len()]);
            at = at
                .checked_add(blocks_in(chunk.len())?)
                .ok_or(DriverError::LengthOutOfRange)?;
        }
        Ok(())
    }

    /// Write `buf` to block `addr` by ADMA2, one staging window per command.
    fn write_blocks_dma(&mut self, addr: u32, buf: &[u8]) -> Result<(), DriverError> {
        self.staging_free()?;
        self.ensure_card_in_tran()?;
        let mut at = addr;
        for chunk in buf.chunks(self.dma_stage_blocks * BLOCK_SIZE as usize) {
            {
                let region = self.host.dma_region().ok_or(DriverError::DeviceFault)?;
                region.data[..chunk.len()].copy_from_slice(chunk);
            }
            self.dma_chunk(at, chunk.len(), true)?;
            self.confirm_write()?;
            at = at
                .checked_add(blocks_in(chunk.len())?)
                .ok_or(DriverError::LengthOutOfRange)?;
        }
        Ok(())
    }

    /// Zero the first `len` bytes of the data staging — whatever a sensitive
    /// transfer left there — and push the zeroes out to memory.
    fn scrub_staging(&mut self, len: usize) {
        let Some(region) = self.host.dma_region() else {
            return;
        };
        let used = len.min(region.data.len());
        region.data[..used].fill(0);
        self.host.sync_dma(DmaArea::Data, 0, used);
    }

    /// Recover from a failed transfer by the SDHCI error-interrupt sequence:
    /// reset the command and data lines, then abort a `multi`-block transfer
    /// with `CMD12` and wait out its busy.
    ///
    /// Only an answered abort proves the card back in `tran`. A line reset
    /// that never confirms sends no abort and wedges DMA, since only the
    /// data-line reset halts the DMA engine. A failed abort leaves the
    /// transfer's own error standing.
    fn recover_transfer(&mut self, multi: bool) {
        self.card_state_unknown = true;
        if self.reset_lines().is_err() {
            if self.dma_stage_blocks != 0 {
                self.dma_wedged = true;
            }
            return;
        }
        if multi
            && self
                .issue_awaiting_busy(command::STOP_TRANSMISSION, 0)
                .is_ok()
        {
            self.card_state_unknown = false;
        }
    }

    /// Before a data command, prove a card of unknown state back in `tran`
    /// with `SEND_STATUS`, aborting a transfer still open and awaiting a
    /// programming card's busy, for at most [`CARD_STATE_ROUNDS`] rounds.
    ///
    /// Any other answer, or a failed command, fails closed and keeps the
    /// state unknown, so the next data command asks again.
    fn ensure_card_in_tran(&mut self) -> Result<(), DriverError> {
        if !self.card_state_unknown {
            return Ok(());
        }
        // Whatever left the state unknown may have left a line error latched.
        self.reset_lines()?;
        for _ in 0..CARD_STATE_ROUNDS {
            let status = self.issue(command::SEND_STATUS, self.rca, 0)?[0];
            match command::card_condition(status) {
                CardCondition::Ready => {
                    self.card_state_unknown = false;
                    return Ok(());
                }
                CardCondition::Transferring => {
                    self.issue_awaiting_busy(command::STOP_TRANSMISSION, 0)?;
                }
                CardCondition::Busy => {
                    self.issue_awaiting_busy(command::SEND_STATUS_BUSY, self.rca)?;
                }
                CardCondition::Unusable => return Err(DriverError::DeviceFault),
            }
        }
        Err(DriverError::DeviceFault)
    }

    /// Reset the command and data lines; the data-line reset also halts the
    /// DMA engine.
    fn reset_lines(&mut self) -> Result<(), DriverError> {
        let lines = regs::CONTROL1_SRST_CMD | regs::CONTROL1_SRST_DATA;
        let control1 = self.host.read32(regs::REG_CONTROL1)?;
        self.host.write32(regs::REG_CONTROL1, control1 | lines)?;
        self.wait_clear(regs::REG_CONTROL1, lines)
    }

    /// Refuse a transfer while the controller may still be mastering the
    /// staging an earlier one handed it.
    fn staging_free(&self) -> Result<(), DriverError> {
        if self.dma_wedged {
            Err(DriverError::DeviceFault)
        } else {
            Ok(())
        }
    }

    /// Validate a transfer of `buf_len` bytes at `lba` against the geometry,
    /// returning the 32-bit block address SDHC and SDXC cards take.
    fn validate_transfer(&self, lba: u64, buf_len: usize) -> Result<u32, DriverError> {
        let bs = BLOCK_SIZE as usize;
        if buf_len == 0 || !buf_len.is_multiple_of(bs) {
            return Err(DriverError::BufferTooSmall);
        }
        let end = lba
            .checked_add((buf_len / bs) as u64)
            .ok_or(DriverError::LengthOutOfRange)?;
        if end > self.geometry.block_count {
            return Err(DriverError::LengthOutOfRange);
        }
        u32::try_from(lba).map_err(|_| DriverError::LengthOutOfRange)
    }
}

/// The block count of a whole-block span of `len` bytes.
fn blocks_in(len: usize) -> Result<u32, DriverError> {
    u32::try_from(len / BLOCK_SIZE as usize).map_err(|_| DriverError::LengthOutOfRange)
}

impl<H: SdhciHost> Drop for Emmc2<H> {
    /// A controller whose line reset never confirmed may still be mastering
    /// the staging, which is then held for the kernel to quarantine.
    fn drop(&mut self) {
        if self.dma_wedged {
            self.host.withhold_dma();
        }
    }
}

impl<H: SdhciHost> Block for Emmc2<H> {
    /// An SD card in a slot: flash-quick when healthy, but pullable at any
    /// moment, so it is served the removable envelope.
    fn device_class(&self) -> BlkDeviceClass {
        BlkDeviceClass::Removable
    }

    /// The second external mass-media controller, as the silicon names it.
    fn device_name(&self) -> BlkDeviceName {
        BlkDeviceName::new("emmc2")
    }

    fn geometry(&self) -> Result<BlockGeometry, DriverError> {
        Ok(self.geometry)
    }

    fn read_blocks(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), DriverError> {
        let addr = self.validate_transfer(lba, buf.len())?;
        if self.dma_stage_blocks != 0 {
            self.read_blocks_dma(addr, buf)
        } else {
            self.read_blocks_pio(addr, buf)
        }
    }

    fn write_blocks(&mut self, lba: u64, buf: &[u8]) -> Result<(), DriverError> {
        let addr = self.validate_transfer(lba, buf.len())?;
        if self.dma_stage_blocks != 0 {
            self.write_blocks_dma(addr, buf)
        } else {
            self.write_blocks_pio(addr, buf)
        }
    }

    /// The card's write cache is never enabled, and every write waits for the
    /// card to leave programming, so nothing volatile remains to commit.
    fn flush(&mut self) -> Result<(), DriverError> {
        Ok(())
    }

    /// As [`Block::read_blocks`]; a sensitive payload's staging copy is
    /// zeroed before the call returns, whatever the outcome.
    fn read_blocks_with_class(
        &mut self,
        lba: u64,
        buf: &mut [u8],
        class: BufferClass,
    ) -> Result<(), DriverError> {
        let result = self.read_blocks(lba, buf);
        if class == BufferClass::Sensitive {
            self.scrub_staging(buf.len());
        }
        result
    }

    /// As [`Block::write_blocks`]; a sensitive payload's staging copy is
    /// zeroed before the call returns, whatever the outcome.
    fn write_blocks_with_class(
        &mut self,
        lba: u64,
        buf: &[u8],
        class: BufferClass,
    ) -> Result<(), DriverError> {
        let result = self.write_blocks(lba, buf);
        if class == BufferClass::Sensitive {
            self.scrub_staging(buf.len());
        }
        result
    }
}
