//! The checksum against the catalogued check value and a bitwise reference,
//! and a carried checksum against the one-shot one.

use super::{checksum, update, POLY};

/// The definition, a bit at a time: the oracle the table is held to.
fn bitwise(mut crc: u16, data: &[u8]) -> u16 {
    for &byte in data {
        crc ^= u16::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 0 {
                crc >> 1
            } else {
                (crc >> 1) ^ POLY
            };
        }
    }
    crc
}

#[test]
fn the_check_value_is_the_catalogues() {
    assert_eq!(checksum(b"123456789"), 0xBB3D);
    // CRC-16/MODBUS: the same register started from all ones.
    assert_eq!(update(0xFFFF, b"123456789"), 0x4B37);
}

#[test]
fn no_bytes_leave_the_start_value() {
    assert_eq!(checksum(&[]), 0);
    assert_eq!(update(0x1234, &[]), 0x1234);
}

#[test]
fn the_table_agrees_with_the_definition_on_every_byte_and_length() {
    let data: [u8; 600] =
        core::array::from_fn(|at| u8::try_from((at * 151 + 7) % 256).unwrap_or(0));
    for len in 0..data.len() {
        for start in [0, 0xFFFF, 0x5A5A] {
            assert_eq!(
                update(start, &data[..len]),
                bitwise(start, &data[..len]),
                "length {len}"
            );
        }
    }
    for byte in 0..=u8::MAX {
        assert_eq!(checksum(&[byte]), bitwise(0, &[byte]), "byte {byte}");
    }
}

#[test]
fn pieces_carried_in_order_checksum_as_their_concatenation() {
    let data: [u8; 97] = core::array::from_fn(|at| u8::try_from(at * 13 % 256).unwrap_or(0));
    let whole = checksum(&data);
    for split in 0..=data.len() {
        let (head, tail) = data.split_at(split);
        assert_eq!(
            update(update(checksum(head), &[]), tail),
            whole,
            "split at {split}"
        );
    }
}
