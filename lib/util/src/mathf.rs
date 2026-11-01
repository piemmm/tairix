//! Bounded `f64` maths for `no_std` geometry, and the rounding to single
//! precision and the single-precision smoothstep its `f32` callers need.
//!
//! `floor`, `sqrt`, `sin` and friends live in `std`, where they call the
//! platform libm, so a `no_std` crate cannot reach them. One first-party copy
//! here keeps an external libm out of the trusted computing base and makes
//! every consumer — the glyph rasteriser, the SVG decoder, the figure engine,
//! the world generator — round and rotate identically on every target.
//!
//! That is a cross-target contract: the same source yields the same bits on
//! `x86_64`, `aarch64`, `riscv64` and `wasm32`. So everything here is built
//! from what IEEE 754 defines exactly — the basic arithmetic, plus the square
//! root and integer rounding every conforming implementation must round
//! correctly. [`sqrt`], [`floor`] and [`ceil`] therefore take the toolchain's
//! own forms: an instruction where the target has one, and otherwise the
//! correctly rounded routine of the compiler runtime (the SSE2 `x86_64`
//! baseline has no rounding instruction) — part of the toolchain, not a libm.
//! Those forms are reachable from `core` only as `core::f64::math`, behind
//! `core_float_math`; once they are stable as inherent methods the calls
//! become `x.sqrt()`, `x.floor()` and `x.ceil()` and the gate goes.
//!
//! The transcendentals have no exact form anywhere, so each is evaluated in
//! one fixed order of basic operations, with no fused multiply-add: fdlibm's
//! range reductions and minimax polynomials. Each is within an ulp of the true
//! value but the tangent and the hyperbolic functions, within three, and
//! `atan2`, within two, as its quotient rounds before the arctangent is taken.
//! The hyperbolic ones take `e^x - 1` and `ln(1 + x)` through fdlibm's `expm1`
//! and `log1p`, so they are as close near zero as anywhere.
//!
//! Every function is total: it returns a finite answer for every finite input
//! and a defined one for the degenerate cases (a negative square root, a
//! vertical `atan2`, an out-of-domain `acos`, an infinite or `NaN` angle), so
//! no caller has to guard against a `NaN` it cannot render.

// fdlibm's kernels and constants are used under its notices:
//
// Copyright (C) 1993 by Sun Microsystems, Inc. All rights reserved.
//
// Developed at SunSoft, a Sun Microsystems, Inc. business.
// Permission to use, copy, modify, and distribute this
// software is freely granted, provided that this notice
// is preserved.
//
// Copyright (C) 2004 by Sun Microsystems, Inc. All rights reserved.
//
// Permission to use, copy, modify, and distribute this
// software is freely granted, provided that this notice
// is preserved.

use core::f64::consts::{FRAC_2_PI, FRAC_PI_2, FRAC_PI_4, LN_2, LOG2_E, PI};
use core::f64::math;

/// The largest integer not greater than `x`, and `0.0` for `NaN`.
#[must_use]
pub fn floor(x: f64) -> f64 {
    if x.is_nan() {
        0.0
    } else {
        math::floor(x)
    }
}

/// The smallest integer not less than `x`, and `0.0` for `NaN`.
#[must_use]
pub fn ceil(x: f64) -> f64 {
    if x.is_nan() {
        0.0
    } else {
        math::ceil(x)
    }
}

/// `x` rounded toward zero, and `0.0` for `NaN`.
#[must_use]
pub fn trunc(x: f64) -> f64 {
    if x.is_nan() {
        0.0
    } else {
        math::trunc(x)
    }
}

/// `x` rounded to the nearest integer, ties to even, and `0.0` for `NaN`.
#[must_use]
pub fn round_even(x: f64) -> f64 {
    if x.is_nan() {
        0.0
    } else {
        math::round_ties_even(x)
    }
}

/// The magnitude of `x`.
#[must_use]
pub fn fabs(x: f64) -> f64 {
    if x < 0.0 {
        -x
    } else {
        x
    }
}

/// Round `x` to the nearest integer, halves toward positive infinity, and
/// `0.0` for `NaN`.
///
/// Decided on `x - floor(x)`, which is exact wherever it falls below a half,
/// so the choice never errs; `floor(x + 0.5)` rounds the sum first and so takes
/// `0.49999999999999994`, and every odd integer past 2^52, up by one.
#[must_use]
pub fn round(x: f64) -> f64 {
    let down = floor(x);
    if x - down >= 0.5 {
        down + 1.0
    } else {
        down
    }
}

/// The greater of `a` and `b` by one compare and select, where `f64::max`'s
/// `NaN` handling takes several instructions: `b` where the two are unordered.
#[inline]
#[must_use]
pub const fn fmax(a: f64, b: f64) -> f64 {
    if a > b {
        a
    } else {
        b
    }
}

/// The lesser of `a` and `b` by one compare and select, where `f64::min`'s
/// `NaN` handling takes several instructions: `b` where the two are unordered.
#[inline]
#[must_use]
pub const fn fmin(a: f64, b: f64) -> f64 {
    if a < b {
        a
    } else {
        b
    }
}

/// `x` clamped to `lo..=hi` (with `lo <= hi`), and `lo` for `NaN`.
#[must_use]
pub fn clamp(x: f64, lo: f64, hi: f64) -> f64 {
    fmin(fmax(x, lo), hi)
}

/// The smoothstep `3t² − 2t³` of `t` held to `0..=1`, and `0.0` for `NaN`:
/// flat at both ends, so a ramp built on it meets what lies either side
/// without a crease.
#[must_use]
pub const fn smoothstep(t: f64) -> f64 {
    let t = if t > 0.0 { fmin(t, 1.0) } else { 0.0 };
    t * t * (3.0 - 2.0 * t)
}

/// [`smoothstep`] in single precision, for callers that keep their values
/// in `f32`.
#[must_use]
pub const fn smoothstepf(t: f32) -> f32 {
    let t = if t > 1.0 {
        1.0
    } else if t > 0.0 {
        t
    } else {
        0.0
    };
    t * t * (3.0 - 2.0 * t)
}

/// `x` rounded to the nearest single-precision value, ties to even, and
/// infinite past single precision's range.
#[allow(
    clippy::cast_possible_truncation,
    reason = "rounding to single precision is the conversion asked for, and \
              IEEE 754 defines it exactly"
)]
#[must_use]
pub const fn single(x: f64) -> f32 {
    x as f32
}

/// `x` as an IEEE 754 half, rounded to nearest with ties to even: infinite
/// past the half's range, and a `NaN` quiet.
#[must_use]
pub const fn to_half(x: f32) -> u16 {
    let bits = x.to_bits();
    let sign = (bits >> 16) & 0x8000;
    let exponent = (bits >> 23) & 0xff;
    let mantissa = bits & 0x007f_ffff;
    let magnitude = if exponent == 0xff {
        if mantissa == 0 {
            0x7c00
        } else {
            0x7e00 | (mantissa >> 13)
        }
    } else if exponent >= 127 + 16 {
        0x7c00
    } else if exponent < 127 - 25 {
        0
    } else {
        // A normal half keeps its leading one at bit 10 after a shift of 13;
        // a subnormal shifts further, by how far below the normals it lies.
        let normal = exponent >= 127 - 14;
        let shift = if normal {
            13
        } else {
            13 + (127 - 14 - exponent)
        };
        let significand = mantissa | 0x0080_0000;
        let kept = significand >> shift;
        let rest = significand & ((1 << shift) - 1);
        let halfway = 1 << (shift - 1);
        let up = rest > halfway || (rest == halfway && kept & 1 == 1);
        let rounded = kept + up as u32;
        // A normal's leading one is folded into its exponent, so a carry out
        // of the mantissa steps the exponent, to infinity at the top.
        let magnitude = if normal {
            ((exponent - (127 - 15)) << 10) + rounded - 0x400
        } else {
            rounded
        };
        if magnitude > 0x7c00 {
            0x7c00
        } else {
            magnitude
        }
    };
    let [low, high, _, _] = (sign | magnitude).to_le_bytes();
    u16::from_le_bytes([low, high])
}

/// `x` as an IEEE 754 half, rounded once to nearest with ties to even. The
/// single it passes through is rounded to odd, which keeps every bit the
/// second rounding needs: rounding to nearest twice would round some values
/// just past a half's tie onto the tie, then to even.
#[must_use]
fn double_to_half(x: f64) -> u16 {
    let nearest = single(x);
    if !nearest.is_finite() || f64::from(nearest).to_bits() == x.to_bits() {
        return to_half(nearest);
    }
    let mut bits = nearest.to_bits();
    if f64::from(nearest).abs() > x.abs() {
        bits -= 1;
    }
    to_half(f32::from_bits(bits | 1))
}

/// The IEEE 754 half `bits` as a single, exactly: every half is one.
#[must_use]
pub const fn from_half(bits: u16) -> f32 {
    let bits = bits as u32;
    let sign = (bits & 0x8000) << 16;
    let exponent = (bits >> 10) & 0x1f;
    let mantissa = bits & 0x3ff;
    let magnitude = if exponent == 0x1f {
        0x7f80_0000 | (mantissa << 13)
    } else if exponent != 0 {
        ((exponent + (127 - 15)) << 23) | (mantissa << 13)
    } else if mantissa == 0 {
        0
    } else {
        // A subnormal half is normal as a single: shift its leading one to
        // the implicit place, lowering the exponent by as much.
        let lead = mantissa.leading_zeros() - (32 - 10);
        let normalised = (mantissa << (lead + 1)) & 0x3ff;
        ((127 - 15 - lead) << 23) | (normalised << 13)
    };
    f32::from_bits(sign | magnitude)
}

/// The float `width` bits wide held in the low bits of `bits`, as a double,
/// exactly: a half at 16, a double at 64, and a single at any other width.
#[must_use]
pub fn float_of(width: u32, bits: u64) -> f64 {
    match width {
        16 => f64::from(from_half(u16::try_from(bits & 0xffff).unwrap_or(0))),
        64 => f64::from_bits(bits),
        _ => f64::from(f32::from_bits(
            u32::try_from(bits & 0xffff_ffff).unwrap_or(0),
        )),
    }
}

/// `x` rounded once to nearest, ties to even, to a float `width` bits wide,
/// as its bits: a half at 16, a double at 64, and a single at any other width.
/// A `NaN` is that width's one quiet `NaN`, since the sign and payload a cast
/// keeps differ between targets.
#[must_use]
pub fn float_bits(width: u32, x: f64) -> u64 {
    match width {
        16 if x.is_nan() => 0x7e00,
        64 if x.is_nan() => 0x7ff8_0000_0000_0000,
        _ if x.is_nan() => 0x7fc0_0000,
        16 => u64::from(double_to_half(x)),
        64 => x.to_bits(),
        _ => u64::from(single(x).to_bits()),
    }
}

/// The single `bits` rounded to the nearest half and back: an infinity or a
/// `NaN` keeps, and what no normal half holds flushes to zero of its sign.
#[must_use]
pub const fn quantized_to_half(bits: u32) -> u32 {
    let half = to_half(f32::from_bits(bits));
    let flushed = if half & 0x7c00 == 0 {
        half & 0x8000
    } else {
        half
    };
    from_half(flushed).to_bits()
}

/// The blend of a cell's corners `[c00, c10, c01, c11]` at `(across, down)`
/// within it, the first corner at `(0, 0)` and the last at `(1, 1)`.
#[must_use]
pub fn bilinear([c00, c10, c01, c11]: [f64; 4], (across, down): (f64, f64)) -> f64 {
    let top = c00 + (c10 - c00) * across;
    let bottom = c01 + (c11 - c01) * across;
    top + (bottom - top) * down
}

/// [`round`], returned as an `i32`.
///
/// Saturating rather than wrapping: a coordinate a hostile document drove far
/// out of range clamps to the extreme instead of wrapping to the opposite
/// side of the canvas.
#[allow(
    clippy::cast_possible_truncation,
    reason = "the value is clamped into i32's range on the line above, so the \
              truncation the lint warns about cannot occur"
)]
#[must_use]
pub fn round_i32(x: f64) -> i32 {
    clamp(round(x), f64::from(i32::MIN), f64::from(i32::MAX)) as i32
}

/// The correctly rounded non-negative square root of `x`: `0.0` for a
/// negative, zero or `NaN` input, and `+∞` for `+∞`.
#[must_use]
pub fn sqrt(x: f64) -> f64 {
    if x.is_nan() || x <= 0.0 {
        return 0.0;
    }
    math::sqrt(x)
}

/// The length of the vector `(x, y)`, computed without squaring a magnitude
/// that would overflow, saturating at [`f64::MAX`], and `0.0` with a `NaN`
/// either side.
#[must_use]
pub fn hypot(x: f64, y: f64) -> f64 {
    if x.is_nan() || y.is_nan() {
        return 0.0;
    }
    let (ax, ay) = (fabs(x), fabs(y));
    let big = fmax(ax, ay);
    if big == 0.0 {
        return 0.0;
    }
    let small = fmin(ax, ay) / big;
    fmin(big * sqrt(1.0 + small * small), f64::MAX)
}

/// Added and subtracted again, rounds a double below 2^51 to the nearest
/// integer without leaving the basic operations.
const TO_INTEGER: f64 = 1.5 / f64::EPSILON;

// `PI/2` as three 33-bit parts, each with the tail the one before it
// leaves, so a whole number of quarter turns below 2^20 times any part is
// exact: fdlibm's `__rem_pio2` constants.
const PIO2_1: f64 = 1.570_796_326_734_125_6;
const PIO2_1T: f64 = 6.077_100_506_506_192e-11;
const PIO2_2: f64 = 6.077_100_506_303_966e-11;
const PIO2_2T: f64 = 2.022_266_248_795_950_6e-21;
const PIO2_3: f64 = 2.022_266_248_711_166_5e-21;
const PIO2_3T: f64 = 8.478_427_660_368_9e-32;

/// The largest angle the parts of `PI/2` still reduce exactly; past it
/// [`reduce_large`] takes over.
const REDUCIBLE: f64 = 1_048_576.0 * FRAC_PI_2;

/// The first 1216 bits of `2/PI` after the binary point, most significant
/// first, behind a zero word standing for its integer part: as far as the
/// largest double's reduction reaches.
const TWO_OVER_PI: [u64; 20] = [
    0,
    0xa2f9_836e_4e44_1529,
    0xfc27_57d1_f534_ddc0,
    0xdb62_9599_3c43_9041,
    0xfe51_63ab_debb_c561,
    0xb724_6e3a_424d_d2e0,
    0x0649_2eea_09d1_921c,
    0xfe1d_eb1c_b129_a73e,
    0xe882_35f5_2ebb_4484,
    0xe99c_7026_b45f_7e41,
    0x3991_d639_8353_39f4,
    0x9c84_5f8b_bdf9_283b,
    0x1ff8_97ff_de05_980f,
    0xef2f_118b_5a0a_6d1f,
    0x6d36_7ecf_27cb_09b7,
    0x4f46_3f66_9e5f_ea2d,
    0x7527_bac7_ebe5_f17b,
    0x3d07_39f7_8a52_92ea,
    0x6bfb_5fb1_1f8d_5d08,
    0x5603_3046_fc7b_6bab,
];

/// `PI/2` in fixed point with 126 fraction bits, rounded to nearest.
const PIO2_FIXED: u128 = 0x6487_ed51_10b4_611a_6263_3145_c06e_0e69;

/// `2^-128`, the weight of a fixed-point remainder's lowest bit.
const REMAINDER_ULP: f64 = f64::from_bits((1023 - 128) << 52);

/// The low 64 bits of a `u128`.
const LOW_WORD: u128 = (1 << 64) - 1;

/// The biased exponent of `x`: its magnitude, counted in bits.
#[allow(
    clippy::cast_possible_truncation,
    reason = "an eleven-bit field, which a u16 holds exactly"
)]
fn exponent(x: f64) -> u16 {
    ((x.to_bits() >> 52) & 0x7ff) as u16
}

/// `rest` less `turns` more parts of `PI/2`: the new remainder, the tail its
/// rounding lost, and their sum.
fn subtract_part(rest: f64, turns: f64, part: f64, part_tail: f64) -> (f64, f64, f64) {
    let taken = turns * part;
    let next = rest - taken;
    let lost = turns * part_tail - ((rest - next) - taken);
    (next, lost, next - lost)
}

/// `x` less its nearest whole number of quarter turns, as a remainder within
/// about `PI/4` of zero — carried as a head and the tail its rounding lost —
/// and that number of turns modulo four. An infinite or `NaN` angle has no
/// direction and reduces as zero.
///
/// fdlibm's `__rem_pio2` for moderate angles: Cody and Waite's subtraction of
/// `PI/2` in parts, taking a further part wherever the last cancelled so many
/// bits that too few are left, which near a multiple of `PI/2` is what keeps
/// the tiny remainder accurate to its own last bit.
fn reduce(x: f64) -> (f64, f64, u8) {
    if !x.is_finite() {
        return (0.0, 0.0, 0);
    }
    if fabs(x) > REDUCIBLE {
        return reduce_large(x);
    }
    let turns = (x * FRAC_2_PI + TO_INTEGER) - TO_INTEGER;
    let first = x - turns * PIO2_1;
    let first_lost = turns * PIO2_1T;
    let (mut rest, mut lost, mut head) = (first, first_lost, first - first_lost);
    if exponent(x).saturating_sub(exponent(head)) > 16 {
        (rest, lost, head) = subtract_part(rest, turns, PIO2_2, PIO2_2T);
        if exponent(x).saturating_sub(exponent(head)) > 49 {
            (rest, lost, head) = subtract_part(rest, turns, PIO2_3, PIO2_3T);
        }
    }
    let tail = (rest - head) - lost;
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "turns is a whole number no larger than 2^20, far inside \
                  i64, and only its two low bits are kept"
    )]
    let quadrant = ((turns as i64) & 3) as u8;
    (head, tail, quadrant)
}

/// [`reduce`] for a finite angle past [`REDUCIBLE`]: Payne and Hanek's, in
/// integers, so it is exact whatever the angle's size.
///
/// With `x = mantissa * 2^scale`, a bit of `2/PI` weighing `2^-i` adds
/// `mantissa * 2^(scale - i)` quarter turns, a multiple of four once
/// `i <= scale - 2`. So four words of `2/PI` from the one holding bit
/// `scale - 1` on give the quadrant and a 128-bit fraction of a turn, short of
/// the true one by under 2^-138 — against a double's nearest approach to a
/// quarter turn, about 2^-62.
fn reduce_large(x: f64) -> (f64, f64, u8) {
    let mantissa = u128::from((x.to_bits() & ((1 << 52) - 1)) | (1 << 52));
    // How far into the table bit `scale - 1` of `2/PI` sits.
    let offset = exponent(x).saturating_sub(1013);
    let first = usize::from(offset / 64);
    let shift = 126 - offset % 64;
    let mut limbs = [0_u128; 4];
    let mut carry = 0;
    for (limb, &word) in limbs
        .iter_mut()
        .zip(TWO_OVER_PI[first..first + 4].iter().rev())
    {
        let sum = mantissa * u128::from(word) + carry;
        *limb = sum & LOW_WORD;
        carry = sum >> 64;
    }
    let (low, high) = (limbs[0] | limbs[1] << 64, limbs[2] | limbs[3] << 64);
    let fraction = (low >> shift) | (high << (128 - shift));
    #[allow(
        clippy::cast_possible_truncation,
        reason = "only the two low bits, the quarter turns modulo four, are kept"
    )]
    let turns = ((high >> shift) as u8) & 3;
    let past_half = fraction >> 127 == 1;
    let (distance, turns) = if past_half {
        (fraction.wrapping_neg(), turns + 1)
    } else {
        (fraction, turns)
    };
    let (product_high, product_low) = widening_mul(distance, PIO2_FIXED);
    let (head, tail) = fixed_to_pair((product_high << 2) | (product_low >> 126));
    let (head, tail) = if past_half {
        (-head, -tail)
    } else {
        (head, tail)
    };
    if x.is_sign_negative() {
        (-head, -tail, turns.wrapping_neg() & 3)
    } else {
        (head, tail, turns & 3)
    }
}

/// The high and low halves of the 256-bit product `a * b`.
fn widening_mul(a: u128, b: u128) -> (u128, u128) {
    let (a_high, a_low) = (a >> 64, a & LOW_WORD);
    let (b_high, b_low) = (b >> 64, b & LOW_WORD);
    let (middle, middle_carry) = (a_high * b_low).overflowing_add(a_low * b_high);
    let (low, low_carry) = (a_low * b_low).overflowing_add(middle << 64);
    let high =
        a_high * b_high + (middle >> 64) + (u128::from(middle_carry) << 64) + u128::from(low_carry);
    (high, low)
}

/// `fixed * 2^-128` as the nearest double and the part of it that rounding
/// dropped.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "the head is `fixed` rounded to the nearest double, an integer \
              below 2^128 that converts back exactly; the two differ by under \
              2^75, which the wrapping difference read as an `i128` holds \
              exactly, and whose own rounding is far below the head's last bit"
)]
fn fixed_to_pair(fixed: u128) -> (f64, f64) {
    let head = fixed as f64;
    let tail = fixed.wrapping_sub(head as u128) as i128 as f64;
    (head * REMAINDER_ULP, tail * REMAINDER_ULP)
}

// fdlibm's `__kernel_sin`: `sin(x) ~ x + S1 x^3 + … + S6 x^13` over
// `-PI/4..=PI/4`.
const S1: f64 = -0.166_666_666_666_666_32;
const S2: f64 = 0.008_333_333_333_322_49;
const S3: f64 = -0.000_198_412_698_298_579_5;
const S4: f64 = 2.755_731_370_707_006_8e-6;
const S5: f64 = -2.505_076_025_340_686_3e-8;
const S6: f64 = 1.589_690_995_211_55e-10;

// fdlibm's `__kernel_cos`: `cos(x) ~ 1 - x^2/2 + C1 x^4 + … + C6 x^14` over
// `-PI/4..=PI/4`.
const C1: f64 = 0.041_666_666_666_666_6;
const C2: f64 = -0.001_388_888_888_887_411;
const C3: f64 = 2.480_158_728_947_673e-5;
const C4: f64 = -2.755_731_435_139_066_3e-7;
const C5: f64 = 2.087_572_321_298_175e-9;
const C6: f64 = -1.135_964_755_778_819_5e-11;

/// `sin(head + tail)` for a remainder from [`reduce`].
fn sin_kernel(head: f64, tail: f64) -> f64 {
    let z = head * head;
    let w = z * z;
    let r = S2 + z * (S3 + z * S4) + z * w * (S5 + z * S6);
    let v = z * head;
    head - ((z * (0.5 * tail - v * r) - tail) - v * S1)
}

/// `cos(head + tail)` for a remainder from [`reduce`].
fn cos_kernel(head: f64, tail: f64) -> f64 {
    let z = head * head;
    let w = z * z;
    let r = z * (C1 + z * (C2 + z * C3)) + w * w * (C4 + z * (C5 + z * C6));
    let half = 0.5 * z;
    let near = 1.0 - half;
    near + (((1.0 - near) - half) + (z * r - head * tail))
}

/// The sine of a reduced angle `quadrant` quarter turns on from its remainder.
fn sin_of(head: f64, tail: f64, quadrant: u8) -> f64 {
    match quadrant & 3 {
        0 => sin_kernel(head, tail),
        1 => cos_kernel(head, tail),
        2 => -sin_kernel(head, tail),
        _ => -cos_kernel(head, tail),
    }
}

/// The sine of `x` radians.
#[must_use]
pub fn sin(x: f64) -> f64 {
    let (head, tail, quadrant) = reduce(x);
    sin_of(head, tail, quadrant)
}

/// The cosine of `x` radians.
#[must_use]
pub fn cos(x: f64) -> f64 {
    let (head, tail, quadrant) = reduce(x);
    sin_of(head, tail, quadrant + 1)
}

/// A sine swept a step at a time by turning its phase: one rotation a step
/// where evaluating the sine afresh would be a series.
///
/// Each turn rounds, so a long sweep drifts from the exact sine by an ulp or
/// so a step: it suits a row of samples, not a clock.
#[derive(Copy, Clone, Debug)]
pub struct Phasor {
    amplitude: f64,
    sin: f64,
    cos: f64,
    step_sin: f64,
    step_cos: f64,
}

impl Phasor {
    /// `amplitude · sin(angle)`, turning by `step` radians a step.
    #[must_use]
    pub fn new(amplitude: f64, angle: f64, step: f64) -> Self {
        Self {
            amplitude,
            sin: sin(angle),
            cos: cos(angle),
            step_sin: sin(step),
            step_cos: cos(step),
        }
    }

    /// The sine where the sweep stands, scaled by its amplitude.
    #[must_use]
    pub fn value(&self) -> f64 {
        self.amplitude * self.sin
    }

    /// Turn on by one step.
    pub fn advance(&mut self) {
        let sin = self.sin * self.step_cos + self.cos * self.step_sin;
        self.cos = self.cos * self.step_cos - self.sin * self.step_sin;
        self.sin = sin;
    }
}

/// The tangent of `x` radians: the sine over the cosine of one reduction.
///
/// No double lies on a pole, and the reduction keeps a remainder near one to
/// its last bit, so the cosine is never zero: the tangent is finite
/// everywhere, about `1.6e16` at the double nearest `PI/2`.
#[must_use]
pub fn tan(x: f64) -> f64 {
    let (head, tail, quadrant) = reduce(x);
    sin_of(head, tail, quadrant) / sin_of(head, tail, quadrant + 1)
}

// fdlibm's `atan`: `atan(t) ~ t - t (AT0 t^2 + … + AT10 t^22)` for
// `|t| < 7/16`.
const AT0: f64 = 0.333_333_333_333_329_3;
const AT1: f64 = -0.199_999_999_998_764_83;
const AT2: f64 = 0.142_857_142_725_034_66;
const AT3: f64 = -0.111_111_104_054_623_56;
const AT4: f64 = 0.090_908_871_334_365_07;
const AT5: f64 = -0.076_918_762_050_448_3;
const AT6: f64 = 0.066_610_731_373_875_31;
const AT7: f64 = -0.058_335_701_337_905_735;
const AT8: f64 = 0.049_768_779_946_159_324;
const AT9: f64 = -0.036_531_572_744_216_916;
const AT10: f64 = 0.016_285_820_115_365_782;

/// The tail `PI/2` loses in rounding to [`FRAC_PI_2`].
const PIO2_LO: f64 = 6.123_233_995_736_766e-17;

/// The tail `PI` loses in rounding to [`PI`].
const PI_LO: f64 = 2.0 * PIO2_LO;

// `atan` of the breakpoints `1/2`, `1`, `3/2` and infinity, each as a head
// and the tail its rounding lost.
const ATAN_HALF: (f64, f64) = (0.463_647_609_000_806_1, 2.269_877_745_296_168_7e-17);
const ATAN_ONE: (f64, f64) = (FRAC_PI_4, 0.5 * PIO2_LO);
const ATAN_THREE_HALVES: (f64, f64) = (0.982_793_723_247_329, 1.390_331_103_123_099_8e-17);
const ATAN_INFINITY: (f64, f64) = (FRAC_PI_2, PIO2_LO);

/// 2^66, past which `atan` is `PI/2` to the last bit.
const ATAN_SATURATES: f64 = 7.378_697_629_483_821e19;

/// The arctangent of `x` radians, in `-PI/2..=PI/2`, and `0.0` for `NaN`.
///
/// fdlibm's reduction: each of four intervals is mapped onto `|t| < 7/16` and
/// offset by the arctangent of its breakpoint.
#[must_use]
pub fn atan(x: f64) -> f64 {
    if x.is_nan() {
        return 0.0;
    }
    let magnitude = fabs(x);
    let angle = if magnitude >= ATAN_SATURATES {
        FRAC_PI_2
    } else {
        let (t, offset) = if magnitude < 0.4375 {
            (magnitude, None)
        } else if magnitude < 0.6875 {
            ((2.0 * magnitude - 1.0) / (2.0 + magnitude), Some(ATAN_HALF))
        } else if magnitude < 1.1875 {
            ((magnitude - 1.0) / (magnitude + 1.0), Some(ATAN_ONE))
        } else if magnitude < 2.4375 {
            (
                (magnitude - 1.5) / (1.0 + 1.5 * magnitude),
                Some(ATAN_THREE_HALVES),
            )
        } else {
            (-1.0 / magnitude, Some(ATAN_INFINITY))
        };
        let z = t * t;
        let w = z * z;
        let odd = z * (AT0 + w * (AT2 + w * (AT4 + w * (AT6 + w * (AT8 + w * AT10)))));
        let even = w * (AT1 + w * (AT3 + w * (AT5 + w * (AT7 + w * AT9))));
        match offset {
            None => t - t * (odd + even),
            Some((hi, lo)) => hi - ((t * (odd + even) - lo) - t),
        }
    };
    if x.is_sign_negative() {
        -angle
    } else {
        angle
    }
}

/// The angle of the vector `(x, y)` in `-PI..=PI`, measured from the positive
/// x axis.
///
/// The origin has no angle; it answers `0.0` rather than a `NaN`, so a
/// degenerate segment still yields a drawable direction.
#[must_use]
pub fn atan2(y: f64, x: f64) -> f64 {
    if x > 0.0 {
        return atan(y / x);
    }
    if x < 0.0 {
        // Measured from `PI`, whose rounding is put back as fdlibm's does.
        let off = atan(fabs(y / x)) - PI_LO;
        return if y >= 0.0 { PI - off } else { off - PI };
    }
    if y > 0.0 {
        FRAC_PI_2
    } else if y < 0.0 {
        -FRAC_PI_2
    } else {
        0.0
    }
}

// fdlibm's `asin`: `(asin(x) - x)/x ~ P(x²)/Q(x²)` over `|x| <= 1/2`.
const PS0: f64 = 0.166_666_666_666_666_66;
const PS1: f64 = -0.325_565_818_622_400_9;
const PS2: f64 = 0.201_212_532_134_862_93;
const PS3: f64 = -0.040_055_534_500_679_41;
const PS4: f64 = 0.000_791_534_994_289_814_5;
const PS5: f64 = 3.479_331_075_960_212e-5;
const QS1: f64 = -2.403_394_911_734_414;
const QS2: f64 = 2.020_945_760_233_505_7;
const QS3: f64 = -0.688_283_971_605_453_3;
const QS4: f64 = 0.077_038_150_555_901_94;

/// `(asin(x) - x)/x` for `z = x²`.
fn asin_series(z: f64) -> f64 {
    let p = z * (PS0 + z * (PS1 + z * (PS2 + z * (PS3 + z * (PS4 + z * PS5)))));
    let q = 1.0 + z * (QS1 + z * (QS2 + z * (QS3 + z * QS4)));
    p / q
}

/// `x` with the low 32 bits of its significand cleared: a head of at most 21
/// significant bits, so its product with anything of 32 is exact.
const fn high_part(x: f64) -> f64 {
    f64::from_bits(x.to_bits() & !0xffff_ffff)
}

/// The arccosine of `x` in `0..=PI`, with the domain clamped to `-1..=1` so an
/// out-of-range input (a rounding overshoot in an arc conversion) answers an
/// endpoint rather than a `NaN`, and a `NaN` the one at `-1`.
///
/// fdlibm's: `PI/2 - asin(x)` below a half, and past it
/// `2·asin(√((1 - |x|)/2))` measured from the nearer end, which keeps every
/// digit of the angle near either end.
#[must_use]
pub fn acos(x: f64) -> f64 {
    let c = clamp(x, -1.0, 1.0);
    let high = high_word(c.to_bits()) & 0x7fff_ffff;
    if high < 0x3fe0_0000 {
        // Below 2^-57 the angle rounds to `PI/2`.
        if high <= 0x3c60_0000 {
            return FRAC_PI_2;
        }
        return FRAC_PI_2 - (c - (PIO2_LO - c * asin_series(c * c)));
    }
    if c.to_bits() == 1.0_f64.to_bits() {
        return 0.0;
    }
    let z = (1.0 - fabs(c)) * 0.5;
    let root = sqrt(z);
    if c < 0.0 {
        return PI - 2.0 * (root + (asin_series(z) * root - PIO2_LO));
    }
    let head = high_part(root);
    let tail = (z - head * head) / (root + head);
    2.0 * (head + (asin_series(z) * root + tail))
}

/// The arcsine of `x` in `-PI/2..=PI/2`, with the domain clamped to `-1..=1`
/// so an out-of-range input answers an endpoint rather than a `NaN`, and a
/// `NaN` the one at `-1`.
///
/// fdlibm's: the series below a half, and past it
/// `PI/2 - 2·asin(√((1 - |x|)/2))`, its square root in two parts below
/// `0.975`, where `PI/4` would otherwise cancel its leading digits.
#[must_use]
pub fn asin(x: f64) -> f64 {
    let c = clamp(x, -1.0, 1.0);
    let high = high_word(c.to_bits()) & 0x7fff_ffff;
    let magnitude = fabs(c);
    let angle = if high < 0x3fe0_0000 {
        // Below 2^-27 the angle rounds to `x`.
        if high < 0x3e40_0000 {
            magnitude
        } else {
            magnitude + magnitude * asin_series(magnitude * magnitude)
        }
    } else {
        let z = (1.0 - magnitude) * 0.5;
        let root = sqrt(z);
        let series = asin_series(z);
        if high >= 0x3fef_3333 {
            FRAC_PI_2 - (2.0 * (root + root * series) - PIO2_LO)
        } else {
            let head = high_part(root);
            let tail = (z - head * head) / (root + head);
            let off = 2.0 * root * series - (PIO2_LO - 2.0 * tail);
            FRAC_PI_4 - (off - (FRAC_PI_4 - 2.0 * head))
        }
    };
    angle.copysign(c)
}

/// Largest argument [`exp`] evaluates, the greatest double below
/// `ln(f64::MAX)`; above it the true result exceeds a double and the answer
/// saturates.
const EXP_MAX_ARG: f64 = 709.782_712_893_384;

/// Smallest argument [`exp`] evaluates, fdlibm's underflow threshold: below it
/// the true result is under half the least subnormal and rounds to zero.
const EXP_MIN_ARG: f64 = -745.133_219_101_941_1;

/// `ln(2)`'s leading bits, chosen with a zero tail so `k * LN_2_HI` is exact.
const LN_2_HI: f64 = 6.931_471_803_691_238e-1;

/// The remainder of `ln(2)` past [`LN_2_HI`], subtracted separately so the
/// range reduction keeps its low bits.
const LN_2_LO: f64 = 1.908_214_929_270_587_7e-10;

// fdlibm's `exp`: `r (e^r + 1) / (e^r - 1) ~ 2 + P1 r^2 + … + P5 r^10` over
// `|r| <= ln(2)/2`.
const P1: f64 = 0.166_666_666_666_666_02;
const P2: f64 = -0.002_777_777_777_701_559_3;
const P3: f64 = 6.613_756_321_437_934e-5;
const P4: f64 = -1.653_390_220_546_525_2e-6;
const P5: f64 = 4.138_136_797_057_238_5e-8;

/// `e` raised to `x`.
///
/// Range-reduced to `x = k*ln(2) + r` with `|r| <= ln(2)/2`, where fdlibm's
/// rational approximation holds, then scaled by `2^k`, rounding once into the
/// subnormals.
///
/// Total, like the rest of this module, and saturating rather than infinite:
/// a result past the double's range answers [`f64::MAX`], one under half its
/// least subnormal zero, and a `NaN` zero. A consumer converting a decibel
/// gain therefore gets silence from a corrupt input rather than a `NaN` that
/// would spread through everything it is multiplied into.
#[must_use]
pub fn exp(x: f64) -> f64 {
    if x.is_nan() || x < EXP_MIN_ARG {
        return 0.0;
    }
    if x > EXP_MAX_ARG {
        return f64::MAX;
    }
    let exponent = round_i32(x * LOG2_E);
    let whole = f64::from(exponent);
    let hi = x - whole * LN_2_HI;
    let lo = whole * LN_2_LO;
    let r = hi - lo;
    let rr = r * r;
    let c = r - rr * (P1 + rr * (P2 + rr * (P3 + rr * (P4 + rr * P5))));
    scaled(1.0 + ((r * c / (2.0 - c) - lo) + hi), exponent)
}

/// `2^54`, which lifts a subnormal into the normal range.
const TWO_54: f64 = 1.801_439_850_948_198_4e16;

/// The high word of a double's bits where the normal numbers begin.
const LEAST_NORMAL_HIGH: u32 = 0x0010_0000;

// fdlibm's `log`: `ln(1 + f) = f − f²/2 + s·(f²/2 + R)` for `s = f / (2 + f)`,
// with `R = LG1·s² + … + LG7·s¹⁴` its minimax series over `1 + f` in
// `[√2/2, √2)`.
const LG1: f64 = 0.666_666_666_666_673_5;
const LG2: f64 = 0.399_999_999_994_094_2;
const LG3: f64 = 0.285_714_287_436_623_9;
const LG4: f64 = 0.222_221_984_321_497_84;
const LG5: f64 = 0.181_835_721_616_180_5;
const LG6: f64 = 0.153_138_376_992_093_73;
const LG7: f64 = 0.147_981_986_051_165_86;

/// A finite positive `x` as `2^k·m` with `m` in `[√2/2, √2)`: `k`, `m`, and
/// the leading fraction bits of `x`'s significand, which [`ln`] picks its
/// form by.
fn log_reduce(x: f64) -> (i32, f64, u32) {
    let mut bits = x.to_bits();
    let mut exponent = 0;
    if high_word(bits) < LEAST_NORMAL_HIGH {
        exponent -= 54;
        bits = (x * TWO_54).to_bits();
    }
    let high = high_word(bits);
    exponent += low_int(high >> 20) - 1023;
    let mantissa = high & 0x000f_ffff;
    // Set when `m` would reach `√2`, which then halves it into range.
    let halve = (mantissa + 0x95f64) & 0x0010_0000;
    let reduced =
        f64::from_bits((u64::from(mantissa | (halve ^ 0x3ff0_0000)) << 32) | (bits & 0xffff_ffff));
    (exponent + low_int(halve >> 20), reduced, mantissa)
}

/// fdlibm's `R` for `s = f/(2 + f)`, its odd and even terms summed apart.
fn log_series(ratio: f64) -> f64 {
    let square = ratio * ratio;
    let fourth = square * square;
    square * (LG1 + fourth * (LG3 + fourth * (LG5 + fourth * LG7)))
        + fourth * (LG2 + fourth * (LG4 + fourth * LG6))
}

/// The natural logarithm of `x`.
///
/// fdlibm's: `x` is reduced to `2^k·(1 + f)` with `1 + f` in `[√2/2, √2)`,
/// `ln(1 + f)` is evaluated through its minimax series in `s = f / (2 + f)`,
/// and `k·ln(2)` is added back in two parts so its low bits survive.
///
/// Total and saturating like [`exp`]: zero, a negative or a `NaN` answers
/// `f64::MIN`, and `+∞` answers `f64::MAX`.
#[must_use]
pub fn ln(x: f64) -> f64 {
    if x.is_nan() || x <= 0.0 {
        return f64::MIN;
    }
    if x.is_infinite() {
        return f64::MAX;
    }
    let (exponent, reduced, mantissa) = log_reduce(x);
    let scale = f64::from(exponent);
    let f = reduced - 1.0;
    if (0x000f_ffff & (2 + mantissa)) < 3 {
        // |f| < 2^-20, where the series' first terms are exact enough.
        let tail = f * f * (0.5 - 0.333_333_333_333_333_3 * f);
        return scale * LN_2_HI - ((tail - scale * LN_2_LO) - f);
    }
    let ratio = f / (2.0 + f);
    let series = log_series(ratio);
    let mantissa = low_int(mantissa);
    if (mantissa - 0x6147a) | (0x6b851 - mantissa) > 0 {
        let half_square = 0.5 * f * f;
        scale * LN_2_HI - ((half_square - (ratio * (half_square + series) + scale * LN_2_LO)) - f)
    } else {
        scale * LN_2_HI - ((ratio * (f - series) - scale * LN_2_LO) - f)
    }
}

// fdlibm's `pow`: `(3/2)·(ln(1 + f) - 2s - (2/3)s³)` for `s = f/(2 + f)` as a
// polynomial in `s²`.
const L1: f64 = 0.599_999_999_999_994_6;
const L2: f64 = 0.428_571_428_578_550_2;
const L3: f64 = 0.333_333_329_818_377_43;
const L4: f64 = 0.272_728_123_808_534;
const L5: f64 = 0.230_660_745_775_561_75;
const L6: f64 = 0.206_975_017_800_338_42;

// `log2(3/2)`, `2/(3·ln(2))` and `ln(2)` each as a short head and the tail
// it leaves, and `2/(3·ln(2))` whole.
const LOG2_THREE_HALVES: (f64, f64) = (0.584_962_487_220_764_2, 1.350_039_202_129_749e-8);
const TWO_THIRDS_LOG2_E_SPLIT: (f64, f64) = (0.961_796_700_954_437_3, -7.028_461_650_952_758e-9);
const TWO_THIRDS_LOG2_E: f64 = 0.961_796_693_925_975_6;
const LN_2_SPLIT: (f64, f64) = (0.693_147_182_464_599_6, -1.904_654_299_957_768e-9);

/// `1/ln(2)` as a head of 21 bits and the tail it leaves, so the head's
/// product with anything of 32 bits is exact. fdlibm's `pow` takes a 24-bit
/// head, whose product with `x - 1` near one rounds, and a power past 2^31
/// magnifies that rounding into hundreds of ulps.
const LOG2_E_SPLIT: (f64, f64) = (high_part(LOG2_E), 3.768_874_985_636_099e-7);

/// How far short of 1024 a power must fall to round within the double's
/// range: `1024 - log2(f64::MAX + half an ulp)`.
const POWER_OVERFLOW_TAIL: f64 = 8.008_566_259_537_294e-17;

/// `log2(x)` for a finite positive `x` as a [`high_part`] head and a tail:
/// fdlibm's `pow`'s, near enough the true value that any power of it a double
/// holds loses only its own last bit.
///
/// `x = 2^n·m` with `m` taken about one or `3/2`, whichever is nearer, so
/// `s = (m - b)/(m + b)` stays small and `log2(m)` is `log2(b)` plus a series
/// in `s` whose leading terms are carried in parts.
fn log2_split(x: f64) -> (f64, f64) {
    let (mut x, mut n) = (x, 0);
    if high_word(x.to_bits()) < LEAST_NORMAL_HIGH {
        x *= TWO_54;
        n -= 54;
    }
    let high = high_word(x.to_bits());
    n += low_int(high >> 20) - 1023;
    let fraction = high & 0x000f_ffff;
    // About one below √(3/2), about 3/2 below √3, and halved about one past;
    // `bias` lifts `m`'s high word to the head of `m + base`.
    let (high, base, base_log2, bias) = if fraction <= 0x3_988e {
        (fraction | 0x3ff0_0000, 1.0, (0.0, 0.0), 0x0008_0000)
    } else if fraction < 0xb_b67a {
        (fraction | 0x3ff0_0000, 1.5, LOG2_THREE_HALVES, 0x000c_0000)
    } else {
        n += 1;
        (fraction | 0x3fe0_0000, 1.0, (0.0, 0.0), 0x0008_0000)
    };
    let m = f64::from_bits((u64::from(high) << 32) | (x.to_bits() & 0xffff_ffff));
    let numerator = m - base;
    let reciprocal = 1.0 / (m + base);
    let s = numerator * reciprocal;
    let s_head = high_part(s);
    let sum_head = f64::from_bits(u64::from(((high >> 1) | 0x2000_0000) + bias) << 32);
    let sum_tail = m - (sum_head - base);
    let s_tail = reciprocal * ((numerator - s_head * sum_head) - s_head * sum_tail);
    let square = s * s;
    let series = square
        * square
        * (L1 + square * (L2 + square * (L3 + square * (L4 + square * (L5 + square * L6)))))
        + s_tail * (s_head + s);
    let head_square = s_head * s_head;
    let cubic_head = high_part(3.0 + head_square + series);
    let cubic_tail = series - ((cubic_head - 3.0) - head_square);
    let product = s_head * cubic_head;
    let product_tail = s_tail * cubic_head + cubic_tail * s;
    let scaled_head = high_part(product + product_tail);
    let scaled_tail = product_tail - (scaled_head - product);
    let log_head = TWO_THIRDS_LOG2_E_SPLIT.0 * scaled_head;
    let log_tail =
        TWO_THIRDS_LOG2_E_SPLIT.1 * scaled_head + scaled_tail * TWO_THIRDS_LOG2_E + base_log2.1;
    let whole = f64::from(n);
    let head = high_part(((log_head + log_tail) + base_log2.0) + whole);
    (head, log_tail - (((head - whole) - base_log2.0) - log_head))
}

/// `2^(high + low)` for a power split into a head and a tail: fdlibm's
/// `pow`'s, saturating at `f64::MAX` above the double's range and at zero
/// below it.
fn exp2_split(high: f64, low: f64) -> f64 {
    let sum = high + low;
    let bits = sum.to_bits();
    let sum_high = high_word(bits);
    let negative = sum_high >> 31 == 1;
    let magnitude = sum_high & 0x7fff_ffff;
    let low_clear = bits.trailing_zeros() >= 32;
    // A power past 1024 overflows and one below -1075 rounds to zero; at
    // either bound exactly, the tail the sum rounded away decides.
    if !negative && magnitude >= 0x4090_0000 {
        let below = sum_high == 0x4090_0000 && low_clear && low + POWER_OVERFLOW_TAIL <= sum - high;
        if !below {
            return f64::MAX;
        }
    } else if negative && magnitude >= 0x4090_cc00 {
        let above = sum_high == 0xc090_cc00 && low_clear && low > sum - high;
        if !above {
            return 0.0;
        }
    }
    // From `0.5 + 2^-21`, as the high word tells, the nearest integer, halves
    // away from zero, comes off the head exactly. Rounding the signed word
    // keeps the sign in the whole, so one subtraction serves either sign.
    let (high, n) = if magnitude > 0x3fe0_0000 {
        let rounded = sum_high + (0x0010_0000 >> ((magnitude >> 20) - 0x3fe));
        let shift = ((rounded & 0x7fff_ffff) >> 20) - 0x3ff;
        let whole = f64::from_bits(u64::from(rounded & !(0x000f_ffff >> shift)) << 32);
        let count = low_int(((rounded & 0x000f_ffff) | 0x0010_0000) >> (20 - shift));
        (high - whole, if negative { -count } else { count })
    } else {
        (high, 0)
    };
    let fraction = high_part(low + high);
    let fraction_head = fraction * LN_2_SPLIT.0;
    let fraction_tail = (low - (fraction - high)) * LN_2 + fraction * LN_2_SPLIT.1;
    let r = fraction_head + fraction_tail;
    let lost = fraction_tail - (r - fraction_head);
    let rr = r * r;
    let c = r - rr * (P1 + rr * (P2 + rr * (P3 + rr * (P4 + rr * P5))));
    let reduced = 1.0 - (((r * c) / (c - 2.0) - (lost + r * lost)) - r);
    fmin(scaled(reduced, n), f64::MAX)
}

/// `x` to the power `p`, nearly correctly rounded: fdlibm's, `p·log2(x)`
/// carried in parts so a result filling the double's range still loses only
/// its last bit, and an integer to an integer power exact wherever a double
/// holds the result.
///
/// Total and saturating like [`exp`]: nought for `x` at or below nought or a
/// `NaN` either side, one for `p` of zero, and `f64::MAX` or nought past the
/// double's range.
#[must_use]
pub fn power(x: f64, p: f64) -> f64 {
    if x.is_nan() || p.is_nan() || x <= 0.0 {
        return 0.0;
    }
    let power_is = |value: f64| p.to_bits() == value.to_bits();
    if p == 0.0 || x.to_bits() == 1.0_f64.to_bits() {
        return 1.0;
    }
    let grows = (x > 1.0) == (p > 0.0);
    if x.is_infinite() || p.is_infinite() {
        return if grows { f64::MAX } else { 0.0 };
    }
    if power_is(1.0) {
        return x;
    }
    if power_is(-1.0) {
        return fmin(1.0 / x, f64::MAX);
    }
    if power_is(2.0) {
        return fmin(x * x, f64::MAX);
    }
    if power_is(0.5) {
        return sqrt(x);
    }
    let (head, tail) = if high_word(p.to_bits()) & 0x7fff_ffff > 0x41e0_0000 {
        // |p| > 2^31: past 2^64, or with `x` outside `[1 - 2^-21, 1 + 2^-20)`,
        // the result leaves the range; otherwise `ln(x)` is its series' first
        // terms in `x - 1`, of at most 32 significant bits.
        let x_high = high_word(x.to_bits());
        let near_one = x_high == 0x3fef_ffff || x_high == 0x3ff0_0000;
        if !near_one || high_word(p.to_bits()) & 0x7fff_ffff > 0x43f0_0000 {
            return if grows { f64::MAX } else { 0.0 };
        }
        let offset = x - 1.0;
        let curve = (offset * offset) * (0.5 - offset * (0.333_333_333_333_333_3 - offset * 0.25));
        let leading = LOG2_E_SPLIT.0 * offset;
        let rest = offset * LOG2_E_SPLIT.1 - curve * LOG2_E;
        let head = high_part(leading + rest);
        (head, rest - (head - leading))
    } else {
        log2_split(x)
    };
    let p_head = high_part(p);
    exp2_split(p_head * head, (p - p_head) * head + p * tail)
}

/// Two raised to `x`: fdlibm's `pow`'s power of two, exact at every integer
/// a double reaches, and saturating like [`exp`].
#[must_use]
pub fn exp2(x: f64) -> f64 {
    if x.is_nan() {
        return 0.0;
    }
    exp2_split(x, 0.0)
}

/// The base-two logarithm of `x`, exact at every power of two and saturating
/// like [`ln`].
///
/// Reduced and summed as [`ln`] is, with `f - f²/2` carried as a head and a
/// tail, since near one and `√2` its terms cancel, the head's product with
/// the head of `1/ln(2)` exact, and the exponent added in extra precision:
/// FreeBSD's form of fdlibm's `log`.
#[must_use]
pub fn log2(x: f64) -> f64 {
    if x.is_nan() || x <= 0.0 {
        return f64::MIN;
    }
    if x.is_infinite() {
        return f64::MAX;
    }
    let (exponent, reduced, _) = log_reduce(x);
    let f = reduced - 1.0;
    let half_square = 0.5 * f * f;
    let ratio = f / (2.0 + f);
    let tail = ratio * (half_square + log_series(ratio));
    let head = high_part(f - half_square);
    let rest = (f - head) - half_square + tail;
    let (inverse_head, inverse_tail) = LOG2_E_SPLIT;
    let value_head = head * inverse_head;
    let value_tail = (rest + head) * inverse_tail + rest * inverse_head;
    let whole = f64::from(exponent);
    let sum = whole + value_head;
    (value_tail + ((whole - sum) + value_head)) + sum
}

// fdlibm's `expm1`: the even function `x/(e^x - 1) + x/2` as a polynomial in
// `x²/2` over |x| ≤ ln(2)/2, `Q1`..`Q5` its minimax coefficients.
const Q1: f64 = -3.333_333_333_333_313e-2;
const Q2: f64 = 1.587_301_587_254_814_6e-3;
const Q3: f64 = -7.936_507_578_674_88e-5;
const Q4: f64 = 4.008_217_827_329_362e-6;
const Q5: f64 = -2.010_992_181_836_243_7e-7;

/// Past this magnitude the hyperbolic sine and cosine pass a double's range.
const HYPERBOLIC_MAX_ARG: f64 = 710.475_860_073_943_9;

/// `e^x - 1`, with none of the cancellation `exp(x) - 1` suffers near zero:
/// fdlibm's, `x` reduced to `k·ln(2) + r` with |r| ≤ ln(2)/2 and `e^r - 1`
/// from its rational approximation. Saturates like [`exp`]: `f64::MAX` past
/// the double's range, `-1` far below it, and `0.0` for `NaN`.
fn expm1(x: f64) -> f64 {
    if x.is_nan() {
        return 0.0;
    }
    let high = high_word(x.to_bits()) & 0x7fff_ffff;
    // |x| ≥ 56·ln(2): `e^x` is past the range, or past the last bit of `-1`.
    if high >= 0x4043_687a {
        if x > EXP_MAX_ARG {
            return f64::MAX;
        }
        if x < 0.0 {
            return -1.0;
        }
    }
    let (reduced, lost, k) = if high > 0x3fd6_2e42 {
        // |x| > ln(2)/2, so a multiple of ln(2) comes off, in two parts.
        let (hi, lo, k) = if high < 0x3ff0_a2b2 {
            if x < 0.0 {
                (x + LN_2_HI, -LN_2_LO, -1)
            } else {
                (x - LN_2_HI, LN_2_LO, 1)
            }
        } else {
            let k = round_i32(x * LOG2_E);
            let turns = f64::from(k);
            (x - turns * LN_2_HI, turns * LN_2_LO, k)
        };
        let reduced = hi - lo;
        (reduced, (hi - reduced) - lo, k)
    } else if high < 0x3c90_0000 {
        // |x| < 2^-54, where `e^x - 1` rounds to `x`.
        return x;
    } else {
        (x, 0.0, 0)
    };
    let half = 0.5 * reduced;
    let half_square = reduced * half;
    let r1 = 1.0
        + half_square
            * (Q1
                + half_square * (Q2 + half_square * (Q3 + half_square * (Q4 + half_square * Q5))));
    let shift = 3.0 - r1 * half;
    let error = half_square * ((r1 - shift) / (6.0 - reduced * shift));
    if k == 0 {
        return reduced - (reduced * error - half_square);
    }
    let error = (reduced * (error - lost) - lost) - half_square;
    match k {
        -1 => 0.5 * (reduced - error) - 0.5,
        1 if reduced < -0.25 => -2.0 * (error - (reduced + 0.5)),
        1 => 1.0 + 2.0 * (reduced - error),
        k if k <= -2 || k > 56 => scaled(1.0 - (error - reduced), k) - 1.0,
        k if k < 20 => scaled(1.0 - power_of_two(-k) - (error - reduced), k),
        k => scaled((reduced - (error + power_of_two(-k))) + 1.0, k),
    }
}

/// `y·2^k` in two halves, as 2^1024 is past what a double holds: exact where
/// the result is normal, and rounded once where it falls among the subnormals.
fn scaled(y: f64, k: i32) -> f64 {
    y * power_of_two(k / 2) * power_of_two(k - k / 2)
}

/// `ln(1 + x)`, with none of the cancellation `ln(1.0 + x)` suffers near
/// zero: fdlibm's, `1 + x` reduced as [`ln`] reduces its argument, the error
/// of rounding the sum carried beside it. Saturates like [`ln`]: `-1`,
/// anything below it and `NaN` answer `f64::MIN`, and `+∞` answers `f64::MAX`.
fn log1p(x: f64) -> f64 {
    if x.is_nan() || x <= -1.0 {
        return f64::MIN;
    }
    if x.is_infinite() {
        return f64::MAX;
    }
    let high = high_word(x.to_bits());
    let negative = high >> 31 == 1;
    let magnitude = high & 0x7fff_ffff;
    // |x| < 2^-29: the series' first two terms, and past 2^-54 the first.
    if magnitude < 0x3e20_0000 {
        return if magnitude < 0x3c90_0000 {
            x
        } else {
            x - x * x * 0.5
        };
    }
    // Within -0.2929 < x < √2 - 1 the series takes `x` as it is.
    let direct = if negative {
        high <= 0xbfd2_bec3
    } else {
        high < 0x3fda_827a
    };
    let (k, f, c, tiny) = if direct {
        (0, x, 0.0, false)
    } else {
        let (u, k, c) = if negative || high < 0x4340_0000 {
            let u = 1.0 + x;
            let k = low_int(high_word(u.to_bits()) >> 20) - 1023;
            let c = if k > 0 { 1.0 - (u - x) } else { x - (u - 1.0) };
            (u, k, c / u)
        } else {
            (x, low_int(high >> 20) - 1023, 0.0)
        };
        let mantissa = high_word(u.to_bits()) & 0x000f_ffff;
        let low = u.to_bits() & 0xffff_ffff;
        // `u` scaled into [√2/2, √2), its leading fraction kept to see how
        // close to one the scaled value is.
        let (scaled, k, lead) = if mantissa < 0x6_a09e {
            (u64::from(mantissa | 0x3ff0_0000), k, mantissa)
        } else {
            (
                u64::from(mantissa | 0x3fe0_0000),
                k + 1,
                (0x0010_0000 - mantissa) >> 2,
            )
        };
        let u = f64::from_bits((scaled << 32) | low);
        (k, u - 1.0, c, lead == 0)
    };
    let scale = f64::from(k);
    let half_square = 0.5 * f * f;
    if tiny {
        // |f| < 2^-20.
        if f == 0.0 {
            return if k == 0 {
                0.0
            } else {
                scale * LN_2_HI + (c + scale * LN_2_LO)
            };
        }
        let tail = half_square * (1.0 - 0.666_666_666_666_666_6 * f);
        return if k == 0 {
            f - tail
        } else {
            scale * LN_2_HI - ((tail - (scale * LN_2_LO + c)) - f)
        };
    }
    let ratio = f / (2.0 + f);
    let series = log_series(ratio);
    if k == 0 {
        f - (half_square - ratio * (half_square + series))
    } else {
        scale * LN_2_HI
            - ((half_square - (ratio * (half_square + series) + (scale * LN_2_LO + c))) - f)
    }
}

/// The hyperbolic sine of `x`: fdlibm's, through `expm1` below 22 so no
/// digit is lost near zero. Saturates at `±f64::MAX` past the double's range;
/// `0.0` for `NaN`.
#[must_use]
pub fn sinh(x: f64) -> f64 {
    if x.is_nan() {
        return 0.0;
    }
    let high = high_word(x.to_bits()) & 0x7fff_ffff;
    let magnitude = fabs(x);
    let half = 0.5_f64.copysign(x);
    if high < 0x4036_0000 {
        // |x| < 22; below 2^-28 `sinh(x)` rounds to `x`.
        if high < 0x3e30_0000 {
            return x;
        }
        let t = expm1(magnitude);
        return if high < 0x3ff0_0000 {
            half * (2.0 * t - t * t / (t + 1.0))
        } else {
            half * (t + t / (t + 1.0))
        };
    }
    if high < 0x4086_2e42 {
        return half * exp(magnitude);
    }
    if magnitude <= HYPERBOLIC_MAX_ARG {
        let w = exp(0.5 * magnitude);
        return (half * w) * w;
    }
    f64::MAX.copysign(x)
}

/// The hyperbolic cosine of `x`: fdlibm's, through `expm1` below `ln(2)/2`.
/// Saturates at `f64::MAX` past the double's range; `0.0` for `NaN`.
#[must_use]
pub fn cosh(x: f64) -> f64 {
    if x.is_nan() {
        return 0.0;
    }
    let high = high_word(x.to_bits()) & 0x7fff_ffff;
    let magnitude = fabs(x);
    if high < 0x3fd6_2e43 {
        // |x| < ln(2)/2; below 2^-55 `cosh(x)` rounds to one.
        let t = expm1(magnitude);
        let w = 1.0 + t;
        return if high < 0x3c80_0000 {
            w
        } else {
            1.0 + (t * t) / (w + w)
        };
    }
    if high < 0x4036_0000 {
        let t = exp(magnitude);
        return 0.5 * t + 0.5 / t;
    }
    if high < 0x4086_2e42 {
        return 0.5 * exp(magnitude);
    }
    if magnitude <= HYPERBOLIC_MAX_ARG {
        let w = exp(0.5 * magnitude);
        return (0.5 * w) * w;
    }
    f64::MAX
}

/// The hyperbolic tangent of `x`: fdlibm's, through `expm1`. `±1` past 22,
/// where it rounds there; `0.0` for `NaN`.
#[must_use]
pub fn tanh(x: f64) -> f64 {
    if x.is_nan() {
        return 0.0;
    }
    let high = high_word(x.to_bits()) & 0x7fff_ffff;
    let magnitude = fabs(x);
    let value = if high < 0x4036_0000 {
        // |x| < 22; below 2^-55 `tanh(x)` rounds to `x`.
        if high < 0x3c80_0000 {
            return x * (1.0 + x);
        }
        if high >= 0x3ff0_0000 {
            let t = expm1(2.0 * magnitude);
            1.0 - 2.0 / (t + 2.0)
        } else {
            let t = expm1(-2.0 * magnitude);
            -t / (t + 2.0)
        }
    } else {
        1.0
    };
    value.copysign(x)
}

/// The inverse hyperbolic sine of `x`: fdlibm's, through `log1p` below 2,
/// and `ln(2|x|)` past 2^28, where `x² + 1` is `x²` to a double. `0.0` for
/// `NaN`.
#[must_use]
pub fn asinh(x: f64) -> f64 {
    if x.is_nan() {
        return 0.0;
    }
    let high = high_word(x.to_bits()) & 0x7fff_ffff;
    // Below 2^-28 `asinh(x)` rounds to `x`.
    if high < 0x3e30_0000 {
        return x;
    }
    let magnitude = fabs(x);
    let value = if high > 0x41b0_0000 {
        ln(magnitude) + LN_2
    } else if high > 0x4000_0000 {
        ln(2.0 * magnitude + 1.0 / (sqrt(magnitude * magnitude + 1.0) + magnitude))
    } else {
        let square = magnitude * magnitude;
        log1p(magnitude + square / (1.0 + sqrt(1.0 + square)))
    };
    value.copysign(x)
}

/// The inverse hyperbolic cosine of `x`: fdlibm's, through `log1p` below 2,
/// and `ln(2x)` past 2^28. `0.0` below one or for `NaN`.
#[must_use]
pub fn acosh(x: f64) -> f64 {
    if x.is_nan() || x <= 1.0 {
        return 0.0;
    }
    let high = high_word(x.to_bits());
    if high >= 0x41b0_0000 {
        return ln(x) + LN_2;
    }
    if high > 0x4000_0000 {
        return ln(2.0 * x - 1.0 / (x + sqrt(x * x - 1.0)));
    }
    let t = x - 1.0;
    log1p(t + sqrt(2.0 * t + t * t))
}

/// The inverse hyperbolic tangent of `x`: fdlibm's, through `log1p`.
/// Saturates at `±f64::MAX` at `±1` and past; `0.0` for `NaN`.
#[must_use]
pub fn atanh(x: f64) -> f64 {
    if x.is_nan() {
        return 0.0;
    }
    if x >= 1.0 {
        return f64::MAX;
    }
    if x <= -1.0 {
        return f64::MIN;
    }
    let high = high_word(x.to_bits()) & 0x7fff_ffff;
    // Below 2^-28 `atanh(x)` rounds to `x`.
    if high < 0x3e30_0000 {
        return x;
    }
    let magnitude = fabs(x);
    let twice = magnitude + magnitude;
    let value = if high < 0x3fe0_0000 {
        0.5 * log1p(twice + twice * magnitude / (1.0 - magnitude))
    } else {
        0.5 * log1p(twice / (1.0 - magnitude))
    };
    value.copysign(x)
}

/// A count as a real number: exact below 2³², saturating at `u32::MAX` past
/// it, which no count of a grid's vertices or a scene's parts approaches.
#[must_use]
pub fn real(count: usize) -> f64 {
    f64::from(u32::try_from(count).unwrap_or(u32::MAX))
}

/// `done` of `total` as a share in `0.0..=1.0`, whole when there was nothing
/// to do.
#[must_use]
pub fn share(done: usize, total: usize) -> f64 {
    if total == 0 {
        1.0
    } else {
        fmin(real(done) / real(total), 1.0)
    }
}

/// `share`, held to `0.0..=1.0`, as the nearest byte.
#[must_use]
pub fn byte(share: f64) -> u8 {
    u8::try_from(round_i32(255.0 * share.clamp(0.0, 1.0))).unwrap_or(u8::MAX)
}

/// A double's high 32 bits: its sign, exponent and leading fraction.
fn high_word(bits: u64) -> u32 {
    u32::try_from(bits >> 32).unwrap_or(u32::MAX)
}

/// A field of at most 21 bits as an `i32`, which holds it exactly.
fn low_int(field: u32) -> i32 {
    i32::try_from(field).unwrap_or(i32::MAX)
}

/// `2^n`, exact for `n` in a double's normal exponents, `-1022..=1023`.
#[allow(
    clippy::cast_sign_loss,
    reason = "the clamped exponent plus its bias is at least 1"
)]
fn power_of_two(n: i32) -> f64 {
    f64::from_bits(((n.clamp(-1022, 1023) + 1023) as u64) << 52)
}

#[cfg(test)]
#[path = "mathf_tests.rs"]
mod tests;
