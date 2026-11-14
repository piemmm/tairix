//! The 6850 asynchronous communications interface adapter: the serial port
//! the cassette and RS423 share.
//!
//! The transmitter is timed exactly: a byte written to the transmit data
//! register moves to the shift register as soon as it is free, one bit time
//! after the write when the transmitter was idle, and takes its frame's bits
//! at the rate the serial ULA's clock and the divide ratio set.
//! The receiver is handed whole frames by whatever drives its data line (the
//! cassette or RS423), each already sampled at the receiver's own rate.

use alloc::vec::Vec;

use crate::machine::cassette::Format;
use crate::machine::CLOCK;

/// Control register fields.
pub mod control {
    /// Counter divide select.
    pub const DIVIDE: u8 = 0x03;
    /// The divide select that holds the chip in master reset.
    pub const MASTER_RESET: u8 = 0x03;
    /// Word select.
    pub const WORD: u8 = 0x1C;
    /// Transmitter control.
    pub const TRANSMIT: u8 = 0x60;
    /// Transmitter control: RTS low, no transmit interrupt.
    pub const READY: u8 = 0x00;
    /// Transmit interrupt enabled, RTS low.
    pub const TRANSMIT_INTERRUPT: u8 = 0x20;
    /// Transmitter control: RTS high, no transmit interrupt.
    pub const NOT_READY: u8 = 0x40;
    /// Receive interrupt enable.
    pub const RECEIVE_INTERRUPT: u8 = 0x80;
}

/// Status register bits.
pub mod status {
    /// Receive data register full.
    pub const RDRF: u8 = 0x01;
    /// Transmit data register empty.
    pub const TDRE: u8 = 0x02;
    /// Data carrier detect: high when the carrier is lost.
    pub const DCD: u8 = 0x04;
    /// Clear to send: high when the far end may not be sent to.
    pub const CTS: u8 = 0x08;
    /// Framing error.
    pub const FE: u8 = 0x10;
    /// Receiver overrun.
    pub const OVRN: u8 = 0x20;
    /// Parity error.
    pub const PE: u8 = 0x40;
    /// The IRQ output is asserted.
    pub const IRQ: u8 = 0x80;
}

/// The frame shape the word select bits choose.
const fn format_of(control: u8) -> Format {
    let (data_bits, parity, stop_bits) = match (control & control::WORD) >> 2 {
        0 => (7, Some(false), 2),
        1 => (7, Some(true), 2),
        2 => (7, Some(false), 1),
        3 => (7, Some(true), 1),
        4 => (8, None, 2),
        5 => (8, None, 1),
        6 => (8, Some(false), 1),
        _ => (8, Some(true), 1),
    };
    Format {
        data_bits,
        parity,
        stop_bits,
    }
}

/// The chip.
#[derive(Clone, Debug, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each is an independent status bit or modem line"
)]
pub struct Acia {
    control: u8,
    /// In master reset until the control register is next written otherwise.
    resetting: bool,
    rdr: u8,
    rdrf: bool,
    /// A frame arrived over an unread one; the status shows it once the
    /// unread one has been read.
    overrun_pending: bool,
    overrun: bool,
    framing_error: bool,
    parity_error: bool,
    tdr: u8,
    /// The transmit data register holds a byte the shift register has not
    /// yet taken.
    tdr_full: bool,
    /// When a byte waiting in the transmit data register moves into the
    /// idle shift register: the transmitter's next bit-clock edge.
    transfer_at: Option<u64>,
    /// The byte the shift register is sending, and the cycle it finishes.
    shifting: Option<(u8, u64)>,
    /// The cycle the transmitter's bit clock counts from: where master reset
    /// last released it, or its clock last changed.
    bit_origin: u64,
    /// The input lines.
    dcd: bool,
    cts: bool,
    /// DCD rose and the status-then-data read that clears it has not run.
    dcd_latched: bool,
    /// The status register was read since DCD's rise.
    dcd_status_read: bool,
    /// The serial ULA's clock rates, in Hz, for the two directions.
    transmit_clock: u64,
    receive_clock: u64,
    /// Bytes transmitted since last taken, with the cycle each finished.
    transmitted: Vec<(u64, u8)>,
}

impl Default for Acia {
    fn default() -> Self {
        Self::new()
    }
}

impl Acia {
    /// A chip at power-on, held in master reset.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            control: control::MASTER_RESET,
            resetting: true,
            rdr: 0,
            rdrf: false,
            overrun_pending: false,
            overrun: false,
            framing_error: false,
            parity_error: false,
            tdr: 0,
            tdr_full: false,
            transfer_at: None,
            shifting: None,
            bit_origin: 0,
            dcd: false,
            cts: false,
            dcd_latched: false,
            dcd_status_read: false,
            transmit_clock: 64 * 19_200,
            receive_clock: 64 * 19_200,
            transmitted: Vec::new(),
        }
    }

    /// The serial ULA's clocks, in Hz, from `cycle`.
    pub fn set_clocks(&mut self, cycle: u64, transmit: u64, receive: u64) {
        self.advance(cycle);
        if transmit != self.transmit_clock {
            self.transmit_clock = transmit;
            self.bit_origin = cycle;
            self.transfer_at = self.transfer_at.map(|_| self.bit_edge(cycle, 1));
        }
        self.receive_clock = receive;
    }

    /// The divide ratio the control register selects.
    const fn divide(&self) -> u64 {
        match self.control & control::DIVIDE {
            0 => 1,
            1 => 16,
            _ => 64,
        }
    }

    /// Cycles a transmitted frame takes, to the nearest cycle below.
    #[must_use]
    pub fn transmit_frame(&self) -> u64 {
        self.format().frame_bits() * CLOCK * self.divide() / self.transmit_clock.max(1)
    }

    /// The transmitter's bit-clock edge `bits` edges after the last at or
    /// before `cycle`.
    fn bit_edge(&self, cycle: u64, bits: u64) -> u64 {
        let period = u128::from(CLOCK) * u128::from(self.divide());
        let clock = u128::from(self.transmit_clock.max(1));
        let elapsed = u128::from(cycle.saturating_sub(self.bit_origin)) * clock / period;
        let edge = (elapsed + u128::from(bits)) * period;
        self.bit_origin + u64::try_from(edge.div_ceil(clock)).unwrap_or(u64::MAX)
    }

    /// The rate, in bits per second, the receiver samples at, and the frame
    /// shape it expects: what a source driving the data line must frame by.
    #[must_use]
    pub const fn receive_rate(&self) -> (u64, Format) {
        (self.receive_clock / self.divide(), format_of(self.control))
    }

    /// The frame shape the transmitter sends.
    #[must_use]
    pub const fn format(&self) -> Format {
        format_of(self.control)
    }

    /// Whether RTS is asserted (low): the data carrier enable for the
    /// cassette, request to send for RS423. Master reset leaves it to the
    /// transmitter control bits.
    #[must_use]
    pub const fn rts(&self) -> bool {
        self.control & control::TRANSMIT != 0x40
    }

    /// Whether the chip asserts IRQ.
    #[must_use]
    pub const fn irq(&self) -> bool {
        if self.resetting {
            return false;
        }
        let receive = self.control & control::RECEIVE_INTERRUPT != 0
            && (self.rdrf || self.overrun || self.dcd_latched);
        let transmit =
            self.control & control::TRANSMIT == control::TRANSMIT_INTERRUPT && self.tdre();
        receive || transmit
    }

    /// The transmit data register reads empty: CTS high (the far end not
    /// ready) inhibits it, and so does master reset.
    const fn tdre(&self) -> bool {
        !self.tdr_full && !self.cts && !self.resetting
    }

    /// The next cycle at which the transmitter changes the IRQ output or
    /// finishes a byte, if any.
    #[must_use]
    pub fn next_event(&self) -> Option<u64> {
        [self.transfer_at, self.shifting.map(|(_, done)| done)]
            .into_iter()
            .flatten()
            .min()
    }

    /// Bring the transmitter up to `cycle`.
    pub fn advance(&mut self, cycle: u64) {
        loop {
            match (self.transfer_at, self.shifting) {
                (Some(at), _) if at <= cycle => self.transfer(at),
                (_, Some((byte, done))) if done <= cycle => {
                    self.transmitted.push((done, byte));
                    self.shifting = None;
                    // A frame ends on a bit-clock edge, where a waiting byte
                    // moves in at once.
                    if self.tdr_full {
                        self.transfer(done);
                    }
                }
                _ => return,
            }
        }
    }

    /// The waiting byte moves into the shift register at bit-clock edge
    /// `at`.
    fn transfer(&mut self, at: u64) {
        self.transfer_at = None;
        if self.tdr_full && self.shifting.is_none() && !self.resetting {
            let bits = self.format().frame_bits();
            self.shifting = Some((self.tdr, self.bit_edge(at, bits)));
            self.tdr_full = false;
        }
    }

    /// The bytes transmitted since last asked, each with the cycle its stop
    /// bit finished.
    pub fn take_transmitted(&mut self) -> alloc::vec::Drain<'_, (u64, u8)> {
        self.transmitted.drain(..)
    }

    /// The serial ULA's clocks, in Hz, for the two directions.
    pub(crate) const fn clocks(&self) -> (u64, u64) {
        (self.transmit_clock, self.receive_clock)
    }

    /// The DCD and CTS input lines.
    pub(crate) const fn lines(&self) -> (bool, bool) {
        (self.dcd, self.cts)
    }

    /// A frame arrived on the data line, finishing at `cycle`.
    pub fn receive(&mut self, cycle: u64, byte: u8, framing_error: bool, parity_error: bool) {
        self.advance(cycle);
        if self.resetting {
            return;
        }
        if self.rdrf {
            self.overrun_pending = true;
            return;
        }
        let mask = if self.format().data_bits == 7 {
            0x7F
        } else {
            0xFF
        };
        self.rdr = byte & mask;
        self.rdrf = true;
        self.framing_error = framing_error;
        self.parity_error = parity_error;
    }

    /// Drive the data carrier detect input.
    pub fn set_dcd(&mut self, level: bool) {
        if level && !self.dcd {
            self.dcd_latched = true;
            self.dcd_status_read = false;
        }
        self.dcd = level;
    }

    /// Drive the clear to send input.
    pub fn set_cts(&mut self, level: bool) {
        self.cts = level;
    }

    /// The CPU reads the status (`data` false) or receive data register.
    pub fn read(&mut self, cycle: u64, data: bool) -> u8 {
        self.advance(cycle);
        if !data {
            if self.dcd_latched {
                self.dcd_status_read = true;
            }
            return self.status();
        }
        if self.dcd_status_read {
            self.dcd_latched = false;
            self.dcd_status_read = false;
        }
        // The read that takes the last good frame shows the overrun, and
        // RDRF stays set until the next read clears it.
        if self.overrun_pending {
            self.overrun_pending = false;
            self.overrun = true;
        } else {
            self.rdrf = false;
            self.overrun = false;
        }
        self.rdr
    }

    /// The CPU writes the control (`data` false) or transmit data register.
    pub fn write(&mut self, cycle: u64, data: bool, value: u8) {
        self.advance(cycle);
        if data {
            // An idle transmitter's bit clock starts at the write, so a byte
            // written within a bit time replaces it: the OS's doubled sync
            // byte puts one on tape.
            self.tdr = value;
            self.tdr_full = true;
            if self.shifting.is_none() && self.transfer_at.is_none() && !self.resetting {
                self.bit_origin = cycle;
                self.transfer_at = Some(self.bit_edge(cycle, 1));
            }
            return;
        }
        self.control = value;
        if value & control::DIVIDE == control::MASTER_RESET {
            self.master_reset();
        } else if self.resetting {
            self.resetting = false;
            self.bit_origin = cycle;
        }
    }

    fn master_reset(&mut self) {
        self.resetting = true;
        self.rdrf = false;
        self.overrun_pending = false;
        self.overrun = false;
        self.framing_error = false;
        self.parity_error = false;
        self.tdr_full = false;
        self.transfer_at = None;
        self.shifting = None;
        self.dcd_latched = false;
        self.dcd_status_read = false;
    }

    fn status(&self) -> u8 {
        let mut status = 0;
        // A lost carrier makes the receive register read as empty.
        if self.rdrf && !self.dcd {
            status |= status::RDRF;
        }
        if self.tdre() {
            status |= status::TDRE;
        }
        if self.dcd || self.dcd_latched {
            status |= status::DCD;
        }
        if self.cts {
            status |= status::CTS;
        }
        if self.framing_error {
            status |= status::FE;
        }
        if self.overrun {
            status |= status::OVRN;
        }
        if self.parity_error {
            status |= status::PE;
        }
        if self.irq() {
            status |= status::IRQ;
        }
        status
    }
}
crate::snapshot::fields!(
    Acia {
        control,
        resetting,
        rdr,
        rdrf,
        overrun_pending,
        overrun,
        framing_error,
        parity_error,
        tdr,
        tdr_full,
        transfer_at,
        shifting,
        bit_origin,
        dcd,
        cts,
        dcd_latched,
        dcd_status_read,
        transmit_clock,
        receive_clock,
    }
    transient { transmitted },
    check | acia,
    r | acia.restorable(r)
);

impl Acia {
    /// Whether a restored chip holds only what one can: in master reset
    /// exactly while its control register says so, with nothing received or
    /// moving then, a waiting transfer only for a full register and an idle
    /// shifter, clocks no faster than the machine's, and its times at the
    /// machine's.
    fn restorable(&self, r: &crate::snapshot::Reader<'_>) -> bool {
        let reset = self.control & control::DIVIDE == control::MASTER_RESET;
        let quiet = !(self.rdrf
            || self.overrun_pending
            || self.overrun
            || self.framing_error
            || self.parity_error
            || self.transfer_at.is_some()
            || self.shifting.is_some());
        let transfer = self
            .transfer_at
            .is_none_or(|at| self.tdr_full && self.shifting.is_none() && r.pending(at));
        self.resetting == reset
            && (!reset || quiet)
            && (!self.overrun_pending || self.rdrf)
            && transfer
            && self.shifting.is_none_or(|(_, done)| r.pending(done))
            && r.reached(self.bit_origin)
            && [self.transmit_clock, self.receive_clock]
                .iter()
                .all(|hz| (1..=CLOCK).contains(hz))
    }
}
