//! The 6522 versatile interface adapter.
//!
//! The chip is clocked at 1 MHz; the machine addresses it in 2 MHz cycles. A
//! 1 MHz tick `t` spans cycles `2t` and `2t + 1`: the CPU's access to the chip
//! always lands on the odd cycle (the bus stretches it there), and the timers'
//! interrupt flags rise on the even one, half a tick ahead of any access that
//! could read them, which is where the silicon raises them.
//!
//! Nothing is stepped per tick. Each timer is an anchor (a counter value at a
//! tick) and its latch, from which the counter at any tick, the underflows
//! between two ticks and the next interrupt are closed forms; so a timer
//! free-running at its fastest costs nothing until something observes it.

use super::{tick, tick_start};

/// Register offsets within the chip's sixteen.
pub mod reg {
    /// Output/input register B.
    pub const ORB: u8 = 0x0;
    /// Output/input register A, with handshaking.
    pub const ORA: u8 = 0x1;
    /// Data direction B.
    pub const DDRB: u8 = 0x2;
    /// Data direction A.
    pub const DDRA: u8 = 0x3;
    /// Timer 1 counter low; writes the latch.
    pub const T1C_L: u8 = 0x4;
    /// Timer 1 counter high; a write starts the timer.
    pub const T1C_H: u8 = 0x5;
    /// Timer 1 latch low.
    pub const T1L_L: u8 = 0x6;
    /// Timer 1 latch high.
    pub const T1L_H: u8 = 0x7;
    /// Timer 2 counter low; writes the latch.
    pub const T2C_L: u8 = 0x8;
    /// Timer 2 counter high; a write starts the timer.
    pub const T2C_H: u8 = 0x9;
    /// Shift register.
    pub const SR: u8 = 0xA;
    /// Auxiliary control.
    pub const ACR: u8 = 0xB;
    /// Peripheral control.
    pub const PCR: u8 = 0xC;
    /// Interrupt flags.
    pub const IFR: u8 = 0xD;
    /// Interrupt enables.
    pub const IER: u8 = 0xE;
    /// Output/input register A, without handshaking.
    pub const ORA_NH: u8 = 0xF;
}

/// Interrupt flag and enable bits.
pub mod irq {
    /// CA2's active edge.
    pub const CA2: u8 = 0x01;
    /// CA1's active edge.
    pub const CA1: u8 = 0x02;
    /// Eight bits shifted.
    pub const SR: u8 = 0x04;
    /// CB2's active edge.
    pub const CB2: u8 = 0x08;
    /// CB1's active edge.
    pub const CB1: u8 = 0x10;
    /// Timer 2 underflow.
    pub const T2: u8 = 0x20;
    /// Timer 1 underflow.
    pub const T1: u8 = 0x40;
    /// Any enabled flag, as IFR reads it; the "set" bit of an IER write.
    pub const ANY: u8 = 0x80;
}

/// ACR: timer 1 free-runs rather than firing once.
const ACR_T1_CONTINUOUS: u8 = 0x40;
/// ACR: timer 1 drives PB7.
const ACR_T1_PB7: u8 = 0x80;
/// ACR: timer 2 counts PB6 pulses rather than ticks.
const ACR_T2_PULSES: u8 = 0x20;
/// ACR: port A input is latched on CA1's active edge.
const ACR_LATCH_A: u8 = 0x01;
/// ACR: port B input is latched on CB1's active edge.
const ACR_LATCH_B: u8 = 0x02;
/// PB7, timer 1's output pin.
const PB7: u8 = 0x80;
/// PB6, timer 2's pulse input.
const PB6: u8 = 0x40;
/// Cycles a pulse-mode CA2 or CB2 output stays low: one 1 MHz cycle.
const PULSE_CYCLES: u64 = 2;

/// What one of CA2 or CB2 does, from its three PCR bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Control2 {
    /// An interrupt input on the given edge (`true`: positive); `independent`
    /// flags are not cleared by accessing the port.
    Input { positive: bool, independent: bool },
    /// Goes low on a port access, back high on the port's control-1 edge.
    Handshake,
    /// Goes low for one cycle on a port access.
    Pulse,
    /// Held at a level.
    Manual(bool),
}

impl Control2 {
    const fn from_bits(bits: u8) -> Self {
        match bits & 0x07 {
            0..=3 => Self::Input {
                positive: bits & 0x02 != 0,
                independent: bits & 0x01 != 0,
            },
            4 => Self::Handshake,
            5 => Self::Pulse,
            6 => Self::Manual(false),
            _ => Self::Manual(true),
        }
    }
}

/// Ticks from `anchor` to `t`, negative before it.
const fn since(t: u64, anchor: u64) -> i64 {
    t.wrapping_sub(anchor).cast_signed()
}

/// The low sixteen bits of a tick count.
const fn low16(ticks: u64) -> u16 {
    let bytes = ticks.to_le_bytes();
    u16::from_le_bytes([bytes[0], bytes[1]])
}

/// Timer 1: a counter reloading from its latch at every underflow.
///
/// The data sheet's prose has a one-shot counter carry on decrementing past
/// zero; the silicon reloads from the latch in both modes, as BBC software
/// timing the counter observes, and the one-shot mode only stops further
/// interrupts and ends the PB7 pulse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Timer1 {
    latch: u16,
    /// The tick at which the counter held `value`.
    anchor: u64,
    /// The counter at `anchor`, `-1` standing for the tick it reads `&FFFF`
    /// before reloading.
    value: i32,
    /// A one-shot timer that has not yet timed out since it was started.
    armed: bool,
    /// The level the timer drives PB7 to, at `anchor`.
    pb7: bool,
}

impl Timer1 {
    /// The counter within its period at tick `t`: `0..=latch`, `-1` on an
    /// underflow tick, and above `value` only before `anchor`.
    fn remaining(&self, t: u64) -> i64 {
        let first = i64::from(self.value) - since(t, self.anchor);
        if first >= -1 {
            return first;
        }
        let period = i64::from(self.latch) + 2;
        i64::from(self.latch) - (-first - 2) % period
    }

    fn counter(&self, t: u64) -> u16 {
        match self.remaining(t) {
            -1 => 0xFFFF,
            n => low16(n.cast_unsigned()),
        }
    }

    /// The first underflow tick after `t`.
    fn next_underflow(&self, t: u64) -> u64 {
        let r = self.remaining(t);
        let ahead = if r >= 0 {
            r + 1
        } else {
            i64::from(self.latch) + 2
        };
        t.wrapping_add(ahead.cast_unsigned())
    }

    /// How many underflows fall in `(from, to]`.
    fn underflows(&self, from: u64, to: u64) -> u64 {
        if to <= from {
            return 0;
        }
        let first = self.next_underflow(from);
        if first > to {
            0
        } else {
            1 + (to - first) / (u64::from(self.latch) + 2)
        }
    }

    /// PB7's level at tick `t`.
    fn pb7(&self, t: u64, continuous: bool) -> bool {
        let flips = self.underflows(self.anchor, t);
        if continuous {
            self.pb7 ^ (flips % 2 == 1)
        } else {
            self.pb7 || flips > 0
        }
    }

    /// Re-anchor at tick `t`, so a latch written now governs the next reload.
    fn rebase(&mut self, t: u64, continuous: bool) {
        self.pb7 = self.pb7(t, continuous);
        self.value = i32::try_from(self.remaining(t)).unwrap_or(-1);
        self.anchor = t;
    }
}

/// Timer 2: a one-shot interval timer or a PB6 pulse counter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Timer2 {
    latch_low: u8,
    /// Counting ticks, the counter held `value` at `anchor`; counting pulses,
    /// the counter is `value`.
    anchor: u64,
    value: u16,
    armed: bool,
}

impl Timer2 {
    fn counter(&self, t: u64, counting_pulses: bool) -> u16 {
        if counting_pulses {
            self.value
        } else {
            self.value.wrapping_sub(low16(t.wrapping_sub(self.anchor)))
        }
    }

    /// The first tick after `t` at which a tick-counting timer passes from 0
    /// to `&FFFF`.
    fn next_underflow(&self, t: u64) -> u64 {
        let first = self.anchor.wrapping_add(u64::from(self.value) + 1);
        if first > t {
            first
        } else {
            first + ((t - first) / 0x1_0000 + 1) * 0x1_0000
        }
    }
}

/// The shift register and its modulo-8 counter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Shifter {
    value: u8,
    /// Shifts left in the current byte of a counted mode.
    remaining: u8,
    /// The tick of the next internally clocked shift, if one is due.
    next: Option<u64>,
    /// CB2's level when the register drives it.
    cb2: bool,
}

/// The chip.
#[derive(Clone, Debug, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each is an independent control line"
)]
pub struct Via {
    ora: u8,
    orb: u8,
    ddra: u8,
    ddrb: u8,
    /// Port A as latched on CA1, when latching is enabled.
    ira_latch: u8,
    /// Port B as latched on CB1, when latching is enabled.
    irb_latch: u8,
    /// The levels the outside world drives onto the port pins.
    pins_a: u8,
    pins_b: u8,
    acr: u8,
    pcr: u8,
    ifr: u8,
    ier: u8,
    t1: Timer1,
    t2: Timer2,
    sr: Shifter,
    /// Input levels on the control lines.
    ca1: bool,
    ca2_in: bool,
    cb1: bool,
    cb2_in: bool,
    /// Output levels of CA2 and CB2 in their output modes.
    ca2_out: bool,
    cb2_out: bool,
    /// The cycle a pulse-mode CA2/CB2 output returns high.
    ca2_pulse_end: Option<u64>,
    cb2_pulse_end: Option<u64>,
    /// The tick the timers and shifter have been brought up to.
    now: u64,
}

impl Default for Via {
    fn default() -> Self {
        Self::new()
    }
}

impl Via {
    /// A chip at power-on: every register clear, both ports inputs, the
    /// timers counting from `&FFFF` with their interrupts disarmed.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            ora: 0,
            orb: 0,
            ddra: 0,
            ddrb: 0,
            ira_latch: 0,
            irb_latch: 0,
            pins_a: 0xFF,
            pins_b: 0xFF,
            acr: 0,
            pcr: 0,
            ifr: 0,
            ier: 0,
            t1: Timer1 {
                latch: 0xFFFF,
                anchor: 0,
                value: 0xFFFF,
                armed: false,
                pb7: true,
            },
            t2: Timer2 {
                latch_low: 0xFF,
                anchor: 0,
                value: 0xFFFF,
                armed: false,
            },
            sr: Shifter {
                value: 0,
                remaining: 0,
                next: None,
                cb2: true,
            },
            ca1: true,
            ca2_in: true,
            cb1: true,
            cb2_in: true,
            ca2_out: true,
            cb2_out: true,
            ca2_pulse_end: None,
            cb2_pulse_end: None,
            now: 0,
        }
    }

    /// The RES pin: the control and data registers clear; the timers'
    /// counters and latches and the shift register's contents are kept.
    pub fn reset(&mut self, cycle: u64) {
        self.advance(cycle);
        // The ACR clears too, so each timer counts on from where it stands.
        self.t1.rebase(self.now, self.t1_continuous());
        self.t2.value = self.t2.counter(self.now, self.t2_counts_pulses());
        self.t2.anchor = self.now;
        *self = Self {
            t1: Timer1 {
                armed: false,
                pb7: true,
                ..self.t1
            },
            t2: Timer2 {
                armed: false,
                ..self.t2
            },
            sr: Shifter {
                remaining: 0,
                next: None,
                ..self.sr
            },
            pins_a: self.pins_a,
            pins_b: self.pins_b,
            ca1: self.ca1,
            ca2_in: self.ca2_in,
            cb1: self.cb1,
            cb2_in: self.cb2_in,
            now: self.now,
            ..Self::new()
        };
    }

    /// Whether the chip asserts IRQ.
    #[must_use]
    pub const fn irq(&self) -> bool {
        self.ifr & self.ier & 0x7F != 0
    }

    /// The interrupt flags and enables, seen without the side effects a
    /// register read has.
    #[must_use]
    pub const fn interrupts(&self) -> (u8, u8) {
        (self.ifr, self.ier)
    }

    /// The levels on port A's pins: outputs where DDRA says so, the outside
    /// world's elsewhere.
    #[must_use]
    pub const fn port_a(&self) -> u8 {
        (self.ora & self.ddra) | (self.pins_a & !self.ddra)
    }

    /// The levels on port B's pins, PB7 following timer 1 when it drives it.
    #[must_use]
    pub fn port_b(&self) -> u8 {
        let levels = (self.orb & self.ddrb) | (self.pins_b & !self.ddrb);
        self.with_timer_pb7(levels)
    }

    /// CA2's level, driven by the chip in its output modes.
    #[must_use]
    pub const fn ca2(&self) -> bool {
        match Control2::from_bits(self.pcr >> 1) {
            Control2::Input { .. } => self.ca2_in,
            _ => self.ca2_out,
        }
    }

    /// The level the outside world last drove onto CA2.
    #[must_use]
    pub const fn ca2_input(&self) -> bool {
        self.ca2_in
    }

    /// CB2's level, driven by the chip in its output and shift-out modes.
    #[must_use]
    pub const fn cb2(&self) -> bool {
        if self.shift_mode() >= 4 {
            return self.sr.cb2;
        }
        match Control2::from_bits(self.pcr >> 5) {
            Control2::Input { .. } => self.cb2_in,
            _ => self.cb2_out,
        }
    }

    /// Set the levels the outside world drives onto port A's pins.
    pub fn set_port_a_pins(&mut self, levels: u8) {
        self.pins_a = levels;
    }

    /// Set the levels the outside world drives onto port B's pins; a falling
    /// edge on PB6 counts a pulse for timer 2.
    pub fn set_port_b_pins(&mut self, cycle: u64, levels: u8) {
        self.advance(cycle);
        let pb6_fell = self.pins_b & PB6 != 0 && levels & PB6 == 0;
        self.pins_b = levels;
        if pb6_fell && self.acr & ACR_T2_PULSES != 0 {
            let (value, passed_zero) = self.t2.value.overflowing_sub(1);
            self.t2.value = value;
            if passed_zero && self.t2.armed {
                self.t2.armed = false;
                self.ifr |= irq::T2;
            }
        }
    }

    /// Drive CA1.
    pub fn set_ca1(&mut self, cycle: u64, level: bool) {
        self.advance(cycle);
        let active = edge(self.ca1, level, self.pcr & 0x01 != 0);
        self.ca1 = level;
        if !active {
            return;
        }
        self.ifr |= irq::CA1;
        if self.acr & ACR_LATCH_A != 0 {
            self.ira_latch = self.port_a();
        }
        if Control2::from_bits(self.pcr >> 1) == Control2::Handshake {
            self.ca2_out = true;
        }
    }

    /// Drive CA2, which is an input in CA2's input modes.
    pub fn set_ca2(&mut self, cycle: u64, level: bool) {
        self.advance(cycle);
        if let Control2::Input { positive, .. } = Control2::from_bits(self.pcr >> 1) {
            if edge(self.ca2_in, level, positive) {
                self.ifr |= irq::CA2;
            }
        }
        self.ca2_in = level;
    }

    /// The level CA2's active edge takes it to, while that edge would raise
    /// its flag: `None` while CA2 is an output or its flag is already up,
    /// when no edge on it changes anything.
    #[must_use]
    pub const fn ca2_edge_awaited(&self) -> Option<bool> {
        match Control2::from_bits(self.pcr >> 1) {
            Control2::Input { positive, .. } if self.ifr & irq::CA2 == 0 => Some(positive),
            _ => None,
        }
    }

    /// Take CA2's level with no edge: the line moved while no edge on it
    /// could change anything.
    pub const fn sync_ca2(&mut self, level: bool) {
        self.ca2_in = level;
    }

    /// Drive CB1. Its rising edge shifts the register when shifting is
    /// disabled or externally clocked.
    pub fn set_cb1(&mut self, cycle: u64, level: bool) {
        self.advance(cycle);
        let rising = !self.cb1 && level;
        let active = edge(self.cb1, level, self.pcr & 0x10 != 0);
        self.cb1 = level;
        if rising && matches!(self.shift_mode(), 0 | 3 | 7) {
            self.shift_bit();
            if self.shift_mode() != 0 {
                self.sr.remaining = self.sr.remaining.saturating_sub(1);
                if self.sr.remaining == 0 {
                    self.ifr |= irq::SR;
                    self.sr.remaining = 8;
                }
            }
        }
        if !active {
            return;
        }
        self.ifr |= irq::CB1;
        if self.acr & ACR_LATCH_B != 0 {
            self.irb_latch = self.port_b();
        }
        if Control2::from_bits(self.pcr >> 5) == Control2::Handshake {
            self.cb2_out = true;
        }
    }

    /// Drive CB2, which is an input in CB2's input modes and shifted in by
    /// the shift-in modes.
    pub fn set_cb2(&mut self, cycle: u64, level: bool) {
        self.advance(cycle);
        if let Control2::Input { positive, .. } = Control2::from_bits(self.pcr >> 5) {
            if edge(self.cb2_in, level, positive) {
                self.ifr |= irq::CB2;
            }
        }
        self.cb2_in = level;
    }

    /// Bring the timers and the shift register up to `cycle`.
    pub fn advance(&mut self, cycle: u64) {
        self.end_pulses(cycle);
        // A flag raised at tick `u` is visible from cycle `2u`, so by any cycle
        // every tick up to `tick(cycle)` has had its effect.
        let to = tick(cycle);
        let from = self.now;
        if to <= from {
            return;
        }
        if self.t1_continuous() {
            if self.t1.underflows(from, to) > 0 {
                self.ifr |= irq::T1;
            }
        } else if self.t1.armed && self.t1.next_underflow(from) <= to {
            self.t1.armed = false;
            self.ifr |= irq::T1;
        }
        if !self.t2_counts_pulses() && self.t2.armed && self.t2.next_underflow(from) <= to {
            self.t2.armed = false;
            self.ifr |= irq::T2;
        }
        self.run_shifter(to);
        self.now = to;
    }

    /// The next cycle at which the chip's IRQ output could change with no
    /// access to it, if any.
    #[must_use]
    pub fn next_event(&self) -> Option<u64> {
        let pending = self.ier & !self.ifr;
        let t1 = (pending & irq::T1 != 0 && (self.t1_continuous() || self.t1.armed))
            .then(|| tick_start(self.t1.next_underflow(self.now)));
        let t2 = (pending & irq::T2 != 0 && self.t2.armed && !self.t2_counts_pulses())
            .then(|| tick_start(self.t2.next_underflow(self.now)));
        let sr = (pending & irq::SR != 0)
            .then(|| self.shift_completion())
            .flatten()
            .map(tick_start);
        [t1, t2, sr].into_iter().flatten().min()
    }

    /// The CPU reads register `register` (0–15) in `cycle`.
    pub fn read(&mut self, cycle: u64, register: u8) -> u8 {
        self.advance(cycle);
        let t = tick(cycle);
        match register & 0x0F {
            reg::ORB => {
                self.clear_port_flags_b();
                let input = if self.acr & ACR_LATCH_B != 0 {
                    self.irb_latch
                } else {
                    self.pins_b
                };
                // IRB reads ORB back for output pins, whatever loads them.
                self.with_timer_pb7((self.orb & self.ddrb) | (input & !self.ddrb))
            }
            reg::ORA => {
                self.clear_port_flags_a();
                self.handshake_a(cycle);
                self.input_a()
            }
            reg::DDRB => self.ddrb,
            reg::DDRA => self.ddra,
            reg::T1C_L => {
                self.ifr &= !irq::T1;
                self.t1.counter(t).to_le_bytes()[0]
            }
            reg::T1C_H => self.t1.counter(t).to_le_bytes()[1],
            reg::T1L_L => self.t1.latch.to_le_bytes()[0],
            reg::T1L_H => self.t1.latch.to_le_bytes()[1],
            reg::T2C_L => {
                self.ifr &= !irq::T2;
                self.t2.counter(t, self.t2_counts_pulses()).to_le_bytes()[0]
            }
            reg::T2C_H => self.t2.counter(t, self.t2_counts_pulses()).to_le_bytes()[1],
            reg::SR => {
                self.restart_shifter(t);
                self.sr.value
            }
            reg::ACR => self.acr,
            reg::PCR => self.pcr,
            reg::IFR => self.ifr | if self.irq() { irq::ANY } else { 0 },
            reg::IER => self.ier | irq::ANY,
            _ => self.input_a(),
        }
    }

    /// The CPU writes `value` to register `register` (0–15) in `cycle`.
    pub fn write(&mut self, cycle: u64, register: u8, value: u8) {
        self.advance(cycle);
        let t = tick(cycle);
        match register & 0x0F {
            reg::ORB => {
                self.orb = value;
                self.clear_port_flags_b();
                match Control2::from_bits(self.pcr >> 5) {
                    Control2::Handshake => self.cb2_out = false,
                    Control2::Pulse => {
                        self.cb2_out = false;
                        self.cb2_pulse_end = Some(cycle + PULSE_CYCLES);
                    }
                    _ => {}
                }
            }
            reg::ORA => {
                self.ora = value;
                self.clear_port_flags_a();
                self.handshake_a(cycle);
            }
            reg::DDRB => self.ddrb = value,
            reg::DDRA => self.ddra = value,
            reg::T1C_L | reg::T1L_L => {
                self.t1.rebase(t, self.t1_continuous());
                self.t1.latch = (self.t1.latch & 0xFF00) | u16::from(value);
            }
            reg::T1L_H => {
                self.t1.rebase(t, self.t1_continuous());
                self.t1.latch = (self.t1.latch & 0x00FF) | (u16::from(value) << 8);
                self.ifr &= !irq::T1;
            }
            reg::T1C_H => {
                self.t1.latch = (self.t1.latch & 0x00FF) | (u16::from(value) << 8);
                self.t1.anchor = t + 1;
                self.t1.value = i32::from(self.t1.latch);
                self.t1.armed = true;
                self.t1.pb7 = false;
                self.ifr &= !irq::T1;
            }
            reg::T2C_L => self.t2.latch_low = value,
            reg::T2C_H => {
                self.t2.value = u16::from_le_bytes([self.t2.latch_low, value]);
                self.t2.anchor = t + 1;
                self.t2.armed = true;
                self.ifr &= !irq::T2;
            }
            reg::SR => {
                self.sr.value = value;
                self.restart_shifter(t);
            }
            reg::ACR => {
                self.t1.rebase(t, self.t1_continuous());
                self.t2.value = self.t2.counter(t, self.t2_counts_pulses());
                self.t2.anchor = t;
                self.acr = value;
                if self.shift_period().is_none() {
                    self.sr.next = None;
                }
            }
            reg::PCR => {
                self.pcr = value;
                if let Control2::Manual(level) = Control2::from_bits(value >> 1) {
                    self.ca2_out = level;
                }
                if let Control2::Manual(level) = Control2::from_bits(value >> 5) {
                    self.cb2_out = level;
                }
            }
            reg::IFR => self.ifr &= !(value & 0x7F),
            reg::IER => {
                if value & irq::ANY != 0 {
                    self.ier |= value & 0x7F;
                } else {
                    self.ier &= !value;
                }
            }
            _ => self.ora = value,
        }
    }

    const fn t1_continuous(&self) -> bool {
        self.acr & ACR_T1_CONTINUOUS != 0
    }

    const fn t2_counts_pulses(&self) -> bool {
        self.acr & ACR_T2_PULSES != 0
    }

    fn with_timer_pb7(&self, levels: u8) -> u8 {
        if self.acr & ACR_T1_PB7 == 0 {
            return levels;
        }
        let pb7 = self.t1.pb7(self.now, self.t1_continuous());
        (levels & !PB7) | if pb7 { PB7 } else { 0 }
    }

    const fn input_a(&self) -> u8 {
        if self.acr & ACR_LATCH_A != 0 {
            self.ira_latch
        } else {
            // IRA reads the pins themselves, outputs included.
            self.port_a()
        }
    }

    fn clear_port_flags_a(&mut self) {
        self.ifr &= !irq::CA1;
        if let Control2::Input {
            independent: false, ..
        } = Control2::from_bits(self.pcr >> 1)
        {
            self.ifr &= !irq::CA2;
        }
    }

    fn clear_port_flags_b(&mut self) {
        self.ifr &= !irq::CB1;
        if let Control2::Input {
            independent: false, ..
        } = Control2::from_bits(self.pcr >> 5)
        {
            self.ifr &= !irq::CB2;
        }
    }

    fn handshake_a(&mut self, cycle: u64) {
        match Control2::from_bits(self.pcr >> 1) {
            Control2::Handshake => self.ca2_out = false,
            Control2::Pulse => {
                self.ca2_out = false;
                self.ca2_pulse_end = Some(cycle + PULSE_CYCLES);
            }
            _ => {}
        }
    }

    fn end_pulses(&mut self, cycle: u64) {
        if self.ca2_pulse_end.is_some_and(|end| cycle >= end) {
            self.ca2_pulse_end = None;
            self.ca2_out = true;
        }
        if self.cb2_pulse_end.is_some_and(|end| cycle >= end) {
            self.cb2_pulse_end = None;
            self.cb2_out = true;
        }
    }

    const fn shift_mode(&self) -> u8 {
        (self.acr >> 2) & 0x07
    }

    /// Ticks between internally clocked shifts: a timer 2 clock changes level
    /// every `N + 2` ticks of its low latch and shifts once per full period;
    /// the system clock shifts once a tick.
    fn shift_period(&self) -> Option<u64> {
        match self.shift_mode() {
            1 | 4 | 5 => Some(timer_2_shift_period(self.t2.latch_low)),
            2 | 6 => Some(1),
            _ => None,
        }
    }

    /// The tick the current byte's last internally clocked shift lands on, in
    /// a mode that counts.
    fn shift_completion(&self) -> Option<u64> {
        let period = self.shift_period()?;
        let next = self.sr.next?;
        (self.shift_mode() != 4)
            .then(|| next + u64::from(self.sr.remaining.saturating_sub(1)) * period)
    }

    /// An access to the shift register clears its flag and starts a byte.
    fn restart_shifter(&mut self, t: u64) {
        self.ifr &= !irq::SR;
        self.sr.remaining = 8;
        self.sr.next = self.shift_period().map(|period| t + period);
    }

    /// Run the internally clocked shifts due by tick `t`.
    fn run_shifter(&mut self, t: u64) {
        let (Some(period), Some(next)) = (self.shift_period(), self.sr.next) else {
            return;
        };
        if next > t {
            return;
        }
        let due = (t - next) / period + 1;
        if self.shift_mode() == 4 {
            // Recirculating, the register repeats every eight shifts.
            for _ in 0..due % 8 {
                self.shift_bit();
            }
            self.sr.cb2 = self.sr.value & 0x01 != 0;
            self.sr.next = Some(next + due * period);
            return;
        }
        let shifts = due.min(u64::from(self.sr.remaining));
        for _ in 0..shifts {
            self.shift_bit();
        }
        self.sr.remaining -= u8::try_from(shifts).unwrap_or(self.sr.remaining);
        if self.sr.remaining == 0 {
            self.ifr |= irq::SR;
            self.sr.next = None;
        } else {
            self.sr.next = Some(next + shifts * period);
        }
    }

    /// Shift one bit: out of bit 7 onto CB2, recirculating, in the shift-out
    /// modes; in from CB2 at bit 0 otherwise.
    fn shift_bit(&mut self) {
        let msb = self.sr.value & 0x80 != 0;
        if self.shift_mode() >= 4 {
            self.sr.cb2 = msb;
            self.sr.value = (self.sr.value << 1) | u8::from(msb);
        } else {
            self.sr.value = (self.sr.value << 1) | u8::from(self.cb2_in);
        }
    }
}

/// Ticks between shifts clocked by timer 2 with `latch_low` in its low latch.
fn timer_2_shift_period(latch_low: u8) -> u64 {
    2 * (u64::from(latch_low) + 2)
}

crate::snapshot::fields!(
    Timer1 {
        latch,
        anchor,
        value,
        armed,
        pb7
    },
    check | t | (-1..=0x1_0000).contains(&t.value)
);
crate::snapshot::fields!(Timer2 {
    latch_low,
    anchor,
    value,
    armed
});
crate::snapshot::fields!(
    Shifter {
        value,
        remaining,
        next,
        cb2
    },
    check | s | s.remaining <= 8
);
crate::snapshot::fields!(
    Via {
        ora,
        orb,
        ddra,
        ddrb,
        ira_latch,
        irb_latch,
        pins_a,
        pins_b,
        acr,
        pcr,
        ifr,
        ier,
        t1,
        t2,
        sr,
        ca1,
        ca2_in,
        cb1,
        cb2_in,
        ca2_out,
        cb2_out,
        ca2_pulse_end,
        cb2_pulse_end,
        now,
    },
    check | via,
    r | via.restorable(r)
);

impl Via {
    /// Whether a restored chip holds only what one can: brought up to the
    /// machine's time, as a snapshot leaves it; flags and enables in their
    /// seven bits; its timers anchored no later than the next tick; the next
    /// internal shift within one period and pulses within their width of the
    /// machine's time.
    fn restorable(&self, r: &crate::snapshot::Reader<'_>) -> bool {
        let pulse = |end: Option<u64>| end.is_none_or(|end| end <= r.clock() + PULSE_CYCLES);
        // Its time is checked before anything is reckoned from it.
        self.now == tick(r.clock())
            && (self.ifr | self.ier) & irq::ANY == 0
            && self.t1.anchor <= self.now + 1
            && self.t2.anchor <= self.now + 1
            && self.sr.next.is_none_or(|next| {
                self.shift_period().is_some()
                    && next > self.now
                    && next <= self.now + timer_2_shift_period(u8::MAX)
            })
            && pulse(self.ca2_pulse_end)
            && pulse(self.cb2_pulse_end)
    }
}

/// Whether a line moving from `was` to `now` makes its active edge.
const fn edge(was: bool, now: bool, positive: bool) -> bool {
    if positive {
        !was && now
    } else {
        was && !now
    }
}

#[cfg(test)]
#[path = "via_tests.rs"]
mod tests;
