//! What each component-wise operation computes from a lane's bits: total
//! over every bit pattern, the same on every target.
//!
//! A float of any width is widened to a double, computed there and rounded
//! once to its width, which a double's 53 bits make correctly rounded for the
//! basic operations of a single or a half. Transcendental functions take
//! `mathf`'s double result, far inside the precision a shader is owed.

use tairix_util::mathf;

use super::ir::{Cmp, Conv, Num, Op1, Op2, Op3, Relation, Shift};

/// The low `bits` bits.
pub(crate) const fn mask(bits: u32) -> u64 {
    if bits >= 64 {
        u64::MAX
    } else {
        (1 << bits) - 1
    }
}

/// `x`, `bits` wide, sign-extended.
pub(crate) const fn signed(x: u64, bits: u32) -> i64 {
    if bits == 0 || bits >= 64 {
        x.cast_signed()
    } else {
        let shift = 64 - bits;
        (x << shift).cast_signed() >> shift
    }
}

/// The float `x` of `num` as a double, exactly.
pub(crate) fn float(num: Num, x: u64) -> f64 {
    mathf::float_of(num.bits(), x)
}

/// `value` rounded once to the float `num`, as its bits, as
/// [`mathf::float_bits`] has it: a `NaN` as its width's one quiet `NaN`.
pub(crate) fn unfloat(num: Num, value: f64) -> u64 {
    mathf::float_bits(num.bits(), value)
}

/// `value` rounded toward zero to the float `num`, as its bits; a `NaN` as
/// [`unfloat`] has it.
///
/// The nearest is a step past `value` only where it rounded away from zero,
/// and a step back along the sign-magnitude bits is toward zero, so one step
/// takes an overflow to the largest finite value and leaves an infinity, a
/// value held exactly, and a zero as they are.
fn toward_zero(num: Num, value: f64) -> u64 {
    let nearest = unfloat(num, value);
    if !value.is_nan() && float(num, nearest).abs() > value.abs() {
        nearest - 1
    } else {
        nearest
    }
}

/// `value` rounded once to a single, as its bits, as [`unfloat`] has it.
pub(crate) fn single_bits(value: f64) -> u32 {
    low32(unfloat(Num::F32, value))
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "a register word is the low half of a lane's value"
)]
pub(crate) const fn low32(value: u64) -> u32 {
    value as u32
}

pub(crate) const fn high32(value: u64) -> u32 {
    low32(value >> 32)
}

/// GLSL's `FMin`: `y` where `y < x`, else `x`, a `NaN` as it falls.
fn minimum(x: f64, y: f64) -> f64 {
    if y < x {
        y
    } else {
        x
    }
}

/// GLSL's `FMax`: `y` where `x < y`, else `x`.
fn maximum(x: f64, y: f64) -> f64 {
    if x < y {
        y
    } else {
        x
    }
}

/// GLSL's `NMin` and `NMax`: of a `NaN` and a number, the number.
fn numeric(x: f64, y: f64, pick: impl Fn(f64, f64) -> f64) -> f64 {
    if x.is_nan() {
        y
    } else if y.is_nan() {
        x
    } else {
        pick(x, y)
    }
}

fn truth(holds: bool) -> u64 {
    u64::from(holds)
}

/// An operation over one operand's components, each changed in place.
pub(crate) type Unary = fn(Num, &mut [u64]);

/// An operation over two operands' components, the first changed in place.
pub(crate) type Binary = fn(Num, &mut [u64], &[u64]);

/// An operation over three operands' components, the first changed in place.
pub(crate) type Ternary = fn(Num, &mut [u64], &[u64], &[u64]);

/// A shift of components of the first `Num` by counts of the second.
pub(crate) type Shifter = fn(Num, Num, &mut [u64], &[u64]);

/// A conversion of components of the first `Num` into the second.
pub(crate) type Converter = fn(Num, Num, &mut [u64]);

/// `f` of each integer component, masked to its width.
fn ints(num: Num, xs: &mut [u64], f: impl Fn(u64, u32) -> u64) {
    let bits = num.bits();
    for x in xs {
        *x = f(*x, bits) & mask(bits);
    }
}

/// `f` of each float component, computed as a double and rounded once.
fn floats(num: Num, xs: &mut [u64], f: impl Fn(f64) -> f64) {
    for x in xs {
        *x = unfloat(num, f(float(num, *x))) & mask(num.bits());
    }
}

fn ints2(num: Num, xs: &mut [u64], ys: &[u64], f: impl Fn(u64, u64, u32) -> u64) {
    let bits = num.bits();
    for (x, &y) in xs.iter_mut().zip(ys) {
        *x = f(*x, y, bits) & mask(bits);
    }
}

fn floats2(num: Num, xs: &mut [u64], ys: &[u64], f: impl Fn(f64, f64) -> f64) {
    for (x, &y) in xs.iter_mut().zip(ys) {
        *x = unfloat(num, f(float(num, *x), float(num, y))) & mask(num.bits());
    }
}

fn floats3(num: Num, xs: &mut [u64], (ys, zs): (&[u64], &[u64]), f: impl Fn(f64, f64, f64) -> f64) {
    for ((x, &y), &z) in xs.iter_mut().zip(ys).zip(zs) {
        *x = unfloat(num, f(float(num, *x), float(num, y), float(num, z))) & mask(num.bits());
    }
}

fn ints3(
    num: Num,
    xs: &mut [u64],
    (ys, zs): (&[u64], &[u64]),
    f: impl Fn(u64, u64, u64, u32) -> u64,
) {
    let bits = num.bits();
    for ((x, &y), &z) in xs.iter_mut().zip(ys).zip(zs) {
        *x = f(*x, y, z, bits) & mask(bits);
    }
}

/// Whether `holds` of each pair of integer components, as a Boolean.
fn compares(num: Num, xs: &mut [u64], ys: &[u64], holds: impl Fn(u64, u64, u32) -> bool) {
    let bits = num.bits();
    for (x, &y) in xs.iter_mut().zip(ys) {
        *x = truth(holds(*x, y, bits));
    }
}

/// Whether each pair of float components stands as `holds` asks, an
/// unordered pair answering `unordered`.
fn float_compares(
    num: Num,
    xs: &mut [u64],
    ys: &[u64],
    unordered: bool,
    holds: impl Fn(core::cmp::Ordering) -> bool,
) {
    for (x, &y) in xs.iter_mut().zip(ys) {
        *x = truth(
            float(num, *x)
                .partial_cmp(&float(num, y))
                .map_or(unordered, &holds),
        );
    }
}

/// The kernel computing `op`, chosen once an instruction so no lane decides
/// what it computes.
pub(crate) fn unary(op: Op1) -> Unary {
    match op {
        Op1::SNegate => |num, xs| {
            ints(num, xs, |x, bits| {
                signed(x, bits).wrapping_neg().cast_unsigned()
            });
        },
        Op1::Not => |num, xs| ints(num, xs, |x, _| !x),
        Op1::FNegate => |num, xs| floats(num, xs, |v| -v),
        Op1::BitReverse => |num, xs| {
            ints(num, xs, |x, bits| {
                (x & mask(bits)).reverse_bits() >> (64 - bits)
            });
        },
        Op1::BitCount => {
            |num, xs| ints(num, xs, |x, bits| u64::from((x & mask(bits)).count_ones()))
        }
        Op1::LogicalNot => |num, xs| ints(num, xs, |x, _| truth(x == 0)),
        Op1::IsNan => |num, xs| {
            for x in xs {
                *x = truth(float(num, *x).is_nan());
            }
        },
        Op1::IsInf => |num, xs| {
            for x in xs {
                *x = truth(float(num, *x).is_infinite());
            }
        },
        Op1::Round => |num, xs| floats(num, xs, mathf::round),
        Op1::RoundEven => |num, xs| floats(num, xs, mathf::round_even),
        Op1::Trunc => |num, xs| floats(num, xs, mathf::trunc),
        Op1::FAbs => |num, xs| floats(num, xs, f64::abs),
        Op1::SAbs => |num, xs| {
            ints(num, xs, |x, bits| {
                signed(x, bits).wrapping_abs().cast_unsigned()
            });
        },
        Op1::FSign => |num, xs| floats(num, xs, sign),
        Op1::SSign => |num, xs| ints(num, xs, |x, bits| signed(x, bits).signum().cast_unsigned()),
        Op1::Floor => |num, xs| floats(num, xs, mathf::floor),
        Op1::Ceil => |num, xs| floats(num, xs, mathf::ceil),
        Op1::Fract => |num, xs| floats(num, xs, |v| v - mathf::floor(v)),
        Op1::Radians => |num, xs| floats(num, xs, f64::to_radians),
        Op1::Degrees => |num, xs| floats(num, xs, f64::to_degrees),
        Op1::Sin => |num, xs| floats(num, xs, mathf::sin),
        Op1::Cos => |num, xs| floats(num, xs, mathf::cos),
        Op1::Tan => |num, xs| floats(num, xs, mathf::tan),
        Op1::Asin => |num, xs| floats(num, xs, mathf::asin),
        Op1::Acos => |num, xs| floats(num, xs, mathf::acos),
        Op1::Atan => |num, xs| floats(num, xs, mathf::atan),
        Op1::Sinh => |num, xs| floats(num, xs, mathf::sinh),
        Op1::Cosh => |num, xs| floats(num, xs, mathf::cosh),
        Op1::Tanh => |num, xs| floats(num, xs, mathf::tanh),
        Op1::Asinh => |num, xs| floats(num, xs, mathf::asinh),
        Op1::Acosh => |num, xs| floats(num, xs, mathf::acosh),
        Op1::Atanh => |num, xs| floats(num, xs, mathf::atanh),
        Op1::Exp => |num, xs| floats(num, xs, mathf::exp),
        Op1::Log => |num, xs| floats(num, xs, mathf::ln),
        Op1::Exp2 => |num, xs| floats(num, xs, mathf::exp2),
        Op1::Log2 => |num, xs| floats(num, xs, mathf::log2),
        Op1::Sqrt => |num, xs| floats(num, xs, mathf::sqrt),
        Op1::InverseSqrt => |num, xs| floats(num, xs, |v| 1.0 / mathf::sqrt(v)),
        Op1::FindILsb => |num, xs| ints(num, xs, |x, bits| lowest(x & mask(bits))),
        Op1::FindUMsb => |num, xs| ints(num, xs, |x, bits| highest(x & mask(bits))),
        Op1::FindSMsb => |num, xs| {
            ints(num, xs, |x, bits| {
                let v = signed(x, bits);
                highest(if v < 0 { !v } else { v }.cast_unsigned())
            });
        },
    }
}

/// The position of `v`'s lowest set bit, or minus one for none.
const fn lowest(v: u64) -> u64 {
    if v == 0 {
        u64::MAX
    } else {
        v.trailing_zeros() as u64
    }
}

/// The position of `v`'s highest set bit, or minus one for none.
const fn highest(v: u64) -> u64 {
    match v.checked_ilog2() {
        Some(at) => at as u64,
        None => u64::MAX,
    }
}

fn sign(x: f64) -> f64 {
    if x > 0.0 {
        1.0
    } else if x < 0.0 {
        -1.0
    } else {
        0.0
    }
}

/// A floored modulo of doubles: the remainder takes the divisor's sign.
fn floored(a: f64, b: f64) -> f64 {
    let remainder = a % b;
    if remainder != 0.0 && (remainder < 0.0) != (b < 0.0) {
        remainder + b
    } else {
        remainder
    }
}

/// The kernel computing `op`, chosen once an instruction.
pub(crate) fn binary(op: Op2) -> Binary {
    match op {
        Op2::IAdd => |num, xs, ys| ints2(num, xs, ys, |x, y, _| x.wrapping_add(y)),
        Op2::ISub => |num, xs, ys| ints2(num, xs, ys, |x, y, _| x.wrapping_sub(y)),
        Op2::IMul => |num, xs, ys| ints2(num, xs, ys, |x, y, _| x.wrapping_mul(y)),
        Op2::SDiv => |num, xs, ys| {
            ints2(num, xs, ys, |x, y, bits| {
                let (sx, sy) = (signed(x, bits), signed(y, bits));
                if sy == 0 {
                    0
                } else {
                    sx.wrapping_div(sy).cast_unsigned()
                }
            });
        },
        Op2::UDiv => |num, xs, ys| {
            ints2(num, xs, ys, |x, y, bits| {
                (x & mask(bits)).checked_div(y & mask(bits)).unwrap_or(0)
            });
        },
        Op2::SRem => |num, xs, ys| {
            ints2(num, xs, ys, |x, y, bits| {
                let (sx, sy) = (signed(x, bits), signed(y, bits));
                if sy == 0 {
                    0
                } else {
                    sx.wrapping_rem(sy).cast_unsigned()
                }
            });
        },
        Op2::SMod => |num, xs, ys| {
            ints2(num, xs, ys, |x, y, bits| {
                let (sx, sy) = (signed(x, bits), signed(y, bits));
                if sy == 0 {
                    return 0;
                }
                let remainder = sx.wrapping_rem(sy);
                if remainder != 0 && (remainder < 0) != (sy < 0) {
                    remainder.wrapping_add(sy).cast_unsigned()
                } else {
                    remainder.cast_unsigned()
                }
            });
        },
        Op2::UMod => |num, xs, ys| {
            ints2(num, xs, ys, |x, y, bits| {
                (x & mask(bits)).checked_rem(y & mask(bits)).unwrap_or(0)
            });
        },
        Op2::BitAnd => |num, xs, ys| ints2(num, xs, ys, |x, y, _| x & y),
        Op2::BitOr => |num, xs, ys| ints2(num, xs, ys, |x, y, _| x | y),
        Op2::BitXor => |num, xs, ys| ints2(num, xs, ys, |x, y, _| x ^ y),
        Op2::FAdd => |num, xs, ys| floats2(num, xs, ys, |a, b| a + b),
        Op2::FSub => |num, xs, ys| floats2(num, xs, ys, |a, b| a - b),
        Op2::FMul => |num, xs, ys| floats2(num, xs, ys, |a, b| a * b),
        Op2::FDiv => |num, xs, ys| floats2(num, xs, ys, |a, b| a / b),
        Op2::FRem => |num, xs, ys| floats2(num, xs, ys, |a, b| a % b),
        Op2::FMod => |num, xs, ys| floats2(num, xs, ys, floored),
        Op2::LogicalAnd => |num, xs, ys| ints2(num, xs, ys, |x, y, _| truth(x != 0 && y != 0)),
        Op2::LogicalOr => |num, xs, ys| ints2(num, xs, ys, |x, y, _| truth(x != 0 || y != 0)),
        Op2::LogicalEqual => {
            |num, xs, ys| ints2(num, xs, ys, |x, y, _| truth((x != 0) == (y != 0)))
        }
        Op2::LogicalNotEqual => {
            |num, xs, ys| ints2(num, xs, ys, |x, y, _| truth((x != 0) != (y != 0)))
        }
        Op2::Atan2 => |num, xs, ys| floats2(num, xs, ys, mathf::atan2),
        Op2::Pow => |num, xs, ys| floats2(num, xs, ys, mathf::power),
        Op2::FMin => |num, xs, ys| floats2(num, xs, ys, minimum),
        Op2::FMax => |num, xs, ys| floats2(num, xs, ys, maximum),
        Op2::NMin => |num, xs, ys| floats2(num, xs, ys, |a, b| numeric(a, b, minimum)),
        Op2::NMax => |num, xs, ys| floats2(num, xs, ys, |a, b| numeric(a, b, maximum)),
        Op2::UMin => |num, xs, ys| {
            ints2(num, xs, ys, |x, y, bits| {
                (x & mask(bits)).min(y & mask(bits))
            });
        },
        Op2::UMax => |num, xs, ys| {
            ints2(num, xs, ys, |x, y, bits| {
                (x & mask(bits)).max(y & mask(bits))
            });
        },
        Op2::SMin => |num, xs, ys| {
            ints2(num, xs, ys, |x, y, bits| {
                signed(x, bits).min(signed(y, bits)).cast_unsigned()
            });
        },
        Op2::SMax => |num, xs, ys| {
            ints2(num, xs, ys, |x, y, bits| {
                signed(x, bits).max(signed(y, bits)).cast_unsigned()
            });
        },
        Op2::Step => |num, xs, ys| floats2(num, xs, ys, |edge, x| if x < edge { 0.0 } else { 1.0 }),
    }
}

/// `kernel` of one pair of components, as a span of one lane has it.
pub(crate) fn one_lane(kernel: Binary, num: Num, x: u64, y: u64) -> u64 {
    let mut lanes = [x];
    kernel(num, &mut lanes, &[y]);
    let [value] = lanes;
    value
}

/// The kernel computing `op`, chosen once an instruction.
pub(crate) fn ternary(op: Op3) -> Ternary {
    match op {
        Op3::FClamp => |num, xs, ys, zs| {
            floats3(num, xs, (ys, zs), |x, low, high| {
                minimum(maximum(x, low), high)
            });
        },
        Op3::NClamp => |num, xs, ys, zs| {
            floats3(num, xs, (ys, zs), |x, low, high| {
                numeric(numeric(x, low, maximum), high, minimum)
            });
        },
        Op3::UClamp => |num, xs, ys, zs| {
            ints3(num, xs, (ys, zs), |x, low, high, bits| {
                (x & mask(bits))
                    .max(low & mask(bits))
                    .min(high & mask(bits))
            });
        },
        Op3::SClamp => |num, xs, ys, zs| {
            ints3(num, xs, (ys, zs), |x, low, high, bits| {
                signed(x, bits)
                    .max(signed(low, bits))
                    .min(signed(high, bits))
                    .cast_unsigned()
            });
        },
        Op3::FMix => |num, xs, ys, zs| floats3(num, xs, (ys, zs), |x, y, a| x * (1.0 - a) + y * a),
        Op3::SmoothStep => |num, xs, ys, zs| {
            floats3(num, xs, (ys, zs), |low, high, x| {
                let t = ((x - low) / (high - low)).clamp(0.0, 1.0);
                let t = if t.is_nan() { 0.0 } else { t };
                t * t * (3.0 - 2.0 * t)
            });
        },
        Op3::Fma => |num, xs, ys, zs| floats3(num, xs, (ys, zs), |a, b, c| a * b + c),
    }
}

/// The kernel shifting by `op`: a count of the width or more gives zero.
pub(crate) fn shifter(op: Shift) -> Shifter {
    fn shifting(num: Num, by: Num, xs: &mut [u64], ys: &[u64], f: impl Fn(u64, u32, u32) -> u64) {
        let bits = num.bits();
        for (x, &y) in xs.iter_mut().zip(ys) {
            let count = y & mask(by.bits());
            *x = match u32::try_from(count) {
                Ok(count) if count < bits => f(*x, count, bits) & mask(bits),
                _ => 0,
            };
        }
    }
    match op {
        Shift::Left => |num, by, xs, ys| shifting(num, by, xs, ys, |x, count, _| x << count),
        Shift::RightLogical => {
            |num, by, xs, ys| shifting(num, by, xs, ys, |x, count, bits| (x & mask(bits)) >> count)
        }
        Shift::RightArithmetic => |num, by, xs, ys| {
            shifting(num, by, xs, ys, |x, count, bits| {
                (signed(x, bits) >> count).cast_unsigned()
            });
        },
    }
}

/// The kernel answering whether `op` holds, as a Boolean.
pub(crate) fn comparer(op: Cmp) -> Binary {
    use core::cmp::Ordering;
    use Relation::{Equal, Greater, GreaterEqual, Less, LessEqual, NotEqual};
    match op {
        Cmp::IEqual => {
            |num, xs, ys| compares(num, xs, ys, |x, y, bits| x & mask(bits) == y & mask(bits))
        }
        Cmp::INotEqual => {
            |num, xs, ys| compares(num, xs, ys, |x, y, bits| x & mask(bits) != y & mask(bits))
        }
        Cmp::UGreater => {
            |num, xs, ys| compares(num, xs, ys, |x, y, bits| x & mask(bits) > y & mask(bits))
        }
        Cmp::UGreaterEqual => {
            |num, xs, ys| compares(num, xs, ys, |x, y, bits| x & mask(bits) >= y & mask(bits))
        }
        Cmp::ULess => {
            |num, xs, ys| compares(num, xs, ys, |x, y, bits| x & mask(bits) < y & mask(bits))
        }
        Cmp::ULessEqual => {
            |num, xs, ys| compares(num, xs, ys, |x, y, bits| x & mask(bits) <= y & mask(bits))
        }
        Cmp::SGreater => {
            |num, xs, ys| compares(num, xs, ys, |x, y, bits| signed(x, bits) > signed(y, bits))
        }
        Cmp::SGreaterEqual => {
            |num, xs, ys| compares(num, xs, ys, |x, y, bits| signed(x, bits) >= signed(y, bits))
        }
        Cmp::SLess => {
            |num, xs, ys| compares(num, xs, ys, |x, y, bits| signed(x, bits) < signed(y, bits))
        }
        Cmp::SLessEqual => {
            |num, xs, ys| compares(num, xs, ys, |x, y, bits| signed(x, bits) <= signed(y, bits))
        }
        Cmp::Float { relation, ordered } => match (relation, ordered) {
            (Equal, true) => |num, xs, ys| float_compares(num, xs, ys, false, Ordering::is_eq),
            (Equal, false) => |num, xs, ys| float_compares(num, xs, ys, true, Ordering::is_eq),
            (NotEqual, true) => |num, xs, ys| float_compares(num, xs, ys, false, Ordering::is_ne),
            (NotEqual, false) => |num, xs, ys| float_compares(num, xs, ys, true, Ordering::is_ne),
            (Less, true) => |num, xs, ys| float_compares(num, xs, ys, false, Ordering::is_lt),
            (Less, false) => |num, xs, ys| float_compares(num, xs, ys, true, Ordering::is_lt),
            (Greater, true) => |num, xs, ys| float_compares(num, xs, ys, false, Ordering::is_gt),
            (Greater, false) => |num, xs, ys| float_compares(num, xs, ys, true, Ordering::is_gt),
            (LessEqual, true) => |num, xs, ys| float_compares(num, xs, ys, false, Ordering::is_le),
            (LessEqual, false) => |num, xs, ys| float_compares(num, xs, ys, true, Ordering::is_le),
            (GreaterEqual, true) => {
                |num, xs, ys| float_compares(num, xs, ys, false, Ordering::is_ge)
            }
            (GreaterEqual, false) => {
                |num, xs, ys| float_compares(num, xs, ys, true, Ordering::is_ge)
            }
        },
    }
}

/// The kernel converting by `op`.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "each cast is the conversion asked for: float to integer \
              saturates with NaN at zero, and integer to float rounds once"
)]
pub(crate) fn converter(op: Conv) -> Converter {
    fn converting(to: Num, xs: &mut [u64], f: impl Fn(u64) -> u64) {
        for x in xs {
            *x = f(*x) & mask(to.bits());
        }
    }
    match op {
        Conv::FloatToUnsigned => |from, to, xs| {
            let top = mask(to.bits());
            converting(to, xs, |x| {
                let value = float(from, x);
                if value.is_nan() || value <= 0.0 {
                    0
                } else if value >= top as f64 {
                    top
                } else {
                    value as u64
                }
            });
        },
        Conv::FloatToSigned => |from, to, xs| {
            let high = (mask(to.bits()) >> 1).cast_signed();
            let low = -high - 1;
            converting(to, xs, |x| {
                let value = float(from, x);
                let clamped = if value.is_nan() {
                    0
                } else if value <= low as f64 {
                    low
                } else if value >= high as f64 {
                    high
                } else {
                    value as i64
                };
                clamped.cast_unsigned()
            });
        },
        Conv::SignedToFloat => |from, to, xs| {
            converting(to, xs, |x| {
                let value = signed(x, from.bits());
                match to {
                    Num::F32 => u64::from((value as f32).to_bits()),
                    Num::F64 => (value as f64).to_bits(),
                    _ => unfloat(to, value as f64),
                }
            });
        },
        Conv::UnsignedToFloat => |from, to, xs| {
            converting(to, xs, |x| {
                let value = x & mask(from.bits());
                match to {
                    Num::F32 => u64::from((value as f32).to_bits()),
                    Num::F64 => (value as f64).to_bits(),
                    _ => unfloat(to, value as f64),
                }
            });
        },
        Conv::Unsigned => |from, to, xs| converting(to, xs, |x| x & mask(from.bits())),
        Conv::Signed => {
            |from, to, xs| converting(to, xs, |x| signed(x, from.bits()).cast_unsigned())
        }
        Conv::Float => |from, to, xs| converting(to, xs, |x| unfloat(to, float(from, x))),
        Conv::FloatTowardZero => {
            |from, to, xs| converting(to, xs, |x| toward_zero(to, float(from, x)))
        }
        Conv::QuantizeToF16 => |_, to, xs| {
            converting(to, xs, |x| u64::from(mathf::quantized_to_half(low32(x))));
        },
    }
}

/// The bits `offset..offset + count` of `base` replaced by the low bits of
/// `insert`; only the bits within the width change.
pub(crate) fn bit_insert(num: Num, base: u64, insert: u64, offset: u64, count: u64) -> u64 {
    let bits = u64::from(num.bits());
    if offset >= bits || count == 0 {
        return base & mask(num.bits());
    }
    let span = count.min(bits - offset);
    let field = mask(u32::try_from(span).unwrap_or(64)) << offset;
    ((base & !field) | ((insert << offset) & field)) & mask(num.bits())
}

/// The bits `offset..offset + count` of `base`, moved down, sign-extended
/// from the field's top where `signed`; zero for a field of no bits.
pub(crate) fn bit_extract(num: Num, base: u64, offset: u64, count: u64, sign: bool) -> u64 {
    let bits = u64::from(num.bits());
    if offset >= bits || count == 0 {
        return 0;
    }
    let span = u32::try_from(count.min(bits - offset)).unwrap_or(64);
    let field = (base >> offset) & mask(span);
    let value = if sign {
        signed(field, span).cast_unsigned()
    } else {
        field
    };
    value & mask(num.bits())
}

/// Two to the power `k`, exactly, for `k` in a double's normal exponents.
fn two_to(k: i64) -> f64 {
    f64::from_bits((k.clamp(-1022, 1023) + 1023).cast_unsigned() << 52)
}

/// `x` scaled by two to the power `exponent`, rounded once: the
/// significand is scaled exactly into the normal range, and only a step
/// down into the subnormals rounds.
pub(crate) fn ldexp(num: Num, x: u64, exponent: i64) -> u64 {
    let value = float(num, x);
    if value == 0.0 || !value.is_finite() {
        return unfloat(num, value);
    }
    let (significand, own) = split(value);
    let target = own.saturating_add(exponent);
    let scaled = if target > 1024 {
        f64::INFINITY.copysign(value)
    } else if target < -1080 {
        0.0_f64.copysign(value)
    } else if target >= -1021 {
        significand * 2.0 * two_to(target - 1)
    } else {
        significand * two_to(target + 64) * two_to(-64)
    };
    unfloat(num, scaled)
}

/// A finite, nonzero double as its significand in `[0.5, 1)`, with its
/// sign, and the exponent that scales it back.
fn split(value: f64) -> (f64, i64) {
    let exponent_of = |v: f64| i64::try_from((v.to_bits() >> 52) & 0x7ff).unwrap_or(0);
    let (normal, raw) = if exponent_of(value) == 0 {
        let lifted = value * two_to(64);
        (lifted, exponent_of(lifted) - 64)
    } else {
        (value, exponent_of(value))
    };
    let significand = f64::from_bits((normal.to_bits() & !(0x7ff << 52)) | (1022 << 52));
    (significand, raw - 1022)
}

/// A float's significand in `[0.5, 1)` with its sign, and its exponent:
/// zero, an infinity and a `NaN` keep themselves with an exponent of zero.
pub(crate) fn frexp(num: Num, x: u64) -> (u64, u64) {
    let value = float(num, x);
    if value == 0.0 || !value.is_finite() {
        return (unfloat(num, value), 0);
    }
    let (significand, exponent) = split(value);
    (unfloat(num, significand), exponent.cast_unsigned())
}

/// A float's fraction and its whole part, both of its sign.
pub(crate) fn modf(num: Num, x: u64) -> (u64, u64) {
    let value = float(num, x);
    let whole = if value.is_nan() {
        value
    } else {
        mathf::trunc(value)
    };
    let fraction = if value.is_nan() {
        value
    } else if value.is_infinite() {
        0.0_f64.copysign(value)
    } else {
        (value - whole).copysign(value)
    };
    (unfloat(num, fraction), unfloat(num, whole))
}

#[cfg(test)]
#[path = "ops_tests.rs"]
mod tests;
