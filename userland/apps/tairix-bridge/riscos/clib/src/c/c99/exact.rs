//! The two C99 operations no FPA instruction gives: the fused multiply-add,
//! worked exactly and rounded first to the FPA's extended precision, as RISC
//! OS 5.30's flags show it is, then to the answer's own; and the low bits of
//! the quotient a remainder was taken with.

use tairix_riscos_arm::fpa::{Exception, Rounding};
use tairix_riscos_arm::instruction::{FpaDyadicOp, FpaMonadicOp};
use tairix_softfloat::exact::{self, Parts};
use tairix_softfloat::extended::BIAS;
use tairix_softfloat::{fpa_words, Binary, Mode, Rounding as Round, Unpacked};

use crate::c::fpa::Unit;

/// A nonzero finite double's parts, normalized; none for a nought, an
/// infinity or a NaN.
fn parts(x: f64) -> Option<Parts> {
    match Binary::DOUBLE.unpack(x.to_bits()) {
        Unpacked::Finite { parts, .. } => Some(parts.normalized()),
        _ => None,
    }
}

/// `a·b + c`, `c` none for a nought: worked exactly, then rounded to the
/// extended precision's 64 bits, inexact raised where that lost anything;
/// the extended words as `LDFE` takes them, none for an exact nought.
fn extended_sum(
    unit: &mut Unit,
    a: Parts,
    b: Parts,
    c: Option<Parts>,
) -> Result<Option<[u32; 3]>, Exception> {
    let sum = match c {
        Some(c) => exact::fused(a, b, c),
        None => Some(exact::multiply(a, b)),
    };
    let Some(sum) = sum else {
        return Ok(None);
    };
    let rounded = sum.rounded(Round::extended(Mode::Nearest));
    if rounded.inexact {
        unit.raise(Exception::Inexact)?;
    }
    let [high, low] = fpa_words(rounded.sig);
    let sign = if sum.negative { 1 << 31 } else { 0 };
    let exponent = sum.exp + i32::from(rounded.carried) + BIAS;
    Ok(Some([sign | exponent.cast_unsigned(), high, low]))
}

/// `fma(x, y, z)`'s work, rounded to a float where `float`: no numbers,
/// infinities and noughts as the FPA's own product and sum give them, but
/// for a finite product, which is exact and so never overflows.
pub(super) fn fused(
    unit: &mut Unit,
    x: f64,
    y: f64,
    z: f64,
    float: bool,
) -> Result<f64, Exception> {
    let precision = super::precision(float);
    if x.is_finite() && y.is_finite() && z.is_infinite() {
        return Ok(z);
    }
    let (Some(x_parts), Some(y_parts), true) = (parts(x), parts(y), z.is_finite()) else {
        let product = unit.mul(x, y)?;
        return unit.dyadic(FpaDyadicOp::Adf, precision, Rounding::Nearest, product, z);
    };
    let Some(words) = extended_sum(unit, x_parts, y_parts, parts(z))? else {
        return Ok(0.0);
    };
    unit.load_extended(1, words);
    unit.monadic_in(FpaMonadicOp::Mvf, precision, Rounding::Nearest, 0, 1)?;
    unit.take(0)
}

/// The low 31 bits of the whole number nearest `x / y` — the quotient
/// `remainder` takes, halfway to even — with that quotient's sign; nought
/// where there is no such number.
pub(super) fn quotient_bits(x: f64, y: f64) -> u32 {
    let (Some(a), Some(b)) = (parts(x), parts(y)) else {
        return 0;
    };
    let magnitude = u32::try_from(exact::remainder(a, b).quotient & 0x7FFF_FFFF).unwrap_or(0);
    if a.negative == b.negative {
        magnitude
    } else {
        magnitude.wrapping_neg()
    }
}

#[cfg(test)]
mod tests;
