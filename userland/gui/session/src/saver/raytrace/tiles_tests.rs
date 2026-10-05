//! Host tests of a change's tiles: merging them two by two, their runs as the
//! pixels they cover, and a cover that fits its budget without touching the
//! marks it was worked out from.

use alloc::vec::Vec;

use tairix_wm::{Rect, Region};

use super::{coarsen, runs, Tiles, COVER_BUDGET, TILE};

/// Merging tiles two by two keeps every marked one within a marked merger,
/// and marks nothing a merger did not reach.
#[test]
fn merged_tiles_cover_what_they_merged() {
    let (across, down) = (7usize, 5usize);
    let marked = [(0usize, 0usize), (6, 0), (3, 2), (4, 2), (6, 4)];
    let mut tiles = alloc::vec![false; across * down];
    for (x, y) in marked {
        tiles[y * across + x] = true;
    }
    coarsen(&mut tiles, across, down);
    let (half_across, half_down) = (across.div_ceil(2), down.div_ceil(2));
    let merged: Vec<(usize, usize)> = (0..half_down)
        .flat_map(|y| (0..half_across).map(move |x| (x, y)))
        .filter(|(x, y)| tiles[y * half_across + x])
        .collect();
    assert_eq!(merged, [(0, 0), (3, 0), (1, 1), (2, 1), (3, 2)]);
    let lines: Vec<_> = runs(&tiles, half_across, 0..half_down).collect();
    assert_eq!(lines, [(0, 0..1), (0, 3..4), (1, 1..3), (2, 3..4)]);
}

/// A run of marked tiles comes back as the pixels it covers, cut at the
/// picture's edges, and a rectangle reaching past the picture marks only what
/// lies within it.
#[test]
fn a_runs_pixels_are_its_tiles_within_the_picture() {
    let size = (40u32, 20u32);
    let mut tiles = Tiles::new(size).expect("tiles");
    tiles.mark(Rect::new(10, 2, 20, 15));
    tiles.mark(Rect::new(35, 18, 100, 100));
    assert_eq!(tiles.count(), 5);
    let spans: Vec<_> = tiles.spans(0..tiles.rows()).collect();
    assert_eq!(spans, [(0..32, 0..16), (0..40, 16..20)]);
    assert_eq!(tiles.spans(1..2).count(), 1, "one row of tiles alone");
    assert_eq!(tiles.spans(2..9).count(), 0, "no rows past the picture");
    tiles.clear();
    assert_eq!(tiles.count(), 0);
}

/// However scattered the marks, a cover fits its budget, covers every marked
/// tile, and leaves the marks as they were.
#[test]
fn a_cover_fits_its_budget_and_leaves_the_marks_alone() {
    let size = (640u32, 360u32);
    let mut tiles = Tiles::new(size).expect("tiles");
    let mut room = Tiles::new(size).expect("room");
    let scattered: Vec<Rect> = (0..400u32)
        .map(|n| {
            let x = (n * 97) % size.0;
            let y = (n * 61) % size.1;
            Rect::new(
                i32::try_from(x).expect("small"),
                i32::try_from(y).expect("small"),
                1,
                1,
            )
        })
        .collect();
    for rect in &scattered {
        tiles.mark(*rect);
    }
    let marked = tiles.count();
    let spans: Vec<_> = tiles.spans(0..tiles.rows()).collect();
    let mut damage = Region::new();
    tiles.cover(&mut room, &mut damage);
    assert!(
        damage.rects().len() <= COVER_BUDGET,
        "{}",
        damage.rects().len()
    );
    for rect in &scattered {
        assert!(damage.contains(rect.origin), "{rect:?} uncovered");
    }
    assert_eq!(tiles.count(), marked);
    assert_eq!(tiles.spans(0..tiles.rows()).collect::<Vec<_>>(), spans);
    let tile = i32::try_from(TILE).expect("small");
    for rect in damage.rects() {
        assert_eq!(rect.left() % tile, 0, "{rect:?}");
        assert_eq!(rect.top() % tile, 0, "{rect:?}");
    }
}
