//! `math.h` as RISC OS 5.30's library computes it, every step worked by an
//! FPA as the processor's would work it: the elementary functions its
//! double-precision instructions, the hyperbolic ones Cody and Waite's
//! algorithms over them (*Software Manual for the Elementary Functions*,
//! 1980).
//!
//! The errors are the exceptions the work raised: an invalid operation a
//! domain error, `-HUGE_VAL` with `EDOM`; a division by zero or an overflow a
//! range error, `HUGE_VAL` with the answer's sign and `ERANGE`. The three
//! never trap and are never left flagged. Some functions are worked on an
//! FPA of the library's own, leaving the client's flags as they were, and
//! answer an underflow — a result tiny and inexact, nought among them — as a
//! range error too; the rest leave the client the other flags they raise, an
//! underflow among them, and take the underflow or inexact trap where the
//! client enables it, as RISC OS 5.30's do. `ldexp` is worked on the
//! library's own FPA, its underflow a range error, then raises its underflow
//! or inexact result to the client.
//!
//! A double comes in a register pair, its high word first, and goes back in
//! F0.

use tairix_riscos_arm::fpa::{fpsr, Exception, Precision, Rounding};
use tairix_riscos_arm::instruction::{FpaDyadicOp, FpaMonadicOp};
use tairix_riscos_kernel::os::{Fail, Os, Regs};
use tairix_softfloat::fpa_words;

use super::errno::{self, EDOM, ERANGE};
use super::fpa::{answer, biased_exponent, double, quietly, Unit, FRACTION};
use crate::kernel::context;

/// `HUGE_VAL`: the largest double, as RISC OS 5.30's library has it.
pub(crate) const HUGE_VAL: f64 = f64::MAX;

/// The double R0 and R1 hold.
fn first(regs: &Regs) -> f64 {
    double(regs, 0)
}

/// The double R2 and R3 hold.
fn second(regs: &Regs) -> f64 {
    double(regs, 2)
}

/// `errno` made `value` for the client calling.
pub(crate) fn set_errno(os: &mut Os<'_>, regs: &Regs, value: u32) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    errno::set(os, statics, value)
}

/// The exceptions the library answers as errors, by their flags.
const ERRORS: u32 = Exception::InvalidOperation.flag()
    | Exception::DivisionByZero.flag()
    | Exception::Overflow.flag();

/// How a function answers an underflow its work raises: a result tiny and
/// inexact, nought among them.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tiny {
    /// A range error, as a function whose flags the client never sees, and
    /// `ldexp`, answer it.
    RangeError,
    /// The flag alone, the client's to see.
    Flagged,
}

/// `value`, worked with the exceptions `raised`, as the library answers it.
fn settled(os: &mut Os<'_>, regs: &Regs, value: f64, raised: u32, tiny: Tiny) -> Result<f64, Fail> {
    if raised & Exception::InvalidOperation.flag() != 0 {
        set_errno(os, regs, EDOM)?;
        Ok(-HUGE_VAL)
    } else if raised & ERRORS != 0 {
        set_errno(os, regs, ERANGE)?;
        Ok(HUGE_VAL.copysign(value))
    } else {
        if tiny == Tiny::RangeError && raised & Exception::Underflow.flag() != 0 {
            set_errno(os, regs, ERANGE)?;
        }
        Ok(value)
    }
}

/// `work` on the library's own FPA, nothing trapped: its answer, and the
/// exceptions it raised.
fn quiet(work: impl FnOnce(&mut Unit) -> Result<f64, Exception>) -> (f64, u32) {
    let mut unit = Unit::own();
    let value = work(&mut unit).unwrap_or(f64::NAN);
    (value, unit.fpsr() & fpsr::FLAGS)
}

/// `work` on an FPA working as the client's, but for the errors, which
/// neither trap nor stay flagged: its answer and the exceptions it raised, the
/// others left the client's flags. None where a trap of the client's was
/// raised and came back.
fn visible(
    os: &mut Os<'_>,
    work: impl FnOnce(&mut Unit) -> Result<f64, Exception>,
) -> Result<Option<(f64, u32)>, Fail> {
    let entry = os.fpsr();
    let mut unit = Unit::client(os);
    unit.set_fpsr(entry & !(fpsr::FLAGS | ERRORS << 16));
    let worked = work(&mut unit);
    let raised = unit.fpsr() & fpsr::FLAGS;
    os.set_fpsr(entry | raised & !ERRORS);
    match worked {
        Ok(value) => Ok(Some((value, raised))),
        Err(exception) => {
            os.floating_point(exception)?;
            Ok(None)
        }
    }
}

/// A function worked on the library's own FPA, answered in F0.
fn give_quiet(
    os: &mut Os<'_>,
    regs: &Regs,
    work: impl FnOnce(&mut Unit) -> Result<f64, Exception>,
) -> Result<(), Fail> {
    let (value, raised) = quiet(work);
    let value = settled(os, regs, value, raised, Tiny::RangeError)?;
    answer(os, value);
    Ok(())
}

/// A function worked on the library's own FPA and settled, then the
/// underflow or inexact result its work raised raised to the client as an
/// operation raises one: `errno` set before any trap, and an exact answer,
/// however small, raising nothing.
fn give_settled_then_raised(
    os: &mut Os<'_>,
    regs: &Regs,
    work: impl FnOnce(&mut Unit) -> Result<f64, Exception>,
) -> Result<(), Fail> {
    let (value, raised) = quiet(work);
    let value = settled(os, regs, value, raised, Tiny::RangeError)?;
    let rounding = [Exception::Underflow, Exception::Inexact]
        .into_iter()
        .find(|exception| raised & exception.flag() != 0);
    let trapped = rounding.and_then(|exception| {
        let mut unit = Unit::client(os);
        let trapped = unit.raise(exception).err();
        os.set_fpsr(unit.fpsr());
        trapped
    });
    if let Some(exception) = trapped {
        os.floating_point(exception)?;
    } else {
        answer(os, value);
    }
    Ok(())
}

/// A function worked as the client's FPA, answered in F0, or its value for
/// another to answer.
pub(crate) fn worked_visibly(
    os: &mut Os<'_>,
    regs: &Regs,
    work: impl FnOnce(&mut Unit) -> Result<f64, Exception>,
) -> Result<Option<f64>, Fail> {
    let Some((value, raised)) = visible(os, work)? else {
        return Ok(None);
    };
    settled(os, regs, value, raised, Tiny::Flagged).map(Some)
}

/// [`worked_visibly`]'s answer given in F0.
fn give_visibly(
    os: &mut Os<'_>,
    regs: &Regs,
    work: impl FnOnce(&mut Unit) -> Result<f64, Exception>,
) -> Result<(), Fail> {
    if let Some(value) = worked_visibly(os, regs, work)? {
        answer(os, value);
    }
    Ok(())
}

/// `acos(x)`.
pub(super) fn acos(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let x = first(regs);
    give_quiet(os, regs, |unit| unit.double_op(FpaMonadicOp::Acs, x))
}

/// `asin(x)`.
pub(super) fn asin(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let x = first(regs);
    give_quiet(os, regs, |unit| unit.double_op(FpaMonadicOp::Asn, x))
}

/// `atan(x)`.
pub(super) fn atan(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let x = first(regs);
    give_visibly(os, regs, |unit| unit.double_op(FpaMonadicOp::Atn, x))
}

/// `cos(x)`.
pub(super) fn cos(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let x = first(regs);
    give_quiet(os, regs, |unit| unit.double_op(FpaMonadicOp::Cos, x))
}

/// `sin(x)`.
pub(super) fn sin(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let x = first(regs);
    give_quiet(os, regs, |unit| unit.double_op(FpaMonadicOp::Sin, x))
}

/// `tan(x)`.
pub(super) fn tan(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let x = first(regs);
    give_quiet(os, regs, |unit| unit.double_op(FpaMonadicOp::Tan, x))
}

/// `exp(x)`.
pub(super) fn exp(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let x = first(regs);
    give_quiet(os, regs, |unit| unit.double_op(FpaMonadicOp::Exp, x))
}

/// `log(x)`.
pub(super) fn log(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let x = first(regs);
    give_visibly(os, regs, |unit| unit.double_op(FpaMonadicOp::Lgn, x))
}

/// `log10(x)`.
pub(super) fn log10(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let x = first(regs);
    give_visibly(os, regs, |unit| unit.double_op(FpaMonadicOp::Log, x))
}

/// `sqrt(x)`.
pub(super) fn sqrt(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let x = first(regs);
    give_visibly(os, regs, |unit| unit.double_op(FpaMonadicOp::Sqt, x))
}

/// `atan2(y, x)`: the FPA's polar angle of (x, y), a multiple of a quarter
/// turn — either nought or infinite — raising nothing, as RISC OS 5.30's does.
pub(super) fn atan2(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (y, x) = (first(regs), second(regs));
    let polar =
        |unit: &mut Unit| unit.dyadic(FpaDyadicOp::Pol, Precision::Double, Rounding::Nearest, x, y);
    if x == 0.0 || y == 0.0 || x.is_infinite() || y.is_infinite() {
        return give_quiet(os, regs, polar);
    }
    give_visibly(os, regs, polar)
}

/// `pow(x, y)`: the FPA's power, but nought to a negative power is a domain
/// error, not the infinity the FPA divides by zero to.
pub(super) fn pow(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (x, y) = (first(regs), second(regs));
    if x == 0.0 && y < 0.0 {
        set_errno(os, regs, EDOM)?;
        answer(os, -HUGE_VAL);
        return Ok(());
    }
    give_quiet(os, regs, |unit| {
        unit.dyadic(FpaDyadicOp::Pow, Precision::Double, Rounding::Nearest, x, y)
    })
}

/// `fmod(x, y)`, exactly: `x` itself where its exponent field is below a
/// finite `y`'s, and otherwise the FPA's remainder, whose quotient is rounded
/// to nearest, moved by `y` where its sign is not `x`'s — a sum whose exact
/// value is the answer, so it is not rounded. The remainder is worked as the
/// client's FPA works it, so a subnormal one takes the client's underflow
/// trap, as RISC OS 5.30's does; being exact, it flags nothing.
pub(super) fn fmod(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (x, y) = (first(regs), second(regs));
    if y.is_finite() && biased_exponent(x.to_bits()) < biased_exponent(y.to_bits()) {
        answer(os, x);
        return Ok(());
    }
    give_visibly(os, regs, |unit| truncated_remainder(unit, x, y))
}

/// [`fmod`]'s work on `unit`.
pub(crate) fn truncated_remainder(unit: &mut Unit, x: f64, y: f64) -> Result<f64, Exception> {
    let remainder = unit.dyadic(FpaDyadicOp::Rmf, Precision::Double, Rounding::Nearest, x, y)?;
    if remainder != 0.0 && remainder.is_sign_negative() != x.is_sign_negative() {
        unit.add(remainder, y.abs().copysign(x))
    } else {
        Ok(remainder)
    }
}

/// Cody and Waite's double-precision coefficients: `sinh`'s P and Q, then
/// `tanh`'s, each from its constant term up, Q's leading one left out.
const SINH_P: [f64; 4] = [
    -351_812.834_301_771_2,
    -11_563.521_196_851_769,
    -163.757_982_026_307_5,
    -0.789_661_274_173_571,
];
const SINH_Q: [f64; 3] = [
    -2_110_877.005_810_627_2,
    36_162.723_109_421_84,
    -277.735_231_196_507,
];
const TANH_P: [f64; 3] = [
    -1_613.411_902_399_622_7,
    -99.225_929_672_236_08,
    -0.964_374_927_772_254_8,
];
const TANH_Q: [f64; 3] = [
    4_840.235_707_198_869,
    2_233.772_071_896_231_4,
    112.744_743_805_349_49,
];
/// Where `tanh`'s rational form gives way: ln 3 / 2.
const TANH_RATIONAL_MOST: f64 = 0.549_306_144_334_054_9;
/// Cody and Waite's ln v, short enough that `y − ln v` is exact, and v/2 − 1.
const LN_V: f64 = 0.693_161_010_742_187_5;
const HALF_V_LESS_ONE: f64 = 1.383_027_787_960_190_2e-5;

/// `y + y·R` for `R = g·P(g) / Q(g)` and `g = y²`.
fn rational(unit: &mut Unit, y: f64, p: &[f64], q: &[f64]) -> Result<f64, Exception> {
    let g = unit.mul(y, y)?;
    let mut numerator = 0.0;
    for (n, &c) in p.iter().rev().enumerate() {
        numerator = if n == 0 {
            c
        } else {
            let product = unit.mul(numerator, g)?;
            unit.add(product, c)?
        };
    }
    let mut denominator = 1.0;
    for (n, &c) in q.iter().rev().enumerate() {
        let product = if n == 0 { g } else { unit.mul(denominator, g)? };
        denominator = unit.add(product, c)?;
    }
    let numerator = unit.mul(numerator, g)?;
    let ratio = unit.div(numerator, denominator)?;
    let correction = unit.mul(ratio, y)?;
    unit.add(y, correction)
}

/// Where `sinh` and `tanh` answer their argument as it stands, raising
/// nothing, as RISC OS 5.30's do.
pub(crate) const LINEAR_MOST: f64 = 1e-10;
/// Below which `cosh` answers one, raising nothing: 2⁻⁵³.
const COSH_ONE_BELOW: f64 = 1.110_223_024_625_156_5e-16;
/// Beyond which `tanh` answers one with the argument's sign.
const TANH_ONE_BEYOND: f64 = 27.0;
/// From where `cosh` and `sinh` are `e^y/2` alone, `e^−y/2` below their
/// last place.
pub(super) const LARGE_ARGUMENT: f64 = 19.0;

/// `e^y / 2` for `y` over one: `exp(y − ln v) · v/2`. Where that would
/// overflow, the overflow is flagged before the exponential is taken, so
/// only the reduction's rounding is flagged with it.
fn half_exponential(unit: &mut Unit, y: f64) -> Result<f64, Exception> {
    let reduced = unit.sub(y, LN_V)?;
    let finish = |unit: &mut Unit| {
        let z = unit.double_op(FpaMonadicOp::Exp, reduced)?;
        let part = unit.mul(HALF_V_LESS_ONE, z)?;
        unit.add(z, part)
    };
    if reduced > EXPONENTIAL_FITS && quietly(finish).is_infinite() {
        unit.set_fpsr(unit.fpsr() | Exception::Overflow.flag());
        return Ok(f64::INFINITY);
    }
    finish(unit)
}

/// Up to which `e^y · v/2` cannot overflow a double: below ln of the
/// largest, less what `v/2` adds.
const EXPONENTIAL_FITS: f64 = 700.0;

/// `cosh(x)`'s work: `e^y/2 + e^−y/2` for `y = |x|`.
pub(crate) fn cosh_of(unit: &mut Unit, x: f64) -> Result<f64, Exception> {
    let y = x.abs();
    if y.is_infinite() {
        return Ok(y);
    }
    if y < COSH_ONE_BELOW {
        return Ok(1.0);
    }
    if y <= 1.0 {
        let z = unit.double_op(FpaMonadicOp::Exp, y)?;
        let half = unit.mul(z, 0.5)?;
        let reciprocal = unit.div(0.5, z)?;
        return unit.add(half, reciprocal);
    }
    let half = half_exponential(unit, y)?;
    if half.is_infinite() || y >= LARGE_ARGUMENT {
        return Ok(half);
    }
    let reciprocal = unit.div(0.25, half)?;
    unit.add(half, reciprocal)
}

/// `sinh(x)`'s work: the rational form to one, `e^y/2 − e^−y/2` beyond.
pub(crate) fn sinh_of(unit: &mut Unit, x: f64) -> Result<f64, Exception> {
    let y = x.abs();
    if y.is_infinite() || y <= LINEAR_MOST {
        return Ok(x);
    }
    let magnitude = if y <= 1.0 {
        rational(unit, y, &SINH_P, &SINH_Q)?
    } else {
        let half = half_exponential(unit, y)?;
        if half.is_infinite() || y >= LARGE_ARGUMENT {
            return Ok(half.copysign(x));
        }
        let reciprocal = unit.div(0.25, half)?;
        unit.sub(half, reciprocal)?
    };
    Ok(magnitude.copysign(x))
}

/// `tanh(x)`'s work: the rational form to ln 3 / 2, `1 − 2/(e^2y + 1)`
/// beyond.
pub(crate) fn tanh_of(unit: &mut Unit, x: f64) -> Result<f64, Exception> {
    let y = x.abs();
    if y <= LINEAR_MOST {
        return Ok(x);
    }
    if y > TANH_ONE_BEYOND {
        return Ok(1.0_f64.copysign(x));
    }
    let magnitude = if y <= TANH_RATIONAL_MOST {
        rational(unit, y, &TANH_P, &TANH_Q)?
    } else {
        let doubled = unit.mul(y, 2.0)?;
        let z = unit.double_op(FpaMonadicOp::Exp, doubled)?;
        let denominator = unit.add(z, 1.0)?;
        let reciprocal = unit.div(1.0, denominator)?;
        let half = unit.sub(0.5, reciprocal)?;
        unit.add(half, half)?
    };
    Ok(magnitude.copysign(x))
}

/// `cosh(x)`.
pub(super) fn cosh(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let x = first(regs);
    give_visibly(os, regs, |unit| cosh_of(unit, x))
}

/// `sinh(x)`.
pub(super) fn sinh(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let x = first(regs);
    give_visibly(os, regs, |unit| sinh_of(unit, x))
}

/// `tanh(x)`.
pub(super) fn tanh(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let x = first(regs);
    give_visibly(os, regs, |unit| tanh_of(unit, x))
}

/// `x` rounded to a whole number as `rounding` says.
fn rounded(unit: &mut Unit, x: f64, rounding: Rounding) -> Result<f64, Exception> {
    unit.monadic(FpaMonadicOp::Rnd, Precision::Double, rounding, x)
}

/// `ceil(x)`.
pub(super) fn ceil(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let x = first(regs);
    give_visibly(os, regs, |unit| rounded(unit, x, Rounding::PlusInfinity))
}

/// `floor(x)`.
pub(super) fn floor(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let x = first(regs);
    give_visibly(os, regs, |unit| rounded(unit, x, Rounding::MinusInfinity))
}

/// `fabs(x)`.
#[expect(clippy::unnecessary_wraps, reason = "an entry's signature")]
pub(super) fn fabs(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let x = first(regs);
    answer(os, quietly(|unit| unit.double_op(FpaMonadicOp::Abs, x)));
    Ok(())
}

/// A double's exponent bits, all set as an infinity's are.
const EXPONENT: u64 = f64::INFINITY.to_bits();
/// The biased exponent of a fraction in [½, 1), in place: a half's own.
const HALF_EXPONENT: u64 = 0.5f64.to_bits();

/// `x` as a fraction in [½, 1) times a power of two, exactly; nought, an
/// infinity or no number as it stands, with the power RISC OS 5.30 gives it:
/// nought's nought, an infinity's `INT_MAX` and no number's `INT_MIN`.
pub(crate) fn fraction_and_power(x: f64) -> (f64, i32) {
    if x.is_nan() {
        return (x, i32::MIN);
    }
    if x.is_infinite() {
        return (x, i32::MAX);
    }
    if x == 0.0 {
        return (x, 0);
    }
    let bits = x.to_bits();
    let biased = biased_exponent(bits);
    let (fraction, power) = if biased == 0 {
        // A subnormal's leading one brought up to the implicit bit's place.
        let shift = (bits & FRACTION).leading_zeros() - 11;
        ((bits << shift) & FRACTION, -1021 - shift.cast_signed())
    } else {
        (bits & FRACTION, biased - 1022)
    };
    let sign = bits & !(FRACTION | EXPONENT);
    (f64::from_bits(sign | HALF_EXPONENT | fraction), power)
}

/// `frexp(value, exp)`: the fraction in [½, 1) answered, the power of two
/// stored at `exp`.
pub(super) fn frexp(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (fraction, power) = fraction_and_power(first(regs));
    os.as_caller()
        .write_word(regs.r[2], power.cast_unsigned())?;
    answer(os, fraction);
    Ok(())
}

/// Two to `power`, a normal double's.
fn two_to(power: i32) -> f64 {
    f64::from_bits(u64::from((power + 1023).cast_unsigned()) << 52)
}

/// `x` times two to `power`, rounded once: a step up is exact or overflows,
/// and one down by 2^−969 is exact or leaves the last step's answer so far
/// below the least subnormal that the two roundings agree.
pub(crate) fn scaled(unit: &mut Unit, x: f64, power: i32) -> Result<f64, Exception> {
    let (mut value, mut left) = (x, power);
    for _ in 0..2 {
        if left > 1023 {
            value = unit.mul(value, two_to(1023))?;
            left -= 1023;
        } else if left < -1022 {
            value = unit.mul(value, two_to(-969))?;
            left += 969;
        }
    }
    unit.mul(value, two_to(left.clamp(-1022, 1023)))
}

/// `ldexp(x, exp)`.
pub(super) fn ldexp(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (x, power) = (first(regs), regs.r[2].cast_signed());
    give_settled_then_raised(os, regs, |unit| scaled(unit, x, power))
}

/// `modf(value, iptr)`: the whole part stored at `iptr`, the value less it
/// answered — so a whole negative value's fraction is a positive nought — and
/// a nought's, an infinity's or no number's fraction a nought with its sign.
pub(super) fn modf(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let x = first(regs);
    let whole = quietly(|unit| rounded(unit, x, Rounding::Zero));
    let fraction = if x.is_finite() && x != 0.0 {
        quietly(|unit| unit.sub(x, whole))
    } else {
        0.0_f64.copysign(x)
    };
    os.as_caller()
        .write_words(regs.r[2], &fpa_words(whole.to_bits()))?;
    answer(os, fraction);
    Ok(())
}

#[cfg(test)]
mod tests;
