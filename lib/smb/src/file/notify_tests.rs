//! Unit tests for `CHANGE_NOTIFY` and the walk of its records.

use alloc::vec::Vec;

use super::{
    write_changes, Action, Change, ChangeNotifyRequest, ChangeNotifyResponse, NotifyFilter,
    MAX_CHANGES,
};
use crate::file::name::SharePath;
use crate::header::HEADER_LEN;
use crate::vectors;
use crate::wire::Encoder;
use crate::SmbError;

fn shared(names: &[&str]) -> SharePath {
    names
        .iter()
        .try_fold(SharePath::root(), |path, name| path.join(name))
        .expect("a path")
}

fn change(action: Action, names: &[&str]) -> Change {
    Change {
        action: Some(action),
        path: shared(names),
    }
}

fn records(changes: &[Change]) -> Vec<u8> {
    let mut enc = Encoder::new();
    write_changes(&mut enc, changes);
    enc.finish().expect("encodes")
}

/// One record as a server writes it.
fn record(next: u32, action: u32, name: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&next.to_le_bytes());
    out.extend_from_slice(&action.to_le_bytes());
    out.extend_from_slice(&u32::try_from(name.len()).expect("a name").to_le_bytes());
    out.extend_from_slice(name);
    out
}

fn walked(output: &[u8]) -> Vec<Result<Change, SmbError>> {
    ChangeNotifyResponse { output }.changes().collect()
}

#[test]
fn a_watch_names_its_directory_its_filter_and_its_reach() {
    let watch = ChangeNotifyRequest {
        watch_tree: true,
        output_length: 4096,
        file_id: [5; 16],
        filter: NotifyFilter::FILE_NAME
            .with(NotifyFilter::DIR_NAME)
            .with(NotifyFilter::LAST_WRITE),
    };
    let bytes = vectors::message(|enc| watch.encode(enc));
    assert_eq!(bytes.len(), HEADER_LEN + 32);
    assert_eq!(bytes[HEADER_LEN + 2], 1, "the whole subtree");
    assert_eq!(bytes[HEADER_LEN + 24], 0x13, "the filter");
    assert_eq!(ChangeNotifyRequest::decode(&bytes), Ok(watch));

    let greedy = ChangeNotifyRequest {
        output_length: u32::try_from(MAX_CHANGES + 1).expect("a length"),
        ..watch
    };
    let mut enc = Encoder::new();
    greedy.encode(&mut enc);
    assert_eq!(enc.finish(), Err(SmbError::TooLarge));
}

#[test]
fn changes_walk_four_byte_aligned_with_paths_beneath_the_watch() {
    let changes = [
        change(Action::Added, &["new.txt"]),
        change(Action::RenamedOld, &["docs", "old name"]),
        change(Action::RenamedNew, &["docs", "new name"]),
        change(Action::Removed, &["a"]),
        change(Action::Modified, &["docs", "2026", "report.txt"]),
    ];
    let output = records(&changes);
    let mut at = 0;
    while let next @ 1.. = u32::from_le_bytes(output[at..at + 4].try_into().expect("a field")) {
        at += usize::try_from(next).expect("an offset");
        assert!(at.is_multiple_of(4), "a record at {at}");
    }
    let bytes = vectors::message(|enc| ChangeNotifyResponse { output: &output }.encode(enc));
    let response = ChangeNotifyResponse::decode(&bytes).expect("decodes");
    let walked: Vec<_> = response
        .changes()
        .collect::<Result<_, _>>()
        .expect("well-formed");
    assert_eq!(walked, changes);
}

#[test]
fn a_change_no_path_names_keeps_its_path_and_loses_its_action() {
    let name = [b'a', 0];
    let output = record(0, 6, &name);
    assert_eq!(
        walked(&output),
        [Ok(Change {
            action: None,
            path: shared(&["a"]),
        })]
    );
}

#[test]
fn a_path_either_side_refuses_costs_its_record_not_the_response() {
    let mut output = record(20, 1, &[b'a', 0, b':', 0, b'b', 0]);
    output.extend_from_slice(&[0; 2]);
    output.extend_from_slice(&record(0, 2, &[b'b', 0]));
    assert_eq!(
        walked(&output),
        [Err(SmbError::Name), Ok(change(Action::Removed, &["b"]))]
    );
}

#[test]
fn a_broken_record_ends_the_walk_there() {
    let name = [b'a', 0];
    let mut misaligned = record(15, 1, &name);
    misaligned.resize(15, 0);
    misaligned.extend_from_slice(&record(0, 1, &name));
    let mut overlapping = record(12, 1, &name);
    overlapping.extend_from_slice(&record(0, 1, &name));
    let mut truncated = record(16, 1, &name);
    truncated.resize(16, 0);
    truncated.extend_from_slice(&record(0, 1, &name)[..13]);
    assert_eq!(walked(&misaligned), [Err(SmbError::Malformed)]);
    assert_eq!(walked(&overlapping), [Err(SmbError::Malformed)]);
    assert_eq!(
        walked(&truncated),
        [Ok(change(Action::Added, &["a"])), Err(SmbError::Malformed)]
    );
    let mut nameless = record(12, 1, &[]);
    nameless.extend_from_slice(&record(0, 1, &name));
    assert_eq!(walked(&nameless), [Err(SmbError::Malformed)]);
}

#[test]
fn more_change_than_a_server_could_say_carries_no_records() {
    let bytes = vectors::message(|enc| ChangeNotifyResponse { output: &[] }.encode(enc));
    assert_eq!(bytes.len(), HEADER_LEN + 9);
    let response = ChangeNotifyResponse::decode(&bytes).expect("decodes");
    assert_eq!(response.changes().count(), 0);

    let mut long = bytes;
    let len = u32::try_from(MAX_CHANGES + 1).expect("a length");
    long[HEADER_LEN + 4..HEADER_LEN + 8].copy_from_slice(&len.to_le_bytes());
    assert_eq!(ChangeNotifyResponse::decode(&long), Err(SmbError::TooLarge));
}
