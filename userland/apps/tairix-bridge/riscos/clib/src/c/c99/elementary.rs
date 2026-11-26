//! The C99 elementary functions, as RISC OS 5.30's library computes them:
//! the float ones that are single-precision FPA instructions, `exp2` and
//! `log2` over the FPA's power and logarithm — `log2f` with RISC OS 5.30's
//! misreading of an exact power of two — the cube root, the float
//! hyperbolics as the library's double ones narrowed, the inverse
//! hyperbolics, `expm1` and `log1p` in RISC OS 5.30's forms, the error
//! functions, and the gamma functions with RISC OS 5.30's products,
//! reflection and range.
//!
//! Every one is worked on an FPA working as the client's: its flags are the
//! client's and an exception the client traps is raised as its trap.

use core::f64::consts::{FRAC_PI_4, LN_2, PI};

use tairix_riscos_arm::fpa::{fpsr, Exception, Precision, Rounding};
use tairix_riscos_arm::instruction::{FpaDyadicOp, FpaMonadicOp};
use tairix_riscos_kernel::os::{Fail, Os, Regs};
use tairix_softfloat::Binary;

use super::{as_client, give_double, give_single, overflows_a_float, FRAC_3_PI_4};
use crate::c::errno::ERANGE;
use crate::c::fpa::{
    biased_exponent, double, is_whole, quietly, single_bits, Extended, Unit, BIAS, FRACTION,
};
use crate::c::math::{cosh_of, scaled, set_errno, sinh_of, tanh_of, worked_visibly, LINEAR_MOST};

/// The FPA's single-precision instruction `op` of a float.
pub(super) fn single_instruction<const OP: u8>(
    os: &mut Os<'_>,
    regs: &mut Regs,
) -> Result<(), Fail> {
    let x = double(regs, 0);
    give_single(os, |unit| {
        unit.monadic(monadic_op(OP), Precision::Single, Rounding::Nearest, x)
    })
}

/// The instructions [`single_instruction`] names.
pub(super) const ACS: u8 = 0;
pub(super) const ASN: u8 = 1;
pub(super) const ATN: u8 = 2;
pub(super) const COS: u8 = 3;
pub(super) const SIN: u8 = 4;
pub(super) const TAN: u8 = 5;
pub(super) const EXP: u8 = 6;
pub(super) const LGN: u8 = 7;
pub(super) const LOG: u8 = 8;
pub(super) const SQT: u8 = 9;

const fn monadic_op(op: u8) -> FpaMonadicOp {
    match op {
        ACS => FpaMonadicOp::Acs,
        ASN => FpaMonadicOp::Asn,
        ATN => FpaMonadicOp::Atn,
        COS => FpaMonadicOp::Cos,
        SIN => FpaMonadicOp::Sin,
        TAN => FpaMonadicOp::Tan,
        EXP => FpaMonadicOp::Exp,
        LGN => FpaMonadicOp::Lgn,
        LOG => FpaMonadicOp::Log,
        _ => FpaMonadicOp::Sqt,
    }
}

/// `powf(x, y)`: the FPA's single-precision power.
pub(super) fn powf(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (x, y) = (double(regs, 0), double(regs, 2));
    give_single(os, |unit| {
        unit.dyadic(FpaDyadicOp::Pow, Precision::Single, Rounding::Nearest, x, y)
    })
}

/// `atan2f(y, x)`: the FPA's single-precision polar angle, but the pairs of
/// noughts and of infinities C gives angles to taken first.
pub(super) fn atan2f(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (y, x) = (double(regs, 0), double(regs, 2));
    give_single(os, |unit| match () {
        () if y == 0.0 && x == 0.0 => Ok(if x.is_sign_negative() { PI } else { 0.0 }.copysign(y)),
        () if y.is_infinite() && x.is_infinite() => {
            let angle = if x < 0.0 { FRAC_3_PI_4 } else { FRAC_PI_4 };
            unit.single(angle).map(|angle| angle.copysign(y))
        }
        () => unit.dyadic(FpaDyadicOp::Pol, Precision::Single, Rounding::Nearest, x, y),
    })
}

/// The invalid operation's answer, as `(x − x)/(x − x)` raises it.
fn invalid(unit: &mut Unit) -> Result<f64, Exception> {
    unit.div(0.0, 0.0)
}

/// Whether `x` is a power of two.
fn is_power_of_two(x: f64) -> bool {
    x.is_normal() && x.to_bits() & FRACTION == 0
}

/// `2^x` as RISC OS 5.30 works it: `x` converted to a word to nearest,
/// quietly, and where that is exact, two to that power built exactly;
/// anything else the FPA's power, flagged inexact first unless `x` is a
/// power of two.
fn two_to(unit: &mut Unit, x: f64) -> Result<f64, Exception> {
    let mut own = Unit::own();
    if let Ok(power) = own.fix(Rounding::Nearest, x) {
        if own.fpsr() & fpsr::FLAGS == 0 {
            return scaled(unit, 1.0, power);
        }
    }
    if !is_power_of_two(x) {
        unit.raise(Exception::Inexact)?;
    }
    unit.dyadic(
        FpaDyadicOp::Pow,
        Precision::Double,
        Rounding::Nearest,
        2.0,
        x,
    )
}

/// `exp2(x)`.
pub(super) fn exp2(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let x = double(regs, 0);
    give_double(os, |unit| two_to(unit, x))
}

/// `exp2f(x)`: `exp2`, narrowed.
pub(super) fn exp2f(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let x = double(regs, 0);
    give_single(os, |unit| two_to(unit, x))
}

/// `log x / log 2` worked in extended precision, `x`'s logarithm first so
/// that its domain is checked before anything is rounded.
fn logarithm_base_two(unit: &mut Unit, x: f64) -> Result<f64, Exception> {
    unit.load(1, x);
    unit.load(2, 2.0);
    let extended = Precision::Extended;
    unit.monadic_in(FpaMonadicOp::Lgn, extended, Rounding::Nearest, 3, 1)?;
    unit.monadic_in(FpaMonadicOp::Lgn, extended, Rounding::Nearest, 4, 2)?;
    unit.dyadic_in(FpaDyadicOp::Dvf, extended, Rounding::Nearest, [0, 3, 4])?;
    unit.take(0)
}

/// `log2(x)`: an exact power of two's power as it stands, an infinity an
/// infinity.
pub(super) fn log2(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let x = double(regs, 0);
    give_double(os, |unit| {
        let bits = x.to_bits();
        let biased = biased_exponent(bits);
        if x > 0.0 && bits & FRACTION == 0 && biased != 0 {
            return Ok(if x.is_infinite() {
                x
            } else {
                f64::from(biased - BIAS)
            });
        }
        logarithm_base_two(unit, x)
    })
}

/// `log2f(x)`: for an exact power of two, an infinity among them, the
/// float's exponent field less a double's bias — the misreading RISC OS 5.30
/// makes, `log2f(1)` minus 896 — else `log2`, narrowed.
pub(super) fn log2f(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let x = double(regs, 0);
    give_single(os, |unit| {
        let bits = u64::from(single_bits(x));
        let fraction_bits = Binary::SINGLE.fraction_bits();
        let field = bits >> fraction_bits & Binary::SINGLE.exp_mask();
        if x > 0.0 && bits & ((1 << fraction_bits) - 1) == 0 && field != 0 {
            return Ok(f64::from(i32::try_from(field).unwrap_or(0) - BIAS));
        }
        logarithm_base_two(unit, x)
    })
}

/// `∛x`: noughts and no number as they stand; else the FPA's power to an
/// extended third, refined by a Newton step in extended precision, then
/// rounded — the third's own rounding inexact for every other argument.
fn cube_root(unit: &mut Unit, x: f64) -> Result<f64, Exception> {
    if x.is_nan() || x == 0.0 {
        return Ok(x);
    }
    let extended = Precision::Extended;
    let near = Rounding::Nearest;
    unit.load(1, 1.0);
    unit.load(2, 3.0);
    unit.load(5, x.abs());
    unit.dyadic_in(FpaDyadicOp::Dvf, extended, near, [3, 1, 2])?;
    unit.dyadic_in(FpaDyadicOp::Pow, extended, near, [4, 5, 3])?;
    if x.is_finite() {
        // r − (r³ − x) / 3r²
        unit.dyadic_in(FpaDyadicOp::Muf, extended, near, [6, 4, 4])?;
        unit.dyadic_in(FpaDyadicOp::Muf, extended, near, [7, 6, 4])?;
        unit.dyadic_in(FpaDyadicOp::Suf, extended, near, [7, 7, 5])?;
        unit.dyadic_in(FpaDyadicOp::Muf, extended, near, [6, 6, 2])?;
        unit.dyadic_in(FpaDyadicOp::Dvf, extended, near, [7, 7, 6])?;
        unit.dyadic_in(FpaDyadicOp::Suf, extended, near, [4, 4, 7])?;
    }
    unit.monadic_in(FpaMonadicOp::Mvf, Precision::Double, near, 0, 4)?;
    unit.take(0).map(|root| root.copysign(x))
}

/// `cbrt(x)`.
pub(super) fn cbrt(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let x = double(regs, 0);
    give_double(os, |unit| cube_root(unit, x))
}

/// `cbrtf(x)`: `cbrt`, narrowed.
pub(super) fn cbrtf(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let x = double(regs, 0);
    give_single(os, |unit| cube_root(unit, x))
}

/// Which hyperbolic function a float one narrows.
pub(super) const COSH: u8 = 0;
pub(super) const SINH: u8 = 1;
pub(super) const TANH: u8 = 2;

/// `coshf(x)`, `sinhf` and `tanhf`: the library's double function, its
/// flags and range error the client's, narrowed — but an answer beyond a
/// float's range overflowing with the flags the client had before, as RISC
/// OS 5.30's do.
pub(super) fn hyperbolic_float<const WHICH: u8>(
    os: &mut Os<'_>,
    regs: &mut Regs,
) -> Result<(), Fail> {
    let x = double(regs, 0);
    let work: fn(&mut Unit, f64) -> Result<f64, Exception> = match WHICH {
        COSH => cosh_of,
        SINH => sinh_of,
        _ => tanh_of,
    };
    let entry = os.fpsr();
    let Some(value) = worked_visibly(os, regs, |unit| work(unit, x))? else {
        return Ok(());
    };
    if overflows_a_float(value) {
        os.set_fpsr(entry);
    }
    give_double(os, |unit| unit.single(value))
}

/// Which inverse hyperbolic, or near-unity exponential or logarithm, a
/// function is.
pub(super) const ACOSH: u8 = 0;
pub(super) const ASINH: u8 = 1;
pub(super) const ATANH: u8 = 2;
pub(super) const EXPM1: u8 = 3;
pub(super) const LOG1P: u8 = 4;

/// Beyond which `acosh` and `asinh` answer `ln x + ln 2`: 2⁶⁴.
const LOGARITHMIC_BEYOND: f64 = 18_446_744_073_709_551_616.0;

/// `ln x + ln 2`, each rounded to a double.
fn logarithm_doubled(unit: &mut Unit, x: f64) -> Result<f64, Exception> {
    let logarithm = unit.double_op(FpaMonadicOp::Lgn, x)?;
    unit.add(logarithm, LN_2)
}

/// `log1p(x)`: `ln(1 + x)` in extended precision, at and below −½ as it
/// stands and above with Kahan's correction `ln u · x / (u − 1)` for `u = 1 +
/// x`, which is `x` itself where `u` is one.
fn log1p_of(e: &mut Extended<'_>, x: f64) -> Result<f64, Exception> {
    if x == 0.0 || x.is_nan() || x == f64::INFINITY {
        return Ok(x);
    }
    e.constant(1, x);
    e.constant(2, 1.0);
    e.op(FpaDyadicOp::Adf, 3, 2, 1)?;
    e.mono(FpaMonadicOp::Lgn, 4, 3)?;
    if x <= -0.5 {
        return e.answer(4);
    }
    e.op(FpaDyadicOp::Suf, 5, 3, 2)?;
    if e.0.is_zero(5) {
        return Ok(x);
    }
    e.op(FpaDyadicOp::Muf, 4, 4, 1)?;
    e.op(FpaDyadicOp::Dvf, 4, 4, 5)?;
    e.answer(4)
}

/// `acosh(x)`: `log1p(t + √(2t + t²))` for `t = x − 1` in double precision,
/// the domain trapping first and the huge `ln 2x`. RISC OS 5.30 flags
/// `acosh(1)`'s nought inexact.
fn acosh_of(e: &mut Extended<'_>, x: f64) -> Result<f64, Exception> {
    if x.is_nan() {
        return Ok(x);
    }
    if x < 1.0 {
        return invalid(e.0);
    }
    if x > LOGARITHMIC_BEYOND {
        return logarithm_doubled(e.0, x);
    }
    let unit = &mut *e.0;
    let t = unit.sub(x, 1.0)?;
    let doubled = unit.mul(t, 2.0)?;
    let square = unit.mul(t, t)?;
    let sum = unit.add(doubled, square)?;
    let root = unit.double_op(FpaMonadicOp::Sqt, sum)?;
    let v = unit.add(t, root)?;
    let answer = log1p_of(e, v)?;
    if v == 0.0 {
        e.0.raise(Exception::Inexact)?;
    }
    Ok(answer)
}

/// `asinh(x)`: `log1p(|x| + x²/(1 + √(1 + x²)))` with `x`'s sign, the square
/// rounded to a double and the rest worked in extended precision; a tiny `x`
/// as it stands and a huge one `ln 2|x|`.
fn asinh_of(e: &mut Extended<'_>, x: f64) -> Result<f64, Exception> {
    let magnitude = x.abs();
    if !x.is_finite() {
        return Ok(x);
    }
    if magnitude <= LINEAR_MOST {
        if x != 0.0 {
            e.0.raise(Exception::Inexact)?;
        }
        return Ok(x);
    }
    if magnitude > LOGARITHMIC_BEYOND {
        return logarithm_doubled(e.0, magnitude).map(|y| y.copysign(x));
    }
    let square = e.0.mul(magnitude, magnitude)?;
    e.constant(1, magnitude);
    e.constant(2, 1.0);
    e.constant(3, square);
    e.op(FpaDyadicOp::Adf, 4, 2, 3)?;
    e.mono(FpaMonadicOp::Sqt, 4, 4)?;
    e.op(FpaDyadicOp::Adf, 5, 2, 4)?;
    e.op(FpaDyadicOp::Dvf, 5, 3, 5)?;
    e.op(FpaDyadicOp::Adf, 6, 1, 5)?;
    let v = e.answer(6)?;
    log1p_of(e, v).map(|y| y.copysign(x))
}

/// `atanh(x)`: `½ log1p(2|x| / (1 − |x|))` with `x`'s sign, its quotient in
/// double precision; one a division by nought and beyond it the domain.
fn atanh_of(e: &mut Extended<'_>, x: f64) -> Result<f64, Exception> {
    let magnitude = x.abs();
    if magnitude.is_nan() {
        return Ok(x);
    }
    if magnitude >= 1.0 {
        return if magnitude > 1.0 {
            invalid(e.0)
        } else {
            e.0.div(x, 0.0)
        };
    }
    let unit = &mut *e.0;
    let doubled = unit.add(magnitude, magnitude)?;
    let rest = unit.sub(1.0, magnitude)?;
    let v = unit.div(doubled, rest)?;
    let logarithm = log1p_of(e, v)?;
    e.0.mul(logarithm, 0.5).map(|y| y.copysign(x))
}

/// Below which `expm1` answers −1, flagged inexact: −54 ln 2.
const EXPM1_LEAST: f64 = -37.429_947_750_237_04;

/// `expm1(x)`: `eˣ − 1` in extended precision, worked where it cancels as
/// Kahan works it, `(u − 1) · x / ln u` for `u = eˣ`, which is `x` itself
/// where `u` is one.
fn expm1_of(e: &mut Extended<'_>, x: f64) -> Result<f64, Exception> {
    if x == 0.0 || x.is_nan() {
        return Ok(x);
    }
    if x == f64::NEG_INFINITY {
        return Ok(-1.0);
    }
    if x < EXPM1_LEAST {
        e.0.raise(Exception::Inexact)?;
        return Ok(-1.0);
    }
    e.constant(1, x);
    e.constant(2, 1.0);
    e.mono(FpaMonadicOp::Exp, 3, 1)?;
    e.op(FpaDyadicOp::Suf, 4, 3, 2)?;
    if !x.is_finite() || x.abs() > 0.5 {
        return e.answer(4);
    }
    if e.0.is_zero(4) {
        return Ok(x);
    }
    e.mono(FpaMonadicOp::Lgn, 5, 3)?;
    e.op(FpaDyadicOp::Muf, 4, 4, 1)?;
    e.op(FpaDyadicOp::Dvf, 4, 4, 5)?;
    e.answer(4)
}

/// One of the inverse hyperbolics, `expm1` or `log1p`, as a double, or
/// narrowed to a float where `FLOAT`: `expm1f` raising its overflow before
/// it works anything, and `log1pf` answering the double unnarrowed at and
/// below −½, as RISC OS 5.30's do.
pub(super) fn near_unity<const WHICH: u8, const FLOAT: bool>(
    os: &mut Os<'_>,
    regs: &mut Regs,
) -> Result<(), Fail> {
    let x = double(regs, 0);
    let work = move |unit: &mut Unit| {
        let e = &mut Extended(unit);
        match WHICH {
            ACOSH => acosh_of(e, x),
            ASINH => asinh_of(e, x),
            ATANH => atanh_of(e, x),
            EXPM1 => expm1_of(e, x),
            _ => log1p_of(e, x),
        }
    };
    if !FLOAT || (WHICH == LOG1P && x <= -0.5) {
        return give_double(os, work);
    }
    let narrowed = |unit: &mut Unit| {
        let value = work(unit)?;
        unit.single(value)
    };
    let may_overflow = x.is_finite() && x > FLOAT_EXPONENTIAL_FITS;
    if WHICH == EXPM1 && may_overflow && quietly(narrowed).is_infinite() {
        return give_double(os, |unit| unit.single(f64::MAX));
    }
    give_single(os, work)
}

/// Up to which `e^x − 1` cannot overflow a float: below ln of the largest.
const FLOAT_EXPONENTIAL_FITS: f64 = 88.0;

/// Where `x²` underflows, and where `e^−x²` does but `x²` is still finite.
const SQUARE_UNDERFLOWS: f64 = 1.491_668_146_240_041_3e-154;
const EXPONENTIAL_UNDERFLOWS: f64 = 26.616_317_150_803_7;
/// Where `x²` overflows a double, and where a float.
const SQUARE_OVERFLOWS: f64 = 1.340_780_792_994_259_6e154;
const FLOAT_SQUARE_OVERFLOWS: f64 = 1.844_674_352_395_373e19;

/// Which error function.
pub(super) const ERF: u8 = 0;
pub(super) const ERFC: u8 = 1;

/// An error function's saturated answers, a float one's where `float`, as
/// RISC OS 5.30 gives them: where `x²` underflows, `erf`'s nought and
/// `erfc`'s one with a range error and an underflow; where `e^−x²`
/// underflows, their limits with a range error; beyond where `x²` overflows
/// in the answer's precision, their limits raising nothing. `erfc(±0)` is
/// one, flagged inexact, and for the double a range error too.
fn error_function_limit(
    unit: &mut Unit,
    x: f64,
    complement: bool,
    float: bool,
) -> Result<Option<(f64, bool)>, Exception> {
    let magnitude = x.abs();
    let limit = |positive: bool| match (complement, positive) {
        (false, positive) => {
            if positive {
                1.0
            } else {
                -1.0
            }
        }
        (true, true) => 0.0,
        (true, false) => 2.0,
    };
    if x.is_nan() {
        return Ok(Some((x, false)));
    }
    if magnitude
        >= if float {
            FLOAT_SQUARE_OVERFLOWS
        } else {
            SQUARE_OVERFLOWS
        }
    {
        return Ok(Some((limit(x > 0.0), false)));
    }
    if magnitude > EXPONENTIAL_UNDERFLOWS {
        unit.raise(Exception::Inexact)?;
        return Ok(Some((limit(x > 0.0), true)));
    }
    if x == 0.0 {
        if !complement {
            return Ok(Some((x, false)));
        }
        unit.raise(Exception::Inexact)?;
        return Ok(Some((1.0, !float)));
    }
    if magnitude < SQUARE_UNDERFLOWS {
        unit.raise(Exception::Underflow)?;
        return Ok(Some((
            if complement { 1.0 } else { 0.0_f64.copysign(x) },
            true,
        )));
    }
    Ok(None)
}

/// Below which the error functions are summed from their series, and from
/// which Laplace's fraction takes over.
const SERIES_BELOW: f64 = 2.5;

/// More of the error function's series than any argument below
/// [`SERIES_BELOW`] needs before its terms fall below the sum's last place.
const SERIES_MOST: u32 = 60;

/// How deep Laplace's fraction is started from each argument up: deep enough
/// to lie within 2⁻⁷⁰ of its limit there, as measured exactly, the fraction
/// converging the faster the larger the argument.
const FRACTION_DEPTHS: [(f64, u32); 5] = [
    (10.0, 13),
    (6.0, 20),
    (4.0, 32),
    (3.0, 47),
    (SERIES_BELOW, 63),
];

/// [`FRACTION_DEPTHS`]' depth for `magnitude`, at least [`SERIES_BELOW`].
fn fraction_depth(magnitude: f64) -> u32 {
    FRACTION_DEPTHS
        .iter()
        .find(|&&(from, _)| magnitude >= from)
        .map_or(
            FRACTION_DEPTHS[FRACTION_DEPTHS.len() - 1].1,
            |&(_, depth)| depth,
        )
}

/// Laplace's fraction `1 + 1/t/(1 + 2/t/(1 + …))`, `t` in F4 and one in F2,
/// into F5, started `depth` deep.
fn laplace_fraction(e: &mut Extended<'_>, depth: u32) -> Result<(), Exception> {
    e.constant(5, 1.0);
    for n in (1..=depth).rev() {
        e.constant(6, f64::from(n));
        e.op(FpaDyadicOp::Dvf, 6, 6, 4)?;
        e.op(FpaDyadicOp::Dvf, 6, 6, 5)?;
        e.op(FpaDyadicOp::Adf, 5, 2, 6)?;
    }
    Ok(())
}

/// `erf(x)` or `erfc(x)` in extended precision: from the Maclaurin series
/// below [`SERIES_BELOW`] and from Laplace's continued fraction above.
fn error_function_value(e: &mut Extended<'_>, x: f64, complement: bool) -> Result<f64, Exception> {
    let magnitude = x.abs();
    e.constant(1, magnitude);
    e.constant(2, 1.0);
    e.constant(7, 2.0);
    if magnitude < SERIES_BELOW {
        // erf = 2/√π · Σ (−1)ⁿ x^(2n+1) / (n! (2n+1))
        e.op(FpaDyadicOp::Muf, 3, 1, 1)?;
        e.op(FpaDyadicOp::Muf, 4, 1, 2)?;
        e.op(FpaDyadicOp::Muf, 5, 1, 2)?;
        for n in 1..=SERIES_MOST {
            e.constant(6, -f64::from(n));
            e.op(FpaDyadicOp::Muf, 4, 4, 3)?;
            e.op(FpaDyadicOp::Dvf, 4, 4, 6)?;
            e.constant(6, f64::from(2 * n + 1));
            e.op(FpaDyadicOp::Dvf, 6, 4, 6)?;
            e.mono(FpaMonadicOp::Mvf, 0, 5)?;
            e.op(FpaDyadicOp::Adf, 5, 5, 6)?;
            // Past x² the terms shrink and alternate, so the rest sum to less
            // than one that left the sum unmoved; working them on would only
            // underflow.
            if f64::from(n) > magnitude * magnitude && e.0.equal(5, 0) {
                break;
            }
        }
        e.constant(6, core::f64::consts::FRAC_2_SQRT_PI);
        e.op(FpaDyadicOp::Muf, 5, 5, 6)?;
        if complement {
            e.op(FpaDyadicOp::Suf, 5, 2, 5)?;
            if x < 0.0 {
                e.op(FpaDyadicOp::Suf, 5, 7, 5)?;
            }
            return e.answer(5);
        }
        return e.answer(5).map(|y| y.copysign(x));
    }
    // erfc = e^−x² / (x√π) · 1/(1 + 1/(2x²)/(1 + 2/(2x²)/(1 + …)))
    e.op(FpaDyadicOp::Muf, 3, 1, 1)?;
    e.op(FpaDyadicOp::Muf, 4, 3, 7)?;
    laplace_fraction(e, fraction_depth(magnitude))?;
    e.mono(FpaMonadicOp::Mnf, 6, 3)?;
    e.mono(FpaMonadicOp::Exp, 6, 6)?;
    e.constant(0, PI);
    e.mono(FpaMonadicOp::Sqt, 0, 0)?;
    e.op(FpaDyadicOp::Muf, 0, 0, 1)?;
    e.op(FpaDyadicOp::Muf, 0, 0, 5)?;
    e.op(FpaDyadicOp::Dvf, 6, 6, 0)?;
    if complement {
        if x < 0.0 {
            e.op(FpaDyadicOp::Suf, 6, 7, 6)?;
        }
        return e.answer(6);
    }
    e.op(FpaDyadicOp::Suf, 6, 2, 6)?;
    e.answer(6).map(|y| y.copysign(x))
}

/// `erf(x)`, `erff`, `erfc` and `erfcf`.
pub(super) fn error_function<const WHICH: u8, const FLOAT: bool>(
    os: &mut Os<'_>,
    regs: &mut Regs,
) -> Result<(), Fail> {
    let x = double(regs, 0);
    let complement = WHICH == ERFC;
    let worked = as_client(os, |unit| {
        if let Some(limit) = error_function_limit(unit, x, complement, FLOAT)? {
            return Ok(limit);
        }
        error_function_value(&mut Extended(unit), x, complement).map(|value| (value, false))
    })?;
    let Some((value, range_error)) = worked else {
        return Ok(());
    };
    if range_error {
        set_errno(os, regs, ERANGE)?;
    }
    if FLOAT {
        give_single(os, |unit| unit.single(value))
    } else {
        give_double(os, |_| Ok(value))
    }
}

/// Which gamma function.
pub(super) const LGAMMA: u8 = 0;
pub(super) const TGAMMA: u8 = 1;

/// Lanczos's coefficients for g = 7, n = 9 (Godfrey's set), with which
/// `Γ(x + 1) = √(2π) (x + g + ½)^(x + ½) e^−(x + g + ½) Σ`.
const LANCZOS: [f64; 9] = [
    0.999_999_999_999_809_9,
    676.520_368_121_885_1,
    -1_259.139_216_722_402_8,
    771.323_428_777_653_1,
    -176.615_029_162_140_6,
    12.507_343_278_686_905,
    -0.138_571_095_265_720_12,
    9.984_369_578_019_572e-6,
    1.505_632_735_149_311_6e-7,
];
const LANCZOS_G: f64 = 7.0;
/// ½ ln 2π.
const HALF_LN_TWO_PI: f64 = 0.918_938_533_204_672_7;
/// Where Γ grows past the largest double.
const GAMMA_OVERFLOWS: f64 = 171.624_376_956_302_7;
/// Stirling's coefficients after the first: `1/12`, `1/360`, `1/1260`,
/// `1/1680`, their signs alternating.
const STIRLING: [f64; 4] = [1.0 / 12.0, 1.0 / 360.0, 1.0 / 1260.0, 1.0 / 1680.0];

/// `ln Γ(z)` for `z` at least ½, by Lanczos's series in extended precision,
/// in F0.
fn log_gamma_right(e: &mut Extended<'_>, z: f64) -> Result<(), Exception> {
    e.constant(1, z - 1.0);
    e.constant(2, LANCZOS[0]);
    for (k, &c) in (1u8..).zip(&LANCZOS[1..]) {
        e.constant(3, f64::from(k));
        e.op(FpaDyadicOp::Adf, 3, 1, 3)?;
        e.constant(4, c);
        e.op(FpaDyadicOp::Dvf, 4, 4, 3)?;
        e.op(FpaDyadicOp::Adf, 2, 2, 4)?;
    }
    e.constant(3, LANCZOS_G + 0.5);
    e.op(FpaDyadicOp::Adf, 3, 1, 3)?;
    e.constant(4, 0.5);
    e.op(FpaDyadicOp::Adf, 4, 1, 4)?;
    e.mono(FpaMonadicOp::Lgn, 5, 3)?;
    e.op(FpaDyadicOp::Muf, 5, 5, 4)?;
    e.op(FpaDyadicOp::Suf, 5, 5, 3)?;
    e.mono(FpaMonadicOp::Lgn, 2, 2)?;
    e.op(FpaDyadicOp::Adf, 5, 5, 2)?;
    e.constant(6, HALF_LN_TWO_PI);
    e.op(FpaDyadicOp::Adf, 0, 5, 6)
}

/// Γ(z) for `z` past nought: Lanczos's below its overflow, `Γ(z + 1) / z`
/// below ½.
fn gamma_positive(e: &mut Extended<'_>, z: f64) -> Result<f64, Exception> {
    if z < 0.5 {
        let up = e.0.add(z, 1.0)?;
        let gamma = gamma_positive(e, up)?;
        return e.0.div(gamma, z);
    }
    log_gamma_right(e, z)?;
    e.mono(FpaMonadicOp::Exp, 0, 0)?;
    e.answer(0)
}

/// Γ(x) for `x` below one as RISC OS 5.30 reflects it: `π / (sin πx · Γ(1 −
/// x))`, the sine that of the rounded product, which strays beside a whole
/// number.
fn reflected(e: &mut Extended<'_>, x: f64) -> Result<f64, Exception> {
    let unit = &mut *e.0;
    let angle = unit.mul(PI, x)?;
    let sine = unit.double_op(FpaMonadicOp::Sin, angle)?;
    let rest = unit.sub(1.0, x)?;
    let gamma = gamma_positive(e, rest)?;
    let product = e.0.mul(sine, gamma)?;
    e.0.div(PI, product)
}

/// Γ(x) as RISC OS 5.30 works it where no product of whole numbers does.
fn gamma_general(e: &mut Extended<'_>, x: f64) -> Result<f64, Exception> {
    if x < 1.0 {
        reflected(e, x)
    } else {
        gamma_positive(e, x)
    }
}

/// `ln Γ(z)` for a large `z`: Stirling's series, worked in double precision
/// as RISC OS 5.30 works it, its last terms underflowing for the largest.
fn stirling(unit: &mut Unit, z: f64) -> Result<f64, Exception> {
    let logarithm = unit.double_op(FpaMonadicOp::Lgn, z)?;
    let less_half = unit.sub(z, 0.5)?;
    let main = unit.mul(less_half, logarithm)?;
    let main = unit.sub(main, z)?;
    let main = unit.add(main, HALF_LN_TWO_PI)?;
    let reciprocal = unit.div(1.0, z)?;
    let square = unit.mul(reciprocal, reciprocal)?;
    let [first, rest @ ..] = STIRLING;
    let mut series = 0.0;
    for &coefficient in rest.iter().rev() {
        let term = unit.sub(coefficient, series)?;
        series = unit.mul(square, term)?;
    }
    let series = unit.sub(first, series)?;
    let correction = unit.mul(reciprocal, series)?;
    unit.add(main, correction)
}

/// `x` as a word, where it is a whole number with a place in one.
fn whole_word(x: f64) -> Option<i32> {
    let mut own = Unit::own();
    let word = own.fix(Rounding::Zero, x).ok()?;
    (own.fpsr() & fpsr::FLAGS == 0).then_some(word)
}

/// `Γ(n) = (n − 1)!`, multiplied out downward at `precision`, as RISC OS
/// 5.30 multiplies it.
fn factorial(unit: &mut Unit, n: i32, precision: Precision) -> Result<f64, Exception> {
    let mut product = 1.0;
    for k in (2..n).rev() {
        product = unit.dyadic(
            FpaDyadicOp::Muf,
            precision,
            Rounding::Nearest,
            product,
            f64::from(k),
        )?;
        if product.is_infinite() {
            break;
        }
    }
    Ok(product)
}

/// `lgamma(x)`'s work: one and two exactly nought, a pole a division by
/// nought, Stirling's series where Γ overflows, and elsewhere the logarithm
/// of Γ as worked without whole-number products.
#[expect(
    clippy::float_cmp,
    reason = "lgamma is exactly nought at exactly one and two"
)]
fn lgamma_of(unit: &mut Unit, x: f64) -> Result<f64, Exception> {
    if x.is_nan() {
        return Ok(x);
    }
    if x.is_infinite() {
        return Ok(f64::INFINITY);
    }
    if x == 1.0 || x == 2.0 {
        return Ok(0.0);
    }
    if x <= 0.0 && is_whole(x) {
        return unit.div(1.0, 0.0);
    }
    if x > GAMMA_OVERFLOWS {
        return stirling(unit, x);
    }
    if x < 1.0 - GAMMA_OVERFLOWS {
        // ln π − ln |sin πx| − ln Γ(1 − x)
        let angle = unit.mul(PI, x)?;
        let sine = unit.double_op(FpaMonadicOp::Sin, angle)?;
        let ln_sine = unit.double_op(FpaMonadicOp::Lgn, sine.abs())?;
        let ln_pi = unit.double_op(FpaMonadicOp::Lgn, PI)?;
        let reflection = unit.sub(ln_pi, ln_sine)?;
        let rest = unit.sub(1.0, x)?;
        let ln_gamma = stirling(unit, rest)?;
        return unit.sub(reflection, ln_gamma);
    }
    let gamma = gamma_general(&mut Extended(unit), x)?;
    unit.double_op(FpaMonadicOp::Lgn, gamma.abs())
}

/// `tgamma(x)`'s work, a float's where `float`: nought a division by
/// nought, a negative whole number or minus infinity the domain, a whole
/// number with a place in a word its factorial in the answer's own
/// precision, `e` to Stirling's series where Γ overflows, and else the
/// general form.
fn tgamma_of(unit: &mut Unit, x: f64, float: bool) -> Result<f64, Exception> {
    if x.is_nan() || x == f64::INFINITY {
        return Ok(x);
    }
    if x == 0.0 {
        return unit.div(1.0, x);
    }
    if x == f64::NEG_INFINITY || (x < 0.0 && is_whole(x)) {
        return invalid(unit);
    }
    if let Some(n) = whole_word(x).filter(|&n| n >= 1) {
        return factorial(unit, n, super::precision(float));
    }
    if x > GAMMA_OVERFLOWS {
        let logarithm = stirling(unit, x)?;
        return unit.double_op(FpaMonadicOp::Exp, logarithm);
    }
    gamma_general(&mut Extended(unit), x)
}

/// `lgamma(x)`, `lgammaf`, `tgamma` and `tgammaf`.
pub(super) fn gamma<const WHICH: u8, const FLOAT: bool>(
    os: &mut Os<'_>,
    regs: &mut Regs,
) -> Result<(), Fail> {
    let x = double(regs, 0);
    let work = move |unit: &mut Unit| {
        if WHICH == LGAMMA {
            lgamma_of(unit, x)
        } else {
            tgamma_of(unit, x, FLOAT)
        }
    };
    if FLOAT {
        give_single(os, work)
    } else {
        give_double(os, work)
    }
}

#[cfg(test)]
mod tests;
