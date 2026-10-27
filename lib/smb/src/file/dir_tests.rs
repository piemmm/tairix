//! Unit tests for `QUERY_DIRECTORY` and the walk of its entries.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::time::Time64;

use super::{
    write_entries, EntryRecord, ListingFlags, QueryDirectoryRequest, QueryDirectoryResponse,
    MAX_LISTING,
};
use crate::file::{FileAttributes, FileTimes};
use crate::header::HEADER_LEN;
use crate::vectors;
use crate::wire::Encoder;
use crate::SmbError;

/// A symbolic link's reparse tag (MS-FSCC §2.1.2.1).
const SYMLINK_TAG: u32 = 0xa000_000c;

fn le32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

fn record(name: &str, attributes: FileAttributes) -> EntryRecord<'_> {
    let time = Time64::from_secs(4_102_444_800);
    EntryRecord {
        file_index: 0,
        times: FileTimes {
            created: time,
            accessed: time,
            written: time,
            changed: time,
        },
        end_of_file: 23,
        allocation_size: 4096,
        attributes,
        ea_size: 0,
        file_id: 0x0001_0000_0000_0042,
        name,
    }
}

fn listing(records: &[EntryRecord<'_>]) -> Vec<u8> {
    let mut enc = Encoder::new();
    write_entries(&mut enc, records);
    enc.finish().expect("encodes")
}

/// Each entry's offset in `output`, by its `Next`.
fn offsets(output: &[u8]) -> Vec<usize> {
    let mut at = 0;
    let mut offsets = Vec::from([0]);
    while let next @ 1.. = le32(output, at) {
        at += usize::try_from(next).expect("an offset");
        offsets.push(at);
    }
    offsets
}

fn walked(output: &[u8]) -> Vec<Result<Option<String>, SmbError>> {
    QueryDirectoryResponse { output }
        .entries()
        .map(|entry| entry.map(|entry| entry.name().ok()))
        .collect()
}

#[test]
fn a_listing_asks_for_every_entry_of_its_one_class() {
    let request = QueryDirectoryRequest {
        flags: ListingFlags::RESTART_SCANS,
        file_index: 0,
        file_id: [8; 16],
        output_length: 65_536,
    };
    let bytes = vectors::message(|enc| request.encode(enc));
    assert_eq!(
        bytes[HEADER_LEN + 2],
        0x25,
        "FileIdBothDirectoryInformation"
    );
    assert_eq!(
        le32(&bytes, HEADER_LEN + 24),
        0x0002_0060,
        "the pattern at 96"
    );
    assert_eq!(bytes[96..], [b'*', 0]);
    assert_eq!(QueryDirectoryRequest::decode(&bytes), Ok(request));

    let mut other = bytes;
    other[HEADER_LEN + 2] = 0x01;
    assert_eq!(
        QueryDirectoryRequest::decode(&other),
        Err(SmbError::Malformed)
    );

    let greedy = QueryDirectoryRequest {
        output_length: u32::try_from(MAX_LISTING + 1).expect("a length"),
        ..request
    };
    let mut enc = Encoder::new();
    greedy.encode(&mut enc);
    assert_eq!(enc.finish(), Err(SmbError::TooLarge));
}

#[test]
fn a_listing_walks_its_entries_in_place_eight_byte_aligned() {
    let records = [
        record(".", FileAttributes::DIRECTORY),
        record("..", FileAttributes::DIRECTORY),
        record("report.txt", FileAttributes::ARCHIVE),
        EntryRecord {
            ea_size: SYMLINK_TAG,
            ..record("link", FileAttributes::REPARSE_POINT)
        },
    ];
    let output = listing(&records);
    let offsets = offsets(&output);
    assert_eq!(offsets.len(), records.len());
    assert!(offsets.iter().all(|at| at.is_multiple_of(8)), "{offsets:?}");

    let bytes = vectors::message(|enc| QueryDirectoryResponse { output: &output }.encode(enc));
    let response = QueryDirectoryResponse::decode(&bytes).expect("decodes");
    let entries: Vec<_> = response
        .entries()
        .collect::<Result<_, _>>()
        .expect("well-formed");
    assert_eq!(entries.len(), 4);
    assert!(entries[0].is_dot() && entries[1].is_dot() && !entries[2].is_dot());
    assert_eq!(entries[1].name(), Err(SmbError::Name));
    assert_eq!(entries[2].name(), Ok(String::from("report.txt")));
    assert_eq!(
        (
            entries[2].end_of_file,
            entries[2].allocation_size,
            entries[2].file_id
        ),
        (23, 4096, 0x0001_0000_0000_0042)
    );
    assert_eq!(entries[2].times, records[2].times);
    assert_eq!(entries[2].reparse_tag, None);
    assert_eq!(entries[3].reparse_tag, Some(SYMLINK_TAG));
}

#[test]
fn a_name_either_side_refuses_costs_its_entry_not_the_listing() {
    let output = listing(&[
        record("a:stream", FileAttributes::NONE),
        record("x*", FileAttributes::NONE),
        record("kept", FileAttributes::NONE),
    ]);
    assert_eq!(
        walked(&output),
        [Ok(None), Ok(None), Ok(Some(String::from("kept")))]
    );
}

#[test]
fn a_broken_entry_ends_the_walk_there() {
    let records = [
        record("first", FileAttributes::NONE),
        record("second", FileAttributes::NONE),
    ];
    let output = listing(&records);
    let second = offsets(&output)[1];

    let mut misaligned = output.clone();
    misaligned[0] += 1;
    let mut overlapping = output.clone();
    overlapping[..4].copy_from_slice(&104_u32.to_le_bytes());
    let mut past = output.clone();
    past[..4].copy_from_slice(&0x1000_u32.to_le_bytes());
    for broken in [misaligned, overlapping] {
        assert_eq!(walked(&broken), [Err(SmbError::Malformed)]);
    }
    assert_eq!(
        walked(&past),
        [Ok(Some(String::from("first"))), Err(SmbError::Malformed)]
    );
    assert_eq!(
        walked(&output[..second + 50]),
        [Ok(Some(String::from("first"))), Err(SmbError::Malformed)]
    );
    assert_eq!(
        walked(&listing(&[
            record("", FileAttributes::NONE),
            record("after", FileAttributes::NONE),
        ])),
        [Err(SmbError::Malformed)]
    );
}

#[test]
fn an_empty_listing_has_no_entries() {
    assert!(walked(&[]).is_empty());
    let bytes = vectors::message(|enc| QueryDirectoryResponse { output: &[] }.encode(enc));
    assert_eq!(bytes.len(), HEADER_LEN + 9);
    let response = QueryDirectoryResponse::decode(&bytes).expect("decodes");
    assert_eq!(response.entries().count(), 0);
}

#[test]
fn a_listing_past_its_bound_is_refused() {
    let mut bytes = vectors::message(|enc| QueryDirectoryResponse { output: &[0] }.encode(enc));
    let len = u32::try_from(MAX_LISTING + 1).expect("a length");
    bytes[HEADER_LEN + 4..HEADER_LEN + 8].copy_from_slice(&len.to_le_bytes());
    assert_eq!(
        QueryDirectoryResponse::decode(&bytes),
        Err(SmbError::TooLarge)
    );
}
