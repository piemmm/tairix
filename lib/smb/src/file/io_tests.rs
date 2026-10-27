//! Unit tests for `READ`, `WRITE`, `FLUSH` and `CLOSE`, against what Windows
//! sent where Microsoft published it.

use alloc::vec;

use tairix_abi::time::Time64;

use super::{
    CloseRequest, CloseResponse, ClosedAttributes, FlushRequest, ReadRequest, ReadResponse,
    WriteRequest, WriteResponse,
};
use crate::file::{FileAttributes, FileTimes, MAX_IO};
use crate::header::HEADER_LEN;
use crate::simple::decode_empty;
use crate::vectors::{self, Capture, SMB300, SMB311_CCM, SMB311_GCM};
use crate::wire::Encoder;
use crate::SmbError;

const CAPTURES: [&Capture; 3] = [&SMB300, &SMB311_GCM, &SMB311_CCM];

/// What each captured `WRITE` wrote and each `READ` read back.
const DATA: &[u8] = b"Smb3 encryption testing";

fn past_the_bound() -> u32 {
    u32::try_from(MAX_IO + 1).expect("a 32-bit length")
}

fn encoded(encode: impl FnOnce(&mut Encoder)) -> Result<(), SmbError> {
    let mut enc = Encoder::new();
    encode(&mut enc);
    enc.finish().map(drop)
}

fn patch_u32(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

#[test]
fn windows_writes_and_reads_decode_and_reencode_byte_for_byte() {
    for capture in CAPTURES {
        let request = vectors::bytes(capture.write.request);
        let write = WriteRequest::decode(&request).expect("a write");
        assert_eq!((write.offset, write.data), (0, DATA));
        assert!(!write.write_through);
        assert_eq!(
            vectors::message(|enc| write.encode(enc))[HEADER_LEN..],
            request[HEADER_LEN..]
        );

        let response = vectors::bytes(capture.write.response);
        let written = WriteResponse::decode(&response).expect("a write response");
        assert_eq!(written.count, 23);
        assert_eq!(
            vectors::message(|enc| written.encode(enc))[HEADER_LEN..],
            response[HEADER_LEN..]
        );

        let request = vectors::bytes(capture.read.request);
        let read = ReadRequest::decode(&request).expect("a read");
        assert_eq!((read.length, read.offset, read.minimum), (23, 0, 0));
        assert_eq!(read.file_id, write.file_id);
        assert_eq!(
            vectors::message(|enc| read.encode(enc))[HEADER_LEN..],
            request[HEADER_LEN..]
        );

        let response = vectors::bytes(capture.read.response);
        let data = ReadResponse::decode(&response).expect("a read response");
        assert_eq!(data.data, DATA);
        assert_eq!(
            vectors::message(|enc| data.encode(enc))[HEADER_LEN..],
            response[HEADER_LEN..]
        );
    }
}

#[test]
fn a_new_read_asks_its_data_placed_past_the_response_s_fixed_part() {
    let read = ReadRequest::new([7; 16], 4096, 65_536);
    let bytes = vectors::message(|enc| read.encode(enc));
    assert_eq!(bytes.len(), HEADER_LEN + 49);
    assert_eq!(bytes[HEADER_LEN + 2], 0x50);
    assert_eq!(ReadRequest::decode(&bytes), Ok(read));
}

#[test]
fn a_read_or_write_past_the_bound_is_refused_both_ways() {
    let at_bound = u32::try_from(MAX_IO).expect("a 32-bit bound");
    assert_eq!(
        encoded(|enc| ReadRequest::new([7; 16], 0, at_bound).encode(enc)),
        Ok(())
    );
    assert_eq!(
        encoded(|enc| ReadRequest::new([7; 16], 0, past_the_bound()).encode(enc)),
        Err(SmbError::TooLarge)
    );
    let mut read = vectors::message(|enc| ReadRequest::new([7; 16], 0, 1).encode(enc));
    patch_u32(&mut read, HEADER_LEN + 4, past_the_bound());
    assert_eq!(ReadRequest::decode(&read), Err(SmbError::TooLarge));

    let data = vec![0x5a; MAX_IO + 1];
    let write = WriteRequest {
        offset: 0,
        file_id: [7; 16],
        write_through: false,
        data: &data,
    };
    assert_eq!(encoded(|enc| write.encode(enc)), Err(SmbError::TooLarge));
    let largest = WriteRequest {
        data: &data[..MAX_IO],
        ..write
    };
    let bytes = vectors::message(|enc| largest.encode(enc));
    assert_eq!(WriteRequest::decode(&bytes), Ok(largest));
    let mut long = bytes;
    patch_u32(&mut long, HEADER_LEN + 4, past_the_bound());
    assert_eq!(WriteRequest::decode(&long), Err(SmbError::TooLarge));

    let mut response = vectors::message(|enc| ReadResponse { data: DATA }.encode(enc));
    patch_u32(&mut response, HEADER_LEN + 4, past_the_bound());
    assert_eq!(ReadResponse::decode(&response), Err(SmbError::TooLarge));
}

#[test]
fn a_read_response_s_data_lies_past_its_fixed_part_and_inside_the_message() {
    let bytes = vectors::message(|enc| ReadResponse { data: DATA }.encode(enc));
    let mut inside = bytes.clone();
    inside[HEADER_LEN + 2] = 0x4f;
    assert_eq!(ReadResponse::decode(&inside), Err(SmbError::Malformed));
    let mut past = bytes;
    past[HEADER_LEN + 4] = 24;
    assert_eq!(ReadResponse::decode(&past), Err(SmbError::Malformed));

    let empty = vectors::message(|enc| ReadResponse { data: &[] }.encode(enc));
    assert_eq!(empty.len(), HEADER_LEN + 17);
    let mut anywhere = empty;
    anywhere[HEADER_LEN + 2] = 0;
    assert_eq!(
        ReadResponse::decode(&anywhere),
        Ok(ReadResponse { data: &[] })
    );
}

#[test]
fn an_empty_write_carries_one_byte_of_buffer() {
    let write = WriteRequest {
        offset: 1 << 40,
        file_id: [9; 16],
        write_through: true,
        data: &[],
    };
    let bytes = vectors::message(|enc| write.encode(enc));
    assert_eq!(bytes.len(), HEADER_LEN + 49);
    assert_eq!(WriteRequest::decode(&bytes), Ok(write));
}

#[test]
fn a_flush_names_its_open_and_is_answered_with_an_empty_body() {
    let flush = FlushRequest { file_id: [4; 16] };
    let bytes = vectors::message(|enc| flush.encode(enc));
    assert_eq!(bytes.len(), HEADER_LEN + 24);
    assert_eq!(FlushRequest::decode(&bytes), Ok(flush));
    let answer = vectors::message(|enc| {
        enc.u16(4);
        enc.u16(0);
    });
    assert_eq!(decode_empty(&answer), Ok(()));
}

#[test]
fn a_close_reads_attributes_only_when_they_are_flagged() {
    let close = CloseRequest {
        post_query: true,
        file_id: [3; 16],
    };
    let bytes = vectors::message(|enc| close.encode(enc));
    assert_eq!((bytes.len(), bytes[HEADER_LEN + 2]), (HEADER_LEN + 24, 1));
    assert_eq!(CloseRequest::decode(&bytes), Ok(close));

    let time = Time64::from_secs(-86_400);
    let reported = CloseResponse {
        attributes: Some(ClosedAttributes {
            times: FileTimes {
                created: time,
                accessed: time,
                written: time,
                changed: time,
            },
            allocation_size: 4096,
            end_of_file: 23,
            attributes: FileAttributes::ARCHIVE,
        }),
    };
    let bytes = vectors::message(|enc| reported.encode(enc));
    assert_eq!(bytes.len(), HEADER_LEN + 60);
    assert_eq!(CloseResponse::decode(&bytes), Ok(reported));
    let mut unflagged = bytes;
    unflagged[HEADER_LEN + 2] = 0;
    assert_eq!(
        CloseResponse::decode(&unflagged),
        Ok(CloseResponse { attributes: None })
    );

    let bare = vectors::message(|enc| CloseResponse { attributes: None }.encode(enc));
    assert_eq!(bare.len(), HEADER_LEN + 60);
    assert_eq!(
        CloseResponse::decode(&bare[..bare.len() - 1]),
        Err(SmbError::Malformed)
    );
}
