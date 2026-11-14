//! The ULA's sound: while its counter is given to sound, a square wave of
//! 1 MHz / (16 (S + 1)), rendered a sample each 250 kHz as the Model B's
//! sound is, unipolar as the ULA drives the speaker; silence otherwise.

use alloc::vec::Vec;

use crate::machine::CYCLES_PER_SAMPLE;

/// The level the speaker is driven to while the wave is high: one of the
/// SN76489's four channels at full volume, so the two machines sound alike.
const LEVEL: f32 = 0.25;

/// Cycles each half of the wave lasts for each step of counter value S: it
/// lasts 16 (S + 1).
const HALF_CYCLES: u64 = 16;
const _: () = assert!(HALF_CYCLES.is_multiple_of(CYCLES_PER_SAMPLE));

/// Samples each half of the wave lasts for counter value `counter`.
fn half_period(counter: u8) -> u64 {
    (u64::from(counter) + 1) * HALF_CYCLES / CYCLES_PER_SAMPLE
}

/// The square wave.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sound {
    counter: u8,
    on: bool,
    high: bool,
    /// Samples left in the current half of the wave.
    left: u64,
    /// Samples rendered since power-on.
    samples: u64,
}

impl Default for Sound {
    fn default() -> Self {
        Self::new()
    }
}

impl Sound {
    /// Silent, its counter zero.
    #[must_use]
    pub fn new() -> Self {
        Self {
            counter: 0,
            on: false,
            high: false,
            left: half_period(0),
            samples: 0,
        }
    }

    /// The counter is written: the wave takes it at its next half.
    pub const fn set_counter(&mut self, counter: u8) {
        self.counter = counter;
    }

    /// The counter is given to sound, or taken from it.
    pub const fn set_on(&mut self, on: bool) {
        self.on = on;
    }

    /// Samples rendered since power-on.
    #[must_use]
    pub const fn samples(&self) -> u64 {
        self.samples
    }

    /// Render up to sample `until` into `out`.
    pub fn render(&mut self, until: u64, out: &mut Vec<f32>) {
        while self.samples < until {
            self.step();
            out.push(if self.on && self.high { LEVEL } else { 0.0 });
        }
    }

    /// Run up to sample `until`, discarding the output, as a muted or
    /// unpaced machine does.
    pub fn skip(&mut self, until: u64) {
        while self.samples < until {
            self.step();
        }
    }

    fn step(&mut self) {
        self.samples += 1;
        if self.left > 1 {
            self.left -= 1;
            return;
        }
        self.high = !self.high;
        self.left = half_period(self.counter);
    }
}

crate::snapshot::fields!(
    Sound {
        counter,
        on,
        high,
        left,
        samples
    },
    check | sound,
    r | (1..=half_period(u8::MAX)).contains(&sound.left)
        && sound.samples == r.clock() / CYCLES_PER_SAMPLE
);

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::*;

    #[test]
    fn the_wave_is_the_counters_frequency_and_silent_unless_given_sound() {
        let mut sound = Sound::new();
        sound.set_counter(124);
        let mut out = Vec::new();
        sound.render(1000, &mut out);
        assert!(out.iter().all(|&level| level == 0.0), "off");
        sound.set_on(true);
        out.clear();
        sound.render(1000 + 2000, &mut out);
        let rises = out
            .windows(2)
            .filter(|pair| pair[0] == 0.0 && pair[1] == LEVEL)
            .count();
        // 1 MHz / (16 × 125) is 500 Hz: a rise each 500 samples at 250 kHz.
        assert!((3..=4).contains(&rises), "{rises}");
        assert_eq!(half_period(124), 250);
        assert_eq!(half_period(0), 2, "62.5 kHz");
    }
}
