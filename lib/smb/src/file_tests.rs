//! Unit tests for what the file requests share: credit charges, attributes,
//! times, and output buffers.

use alloc::vec::Vec;

use tairix_abi::time::Time64;

use super::{
    credit_charge, exactly, output_buffer, write_output_buffer, FileAttributes, FileTimes, MAX_IO,
};
use crate::header::HEADER_LEN;
use crate::vectors;
use crate::wire::{filetime_exact, time_of_filetime, Encoder, Reader};
use crate::SmbError;

fn time(secs: i64, nanos: u32) -> Time64 {
    Time64::new(secs, nanos).expect("a valid time")
}

fn times(each: Time64) -> FileTimes {
    FileTimes {
        created: each,
        accessed: each,
        written: each,
        changed: each,
    }
}

fn written(times: &FileTimes) -> Result<Vec<u8>, SmbError> {
    let mut enc = Encoder::new();
    times.write(&mut enc);
    enc.finish()
}

#[test]
fn a_request_is_charged_a_credit_per_64_kib_and_never_none() {
    assert_eq!(credit_charge(0), 1);
    assert_eq!(credit_charge(1), 1);
    assert_eq!(credit_charge(64 << 10), 1);
    assert_eq!(credit_charge((64 << 10) + 1), 2);
    assert_eq!(credit_charge(MAX_IO), 16);
    assert_eq!(credit_charge(usize::MAX), u16::MAX);
}

#[test]
fn attributes_are_set_cleared_and_tested_bit_by_bit() {
    let attributes = FileAttributes::DIRECTORY.with(FileAttributes::HIDDEN);
    assert_eq!(attributes.bits(), 0x12);
    assert!(attributes.contains(FileAttributes::DIRECTORY));
    assert!(!attributes.contains(FileAttributes::DIRECTORY.with(FileAttributes::SYSTEM)));
    assert_eq!(
        attributes.without(FileAttributes::HIDDEN),
        FileAttributes::DIRECTORY
    );
    assert_eq!(FileAttributes::from_bits(0x8000_0000).bits(), 0x8000_0000);
    assert_eq!(FileAttributes::default(), FileAttributes::NONE);
}

#[test]
fn times_cross_exactly_before_1970_and_after_2038() {
    let crossed = FileTimes {
        created: time(-11_644_473_600, 100),
        accessed: time(-1, 999_999_900),
        written: time(2_147_483_648, 0),
        changed: time(4_102_444_800, 500),
    };
    let bytes = written(&crossed).expect("encodes");
    assert_eq!(bytes.len(), 32);
    assert_eq!(
        bytes[..8],
        1_u64.to_le_bytes(),
        "a FILETIME of one interval"
    );
    assert_eq!(FileTimes::read(&mut Reader::new(&bytes)), Ok(crossed));
}

#[test]
fn every_filetime_is_a_time_and_back() {
    for filetime in [
        0,
        1,
        116_444_736_000_000_000,
        0x01d8_0000_0000_0000,
        u64::MAX,
    ] {
        let time = time_of_filetime(filetime).expect("a time");
        assert_eq!(filetime_exact(time), Ok(filetime), "{filetime:#x}");
    }
    assert_eq!(
        time_of_filetime(116_444_736_000_000_000),
        Ok(Time64::UNIX_EPOCH)
    );
}

#[test]
fn a_time_filetime_cannot_carry_exactly_is_refused_not_rounded() {
    for refused in [
        time(0, 1),
        time(0, 50),
        time(-11_644_473_601, 0),
        time(i64::MAX, 0),
    ] {
        assert_eq!(
            written(&times(refused)),
            Err(SmbError::TimeOutOfRange),
            "{refused:?}"
        );
    }
}

#[test]
fn a_fixed_structure_is_read_only_at_its_exact_length() {
    assert!(exactly(&[0; 8], 8).is_ok());
    assert_eq!(exactly(&[0; 7], 8).err(), Some(SmbError::Malformed));
    assert_eq!(exactly(&[0; 9], 8).err(), Some(SmbError::Malformed));
}

#[test]
fn an_output_buffer_names_its_bytes_and_counts_one_when_empty() {
    let bytes = vectors::message(|enc| write_output_buffer(enc, b"abc"));
    assert_eq!(
        bytes[HEADER_LEN..],
        [9, 0, 72, 0, 3, 0, 0, 0, b'a', b'b', b'c']
    );
    assert_eq!(output_buffer(&bytes, 9, 3), Ok(&b"abc"[..]));
    assert_eq!(output_buffer(&bytes, 9, 2), Err(SmbError::TooLarge));

    let empty = vectors::message(|enc| write_output_buffer(enc, &[]));
    assert_eq!(empty[HEADER_LEN..], [9, 0, 72, 0, 0, 0, 0, 0, 0]);
    assert_eq!(output_buffer(&empty, 9, 0), Ok(&[][..]));
}

#[test]
fn an_output_buffer_outside_its_message_or_structure_is_refused() {
    let bytes = vectors::message(|enc| write_output_buffer(enc, b"abc"));
    let mut long = bytes.clone();
    long[HEADER_LEN + 4] = 4;
    assert_eq!(output_buffer(&long, 9, 16), Err(SmbError::Malformed));
    let mut moved = bytes.clone();
    moved[HEADER_LEN + 2] = 0xff;
    assert_eq!(output_buffer(&moved, 9, 16), Err(SmbError::Malformed));
    assert_eq!(output_buffer(&bytes, 17, 16), Err(SmbError::Malformed));
    assert_eq!(
        output_buffer(&bytes[..HEADER_LEN + 6], 9, 16),
        Err(SmbError::Malformed)
    );
}
