//! Little-endian integers read from a byte slice: the one read every format
//! of that byte order shares, each naming its own refusal for a field the
//! slice ends before.

/// The 16-bit value at `at` of `bytes`, or `None` where they hold fewer than
/// two bytes there.
#[must_use]
pub fn le_u16(bytes: &[u8], at: usize) -> Option<u16> {
    bytes
        .get(at..)
        .and_then(<[u8]>::first_chunk::<2>)
        .map(|two| u16::from_le_bytes(*two))
}

/// The 32-bit value at `at` of `bytes`, or `None` where they hold fewer than
/// four bytes there.
#[must_use]
pub fn le_u32(bytes: &[u8], at: usize) -> Option<u32> {
    bytes
        .get(at..)
        .and_then(<[u8]>::first_chunk::<4>)
        .map(|four| u32::from_le_bytes(*four))
}

/// The two's-complement 32-bit value at `at` of `bytes`, or `None` where
/// they hold fewer than four bytes there.
#[must_use]
pub fn le_i32(bytes: &[u8], at: usize) -> Option<i32> {
    le_u32(bytes, at).map(u32::cast_signed)
}

#[cfg(test)]
mod tests {
    use super::{le_i32, le_u16, le_u32};

    #[test]
    fn a_value_is_read_low_byte_first() {
        let bytes = [0x78, 0x56, 0x34, 0x12, 0xFF];
        assert_eq!(le_u16(&bytes, 0), Some(0x5678));
        assert_eq!(le_u32(&bytes, 0), Some(0x1234_5678));
        assert_eq!(le_u32(&bytes, 1), Some(0xFF12_3456));
        assert_eq!(le_i32(&[0xFF; 4], 0), Some(-1));
    }

    #[test]
    fn a_value_past_the_bytes_is_none() {
        assert_eq!(le_u16(&[1], 0), None);
        assert_eq!(le_u32(&[1, 2, 3], 0), None);
        assert_eq!(le_u32(&[1, 0, 0, 0], 1), None);
        assert_eq!(le_u32(&[1, 0, 0, 0], usize::MAX), None);
        assert_eq!(le_i32(&[0; 4], 5), None);
    }
}
