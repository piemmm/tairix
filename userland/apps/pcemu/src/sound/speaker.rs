//! The PC speaker: timer 2's output gated by port 61h's data bit, drawn
//! band-limited edge by edge. A tone too high for the mix to carry — the
//! ultrasonic carrier a program parks the speaker on, the pulse train PWM
//! sound rides on — is heard as its duty, the level the cone settles to.

use super::{Mixer, Source};
use crate::chips::pit::Pit;

/// Bus-oscillator ticks in a PIT tick.
const PIT_TICKS: u64 = 12;

/// The shortest period, in PIT ticks, followed edge by edge: two samples of
/// the mix, its Nyquist.
const AUDIBLE_PERIOD: u32 = 48;

/// The speaker.
#[derive(Clone, Debug, Default)]
pub struct Speaker {
    /// Port 61h's speaker-data bit.
    data: bool,
    /// The level the mix last heard, from 0 to 1, as a fraction.
    heard: (u32, u32),
    /// PIT ticks the mix has heard through.
    done: u64,
}

impl Speaker {
    /// A silent speaker.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            data: false,
            heard: (0, 1),
            done: 0,
        }
    }

    /// The level the speaker gives at PIT tick `tick`, as a fraction:
    /// timer 2's output while the data bit is on, or the duty of a tone too
    /// high to carry; and whether it is that steady duty.
    fn level(&self, pit: &Pit, tick: u64) -> ((u32, u32), bool) {
        match pit.duty(2, tick) {
            Some((high, period)) if period < AUDIBLE_PERIOD => {
                (if self.data { (high, period) } else { (0, 1) }, true)
            }
            _ => ((u32::from(self.data && pit.out(2, tick)), 1), false),
        }
    }

    /// Follows the speaker through PIT tick `until`, as timer 2 drives it.
    pub fn advance(&mut self, pit: &Pit, until: u64, mixer: &mut Mixer) {
        let mut tick = self.done;
        while tick < until {
            let (level, steady) = self.level(pit, tick);
            self.set(tick, level, mixer);
            let next = if steady {
                pit.reload_at(2).filter(|&reload| reload > tick)
            } else {
                pit.next_change(2, tick)
            };
            tick = next.map_or(until, |next| next.min(until));
        }
        self.done = self.done.max(until);
    }

    /// Port 61h's data bit becomes `data` at PIT tick `tick`.
    pub fn set_data(&mut self, data: bool, pit: &Pit, tick: u64, mixer: &mut Mixer) {
        self.advance(pit, tick, mixer);
        if data != self.data {
            self.data = data;
            let (level, _) = self.level(pit, tick);
            self.set(tick, level, mixer);
        }
    }

    fn set(&mut self, tick: u64, (over, under): (u32, u32), mixer: &mut Mixer) {
        let (was_over, was_under) = self.heard;
        if u64::from(over) * u64::from(was_under) != u64::from(was_over) * u64::from(under) {
            let share = |n: u32| f64::from(n);
            let delta = share(over) / share(under) - share(was_over) / share(was_under);
            mixer.step(
                Source::Speaker,
                tick * PIT_TICKS,
                tairix_util::mathf::single(delta),
            );
            self.heard = (over, under);
        }
    }
}

crate::snapshot::state!(Speaker { data, heard, done });

#[cfg(test)]
#[path = "speaker_tests.rs"]
mod tests;
