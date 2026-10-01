//! The checksum against the standard's check value and a bitwise reference,
//! and the streaming form against the one-shot one.

use super::{checksum, Crc32, POLY};

/// The definition, a bit at a time: the oracle the table is held to.
fn bitwise(data: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 0 {
                crc >> 1
            } else {
                (crc >> 1) ^ POLY
            };
        }
    }
    !crc
}

#[test]
fn the_check_value_is_the_standards() {
    assert_eq!(checksum(b"123456789"), 0xCBF4_3926);
}

#[test]
fn no_bytes_checksum_to_zero() {
    assert_eq!(checksum(&[]), 0);
    assert_eq!(Crc32::new().finish(), 0);
    assert_eq!(Crc32::default().finish(), 0);
}

#[test]
fn the_table_agrees_with_the_definition_on_every_byte_and_length() {
    let data: [u8; 600] =
        core::array::from_fn(|at| u8::try_from((at * 151 + 7) % 256).unwrap_or(0));
    for len in 0..data.len() {
        assert_eq!(
            checksum(&data[..len]),
            bitwise(&data[..len]),
            "length {len}"
        );
    }
    for byte in 0..=u8::MAX {
        assert_eq!(checksum(&[byte]), bitwise(&[byte]), "byte {byte}");
    }
}

#[test]
fn pieces_fed_in_order_checksum_as_their_concatenation() {
    let data: [u8; 97] = core::array::from_fn(|at| u8::try_from(at * 13 % 256).unwrap_or(0));
    let whole = checksum(&data);
    for split in 0..=data.len() {
        let mut crc = Crc32::new();
        let (head, tail) = data.split_at(split);
        crc.update(head);
        crc.update(&[]);
        crc.update(tail);
        assert_eq!(crc.finish(), whole, "split at {split}");
    }
}
