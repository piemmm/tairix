//! A line number in a program's text: the byte `&8D`, then three bytes each in
//! `&40`–`&7F`, so none looks like a token, a terminator, a quote or a digit.
//! The first holds the top two bits of the number's low and high bytes,
//! flipped; the others each byte's low six.

/// The byte a line number's bytes begin after.
pub const MARKER: u8 = 0x8D;

/// How many bytes after the marker a line number is held in.
pub const ENCODED: usize = 3;

/// Each byte of a line number's top two bits, which the first encoded byte
/// carries for both.
pub const TOP_BITS: u8 = 0xC0;
/// Each byte's low six bits, which the second and third carry.
pub const LOW_BITS: u8 = 0x3F;
/// What the first encoded byte is exclusive-ORed with.
pub const FIRST_FLIP: u8 = 0x54;
/// The bit set in the second and third, so no encoded byte is a control
/// character.
pub const PRINTABLE: u8 = 0x40;

/// The bytes line `n` is held in.
#[must_use]
pub const fn encode(n: u16) -> [u8; ENCODED] {
    let [low, high] = n.to_le_bytes();
    [
        FIRST_FLIP ^ (((low & TOP_BITS) >> 2) | ((high & TOP_BITS) >> 4)),
        PRINTABLE | (low & LOW_BITS),
        PRINTABLE | (high & LOW_BITS),
    ]
}

/// The line number its bytes hold.
#[must_use]
pub const fn decode([first, low, high]: [u8; ENCODED]) -> u16 {
    let low = low ^ ((first << 2) & TOP_BITS);
    let high = high ^ ((first << 4) & TOP_BITS);
    u16::from_le_bytes([low, high])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_line_number_comes_back_and_its_bytes_lie_in_their_range() {
        assert_eq!(encode(10), [0x54, 0x4A, 0x40]);
        assert_eq!(encode(1000), [0x64, 0x68, 0x43]);
        for n in 0..=u16::MAX {
            let bytes = encode(n);
            assert!(bytes.iter().all(|b| (0x40..=0x7F).contains(b)), "{n}");
            assert_eq!(decode(bytes), n);
        }
    }
}
