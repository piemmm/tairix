//! Unit tests for fitting cursor artwork to a pixel grid: where edges land,
//! how the hotspot anchors the layout, and that the fit keeps what the
//! artwork was.

use alloc::vec;
use alloc::vec::Vec;

use tairix_raster::{
    Affine, Color, FillRule, Gradient, GradientKind, GradientStop, Group, Layer, Mask, MaskKind,
    Node, Paint, Pattern, SpreadMethod, TileFold, MAX_DRAWING_EXTENT,
};
use tairix_svg::font::NoFonts;

use super::{Fit, FIT_UNITS};
use crate::vector::{Shape, VectorCursor};

/// Sides from a quarter-size pointer to a very large one, every ratio
/// between whole ones included.
const SIDES: core::ops::RangeInclusive<u32> = 16..=128;

const INK: Color = Color::rgb(20, 30, 40);

/// A cursor on a 32-unit grid holding one rectangle from `from` to `to`,
/// with its hotspot at `hotspot`.
fn rectangle(from: (i32, i32), to: (i32, i32), hotspot: (i32, i32)) -> VectorCursor {
    let shape = Shape::from_points(INK, &[from, (to.0, from.1), to, (from.0, to.1)]);
    VectorCursor::new(32, hotspot.0, hotspot.1, vec![shape])
}

/// The alpha of every pixel of `cursor` at `side`, row by row.
fn coverage(cursor: &VectorCursor, side: u32) -> Vec<u8> {
    cursor
        .rasterise(side)
        .expect("renderable")
        .surface()
        .pixels()
        .iter()
        .map(|pixel| pixel.a)
        .collect()
}

#[test]
fn every_upright_and_level_edge_lands_on_a_pixel_boundary() {
    // Stretched, these edges fall part-way across a pixel at almost every
    // side and smear into grey columns and rows; fitted, a rectangle is only
    // ever wholly covered pixels and wholly empty ones.
    for hotspot in [(0, 0), (16, 16), (5, 29)] {
        let cursor = rectangle((3, 5), (21, 27), hotspot);
        for side in SIDES {
            let alphas = coverage(&cursor, side);
            assert!(
                alphas.iter().all(|&alpha| alpha == 0 || alpha == u8::MAX),
                "a partly covered pixel at side {side}, hotspot {hotspot:?}"
            );
            assert!(alphas.contains(&u8::MAX), "nothing drawn at side {side}");
        }
    }
}

#[test]
fn the_hotspot_is_the_pixel_corner_the_artwork_is_laid_out_from() {
    let cursor = rectangle((5, 7), (20, 25), (5, 7));
    for side in SIDES {
        let image = cursor.rasterise(side).expect("renderable");
        let hotspot = image.hotspot();
        let (hx, hy) = (hotspot.x.unsigned_abs(), hotspot.y.unsigned_abs());
        let surface = image.surface();
        let at = |x: u32, y: u32| surface.get(x, y).map_or(0, |pixel| pixel.a);
        assert_eq!(at(hx, hy), u8::MAX, "the hotspot pixel at side {side}");
        assert_eq!(at(hx.wrapping_sub(1), hy), 0, "left of it at side {side}");
        assert_eq!(at(hx, hy.wrapping_sub(1)), 0, "above it at side {side}");
    }
}

/// A cross with a diagonal notch, symmetric under a half turn about
/// `(16, 16)` but about neither axis alone.
fn half_turn_symmetric() -> VectorCursor {
    let shape = Shape::from_points(
        INK,
        &[
            (13, 3),
            (19, 3),
            (19, 13),
            (29, 13),
            (29, 19),
            (21, 19),
            (19, 29),
            (13, 29),
            (13, 19),
            (3, 19),
            (3, 13),
            (11, 13),
        ],
    );
    VectorCursor::new(32, 16, 16, vec![shape])
}

#[test]
fn artwork_symmetric_about_its_hotspot_fits_symmetric_at_every_side() {
    let cursor = half_turn_symmetric();
    for side in SIDES {
        let fit = Fit::new(&cursor, side).expect("a fit");
        let (ax, ay) = fit.anchor();
        let fitted = fit.nodes(cursor.nodes()).expect("within the bound");
        let [Node::Fill(layer)] = fitted.as_slice() else {
            panic!("one layer");
        };
        let mut points: Vec<(i64, i64)> = layer.contours[0]
            .iter()
            .map(|&(x, y)| (i64::from(x), i64::from(y)))
            .collect();
        let mut turned: Vec<(i64, i64)> = points
            .iter()
            .map(|&(x, y)| (2 * ax - x, 2 * ay - y))
            .collect();
        points.sort_unstable();
        turned.sort_unstable();
        assert_eq!(points, turned, "side {side}");
    }
}

#[test]
fn artwork_symmetric_about_its_hotspot_rasterises_symmetric_at_every_side() {
    // Within one alpha level: the scan converter places each crossing to a
    // 256th of a pixel measured from the edge's upper end, which a half turn
    // swaps for its lower one.
    let cursor = half_turn_symmetric();
    for side in SIDES {
        let image = cursor.rasterise(side).expect("renderable");
        let hotspot = image.hotspot();
        let surface = image.surface();
        let turned = |x: u32, y: u32| {
            let tx = u32::try_from(2 * i64::from(hotspot.x) - 1 - i64::from(x)).ok()?;
            let ty = u32::try_from(2 * i64::from(hotspot.y) - 1 - i64::from(y)).ok()?;
            surface.get(tx, ty).map(|pixel| pixel.a)
        };
        for y in 0..side {
            for x in 0..side {
                let here = surface.get(x, y).map_or(0, |pixel| pixel.a);
                let there = turned(x, y).unwrap_or(0);
                assert!(
                    here.abs_diff(there) <= 1,
                    "{here} against {there} at ({x}, {y}), side {side}"
                );
            }
        }
    }
}

#[test]
fn the_fit_never_reorders_two_coordinates() {
    // Knots close together on both sides of the hotspot, so a fit that
    // pushed one past its neighbour would show here.
    let shapes = [(2, 6), (7, 9), (10, 11), (20, 23), (24, 25)]
        .into_iter()
        .map(|(left, right)| {
            Shape::from_points(INK, &[(left, 0), (right, 0), (right, 32), (left, 32)])
        })
        .collect();
    let cursor = VectorCursor::new(32, 12, 0, shapes);
    for side in SIDES {
        let fit = Fit::new(&cursor, side).expect("a fit");
        let mapped: Vec<i32> = (-8..=40).map(|x| fit.point((x, 0)).0).collect();
        assert!(
            mapped.windows(2).all(|pair| pair[0] <= pair[1]),
            "reordered at side {side}: {mapped:?}"
        );
    }
}

#[test]
fn a_stem_half_a_pixel_wide_or_more_never_vanishes() {
    // Two units on a 64-unit grid: half a pixel at side 16, where plain
    // rounding lands both of the stem's edges on one boundary.
    for (left, right) in [(29, 31), (30, 32), (33, 35)] {
        let cursor = VectorCursor::new(
            64,
            32,
            32,
            vec![Shape::from_points(
                INK,
                &[(left, 0), (right, 0), (right, 64), (left, 64)],
            )],
        );
        for side in 16..=40 {
            let fit = Fit::new(&cursor, side).expect("a fit");
            let width = fit.point((right, 0)).0 - fit.point((left, 0)).0;
            let units = i32::try_from(FIT_UNITS).unwrap_or(0);
            assert!(
                width >= units,
                "stem {left}..{right} vanished at side {side}"
            );
        }
    }
}

#[test]
fn pieces_that_overlapped_still_overlap_once_fitted() {
    // A stroked ring is a union of a rectangle per segment and a join per
    // vertex. Fitting only their corners bent each piece differently and
    // opened cracks some thirty levels deep where they met.
    let svg = br##"<svg viewBox="0 0 32 32"><circle cx="16" cy="16" r="7.5" fill="none" stroke="#000" stroke-width="5"/></svg>"##;
    let cursor = crate::decode_svg(svg, &mut NoFonts).expect("a ring");
    for side in 24..=96 {
        let image = cursor.rasterise(side).expect("renderable");
        let scale = f64::from(side) / 32.0;
        for y in 0..side {
            for x in 0..side {
                let dx = (f64::from(x) + 0.5) / scale - 16.0;
                let dy = (f64::from(y) + 0.5) / scale - 16.0;
                let radius = tairix_util::mathf::sqrt(dx * dx + dy * dy);
                // Well inside the band, clear of both anti-aliased rims.
                if radius > 5.0 + 1.5 / scale && radius < 10.0 - 1.5 / scale {
                    let alpha = image.surface().get(x, y).map_or(0, |pixel| pixel.a);
                    assert_eq!(alpha, u8::MAX, "a crack at ({x}, {y}), side {side}");
                }
            }
        }
    }
}

#[test]
fn a_gradient_keeps_its_place_on_fitted_geometry() {
    // Red at the square's left edge to blue at its right, on a grid whose
    // fit moves both edges.
    let gradient = Gradient {
        kind: GradientKind::Linear,
        stops: vec![
            GradientStop {
                offset: 0.0,
                color: Color::rgb(255, 0, 0),
            },
            GradientStop {
                offset: 1.0,
                color: Color::rgb(0, 0, 255),
            },
        ],
        spread: SpreadMethod::Pad,
        // Design x 5 → 0 and x 27 → 1.
        to_gradient: Affine::translate(-5.0, 0.0).then(Affine::scale(1.0 / 22.0, 1.0)),
    };
    let layer = Layer::filled(
        Paint::Gradient(gradient),
        FillRule::NonZero,
        vec![vec![(5, 5), (27, 5), (27, 27), (5, 27)]],
    );
    let cursor = VectorCursor::from_artwork(32, 16, 16, vec![Node::Fill(layer)]);
    for side in [24, 40, 50, 72] {
        let image = cursor.rasterise(side).expect("renderable");
        let surface = image.surface();
        let row = side / 2;
        let drawn: Vec<u32> = (0..side)
            .filter(|&x| surface.get(x, row).is_some_and(|pixel| pixel.a == u8::MAX))
            .collect();
        let (Some(&first), Some(&last)) = (drawn.first(), drawn.last()) else {
            panic!("nothing drawn at side {side}");
        };
        let left = surface.get(first, row).expect("drawn").unpremultiply();
        let right = surface.get(last, row).expect("drawn").unpremultiply();
        assert!(
            left.r > 200 && left.b < 55,
            "left edge {left:?} at side {side}"
        );
        assert!(
            right.b > 200 && right.r < 55,
            "right edge {right:?} at side {side}"
        );
    }
}

#[test]
fn a_pattern_tile_is_restated_on_the_fitted_grid() {
    // A tile the colour of its whole grid paints that colour everywhere; one
    // left on the old, coarser grid would cover a sliver of each tile.
    let fill = Color::rgb(10, 200, 90);
    let tile = Layer::filled(
        Paint::Solid(fill),
        FillRule::NonZero,
        vec![vec![(0, 0), (32, 0), (32, 32), (0, 32)]],
    );
    let pattern = Pattern {
        content: vec![Node::Fill(tile)],
        to_tile: Affine::scale(1.0 / 8.0, 1.0 / 8.0),
        fold: TileFold::default(),
        opacity: u8::MAX,
    };
    let layer = Layer::filled(
        Paint::Pattern(pattern),
        FillRule::NonZero,
        vec![vec![(4, 4), (28, 4), (28, 28), (4, 28)]],
    );
    let cursor = VectorCursor::from_artwork(32, 16, 16, vec![Node::Fill(layer)]);
    for side in [24, 32, 40, 64] {
        let image = cursor.rasterise(side).expect("renderable");
        let centre = image.surface().get(side / 2, side / 2).expect("in bounds");
        assert_eq!(centre.a, u8::MAX, "side {side}");
        assert_eq!(centre.unpremultiply(), fill, "side {side}");
    }
}

#[test]
fn a_clip_is_fitted_with_what_it_clips() {
    let inside = Layer::filled(
        Paint::Solid(INK),
        FillRule::NonZero,
        vec![vec![(0, 0), (32, 0), (32, 32), (0, 32)]],
    );
    let clip = Layer::filled(
        Paint::Solid(Color::rgb(255, 255, 255)),
        FillRule::NonZero,
        vec![vec![(7, 9), (23, 9), (23, 21), (7, 21)]],
    );
    let group = Group {
        opacity: u8::MAX,
        mask: Some(Mask {
            kind: MaskKind::Alpha,
            content: vec![Node::Fill(clip)],
        }),
        children: vec![Node::Fill(inside)],
    };
    let cursor = VectorCursor::from_artwork(32, 0, 0, vec![Node::Group(group)]);
    for side in [24, 40, 56] {
        let alphas = coverage(&cursor, side);
        assert!(
            alphas.iter().all(|&alpha| alpha == 0 || alpha == u8::MAX),
            "the clip's edges land between pixels at side {side}"
        );
        assert!(
            alphas.contains(&u8::MAX) && alphas.contains(&0),
            "the clip did not clip at side {side}"
        );
    }
}

#[test]
fn a_side_or_grid_that_cannot_be_drawn_has_no_fit() {
    let cursor = rectangle((0, 0), (32, 32), (0, 0));
    assert!(Fit::new(&cursor, 0).is_none());
    assert!(Fit::new(&cursor, MAX_DRAWING_EXTENT + 1).is_none());
    assert!(Fit::new(&VectorCursor::new(0, 0, 0, Vec::new()), 32).is_none());
}

#[test]
fn coordinates_at_the_edge_of_the_integer_range_fit_without_overflow() {
    let extremes = [i32::MIN, i32::MIN + 1, -1, 0, 1, i32::MAX - 1, i32::MAX];
    for &hotspot in &extremes {
        let shapes = vec![
            Shape::from_points(
                INK,
                &[
                    (i32::MIN, i32::MIN),
                    (i32::MAX, i32::MIN),
                    (i32::MAX, i32::MAX),
                ],
            ),
            Shape::from_points(INK, &[(i32::MIN, 0), (i32::MAX, 0), (i32::MAX, 5)]),
        ];
        let cursor = VectorCursor::new(1, hotspot, hotspot, shapes);
        for side in [1, 32, MAX_DRAWING_EXTENT] {
            let fit = Fit::new(&cursor, side).expect("a fit");
            for &x in &extremes {
                let _ = fit.point((x, x));
            }
            assert!(fit.nodes(cursor.nodes()).is_some());
        }
    }
}

#[test]
fn artwork_crossing_past_the_bound_draws_no_cursor() {
    // Hundreds of upright bars, each a pair of lines the fit bends at, and
    // hundreds of diagonals crossing every one: the splits alone would run to
    // hundreds of thousands of points, which is a drawing built to be costly,
    // not a pointer.
    let bars: Vec<Shape> = (0..400)
        .map(|bar| {
            let x = 100 + 150 * bar;
            Shape::from_points(INK, &[(x, 0), (x + 60, 0), (x + 60, 65_000), (x, 65_000)])
        })
        .collect();
    let uncrossed = VectorCursor::new(65_536, 0, 0, bars.clone());
    assert!(uncrossed.rasterise(32).is_some());
    let diagonals = (0..400).map(|line| {
        let y = 100 * line;
        Shape::from_points(
            INK,
            &[
                (0, y),
                (65_000, y + 30_000),
                (65_000, y + 30_010),
                (0, y + 10),
            ],
        )
    });
    let crossed = VectorCursor::new(65_536, 0, 0, bars.into_iter().chain(diagonals).collect());
    assert!(crossed.rasterise(32).is_none());
}

#[test]
fn a_group_past_the_drawn_depth_is_refused_as_the_renderer_refuses_it() {
    // Nested far past anything the renderer draws.
    let mut nested = Node::Fill(Shape::from_points(INK, &[(4, 4), (28, 4), (28, 28)]));
    for _ in 0..256 {
        nested = Node::Group(Group {
            opacity: u8::MAX,
            mask: None,
            children: vec![nested],
        });
    }
    let cursor = VectorCursor::from_artwork(32, 16, 16, vec![nested]);
    let fit = Fit::new(&cursor, 32).expect("a fit");
    assert!(fit.nodes(cursor.nodes()).is_some());
    assert!(cursor.rasterise(32).is_none());
    assert!(cursor
        .with_outline(crate::Outline {
            color: INK,
            width: 1,
        })
        .rasterise(32)
        .is_none());
}
