//! The depth stage: how a fragment's depth is compared with what is held,
//! how each depth format stores it, and the bias a polygon's depth is offset
//! by.
//!
//! A depth is stored in `0..=1`; one past that range, or not a number, is held
//! to it first. Every format's stored word orders as the depth it holds, so a
//! comparison reads the words of all of them alike. The mesh path's reversed
//! reciprocal [`Depth`](crate::mesh::Depth) is the reversed-Z configuration of
//! the same comparison: nearer is [`CompareOp::Greater`].

use tairix_util::mathf;

/// How a value is compared against what is held: a fragment's depth against
/// its attachment's, a stencil reference against the stencil held, or a
/// depth reference against a texel.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum CompareOp {
    /// Never passes.
    Never,
    /// Passes where the value is less than what is held.
    Less,
    /// Passes where they are equal.
    Equal,
    /// Passes where the value is less or equal.
    LessOrEqual,
    /// Passes where the value is greater.
    Greater,
    /// Passes where they differ.
    NotEqual,
    /// Passes where the value is greater or equal.
    GreaterOrEqual,
    /// Always passes.
    Always,
}

impl CompareOp {
    /// Whether `value` passes against `held`.
    #[must_use]
    pub fn passes<T: PartialOrd + Copy>(self, value: T, held: T) -> bool {
        match self {
            Self::Never => false,
            Self::Less => value < held,
            Self::Equal => value == held,
            Self::LessOrEqual => value <= held,
            Self::Greater => value > held,
            Self::NotEqual => value != held,
            Self::GreaterOrEqual => value >= held,
            Self::Always => true,
        }
    }
}

/// A depth format the stage stores.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum DepthFormat {
    /// Sixteen-bit unsigned normalised.
    Unorm16,
    /// Twenty-four-bit unsigned normalised: the depth of a format that keeps
    /// stencil beside it.
    Unorm24,
    /// Thirty-two-bit float.
    Float32,
}

/// Bits in a single-precision mantissa: what fixes a float depth's
/// resolution about its exponent.
const MANTISSA_BITS: i32 = 23;

impl DepthFormat {
    /// `depth` as the format stores it: held to `0..=1` — nought where it is
    /// not a number — and rounded to the nearest value the format holds.
    #[must_use]
    pub fn store(self, depth: f64) -> u32 {
        let depth = if depth > 0.0 {
            mathf::fmin(depth, 1.0)
        } else {
            0.0
        };
        match self {
            Self::Unorm16 => unorm(depth, 16),
            Self::Unorm24 => unorm(depth, 24),
            Self::Float32 => mathf::single(depth).to_bits(),
        }
    }

    /// The depth the stored word `stored` holds.
    #[must_use]
    pub fn load(self, stored: u32) -> f64 {
        match self {
            Self::Unorm16 => f64::from(stored & 0xffff) / f64::from(u16::MAX),
            Self::Unorm24 => f64::from(stored & 0x00ff_ffff) / f64::from(0x00ff_ffffu32),
            Self::Float32 => f64::from(f32::from_bits(stored)),
        }
    }

    /// The least difference in depth the format keeps distinct for a
    /// polygon whose depths reach `deepest`: what a constant depth bias is a
    /// multiple of. One step of a normalised format; for a float, a unit in
    /// the last place of the polygon's largest exponent.
    #[must_use]
    pub fn resolution(self, deepest: f64) -> f64 {
        match self {
            Self::Unorm16 => 1.0 / f64::from(u16::MAX),
            Self::Unorm24 => 1.0 / f64::from(0x00ff_ffffu32),
            Self::Float32 => {
                let bits = mathf::single(mathf::fabs(deepest)).to_bits();
                let exponent = i32::try_from((bits >> MANTISSA_BITS) & 0xff).unwrap_or(1);
                // A zero or subnormal depth takes the smallest normal exponent.
                power_of_two(exponent.max(1) - 127 - MANTISSA_BITS)
            }
        }
    }
}

/// `depth` in `0..=1` as a `bits`-bit unsigned normalised word.
fn unorm(depth: f64, bits: u32) -> u32 {
    let most = f64::from((1u32 << bits) - 1);
    u32::try_from(mathf::round_i32(depth * most)).unwrap_or(0)
}

/// `2^exponent`, exactly, for an exponent a double holds normally.
fn power_of_two(exponent: i32) -> f64 {
    let biased = u64::try_from(exponent + 1023).unwrap_or(1);
    f64::from_bits(biased << 52)
}

/// What a polygon's depth is offset by before it is tested.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct DepthBias {
    /// Multiples of the format's resolution.
    pub constant: f64,
    /// Multiples of the polygon's steepest change of depth a pixel.
    pub slope: f64,
    /// The furthest the offset may reach: above nought, its greatest; below
    /// it, its least; nought, unbounded.
    pub clamp: f64,
}

impl DepthBias {
    /// No offset.
    pub const NONE: Self = Self {
        constant: 0.0,
        slope: 0.0,
        clamp: 0.0,
    };

    /// The offset for a polygon whose depth changes by at most `slope` a
    /// pixel and reaches `deepest`, stored as `format`.
    #[must_use]
    pub fn offset(&self, format: DepthFormat, slope: f64, deepest: f64) -> f64 {
        let offset = slope * self.slope + format.resolution(deepest) * self.constant;
        if self.clamp > 0.0 {
            mathf::fmin(offset, self.clamp)
        } else if self.clamp < 0.0 {
            mathf::fmax(offset, self.clamp)
        } else {
            offset
        }
    }
}

#[cfg(test)]
#[path = "depth_tests.rs"]
mod tests;
