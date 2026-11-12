//! Unit tests for the RFC 1952 gzip member.

extern crate alloc;

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;

use super::{decompress, Error, Header, Member, MAGIC};
use crate::deflate::{Deflate, Flush};

/// A member holding `data`, compressed for real.
fn member(data: &[u8]) -> Vec<u8> {
    let mut out = vec![MAGIC[0], MAGIC[1], 8, 0, 0, 0, 0, 0, 0, 3];
    let mut deflate = Box::new(Deflate::new());
    let mut body = vec![0; deflate.bound(data.len())];
    let len = deflate.deflate(data, &mut body, Flush::Finish).unwrap();
    out.extend_from_slice(&body[..len]);
    out.extend_from_slice(&tairix_crc32::checksum(data).to_le_bytes());
    out.extend_from_slice(&u32::try_from(data.len()).unwrap().to_le_bytes());
    out
}

fn decoded(src: &[u8], limit: usize) -> Result<Vec<u8>, Error> {
    let mut member = Box::new(Member::new());
    let mut scratch = [0; 97];
    let mut out = Vec::new();
    decompress(src, &mut member, &mut scratch, |piece| {
        out.extend_from_slice(piece);
        out.len() <= limit
    })?;
    Ok(out)
}

fn text(len: usize) -> Vec<u8> {
    (0..len)
        .map(|at| b"the quick brown fox "[at % 20])
        .collect()
}

#[test]
fn a_header_is_read_field_by_field() {
    let mut header = vec![0x1f, 0x8b, 8, 0x08, 1, 2, 3, 4, 0, 3];
    header.extend_from_slice(b"report.txt\0");
    let read = Header::parse(&header).unwrap();
    assert_eq!(read.len, header.len());
    assert_eq!(&header[read.name.clone().unwrap()], b"report.txt");
    assert_eq!(read.mtime, 0x0403_0201);
    let mut reserved = header.clone();
    reserved[3] |= 0x20;
    assert_eq!(Header::parse(&reserved), None);
    assert_eq!(
        Header::parse(&[0x1f, 0x8b, 8, 0x08, 0, 0, 0, 0, 0, 3, b'a']),
        None
    );
    assert_eq!(
        Header::parse(&[0x1f, 0x8b, 9, 0, 0, 0, 0, 0, 0, 3]),
        None,
        "not deflate"
    );
}

#[test]
fn a_header_checksum_must_hold() {
    let mut header = vec![0x1f, 0x8b, 8, 0x02, 0, 0, 0, 0, 0, 255];
    let crc = u16::try_from(tairix_crc32::checksum(&header) & 0xFFFF).unwrap();
    header.extend_from_slice(&crc.to_le_bytes());
    assert_eq!(Header::parse(&header).unwrap().len, 12);
    let at = header.len() - 1;
    header[at] ^= 1;
    assert_eq!(Header::parse(&header), None);
}

#[test]
fn a_stream_decodes_member_after_member_and_stops_at_zero_padding() {
    let first = text(5000);
    let second = text(300);
    let mut stream = member(&first);
    stream.extend_from_slice(&member(&second));
    stream.extend_from_slice(&[0; 16]);
    let mut expected = first;
    expected.extend_from_slice(&second);
    assert_eq!(decoded(&stream, usize::MAX).unwrap(), expected);
}

#[test]
fn a_member_whose_data_misses_its_trailer_is_refused() {
    let mut stream = member(&text(100));
    let crc = stream.len() - 8;
    stream[crc] ^= 1;
    assert_eq!(decoded(&stream, usize::MAX), Err(Error::Trailer));
    let mut length = member(&text(100));
    let at = length.len() - 1;
    length[at] ^= 1;
    assert_eq!(decoded(&length, usize::MAX), Err(Error::Trailer));
}

#[test]
fn a_stream_ending_inside_a_member_is_truncated() {
    let stream = member(&text(1000));
    assert_eq!(
        decoded(&stream[..stream.len() - 3], usize::MAX),
        Err(Error::Truncated)
    );
    assert!(matches!(
        decoded(&stream[..20], usize::MAX),
        Err(Error::Truncated | Error::Body(_))
    ));
    assert_eq!(decoded(&[], usize::MAX), Err(Error::Header));
}

#[test]
fn bytes_after_a_member_that_are_not_padding_must_be_another_member() {
    let mut stream = member(&text(10));
    stream.extend_from_slice(b"junk");
    assert_eq!(decoded(&stream, usize::MAX), Err(Error::Header));
}

#[test]
fn the_sink_bounds_the_output() {
    let stream = member(&text(10_000));
    assert_eq!(decoded(&stream, 4096), Err(Error::Refused));
}

#[test]
fn a_resumed_member_cannot_be_held_to_its_trailer() {
    let mut fresh = Member::new();
    fresh.begin();
    assert!(!fresh.verify(&[1; 8]), "a member from its start is checked");
    let mut resumed = Member::new();
    let checkpoint = crate::inflate::Checkpoint::default();
    resumed.resume(&checkpoint);
    assert!(resumed.verify(&[1; 8]));
}
