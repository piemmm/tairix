//! The clip: a triangle within every plane passes whole, one crossing a plane
//! keeps only what lies inside it, a shared edge is cut at one point, and what
//! survives lands on the converter's grid within the guard band.

use tairix_fuzzseed::Prng;
use tairix_inline::ArrayVec;

use super::{Cut, Mapped, Viewport, GUARD_PIXELS, LEAST_W, MAX_CLIP_CORNERS};
use crate::mesh::MESH_SUB_UNITS;

const SCREEN: (f64, f64) = (640.0, 480.0);

fn viewport(clamp: bool) -> Viewport {
    Viewport::new((0.0, 0.0), SCREEN, (0.0, 1.0), clamp).expect("a viewport")
}

fn clipped(viewport: &Viewport, triangle: [[f64; 4]; 3]) -> ArrayVec<Mapped, MAX_CLIP_CORNERS> {
    let mut out = ArrayVec::new();
    viewport.clip(triangle, &mut out);
    out
}

fn sub(pixels: f64) -> i32 {
    tairix_util::mathf::round_i32(pixels * f64::from(MESH_SUB_UNITS))
}

/// The clip-space point `weights` make of `triangle`'s corners.
fn rebuilt(triangle: [[f64; 4]; 3], weights: [f64; 3]) -> [f64; 4] {
    core::array::from_fn(|axis| {
        (0..3)
            .map(|corner| weights[corner] * triangle[corner][axis])
            .sum()
    })
}

fn near(a: f64, b: f64, by: f64) -> bool {
    (a - b).abs() <= by
}

#[test]
fn a_triangle_inside_every_plane_maps_its_corners_whole() {
    let triangle = [
        [-0.5, -0.5, 0.25, 1.0],
        [1.0, -1.0, 1.0, 2.0],
        [0.0, 2.0, 1.5, 4.0],
    ];
    let out = clipped(&viewport(false), triangle);
    assert_eq!(out.len(), 3);
    for (index, (corner, [x, y, z, w])) in out.iter().zip(triangle).enumerate() {
        let mut one = [0.0; 3];
        one[index] = 1.0;
        assert_eq!(corner.weights.map(f64::to_bits), one.map(f64::to_bits));
        assert_eq!(corner.linear.map(f64::to_bits), one.map(f64::to_bits));
        assert_eq!(corner.x, sub(320.0 + 320.0 * (x / w)));
        assert_eq!(corner.y, sub(240.0 + 240.0 * (y / w)));
        assert_eq!(corner.depth.to_bits(), (z / w).to_bits());
        assert_eq!(corner.inverse_w.to_bits(), (1.0 / w).to_bits());
    }
}

#[test]
fn a_flipped_viewport_maps_the_top_of_clip_space_to_its_bottom() {
    let flipped = Viewport::new((0.0, 480.0), (640.0, -480.0), (0.0, 1.0), false).expect("flipped");
    let out = clipped(
        &flipped,
        [
            [0.0, -1.0, 0.5, 1.0],
            [1.0, 1.0, 0.5, 1.0],
            [-1.0, 1.0, 0.5, 1.0],
        ],
    );
    assert_eq!(out.len(), 3);
    assert_eq!(out[0].y, sub(480.0));
    assert_eq!(out[1].y, sub(0.0));
    let reversed = Viewport::new((0.0, 0.0), SCREEN, (1.0, 0.0), false).expect("reversed depth");
    let out = clipped(
        &reversed,
        [
            [0.0, 0.0, 0.25, 1.0],
            [0.5, 0.0, 0.25, 1.0],
            [0.0, 0.5, 0.25, 1.0],
        ],
    );
    assert!(near(out[0].depth, 0.75, 1e-12));
}

#[test]
fn a_cut_keeps_only_what_lies_inside_the_planes_and_shares_what_it_was() {
    let mut rng = Prng::new(0x7121_0020);
    let view = viewport(false);
    let mut cut = 0;
    for _ in 0..2000 {
        let corner = |rng: &mut Prng| {
            let mut value =
                |span: f64| (f64::from(rng.next_u32()) / f64::from(u32::MAX) - 0.5) * span;
            let w = value(4.0) + 1.0;
            [value(6.0), value(6.0), value(3.0), w]
        };
        let triangle = [corner(&mut rng), corner(&mut rng), corner(&mut rng)];
        let out = clipped(&view, triangle);
        if out.len() > 3
            || (out.len() == 3
                && out
                    .iter()
                    .any(|c| c.weights.iter().all(|w| w.to_bits() != 1.0f64.to_bits())))
        {
            cut += 1;
        }
        for corner in &out {
            assert!(
                near(corner.weights.iter().sum(), 1.0, 1e-12),
                "{triangle:?}"
            );
            assert!(near(corner.linear.iter().sum(), 1.0, 1e-9), "{triangle:?}");
            assert!(corner.weights.iter().all(|w| *w >= 0.0 && *w <= 1.0));
            let [x, y, z, w] = rebuilt(triangle, corner.weights);
            assert!(w > 0.0, "{triangle:?}: a corner behind the eye");
            assert!(
                z >= -1e-12 && z <= w + 1e-12,
                "{triangle:?}: depth outside the planes"
            );
            let guard = f64::from(GUARD_PIXELS) / 320.0 + 1e-9;
            assert!(x.abs() <= guard * w && y.abs() <= guard * 4.0 / 3.0 * w + 1e-9);
            assert!(
                (0.0..=1.0).contains(&corner.depth),
                "{triangle:?}: {}",
                corner.depth
            );
        }
    }
    assert!(cut > 200, "the sweep cut real triangles: {cut}");
}

#[test]
fn a_shared_edge_is_cut_at_one_point_either_way_round() {
    let view = viewport(false);
    let (a, b) = ([-0.5, 0.2, -0.5, 1.0], [0.6, -0.3, 0.8, 1.5]);
    let (c, d) = ([0.1, 0.9, 0.5, 1.2], [0.2, -0.9, 0.3, 1.1]);
    let one = clipped(&view, [a, b, c]);
    let other = clipped(&view, [b, a, d]);
    let on_edge = |out: &ArrayVec<Mapped, MAX_CLIP_CORNERS>, first: usize, second: usize| {
        out.iter()
            .find(|corner| {
                corner.weights[3 - first - second] == 0.0
                    && corner.weights[first] > 0.0
                    && corner.weights[first] < 1.0
            })
            .map(|corner| {
                (
                    corner.x,
                    corner.y,
                    corner.depth.to_bits(),
                    corner.inverse_w.to_bits(),
                )
            })
    };
    let cut = on_edge(&one, 0, 1).expect("the edge is cut");
    assert_eq!(on_edge(&other, 1, 0), Some(cut), "the two cut it apart");
}

#[test]
fn a_triangle_behind_the_eye_leaves_nothing_and_one_reaching_it_stops_short() {
    let view = viewport(true);
    assert!(clipped(
        &view,
        [
            [0.0, 0.0, 0.0, -1.0],
            [1.0, 0.0, 0.0, -2.0],
            [0.0, 1.0, 0.0, -0.5]
        ]
    )
    .is_empty());
    let out = clipped(
        &view,
        [
            [0.0, 0.0, 0.5, -1.0],
            [0.1, 0.0, 0.5, 2.0],
            [0.0, 0.1, 0.5, 2.0],
        ],
    );
    assert!(!out.is_empty());
    assert!(out
        .iter()
        .all(|corner| corner.inverse_w <= 1.0 / LEAST_W && corner.inverse_w > 0.0));
}

#[test]
fn a_clamped_viewport_cuts_no_depth() {
    let triangle = [
        [0.0, 0.0, -0.5, 1.0],
        [0.5, 0.0, 2.5, 1.0],
        [0.0, 0.5, 0.5, 1.0],
    ];
    let clamped = clipped(&viewport(true), triangle);
    assert_eq!(clamped.len(), 3);
    assert!(
        clamped.iter().any(|corner| corner.depth < 0.0)
            && clamped.iter().any(|corner| corner.depth > 1.0)
    );
    let clipped = clipped(&viewport(false), triangle);
    assert!(clipped.len() > 3, "near and far both cut it");
    assert!(clipped
        .iter()
        .all(|corner| (0.0..=1.0).contains(&corner.depth)));
}

#[test]
fn linear_weights_place_a_corner_where_the_screen_interpolation_does() {
    let view = viewport(false);
    let triangle = [
        [-0.9, -0.2, -0.4, 1.0],
        [0.8, 0.3, 0.9, 3.0],
        [0.2, 0.9, 0.6, 2.0],
    ];
    let screen = triangle.map(|[x, y, _, w]| (320.0 + 320.0 * x / w, 240.0 + 240.0 * y / w));
    let out = clipped(&view, triangle);
    assert!(out.len() > 3);
    for corner in &out {
        let x: f64 = (0..3).map(|i| corner.linear[i] * screen[i].0).sum();
        let y: f64 = (0..3).map(|i| corner.linear[i] * screen[i].1).sum();
        assert!(
            (corner.x - sub(x)).abs() <= 1 && (corner.y - sub(y)).abs() <= 1,
            "{corner:?}"
        );
    }
}

#[test]
fn every_corner_lands_within_the_guard_band_however_far_the_triangle_reaches() {
    let mut rng = Prng::new(0x7121_0021);
    let view = viewport(false);
    let band = (i64::from(GUARD_PIXELS) + 1) * i64::from(MESH_SUB_UNITS);
    for _ in 0..2000 {
        let corner = |rng: &mut Prng| {
            let mut huge = || (f64::from(rng.next_u32()) / f64::from(u32::MAX) - 0.5) * 1e12;
            [huge(), huge(), huge(), huge()]
        };
        let out = clipped(
            &view,
            [corner(&mut rng), corner(&mut rng), corner(&mut rng)],
        );
        for corner in &out {
            assert!(
                (i64::from(corner.x) - 320 * 256).abs() <= band,
                "{corner:?}"
            );
            assert!(
                (i64::from(corner.y) - 240 * 256).abs() <= band,
                "{corner:?}"
            );
            assert!(corner.depth.is_finite() && corner.inverse_w.is_finite());
            assert!(
                corner.linear.iter().all(|share| share.is_finite()),
                "{corner:?}"
            );
            assert!(
                (corner.linear.iter().sum::<f64>() - 1.0).abs() < 1e-6,
                "{corner:?}"
            );
        }
    }
}

#[test]
fn hostile_corners_clip_to_nothing() {
    let view = viewport(false);
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(clipped(
            &view,
            [
                [bad, 0.0, 0.5, 1.0],
                [0.5, 0.0, 0.5, 1.0],
                [0.0, 0.5, 0.5, 1.0]
            ]
        )
        .is_empty());
        assert!(clipped(
            &view,
            [
                [0.0, 0.0, 0.5, bad],
                [0.5, 0.0, 0.5, 1.0],
                [0.0, 0.5, 0.5, 1.0]
            ]
        )
        .is_empty());
    }
}

#[test]
fn a_triangle_crossing_every_plane_fits_its_corners() {
    let view = viewport(false);
    let far = f64::from(GUARD_PIXELS);
    let out = clipped(
        &view,
        [
            [-far, -far, -1.0, 0.5],
            [far * 3.0, 0.0, 3.0, 1.0],
            [0.0, far * 3.0, 0.5, -0.5],
        ],
    );
    assert!(
        out.len() >= 3 && out.len() <= MAX_CLIP_CORNERS,
        "{}",
        out.len()
    );
}

#[test]
fn a_viewport_refuses_what_it_cannot_keep_exact() {
    let made = |origin, size, range| Viewport::new(origin, size, range, false).is_some();
    assert!(made((0.0, 0.0), (1.0, 1.0), (0.0, 1.0)));
    assert!(!made((f64::NAN, 0.0), (1.0, 1.0), (0.0, 1.0)));
    assert!(!made((0.0, 0.0), (0.0, 1.0), (0.0, 1.0)));
    assert!(!made((0.0, 0.0), (-1.0, 1.0), (0.0, 1.0)));
    assert!(!made((0.0, 0.0), (1.0, 0.0), (0.0, 1.0)));
    assert!(!made((0.0, 0.0), (1.0, 1.0), (-0.1, 1.0)));
    assert!(!made((0.0, 0.0), (1.0, 1.0), (0.0, 1.5)));
    let guard = f64::from(GUARD_PIXELS);
    assert!(made((0.0, 0.0), (2.0 * guard, 2.0 * guard), (0.0, 1.0)));
    assert!(!made((0.0, 0.0), (2.0 * guard + 2.0, 1.0), (0.0, 1.0)));
    let extent = f64::from(crate::scan::MAX_DRAWING_EXTENT);
    assert!(!made((extent, 0.0), (1.0, 1.0), (0.0, 1.0)));
}

#[test]
fn a_corner_whose_shares_lose_their_sign_is_refused_not_mapped() {
    // Cut onto the eye plane halfway between `w` of -1e15 and 1e15: the
    // shares cancel to nought though the corner's own `w` is positive.
    let corner = Cut {
        at: [0.0, 0.0, 0.5, LEAST_W],
        weights: [0.5, 0.5, 0.0],
    };
    assert_eq!(viewport(true).map(&corner, [-1e15, 1e15, 1e15]), None);
}
