//! Unit tests for the half-resolution blur.
//!
//! The blur is held to its definition ([`halved_reference`]) band for band,
//! wherever its field lies and however its work is divided; it reads nothing,
//! and a change to its backdrop changes nothing, past [`halved_reach`]; a frost
//! mixed back is its field mixed back; and a reserved scratch runs it without
//! growing.

extern crate std;

use core::ops::Range;
use std::sync::Mutex;

use alloc::vec;
use alloc::vec::Vec;

use tairix_fuzzseed::Prng;
use tairix_parallel::{JobRunner, Reversed, SERIAL};

use super::{halved_reach, halved_reference};
use crate::blur::{BlurScratch, Blurring};
use crate::color::Pixel;
use crate::round::round_rect_coverage;
use crate::surface::Surface;

/// What a field holds where no blur was written.
const MARKER: Pixel = Pixel {
    r: 1,
    g: 2,
    b: 3,
    a: 4,
};

/// A `width` × `height` premultiplied backdrop of noise over a gradient, so a
/// blur that reads or writes the wrong pixel cannot hide behind a flat field.
fn backdrop(rng: &mut Prng, width: u32, height: u32) -> Surface {
    let mut surface = Surface::new(width, height).expect("a backdrop allocates");
    for y in 0..height {
        for x in 0..width {
            let [noise, r, g, alpha] = rng.next_u32().to_le_bytes();
            let a = alpha.max(u8::try_from((x * 7 + y * 3) % 256).unwrap_or(0));
            let ramp = u8::try_from((x * 5 + y * 11) % 256).unwrap_or(0);
            surface.set(
                x,
                y,
                Pixel {
                    r: r.min(a),
                    g: g.min(a),
                    b: ramp.wrapping_add(noise / 8).min(a),
                    a,
                },
            );
        }
    }
    surface
}

fn blurring(
    rect: (u32, u32, u32, u32),
    bands: &[(Range<u32>, Range<u32>)],
    radius: u32,
) -> Blurring<'_> {
    Blurring {
        rect,
        bands,
        radius,
    }
}

/// A `w` × `h` field whose top-left lies at `at` of `plane`, holding the
/// half-resolution blur `blurring` describes and the marker everywhere else.
fn field_halved(
    plane: &Surface,
    at: (i32, i32),
    (w, h): (u32, u32),
    blurring: &Blurring<'_>,
    runner: &dyn JobRunner,
) -> Surface {
    let mut field = Surface::filled(w, h, MARKER).expect("a field allocates");
    let mut plane = plane.clone();
    assert!(field.blur_halved_from(at, &mut plane, blurring, &mut BlurScratch::new(), runner));
    field
}

/// The definition's blur of the `w` × `h` block of `surface` at `(x, y)`.
fn reference_of(surface: &Surface, (x, y, w, h): (u32, u32, u32, u32), radius: u32) -> Vec<Pixel> {
    let block: Vec<Pixel> = (y..y + h)
        .flat_map(|row| (x..x + w).map(move |column| (column, row)))
        .map(|(column, row)| surface.get(column, row).expect("inside the surface"))
        .collect();
    halved_reference(&block, w as usize, h as usize, radius)
}

/// Sorted cut points over `start..start + len`, both ends included, so their
/// windows tile it.
fn cuts(rng: &mut Prng, start: u32, len: u32) -> Vec<u32> {
    let mut at: Vec<u32> = (0..3).map(|_| start + rng.next_u32() % len).collect();
    at.push(start);
    at.push(start + len);
    at.sort_unstable();
    at.dedup();
    at
}

/// Some of the cells the cuts of `rect` make: disjoint bands.
fn some_bands(rng: &mut Prng, (x, y, w, h): (u32, u32, u32, u32)) -> Vec<(Range<u32>, Range<u32>)> {
    let (xs, ys) = (cuts(rng, x, w), cuts(rng, y, h));
    let mut bands = Vec::new();
    for cols in xs.windows(2) {
        for rows in ys.windows(2) {
            if rng.next_u32().is_multiple_of(2) {
                bands.push((cols[0]..cols[1], rows[0]..rows[1]));
            }
        }
    }
    bands
}

fn signed(at: u32) -> i32 {
    i32::try_from(at).expect("a test coordinate fits")
}

/// Every shape — one pixel, one row, one column, odd and even sides — at every
/// radius from none to wider than the backdrop blurs to its definition.
#[test]
fn the_blur_is_its_definition_for_every_shape_and_radius() {
    let mut rng = Prng::new(0x4A1F_0B1D_5EED_0001);
    for (width, height) in [
        (1u32, 1u32),
        (1, 9),
        (9, 1),
        (2, 2),
        (3, 3),
        (5, 4),
        (8, 7),
        (17, 12),
        (31, 24),
    ] {
        for radius in [0u32, 1, 2, 3, 4, 5, 8, 13, 40] {
            let plane = backdrop(&mut rng, width, height);
            let whole = [(0..width, 0..height)];
            let rect = (0, 0, width, height);
            let field = field_halved(
                &plane,
                (0, 0),
                (width, height),
                &blurring(rect, &whole, radius),
                &SERIAL,
            );
            assert_eq!(
                field.pixels(),
                &halved_reference(plane.pixels(), width as usize, height as usize, radius)[..],
                "{width}x{height} at radius {radius}"
            );
        }
    }
}

/// Any set of disjoint bands, divided across any runner, writes exactly what
/// the whole blur writes there and nothing anywhere else.
#[test]
fn bands_blur_exactly_what_the_whole_blur_writes_there() {
    let mut rng = Prng::new(0x4A1F_0B1D_5EED_0002);
    for (width, height) in [(23u32, 19u32), (240, 160)] {
        let plane = backdrop(&mut rng, width, height);
        let rect = (0, 0, width, height);
        for radius in [1u32, 2, 5, 12] {
            let reference = reference_of(&plane, rect, radius);
            for participants in [1usize, 3, 5] {
                let runner = Reversed::new(participants);
                let bands = some_bands(&mut rng, rect);
                let field = field_halved(
                    &plane,
                    (0, 0),
                    (width, height),
                    &blurring(rect, &bands, radius),
                    &runner,
                );
                for y in 0..height {
                    for x in 0..width {
                        let inside = bands.iter().any(|(c, r)| c.contains(&x) && r.contains(&y));
                        let expected = if inside {
                            reference[(y * width + x) as usize]
                        } else {
                            MARKER
                        };
                        assert_eq!(
                            field.get(x, y),
                            Some(expected),
                            "({x}, {y}) radius {radius} on {participants} bands {bands:?}"
                        );
                    }
                }
            }
        }
    }
}

/// A field placed anywhere over the plane, past its edges or not, holds the
/// blur at the pixels it covers: the cells are anchored at the rectangle, not
/// at the field.
#[test]
fn a_field_anywhere_holds_the_blur_at_its_own_pixels() {
    let mut rng = Prng::new(0x4A1F_0B1D_5EED_0003);
    let (width, height) = (29u32, 21u32);
    let plane = backdrop(&mut rng, width, height);
    let rect = (0, 0, width, height);
    let whole = [(0..width, 0..height)];
    for radius in [1u32, 4, 7] {
        let reference = reference_of(&plane, rect, radius);
        for (at, size) in [
            ((-3, -2), (10u32, 8u32)),
            ((5, 4), (11, 9)),
            ((22, 15), (12, 10)),
            ((1, 0), (27, 21)),
        ] {
            let field = field_halved(&plane, at, size, &blurring(rect, &whole, radius), &SERIAL);
            for y in 0..size.1 {
                for x in 0..size.0 {
                    let (px, py) = (
                        i64::from(x) + i64::from(at.0),
                        i64::from(y) + i64::from(at.1),
                    );
                    let on =
                        (0..i64::from(width)).contains(&px) && (0..i64::from(height)).contains(&py);
                    let expected = if on {
                        let at = usize::try_from(py * i64::from(width) + px).expect("on the plane");
                        reference[at]
                    } else {
                        MARKER
                    };
                    assert_eq!(
                        field.get(x, y),
                        Some(expected),
                        "({px}, {py}) radius {radius} at {at:?}"
                    );
                }
            }
        }
    }
}

/// A rectangle inside the plane anchors its cells at its own top-left, odd or
/// not, and replicates at its own edges: the blur of its block alone.
#[test]
fn a_rectangle_anchors_its_cells_at_its_own_top_left() {
    let mut rng = Prng::new(0x4A1F_0B1D_5EED_0004);
    let plane = backdrop(&mut rng, 30, 26);
    for rect in [(3u32, 5u32, 17u32, 13u32), (4, 2, 20, 21), (1, 1, 1, 7)] {
        let bands = [(rect.0..rect.0 + rect.2, rect.1..rect.1 + rect.3)];
        for radius in [1u32, 3, 6] {
            let reference = reference_of(&plane, rect, radius);
            let at = (signed(rect.0), signed(rect.1));
            let field = field_halved(
                &plane,
                at,
                (rect.2, rect.3),
                &blurring(rect, &bands, radius),
                &SERIAL,
            );
            assert_eq!(
                field.pixels(),
                &reference[..],
                "rect {rect:?} radius {radius}"
            );
        }
    }
}

/// Nothing past the reach of the bands is read: a plane holding the backdrop
/// only there blurs exactly as one holding all of it.
#[test]
fn the_backdrop_is_read_only_within_the_reach() {
    const POISON: Pixel = Pixel {
        r: 255,
        g: 0,
        b: 255,
        a: 255,
    };
    let mut rng = Prng::new(0x4A1F_0B1D_5EED_0005);
    let (width, height) = (34u32, 27u32);
    let honest = backdrop(&mut rng, width, height);
    let rect = (0, 0, width, height);
    for radius in [1u32, 2, 3, 6, 9] {
        let reach = halved_reach(radius);
        for _ in 0..16 {
            let bands = some_bands(&mut rng, rect);
            let expected = field_halved(
                &honest,
                (0, 0),
                (width, height),
                &blurring(rect, &bands, radius),
                &SERIAL,
            );
            let mut poisoned = Surface::filled(width, height, POISON).expect("allocates");
            for (cols, rows) in &bands {
                let near = |range: &Range<u32>, limit: u32| {
                    range.start.saturating_sub(reach)..(range.end + reach).min(limit)
                };
                for y in near(rows, height) {
                    for x in near(cols, width) {
                        poisoned.set(x, y, honest.get(x, y).expect("inside the backdrop"));
                    }
                }
            }
            let read = field_halved(
                &poisoned,
                (0, 0),
                (width, height),
                &blurring(rect, &bands, radius),
                &SERIAL,
            );
            assert_eq!(
                read, expected,
                "radius {radius} bands {bands:?} read past its reach"
            );
        }
    }
}

/// A change to one pixel of the backdrop changes the blur nowhere past the
/// reach of it — the edges included, where cells replicate.
#[test]
fn a_change_spreads_no_further_than_the_reach() {
    let mut rng = Prng::new(0x4A1F_0B1D_5EED_0006);
    let (width, height) = (25u32, 19u32);
    let before = backdrop(&mut rng, width, height);
    let blur = |surface: &Surface, radius: u32| {
        halved_reference(surface.pixels(), width as usize, height as usize, radius)
    };
    for radius in [0u32, 1, 2, 3, 6, 11] {
        let reach = halved_reach(radius);
        let blurred = blur(&before, radius);
        for (x, y) in [
            (0u32, 0u32),
            (1, 0),
            (12, 9),
            (13, 10),
            (24, 18),
            (23, 17),
            (24, 0),
        ] {
            let mut changed = before.clone();
            changed.set(x, y, Pixel::TRANSPARENT);
            let after = blur(&changed, radius);
            for ((old, new), (px, py)) in blurred
                .iter()
                .zip(&after)
                .zip((0..height).flat_map(|py| (0..width).map(move |px| (px, py))))
            {
                if old != new {
                    assert!(
                        px.abs_diff(x) <= reach && py.abs_diff(y) <= reach,
                        "radius {radius}: a change at ({x}, {y}) reached ({px}, {py})"
                    );
                }
            }
        }
    }
}

/// A frost mixed back at a shape's coverage is the field mixed back by
/// [`Surface::mix_from`], to the bit: the retained and the unretained frost of
/// a window agree.
#[test]
fn a_frost_mixed_back_is_the_field_mixed_back() {
    let mut rng = Prng::new(0x4A1F_0B1D_5EED_0007);
    let (width, height) = (36u32, 28u32);
    let before = backdrop(&mut rng, width, height);
    let rect = (0, 0, width, height);
    let window = (6u32, 5u32, 23u32, 17u32);
    let coverage = |x: u32, y: u32| {
        round_rect_coverage(
            x.saturating_sub(window.0),
            y.saturating_sub(window.1),
            window.2,
            window.3,
            6,
        )
    };
    for radius in [0u32, 1, 2, 7] {
        for participants in [1usize, 4] {
            let runner = Reversed::new(participants);
            let bands = some_bands(&mut rng, window);
            let mut once = before.clone();
            let mut plane = before.clone();
            assert!(once.frost_halved_from(
                &mut plane,
                &blurring(rect, &bands, radius),
                &mut BlurScratch::new(),
                &runner,
                coverage
            ));
            let field = field_halved(
                &before,
                (0, 0),
                (width, height),
                &blurring(rect, &bands, radius),
                &SERIAL,
            );
            let mut mixed = before.clone();
            for (cols, rows) in &bands {
                mixed.mix_from(
                    &field,
                    (0, 0),
                    (
                        cols.start,
                        rows.start,
                        cols.end - cols.start,
                        rows.end - rows.start,
                    ),
                    coverage,
                );
            }
            assert_eq!(
                mixed, once,
                "radius {radius} on {participants} bands {bands:?}"
            );
        }
    }
}

/// A blur of radius zero is the backdrop: a field takes it, and a frost writes
/// it wherever the shape covers, whatever the destination held there.
///
/// A frost that wrote nothing at radius zero left a window's interior showing
/// whatever was last composed there — a terminal's blur scaled down to nothing
/// at a quarter scale did.
#[test]
fn a_radius_of_zero_lays_the_backdrop() {
    let mut rng = Prng::new(0x4A1F_0B1D_5EED_0008);
    let (width, height) = (12u32, 9u32);
    let plane = backdrop(&mut rng, width, height);
    let rect = (0, 0, width, height);
    let bands = [(2u32..10, 1u32..8)];
    let field = field_halved(
        &plane,
        (0, 0),
        (width, height),
        &blurring(rect, &bands, 0),
        &SERIAL,
    );
    let mut stale = Surface::filled(width, height, MARKER).expect("allocates");
    let mut scratch_plane = plane.clone();
    assert!(stale.frost_halved_from(
        &mut scratch_plane,
        &blurring(rect, &bands, 0),
        &mut BlurScratch::new(),
        &SERIAL,
        |_, _| 255
    ));
    for y in 0..height {
        for x in 0..width {
            let inside = bands[0].0.contains(&x) && bands[0].1.contains(&y);
            let expected = if inside {
                plane.get(x, y)
            } else {
                Some(MARKER)
            };
            assert_eq!(field.get(x, y), expected, "the field at ({x}, {y})");
            assert_eq!(stale.get(x, y), expected, "the frost at ({x}, {y})");
        }
    }
}

/// A uniform backdrop blurs to itself, and a premultiplied one stays
/// premultiplied: no channel ever exceeds its alpha.
#[test]
fn the_blur_keeps_a_uniform_field_and_the_premultiplied_invariant() {
    let mut rng = Prng::new(0x4A1F_0B1D_5EED_0009);
    let uniform = Pixel {
        r: 37,
        g: 90,
        b: 140,
        a: 200,
    };
    for radius in [1u32, 4, 9] {
        let flat = Surface::filled(19, 14, uniform).expect("allocates");
        let whole = [(0u32..19, 0u32..14)];
        let field = field_halved(
            &flat,
            (0, 0),
            (19, 14),
            &blurring((0, 0, 19, 14), &whole, radius),
            &SERIAL,
        );
        assert!(
            field.pixels().iter().all(|&pixel| pixel == uniform),
            "radius {radius} moved a flat field"
        );
        let noisy = backdrop(&mut rng, 19, 14);
        let field = field_halved(
            &noisy,
            (0, 0),
            (19, 14),
            &blurring((0, 0, 19, 14), &whole, radius),
            &SERIAL,
        );
        assert!(
            field
                .pixels()
                .iter()
                .all(|p| p.r <= p.a && p.g <= p.a && p.b <= p.a),
            "radius {radius} broke the premultiplied invariant"
        );
    }
}

/// The coordinates a frost asked its shape about.
struct Recorder(Mutex<Vec<(u32, u32)>>);

impl Recorder {
    fn record(&self, x: u32, y: u32) -> u8 {
        if let Ok(mut asked) = self.0.lock() {
            asked.push((x, y));
        }
        255
    }
}

/// A frost asks its shape about each pixel it mixes at the destination's own
/// coordinates, exactly once: the rectangle is what the cells are anchored to,
/// and a window's shape lies wherever the window does.
#[test]
fn a_frost_asks_its_coverage_at_the_destination_coordinates() {
    let mut rng = Prng::new(0x4A1F_0B1D_5EED_000A);
    let before = backdrop(&mut rng, 14, 12);
    let bands = [(4u32..7, 3u32..5), (9u32..13, 7u32..8)];
    let asked = Recorder(Mutex::new(Vec::new()));
    let mut dest = before.clone();
    let mut plane = before.clone();
    assert!(dest.frost_halved_from(
        &mut plane,
        &blurring((0, 0, 14, 12), &bands, 3),
        &mut BlurScratch::new(),
        &SERIAL,
        |x, y| asked.record(x, y)
    ));
    let mut expected: Vec<(u32, u32)> = bands
        .iter()
        .flat_map(|(cols, rows)| {
            rows.clone()
                .flat_map(move |y| cols.clone().map(move |x| (x, y)))
        })
        .collect();
    let mut taken = asked.0.into_inner().unwrap_or_default();
    expected.sort_unstable();
    taken.sort_unstable();
    assert_eq!(taken, expected);
}

/// A clip window on the destination confines the frost, and what it admits is
/// the frost the unclipped destination takes there.
#[test]
fn a_clip_confines_the_frost() {
    let mut rng = Prng::new(0x4A1F_0B1D_5EED_000B);
    let before = backdrop(&mut rng, 20, 16);
    let bands = [(2u32..18, 1u32..15)];
    let frost = |dest: &mut Surface| {
        let mut plane = before.clone();
        assert!(dest.frost_halved_from(
            &mut plane,
            &blurring((0, 0, 20, 16), &bands, 4),
            &mut BlurScratch::new(),
            &SERIAL,
            |_, _| 230
        ));
    };
    let mut whole = before.clone();
    frost(&mut whole);
    let mut clipped = before.clone();
    clipped.with_clip(5, 4, 7, 6, |surface| frost(surface));
    for y in 0..16u32 {
        for x in 0..20u32 {
            let admitted = (5..12).contains(&x) && (4..10).contains(&y);
            let expected = if admitted {
                whole.get(x, y)
            } else {
                before.get(x, y)
            };
            assert_eq!(clipped.get(x, y), expected, "({x}, {y})");
        }
    }
}

/// A scratch reserved for a rectangle runs any frost of it no larger on the
/// runner it was reserved for, field or mixed back, without growing: the
/// compositor frosts every frame from one reservation.
#[test]
fn a_reserved_scratch_halves_without_growing() {
    let mut rng = Prng::new(0x4A1F_0B1D_5EED_000C);
    let runner = Reversed::new(4);
    let (width, height) = (61u32, 47u32);
    let mut scratch = BlurScratch::new();
    assert!(scratch.reserve_halved(width, height, 4, &runner));
    let held = |scratch: &BlurScratch| {
        (
            scratch.lines.capacity(),
            scratch.sums.capacity(),
            scratch.strip.capacity(),
            scratch.bands.capacity(),
            scratch.pieces.capacity(),
            scratch.cells.capacity(),
            scratch.spread.capacity(),
            scratch
                .reduced
                .as_ref()
                .map(|reduced| (reduced.width(), reduced.height())),
        )
    };
    let reserved = held(&scratch);
    let before = backdrop(&mut rng, width, height);
    let rect = (0, 0, width, height);
    for radius in [1u32, 6, 43] {
        for bands in [
            vec![(0u32..width, 0u32..height)],
            vec![
                (3u32..40, 2u32..9),
                (3..9, 9..30),
                (34..40, 9..30),
                (3..40, 30..37),
            ],
            vec![(10u32..11, 0u32..height), (50..61, 46..47)],
        ] {
            let (mut plane, mut field) = (
                before.clone(),
                Surface::new(width, height).expect("allocates"),
            );
            assert!(field.blur_halved_from(
                (0, 0),
                &mut plane,
                &blurring(rect, &bands, radius),
                &mut scratch,
                &runner
            ));
            let (mut plane, mut dest) = (before.clone(), before.clone());
            assert!(dest.frost_halved_from(
                &mut plane,
                &blurring(rect, &bands, radius),
                &mut scratch,
                &runner,
                |_, _| 200
            ));
        }
    }
    assert_eq!(
        held(&scratch),
        reserved,
        "a frost within the reservation grew the scratch"
    );
}

/// A wiped scratch holds no picture of what it last frosted, and frosts after
/// the wipe exactly as a fresh one does.
#[test]
fn a_wiped_scratch_holds_no_picture_and_frosts_the_same() {
    let mut rng = Prng::new(0x4A1F_0B1D_5EED_000D);
    let before = backdrop(&mut rng, 26, 20);
    let rect = (0, 0, 26, 20);
    let bands = [(1u32..25, 2u32..19)];
    let mut scratch = BlurScratch::new();
    assert!(scratch.reserve_halved(26, 20, 1, &SERIAL));
    let mut field = Surface::new(26, 20).expect("allocates");
    let mut plane = before.clone();
    assert!(field.blur_halved_from(
        (0, 0),
        &mut plane,
        &blurring(rect, &bands, 5),
        &mut scratch,
        &SERIAL
    ));
    scratch.wipe();
    let blank = |pixels: &[Pixel]| pixels.iter().all(|&pixel| pixel == Pixel::TRANSPARENT);
    assert!(scratch
        .reduced
        .as_ref()
        .is_some_and(|reduced| blank(reduced.pixels())));
    assert!(scratch.spread.iter().all(|&lane| lane == 0));
    let mut again = Surface::new(26, 20).expect("allocates");
    let mut plane = before.clone();
    assert!(again.blur_halved_from(
        (0, 0),
        &mut plane,
        &blurring(rect, &bands, 5),
        &mut scratch,
        &SERIAL
    ));
    assert_eq!(again, field);
}

/// The reach is what the definition's two stages read, and no more: half the
/// radius in cells either side, a cell for the spread, and the pair of pixels
/// a cell averages.
#[test]
fn the_reach_follows_the_radius() {
    assert_eq!(halved_reach(0), 0);
    assert_eq!(halved_reach(1), 2);
    assert_eq!(halved_reach(2), 4);
    assert_eq!(halved_reach(3), 4);
    assert_eq!(halved_reach(43), 44);
    assert_eq!(halved_reach(u32::MAX), u32::MAX);
}
