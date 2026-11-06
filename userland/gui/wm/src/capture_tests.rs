//! A capture is the scene's software composite at full strength and desktop
//! resolution, the windows its tap leaves out absent and those it blanks
//! black, and composing one leaves the live frame exactly as it was.

use alloc::vec;

use super::{CaptureError, CaptureTap};
use crate::color::Color;
use crate::corner::Corners;
use crate::geometry::{Point, Rect, Region};
use crate::surface::Surface;
use crate::tests::{
    captured, decorated, mode, new_compositor, opaque, solid_cursor, veiled, whole_tap, MockDisplay,
};
use crate::{Compositor, WindowFrame, WindowId};

const BLUE: Color = Color::rgb(0, 0, 255);
const RED: Color = Color::rgb(255, 0, 0);
const GREEN: Color = Color::rgb(0, 255, 0);
const WHITE: Color = Color::rgb(250, 250, 250);
const BLACK: Color = Color::rgb(0, 0, 0);
/// What the capture's own chrome is painted in, which no capture may show.
const LEFT_OUT: Color = Color::rgb(201, 3, 7);
/// What a sheet a capture blanks is painted in, which no capture may show.
const SECRET: Color = Color::rgb(9, 202, 5);

/// Every kind of layer on a 128×96 desktop: the desktop layer, an opaque
/// window, a decorated one casting its shadow, a rounded translucent one, and
/// frosted glass over the first. Answers the four windows, back to front.
fn busy(c: &mut Compositor) -> [WindowId; 4] {
    let mut desktop = opaque(128, 96, GREEN);
    desktop.fill_rect(8, 8, 20, 12, RED);
    c.set_desktop(desktop);
    let under = c.add_window(Point::new(4, 30), opaque(40, 24, RED));
    let framed = c.add_window(Point::new(56, 24), opaque(40, 24, Color::rgb(200, 180, 40)));
    assert!(c.set_window_frame(framed, WindowFrame::new(decorated())));
    let veil = c.add_window(Point::new(80, 6), opaque(30, 20, BLUE));
    assert!(c.set_opacity(veil, 150));
    assert!(c.set_corners(veil, Corners::Rounded { radius: 6 }));
    let pane = c.add_window(Point::new(16, 40), veiled(50, 24, 0xD0));
    assert!(c.set_backdrop_blur(pane, 3));
    [under, framed, veil, pane]
}

/// A capture of a composited desktop is its back buffer, the pointer drawn
/// where the tap shows it and nowhere else.
#[test]
fn a_capture_of_a_composited_desktop_is_its_back_buffer() {
    let mut c = new_compositor(mode(128, 96), BLUE).expect("compositor");
    busy(&mut c);
    c.composite();
    let unpointed = c.back_buffer().clone();
    c.set_cursor(solid_cursor(6, WHITE), Point::new(40, 30));
    c.composite();
    let tap = whole_tap(&c, true);
    assert_eq!(captured(&mut c, tap).pixels(), c.back_buffer().pixels());
    let tap = whole_tap(&c, false);
    assert_eq!(captured(&mut c, tap).pixels(), unpointed.pixels());
}

/// A capture is at full strength whatever the screen reveal, and of its area
/// alone, glass crossing the area's edge blurring what lies past it.
#[test]
fn a_capture_is_at_full_strength_and_of_its_area_alone() {
    let mut c = new_compositor(mode(128, 96), BLUE).expect("compositor");
    busy(&mut c);
    assert!(c.set_reveal(40));
    c.composite();
    let area = Rect::new(13, 9, 50, 41);
    let picture = captured(
        &mut c,
        CaptureTap {
            area,
            ..CaptureTap::default()
        },
    );
    for y in 0..area.height {
        for x in 0..area.width {
            assert_eq!(
                picture.get(x, y),
                c.back_buffer().get(x + 13, y + 9),
                "({x}, {y})"
            );
        }
    }
}

/// A capture leaves out the windows its tap excludes, and their transients,
/// as though they were not there — glass over them blurs what lies beneath
/// them instead — and shows those it blanks, and theirs, as opaque black.
#[test]
fn a_capture_leaves_out_what_it_excludes_and_blanks_what_it_blanks() {
    let output = mode(32, 24);
    let mut c = new_compositor(output, BLUE).expect("compositor");
    c.add_window(Point::ORIGIN, opaque(30, 20, GREEN));
    let chrome = c.add_window(Point::new(4, 4), opaque(10, 10, LEFT_OUT));
    c.add_transient_window_in_place(chrome, Point::new(26, 14), opaque(4, 4, LEFT_OUT))
        .expect("a transient of the chrome");
    let sheet = c.add_window(Point::new(14, 6), opaque(12, 8, SECRET));
    c.add_transient_window_in_place(sheet, Point::new(28, 2), opaque(3, 3, SECRET))
        .expect("a transient of the sheet");
    let pane = c.add_window(Point::new(2, 2), veiled(20, 10, 0xD0));
    assert!(c.set_backdrop_blur(pane, 2));
    c.add_window(Point::new(20, 10), opaque(6, 6, RED));
    let tap = CaptureTap {
        area: c.screen_rect(),
        only: None,
        exclude: vec![chrome],
        blank: vec![sheet],
        pointer: false,
    };
    let picture = captured(&mut c, tap);

    let mut absent = new_compositor(output, BLUE).expect("compositor");
    absent.add_window(Point::ORIGIN, opaque(30, 20, GREEN));
    absent.add_window(Point::new(14, 6), opaque(12, 8, BLACK));
    absent.add_window(Point::new(28, 2), opaque(3, 3, BLACK));
    let twin_pane = absent.add_window(Point::new(2, 2), veiled(20, 10, 0xD0));
    assert!(absent.set_backdrop_blur(twin_pane, 2));
    absent.add_window(Point::new(20, 10), opaque(6, 6, RED));
    absent.composite();
    assert_eq!(picture.pixels(), absent.back_buffer().pixels());
    let (left_out, secret) = (LEFT_OUT.premultiply(), SECRET.premultiply());
    assert!(
        picture
            .pixels()
            .iter()
            .all(|pixel| *pixel != left_out && *pixel != secret),
        "no pixel of a window left out or blanked is shown"
    );
    assert_eq!(
        picture.get(23, 7),
        Some(BLACK.premultiply()),
        "the sheet is blanked"
    );
}

/// A tap is owed what changes inside its area, through the rectangle and the
/// region funnels alike, and nothing outside it.
#[test]
fn a_tap_is_owed_what_changes_inside_its_area() {
    let mut c = new_compositor(mode(32, 24), BLUE).expect("compositor");
    let moved = c.add_window(Point::new(2, 2), opaque(6, 6, RED));
    let painted = c.add_window(Point::new(10, 6), opaque(10, 6, GREEN));
    let area = Rect::new(0, 0, 16, 12);
    c.arm_capture(CaptureTap {
        area,
        ..CaptureTap::default()
    })
    .expect("armed");
    let mut changed = Region::new();
    assert!(c.take_capture_changes(&mut changed));
    assert_eq!(
        changed,
        Region::from(area),
        "a tap armed is owed its whole area"
    );

    assert!(c.move_window(moved, Point::new(12, 9)));
    let mut strokes = Region::new();
    strokes.add(Rect::new(0, 0, 2, 2));
    strokes.add(Rect::new(8, 4, 2, 2));
    assert!(
        c.repaint_window(painted, (10, 6), &strokes, |surface, rects| {
            for rect in rects {
                surface.fill_rect(
                    rect.left().cast_unsigned(),
                    rect.top().cast_unsigned(),
                    rect.width,
                    rect.height,
                    WHITE,
                );
            }
        })
    );
    assert!(c.take_capture_changes(&mut changed));
    let mut owed = Region::new();
    owed.add(Rect::new(2, 2, 6, 6));
    owed.add(Rect::new(12, 9, 4, 3));
    owed.add(Rect::new(10, 6, 2, 2));
    assert_eq!(changed, owed);
}

/// Two compositors given the same scene and the same changes, one of them
/// composing captures between: what the display is shown and owed, the
/// counters, the frosts and the caches all come out the same.
#[test]
fn composing_a_capture_leaves_the_live_frame_as_it_was() {
    let output = mode(128, 96);
    let mut tapped = new_compositor(output, BLUE).expect("compositor");
    let mut plain = new_compositor(output, BLUE).expect("compositor");
    let ids = busy(&mut tapped);
    assert_eq!(busy(&mut plain), ids);
    let [under, _, veil, _] = ids;
    let mut refusing = MockDisplay::new(output);
    refusing.fail = true;
    for c in [&mut tapped, &mut plain] {
        c.set_cursor(solid_cursor(6, WHITE), Point::new(20, 30));
        c.present(&mut MockDisplay::new(output))
            .expect("the first frame");
        assert!(c.set_opacity(veil, 120));
        assert!(
            c.present(&mut refusing).is_err(),
            "the display refused the frame"
        );
    }
    let live = |c: &Compositor| (c.frame_stats(), c.frame_totals(), c.has_damage());
    let owed = live(&tapped);
    let tap = whole_tap(&tapped, true);
    captured(&mut tapped, tap);
    assert_eq!(
        live(&tapped),
        owed,
        "a capture leaves the refused frame owed"
    );
    for c in [&mut tapped, &mut plain] {
        assert!(c.move_window(under, Point::new(6, 32)));
        assert!(c.move_cursor(Point::new(60, 44)));
    }
    let pending = live(&tapped);
    let mut picture = Surface::new(128, 96).expect("a picture");
    let all = Region::from(tapped.screen_rect());
    tapped
        .compose_capture(&mut picture, &all)
        .expect("composed");
    assert_eq!(live(&tapped), pending, "a capture is no frame");
    tapped
        .present(&mut MockDisplay::new(output))
        .expect("the owed frame");
    plain
        .present(&mut MockDisplay::new(output))
        .expect("the owed frame");
    assert_eq!(tapped.frame(), plain.frame());
    assert_eq!(live(&tapped), live(&plain));
    assert!(
        plain.frame_stats().blur_px > 0,
        "the frame blurs what changed beneath the glass"
    );
    let caches = |c: &Compositor| {
        let (chrome, frost) = (c.chrome_cache_stats(), c.frost_cache_stats());
        (chrome.hits(), chrome.misses(), frost.hits(), frost.misses())
    };
    assert_eq!(
        caches(&tapped),
        caches(&plain),
        "a capture fills and asks no cache"
    );
}

/// A tap lies wholly on the desktop, its surface is its area's size, and it
/// lasts until it is disarmed or the desktop changes size.
#[test]
fn a_tap_lies_on_the_desktop_and_lasts_while_its_size_does() {
    let mut c = new_compositor(mode(32, 24), BLUE).expect("compositor");
    let tap = |area| CaptureTap {
        area,
        ..CaptureTap::default()
    };
    assert_eq!(
        c.arm_capture(tap(Rect::new(20, 10, 16, 8))),
        Err(CaptureError::Area)
    );
    assert_eq!(c.arm_capture(tap(Rect::EMPTY)), Err(CaptureError::Area));
    let all = Region::from(c.screen_rect());
    let mut picture = Surface::new(32, 24).expect("a picture");
    assert_eq!(
        c.compose_capture(&mut picture, &all),
        Err(CaptureError::Disarmed)
    );
    c.arm_capture(tap(c.screen_rect())).expect("armed");
    let mut short = Surface::new(32, 23).expect("a picture");
    assert_eq!(
        c.compose_capture(&mut short, &all),
        Err(CaptureError::Surface)
    );
    let mut changed = Region::new();
    assert!(c.take_capture_changes(&mut changed));
    assert_eq!(changed, all, "a tap armed is owed its whole area");
    assert!(c.set_desktop_size(16, 12));
    assert!(
        !c.take_capture_changes(&mut changed),
        "a desktop of another size disarms it"
    );
    assert!(changed.is_empty());
    assert_eq!(
        c.compose_capture(&mut picture, &all),
        Err(CaptureError::Disarmed)
    );
    c.arm_capture(tap(c.screen_rect()))
        .expect("armed on the new desktop");
    assert!(c.disarm_capture());
    assert!(!c.disarm_capture(), "nothing is left to disarm");
}

/// A desktop drawn smaller than its output is captured at its own size, as it
/// is composed, never as the output it is scaled onto.
#[test]
fn a_desktop_drawn_smaller_than_its_output_is_captured_at_its_own_size() {
    let output = mode(64, 48);
    let mut c = new_compositor(output, BLUE).expect("compositor");
    assert!(c.set_desktop_size(32, 24));
    c.add_window(Point::new(4, 4), opaque(10, 10, RED));
    c.present(&mut MockDisplay::new(output)).expect("presents");
    let tap = whole_tap(&c, false);
    let picture = captured(&mut c, tap);
    assert_eq!((picture.width(), picture.height()), (32, 24));
    assert_eq!(picture.pixels(), c.back_buffer().pixels());
}

/// A capture of one window shows it and the transients it owns, every other
/// window left out — under it or over it — and one it blanks still black.
#[test]
fn a_capture_of_one_window_shows_it_and_its_transients_alone() {
    let output = mode(32, 24);
    let mut c = new_compositor(output, BLUE).expect("compositor");
    c.add_window(Point::ORIGIN, opaque(30, 20, RED));
    let picked = c.add_window(Point::new(4, 4), opaque(12, 10, GREEN));
    c.add_transient_window_in_place(picked, Point::new(18, 2), opaque(4, 4, WHITE))
        .expect("a transient of the window");
    c.add_window(Point::new(8, 8), opaque(10, 10, LEFT_OUT));
    let sheet = c.add_window(Point::new(24, 14), opaque(6, 6, SECRET));
    let tap = CaptureTap {
        area: c.screen_rect(),
        only: Some(picked),
        exclude: vec![],
        blank: vec![sheet],
        pointer: false,
    };
    let picture = captured(&mut c, tap);

    let mut alone = new_compositor(output, BLUE).expect("compositor");
    alone.add_window(Point::new(4, 4), opaque(12, 10, GREEN));
    alone.add_window(Point::new(18, 2), opaque(4, 4, WHITE));
    alone.add_window(Point::new(24, 14), opaque(6, 6, BLACK));
    alone.composite();
    assert_eq!(picture.pixels(), alone.back_buffer().pixels());
}
