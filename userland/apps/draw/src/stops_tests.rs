//! The stop bar's hits and its painting.

use alloc::vec;

use tairix_colour::Rgba;
use tairix_drawing::Stop;
use tairix_geometry::{Point, Rect, Scale};
use tairix_raster::Surface;
use tairix_theme::Theme;

use super::Bar;

const RED: Rgba = Rgba::new(255, 0, 0, 255);
const BLUE: Rgba = Rgba::new(0, 0, 255, 255);

fn stops() -> vec::Vec<Stop> {
    vec![
        Stop {
            offset: 0.0,
            colour: RED,
        },
        Stop {
            offset: 0.5,
            colour: BLUE,
        },
        Stop {
            offset: 0.5,
            colour: RED,
        },
        Stop {
            offset: 1.0,
            colour: BLUE,
        },
    ]
}

fn bar() -> Bar {
    Bar::of(Rect::new(10, 20, 220, 40))
}

#[test]
fn the_strip_runs_from_offset_zero_to_one() {
    let bar = bar();
    assert!(bar.offset_at(0).abs() < f64::EPSILON, "held at the start");
    assert!(
        (bar.offset_at(1000) - 1.0).abs() < f64::EPSILON,
        "and the end"
    );
    let middle = bar.offset_at(10 + 110);
    assert!((middle - 0.5).abs() < 0.01, "{middle}");
    assert!(bar.on_strip(Point::new(120, 25)));
    assert!(!bar.on_strip(Point::new(120, 55)), "the handles' band");
}

#[test]
fn a_press_takes_the_stop_in_hand_before_a_nearer_one() {
    let bar = bar();
    let stops = stops();
    let handles_y = 52;
    assert_eq!(
        bar.handle_at(&stops, 0, Point::new(120, handles_y)),
        Some(1)
    );
    assert_eq!(
        bar.handle_at(&stops, 2, Point::new(120, handles_y)),
        Some(2),
        "of two stops at one place, the one in hand"
    );
    assert_eq!(bar.handle_at(&stops, 0, Point::new(70, handles_y)), None);
    assert_eq!(
        bar.handle_at(&stops, 0, Point::new(120, 25)),
        None,
        "the strip holds no handle"
    );
}

#[test]
fn each_handle_shows_its_stops_colour() {
    let mut surface = Surface::new(240, 80).expect("a surface");
    let bar = bar();
    let stops = stops();
    bar.render(
        &mut surface,
        (&stops, 1, false),
        Scale::ONE,
        &Theme::light(),
    );
    let at = |x: u32, y: u32| surface.pixels()[usize::try_from(y * 240 + x).expect("index")];
    let first = at(10 + 8, 52);
    assert_eq!((first.r, first.g, first.b), (255, 0, 0), "{first:?}");
    let last = at(10 + 220 - 8, 52);
    assert_eq!((last.r, last.g, last.b), (0, 0, 255), "{last:?}");
    let start = at(20, 30);
    assert!(
        start.r > 200 && start.b < 60,
        "the strip begins red: {start:?}"
    );
}
