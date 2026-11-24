use alloc::vec::Vec;

use tairix_riscos_filing::session::Session;
use tairix_riscos_kernel::vectors;

use super::*;
use crate::fixture::{fonts, BLOCK};

/// Where a test's claimant of `GbpbV` is.
const CODE: u32 = 0xC000;
/// The instructions it is made of: `MOV R3, #0`, `MOV R3, #1000`,
/// `MOV R4, #0`, `MVN R4, #0`, and `LDR PC, [R13], #4`, which claims the
/// call.
const READ_NONE: u32 = 0xE3A0_3000;
const READ_THOUSAND: u32 = 0xE3A0_3FFA;
const NEXT_START: u32 = 0xE3A0_4000;
const NEXT_NONE: u32 = 0xE3E0_4000;
const CLAIMED: u32 = 0xE49D_F004;

/// `GbpbV` claimed by code answering every call with `count` and `next`, a
/// read's records and its next offset.
fn answering(s: &mut Session, count: u32, next: u32) {
    let mut os = s.kernel.os();
    for (at, word) in (CODE..).step_by(4).zip([count, next, CLAIMED]) {
        os.write_word(at, word).expect("written");
    }
    let mut regs = Regs::default();
    regs.r[..3].copy_from_slice(&[vectors::GBPB.0, CODE, 0]);
    os.perform(X | kos::CLAIM, &mut regs).expect("claimed");
}

fn listed(s: &mut Session, budget: &mut usize) -> Result<Vec<Entry>, Fail> {
    listing(
        &mut s.kernel.os(),
        b"HostFS:$.Lib",
        budget,
        Unreadable::Fails,
    )
}

#[test]
fn a_read_s_records_are_those_wholly_within_its_room() {
    let mut s = fonts();
    let mut os = s.kernel.os();
    let mut room = alloc::vec![0u8; ENTRIES_ROOM as usize];
    let mut at = 0;
    for (kind, name) in [(FILE, &b"IntMetric0"[..]), (DIRECTORY, b"Encodings")] {
        let (kind_at, name_at) = (at + KIND_AT as usize, at + NAME_AT as usize);
        room[kind_at..kind_at + 4].copy_from_slice(&kind.to_le_bytes());
        room[name_at..name_at + name.len()].copy_from_slice(name);
        at = (name_at + name.len() + 1).next_multiple_of(4);
    }
    room[at + NAME_AT as usize..].fill(b'x');
    os.write_bytes(BLOCK, &room).expect("written");
    let read = records(&mut os, BLOCK, 42).expect("read");
    let found: Vec<(u32, &[u8])> = read.iter().map(|e| (e.kind, e.name.as_slice())).collect();
    assert_eq!(
        found,
        [(FILE, &b"IntMetric0"[..]), (DIRECTORY, b"Encodings")],
        "not the third, which runs past the room"
    );
    assert_eq!(records(&mut os, BLOCK, 1).expect("read").len(), 1);
}

#[test]
fn a_filing_system_reading_nothing_short_of_the_end_ends_the_listing() {
    let mut s = fonts();
    answering(&mut s, READ_NONE, NEXT_START);
    let mut budget = ENTRIES_MOST;
    assert!(listed(&mut s, &mut budget).expect("listed").is_empty());
    assert_eq!(budget, ENTRIES_MOST);
}

#[test]
fn a_filing_system_claiming_more_records_than_asked_gives_no_more() {
    let mut s = fonts();
    answering(&mut s, READ_THOUSAND, NEXT_NONE);
    let mut budget = 3;
    let found = listed(&mut s, &mut budget).expect("listed");
    assert!(found.len() <= 3, "{} records", found.len());
    assert_eq!(budget, 3 - found.len());
}

#[test]
fn a_path_is_read_no_further_than_its_most_elements() {
    let mut s = fonts();
    let mut os = s.kernel.os();
    let path = alloc::vec!["HostFS:$.None."; PATH_ELEMENTS_MOST + 10].join(",");
    os.set_string_variable(b"Font$Path", path.as_bytes())
        .expect("set");
    let elements = with_catalogue(&mut os, true, |_, catalogue| Ok(catalogue.elements.len()));
    assert_eq!(elements.expect("a catalogue"), PATH_ELEMENTS_MOST);
}
