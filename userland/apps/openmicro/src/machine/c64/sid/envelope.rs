//! A voice's envelope generator: an eight-bit counter stepped by a 15-bit
//! rate counter, through attack, decay to the sustain level, and release,
//! with the decay's exponential divider and the pipeline delays die
//! analysis found between a step's cause and its effect (reSID 1.0's
//! measurements).
//!
//! The rate counter compares for equality, so a period set below its count
//! is missed until the counter wraps at $8000: the ADSR delay bug.

use crate::snapshot;

/// The rate counter's period for each of the sixteen rates; with the
/// cycle the counter takes to reset, a step comes every period + 1 cycles.
const PERIOD: [u16; 16] = [
    8, 31, 62, 94, 148, 219, 266, 312, 391, 976, 1953, 3125, 3906, 11719, 19531, 31250,
];

/// The phase the generator is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Attack,
    DecaySustain,
    Release,
}

impl State {
    const fn number(self) -> u8 {
        match self {
            Self::Attack => 0,
            Self::DecaySustain => 1,
            Self::Release => 2,
        }
    }
}

impl snapshot::Save for State {
    fn save(&self, w: &mut snapshot::Writer) {
        w.put(&self.number());
    }
}

impl snapshot::Load for State {
    fn load(r: &mut snapshot::Reader<'_>) -> Result<Self, snapshot::SnapshotError> {
        match r.get::<u8>()? {
            0 => Ok(Self::Attack),
            1 => Ok(Self::DecaySustain),
            2 => Ok(Self::Release),
            _ => Err(snapshot::SnapshotError::Malformed),
        }
    }
}

/// An envelope generator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Envelope {
    counter: u8,
    rate_counter: u16,
    rate_period: u16,
    /// The rate counter matched last cycle: it resets in this one.
    reset_rate_counter: bool,
    exponential_counter: u8,
    exponential_period: u8,
    state: State,
    next_state: State,
    /// Cycles until a gate change takes effect, a step lands, and the
    /// exponential divider's carry arrives.
    state_pipeline: u8,
    step_pipeline: u8,
    exponential_pipeline: u8,
    /// The counter reached zero: it holds there until attack.
    hold_zero: bool,
    gate: bool,
    attack: u8,
    decay: u8,
    sustain: u8,
    release: u8,
    /// What ENV3 reads: the counter as the cycle began.
    env3: u8,
}

snapshot::fields!(
    Envelope {
        counter,
        rate_counter,
        rate_period,
        reset_rate_counter,
        exponential_counter,
        exponential_period,
        state,
        next_state,
        state_pipeline,
        step_pipeline,
        exponential_pipeline,
        hold_zero,
        gate,
        attack,
        decay,
        sustain,
        release,
        env3
    },
    check | e | e.rate_counter < 0x8000
        && PERIOD.contains(&e.rate_period)
        && [1, 2, 4, 8, 16, 30].contains(&e.exponential_period)
        && e.state_pipeline <= 3
        && e.step_pipeline <= 4
        && e.exponential_pipeline <= 2
        && e.attack < 16
        && e.decay < 16
        && e.sustain < 16
        && e.release < 16
);

impl Envelope {
    /// A generator after reset: released, its counter where it was.
    pub(super) const fn new() -> Self {
        Self {
            counter: 0,
            rate_counter: 0,
            rate_period: PERIOD[0],
            reset_rate_counter: false,
            exponential_counter: 0,
            exponential_period: 1,
            state: State::Release,
            next_state: State::Release,
            state_pipeline: 0,
            step_pipeline: 0,
            exponential_pipeline: 0,
            hold_zero: false,
            gate: false,
            attack: 0,
            decay: 0,
            sustain: 0,
            release: 0,
            env3: 0,
        }
    }

    pub(super) const fn reset(&mut self) {
        let counter = self.counter;
        *self = Self::new();
        self.counter = counter;
    }

    /// The counter, as the DAC reads it.
    pub(super) const fn output(&self) -> u8 {
        self.counter
    }

    /// What ENV3 reads.
    pub(super) const fn env3(&self) -> u8 {
        self.env3
    }

    const fn sustain_level(&self) -> u8 {
        self.sustain * 0x11
    }

    /// The gate bit of `control`.
    pub(super) fn write_control(&mut self, control: u8) {
        let gate = control & super::wave::control::GATE != 0;
        if gate == self.gate {
            return;
        }
        if gate {
            // The decay rate governs the first cycle of an attack.
            self.next_state = State::Attack;
            self.state = State::DecaySustain;
            self.rate_period = PERIOD[usize::from(self.decay)];
            self.state_pipeline = 2;
            if self.reset_rate_counter || self.exponential_pipeline == 2 {
                self.step_pipeline =
                    if self.exponential_period == 1 || self.exponential_pipeline == 2 {
                        2
                    } else {
                        4
                    };
            } else if self.exponential_pipeline == 1 {
                self.state_pipeline = 3;
            }
        } else {
            self.next_state = State::Release;
            self.state_pipeline = if self.step_pipeline > 0 { 3 } else { 2 };
        }
        self.gate = gate;
    }

    pub(super) fn write_attack_decay(&mut self, value: u8) {
        self.attack = value >> 4;
        self.decay = value & 0x0F;
        match self.state {
            State::Attack => self.rate_period = PERIOD[usize::from(self.attack)],
            State::DecaySustain => self.rate_period = PERIOD[usize::from(self.decay)],
            State::Release => {}
        }
    }

    pub(super) fn write_sustain_release(&mut self, value: u8) {
        self.sustain = value >> 4;
        self.release = value & 0x0F;
        if self.state == State::Release {
            self.rate_period = PERIOD[usize::from(self.release)];
        }
    }

    /// The divider's period after the counter changes: it lengthens as the
    /// counter falls past each threshold.
    const fn set_exponential_period(&mut self) {
        match self.counter {
            0xFF => self.exponential_period = 1,
            0x5D => self.exponential_period = 2,
            0x36 => self.exponential_period = 4,
            0x1A => self.exponential_period = 8,
            0x0E => self.exponential_period = 16,
            0x06 => self.exponential_period = 30,
            0x00 => {
                self.exponential_period = 1;
                self.hold_zero = true;
            }
            _ => {}
        }
    }

    fn change_state(&mut self) {
        self.state_pipeline -= 1;
        match self.next_state {
            State::Attack if self.state_pipeline == 0 => {
                self.state = State::Attack;
                self.rate_period = PERIOD[usize::from(self.attack)];
                self.hold_zero = false;
            }
            State::Release
                if (self.state == State::Attack && self.state_pipeline == 0)
                    || (self.state == State::DecaySustain && self.state_pipeline == 1) =>
            {
                self.state = State::Release;
                self.rate_period = PERIOD[usize::from(self.release)];
            }
            State::Attack | State::DecaySustain | State::Release => {}
        }
    }

    /// One cycle.
    pub(super) fn clock(&mut self) {
        self.env3 = self.counter;
        if self.state_pipeline != 0 {
            self.change_state();
        }
        if self.step_pipeline != 0 {
            self.step_pipeline -= 1;
            if self.step_pipeline == 0 && !self.hold_zero {
                match self.state {
                    State::Attack => {
                        self.counter = self.counter.wrapping_add(1);
                        if self.counter == 0xFF {
                            self.state = State::DecaySustain;
                            self.rate_period = PERIOD[usize::from(self.decay)];
                        }
                    }
                    State::DecaySustain | State::Release => {
                        self.counter = self.counter.wrapping_sub(1);
                    }
                }
                self.set_exponential_period();
            }
        }
        if self.exponential_pipeline != 0 {
            self.exponential_pipeline -= 1;
            if self.exponential_pipeline == 0 {
                self.exponential_counter = 0;
                // Released, the counter runs on past zero if an attack
                // wrapped it there.
                let steps = match self.state {
                    State::DecaySustain => self.counter != self.sustain_level(),
                    State::Release => true,
                    State::Attack => false,
                };
                if steps {
                    self.step_pipeline = 1;
                }
            }
        } else if self.reset_rate_counter {
            self.rate_counter = 0;
            self.reset_rate_counter = false;
            if self.state == State::Attack {
                // An attack step clears the divider: it does not slow the
                // climb.
                self.exponential_counter = 0;
                self.step_pipeline = 2;
            } else if !self.hold_zero {
                // Eight bits, as the chip's: a count past a period lowered
                // under it wraps round to it.
                self.exponential_counter = self.exponential_counter.wrapping_add(1);
                if self.exponential_counter == self.exponential_period {
                    self.exponential_pipeline = if self.exponential_period == 1 { 1 } else { 2 };
                }
            }
        }
        if self.rate_counter == self.rate_period {
            self.reset_rate_counter = true;
        } else {
            self.rate_counter += 1;
            if self.rate_counter & 0x8000 != 0 {
                self.rate_counter = (self.rate_counter + 1) & 0x7FFF;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cycles until the counter first reads `level`.
    fn cycles_to(envelope: &mut Envelope, level: u8, limit: u32) -> Option<u32> {
        (1..=limit).find(|_| {
            envelope.clock();
            envelope.output() == level
        })
    }

    #[test]
    fn the_fastest_attack_steps_every_nine_cycles() {
        let mut envelope = Envelope::new();
        envelope.write_attack_decay(0x00);
        envelope.write_control(1);
        let first = cycles_to(&mut envelope, 1, 100).expect("a first step");
        let second = cycles_to(&mut envelope, 2, 100).expect("a second step");
        assert_eq!(second, 9);
        assert!(first <= 12);
    }

    #[test]
    fn an_attack_climbs_to_ff_then_decays_to_the_sustain_level() {
        let mut envelope = Envelope::new();
        envelope.write_attack_decay(0x00);
        envelope.write_sustain_release(0x80);
        envelope.write_control(1);
        assert!(cycles_to(&mut envelope, 0xFF, 5_000).is_some());
        assert!(cycles_to(&mut envelope, 0x88, 50_000).is_some());
        for _ in 0..100_000 {
            envelope.clock();
        }
        assert_eq!(envelope.output(), 0x88, "sustained");
    }

    #[test]
    fn a_release_falls_exponentially_and_holds_at_zero() {
        let mut envelope = Envelope::new();
        envelope.write_sustain_release(0xF0);
        envelope.write_control(1);
        assert!(cycles_to(&mut envelope, 0xFF, 5_000).is_some());
        envelope.write_control(0);
        let fast = cycles_to(&mut envelope, 0x5D, 100_000).expect("the first stretch");
        let slow = cycles_to(&mut envelope, 0x36, 100_000).expect("the second stretch");
        // 162 steps a period apart, then 39 two periods apart.
        let (per_fast, per_slow) = (fast * 39, slow * 162);
        assert!(
            per_slow * 10 > per_fast * 18 && per_slow * 10 < per_fast * 22,
            "{fast} then {slow}"
        );
        assert!(cycles_to(&mut envelope, 0, 1_000_000).is_some());
        for _ in 0..10_000 {
            envelope.clock();
        }
        assert_eq!(envelope.output(), 0);
    }

    #[test]
    fn an_exponential_count_past_its_period_wraps_round_to_it() {
        let mut envelope = Envelope::new();
        envelope.write_sustain_release(0x00);
        envelope.state = State::Release;
        envelope.counter = 0x80;
        envelope.hold_zero = false;
        envelope.exponential_period = 8;
        envelope.exponential_counter = u8::MAX;
        envelope.reset_rate_counter = true;
        assert!(
            cycles_to(&mut envelope, 0x7F, 400).is_some(),
            "the release steps again once the count has wrapped"
        );
    }

    #[test]
    fn a_period_lowered_below_the_count_waits_for_the_wrap() {
        let mut envelope = Envelope::new();
        envelope.write_attack_decay(0xF0);
        envelope.write_control(1);
        for _ in 0..1_000 {
            envelope.clock();
        }
        envelope.write_attack_decay(0x00);
        let delay = cycles_to(&mut envelope, 1, 40_000).expect("a step after the wrap");
        assert!(delay > 30_000, "{delay}");
    }
}
