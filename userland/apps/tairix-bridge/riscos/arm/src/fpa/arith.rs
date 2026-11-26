//! The data operations: `lib/softfloat`'s arithmetic, with FPEmulator's
//! choices where the data sheet leaves one, observed black-box.

use tairix_softfloat::exact::{self, Exact, Parts};

use super::internal::{
    denormalise, exponent_field, keep_mask, round, sign, Direction, Internal, Number, Rounded,
    BIAS, EXPONENT, QUIET, SIGN, UNCOMMON, UNITS,
};
use super::trans::Function;
use super::{fpsr, Exception, Precision, Rounding};
use crate::instruction::{FpaCompareOp, FpaDyadicOp, FpaMonadicOp};

/// Why an operation is invalid, recorded in the NaN it leaves.
#[derive(Clone, Copy)]
#[repr(u8)]
pub(super) enum Reason {
    MagnitudeSubtractionOfInfinities = 4,
    InfinityTimesZero = 5,
    ZeroTimesInfinity = 6,
    ZeroDividedByZero = 7,
    InfinityDividedByInfinity = 8,
    RemainderOfInfinity = 9,
    RemainderByZero = 10,
    RootOfNegative = 11,
    SinCosRange = 16,
    SinCosInfinity = 17,
    TanRange = 18,
    TanInfinity = 19,
    AsnAcsRange = 20,
    AsnAcsInfinity = 21,
    PolarOfZeros = 22,
    PolarOfInfinities = 23,
    LogOfNegative = 24,
    NegativeToPower = 25,
    BadInfinitePower = 27,
}

/// What an operation produced: a value final as it stands, or one the
/// round stage is still to round to the precision.
#[derive(Clone, Copy)]
pub(super) enum Value {
    Final(Internal),
    Exact(Exact),
}

/// A nonzero value past every precision's exponent range, above it or
/// below, for the round stage to overflow or underflow.
pub(super) fn beyond_range(negative: bool, above: bool) -> Value {
    Value::Exact(Exact::of(Parts {
        negative,
        exp: if above { 1 << 16 } else { -(1 << 16) },
        sig: UNITS,
    }))
}

/// How a NaN copied to another precision is converted.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Conversion {
    None,
    Quiet,
    Full,
}

/// One instruction's arithmetic: the FPSR as it leaves it, the precision and
/// rounding it names, and the operations over them.
pub(super) struct Unit {
    pub fpsr: u32,
    pub precision: Precision,
    pub rounding: Rounding,
}

impl Unit {
    pub const fn new(fpsr: u32, precision: Precision, rounding: Rounding) -> Self {
        Self {
            fpsr,
            precision,
            rounding,
        }
    }

    /// Raises `exception`: a trap if enabled, else its cumulative flag.
    pub fn raise(&mut self, exception: Exception) -> Result<(), Exception> {
        if self.fpsr & exception.enable() != 0 {
            return Err(exception);
        }
        self.fpsr |= exception.flag();
        Ok(())
    }

    /// The register value of `value`.
    pub fn finish(&mut self, value: Value) -> Result<Internal, Exception> {
        match value {
            Value::Final(value) => Ok(value),
            Value::Exact(x) => {
                let r = round(x, self.precision.bits(), self.rounding);
                self.finish_rounded(r)
            }
        }
    }

    /// A result rounded once, checked against the precision's exponent range.
    pub fn finish_rounded(&mut self, r: Rounded) -> Result<Internal, Exception> {
        let precision = self.precision;
        let (value, direction) = if r.exponent > precision.max_exponent() {
            self.overflow(r)?
        } else if r.exponent < precision.min_exponent() {
            self.underflow(r)?
        } else {
            (
                Internal {
                    head: sign(r.negative) | exponent_field(r.exponent),
                    mantissa: r.mantissa,
                },
                r.direction,
            )
        };
        if direction != Direction::Exact {
            self.raise(Exception::Inexact)?;
        }
        Ok(value)
    }

    fn overflow(&mut self, r: Rounded) -> Result<(Internal, Direction), Exception> {
        self.raise(Exception::Overflow)?;
        let finite = match self.rounding {
            Rounding::Nearest => false,
            Rounding::Zero => true,
            Rounding::PlusInfinity => r.negative,
            Rounding::MinusInfinity => !r.negative,
        };
        Ok(if finite {
            let precision = self.precision;
            (
                Internal {
                    head: sign(r.negative) | exponent_field(precision.max_exponent()),
                    mantissa: keep_mask(precision.bits()),
                },
                Direction::Down,
            )
        } else {
            (Internal::infinity(r.negative), Direction::Up)
        })
    }

    /// A result below the precision's normalised range: a trap if enabled,
    /// else the value denormalised and rounded again, the first rounding's
    /// direction breaking a tie. Underflow is flagged where this second
    /// rounding loses bits; a single or double denormal keeps its own form.
    fn underflow(&mut self, r: Rounded) -> Result<(Internal, Direction), Exception> {
        if r.mantissa == 0 {
            return Ok((Internal::zero(r.negative), r.direction));
        }
        if self.fpsr & Exception::Underflow.enable() != 0 {
            return Err(Exception::Underflow);
        }
        let precision = self.precision;
        let min = precision.min_exponent();
        let shift = u32::try_from(min - r.exponent).unwrap_or(u32::MAX);
        let (mut mantissa, second) = denormalise(r, shift, precision.bits(), self.rounding);
        let mut direction = if second == Direction::Exact {
            r.direction
        } else {
            second
        };
        let mut lost = second != Direction::Exact;
        if self.fpsr & fpsr::ND != 0 && mantissa != 0 && mantissa != UNITS {
            mantissa = 0;
            direction = Direction::Down;
            lost = true;
        }
        if lost {
            self.fpsr |= Exception::Underflow.flag();
        }
        let negative = sign(r.negative);
        let value = if mantissa == 0 {
            Internal::zero(r.negative)
        } else if mantissa & UNITS != 0 {
            Internal {
                head: negative | exponent_field(min),
                mantissa,
            }
        } else if precision == Precision::Extended {
            Internal {
                head: negative | UNCOMMON,
                mantissa,
            }
        } else {
            Internal {
                head: negative | UNCOMMON | exponent_field(min - 1),
                mantissa: mantissa | UNITS,
            }
        };
        Ok((value, direction))
    }

    /// An invalid operation not caused by a signalling NaN: a trap, or the
    /// extended quiet NaN recording why, with the sign the operation would
    /// have given.
    pub(super) fn invalid(&mut self, reason: Reason, negative: bool) -> Result<Value, Exception> {
        self.raise(Exception::InvalidOperation)?;
        Ok(Value::Final(Internal {
            head: sign(negative) | UNCOMMON | EXPONENT,
            mantissa: QUIET | u64::from(reason as u8) << 40,
        }))
    }

    /// A NaN operand's result: invalid if it signals, then quieted and
    /// converted to the precision.
    pub(super) fn propagate(&mut self, nan: Internal) -> Result<Value, Exception> {
        let mut nan = nan;
        if nan.signalling() {
            self.raise(Exception::InvalidOperation)?;
            nan.mantissa |= QUIET;
        }
        Ok(Value::Final(convert_nan(nan, self.precision)))
    }

    /// Two NaN or infinite operands, at least one a NaN: the first operand's
    /// NaN wins, though a signalling second still signals.
    pub(super) fn propagate_either(
        &mut self,
        a: Internal,
        b: Internal,
    ) -> Result<Value, Exception> {
        if !a.nan() {
            return self.propagate(b);
        }
        if !a.signalling() && b.nan() && b.signalling() {
            self.raise(Exception::InvalidOperation)?;
        }
        self.propagate(a)
    }

    /// How a NaN copied without arithmetic converts to the precision `to`.
    /// With the FPSR's NE bit clear, an extended NaN's lowest bit says which
    /// of single and double it stands for, and only a change between those
    /// signals.
    fn conversion(&self, nan: Internal, to: Precision) -> Conversion {
        let from = nan.nan_precision();
        if from == to {
            return Conversion::None;
        }
        if self.fpsr & fpsr::NE != 0 {
            return Conversion::Full;
        }
        // Exponent bit 10 marks extended, whose lowest mantissa bit says
        // which it stands for; below it, bit 9 marks double.
        let exponent = nan.exponent();
        let double = if exponent & 0x400 != 0 {
            nan.mantissa & 1 != 0
        } else {
            exponent & 0x200 != 0
        };
        let changes = match to {
            Precision::Single => double,
            Precision::Double => !double,
            Precision::Extended => false,
        };
        if changes {
            Conversion::Full
        } else {
            Conversion::Quiet
        }
    }

    /// A NaN copied to the precision `to`, as `MVF` and `STF` copy one.
    pub fn copy_nan(&mut self, nan: Internal, to: Precision) -> Result<Internal, Exception> {
        match self.conversion(nan, to) {
            Conversion::None => Ok(nan),
            Conversion::Quiet => Ok(convert_nan(nan, to)),
            Conversion::Full => {
                let mut nan = nan;
                if nan.signalling() {
                    self.raise(Exception::InvalidOperation)?;
                    nan.mantissa |= QUIET;
                }
                Ok(convert_nan(nan, to))
            }
        }
    }

    pub fn dyadic(
        &mut self,
        op: FpaDyadicOp,
        a: Internal,
        b: Internal,
    ) -> Result<Internal, Exception> {
        let value = match op {
            FpaDyadicOp::Adf | FpaDyadicOp::Suf | FpaDyadicOp::Rsf => self.add(op, a, b)?,
            FpaDyadicOp::Muf | FpaDyadicOp::Fml => self.multiply(op == FpaDyadicOp::Fml, a, b)?,
            FpaDyadicOp::Dvf | FpaDyadicOp::Fdv => self.divide(false, a, b)?,
            FpaDyadicOp::Rdf | FpaDyadicOp::Frd => self.divide(true, a, b)?,
            FpaDyadicOp::Rmf => self.remainder(a, b)?,
            FpaDyadicOp::Pow => self.power(a, b, false)?,
            FpaDyadicOp::Rpw => self.power(a, b, true)?,
            FpaDyadicOp::Pol => self.polar(a, b)?,
        };
        self.finish(value)
    }

    pub fn monadic(&mut self, op: FpaMonadicOp, a: Internal) -> Result<Internal, Exception> {
        let value = match op {
            FpaMonadicOp::Mvf | FpaMonadicOp::Mnf | FpaMonadicOp::Abs => self.move_(op, a)?,
            FpaMonadicOp::Rnd => self.round_integral(a)?,
            FpaMonadicOp::Sqt => self.square_root(a)?,
            FpaMonadicOp::Urd => self.unnormalised_round(a)?,
            FpaMonadicOp::Nrm => self.normalise(a)?,
            FpaMonadicOp::Log => self.transcendental(Function::Log, a)?,
            FpaMonadicOp::Lgn => self.transcendental(Function::Lgn, a)?,
            FpaMonadicOp::Exp => self.transcendental(Function::Exp, a)?,
            FpaMonadicOp::Sin => self.transcendental(Function::Sin, a)?,
            FpaMonadicOp::Cos => self.transcendental(Function::Cos, a)?,
            FpaMonadicOp::Tan => self.transcendental(Function::Tan, a)?,
            FpaMonadicOp::Asn => self.transcendental(Function::Asn, a)?,
            FpaMonadicOp::Acs => self.transcendental(Function::Acs, a)?,
            FpaMonadicOp::Atn => self.transcendental(Function::Atn, a)?,
        };
        self.finish(value)
    }

    fn add(&mut self, op: FpaDyadicOp, a: Internal, b: Internal) -> Result<Value, Exception> {
        let subtract = op != FpaDyadicOp::Adf;
        let reverse = op == FpaDyadicOp::Rsf;
        if a.special() || b.special() {
            if a.special() && b.special() {
                if !(a.infinite() && b.infinite()) {
                    return self.propagate_either(a, b);
                }
                if (b.negative() != subtract) == a.negative() {
                    return Ok(Value::Final(Internal::infinity(a.negative() != reverse)));
                }
                return self.invalid(
                    Reason::MagnitudeSubtractionOfInfinities,
                    self.magnitude_subtraction_sign(),
                );
            }
            if a.special() {
                if a.nan() {
                    return self.propagate(a);
                }
                return Ok(Value::Final(Internal::infinity(a.negative() != reverse)));
            }
            if b.nan() {
                return self.propagate(b);
            }
            return Ok(Value::Final(Internal::infinity(
                b.negative() ^ subtract ^ reverse,
            )));
        }
        let (mut x, mut y) = (a.number(), b.number());
        if reverse {
            x.negative = !x.negative;
        } else if subtract {
            y.negative = !y.negative;
        }
        let sum = exact::add(x.parts(), y.parts(), false);
        if sum.is_zero() {
            let negative = if x.negative == y.negative {
                x.negative
            } else {
                self.rounding == Rounding::MinusInfinity
            };
            return Ok(Value::Final(Internal::zero(negative)));
        }
        Ok(Value::Exact(sum))
    }

    /// The sign of `∞ − ∞`'s NaN: set when rounding toward minus infinity or
    /// zero at single or extended precision, as FPEmulator sets it.
    fn magnitude_subtraction_sign(&self) -> bool {
        matches!(self.rounding, Rounding::MinusInfinity | Rounding::Zero)
            && self.precision != Precision::Double
    }

    fn multiply(&mut self, fast: bool, a: Internal, b: Internal) -> Result<Value, Exception> {
        let negative = a.negative() != b.negative();
        if a.special() || b.special() {
            if a.special() && b.special() {
                if a.infinite() && b.infinite() {
                    return Ok(Value::Final(Internal::infinity(negative)));
                }
                return self.propagate_either(a, b);
            }
            if a.special() {
                if a.nan() {
                    return self.propagate(a);
                }
                if b.is_zero() {
                    return self.invalid(Reason::InfinityTimesZero, negative);
                }
                return Ok(Value::Final(Internal::infinity(negative)));
            }
            if b.nan() {
                return self.propagate(b);
            }
            if a.is_zero() {
                return self.invalid(Reason::ZeroTimesInfinity, negative);
            }
            return Ok(Value::Final(Internal::infinity(negative)));
        }
        if a.is_zero() || b.is_zero() {
            return Ok(Value::Final(Internal::zero(negative)));
        }
        let (mut x, mut y) = (a.number(), b.number());
        if fast {
            // FML multiplies the mantissas' high words alone.
            x.mantissa &= keep_mask(32);
            y.mantissa &= keep_mask(32);
        }
        Ok(Value::Exact(exact::multiply(x.parts(), y.parts())))
    }

    /// `a ÷ b`, or `b ÷ a` where `reverse`.
    fn divide(&mut self, reverse: bool, a: Internal, b: Internal) -> Result<Value, Exception> {
        let negative = a.negative() != b.negative();
        if a.special() || b.special() {
            if a.special() && b.special() {
                if a.infinite() && b.infinite() {
                    return self.invalid(Reason::InfinityDividedByInfinity, negative);
                }
                return self.propagate_either(a, b);
            }
            if a.special() {
                if a.nan() {
                    return self.propagate(a);
                }
                return Ok(Value::Final(if reverse {
                    Internal::zero(negative)
                } else {
                    Internal::infinity(negative)
                }));
            }
            if b.nan() {
                return self.propagate(b);
            }
            return Ok(Value::Final(if reverse {
                Internal::infinity(negative)
            } else {
                Internal::zero(negative)
            }));
        }
        let (dividend, divisor) = if reverse { (b, a) } else { (a, b) };
        if dividend.is_zero() && divisor.is_zero() {
            return self.invalid(Reason::ZeroDividedByZero, negative);
        }
        if dividend.is_zero() {
            return Ok(Value::Final(Internal::zero(negative)));
        }
        let (x, y) = (dividend.number(), divisor.number());
        if let Some(quotient) = exact::divide(x.parts(), y.parts()).filter(|_| !divisor.is_zero()) {
            return Ok(Value::Exact(quotient));
        }
        self.raise(Exception::DivisionByZero)?;
        Ok(Value::Final(Internal::infinity(negative)))
    }

    /// `RMF`: the IEEE remainder, `a − n × b` for the integer `n` nearest
    /// `a ÷ b`, ties to the even one.
    fn remainder(&mut self, a: Internal, b: Internal) -> Result<Value, Exception> {
        if a.special() || b.special() {
            if a.special() && b.special() {
                if a.infinite() && b.infinite() {
                    return self.invalid(Reason::RemainderOfInfinity, false);
                }
                return self.propagate_either(a, b);
            }
            if a.special() {
                if a.nan() {
                    return self.propagate(a);
                }
                return self.invalid(Reason::RemainderOfInfinity, false);
            }
            if b.nan() {
                return self.propagate(b);
            }
            return Ok(first_operand(a));
        }
        if b.is_zero() {
            return self.invalid(Reason::RemainderByZero, false);
        }
        if a.is_zero() {
            return Ok(Value::Final(Internal::zero(a.negative())));
        }
        let (x, y) = (a.number(), b.number());
        let rest = exact::remainder(x.parts(), y.parts()).rest;
        if rest.is_zero() {
            return Ok(Value::Final(Internal::zero(x.negative)));
        }
        Ok(Value::Exact(Exact::of(rest)))
    }

    fn square_root(&mut self, a: Internal) -> Result<Value, Exception> {
        if a.special() {
            if a.nan() {
                return self.propagate(a);
            }
            if a.negative() {
                return self.invalid(Reason::RootOfNegative, true);
            }
            return Ok(Value::Final(Internal::infinity(false)));
        }
        if a.is_zero() {
            return Ok(Value::Final(Internal::zero(a.negative())));
        }
        if a.negative() {
            return self.invalid(Reason::RootOfNegative, true);
        }
        Ok(Value::Exact(exact::square_root(a.number().parts())))
    }

    /// `MVF`, `MNF` and `ABS`: a copy to the precision, its sign changed.
    fn move_(&mut self, op: FpaMonadicOp, a: Internal) -> Result<Value, Exception> {
        let signed = |negative: bool| match op {
            FpaMonadicOp::Mnf => !negative,
            FpaMonadicOp::Abs => false,
            _ => negative,
        };
        if a.special() {
            if !a.nan() {
                return Ok(Value::Final(Internal::infinity(signed(a.negative()))));
            }
            let copy = self.copy_nan(a, self.precision)?;
            return Ok(Value::Final(copy.with_sign(signed(copy.negative()))));
        }
        if !a.uncommon() {
            let x = Number {
                negative: signed(a.negative()),
                ..a.number()
            };
            return Ok(Value::Exact(x.exact()));
        }
        if a.is_zero() {
            return Ok(Value::Final(Internal::zero(signed(a.negative()))));
        }
        if self.denormal_in_precision(a) {
            return Ok(Value::Final(a.with_sign(signed(a.negative()))));
        }
        let mut x = a.number();
        x.negative = signed(x.negative);
        Ok(Value::Exact(x.exact()))
    }

    /// Whether an uncommon numeric value is already a denormal of the
    /// precision, so a copy needs no conversion and cannot underflow.
    fn denormal_in_precision(&self, a: Internal) -> bool {
        let precision = if !a.units() {
            if a.exponent() != 0 {
                return false;
            }
            Precision::Extended
        } else if a.exponent() == 0x3F80 {
            Precision::Single
        } else {
            Precision::Double
        };
        precision == self.precision
    }

    /// `NRM`: the value normalised, a `URD` result taken as the unnormal it
    /// is.
    fn normalise(&mut self, a: Internal) -> Result<Value, Exception> {
        if a.special() {
            return self.standard_special(a);
        }
        if !a.uncommon() && a.units() {
            return Ok(Value::Exact(a.reduced().exact()));
        }
        if a.is_zero() {
            return Ok(Value::Final(Internal::zero(a.negative())));
        }
        Ok(Value::Exact(a.number().exact()))
    }

    /// A NaN propagated, or an infinity made the standard one.
    fn standard_special(&mut self, a: Internal) -> Result<Value, Exception> {
        if a.nan() {
            return self.propagate(a);
        }
        Ok(Value::Final(Internal::infinity(a.negative())))
    }

    /// `URD`: rounded to an integer at the precision's binary point and left
    /// unnormalised there, for `NRM` to finish.
    fn unnormalised_round(&mut self, a: Internal) -> Result<Value, Exception> {
        if a.special() {
            return self.standard_special(a);
        }
        let x = a.reduced();
        let point = BIAS + self.precision.bits().cast_signed() - 1;
        let exact = x.exact();
        Ok(Value::Exact(match u32::try_from(point - x.exponent) {
            Ok(shift) => exact.shifted_right(shift),
            Err(_) => exact,
        }))
    }

    /// `RND`: rounded to an integer in the instruction's mode, and where the
    /// precision's values lie further apart than one, to one of those as
    /// though straight from the operand: the integer's mantissa is handed on
    /// just short of it, on the operand's side.
    fn round_integral(&mut self, a: Internal) -> Result<Value, Exception> {
        if a.special() {
            return self.standard_special(a);
        }
        if a.is_zero() {
            return Ok(Value::Final(Internal::zero(a.negative())));
        }
        let x = a.number();
        let point = BIAS + 63;
        let Ok(places) = u32::try_from(point - x.exponent) else {
            return Ok(Value::Exact(x.exact()));
        };
        if places == 0 {
            return Ok(Value::Exact(x.exact()));
        }
        let up_away = match self.rounding {
            Rounding::Zero => Some(false),
            Rounding::PlusInfinity => Some(!x.negative),
            Rounding::MinusInfinity => Some(x.negative),
            Rounding::Nearest => None,
        };
        if places > 64 {
            // Below one half: zero, or one where rounding away.
            return Ok(Value::Exact(if up_away == Some(true) {
                pseudo(x.negative, BIAS - 1, u64::MAX, true)
            } else {
                pseudo(x.negative, 0, 0, false)
            }));
        }
        let fraction_mask = if places == 64 {
            u64::MAX
        } else {
            (1u64 << places) - 1
        };
        let fraction = x.mantissa & fraction_mask;
        if fraction == 0 {
            return Ok(Value::Exact(x.exact()));
        }
        let up = up_away.unwrap_or_else(|| {
            let half = 1u64 << (places - 1);
            fraction > half || (fraction == half && places < 64 && x.mantissa >> places & 1 != 0)
        });
        let whole = x.mantissa & !fraction_mask;
        Ok(Value::Exact(if up {
            pseudo(x.negative, x.exponent, whole | fraction_mask, true)
        } else if whole == 0 {
            pseudo(x.negative, 0, 0, false)
        } else {
            pseudo(x.negative, x.exponent, whole, false)
        }))
    }

    /// `FLT`: a signed integer at the precision.
    pub fn float(&mut self, value: u32) -> Result<Internal, Exception> {
        let negative = value & SIGN != 0;
        let magnitude = if negative {
            value.wrapping_neg()
        } else {
            value
        };
        if magnitude == 0 {
            return Ok(Internal::ZERO);
        }
        let x = Number {
            negative,
            exponent: BIAS + 31,
            mantissa: u64::from(magnitude) << 32,
        }
        .normalised();
        self.finish(Value::Exact(x.exact()))
    }

    /// `FIX`: rounded to a signed integer in the instruction's mode. An
    /// invalid one is `&7FFFFFFF` or `&80000000` by sign, and zero for a
    /// NaN.
    pub fn fix(&mut self, a: Internal) -> Result<u32, Exception> {
        let limit: u32 = if a.negative() {
            0x8000_0000
        } else {
            0x7FFF_FFFF
        };
        if a.special() {
            self.raise(Exception::InvalidOperation)?;
            return Ok(if a.nan() { 0 } else { limit });
        }
        let x = a.reduced();
        let shift = match u32::try_from(BIAS + 63 - x.exponent) {
            Ok(shift) if shift != 0 => shift,
            _ => {
                self.raise(Exception::InvalidOperation)?;
                return Ok(limit);
            }
        };
        // Shifted at least one place, the integer cannot carry out of 64 bits.
        let r = round(x.exact().shifted_right(shift), 64, self.rounding);
        if r.mantissa > u64::from(limit) {
            self.raise(Exception::InvalidOperation)?;
            return Ok(limit);
        }
        if r.direction != Direction::Exact {
            self.raise(Exception::Inexact)?;
        }
        let magnitude = u32::try_from(r.mantissa).unwrap_or(0);
        Ok(if a.negative() {
            magnitude.wrapping_neg()
        } else {
            magnitude
        })
    }

    /// A comparison's flags: N, Z, C and V in bits 31 to 28.
    pub fn compare(
        &mut self,
        op: FpaCompareOp,
        a: Internal,
        b: Internal,
    ) -> Result<u32, Exception> {
        const GREATER: u32 = 0x2000_0000;
        const EQUAL: u32 = 0x6000_0000;
        const LESS: u32 = 0x8000_0000;
        if a.nan() || b.nan() {
            let unordered = if self.fpsr & fpsr::AC != 0 {
                0x3000_0000
            } else {
                0x1000_0000
            };
            let signals = (a.nan() && a.signalling())
                || (b.nan() && b.signalling())
                || matches!(op, FpaCompareOp::Cmfe | FpaCompareOp::Cnfe);
            if signals {
                self.raise(Exception::InvalidOperation)?;
            }
            return Ok(unordered);
        }
        let negate = matches!(op, FpaCompareOp::Cnf | FpaCompareOp::Cnfe);
        let x = comparable(a, false);
        let y = comparable(b, negate);
        Ok(match x.cmp(&y) {
            core::cmp::Ordering::Less => LESS,
            core::cmp::Ordering::Equal => EQUAL,
            core::cmp::Ordering::Greater => GREATER,
        })
    }
}

/// A NaN in the precision `to`: its fraction cut to the precision and, for
/// single or double, its units bit set. Converted to extended, its lowest
/// bit is cleared from a single NaN and set from a double one, whatever the
/// NE bit.
pub(super) fn convert_nan(nan: Internal, to: Precision) -> Internal {
    let keep = nan.head & (SIGN | UNCOMMON);
    match to {
        Precision::Single => Internal {
            head: keep | Precision::Single.special_exponent(),
            mantissa: (nan.mantissa & keep_mask(24)) | UNITS,
        },
        Precision::Double => Internal {
            head: keep | Precision::Double.special_exponent(),
            mantissa: (nan.mantissa & keep_mask(53)) | UNITS,
        },
        Precision::Extended => {
            let mantissa = match nan.nan_precision() {
                Precision::Single => nan.mantissa & !1,
                Precision::Double => nan.mantissa | 1,
                Precision::Extended => nan.mantissa,
            };
            Internal {
                head: keep | EXPONENT,
                mantissa,
            }
        }
    }
}

/// The first operand of a remainder by infinity: itself, at its value.
fn first_operand(a: Internal) -> Value {
    if a.is_zero() {
        return Value::Final(Internal::zero(a.negative()));
    }
    Value::Exact(a.number().exact())
}

/// A mantissa with the round stage's view of what lies below it: just
/// above it when `up`, just below otherwise.
fn pseudo(negative: bool, exponent: i32, mantissa: u64, up: bool) -> Exact {
    let mut x = Number {
        negative,
        exponent,
        mantissa,
    }
    .exact();
    if up {
        x.sig |= (1 << 62) | 1;
    }
    x.sticky = true;
    x
}

/// A finite or infinite value as a comparison orders it: infinities above
/// every number, numbers by value, zeros of either sign equal.
fn comparable(a: Internal, negate: bool) -> Ordered {
    let negative = a.negative() != negate;
    if a.special() {
        return Ordered {
            negative,
            magnitude: (i64::MAX, 0),
        };
    }
    let x = a.number();
    if x.mantissa == 0 {
        return Ordered {
            negative: false,
            magnitude: (i64::MIN, 0),
        };
    }
    Ordered {
        negative,
        magnitude: (i64::from(x.exponent), x.mantissa),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Ordered {
    negative: bool,
    magnitude: (i64, u64),
}

impl PartialOrd for Ordered {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Ordered {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        let zero = (i64::MIN, 0);
        if self.magnitude == zero && other.magnitude == zero {
            return core::cmp::Ordering::Equal;
        }
        match (self.negative, other.negative) {
            (false, true) => core::cmp::Ordering::Greater,
            (true, false) => core::cmp::Ordering::Less,
            (false, false) => self.magnitude.cmp(&other.magnitude),
            (true, true) => other.magnitude.cmp(&self.magnitude),
        }
    }
}
