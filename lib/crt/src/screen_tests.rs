use alloc::vec::Vec;

use tairix_fuzzseed::meter::{refusing_above, refusing_past};
use tairix_parallel::SERIAL;

use super::*;
use crate::frame_interval_ns;

#[test]
fn the_picture_is_the_largest_four_by_three_the_window_holds_centred() {
    let at = |x, y, width, height| {
        Some(Target {
            x,
            y,
            width,
            height,
        })
    };
    assert_eq!(fit(1280, 960), at(0, 0, 1280, 960));
    assert_eq!(
        fit(1920, 1080),
        at(240, 0, 1440, 1080),
        "pillars beside a wide screen"
    );
    assert_eq!(
        fit(800, 900),
        at(0, 150, 800, 600),
        "bands above and below a tall one"
    );
    assert_eq!(fit(1, 1), None, "too small for a picture");
    assert_eq!(fit(0, 600), None);
}

fn levels(width: u32, height: u32, level: impl Fn(u32) -> u8) -> Vec<u8> {
    (0..height)
        .flat_map(|y| {
            let l = level(y);
            (0..width).flat_map(move |_| [l, l, l, 255])
        })
        .collect()
}

const SMALL_TARGET: Target = Target {
    x: 2,
    y: 1,
    width: 16,
    height: 8,
};

#[test]
fn a_draw_redraws_only_the_rows_a_change_reaches_in_a_target_it_drew() {
    let mut screen = Screen::new();
    let mut surface = Surface::new(20, 10).unwrap();
    let target = SMALL_TARGET;
    assert_eq!(screen.draw(&mut surface, target), Ok(0..0), "no frame yet");
    assert!(screen.take(2, 2, &levels(2, 2, |y| [40, 200][y as usize])));
    assert_eq!(
        screen.draw(&mut surface, target),
        Ok(0..8),
        "a new target, whole"
    );
    assert_eq!(surface.get(2, 1).unwrap().r, 40);
    assert_eq!(surface.get(17, 8).unwrap().r, 200);
    assert_eq!(
        surface.get(1, 1).unwrap().a,
        0,
        "outside the target untouched"
    );
    assert!(
        !screen.take(2, 2, &levels(2, 2, |y| [40, 200][y as usize])),
        "the same frame"
    );
    assert_eq!(screen.draw(&mut surface, target), Ok(0..0), "nothing owed");
    assert!(screen.take(2, 2, &levels(2, 2, |y| [40, 90][y as usize])));
    assert_eq!(
        screen.draw(&mut surface, target),
        Ok(4..8),
        "the lower row's block"
    );
    assert_eq!(surface.get(17, 8).unwrap().r, 90);
    assert!(screen.take(2, 2, &levels(2, 2, |y| [41, 90][y as usize])));
    assert!(screen.take(2, 2, &levels(2, 2, |y| [41, 91][y as usize])));
    assert_eq!(
        screen.draw(&mut surface, target),
        Ok(0..8),
        "two frames' changes together"
    );
    let wider = Target {
        width: 18,
        ..target
    };
    assert!(!screen.take(2, 2, &levels(2, 2, |y| [41, 91][y as usize])));
    assert_eq!(
        screen.draw(&mut surface, wider),
        Ok(0..8),
        "a resized target, whole"
    );
    screen.invalidate();
    assert_eq!(
        screen.draw(&mut surface, wider),
        Ok(0..8),
        "and after its surface was lost"
    );
    assert!(screen.take(4, 2, &levels(4, 2, |_| 5)));
    assert_eq!(
        screen.draw(&mut surface, wider),
        Ok(0..8),
        "a frame of a new size, whole"
    );
}

#[test]
fn a_frame_that_is_not_its_size_changes_nothing() {
    let mut screen = Screen::new();
    assert!(!screen.take(4, 6, &[0; 4 * 6 * 4 - 1]));
    assert!(!screen.take(0, 6, &[]));
    assert!(!screen.take(u32::MAX, u32::MAX, &[]));
    assert_eq!(screen.size(), (0, 0), "and kept from the screen");
}

#[test]
fn a_frame_written_by_rows_draws_only_the_rows_written() {
    let mut screen = Screen::new();
    let mut surface = Surface::new(20, 10).unwrap();
    assert!(screen.reshape(2, 2));
    assert!(!screen.reshape(u32::MAX, u32::MAX), "too large to hold");
    assert_eq!(screen.size(), (2, 2));
    assert_eq!(
        screen.draw(&mut surface, SMALL_TARGET),
        Ok(0..8),
        "a new frame, whole"
    );
    assert_eq!(surface.get(2, 1).unwrap().r, 0, "black until written");
    screen
        .row_mut(1)
        .unwrap()
        .copy_from_slice(&[70, 70, 70, 255, 70, 70, 70, 255]);
    assert!(screen.row_mut(2).is_none(), "no third row");
    assert_eq!(screen.draw(&mut surface, SMALL_TARGET), Ok(4..8));
    assert_eq!(surface.get(17, 8).unwrap().r, 70);
    assert_eq!(surface.get(2, 1).unwrap().r, 0, "the first row as it was");
    assert!(screen.reshape(2, 2), "the same size");
    assert_eq!(
        screen.draw(&mut surface, SMALL_TARGET),
        Ok(0..0),
        "kept, and nothing owed"
    );
}

#[test]
fn a_smooth_screen_blends_and_redraws_what_a_change_reaches() {
    let mut screen = Screen::new();
    screen.set_scaling(Scaling::Smooth);
    let target = Target {
        x: 0,
        y: 0,
        width: 8,
        height: 32,
    };
    let mut surface = Surface::new(8, 32).unwrap();
    assert!(screen.take(2, 8, &levels(2, 8, |y| if y < 4 { 0 } else { 240 })));
    assert_eq!(screen.draw(&mut surface, target), Ok(0..32));
    let between = surface.get(3, 16).unwrap().r;
    assert!(
        0 < between && between < 240,
        "blended across the edge: {between}"
    );
    assert_eq!(surface.get(3, 0).unwrap().r, 0);
    assert_eq!(surface.get(3, 31).unwrap().r, 240);
    assert!(screen.take(
        2,
        8,
        &levels(2, 8, |y| match y {
            0..4 => 0,
            7 => 100,
            _ => 240,
        })
    ));
    let rows = screen.draw(&mut surface, target).unwrap();
    assert!(rows.start > 16 && rows.end == 32, "{rows:?}");
    assert!(surface.get(3, 31).unwrap().r < 240, "the change drawn");
    let mut whole = Surface::new(8, 32).unwrap();
    let mut fresh = Screen::new();
    fresh.set_scaling(Scaling::Smooth);
    assert!(fresh.take(2, 8, screen.pixels()));
    fresh.draw(&mut whole, target).unwrap();
    assert_eq!(surface, whole, "as if drawn whole");
}

#[test]
fn changing_the_scaling_draws_whole() {
    let mut screen = Screen::new();
    let mut surface = Surface::new(20, 10).unwrap();
    assert!(screen.take(2, 2, &levels(2, 2, |y| [10, 20][y as usize])));
    screen.draw(&mut surface, SMALL_TARGET).unwrap();
    screen.set_scaling(Scaling::Sharp);
    assert_eq!(
        screen.draw(&mut surface, SMALL_TARGET),
        Ok(0..0),
        "the same scaling"
    );
    screen.set_scaling(Scaling::Smooth);
    assert_eq!(screen.scaling(), Scaling::Smooth);
    assert_eq!(screen.draw(&mut surface, SMALL_TARGET), Ok(0..8));
}

#[test]
fn a_change_reaches_its_neighbours_footprint_and_no_further() {
    assert_eq!(smoothed_rows(0..0, 600, 900), 0..0);
    let reached = smoothed_rows(300..301, 600, 900);
    assert!(reached.start <= 450 && reached.end > 451, "{reached:?}");
    assert!(reached.end - reached.start < 12, "{reached:?}");
    assert_eq!(smoothed_rows(599..600, 600, 900).end, 900);
    let shrunk = smoothed_rows(100..101, 600, 300);
    assert!(shrunk.start <= 50 && shrunk.end > 50, "{shrunk:?}");
}

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
    let stride = usize::try_from(W).unwrap() * DOT;
    for dot in pixels[10 * stride..12 * stride].as_chunks_mut::<DOT>().0 {
        for channel in &mut dot[..DOT - 1] {
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

#[test]
fn a_refused_smooth_draw_is_drawn_whole_next() {
    let mut screen = Screen::new();
    screen.set_scaling(Scaling::Smooth);
    let mut window = window();
    assert!(screen.take(W, H, &frame(0)));
    let refused = refusing_above(SMALL, || screen.draw(&mut window, TARGET));
    assert_eq!(refused, Err(DrawError::Smooth(ResampleError::OutOfMemory)));
    assert!(screen.take(W, H, &changed(frame(0))));
    assert_eq!(
        screen.draw(&mut window, TARGET),
        Ok(0..TARGET.height),
        "whole"
    );
}
