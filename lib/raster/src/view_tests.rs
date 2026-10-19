use tairix_inline::ArrayVec;

use super::{Clipped, Perspective, GUARD_PIXELS, MAX_CLIPPED};
use crate::mesh::{Depth, MESH_SUB_UNITS};
use crate::scan::MAX_DRAWING_EXTENT;

/// An eye at the origin looking along +y, with +x to the right and -z down
/// the screen: a world whose third axis points up.
fn level() -> Perspective {
    Perspective::new(
        [0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        [1.0, 0.0, 0.0],
        [0.0, 0.0, -1.0],
        500.0,
        (640, 480),
        0.25,
    )
    .expect("a view")
}

/// Whether two reals are the same bits: the claim an exact projection makes.
fn same(a: f64, b: f64) -> bool {
    a.to_bits() == b.to_bits()
}

fn sum(weights: [f64; 3]) -> f64 {
    weights[0] + weights[1] + weights[2]
}

fn clipped(view: &Perspective, triangle: [[f64; 3]; 3]) -> ArrayVec<Clipped, MAX_CLIPPED> {
    let mut out = ArrayVec::new();
    view.clip(triangle, &mut out);
    out
}

/// Where the weights put a corner, from the triangle's own corners.
fn rebuilt(triangle: [[f64; 3]; 3], weights: [f64; 3]) -> [f64; 3] {
    let mut at = [0.0; 3];
    for (corner, share) in triangle.iter().zip(weights) {
        for axis in 0..3 {
            at[axis] += corner[axis] * share;
        }
    }
    at
}

#[test]
fn a_point_ahead_lands_on_the_centre_and_offsets_keep_their_sense() {
    let view = level();
    let ahead = view.project(view.view([0.0, 10.0, 0.0])).expect("ahead");
    assert!(same(ahead.x, 320.0) && same(ahead.y, 240.0));
    let right = view.project(view.view([1.0, 10.0, 0.0])).expect("right");
    assert!(right.x > 320.0 && same(right.y, 240.0));
    let up = view.project(view.view([0.0, 10.0, 1.0])).expect("up");
    assert!(up.y < 240.0 && same(up.x, 320.0));
    // Twice as far is half the reciprocal and half the spread.
    let far = view.project(view.view([1.0, 20.0, 0.0])).expect("far");
    assert!((ahead.depth - 2.0 * far.depth).abs() < 1e-3);
    assert!(((right.x - 320.0) - 2.0 * (far.x - 320.0)).abs() < 1e-9);
}

#[test]
fn the_near_plane_is_the_nearest_depth_and_nothing_nearer_projects() {
    let view = level();
    let on = view.vertex([0.0, 0.0, 0.25]).expect("on the plane");
    assert_eq!(on.depth, Depth::NEAREST);
    assert!(view.project([0.0, 0.0, 0.249_999]).is_none());
    assert!(view.project([0.0, 0.0, -5.0]).is_none());
    assert!(view.project([f64::NAN, 0.0, 5.0]).is_none());
    let far = view.vertex([0.0, 0.0, 1.0e9]).expect("far away");
    assert!(far.depth > Depth::CLEAR);
}

#[test]
fn directions_need_not_be_unit_or_square() {
    let tidy = level();
    let loose = Perspective::new(
        [0.0, 0.0, 0.0],
        [0.0, 7.0, 0.0],
        [3.0, 1.0, 0.0],
        [0.0, 2.0, -5.0],
        500.0,
        (640, 480),
        0.25,
    )
    .expect("a view");
    for world in [[1.5, 9.0, -2.0], [-4.0, 30.0, 6.0], [0.0, 0.5, 0.0]] {
        let (a, b) = (tidy.view(world), loose.view(world));
        for axis in 0..3 {
            assert!((a[axis] - b[axis]).abs() < 1e-12, "{world:?}");
        }
    }
}

#[test]
fn a_view_that_names_no_screen_is_refused() {
    let eye = [0.0, 0.0, 0.0];
    let (f, r, d) = ([0.0, 1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, -1.0]);
    let screen = (640, 480);
    assert!(Perspective::new(eye, [0.0; 3], r, d, 500.0, screen, 0.25).is_none());
    assert!(Perspective::new(eye, f, [0.0, 3.0, 0.0], d, 500.0, screen, 0.25).is_none());
    assert!(Perspective::new(eye, f, r, [2.0, 5.0, 0.0], 500.0, screen, 0.25).is_none());
    assert!(Perspective::new(eye, f, r, d, 0.0, screen, 0.25).is_none());
    assert!(Perspective::new(eye, f, r, d, 500.0, screen, 0.0).is_none());
    assert!(Perspective::new(eye, f, r, d, f64::NAN, screen, 0.25).is_none());
    assert!(Perspective::new([f64::INFINITY, 0.0, 0.0], f, r, d, 500.0, screen, 0.25).is_none());
    let beyond = GUARD_PIXELS * 4;
    assert!(Perspective::new(eye, f, r, d, 500.0, (beyond, 480), 0.25).is_none());
    assert!(Perspective::new(eye, f, r, d, 500.0, (0, 480), 0.25).is_none());
}

#[test]
fn a_triangle_inside_is_kept_whole() {
    let view = level();
    let triangle = [[0.0, 0.0, 5.0], [1.0, 0.0, 6.0], [0.0, 1.0, 7.0]];
    let out = clipped(&view, triangle);
    assert_eq!(out.len(), 3);
    for (index, corner) in out.iter().enumerate() {
        assert_eq!(
            corner.view.map(f64::to_bits),
            triangle[index].map(f64::to_bits)
        );
        assert!(same(corner.weights[index], 1.0));
        assert!(same(sum(corner.weights), 1.0));
    }
}

#[test]
fn a_triangle_through_the_near_plane_keeps_its_far_part() {
    let view = level();
    let triangle = [[0.0, 0.0, -2.0], [1.0, 0.0, 4.0], [-1.0, 0.5, 4.0]];
    let out = clipped(&view, triangle);
    assert_eq!(out.len(), 4);
    for corner in &out {
        assert!(corner.view[2] >= view.near());
        assert!((sum(corner.weights) - 1.0).abs() < 1e-12);
        let at = rebuilt(triangle, corner.weights);
        for (rebuilt, kept) in at.iter().zip(&corner.view).take(2) {
            assert!((rebuilt - kept).abs() < 1e-9);
        }
        assert_eq!(view.project(corner.view).map(|_| ()), Some(()));
    }
    assert!(out.iter().any(|corner| same(corner.view[2], view.near())));
}

#[test]
fn a_triangle_behind_the_eye_leaves_nothing() {
    let view = level();
    assert!(clipped(&view, [[0.0, 0.0, -1.0], [1.0, 0.0, 0.1], [0.0, 1.0, 0.2]]).is_empty());
    assert!(clipped(
        &view,
        [[f64::NAN, 0.0, 5.0], [1.0, 0.0, 6.0], [0.0, 1.0, 7.0]]
    )
    .is_empty());
}

#[test]
fn an_edge_two_triangles_share_is_cut_at_one_point() {
    let view = level();
    let (a, b) = ([0.3, -0.2, -3.0], [-0.7, 0.4, 9.0]);
    let first = clipped(&view, [a, b, [2.0, 1.0, 5.0]]);
    let second = clipped(&view, [b, a, [-3.0, -2.0, 6.0]]);
    let on_plane = |out: &ArrayVec<Clipped, MAX_CLIPPED>| {
        out.iter()
            .filter(|corner| same(corner.view[2], view.near()))
            .map(|corner| corner.view)
            .collect::<alloc::vec::Vec<_>>()
    };
    let shared = on_plane(&first)
        .into_iter()
        .filter(|point| on_plane(&second).contains(point))
        .count();
    assert_eq!(
        shared, 1,
        "the shared edge's cut is one bit-identical point"
    );
}

#[test]
fn a_corner_far_off_screen_is_clipped_into_the_converters_reach() {
    let view = level();
    // Just past the near plane, a long way to the side: unclipped it would
    // project far beyond the drawing extent.
    let triangle = [[5000.0, 0.0, 0.3], [0.0, 0.0, 5.0], [0.0, 1.0, 5.0]];
    assert!(view.vertex(triangle[0]).is_none());
    let out = clipped(&view, triangle);
    assert!(out.len() >= 3);
    let extent = i64::from(MAX_DRAWING_EXTENT) * i64::from(MESH_SUB_UNITS);
    for corner in &out {
        let vertex = corner.vertex;
        assert!(i64::from(vertex.x).abs() <= extent && i64::from(vertex.y).abs() <= extent);
        let at = rebuilt(triangle, corner.weights);
        assert!((at[2] - corner.view[2]).abs() < 1e-9);
    }
}

/// A corner past the band the clip keeps but inside the converter's extent:
/// were it drawn whole beside a neighbour the clip cuts, the two would meet
/// at different points along their edge.
#[test]
fn a_triangle_is_drawn_whole_only_where_the_clip_would_leave_it_whole() {
    let view = level();
    let (ring, ahead, beside, behind) = (
        [-200.0, 0.0, 0.3],
        [0.0, 0.0, 5.0],
        [0.0, 1.0, 5.0],
        [0.0, 1.0, -1.0],
    );
    assert!(view.project(ring).is_some(), "it projects");
    assert!(view.vertex(ring).is_none(), "but the clip cuts it");
    let corners = |out: &ArrayVec<Clipped, MAX_CLIPPED>| {
        out.iter()
            .map(|corner| (corner.view.map(f64::to_bits), corner.vertex))
            .collect::<alloc::vec::Vec<_>>()
    };
    let first = corners(&clipped(&view, [ring, ahead, beside]));
    let second = corners(&clipped(&view, [ahead, ring, behind]));
    assert!(
        first
            .iter()
            .any(|corner| corner.0 != ahead.map(f64::to_bits) && second.contains(corner)),
        "the two meet where their shared edge is cut"
    );

    let points = [
        [0.0, 0.0, 0.25],
        [0.4, -0.3, 2.0],
        [-200.0, 0.0, 0.3],
        [5000.0, 0.0, 0.3],
        [0.0, 900.0, 1.0],
        [0.0, 0.0, -1.0],
        [3.0, 2.0, 40.0],
    ];
    for a in points {
        for b in points {
            for c in points {
                let out = clipped(&view, [a, b, c]);
                let whole = [a, b, c].iter().all(|&p| view.vertex(p).is_some());
                let untouched = out.len() == 3
                    && out
                        .iter()
                        .zip([a, b, c])
                        .all(|(corner, p)| corner.view.map(f64::to_bits) == p.map(f64::to_bits));
                assert_eq!(whole, untouched, "{a:?} {b:?} {c:?}");
            }
        }
    }
}

/// A side cut toward a corner the near plane cut, from a corner so far that
/// the cut's interpolation rounds onto the near corner: its distance rounds
/// to nothing unless it is held at the near plane.
#[test]
fn a_side_cut_never_rounds_nearer_than_the_near_plane() {
    let view = level();
    let triangle = [[0.0, 0.0, 1e17], [-462.0, 0.0, -1e17], [0.0, 1.0, 1e17]];
    let out = clipped(&view, triangle);
    assert!(out.len() >= 3, "the far part remains");
    for corner in &out {
        assert!(corner.view.iter().all(|value| value.is_finite()));
        assert!(corner.view[2] >= view.near(), "{corner:?}");
        assert!(view.project(corner.view).is_some());
    }
}

#[test]
fn projection_is_reproducible() {
    let view = level();
    let point = [0.123_456_789, 13.0, -0.987_654_321];
    let a = view.vertex(view.view(point)).expect("projects");
    let b = view.vertex(view.view(point)).expect("projects");
    assert_eq!(a, b);
}

#[test]
fn a_box_is_seen_unless_wholly_behind_the_eye_past_its_reach_or_past_an_edge() {
    let view = level();
    let reach = 100.0;
    assert!(
        view.sees([-1.0, 9.0, -1.0], [1.0, 11.0, 1.0], reach),
        "ahead"
    );
    assert!(
        !view.sees([-1.0, -11.0, -1.0], [1.0, -9.0, 1.0], reach),
        "behind"
    );
    assert!(
        !view.sees([-1.0, 120.0, -1.0], [1.0, 130.0, 1.0], reach),
        "past the reach"
    );
    // The screen's edge one unit ahead lies 320/500 of a unit across.
    assert!(
        !view.sees([7.0, 9.0, -1.0], [8.0, 10.0, 1.0], reach),
        "off to the right"
    );
    assert!(
        view.sees([5.0, 9.0, -1.0], [8.0, 10.0, 1.0], reach),
        "straddling the edge"
    );
    assert!(
        !view.sees([-1.0, 9.0, 8.0], [1.0, 10.0, 9.0], reach),
        "above the top"
    );
    // Straddling the eye itself, so in front of and behind it at once.
    assert!(view.sees([-1.0, -1.0, -1.0], [1.0, 1.0, 1.0], reach));
}

/// A tall box behind the eye has corners inside every one of the view's
/// planes, so the planes alone cannot cull it; the view lying wholly to one
/// side of the box does.
#[test]
fn a_tall_box_behind_an_eye_looking_down_is_not_seen() {
    let down = Perspective::new(
        [0.0, 0.0, 40.0],
        [0.0, 0.6, -0.8],
        [1.0, 0.0, 0.0],
        [0.0, -0.8, -0.6],
        200.0,
        (320, 180),
        0.25,
    )
    .expect("a view");
    let behind = ([-10.0, -60.0, -2_000.0], [10.0, -30.0, 2_000.0]);
    assert!(!down.sees(behind.0, behind.1, 3_000.0));
    let ahead = ([-10.0, 30.0, -10.0], [10.0, 60.0, 10.0]);
    assert!(down.sees(ahead.0, ahead.1, 3_000.0));
}

#[test]
fn a_ray_runs_from_the_eye_through_its_screen_point() {
    let view = level();
    let ahead = view.ray(320.0, 240.0);
    assert!((ahead[0]).abs() < 1e-12 && (ahead[1] - 1.0).abs() < 1e-12 && ahead[2].abs() < 1e-12);
    // A point on a ray projects back to where the ray was cast through.
    let ray = view.ray(97.25, 410.5);
    let point = [ray[0] * 30.0, ray[1] * 30.0, ray[2] * 30.0];
    let back = view.project(view.view(point)).expect("ahead of the eye");
    assert!((back.x - 97.25).abs() < 1e-9 && (back.y - 410.5).abs() < 1e-9);
    assert!((ray[0] * ray[0] + ray[1] * ray[1] + ray[2] * ray[2] - 1.0).abs() < 1e-12);
}
