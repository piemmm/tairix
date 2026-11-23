//! Hexadecimal text: byte strings — digests, keys and seeds — two digits a
//! byte, written lowercase and read in either case; and numbers written at a
//! fixed width or with their leading noughts dropped, in either case.

const DIGITS: &[u8; 16] = b"0123456789abcdef";

/// The lowercase hex digit spelling the low four bits of `nibble`.
#[must_use]
pub const fn digit(nibble: u8) -> u8 {
    DIGITS[(nibble & 0x0f) as usize]
}

/// The uppercase hex digit spelling the low four bits of `nibble`.
#[must_use]
pub const fn upper_digit(nibble: u8) -> u8 {
    digit(nibble).to_ascii_uppercase()
}

/// The two lowercase hex digits spelling `byte`.
#[must_use]
pub const fn digits(byte: u8) -> [u8; 2] {
    [digit(byte >> 4), digit(byte)]
}

/// The two uppercase hex digits spelling `byte`.
#[must_use]
pub const fn upper_digits(byte: u8) -> [u8; 2] {
    [upper_digit(byte >> 4), upper_digit(byte)]
}

/// Fills `out` with the lowercase hex digits of `value`'s low nibbles, the
/// highest first: as many as `out` holds, noughts past the value's own.
pub fn fill(out: &mut [u8], value: u64) {
    fill_spelt(out, value, digit);
}

/// As [`fill`], in capitals.
pub fn fill_upper(out: &mut [u8], value: u64) {
    fill_spelt(out, value, upper_digit);
}

fn fill_spelt(out: &mut [u8], value: u64, spell: fn(u8) -> u8) {
    let mut rest = value;
    for slot in out.iter_mut().rev() {
        *slot = spell(rest.to_le_bytes()[0]);
        rest >>= 4;
    }
}

/// How many hex digits `value` takes with its leading noughts dropped: one
/// at least.
#[must_use]
pub const fn significant(value: u64) -> usize {
    match 64 - value.leading_zeros() {
        0 => 1,
        bits => bits.div_ceil(4) as usize,
    }
}

/// `bytes` as lowercase hex text.
#[must_use]
pub fn encode(bytes: &[u8]) -> alloc::string::String {
    bytes
        .iter()
        .flat_map(|&byte| digits(byte))
        .map(char::from)
        .collect()
}

/// Fill `out` with the bytes `text` spells, answering whether it spelled
/// exactly `out.len()` of them; on `false`, `out` holds no meaning.
///
/// The bytes are written straight into `out`, so a secret is never staged in
/// a buffer its caller cannot erase.
#[must_use]
pub fn decode_into(text: &str, out: &mut [u8]) -> bool {
    let text = text.as_bytes();
    if text.len() != out.len().saturating_mul(2) {
        return false;
    }
    let (pairs, _) = text.as_chunks::<2>();
    for (slot, &[high, low]) in out.iter_mut().zip(pairs) {
        let (Some(high), Some(low)) = (value(high), value(low)) else {
            return false;
        };
        *slot = (high << 4) | low;
    }
    true
}

const fn value(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        b'A'..=b'F' => Some(digit - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{decode_into, digits, fill, fill_upper, significant, upper_digit, upper_digits};

    #[test]
    fn every_byte_round_trips_through_its_digits() {
        for byte in 0..=u8::MAX {
            let pair = digits(byte);
            let text = core::str::from_utf8(&pair).expect("ASCII digits");
            let mut back = [0u8; 1];
            assert!(decode_into(text, &mut back), "{text}");
            assert_eq!(back[0], byte);
        }
        assert_eq!(digits(0xab), *b"ab");
    }

    #[test]
    fn an_upper_digit_spells_its_nibble_in_capitals_and_ignores_the_high_bits() {
        let spelled: alloc::vec::Vec<u8> = (0..16u8).map(|n| upper_digit(n | 0xF0)).collect();
        assert_eq!(spelled, b"0123456789ABCDEF");
    }

    #[test]
    fn a_number_fills_its_width_from_its_low_nibbles_in_either_case() {
        let mut out = [0u8; 4];
        fill_upper(&mut out, 0x1234_ABCD);
        assert_eq!(&out, b"ABCD", "the low digits the width holds");
        let mut wide = [0u8; 20];
        fill(&mut wide, u64::MAX);
        assert_eq!(&wide, b"0000ffffffffffffffff", "noughts past the value");
        fill(&mut [], 1);
        assert_eq!(upper_digits(0x0f), *b"0F");
        assert_eq!(
            [0, 1, 0xF, 0x10, 0xFFF, 0x1000, u64::MAX].map(significant),
            [1, 1, 1, 2, 3, 4, 16]
        );
    }

    #[test]
    fn either_case_is_read_and_anything_else_is_refused() {
        let mut out = [0u8; 2];
        assert!(decode_into("aBcD", &mut out));
        assert_eq!(out, [0xab, 0xcd]);
        for refused in ["abc", "abcdef", "ab g", "+abc", "", " abcd"] {
            assert!(!decode_into(refused, &mut out), "{refused:?}");
        }
        assert!(decode_into("", &mut []));
    }
}
