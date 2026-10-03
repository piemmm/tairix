use alloc::vec;
use alloc::vec::Vec;

use super::{scale, Combine, Mask};
use crate::canvas::{CanvasBuilder, Kind, Sample};
use crate::fill::region;
use crate::shape::{Bounds, Point, Shape, ShapeScratch, Span, FX};

const PICTURE: Bounds = Bounds {
    x0: 0,
    y0: 0,
    x1: 40,
    y1: 30,
};

fn bounds(x0: i64, y0: i64, x1: i64, y1: i64) -> Bounds {
    Bounds { x0, y0, x1, y1 }
}

/// Every pixel of `mask` over the picture.
fn grid(mask: &Mask) -> Vec<Vec<u8>> {
    (PICTURE.y0..PICTURE.y1)
        .map(|y| {
            let mut row = vec![0u8; 40];
            mask.row(y, 0, &mut row);
            row
        })
        .collect()
}

fn chosen(mask: &Mask) -> u64 {
    grid(mask).iter().flatten().map(|&a| u64::from(a)).sum()
}

#[test]
fn a_rectangle_holds_no_pixels_of_its_own() {
    let mask = Mask::rect(bounds(2, 3, 10, 8)).expect("pixels");
    assert!(mask.is_rect());
    assert_eq!(mask.at(2, 3), 255);
    assert_eq!(mask.at(10, 3), 0);
    assert_eq!(chosen(&mask), 8 * 5 * 255);
    assert!(Mask::rect(bounds(4, 4, 4, 9)).is_none());
}

#[test]
fn an_ellipse_is_traced_and_trimmed_to_what_it_chooses() {
    let mut scratch = ShapeScratch::default();
    let oval = Shape::Ellipse {
        span: Span {
            from: (5, 5),
            to: (24, 14),
        },
        outline: None,
    };
    let mask = Mask::shape(&oval, true, PICTURE, &mut scratch)
        .expect("room")
        .expect("pixels");
    assert!(!mask.is_rect());
    assert_eq!(mask.at(15, 10), 255);
    assert_eq!(mask.at(5, 5), 0);
    let area = f64::from(u32::try_from(chosen(&mask)).expect("small")) / 255.0;
    assert!(
        (area - core::f64::consts::PI * 10.0 * 5.0).abs() < 2.0,
        "{area}"
    );
    let b = mask.bounds();
    assert!(b.x0 >= 5 && b.x1 <= 25 && b.y0 >= 5 && b.y1 <= 15);
}

#[test]
fn a_lasso_encloses_its_path() {
    let mut scratch = ShapeScratch::default();
    let at = |x: i64, y: i64| Point {
        x: x * FX,
        y: y * FX,
    };
    let path = [at(2, 2), at(20, 2), at(20, 20), at(2, 20)];
    let mask = Mask::outline(&path, false, PICTURE, &mut scratch)
        .expect("room")
        .expect("pixels");
    assert_eq!(mask.at(10, 10), 255);
    assert_eq!(mask.at(1, 10), 0);
    assert_eq!(chosen(&mask), 18 * 18 * 255);
    assert!(
        Mask::outline(&path[..2], false, PICTURE, &mut scratch)
            .expect("room")
            .is_none(),
        "a line encloses nothing"
    );
}

#[test]
fn a_wand_chooses_the_region_its_flood_reaches() {
    let mut built = CanvasBuilder::new(40, 30, Kind::Rgba, Sample::Rgba([255; 4])).expect("fits");
    for y in 0..30 {
        built.set(20, y, Sample::Rgba([0, 0, 0, 255]));
    }
    let canvas = built.finish();
    let reached = region(&canvas, 3, 3, 0)
        .expect("room")
        .expect("on the picture");
    let mask = Mask::region(&reached).expect("room").expect("pixels");
    assert_eq!(mask.bounds(), bounds(0, 0, 20, 30));
    assert_eq!(chosen(&mask), 20 * 30 * 255);
}

#[test]
fn selections_combine_four_ways() {
    let left = Mask::rect(bounds(0, 0, 10, 10)).expect("pixels");
    let right = Mask::rect(bounds(5, 5, 15, 15)).expect("pixels");
    let added = left
        .combined(&right, Combine::Add)
        .expect("room")
        .expect("pixels");
    assert_eq!(chosen(&added), (100 + 100 - 25) * 255);
    let taken = left
        .combined(&right, Combine::Subtract)
        .expect("room")
        .expect("pixels");
    assert_eq!(chosen(&taken), 75 * 255);
    assert_eq!(taken.at(7, 7), 0);
    let both = left
        .combined(&right, Combine::Intersect)
        .expect("room")
        .expect("pixels");
    assert!(both.is_rect(), "two rectangles meet in one");
    assert_eq!(both.bounds(), bounds(5, 5, 10, 10));
    assert_eq!(
        left.combined(&right, Combine::Replace).expect("room"),
        Some(right.clone())
    );
    let inner = Mask::rect(bounds(2, 2, 4, 4)).expect("pixels");
    assert_eq!(
        left.combined(&inner, Combine::Add).expect("room"),
        Some(left.clone()),
        "the larger rectangle"
    );
    let apart = Mask::rect(bounds(30, 20, 35, 25)).expect("pixels");
    assert_eq!(
        left.combined(&apart, Combine::Intersect).expect("room"),
        None
    );
    assert_eq!(
        left.combined(&left, Combine::Subtract).expect("room"),
        None,
        "nothing is left"
    );
}

#[test]
fn a_feathered_edge_falls_away_smoothly_and_keeps_the_middle() {
    let mask = Mask::rect(bounds(10, 10, 30, 20)).expect("pixels");
    let soft = mask.feathered(3, PICTURE).expect("room").expect("pixels");
    assert_eq!(soft.at(20, 15), 255, "the middle stays chosen");
    let across: Vec<u8> = (6..14).map(|x| soft.at(x, 15)).collect();
    assert!(
        across.windows(2).all(|pair| pair[0] <= pair[1]),
        "{across:?}"
    );
    assert!(soft.at(10, 15) > 64 && soft.at(10, 15) < 255);
    assert!(soft.bounds().x0 >= 7);
    assert_eq!(
        mask.feathered(0, PICTURE).expect("room"),
        Some(mask.clone())
    );
}

#[test]
fn a_pixel_counts_as_inside_once_chosen_at_least_half() {
    let soft = Mask::rect(bounds(10, 10, 30, 20))
        .expect("pixels")
        .feathered(4, PICTURE)
        .expect("room")
        .expect("pixels");
    let first = (0..40)
        .find(|&x| soft.chooses(x, 15))
        .expect("inside somewhere");
    assert!(soft.at(first, 15) >= 128 && soft.at(first - 1, 15) < 128);
    assert!(soft.chooses(20, 15));
    assert!(!soft.chooses(0, 15));
}

#[test]
fn a_selection_held_to_the_picture_cuts_a_rectangle_and_keeps_an_outline() {
    let off = Mask::rect(bounds(-5, -5, 10, 10)).expect("pixels");
    assert_eq!(
        off.within(PICTURE).map(|mask| mask.bounds()),
        Some(bounds(0, 0, 10, 10))
    );
    let gone = Mask::rect(bounds(50, 50, 60, 60)).expect("pixels");
    assert_eq!(gone.within(PICTURE), None, "wholly off the picture");
    let mut scratch = ShapeScratch::default();
    let at = |x: i64, y: i64| Point {
        x: x * FX,
        y: y * FX,
    };
    let lasso = Mask::outline(
        &[at(1, 2), at(13, 2), at(13, 9)],
        false,
        PICTURE,
        &mut scratch,
    )
    .expect("room")
    .expect("pixels")
    .shifted(-5, 0);
    let held = lasso.bounds();
    assert!(held.x0 < 0, "moved partly off the picture");
    assert_eq!(
        lasso.within(PICTURE).map(|mask| mask.bounds()),
        Some(held),
        "kept as it is"
    );
}

#[test]
fn a_shifted_selection_shares_its_pixels() {
    let mut scratch = ShapeScratch::default();
    let oval = Shape::Ellipse {
        span: Span {
            from: (2, 2),
            to: (12, 9),
        },
        outline: None,
    };
    let mask = Mask::shape(&oval, true, PICTURE, &mut scratch)
        .expect("room")
        .expect("pixels");
    let moved = mask.shifted(5, -1);
    assert_eq!(moved.at(12, 4), mask.at(7, 5));
    assert!(core::ptr::eq(
        moved.alpha.as_deref().expect("soft"),
        mask.alpha.as_deref().expect("soft")
    ));
}

#[test]
fn a_recipe_makes_and_meets_a_selection() {
    let square = |x0: i64, y0: i64, x1: i64, y1: i64| {
        super::Recipe::Shape(Shape::Rect {
            span: Span {
                from: (x0, y0),
                to: (x1 - 1, y1 - 1),
            },
            outline: None,
        })
    };
    let held = Mask::rect(bounds(0, 0, 10, 10)).expect("pixels");
    let made = |recipe: &super::Recipe, before: Option<&Mask>, combine| {
        super::select(recipe, before, combine, (0, false), PICTURE).expect("room")
    };
    let apart = square(20, 20, 25, 25);
    assert_eq!(
        made(&apart, None, Combine::Subtract),
        None,
        "nothing to take from"
    );
    assert_eq!(
        made(&apart, None, Combine::Add).map(|mask| mask.bounds()),
        Some(bounds(20, 20, 25, 25))
    );
    let off = square(50, 50, 60, 60);
    assert_eq!(
        made(&off, Some(&held), Combine::Add),
        Some(held.clone()),
        "adding nothing"
    );
    assert_eq!(made(&off, Some(&held), Combine::Intersect), None);
    let soft = super::select(
        &square(5, 5, 15, 15),
        None,
        Combine::Replace,
        (3, false),
        PICTURE,
    )
    .expect("room")
    .expect("pixels");
    assert!(!soft.is_rect(), "feathered");
}

#[test]
fn scaling_rounds_to_the_nearest() {
    assert_eq!(scale(255, 255), 255);
    assert_eq!(scale(255, 0), 0);
    assert_eq!(scale(128, 128), 64);
}
