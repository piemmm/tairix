//! The SN76489 sound generator: three square-wave tone channels and a noise
//! channel, clocked at 4 MHz and dividing it by sixteen, so its output changes
//! only on 250 kHz updates. That output is produced exactly, one sample per
//! update, unipolar as the chip drives it; removing its DC and resampling it
//! for a host is the consumer's job.

use alloc::vec::Vec;

use crate::machine::CYCLES_PER_SAMPLE;

/// The level each attenuation step leaves, 2 dB apart, as a fraction of full
/// scale; the last step is silence.
const LEVELS: [f32; 16] = [
    1.0, 0.794_328, 0.630_957, 0.501_187, 0.398_107, 0.316_228, 0.251_189, 0.199_526, 0.158_489,
    0.125_893, 0.1, 0.079_433, 0.063_096, 0.050_119, 0.039_811, 0.0,
];

/// The noise shift register after any write to the noise control register.
const NOISE_SEED: u16 = 0x4000;

/// Noise rate 3: shift on tone 2's output rather than a fixed divider.
const NOISE_FROM_TONE_2: u8 = 0x03;

/// The longest a tone channel's divider counts: a period of zero divides by
/// 1024.
const LONGEST_TONE: u16 = 0x400;
/// The longest the noise channel's own divider counts.
const LONGEST_NOISE: u16 = 0x40;

/// A channel's divider and the square wave it produces.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Divider {
    /// The reload value; zero divides by 1024.
    period: u16,
    counter: u16,
    high: bool,
}

impl Divider {
    /// Count one update, returning whether the output rose.
    fn update(&mut self) -> bool {
        if self.counter > 1 {
            self.counter -= 1;
            return false;
        }
        self.counter = if self.period == 0 {
            LONGEST_TONE
        } else {
            self.period
        };
        self.high = !self.high;
        self.high
    }
}

/// The chip's registers and generators.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sn76489 {
    tones: [Divider; 3],
    /// The noise channel's own divider, at rates 0–2.
    noise: Divider,
    /// The noise control nibble: feedback (bit 2) and rate (bits 0–1).
    noise_control: u8,
    lfsr: u16,
    /// Attenuation of tones 0–2 and noise, 0 loudest, 15 silent.
    attenuation: [u8; 4],
    /// The register the last latch byte named.
    latched: u8,
    /// Updates performed since power-on.
    updates: u64,
}

impl Default for Sn76489 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sn76489 {
    /// A chip at power-on: every channel silent.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            tones: [Divider {
                period: 0,
                counter: 0,
                high: false,
            }; 3],
            noise: Divider {
                period: 0x10,
                counter: 0,
                high: false,
            },
            noise_control: 0,
            lfsr: NOISE_SEED,
            attenuation: [15; 4],
            latched: 0,
            updates: 0,
        }
    }

    /// The chip latches `byte` from its data bus.
    pub fn write(&mut self, byte: u8) {
        let (register, data) = if byte & 0x80 != 0 {
            self.latched = (byte >> 4) & 0x07;
            (self.latched, byte & 0x0F)
        } else {
            (self.latched, byte)
        };
        match register {
            0 | 2 | 4 => {
                let tone = &mut self.tones[usize::from(register >> 1)];
                tone.period = if byte & 0x80 != 0 {
                    (tone.period & 0x3F0) | u16::from(data)
                } else {
                    (tone.period & 0x00F) | (u16::from(data & 0x3F) << 4)
                };
            }
            6 => {
                self.noise_control = data & 0x07;
                self.lfsr = NOISE_SEED;
                self.noise.period = match self.noise_control & 0x03 {
                    0 => 0x10,
                    1 => 0x20,
                    _ => LONGEST_NOISE,
                };
            }
            _ => self.attenuation[usize::from(register >> 1)] = data & 0x0F,
        }
    }

    /// Updates performed since power-on.
    #[must_use]
    pub const fn updates(&self) -> u64 {
        self.updates
    }

    /// Run the chip until it has performed `until` updates, appending each
    /// update's output to `out`: the four channels' levels, a quarter of full
    /// scale each, summed.
    pub fn render(&mut self, until: u64, out: &mut Vec<f32>) {
        let mut levels = [0.0f32; 4];
        for (level, &attenuation) in levels.iter_mut().zip(&self.attenuation) {
            *level = LEVELS[usize::from(attenuation)] * 0.25;
        }
        while self.updates < until {
            self.step();
            let tones: f32 = self
                .tones
                .iter()
                .zip(&levels)
                .filter(|(tone, _)| tone.high)
                .map(|(_, level)| level)
                .sum();
            let noise = if self.lfsr & 1 != 0 { levels[3] } else { 0.0 };
            out.push(tones + noise);
        }
    }

    /// Run the chip until it has performed `until` updates, discarding its
    /// output, as a muted or unpaced machine does.
    pub fn skip(&mut self, until: u64) {
        while self.updates < until {
            self.step();
        }
    }

    fn step(&mut self) {
        let mut tone_2_rose = false;
        for (index, tone) in self.tones.iter_mut().enumerate() {
            let rose = tone.update();
            if index == 2 {
                tone_2_rose = rose;
            }
        }
        let noise_clocked = if self.noise_control & 0x03 == NOISE_FROM_TONE_2 {
            tone_2_rose
        } else {
            self.noise.update()
        };
        if noise_clocked {
            self.shift_noise();
        }
        self.updates += 1;
    }

    /// Shift the noise register once: white noise feeds back bits 0 and 1,
    /// periodic noise bit 0 alone, into bit 14.
    fn shift_noise(&mut self) {
        let feedback = if self.noise_control & 0x04 != 0 {
            (self.lfsr ^ (self.lfsr >> 1)) & 1
        } else {
            self.lfsr & 1
        };
        self.lfsr = (self.lfsr >> 1) | (feedback << 14);
    }
}

crate::snapshot::fields!(Divider {
    period,
    counter,
    high
});
crate::snapshot::fields!(
    Sn76489 {
        tones,
        noise,
        noise_control,
        lfsr,
        attenuation,
        latched,
        updates,
    },
    check | chip,
    r | chip.restorable(r.clock())
);

impl Sn76489 {
    /// Whether a restored chip holds only what one can, having performed every
    /// update up to the machine's time `clock`, as a snapshot leaves it.
    fn restorable(&self, clock: u64) -> bool {
        let tones = self
            .tones
            .iter()
            .all(|tone| tone.period < LONGEST_TONE && tone.counter <= LONGEST_TONE);
        let noise = matches!(self.noise.period, 0x10 | 0x20 | LONGEST_NOISE)
            && self.noise.counter <= LONGEST_NOISE;
        // The register is never all zeros: no shift reaches that from its seed.
        tones
            && noise
            && self.noise_control <= 0x07
            && self.lfsr != 0
            && self.lfsr < 0x8000
            && self
                .attenuation
                .iter()
                .all(|&level| usize::from(level) < LEVELS.len())
            && self.latched <= 0x07
            && self.updates == clock / CYCLES_PER_SAMPLE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rising edges of tone 0 over `updates` updates.
    fn edges(chip: &mut Sn76489, updates: u64) -> usize {
        let mut out = Vec::new();
        chip.render(chip.updates() + updates, &mut out);
        out.windows(2)
            .filter(|pair| pair[0] == 0.0 && pair[1] > 0.0)
            .count()
    }

    fn loud_tone(period_low: u8, period_high: u8) -> Sn76489 {
        let mut chip = Sn76489::new();
        chip.write(0x80 | period_low);
        chip.write(period_high);
        chip.write(0x90);
        chip
    }

    #[test]
    fn a_latch_byte_sets_the_period_s_low_bits_and_a_data_byte_its_high_six() {
        // Period 0x123, 291: 250 kHz over twice that is 429.6 Hz.
        let mut chip = loud_tone(0x03, 0x12);
        assert_eq!(edges(&mut chip, 250_000), 429);
    }

    #[test]
    fn a_zero_period_divides_by_1024() {
        let mut chip = loud_tone(0x00, 0x00);
        assert_eq!(edges(&mut chip, 250_000), 122);
    }

    #[test]
    fn attenuation_falls_two_decibels_a_step_and_fifteen_is_silent() {
        let mut chip = loud_tone(0x00, 0x01);
        let mut out = Vec::new();
        chip.render(64, &mut out);
        let loudest = out.iter().copied().fold(0.0, f32::max);
        assert!((loudest - 0.25).abs() < 1e-6);
        chip.write(0x91);
        out.clear();
        chip.render(chip.updates() + 64, &mut out);
        let step = out.iter().copied().fold(0.0, f32::max) / loudest;
        assert!((20.0 * step.log10() + 2.0).abs() < 1e-3);
        chip.write(0x9F);
        out.clear();
        chip.render(chip.updates() + 2048, &mut out);
        assert!(out.iter().all(|&sample| sample == 0.0));
    }

    /// The noise register's period: shifts until it returns to the seed.
    fn noise_period(control: u8) -> usize {
        let mut chip = Sn76489::new();
        chip.write(0xE0 | control);
        (1..=40_000)
            .find(|_| {
                chip.shift_noise();
                chip.lfsr == NOISE_SEED
            })
            .unwrap()
    }

    #[test]
    fn white_noise_runs_32767_shifts_and_periodic_noise_15() {
        assert_eq!(noise_period(0x04), 32_767);
        assert_eq!(noise_period(0x00), 15);
    }

    #[test]
    fn writing_the_noise_register_reseeds_it() {
        let mut chip = Sn76489::new();
        chip.write(0xE4);
        chip.skip(10_000);
        assert_ne!(chip.lfsr, NOISE_SEED);
        chip.write(0xE4);
        assert_eq!(chip.lfsr, NOISE_SEED);
    }

    #[test]
    fn noise_rates_divide_by_512_1024_and_2048_or_follow_tone_2() {
        let shifts = |control: u8| {
            let mut chip = Sn76489::new();
            chip.write(0xC0 | 0x08);
            chip.write(0x00);
            chip.write(0xE0 | control);
            let mut count = 0;
            let mut last = chip.lfsr;
            for _ in 0..250_000 {
                chip.step();
                if chip.lfsr != last {
                    count += 1;
                    last = chip.lfsr;
                }
            }
            count
        };
        // Periodic noise from the seed shifts a set bit along, so every shift
        // changes the register; the dividers start at zero, so the first
        // shift comes on the first update.
        assert_eq!(shifts(0), 250_000 / 32 + 1);
        assert_eq!(shifts(1), 250_000 / 64 + 1);
        assert_eq!(shifts(2), 250_000 / 128 + 1);
        // Tone 2 at period 8 rises every sixteen updates.
        assert_eq!(shifts(3), 250_000 / 16);
    }

    #[test]
    fn skipping_leaves_the_chip_where_rendering_would() {
        let mut rendered = loud_tone(0x07, 0x05);
        rendered.write(0xE5);
        let mut skipped = rendered.clone();
        let mut out = Vec::new();
        rendered.render(12_345, &mut out);
        skipped.skip(12_345);
        assert_eq!(rendered, skipped);
        assert_eq!(out.len(), 12_345);
    }
}
