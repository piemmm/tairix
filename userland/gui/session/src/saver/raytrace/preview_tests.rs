//! Host tests of what a reveal shows under way: the finished picture is
//! every pixel's own trace however the steps are split into frames, a pass
//! leaves a smooth bilinear picture over its grid, the first pass grows from
//! black, the cells painted are those marked, and a painter refused its room
//! paints as one given it.

use alloc::vec::Vec;

use tairix_raster::Pixel;
use tairix_raytrace::Reveal;
use tairix_wm::{Point, Region, Surface};

use super::{Preview, BLACK};
use crate::saver::raytrace::engine::Traced;

/// The colour pixel `(x, y)` traces to: different from its neighbours.
fn colour((x, y): (u32, u32)) -> Pixel {
    let mix = |seed: u32| {
        let hashed = (x.wrapping_mul(0x9e37_79b9) ^ y.wrapping_mul(0x85eb_ca6b) ^ seed)
            .wrapping_mul(0x2c1b_3c6d);
        u8::try_from(hashed >> 24).expect("a byte")
    };
    Pixel {
        r: mix(1),
        g: mix(2),
        b: mix(3),
        a: u8::MAX,
    }
}

/// Every step of a `size` picture's reveal under `key`, traced to `colour`.
fn reveal(size: (u32, u32), key: u64) -> Vec<Traced> {
    let order = Reveal::new(size, key).expect("a picture");
    (0..order.count())
        .map(|index| {
            let step = order.step(index).expect("a step");
            Traced {
                step,
                pixel: colour((step.x, step.y)),
            }
        })
        .collect()
}

/// A black picture of `size`.
fn black(size: (u32, u32)) -> Surface {
    let mut surface = Surface::new(size.0, size.1).expect("a surface");
    surface.fill(tairix_wm::Color::rgb(0, 0, 0));
    surface
}

/// Paint `steps` in frames of `frame` steps over `surface`, as the saver
/// does, answering every frame's damage.
fn paint_in_frames(
    preview: &mut Preview,
    surface: &mut Surface,
    steps: &[Traced],
    frame: usize,
    runner: &dyn tairix_parallel::JobRunner,
) -> Vec<Region> {
    let mut damages = Vec::new();
    for chunk in steps.chunks(frame.max(1)) {
        let mut damage = Region::new();
        preview.plan(chunk, &mut damage);
        let before = surface.clone();
        preview.paint(surface, chunk, runner);
        for y in 0..surface.height() {
            for x in 0..surface.width() {
                if surface.get(x, y) != before.get(x, y) {
                    let at = Point::new(
                        i32::try_from(x).expect("small"),
                        i32::try_from(y).expect("small"),
                    );
                    assert!(damage.contains(at), "({x}, {y}) changed unmarked");
                }
            }
        }
        damages.push(damage);
    }
    damages
}

const SIZES: [(u32, u32); 5] = [(1, 1), (9, 5), (48, 27), (61, 34), (130, 70)];

/// However the steps arrive — one a frame, a few, or all at once, across one
/// core or several — the finished picture is every pixel's own trace.
#[test]
fn a_whole_reveal_shows_every_pixel_as_traced_however_it_arrives() {
    let pool = tairix_parallel::Threaded::new(4);
    for size in SIZES {
        let steps = reveal(size, 5);
        for frame in [1, 7, 64, steps.len()] {
            let mut preview = Preview::new(size).expect("a preview");
            let mut surface = black(size);
            let runner: &dyn tairix_parallel::JobRunner = if frame == 64 {
                &pool
            } else {
                &tairix_parallel::SERIAL
            };
            paint_in_frames(&mut preview, &mut surface, &steps, frame, runner);
            for y in 0..size.1 {
                for x in 0..size.0 {
                    assert_eq!(
                        surface.get(x, y),
                        Some(colour((x, y))),
                        "{size:?} in frames of {frame}: ({x}, {y})"
                    );
                }
            }
        }
    }
}

/// Once a pass ends, the picture is the bilinear blend of its grid's traced
/// points: smooth between them, and each traced point its own colour.
#[test]
fn a_finished_pass_leaves_its_grid_blended_smoothly() {
    let size = (61u32, 34u32);
    let steps = reveal(size, 9);
    let mut preview = Preview::new(size).expect("a preview");
    let mut surface = black(size);
    let coarsest = Reveal::coarsest(size);
    let first_pass = steps
        .iter()
        .take_while(|traced| traced.step.side == coarsest)
        .count();
    let second_pass = steps[first_pass..]
        .iter()
        .take_while(|traced| traced.step.side == coarsest / 2)
        .count();
    let through = first_pass + second_pass;
    paint_in_frames(
        &mut preview,
        &mut surface,
        &steps[..through],
        5,
        &tairix_parallel::SERIAL,
    );
    let spacing = coarsest / 2;
    let last = |extent: u32| (extent - 1) / spacing * spacing;
    for y in 0..size.1 {
        for x in 0..size.0 {
            let (x0, y0) = (x / spacing * spacing, y / spacing * spacing);
            let (x1, y1) = (
                (x0 + spacing).min(last(size.0)),
                (y0 + spacing).min(last(size.1)),
            );
            let (fx, fy) = (((x - x0) << 8) / spacing, ((y - y0) << 8) / spacing);
            let corner = |cx, cy| colour((cx, cy));
            let blend = |pick: fn(Pixel) -> u8| {
                let c = |cx, cy| u32::from(pick(corner(cx, cy)));
                let left = c(x0, y0) * (256 - fy) + c(x0, y1) * fy;
                let right = c(x1, y0) * (256 - fy) + c(x1, y1) * fy;
                u8::try_from((left * (256 - fx) + right * fx + (1 << 15)) >> 16).expect("a byte")
            };
            let expected = Pixel {
                r: blend(|p| p.r),
                g: blend(|p| p.g),
                b: blend(|p| p.b),
                a: u8::MAX,
            };
            assert_eq!(surface.get(x, y), Some(expected), "({x}, {y})");
        }
    }
}

/// The first pass grows from black: a lone traced point lights the cells
/// about it, brightest at itself and fading to black at their far corners,
/// and leaves everything further black.
#[test]
fn a_lone_first_point_glows_softly_out_of_black() {
    let size = (64u32, 36u32);
    let coarsest = Reveal::coarsest(size);
    let at = (coarsest, coarsest);
    let white = Pixel {
        r: 255,
        g: 255,
        b: 255,
        a: u8::MAX,
    };
    let lone = [Traced {
        step: tairix_raytrace::Step {
            x: at.0,
            y: at.1,
            side: coarsest,
        },
        pixel: white,
    }];
    let mut preview = Preview::new(size).expect("a preview");
    let mut surface = black(size);
    paint_in_frames(
        &mut preview,
        &mut surface,
        &lone,
        1,
        &tairix_parallel::SERIAL,
    );
    assert_eq!(surface.get(at.0, at.1), Some(white));
    let half = coarsest / 2;
    let between = surface.get(at.0 + half, at.1).expect("a pixel");
    assert!(between.r > 100 && between.r < 160, "{between:?}");
    assert_eq!(surface.get(at.0 + coarsest, at.1), Some(BLACK));
    assert_eq!(surface.get(0, 0), Some(BLACK));
    assert_eq!(
        surface.get(at.0 + coarsest + 1, at.1 + coarsest + 1),
        Some(BLACK)
    );
}

/// A reveal that begins again over black forgets the first pass's points it
/// had traced.
#[test]
fn a_reset_preview_starts_the_first_pass_over_from_black() {
    let size = (48u32, 27u32);
    let steps = reveal(size, 3);
    let coarsest = Reveal::coarsest(size);
    let first = steps
        .iter()
        .take_while(|traced| traced.step.side == coarsest)
        .count();
    let mut preview = Preview::new(size).expect("a preview");
    let mut surface = black(size);
    paint_in_frames(
        &mut preview,
        &mut surface,
        &steps[..first],
        first,
        &tairix_parallel::SERIAL,
    );
    preview.reset();
    let mut fresh = Preview::new(size).expect("a preview");
    let (mut again, mut alone) = (black(size), black(size));
    paint_in_frames(
        &mut preview,
        &mut again,
        &steps[..2],
        2,
        &tairix_parallel::SERIAL,
    );
    paint_in_frames(
        &mut fresh,
        &mut alone,
        &steps[..2],
        2,
        &tairix_parallel::SERIAL,
    );
    assert_eq!(again, alone);
}

/// A frame of many cells marks the box they span rather than each one.
#[test]
fn many_cells_are_marked_as_the_box_they_span() {
    let size = (130u32, 70u32);
    let steps = reveal(size, 1);
    let mut preview = Preview::new(size).expect("a preview");
    let mut surface = black(size);
    let damages = paint_in_frames(
        &mut preview,
        &mut surface,
        &steps,
        steps.len(),
        &tairix_parallel::SERIAL,
    );
    assert_eq!(damages.len(), 1);
    assert_eq!(damages[0].rects().len(), 1);
}

/// A painter refused the room to lay its cells out paints step by step, to
/// the same picture.
#[test]
fn painting_step_by_step_shows_what_laying_the_cells_out_shows() {
    let size = (61u32, 34u32);
    let steps = reveal(size, 4);
    let mut planned = Preview::new(size).expect("a preview");
    let mut surface = black(size);
    let mut one_by_one = Preview::new(size).expect("a preview");
    let mut alone = black(size);
    for chunk in steps.chunks(37) {
        let mut damage = Region::new();
        planned.plan(chunk, &mut damage);
        planned.paint(&mut surface, chunk, &tairix_parallel::SERIAL);
        one_by_one.unplanned = true;
        one_by_one.paint(&mut alone, chunk, &tairix_parallel::SERIAL);
        assert_eq!(surface, alone);
    }
}
