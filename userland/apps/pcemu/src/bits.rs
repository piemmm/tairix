//! The width conversions the emulated hardware's own arithmetic performs.
//!
//! A 16-bit register's halves, a 20-bit bus address and a sign-extended byte
//! are the machine's semantics, not lossy conversions, so they are spelled
//! here once and nowhere else.

/// The low byte of a word.
#[inline]
#[must_use]
pub const fn lo(word: u16) -> u8 {
    word.to_le_bytes()[0]
}

/// The high byte of a word.
#[inline]
#[must_use]
pub const fn hi(word: u16) -> u8 {
    word.to_le_bytes()[1]
}

/// The word with these halves.
#[inline]
#[must_use]
pub const fn word(lo: u8, hi: u8) -> u16 {
    u16::from_le_bytes([lo, hi])
}

/// The low 16 bits of a 32-bit value.
#[inline]
#[must_use]
pub const fn low16(value: u32) -> u16 {
    let bytes = value.to_le_bytes();
    u16::from_le_bytes([bytes[0], bytes[1]])
}

/// The high 16 bits of a 32-bit value.
#[inline]
#[must_use]
pub const fn high16(value: u32) -> u16 {
    let bytes = value.to_le_bytes();
    u16::from_le_bytes([bytes[2], bytes[3]])
}

/// The low byte of a 32-bit value.
#[inline]
#[must_use]
pub const fn low8(value: u32) -> u8 {
    value.to_le_bytes()[0]
}

/// A byte sign-extended to a word.
#[inline]
#[must_use]
pub fn sext8(byte: u8) -> u16 {
    i16::from(byte.cast_signed()).cast_unsigned()
}

/// The 32-bit value with these halves.
#[inline]
#[must_use]
pub fn dword(lo: u16, hi: u16) -> u32 {
    (u32::from(hi) << 16) | u32::from(lo)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn halves_round_trip() {
        assert_eq!(lo(0xBEEF), 0xEF);
        assert_eq!(hi(0xBEEF), 0xBE);
        assert_eq!(word(0xEF, 0xBE), 0xBEEF);
        assert_eq!(low16(0x1234_5678), 0x5678);
        assert_eq!(high16(0x1234_5678), 0x1234);
        assert_eq!(low8(0x1234_5678), 0x78);
        assert_eq!(dword(0x5678, 0x1234), 0x1234_5678);
    }

    #[test]
    fn sign_extension_follows_bit_seven() {
        assert_eq!(sext8(0x7F), 0x007F);
        assert_eq!(sext8(0x80), 0xFF80);
        assert_eq!(sext8(0xFF), 0xFFFF);
        assert_eq!(sext8(0x00), 0x0000);
    }
}
