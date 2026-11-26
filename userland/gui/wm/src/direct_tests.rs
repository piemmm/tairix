//! A promoted window's presents laid straight into the display's own frame,
//! its content owed what they showed and brought up to date before anything
//! composes, captures or pictures it.

use alloc::rc::Rc;
use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::driver::display::{DamageList, DamageRect, DisplayMode};
use tairix_display::{winframe, SharedFrame};

use crate::color::Color;
use crate::geometry::{Point, Rect};
use crate::surface::Surface;
use crate::tests::{
    captured, client_frame, fullscreenable, mode, opaque, whole_tap, FrameBytes, MockDisplay,
};
use crate::{Compositor, Presentation, WindowId, WindowSizeState};

const RED: Color = Color::rgb(255, 0, 0);
const GREEN: Color = Color::rgb(0, 255, 0);
const CYAN: Color = Color::rgb(0, 200, 200);

/// The output, and every frame's shape: the screen's.
fn output() -> DisplayMode {
    mode(24, 16)
}

/// A screen-shaped client frame holding `picture`, and the region it lies in.
fn frame_of(picture: &Surface) -> (SharedFrame, Rc<FrameBytes>) {
    client_frame(picture, &output())
}

/// The whole of a frame.
fn whole() -> DamageList {
    DamageList::new(&[DamageRect::full(&output())]).expect("one rectangle")
}

/// A compositor drawing into a display's own frames, its red window covering
/// the screen and promoted by the frame that first showed it alone.
fn promoted() -> (Compositor, MockDisplay, WindowId) {
    let (mut c, id) = fullscreenable(output());
    c.set_window_size_state(id, WindowSizeState::Fullscreen, c.screen_rect())
        .expect("fullscreen");
    let mut display = MockDisplay::drawn(output());
    c.present(&mut display).expect("presents");
    assert_eq!(
        c.presentation(),
        Some(Presentation::Promoted(id)),
        "a frame showing the window alone"
    );
    (c, display, id)
}

/// The display pixel at `(x, y)` as its bytes.
fn shown_at(display: &MockDisplay, x: u32, y: u32) -> [u8; 4] {
    let at = ((y * output().width_px + x) * 4) as usize;
    let mut pixel = [0; 4];
    pixel.copy_from_slice(&display.shown()[at..at + 4]);
    pixel
}

/// Window `id`'s content pixel at `(x, y)`.
fn content_at(c: &Compositor, id: WindowId, x: u32, y: u32) -> Option<crate::color::Pixel> {
    c.window(id)?.content()?.get(x, y)
}

#[test]
fn a_promoted_windows_frame_is_laid_straight_onto_the_screen_and_owed_to_its_content() {
    let (mut c, mut display, id) = promoted();
    let (frame, _) = frame_of(&opaque(24, 16, CYAN));
    assert!(c.owe_window_content(id, &frame, &output(), &whole()));
    c.present(&mut display).expect("presents");
    assert_eq!(c.presentation(), Some(Presentation::Promoted(id)));
    assert_eq!(shown_at(&display, 0, 0), [0, 200, 200, 255]);
    assert_eq!(shown_at(&display, 23, 15), [0, 200, 200, 255]);
    assert_eq!(
        content_at(&c, id, 5, 5),
        Some(RED.premultiply()),
        "decoded nowhere but the screen"
    );
    let tap = whole_tap(&c, false);
    let picture = captured(&mut c, tap);
    assert_eq!(
        content_at(&c, id, 5, 5),
        Some(CYAN.premultiply()),
        "settled first"
    );
    assert_eq!(
        display.shown(),
        software(&picture),
        "the scene's software composite"
    );
}

/// `picture`, a composite of the whole screen, as the display's bytes.
fn software(picture: &Surface) -> Vec<u8> {
    let shape = output();
    let mut bytes = vec![0; (shape.stride_bytes * shape.height_px) as usize];
    winframe::encode(
        picture,
        &mut bytes,
        &shape,
        DamageRect::full(&shape),
        &tairix_parallel::SERIAL,
    )
    .expect("encoded");
    bytes
}

#[test]
fn what_was_laid_straight_is_composed_from_the_settled_content_once_another_window_shows() {
    let (mut c, mut display, id) = promoted();
    let (frame, _) = frame_of(&opaque(24, 16, CYAN));
    assert!(c.owe_window_content(id, &frame, &output(), &whole()));
    c.present(&mut display).expect("presents");
    let above = c.add_window(Point::new(2, 2), opaque(4, 4, GREEN));
    c.present(&mut display).expect("presents");
    assert_eq!(c.presentation(), Some(Presentation::Composited));
    assert!(c.on_display(above) && c.on_display(id));
    assert_eq!(shown_at(&display, 3, 3), [0, 255, 0, 255]);
    assert_eq!(shown_at(&display, 20, 12), [0, 200, 200, 255]);
    assert_eq!(
        c.back_buffer().get(20, 12),
        Some(CYAN.premultiply()),
        "the back buffer composed what the screen showed"
    );
    assert!(!c.presents_directly(id), "covered in part");
    let tap = whole_tap(&c, false);
    let picture = captured(&mut c, tap);
    assert_eq!(display.shown(), software(&picture));
}

#[test]
fn a_frame_short_of_opaque_ends_the_promotion_and_is_composed_from_the_content() {
    let (mut c, mut display, id) = promoted();
    let mut picture = opaque(24, 16, CYAN);
    picture.fill_rect(10, 6, 2, 2, Color::rgba(0, 0, 0, 0));
    let (frame, _) = frame_of(&picture);
    assert!(c.owe_window_content(id, &frame, &output(), &whole()));
    c.present(&mut display).expect("presents");
    assert_eq!(c.presentation(), Some(Presentation::Composited));
    assert_eq!(
        content_at(&c, id, 10, 6),
        Some(Color::rgba(0, 0, 0, 0).premultiply())
    );
    assert_eq!(shown_at(&display, 0, 0), [0, 200, 200, 255]);
    assert_eq!(
        shown_at(&display, 10, 6),
        [0, 0, 255, 255],
        "the desktop beneath shows through"
    );
    assert!(!c.presents_directly(id));
    let tap = whole_tap(&c, false);
    let picture = captured(&mut c, tap);
    assert_eq!(display.shown(), software(&picture));
}

#[test]
fn a_present_is_owed_only_while_its_window_is_shown_alone_at_its_frames_size() {
    let (mut c, id) = fullscreenable(output());
    let (frame, _) = frame_of(&opaque(24, 16, CYAN));
    assert!(
        !c.owe_window_content(id, &frame, &output(), &whole()),
        "not yet promoted"
    );
    let (mut c, _display, id) = promoted();
    let short = DisplayMode {
        height_px: 17,
        ..output()
    };
    assert!(
        !c.owe_window_content(id, &frame, &short, &whole()),
        "a frame shaped past the bytes it holds"
    );
    c.arm_capture(whole_tap(&c, false)).expect("armed");
    assert!(!c.owe_window_content(id, &frame, &output(), &whole()));
    assert!(c.disarm_capture());
    assert!(c.owe_window_content(id, &frame, &output(), &whole()));
}

#[test]
fn reshaping_or_closing_the_window_settles_its_content_and_lets_its_frame_go() {
    let (mut c, _display, id) = promoted();
    let (frame, region) = frame_of(&opaque(24, 16, CYAN));
    assert!(c.owe_window_content(id, &frame, &output(), &whole()));
    drop(frame);
    assert_eq!(Rc::strong_count(&region), 2, "the compositor holds it");
    assert!(c.resize_window_client(id, 20, 16));
    assert_eq!(Rc::strong_count(&region), 1, "let go with the reshape");
    assert_eq!(content_at(&c, id, 5, 5), Some(CYAN.premultiply()));

    let (mut c, _display, id) = promoted();
    let (frame, region) = frame_of(&opaque(24, 16, CYAN));
    assert!(c.owe_window_content(id, &frame, &output(), &whole()));
    drop(frame);
    assert_eq!(
        c.window_picture(id).and_then(|picture| picture.get(5, 5)),
        Some(CYAN.premultiply()),
        "a picture is of the settled content"
    );
    assert!(c.remove(id));
    assert_eq!(Rc::strong_count(&region), 1, "let go with the window");
}

#[test]
fn a_decoded_present_settles_what_was_owed_before_its_own_damage() {
    let (mut c, _display, id) = promoted();
    let (frame, region) = frame_of(&opaque(24, 16, CYAN));
    assert!(c.owe_window_content(id, &frame, &output(), &whole()));
    drop(frame);
    let decoded = c.present_window_content(id, 24, 16, |content, changed| {
        content.fill_rect(0, 0, 2, 2, GREEN);
        changed.add(Rect::new(0, 0, 2, 2));
    });
    assert!(decoded.is_some());
    assert_eq!(content_at(&c, id, 0, 0), Some(GREEN.premultiply()));
    assert_eq!(content_at(&c, id, 5, 5), Some(CYAN.premultiply()));
    assert_eq!(Rc::strong_count(&region), 1, "nothing is owed of it now");
}
