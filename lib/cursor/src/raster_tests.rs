//! Unit tests for the outline band: its width in whole pixels on every edge,
//! where it lies, what it follows, and what it refuses.

use alloc::vec;
use alloc::vec::Vec;

use tairix_raster::{Color, FillRule, Group, Layer, Mask, MaskKind, Node, Paint};

use crate::image::CursorImage;
use crate::vector::{Outline, Shape, VectorCursor};

const FACE: Color = Color::rgb(255, 255, 255);
const RIM: Color = Color::rgb(0, 0, 0);

/// A one-unit rim on the 32-unit grid.
const ONE_UNIT: Outline = Outline {
    color: RIM,
    width: 1,
};

/// A square from `(8, 8)` to `(24, 24)` on a 32-unit grid, pivoting on its
/// centre.
fn square() -> Vec<(i32, i32)> {
    vec![(8, 8), (24, 8), (24, 24), (8, 24)]
}

/// What a pixel of a rasterised rim test shows.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Seen {
    Face,
    Rim,
    Nothing,
    Blend,
}

fn seen(image: &CursorImage, x: i64, y: i64) -> Seen {
    let pixel = u32::try_from(x)
        .ok()
        .zip(u32::try_from(y).ok())
        .and_then(|(x, y)| image.surface().get(x, y));
    match pixel {
        None => Seen::Nothing,
        Some(pixel) if pixel.a == 0 => Seen::Nothing,
        Some(pixel) if pixel.a == u8::MAX && pixel.unpremultiply() == FACE => Seen::Face,
        Some(pixel) if pixel.a == u8::MAX && pixel.unpremultiply() == RIM => Seen::Rim,
        Some(_) => Seen::Blend,
    }
}

/// What is seen walking out from the hotspot along `(dx, dy)` to the edge
/// of the image, run-length encoded.
fn walk(image: &CursorImage, (dx, dy): (i64, i64)) -> Vec<(Seen, i64)> {
    let inside = |x: i64, y: i64| {
        u32::try_from(x).is_ok_and(|x| x < image.width())
            && u32::try_from(y).is_ok_and(|y| y < image.height())
    };
    let (mut x, mut y) = (i64::from(image.hotspot().x), i64::from(image.hotspot().y));
    let mut runs: Vec<(Seen, i64)> = Vec::new();
    while inside(x, y) {
        let here = seen(image, x, y);
        match runs.last_mut() {
            Some((last, count)) if *last == here => *count += 1,
            _ => runs.push((here, 1)),
        }
        (x, y) = (x + dx, y + dy);
    }
    runs
}

/// The rim a one-unit outline draws at `side` on the 32-unit grid: the
/// nearest whole number of pixels, and never none.
fn rim_pixels(side: u32) -> i64 {
    i64::from((2 * side + 32) / 64).max(1)
}

#[test]
fn an_outline_is_a_whole_number_of_pixels_wide_on_every_edge() {
    // The artwork beneath is fitted, so the body's edges are on pixel
    // boundaries, and the rim is a whole number of pixels past them: every
    // rim pixel is the rim's colour, wholly, on all four sides alike.
    let cursor = VectorCursor::new(32, 16, 16, vec![Shape::from_points(FACE, &square())])
        .with_outline(ONE_UNIT);
    for side in 16..=128 {
        let image = cursor.rasterise(side).expect("renderable");
        for direction in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
            let runs = walk(&image, direction);
            let rim = runs.get(1).copied();
            assert_eq!(
                runs.first().map(|run| run.0),
                Some(Seen::Face),
                "the body lies over the rim, side {side}, {direction:?}"
            );
            assert_eq!(
                rim,
                Some((Seen::Rim, rim_pixels(side))),
                "the rim, side {side}, {direction:?}: {runs:?}"
            );
            assert_eq!(
                runs.get(2).map(|run| run.0),
                Some(Seen::Nothing),
                "past the rim, side {side}, {direction:?}"
            );
        }
    }
}

#[test]
fn an_outline_rims_a_hole_as_well_as_the_outside() {
    let ring = Layer::filled(
        Paint::Solid(FACE),
        FillRule::EvenOdd,
        vec![square(), vec![(13, 13), (19, 13), (19, 19), (13, 19)]],
    );
    let cursor =
        VectorCursor::from_artwork(32, 16, 16, vec![Node::Fill(ring)]).with_outline(ONE_UNIT);
    for side in [32, 48, 64, 96] {
        let image = cursor.rasterise(side).expect("renderable");
        let runs: Vec<Seen> = walk(&image, (1, 0)).into_iter().map(|run| run.0).collect();
        assert_eq!(
            runs,
            [
                Seen::Nothing,
                Seen::Rim,
                Seen::Face,
                Seen::Rim,
                Seen::Nothing
            ],
            "side {side}"
        );
    }
}

#[test]
fn a_masked_group_draws_no_outline() {
    // Its visible edge is the mask's, which the band cannot follow, so the
    // group is drawn as authored.
    let body = Layer::filled(Paint::Solid(FACE), FillRule::NonZero, vec![square()]);
    let clip = Layer::filled(Paint::Solid(FACE), FillRule::NonZero, vec![square()]);
    let group = Group {
        opacity: u8::MAX,
        mask: Some(Mask {
            kind: MaskKind::Alpha,
            content: vec![Node::Fill(clip)],
        }),
        children: vec![Node::Fill(body)],
    };
    let cursor =
        VectorCursor::from_artwork(32, 16, 16, vec![Node::Group(group)]).with_outline(ONE_UNIT);
    let image = cursor.rasterise(48).expect("renderable");
    assert!(image
        .surface()
        .pixels()
        .iter()
        .all(|pixel| pixel.a == 0 || pixel.unpremultiply() == FACE));
}

#[test]
fn a_group_seen_through_its_opacity_alone_is_outlined() {
    let body = Layer::filled(Paint::Solid(FACE), FillRule::NonZero, vec![square()]);
    let group = Group {
        opacity: 128,
        mask: None,
        children: vec![Node::Fill(body)],
    };
    let cursor =
        VectorCursor::from_artwork(32, 16, 16, vec![Node::Group(group)]).with_outline(ONE_UNIT);
    let image = cursor.rasterise(32).expect("renderable");
    assert_eq!(seen(&image, 7, 16), Seen::Rim);
}

#[test]
fn a_rim_survives_the_smallest_side() {
    // A one-unit rim at an eighth of the grid is a quarter of a pixel, which
    // would round to none.
    let cursor = VectorCursor::new(32, 16, 16, vec![Shape::from_points(FACE, &square())])
        .with_outline(ONE_UNIT);
    let image = cursor.rasterise(8).expect("renderable");
    assert!(image
        .surface()
        .pixels()
        .iter()
        .any(|pixel| pixel.a == u8::MAX && pixel.unpremultiply() == RIM));
}

#[test]
fn an_outline_too_complex_to_build_draws_no_cursor() {
    // Past the containment bound the band is refused, and a cursor missing
    // its rim is not drawn in its place.
    let steps = 70_000_u32;
    let many: Vec<(i32, i32)> = (0..steps)
        .map(|step| {
            let angle = core::f64::consts::TAU * f64::from(step) / f64::from(steps);
            (
                tairix_util::mathf::round_i32(16_000.0 + 12_000.0 * tairix_util::mathf::cos(angle)),
                tairix_util::mathf::round_i32(16_000.0 + 12_000.0 * tairix_util::mathf::sin(angle)),
            )
        })
        .collect();
    let bare = VectorCursor::new(
        32_000,
        16_000,
        16_000,
        vec![Shape::from_points(FACE, &many)],
    );
    assert!(bare.rasterise(32).is_some());
    assert!(bare.with_outline(ONE_UNIT).rasterise(32).is_none());
}

#[test]
fn an_empty_cursor_with_an_outline_draws_nothing() {
    let cursor = VectorCursor::new(32, 0, 0, Vec::new()).with_outline(ONE_UNIT);
    let image = cursor.rasterise(32).expect("renderable");
    assert!(image.surface().pixels().iter().all(|pixel| pixel.a == 0));
}

#[test]
fn a_contour_enclosing_nothing_is_not_outlined() {
    // A line drawn as a closed contour of two points, and three points on one
    // line, fill nothing, so there is nothing for a rim to go round.
    let nothing = Layer::filled(
        Paint::Solid(FACE),
        FillRule::NonZero,
        vec![vec![(4, 4), (28, 28)], vec![(4, 16), (16, 16), (28, 16)]],
    );
    let cursor =
        VectorCursor::from_artwork(32, 16, 16, vec![Node::Fill(nothing)]).with_outline(ONE_UNIT);
    let image = cursor.rasterise(32).expect("renderable");
    assert!(image.surface().pixels().iter().all(|pixel| pixel.a == 0));
}
