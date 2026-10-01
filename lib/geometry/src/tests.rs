//! Unit tests for the integer geometry primitives.

use super::{to_i32, Point, Rect, Scale, REFERENCE_DPI};

#[test]
fn rect_intersection_overlap() {
    let a = Rect::new(0, 0, 4, 4);
    let b = Rect::new(2, 2, 4, 4);
    assert_eq!(a.intersection(&b), Rect::new(2, 2, 2, 2));
}

#[test]
fn rect_intersection_disjoint_is_empty() {
    let a = Rect::new(0, 0, 2, 2);
    let b = Rect::new(5, 5, 2, 2);
    assert!(a.intersection(&b).is_empty());
}

#[test]
fn rect_union_with_empty_is_other() {
    let r = Rect::new(1, 2, 3, 4);
    assert_eq!(Rect::EMPTY.union(&r), r);
    assert_eq!(r.union(&Rect::EMPTY), r);
}

#[test]
fn rect_union_covers_both() {
    let a = Rect::new(0, 0, 2, 2);
    let b = Rect::new(4, 4, 2, 2);
    assert_eq!(a.union(&b), Rect::new(0, 0, 6, 6));
}

#[test]
fn rect_contains_is_half_open() {
    let r = Rect::new(0, 0, 2, 2);
    assert!(r.contains(Point::new(0, 0)));
    assert!(r.contains(Point::new(1, 1)));
    assert!(!r.contains(Point::new(2, 0)));
    assert!(!r.contains(Point::new(0, 2)));
}

#[test]
fn clamped_onto_leaves_a_wholly_contained_rect_where_it_is() {
    let screen = Rect::new(0, 0, 100, 100);
    let r = Rect::new(10, 20, 30, 40);
    assert_eq!(r.clamped_onto(screen), r);
}

#[test]
fn clamped_onto_pulls_back_a_rect_past_the_far_edge() {
    let screen = Rect::new(0, 0, 100, 100);
    // Right/bottom edges would fall at 130/150; pull back so the far edge
    // stops at the screen far edge (origin 70/60).
    let r = Rect::new(90, 80, 40, 70);
    assert_eq!(r.clamped_onto(screen), Rect::new(60, 30, 40, 70));
}

#[test]
fn clamped_onto_pushes_a_rect_past_the_near_edge_forward() {
    let screen = Rect::new(0, 0, 100, 100);
    let r = Rect::new(-20, -5, 30, 30);
    assert_eq!(r.clamped_onto(screen), Rect::new(0, 0, 30, 30));
}

#[test]
fn clamped_onto_pins_an_oversize_rect_to_the_leading_edge() {
    let screen = Rect::new(0, 0, 50, 50);
    let r = Rect::new(20, 20, 80, 80);
    assert_eq!(r.clamped_onto(screen), Rect::new(0, 0, 80, 80));
}

#[test]
fn clamped_onto_respects_a_non_origin_screen() {
    let screen = Rect::new(100, 100, 200, 200);
    let r = Rect::new(0, 0, 40, 40);
    assert_eq!(r.clamped_onto(screen), Rect::new(100, 100, 40, 40));
}

#[test]
fn empty_rect_is_empty() {
    assert!(Rect::EMPTY.is_empty());
    assert!(Rect::new(0, 0, 0, 5).is_empty());
    assert!(Rect::new(0, 0, 5, 0).is_empty());
    assert!(!Rect::new(0, 0, 1, 1).is_empty());
}

#[test]
fn edges_saturate_rather_than_wrap() {
    let r = Rect::new(i32::MAX - 1, i32::MAX - 1, 100, 100);
    assert_eq!(r.right(), i32::MAX);
    assert_eq!(r.bottom(), i32::MAX);
}

#[test]
fn scale_one_is_identity() {
    let one = Scale::ONE;
    assert_eq!(one.percent(), 100);
    assert_eq!(one.dpi(), REFERENCE_DPI);
    assert_eq!(one.scale_length(40), 40);
    assert_eq!(one.scale_length(0), 0);
    assert_eq!(Scale::default(), Scale::ONE);
}

#[test]
fn scale_length_scales_logical_to_physical() {
    let double = Scale::from_percent(200).expect("200% is in range");
    assert_eq!(double.scale_length(40), 80);
    let half = Scale::from_percent(50).expect("50% is in range");
    assert_eq!(half.scale_length(40), 20);
    let one_and_half = Scale::from_percent(150).expect("150% is in range");
    // Truncating division: 12 logical px at 150% is 18 physical px.
    assert_eq!(one_and_half.scale_length(12), 18);
}

#[test]
fn scale_length_saturates_rather_than_wrapping() {
    let big = Scale::from_percent(800).expect("800% is in range");
    assert_eq!(big.scale_length(u32::MAX), u32::MAX);
}

#[test]
fn out_of_range_percentages_are_rejected() {
    assert_eq!(Scale::from_percent(0), None);
    assert_eq!(Scale::from_percent(Scale::MIN_PERCENT - 1), None);
    assert_eq!(Scale::from_percent(Scale::MAX_PERCENT + 1), None);
    assert!(Scale::from_percent(Scale::MIN_PERCENT).is_some());
    assert!(Scale::from_percent(Scale::MAX_PERCENT).is_some());
}

#[test]
fn dpi_round_trips_through_the_reference_density() {
    let from_dpi = Scale::from_dpi(REFERENCE_DPI * 2).expect("192 DPI is in range");
    assert_eq!(from_dpi.percent(), 200);
    assert_eq!(from_dpi.dpi(), REFERENCE_DPI * 2);
    assert_eq!(Scale::from_dpi(REFERENCE_DPI), Some(Scale::ONE));
    // A density below the floor maps to a rejected percentage.
    assert_eq!(Scale::from_dpi(1), None);
}

#[test]
fn to_i32_carries_ordinary_extents_through() {
    assert_eq!(to_i32(0), 0);
    assert_eq!(to_i32(40), 40);
    assert_eq!(to_i32(u32::try_from(i32::MAX).expect("fits")), i32::MAX);
}

#[test]
fn to_i32_saturates_rather_than_wrapping() {
    assert_eq!(to_i32(u32::MAX), i32::MAX);
}

#[test]
fn center_rounds_toward_the_top_left_on_an_odd_extent() {
    assert_eq!(Rect::new(10, 20, 4, 6).center(), Point::new(12, 23));
    assert_eq!(Rect::new(10, 20, 5, 7).center(), Point::new(12, 23));
    assert_eq!(Rect::new(-10, -20, 4, 6).center(), Point::new(-8, -17));
}

#[test]
fn center_saturates_rather_than_wrapping() {
    // The widest rectangle the space admits: half its extent is exactly
    // `i32::MAX`, so a wrapping add would land negative.
    let r = Rect::new(1, 1, u32::MAX, u32::MAX);
    assert_eq!(r.center(), Point::new(i32::MAX, i32::MAX));
}

#[test]
fn center_of_the_empty_rect_is_its_origin() {
    assert_eq!(Rect::new(7, 9, 0, 0).center(), Point::new(7, 9));
}

#[test]
fn inset_takes_the_margin_off_every_side() {
    assert_eq!(
        Rect::new(10, 20, 30, 40).inset(4),
        Rect::new(14, 24, 22, 32)
    );
    assert_eq!(
        Rect::new(-10, -20, 30, 40).inset(0),
        Rect::new(-10, -20, 30, 40)
    );
}

#[test]
fn inset_past_the_middle_leaves_nothing() {
    assert_eq!(Rect::new(10, 20, 8, 40).inset(4), Rect::EMPTY);
    assert_eq!(Rect::new(10, 20, 40, 7).inset(4), Rect::EMPTY);
    assert_eq!(Rect::new(0, 0, 40, 40).inset(u32::MAX), Rect::EMPTY);
}

#[test]
fn taking_bands_partitions_the_rectangle() {
    let mut rest = Rect::new(5, 7, 100, 60);
    assert_eq!(rest.take_top(10), Rect::new(5, 7, 100, 10));
    assert_eq!(rest.take_bottom(12), Rect::new(5, 55, 100, 12));
    assert_eq!(rest.take_left(20), Rect::new(5, 17, 20, 38));
    assert_eq!(rest.take_right(30), Rect::new(75, 17, 30, 38));
    assert_eq!(rest, Rect::new(25, 17, 50, 38));
}

#[test]
fn a_band_wider_than_the_rest_takes_all_of_it() {
    let mut rest = Rect::new(0, 0, 10, 8);
    assert_eq!(rest.take_top(20), Rect::new(0, 0, 10, 8));
    assert!(rest.is_empty());
    assert_eq!(rest.take_left(5), Rect::new(0, 8, 5, 0));
    let mut rest = Rect::new(0, 0, 10, 8);
    assert_eq!(rest.take_right(u32::MAX), Rect::new(0, 0, 10, 8));
    assert_eq!(rest.width, 0);
}

#[test]
fn surface_origin_refuses_what_a_surface_cannot_address() {
    assert_eq!(Rect::new(3, 4, 5, 6).surface_origin(), Some((3, 4)));
    assert_eq!(Rect::new(-1, 4, 5, 6).surface_origin(), None);
    assert_eq!(Rect::new(3, -4, 5, 6).surface_origin(), None);
    assert_eq!(Rect::new(3, 4, 0, 6).surface_origin(), None);
}
