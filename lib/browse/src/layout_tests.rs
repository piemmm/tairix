//! Tests for the item-view geometry: the list, the grid in every flow and fill,
//! the places rail, and the pixel scroll they share.

use super::{GridFill, GridFlow, GridMetrics, GridView, ListView, SidebarView};
use alloc::vec::Vec;
use tairix_geometry::{Point, Rect};

const ROW: u32 = 10;
const WIDTH: u32 = 200;

/// `value` as a coordinate, for the small extents these tests use.
fn at(value: u32) -> i32 {
    i32::try_from(value).expect("a small extent")
}

/// A view `rows` rows tall (plus the one-row header) holding `count`
/// entries.
fn view(rows: u32, count: usize) -> ListView {
    let height = ROW * (rows + 1);
    ListView::new(Rect::new(0, 0, WIDTH, height), ROW, ROW, count)
}

/// A full-width row rectangle at window top `y`.
fn rect_at(y: u32) -> Rect {
    Rect::new(0, at(y), WIDTH, ROW)
}

/// The four corner pixels of `rect` and its centre — the pixels a hit-test
/// must resolve to the item shown there and to no other.
fn corners_and_centre(rect: Rect) -> [Point; 5] {
    let (right, bottom) = (rect.right() - 1, rect.bottom() - 1);
    [
        Point::new(rect.left(), rect.top()),
        Point::new(right, rect.top()),
        Point::new(rect.left(), bottom),
        Point::new(right, bottom),
        rect.center(),
    ]
}

#[test]
fn the_list_scrolls_the_viewport_below_its_header() {
    let v = view(4, 10);
    assert_eq!(v.list_area(), Rect::new(0, at(ROW), WIDTH, ROW * 4));
    assert_eq!(v.content_height(), u64::from(ROW * 10));
    assert_eq!(v.scroll_range(0).viewport_extent(), u64::from(ROW * 4));
}

#[test]
fn a_viewport_with_no_room_below_the_header_shows_nothing() {
    let short = ListView::new(Rect::new(0, 0, WIDTH, ROW), ROW, ROW, 10);
    assert_eq!(short.visible_range(0), 0..0);
    assert_eq!(short.shown_rect(0, 0), None);
    assert_eq!(short.index_at(0, Point::new(0, 0)), None);
    assert_eq!(short.index_at(0, Point::new(0, at(ROW))), None);
}

#[test]
fn a_zero_row_height_shows_nothing_rather_than_dividing_by_zero() {
    let degenerate = ListView::new(Rect::new(0, 0, WIDTH, 100), 0, 0, 5);
    assert_eq!(degenerate.content_height(), 0);
    assert_eq!(degenerate.visible_range(0), 0..0);
    assert_eq!(degenerate.row_rect(0), None);
    assert_eq!(degenerate.index_at(0, Point::new(0, 0)), None);
}

/// Every entry is laid out, unscrolled, whether or not the viewport
/// reaches it; only an index past the listing has no row.
#[test]
fn rows_stack_below_the_header_whether_or_not_they_show() {
    let v = view(4, 10);
    assert_eq!(v.row_rect(0), Some(rect_at(ROW)));
    assert_eq!(v.row_rect(2), Some(rect_at(ROW * 3)));
    assert_eq!(v.row_rect(9), Some(rect_at(ROW * 10)));
    assert_eq!(v.shown_rect(0, 9), None, "below the viewport");
    assert_eq!(v.row_rect(10), None, "no such entry");
}

/// A view resting half a row down draws the row above the fold's foot and
/// the row below it's head, each cut by the edge rather than skipped, and
/// a press resolves on either where it shows.
#[test]
fn a_list_scrolled_part_way_shows_the_rows_its_edges_cut() {
    let v = view(3, 10);
    let offset = u64::from(ROW / 2);
    assert_eq!(v.visible_range(offset), 0..4);
    assert_eq!(
        v.shown_rect(offset, 0),
        Some(Rect::new(0, at(ROW), WIDTH, ROW / 2))
    );
    assert_eq!(
        v.shown_rect(offset, 3),
        Some(Rect::new(0, at(ROW * 4 - ROW / 2), WIDTH, ROW / 2))
    );
    assert_eq!(v.index_at(offset, Point::new(0, at(ROW))), Some(0));
    assert_eq!(
        v.index_at(offset, Point::new(0, at(ROW + ROW / 2 - 1))),
        Some(0)
    );
    assert_eq!(
        v.index_at(offset, Point::new(0, at(ROW + ROW / 2))),
        Some(1)
    );
    assert_eq!(v.index_at(offset, Point::new(0, at(ROW * 4 - 1))), Some(3));
    assert_eq!(v.index_at(offset, Point::new(0, at(ROW * 4))), None);
}

#[test]
fn reveal_scrolls_the_least_that_shows_the_whole_row() {
    let v = view(3, 10);
    // Row 6 brought up from below rests its foot on the list's.
    let offset = v.reveal(0, Some(6));
    assert_eq!(offset, u64::from(ROW * 4));
    assert_eq!(v.shown_rect(offset, 6), Some(rect_at(ROW * 3)));
    assert_eq!(v.reveal(offset, Some(5)), offset, "already whole on screen");
    assert_eq!(v.reveal(offset, Some(2)), u64::from(ROW * 2), "from above");
    // A row either edge cuts is brought wholly into view.
    assert_eq!(
        v.reveal(u64::from(ROW * 4 + ROW / 2), Some(4)),
        u64::from(ROW * 4)
    );
    assert_eq!(v.reveal(u64::from(ROW / 2), Some(3)), u64::from(ROW));
    // With nothing to reveal the offset is only clamped.
    assert_eq!(v.reveal(u64::MAX, None), u64::from(ROW * 7));
}

#[test]
fn the_offset_is_clamped_to_the_content() {
    // Fifty pixels of rows in a thirty-pixel list scroll twenty at most.
    let v = view(3, 5);
    assert_eq!(v.scroll_range(99).offset(), u64::from(ROW * 2));
    assert_eq!(v.view(99).offset(), ROW * 2);
    assert_eq!(v.visible_range(99), 2..5);
}

/// At every offset, including ones that rest part-way through a row, the
/// rows the list shows are exactly the ones with an on-screen part, and
/// each part resolves back to its own row.
#[test]
fn the_list_hit_test_mirrors_its_shown_rows_at_any_offset() {
    let v = view(3, 10);
    for offset in [0, 3, 17, 45, 70] {
        let range = v.visible_range(offset);
        for index in 0..10 {
            assert_eq!(
                v.shown_rect(offset, index).is_some(),
                range.contains(&index),
                "offset {offset} row {index}"
            );
        }
        for index in range {
            let shown = v.shown_rect(offset, index).expect("shown");
            for point in corners_and_centre(shown) {
                assert_eq!(v.index_at(offset, point), Some(index), "offset {offset}");
            }
        }
        assert_eq!(v.index_at(offset, Point::new(0, 0)), None, "the header");
        assert_eq!(
            v.index_at(offset, Point::new(at(WIDTH), at(ROW + 1))),
            None,
            "the scrollbar gutter"
        );
    }
}

// --- The icon grid -------------------------------------------------

const CELL: u32 = 40;
const GAP: u32 = 10;
const PITCH: u32 = CELL + GAP;

/// Square `CELL`-pixel tiles a `GAP` apart — the metrics every grid below
/// is laid out with.
const TILES: GridMetrics = GridMetrics {
    cell_width: CELL,
    cell_height: CELL,
    gap: GAP,
};

/// A grid `cols` columns wide and `rows` pitches tall (plus a one-`CELL`
/// header) holding `count` tiles, flowing as the file manager's does. The
/// viewport fits exactly `cols` columns — `cols` tiles plus `cols - 1` gaps
/// — so there is nothing left over and both fill policies agree.
fn grid(cols: u32, rows: u32, count: usize) -> GridView {
    grid_flowing(
        cols,
        rows,
        count,
        GridFlow::RowsFromLeading,
        GridFill::Spread,
    )
}

/// The same exact fit under an explicit `flow` and fill policy.
fn grid_flowing(cols: u32, rows: u32, count: usize, flow: GridFlow, fill: GridFill) -> GridView {
    grid_sized(
        CELL * cols + GAP * cols.saturating_sub(1),
        CELL + PITCH * rows,
        count,
        flow,
        fill,
    )
}

/// A `width`×`height` grid (the height including the one-`CELL` header)
/// holding `count` tiles.
fn grid_sized(width: u32, height: u32, count: usize, flow: GridFlow, fill: GridFill) -> GridView {
    GridView::new(
        Rect::new(0, 0, width, height),
        TILES,
        CELL,
        count,
        flow,
        fill,
    )
}

/// A file-manager grid whose viewport is `slack` pixels wider and taller
/// than an exact `cols`×`rows` fit — space left over, but not enough for a
/// further tile across while `slack` is under one gap plus one tile.
fn grid_with_slack(cols: u32, rows: u32, count: usize, slack: u32, fill: GridFill) -> GridView {
    grid_sized(
        CELL * cols + GAP * cols.saturating_sub(1) + slack,
        CELL + PITCH * rows + slack,
        count,
        GridFlow::RowsFromLeading,
        fill,
    )
}

/// The laid-out tile rectangles of the first `count` entries, in index
/// order.
fn laid_out(g: &GridView, count: usize) -> Vec<Rect> {
    (0..count).filter_map(|index| g.cell_rect(index)).collect()
}

const FLOWS: [GridFlow; 3] = [
    GridFlow::RowsFromLeading,
    GridFlow::ColumnsFromLeading,
    GridFlow::ColumnsFromTrailing,
];

const FILLS: [GridFill; 2] = [GridFill::FixedPitch, GridFill::Spread];

#[test]
fn grid_columns_and_rows_wrap_the_entries() {
    let g = grid(3, 2, 7);
    assert_eq!(g.cells_per_line(), 3);
    // Seven tiles across three columns need three rows (ceil).
    assert_eq!(g.lines_total(), 3);
    // Three rows of tiles and the two gaps between them.
    assert_eq!(g.content_extent(), u64::from(CELL * 3 + GAP * 2));
    assert_eq!(g.scroll_range(0).viewport_extent(), u64::from(PITCH * 2));
}

/// Too narrow for one whole tile across a line, a grid lays out nothing,
/// whatever it does with the space it has left over.
#[test]
fn a_grid_too_narrow_for_one_tile_across_shows_nothing() {
    for fill in FILLS {
        let g = grid_sized(CELL - 1, 500, 5, GridFlow::RowsFromLeading, fill);
        assert_eq!(g.cells_per_line(), 0, "{fill:?}");
        assert_eq!(g.cell_rect(0), None, "{fill:?}");
        assert_eq!(g.index_at(0, Point::new(0, at(CELL))), None, "{fill:?}");
        assert_eq!(g.visible_range(0), 0..0, "{fill:?}");
        assert!(!g.scroll_range(0).is_scrollable(), "{fill:?}");
    }
    let short = grid_sized(
        500,
        CELL,
        5,
        GridFlow::ColumnsFromTrailing,
        GridFill::FixedPitch,
    );
    assert_eq!(short.cells_per_line(), 0, "no room down a column");
    assert_eq!(short.visible_range(0), 0..0);
    let narrow = grid_flowing(0, 3, 5, GridFlow::ColumnsFromTrailing, GridFill::FixedPitch);
    assert_eq!(narrow.visible_range(0), 0..0, "no room across for a column");
    assert_eq!(narrow.shown_rect(0, 0), None);
    assert_eq!(narrow.index_at(0, Point::new(0, at(CELL))), None);
}

/// A view shorter than one tile still shows the first row, cut at its
/// foot, and scrolls through the rest rather than showing nothing.
#[test]
fn a_grid_shorter_than_a_tile_shows_its_rows_cut() {
    let g = grid_sized(
        CELL * 3 + GAP * 2,
        CELL + CELL / 2,
        7,
        GridFlow::RowsFromLeading,
        GridFill::Spread,
    );
    let header = at(CELL);
    assert_eq!(g.visible_range(0), 0..3);
    assert_eq!(
        g.shown_rect(0, 0),
        Some(Rect::new(0, header, CELL, CELL / 2))
    );
    assert_eq!(g.index_at(0, Point::new(1, header + 1)), Some(0));
    let end = g.scroll_range(u64::MAX).offset();
    assert_eq!(end, u64::from(PITCH * 2 + CELL - CELL / 2));
    assert_eq!(g.visible_range(end), 6..7);
    assert_eq!(
        g.shown_rect(end, 6),
        Some(Rect::new(0, header, CELL, CELL / 2)),
        "the last row shows its foot"
    );
}

#[test]
fn grid_tiles_lay_out_left_to_right_then_wrap() {
    let g = grid(3, 2, 7);
    let header = at(CELL);
    assert_eq!(g.cell_rect(0), Some(Rect::new(0, header, CELL, CELL)));
    assert_eq!(
        g.cell_rect(2),
        Some(Rect::new(at(PITCH * 2), header, CELL, CELL))
    );
    // Tile 3 wraps to row 1, and tile 6 onto row 2 below the viewport:
    // laid out all the same.
    assert_eq!(
        g.cell_rect(3),
        Some(Rect::new(0, header + at(PITCH), CELL, CELL))
    );
    assert_eq!(
        g.cell_rect(6),
        Some(Rect::new(0, header + at(PITCH * 2), CELL, CELL))
    );
    assert_eq!(g.cell_rect(7), None);
}

#[test]
fn grid_hit_test_mirrors_the_tile_rects_and_rejects_gaps() {
    let g = grid(3, 2, 7);
    let header = at(CELL);
    let half = at(CELL / 2);
    assert_eq!(g.index_at(0, Point::new(half, header + half)), Some(0));
    assert_eq!(
        g.index_at(0, Point::new(at(PITCH) + half, header + at(PITCH) + half)),
        Some(4)
    );
    assert_eq!(
        g.index_at(0, Point::new(at(CELL + GAP / 2), header + half)),
        None,
        "the gap between columns"
    );
    assert_eq!(
        g.index_at(0, Point::new(half, header + at(CELL + GAP / 2))),
        None,
        "the gap between rows"
    );
    assert_eq!(g.index_at(0, Point::new(half, 0)), None, "the header");
}

#[test]
fn grid_reveal_scrolls_by_pixels() {
    // Three columns, one pitch of view, nine tiles: three rows.
    let g = grid(3, 1, 9);
    // Tile 8's row reaches 140px down; revealing it rests that foot on the
    // view's.
    assert_eq!(g.reveal(0, Some(8)), u64::from(PITCH * 2 + CELL - PITCH));
    assert_eq!(
        g.reveal(u64::from(PITCH), Some(4)),
        u64::from(PITCH),
        "a row wholly shown does not move the view"
    );
    assert_eq!(
        g.reveal(u64::from(PITCH + GAP), Some(4)),
        u64::from(PITCH),
        "a row the top edge cuts is revealed from its top"
    );
}

/// Resting part-way down, the grid draws the row the top edge cuts and
/// the row the foot cuts, and a press lands on either where it shows —
/// never on the gap between rows.
#[test]
fn a_grid_scrolled_part_way_shows_and_hits_the_rows_its_edges_cut() {
    let g = grid(3, 2, 9);
    let header = at(CELL);
    let offset = 25;
    assert_eq!(g.visible_range(offset), 0..9);
    assert_eq!(
        g.shown_rect(offset, 0),
        Some(Rect::new(0, header, CELL, 15))
    );
    assert_eq!(
        g.shown_rect(offset, 6),
        Some(Rect::new(0, header + 75, CELL, 25))
    );
    assert_eq!(g.index_at(offset, Point::new(5, header + 2)), Some(0));
    assert_eq!(g.index_at(offset, Point::new(5, header + 99)), Some(6));
    assert_eq!(
        g.index_at(offset, Point::new(5, header + 20)),
        None,
        "the gap between the first two rows"
    );
}

/// A row the viewport holds only part of is a row like any other: it is
/// drawn to the foot and a press on its visible part finds it.
#[test]
fn a_row_past_the_whole_rows_that_fit_is_hit_where_it_shows() {
    let g = grid_sized(
        CELL * 3 + GAP * 2,
        CELL + PITCH + 20,
        9,
        GridFlow::RowsFromLeading,
        GridFill::Spread,
    );
    let header = at(CELL);
    assert_eq!(g.visible_range(0), 0..6);
    assert_eq!(
        g.shown_rect(0, 3),
        Some(Rect::new(0, header + at(PITCH), CELL, 20))
    );
    assert_eq!(g.index_at(0, Point::new(5, header + 60)), Some(3));
}

#[test]
fn the_visible_range_is_exactly_the_tiles_on_screen() {
    for flow in FLOWS {
        for fill in FILLS {
            let g = grid_flowing(3, 2, 7, flow, fill);
            let end = g.scroll_range(u64::MAX).offset();
            assert!(end > 0, "{flow:?} {fill:?} overflows its view");
            for offset in 0..=end {
                let range = g.visible_range(offset);
                for index in 0..7 {
                    assert_eq!(
                        g.shown_rect(offset, index).is_some(),
                        range.contains(&index),
                        "{flow:?} {fill:?} offset {offset} index {index}"
                    );
                }
            }
        }
    }
}

/// Every tile a grid shows is whole across its line — only the edges the
/// view scrolls past cut a tile — lies inside the tile area, and resolves
/// each of its shown corners and its centre back to itself. Swept over
/// every flow, both fill policies, viewports that divide the tiles evenly
/// and unevenly, and offsets from rest to the end.
#[test]
fn every_shown_tile_is_whole_across_its_line_and_hit_tests_to_itself() {
    const COUNT: usize = 40;
    for flow in FLOWS {
        for fill in FILLS {
            for width in (CELL..=CELL * 6).step_by(7) {
                for height in (CELL..=CELL * 6).step_by(13) {
                    let g = grid_sized(width, height, COUNT, flow, fill);
                    let area = g.tile_area();
                    let end = g.scroll_range(u64::MAX).offset();
                    for offset in [0, 1, end / 2, end] {
                        for index in g.visible_range(offset) {
                            let shown = g.shown_rect(offset, index).expect("shown");
                            let across = if flow.wraps_down_a_column() {
                                shown.height
                            } else {
                                shown.width
                            };
                            assert_eq!(
                                across, CELL,
                                "{flow:?} {fill:?} {width}x{height} offset {offset}"
                            );
                            assert_eq!(shown.intersection(&area), shown);
                            for point in corners_and_centre(shown) {
                                assert_eq!(
                                    g.index_at(offset, point),
                                    Some(index),
                                    "{point:?} of {shown:?}: {flow:?} {fill:?} \
                                     {width}x{height} offset {offset}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}

// --- The space a line has left over ---------------------------------

/// A run that fits its tiles exactly has nothing left over, so both
/// policies place it identically: the row begins at the leading edge and
/// ends at the trailing one.
#[test]
fn an_exact_fit_is_laid_out_identically_under_either_fill() {
    let fixed = grid_flowing(3, 2, 7, GridFlow::RowsFromLeading, GridFill::FixedPitch);
    let spread = grid_flowing(3, 2, 7, GridFlow::RowsFromLeading, GridFill::Spread);
    assert_eq!(spread.cells_per_line(), fixed.cells_per_line());
    assert_eq!(laid_out(&spread, 7), laid_out(&fixed, 7));
    let row = laid_out(&spread, 3);
    assert_eq!(row[0].left(), 0);
    assert_eq!(row[2].right(), at(CELL * 3 + GAP * 2));
}

/// The leftover width is shared out along the row: the gaps widen by equal
/// amounts, the two end margins match, and what is left is under a gap plus
/// the pixels that will not divide into one per slot. Nothing is parked as a
/// blank strip at the trailing edge.
#[test]
fn a_spread_row_shares_its_leftover_width_out_evenly() {
    let slack = 30;
    let g = grid_with_slack(3, 2, 40, slack, GridFill::Spread);
    let width = CELL * 3 + GAP * 2 + slack;
    assert_eq!(g.cells_per_line(), 3, "30px short of a fourth column");
    let row = laid_out(&g, 3);
    let gaps: Vec<i32> = row.windows(2).map(|p| p[1].left() - p[0].right()).collect();
    assert!(
        gaps.iter().all(|gap| *gap == gaps[0]),
        "the gaps stay identical: {gaps:?}"
    );
    assert!(gaps[0] > at(GAP), "and widened past the minimum: {gaps:?}");
    let lead = row[0].left();
    let trail = at(width) - row[2].right();
    assert!(lead > 0 && (lead - trail).abs() <= 1, "{lead} vs {trail}");
    assert!(
        lead + trail < gaps[0] + i32::try_from(row.len()).expect("small"),
        "only one gap's worth plus the indivisible pixels reach the ends: \
         {lead} + {trail} against {gaps:?}"
    );
}

/// The fixed field does the opposite with the same viewport: the pitch is
/// kept from the anchored edge and the remainder stays at the far end, so an
/// icon does not move when the area's extent changes by a few pixels.
#[test]
fn a_fixed_pitch_row_leaves_its_leftover_width_at_the_far_end() {
    let slack = 30;
    let g = grid_with_slack(3, 2, 40, slack, GridFill::FixedPitch);
    let row = laid_out(&g, 3);
    assert_eq!(row[0].left(), 0, "anchored to the leading edge");
    for pair in row.windows(2) {
        assert_eq!(pair[1].left() - pair[0].right(), at(GAP));
    }
    let width = CELL * 3 + GAP * 2 + slack;
    assert_eq!(
        at(width) - row[2].right(),
        at(slack),
        "the whole remainder is left at the far end"
    );
}

/// Widening the view spreads the row it has until one more whole tile fits,
/// and then re-flows the listing into the extra column. The tiles keep their
/// size throughout: only the space between them moves.
#[test]
fn a_widening_view_spreads_until_one_more_tile_fits_then_re_flows() {
    let three = CELL * 3 + GAP * 2;
    let four = CELL * 4 + GAP * 3;
    for width in three..=four {
        let g = grid_sized(
            width,
            CELL + PITCH * 3,
            40,
            GridFlow::RowsFromLeading,
            GridFill::Spread,
        );
        let want = if width < four { 3 } else { 4 };
        assert_eq!(g.cells_per_line(), want, "{width} wide");
        let row = laid_out(&g, want);
        assert!(
            row.iter().all(|tile| tile.width == CELL),
            "{width} wide: a tile never stretches"
        );
        let spent = row[want - 1].right() - row[0].left();
        let tiles = at(CELL * u32::try_from(want).unwrap());
        assert!(
            spent >= tiles + at(GAP * u32::try_from(want - 1).unwrap()),
            "{width} wide: the gaps never fall below the minimum"
        );
    }
}

/// The axis the grid scrolls along is never spread: the rows keep the fixed
/// pitch below the header, and the space past the last whole row is the next
/// row's head, cut by the view's foot, one scroll from showing whole.
#[test]
fn spreading_leaves_the_axis_the_grid_scrolls_along_alone() {
    let slack = 30;
    let g = grid_with_slack(3, 2, 40, slack, GridFill::Spread);
    let first = g.cell_rect(0).expect("laid out");
    let second = g.cell_rect(3).expect("laid out");
    assert_eq!(first.top(), at(CELL));
    assert_eq!(second.top() - first.top(), at(PITCH));
    let third = g.shown_rect(0, 6).expect("the third row shows its head");
    assert_eq!(third.top(), at(CELL + PITCH * 2));
    assert_eq!(third.height, slack, "cut by the foot, not squeezed into it");
    let offset = g.reveal(0, Some(7));
    assert_eq!(offset, u64::from(CELL - slack));
    assert_eq!(g.shown_rect(offset, 7).map(|tile| tile.height), Some(CELL));
}

// --- The desktop's icon columns -------------------------------------

#[test]
fn the_desktop_column_fills_downward_from_the_leading_edge() {
    // Three columns' worth of width, two tiles per column, five icons.
    let g = grid_flowing(3, 2, 5, GridFlow::ColumnsFromLeading, GridFill::FixedPitch);
    assert_eq!(g.cells_per_line(), 2, "two icons fit down one column");
    assert_eq!(g.lines_total(), 3, "five icons need three columns");
    assert!(
        !g.scroll_range(0).is_scrollable(),
        "three columns fit across"
    );
    let header = at(CELL);
    let pitch = at(PITCH);
    // The first icon hugs the leading edge, below the header.
    assert_eq!(g.cell_rect(0), Some(Rect::new(0, header, CELL, CELL)));
    // The second falls directly beneath it, in the same column.
    assert_eq!(
        g.cell_rect(1),
        Some(Rect::new(0, header + pitch, CELL, CELL))
    );
    // The third starts a new column one pitch further across.
    assert_eq!(g.cell_rect(2), Some(Rect::new(pitch, header, CELL, CELL)));
}

/// The two column flows are exact mirrors of one another: an icon that
/// sits `n` pixels in from the leading edge under one sits `n` pixels in
/// from the trailing edge under the other, at the very same height. That
/// is the whole difference between the two desktop arrangements, so it is
/// asserted rather than assumed.
#[test]
fn the_two_desktop_columns_are_mirror_images() {
    let leading = grid_flowing(3, 2, 5, GridFlow::ColumnsFromLeading, GridFill::FixedPitch);
    let trailing = grid_flowing(3, 2, 5, GridFlow::ColumnsFromTrailing, GridFill::FixedPitch);
    let width = at(CELL * 3 + GAP * 2);
    for index in 0..5 {
        let left = leading.shown_rect(0, index).expect("on screen");
        let right = trailing.shown_rect(0, index).expect("on screen");
        assert_eq!(left.top(), right.top(), "icon {index} keeps its height");
        assert_eq!(
            left.left(),
            width - right.right(),
            "icon {index} is the same distance in from its own edge"
        );
    }
}

#[test]
fn the_leading_desktop_hit_test_mirrors_its_tile_rects_and_rejects_gaps() {
    let g = grid_flowing(3, 2, 5, GridFlow::ColumnsFromLeading, GridFill::FixedPitch);
    for index in 0..5 {
        let rect = g.shown_rect(0, index).expect("every icon is on screen");
        assert_eq!(g.index_at(0, rect.center()), Some(index));
    }
    let half = at(CELL / 2);
    let header = at(CELL);
    assert_eq!(
        g.index_at(0, Point::new(at(CELL + GAP / 2), header + half)),
        None,
        "the gap between the leading column and the next"
    );
    assert_eq!(g.index_at(0, Point::new(half, 0)), None, "the header");
    assert_eq!(
        g.index_at(
            0,
            Point::new(at(PITCH * 2) + half, header + at(PITCH) + half)
        ),
        None,
        "the empty slot past the last icon"
    );
}

#[test]
fn the_desktop_column_fills_downward_from_the_trailing_edge() {
    // Three columns' worth of width, two tiles per column, five icons.
    let g = grid_flowing(3, 2, 5, GridFlow::ColumnsFromTrailing, GridFill::FixedPitch);
    assert_eq!(g.cells_per_line(), 2, "two icons fit down one column");
    assert_eq!(g.lines_total(), 3, "five icons need three columns");
    let right = at(CELL * 3 + GAP * 2 - CELL);
    let header = at(CELL);
    // The first icon hugs the trailing edge, below the header.
    assert_eq!(g.cell_rect(0), Some(Rect::new(right, header, CELL, CELL)));
    // The second falls directly beneath it, in the same column.
    assert_eq!(
        g.cell_rect(1),
        Some(Rect::new(right, header + at(PITCH), CELL, CELL))
    );
    // The third starts a new column one pitch further inward.
    assert_eq!(
        g.cell_rect(2),
        Some(Rect::new(right - at(PITCH), header, CELL, CELL))
    );
}

#[test]
fn the_desktop_hit_test_mirrors_its_tile_rects_and_rejects_gaps() {
    let g = grid_flowing(3, 2, 5, GridFlow::ColumnsFromTrailing, GridFill::FixedPitch);
    for index in 0..5 {
        let rect = g.shown_rect(0, index).expect("every icon is on screen");
        assert_eq!(g.index_at(0, rect.center()), Some(index));
    }
    let width = at(CELL * 3 + GAP * 2);
    let half = at(CELL / 2);
    let header = at(CELL);
    assert_eq!(
        g.index_at(0, Point::new(width - at(CELL + GAP / 2), header + half)),
        None,
        "the gap between the trailing column and the one inside it"
    );
    assert_eq!(
        g.index_at(0, Point::new(width - half, 0)),
        None,
        "the header"
    );
    assert_eq!(
        g.index_at(0, Point::new(half, header + at(PITCH) + half)),
        None,
        "the empty slot past the last icon"
    );
}

/// More columns than a trailing arrangement fits grow off its leading
/// edge, and the offset scrolls inward from the trailing one: at rest the
/// first column hugs that edge, at the end the last hugs the other, and a
/// column either edge cuts is found where it shows.
#[test]
fn a_trailing_column_scrolls_inward_from_its_edge() {
    let g = grid_flowing(
        3,
        2,
        10,
        GridFlow::ColumnsFromTrailing,
        GridFill::FixedPitch,
    );
    let width = at(CELL * 3 + GAP * 2);
    let header = at(CELL);
    assert_eq!(g.content_extent(), u64::from(PITCH * 4 + CELL));
    let end = g.scroll_range(u64::MAX).offset();
    assert_eq!(end, u64::from(PITCH * 2));

    assert_eq!(g.shown_rect(0, 0).map(|tile| tile.right()), Some(width));
    assert_eq!(g.shown_rect(0, 4).map(|tile| tile.left()), Some(0));
    assert_eq!(g.visible_range(0), 0..6);
    assert_eq!(g.shown_rect(0, 6), None, "past the leading edge");

    assert_eq!(g.shown_rect(end, 8).map(|tile| tile.left()), Some(0));
    assert_eq!(g.shown_rect(end, 4).map(|tile| tile.right()), Some(width));
    assert_eq!(g.visible_range(end), 4..10);
    assert_eq!(
        g.shown_rect(end, 0),
        None,
        "scrolled past the trailing edge"
    );
    let last = g.shown_rect(end, 8).expect("shown");
    assert_eq!(g.index_at(end, last.center()), Some(8));

    let part = 25;
    assert_eq!(
        g.shown_rect(part, 6),
        Some(Rect::new(0, header, 15, CELL)),
        "the fourth column, cut by the leading edge"
    );
    assert_eq!(g.index_at(part, Point::new(5, header + 5)), Some(6));
    assert_eq!(
        g.shown_rect(part, 0),
        Some(Rect::new(width - 15, header, 15, CELL)),
        "the first column, cut by the trailing edge"
    );
}

#[test]
fn a_leading_column_scrolls_across_from_its_edge() {
    let g = grid_flowing(3, 2, 10, GridFlow::ColumnsFromLeading, GridFill::FixedPitch);
    let offset = u64::from(PITCH);
    assert_eq!(g.shown_rect(offset, 2).map(|tile| tile.left()), Some(0));
    assert_eq!(g.shown_rect(offset, 0), None);
    assert_eq!(g.visible_range(offset), 2..8);
    assert_eq!(
        g.index_at(offset, Point::new(5, at(CELL) + 5)),
        Some(2),
        "the second column now at the leading edge"
    );
}

/// A rail of `rows` rows in a 95px window, scrolled `offset` pixels, with a
/// separator band before the row at `volumes`.
fn rail(rows: usize, volumes: Option<usize>, offset: u64) -> SidebarView {
    SidebarView::new(
        Rect::new(0, 0, WIDTH, 95),
        WIDTH,
        (ROW, 4),
        rows,
        volumes,
        (offset, 6),
    )
}

/// The rows the rail's range names are exactly the rows its drawn geometry
/// puts on screen, at every offset: across the separator band, at both ends,
/// past the end, where the offset is clamped, and for a rail that fits.
#[test]
fn the_rails_visible_range_is_exactly_the_rows_it_shows() {
    for rows in [40, 5] {
        for volumes in [Some(3), Some(0), Some(rows - 1), None, Some(rows)] {
            let end = rail(rows, volumes, 0).content_height() + 3;
            for offset in 0..=end {
                let view = rail(rows, volumes, offset);
                let shown: Vec<usize> = (0..rows)
                    .filter(|&row| view.shown_row_rect(row).is_some())
                    .collect();
                let range: Vec<usize> = view.visible_range().collect();
                assert_eq!(
                    range, shown,
                    "{rows} rows, volumes at {volumes:?}, offset {offset}"
                );
            }
        }
    }
}

#[test]
fn a_rail_with_no_height_or_no_rows_shows_no_range() {
    let flat = SidebarView::new(
        Rect::new(0, 0, WIDTH, 0),
        WIDTH,
        (ROW, 4),
        8,
        Some(3),
        (0, 6),
    );
    assert_eq!(flat.visible_range(), 0..0);
    let empty = rail(0, None, 0);
    assert_eq!(empty.visible_range(), 0..0);
    let unmeasured = SidebarView::new(Rect::new(0, 0, WIDTH, 95), WIDTH, (0, 4), 8, None, (0, 6));
    assert_eq!(unmeasured.visible_range(), 0..0);
}
