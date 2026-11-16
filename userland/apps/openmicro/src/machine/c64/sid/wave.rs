//! A voice's oscillator and waveforms: the 24-bit phase accumulator, hard
//! sync and ring modulation from its neighbour, the noise shift register,
//! and the twelve bits the selected waveforms put on the DAC, combined
//! digitally and then pulled down as the chip's bits pull on each other.

use super::dac::{Combination, Tables};
use super::Model;
use crate::snapshot;

/// Waveform select bits, as the control register's high nybble.
mod select {
    pub const TRIANGLE: u8 = 0x1;
    pub const SAW: u8 = 0x2;
    pub const PULSE: u8 = 0x4;
    pub const NOISE: u8 = 0x8;
}

/// Control register bits below the waveforms.
pub(super) mod control {
    pub const GATE: u8 = 0x01;
    pub const SYNC: u8 = 0x02;
    pub const RING: u8 = 0x04;
    pub const TEST: u8 = 0x08;
}

const ACCUMULATOR_MASK: u32 = 0x00FF_FFFF;
const MSB: u32 = 0x0080_0000;
/// The accumulator bit whose rise clocks the noise register.
const NOISE_CLOCK: u32 = 0x0008_0000;
const SHIFT_MASK: u32 = 0x007F_FFFF;

/// The shift register's bits the noise waveform outputs, as waveform bits
/// 11 down to 4.
const NOISE_TAPS: [(u32, u16); 8] = [
    (20, 11),
    (18, 10),
    (14, 9),
    (11, 8),
    (9, 7),
    (5, 6),
    (2, 5),
    (0, 4),
];

/// Cycles a chip's held test bit takes to fill the shift register with
/// ones, the first bit and each after; and how long a waveform left
/// floating holds its last value, at first and per bit after.
struct Fades {
    shift_start: u32,
    shift_bit: u32,
    floating_start: u32,
    floating_bit: u32,
}

const fn fades(model: Model) -> Fades {
    match model {
        Model::Mos6581 => Fades {
            shift_start: 35_000,
            shift_bit: 1_000,
            floating_start: 182_000,
            floating_bit: 1_500,
        },
        Model::Mos8580 => Fades {
            shift_start: 2_519_864,
            shift_bit: 315_000,
            floating_start: 4_400_000,
            floating_bit: 50_000,
        },
    }
}

/// A voice's oscillator and waveform generator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each is an independent bit of the control register or the oscillator"
)]
pub(super) struct Wave {
    accumulator: u32,
    pub(super) frequency: u16,
    /// The pulse width, twelve bits.
    pub(super) width: u16,
    waveform: u8,
    test: bool,
    ring: bool,
    sync: bool,
    /// The accumulator's top bit rose in the last clock.
    msb_rising: bool,
    shift_register: u32,
    /// Cycles until the noise register shifts after bit 19 rose.
    shift_pipeline: u8,
    /// Cycles until the held test bit fills another bit of the register.
    shift_reset: u32,
    noise: u16,
    /// The pulse comparator's result, a cycle late.
    pulse: u16,
    /// Cycles until a floating DAC input loses another bit.
    floating: u32,
    output: u16,
    /// What OSC3 reads: the 8580's triangle and sawtooth a cycle late.
    osc3: u16,
    tri_saw: u16,
}

snapshot::fields!(
    Wave {
        accumulator,
        frequency,
        width,
        waveform,
        test,
        ring,
        sync,
        msb_rising,
        shift_register,
        shift_pipeline,
        shift_reset,
        noise,
        pulse,
        floating,
        output,
        osc3,
        tri_saw
    },
    check | w | w.accumulator <= ACCUMULATOR_MASK
        && w.width <= 0x0FFF
        && w.waveform <= 0x0F
        && w.shift_register <= SHIFT_MASK
        && w.shift_pipeline <= 2
        && w.noise <= 0x0FFF
        && (w.pulse == 0 || w.pulse == 0x0FFF)
        && w.output <= 0x0FFF
        && w.osc3 <= 0x0FFF
        && w.tri_saw <= 0x0FFF
);

/// The triangle for the accumulator's top twelve bits `index`: folded on
/// its top bit, which it then drops.
const fn triangle(index: u16) -> u16 {
    let folded = if index & 0x800 == 0 {
        index
    } else {
        index ^ 0x0FFF
    };
    (folded << 1) & 0x0FFF
}

impl Wave {
    /// A voice after reset; the accumulator keeps whatever it held.
    pub(super) const fn new() -> Self {
        let mut wave = Self {
            accumulator: 0,
            frequency: 0,
            width: 0,
            waveform: 0,
            test: false,
            ring: false,
            sync: false,
            msb_rising: false,
            // Reset leaves the register to clock once as it is released.
            shift_register: 0x7F_FFFE,
            shift_pipeline: 0,
            shift_reset: 0,
            noise: 0,
            pulse: 0x0FFF,
            floating: 0,
            output: 0,
            osc3: 0,
            tri_saw: 0,
        };
        wave.noise = wave.noise_output();
        wave
    }

    pub(super) const fn reset(&mut self) {
        let accumulator = self.accumulator;
        *self = Self::new();
        self.accumulator = accumulator;
    }

    pub(super) const fn accumulator(&self) -> u32 {
        self.accumulator
    }

    pub(super) const fn msb_rising(&self) -> bool {
        self.msb_rising
    }

    pub(super) const fn sync(&self) -> bool {
        self.sync
    }

    pub(super) const fn synchronise(&mut self) {
        self.accumulator = 0;
    }

    pub(super) const fn output(&self) -> u16 {
        self.output
    }

    /// OSC3's reading: the output's top eight bits.
    pub(super) const fn osc3(&self) -> u8 {
        (self.osc3 >> 4).to_le_bytes()[0]
    }

    const fn noise_output(&self) -> u16 {
        let mut noise = 0;
        let mut i = 0;
        while i < NOISE_TAPS.len() {
            let (from, to) = NOISE_TAPS[i];
            if self.shift_register >> from & 1 != 0 {
                noise |= 1 << to;
            }
            i += 1;
        }
        noise
    }

    fn clock_shift_register(&mut self) {
        let bit = ((self.shift_register >> 22) ^ (self.shift_register >> 17)) & 1;
        self.shift_register = ((self.shift_register << 1) | bit) & SHIFT_MASK;
        self.noise = self.noise_output();
    }

    /// Write `control`, the control register.
    pub(super) fn write_control(&mut self, model: Model, value: u8) {
        let was_waveform = self.waveform;
        let was_test = self.test;
        self.waveform = value >> 4;
        self.test = value & control::TEST != 0;
        self.ring = value & control::RING != 0;
        self.sync = value & control::SYNC != 0;
        if self.test && !was_test {
            // The accumulator clears, and the shift register's cells, joined
            // for shifting, slowly fill with ones.
            self.accumulator = 0;
            self.shift_pipeline = 0;
            self.shift_reset = fades(model).shift_start;
            self.pulse = 0x0FFF;
        } else if was_test && !self.test {
            // Releasing the test bit completes a shift with test feeding bit 22.
            let bit = (!self.shift_register >> 17) & 1;
            self.shift_register = ((self.shift_register << 1) | bit) & SHIFT_MASK;
            self.noise = self.noise_output();
        }
        if self.waveform == 0 && was_waveform != 0 {
            self.floating = fades(model).floating_start;
        }
    }

    /// The oscillator's clock.
    pub(super) fn clock(&mut self, model: Model) {
        if self.test {
            if self.shift_reset != 0 {
                self.shift_reset -= 1;
                if self.shift_reset == 0 {
                    self.shift_register |= 1;
                    self.shift_register |= self.shift_register << 1;
                    self.shift_register &= SHIFT_MASK;
                    self.noise = self.noise_output();
                    if self.shift_register != SHIFT_MASK {
                        self.shift_reset = fades(model).shift_bit;
                    }
                }
            }
            self.pulse = 0x0FFF;
            return;
        }
        let next = (self.accumulator + u32::from(self.frequency)) & ACCUMULATOR_MASK;
        let rose = !self.accumulator & next;
        self.accumulator = next;
        self.msb_rising = rose & MSB != 0;
        // The register shifts two cycles after bit 19 rises.
        if rose & NOISE_CLOCK != 0 {
            self.shift_pipeline = 2;
        } else if self.shift_pipeline != 0 {
            self.shift_pipeline -= 1;
            if self.shift_pipeline == 0 {
                self.clock_shift_register();
            }
        }
    }

    /// The waveform output for this cycle, the ring modulator reading
    /// `source`, the voice that also syncs this one.
    pub(super) fn update_output(&mut self, model: Model, tables: &Tables, source: u32) {
        if self.waveform == 0 {
            if self.floating != 0 {
                self.floating -= 1;
                if self.floating == 0 {
                    self.output &= self.output >> 1;
                    self.osc3 = self.output;
                    if self.output != 0 {
                        self.floating = fades(model).floating_bit;
                    }
                }
            }
        } else {
            // Ring modulation puts the top bit through the source's top bit,
            // inverted, when the sawtooth is off.
            let ring = if self.ring && self.waveform & select::SAW == 0 {
                !source & MSB
            } else {
                0
            };
            let index = u16::try_from((self.accumulator ^ ring) >> 12).unwrap_or(0x0FFF);
            let saw = index;
            let shape = match self.waveform & (select::TRIANGLE | select::SAW) {
                0 => 0x0FFF,
                select::TRIANGLE => triangle(index),
                select::SAW => saw,
                _ => saw & (saw << 1),
            };
            let pulse = if self.waveform & select::PULSE != 0 {
                self.pulse
            } else {
                0x0FFF
            };
            let noise = if self.waveform & select::NOISE != 0 {
                self.noise
            } else {
                0x0FFF
            };
            let digital = shape & pulse & noise;
            let output = Combination::of(self.waveform)
                .map_or(digital, |combination| tables.pulldown(combination, digital));
            self.output = output;
            if model == Model::Mos8580 && self.waveform & (select::TRIANGLE | select::SAW) != 0 {
                self.osc3 = self.tri_saw & pulse & noise;
                self.tri_saw = shape;
            } else {
                self.osc3 = output;
            }
            // On the 6581 a combined waveform with the sawtooth can pull the
            // accumulator's top bit low.
            if model == Model::Mos6581
                && self.waveform & select::SAW != 0
                && self.waveform & !select::SAW != 0
            {
                self.accumulator &= (u32::from(output) << 12) | (!MSB & ACCUMULATOR_MASK);
            }
            if self.waveform > select::NOISE && !self.test && self.shift_pipeline != 1 {
                self.write_back_noise();
            }
        }
        self.pulse = if self.accumulator >> 12 >= u32::from(self.width) {
            0x0FFF
        } else {
            0
        };
    }

    /// A waveform combined with noise drives its zeros back into the
    /// shift register, where no one can set them again.
    fn write_back_noise(&mut self) {
        for (from, to) in NOISE_TAPS {
            if self.output >> to & 1 == 0 {
                self.shift_register &= !(1 << from);
            }
        }
        self.noise &= self.output;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tables() -> Tables {
        Tables::new(Model::Mos8580)
    }

    /// A voice with `control` written and `frequency` set.
    fn voice(control: u8, frequency: u16) -> Wave {
        let mut wave = Wave::new();
        wave.frequency = frequency;
        wave.write_control(Model::Mos8580, control);
        wave
    }

    #[test]
    fn the_sawtooth_is_the_accumulator_s_top_twelve_bits() {
        let tables = tables();
        let mut wave = voice(0x20, 0x1000);
        for _ in 0..3 {
            wave.clock(Model::Mos8580);
        }
        wave.update_output(Model::Mos8580, &tables, 0);
        assert_eq!(wave.output(), 0x003);
    }

    #[test]
    fn the_triangle_folds_on_the_top_bit() {
        assert_eq!(triangle(0x000), 0x000);
        assert_eq!(triangle(0x7FF), 0xFFE);
        assert_eq!(triangle(0x800), 0xFFE);
        assert_eq!(triangle(0xFFF), 0x000);
    }

    #[test]
    fn the_pulse_compares_a_cycle_late() {
        let tables = tables();
        let mut wave = voice(0x40, 0x8000);
        wave.width = 0x800;
        wave.clock(Model::Mos8580);
        wave.update_output(Model::Mos8580, &tables, 0);
        assert_eq!(
            wave.output(),
            0x0FFF,
            "the comparator still holds the test bit's high"
        );
        wave.clock(Model::Mos8580);
        wave.update_output(Model::Mos8580, &tables, 0);
        assert_eq!(wave.output(), 0, "accumulator $008000 is under the width");
    }

    #[test]
    fn the_test_bit_holds_the_accumulator_at_zero() {
        let mut wave = voice(0x28, 0x1234);
        for _ in 0..100 {
            wave.clock(Model::Mos8580);
        }
        assert_eq!(wave.accumulator(), 0);
    }

    #[test]
    fn noise_shifts_two_cycles_after_bit_19_rises() {
        let mut wave = voice(0x80, 0);
        wave.accumulator = 0x07_FFFF;
        wave.frequency = 1;
        let before = wave.shift_register;
        wave.clock(Model::Mos8580);
        wave.frequency = 0;
        wave.clock(Model::Mos8580);
        assert_eq!(wave.shift_register, before);
        wave.clock(Model::Mos8580);
        assert_ne!(wave.shift_register, before);
    }

    #[test]
    fn ring_modulation_inverts_the_triangle_with_the_source_s_top_bit() {
        let tables = tables();
        let mut plain = voice(0x10, 0);
        plain.accumulator = 0x10_0000;
        plain.update_output(Model::Mos8580, &tables, 0);
        let mut ringed = voice(0x14, 0);
        ringed.accumulator = 0x10_0000;
        ringed.update_output(Model::Mos8580, &tables, 0);
        assert_eq!(plain.output(), triangle(0x100));
        assert_ne!(ringed.output(), plain.output());
    }

    #[test]
    fn noise_combined_with_another_waveform_locks_its_bits_at_zero() {
        let tables = tables();
        let mut wave = voice(0xA0, 0);
        wave.accumulator = 0;
        wave.update_output(Model::Mos8580, &tables, 0);
        assert_eq!(wave.output(), 0);
        assert_eq!(wave.noise_output(), 0, "every tap cleared");
    }
}
