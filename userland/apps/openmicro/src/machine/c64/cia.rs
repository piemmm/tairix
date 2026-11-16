//! The 6526 complex interface adapter, and the 8521 that replaced it: two
//! ports, two timers, a time-of-day clock and a serial register, each cause
//! able to raise the chip's interrupt line.
//!
//! Each cycle has work done before the CPU's access in it — the timers step,
//! and an underflow raises its flag — and work done after: the serial
//! register, the time-of-day clock, and the interrupt pipeline's cycle, which
//! an access to the interrupt register runs ahead of itself. The pipeline is
//! where the two chips differ: the 6526 raises its line a cycle later, and a
//! read of its flags just before timer B's underflow loses that flag.
//!
//! A timer counting steadily, or stopped, is not stepped: the chip runs a
//! cycle at a time only around the moments something changes.

use crate::snapshot;

/// Registers, repeated through the chip's page.
pub mod reg {
    /// Port A.
    pub const PRA: u8 = 0x0;
    /// Port B.
    pub const PRB: u8 = 0x1;
    /// Port A's directions, set for an output.
    pub const DDRA: u8 = 0x2;
    /// Port B's directions.
    pub const DDRB: u8 = 0x3;
    /// Timer A, low byte: the counter read, the latch written.
    pub const TA_LO: u8 = 0x4;
    /// Timer A, high byte.
    pub const TA_HI: u8 = 0x5;
    /// Timer B, low byte.
    pub const TB_LO: u8 = 0x6;
    /// Timer B, high byte.
    pub const TB_HI: u8 = 0x7;
    /// The time of day's tenths of a second.
    pub const TOD_TENTHS: u8 = 0x8;
    /// Its seconds, in BCD.
    pub const TOD_SECONDS: u8 = 0x9;
    /// Its minutes, in BCD.
    pub const TOD_MINUTES: u8 = 0xA;
    /// Its hours, 1 to 12 in BCD, bit 7 set after noon.
    pub const TOD_HOURS: u8 = 0xB;
    /// The serial data register.
    pub const SDR: u8 = 0xC;
    /// Interrupts: the flags read, the mask written.
    pub const ICR: u8 = 0xD;
    /// Control A.
    pub const CRA: u8 = 0xE;
    /// Control B.
    pub const CRB: u8 = 0xF;
}

/// Interrupt causes, as the interrupt register's bits.
pub mod icr {
    /// Timer A underflowed.
    pub const TA: u8 = 0x01;
    /// Timer B underflowed.
    pub const TB: u8 = 0x02;
    /// The time of day reached the alarm.
    pub const ALARM: u8 = 0x04;
    /// The serial register finished a byte.
    pub const SDR: u8 = 0x08;
    /// FLAG fell.
    pub const FLAG: u8 = 0x10;
    /// Read: an enabled cause is flagged. Written: the mask's named bits are
    /// set rather than cleared.
    pub const SET: u8 = 0x80;
    /// Every cause.
    pub const CAUSES: u8 = 0x1F;
}

/// Control register bits.
pub mod cr {
    /// The timer runs.
    pub const START: u8 = 0x01;
    /// The timer drives PB6 (timer A) or PB7 (timer B).
    pub const PB_ON: u8 = 0x02;
    /// That output toggles at each underflow rather than pulsing for a cycle.
    pub const TOGGLE: u8 = 0x04;
    /// The timer stops at its next underflow.
    pub const ONE_SHOT: u8 = 0x08;
    /// Load the latch into the counter; reads back clear.
    pub const FORCE_LOAD: u8 = 0x10;
    /// The timer counts CNT's rising edges rather than cycles.
    pub const COUNT_CNT: u8 = 0x20;
    /// Control B: timer B counts timer A's underflows; with
    /// [`COUNT_CNT`], only while CNT is high.
    pub const COUNT_TA: u8 = 0x40;
    /// Control A: the serial register shifts out.
    pub const SERIAL_OUT: u8 = 0x40;
    /// Control A: the time of day divides 50 Hz mains rather than 60.
    pub const TOD_50HZ: u8 = 0x80;
    /// Control B: time-of-day writes set the alarm.
    pub const ALARM: u8 = 0x80;
}

/// Which chip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Model {
    /// The original 6526.
    Mos6526,
    /// The 8521, and the 6526A like it.
    Mos8521,
}

impl Model {
    const fn number(self) -> u8 {
        match self {
            Self::Mos6526 => 0,
            Self::Mos8521 => 1,
        }
    }
}

impl snapshot::Save for Model {
    fn save(&self, w: &mut snapshot::Writer) {
        w.put(&self.number());
    }
}

impl snapshot::Load for Model {
    fn load(r: &mut snapshot::Reader<'_>) -> Result<Self, snapshot::SnapshotError> {
        match r.get::<u8>()? {
            0 => Ok(Self::Mos6526),
            1 => Ok(Self::Mos8521),
            _ => Err(snapshot::SnapshotError::Malformed),
        }
    }
}

/// The mains the time-of-day clock divides, against the machine's clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mains {
    /// Machine cycles a second.
    pub clock: u64,
    /// Mains cycles a second.
    pub hz: u64,
}

impl Mains {
    /// The machine cycle mains cycle `tick` begins in.
    fn tick_cycle(self, tick: u64) -> u64 {
        let cycle = u128::from(tick) * u128::from(self.clock) / u128::from(self.hz.max(1));
        u64::try_from(cycle).unwrap_or(u64::MAX)
    }
}

snapshot::fields!(
    Mains { clock, hz },
    check | m | m.hz != 0 && m.hz <= m.clock
);

/// A timer's pipeline: the control inputs, and the stages a start, a count,
/// a load and one-shot mode each pass through a cycle at a time.
mod stage {
    pub const START: u16 = 0x0001;
    pub const ONE_SHOT: u16 = 0x0002;
    pub const FORCE_LOAD: u16 = 0x0004;
    /// Counting cycles rather than pulses.
    pub const PHI2: u16 = 0x0008;
    /// A pulse to count: timer A's underflow, or CNT's edge.
    pub const PULSE: u16 = 0x0010;
    pub const COUNTING_1: u16 = 0x0020;
    /// A count lands in this step.
    pub const COUNTING: u16 = 0x0040;
    pub const COUNTED: u16 = 0x0080;
    pub const LOADING_1: u16 = 0x0100;
    /// The latch is loaded into the counter this cycle.
    pub const LOADING: u16 = 0x0200;
    pub const ONE_SHOT_1: u16 = 0x0400;
    pub const ONE_SHOT_2: u16 = 0x0800;
    /// The counter underflowed this cycle.
    pub const UNDERFLOW: u16 = 0x1000;
    pub const ALL: u16 = 0x1FFF;
    pub const CONTROL: u16 = START | ONE_SHOT | FORCE_LOAD | PHI2;
    pub const ONE_SHOT_STAGES: u16 = ONE_SHOT | ONE_SHOT_1 | ONE_SHOT_2;
    /// Counting a cycle a step with nothing in flight.
    pub const STEADY: u16 = START | PHI2 | COUNTING_1 | COUNTING | COUNTED;
    pub const STEADY_MASK: u16 = STEADY | FORCE_LOAD | PULSE | LOADING_1 | LOADING | UNDERFLOW;
    /// Bits a stopped timer with nothing in flight has none of.
    pub const IN_FLIGHT: u16 =
        COUNTING_1 | COUNTING | COUNTED | FORCE_LOAD | LOADING_1 | LOADING | PULSE | UNDERFLOW;

    /// The stages a cycle on from `s`, before the counter is looked at.
    pub const fn next(s: u16) -> u16 {
        let mut n = s & (START | ONE_SHOT | PHI2);
        if s & START != 0 && s & PHI2 != 0 {
            n |= COUNTING_1;
        }
        if s & COUNTING_1 != 0 || (s & PULSE != 0 && s & START != 0) {
            n |= COUNTING;
        }
        if s & COUNTING != 0 {
            n |= COUNTED;
        }
        if s & FORCE_LOAD != 0 {
            n |= LOADING_1;
        }
        if s & LOADING_1 != 0 {
            n |= LOADING;
        }
        if s & ONE_SHOT != 0 {
            n |= ONE_SHOT_1;
        }
        if s & ONE_SHOT_1 != 0 {
            n |= ONE_SHOT_2;
        }
        n
    }

    /// Whether one-shot mode has passed through every stage, on or off.
    pub const fn one_shot_settled(s: u16) -> bool {
        let bits = s & ONE_SHOT_STAGES;
        bits == 0 || bits == ONE_SHOT_STAGES
    }
}

/// One timer: its counter, its latch and its pipeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Timer {
    counter: u16,
    latch: u16,
    stages: u16,
}

snapshot::fields!(
    Timer {
        counter,
        latch,
        stages
    },
    check | t | t.stages & !stage::ALL == 0
);

impl Timer {
    /// A timer after reset: stopped, its counter and latch all ones.
    const RESET: Self = Self {
        counter: 0xFFFF,
        latch: 0xFFFF,
        stages: 0,
    };

    fn set_control(&mut self, control: u8, phi2: bool) {
        let mut inputs = 0;
        for (bit, input) in [
            (cr::START, stage::START),
            (cr::ONE_SHOT, stage::ONE_SHOT),
            (cr::FORCE_LOAD, stage::FORCE_LOAD),
        ] {
            if control & bit != 0 {
                inputs |= input;
            }
        }
        if phi2 {
            inputs |= stage::PHI2;
        }
        self.stages = self.stages & !stage::CONTROL | inputs;
    }

    const fn running(self) -> bool {
        self.stages & stage::START != 0
    }

    const fn underflowed(self) -> bool {
        self.stages & stage::UNDERFLOW != 0
    }

    /// Count a pulse in the next step, if running.
    fn pulse(&mut self) {
        if self.running() {
            self.stages |= stage::PULSE;
        }
    }

    fn write_latch_low(&mut self, value: u8) {
        self.latch = self.latch & 0xFF00 | u16::from(value);
        if self.stages & stage::LOADING != 0 {
            self.counter = self.counter & 0xFF00 | u16::from(value);
        }
    }

    fn write_latch_high(&mut self, value: u8) {
        self.latch = self.latch & 0x00FF | u16::from(value) << 8;
        if self.stages & stage::LOADING != 0 || !self.running() {
            self.counter = self.latch;
        }
    }

    /// One cycle on: whether the counter underflowed in it, reloading.
    fn step(&mut self) -> bool {
        if self.counter != 0 && self.stages & stage::COUNTING != 0 {
            self.counter -= 1;
        }
        let mut s = stage::next(self.stages);
        let underflow = self.counter == 0 && s & stage::COUNTING != 0;
        if underflow {
            s |= stage::LOADING | stage::UNDERFLOW;
        }
        if s & stage::LOADING != 0 {
            self.counter = self.latch;
            s &= !stage::COUNTING;
        }
        if s & stage::UNDERFLOW != 0 && s & (stage::ONE_SHOT_1 | stage::ONE_SHOT_2) != 0 {
            s &= !(stage::START | stage::COUNTING_1);
        }
        self.stages = s;
        underflow
    }

    const fn steady(self) -> bool {
        self.stages & stage::STEADY_MASK == stage::STEADY
            && stage::one_shot_settled(self.stages)
            && self.counter != 0
    }

    const fn idle(self) -> bool {
        self.stages & stage::IN_FLIGHT == 0
            && !(self.running() && self.stages & stage::PHI2 != 0)
            && stage::one_shot_settled(self.stages)
    }

    /// Steps that change nothing but the counter, at most: `None` without
    /// end, and 0 when the next must be taken a cycle at a time.
    fn quiet_steps(self) -> Option<u64> {
        if self.idle() {
            None
        } else if self.steady() {
            // The step that takes the counter to zero underflows.
            Some(u64::from(self.counter) - 1)
        } else {
            Some(0)
        }
    }

    /// Steps until an underflow with nothing else changing first: `None`
    /// while stopped, 1 while the pipeline moves.
    fn steps_to_underflow(self) -> Option<u64> {
        if self.idle() {
            None
        } else if self.steady() {
            Some(u64::from(self.counter))
        } else {
            Some(1)
        }
    }

    /// `steps` quiet steps on.
    fn skip(&mut self, steps: u64) {
        if self.steady() {
            self.counter -= u16::try_from(steps).unwrap_or(u16::MAX);
        }
    }
}

/// The interrupt pipeline's stages, each moving a place a cycle.
mod pipe {
    /// The flags a read acknowledged clear next cycle.
    pub const ACK_1: u16 = 0x0001;
    pub const ACK_0: u16 = 0x0002;
    /// Acknowledged last cycle.
    pub const ACKED: u16 = 0x0004;
    /// Bit 7 of the flags sets.
    pub const SET_1: u16 = 0x0010;
    pub const SET_0: u16 = 0x0020;
    /// The line rises.
    pub const RAISE_1: u16 = 0x0100;
    pub const RAISE_0: u16 = 0x0200;
    /// The flags were read this cycle, and last cycle.
    pub const READ_0: u16 = 0x1000;
    pub const READ_1: u16 = 0x2000;
    /// Stages that have moved past their last place.
    pub const EXPIRED: u16 = 0x0008 | 0x0040 | 0x0400 | 0x4000;
    pub const ALL: u16 = 0x3FFF & !EXPIRED;
}

/// The interrupt register and the pipeline between a cause and the line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Interrupts {
    /// The causes flagged, and [`icr::SET`].
    flags: u8,
    /// Causes raised since the pipeline's last cycle.
    raised: u8,
    /// Flags a read acknowledged, cleared when its acknowledge lands.
    acknowledged: u8,
    mask: u8,
    pipeline: u16,
    /// The line, low while asserted.
    line: bool,
    /// The cycle the flags were last read in.
    last_read: Option<u64>,
    /// Timer B underflowed the cycle after a read: the next read loses it.
    timer_b_lost: bool,
}

snapshot::fields!(
    Interrupts {
        flags,
        raised,
        acknowledged,
        mask,
        pipeline,
        line,
        last_read,
        timer_b_lost
    },
    check | i | {
        let flag_bits = icr::CAUSES | icr::SET;
        i.flags & !flag_bits == 0
            && i.raised & !icr::CAUSES == 0
            && i.acknowledged & !flag_bits == 0
            && i.mask & !icr::CAUSES == 0
            && i.pipeline & !pipe::ALL == 0
    }
);

impl Interrupts {
    fn raise(&mut self, causes: u8) {
        self.flags |= causes;
        self.raised |= causes;
        self.acknowledged &= !causes;
    }

    const fn read_cycle_before(&self, t: u64) -> bool {
        match (t.checked_sub(1), self.last_read) {
            (Some(before), Some(read)) => before == read,
            _ => false,
        }
    }

    /// The pipeline's cycle `t`.
    fn run_cycle(&mut self, model: Model, t: u64) {
        if self.pipeline & pipe::ACK_0 != 0 {
            self.flags &= !self.acknowledged;
            if model == Model::Mos6526 {
                self.flags &= !icr::SET;
            }
            self.acknowledged = 0;
        }
        if self.raised & self.mask != 0 {
            // The 8521 is late too when the flags were read the cycle before.
            let late = model == Model::Mos6526 || self.read_cycle_before(t);
            self.pipeline |= if late {
                pipe::RAISE_1 | pipe::SET_1
            } else {
                pipe::RAISE_0 | pipe::SET_0
            };
        }
        if self.pipeline & pipe::SET_0 != 0 {
            self.flags |= icr::SET;
        }
        if self.pipeline & pipe::RAISE_0 != 0 {
            self.line = true;
        }
        self.raised = 0;
        self.pipeline = (self.pipeline << 1) & !pipe::EXPIRED;
    }

    fn read(&mut self, model: Model, t: u64) -> u8 {
        self.last_read = Some(t);
        if self.timer_b_lost {
            self.flags &= !icr::TB;
            self.timer_b_lost = false;
        }
        let flags = self.flags;
        match model {
            Model::Mos8521 => {
                let mut flags = flags;
                if self.pipeline & pipe::RAISE_0 != 0 && flags & icr::CAUSES != 0 {
                    flags |= icr::SET;
                    self.flags = flags;
                }
                if flags != 0 {
                    self.acknowledged |= flags | icr::SET;
                }
                self.pipeline = (self.pipeline | pipe::ACK_1) & !(pipe::RAISE_0 | pipe::SET_0);
                self.pipeline |= pipe::READ_0;
                self.line = false;
                flags
            }
            Model::Mos6526 => {
                self.pipeline = (self.pipeline | pipe::ACK_1 | pipe::READ_0) & !pipe::RAISE_0;
                self.flags &= icr::SET;
                self.raised = 0;
                self.line = false;
                flags
            }
        }
    }

    fn write_mask(&mut self, model: Model, value: u8) {
        if value & icr::SET != 0 {
            self.mask |= value & icr::CAUSES;
        } else {
            self.mask &= !(value & icr::CAUSES);
        }
        if self.flags & self.mask != 0 {
            if !self.line {
                match model {
                    Model::Mos8521 if self.pipeline & pipe::READ_1 == 0 => {
                        self.pipeline |= pipe::RAISE_0 | pipe::SET_0;
                    }
                    Model::Mos8521 => {}
                    Model::Mos6526 => self.pipeline |= pipe::RAISE_1 | pipe::SET_1,
                }
            }
        } else if model == Model::Mos6526 && self.pipeline & pipe::ACKED != 0 {
            self.pipeline &= !(pipe::RAISE_0 | pipe::SET_0);
        }
    }

    const fn idle(&self) -> bool {
        self.pipeline == 0 && self.raised == 0 && self.acknowledged == 0
    }
}

/// The serial register's delay line: events a few cycles ahead, each moving
/// a place a cycle, and the history of CNT's output.
mod line {
    /// CNT toggles: timer A underflowed with bits to send.
    pub const TOGGLE_2: u32 = 0x0000_0001;
    pub const TOGGLE_1: u32 = 0x0000_0002;
    pub const TOGGLE_0: u32 = 0x0000_0004;
    pub const TOGGLES: u32 = 0x0000_000F;
    /// An underflow too close behind the last to toggle CNT.
    pub const SKIP_1: u32 = 0x0000_0020;
    pub const SKIP_0: u32 = 0x0000_0040;
    pub const SKIPS: u32 = 0x0000_00F0;
    /// The serial register's flag rises.
    pub const FLAG_2: u32 = 0x0000_0200;
    pub const FLAG_0: u32 = 0x0000_0800;
    pub const FLAGS: u32 = 0x0000_0F00;
    /// CNT was high this cycle, and the three before.
    pub const CNT_0: u32 = 0x0000_1000;
    pub const CNT_1: u32 = 0x0000_2000;
    pub const CNT_2: u32 = 0x0000_4000;
    pub const CNT_HISTORY: u32 = 0x0000_F000;
    /// The shifter takes the data register.
    pub const LOAD_1: u32 = 0x0004_0000;
    pub const LOAD_0: u32 = 0x0008_0000;
    pub const LOADS: u32 = 0x000F_0000;
    /// Places a group's last stage moves into.
    pub const EXPIRED: u32 = 0x0000_0010 | 0x0000_0100 | 0x0000_1000 | 0x0001_0000 | 0x0010_0000;
    pub const ACTIVE: u32 = TOGGLES | SKIPS | FLAGS | LOADS;
    pub const ALL: u32 = 0x000F_FFFF;
}

/// The serial register, shifting out at half timer A's underflow rate. Its
/// CNT and SP pins reach only the user port, where nothing answers, so it
/// never receives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Serial {
    data: u8,
    shifter: u16,
    /// Half-bits left to send: two each, CNT low then high.
    bits: u8,
    /// A byte written while another shifts, sent next.
    pending: bool,
    /// Switched to input mid-byte: the flag rises when switched back.
    force_finish: bool,
    line: u32,
    /// CNT as this chip drives it.
    cnt: bool,
}

snapshot::fields!(
    Serial {
        data,
        shifter,
        bits,
        pending,
        force_finish,
        line,
        cnt
    },
    check | s | s.bits <= 17 && s.line & !line::ALL == 0
);

impl Serial {
    const RESET: Self = Self {
        data: 0,
        shifter: 0,
        bits: 0,
        pending: false,
        force_finish: false,
        line: line::CNT_HISTORY,
        cnt: true,
    };

    /// Whether a step would change anything.
    const fn busy(&self) -> bool {
        let history = self.line & line::CNT_HISTORY;
        self.line & line::ACTIVE != 0 || (history != 0 && history != line::CNT_HISTORY)
    }

    /// The direction changes, to output if `output`.
    fn switch(&mut self, output: bool) {
        if (self.bits > 1 && self.bits < 15) || (self.bits == 15 && self.line & line::CNT_2 == 0) {
            self.line |= line::FLAG_2;
        }
        if output {
            if !self.cnt && self.bits != 0 {
                self.shifter <<= 1;
            }
            if self.force_finish {
                self.line |= line::FLAG_2;
                self.force_finish = false;
            }
        } else {
            let high = line::CNT_1 | line::CNT_2;
            self.force_finish = self.line & high != high
                || (self.bits != 2
                    && self.line & (line::TOGGLE_2 | line::TOGGLE_1) == 0
                    && self.line & line::TOGGLE_0 != 0);
        }
        self.bits = 0;
        self.pending = false;
        self.line &= !(line::TOGGLES | line::SKIPS);
    }

    /// Timer A underflowed with the register sending.
    fn underflow(&mut self) {
        if self.bits != 0 || self.pending {
            self.line |= if self.line & (line::TOGGLE_0 | line::SKIP_0) != 0 {
                line::SKIP_1
            } else {
                line::TOGGLE_1
            };
        }
    }

    /// The register's cycle: whether CNT rose, and whether a byte finished
    /// in time to flag it now.
    fn step(&mut self) -> (bool, bool) {
        let mut feed = 0;
        let mut rose = false;
        if self.line & line::LOAD_0 != 0 {
            match self.bits {
                0 => {
                    self.bits = 16;
                    self.shifter = u16::from(self.data) << 1;
                }
                1 => {
                    self.shifter |= u16::from(self.data);
                    self.bits = 17;
                }
                _ => self.pending = true,
            }
        }
        if self.line & line::TOGGLE_0 != 0 {
            if self.bits != 0 && (self.bits - 1) & 1 != 0 {
                self.bits -= 1;
                self.cnt = false;
                if self.bits == 1 {
                    feed |= line::FLAG_2;
                    if self.pending {
                        self.shifter |= u16::from(self.data);
                        self.pending = false;
                        self.bits = 17;
                    }
                }
            } else {
                self.bits = self.bits.saturating_sub(1);
                self.shifter <<= 1;
                rose = !self.cnt;
                self.cnt = true;
            }
        }
        let flagged = self.line & line::FLAG_0 != 0;
        self.line = ((self.line | feed) << 1) & !line::EXPIRED & line::ALL;
        if self.cnt {
            self.line |= line::CNT_0;
        }
        (rose, flagged)
    }
}

/// The time-of-day clock: tenths, seconds, minutes and hours in BCD, an
/// alarm, the latch a read of the hours holds, and the divider from mains.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Tod {
    time: [u8; 4],
    alarm: [u8; 4],
    latch: [u8; 4],
    latched: bool,
    stopped: bool,
    /// The divider's ring counter, 0 to 5.
    divider: u8,
    /// Mains cycles counted since power-on.
    ticks: u64,
}

snapshot::fields!(
    Tod {
        time,
        alarm,
        latch,
        latched,
        stopped,
        divider,
        ticks
    },
    check | t | t.divider <= 5
);

/// The time of day's registers in order, tenths first.
const TOD_TENTHS: usize = 0;
const TOD_HOURS: usize = 3;
/// Bit 7 of the hours: after noon.
const PM: u8 = 0x80;

impl Tod {
    /// The clock after reset: stopped at 1 o'clock until the tenths are
    /// written.
    const fn reset(ticks: u64) -> Self {
        let time = [0, 0, 0, 1];
        Self {
            time,
            alarm: [0; 4],
            latch: time,
            latched: false,
            stopped: true,
            divider: 0,
            ticks,
        }
    }

    const fn at_alarm(&self) -> bool {
        let (t, a) = (self.time, self.alarm);
        t[0] == a[0] && t[1] == a[1] && t[2] == a[2] && t[3] == a[3]
    }

    /// One mains cycle: whether the time moved on.
    fn tick(&mut self, fifty_hz: bool) -> bool {
        self.ticks += 1;
        if self.stopped {
            return false;
        }
        // The divider matches after five mains cycles in 50 Hz mode, six in
        // 60; the tenths move on with the cycle that sees the match.
        if self.divider != if fifty_hz { 4 } else { 5 } {
            self.divider = if self.divider >= 5 {
                0
            } else {
                self.divider + 1
            };
            return false;
        }
        self.divider = 0;
        self.advance();
        true
    }

    /// A tenth of a second on. Each digit counts within its own bits and
    /// carries only on reaching its limit, so a digit out of range runs on
    /// until it wraps, carrying nothing.
    fn advance(&mut self) {
        let [tenths, seconds, minutes, hours] = &mut self.time;
        let tenth = ((*tenths & 0x0F) + 1) & 0x0F;
        *tenths = if tenth == 10 { 0 } else { tenth };
        if tenth != 10 || !Self::advance_sixty(seconds) || !Self::advance_sixty(minutes) {
            return;
        }
        let (mut high, mut low, mut pm) = ((*hours >> 4) & 1, *hours & 0x0F, *hours & PM);
        if (high == 1 && low == 2) || (high == 0 && low == 9) {
            low = high;
            high ^= 1;
        } else {
            low = (low + 1) & 0x0F;
            if high == 1 && low == 2 {
                pm ^= PM;
            }
        }
        *hours = pm | high << 4 | low;
    }

    /// Seconds or minutes on: whether they wrapped past 59.
    fn advance_sixty(value: &mut u8) -> bool {
        let mut low = ((*value & 0x0F) + 1) & 0x0F;
        let mut high = (*value >> 4) & 0x07;
        let mut wrapped = false;
        if low == 10 {
            low = 0;
            high = (high + 1) & 0x07;
            if high == 6 {
                high = 0;
                wrapped = true;
            }
        }
        *value = high << 4 | low;
        wrapped
    }

    fn read(&mut self, index: usize) -> u8 {
        if !self.latched {
            self.latch = self.time;
        }
        match index {
            TOD_TENTHS => self.latched = false,
            TOD_HOURS => self.latched = true,
            _ => {}
        }
        self.latch[index]
    }

    /// `value` written to register `index`, to the alarm if `alarm`, else
    /// to the time: whether a value changed.
    fn write(&mut self, index: usize, value: u8, alarm: bool) -> bool {
        let mut value = value & [0x0F, 0x7F, 0x7F, 0x9F][index];
        if alarm {
            let changed = self.alarm[index] != value;
            self.alarm[index] = value;
            return changed;
        }
        match index {
            TOD_TENTHS if self.stopped => {
                self.divider = 0;
                self.stopped = false;
            }
            TOD_HOURS => {
                self.stopped = true;
                // Writing 12 flips the half of the day.
                if value & 0x1F == 0x12 {
                    value ^= PM;
                }
            }
            _ => {}
        }
        let changed = self.time[index] != value;
        self.time[index] = value;
        changed
    }
}

/// A CIA.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cia {
    model: Model,
    mains: Mains,
    /// The cycle the chip has been brought up to: its timers have stepped
    /// into it, and every earlier cycle is finished.
    now: u64,
    /// The interrupt pipeline has run its cycle for `now`.
    pipeline_ran: bool,
    pra: u8,
    prb: u8,
    ddra: u8,
    ddrb: u8,
    cra: u8,
    crb: u8,
    a: Timer,
    b: Timer,
    /// The toggling outputs of timer A and timer B.
    toggles: [bool; 2],
    interrupts: Interrupts,
    serial: Serial,
    tod: Tod,
    /// FLAG's level.
    flag: bool,
}

snapshot::fields!(
    Cia {
        model,
        mains,
        now,
        pipeline_ran,
        pra,
        prb,
        ddra,
        ddrb,
        cra,
        crb,
        a,
        b,
        toggles,
        interrupts,
        serial,
        tod,
        flag
    },
    check | c,
    r | c.now == r.clock()
        && c.cra & cr::FORCE_LOAD == 0
        && c.crb & cr::FORCE_LOAD == 0
        && c.mains.tick_cycle(c.tod.ticks) <= c.now
        && c.mains.tick_cycle(c.tod.ticks + 1) >= c.now
);

impl Cia {
    /// A chip as reset leaves it at power-on, counting `mains` for its time
    /// of day.
    #[must_use]
    pub const fn new(model: Model, mains: Mains) -> Self {
        Self {
            model,
            mains,
            now: 0,
            pipeline_ran: false,
            pra: 0,
            prb: 0,
            ddra: 0,
            ddrb: 0,
            cra: 0,
            crb: 0,
            a: Timer::RESET,
            b: Timer::RESET,
            toggles: [false; 2],
            interrupts: Interrupts {
                flags: 0,
                raised: 0,
                acknowledged: 0,
                mask: 0,
                pipeline: 0,
                line: false,
                last_read: None,
                timer_b_lost: false,
            },
            serial: Serial::RESET,
            tod: Tod::reset(0),
            flag: true,
        }
    }

    /// Which chip it is.
    #[must_use]
    pub const fn model(&self) -> Model {
        self.model
    }

    /// The mains it counts.
    #[must_use]
    pub const fn mains(&self) -> Mains {
        self.mains
    }

    /// RES at cycle `now`: the registers cleared, the timers stopped with
    /// their latches all ones, the clock stopped at 1 o'clock.
    pub fn reset(&mut self, now: u64) {
        self.advance(now);
        let (model, mains, flag, ticks) = (self.model, self.mains, self.flag, self.tod.ticks);
        *self = Self::new(model, mains);
        self.now = now;
        self.flag = flag;
        self.tod = Tod::reset(ticks);
    }

    /// The interrupt line, asserted, as the last finished cycle left it.
    #[must_use]
    pub const fn irq(&self) -> bool {
        self.interrupts.line
    }

    /// Port A's levels as the chip drives them, undriven lines pulled up.
    #[must_use]
    pub const fn port_a(&self) -> u8 {
        self.pra | !self.ddra
    }

    /// Port B's levels as the chip drives them, the timers' outputs on PB6
    /// and PB7 where they are on; as of the cycle it was brought up to.
    #[must_use]
    pub fn port_b(&self) -> u8 {
        let mut levels = self.prb | !self.ddrb;
        let outputs = [(self.cra, self.a, 0x40), (self.crb, self.b, 0x80)];
        for ((control, timer, bit), toggle) in outputs.into_iter().zip(self.toggles) {
            if control & cr::PB_ON != 0 {
                let high = if control & cr::TOGGLE != 0 {
                    toggle
                } else {
                    timer.underflowed()
                };
                levels = if high { levels | bit } else { levels & !bit };
            }
        }
        levels
    }

    /// CNT's level: high unless the serial register drives it low.
    const fn cnt(&self) -> bool {
        self.serial.cnt
    }

    /// Bring the chip up to cycle `to`.
    pub fn advance(&mut self, to: u64) {
        while self.now < to {
            let quiet = self.quiet_cycles().min(to - self.now);
            if quiet > 0 {
                self.a.skip(quiet);
                self.b.skip(quiet);
                self.pipeline_ran = false;
                self.now += quiet;
            } else {
                self.finish_cycle();
                self.now += 1;
                self.step_timers();
            }
        }
    }

    /// Cycles from `now` that finish and step changing nothing but steadily
    /// counting timers.
    fn quiet_cycles(&self) -> u64 {
        if !self.interrupts.idle() || self.serial.busy() {
            return 0;
        }
        let tod = self.next_tick().saturating_sub(self.now);
        [self.a.quiet_steps(), self.b.quiet_steps()]
            .into_iter()
            .flatten()
            .fold(tod, u64::min)
    }

    /// The cycle the next mains cycle begins in.
    fn next_tick(&self) -> u64 {
        self.mains.tick_cycle(self.tod.ticks + 1)
    }

    /// Cycle `now`'s work after the CPU's access.
    fn finish_cycle(&mut self) {
        let now = self.now;
        if self.serial.busy() {
            let (rose, flagged) = self.serial.step();
            if flagged {
                self.interrupts.raise(icr::SDR);
            }
            if rose {
                self.cnt_rose();
            }
        }
        while self.next_tick() <= now {
            if self.tod.tick(self.cra & cr::TOD_50HZ != 0) && self.tod.at_alarm() {
                self.interrupts.raise(icr::ALARM);
            }
        }
        if !self.pipeline_ran {
            self.interrupts.run_cycle(self.model, now);
        }
        self.pipeline_ran = false;
    }

    /// The timers' steps into cycle `now`, and what their underflows raise.
    fn step_timers(&mut self) {
        let a = self.a.step();
        let b = self.b.step();
        if a {
            self.interrupts.raise(icr::TA);
            self.toggles[0] = !self.toggles[0];
            if self.cra & cr::SERIAL_OUT != 0 {
                self.serial.underflow();
            }
            let counts_a = match self.crb & (cr::COUNT_TA | cr::COUNT_CNT) {
                cr::COUNT_TA => true,
                both if both == cr::COUNT_TA | cr::COUNT_CNT => self.cnt(),
                _ => false,
            };
            if counts_a {
                self.b.pulse();
            }
        }
        if b {
            self.interrupts.raise(icr::TB);
            self.interrupts.timer_b_lost =
                self.model == Model::Mos6526 && self.interrupts.read_cycle_before(self.now);
            self.toggles[1] = !self.toggles[1];
        }
    }

    /// CNT rose: a pulse for each timer counting its edges.
    fn cnt_rose(&mut self) {
        if self.cra & cr::COUNT_CNT != 0 {
            self.a.pulse();
        }
        if self.crb & (cr::COUNT_TA | cr::COUNT_CNT) == cr::COUNT_CNT {
            self.b.pulse();
        }
    }

    /// Run the interrupt pipeline's cycle for `now` ahead of an access that
    /// reads or writes what it settles.
    fn settle_interrupts(&mut self) {
        if !self.pipeline_ran {
            self.interrupts.run_cycle(self.model, self.now);
            self.pipeline_ran = true;
        }
    }

    /// The CPU reads `register` (its low four bits) in cycle `now`.
    pub fn read(&mut self, now: u64, register: u8) -> u8 {
        self.advance(now);
        match register & 0x0F {
            reg::PRA => self.port_a(),
            reg::PRB => self.port_b(),
            reg::DDRA => self.ddra,
            reg::DDRB => self.ddrb,
            reg::TA_LO => self.a.counter.to_le_bytes()[0],
            reg::TA_HI => self.a.counter.to_le_bytes()[1],
            reg::TB_LO => self.b.counter.to_le_bytes()[0],
            reg::TB_HI => self.b.counter.to_le_bytes()[1],
            index @ reg::TOD_TENTHS..=reg::TOD_HOURS => {
                self.tod.read(usize::from(index - reg::TOD_TENTHS))
            }
            reg::SDR => self.serial.data,
            reg::ICR => {
                self.settle_interrupts();
                self.interrupts.read(self.model, now)
            }
            reg::CRA => Self::control_read(self.cra, self.a),
            _ => Self::control_read(self.crb, self.b),
        }
    }

    /// A control register as read: running as the timer is, never loading.
    const fn control_read(control: u8, timer: Timer) -> u8 {
        let running = if timer.running() { cr::START } else { 0 };
        control & !cr::START | running
    }

    /// The CPU writes `value` to `register` in cycle `now`.
    pub fn write(&mut self, now: u64, register: u8, value: u8) {
        self.advance(now);
        match register & 0x0F {
            reg::PRA => self.pra = value,
            reg::PRB => self.prb = value,
            reg::DDRA => self.ddra = value,
            reg::DDRB => self.ddrb = value,
            reg::TA_LO => self.a.write_latch_low(value),
            reg::TA_HI => self.a.write_latch_high(value),
            reg::TB_LO => self.b.write_latch_low(value),
            reg::TB_HI => self.b.write_latch_high(value),
            index @ reg::TOD_TENTHS..=reg::TOD_HOURS => {
                let index = usize::from(index - reg::TOD_TENTHS);
                if self.tod.write(index, value, self.crb & cr::ALARM != 0) && self.tod.at_alarm() {
                    self.interrupts.raise(icr::ALARM);
                }
            }
            reg::SDR => {
                self.serial.data = value;
                if self.cra & cr::SERIAL_OUT != 0 {
                    self.serial.line |= line::LOAD_1;
                }
            }
            reg::ICR => {
                self.settle_interrupts();
                self.interrupts.write_mask(self.model, value);
            }
            reg::CRA => {
                if value & cr::START != 0 && !self.a.running() {
                    self.toggles[0] = true;
                }
                if (value ^ self.cra) & cr::SERIAL_OUT != 0 {
                    self.serial.switch(value & cr::SERIAL_OUT != 0);
                    if !self.serial.cnt {
                        self.serial.cnt = true;
                        self.cnt_rose();
                    }
                }
                self.a.set_control(value, value & cr::COUNT_CNT == 0);
                self.cra = value & !cr::FORCE_LOAD;
            }
            _ => {
                if value & cr::START != 0 && !self.b.running() {
                    self.toggles[1] = true;
                }
                self.b
                    .set_control(value, value & (cr::COUNT_TA | cr::COUNT_CNT) == 0);
                self.crb = value & !cr::FORCE_LOAD;
            }
        }
    }

    /// FLAG at `level` from cycle `now`: falling, it raises its cause.
    pub fn set_flag(&mut self, now: u64, level: bool) {
        self.advance(now);
        if self.flag && !level {
            self.interrupts.raise(icr::FLAG);
        }
        self.flag = level;
    }

    /// The first cycle the chip must be brought up to for its interrupt line
    /// to be right, if any can change it unprompted.
    #[must_use]
    pub fn next_event(&self) -> Option<u64> {
        if !self.interrupts.idle() || self.serial.busy() {
            return Some(self.now + 1);
        }
        let mask = self.interrupts.mask;
        // An underflow raises its flag before the cycle it falls in ends,
        // and the line no sooner than that cycle's end.
        let after = |steps: Option<u64>| steps.map(|steps| self.now + steps + 1);
        let serial_waits =
            self.cra & cr::SERIAL_OUT != 0 && (self.serial.bits != 0 || self.serial.pending);
        let b_counts_a = self.crb & cr::COUNT_TA != 0;
        let a = (mask & icr::TA != 0 || serial_waits || (b_counts_a && mask & icr::TB != 0))
            .then(|| after(self.a.steps_to_underflow()))
            .flatten();
        let b = (mask & icr::TB != 0)
            .then(|| after(self.b.steps_to_underflow()))
            .flatten();
        let tod = (mask & icr::ALARM != 0 && !self.tod.stopped).then(|| self.next_tick() + 1);
        [a, b, tod].into_iter().flatten().min()
    }
}

#[cfg(test)]
#[path = "cia_tests.rs"]
mod tests;
