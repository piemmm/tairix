//! A line number in a program's text: the byte `&8D`, then three bytes each in
//! `&40`–`&7F`, so none looks like a token, a terminator, a quote or a digit.
//! The first holds the top two bits of the number's low and high bytes,
//! flipped; the others each byte's low six.

/// The byte a line number's bytes begin after.
pub const MARKER: u8 = 0x8D;

/// How many bytes after the marker a line number is held in.
pub const ENCODED: usize = 3;

/// The bytes line `n` is held in.
#[must_use]
pub const fn encode(n: u16) -> [u8; ENCODED] {
    let [low, high] = n.to_le_bytes();
    [
        0x54 ^ (((low & 0xC0) >> 2) | ((high & 0xC0) >> 4)),
        0x40 | (low & 0x3F),
        0x40 | (high & 0x3F),
    ]
}

/// The line number its bytes hold.
#[must_use]
pub const fn decode([first, low, high]: [u8; ENCODED]) -> u16 {
    let low = low ^ ((first << 2) & 0xC0);
    let high = high ^ ((first << 4) & 0xC0);
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
