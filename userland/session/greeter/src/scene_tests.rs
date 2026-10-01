//! Host tests of the ribbon behind the login column: what it paints, where it
//! keeps dark, the frames it asks for, and that a frame repaints only what it
//! reports.

use tairix_geometry::{Point, Rect};
use tairix_raster::Surface;
use tairix_ribbon::SKY;
use tairix_theme::motion::SceneClock;

use super::Scene;

/// A screen and a column across its upper middle, as the login screen lays
/// them out.
const SCREEN: Rect = Rect::new(0, 0, 640, 360);
const COLUMN: Rect = Rect::new(210, 20, 220, 210);

fn moving() -> Scene {
    Scene::new(SCREEN, COLUMN, 0, false).expect("a ribbon")
}

/// Every pixel whose value differs between `before` and `after`.
fn changed(before: &Surface, after: &Surface) -> impl Iterator<Item = Point> {
    let (width, height) = (before.width(), before.height());
    let pairs: alloc::vec::Vec<Point> = (0..height)
        .flat_map(|y| (0..width).map(move |x| (x, y)))
        .filter(|(x, y)| before.get(*x, *y) != after.get(*x, *y))
        .map(|(x, y)| {
            Point::new(
                i32::try_from(x).expect("a small screen"),
                i32::try_from(y).expect("a small screen"),
            )
        })
        .collect();
    pairs.into_iter()
}

#[test]
fn the_ribbon_is_painted_as_it_is_raised_and_keeps_the_column_dark() {
    let scene = moving();
    let layer = scene.layer();
    let sky = SKY.premultiply();
    let lit = (0..SCREEN.height)
        .flat_map(|y| (0..SCREEN.width).map(move |x| (x, y)))
        .filter(|(x, y)| layer.get(*x, *y) != Some(sky))
        .count();
    assert!(lit > 0, "the ribbon shows");
    for y in 20..230 {
        for x in 210..430 {
            assert_eq!(layer.get(x, y), Some(sky), "light at ({x}, {y})");
        }
    }
}

#[test]
fn a_moving_ribbon_asks_for_its_frames_and_a_still_one_for_none() {
    let scene = moving();
    assert_eq!(scene.due_in(0), Some(SceneClock::FRAME_NS));
    assert_eq!(
        scene.due_in(SceneClock::FRAME_NS / 2),
        Some(SceneClock::FRAME_NS - SceneClock::FRAME_NS / 2)
    );
    assert_eq!(
        scene.due_in(SceneClock::FRAME_NS * 3),
        Some(0),
        "an overdue frame is due now"
    );

    let mut still = Scene::new(SCREEN, COLUMN, 0, true).expect("a ribbon");
    assert_eq!(still.due_in(0), None);
    assert!(!still.advance(u64::MAX), "a still ribbon never moves");
}

#[test]
fn an_early_wake_moves_nothing() {
    let mut scene = moving();
    let before = scene.layer().clone();
    assert!(!scene.advance(SceneClock::FRAME_NS - 1));
    assert_eq!(changed(&before, scene.layer()).count(), 0, "it repainted");
}

/// A frame repaints the ribbon's layer only within the damage it reports, so
/// composing that damage and nothing else leaves no stale pixel on screen.
#[test]
fn a_frame_repaints_only_what_it_reports() {
    let mut scene = moving();
    let mut now = 0;
    for _ in 0..12 {
        now += SceneClock::FRAME_NS;
        let before = scene.layer().clone();
        assert!(scene.advance(now), "a frame moved the ribbon");
        let damage = scene.damage();
        assert!(!damage.is_empty());
        for pixel in changed(&before, scene.layer()) {
            assert!(
                damage.rects().iter().any(|rect| rect.contains(pixel)),
                "{pixel:?} changed outside the damage"
            );
        }
        assert_eq!(
            scene.due_in(now),
            Some(SceneClock::FRAME_NS),
            "the next frame a frame on"
        );
    }
}

#[test]
fn an_empty_screen_has_no_ribbon() {
    assert!(Scene::new(Rect::new(0, 0, 0, 360), COLUMN, 0, false).is_none());
}
