//! The C99 real functions that are exact or a single FPA instruction, as
//! RISC OS 5.30's library computes them: classification and sign, copying
//! a sign, the next value, the positive difference, the larger and smaller,
//! the hypotenuse, every rounding, the remainders, a number's parts and its
//! scaling by a power of two, and the fused multiply-add.
//!
//! A double argument comes in a register pair, its high word first; a float
//! one as a double; a float answer is rounded to a float in F0. Everything
//! is worked on an FPA working as the client's, so its flags are the
//! client's and an exception the client traps is raised as its trap.

use core::cmp::Ordering;

use tairix_riscos_arm::fpa::{Exception, Precision, Rounding};
use tairix_riscos_arm::instruction::{FpaDyadicOp, FpaMonadicOp};
use tairix_riscos_kernel::os::{Fail, Os, Regs};
use tairix_softfloat::from_fpa_words;

use super::{exact, give_double, give_long, give_single, give_word, precision};
use crate::c::fpa::{
    answer, answer_as_client, as_client, biased_exponent, double, single_bits, whole_part, Unit,
    FRACTION,
};
use crate::c::math::{fraction_and_power, scaled, truncated_remainder};
use crate::c99low::{saturated, truncated};
use crate::client::stacked;

/// `fpclassify`'s answers.
const FP_ZERO: u32 = 0;
const FP_SUBNORMAL: u32 = 1;
const FP_NORMAL: u32 = 2;
const FP_INFINITE: u32 = 3;
const FP_NAN: u32 = 4;

/// What `ilogb` answers for nought, an infinity and no number.
const FP_ILOGB0: i32 = -i32::MAX;
const ILOGB_INFINITE: i32 = i32::MAX;
const FP_ILOGBNAN: i32 = i32::MIN;

/// The double R0 and R1 hold.
fn x_of(regs: &Regs) -> f64 {
    double(regs, 0)
}

/// The doubles R0 to R3 hold.
fn xy_of(regs: &Regs) -> (f64, f64) {
    (double(regs, 0), double(regs, 2))
}

/// `x`'s class as `fpclassify` names it, `x` taken as a float where
/// `float`.
fn class(x: f64, float: bool) -> u32 {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "a float argument's own value"
    )]
    let subnormal = if float {
        (x as f32).is_subnormal()
    } else {
        x.is_subnormal()
    };
    match () {
        () if x.is_nan() => FP_NAN,
        () if x.is_infinite() => FP_INFINITE,
        () if x == 0.0 => FP_ZERO,
        () if subnormal => FP_SUBNORMAL,
        () => FP_NORMAL,
    }
}

/// `__fpclassifyf(x)` and `__fpclassifyd`.
#[expect(clippy::unnecessary_wraps, reason = "an entry's signature")]
pub(super) fn fpclassify<const FLOAT: bool>(_os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    regs.r[0] = class(x_of(regs), FLOAT);
    Ok(())
}

/// `__signbitf(x)` and `__signbitd`.
#[expect(clippy::unnecessary_wraps, reason = "an entry's signature")]
pub(super) fn signbit(_os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    regs.r[0] = u32::from(x_of(regs).is_sign_negative());
    Ok(())
}

/// `copysign(x, y)` and `copysignf`, the arguments in R0 to R3.
pub(super) fn copysign<const FLOAT: bool>(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (x, y) = xy_of(regs);
    give::<FLOAT>(os, |_| Ok(x.copysign(y)))
}

/// `copysign(x, y)` and `copysignf` as chunk 5 first gives them, the
/// arguments in F0 and F1.
pub(super) fn copysign_in_registers<const FLOAT: bool>(
    os: &mut Os<'_>,
    _regs: &mut Regs,
) -> Result<(), Fail> {
    let operands = os
        .fpa_double(0)
        .and_then(|x| os.fpa_double(1).map(|y| (x, y)));
    match operands {
        Ok((x, y)) => give::<FLOAT>(os, |_| Ok(f64::from_bits(x).copysign(f64::from_bits(y)))),
        Err(exception) => Ok(os.floating_point(exception)?),
    }
}

/// `work`'s answer given as a float where `FLOAT`, else as a double.
fn give<const FLOAT: bool>(
    os: &mut Os<'_>,
    work: impl FnOnce(&mut Unit) -> Result<f64, Exception>,
) -> Result<(), Fail> {
    if FLOAT {
        give_single(os, work)
    } else {
        give_double(os, work)
    }
}

/// The least subnormal double.
const LEAST: f64 = 5e-324;

/// The double after `x` toward `y`, neither a NaN; none where they are
/// equal, which C99 answers `y` for. Neither step passes an infinity.
#[expect(clippy::float_cmp, reason = "C99's own test: equal arguments answer y")]
fn next_double(x: f64, y: f64) -> Option<f64> {
    if x == y {
        return None;
    }
    let bits = x.to_bits();
    Some(if x == 0.0 {
        LEAST.copysign(y)
    } else if (y > x) == (x > 0.0) {
        f64::from_bits(bits + 1)
    } else {
        f64::from_bits(bits - 1)
    })
}

/// As [`next_double`], for the float the argument `x` narrows to: a double
/// no float holds is taken as the float it rounds to, as a float argument
/// would have been passed.
#[expect(clippy::float_cmp, reason = "C99's own test: equal arguments answer y")]
#[expect(
    clippy::cast_possible_truncation,
    reason = "the float argument's own value"
)]
fn next_single(x: f64, y: f64) -> Option<f64> {
    let x = x as f32;
    let wide = f64::from(x);
    if wide == y {
        return None;
    }
    let bits = x.to_bits();
    let next = if x == 0.0 {
        let least = f32::from_bits(1);
        if y < 0.0 {
            -least
        } else {
            least
        }
    } else if (y > wide) == (x > 0.0) {
        f32::from_bits(bits + 1)
    } else {
        f32::from_bits(bits - 1)
    };
    Some(f64::from(next))
}

/// The value after `x` toward `y` in the format a float or a double has:
/// an underflow where it is no normal number, an overflow where it is
/// infinite, as an operation raises them.
fn next_toward(unit: &mut Unit, x: f64, y: f64, float: bool) -> Result<f64, Exception> {
    if x.is_nan() || y.is_nan() {
        return unit.add(x, y);
    }
    let next = if float {
        next_single(x, y)
    } else {
        next_double(x, y)
    };
    let Some(next) = next else {
        return Ok(y);
    };
    let tiny = if float {
        #[expect(clippy::cast_possible_truncation, reason = "the value a float has")]
        !(next as f32).is_normal()
    } else {
        !next.is_normal()
    };
    if next.is_infinite() {
        unit.raise(Exception::Overflow)?;
    } else if tiny {
        unit.raise(Exception::Underflow)?;
    }
    Ok(next)
}

/// `nextafter(x, y)`, `nextafterf`, `nexttoward` and `nexttowardf`: `y` a
/// double each time.
pub(super) fn nextafter<const FLOAT: bool>(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (x, y) = xy_of(regs);
    give::<FLOAT>(os, |unit| next_toward(unit, x, y, FLOAT))
}

/// `fdim(x, y)` and `fdimf`: `x − y` where `x` is the greater, else nought.
pub(super) fn fdim<const FLOAT: bool>(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (x, y) = xy_of(regs);
    give::<FLOAT>(os, |unit| {
        if x.is_nan() || y.is_nan() || x > y {
            unit.sub(x, y)
        } else {
            Ok(0.0)
        }
    })
}

/// `fmax(x, y)`, `fmaxf`, `fmin` and `fminf`: a number before no number,
/// and minus nought below nought.
pub(super) fn extreme<const FLOAT: bool, const LARGER: bool>(
    os: &mut Os<'_>,
    regs: &mut Regs,
) -> Result<(), Fail> {
    let (x, y) = xy_of(regs);
    let x_first = match (x.is_nan(), y.is_nan()) {
        (true, _) => false,
        (false, true) => true,
        (false, false) => (x.total_cmp(&y) == Ordering::Greater) == LARGER,
    };
    let chosen = if x_first { x } else { y };
    give::<FLOAT>(os, |_| Ok(chosen))
}

/// `fabsf(x)`.
pub(super) fn fabsf(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let x = x_of(regs);
    give_single(os, |unit| {
        unit.monadic(FpaMonadicOp::Abs, Precision::Single, Rounding::Nearest, x)
    })
}

/// `hypot(x, y)` and `hypotf`: `√(x² + y²)` worked in extended precision,
/// whose range holds the squares, then rounded; an infinity before no
/// number.
pub(super) fn hypot<const FLOAT: bool>(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (x, y) = xy_of(regs);
    give::<FLOAT>(os, |unit| {
        if x.is_infinite() || y.is_infinite() {
            return Ok(f64::INFINITY);
        }
        unit.load(1, x);
        unit.load(2, y);
        let extended = |unit: &mut Unit, op, operands| {
            unit.dyadic_in(op, Precision::Extended, Rounding::Nearest, operands)
        };
        extended(unit, FpaDyadicOp::Muf, [3, 1, 1])?;
        extended(unit, FpaDyadicOp::Muf, [4, 2, 2])?;
        extended(unit, FpaDyadicOp::Adf, [3, 3, 4])?;
        unit.monadic_in(
            FpaMonadicOp::Sqt,
            Precision::Extended,
            Rounding::Nearest,
            0,
            3,
        )?;
        unit.take(0)
    })
}

/// `x` rounded to a whole number as `rounding` says, in `precision`.
fn rounded(
    unit: &mut Unit,
    x: f64,
    precision: Precision,
    rounding: Rounding,
) -> Result<f64, Exception> {
    unit.monadic(FpaMonadicOp::Rnd, precision, rounding, x)
}

/// `ceilf(x)`, `floorf`, `rint`, `rintf`, `trunc` and `truncf`: the FPA's
/// rounding, inexact where it is.
pub(super) fn round_to<const FLOAT: bool, const ROUNDING: u8>(
    os: &mut Os<'_>,
    regs: &mut Regs,
) -> Result<(), Fail> {
    let x = x_of(regs);
    give::<FLOAT>(os, |unit| {
        rounded(unit, x, precision(FLOAT), rounding_of(ROUNDING))
    })
}

/// The roundings, as a const generic names them.
pub(super) const NEAREST: u8 = 0;
pub(super) const UP: u8 = 1;
pub(super) const DOWN: u8 = 2;
pub(super) const ZERO: u8 = 3;

const fn rounding_of(rounding: u8) -> Rounding {
    match rounding {
        UP => Rounding::PlusInfinity,
        DOWN => Rounding::MinusInfinity,
        ZERO => Rounding::Zero,
        _ => Rounding::Nearest,
    }
}

/// `nearbyint(x)` and `nearbyintf`: `rint`, its flags put back.
pub(super) fn nearbyint<const FLOAT: bool>(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let x = x_of(regs);
    give::<FLOAT>(os, |unit| {
        let fpsr = unit.fpsr();
        let value = rounded(unit, x, precision(FLOAT), Rounding::Nearest)?;
        unit.set_fpsr(fpsr);
        Ok(value)
    })
}

/// `x` rounded to the nearest whole number, halfway away from nought,
/// exactly and raising nothing.
fn half_away(x: f64) -> f64 {
    let whole = whole_part(x);
    if !x.is_finite() || (x - whole).abs() < 0.5 {
        whole
    } else {
        whole + 1.0_f64.copysign(x)
    }
}

/// `round(x)` and `roundf`.
pub(super) fn round(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let x = x_of(regs);
    give_double(os, |_| Ok(half_away(x)))
}

/// `lrint(x)` and `lrintf`: the FPA's `FIX`, to nearest.
pub(super) fn lrint(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let x = x_of(regs);
    give_word(os, regs, |unit| unit.fix(Rounding::Nearest, x))
}

/// `lround(x)` and `lroundf`: inexact where `x` is not whole, as a
/// truncation finds, then rounded halfway away from nought and `FIX`ed, so
/// the only further exception is a value with no place in a word.
pub(super) fn lround(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let x = x_of(regs);
    give_word(os, regs, |unit| {
        rounded(unit, x, Precision::Double, Rounding::Zero)?;
        unit.fix(Rounding::Zero, half_away(x))
    })
}

/// `whole` as a 64-bit integer, as chunk 4 converts it: with no place in
/// the type the invalid operation, its flag set before its trap, else the
/// type's limit on its side.
fn converted(unit: &mut Unit, whole: f64) -> Result<i64, Exception> {
    if let Some((value, _)) = truncated::<true>(whole) {
        return Ok(value.cast_signed());
    }
    unit.flag_then_raise(Exception::InvalidOperation)?;
    Ok(saturated::<true>(whole).cast_signed())
}

/// `llrint(x)` and `llrintf`: `rint`, then converted.
pub(super) fn llrint(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let x = x_of(regs);
    give_long(os, regs, |unit| {
        let whole = rounded(unit, x, Precision::Double, Rounding::Nearest)?;
        converted(unit, whole)
    })
}

/// `llround(x)` and `llroundf`: inexact where `x` is not whole, then
/// rounded halfway away from nought and converted.
pub(super) fn llround(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let x = x_of(regs);
    give_long(os, regs, |unit| {
        rounded(unit, x, Precision::Double, Rounding::Zero)?;
        converted(unit, half_away(x))
    })
}

/// `remainder(x, y)` and `remainderf`: the FPA's remainder, its quotient
/// rounded to nearest.
pub(super) fn remainder<const FLOAT: bool>(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (x, y) = xy_of(regs);
    give::<FLOAT>(os, |unit| {
        unit.dyadic(FpaDyadicOp::Rmf, precision(FLOAT), Rounding::Nearest, x, y)
    })
}

/// `remquo(x, y, quo)` and `remquof`: the remainder answered, and the low
/// bits of its quotient with the quotient's sign stored at `quo`, which is
/// on the stack.
pub(super) fn remquo<const FLOAT: bool>(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (x, y) = xy_of(regs);
    let [quo] = stacked(os)?;
    if let Some(remainder) = as_client(os, |unit| {
        unit.dyadic(FpaDyadicOp::Rmf, precision(FLOAT), Rounding::Nearest, x, y)
    })? {
        os.as_caller().write_word(quo, exact::quotient_bits(x, y))?;
        answer(os, remainder);
    }
    Ok(())
}

/// `fmodf(x, y)`: `fmod`'s exact remainder of the floats.
pub(super) fn fmodf(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (x, y) = xy_of(regs);
    give_single(os, |unit| truncated_remainder(unit, x, y))
}

/// `frexpf(value, exp)`: the fraction answered, the power stored at `exp`.
pub(super) fn frexpf(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (fraction, power) = fraction_and_power(x_of(regs));
    os.as_caller()
        .write_word(regs.r[2], power.cast_unsigned())?;
    give_single(os, |_| Ok(fraction))
}

/// `modff(value, iptr)`: the whole part stored at `iptr` as a float, the
/// rest answered — nought keeping its sign, an infinity's and no number's
/// rest a nought with their sign.
pub(super) fn modff(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let x = x_of(regs);
    let (whole, fraction) = match () {
        () if x == 0.0 => (x, x),
        () if !x.is_finite() => (x, 0.0_f64.copysign(x)),
        () => {
            let whole = whole_part(x);
            (whole, x - whole)
        }
    };
    os.as_caller().write_word(regs.r[2], single_bits(whole))?;
    give_single(os, |_| Ok(fraction))
}

/// `ilogb(x)` and `ilogbf`: the power of two `x` lies at.
#[expect(clippy::unnecessary_wraps, reason = "an entry's signature")]
pub(super) fn ilogb(_os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let x = x_of(regs);
    let power = match () {
        () if x.is_nan() => FP_ILOGBNAN,
        () if x.is_infinite() => ILOGB_INFINITE,
        () if x == 0.0 => FP_ILOGB0,
        () => fraction_and_power(x).1 - 1,
    };
    regs.r[0] = power.cast_unsigned();
    Ok(())
}

/// `logb(x)` and `logbf`: that power as a number, an infinity's an infinity
/// and nought's the division by zero of `log`.
pub(super) fn logb<const FLOAT: bool>(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let x = x_of(regs);
    give::<FLOAT>(os, |unit| match () {
        () if x.is_nan() => Ok(x),
        () if x.is_infinite() => Ok(f64::INFINITY),
        () if x == 0.0 => unit.div(-1.0, 0.0),
        () => Ok(f64::from(fraction_and_power(x).1 - 1)),
    })
}

/// `scalbn(x, n)`, `scalbnf`, `scalbln`, `scalblnf` and `ldexpf`: `x` times
/// two to `n`, rounded once.
pub(super) fn scalbn<const FLOAT: bool>(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (x, power) = (x_of(regs), regs.r[2].cast_signed());
    give::<FLOAT>(os, |unit| {
        if power_wraps(x, power) {
            return unit.mul(f64::MAX, f64::MAX.copysign(x));
        }
        scaled(unit, x, power)
    })
}

/// Whether RISC OS 5.30's `scalbn` adds `power` to a subnormal `x`'s
/// normalised exponent past the bottom of a word, wrapping to a power that
/// overflows.
fn power_wraps(x: f64, power: i32) -> bool {
    let fraction = x.to_bits() & FRACTION;
    let subnormal = biased_exponent(x.to_bits()) == 0 && fraction != 0;
    // x · 2⁵⁴'s biased exponent, less 54.
    let normalised = 12 - fraction.leading_zeros().cast_signed();
    subnormal && power.checked_add(normalised).is_none()
}

/// `fma(x, y, z)` and `fmaf`: `x·y + z` worked exactly, rounded to the
/// FPA's extended precision and then to the answer's, as RISC OS 5.30
/// rounds it; `z` on the stack.
pub(super) fn fma<const FLOAT: bool>(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (x, y) = xy_of(regs);
    let z = f64::from_bits(from_fpa_words(stacked(os)?));
    answer_as_client(os, |unit| exact::fused(unit, x, y, z, FLOAT))
}

#[cfg(test)]
mod tests {
    use super::{next_double, next_single, LEAST};

    #[test]
    fn a_float_steps_from_the_float_its_argument_narrows_to() {
        let least = f64::from(f32::from_bits(1));
        let most = f64::from(f32::MAX);
        for (x, y, next) in [
            (1e-50, 0.0, None),
            (1e-50, -1.0, Some(-least)),
            (-0.0, 1.0, Some(least)),
            (1e39, 2e39, Some(most)),
            (most, f64::INFINITY, Some(f64::INFINITY)),
            (-f64::INFINITY, 0.0, Some(-most)),
            (least, 0.0, Some(0.0)),
        ] {
            assert_eq!(next_single(x, y), next, "({x:e}, {y:e})");
        }
    }

    #[test]
    fn a_double_steps_by_its_last_place_toward_y() {
        assert_eq!(next_double(0.0, -1.0), Some(-LEAST));
        assert_eq!(
            next_double(1.0, 2.0),
            Some(f64::from_bits(1.0f64.to_bits() + 1))
        );
        assert_eq!(next_double(f64::INFINITY, 0.0), Some(f64::MAX));
        assert_eq!(next_double(2.0, 2.0), None);
    }
}
