//! BASIC V's real: a normalised 32-bit mantissa, an excess-128 exponent and
//! a sign, kept in memory as five bytes, with the arithmetic RISC OS 5's
//! BASIC V does on it.
//!
//! In memory the mantissa comes first, little-endian, its top bit — always
//! set in a normalised mantissa — standing for the sign; the exponent byte
//! follows. Only the all-zero pattern is zero: an exponent byte of zero with
//! any other mantissa is a number.
//!
//! Each operation rounds its exact result once, to nearest with ties to odd,
//! but for a subtraction whose result keeps the larger operand's exponent,
//! which BASIC V truncates. Addition and subtraction ignore an operand more
//! than 32 places below the other. A result past exponent 255 is too big; one
//! below exponent 0 is zero.
//!
//! Its transcendental functions are rounded correctly, where BASIC V's are a
//! place or two off; its conversions, integer powers and every function's
//! refusals are BASIC V's exactly.

use core::cmp::Ordering;

use tairix_softfloat::decimal::{exact_digits, Beyond, Decimal};
use tairix_softfloat::exact::{Exact, Parts, POINT};
use tairix_softfloat::Mode;

use crate::text::{Digits, Written};
use crate::Fault;

/// The bytes a value takes in memory.
pub const BYTES: usize = 5;

/// The most exponent digits BASIC V reads.
pub const EXPONENT_DIGITS: usize = 2;

/// The mantissa's width, and the places below it an operand still reaches.
const MANTISSA_BITS: u32 = 32;
/// A mantissa's top bit, set in every number.
const TOP: u32 = 1 << 31;
/// Half the last place, as the fraction below a kept mantissa.
const HALF: u32 = 1 << 31;
/// The exponent byte of a value in `[0.5, 1)`.
const BIAS: i32 = 128;

/// A BASIC V real.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Five {
    negative: bool,
    exponent: u8,
    /// [`TOP`] set, but for zero's, which is zero.
    mantissa: u32,
}

/// How an exact result is cut to the mantissa.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Cut {
    /// To nearest, ties to odd.
    NearestOdd,
    /// Toward zero.
    Truncate,
}

impl Five {
    /// Zero.
    pub const ZERO: Self = Self {
        negative: false,
        exponent: 0,
        mantissa: 0,
    };

    /// One.
    pub const ONE: Self = Self {
        negative: false,
        exponent: 0x81,
        mantissa: TOP,
    };

    /// 2^31, the least magnitude `INT` refuses.
    const INTEGER_LIMIT: Self = Self {
        negative: false,
        exponent: 0xA0,
        mantissa: TOP,
    };

    /// The value `bytes` hold in memory.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; BYTES]) -> Self {
        let [a, b, c, d, exponent] = bytes;
        let word = u32::from_le_bytes([a, b, c, d]);
        if word == 0 && exponent == 0 {
            return Self::ZERO;
        }
        Self {
            negative: word & TOP != 0,
            exponent,
            mantissa: word | TOP,
        }
    }

    /// The bytes the value is kept as in memory. A value whose bytes would
    /// be all zero but for a sign it lacks — half of the least exponent —
    /// reads back as zero, as it does on BASIC V.
    #[must_use]
    pub const fn to_bytes(self) -> [u8; BYTES] {
        if self.mantissa == 0 {
            return [0; BYTES];
        }
        let sign = if self.negative { TOP } else { 0 };
        let [a, b, c, d] = ((self.mantissa & !TOP) | sign).to_le_bytes();
        [a, b, c, d, self.exponent]
    }

    /// The value of the double whose bits are `bits` as BASIC V reads one
    /// from a file, as recorded: its mantissa cut to 32 bits, a magnitude
    /// past the range the largest, one below its least exponent nought.
    #[must_use]
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the exponent is checked to 1..=0xFF, and the mantissa is the top 32 of 53 bits"
    )]
    pub const fn from_double_bits(bits: u64) -> Self {
        let negative = bits >> 63 != 0;
        let biased = ((bits >> 52) & 0x7FF) as i32;
        if biased == 0 {
            return Self::ZERO;
        }
        let exponent = biased - 1022 + 0x80;
        if exponent <= 0 {
            return Self::ZERO;
        }
        if exponent > 0xFF || biased == 0x7FF {
            return Self {
                negative,
                exponent: 0xFF,
                mantissa: u32::MAX,
            };
        }
        let significand = 1 << 52 | (bits & ((1 << 52) - 1));
        Self {
            negative,
            exponent: exponent as u8,
            mantissa: (significand >> 21) as u32,
        }
    }

    /// The value as a double's bits, exactly: BASIC VI's reading of BASIC
    /// V's real from a file.
    #[must_use]
    pub const fn to_double_bits(self) -> u64 {
        if self.mantissa == 0 {
            return 0;
        }
        let sign = if self.negative { 1 << 63 } else { 0 };
        let biased = self.exponent as u64 + 1022 - 0x80;
        let fraction = ((self.mantissa & !TOP) as u64) << 21;
        sign | biased << 52 | fraction
    }

    /// Whether the value is zero.
    #[must_use]
    pub const fn is_zero(self) -> bool {
        self.mantissa == 0
    }

    /// Whether the value is below zero.
    #[must_use]
    pub const fn is_negative(self) -> bool {
        self.negative
    }

    /// The value with its sign changed; zero stays zero.
    #[must_use]
    pub const fn negated(self) -> Self {
        if self.is_zero() {
            return self;
        }
        Self {
            negative: !self.negative,
            ..self
        }
    }

    /// The value without its sign.
    #[must_use]
    pub const fn magnitude(self) -> Self {
        Self {
            negative: false,
            ..self
        }
    }

    /// `n`, exactly: every integer has a mantissa wide enough.
    #[must_use]
    pub const fn from_int(n: i32) -> Self {
        let magnitude = n.unsigned_abs();
        if magnitude == 0 {
            return Self::ZERO;
        }
        let shift = magnitude.leading_zeros();
        // Between 129 and 160, so the low byte is the whole exponent.
        let [exponent, ..] = (0x80 + MANTISSA_BITS - shift).to_le_bytes();
        Self {
            negative: n < 0,
            exponent,
            mantissa: magnitude << shift,
        }
    }

    /// `n`, rounded to nearest with ties to odd, as BASIC V makes an integer
    /// product that overflows an integer a real.
    #[must_use]
    pub fn from_i64(n: i64) -> Self {
        let magnitude = n.unsigned_abs();
        if magnitude == 0 {
            return Self::ZERO;
        }
        let shift = magnitude.leading_zeros();
        let exponent = BIAS + 64 - to_i32(shift);
        // Every 64-bit magnitude lies far inside the exponent's range.
        cut(n < 0, exponent, magnitude << shift, false, Cut::NearestOdd).unwrap_or(Self::ZERO)
    }

    /// The square root, rounded to nearest: a square root never lies on a
    /// tie.
    ///
    /// # Errors
    ///
    /// [`Fault::NegativeRoot`] for a value below zero.
    pub fn sqrt(self) -> Result<Self, Fault> {
        if self.is_zero() {
            return Ok(Self::ZERO);
        }
        if self.negative {
            return Err(Fault::NegativeRoot);
        }
        // An odd exponent lends the mantissa a bit so the root's exponent
        // halves exactly: the root then fills 64 bits, else 63.
        let unbiased = i32::from(self.exponent) - BIAS;
        let odd = unbiased.rem_euclid(2) != 0;
        let radicand = u128::from(self.mantissa) << (94 + u32::from(odd));
        let root = isqrt(radicand);
        let sticky = root * root != radicand;
        let root = low_u64(root);
        let wide = if odd { root } else { root << 1 };
        cut(
            false,
            unbiased.div_euclid(2) + BIAS + i32::from(odd),
            wide,
            sticky,
            Cut::NearestOdd,
        )
    }

    /// The value as an integer, truncated toward zero, as BASIC V assigns a
    /// real to an integer variable.
    ///
    /// # Errors
    ///
    /// [`Fault::TooBig`] where the truncated value is not a 32-bit integer.
    pub fn to_int(self) -> Result<i32, Fault> {
        let (whole, _) = self.whole()?;
        int_of(self.negative, whole)
    }

    /// The greatest integer not above the value: BASIC V's `INT`.
    ///
    /// # Errors
    ///
    /// [`Fault::TooBig`] for a magnitude of 2^31 or more, −2^31 among
    /// them, or a result past a 32-bit integer.
    pub fn floor(self) -> Result<i32, Fault> {
        if self.magnitude() >= Self::INTEGER_LIMIT {
            return Err(Fault::TooBig);
        }
        let (whole, inexact) = self.whole()?;
        let whole = if self.negative && inexact {
            whole.checked_add(1).ok_or(Fault::TooBig)?
        } else {
            whole
        };
        int_of(self.negative, whole)
    }

    /// The magnitude's whole part, and whether a fraction was dropped.
    fn whole(self) -> Result<(u64, bool), Fault> {
        if self.is_zero() {
            return Ok((0, false));
        }
        let unbiased = i32::from(self.exponent) - BIAS;
        if unbiased <= 0 {
            return Ok((0, true));
        }
        // Past 32 places the magnitude is at least 2^32, beyond any integer.
        let places = u32::try_from(unbiased).map_err(|_| Fault::TooBig)?;
        if places > MANTISSA_BITS {
            return Err(Fault::TooBig);
        }
        let scaled = u64::from(self.mantissa) << places;
        Ok((scaled >> MANTISSA_BITS, scaled & u64::from(u32::MAX) != 0))
    }

    /// The number `number` writes, rounded to nearest with ties to odd from
    /// every digit it has, where BASIC V, keeping a little less than every
    /// bit, reads a value at or just above a tie a place low.
    ///
    /// # Errors
    ///
    /// [`Fault::TooBig`] for a number past the largest value.
    pub fn read(number: &Written<'_>) -> Result<Self, Fault> {
        match exact_digits(number.negative, number.digits(), number.exponent) {
            Ok(x) => Self::from_exact(x),
            Err(Beyond::Above) => Err(Fault::TooBig),
            Err(Beyond::Below) => Ok(Self::ZERO),
        }
    }

    fn from_exact(x: Exact) -> Result<Self, Fault> {
        let x = x.normalized(true);
        if x.sig == 0 {
            return Ok(Self::ZERO);
        }
        // The significand's top bit is bit POINT: its top 64 bits are kept.
        let shift = POINT - 63;
        let wide = low_u64(x.sig >> shift);
        let sticky = x.sticky || x.sig & ((1 << shift) - 1) != 0;
        cut(
            x.negative,
            x.exp.saturating_add(BIAS + 1),
            wide,
            sticky,
            Cut::NearestOdd,
        )
    }

    fn parts(self) -> Option<Parts> {
        (!self.is_zero()).then(|| Parts {
            negative: self.negative,
            exp: i32::from(self.exponent) - BIAS - 1,
            sig: u64::from(self.mantissa) << MANTISSA_BITS,
        })
    }

    /// The exponent and mantissa, which order magnitudes.
    const fn order_of_magnitude(self) -> (u8, u32) {
        (self.exponent, self.mantissa)
    }

    /// The value's place in the order of every value.
    fn signed_order(self) -> (bool, (u8, u32)) {
        (!self.negative, self.order_of_magnitude())
    }
}

// Fallible: a result past the range is BASIC's error, which the operator
// traits have no way to report.
#[allow(clippy::should_implement_trait)]
impl Five {
    /// The sum of the value and `other`.
    ///
    /// # Errors
    ///
    /// [`Fault::TooBig`] for a sum past the largest value.
    pub fn add(self, other: Self) -> Result<Self, Fault> {
        if self.is_zero() {
            return Ok(other);
        }
        if other.is_zero() {
            return Ok(self);
        }
        let (big, small) = if self.order_of_magnitude() >= other.order_of_magnitude() {
            (self, other)
        } else {
            (other, self)
        };
        let gap = u32::from(big.exponent - small.exponent);
        if gap > MANTISSA_BITS {
            return Ok(big);
        }
        let high = u128::from(big.mantissa) << MANTISSA_BITS;
        let low = u128::from(small.mantissa) << (MANTISSA_BITS - gap);
        let exponent = i32::from(big.exponent);
        if big.negative == small.negative {
            let sum = high + low;
            let carried = sum >> 64 != 0;
            let wide = if carried { sum >> 1 } else { sum };
            return cut(
                big.negative,
                exponent + i32::from(carried),
                low_u64(wide),
                carried && sum & 1 != 0,
                Cut::NearestOdd,
            );
        }
        let difference = low_u64(high - low);
        if difference == 0 {
            return Ok(Self::ZERO);
        }
        let shift = difference.leading_zeros();
        let rule = if shift == 0 {
            Cut::Truncate
        } else {
            Cut::NearestOdd
        };
        cut(
            big.negative,
            exponent - to_i32(shift),
            difference << shift,
            false,
            rule,
        )
    }

    /// The value less `other`.
    ///
    /// # Errors
    ///
    /// [`Fault::TooBig`] for a difference past the largest value.
    pub fn sub(self, other: Self) -> Result<Self, Fault> {
        self.add(other.negated())
    }

    /// The product of the value and `other`.
    ///
    /// # Errors
    ///
    /// [`Fault::TooBig`] for a product past the largest value.
    pub fn mul(self, other: Self) -> Result<Self, Fault> {
        if self.is_zero() || other.is_zero() {
            return Ok(Self::ZERO);
        }
        let product = u64::from(self.mantissa) * u64::from(other.mantissa);
        let short = product >> 63 == 0;
        let exponent =
            i32::from(self.exponent) + i32::from(other.exponent) - BIAS - i32::from(short);
        let wide = if short { product << 1 } else { product };
        cut(
            self.negative != other.negative,
            exponent,
            wide,
            false,
            Cut::NearestOdd,
        )
    }

    /// The value divided by `other`.
    ///
    /// # Errors
    ///
    /// [`Fault::DivisionByZero`] where `other` is zero, [`Fault::TooBig`]
    /// for a quotient past the largest value.
    pub fn div(self, other: Self) -> Result<Self, Fault> {
        if other.is_zero() {
            return Err(Fault::DivisionByZero);
        }
        if self.is_zero() {
            return Ok(Self::ZERO);
        }
        let dividend = u128::from(self.mantissa) << 64;
        let divisor = u128::from(other.mantissa);
        let quotient = dividend / divisor;
        let mut sticky = dividend % divisor != 0;
        let mut exponent = i32::from(self.exponent) - i32::from(other.exponent) + BIAS;
        let wide = if quotient >> 64 != 0 {
            sticky |= quotient & 1 != 0;
            exponent += 1;
            low_u64(quotient >> 1)
        } else {
            low_u64(quotient)
        };
        cut(
            self.negative != other.negative,
            exponent,
            wide,
            sticky,
            Cut::NearestOdd,
        )
    }
}

impl Digits for Five {
    const MOST: u8 = 11;
    const EXPONENT_FIELD: usize = 3;

    fn negative(&self) -> bool {
        self.negative
    }

    fn rounded(&self, count: u32) -> Option<(u128, i32)> {
        let (decimal, _) = Decimal::of(self.parts()?, count, Mode::NearestAway)?;
        Some((decimal.significand, decimal.exponent))
    }

    fn first(&self) -> Option<(u8, i32)> {
        let (decimal, _) = Decimal::of(self.parts()?, 1, Mode::Zero)?;
        Some((u8::try_from(decimal.significand).ok()?, decimal.exponent))
    }
}

impl PartialOrd for Five {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Five {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self.is_zero(), other.is_zero()) {
            (true, true) => Ordering::Equal,
            (true, false) => {
                if other.negative {
                    Ordering::Greater
                } else {
                    Ordering::Less
                }
            }
            (false, true) => {
                if self.negative {
                    Ordering::Less
                } else {
                    Ordering::Greater
                }
            }
            (false, false) => {
                let order = self.signed_order().cmp(&other.signed_order());
                if self.negative && other.negative {
                    order.reverse()
                } else {
                    order
                }
            }
        }
    }
}

/// The value of sign `negative` whose normalised exact form is `wide` (bit
/// 63 set) at `exponent`, with `sticky` for any nonzero bits below it, cut
/// to the mantissa by `rule`.
fn cut(negative: bool, exponent: i32, wide: u64, sticky: bool, rule: Cut) -> Result<Five, Fault> {
    let [f0, f1, f2, f3, k0, k1, k2, k3] = wide.to_le_bytes();
    let kept = u32::from_le_bytes([k0, k1, k2, k3]);
    let fraction = u32::from_le_bytes([f0, f1, f2, f3]);
    let up = rule == Cut::NearestOdd
        && (fraction > HALF || (fraction == HALF && (sticky || kept & 1 == 0)));
    let (mantissa, exponent) = match (up, kept.checked_add(1)) {
        (false, _) => (kept, exponent),
        (true, Some(next)) => (next, exponent),
        (true, None) => (TOP, exponent + 1),
    };
    if exponent > 255 {
        return Err(Fault::TooBig);
    }
    match u8::try_from(exponent) {
        Ok(exponent) => Ok(Five {
            negative,
            exponent,
            mantissa,
        }),
        Err(_) => Ok(Five::ZERO),
    }
}

/// The 32-bit integer of sign `negative` and magnitude `whole`.
fn int_of(negative: bool, whole: u64) -> Result<i32, Fault> {
    let signed = i64::try_from(whole).map_err(|_| Fault::TooBig)?;
    let value = if negative { -signed } else { signed };
    i32::try_from(value).map_err(|_| Fault::TooBig)
}

/// The low 64 bits of `x`.
const fn low_u64(x: u128) -> u64 {
    let b = x.to_le_bytes();
    u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
}

/// A bit count as a signed step; every count here is below 129.
fn to_i32(n: u32) -> i32 {
    i32::try_from(n).unwrap_or(i32::MAX)
}

/// The integer square root of `n`: the greatest `r` with `r * r <= n`.
fn isqrt(n: u128) -> u128 {
    if n == 0 {
        return 0;
    }
    // Newton's method from above the root descends to it without overshoot,
    // so the guess must start above: the root is below 2^(log2(n) / 2 + 1).
    let mut x = 1u128 << (n.ilog2() / 2 + 1);
    loop {
        let next = u128::midpoint(x, n / x);
        if next >= x {
            return x;
        }
        x = next;
    }
}

mod functions;

#[cfg(test)]
mod tests;
