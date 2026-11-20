//! BASIC V's functions of a real.
//!
//! The transcendentals are rounded correctly, where BASIC V's own are a place
//! or two off; the replay holds them to BASIC V's measured error. The rest
//! is BASIC V's exactly: the radian and degree conversions by its own
//! constants, a power of a small integer by squares, and every bound at which
//! a function refuses its argument.

use core::ops::RangeInclusive;

use tairix_softfloat::elementary;
use tairix_softfloat::exact::Parts;

use super::Five;
use crate::Fault;

/// One as `lib/softfloat`'s parts.
const ONE: Parts = Parts {
    negative: false,
    exp: 0,
    sig: 1 << 63,
};

/// The integer exponents BASIC V takes a power by squares for; any other
/// takes the real power's way.
const SQUARED: RangeInclusive<i32> = -64..=63;

impl Five {
    /// π, as BASIC V's `PI` holds it.
    pub const PI: Self = Self::from_bytes([0xA2, 0xDA, 0x0F, 0x49, 0x82]);

    /// `PI` / 180 in BASIC V's arithmetic, a place below π/180 rounded.
    const RADIAN: Self = Self::from_bytes([0x12, 0x35, 0xFA, 0x0E, 0x7B]);

    /// 180 / `PI`.
    const DEGREE: Self = Self::from_bytes([0xD3, 0xE0, 0x2E, 0x65, 0x86]);

    /// The least magnitude a sine, cosine or tangent refuses: 2^23.
    const REDUCIBLE: Self = Self::from_bytes([0, 0, 0, 0, 0x98]);

    /// The least argument `EXP` refuses: 89.5.
    const EXPONENTIAL: Self = Self::from_bytes([0, 0, 0, 0x33, 0x87]);

    /// The least magnitude of a real exponent not taken in two parts: 64.
    const WHOLE_EXPONENT: Self = Self::from_bytes([0, 0, 0, 0, 0x87]);

    /// `SIN`.
    ///
    /// # Errors
    ///
    /// [`Fault::AccuracyLost`] for a magnitude of 2^23 or more.
    pub fn sin(self) -> Result<Self, Fault> {
        self.reducible()?
            .parts()
            .map_or(Ok(Self::ZERO), |x| Self::from_exact(elementary::sin(x)))
    }

    /// `COS`.
    ///
    /// # Errors
    ///
    /// [`Fault::AccuracyLost`] for a magnitude of 2^23 or more.
    pub fn cos(self) -> Result<Self, Fault> {
        self.reducible()?
            .parts()
            .map_or(Ok(Self::ONE), |x| Self::from_exact(elementary::cos(x)))
    }

    /// `TAN`.
    ///
    /// # Errors
    ///
    /// [`Fault::AccuracyLost`] for a magnitude of 2^23 or more.
    pub fn tan(self) -> Result<Self, Fault> {
        self.reducible()?
            .parts()
            .map_or(Ok(Self::ZERO), |x| Self::from_exact(elementary::tan(x)))
    }

    /// `ATN`.
    ///
    /// # Errors
    ///
    /// None arises: the result lies within ±π/2.
    pub fn atn(self) -> Result<Self, Fault> {
        self.parts().map_or(Ok(Self::ZERO), |x| {
            Self::from_exact(elementary::atan2(x, ONE))
        })
    }

    /// `ASN`.
    ///
    /// # Errors
    ///
    /// [`Fault::ArcRange`] for a magnitude above one.
    pub fn asn(self) -> Result<Self, Fault> {
        self.within_one()?
            .parts()
            .map_or(Ok(Self::ZERO), |x| Self::from_exact(elementary::asin(x)))
    }

    /// `ACS`.
    ///
    /// # Errors
    ///
    /// [`Fault::ArcRange`] for a magnitude above one.
    pub fn acs(self) -> Result<Self, Fault> {
        let x = self.within_one()?;
        if x == Self::ONE {
            return Ok(Self::ZERO);
        }
        match x.parts() {
            Some(x) => Self::from_exact(elementary::acos(x)),
            None => Self::from_exact(elementary::quarter_pis(2)),
        }
    }

    /// `EXP`: zero where the result falls below the least value.
    ///
    /// # Errors
    ///
    /// [`Fault::ExponentRange`] for an argument of 89.5 or more,
    /// [`Fault::TooBig`] for a result past the largest value below it.
    pub fn exp(self) -> Result<Self, Fault> {
        if self >= Self::EXPONENTIAL {
            return Err(Fault::ExponentRange);
        }
        self.parts()
            .map_or(Ok(Self::ONE), |x| Self::from_exact(elementary::exp(x)))
    }

    /// `LN`.
    ///
    /// # Errors
    ///
    /// [`Fault::LogRange`] for an argument not above zero.
    pub fn ln(self) -> Result<Self, Fault> {
        self.logarithm(elementary::ln)
    }

    /// `LOG`.
    ///
    /// # Errors
    ///
    /// [`Fault::LogRange`] for an argument not above zero.
    pub fn log(self) -> Result<Self, Fault> {
        self.logarithm(elementary::log10)
    }

    /// `RAD`.
    ///
    /// # Errors
    ///
    /// None arises: the factor is below one.
    pub fn rad(self) -> Result<Self, Fault> {
        self.mul(Self::RADIAN)
    }

    /// `DEG`.
    ///
    /// # Errors
    ///
    /// [`Fault::TooBig`] for a result past the largest value.
    pub fn deg(self) -> Result<Self, Fault> {
        self.mul(Self::DEGREE)
    }

    /// The value to the integer power `n`: from −64 to 63 by squares, the
    /// square of each square kept, multiplied from the highest set bit of
    /// `n` down, after the reciprocal for a negative `n`; any other `n` as
    /// [`Self::pow`] takes it.
    ///
    /// # Errors
    ///
    /// [`Fault::DivisionByZero`] for zero to a negative power, and as
    /// [`Self::pow`] has them past the squares' range.
    pub fn pow_int(self, n: i32) -> Result<Self, Fault> {
        if !SQUARED.contains(&n) {
            return self.real_power(Self::from_int(n));
        }
        let base = if n < 0 { Self::ONE.div(self)? } else { self };
        base.squared(n.unsigned_abs())
    }

    /// The value to the power `y`: an integer below 64 in magnitude as
    /// [`Self::pow_int`] takes it, any other correctly rounded.
    ///
    /// # Errors
    ///
    /// Below 64 in magnitude, `y`'s integer part first, as
    /// [`Self::pow_int`] has them, then [`Fault::LogRange`] for a base not
    /// above zero. Past it, [`Fault::LogRange`] for a base not above zero,
    /// [`Fault::ExponentRange`] where `y` times the base's `LN` reaches 89.5,
    /// and [`Fault::TooBig`] for a result past the largest value.
    pub fn pow(self, y: Self) -> Result<Self, Fault> {
        match y.small_integer() {
            Some(n) => self.pow_int(n),
            None => self.real_power(y),
        }
    }

    fn real_power(self, y: Self) -> Result<Self, Fault> {
        if y.magnitude() < Self::WHOLE_EXPONENT {
            // BASIC V takes the integer part by squares first, which
            // decides the faults its overflow and a zero base meet.
            self.pow_int(y.floor()?)?;
            if self.is_negative() || self.is_zero() {
                return Err(Fault::LogRange);
            }
        } else if self.y_ln_reaches(y)? {
            return Err(Fault::ExponentRange);
        }
        if self == Self::ONE {
            return Ok(Self::ONE);
        }
        match (self.parts(), y.parts()) {
            (Some(x), Some(y)) => Self::from_exact(elementary::pow(x, y)),
            _ => Ok(Self::ONE),
        }
    }

    /// Whether `y` times the value's `LN` reaches `EXP`'s bound.
    fn y_ln_reaches(self, y: Self) -> Result<bool, Fault> {
        Ok(y.mul(self.ln()?)? >= Self::EXPONENTIAL)
    }

    /// The value as an integer, where it is one below 64 in magnitude.
    fn small_integer(self) -> Option<i32> {
        if self.magnitude() >= Self::WHOLE_EXPONENT {
            return None;
        }
        let n = self.to_int().ok()?;
        (Self::from_int(n) == self).then_some(n)
    }

    /// The value to the power `n`, at most 64, by squares.
    fn squared(self, n: u32) -> Result<Self, Fault> {
        // 2^6 is the highest bit a power taken by squares has.
        let mut squares = [self; 7];
        let Some(top) = n
            .checked_ilog2()
            .and_then(|top| usize::try_from(top).ok())
            .map(|top| top.min(squares.len() - 1))
        else {
            return Ok(Self::ONE);
        };
        for at in 1..=top {
            squares[at] = squares[at - 1].mul(squares[at - 1])?;
        }
        let mut power = squares[top];
        for at in (0..top).rev() {
            if n >> at & 1 != 0 {
                power = power.mul(squares[at])?;
            }
        }
        Ok(power)
    }

    fn reducible(self) -> Result<Self, Fault> {
        if self.magnitude() >= Self::REDUCIBLE {
            return Err(Fault::AccuracyLost);
        }
        Ok(self)
    }

    fn within_one(self) -> Result<Self, Fault> {
        if self.magnitude() > Self::ONE {
            return Err(Fault::ArcRange);
        }
        Ok(self)
    }

    fn logarithm(self, f: fn(Parts) -> tairix_softfloat::Exact) -> Result<Self, Fault> {
        if self.is_negative() || self.is_zero() {
            return Err(Fault::LogRange);
        }
        if self == Self::ONE {
            return Ok(Self::ZERO);
        }
        self.parts()
            .map_or(Ok(Self::ZERO), |x| Self::from_exact(f(x)))
    }
}

#[cfg(test)]
mod tests;
