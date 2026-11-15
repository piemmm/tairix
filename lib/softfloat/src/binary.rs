//! The IEEE 754 binary interchange formats: binary32 and binary64, each a
//! sign, a biased exponent field and a fraction with its integer bit hidden.

use crate::exact::Parts;
use crate::extended::INTEGER;

/// A binary interchange format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Binary {
    /// Bits in the whole format.
    pub width: u32,
    /// Significand bits, the hidden integer bit counted.
    pub precision: u32,
    /// The exponent bias.
    pub bias: i32,
}

/// What the bits of a binary value are.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Unpacked {
    /// A zero of this sign.
    Zero {
        /// The sign.
        negative: bool,
    },
    /// A finite nonzero value. A subnormal's parts have the least exponent
    /// and its integer bit clear, the unnormal it equals.
    Finite {
        /// The value's parts.
        parts: Parts,
        /// Whether its bits were subnormal.
        subnormal: bool,
    },
    /// An infinity of this sign.
    Infinity {
        /// The sign.
        negative: bool,
    },
    /// A NaN.
    Nan {
        /// The sign.
        negative: bool,
        /// Its fraction, at the top of the 63 bits below the integer bit's
        /// place.
        fraction: u64,
        /// Whether it signals: its fraction's top bit is clear.
        signalling: bool,
    },
}

impl Binary {
    /// binary32, the single format.
    pub const SINGLE: Self = Self {
        width: 32,
        precision: 24,
        bias: 127,
    };

    /// binary64, the double format.
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

    /// Bits in the stored fraction.
    #[must_use]
    pub const fn fraction_bits(self) -> u32 {
        self.precision - 1
    }

    /// The sign bit.
    #[must_use]
    pub const fn sign_bit(self) -> u64 {
        1 << (self.width - 1)
    }

    /// The quiet bit: the fraction's top one.
    #[must_use]
    pub const fn quiet_bit(self) -> u64 {
        1 << (self.fraction_bits() - 1)
    }

    /// The infinity of sign `negative`.
    #[must_use]
    pub const fn infinity(self, negative: bool) -> u64 {
        self.sign(negative) | self.exp_mask() << self.fraction_bits()
    }

    /// The zero of sign `negative`.
    #[must_use]
    pub const fn zero(self, negative: bool) -> u64 {
        self.sign(negative)
    }

    /// The largest finite value of sign `negative`.
    #[must_use]
    pub const fn largest(self, negative: bool) -> u64 {
        self.infinity(negative) - 1
    }

    /// The default NaN: positive, quiet, its payload zero.
    #[must_use]
    pub const fn default_nan(self) -> u64 {
        self.infinity(false) | self.quiet_bit()
    }

    /// `bits` with its sign set as `negative` says.
    #[must_use]
    pub const fn with_sign(self, bits: u64, negative: bool) -> u64 {
        bits & !self.sign_bit() | self.sign(negative)
    }

    /// Whether `bits` are negative.
    #[must_use]
    pub const fn negative(self, bits: u64) -> bool {
        bits & self.sign_bit() != 0
    }

    const fn sign(self, negative: bool) -> u64 {
        if negative {
            self.sign_bit()
        } else {
            0
        }
    }

    /// What `bits` are, the format's own bits in the low `width` of them.
    #[must_use]
    pub fn unpack(self, bits: u64) -> Unpacked {
        let fraction_bits = self.fraction_bits();
        let negative = self.negative(bits);
        let field = (bits >> fraction_bits) & self.exp_mask();
        let fraction = bits & ((1 << fraction_bits) - 1);
        // The fraction moves to the top of the 63 bits below the integer bit.
        let aligned = fraction << (63 - fraction_bits);
        if field == self.exp_mask() {
            if fraction == 0 {
                return Unpacked::Infinity { negative };
            }
            return Unpacked::Nan {
                negative,
                fraction: aligned,
                signalling: bits & self.quiet_bit() == 0,
            };
        }
        if field == 0 && fraction == 0 {
            return Unpacked::Zero { negative };
        }
        let (exp, integer) = if field == 0 {
            (self.min_exp(), 0)
        } else {
            (i32::try_from(field).unwrap_or(0) - self.bias, INTEGER)
        };
        Unpacked::Finite {
            parts: Parts {
                negative,
                exp,
                sig: integer | aligned,
            },
            subnormal: field == 0,
        }
    }

    /// The bits of sign `negative`, biased exponent field `field` and the
    /// fraction below the integer bit of the 64-bit significand `sig`.
    #[must_use]
    pub const fn pack(self, negative: bool, field: u64, sig: u64) -> u64 {
        let fraction_mask = (1 << self.fraction_bits()) - 1;
        self.sign(negative)
            | field << self.fraction_bits()
            | (sig >> (64 - self.precision)) & fraction_mask
    }

    /// A NaN of sign `negative` whose fraction is `fraction`, aligned as
    /// [`Unpacked::Nan`] holds it, quieted.
    #[must_use]
    pub const fn quiet_nan(self, negative: bool, fraction: u64) -> u64 {
        self.infinity(negative) | (fraction >> (63 - self.fraction_bits())) | self.quiet_bit()
    }
}

/// The two words the FPA keeps a binary64 value in, in memory, lowest address
/// first: the word holding the sign and exponent first, the reverse of the
/// value's own little-endian order.
#[must_use]
pub const fn fpa_words(bits: u64) -> [u32; 2] {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "each word takes its half of the value"
    )]
    let words = [(bits >> 32) as u32, bits as u32];
    words
}

/// The binary64 value two FPA words hold, lowest address first.
#[must_use]
pub const fn from_fpa_words([high, low]: [u32; 2]) -> u64 {
    (high as u64) << 32 | low as u64
}
