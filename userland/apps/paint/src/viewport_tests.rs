use tairix_geometry::{Point, Rect};

use super::{Viewport, ACTUAL, ZOOMS};
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
    assert_eq!(ZOOMS[view.zoom()], (8, 1));
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
