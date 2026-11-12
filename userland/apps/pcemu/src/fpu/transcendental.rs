//! FPATAN, F2XM1, FYL2X and FYL2XP1: their operands' special cases, which
//! the 8087 leaves unchecked and the 80387 defines, the exact answers
//! (a logarithm of a power of two, a power of two at an integer), and the
//! rest computed by `trans` and rounded to 64 bits. None checks for a
//! denormal operand, as the 8087's do not.

use super::super::arith::{self, flag, Exact, Parts, Rounding};
use super::super::real::{Real, INTEGER};
use super::super::trans;
use super::super::Registers;
use super::{screen, Outcome, Value};

/// `x` rounded to 64 bits in the control word's mode.
fn rounded(regs: &Registers, x: Exact) -> Outcome {
    Outcome::rounded(
        arith::round(x, Rounding::extended(regs.mode()), regs.masks()),
        0,
    )
}

/// An exact result, a power of two times `y`: k × y.
fn times_integer(regs: &Registers, y: Parts, k: i64) -> Outcome {
    if k == 0 {
        return Outcome::value(Real::ZERO.with_sign(y.negative), 0);
    }
    let factor = Parts::of(Real::from_int(k));
    Outcome::rounded(
        arith::round(
            arith::multiply(y, factor),
            Rounding::extended(regs.mode()),
            regs.masks(),
        ),
        0,
    )
}

/// The exponent of a positive power of two.
fn power_of_two(p: Parts) -> Option<i32> {
    let p = p.normalized();
    (!p.negative && p.sig == INTEGER).then_some(p.exp)
}

/// π × `quarters` / 4, rounded.
fn quarter_pis(regs: &Registers, quarters: u32, negative: bool) -> Outcome {
    let x = trans::quarter_pis(quarters);
    rounded(regs, Exact { negative, ..x })
}

fn infinity(negative: bool) -> Outcome {
    Outcome::value(Real::INFINITY.with_sign(negative), 0)
}

fn zero(negative: bool) -> Outcome {
    Outcome::value(Real::ZERO.with_sign(negative), 0)
}

/// FPATAN: the angle of (`x`, `y`).
pub(super) fn arctangent(regs: &Registers, y: Value, x: Value) -> Outcome {
    if let Err(outcome) = screen(regs, y, x, false) {
        return outcome;
    }
    let sign = y.negative();
    match (y, x) {
        // On the axis, or against an infinite run: zero or π.
        (
            Value::Zero(_),
            Value::Zero(negative)
            | Value::Infinity(negative)
            | Value::Finite(Parts { negative, .. }, _),
        )
        | (Value::Finite(..), Value::Infinity(negative)) => {
            if negative {
                quarter_pis(regs, 4, sign)
            } else {
                zero(sign)
            }
        }
        (Value::Infinity(_), Value::Infinity(negative)) => {
            quarter_pis(regs, if negative { 3 } else { 1 }, sign)
        }
        (Value::Infinity(_), _) | (Value::Finite(..), Value::Zero(_)) => quarter_pis(regs, 2, sign),
        (Value::Finite(rise, _), Value::Finite(run, _)) => rounded(regs, trans::atan2(rise, run)),
        _ => Outcome::invalid(regs, flag::INVALID),
    }
}

/// F2XM1: 2^x − 1.
pub(super) fn two_power_minus_one(regs: &Registers, x: Value) -> Outcome {
    match x {
        Value::Empty => Outcome::invalid(regs, flag::INVALID),
        Value::Nan(nan) => Outcome {
            value: regs.masked(flag::INVALID).then_some(nan),
            flags: flag::INVALID,
        },
        Value::Zero(negative) => zero(negative),
        Value::Infinity(false) => infinity(false),
        Value::Infinity(true) => Outcome::value(Real::ONE.with_sign(true), 0),
        Value::Finite(p, _) => match small_integer(p) {
            // 2^n − 1 exactly.
            Some(n) => {
                let one = Parts::of(Real::ONE);
                let power = Parts { exp: n, ..one };
                rounded_exact(regs, arith::add(power, one, true))
            }
            None => rounded(regs, trans::exp2m1(p)),
        },
    }
}

/// An exact result rounded as one: no sticky bits below it.
fn rounded_exact(regs: &Registers, x: Exact) -> Outcome {
    if x.is_zero() {
        return zero(regs.mode() == arith::Mode::Down);
    }
    Outcome::rounded(
        arith::round(x, Rounding::extended(regs.mode()), regs.masks()),
        0,
    )
}

/// `p` as an integer of at most 64 places, where it is one.
fn small_integer(p: Parts) -> Option<i32> {
    let p = p.normalized();
    if p.is_zero() || !(0..64).contains(&p.exp) {
        return None;
    }
    let places = u32::try_from(63 - p.exp).ok()?;
    let fraction = if places == 0 {
        0
    } else {
        p.sig & ((1 << places) - 1)
    };
    if fraction != 0 {
        return None;
    }
    let magnitude = i32::try_from(p.sig >> places).ok()?;
    Some(if p.negative { -magnitude } else { magnitude })
}

/// FYL2X: y × log2(x).
pub(super) fn y_log2_x(regs: &Registers, y: Value, x: Value) -> Outcome {
    if let Err(outcome) = screen(regs, y, x, false) {
        return outcome;
    }
    let sign = y.negative();
    match (y, x) {
        (_, Value::Infinity(true) | Value::Finite(Parts { negative: true, .. }, _))
        | (Value::Zero(_), Value::Zero(_) | Value::Infinity(false)) => {
            Outcome::invalid(regs, flag::INVALID)
        }
        // log2(0) is minus infinity.
        (Value::Infinity(_), Value::Zero(_)) => infinity(!sign),
        (Value::Finite(..), Value::Zero(_)) => Outcome {
            value: regs
                .masked(flag::ZERO_DIVIDE)
                .then_some(Real::INFINITY.with_sign(!sign)),
            flags: flag::ZERO_DIVIDE,
        },
        (_, Value::Infinity(false)) => infinity(sign),
        (_, Value::Finite(argument, _)) => {
            let below_one = argument.normalized().exp < 0;
            match (y, power_of_two(argument)) {
                (Value::Infinity(_), Some(0)) => Outcome::invalid(regs, flag::INVALID),
                (Value::Infinity(_), _) => infinity(sign != below_one),
                (Value::Zero(_), _) => zero(sign != below_one),
                (Value::Finite(scale, _), Some(k)) => times_integer(regs, scale, i64::from(k)),
                (Value::Finite(scale, _), None) => rounded(regs, trans::y_log2_x(scale, argument)),
                _ => Outcome::invalid(regs, flag::INVALID),
            }
        }
        _ => Outcome::invalid(regs, flag::INVALID),
    }
}

/// FYL2XP1: y × log2(1 + x).
pub(super) fn y_log2_x_plus_1(regs: &Registers, y: Value, x: Value) -> Outcome {
    if let Err(outcome) = screen(regs, y, x, false) {
        return outcome;
    }
    let sign = y.negative();
    match (y, x) {
        (Value::Infinity(_), Value::Zero(_))
        | (_, Value::Infinity(true))
        | (Value::Zero(_), Value::Infinity(false)) => Outcome::invalid(regs, flag::INVALID),
        (_, Value::Zero(negative)) => zero(sign != negative),
        (_, Value::Infinity(false)) => infinity(sign),
        (_, Value::Finite(argument, _)) => {
            let sum = arith::add(Parts::of(Real::ONE), argument, false);
            if sum.negative || sum.is_zero() {
                // 1 + x not positive: past the 80387's domain.
                return match (y, sum.is_zero()) {
                    (Value::Finite(..), true) => Outcome {
                        value: regs
                            .masked(flag::ZERO_DIVIDE)
                            .then_some(Real::INFINITY.with_sign(!sign)),
                        flags: flag::ZERO_DIVIDE,
                    },
                    _ => Outcome::invalid(regs, flag::INVALID),
                };
            }
            let exact_power = (!sum.sticky).then(|| exact_power(sum)).flatten();
            match (y, exact_power) {
                (Value::Infinity(_), _) => infinity(sign != argument.negative),
                (Value::Zero(_), _) => zero(sign != argument.negative),
                (Value::Finite(scale, _), Some(k)) => times_integer(regs, scale, i64::from(k)),
                (Value::Finite(scale, _), None) => {
                    rounded(regs, trans::y_log2_1p_x(scale, argument))
                }
                _ => Outcome::invalid(regs, flag::INVALID),
            }
        }
        _ => Outcome::invalid(regs, flag::INVALID),
    }
}

/// The exponent of an exact positive sum that is a power of two.
fn exact_power(sum: Exact) -> Option<i32> {
    let zeros = sum.sig.leading_zeros();
    let top = 127u32.checked_sub(zeros)?;
    (sum.sig == 1 << top).then(|| sum.exp - (126 - i32::try_from(top).unwrap_or(126)))
}
