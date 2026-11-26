//! Floating point as the library's routines take and give it: a double in a
//! register pair, its high word first, an answer in F0, and an FPA to work
//! it on — the library's own, every trap off, or one working as the
//! client's, whose traps and flags are the client's.

use tairix_riscos_arm::fpa::{Exception, Format, Fpa, Precision, Rounding};
use tairix_riscos_arm::instruction::{FpaCompareOp, FpaDyadicOp, FpaMonadicOp, FpaOperand};
use tairix_riscos_arm::psr;
use tairix_riscos_kernel::os::{Fail, Os, Regs};
use tairix_softfloat::{
    fpa_words, from_fpa_words, Binary, Ieee, Mode, Nans, Policy, Tininess, Unpacked,
};

/// The double R`n` and R`n + 1` hold, its high word first.
pub(crate) fn double(regs: &Regs, n: usize) -> f64 {
    f64::from_bits(from_fpa_words([regs.r[n], regs.r[n + 1]]))
}

/// The raw single `bits` as a double, exactly: a NaN quiet with its sign and
/// payload, alike on every host, where a host's own widening may change them.
pub(crate) fn widened(bits: u32) -> f64 {
    match Binary::SINGLE.unpack(u64::from(bits)) {
        Unpacked::Nan {
            negative, fraction, ..
        } => f64::from_bits(Binary::DOUBLE.quiet_nan(negative, fraction)),
        _ => f64::from(f32::from_bits(bits)),
    }
}

/// Rounding to a double, to nearest, as the FPA rounds.
pub(crate) const DOUBLE: Ieee = Ieee {
    format: Binary::DOUBLE,
    mode: Mode::Nearest,
    policy: Policy {
        tininess: Tininess::BeforeRounding,
        nans: Nans::Propagate,
        flush_to_zero: false,
    },
};

/// The raw bits of the single nearest `x`, as the FPA stores one: a NaN
/// quiet with its sign and its payload's top, alike on every host.
pub(crate) fn single_bits(x: f64) -> u32 {
    let narrowed = DOUBLE.convert(x.to_bits(), Binary::SINGLE).bits;
    u32::try_from(narrowed).unwrap_or(u32::MAX)
}

/// `value` given in F0.
pub(crate) fn answer(os: &mut Os<'_>, value: f64) {
    os.set_fpa_double(0, value.to_bits());
}

/// A double's fraction bits, their mask, and its exponent's bias.
pub(crate) const FRACTION_BITS: i32 = Binary::DOUBLE.fraction_bits().cast_signed();
pub(crate) const FRACTION: u64 = (1 << FRACTION_BITS) - 1;
pub(crate) const BIAS: i32 = Binary::DOUBLE.bias();

/// The exponent field of the double `bits`, biased.
pub(crate) fn biased_exponent(bits: u64) -> i32 {
    i32::try_from(bits >> FRACTION_BITS & Binary::DOUBLE.exp_mask()).unwrap_or(0)
}

/// `x` with its fraction cut off, exactly: toward nought.
pub(crate) fn whole_part(x: f64) -> f64 {
    let bits = x.to_bits();
    let exponent = biased_exponent(bits) - BIAS;
    if exponent >= FRACTION_BITS {
        return x;
    }
    if exponent < 0 {
        return 0.0_f64.copysign(x);
    }
    f64::from_bits(bits & !((1 << (FRACTION_BITS - exponent)) - 1))
}

/// Whether `x` is its own whole part: a whole number or an infinity.
pub(crate) fn is_whole(x: f64) -> bool {
    !x.is_nan() && whole_part(x).to_bits() == x.to_bits()
}

/// An FPA to work a routine's arithmetic on, its registers the routine's
/// own.
pub(crate) struct Unit(Fpa);

impl Unit {
    /// The library's own: every trap off.
    pub(crate) const fn own() -> Self {
        Self(Fpa::new())
    }

    /// One working as the client's FPA does: its status the client's.
    pub(crate) fn client(os: &mut Os<'_>) -> Self {
        let mut fpa = Fpa::new();
        fpa.write_fpsr(os.fpsr());
        Self(fpa)
    }

    pub(crate) const fn fpsr(&self) -> u32 {
        self.0.fpsr()
    }

    /// The status made `value`, as `WFS` writes it.
    pub(crate) fn set_fpsr(&mut self, value: u32) {
        self.0.write_fpsr(value);
    }

    /// F`n` loaded with `value`.
    pub(crate) fn load(&mut self, n: u8, value: f64) {
        let [high, low] = fpa_words(value.to_bits());
        // A double's load raises nothing.
        let _ = self.0.load(Format::Double, n, [high, low, 0, 0]);
    }

    /// F`n` loaded with the extended `words`, as `LDFE` loads them.
    pub(crate) fn load_extended(&mut self, n: u8, [head, high, low]: [u32; 3]) {
        // An extended load raises nothing.
        let _ = self.0.load(Format::Extended, n, [head, high, low, 0]);
    }

    /// F`n` as `STFD` stores it.
    pub(crate) fn take(&mut self, n: u8) -> Result<f64, Exception> {
        let [high, low, ..] = self.0.store(Format::Double, n)?;
        Ok(f64::from_bits(from_fpa_words([high, low])))
    }

    /// `op Fd, Fm`, the registers the unit's.
    pub(crate) fn monadic_in(
        &mut self,
        op: FpaMonadicOp,
        precision: Precision,
        rounding: Rounding,
        d: u8,
        m: u8,
    ) -> Result<(), Exception> {
        self.0
            .monadic(op, precision, rounding, d, FpaOperand::Register(m))
    }

    /// `op Fd, Fn, Fm`, the registers the unit's.
    pub(crate) fn dyadic_in(
        &mut self,
        op: FpaDyadicOp,
        precision: Precision,
        rounding: Rounding,
        [d, n, m]: [u8; 3],
    ) -> Result<(), Exception> {
        self.0
            .dyadic(op, precision, rounding, d, n, FpaOperand::Register(m))
    }

    /// `op` of `x`, rounded to `precision` as `rounding` says.
    pub(crate) fn monadic(
        &mut self,
        op: FpaMonadicOp,
        precision: Precision,
        rounding: Rounding,
        x: f64,
    ) -> Result<f64, Exception> {
        self.load(1, x);
        self.monadic_in(op, precision, rounding, 0, 1)?;
        self.take(0)
    }

    /// `op` of `a` and `b`, rounded to `precision` as `rounding` says.
    pub(crate) fn dyadic(
        &mut self,
        op: FpaDyadicOp,
        precision: Precision,
        rounding: Rounding,
        a: f64,
        b: f64,
    ) -> Result<f64, Exception> {
        self.load(1, a);
        self.load(2, b);
        self.dyadic_in(op, precision, rounding, [0, 1, 2])?;
        self.take(0)
    }

    /// A double-precision operation of one double, rounded to nearest.
    pub(crate) fn double_op(&mut self, op: FpaMonadicOp, x: f64) -> Result<f64, Exception> {
        self.monadic(op, Precision::Double, Rounding::Nearest, x)
    }

    /// `x` rounded to a single, as `MVFS` rounds it.
    pub(crate) fn single(&mut self, x: f64) -> Result<f64, Exception> {
        self.monadic(FpaMonadicOp::Mvf, Precision::Single, Rounding::Nearest, x)
    }

    fn arithmetic(&mut self, op: FpaDyadicOp, a: f64, b: f64) -> Result<f64, Exception> {
        self.dyadic(op, Precision::Double, Rounding::Nearest, a, b)
    }

    pub(crate) fn add(&mut self, a: f64, b: f64) -> Result<f64, Exception> {
        self.arithmetic(FpaDyadicOp::Adf, a, b)
    }

    pub(crate) fn sub(&mut self, a: f64, b: f64) -> Result<f64, Exception> {
        self.arithmetic(FpaDyadicOp::Suf, a, b)
    }

    pub(crate) fn mul(&mut self, a: f64, b: f64) -> Result<f64, Exception> {
        self.arithmetic(FpaDyadicOp::Muf, a, b)
    }

    pub(crate) fn div(&mut self, a: f64, b: f64) -> Result<f64, Exception> {
        self.arithmetic(FpaDyadicOp::Dvf, a, b)
    }

    /// Whether F`n` is nought, as `CMF Fn, #0` finds it, raising nothing for
    /// a number.
    pub(crate) fn is_zero(&mut self, n: u8) -> bool {
        self.compares_equal(n, FpaOperand::Constant(0))
    }

    /// Whether F`n` equals F`m`, as `CMF Fn, Fm` finds them, raising nothing
    /// for numbers.
    pub(crate) fn equal(&mut self, n: u8, m: u8) -> bool {
        self.compares_equal(n, FpaOperand::Register(m))
    }

    fn compares_equal(&mut self, n: u8, operand: FpaOperand) -> bool {
        self.0
            .compare(FpaCompareOp::Cmf, n, operand)
            .is_ok_and(|flags| flags & psr::Z != 0)
    }

    /// `FIX`: `x` rounded to a signed word as `rounding` says.
    pub(crate) fn fix(&mut self, rounding: Rounding, x: f64) -> Result<i32, Exception> {
        self.load(1, x);
        self.0
            .fix(rounding, FpaOperand::Register(1))
            .map(u32::cast_signed)
    }

    /// `exception` raised as an operation raises it: trapped where enabled,
    /// else flagged, an overflow or an underflow flagging inexact too.
    pub(crate) fn raise(&mut self, exception: Exception) -> Result<(), Exception> {
        let rounded = matches!(exception, Exception::Overflow | Exception::Underflow);
        for raised in core::iter::once(exception).chain(rounded.then_some(Exception::Inexact)) {
            let fpsr = self.fpsr();
            if fpsr & raised.enable() != 0 {
                return Err(raised);
            }
            self.set_fpsr(fpsr | raised.flag());
        }
        Ok(())
    }

    /// `exception` flagged, then trapped where enabled: how chunk 4's
    /// conversions raise an invalid operation.
    pub(crate) fn flag_then_raise(&mut self, exception: Exception) -> Result<(), Exception> {
        let fpsr = self.fpsr();
        self.set_fpsr(fpsr | exception.flag());
        if fpsr & exception.enable() != 0 {
            return Err(exception);
        }
        Ok(())
    }
}

/// A unit's registers worked at extended precision, to nearest.
pub(crate) struct Extended<'a>(pub(crate) &'a mut Unit);

impl Extended<'_> {
    /// F`n` loaded with `value`.
    pub(crate) fn constant(&mut self, n: u8, value: f64) {
        self.0.load(n, value);
    }

    /// `op Fd, Fn, Fm`.
    pub(crate) fn op(&mut self, op: FpaDyadicOp, d: u8, n: u8, m: u8) -> Result<(), Exception> {
        self.0
            .dyadic_in(op, Precision::Extended, Rounding::Nearest, [d, n, m])
    }

    /// `op Fd, Fm`.
    pub(crate) fn mono(&mut self, op: FpaMonadicOp, d: u8, m: u8) -> Result<(), Exception> {
        self.0
            .monadic_in(op, Precision::Extended, Rounding::Nearest, d, m)
    }

    /// F`n` rounded to `precision`, through F0.
    pub(crate) fn answer_in(&mut self, n: u8, precision: Precision) -> Result<f64, Exception> {
        self.0
            .monadic_in(FpaMonadicOp::Mvf, precision, Rounding::Nearest, 0, n)?;
        self.0.take(0)
    }

    /// F`n` rounded to a double, through F0.
    pub(crate) fn answer(&mut self, n: u8) -> Result<f64, Exception> {
        self.answer_in(n, Precision::Double)
    }
}

/// `work` done on the library's own unit, whose traps are all off, so that
/// nothing stops it.
pub(crate) fn quietly(work: impl FnOnce(&mut Unit) -> Result<f64, Exception>) -> f64 {
    work(&mut Unit::own()).unwrap_or(f64::NAN)
}

/// `work` done as the client's FPA would do it: the flags it raises left
/// the client's, and an exception the client traps raised as that trap. None
/// where a trap was raised and came back.
pub(crate) fn as_client<T>(
    os: &mut Os<'_>,
    work: impl FnOnce(&mut Unit) -> Result<T, Exception>,
) -> Result<Option<T>, Fail> {
    let mut unit = Unit::client(os);
    let worked = work(&mut unit);
    os.set_fpsr(unit.fpsr());
    match worked {
        Ok(value) => Ok(Some(value)),
        Err(exception) => {
            os.floating_point(exception)?;
            Ok(None)
        }
    }
}

/// [`as_client`]'s work, its double answered in F0.
pub(crate) fn answer_as_client(
    os: &mut Os<'_>,
    work: impl FnOnce(&mut Unit) -> Result<f64, Exception>,
) -> Result<(), Fail> {
    if let Some(value) = as_client(os, work)? {
        answer(os, value);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
