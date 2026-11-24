use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::*;
use crate::stage::testkit::{alike, feed, parse, Collect, Memory};

#[path = "recorded.rs"]
mod recorded;

const BLOCK: u32 = 0x1_0000;

/// The recorded cases whose dashes cross a slanting segment many times,
/// where RISC OS's points stray from the shares worked here.
const SLANTED: &[&str] = &[
    "p16_diag_d1",
    "p16_diag_d2",
    "p16_diag_odd",
    "p16_diag_many",
];
/// How far they stray: six 256ths of an OS unit at worst.
const SLACK: i32 = 6;

fn dashed(case: &recorded::Case) -> Result<Vec<Element>, Stop> {
    let mut memory = Memory::with_words(BLOCK, &[case.start, case.count]);
    memory.put(BLOCK + 8, case.lengths);
    let mut out = Collect::default();
    feed(
        &mut Dasher::new(&mut out, Pattern::at(BLOCK)),
        &mut memory,
        &parse(case.path),
    )?;
    Ok(out.elements)
}

#[test]
fn every_recorded_dash_is_cut_where_genuine_risc_os_cut_it() {
    let mut wrong: Vec<String> = Vec::new();
    for case in recorded::CASES {
        let ours = dashed(case).expect("dashed");
        let genuine = parse(case.dashed);
        let matched = if SLANTED.contains(&case.name) {
            alike(&ours, &genuine, |a, b| (a - b).abs() <= SLACK)
        } else {
            ours == genuine
        };
        if !matched {
            wrong.push(format!(
                "{}:\n ours    {ours:?}\n genuine {genuine:?}",
                case.name
            ));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

#[test]
fn the_slanted_cases_still_differ_so_the_known_difference_is_still_needed() {
    let exact = SLANTED.iter().filter(|name| {
        let case = recorded::CASES
            .iter()
            .find(|case| case.name == **name)
            .expect("listed");
        dashed(case).expect("dashed") == parse(case.dashed)
    });
    assert!(exact.count() < SLANTED.len());
}

#[test]
fn a_pattern_of_no_length_leaves_the_path_undashed() {
    let mut memory = Memory::with_words(BLOCK, &[5000, 2, 0, 0]);
    let path = parse("|2 0 0 |8 30000 0 |8 30000 9000 |5");
    let mut out = Collect::default();
    feed(
        &mut Dasher::new(&mut out, Pattern::at(BLOCK)),
        &mut memory,
        &path,
    )
    .expect("run");
    assert_eq!(out.elements, path);
}

#[test]
fn a_huge_start_offset_is_found_without_walking_every_cycle() {
    let mut memory = Memory::with_words(BLOCK, &[u32::MAX - 1, 2, 1, 2]);
    let mut out = Collect::default();
    let path = parse("|2 0 0 |8 6 0");
    feed(
        &mut Dasher::new(&mut out, Pattern::at(BLOCK)),
        &mut memory,
        &path,
    )
    .expect("run");
    // The offset lands one unit into the off element of the three-unit cycle.
    assert_eq!(
        out.elements,
        parse("|2 0 0 |7 1 0 |8 2 0 |7 4 0 |8 5 0 |7 6 0")
    );
}

#[test]
fn the_lengths_are_read_only_as_dashing_reaches_them() {
    // A count far past the block: only the lengths a short path needs are read.
    let mut memory = Memory::with_words(BLOCK, &[0, u32::MAX, 10, 20, 30]);
    let mut out = Collect::default();
    feed(
        &mut Dasher::new(&mut out, Pattern::at(BLOCK)),
        &mut memory,
        &parse("|2 0 0 |8 55 0"),
    )
    .expect("three lengths suffice");
    assert_eq!(out.elements, parse("|2 0 0 |8 10 0 |7 30 0 |8 55 0"));
    let mut out = Collect::default();
    let failed = feed(
        &mut Dasher::new(&mut out, Pattern::at(BLOCK)),
        &mut memory,
        &parse("|2 0 0 |8 70 0"),
    );
    assert_eq!(
        failed,
        Err(Stop::Fail(tairix_riscos_kernel::os::Fail::Error(
            BLOCK + 8 + 12
        )))
    );
}

#[test]
fn an_empty_path_never_reads_the_block() {
    let mut out = Collect::default();
    feed(
        &mut Dasher::new(&mut out, Pattern::at(BLOCK)),
        &mut Memory::default(),
        &[],
    )
    .expect("nothing read");
    assert!(out.finished);
}

#[test]
fn a_curve_must_be_flattened_first() {
    let mut memory = Memory::with_words(BLOCK, &[0, 2, 10, 10]);
    let mut out = Collect::default();
    let failed = feed(
        &mut Dasher::new(&mut out, Pattern::at(BLOCK)),
        &mut memory,
        &parse("|2 0 0 |6 0 9 9 9 9 0"),
    );
    assert_eq!(failed, Err(Stop::Refused(Refusal::NeedsFlattening)));
    assert_eq!(
        out.elements,
        parse("|2 0 0"),
        "the move went before the curve"
    );
}
