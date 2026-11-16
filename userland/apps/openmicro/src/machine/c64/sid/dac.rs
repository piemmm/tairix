//! What turns the SID's digital values into levels: its R-2R ladder DACs —
//! the 6581's with arms 2.2 times the chain's resistance and no
//! termination, the 8580's true — and, for combined waveforms, the
//! pulldown each grounded bit exerts on its neighbours, as a model fitted
//! to sampled chips (libresidfp's parameters).

use alloc::boxed::Box;
use alloc::vec::Vec;

use super::Model;

/// Combined waveforms with their own pulldown: triangle and sawtooth,
/// pulse with each of those and with both, and noise with pulse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Combination {
    TriangleSaw,
    PulseTriangle,
    PulseSaw,
    PulseTriangleSaw,
    NoisePulse,
}

impl Combination {
    /// The combination waveform bits (triangle 1, sawtooth 2, pulse 4,
    /// noise 8) select, if they combine.
    pub(super) const fn of(waveform: u8) -> Option<Self> {
        if waveform & 0x0C == 0x0C {
            return Some(Self::NoisePulse);
        }
        match waveform {
            0x3 => Some(Self::TriangleSaw),
            0x5 => Some(Self::PulseTriangle),
            0x6 => Some(Self::PulseSaw),
            0x7 => Some(Self::PulseTriangleSaw),
            _ => None,
        }
    }

    const fn index(self) -> usize {
        self as usize
    }
}

/// How a bit's pull on another falls off with the distance between them.
#[derive(Clone, Copy, Debug)]
enum Falloff {
    Exponential,
    Linear,
    Quadratic,
}

impl Falloff {
    fn weight(self, distance: f32, bits: u8) -> f32 {
        let i = f32::from(bits);
        match self {
            Self::Exponential => (0..bits).fold(1.0, |weight, _| weight / distance),
            Self::Linear => 1.0 / (1.0 + i * distance),
            Self::Quadratic => 1.0 / (1.0 + i * i * distance),
        }
    }
}

/// A combination's fitted parameters: the level a bit must hold to read
/// set, the top bit's strength, the pulse's, and the falloff upward and
/// downward.
#[derive(Clone, Copy, Debug)]
struct Pulldown {
    falloff: Falloff,
    threshold: f32,
    top_bit: f32,
    pulse_strength: f32,
    upward: f32,
    downward: f32,
}

const fn pulldown(
    falloff: Falloff,
    threshold: f32,
    top_bit: f32,
    pulse_strength: f32,
    upward: f32,
    downward: f32,
) -> Pulldown {
    Pulldown {
        falloff,
        threshold,
        top_bit,
        pulse_strength,
        upward,
        downward,
    }
}

/// Each chip's combinations, in [`Combination`]'s order, as fitted to
/// an average 6581 (R3 0486S) and 8580 (R5 1088).
const PULLDOWNS: [[Pulldown; 5]; 2] = {
    use Falloff::{Exponential as E, Linear as L, Quadratic as Q};
    [
        [
            pulldown(E, 0.791_115_8, 1.060_534_8, 0.0, 1.979_229_6, 2.678_481_8),
            pulldown(L, 0.941_692_8, 1.0, 1.800_726_6, 0.033_124_88, 0.232_303_44),
            pulldown(
                L,
                1.093_946_8,
                1.423_32,
                3.442_516_3,
                0.079_730_116,
                0.102_444_135,
            ),
            pulldown(
                L,
                1.536_097_2,
                0.039_101_75,
                1.676_012_3,
                1.445_807_9,
                1.424_488_1,
            ),
            pulldown(E, 0.96, 1.0, 2.5, 1.1, 1.2),
        ],
        [
            pulldown(E, 0.853_578_3, 1.096_156_4, 0.0, 1.881_937_5, 6.807_949),
            pulldown(E, 0.929_835_6, 1.0, 1.128_368_1, 1.104_536_5, 1.480_657_5),
            pulldown(
                Q,
                0.911_938_6,
                0.996_440_4,
                1.227_807_4,
                0.000_117_214_3,
                0.189_484_76,
            ),
            pulldown(
                E,
                0.938_004_7,
                1.048_276_3,
                1.211_782_5,
                0.915_959,
                1.426_980_4,
            ),
            pulldown(E, 0.95, 1.0, 1.15, 1.0, 1.45),
        ],
    ]
};

/// Bits in a waveform value.
const WAVE_BITS: usize = 12;

impl Pulldown {
    /// The value a waveform value `wave` settles at with its bits pulling
    /// on one another.
    fn settle(&self, distances: &[f32; 2 * WAVE_BITS + 1], wave: u16) -> u16 {
        let mut bits = [0.0f32; WAVE_BITS];
        for (i, bit) in bits.iter_mut().enumerate() {
            *bit = f32::from(wave >> i & 1);
        }
        bits[WAVE_BITS - 1] *= self.top_bit;
        let mut value = 0;
        for sb in 0..WAVE_BITS {
            let (mut pull, mut total) = (0.0, 0.0);
            for (cb, &bit) in bits.iter().enumerate().filter(|&(cb, _)| cb != sb) {
                let weight = distances[sb + WAVE_BITS - cb];
                pull += (1.0 - bit) * weight;
                total += weight;
            }
            let pulled = (pull - self.pulse_strength) / total;
            let level = if bits[sb] > 0.0 { 1.0 - pulled } else { 0.0 };
            if level > self.threshold {
                value |= 1 << sb;
            }
        }
        value
    }

    fn table(&self) -> Box<[u16]> {
        let mut distances = [0.0; 2 * WAVE_BITS + 1];
        distances[WAVE_BITS] = 1.0;
        for i in 1..=WAVE_BITS {
            let steps = u8::try_from(i).unwrap_or(u8::MAX);
            distances[WAVE_BITS - i] = self.falloff.weight(self.upward, steps);
            distances[WAVE_BITS + i] = self.falloff.weight(self.downward, steps);
        }
        (0..1u16 << WAVE_BITS)
            .map(|wave| self.settle(&distances, wave))
            .collect()
    }
}

/// The output of an R-2R ladder of `bits` bits for each code: arms of
/// `arm` times the chain's resistance, a terminating arm at its foot if
/// `terminated`, read unloaded at its top and scaled so the full code
/// reads `2^bits - 1`.
fn ladder(bits: usize, arm: f64, terminated: bool) -> Box<[f64]> {
    let parallel = |a: f64, b: f64| {
        if a.is_infinite() {
            b
        } else if b.is_infinite() {
            a
        } else {
            a * b / (a + b)
        }
    };
    // Resistance to ground from each node through what lies below it, and
    // through what lies above it, every other arm grounded.
    let mut below = Vec::with_capacity(bits);
    let mut down = if terminated { arm } else { f64::INFINITY };
    for _ in 0..bits {
        below.push(down);
        down = 1.0 + parallel(arm, down);
    }
    let mut above = alloc::vec![f64::INFINITY; bits];
    for node in (0..bits.saturating_sub(1)).rev() {
        above[node] = 1.0 + parallel(arm, above[node + 1]);
    }
    let weights: Vec<f64> = (0..bits)
        .map(|bit| {
            let rest = parallel(below[bit], above[bit]);
            above[bit + 1..]
                .iter()
                .fold(rest / (arm + rest), |level, &up| {
                    let onward = parallel(arm, up);
                    level * onward / (1.0 + onward)
                })
        })
        .collect();
    let full: f64 = weights.iter().sum();
    let scale = f64::from((1u32 << bits) - 1) / full;
    (0..1usize << bits)
        .map(|code| {
            let sum: f64 = (0..bits)
                .filter(|&bit| code >> bit & 1 != 0)
                .map(|bit| weights[bit])
                .sum();
            sum * scale
        })
        .collect()
}

/// A chip's DACs and its combined waveforms' pulldown.
#[derive(Debug, PartialEq)]
pub(super) struct Tables {
    /// The waveform DAC's level for each 12-bit value.
    pub(super) wave: Box<[f64]>,
    /// The envelope DAC's level for each 8-bit value.
    pub(super) envelope: Box<[f64]>,
    pulldown: [Box<[u16]>; 5],
}

impl Tables {
    pub(super) fn new(model: Model) -> Self {
        let (arm, terminated, fitted) = match model {
            Model::Mos6581 => (2.20, false, &PULLDOWNS[0]),
            Model::Mos8580 => (2.00, true, &PULLDOWNS[1]),
        };
        Self {
            wave: ladder(WAVE_BITS, arm, terminated),
            envelope: ladder(8, arm, terminated),
            pulldown: core::array::from_fn(|i| fitted[i].table()),
        }
    }

    /// What `combination` makes of the digitally combined value `wave`.
    pub(super) fn pulldown(&self, combination: Combination, wave: u16) -> u16 {
        self.pulldown[combination.index()][usize::from(wave & 0x0FFF)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMBINATIONS: [Combination; 5] = [
        Combination::TriangleSaw,
        Combination::PulseTriangle,
        Combination::PulseSaw,
        Combination::PulseTriangleSaw,
        Combination::NoisePulse,
    ];

    #[test]
    fn a_terminated_ladder_with_arms_of_two_is_binary() {
        let levels = ladder(8, 2.0, true);
        for (code, &level) in levels.iter().enumerate() {
            let code = f64::from(u32::try_from(code).expect("small"));
            assert!((level - code).abs() < 1e-9, "{code}: {level}");
        }
    }

    #[test]
    fn the_6581_s_ladder_is_not_binary() {
        let levels = ladder(12, 2.2, false);
        assert!((levels[0x800] - 2048.0).abs() > 1.0);
        assert!((levels[0xFFF] - 4095.0).abs() < 1e-9);
    }

    #[test]
    fn combined_waveforms_lose_bits_and_never_gain_them() {
        let tables = Tables::new(Model::Mos6581);
        for combination in COMBINATIONS {
            for wave in [0x000u16, 0x123, 0x7FF, 0x800, 0xFFF] {
                let settled = tables.pulldown(combination, wave);
                assert_eq!(settled & !wave, 0, "{combination:?} {wave:03X}");
            }
        }
        assert_eq!(tables.pulldown(Combination::TriangleSaw, 0), 0);
    }

    #[test]
    fn the_waveform_bits_name_their_combination() {
        assert_eq!(Combination::of(0x3), Some(Combination::TriangleSaw));
        assert_eq!(Combination::of(0x7), Some(Combination::PulseTriangleSaw));
        assert_eq!(Combination::of(0xC), Some(Combination::NoisePulse));
        assert_eq!(Combination::of(0x4), None);
        assert_eq!(Combination::of(0x9), None);
    }
}
