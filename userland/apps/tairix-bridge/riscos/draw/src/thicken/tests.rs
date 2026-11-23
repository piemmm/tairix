use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::*;
use crate::dash::{Dasher, Pattern};
use crate::flatten::Threshold;
use crate::stage::testkit::{feed, parse, Collect, Memory};
use crate::stage::Flattener;

#[path = "recorded.rs"]
mod recorded;

const BLOCK: u32 = 0x1_0000;
const DASH: u32 = 0x2_0000;
const FLATTEN: u32 = 1 << 28;
const REFLATTEN: u32 = 1 << 30;

/// What our stages make of a recorded case: the elements, and where it
/// stopped, if it did.
fn thickened(case: &recorded::Case) -> (Vec<Element>, Result<(), Stop>) {
    let mut memory = Memory::with_words(BLOCK, &case.block);
    if let Some((start, lengths)) = case.dash {
        memory.put(DASH, &[start, u32::try_from(lengths.len()).expect("short")]);
        memory.put(DASH + 8, lengths);
    }
    let threshold = Threshold::of(0);
    let mut out = Collect::default();
    let mut reflatten;
    let tail: &mut dyn Sink = if case.flags & REFLATTEN != 0 {
        reflatten = Flattener::new(&mut out, threshold);
        &mut reflatten
    } else {
        &mut out
    };
    let mut thickener = Thickener::new(tail, case.thickness, BLOCK);
    let mut dasher;
    let middle: &mut dyn Sink = if case.dash.is_some() {
        dasher = Dasher::new(&mut thickener, Pattern::at(DASH));
        &mut dasher
    } else {
        &mut thickener
    };
    let mut flatten;
    let head: &mut dyn Sink = if case.flags & FLATTEN != 0 {
        flatten = Flattener::new(middle, threshold);
        &mut flatten
    } else {
        middle
    };
    let ran = feed(head, &mut memory, &parse(case.path));
    (out.elements, ran)
}

fn refusal_number(stop: Stop) -> Option<u32> {
    match stop {
        Stop::Refused(Refusal::BadCapJoin) => Some(0x989),
        Stop::Refused(Refusal::NeedsFlattening) => Some(0x988),
        _ => None,
    }
}

/// Whether two paths have the same elements, each point near: within
/// `SLACK`, and a further 1/20000th of its distance from the origin, which
/// a long mitre's tip strays by.
fn alike(ours: &[Element], genuine: &[Element]) -> bool {
    let near = |a: i32, b: i32| (a - b).abs() <= SLACK + b.abs() / 20000;
    ours.len() == genuine.len()
        && ours.iter().zip(genuine).all(|(a, b)| {
            a.code() == b.code()
                && a.points()
                    .iter()
                    .zip(b.points())
                    .all(|(p, q)| match (p, q) {
                        (Some(p), Some(q)) => near(p.x, q.x) && near(p.y, q.y),
                        (None, None) => true,
                        _ => false,
                    })
        })
}

/// Mitres on lines 2^20 units wide, where RISC OS's limit test overflows,
/// so it mitres or bevels where ours, which does not, would not.
const OVERFLOWED: &[&str] = &["p15_m9", "p15_m22", "p15_m23", "p15_m40", "p15_m49"];

/// `elements` split at each move.
fn subpaths(elements: &[Element]) -> Vec<&[Element]> {
    let mut starts: Vec<usize> = (0..elements.len())
        .filter(|&i| matches!(elements[i], Element::Move { .. }))
        .collect();
    starts.push(elements.len());
    starts.windows(2).map(|w| &elements[w[0]..w[1]]).collect()
}

/// A recorded case's outcome against ours.
enum Outcome {
    Exact,
    /// The same shapes, their points within the slack.
    Near,
    Wrong(String),
}

fn compare(case: &recorded::Case) -> Outcome {
    let (ours, ran) = thickened(case);
    let genuine = parse(case.made);
    let stopped = ran.err().and_then(refusal_number);
    if stopped != case.error {
        return Outcome::Wrong(format!(
            "{}: stopped {stopped:x?}, genuine {:x?}",
            case.name, case.error
        ));
    }
    let (ours_parts, genuine_parts) = (subpaths(&ours), subpaths(&genuine));
    let outside_the_join = |parts: &[&[Element]]| -> Vec<Element> {
        parts
            .iter()
            .enumerate()
            .filter(|&(i, _)| i != 1)
            .flat_map(|(_, part)| part.iter().copied())
            .collect()
    };
    if ours == genuine {
        Outcome::Exact
    } else if alike(&ours, &genuine)
        || (OVERFLOWED.contains(&case.name)
            && alike(
                &outside_the_join(&ours_parts),
                &outside_the_join(&genuine_parts),
            ))
    {
        Outcome::Near
    } else {
        Outcome::Wrong(format!(
            "{}:\n ours    {ours:?}\n genuine {genuine:?}",
            case.name
        ))
    }
}

/// How far a point may stray where only the half width's or a mitre tip's
/// low bits differ.
const SLACK: i32 = 4;

#[test]
fn every_recorded_stroke_has_genuine_risc_os_shapes_in_its_order() {
    let wrong: Vec<String> = recorded::CASES
        .iter()
        .filter_map(|case| match compare(case) {
            Outcome::Wrong(why) => Some(why),
            Outcome::Exact | Outcome::Near => None,
        })
        .collect();
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

#[test]
fn two_thirds_of_recorded_strokes_match_genuine_risc_os_to_the_unit() {
    let exact = recorded::CASES
        .iter()
        .filter(|case| matches!(compare(case), Outcome::Exact))
        .count();
    let total = recorded::CASES.len();
    assert!(exact * 3 >= total * 2, "{exact} of {total} exact");
}

#[test]
fn the_overflowed_mitres_still_join_otherwise_so_the_difference_is_still_needed() {
    for name in OVERFLOWED {
        let case = recorded::CASES
            .iter()
            .find(|case| case.name == *name)
            .expect("listed");
        let (ours, _) = thickened(case);
        assert!(!alike(&ours, &parse(case.made)), "{name}");
    }
}

#[test]
fn a_thickness_of_nought_draws_the_thinnest_line_curves_and_all() {
    let path = parse("|2 0 0 |6 0 9 9 9 9 0 |3 5 5 |8 7 7 |5");
    let mut out = Collect::default();
    feed(
        &mut Thickener::new(&mut out, 0, BLOCK),
        &mut Memory::default(),
        &path,
    )
    .expect("thin");
    assert_eq!(
        out.elements,
        parse("|3 0 0 |6 0 9 9 9 9 0 |3 5 5 |8 7 7 |5")
    );
}

#[test]
fn the_block_is_read_only_when_a_cap_or_join_needs_it() {
    let mut out = Collect::default();
    let path = parse("|2 0 0 |8 1000 0 |5");
    let ran = feed(
        &mut Thickener::new(&mut out, 100, BLOCK),
        &mut Memory::default(),
        &path,
    );
    assert_eq!(
        ran,
        Err(Stop::Fail(tairix_riscos_kernel::os::Fail::Error(BLOCK)))
    );
    assert_eq!(out.elements.len(), 7, "the hexagon went before the join");
}
