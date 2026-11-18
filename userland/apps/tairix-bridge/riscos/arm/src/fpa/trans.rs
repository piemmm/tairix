//! The transcendental operations. A result with a value is that value
//! correctly rounded, from `lib/softfloat`'s elementary functions; the cases
//! without one, the range limits, and the flags their special cases raise are
//! FPEmulator's, observed black-box.

use tairix_softfloat::elementary;
use tairix_softfloat::exact::{Exact, Parts};

use super::arith::{beyond_range, Reason, Unit, Value};
use super::internal::{Internal, Number, BIAS, UNITS};
use super::Exception;

/// A monadic transcendental function.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Function {
    Log,
    Lgn,
    Exp,
    Sin,
    Cos,
    Tan,
    Asn,
    Acs,
    Atn,
}

/// The least magnitude SIN refuses: (2^31 − ½)π, rounded up.
const SIN_LIMIT: Number = limit(0x401F, 0xC90F_DAA1_5858_E793);
/// The least magnitude COS refuses: (2^31 − 1)π, rounded up.
const COS_LIMIT: Number = limit(0x401F, 0xC90F_DAA0_8F49_0CF1);
/// The least positive argument TAN refuses: (2^31 − ½)π/2, rounded up.
const TAN_LIMIT: Number = limit(0x401E, 0xC90F_DAA1_5858_E793);
/// The least magnitude TAN refuses below zero, one place past
/// (2^31 + ½)π/2 rounded up.
const TAN_NEGATIVE_LIMIT: Number = limit(0x401E, 0xC90F_DAA2_EA78_9CD8);

/// Integer powers below this magnitude are taken as FPEmulator takes them
/// by repeated multiplication: an infinity's overflows, and a negative
/// number's odd one is rounded as though bits lay below it.
const MULTIPLIED_POWERS: i32 = BIAS + 16;

const ONE: Parts = Parts {
    negative: false,
    exp: 0,
    sig: UNITS,
};

const fn limit(exponent: i32, mantissa: u64) -> Number {
    Number {
        negative: false,
        exponent,
        mantissa,
    }
}

/// Whether `x`'s magnitude reaches `limit`'s.
fn reaches(x: Number, limit: Number) -> bool {
    (x.exponent, x.mantissa) >= (limit.exponent, limit.mantissa)
}

const fn is_one(x: Number) -> bool {
    x.exponent == BIAS && x.mantissa == UNITS
}

/// Whether `x`'s magnitude exceeds one.
fn beyond_one(x: Number) -> bool {
    (x.exponent, x.mantissa) > (BIAS, UNITS)
}

/// Whether a finite nonzero `x` is an integer, and if so whether it is odd.
fn parity(x: Number) -> Option<bool> {
    let point = BIAS + 63;
    if x.exponent > point {
        return Some(false);
    }
    let fraction_bits = u32::try_from(point - x.exponent).ok()?;
    if fraction_bits >= 64 {
        return None;
    }
    let fraction = x.mantissa & ((1u64 << fraction_bits) - 1);
    (fraction == 0).then_some(x.mantissa >> fraction_bits & 1 != 0)
}

fn signed(x: Exact, negative: bool) -> Value {
    Value::Exact(Exact { negative, ..x })
}

/// π × `quarters` / 4, signed.
fn quarter_turns(quarters: u32, negative: bool) -> Value {
    signed(elementary::quarter_pis(quarters), negative)
}

const fn one() -> Value {
    Value::Final(Internal::constant(1))
}

const fn zero(negative: bool) -> Value {
    Value::Final(Internal::zero(negative))
}

const fn infinity(negative: bool) -> Value {
    Value::Final(Internal::infinity(negative))
}

impl Unit {
    pub(super) fn transcendental(
        &mut self,
        function: Function,
        a: Internal,
    ) -> Result<Value, Exception> {
        if a.nan() {
            return self.propagate(a);
        }
        if a.infinite() {
            return self.at_infinity(function, a.negative());
        }
        let x = a.number();
        if x.mantissa == 0 {
            return self.at_zero(function, x.negative);
        }
        let p = x.parts();
        Ok(Value::Exact(match function {
            Function::Log | Function::Lgn => {
                if x.negative {
                    return self.invalid(Reason::LogOfNegative, false);
                }
                if is_one(x) {
                    return Ok(zero(false));
                }
                if function == Function::Log {
                    elementary::log10(p)
                } else {
                    elementary::ln(p)
                }
            }
            Function::Exp => elementary::exp(p),
            Function::Sin => {
                if reaches(x, SIN_LIMIT) {
                    return self.invalid(Reason::SinCosRange, false);
                }
                elementary::sin(p)
            }
            Function::Cos => {
                if reaches(x, COS_LIMIT) {
                    return self.invalid(Reason::SinCosRange, false);
                }
                elementary::cos(p)
            }
            Function::Tan => {
                let limit = if x.negative {
                    TAN_NEGATIVE_LIMIT
                } else {
                    TAN_LIMIT
                };
                if reaches(x, limit) {
                    return self.invalid(Reason::TanRange, x.negative);
                }
                elementary::tan(p)
            }
            Function::Asn | Function::Acs => {
                if beyond_one(x) {
                    return self.invalid(Reason::AsnAcsRange, function == Function::Asn);
                }
                if function == Function::Asn {
                    elementary::asin(p)
                } else if is_one(x) && !x.negative {
                    return Ok(zero(false));
                } else {
                    elementary::acos(p)
                }
            }
            Function::Atn => elementary::atan2(p, ONE),
        }))
    }

    fn at_infinity(&mut self, function: Function, negative: bool) -> Result<Value, Exception> {
        match function {
            Function::Log | Function::Lgn if negative => self.invalid(Reason::LogOfNegative, false),
            Function::Exp if negative => Ok(zero(false)),
            Function::Log | Function::Lgn | Function::Exp => Ok(infinity(false)),
            Function::Sin | Function::Cos => self.invalid(Reason::SinCosInfinity, false),
            Function::Tan => self.invalid(Reason::TanInfinity, false),
            Function::Asn | Function::Acs => self.invalid(Reason::AsnAcsInfinity, false),
            Function::Atn => Ok(quarter_turns(2, negative)),
        }
    }

    fn at_zero(&mut self, function: Function, negative: bool) -> Result<Value, Exception> {
        Ok(match function {
            Function::Log | Function::Lgn => {
                self.raise(Exception::DivisionByZero)?;
                infinity(true)
            }
            Function::Exp | Function::Cos => one(),
            Function::Acs => quarter_turns(2, false),
            Function::Sin | Function::Tan | Function::Asn | Function::Atn => zero(negative),
        })
    }

    /// `POW`, `a` to the power `b`, or `RPW`, `b` to the power `a`, where
    /// `reverse`. A NaN in `a` is the one propagated either way.
    pub(super) fn power(
        &mut self,
        a: Internal,
        b: Internal,
        reverse: bool,
    ) -> Result<Value, Exception> {
        if a.nan() || b.nan() {
            return self.propagate_either(a, b);
        }
        let (base, exponent) = if reverse { (b, a) } else { (a, b) };
        self.raised(base, exponent)
    }

    fn raised(&mut self, a: Internal, b: Internal) -> Result<Value, Exception> {
        let y = b.number();
        if !b.infinite() && y.mantissa == 0 {
            return Ok(one());
        }
        let negative_exponent = b.negative();
        // An infinite exponent is no integer.
        let parity = if b.infinite() { None } else { parity(y) };
        let multiplied = parity.is_some() && y.exponent < MULTIPLIED_POWERS;
        if a.infinite() {
            if a.negative() && parity.is_none() {
                return self.invalid(Reason::NegativeToPower, false);
            }
            let negative = a.negative() && parity == Some(true);
            if negative_exponent {
                if negative && multiplied {
                    self.raise(Exception::Inexact)?;
                }
                return Ok(zero(negative));
            }
            return Ok(if multiplied {
                beyond_range(negative, true)
            } else {
                infinity(negative)
            });
        }
        let x = a.number();
        if x.mantissa == 0 {
            if negative_exponent {
                self.raise(Exception::DivisionByZero)?;
                return Ok(infinity(x.negative && parity == Some(true)));
            }
            return Ok(zero(x.negative && parity != Some(false)));
        }
        if is_one(x) {
            if b.infinite() {
                let reason = if x.negative {
                    Reason::NegativeToPower
                } else {
                    Reason::BadInfinitePower
                };
                return self.invalid(reason, false);
            }
            return match (x.negative, parity) {
                (false, _) | (true, Some(false)) => Ok(one()),
                (true, Some(true)) => Ok(Value::Final(Internal::constant(1).with_sign(true))),
                (true, None) => self.invalid(Reason::NegativeToPower, false),
            };
        }
        if x.negative && parity.is_none() {
            return self.invalid(Reason::NegativeToPower, false);
        }
        if b.infinite() {
            return Ok(if beyond_one(x) == negative_exponent {
                zero(false)
            } else {
                infinity(false)
            });
        }
        let magnitude = Number {
            negative: false,
            ..x
        };
        let power = elementary::pow(magnitude.parts(), y.parts());
        let odd = x.negative && parity == Some(true);
        Ok(Value::Exact(Exact {
            negative: odd,
            sticky: power.sticky || odd && multiplied,
            ..power
        }))
    }

    /// `POL`: the angle of the point (`x`, `y`) over the whole circle.
    pub(super) fn polar(&mut self, x: Internal, y: Internal) -> Result<Value, Exception> {
        if x.nan() || y.nan() {
            return self.propagate_either(x, y);
        }
        let below = y.negative();
        match (x.infinite(), y.infinite()) {
            (true, true) => return self.invalid(Reason::PolarOfInfinities, false),
            (false, true) => return Ok(quarter_turns(2, below)),
            (true, false) if x.negative() => return Ok(quarter_turns(4, below)),
            (true, false) => {
                // FPEmulator flags the zero of a nonzero y inexact.
                if !y.is_zero() {
                    self.raise(Exception::Inexact)?;
                }
                return Ok(zero(below));
            }
            (false, false) => {}
        }
        let (p, q) = (x.number(), y.number());
        Ok(match (p.mantissa == 0, q.mantissa == 0) {
            (true, true) => return self.invalid(Reason::PolarOfZeros, false),
            (true, false) => quarter_turns(2, below),
            (false, true) if p.negative => quarter_turns(4, below),
            (false, true) => zero(below),
            (false, false) => Value::Exact(elementary::atan2(q.parts(), p.parts())),
        })
    }
}
