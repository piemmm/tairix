//! Host tests of what a reveal shows under way: every pixel its own trace at
//! the end however the steps arrive, a frame changing only what it marks and
//! marking little, a smooth blur where bilinear creased, detail settling in as
//! its neighbours are traced, passes that do not jump, and a buffer let go
//! painted afresh from what was traced.

use alloc::vec::Vec;

use tairix_parallel::{JobRunner, Reversed, Threaded, SERIAL};
use tairix_raster::Pixel;
use tairix_raytrace::{Reveal, Step};
use tairix_wm::{Point, Rect, Region, Surface};

use super::{Cells, Preview};
use crate::saver::raytrace::engine::Traced;
use crate::saver::raytrace::tiles::{COVER_BUDGET, TILE};

const WHITE: Pixel = Pixel {
    r: 255,
    g: 255,
    b: 255,
    a: u8::MAX,
};

const BLACK: Pixel = Pixel {
    r: 0,
    g: 0,
    b: 0,
    a: u8::MAX,
};

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

/// A step at `(x, y)` of the pass of spacing `side`, traced to `pixel`.
fn traced(x: u32, y: u32, side: u32, pixel: Pixel) -> Traced {
    Traced {
        step: Step { x, y, side },
        pixel,
    }
}

/// A black picture of `size`.
fn black(size: (u32, u32)) -> Surface {
    let mut surface = Surface::new(size.0, size.1).expect("a surface");
    surface.fill(tairix_wm::Color::rgb(0, 0, 0));
    surface
}

/// Paint `steps` in frames of `frame` steps over `surface`, as the saver
/// does, checking every pixel a frame changes lies in the damage it marked
/// and in the tiles it reports changed.
fn paint_in_frames(
    preview: &mut Preview,
    surface: &mut Surface,
    steps: &[Traced],
    frame: usize,
    runner: &dyn JobRunner,
) -> Vec<Region> {
    let mut damages = Vec::new();
    for chunk in steps.chunks(frame.max(1)) {
        let mut damage = Region::new();
        preview.take(chunk, true, &mut damage);
        let changed = preview.changed();
        let tiles: Vec<_> = changed.spans(0..changed.rows()).collect();
        let before = surface.clone();
        preview.paint(surface, runner);
        for y in 0..surface.height() {
            for x in 0..surface.width() {
                if surface.get(x, y) != before.get(x, y) {
                    let at = Point::new(
                        i32::try_from(x).expect("small"),
                        i32::try_from(y).expect("small"),
                    );
                    assert!(damage.contains(at), "({x}, {y}) changed unmarked");
                    assert!(
                        tiles
                            .iter()
                            .any(|(columns, lines)| columns.contains(&x) && lines.contains(&y)),
                        "({x}, {y}) changed outside its tiles"
                    );
                }
            }
        }
        damages.push(damage);
    }
    damages
}

/// The most two pictures' levels differ by.
fn farthest(one: &Surface, other: &Surface) -> u8 {
    one.pixels()
        .iter()
        .zip(other.pixels())
        .map(|(a, b)| {
            a.r.abs_diff(b.r)
                .max(a.g.abs_diff(b.g))
                .max(a.b.abs_diff(b.b))
        })
        .max()
        .unwrap_or(0)
}

/// How many of the steps of `steps` lie in passes of spacing `side` or
/// coarser.
fn through(steps: &[Traced], side: u32) -> usize {
    steps
        .iter()
        .take_while(|traced| traced.step.side >= side)
        .count()
}

const SIZES: [(u32, u32); 5] = [(1, 1), (9, 5), (48, 27), (61, 34), (130, 70)];

/// However the steps arrive — one a frame, a few, or all at once, across one
/// core or several — the finished picture is every pixel's own trace.
#[test]
fn a_whole_reveal_shows_every_pixel_as_traced_however_it_arrives() {
    let pool = Threaded::new(4);
    for size in SIZES {
        let steps = reveal(size, 5);
        for frame in [1, 7, 64, steps.len()] {
            let mut preview = Preview::new(size).expect("a preview");
            let mut surface = black(size);
            let runner: &dyn JobRunner = if frame == 64 { &pool } else { &SERIAL };
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

/// The picture under way does not depend on how its rows are split across
/// cores, nor on which piece runs first.
#[test]
fn painting_across_cores_paints_what_one_core_paints() {
    let size = (130u32, 70u32);
    let steps = reveal(size, 8);
    let pool = Threaded::new(4);
    let backwards = Reversed::new(4);
    let mut pictures = [black(size), black(size), black(size)];
    let mut previews = [0, 1, 2].map(|_| Preview::new(size).expect("a preview"));
    let runners: [&dyn JobRunner; 3] = [&SERIAL, &pool, &backwards];
    for chunk in steps.chunks(97) {
        for ((preview, picture), runner) in previews.iter_mut().zip(&mut pictures).zip(runners) {
            let mut damage = Region::new();
            preview.take(chunk, true, &mut damage);
            preview.paint(picture, runner);
        }
        assert_eq!(pictures[0], pictures[1]);
        assert_eq!(pictures[0], pictures[2]);
    }
}

/// A frame of many steps lists no more rectangles than its budget, each of
/// whole tiles, however scattered the steps.
#[test]
fn a_frame_marks_a_bounded_cover_of_whole_tiles() {
    let size = (320u32, 180u32);
    let steps = reveal(size, 2);
    let mut preview = Preview::new(size).expect("a preview");
    let mut surface = black(size);
    for frame in [1, 50, 400, 5000] {
        for chunk in steps.chunks(frame) {
            let mut damage = Region::new();
            preview.take(chunk, true, &mut damage);
            preview.paint(&mut surface, &SERIAL);
            assert!(damage.rects().len() <= COVER_BUDGET);
            for rect in damage.rects() {
                let tiled = |edge: i32, end: u32| {
                    let edge = u32::try_from(edge).expect("on the picture");
                    edge % TILE == 0 || edge == end
                };
                assert!(
                    tiled(rect.left(), size.0) && tiled(rect.right(), size.0),
                    "{rect:?}"
                );
                assert!(
                    tiled(rect.top(), size.1) && tiled(rect.bottom(), size.1),
                    "{rect:?}"
                );
            }
        }
        preview.reset();
        surface = black(size);
    }
}

/// A frame of a hundred of the last pass's scattered steps on a desktop
/// screen marks a small part of it, not the box they span.
#[test]
fn a_scattered_frame_marks_little_of_the_screen() {
    let size = (1920u32, 1080u32);
    let steps = reveal(size, 6);
    let last = through(&steps, 2);
    let mut preview = Preview::new(size).expect("a preview");
    let mut surface = black(size);
    let mut damage = Region::new();
    preview.take(&steps[..last], true, &mut damage);
    preview.paint(&mut surface, &SERIAL);
    let frame = &steps[last..last + 100];
    let span = frame.iter().fold(Rect::EMPTY, |span, traced| {
        let at = Rect::new(
            i32::try_from(traced.step.x).expect("small"),
            i32::try_from(traced.step.y).expect("small"),
            1,
            1,
        );
        span.union(&at)
    });
    let damage = &paint_in_frames(&mut preview, &mut surface, frame, frame.len(), &SERIAL)[0];
    let marked: u64 = damage
        .rects()
        .iter()
        .map(|rect| u64::from(rect.width) * u64::from(rect.height))
        .sum();
    let screen = u64::from(size.0) * u64::from(size.1);
    let spanned = u64::from(span.width) * u64::from(span.height);
    assert!(spanned * 2 > screen, "the steps span the screen");
    assert!(marked * 8 < screen, "{marked} of {screen} pixels marked");
}

/// A lone lit point of a finished pass blurs into a smooth bump: brightest
/// at itself, falling away with no crease at the grid lines either side,
/// where a bilinear blend made a peak with a cross through it.
#[test]
fn a_lone_point_blurs_without_a_crease() {
    let size = (64u32, 64u32);
    let spacing = Reveal::coarsest(size);
    let centre = 32;
    let first: Vec<Traced> = (0..size.1 / spacing)
        .flat_map(|j| (0..size.0 / spacing).map(move |i| (i * spacing, j * spacing)))
        .map(|(x, y)| {
            let pixel = if (x, y) == (centre, centre) {
                WHITE
            } else {
                BLACK
            };
            traced(x, y, spacing, pixel)
        })
        .collect();
    let mut preview = Preview::new(size).expect("a preview");
    let mut surface = black(size);
    paint_in_frames(&mut preview, &mut surface, &first, first.len(), &SERIAL);
    let level = |x: u32| i32::from(surface.get(x, centre).expect("a pixel").r);
    // The B-spline's own weight at its control, two thirds along each axis.
    assert!((level(centre) - 113).abs() <= 1, "{}", level(centre));
    // Its second difference is at most four thirds of white over a spacing
    // squared, and dithered rounding adds at most four; a bilinear blend
    // bends by at least white over a spacing at every grid line.
    for x in centre - 2 * spacing + 1..centre + 2 * spacing {
        let bend = level(x - 1) - 2 * level(x) + level(x + 1);
        assert!(bend.abs() <= 10, "bent by {bend} at {x}");
        let mirrored = level(2 * centre - x);
        assert!((level(x) - mirrored).abs() <= 1, "lopsided at {x}");
    }
    assert_eq!(level(centre + 2 * spacing), 0);
    assert_eq!(level(centre - 2 * spacing), 0);
}

/// A point the current pass traces shows only its share of its own detail
/// until the points about it are traced too.
#[test]
fn a_new_point_settles_in_as_its_neighbours_are_traced() {
    let size = (64u32, 64u32);
    let coarse = Reveal::coarsest(size);
    let spacing = coarse / 2;
    let first: Vec<Traced> = (0..size.1 / coarse)
        .flat_map(|j| {
            (0..size.0 / coarse).map(move |i| traced(i * coarse, j * coarse, coarse, BLACK))
        })
        .collect();
    let at = (36, 32);
    let mut preview = Preview::new(size).expect("a preview");
    let mut surface = black(size);
    paint_in_frames(&mut preview, &mut surface, &first, first.len(), &SERIAL);
    let lone = [traced(at.0, at.1, spacing, WHITE)];
    paint_in_frames(&mut preview, &mut surface, &lone, 1, &SERIAL);
    let alone = surface.get(at.0, at.1).expect("a pixel").r;
    // One of the seven points about it is traced: a seventh of its detail, at
    // the B-spline's weight of four ninths.
    assert!(alone.abs_diff(16) <= 1, "{alone}");
    // The pass's six other points about it lie in the rows above and below:
    // those beside it in its own row are the coarser pass's.
    let beside: Vec<Traced> = [at.1 - spacing, at.1 + spacing]
        .into_iter()
        .flat_map(|y| [at.0 - spacing, at.0, at.0 + spacing].map(|x| traced(x, y, spacing, BLACK)))
        .collect();
    paint_in_frames(&mut preview, &mut surface, &beside, 1, &SERIAL);
    let settled = surface.get(at.0, at.1).expect("a pixel").r;
    assert!(settled.abs_diff(113) <= 1, "{settled}");
}

/// The picture a pass ends on is the one the next begins from: beginning it
/// changes nothing but rounding.
#[test]
fn a_pass_begins_where_the_last_ended() {
    for size in [(61u32, 34u32), (130, 70), (97, 61)] {
        let steps = reveal(size, 4);
        let mut side = Reveal::coarsest(size);
        while side > 1 {
            let end = through(&steps, side);
            let mut preview = Preview::new(size).expect("a preview");
            let mut ended = black(size);
            paint_in_frames(&mut preview, &mut ended, &steps[..end], end, &SERIAL);
            preview.refine();
            let mut begun = black(size);
            let mut damage = Region::new();
            preview.take(&[], false, &mut damage);
            preview.paint(&mut begun, &SERIAL);
            assert!(
                farthest(&ended, &begun) <= 1,
                "{size:?} after the pass of {side}"
            );
            side /= 2;
        }
    }
}

/// A buffer the compositor let go is painted afresh from what was traced,
/// to the picture painted frame by frame.
#[test]
fn a_buffer_let_go_is_painted_afresh_from_what_was_traced() {
    let size = (130u32, 70u32);
    let steps = reveal(size, 3);
    for upto in [40, through(&steps, 4) + 100, steps.len() - 500] {
        let mut kept = Preview::new(size).expect("a preview");
        let mut shown = black(size);
        paint_in_frames(&mut kept, &mut shown, &steps[..=upto], 37, &SERIAL);
        let mut lost = Preview::new(size).expect("a preview");
        let mut gone = black(size);
        paint_in_frames(&mut lost, &mut gone, &steps[..upto], 37, &SERIAL);
        // A fresh buffer holds nothing, so every pixel must be laid.
        let mut fresh = Surface::new(size.0, size.1).expect("a surface");
        let mut damage = Region::new();
        lost.take(&steps[upto..=upto], false, &mut damage);
        lost.paint(&mut fresh, &SERIAL);
        assert_eq!(damage.rects(), [Rect::new(0, 0, size.0, size.1)]);
        assert!(fresh.pixels().iter().all(|pixel| pixel.a == u8::MAX));
        assert!(farthest(&fresh, &shown) <= 1, "after {upto} steps");
    }
}

/// Steps the reveal could not take from where it stands — off the picture,
/// off their grid, a coarser pass's point, or a pass already over — leave
/// the picture as it was.
#[test]
fn steps_the_reveal_could_not_take_change_nothing() {
    let size = (64u32, 64u32);
    let steps = reveal(size, 9);
    let upto = through(&steps, 4) + 20;
    let mut preview = Preview::new(size).expect("a preview");
    let mut surface = black(size);
    paint_in_frames(&mut preview, &mut surface, &steps[..upto], upto, &SERIAL);
    let before = surface.clone();
    let coarsest = Reveal::coarsest(size);
    let stray = [
        traced(64, 0, 2, WHITE),
        traced(0, 64, 2, WHITE),
        traced(3, 2, 2, WHITE),
        traced(4, 4, 2, WHITE),
        traced(2, 2, 3, WHITE),
        traced(0, 0, 0, WHITE),
        traced(coarsest, 0, coarsest, WHITE),
    ];
    paint_in_frames(&mut preview, &mut surface, &stray, stray.len(), &SERIAL);
    assert_eq!(surface, before);
}

/// A painter reset begins its next reveal over black, as a new one would.
#[test]
fn a_reset_painter_starts_over_from_black() {
    let size = (48u32, 27u32);
    let steps = reveal(size, 3);
    let mut preview = Preview::new(size).expect("a preview");
    let mut surface = black(size);
    paint_in_frames(&mut preview, &mut surface, &steps[..300], 300, &SERIAL);
    preview.reset();
    let mut fresh = Preview::new(size).expect("a preview");
    let (mut again, mut alone) = (black(size), black(size));
    paint_in_frames(&mut preview, &mut again, &steps[..2], 2, &SERIAL);
    paint_in_frames(&mut fresh, &mut alone, &steps[..2], 2, &SERIAL);
    assert_eq!(again, alone);
}

/// A painter is refused a picture with no pixels, or more than a surface
/// holds, which it could never paint.
#[test]
fn a_painter_needs_a_picture_a_surface_can_hold() {
    assert!(Preview::new((0, 5)).is_none());
    assert!(Preview::new((5, 0)).is_none());
    assert!(Preview::new((8192, 8193)).is_none());
    assert!(Surface::new(8192, 8193).is_none());
    assert!(Preview::new((1 << 16, 1 << 16)).is_none());
}

/// The cells of a set come back as the runs they were added in, across the
/// words that hold them.
#[test]
fn cells_come_back_as_their_runs() {
    let mut cells = Cells::new((300, 4)).expect("cells");
    cells.add(0..=63, 1..=1);
    cells.add(64..=70, 1..=2);
    cells.add(130..=200, 1..=1);
    cells.add(299..=299, 3..=3);
    assert_eq!(
        cells.runs(1, 299).collect::<Vec<_>>(),
        [(0, 70), (130, 200)]
    );
    assert_eq!(cells.runs(2, 299).collect::<Vec<_>>(), [(64, 70)]);
    assert_eq!(cells.runs(3, 299).collect::<Vec<_>>(), [(299, 299)]);
    assert_eq!(cells.runs(3, 298).count(), 0);
    assert_eq!(cells.runs(0, 299).count(), 0);
    assert_eq!(cells.count(), 71 + 7 + 71 + 1);
    assert_eq!(cells.rows, 1..4);
    cells.clear();
    assert_eq!(cells.count(), 0);
    assert!(cells.runs(1, 299).next().is_none());
}
