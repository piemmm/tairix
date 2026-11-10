//! A primitive's weights meet its corners, are perspective-correct, linear or
//! flat as asked, and carry through a clip to the triangle the draw named;
//! its depth is affine and its slope the steeper axis.

use tairix_inline::ArrayVec;

use super::{Interpolation, Primitive};
use crate::clip::{Mapped, Viewport, MAX_CLIP_CORNERS};
use crate::mesh::MESH_SUB_UNITS;

fn viewport() -> Viewport {
    Viewport::new((0.0, 0.0), (640.0, 480.0), (0.0, 1.0), false).expect("a viewport")
}

fn clipped(triangle: [[f64; 4]; 3]) -> ArrayVec<Mapped, MAX_CLIP_CORNERS> {
    let mut out = ArrayVec::new();
    viewport().clip(triangle, &mut out);
    out
}

fn pixels(sub: i32) -> f64 {
    f64::from(sub) / f64::from(MESH_SUB_UNITS)
}

fn close(a: [f64; 3], b: [f64; 3], by: f64) -> bool {
    a.iter().zip(b).all(|(a, b)| (a - b).abs() <= by)
}

/// Each screen point's barycentrics in the projected triangle `screen`.
fn barycentrics(screen: [(f64, f64); 3], (x, y): (f64, f64)) -> [f64; 3] {
    let [(x0, y0), (x1, y1), (x2, y2)] = screen;
    let det = (x1 - x0) * (y2 - y0) - (x2 - x0) * (y1 - y0);
    let one = ((x - x0) * (y2 - y0) - (x2 - x0) * (y - y0)) / det;
    let two = ((x1 - x0) * (y - y0) - (x - x0) * (y1 - y0)) / det;
    [1.0 - one - two, one, two]
}

const TRIANGLE: [[f64; 4]; 3] = [
    [-0.8, -0.6, 0.2, 1.0],
    [0.9, -0.2, 0.5, 4.0],
    [0.1, 0.8, 0.9, 2.0],
];

#[test]
fn the_weights_meet_each_corner_and_sum_to_one() {
    let corners = clipped(TRIANGLE);
    assert_eq!(corners.len(), 3);
    let primitive = Primitive::new([corners[0], corners[1], corners[2]]).expect("a primitive");
    for (index, corner) in corners.iter().enumerate() {
        let mut own = [0.0; 3];
        own[index] = 1.0;
        let at = (pixels(corner.x), pixels(corner.y));
        for interpolation in [Interpolation::Perspective, Interpolation::Linear] {
            let weights = primitive.weights(interpolation, at.0, at.1);
            assert!(
                close(weights, own, 1e-9),
                "{interpolation:?} at corner {index}: {weights:?}"
            );
        }
        assert!((primitive.depth(at.0, at.1) - corner.depth).abs() < 1e-12);
        assert!((primitive.inverse_w(at.0, at.1) - corner.inverse_w).abs() < 1e-12);
    }
    let weights = primitive.weights(Interpolation::Perspective, 300.0, 240.0);
    assert!((weights.iter().sum::<f64>() - 1.0).abs() < 1e-12);
    assert_eq!(
        primitive
            .weights(Interpolation::Flat, 300.0, 240.0)
            .map(f64::to_bits),
        [1.0f64, 0.0, 0.0].map(f64::to_bits)
    );
}

#[test]
fn a_clipped_triangle_interpolates_as_the_whole_one_does() {
    // The second corner lies past the far plane, so the clip cuts it.
    let triangle = [
        [-0.8, -0.6, 0.2, 1.0],
        [0.9, -0.2, 5.0, 4.0],
        [0.1, 0.8, 0.9, 2.0],
    ];
    let corners = clipped(triangle);
    assert!(corners.len() > 3, "the far plane cuts it");
    let screen = triangle.map(|[x, y, _, w]| (320.0 + 320.0 * x / w, 240.0 + 240.0 * y / w));
    let q = triangle.map(|corner| 1.0 / corner[3]);
    let mut tested = 0;
    for piece in 1..corners.len() - 1 {
        let Some(primitive) = Primitive::new([corners[0], corners[piece], corners[piece + 1]])
        else {
            continue;
        };
        let centroid = [0, piece, piece + 1]
            .iter()
            .fold((0.0, 0.0), |(x, y), &at| {
                (
                    x + pixels(corners[at].x) / 3.0,
                    y + pixels(corners[at].y) / 3.0,
                )
            });
        let lambda = barycentrics(screen, centroid);
        let total: f64 = (0..3).map(|i| lambda[i] * q[i]).sum();
        let perspective = core::array::from_fn(|i| lambda[i] * q[i] / total);
        // A clipped corner lies on the grid, half a sub-unit at most from
        // where the cut put it; over a triangle hundreds of pixels across that
        // moves a weight by a few parts in a hundred thousand.
        let weights = primitive.weights(Interpolation::Linear, centroid.0, centroid.1);
        assert!(
            close(weights, lambda, 1e-4),
            "{weights:?} against {lambda:?}"
        );
        let weights = primitive.weights(Interpolation::Perspective, centroid.0, centroid.1);
        assert!(
            close(weights, perspective, 1e-4),
            "{weights:?} against {perspective:?}"
        );
        tested += 1;
    }
    assert!(tested >= 2, "every piece was set up");
}

#[test]
fn depth_is_affine_and_its_slope_the_steeper_axis() {
    let corners = clipped(TRIANGLE);
    let primitive = Primitive::new([corners[0], corners[1], corners[2]]).expect("a primitive");
    let (x, y) = (310.0, 230.0);
    let across = primitive.depth(x + 1.0, y) - primitive.depth(x, y);
    let down = primitive.depth(x, y + 1.0) - primitive.depth(x, y);
    assert!((primitive.slope() - across.abs().max(down.abs())).abs() < 1e-12);
    let midway = primitive.depth(x + 0.5, y + 0.5);
    let averaged = f64::midpoint(primitive.depth(x, y), primitive.depth(x + 1.0, y + 1.0));
    assert!((midway - averaged).abs() < 1e-12);
}

#[test]
fn a_primitive_with_no_area_or_an_unfit_corner_is_refused() {
    let corners = clipped(TRIANGLE);
    let [a, b, c] = [corners[0], corners[1], corners[2]];
    assert!(Primitive::new([a, a, c]).is_none());
    for bad in [f64::NAN, f64::INFINITY, 0.0, -1.0] {
        assert!(Primitive::new([
            Mapped {
                inverse_w: bad,
                ..a
            },
            b,
            c
        ])
        .is_none());
    }
    assert!(Primitive::new([
        Mapped {
            depth: f64::NAN,
            ..a
        },
        b,
        c
    ])
    .is_none());
    assert!(Primitive::new([Mapped { x: i32::MAX, ..a }, b, c]).is_none());
}

#[test]
fn the_winding_is_read_from_the_screen() {
    let corners = clipped(TRIANGLE);
    let [a, b, c] = [corners[0], corners[1], corners[2]];
    let one = Primitive::new([a, b, c])
        .expect("a primitive")
        .footprint()
        .clockwise();
    let other = Primitive::new([a, c, b])
        .expect("a primitive")
        .footprint()
        .clockwise();
    assert_ne!(one, other);
}
