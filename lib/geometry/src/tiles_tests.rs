//! Host tests of a change's tiles: marking them, their runs as the pixels they
//! cover, merging them two by two, and a cover that fits its budget, covers
//! every mark and leaves the marks as they were.

extern crate std;

use alloc::vec::Vec;

use super::{runs, Tiles};
use crate::{Point, Rect, Region};

const SIDE: u32 = 16;

fn tile(x: u32, y: u32) -> Rect {
    Rect::new(
        i32::try_from(x * SIDE).expect("small"),
        i32::try_from(y * SIDE).expect("small"),
        1,
        1,
    )
}

/// The marked tiles of `tiles`, as columns and rows.
fn marked(tiles: &Tiles) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for row in 0..tiles.rows() {
        let line = tiles.grid.row(&tiles.bits, row).expect("a row");
        for columns in runs(line) {
            out.extend(columns.map(|column| (column, row)));
        }
    }
    out
}

#[test]
fn a_side_of_nought_holds_no_tiles() {
    assert!(Tiles::new((64, 64), 0).is_none());
    let empty = Tiles::new((0, 0), SIDE).expect("tiles");
    assert_eq!((empty.rows(), empty.count()), (0, 0));
    assert!(empty.is_empty());
}

/// A run of marked tiles comes back as the pixels it covers, cut at the
/// picture's edges, and a rectangle reaching past the picture marks only what
/// lies within it.
#[test]
fn a_runs_pixels_are_its_tiles_within_the_picture() {
    let size = (40u32, 20u32);
    let mut tiles = Tiles::new(size, SIDE).expect("tiles");
    tiles.mark(Rect::new(10, 2, 20, 15));
    tiles.mark(Rect::new(35, 18, 100, 100));
    assert_eq!(tiles.count(), 5);
    let spans: Vec<_> = tiles.spans(0..tiles.rows()).collect();
    assert_eq!(spans, [(0..32, 0..16), (0..40, 16..20)]);
    assert_eq!(tiles.spans(1..2).count(), 1, "one row of tiles alone");
    assert_eq!(tiles.spans(2..9).count(), 0, "no rows past the picture");
    tiles.clear();
    assert_eq!(tiles.count(), 0);
    assert!(tiles.is_empty());
}

/// A run crossing words, and one ending on the last column of a word, come
/// back whole.
#[test]
fn runs_cross_words_whole() {
    let size = (200 * SIDE, SIDE);
    let mut tiles = Tiles::new(size, SIDE).expect("tiles");
    tiles.mark(Rect::new(
        i32::try_from(60 * SIDE).expect("small"),
        0,
        70 * SIDE,
        1,
    ));
    tiles.mark(tile(191, 0));
    tiles.mark(tile(199, 0));
    let line = tiles.grid.row(&tiles.bits, 0).expect("a row");
    assert_eq!(
        runs(line).collect::<Vec<_>>(),
        [60..130, 191..192, 199..200]
    );
    assert_eq!(tiles.count(), 72);
}

/// Including another picture's marks adds them, and copying them replaces
/// what was marked; a picture of another shape changes nothing.
#[test]
fn marks_are_included_and_copied_from_the_same_picture_only() {
    let size = (128, 64);
    let mut mine = Tiles::new(size, SIDE).expect("tiles");
    let mut theirs = Tiles::new(size, SIDE).expect("tiles");
    mine.mark(tile(0, 0));
    theirs.mark(tile(3, 2));
    mine.include(&theirs);
    assert_eq!(marked(&mine), [(0, 0), (3, 2)]);
    mine.copy_from(&theirs);
    assert_eq!(marked(&mine), [(3, 2)]);
    let other = Tiles::new((64, 64), SIDE).expect("tiles");
    mine.include(&other);
    mine.copy_from(&other);
    assert_eq!(marked(&mine), [(3, 2)], "another picture is left alone");
}

/// Merging tiles two by two keeps every marked one within a marked merger,
/// and marks nothing a merger did not reach.
#[test]
fn merged_tiles_cover_what_they_merged() {
    let size = (7 * SIDE, 5 * SIDE);
    let mut tiles = Tiles::new(size, SIDE).expect("tiles");
    for (x, y) in [(0, 0), (6, 0), (3, 2), (4, 2), (6, 4)] {
        tiles.mark(tile(x, y));
    }
    let coarse = tiles.grid.coarsen(&mut tiles.bits);
    assert_eq!((coarse.across, coarse.down, coarse.side), (4, 3, 2 * SIDE));
    let merged: Vec<(usize, usize)> = (0..coarse.down)
        .flat_map(|row| {
            let line = coarse.row(&tiles.bits, row).expect("a row");
            runs(line)
                .flat_map(|columns| columns.map(move |column| (column, row)))
                .collect::<Vec<_>>()
        })
        .collect();
    assert_eq!(merged, [(0, 0), (3, 0), (1, 1), (2, 1), (3, 2)]);
}

/// Merging a grid wider than a word in place merges every pair across the
/// words, as tile by tile merging would.
#[test]
fn merging_a_wide_grid_in_place_matches_tile_by_tile() {
    let (across, down) = (300usize, 9usize);
    let size = (
        u32::try_from(across).expect("small") * SIDE,
        u32::try_from(down).expect("small") * SIDE,
    );
    let mut tiles = Tiles::new(size, SIDE).expect("tiles");
    let mut model = alloc::vec![false; across * down];
    let mut seed = 0x9E37_79B9_u32;
    for _ in 0..500 {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let (x, y) = ((seed >> 8) as usize % across, (seed >> 20) as usize % down);
        model[y * across + x] = true;
        tiles.mark(tile(
            u32::try_from(x).expect("small"),
            u32::try_from(y).expect("small"),
        ));
    }
    let coarse = tiles.grid.coarsen(&mut tiles.bits);
    for row in 0..coarse.down {
        let line = coarse.row(&tiles.bits, row).expect("a row");
        for column in 0..coarse.across {
            let expected = (2 * row..(2 * row + 2).min(down))
                .any(|y| (2 * column..(2 * column + 2).min(across)).any(|x| model[y * across + x]));
            let got = (line[column / 64] >> (column % 64)) & 1 == 1;
            assert_eq!(got, expected, "tile ({column}, {row})");
        }
        let tail = coarse.across % 64;
        if tail != 0 {
            assert_eq!(line[coarse.stride - 1] >> tail, 0, "no mark past the grid");
        }
    }
}

/// The same run in the rows beneath is one rectangle, so a block of whole
/// rows covers as one.
#[test]
fn a_run_repeated_down_the_rows_is_one_rectangle() {
    let size = (640, 360);
    let mut tiles = Tiles::new(size, SIDE).expect("tiles");
    let mut room = Tiles::new(size, SIDE).expect("room");
    tiles.mark(Rect::new(32, 16, 64, 64));
    tiles.mark(Rect::new(0, 200, 640, 160));
    let mut damage = Region::new();
    tiles.cover(128, &mut room, &mut damage);
    assert_eq!(
        damage.rects(),
        [Rect::new(32, 16, 64, 64), Rect::new(0, 192, 640, 168)]
    );
}

/// However scattered the marks, a cover fits its budget, covers every marked
/// tile, is made of whole tiles, and leaves the marks as they were.
#[test]
fn a_cover_fits_its_budget_and_leaves_the_marks_alone() {
    const BUDGET: usize = 128;
    let size = (640u32, 360u32);
    let mut tiles = Tiles::new(size, SIDE).expect("tiles");
    let mut room = Tiles::new(size, SIDE).expect("room");
    let scattered: Vec<Rect> = (0..400u32)
        .map(|n| {
            Rect::new(
                i32::try_from((n * 97) % size.0).expect("small"),
                i32::try_from((n * 61) % size.1).expect("small"),
                1,
                1,
            )
        })
        .collect();
    for rect in &scattered {
        tiles.mark(*rect);
    }
    let count = tiles.count();
    let spans: Vec<_> = tiles.spans(0..tiles.rows()).collect();
    let mut damage = Region::new();
    tiles.cover(BUDGET, &mut room, &mut damage);
    assert!(damage.rects().len() <= BUDGET, "{}", damage.rects().len());
    for rect in &scattered {
        assert!(damage.contains(rect.origin), "{rect:?} uncovered");
    }
    assert_eq!(tiles.count(), count);
    assert_eq!(tiles.spans(0..tiles.rows()).collect::<Vec<_>>(), spans);
    let pitch = i32::try_from(SIDE).expect("small");
    for rect in damage.rects() {
        assert_eq!(rect.left() % pitch, 0, "{rect:?}");
        assert_eq!(rect.top() % pitch, 0, "{rect:?}");
    }
}

/// A cover is counted as a region holds it: columns of tiles reaching down to
/// different rows, few as stacked runs but many as bands, are covered within
/// the budget a region keeps them in rather than collapsing to their bounds.
#[test]
fn a_cover_is_counted_in_the_bands_a_region_holds() {
    const BUDGET: usize = 128;
    let size = (240 * SIDE, 60 * SIDE);
    let mut tiles = Tiles::new(size, SIDE).expect("tiles");
    let mut room = Tiles::new(size, SIDE).expect("room");
    for column in 0..60u32 {
        for row in 0..column {
            tiles.mark(tile(2 * column, row));
        }
    }
    let mut exact = Region::new();
    tiles.cover(BUDGET, &mut room, &mut exact);
    assert!(
        exact.rects().len() <= BUDGET,
        "{} bands",
        exact.rects().len()
    );
    let mut held = Region::with_budget(BUDGET);
    tiles.cover(BUDGET, &mut room, &mut held);
    assert_eq!(held.rects(), exact.rects(), "kept as covered");
    for (x, y) in marked(&tiles) {
        let centre =
            |at: usize| i32::try_from(at * SIDE as usize + SIDE as usize / 2).expect("small");
        assert!(
            exact.contains(Point::new(centre(x), centre(y))),
            "({x}, {y})"
        );
    }
}

/// With room in the budget a cover is exactly the marked tiles: every pixel of
/// a marked tile in it, every pixel of an unmarked one out.
#[test]
fn a_cover_within_its_budget_is_exactly_the_marked_tiles() {
    let size = (8 * SIDE, 6 * SIDE);
    let mut tiles = Tiles::new(size, SIDE).expect("tiles");
    let mut room = Tiles::new(size, SIDE).expect("room");
    let mut seed = 7u32;
    for _ in 0..20 {
        seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        tiles.mark(tile((seed >> 16) % 8, (seed >> 24) % 6));
    }
    let mut damage = Region::new();
    tiles.cover(usize::MAX, &mut room, &mut damage);
    let marks = marked(&tiles);
    for y in 0..6u32 {
        for x in 0..8u32 {
            let inside = damage.contains(Point::new(
                i32::try_from(x * SIDE + SIDE / 2).expect("small"),
                i32::try_from(y * SIDE + SIDE / 2).expect("small"),
            ));
            let want = marks.contains(&(x as usize, y as usize));
            assert_eq!(inside, want, "tile ({x}, {y})");
        }
    }
}

/// A room of another picture cannot be worked in: the whole picture is
/// covered, and nothing at all when nothing is marked.
#[test]
fn a_cover_without_room_covers_the_whole_picture() {
    let size = (64, 64);
    let mut tiles = Tiles::new(size, SIDE).expect("tiles");
    let mut wrong = Tiles::new((32, 32), SIDE).expect("room");
    let mut damage = Region::new();
    tiles.cover(8, &mut wrong, &mut damage);
    assert!(damage.is_empty());
    tiles.mark(tile(1, 1));
    tiles.cover(8, &mut wrong, &mut damage);
    assert_eq!(damage.rects(), [Rect::new(0, 0, 64, 64)]);
}
