//! The forward transforms and the quantiser a residual is coded with: the
//! reference encoder's DCT and Walsh-Hadamard transform, whose rounding is
//! tuned to the decoder's inverse truncations so a block brought back comes
//! within a level of where it began, and a dead-zone quantiser that divides
//! by multiplying.

use super::{BLOCK_COEFFS, CATEGORY_BASES, CATEGORY_PROBS};

/// √2·sin(π/8) and √2·cos(π/8) at twelve bits of fixed point.
const SQRT2_SIN_PI8: i32 = 2217;
const SQRT2_COS_PI8: i32 = 5352;

/// The largest magnitude a token codes: the last category's base with every
/// extra bit set.
const MAX_LEVEL: i32 = CATEGORY_BASES[CATEGORY_BASES.len() - 1]
    + (1 << CATEGORY_PROBS[CATEGORY_PROBS.len() - 1].len())
    - 1;

/// The forward DCT of a 4×4 block of residual samples, in raster order.
pub(super) fn forward_dct(input: &[i32; BLOCK_COEFFS]) -> [i32; BLOCK_COEFFS] {
    let mut middle = [0i32; BLOCK_COEFFS];
    for (row, samples) in input.as_chunks::<4>().0.iter().enumerate() {
        let a = (samples[0] + samples[3]) * 8;
        let b = (samples[1] + samples[2]) * 8;
        let c = (samples[1] - samples[2]) * 8;
        let d = (samples[0] - samples[3]) * 8;
        middle[row * 4] = a + b;
        middle[row * 4 + 2] = a - b;
        middle[row * 4 + 1] = (c * SQRT2_SIN_PI8 + d * SQRT2_COS_PI8 + 14500) >> 12;
        middle[row * 4 + 3] = (d * SQRT2_SIN_PI8 - c * SQRT2_COS_PI8 + 7500) >> 12;
    }
    let mut output = [0i32; BLOCK_COEFFS];
    for column in 0..4 {
        let at = |row: usize| middle[row * 4 + column];
        let a = at(0) + at(3);
        let b = at(1) + at(2);
        let c = at(1) - at(2);
        let d = at(0) - at(3);
        output[column] = (a + b + 7) >> 4;
        output[8 + column] = (a - b + 7) >> 4;
        let odd = (c * SQRT2_SIN_PI8 + d * SQRT2_COS_PI8 + 12000) >> 16;
        output[4 + column] = odd + i32::from(d != 0);
        output[12 + column] = (d * SQRT2_SIN_PI8 - c * SQRT2_COS_PI8 + 51000) >> 16;
    }
    output
}

/// The forward Walsh-Hadamard transform of a macroblock's sixteen luma DC
/// coefficients, its blocks in raster order.
pub(super) fn forward_walsh(input: &[i32; BLOCK_COEFFS]) -> [i32; BLOCK_COEFFS] {
    let mut middle = [0i32; BLOCK_COEFFS];
    for (row, values) in input.as_chunks::<4>().0.iter().enumerate() {
        let a = (values[0] + values[2]) * 4;
        let d = (values[1] + values[3]) * 4;
        let c = (values[1] - values[3]) * 4;
        let b = (values[0] - values[2]) * 4;
        middle[row * 4] = a + d + i32::from(a != 0);
        middle[row * 4 + 1] = b + c;
        middle[row * 4 + 2] = b - c;
        middle[row * 4 + 3] = a - d;
    }
    let mut output = [0i32; BLOCK_COEFFS];
    for column in 0..4 {
        let at = |row: usize| middle[row * 4 + column];
        let a = at(0) + at(2);
        let d = at(1) + at(3);
        let c = at(1) - at(3);
        let b = at(0) - at(2);
        let shifted = |sum: i32| (sum + i32::from(sum < 0) + 3) >> 3;
        output[column] = shifted(a + d);
        output[4 + column] = shifted(b + c);
        output[8 + column] = shifted(b - c);
        output[12 + column] = shifted(a - d);
    }
    output
}

/// A quantiser step, and the reciprocal a coefficient is divided by it with.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) struct Step {
    step: u32,
    reciprocal: u64,
}

impl Step {
    /// The step `step`, held to one or more.
    pub(super) const fn new(step: i32) -> Self {
        let step = if step < 1 { 1 } else { step.unsigned_abs() };
        Self {
            step,
            reciprocal: (1u64 << 32).div_ceil(step as u64),
        }
    }

    /// The level `coefficient` quantises to: the nearest for a block's
    /// `first` coefficient, whose error spreads over the whole block, and
    /// with a dead zone of five eighths of a step for the rest, where a small
    /// one is more often noise than detail.
    pub(super) fn level(self, coefficient: i32, first: bool) -> i16 {
        let bias = if first {
            self.step / 2
        } else {
            self.step * 3 / 8
        };
        let held = u64::from(coefficient.unsigned_abs().saturating_add(bias));
        // Below the last level's next step the reciprocal divides exactly, as
        // `held` times its rounding error stays under 2³²; past it the level
        // is held at the last anyway.
        let magnitude = if held >= (MAX_LEVEL as u64 + 1) * u64::from(self.step) {
            MAX_LEVEL
        } else {
            i32::try_from((held * self.reciprocal) >> 32).unwrap_or(MAX_LEVEL)
        };
        let level = if coefficient < 0 {
            -magnitude
        } else {
            magnitude
        };
        i16::try_from(level).unwrap_or(0)
    }
}
