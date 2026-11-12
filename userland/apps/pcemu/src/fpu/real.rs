//! The 8087's temporary real: its 80-bit format, how the chip classes a
//! value, and the integer, BCD and binary memory formats it loads exactly.

/// The exponent bias.
pub const BIAS: i32 = 16_383;
/// The biased exponent infinities and NaNs carry.
pub const EXP_SPECIAL: u16 = 0x7FFF;
/// The significand's explicit integer bit.
pub const INTEGER: u64 = 1 << 63;
/// The largest biased exponent a finite value has.
pub const EXP_MAX: u16 = 0x7FFE;

/// An 80-bit temporary real: the sign and 15-bit biased exponent, and a
/// 64-bit significand whose integer bit is explicit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Real {
    /// The sign in bit 15, the biased exponent below it.
    pub sign_exp: u16,
    /// The significand, its integer bit the top one.
    pub sig: u64,
}

/// How the 8087 classes the bits of a value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// Exponent and significand zero.
    Zero,
    /// A nonzero exponent below the special one, the integer bit set.
    Normal,
    /// A nonzero exponent below the special one, the integer bit clear and
    /// the significand not zero: a valid number with fewer significant bits.
    Unnormal,
    /// A nonzero exponent below the special one and a zero significand.
    PseudoZero,
    /// A zero exponent and a nonzero significand.
    Denormal,
    /// The special exponent and a zero fraction.
    Infinity,
    /// The special exponent and a nonzero fraction.
    Nan,
}

impl Real {
    /// Positive zero.
    pub const ZERO: Self = Self::new(false, 0, 0);
    /// One.
    pub const ONE: Self = Self::new(false, 0x3FFF, INTEGER);
    /// Positive infinity.
    pub const INFINITY: Self = Self::new(false, EXP_SPECIAL, INTEGER);
    /// The real indefinite: the NaN a masked invalid operation answers.
    pub const INDEFINITE: Self = Self::new(true, EXP_SPECIAL, 0xC000_0000_0000_0000);

    /// The value with this sign, biased exponent and significand.
    #[must_use]
    pub const fn new(negative: bool, exponent: u16, sig: u64) -> Self {
        let sign = if negative { 0x8000 } else { 0 };
        Self {
            sign_exp: sign | (exponent & EXP_SPECIAL),
            sig,
        }
    }

    /// Whether the sign bit is set.
    #[must_use]
    pub const fn negative(self) -> bool {
        self.sign_exp & 0x8000 != 0
    }

    /// The biased exponent.
    #[must_use]
    pub const fn exponent(self) -> u16 {
        self.sign_exp & EXP_SPECIAL
    }

    /// This value with its sign set as `negative` says.
    #[must_use]
    pub const fn with_sign(self, negative: bool) -> Self {
        Self::new(negative, self.exponent(), self.sig)
    }

    /// The class of these bits.
    #[must_use]
    pub const fn class(self) -> Class {
        match self.exponent() {
            EXP_SPECIAL if self.sig & !INTEGER == 0 => Class::Infinity,
            EXP_SPECIAL => Class::Nan,
            0 if self.sig == 0 => Class::Zero,
            0 => Class::Denormal,
            _ if self.sig & INTEGER != 0 => Class::Normal,
            _ if self.sig == 0 => Class::PseudoZero,
            _ => Class::Unnormal,
        }
    }

    /// The ten bytes the value is in memory: the significand first, low
    /// byte first, then the sign and exponent.
    #[must_use]
    pub fn to_bytes(self) -> [u8; 10] {
        let mut bytes = [0; 10];
        bytes[..8].copy_from_slice(&self.sig.to_le_bytes());
        bytes[8..].copy_from_slice(&self.sign_exp.to_le_bytes());
        bytes
    }

    /// The value ten bytes of memory hold.
    #[must_use]
    pub fn from_bytes(bytes: [u8; 10]) -> Self {
        let mut sig = [0; 8];
        sig.copy_from_slice(&bytes[..8]);
        Self {
            sign_exp: u16::from_le_bytes([bytes[8], bytes[9]]),
            sig: u64::from_le_bytes(sig),
        }
    }

    /// The exact value of `n`.
    #[must_use]
    pub const fn from_int(n: i64) -> Self {
        Self::from_magnitude(n < 0, n.unsigned_abs())
    }

    /// The exact value of the integer `magnitude`, negative where it says;
    /// a zero magnitude gives a zero of that sign.
    #[must_use]
    pub const fn from_magnitude(negative: bool, magnitude: u64) -> Self {
        if magnitude == 0 {
            return Self::new(negative, 0, 0);
        }
        let shift = magnitude.leading_zeros();
        // At most 63 places from the integer bit, so the exponent fits.
        Self::new(
            negative,
            crate::bits::low16(0x3FFF + 63 - shift),
            magnitude << shift,
        )
    }

    /// The value 18 packed BCD digits and a sign byte hold. A nibble above 9
    /// counts its own value, which the 8087 leaves undefined.
    #[must_use]
    pub fn from_bcd(bytes: [u8; 10]) -> Self {
        let mut n: u64 = 0;
        for &byte in bytes[..9].iter().rev() {
            n = n * 100 + u64::from(byte >> 4) * 10 + u64::from(byte & 0x0F);
        }
        // Eighteen digits of at most 15 are below 2^63, so the magnitude is
        // exact as a positive integer.
        let magnitude = i64::try_from(n).unwrap_or(i64::MAX);
        Self::from_int(magnitude).with_sign(bytes[9] & 0x80 != 0)
    }

    /// The value a short real holds, and whether it was a denormal, which
    /// the 8087 loads as the equivalent unnormal.
    #[must_use]
    pub fn from_single(bits: u32) -> (Self, bool) {
        Self::from_binary(u64::from(bits), Binary::SINGLE)
    }

    /// The value a long real holds, and whether it was a denormal.
    #[must_use]
    pub fn from_double(bits: u64) -> (Self, bool) {
        Self::from_binary(bits, Binary::DOUBLE)
    }

    fn from_binary(bits: u64, format: Binary) -> (Self, bool) {
        let fraction_bits = format.precision - 1;
        let negative = bits >> (format.width - 1) & 1 != 0;
        let field = (bits >> fraction_bits) & format.exp_mask();
        let fraction = bits & ((1 << fraction_bits) - 1);
        // The fraction moves to the top of the 63 bits below the integer
        // bit.
        let aligned = fraction << (63 - fraction_bits);
        if field == format.exp_mask() {
            return (Self::new(negative, EXP_SPECIAL, INTEGER | aligned), false);
        }
        if field == 0 && fraction == 0 {
            return (Self::new(negative, 0, 0), false);
        }
        // A denormal's exponent is the format's least and its integer bit
        // clear: in temporary real, an unnormal.
        let (unbiased, integer) = if field == 0 {
            (format.min_exp(), 0)
        } else {
            (i32::try_from(field).unwrap_or(0) - format.bias, INTEGER)
        };
        // Every exponent of either format is well inside temporary real's.
        let exponent = u16::try_from(unbiased + BIAS).unwrap_or(0);
        (Self::new(negative, exponent, integer | aligned), field == 0)
    }
}

/// An IEEE binary interchange format the 8087 loads and stores.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Binary {
    /// Bits in the whole format.
    pub width: u32,
    /// Significand bits, the hidden integer bit counted.
    pub precision: u32,
    /// The exponent bias.
    pub bias: i32,
}

impl Binary {
    /// The 32-bit short real.
    pub const SINGLE: Self = Self {
        width: 32,
        precision: 24,
        bias: 127,
    };
    /// The 64-bit long real.
    pub const DOUBLE: Self = Self {
        width: 64,
        precision: 53,
        bias: 1023,
    };

    /// The biased exponent field's mask, which is also its special value.
    #[must_use]
    pub const fn exp_mask(self) -> u64 {
        (1 << (self.width - self.precision)) - 1
    }

    /// The least exponent a normal value has, unbiased.
    #[must_use]
    pub const fn min_exp(self) -> i32 {
        1 - self.bias
    }

    /// The greatest exponent a finite value has, unbiased.
    #[must_use]
    pub const fn max_exp(self) -> i32 {
        self.bias
    }

    /// The real indefinite in this format.
    #[must_use]
    pub const fn indefinite(self) -> u64 {
        let fraction_bits = self.precision - 1;
        let sign = 1 << (self.width - 1);
        sign | (self.exp_mask() << fraction_bits) | (1 << (fraction_bits - 1))
    }
}

#[cfg(test)]
#[path = "real_tests.rs"]
mod tests;
