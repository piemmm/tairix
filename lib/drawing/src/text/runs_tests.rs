//! Tests for runs: what a text may hold of them, and the edits that keep them
//! following its content.

use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_bezier::Point;
use tairix_collections::SharedSlice;
use tairix_colour::Rgba;

use super::{Run, RunPaint, RunStyle};
use crate::limits::{MAX_BASELINE_SHIFT, MAX_TEXT_BYTES, MAX_TEXT_FAMILIES, MAX_TEXT_RUNS};
use crate::testing::shared;
use crate::text::{Baseline, Text, TextAlign};
use crate::DrawingError;

fn text(content: &str) -> Text {
    Text {
        content: shared(content),
        family: shared("sans-serif"),
        size: 10.0,
        weight: 400,
        italic: false,
        letter_spacing: 0.0,
        align: TextAlign::Start,
        baseline: Baseline::Lines {
            origin: Point::ZERO,
            line_height: 1.2,
        },
        runs: SharedSlice::new(),
    }
}

fn bold() -> RunStyle {
    RunStyle {
        weight: Some(700),
        ..RunStyle::default()
    }
}

fn run(start: usize, end: usize, style: RunStyle) -> Run {
    Run {
        start,
        end,
        style: Arc::new(style),
    }
}

fn with_runs(content: &str, runs: Vec<Run>) -> Text {
    Text {
        runs: SharedSlice::try_from_vec(runs).expect("held"),
        ..text(content)
    }
}

/// Each run as its span and what it overrides.
fn spans(text: &Text) -> Vec<(usize, usize, RunStyle)> {
    text.runs
        .iter()
        .map(|run| (run.start, run.end, (*run.style).clone()))
        .collect()
}

#[test]
fn restyling_a_span_parts_it_from_its_neighbours_and_joins_what_comes_to_match() {
    let plain = text("Hello world");
    let emboldened = plain.restyled(0..5, |_| bold()).expect("restyled");
    assert_eq!(spans(&emboldened), [(0, 5, bold())]);
    let slanted = emboldened
        .restyled(3..8, |style| RunStyle {
            italic: Some(true),
            ..style.clone()
        })
        .expect("restyled");
    let both = RunStyle {
        italic: Some(true),
        ..bold()
    };
    let italic = RunStyle {
        italic: Some(true),
        ..RunStyle::default()
    };
    assert_eq!(
        spans(&slanted),
        [(0, 3, bold()), (3, 5, both), (5, 8, italic.clone())]
    );
    let upright = slanted
        .restyled(3..5, |style| RunStyle {
            italic: None,
            ..style.clone()
        })
        .expect("restyled");
    assert_eq!(
        spans(&upright),
        [(0, 5, bold()), (5, 8, italic)],
        "neighbours that come to set the same are one run"
    );
    let cleared = upright
        .restyled(0..11, |_| RunStyle::default())
        .expect("restyled");
    assert!(
        cleared.runs.is_empty(),
        "a style overriding nothing is no run"
    );
    assert_eq!(cleared.check(), Ok(()));
}

#[test]
fn typed_text_takes_the_style_of_the_character_before_it() {
    let styled = with_runs("abcdef", alloc::vec![run(2, 4, bold())]);
    let before = styled.replaced(0..0, "XY").expect("typed");
    assert_eq!(&*before.content, "XYabcdef");
    assert_eq!(
        spans(&before),
        [(4, 6, bold())],
        "at the start, the style of the character after"
    );
    let within = styled.replaced(3..3, "Z").expect("typed");
    assert_eq!(&*within.content, "abcZdef");
    assert_eq!(spans(&within), [(2, 5, bold())]);
    let at_end = styled.replaced(4..4, "Z").expect("typed");
    assert_eq!(
        spans(&at_end),
        [(2, 5, bold())],
        "typing on from a run continues it"
    );
    let after = styled.replaced(5..5, "Z").expect("typed");
    assert_eq!(spans(&after), [(2, 4, bold())]);
    let lead = with_runs("abc", alloc::vec![run(0, 1, bold())]);
    assert_eq!(
        spans(&lead.replaced(0..0, "Z").expect("typed")),
        [(0, 2, bold())]
    );
}

#[test]
fn replacing_a_selection_keeps_its_first_character_s_style_and_moves_what_follows() {
    let styled = with_runs(
        "abcdef",
        alloc::vec![
            run(2, 4, bold()),
            run(
                5,
                6,
                RunStyle {
                    size: Some(20.0),
                    ..RunStyle::default()
                }
            )
        ],
    );
    let over = styled.replaced(2..4, "XYZ").expect("replaced");
    assert_eq!(&*over.content, "abXYZef");
    assert_eq!(spans(&over)[0], (2, 5, bold()));
    assert_eq!(
        spans(&over)[1].0..spans(&over)[1].1,
        6..7,
        "moved with the text"
    );
    let across = styled.replaced(1..3, "").expect("deleted");
    assert_eq!(&*across.content, "adef");
    assert_eq!(
        spans(&across)[0],
        (1, 2, bold()),
        "cut where the range began"
    );
    let all = styled.replaced(0..6, "").expect("deleted");
    assert!(all.runs.is_empty());
    assert_eq!(all.check(), Ok(()));
}

#[test]
fn edits_hold_their_ends_to_character_boundaries() {
    let accented = with_runs("aéb", alloc::vec![run(1, 3, bold())]);
    let within = accented.replaced(2..2, "Z").expect("typed");
    assert_eq!(
        &*within.content, "aZéb",
        "a byte within é stands for its start"
    );
    let restyled = accented
        .restyled(2..4, |_| RunStyle::default())
        .expect("restyled");
    assert_eq!(spans(&restyled), [], "é's span is 1..3, all of it cleared");
    assert_eq!(restyled.check(), Ok(()));
    let past = text("ab").replaced(7..9, "c").expect("typed");
    assert_eq!(&*past.content, "abc", "past the end is the end");
}

#[test]
fn run_at_finds_the_run_holding_a_character() {
    let styled = with_runs("abcdef", alloc::vec![run(1, 2, bold()), run(3, 5, bold())]);
    let held: Vec<Option<usize>> = (0..7)
        .map(|at| styled.run_at(at).map(|run| run.start))
        .collect();
    assert_eq!(held, [None, Some(1), None, Some(3), Some(3), None, None]);
}

#[test]
fn a_text_refuses_runs_out_of_their_form() {
    let refused = |runs: Vec<Run>| with_runs("abcdé", runs).check();
    assert_eq!(
        refused(alloc::vec![run(0, 2, bold()), run(3, 4, bold())]),
        Ok(())
    );
    assert_eq!(
        refused(alloc::vec![
            run(0, 2, bold()),
            run(1, 3, RunStyle::default())
        ]),
        Err(DrawingError::Invalid),
        "overlapping"
    );
    assert_eq!(
        refused(alloc::vec![run(2, 3, bold()), run(0, 1, bold())]),
        Err(DrawingError::Invalid),
        "out of order"
    );
    assert_eq!(
        refused(alloc::vec![run(0, 2, bold()), run(2, 3, bold())]),
        Err(DrawingError::Invalid),
        "touching neighbours that set the same"
    );
    assert_eq!(
        refused(alloc::vec![run(2, 2, bold())]),
        Err(DrawingError::Invalid),
        "empty"
    );
    assert_eq!(
        refused(alloc::vec![run(0, 7, bold())]),
        Err(DrawingError::Invalid),
        "past the end"
    );
    assert_eq!(
        refused(alloc::vec![run(0, 5, bold())]),
        Err(DrawingError::Invalid),
        "within é"
    );
    assert_eq!(
        refused(alloc::vec![run(0, 1, RunStyle::default())]),
        Err(DrawingError::Invalid),
        "overriding nothing"
    );
    for wrong in [
        RunStyle {
            size: Some(0.0),
            ..RunStyle::default()
        },
        RunStyle {
            weight: Some(1001),
            ..RunStyle::default()
        },
        RunStyle {
            baseline_shift: Some(MAX_BASELINE_SHIFT * 1.5),
            ..RunStyle::default()
        },
        RunStyle {
            letter_spacing: Some(f64::NAN),
            ..RunStyle::default()
        },
        RunStyle {
            family: Some(shared("bell\u{7}")),
            ..RunStyle::default()
        },
    ] {
        assert_eq!(
            refused(alloc::vec![run(0, 1, wrong.clone())]),
            Err(DrawingError::Invalid),
            "{wrong:?}"
        );
    }
}

#[test]
fn a_text_holds_no_more_runs_or_families_than_its_bounds() {
    let long: alloc::string::String = core::iter::repeat_n('x', 2 * MAX_TEXT_RUNS + 2).collect();
    let many = |count: usize| {
        (0..count)
            .map(|index| run(2 * index, 2 * index + 1, bold()))
            .collect::<Vec<_>>()
    };
    assert_eq!(with_runs(&long, many(MAX_TEXT_RUNS)).check(), Ok(()));
    assert_eq!(
        with_runs(&long, many(MAX_TEXT_RUNS + 1)).check(),
        Err(DrawingError::TooComplex)
    );
    let families = |count: usize| {
        (0..count)
            .map(|index| {
                run(
                    index,
                    index + 1,
                    RunStyle {
                        family: Some(shared(&alloc::format!("family {index}"))),
                        ..RunStyle::default()
                    },
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        with_runs(&long, families(MAX_TEXT_FAMILIES)).check(),
        Ok(())
    );
    assert_eq!(
        with_runs(&long, families(MAX_TEXT_FAMILIES + 1)).check(),
        Err(DrawingError::TooComplex)
    );
}

#[test]
fn an_edit_past_the_content_s_bound_is_refused() {
    let full: alloc::string::String = core::iter::repeat_n('x', MAX_TEXT_BYTES).collect();
    assert_eq!(
        text(&full).replaced(0..0, "y"),
        Err(DrawingError::TooComplex)
    );
}

#[test]
fn a_run_paint_is_a_colour_or_nothing() {
    let red = Rgba::new(255, 0, 0, 255);
    assert_eq!(
        RunPaint::of(&crate::style::Paint::Colour(red)),
        Some(RunPaint::Colour(red))
    );
    assert_eq!(
        RunPaint::of(&crate::style::Paint::None),
        Some(RunPaint::None)
    );
    assert_eq!(
        RunPaint::Colour(red).paint(),
        crate::style::Paint::Colour(red)
    );
}
