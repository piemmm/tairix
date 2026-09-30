//! Host tests of what lies under the craft: kept and laid back exactly.

use tairix_raster::Pixel;
use tairix_wm::{Rect, Surface};

use super::Under;

/// A surface whose every pixel says where it is.
fn numbered() -> Surface {
    let mut surface = Surface::new(40, 30).expect("a surface");
    for y in 0..30u8 {
        for x in 0..40u8 {
            surface.set(
                u32::from(x),
                u32::from(y),
                Pixel {
                    r: x,
                    g: y,
                    b: x ^ y,
                    a: 255,
                },
            );
        }
    }
    surface
}

/// Whatever is drawn over the kept boxes after they were kept, laying them
/// back restores them exactly, overlapping boxes included, and leaves every
/// other pixel as it is.
#[test]
fn what_was_kept_is_laid_back_exactly() {
    let original = numbered();
    let mut surface = original.clone();
    let boxes = [
        Rect::new(2, 3, 10, 6),
        Rect::new(8, 5, 9, 9),
        Rect::new(30, 20, 10, 10),
    ];
    let mut under = Under::new();
    under.keep(&surface, &boxes);
    assert!(under.is_whole());
    assert_eq!(under.boxes(), &boxes[..]);
    surface.fill(tairix_wm::Color::rgb(200, 10, 10));
    under.lay_back(&mut surface);
    for y in 0..30 {
        for x in 0..40 {
            let point = tairix_wm::Point::new(
                i32::try_from(x).expect("small"),
                i32::try_from(y).expect("small"),
            );
            let inside = boxes.iter().any(|rect| rect.contains(point));
            let pixel = surface.get(x, y).expect("in bounds");
            if inside {
                assert_eq!(Some(pixel), original.get(x, y), "({x}, {y}) laid back");
            } else {
                assert_eq!(
                    (pixel.r, pixel.g, pixel.b),
                    (200, 10, 10),
                    "({x}, {y}) left alone"
                );
            }
        }
    }
}

/// Keeping anew lets go of what was kept before, and keeping nothing lays
/// nothing back.
#[test]
fn keeping_anew_lets_the_last_go() {
    let mut surface = numbered();
    let mut under = Under::new();
    assert!(
        under.is_whole() && under.boxes().is_empty(),
        "nothing to keep at first"
    );
    under.keep(&surface, &[Rect::new(0, 0, 5, 5)]);
    under.keep(&surface, &[]);
    assert!(under.boxes().is_empty());
    surface.fill(tairix_wm::Color::rgb(1, 1, 1));
    under.lay_back(&mut surface);
    assert!(surface
        .pixels()
        .iter()
        .all(|pixel| (pixel.r, pixel.g, pixel.b) == (1, 1, 1)));
}
