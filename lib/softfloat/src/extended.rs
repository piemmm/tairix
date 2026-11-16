//! The double-extended value the 8087's registers and the FPA's extended
//! precision both hold: a sign, a 15-bit biased exponent and a 64-bit
//! significand whose integer bit is explicit.

/// The exponent bias.
pub const BIAS: i32 = 16_383;

/// The biased exponent infinities and NaNs carry.
pub const EXP_SPECIAL: u16 = 0x7FFF;

/// The largest biased exponent a finite value has.
pub const EXP_MAX: u16 = 0x7FFE;

/// The significand's explicit integer bit.
pub const INTEGER: u64 = 1 << 63;

/// A double-extended value: the sign and biased exponent, and a significand
/// with its integer bit explicit, so an unnormal — the integer bit clear above
/// the least exponent — is representable.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Extended {
    /// The sign in bit 15, the biased exponent below it.
    pub sign_exp: u16,
    /// The significand, its integer bit the top one.
    pub sig: u64,
}

impl Extended {
    /// Positive zero.
    pub const ZERO: Self = Self::new(false, 0, 0);

    /// One.
    pub const ONE: Self = Self::new(false, 0x3FFF, INTEGER);

    /// Positive infinity.
    pub const INFINITY: Self = Self::new(false, EXP_SPECIAL, INTEGER);

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

    /// The exact value of `n`.
    #[must_use]
    pub const fn from_int(n: i64) -> Self {
        Self::from_magnitude(n < 0, n.unsigned_abs())
    }

    /// The exact value of the integer `magnitude`, negative where it says; a
    /// zero magnitude gives a zero of that sign.
    #[must_use]
    pub const fn from_magnitude(negative: bool, magnitude: u64) -> Self {
        if magnitude == 0 {
            return Self::new(negative, 0, 0);
        }
        let shift = magnitude.leading_zeros();
        // At most 63 places from the integer bit: the exponent is 0x3FFF to
        // 0x403E, which a u16 holds.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "the biased exponent of a 64-bit integer fits sixteen bits"
        )]
        let exponent = (0x3FFF + 63 - shift) as u16;
        Self::new(negative, exponent, magnitude << shift)
    }

    /// The ten bytes the x87 keeps the value in, in memory: the significand
    /// first, low byte first, then the sign and exponent.
    #[must_use]
    pub fn to_bytes(self) -> [u8; 10] {
        let mut bytes = [0; 10];
        bytes[..8].copy_from_slice(&self.sig.to_le_bytes());
        bytes[8..].copy_from_slice(&self.sign_exp.to_le_bytes());
        bytes
    }

    /// The value ten bytes of x87 memory hold.
    #[must_use]
    pub fn from_bytes(bytes: [u8; 10]) -> Self {
        let mut sig = [0; 8];
        sig.copy_from_slice(&bytes[..8]);
        Self {
            sign_exp: u16::from_le_bytes([bytes[8], bytes[9]]),
            sig: u64::from_le_bytes(sig),
        }
    }
}
