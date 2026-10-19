//! The camera looks at its focus from its distance and tilt, keeps the focus
//! inside the realm, and keeps its eye above the ground.
#![allow(
    clippy::float_arithmetic,
    reason = "these cases measure where the perspective puts points, which \
              it answers in reals"
)]

use super::*;
use crate::quality::RenderScale;
use tairix_wintersun_world::params::{RealmParams, RealmSpec};

fn viewport(width: u32, height: u32) -> Viewport {
    Viewport::new(width, height, RenderScale::ONE).expect("a window with pixels")
}

fn params(extent_chunks: u32) -> RealmParams {
    let spec = RealmSpec {
        extent_chunks,
        ..RealmParams::default_realm(0x5749_4E54_4552).spec()
    };
    RealmParams::new(spec).expect("the spec is within its own stated range")
}

/// Level ground at `height` everywhere.
fn level(height: i32) -> impl Fn(WorldPoint) -> Option<i32> {
    move |_| Some(height)
}

#[test]
fn zoom_stops_are_doublings_within_the_stated_range() {
    assert_eq!(Zoom::NEAREST.distance(), 6 * CELL_SUB_UNITS);
    assert_eq!(Zoom::DEFAULT.distance(), 24 * CELL_SUB_UNITS);
    assert_eq!(Zoom::FURTHEST.distance(), 96 * CELL_SUB_UNITS);
    assert_eq!(Zoom::new(0), Zoom::NEAREST, "below the range clamps in");
    assert_eq!(Zoom::new(99), Zoom::FURTHEST, "above the range clamps in");
    assert_eq!(Zoom::NEAREST.nearer(), None);
    assert_eq!(Zoom::FURTHEST.further(), None);

    let mut stops = alloc::vec![Zoom::NEAREST];
    while let Some(next) = stops.last().and_then(|z| z.further()) {
        stops.push(next);
    }
    assert_eq!(stops.len(), 5, "five stops, each a doubling");
    for pair in stops.windows(2) {
        assert_eq!(pair[1].distance(), pair[0].distance() * 2);
    }
}

#[test]
fn a_tilt_stays_between_across_the_land_and_straight_down() {
    assert_eq!(Tilt::new(0), Tilt::LOWEST);
    assert_eq!(Tilt::new(u16::MAX), Tilt::STRAIGHT_DOWN);
    assert_eq!(Tilt::STRAIGHT_DOWN.units(), 0x4000);
    assert!(Tilt::LOWEST < Tilt::DEFAULT && Tilt::DEFAULT < Tilt::STRAIGHT_DOWN);
    assert_eq!(Tilt::new(Tilt::DEFAULT.units()), Tilt::DEFAULT);
}

#[test]
fn the_camera_looks_at_its_focus_from_its_distance_and_tilt() {
    let target = WorldPoint {
        x: 12_345,
        y: -6_789,
    };
    let view = viewport(640, 360);
    for tilt in [Tilt::LOWEST, Tilt::DEFAULT, Tilt::STRAIGHT_DOWN] {
        for zoom in [Zoom::NEAREST, Zoom::FURTHEST] {
            let mut camera = Camera::new(target, zoom, realm_bounds(params(64)));
            camera.set_tilt(tilt);
            let seen = camera.perspective(&view, 500, level(500)).expect("a view");
            let focus = [
                f64::from(target.x),
                f64::from(target.y),
                f64::from(500 + FOCUS_LIFT),
            ];
            let at = seen.project(seen.view(focus)).expect("in front of the eye");
            assert!(
                (at.x - 320.0).abs() < 1e-6 && (at.y - 180.0).abs() < 1e-6,
                "{tilt:?} {zoom:?}"
            );
            let eye = seen.eye();
            let gap = [eye[0] - focus[0], eye[1] - focus[1], eye[2] - focus[2]];
            let apart = mathf::sqrt(gap[0] * gap[0] + gap[1] * gap[1] + gap[2] * gap[2]);
            assert!((apart - f64::from(zoom.distance())).abs() < 1e-6);
            // It stands to the south and above, never below what it looks at.
            assert!(eye[2] > focus[2] && eye[1] >= focus[1] - 1e-6);
        }
    }
}

#[test]
fn north_is_up_the_screen_and_east_to_its_right() {
    let mut camera = Camera::new(
        WorldPoint { x: 0, y: 0 },
        Zoom::DEFAULT,
        realm_bounds(params(64)),
    );
    camera.set_tilt(Tilt::STRAIGHT_DOWN);
    let view = viewport(640, 360);
    let seen = camera.perspective(&view, 0, level(0)).expect("a view");
    let at = |x: f64, y: f64| seen.project(seen.view([x, y, 0.0])).expect("in view");
    let (middle, north, east) = (at(0.0, 0.0), at(0.0, -2_000.0), at(2_000.0, 0.0));
    assert!(north.y < middle.y, "north is drawn down the screen");
    assert!(east.x > middle.x, "east is drawn to the left");
}

#[test]
fn the_eye_never_stands_in_the_ground_beneath_it() {
    let camera = Camera::new(
        WorldPoint { x: 0, y: 0 },
        Zoom::DEFAULT,
        realm_bounds(params(64)),
    );
    let view = viewport(640, 360);
    // A ridge behind the focus, higher than the eye would otherwise stand.
    let ridge = 200 * CELL_SUB_UNITS;
    let seen = camera
        .perspective(&view, 0, |at| Some(if at.y > 2_000 { ridge } else { 0 }))
        .expect("a view");
    assert!(seen.eye()[2] >= f64::from(ridge + CLEARANCE) - 1e-6);
    // And it still looks at its focus.
    let focus = [0.0, 0.0, f64::from(FOCUS_LIFT)];
    let at = seen.project(seen.view(focus)).expect("in view");
    assert!((at.x - 320.0).abs() < 1e-6 && (at.y - 180.0).abs() < 1e-6);
}

#[test]
fn the_focus_stays_inside_the_realm() {
    let bounds = realm_bounds(params(8));
    let inside = WorldPoint { x: 100, y: -200 };
    let mut camera = Camera::new(inside, Zoom::DEFAULT, bounds);
    assert_eq!(camera.focus(), inside);
    camera.look_at(WorldPoint {
        x: i32::MAX,
        y: i32::MIN,
    });
    let focus = camera.focus();
    assert_eq!((focus.x, focus.y), (bounds.max_x - 1, bounds.min_y));
    // The extremes of the coordinate space project without overflowing.
    assert!(camera
        .perspective(&viewport(4096, 4096), i32::MAX, level(i32::MAX))
        .is_ok());
}

#[test]
fn a_figure_at_the_focus_is_drawn_at_the_distance_over_the_focal_length() {
    let camera = Camera::new(
        WorldPoint { x: 0, y: 0 },
        Zoom::DEFAULT,
        realm_bounds(params(64)),
    );
    let small = viewport(640, 360);
    let large = viewport(1280, 720);
    assert_eq!(focal(&small), 385);
    assert_eq!(focal(&large), 771);
    let expected = u32::try_from(24 * CELL_SUB_UNITS)
        .expect("positive")
        .div_ceil(385);
    assert_eq!(
        camera.focus_step(&small),
        i32::try_from(expected).expect("small")
    );
    assert!(camera.focus_step(&large) < camera.focus_step(&small));
    let further = Camera::new(
        WorldPoint { x: 0, y: 0 },
        Zoom::FURTHEST,
        realm_bounds(params(64)),
    );
    assert!(further.focus_step(&small) >= camera.focus_step(&small) * 4);
}

/// The bounds are exactly the chunks the realm holds: the last sub-unit on
/// each side lies in one, the next lies in none.
#[test]
fn realm_bounds_are_the_chunks_the_realm_holds() {
    let params = params(16);
    let bounds = realm_bounds(params);
    let chunk =
        |at: i32| at.div_euclid(CELL_SUB_UNITS * i32::try_from(CHUNK_CELLS).expect("small"));
    for (inside, outside) in [
        (bounds.min_x, bounds.min_x - 1),
        (bounds.max_x - 1, bounds.max_x),
    ] {
        assert!(
            params.holds_chunk(chunk(inside), 0),
            "{inside} is not in the realm"
        );
        assert!(
            !params.holds_chunk(chunk(outside), 0),
            "{outside} is in the realm"
        );
        assert!(params.holds_chunk(0, chunk(inside)) && !params.holds_chunk(0, chunk(outside)));
    }
    assert_eq!(bounds.min_x, bounds.min_y, "the realm is square");
    assert_eq!(bounds.max_x, bounds.max_y, "the realm is square");
}

#[test]
fn a_camera_over_bounds_of_no_extent_keeps_its_wish() {
    let none = Bounds {
        min_x: 5,
        min_y: 5,
        max_x: 5,
        max_y: 5,
    };
    let wish = WorldPoint { x: 9, y: -3 };
    assert_eq!(Camera::new(wish, Zoom::DEFAULT, none).focus(), wish);
}
