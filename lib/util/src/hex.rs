//! Hexadecimal text for byte strings — digests, keys and seeds — two digits
//! a byte, written lowercase and read in either case.

const DIGITS: &[u8; 16] = b"0123456789abcdef";

/// The two lowercase hex digits spelling `byte`.
#[must_use]
pub fn digits(byte: u8) -> [u8; 2] {
    [
        DIGITS[usize::from(byte >> 4)],
        DIGITS[usize::from(byte & 0x0f)],
    ]
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
    use super::{decode_into, digits};

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
