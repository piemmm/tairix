//! Unit tests for [`View`]: a client's picture taken opaque, refused whole for
//! damage it does not hold, and enlarged into its window's content only where
//! the content lacks it.

use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::driver::display::{DamageRect, DisplayFormat, DisplayMode};
use tairix_abi::window_ipc::{ViewFilter, WindowView};
use tairix_abi::Errno;

use super::View;
use crate::color::Pixel;
use crate::geometry::Rect;
use crate::surface::Surface;

/// A 4 × 2 picture shown at twice its size from the content's (2, 1).
fn spec() -> WindowView {
    WindowView {
        width_px: 4,
        height_px: 2,
        placement: DamageRect {
            x: 2,
            y: 1,
            width_px: 8,
            height_px: 4,
        },
        filter: ViewFilter::Sharp,
    }
}

/// The view's rows of a frame at a stride wider than the picture, every dot
/// `dot`.
fn frame(dot: [u8; 4]) -> (DisplayMode, Vec<u8>) {
    let surface = DisplayMode {
        width_px: 4,
        height_px: 2,
        stride_bytes: 24,
        format: DisplayFormat::Bgra8888,
    };
    let mut bytes = vec![0u8; 48];
    for row in bytes.as_chunks_mut::<24>().0 {
        for out in row[..16].as_chunks_mut::<4>().0 {
            *out = dot;
        }
    }
    (surface, bytes)
}

fn whole() -> DamageRect {
    DamageRect {
        x: 0,
        y: 0,
        width_px: 4,
        height_px: 2,
    }
}

/// A view let go leaves nothing of its picture behind.
#[test]
fn a_view_let_go_overwrites_its_picture() {
    let mut view = View::new(spec()).expect("a picture held");
    let (surface, bytes) = frame([0xFF, 0x80, 0x40, 0xFF]);
    view.take(&surface, &bytes, &[whole()]).expect("taken");
    assert!(view.pixels().iter().any(|&byte| byte != 0));
    let spent = view.take_picture_wiped();
    assert!(spent.pixels().iter().all(|&byte| byte == 0));
}

#[test]
fn a_taken_picture_is_opaque_and_in_red_green_blue_order() {
    let mut view = View::new(spec()).expect("a view");
    let (surface, bytes) = frame([10, 20, 30, 40]);
    view.take(&surface, &bytes, &[whole()]).expect("taken");
    assert!(view
        .pixels()
        .as_chunks::<4>()
        .0
        .iter()
        .all(|dot| *dot == [30, 20, 10, 255]));
    assert!(!view.overlaid(), "the content lacks it");
}

#[test]
fn damage_the_picture_does_not_hold_is_refused_whole() {
    let mut view = View::new(spec()).expect("a view");
    let (surface, bytes) = frame([1, 2, 3, 4]);
    let past = DamageRect {
        x: 3,
        y: 0,
        width_px: 2,
        height_px: 1,
    };
    assert_eq!(
        view.take(&surface, &bytes, &[whole(), past]),
        Err(Errno::LengthOutOfRange)
    );
    assert!(view.pixels().iter().all(|&byte| byte == 0), "nothing taken");
    assert_eq!(
        view.take(&surface, &bytes[..39], &[whole()]),
        Err(Errno::LengthOutOfRange),
        "rows past the frame"
    );
    let other = DisplayMode {
        width_px: 5,
        ..surface
    };
    assert_eq!(
        view.take(&other, &bytes, &[whole()]),
        Err(Errno::OutOfRange)
    );
}

#[test]
fn the_overlay_draws_what_the_content_lacks_and_then_nothing() {
    let mut view = View::new(spec()).expect("a view");
    let (surface, bytes) = frame([0, 0, 200, 0]);
    view.take(&surface, &bytes, &[whole()]).expect("taken");
    let mut content = Surface::new(12, 6).expect("content");
    assert_eq!(view.overlay(&mut content), Some(Rect::new(2, 1, 8, 4)));
    assert!(view.overlaid());
    let red = Pixel {
        r: 200,
        g: 0,
        b: 0,
        a: 255,
    };
    assert_eq!(content.get(2, 1), Some(red), "the placement's corner");
    assert_eq!(content.get(9, 4), Some(red), "and the opposite one");
    assert_eq!(
        content.get(1, 1).map(|dot| dot.a),
        Some(0),
        "nothing outside it"
    );
    assert_eq!(view.overlay(&mut content), None, "nothing new to draw");
    // A change to the picture's second row reaches the placement's lower half.
    let corner = DamageRect {
        x: 0,
        y: 1,
        width_px: 1,
        height_px: 1,
    };
    view.take(&surface, &bytes, &[corner]).expect("taken");
    assert_eq!(view.overlay(&mut content), Some(Rect::new(2, 3, 8, 2)));
    view.invalidate();
    assert_eq!(
        view.overlay(&mut content),
        Some(Rect::new(2, 1, 8, 4)),
        "an overwritten placement is drawn whole"
    );
}

#[test]
fn a_respec_keeps_the_picture_while_its_size_is_kept() {
    let mut view = View::new(spec()).expect("a view");
    let (surface, bytes) = frame([5, 6, 7, 8]);
    view.take(&surface, &bytes, &[whole()]).expect("taken");
    let moved = WindowView {
        placement: DamageRect {
            x: 0,
            y: 0,
            width_px: 4,
            height_px: 2,
        },
        filter: ViewFilter::Smooth,
        ..spec()
    };
    view.respec(moved).expect("respecified");
    assert_eq!(view.spec(), &moved);
    assert!(view
        .pixels()
        .as_chunks::<4>()
        .0
        .iter()
        .all(|dot| *dot == [7, 6, 5, 255]));
    let larger = WindowView {
        width_px: 6,
        ..moved
    };
    view.respec(larger).expect("a larger picture");
    assert_eq!(view.size(), (6, 2));
    assert!(
        view.pixels().iter().all(|&byte| byte == 0),
        "a fresh picture is black"
    );
}
