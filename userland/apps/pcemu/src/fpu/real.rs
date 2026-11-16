//! The 8087's temporary real: the double-extended value `lib/softfloat`
//! holds, how the chip classes it, and the integer, BCD and binary memory
//! formats it loads exactly.

use tairix_softfloat::extended::{BIAS, EXP_SPECIAL, INTEGER};
use tairix_softfloat::{Binary, Extended, Unpacked};

/// The real indefinite: the NaN a masked invalid operation answers.
pub const INDEFINITE: Extended = Extended::new(true, EXP_SPECIAL, 0xC000_0000_0000_0000);

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

/// The class of `x`'s bits.
#[must_use]
pub const fn class(x: Extended) -> Class {
    match x.exponent() {
        EXP_SPECIAL if x.sig & !INTEGER == 0 => Class::Infinity,
        EXP_SPECIAL => Class::Nan,
        0 if x.sig == 0 => Class::Zero,
        0 => Class::Denormal,
        _ if x.sig & INTEGER != 0 => Class::Normal,
        _ if x.sig == 0 => Class::PseudoZero,
        _ => Class::Unnormal,
    }
}

/// The value 18 packed BCD digits and a sign byte hold. A nibble above 9
/// counts its own value, which the 8087 leaves undefined.
#[must_use]
pub fn from_bcd(bytes: [u8; 10]) -> Extended {
    let mut n: u64 = 0;
    for &byte in bytes[..9].iter().rev() {
        n = n * 100 + u64::from(byte >> 4) * 10 + u64::from(byte & 0x0F);
    }
    // Eighteen digits of at most 15 are below 2^63, so the magnitude is
    // exact as a positive integer.
    let magnitude = i64::try_from(n).unwrap_or(i64::MAX);
    Extended::from_int(magnitude).with_sign(bytes[9] & 0x80 != 0)
}

/// The value a short real holds, and whether it was a denormal, which the
/// 8087 loads as the equivalent unnormal.
#[must_use]
pub fn from_single(bits: u32) -> (Extended, bool) {
    from_binary(u64::from(bits), Binary::SINGLE)
}

/// The value a long real holds, and whether it was a denormal.
#[must_use]
pub fn from_double(bits: u64) -> (Extended, bool) {
    from_binary(bits, Binary::DOUBLE)
}

/// The temporary real `bits` of `format` load as: exactly, a NaN keeping its
/// fraction below the integer bit and a denormal its least exponent.
fn from_binary(bits: u64, format: Binary) -> (Extended, bool) {
    match format.unpack(bits) {
        Unpacked::Zero { negative } => (Extended::new(negative, 0, 0), false),
        Unpacked::Infinity { negative } => (Extended::new(negative, EXP_SPECIAL, INTEGER), false),
        Unpacked::Nan {
            negative, fraction, ..
        } => (
            Extended::new(negative, EXP_SPECIAL, INTEGER | fraction),
            false,
        ),
        Unpacked::Finite { parts, subnormal } => {
            // Every exponent of either format is well inside temporary real's.
            let exponent = u16::try_from(parts.exp + BIAS).unwrap_or(0);
            (
                Extended::new(parts.negative, exponent, parts.sig),
                subnormal,
            )
        }
    }
}

/// The real indefinite in `format`.
#[must_use]
pub const fn indefinite(format: Binary) -> u64 {
    format.infinity(true) | format.quiet_bit()
}

#[cfg(test)]
#[path = "real_tests.rs"]
mod tests;
