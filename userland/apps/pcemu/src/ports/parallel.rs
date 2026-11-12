//! The parallel port and what is plugged into it: a printer, or a DAC made
//! for the port — the Covox Speech Thing, an 8-bit DAC on the data lines, or
//! the Disney Sound Source, whose 16-byte FIFO its own 7 kHz clock drains
//! into its DAC.
//!
//! The printer latches a byte as Strobe is asserted and is Busy until it
//! pulses Ack; Init held low resets it. Once its buffer fills it stays Busy
//! until its output is drained, so no byte it accepted is lost. The
//! adapter's interrupt follows Ack while the control register enables it.
//! The Sound Source latches a byte on Select In's rising edge after a short
//! pulse, powers down when the line is held low past its RC time, and shows
//! a full FIFO on Ack while Init feeds its status circuit, as Disney's
//! programmer's guide describes.

use crate::snapshot::{restore_queue, save_queue, In, Out, State, Torn};
use crate::sound::{Mixer, Source};
use crate::time::{Hertz, Nanos};
use alloc::collections::VecDeque;

/// The port's three registers.
pub const PORTS: u16 = 3;
/// Where a parallel adapter may be, in the order the POST lists them: a
/// monochrome display adapter's own, then the two of a separate adapter.
pub const BASES: [u16; 3] = [0x3BC, 0x378, 0x278];

/// The interrupt an adapter at `base` raises: the second adapter's is 5.
#[must_use]
pub const fn irq(base: u16) -> u8 {
    if base == 0x278 {
        5
    } else {
        7
    }
}

/// The registers, from the base.
pub mod reg {
    /// The data latch.
    pub const DATA: u16 = 0;
    /// The status lines.
    pub const STATUS: u16 = 1;
    /// The control lines.
    pub const CONTROL: u16 = 2;
}

/// How long the printer is Busy taking a byte, and the Ack pulse that ends
/// it.
const PRINT_TIME: Nanos = 10_000;
const ACK_PULSE: Nanos = 5_000;
/// What the printer holds before it is drained.
const PAPER: usize = 64 * 1024;

/// How long Select In held low powers the Sound Source down: well past the
/// microsecond pulse that latches a byte.
const POWER_DOWN: Nanos = 1_000_000;
/// The Sound Source's FIFO.
const DEPTH: usize = 16;

/// What is plugged into a port.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Device {
    /// Nothing: the status lines float high.
    Nothing,
    /// The Covox Speech Thing.
    Covox,
    /// The Disney Sound Source.
    SoundSource,
    /// A printer, its output kept for the user.
    Printer,
}

/// The control register's lines.
pub mod control {
    /// Strobe, asserted when set.
    pub const STROBE: u8 = 0x01;
    /// Init, asserted when clear.
    pub const INIT: u8 = 0x04;
    /// Select In, asserted when set.
    pub const SELECT_IN: u8 = 0x08;
    /// Lets Ack interrupt.
    pub const IRQ: u8 = 0x10;
}

/// The status register's lines.
pub mod status {
    /// Busy, inverted: set while the printer can take a byte.
    pub const NOT_BUSY: u8 = 0x80;
    /// Acknowledge, low during its pulse.
    pub const ACK: u8 = 0x40;
    /// Paper out.
    pub const PAPER_OUT: u8 = 0x20;
    /// Selected.
    pub const SELECTED: u8 = 0x10;
    /// Error, low when asserted.
    pub const NO_ERROR: u8 = 0x08;
    /// The register's three unused bits, which read high.
    pub const UNUSED: u8 = 0x07;
    /// Every line but Ack and Busy floating high, Busy reading low.
    pub const FLOATING: u8 = 0x3F;
}

#[derive(Clone, Debug)]
struct Printer {
    printed: VecDeque<u8>,
    /// When it acknowledges the byte it took last.
    ack_at: Option<Nanos>,
}

impl Printer {
    fn full(&self) -> bool {
        self.printed.len() >= PAPER
    }

    fn busy(&self, t: Nanos) -> bool {
        self.full() || self.ack_at.is_some_and(|at| t < at)
    }

    /// Whether Ack's pulse is on at `t`.
    fn acking(&self, t: Nanos) -> bool {
        self.ack_at.is_some_and(|at| at <= t && t < at + ACK_PULSE)
    }
}

#[derive(Clone, Copy, Debug)]
struct SoundSource {
    fifo: [u8; DEPTH],
    head: usize,
    len: usize,
    powered: bool,
    /// When Select In went low, while it is.
    low_since: Option<Nanos>,
    /// Ticks of its clock played.
    played: u64,
}

impl SoundSource {
    fn full(&self) -> bool {
        self.len == DEPTH
    }

    fn push(&mut self, byte: u8) {
        if !self.full() {
            self.fifo[(self.head + self.len) % DEPTH] = byte;
            self.len += 1;
        }
    }

    fn pop(&mut self) -> Option<u8> {
        if self.len == 0 {
            return None;
        }
        let byte = self.fifo[self.head];
        self.head = (self.head + 1) % DEPTH;
        self.len -= 1;
        Some(byte)
    }
}

/// A parallel port and what is plugged into it.
#[derive(Clone, Debug)]
pub struct Parallel {
    data: u8,
    control: u8,
    device: Device,
    source: SoundSource,
    printer: Printer,
    /// The code the mix last heard.
    heard: u8,
}

impl Parallel {
    /// A port at power-up with `device` plugged in.
    #[must_use]
    pub const fn new(device: Device) -> Self {
        Self {
            data: 0,
            control: control::INIT,
            device,
            source: SoundSource {
                fifo: [0; DEPTH],
                head: 0,
                len: 0,
                powered: true,
                low_since: None,
                played: 0,
            },
            printer: Printer {
                printed: VecDeque::new(),
                ack_at: None,
            },
            heard: 0,
        }
    }

    /// Moves what the printer printed into `out`; returns the bytes moved. A
    /// printer held Busy by a full buffer takes bytes again.
    pub fn take_printed(&mut self, out: &mut [u8]) -> usize {
        let moved = out.len().min(self.printer.printed.len());
        for (slot, byte) in out.iter_mut().zip(self.printer.printed.drain(..moved)) {
            *slot = byte;
        }
        moved
    }

    /// The adapter's interrupt line at `t`: Ack, while enabled.
    #[must_use]
    pub fn irq(&self, t: Nanos) -> bool {
        self.device == Device::Printer && self.control & control::IRQ != 0 && self.printer.acking(t)
    }

    /// When the interrupt line next changes, if it is enabled.
    #[must_use]
    pub fn next_event(&self, t: Nanos) -> Option<Nanos> {
        if self.device != Device::Printer || self.control & control::IRQ == 0 {
            return None;
        }
        let at = self.printer.ack_at?;
        [at, at + ACK_PULSE].into_iter().find(|&edge| edge > t)
    }

    /// What is plugged in.
    #[must_use]
    pub const fn device(&self) -> Device {
        self.device
    }

    /// Plays the Sound Source's clock through `t`.
    pub fn advance(&mut self, t: Nanos, mixer: &mut Mixer) {
        if self.device != Device::SoundSource {
            return;
        }
        if let Some(since) = self.source.low_since {
            if t >= since + POWER_DOWN && self.source.powered {
                self.power(false, since + POWER_DOWN, mixer);
            }
        }
        let due = Hertz::SOUND_SOURCE.ticks_at(t);
        while self.source.played < due {
            self.source.played += 1;
            let at = Hertz::SOUND_SOURCE.time_of(self.source.played);
            if self.source.powered {
                if let Some(byte) = self.source.pop() {
                    self.set_level(at, byte, mixer);
                }
            }
        }
    }

    fn power(&mut self, on: bool, t: Nanos, mixer: &mut Mixer) {
        self.source.powered = on;
        if !on {
            self.source.len = 0;
            self.set_level(t, 0, mixer);
        }
    }

    /// The DAC's code becomes `code`: unipolar, as the parallel DACs are.
    fn set_level(&mut self, t: Nanos, code: u8, mixer: &mut Mixer) {
        if code != self.heard {
            let delta = (f32::from(code) - f32::from(self.heard)) / 255.0;
            mixer.step(Source::Parallel, Hertz::CRYSTAL.ticks_at(t), delta);
            self.heard = code;
        }
    }

    /// Reads register `offset` at `t`.
    pub fn input(&mut self, offset: u16, t: Nanos, mixer: &mut Mixer) -> u8 {
        self.advance(t, mixer);
        match offset {
            reg::DATA => self.data,
            reg::STATUS => match self.device {
                Device::Printer => {
                    let busy = self.printer.busy(t);
                    let not_busy = if busy { 0 } else { status::NOT_BUSY };
                    let ack = if self.printer.acking(t) {
                        0
                    } else {
                        status::ACK
                    };
                    not_busy | ack | status::SELECTED | status::NO_ERROR | status::UNUSED
                }
                Device::SoundSource => {
                    let full = self.source.powered
                        && self.control & control::INIT != 0
                        && self.source.full();
                    status::FLOATING | if full { status::ACK } else { 0 }
                }
                Device::Nothing | Device::Covox => status::FLOATING | status::ACK,
            },
            _ => 0xE0 | (self.control & 0x1F),
        }
    }

    /// Writes register `offset` at `t`.
    pub fn output(&mut self, offset: u16, value: u8, t: Nanos, mixer: &mut Mixer) {
        self.advance(t, mixer);
        match offset {
            reg::DATA => {
                self.data = value;
                if self.device == Device::Covox {
                    self.set_level(t, value, mixer);
                }
            }
            reg::CONTROL => {
                let was = self.control;
                self.control = value;
                match self.device {
                    Device::SoundSource => {
                        let (was_low, low) = (
                            was & control::SELECT_IN != 0,
                            value & control::SELECT_IN != 0,
                        );
                        self.select_in(was_low, low, t, mixer);
                    }
                    Device::Printer => self.printer_lines(was, value, t),
                    Device::Nothing | Device::Covox => {}
                }
            }
            _ => {}
        }
    }

    /// The printer's control lines: Init low resets it, and Strobe asserted
    /// hands it the data unless it is busy.
    fn printer_lines(&mut self, was: u8, now: u8, t: Nanos) {
        if now & control::INIT == 0 {
            self.printer.ack_at = None;
            return;
        }
        let strobed = was & control::STROBE == 0 && now & control::STROBE != 0;
        if strobed && !self.printer.busy(t) {
            self.printer.printed.push_back(self.data);
            self.printer.ack_at = Some(t + PRINT_TIME);
        }
    }

    /// Select In's edges: low starts the RC timing; back high latches the
    /// data after a short pulse, or powers up after a long one.
    fn select_in(&mut self, was_low: bool, low: bool, t: Nanos, mixer: &mut Mixer) {
        match (was_low, low) {
            (false, true) => self.source.low_since = Some(t),
            (true, false) => {
                let pulse = self.source.low_since.map_or(0, |since| t - since);
                self.source.low_since = None;
                if pulse >= POWER_DOWN || !self.source.powered {
                    self.power(true, t, mixer);
                    // Its clock runs from power-up.
                    self.source.played = Hertz::SOUND_SOURCE.ticks_at(t);
                } else {
                    self.source.push(self.data);
                }
            }
            _ => {}
        }
    }
}

impl State for Printer {
    fn save(&self, out: &mut Out) {
        let Self { printed, ack_at } = self;
        save_queue(printed, out);
        ack_at.save(out);
    }

    fn restore(&mut self, input: &mut In<'_>) -> Result<(), Torn> {
        restore_queue(&mut self.printed, PAPER, input)?;
        self.ack_at.restore(input)
    }
}

crate::snapshot::state!(
    SoundSource { fifo, head, len, powered, low_since, played }
    check |source: &SoundSource| source.head < DEPTH && source.len <= DEPTH
);

crate::snapshot::state!(
    Parallel { data, control, source, printer, heard }
    as built { device }
);

#[cfg(test)]
#[path = "parallel_tests.rs"]
mod tests;
