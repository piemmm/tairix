//! Host tests of the grid cell arithmetic.

use super::{GridFill, GridRun};

#[test]
fn a_fixed_pitch_run_keeps_the_pitch_from_its_leading_edge() {
    // 100 px holds three 24 px cells 8 apart (24 + 32 + 32 = 88), and the
    // remainder stays at the far end.
    let run = GridRun::new(100, 24, 8, GridFill::FixedPitch);
    assert_eq!(run.count(), 3);
    assert_eq!(run.stride(), 32);
    assert_eq!(run.lead(), 0);
    assert_eq!(run.cell(), 24);
    assert_eq!(run.offset(2), Some(64));
    assert_eq!(run.span(), 88);
}

#[test]
fn a_spread_run_shares_the_remainder_out_and_centres_what_is_left() {
    let run = GridRun::new(100, 24, 8, GridFill::Spread);
    assert_eq!(run.count(), 3);
    // An equal share of the extent per cell, never below the pitch.
    assert_eq!(run.stride(), 33);
    // 33 * 2 + 24 = 90 of 100, so five either side.
    assert_eq!(run.lead(), 5);
    assert_eq!(run.span(), 95);
}

#[test]
fn a_run_its_cells_fit_exactly_is_the_same_under_either_fill() {
    // Three cells of 24 with gaps of 8 fill 88 exactly.
    let fixed = GridRun::new(88, 24, 8, GridFill::FixedPitch);
    let spread = GridRun::new(88, 24, 8, GridFill::Spread);
    assert_eq!(fixed.count(), 3);
    assert_eq!(spread.stride(), fixed.stride());
    assert_eq!(spread.lead(), fixed.lead());
}

#[test]
fn an_axis_too_short_for_one_cell_holds_none() {
    for fill in [GridFill::FixedPitch, GridFill::Spread] {
        assert_eq!(GridRun::new(23, 24, 8, fill).count(), 0);
        assert_eq!(GridRun::new(100, 0, 8, fill).count(), 0);
        assert_eq!(GridRun::new(0, 24, 8, fill).span(), 0);
    }
}

#[test]
fn a_coordinate_resolves_to_the_cell_it_lies_on_and_nowhere_else() {
    let run = GridRun::new(100, 24, 8, GridFill::Spread);
    for index in 0..run.count() {
        let at = run.offset(index).expect("an offset");
        assert_eq!(run.cell_at(at), Some(index), "leading edge of {index}");
        assert_eq!(run.cell_at(at + 23), Some(index), "far edge of {index}");
        assert_eq!(run.cell_at(at + 24), None, "the gap after {index}");
    }
    assert_eq!(run.cell_at(run.lead() - 1), None, "the leading margin");
    assert_eq!(run.cell_at(99), None, "the trailing margin");
}

#[test]
fn a_fixed_run_of_lines_follows_on_however_many_there_are() {
    let lines = GridRun::fixed(5, 40, 10);
    assert_eq!(lines.count(), 5);
    assert_eq!(lines.offset(4), Some(200));
    assert_eq!(lines.span(), 240);
    assert_eq!(lines.cell_at(245), None, "past the last line");
    assert_eq!(GridRun::fixed(0, 40, 10).span(), 0);
}

#[test]
fn the_cells_shown_are_every_one_any_part_of_which_is_in_view() {
    let lines = GridRun::fixed(5, 40, 10);
    assert_eq!(lines.shown(0, 40), 0..1);
    // A view that shows only the gap after the first line shows no line.
    assert_eq!(lines.shown(40, 10), 1..1);
    // The tail of one line and the head of the next.
    assert_eq!(lines.shown(30, 30), 0..2);
    assert_eq!(lines.shown(0, 1_000), 0..5);
    assert_eq!(lines.shown(1_000, 50), 5..5);
    assert_eq!(lines.shown(0, 0), 0..0);
}

#[test]
fn an_offset_past_the_axis_is_refused_rather_than_wrapped() {
    let run = GridRun::fixed(usize::MAX, u32::MAX / 2, 0);
    assert_eq!(run.offset(3), None);
}
