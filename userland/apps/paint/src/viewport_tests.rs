use tairix_geometry::{Point, Rect};

use super::{Viewport, Zoom, ACTUAL, ZOOMS};
use crate::shape::{Bounds, FX};

const AREA: Rect = Rect::new(10, 20, 200, 100);

#[test]
fn a_small_picture_is_centred_and_maps_back_to_its_pixels() {
    let view = Viewport::new((1, 1));
    assert_eq!(view.origin((50, 40), AREA), (10 + 75, 20 + 30));
    assert_eq!(
        view.pixel_at(Point::new(85, 50), (50, 40), AREA),
        Some((0, 0))
    );
    assert_eq!(
        view.pixel_at(Point::new(134, 89), (50, 40), AREA),
        Some((49, 39))
    );
    assert_eq!(view.pixel_at(Point::new(84, 50), (50, 40), AREA), None);
}

#[test]
fn a_magnified_pixel_spans_its_zoom_and_a_point_inside_it_keeps_its_fraction() {
    let mut view = Viewport::new((1, 1));
    let picture = (1000, 1000);
    assert!(view.zoom_to(ACTUAL + 5, Point::new(10, 20), picture, AREA));
    assert_eq!(view.zoom(), Zoom::of((8, 1)));
    assert_eq!(view.rung(), Some(ACTUAL + 5));
    assert_eq!(view.pixel_span(), (8, 8));
    let (x, _) = view.to_picture(Point::new(50, 20), picture, AREA);
    let (next, _) = view.to_picture(Point::new(51, 20), picture, AREA);
    assert_eq!(
        next - x,
        FX / 8,
        "a screen pixel is an eighth of a picture pixel"
    );
    assert_eq!(
        x.rem_euclid(FX / 8),
        FX / 16,
        "each at the middle of its screen pixel"
    );
}

#[test]
fn tall_pixels_are_drawn_twice_as_tall() {
    let view = Viewport::new((1, 2));
    assert_eq!(view.extent((10, 10)), (10, 20));
    let screen = view.to_screen(
        Bounds {
            x0: 1,
            y0: 1,
            x1: 2,
            y1: 2,
        },
        (10, 10),
        AREA,
    );
    assert_eq!((screen.width, screen.height), (1, 2));
}

#[test]
fn scrolling_stops_where_the_picture_ends() {
    let mut view = Viewport::new((1, 1));
    let picture = (500, 300);
    assert!(view.scroll_to(10_000, 10_000, picture, AREA));
    assert_eq!(view.scroll(), (300, 200));
    assert_eq!(view.origin(picture, AREA), (10 - 300, 20 - 200));
    assert!(!view.scroll_to(300, 200, picture, AREA));
}

#[test]
fn zooming_keeps_the_point_under_the_pointer_still() {
    let mut view = Viewport::new((1, 1));
    let picture = (2000, 2000);
    let anchor = Point::new(110, 70);
    let before = view.to_picture(anchor, picture, AREA);
    view.zoom_to(ACTUAL + 1, anchor, picture, AREA);
    let after = view.to_picture(anchor, picture, AREA);
    assert!((before.0 - after.0).abs() <= FX, "{before:?} {after:?}");
    assert!((before.1 - after.1).abs() <= FX);
}

#[test]
fn the_fitting_zoom_is_the_largest_that_shows_it_all_and_never_above_actual() {
    let view = Viewport::new((1, 1));
    assert_eq!(view.fitting((150, 90), AREA), ACTUAL);
    let rung = view.fitting((1600, 800), AREA);
    assert_eq!(ZOOMS[rung], (1, 8));
}

#[test]
fn a_reduced_view_maps_many_pixels_to_one() {
    let mut view = Viewport::new((1, 1));
    view.zoom_to(ACTUAL - 2, Point::new(10, 20), (800, 400), AREA);
    assert_eq!(view.pixel_span(), (0, 0), "a pixel spans less than one");
    assert_eq!(view.percent(), 25);
    assert_eq!(view.extent((800, 400)), (200, 100));
    let screen = view.to_screen(
        Bounds {
            x0: 0,
            y0: 0,
            x1: 3,
            y1: 3,
        },
        (800, 400),
        AREA,
    );
    assert_eq!(
        (screen.width, screen.height),
        (1, 1),
        "three pixels fall in one"
    );
}

#[test]
fn every_rung_is_an_exact_zoom_and_a_pinch_scales_within_the_ladder() {
    for (rung, &(num, den)) in ZOOMS.iter().enumerate() {
        let zoom = Zoom::of((num, den));
        assert_eq!(zoom.percent(), num * 100 / den, "rung {rung}");
    }
    let actual = Zoom::of(ZOOMS[ACTUAL]);
    assert_eq!(
        actual.scaled(tairix_abi::touch::PINCH_SCALE_ONE * 2),
        Zoom::of((2, 1))
    );
    assert_eq!(actual.scaled(u32::MAX), Zoom::MOST, "held to the top");
    assert_eq!(actual.scaled(1), Zoom::LEAST, "and to the bottom");
}

#[test]
fn stepping_goes_from_the_nearest_rung_in_its_direction() {
    let mut view = Viewport::new((1, 1));
    let picture = (1000, 1000);
    assert_eq!(view.rung_beside(1), ACTUAL + 1);
    assert_eq!(view.rung_beside(-2), ACTUAL - 2);
    assert_eq!(view.rung_beside(100), ZOOMS.len() - 1);
    assert_eq!(view.rung_beside(-100), 0);
    // Between 1:1 and 2:1, one step in either direction lands beside it.
    assert!(view.magnify(
        Zoom::of(ZOOMS[ACTUAL]).scaled(tairix_abi::touch::PINCH_SCALE_ONE * 3 / 2),
        Point::new(10, 20),
        picture,
        AREA
    ));
    assert_eq!(view.rung(), None);
    assert_eq!(view.rung_beside(1), ACTUAL + 1);
    assert_eq!(view.rung_beside(-1), ACTUAL);
    assert_eq!(view.rung_beside(2), ACTUAL + 2);
}

#[test]
fn a_scroll_by_is_kept_to_the_picture() {
    let mut view = Viewport::new((1, 1));
    let picture = (1000, 1000);
    assert!(view.scroll_by(30, 40, picture, AREA));
    assert_eq!(view.scroll(), (30, 40));
    assert!(view.scroll_by(-100, -100, picture, AREA));
    assert_eq!(view.scroll(), (0, 0), "held at the top left");
    assert!(view.scroll_by(i64::MAX, i64::MAX, picture, AREA));
    assert_eq!(view.scroll(), (800, 900), "and at the far edge");
}
