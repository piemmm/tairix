//! The PC's serial adapters: an 8250 or 16450 UART, and the line to what is
//! plugged into it.
//!
//! The line is levels in time. A device sends frames — start bit, data,
//! parity, stop bits — at its own rate and in its own format; the receiver
//! takes a falling edge for a start bit, checks it half a bit later and
//! samples a bit apart at the rate and in the format its own registers set.
//! A port set wrongly therefore receives what the hardware does: shifted
//! bits, framing and parity errors, breaks, and the resynchronisation that
//! takes a low stop bit for the next start bit. The adapter gates the UART's
//! interrupt output with OUT2, as IBM's does, and loopback holds that pin
//! inactive, so a looped port interrupts nothing.

use super::mouse::Mouse;
use crate::snapshot::{In, Out, State, Torn};
use crate::time::{Hertz, Nanos};

/// Where COM1 to COM4's registers start.
pub const BASES: [u16; 4] = [0x3F8, 0x2F8, 0x3E8, 0x2E8];
/// COM1 to COM4's interrupts: each pair shares one, as OUT2 lets it.
pub const IRQS: [u8; 4] = [4, 3, 4, 3];

/// The UART's registers, from its base.
pub mod reg {
    /// The receiver buffer and transmitter holding registers, or the
    /// divisor latch's low byte.
    pub const DATA: u16 = 0;
    /// The interrupt enable register, or the divisor latch's high byte.
    pub const IER: u16 = 1;
    /// The interrupt identification register.
    pub const IIR: u16 = 2;
    /// The line control register.
    pub const LCR: u16 = 3;
    /// The modem control register.
    pub const MCR: u16 = 4;
    /// The line status register.
    pub const LSR: u16 = 5;
    /// The modem status register.
    pub const MSR: u16 = 6;
}

/// The line control register's divisor latch access bit.
pub const DLAB: u8 = 0x80;

/// Which UART.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Model {
    /// The original 8250, which has no scratch register.
    Ins8250,
    /// The 16450.
    Ns16450,
}

/// A character's parity bit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Parity {
    /// No parity bit.
    None,
    /// Odd.
    Odd,
    /// Even.
    Even,
    /// Always one.
    Mark,
    /// Always zero.
    Space,
}

/// A character's stop bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    /// One.
    One,
    /// One and a half, which five-bit characters take for two.
    OneAndHalf,
    /// Two.
    Two,
}

/// How characters are framed on the line, and at what rate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Format {
    /// Data bits: 5 to 8.
    pub bits: u8,
    /// The parity bit.
    pub parity: Parity,
    /// The stop bits.
    pub stop: Stop,
    /// The divisor of 115 200 bits a second; 0 counts as 65 536.
    pub divisor: u16,
}

impl Format {
    /// The line control register's framing at `divisor`.
    #[must_use]
    pub const fn from_lcr(lcr: u8, divisor: u16) -> Self {
        let bits = 5 + (lcr & 3);
        let parity = match (lcr & 0x08 != 0, lcr & 0x10 != 0, lcr & 0x20 != 0) {
            (false, _, _) => Parity::None,
            (true, false, false) => Parity::Odd,
            (true, true, false) => Parity::Even,
            (true, false, true) => Parity::Mark,
            (true, true, true) => Parity::Space,
        };
        let stop = match (lcr & 0x04 != 0, bits) {
            (false, _) => Stop::One,
            (true, 5) => Stop::OneAndHalf,
            (true, _) => Stop::Two,
        };
        Self {
            bits,
            parity,
            stop,
            divisor,
        }
    }

    /// The crystal's cycles in half a bit: eight for each count of the
    /// divisor.
    fn half_cycles(self) -> u64 {
        8 * if self.divisor == 0 {
            0x1_0000
        } else {
            u64::from(self.divisor)
        }
    }

    /// How long a bit lasts.
    #[must_use]
    pub fn bit_time(self) -> Nanos {
        Hertz::UART.time_of(2 * self.half_cycles())
    }

    fn half_bit_time(self) -> Nanos {
        Hertz::UART.time_of(self.half_cycles())
    }

    /// How long a character lasts: its start bit, data, parity and stop
    /// bits.
    #[must_use]
    pub fn character_time(self) -> Nanos {
        let stop = match self.stop {
            Stop::One => 2,
            Stop::OneAndHalf => 3,
            Stop::Two => 4,
        };
        let halves = 2 * u64::from(self.stop_index()) + stop;
        Hertz::UART.time_of(halves * self.half_cycles())
    }

    /// The bit a receiver checks for the stop bit, counting the start bit
    /// as 0.
    fn stop_index(self) -> u8 {
        1 + self.bits + u8::from(self.parity != Parity::None)
    }

    /// The parity bit for `data`.
    fn parity_bit(self, data: u8) -> Option<bool> {
        let ones = (data & mask(self.bits)).count_ones();
        match self.parity {
            Parity::None => None,
            Parity::Odd => Some(ones.is_multiple_of(2)),
            Parity::Even => Some(!ones.is_multiple_of(2)),
            Parity::Mark => Some(true),
            Parity::Space => Some(false),
        }
    }

    /// The line's level `bit` bits into a character carrying `data`: the
    /// start bit, data least significant first, parity, then stop and idle.
    fn level(self, data: u8, bit: u64) -> bool {
        let bits = u64::from(self.bits);
        match bit {
            0 => false,
            b if b <= bits => data >> (b - 1) & 1 != 0,
            b => match (b == bits + 1, self.parity_bit(data)) {
                (true, Some(parity)) => parity,
                _ => true,
            },
        }
    }
}

const fn mask(bits: u8) -> u8 {
    if bits >= 8 {
        0xFF
    } else {
        (1 << bits) - 1
    }
}

/// A character on a line: when its start bit falls, and what it carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Frame {
    start: Nanos,
    data: u8,
    format: Format,
}

impl Frame {
    fn end(&self) -> Nanos {
        self.start + self.format.character_time()
    }

    /// The line's level at `t`: marking outside the frame.
    fn level(&self, t: Nanos) -> bool {
        if t < self.start || t >= self.end() {
            return true;
        }
        self.format
            .level(self.data, (t - self.start) / self.format.bit_time())
    }

    /// The first edge at or after `from` that takes the line low
    /// (`falling`) or high.
    fn edge(&self, from: Nanos, falling: bool) -> Option<Nanos> {
        let bit = self.format.bit_time();
        (0..=u64::from(self.format.stop_index())).find_map(|i| {
            let at = self.start + i * bit;
            let before = i == 0 || self.format.level(self.data, i - 1);
            let after = self.format.level(self.data, i);
            (at >= from && before != after && after != falling).then_some(at)
        })
    }
}

/// The modem control lines a UART drives.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Control {
    /// Data terminal ready.
    pub dtr: bool,
    /// Request to send.
    pub rts: bool,
}

/// The modem status lines a device drives.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "the four lines are independent signals"
)]
pub struct Status {
    /// Clear to send.
    pub cts: bool,
    /// Data set ready.
    pub dsr: bool,
    /// Ring indicator.
    pub ri: bool,
    /// Data carrier detect.
    pub dcd: bool,
}

impl Status {
    fn bits(self) -> u8 {
        u8::from(self.cts)
            | (u8::from(self.dsr) << 1)
            | (u8::from(self.ri) << 2)
            | (u8::from(self.dcd) << 3)
    }
}

/// What a machine's configuration plugs into a serial port.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Plug {
    /// Nothing: the status lines inactive.
    Nothing,
    /// The Microsoft serial mouse.
    Mouse,
}

/// What is plugged in, as it stands.
#[derive(Clone, Debug)]
enum Device {
    Nothing,
    Mouse(Mouse),
}

impl Device {
    fn status(&self) -> Status {
        match self {
            Self::Nothing => Status::default(),
            Self::Mouse(mouse) => mouse.status(),
        }
    }

    fn control(&mut self, control: Control, t: Nanos) {
        if let Self::Mouse(mouse) = self {
            mouse.control(control, t);
        }
    }

    fn next_send(&self) -> Option<Nanos> {
        match self {
            Self::Nothing => None,
            Self::Mouse(mouse) => mouse.next_send(),
        }
    }

    fn send(&mut self, t: Nanos) -> Option<(u8, Format)> {
        match self {
            Self::Nothing => None,
            Self::Mouse(mouse) => mouse.send(t),
        }
    }
}

/// The receiver's progress through a character.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rx {
    /// Waiting for the line to fall.
    Hunting,
    /// A start bit falls then.
    Edge(Nanos),
    /// Bit `index` is sampled at `at`, the start bit's check being 0, with
    /// the data and parity bit sampled so far.
    Sampling {
        at: Nanos,
        index: u8,
        data: u8,
        parity: bool,
    },
    /// After a break the line must mark, which it does then, before a start
    /// bit counts.
    Break(Nanos),
}

/// The 16450's scratch register, which the 8250 does not have.
const SCRATCH: u16 = 7;

/// The interrupt identification's sources, highest priority first.
mod source {
    pub const LINE_STATUS: u8 = 0x06;
    pub const RECEIVED: u8 = 0x04;
    pub const TRANSMIT: u8 = 0x02;
    pub const MODEM: u8 = 0x00;
}

/// The interrupt enable register's bits.
pub mod ier {
    /// A character has been received.
    pub const RECEIVED: u8 = 0x01;
    /// The transmitter holding register has emptied.
    pub const TRANSMIT: u8 = 0x02;
    /// The line status has an error or a break.
    pub const LINE_STATUS: u8 = 0x04;
    /// A modem status input has changed.
    pub const MODEM: u8 = 0x08;
    /// The bits the register holds.
    pub const ALL: u8 = 0x0F;
}

/// The line status register's bits.
pub mod lsr {
    /// A character waits in the receiver buffer.
    pub const DATA_READY: u8 = 0x01;
    /// A character arrived before the last was read.
    pub const OVERRUN: u8 = 0x02;
    /// A character's parity bit disagreed.
    pub const PARITY: u8 = 0x04;
    /// A character's stop bit was a space.
    pub const FRAMING: u8 = 0x08;
    /// The line was held at space for a whole character.
    pub const BREAK: u8 = 0x10;
    /// The four errors, which reading the register clears.
    pub const ERRORS: u8 = 0x1E;
    /// The transmitter holding register is empty.
    pub const THR_EMPTY: u8 = 0x20;
    /// The holding and shift registers are both empty.
    pub const EMPTY: u8 = 0x40;
}

/// The modem control register's bits.
pub mod mcr {
    /// Data terminal ready.
    pub const DTR: u8 = 0x01;
    /// Request to send.
    pub const RTS: u8 = 0x02;
    /// Output 1, which IBM's adapter leaves unconnected.
    pub const OUT1: u8 = 0x04;
    /// Output 2, which IBM's adapter gates the interrupt with.
    pub const OUT2: u8 = 0x08;
    /// Loopback.
    pub const LOOP: u8 = 0x10;
}

/// The modem status register's line bits.
pub mod msr {
    /// Clear to send.
    pub const CTS: u8 = 0x10;
    /// Data set ready.
    pub const DSR: u8 = 0x20;
}

/// A serial adapter: its UART and what is plugged into it.
#[derive(Clone, Debug)]
pub struct Uart {
    model: Model,
    rbr: u8,
    thr: Option<u8>,
    /// The character shifting out.
    shifting: Option<Frame>,
    /// The device's latest character.
    incoming: Option<Frame>,
    rx: Rx,
    ier: u8,
    lcr: u8,
    mcr: u8,
    lsr: u8,
    /// The modem status inputs last sensed, and their changes not yet read.
    seen: u8,
    deltas: u8,
    scratch: u8,
    divisor: u16,
    /// The transmitter-empty interrupt is asked for.
    transmit_pending: bool,
    device: Device,
}

impl Uart {
    /// A UART of `model` as reset leaves it, with `plug` on its line.
    #[must_use]
    pub fn new(model: Model, plug: Plug) -> Self {
        let device = match plug {
            Plug::Nothing => Device::Nothing,
            Plug::Mouse => Device::Mouse(Mouse::new()),
        };
        let seen = device.status().bits();
        Self {
            model,
            rbr: 0,
            thr: None,
            shifting: None,
            incoming: None,
            rx: Rx::Hunting,
            ier: 0,
            lcr: 0,
            mcr: 0,
            lsr: lsr::THR_EMPTY | lsr::EMPTY,
            seen,
            deltas: 0,
            scratch: 0,
            divisor: 0,
            transmit_pending: false,
            device,
        }
    }

    /// The mouse on the line moves by `dx`, `dy` with `buttons` held at
    /// `t`, `report` owing a packet whatever changed; returns whether a mouse
    /// is plugged in.
    pub fn mouse(&mut self, dx: i32, dy: i32, buttons: u8, report: bool, t: Nanos) -> bool {
        self.advance(t);
        match &mut self.device {
            Device::Mouse(mouse) => {
                mouse.motion(dx, dy, buttons, report, t);
                true
            }
            Device::Nothing => false,
        }
    }

    fn format(&self) -> Format {
        Format::from_lcr(self.lcr, self.divisor)
    }

    const fn looped(&self) -> bool {
        self.mcr & mcr::LOOP != 0
    }

    /// The modem lines the device sees: inactive while looped back.
    const fn control(&self) -> Control {
        let live = !self.looped();
        Control {
            dtr: live && self.mcr & mcr::DTR != 0,
            rts: live && self.mcr & mcr::RTS != 0,
        }
    }

    /// The modem status inputs: the device's, or the UART's own outputs
    /// while looped back.
    fn inputs(&self) -> u8 {
        if self.looped() {
            Status {
                cts: self.mcr & mcr::RTS != 0,
                dsr: self.mcr & mcr::DTR != 0,
                ri: self.mcr & mcr::OUT1 != 0,
                dcd: self.mcr & mcr::OUT2 != 0,
            }
            .bits()
        } else {
            self.device.status().bits()
        }
    }

    /// Takes the modem status inputs' changes into the deltas: any edge of
    /// CTS, DSR and DCD, and RI's trailing edge.
    fn sense(&mut self) {
        let (was, is) = (self.seen, self.inputs());
        self.deltas |= (was ^ is) & 0x0B;
        if was & 0x04 != 0 && is & 0x04 == 0 {
            self.deltas |= 0x04;
        }
        self.seen = is;
    }

    /// The highest-priority interrupt asked for.
    fn interrupt(&self) -> Option<u8> {
        if self.ier & ier::LINE_STATUS != 0 && self.lsr & lsr::ERRORS != 0 {
            Some(source::LINE_STATUS)
        } else if self.ier & ier::RECEIVED != 0 && self.lsr & lsr::DATA_READY != 0 {
            Some(source::RECEIVED)
        } else if self.ier & ier::TRANSMIT != 0 && self.transmit_pending {
            Some(source::TRANSMIT)
        } else if self.ier & ier::MODEM != 0 && self.deltas != 0 {
            Some(source::MODEM)
        } else {
            None
        }
    }

    /// The adapter's interrupt line: the UART's output, through OUT2's pin.
    #[must_use]
    pub fn irq(&self) -> bool {
        !self.looped() && self.mcr & mcr::OUT2 != 0 && self.interrupt().is_some()
    }

    /// When the UART's interrupts could next change on their own: a
    /// character finishing on either side.
    #[must_use]
    pub fn next_event(&self) -> Option<Nanos> {
        let format = self.format();
        let to_stop = |edge: Nanos| {
            edge + format.half_bit_time() + u64::from(format.stop_index()) * format.bit_time()
        };
        let received = match self.rx {
            Rx::Sampling { at, index, .. } => {
                let left = format.stop_index().saturating_sub(index);
                Some(at + u64::from(left) * format.bit_time())
            }
            Rx::Edge(at) => Some(to_stop(at)),
            Rx::Break(at) => Some(at),
            // A looped port's next character starts as the one shifting
            // ends, or when one is written.
            Rx::Hunting if self.looped() => None,
            Rx::Hunting => self.device.next_send().map(to_stop),
        };
        let shifted = self.shifting.map(|frame| frame.end());
        match (received, shifted) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// Moves the line on through `t`.
    pub fn advance(&mut self, t: Nanos) {
        loop {
            let shifted = self.shifting.map(|frame| frame.end()).filter(|&at| at <= t);
            let sent = self.device.next_send().filter(|&at| at <= t);
            let sampled = match self.rx {
                Rx::Hunting => None,
                Rx::Edge(at) | Rx::Break(at) | Rx::Sampling { at, .. } => Some(at),
            }
            .filter(|&at| at <= t);
            // The line's edges come before the samples of them at the same
            // instant.
            let Some(at) = [shifted, sent, sampled].into_iter().flatten().min() else {
                break;
            };
            if shifted == Some(at) {
                self.shifting = None;
                self.start_shifting(at);
            } else if sent == Some(at) {
                if let Some((data, format)) = self.device.send(at) {
                    self.incoming = Some(Frame {
                        start: at,
                        data,
                        format,
                    });
                    if !self.looped() {
                        self.frame_started(at);
                    }
                }
            } else {
                self.receive_step();
            }
        }
        self.sense();
    }

    /// The frame the receiver hears: the device's, or the transmitter's
    /// while looped back.
    fn source(&self) -> Option<&Frame> {
        if self.looped() {
            self.shifting.as_ref()
        } else {
            self.incoming.as_ref()
        }
    }

    fn line(&self, t: Nanos) -> bool {
        self.source().is_none_or(|frame| frame.level(t))
    }

    /// A frame starts at `at` on what the receiver hears.
    fn frame_started(&mut self, at: Nanos) {
        if self.rx == Rx::Hunting {
            self.rx = Rx::Edge(at);
        }
    }

    /// Looks for a start bit from `from`.
    fn hunt(&mut self, from: Nanos) {
        self.rx = self
            .source()
            .and_then(|frame| frame.edge(from, true))
            .map_or(Rx::Hunting, Rx::Edge);
    }

    /// Waits from `from` for the line to mark before hunting.
    fn await_mark(&mut self, from: Nanos) {
        if self.line(from) {
            self.hunt(from);
        } else {
            self.rx = self
                .source()
                .and_then(|frame| frame.edge(from, false))
                .map_or(Rx::Hunting, Rx::Break);
        }
    }

    fn receive_step(&mut self) {
        match self.rx {
            Rx::Hunting => {}
            Rx::Edge(at) => {
                self.rx = Rx::Sampling {
                    at: at + self.format().half_bit_time(),
                    index: 0,
                    data: 0,
                    parity: false,
                };
            }
            Rx::Break(at) => self.hunt(at),
            Rx::Sampling {
                at,
                index,
                data,
                parity,
            } => self.sample(at, index, data, parity),
        }
    }

    fn sample(&mut self, at: Nanos, index: u8, data: u8, parity: bool) {
        let format = self.format();
        let level = self.line(at);
        let next = |index, data, parity| Rx::Sampling {
            at: at + format.bit_time(),
            index,
            data,
            parity,
        };
        match index {
            // High half a bit after it fell: noise, not a start bit.
            0 if level => self.hunt(at),
            0 => self.rx = next(1, data, parity),
            i if i <= format.bits => {
                self.rx = next(i + 1, data | (u8::from(level) << (i - 1)), parity);
            }
            i if i < format.stop_index() => self.rx = next(i + 1, data, level),
            _ => self.complete(at, data, parity, level),
        }
    }

    /// A character's stop bit is sampled at `at`: it goes to the buffer
    /// register, overrunning one not yet read.
    fn complete(&mut self, at: Nanos, data: u8, parity: bool, stop: bool) {
        let format = self.format();
        let expected = format.parity_bit(data);
        let line_break = !stop && data == 0 && expected.is_none_or(|_| !parity);
        if self.lsr & lsr::DATA_READY != 0 {
            self.lsr |= lsr::OVERRUN;
        }
        self.rbr = data;
        self.lsr |= lsr::DATA_READY;
        if expected.is_some_and(|bit| bit != parity) {
            self.lsr |= lsr::PARITY;
        }
        if !stop {
            self.lsr |= lsr::FRAMING;
        }
        if line_break {
            self.lsr |= lsr::BREAK;
            self.await_mark(at);
        } else if !stop {
            // The low stop bit is taken for the next start bit, already
            // checked.
            self.rx = Rx::Sampling {
                at: at + format.bit_time(),
                index: 1,
                data: 0,
                parity: false,
            };
        } else {
            self.hunt(at);
        }
    }

    /// Moves the holding register into the shift register if it can.
    fn start_shifting(&mut self, at: Nanos) {
        if self.shifting.is_some() {
            return;
        }
        match self.thr.take() {
            Some(data) => {
                self.shifting = Some(Frame {
                    start: at,
                    data,
                    format: self.format(),
                });
                self.lsr |= lsr::THR_EMPTY;
                self.lsr &= !lsr::EMPTY;
                self.transmit_pending = true;
                if self.looped() {
                    self.frame_started(at);
                }
            }
            None => self.lsr |= lsr::EMPTY,
        }
    }

    const fn dlab(&self) -> bool {
        self.lcr & DLAB != 0
    }

    /// Reads register `offset` at `t`.
    pub fn input(&mut self, offset: u16, t: Nanos) -> u8 {
        self.advance(t);
        match offset {
            reg::DATA if self.dlab() => self.divisor.to_le_bytes()[0],
            reg::DATA => {
                self.lsr &= !lsr::DATA_READY;
                self.rbr
            }
            reg::IER if self.dlab() => self.divisor.to_le_bytes()[1],
            reg::IER => self.ier,
            reg::IIR => match self.interrupt() {
                Some(id) => {
                    if id == source::TRANSMIT {
                        self.transmit_pending = false;
                    }
                    id
                }
                None => 0x01,
            },
            reg::LCR => self.lcr,
            reg::MCR => self.mcr,
            reg::LSR => {
                let value = self.lsr;
                self.lsr &= !lsr::ERRORS;
                value
            }
            reg::MSR => {
                let value = (self.seen << 4) | self.deltas;
                self.deltas = 0;
                value
            }
            _ => match self.model {
                Model::Ns16450 => self.scratch,
                Model::Ins8250 => crate::cpu::OPEN_BUS,
            },
        }
    }

    /// Writes register `offset` at `t`.
    pub fn output(&mut self, offset: u16, value: u8, t: Nanos) {
        self.advance(t);
        match offset {
            reg::DATA if self.dlab() => self.divisor = (self.divisor & 0xFF00) | u16::from(value),
            reg::DATA => {
                self.thr = Some(value);
                self.lsr &= !(lsr::THR_EMPTY | lsr::EMPTY);
                self.transmit_pending = false;
                self.start_shifting(t);
            }
            reg::IER if self.dlab() => {
                self.divisor = (self.divisor & 0x00FF) | (u16::from(value) << 8);
            }
            reg::IER => {
                // Enabling the transmitter's interrupt with the holding
                // register empty asks for it at once.
                if value & ier::TRANSMIT != 0
                    && self.ier & ier::TRANSMIT == 0
                    && self.lsr & lsr::THR_EMPTY != 0
                {
                    self.transmit_pending = true;
                }
                self.ier = value & ier::ALL;
            }
            reg::LCR => self.lcr = value,
            reg::MCR => {
                let was_looped = self.looped();
                self.mcr = value & 0x1F;
                let control = self.control();
                self.device.control(control, t);
                if was_looped != self.looped() {
                    self.relisten(t);
                }
                self.sense();
            }
            SCRATCH if self.model == Model::Ns16450 => self.scratch = value,
            _ => {}
        }
    }

    /// Loopback changed what the receiver hears: it looks again from `t`,
    /// keeping a character it is already sampling.
    fn relisten(&mut self, t: Nanos) {
        match self.rx {
            Rx::Hunting | Rx::Edge(_) => self.hunt(t),
            Rx::Break(_) => self.await_mark(t),
            Rx::Sampling { .. } => {}
        }
    }
}

crate::snapshot::state_enum!(Parity {
    0 => None,
    1 => Odd,
    2 => Even,
    3 => Mark,
    4 => Space,
});

crate::snapshot::state_enum!(Stop {
    0 => One,
    1 => OneAndHalf,
    2 => Two,
});

impl State for Format {
    fn save(&self, out: &mut Out) {
        let Self {
            bits,
            parity,
            stop,
            divisor,
        } = self;
        bits.save(out);
        parity.save(out);
        stop.save(out);
        divisor.save(out);
    }

    fn restore(&mut self, input: &mut In<'_>) -> Result<(), Torn> {
        self.bits.restore(input)?;
        self.parity.restore(input)?;
        self.stop.restore(input)?;
        self.divisor.restore(input)?;
        crate::snapshot::check((5..=8).contains(&self.bits))
    }
}

impl Default for Frame {
    fn default() -> Self {
        Self {
            start: 0,
            data: 0,
            format: Format::from_lcr(0, 0),
        }
    }
}

crate::snapshot::state!(Frame {
    start,
    data,
    format,
});

crate::snapshot::state_enum!(Rx {
    0 => Hunting,
    1 => Edge(at),
    2 => Sampling { at, index, data, parity },
    3 => Break(at),
});

impl State for Device {
    fn save(&self, out: &mut Out) {
        match self {
            Self::Nothing => out.u8(0),
            Self::Mouse(mouse) => {
                out.u8(1);
                mouse.save(out);
            }
        }
    }

    fn restore(&mut self, input: &mut In<'_>) -> Result<(), Torn> {
        match (input.u8()?, self) {
            (0, Self::Nothing) => Ok(()),
            (1, Self::Mouse(mouse)) => mouse.restore(input),
            (0 | 1, _) => Err(Torn::Mismatch),
            _ => Err(Torn::Malformed),
        }
    }
}

crate::snapshot::state!(
    Uart {
        rbr,
        thr,
        shifting,
        incoming,
        rx,
        ier,
        lcr,
        mcr,
        lsr,
        seen,
        deltas,
        scratch,
        divisor,
        transmit_pending,
        device,
    }
    as built { model }
);

#[cfg(test)]
#[path = "serial_tests.rs"]
mod tests;
