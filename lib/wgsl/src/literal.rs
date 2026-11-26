//! Literal values: an integer or float literal's text read as the value of
//! the type its suffix names, its significand truncated as the
//! specification has it — twenty significant decimal digits, sixteen
//! hexadecimal — and rounded once to its type.

use alloc::string::String;
use core::cmp::Ordering;
use core::fmt::Write as _;

use tairix_util::mathf;

use crate::diag::{error, Code, Outcome, Span, Stop};
use crate::eval::{half, Value};
use crate::lex::Shape;
use crate::types::Scalar;

/// The most significant digits a decimal significand keeps.
const DECIMAL_DIGITS: usize = 20;

/// The most significant digits a hexadecimal significand keeps.
const HEX_DIGITS: usize = 16;

/// The furthest a decimal exponent is followed: past it every value is
/// infinite or zero in every type.
const EXPONENT_BOUND: i64 = 100_000;

/// The value of the literal `text`, whose shape the lexer found, at `span`;
/// `f16` whether the module enables it.
///
/// # Errors
///
/// A value its type cannot hold, a float overflowing it or a hexadecimal
/// float its suffixed type cannot hold exactly, or `h` without `f16`.
pub(crate) fn value(text: &str, shape: Shape, span: Span, f16: bool) -> Outcome<Value> {
    if shape.suffix == Some(b'h') && !f16 {
        return Err(error(Code::Type, span, |m| {
            m.write_str("an `h` literal is an f16, which needs `enable f16;`")
        }));
    }
    if shape.float {
        float(text, shape, span)
    } else {
        integer(text, shape, span)
    }
}

fn out_of_range(span: Span, of: &str) -> Stop {
    error(Code::LiteralRange, span, |m| {
        write!(m, "the literal is out of the range of {of}")
    })
}

fn integer(text: &str, shape: Shape, span: Span) -> Outcome<Value> {
    let digits = text
        .trim_end_matches(['i', 'u'])
        .trim_start_matches("0x")
        .trim_start_matches("0X");
    let radix = if shape.hex { 16 } else { 10 };
    let mut magnitude: u128 = 0;
    for ch in digits.chars() {
        let digit = ch
            .to_digit(radix)
            .ok_or_else(|| out_of_range(span, "an integer"))?;
        magnitude = magnitude
            .checked_mul(u128::from(radix))
            .and_then(|m| m.checked_add(u128::from(digit)))
            .filter(|&m| m <= u128::from(u64::MAX))
            .ok_or_else(|| out_of_range(span, "abstract-int"))?;
    }
    match shape.suffix {
        Some(b'i') => i32::try_from(magnitude)
            .map(Value::I32)
            .map_err(|_| out_of_range(span, "i32")),
        Some(b'u') => u32::try_from(magnitude)
            .map(Value::U32)
            .map_err(|_| out_of_range(span, "u32")),
        _ => i64::try_from(magnitude)
            .map(Value::AbstractInt)
            .map_err(|_| out_of_range(span, "abstract-int")),
    }
}

fn float(text: &str, shape: Shape, span: Span) -> Outcome<Value> {
    let target = match shape.suffix {
        Some(b'f') => Scalar::F32,
        Some(b'h') => Scalar::F16,
        _ => Scalar::AbstractFloat,
    };
    let body = text.trim_end_matches(['f', 'h']);
    if shape.hex {
        hex_float(body, target, span)
    } else {
        decimal_float(body, target, span)
    }
}

/// The digits of a significand, its point's place among them, and its
/// exponent, read from `body`: `marker` begins the exponent.
fn split(body: &str, marker: [char; 2]) -> (&str, &str, i64) {
    let (significand, exponent) = match body.find(marker) {
        Some(at) => (&body[..at], &body[at + 1..]),
        None => (body, ""),
    };
    let exponent = parse_exponent(exponent);
    let (whole, fraction) = significand.split_once('.').unwrap_or((significand, ""));
    (whole, fraction, exponent)
}

/// A decimal exponent, held within [`EXPONENT_BOUND`] of zero.
fn parse_exponent(text: &str) -> i64 {
    let (negative, digits) = match text.as_bytes().first() {
        Some(b'-') => (true, &text[1..]),
        Some(b'+') => (false, &text[1..]),
        _ => (false, text),
    };
    let mut value: i64 = 0;
    for ch in digits.bytes() {
        value = (value * 10 + i64::from(ch - b'0')).min(EXPONENT_BOUND);
    }
    if negative {
        -value
    } else {
        value
    }
}

fn decimal_float(body: &str, target: Scalar, span: Span) -> Outcome<Value> {
    let (whole, fraction, exponent) = split(body, ['e', 'E']);
    let mut digits = String::new();
    let mut scale = exponent;
    let mut significant = 0usize;
    for (index, ch) in whole.chars().chain(fraction.chars()).enumerate() {
        let in_fraction = index >= whole.len();
        if significant == 0 && ch == '0' {
            if in_fraction {
                scale -= 1;
            }
            continue;
        }
        if significant < DECIMAL_DIGITS {
            digits.try_reserve(1).map_err(|_| Stop::OutOfMemory)?;
            digits.push(ch);
            significant += 1;
            if in_fraction {
                scale -= 1;
            }
        } else if !in_fraction {
            // A dropped digit before the point still counts in the scale.
            scale += 1;
        }
    }
    if digits.is_empty() {
        return Ok(zero(target));
    }
    let scale = scale.clamp(-EXPONENT_BOUND, EXPONENT_BOUND);
    let mut canonical = String::new();
    write!(canonical, "{digits}e{scale}").map_err(|_| Stop::OutOfMemory)?;
    let finite = |x: f64| {
        if x.is_finite() {
            Ok(x)
        } else {
            Err(out_of_range(span, target.name()))
        }
    };
    match target {
        Scalar::F32 => {
            let x: f32 = canonical.parse().map_err(|_| out_of_range(span, "f32"))?;
            finite(f64::from(x)).map(|_| Value::F32(x))
        }
        Scalar::F16 => {
            let x: f64 = canonical.parse().map_err(|_| out_of_range(span, "f16"))?;
            let bits = settle_tie(half(finite(x)?), x, &digits, scale);
            finite(f64::from(mathf::from_half(bits))).map(|_| Value::F16(bits))
        }
        _ => {
            let x: f64 = canonical
                .parse()
                .map_err(|_| out_of_range(span, "abstract-float"))?;
            finite(x).map(Value::AbstractFloat)
        }
    }
}

/// The binary16 `rounded` the double `x` rounds to, corrected where `x` is
/// a tie between two halves: the double nearest a decimal lies on a tie
/// only when the decimal is the tie or lies within half a double's unit of
/// it, so the decimal `digits` × 10^`scale` itself decides which way.
#[expect(
    clippy::float_cmp,
    reason = "a tie is the midpoint exactly, so only exact equality decides one"
)]
fn settle_tie(rounded: u16, x: f64, digits: &str, scale: i64) -> u16 {
    // The largest finite half, and the tie past which a value is infinite.
    const LARGEST: u16 = 0x7bff;
    const OVERFLOW_TIE: f64 = 65520.0;
    if x == OVERFLOW_TIE {
        return match decimal_against(digits, scale, x) {
            Some(Ordering::Less) => LARGEST,
            _ => rounded,
        };
    }
    let here = f64::from(mathf::from_half(rounded));
    if here == x || !here.is_finite() {
        return rounded;
    }
    // The neighbour on x's other side, a step of one in the magnitude bits.
    let other = if here < x { rounded + 1 } else { rounded - 1 };
    let there = f64::from(mathf::from_half(other));
    if f64::midpoint(here, there) != x {
        return rounded;
    }
    let (low, high) = if here < there {
        (rounded, other)
    } else {
        (other, rounded)
    };
    match decimal_against(digits, scale, x) {
        Some(Ordering::Greater) => high,
        Some(Ordering::Less) => low,
        _ => rounded,
    }
}

/// How the decimal `digits` × 10^`scale` compares with the binary64 `x`,
/// exactly: `x`'s decimal expansion is finite and, for a half's tie, short.
fn decimal_against(digits: &str, scale: i64, x: f64) -> Option<Ordering> {
    let decimal: u128 = digits.parse().ok()?;
    let bits = x.to_bits();
    let exponent = i64::try_from((bits >> 52) & 0x7ff).ok()?;
    let significand = (bits & ((1u64 << 52) - 1)) | (1u64 << 52);
    let shift = significand.trailing_zeros();
    let odd = u128::from(significand >> shift);
    let power = exponent - 1075 + i64::from(shift);
    // x = odd × 2^power = mantissa × 10^place.
    let (mantissa, place) = if power >= 0 {
        (odd.checked_shl(u32::try_from(power).ok()?)?, 0)
    } else {
        let fives = 5u128.checked_pow(u32::try_from(-power).ok()?)?;
        (odd.checked_mul(fives)?, power)
    };
    let tens = |n: i64| 10u128.checked_pow(u32::try_from(n).ok()?);
    Some(if scale >= place {
        match tens(scale - place).and_then(|t| decimal.checked_mul(t)) {
            Some(lhs) => lhs.cmp(&mantissa),
            None => Ordering::Greater,
        }
    } else {
        match tens(place - scale).and_then(|t| mantissa.checked_mul(t)) {
            Some(rhs) => decimal.cmp(&rhs),
            None => Ordering::Less,
        }
    })
}

const fn zero(target: Scalar) -> Value {
    match target {
        Scalar::F32 => Value::F32(0.0),
        Scalar::F16 => Value::F16(0),
        _ => Value::AbstractFloat(0.0),
    }
}

/// A binary format: its significand's bits, the binary point included,
/// and its least and greatest normal exponents.
struct Format {
    precision: u32,
    min_exponent: i64,
    max_exponent: i64,
}

const fn format_of(target: Scalar) -> Format {
    match target {
        Scalar::F32 => Format {
            precision: 24,
            min_exponent: -126,
            max_exponent: 127,
        },
        Scalar::F16 => Format {
            precision: 11,
            min_exponent: -14,
            max_exponent: 15,
        },
        _ => Format {
            precision: 53,
            min_exponent: -1022,
            max_exponent: 1023,
        },
    }
}

fn hex_float(body: &str, target: Scalar, span: Span) -> Outcome<Value> {
    let body = body.get(2..).unwrap_or("");
    let (whole, fraction, exponent) = split(body, ['p', 'P']);
    // The significand as an integer of at most sixteen significant digits,
    // and the power of two it is scaled by.
    let mut mantissa: u64 = 0;
    let mut scale = exponent;
    let mut significant = 0usize;
    for (index, ch) in whole.chars().chain(fraction.chars()).enumerate() {
        let in_fraction = index >= whole.len();
        let digit = u64::from(ch.to_digit(16).unwrap_or(0));
        if significant == 0 && digit == 0 {
            if in_fraction {
                scale -= 4;
            }
            continue;
        }
        if significant < HEX_DIGITS {
            mantissa = (mantissa << 4) | digit;
            significant += 1;
            if in_fraction {
                scale -= 4;
            }
        } else if !in_fraction {
            scale += 4;
        }
    }
    if mantissa == 0 {
        return Ok(zero(target));
    }
    let Some((x, exact)) = round(mantissa, scale, &format_of(target)) else {
        return Err(out_of_range(span, target.name()));
    };
    if target != Scalar::AbstractFloat && !exact {
        return Err(error(Code::LiteralRange, span, |m| {
            write!(
                m,
                "the literal is not exactly representable in {}",
                target.name()
            )
        }));
    }
    Ok(match target {
        Scalar::F32 => Value::F32(mathf::single(x)),
        Scalar::F16 => Value::F16(half(x)),
        _ => Value::AbstractFloat(x),
    })
}

/// `mantissa` × 2^`scale` rounded to nearest, ties to even, in `format`,
/// as the binary64 holding it, with whether no bit was lost; `None` where it
/// overflows.
fn round(mantissa: u64, scale: i64, format: &Format) -> Option<(f64, bool)> {
    let bits = i64::from(64 - mantissa.leading_zeros());
    // The weight of the value's leading bit.
    let exponent = scale + bits - 1;
    // The significand narrows below the least normal exponent.
    let precision = i64::from(format.precision) - (format.min_exponent - exponent).max(0);
    let least = format.min_exponent - i64::from(format.precision) + 1;
    if precision <= 0 {
        // At most half the least subnormal: zero, unless past exactly half.
        let past_half = precision == 0 && !mantissa.is_power_of_two();
        return Some((if past_half { pow2(least) } else { 0.0 }, false));
    }
    let drop = (bits - precision).max(0);
    let (kept, exact) = if drop == 0 {
        (mantissa, true)
    } else {
        let drop = u32::try_from(drop).unwrap_or(63);
        let kept = mantissa >> drop;
        let rest = mantissa & ((1u64 << drop) - 1);
        let half = 1u64 << (drop - 1);
        let up = rest > half || (rest == half && kept & 1 == 1);
        (kept + u64::from(up), rest == 0)
    };
    let value = scaled(kept, scale + drop);
    (value.is_finite() && value <= max_of(format)).then_some((value, exact))
}

/// `kept`, of at most 54 significant bits, × 2^`shift`: exact wherever
/// binary64 holds the result, scaled in two steps where one power alone
/// would underflow.
fn scaled(kept: u64, shift: i64) -> f64 {
    // Rounded to at most the binary64 precision first, so exact.
    #[allow(clippy::cast_precision_loss)]
    let base = kept as f64;
    if shift >= -1022 {
        base * pow2(shift)
    } else {
        base * pow2(-1022) * pow2(shift + 1022)
    }
}

/// 2^`n` as a binary64, built from its bits: zero or infinite past its
/// range.
pub(crate) fn pow2(n: i64) -> f64 {
    if n > 1023 {
        f64::INFINITY
    } else if n >= -1022 {
        f64::from_bits(u64::try_from(n + 1023).unwrap_or(0) << 52)
    } else if n >= -1074 {
        f64::from_bits(1u64 << u32::try_from(n + 1074).unwrap_or(0))
    } else {
        0.0
    }
}

/// The largest finite value of `format`, as a binary64.
fn max_of(format: &Format) -> f64 {
    (2.0 - pow2(1 - i64::from(format.precision))) * pow2(format.max_exponent)
}

#[cfg(test)]
#[path = "literal_tests.rs"]
mod tests;
