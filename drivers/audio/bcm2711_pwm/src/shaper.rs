//! Third-order error-feedback noise shaping of PCM onto the PWM's duty levels.
//!
//! A PWM period carries one of `levels + 1` duties, about eight bits at the
//! jack's rate, which plain rounding would leave as a flat noise floor across
//! the whole band. The shaper feeds each sample's quantisation error back
//! through `(1 - z⁻¹)³`, so the noise falls away towards DC and rises towards
//! the PWM's own Nyquist rate, far above what is heard. Triangular dither of
//! one level inside the loop decorrelates the error from the signal, so quiet
//! material carries no tones. The output keeps headroom for the shaped error,
//! so the quantiser never clips and the loop never leaves its linear range.

use tairix_rng::{NonCryptoRng, RandU64};

/// Fractional bits of the fixed-point levels the loop runs in.
const FRACTION_BITS: u32 = 16;
const HALF_LEVEL: i64 = 1 << (FRACTION_BITS - 1);

/// Levels kept clear at each end: the error fed back through `3, -3, 1` is
/// under 1.5 levels a tap, so under 10.5 in all, plus a level of dither and
/// half of rounding.
pub const HEADROOM: u32 = 12;

/// The fewest levels a period may have: the headroom on both sides and as
/// much again for the signal.
pub const MIN_LEVELS: u32 = 4 * HEADROOM;

/// One channel's shaper.
#[derive(Debug)]
pub struct NoiseShaper {
    levels: u32,
    /// The duty of silence, fixed point.
    mid: i64,
    /// Levels a full-scale sample swings either side of silence.
    swing: u32,
    /// The last three errors, newest first, fixed point.
    errors: [i64; 3],
    dither: NonCryptoRng,
}

impl NoiseShaper {
    /// A shaper onto `levels` duty levels, its dither sequence fixed by
    /// `seed`; [`None`] below [`MIN_LEVELS`].
    #[must_use]
    pub fn new(levels: u32, seed: u64) -> Option<Self> {
        if levels < MIN_LEVELS {
            return None;
        }
        Some(Self {
            levels,
            mid: i64::from(levels / 2) << FRACTION_BITS,
            swing: (levels - 2 * HEADROOM) / 2,
            errors: [0; 3],
            dither: NonCryptoRng::seed_from_u64(seed),
        })
    }

    /// The duty of silence.
    #[must_use]
    pub const fn silence(&self) -> u32 {
        self.levels / 2
    }

    /// The levels a full-scale sample swings either side of silence.
    #[must_use]
    pub const fn swing(&self) -> u32 {
        self.swing
    }

    /// Forget the error history, as a stream starts.
    pub fn reset(&mut self) {
        self.errors = [0; 3];
    }

    /// The duty that carries `sample`, a full-scale signed 32-bit sample.
    pub fn duty(&mut self, sample: i32) -> u32 {
        let wanted =
            self.mid + ((i64::from(sample) * i64::from(self.swing)) >> (31 - FRACTION_BITS));
        let [e1, e2, e3] = self.errors;
        let shaped = wanted - 3 * e1 + 3 * e2 - e3;
        // Two uniform levels' worth, differenced: triangular over one level
        // either side.
        let [a0, a1, b0, b1, ..] = self.dither.next_u64().to_le_bytes();
        let dither =
            i64::from(u16::from_le_bytes([a0, a1])) - i64::from(u16::from_le_bytes([b0, b1]));
        let rounded = (shaped + dither + HALF_LEVEL) >> FRACTION_BITS;
        let duty = u32::try_from(rounded.max(0)).map_or(self.levels, |duty| duty.min(self.levels));
        self.errors = [(i64::from(duty) << FRACTION_BITS) - shaped, e1, e2];
        duty
    }
}

#[cfg(test)]
#[path = "shaper_tests.rs"]
mod tests;
