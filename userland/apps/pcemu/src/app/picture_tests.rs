use alloc::vec::Vec;

use tairix_crt::frame_interval_ns;
use tairix_fuzzseed::meter::{refusing_above, refusing_past};
use tairix_parallel::SERIAL;

use super::*;
use crate::link::layout::PIXEL;

const W: u32 = 40;
const H: u32 = 30;

/// The window the picture fills, flush with its corner.
const TARGET: Target = Target {
    x: 0,
    y: 0,
    width: 400,
    height: 300,
};

/// Less than a window-sized buffer, more than anything else a draw asks for.
const SMALL: usize = 64 * 1024;

/// Room for one window-sized buffer more, never two.
const ROOM_FOR_ONE: usize = 600 * 1024;

/// A frame of distinct dots, varied by `seed`.
fn frame(seed: u8) -> Vec<u8> {
    let mut out = Vec::new();
    for y in 0..H {
        for x in 0..W {
            let (x, y) = (u8::try_from(x).unwrap(), u8::try_from(y).unwrap());
            out.extend_from_slice(&[
                x.wrapping_mul(6).wrapping_add(seed),
                y.wrapping_mul(8),
                x.wrapping_add(y).wrapping_mul(3),
                0xFF,
            ]);
        }
    }
    out
}

/// `pixels` with rows 10 and 11 inverted.
fn changed(mut pixels: Vec<u8>) -> Vec<u8> {
    let stride = usize::try_from(W).unwrap() * PIXEL;
    for dot in pixels[10 * stride..12 * stride].as_chunks_mut::<PIXEL>().0 {
        for channel in &mut dot[..PIXEL - 1] {
            *channel = !*channel;
        }
    }
    pixels
}

fn drawing(now_ns: u64) -> Drawing<'static> {
    Drawing {
        percent: 100,
        runner: &SERIAL,
        refresh: RefreshRate::ASSUMED,
        now_ns,
    }
}

fn window_of(width: u32, height: u32) -> Surface {
    Surface::new(width, height).unwrap()
}

fn window() -> Surface {
    window_of(TARGET.width, TARGET.height)
}

fn whole() -> Rect {
    Rect::new(0, 0, TARGET.width, TARGET.height)
}

/// `pixels` drawn whole through `tube` into a window of its own.
fn drawn(tube: Tube, pixels: &[u8]) -> Surface {
    let mut picture = Picture::new(tube);
    let mut window = window();
    assert!(picture.take(W, H, pixels));
    picture.draw(&mut window, TARGET, drawing(0)).unwrap();
    window
}

const SCAN_LINES: Tube = Tube {
    scanlines: 450,
    glow: 0,
    fuzz: 0,
    phosphor: 0,
    wobble: 0,
};

#[test]
fn a_plain_picture_draws_the_frame_whole_then_only_the_rows_it_changed() {
    let mut picture = Picture::new(Tube::default());
    let mut window = window();
    assert!(picture.take(W, H, &frame(0)));
    let first = picture.draw(&mut window, TARGET, drawing(0)).unwrap();
    assert_eq!(first.bounds(), whole());
    assert!(picture
        .draw(&mut window, TARGET, drawing(0))
        .unwrap()
        .is_empty());
    assert!(picture.take(W, H, &changed(frame(0))));
    let rows = picture.draw(&mut window, TARGET, drawing(0)).unwrap();
    assert!(
        !rows.is_empty() && rows.bounds().height < TARGET.height,
        "{:?}",
        rows.bounds()
    );
    assert_eq!(window, drawn(Tube::default(), &changed(frame(0))));
    assert_eq!(picture.due(), None);
}

#[test]
fn the_tube_draws_over_the_picture_and_then_only_what_it_changed() {
    let mut picture = Picture::new(SCAN_LINES);
    let mut window = window();
    picture.take(W, H, &frame(0));
    let first = picture.draw(&mut window, TARGET, drawing(0)).unwrap();
    assert_eq!(first.bounds(), whole());
    assert_ne!(window, drawn(Tube::default(), &frame(0)), "the lines show");
    assert!(picture
        .draw(&mut window, TARGET, drawing(0))
        .unwrap()
        .is_empty());
    picture.take(W, H, &changed(frame(0)));
    let rows = picture.draw(&mut window, TARGET, drawing(0)).unwrap();
    assert!(rows.bounds().height < TARGET.height, "{:?}", rows.bounds());
    assert_eq!(window, drawn(SCAN_LINES, &changed(frame(0))));
    assert_eq!(picture.due(), None, "scan lines stand still");
}

#[test]
fn the_tube_lands_where_the_picture_sits() {
    let (width, height) = (TARGET.width + 100, TARGET.height);
    let target = Target { x: 50, ..TARGET };
    let mut picture = Picture::new(SCAN_LINES);
    let mut window = window_of(width, height);
    picture.take(W, H, &frame(0));
    let first = picture.draw(&mut window, target, drawing(0)).unwrap();
    assert_eq!(
        first.bounds(),
        Rect::new(50, 0, TARGET.width, TARGET.height)
    );
    let (alone, blank) = (drawn(SCAN_LINES, &frame(0)), window_of(width, height));
    for y in 0..height {
        for x in 0..width {
            let expected = if (50..50 + TARGET.width).contains(&x) {
                alone.get(x - 50, y)
            } else {
                blank.get(x, y)
            };
            assert_eq!(window.get(x, y), expected, "({x}, {y})");
        }
    }
}

#[test]
fn turning_the_tube_off_replaces_its_picture_whole() {
    let mut picture = Picture::new(SCAN_LINES);
    let mut window = window();
    picture.take(W, H, &frame(0));
    picture.draw(&mut window, TARGET, drawing(0)).unwrap();
    picture.set_tube(Tube::default());
    let plain = picture.draw(&mut window, TARGET, drawing(0)).unwrap();
    assert_eq!(plain.bounds(), whole());
    assert_eq!(window, drawn(Tube::default(), &frame(0)));
}

#[test]
fn a_refused_tube_leaves_the_plain_picture_whole_and_current() {
    let mut picture = Picture::new(SCAN_LINES);
    let mut window = window();
    picture.take(W, H, &frame(0));
    picture.draw(&mut window, TARGET, drawing(0)).unwrap();
    let glowing = Tube {
        glow: 350,
        ..SCAN_LINES
    };
    picture.set_tube(glowing);
    picture.take(W, H, &changed(frame(0)));
    let refused = refusing_above(SMALL, || picture.draw(&mut window, TARGET, drawing(0))).unwrap();
    assert_eq!(refused.bounds(), whole(), "the tube's picture replaced");
    assert_eq!(
        window,
        drawn(Tube::default(), &changed(frame(0))),
        "by the plain picture of the frame now shown"
    );
    let back = picture.draw(&mut window, TARGET, drawing(0)).unwrap();
    assert_eq!(back.bounds(), whole(), "the tube back, whole");
    assert_eq!(window, drawn(glowing, &changed(frame(0))));
}

#[test]
fn a_tube_refused_again_still_shows_each_new_frame() {
    let mut picture = Picture::new(SCAN_LINES);
    let mut window = window();
    picture.take(W, H, &frame(0));
    picture.draw(&mut window, TARGET, drawing(0)).unwrap();
    let glowing = Tube {
        glow: 350,
        ..SCAN_LINES
    };
    picture.set_tube(glowing);
    for seed in [1, 2, 3] {
        picture.take(W, H, &frame(seed));
        refusing_past(ROOM_FOR_ONE, || {
            picture.draw(&mut window, TARGET, drawing(0))
        })
        .unwrap();
        assert_eq!(window, drawn(Tube::default(), &frame(seed)), "frame {seed}");
    }
}

#[test]
fn a_moving_tube_steps_on_its_frame_deadline() {
    let mut picture = Picture::new(Tube {
        wobble: 250,
        ..Tube::default()
    });
    let mut window = window();
    picture.take(W, H, &frame(0));
    picture.draw(&mut window, TARGET, drawing(1_000)).unwrap();
    let due = picture.due().expect("a wobble moves");
    assert_eq!(due, 1_000 + frame_interval_ns(RefreshRate::ASSUMED));
    assert!(!picture.step(due - 1), "not yet");
    assert!(picture.step(due));
    assert_eq!(picture.due(), None, "until the step is drawn");
    let moved = picture.draw(&mut window, TARGET, drawing(due)).unwrap();
    assert!(!moved.is_empty(), "the rows moved");
    assert!(picture.due().is_some_and(|next| next > due));
    picture.set_tube(SCAN_LINES);
    picture.draw(&mut window, TARGET, drawing(due)).unwrap();
    assert_eq!(picture.due(), None, "scan lines stand still");
    picture.set_tube(Tube {
        wobble: 250,
        ..Tube::default()
    });
    picture.draw(&mut window, TARGET, drawing(due)).unwrap();
    picture.set_tube(Tube::default());
    picture.draw(&mut window, TARGET, drawing(due)).unwrap();
    assert_eq!(picture.due(), None, "a plain picture never steps");
}

#[test]
fn an_invalidated_picture_draws_whole_again() {
    for tube in [Tube::default(), SCAN_LINES] {
        let mut picture = Picture::new(tube);
        let mut window = window();
        picture.take(W, H, &frame(0));
        picture.draw(&mut window, TARGET, drawing(0)).unwrap();
        window.fill(tairix_raster::Color::rgb(0, 0, 0));
        picture.invalidate();
        let again = picture.draw(&mut window, TARGET, drawing(0)).unwrap();
        assert_eq!(again.bounds(), whole(), "{tube:?}");
        assert_eq!(window, drawn(tube, &frame(0)), "{tube:?}");
    }
}
