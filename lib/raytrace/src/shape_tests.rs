//! Host tests of where rays meet each shape.

use tairix_util::mathf;

use super::{first_root, reciprocal, Aabb, Face, Geometry, Shape};
use crate::vector::{Frame, Pose, Ray, Vec3};

const FAR: f64 = f64::INFINITY;

fn ray(origin: (f64, f64, f64), toward: (f64, f64, f64)) -> Ray {
    Ray::new(
        Vec3::new(origin.0, origin.1, origin.2),
        Vec3::new(toward.0, toward.1, toward.2).normalized(),
    )
}

fn meet(shape: &Shape, ray: &Ray, faces: &[Face]) -> Option<(f64, Vec3)> {
    shape
        .intersect(
            ray,
            1e-9,
            FAR,
            Geometry {
                faces,
                fields: &[],
                prototypes: &[],
                lawns: &[],
                far_woods: &[],
                stands: &[],
                materials: &[],
                view: None,
            },
        )
        .map(|hit| (hit.t, hit.normal))
}

/// No faces and no grids, for shapes that need neither.
const NOTHING: Geometry<'static> = Geometry {
    faces: &[],
    fields: &[],
    prototypes: &[],
    lawns: &[],
    far_woods: &[],
    stands: &[],
    materials: &[],
    view: None,
};

fn near(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

fn cube_faces(half: f64) -> [Face; 6] {
    [
        Face {
            normal: Vec3::new(1.0, 0.0, 0.0),
            offset: half,
        },
        Face {
            normal: Vec3::new(-1.0, 0.0, 0.0),
            offset: half,
        },
        Face {
            normal: Vec3::UP,
            offset: half,
        },
        Face {
            normal: -Vec3::UP,
            offset: half,
        },
        Face {
            normal: Vec3::new(0.0, 0.0, 1.0),
            offset: half,
        },
        Face {
            normal: Vec3::new(0.0, 0.0, -1.0),
            offset: half,
        },
    ]
}

#[test]
fn a_sphere_is_met_from_outside_and_from_within_and_missed_beside() {
    let ball = Shape::Sphere {
        centre: Vec3::new(0.0, 1.0, 0.0),
        radius: 1.0,
    };
    let (t, normal) = meet(&ball, &ray((0.0, 1.0, -5.0), (0.0, 0.0, 1.0)), &[]).expect("a hit");
    assert!(near(t, 4.0) && (normal - Vec3::new(0.0, 0.0, -1.0)).length() < 1e-9);
    let (t, normal) =
        meet(&ball, &ray((0.0, 1.0, 0.0), (1.0, 0.0, 0.0)), &[]).expect("from within");
    assert!(near(t, 1.0) && (normal - Vec3::new(1.0, 0.0, 0.0)).length() < 1e-9);
    assert!(meet(&ball, &ray((0.0, 2.5, -5.0), (0.0, 0.0, 1.0)), &[]).is_none());
    assert!(meet(&ball, &ray((0.0, 1.0, -5.0), (0.0, 0.0, -1.0)), &[]).is_none());
    // Far away and small, the answer keeps its precision.
    let distant = Shape::Sphere {
        centre: Vec3::new(0.0, 0.0, 1e5),
        radius: 0.5,
    };
    let (t, _) = meet(&distant, &ray((0.0, 0.0, 0.0), (0.0, 0.0, 1.0)), &[]).expect("a hit");
    assert!((t - (1e5 - 0.5)).abs() < 1e-7);
}

#[test]
fn a_plane_is_met_ahead_and_never_behind_or_alongside() {
    let ground = Shape::Plane {
        normal: Vec3::UP,
        offset: 0.0,
    };
    let (t, normal) = meet(&ground, &ray((0.0, 2.0, 0.0), (0.0, -1.0, 1.0)), &[]).expect("a hit");
    assert!(near(t, 2.0 * mathf::sqrt(2.0)) && (normal - Vec3::UP).length() < 1e-12);
    assert!(meet(&ground, &ray((0.0, 2.0, 0.0), (0.0, 1.0, 1.0)), &[]).is_none());
    assert!(meet(&ground, &ray((0.0, 2.0, 0.0), (1.0, 0.0, 0.0)), &[]).is_none());
    assert!(ground.bounds(NOTHING).is_none());
}

#[test]
fn a_quad_is_met_within_its_edges_alone() {
    let quad = Shape::Quad {
        corner: Vec3::new(-1.0, 0.0, 2.0),
        edge_u: Vec3::new(2.0, 0.0, 0.0),
        edge_v: Vec3::new(0.0, 1.0, 0.0),
    };
    let (t, normal) = meet(&quad, &ray((0.0, 0.5, 0.0), (0.0, 0.0, 1.0)), &[]).expect("a hit");
    assert!(near(t, 2.0));
    assert!(
        (normal - Vec3::new(0.0, 0.0, 1.0)).length() < 1e-9,
        "{normal:?}"
    );
    assert!(meet(&quad, &ray((1.5, 0.5, 0.0), (0.0, 0.0, 1.0)), &[]).is_none());
    assert!(meet(&quad, &ray((0.0, 1.5, 0.0), (0.0, 0.0, 1.0)), &[]).is_none());
}

#[test]
fn a_hull_meets_rays_as_the_box_it_bounds() {
    let faces = cube_faces(1.0);
    let hull = Shape::Hull {
        pose: Pose::new(Vec3::new(0.0, 1.0, 0.0), Frame::WORLD),
        first: 0,
        count: 6,
        extent: Aabb::around(Vec3::ZERO, 1.0),
    };
    let (t, normal) = meet(&hull, &ray((0.0, 1.0, -4.0), (0.0, 0.0, 1.0)), &faces).expect("a hit");
    assert!(near(t, 3.0) && (normal - Vec3::new(0.0, 0.0, -1.0)).length() < 1e-9);
    let (t, normal) =
        meet(&hull, &ray((0.0, 1.0, 0.0), (0.0, 1.0, 0.0)), &faces).expect("from within");
    assert!(near(t, 1.0) && (normal - Vec3::UP).length() < 1e-12);
    assert!(meet(&hull, &ray((0.0, 3.5, -4.0), (0.0, 0.0, 1.0)), &faces).is_none());
    // Turned, it is met where the turned box is.
    let turned = Shape::Hull {
        pose: Pose::new(Vec3::ZERO, Frame::turned(core::f64::consts::FRAC_PI_4, 0.0)),
        first: 0,
        count: 6,
        extent: Aabb::around(Vec3::ZERO, 1.0),
    };
    let (t, _) = meet(&turned, &ray((-4.0, 0.0, 0.0), (1.0, 0.0, 0.0)), &faces).expect("a hit");
    assert!(near(t, 4.0 - mathf::sqrt(2.0)));
    // A face range outside the scene's faces meets nothing.
    let stray = Shape::Hull {
        pose: Pose::new(Vec3::ZERO, Frame::WORLD),
        first: 4,
        count: 6,
        extent: Aabb::around(Vec3::ZERO, 1.0),
    };
    assert!(meet(&stray, &ray((0.0, 0.0, -4.0), (0.0, 0.0, 1.0)), &faces).is_none());
}

#[test]
fn a_frustum_is_met_on_its_side_and_on_its_caps() {
    let cylinder = Shape::Frustum {
        pose: Pose::new(Vec3::ZERO, Frame::WORLD),
        bottom: 0.5,
        top: 0.5,
        height: 2.0,
    };
    let (t, normal) = meet(&cylinder, &ray((-3.0, 1.0, 0.0), (1.0, 0.0, 0.0)), &[]).expect("side");
    assert!(near(t, 2.5) && (normal - Vec3::new(-1.0, 0.0, 0.0)).length() < 1e-9);
    let (t, normal) = meet(&cylinder, &ray((0.1, 5.0, 0.0), (0.0, -1.0, 0.0)), &[]).expect("top");
    assert!(near(t, 3.0) && (normal - Vec3::UP).length() < 1e-12);
    let (t, normal) =
        meet(&cylinder, &ray((0.0, 1.0, 0.0), (0.0, -1.0, 0.0)), &[]).expect("within");
    assert!(near(t, 1.0) && (normal + Vec3::UP).length() < 1e-12);
    assert!(meet(&cylinder, &ray((-3.0, 2.5, 0.0), (1.0, 0.0, 0.0)), &[]).is_none());
    // A cone narrowing to a point: its side slopes, and so does its normal.
    let cone = Shape::Frustum {
        pose: Pose::new(Vec3::ZERO, Frame::WORLD),
        bottom: 1.0,
        top: 0.0,
        height: 1.0,
    };
    let (t, normal) = meet(&cone, &ray((-3.0, 0.5, 0.0), (1.0, 0.0, 0.0)), &[]).expect("side");
    assert!(near(t, 2.5));
    let slope = Vec3::new(-1.0, 1.0, 0.0).normalized();
    assert!((normal - slope).length() < 1e-9, "{normal:?}");
}

/// The torus's implicit equation at `p` in its own frame: negative inside.
fn torus_field(p: Vec3, major: f64, minor: f64) -> f64 {
    let ring = mathf::sqrt(p.x * p.x + p.z * p.z) - major;
    ring * ring + p.y * p.y - minor * minor
}

/// The first place along `ray` the torus's field changes sign, found by
/// marching finely and bisecting; `None` if it never does.
fn torus_reference(ray: &Ray, major: f64, minor: f64) -> Option<f64> {
    let steps = 40_000;
    let reach = 20.0;
    let mut last = torus_field(ray.origin, major, minor);
    for step in 1..=steps {
        let t = reach * f64::from(step) / f64::from(steps);
        let now = torus_field(ray.at(t), major, minor);
        if (last < 0.0) != (now < 0.0) {
            let (mut low, mut high) = (reach * f64::from(step - 1) / f64::from(steps), t);
            for _ in 0..80 {
                let middle = low.midpoint(high);
                if (torus_field(ray.at(middle), major, minor) < 0.0) == (last < 0.0) {
                    low = middle;
                } else {
                    high = middle;
                }
            }
            return Some(low.midpoint(high));
        }
        last = now;
    }
    None
}

#[test]
fn a_torus_is_met_where_its_equation_says_along_the_axes() {
    let ring = Shape::Torus {
        pose: Pose::new(Vec3::ZERO, Frame::WORLD),
        major: 2.0,
        minor: 0.5,
        arc: -1.0,
    };
    let (t, normal) = meet(&ring, &ray((-5.0, 0.0, 0.0), (1.0, 0.0, 0.0)), &[]).expect("outer");
    assert!((t - 2.5).abs() < 1e-9 && (normal - Vec3::new(-1.0, 0.0, 0.0)).length() < 1e-9);
    let (t, normal) = meet(&ring, &ray((0.0, 0.0, 0.0), (1.0, 0.0, 0.0)), &[]).expect("inner");
    assert!((t - 1.5).abs() < 1e-9 && (normal - Vec3::new(-1.0, 0.0, 0.0)).length() < 1e-9);
    let (t, normal) = meet(&ring, &ray((2.0, 3.0, 0.0), (0.0, -1.0, 0.0)), &[]).expect("tube top");
    assert!((t - 2.5).abs() < 1e-9 && (normal - Vec3::UP).length() < 1e-9);
    // Straight down the hole, nothing.
    assert!(meet(&ring, &ray((0.0, 3.0, 0.0), (0.0, -1.0, 0.0)), &[]).is_none());
}

/// Rays from everywhere at the torus in every direction agree with a fine
/// march of its equation, on whether they meet it and where.
#[test]
fn a_torus_agrees_with_its_marched_equation_for_rays_of_every_kind() {
    let (major, minor) = (1.6, 0.45);
    let ring = Shape::Torus {
        pose: Pose::new(Vec3::ZERO, Frame::WORLD),
        major,
        minor,
        arc: -1.0,
    };
    let mut hits = 0;
    let mut state = 0x9e37_79b9_u32;
    let mut draw = || {
        state = crate::sample::mix32(state.wrapping_add(0x6d2b_79f5));
        crate::sample::unit(state) * 2.0 - 1.0
    };
    for _ in 0..600 {
        let origin = Vec3::new(draw() * 4.0, draw() * 2.0, draw() * 4.0);
        let aim = Vec3::new(draw() * major, draw() * minor, draw() * major);
        let probe = Ray::new(origin, (aim - origin).normalized());
        let found = meet(&ring, &probe, &[]).map(|(t, _)| t);
        match torus_reference(&probe, major, minor) {
            Some(expected) => {
                let t = found.expect("the march found a crossing");
                assert!((t - expected).abs() < 1e-6, "{t} against {expected}");
                hits += 1;
            }
            None => {
                // Anything reported must lie on the surface: a graze the march
                // stepped over.
                if let Some(t) = found {
                    assert!(torus_field(probe.at(t), major, minor).abs() < 1e-6);
                }
            }
        }
    }
    assert!(hits > 200, "only {hits} rays met the torus");
}

#[test]
fn a_turned_torus_is_met_in_its_own_frame() {
    let upright = Frame::turned(0.0, core::f64::consts::FRAC_PI_2);
    let ring = Shape::Torus {
        pose: Pose::new(Vec3::new(0.0, 2.0, 0.0), upright),
        major: 1.5,
        minor: 0.4,
        arc: -1.0,
    };
    // Its axis is now level, so a level ray along it passes through the hole.
    let axis = upright.y;
    let along = Ray::new(Vec3::new(0.0, 2.0, 0.0) - axis * 5.0, axis);
    assert!(meet(&ring, &along, &[]).is_none());
    let (t, _) = meet(&ring, &ray((0.0, 6.0, 0.0), (0.0, -1.0, 0.0)), &[]).expect("the top");
    assert!((t - (4.0 - 1.9)).abs() < 1e-9, "{t}");
}

#[test]
fn the_quartic_solver_finds_the_first_root_in_its_interval() {
    // (u - 1)(u - 2)(u - 3)(u - 4)
    let coefficients = [-10.0, 35.0, -50.0, 24.0];
    assert!((first_root(coefficients, 0.0, 10.0).expect("a root") - 1.0).abs() < 1e-12);
    assert!((first_root(coefficients, 1.5, 10.0).expect("a root") - 2.0).abs() < 1e-12);
    assert!(first_root(coefficients, 3.2, 3.9).is_none());
    assert!(first_root(coefficients, 4.5, 10.0).is_none());
    // (u² + 1)(u² + 4): no real roots at all.
    assert!(first_root([0.0, 5.0, 0.0, 4.0], -10.0, 10.0).is_none());
    // A double root is touched, not crossed: whether the solver stops there
    // or goes on to the crossing after it, what it answers is a root.
    // (u - 1)²(u - 3)(u - 5)
    let touching = [-10.0, 32.0, -38.0, 15.0];
    let root = first_root(touching, 0.0, 10.0).expect("a root");
    assert!(
        (root - 1.0).abs() < 1e-6 || (root - 3.0).abs() < 1e-9,
        "{root}"
    );
}

#[test]
fn every_bounded_shape_lies_within_its_box() {
    let faces = cube_faces(0.7);
    let pose = Pose::new(Vec3::new(1.0, 0.5, -2.0), Frame::turned(0.6, 0.9));
    let shapes = [
        Shape::Sphere {
            centre: Vec3::new(3.0, 1.0, 1.0),
            radius: 0.8,
        },
        Shape::Hull {
            pose,
            first: 0,
            count: 6,
            extent: Aabb::around(Vec3::ZERO, 0.7),
        },
        Shape::Frustum {
            pose,
            bottom: 0.6,
            top: 0.2,
            height: 1.5,
        },
        Shape::Torus {
            pose,
            major: 1.0,
            minor: 0.3,
            arc: -1.0,
        },
        Shape::Quad {
            corner: Vec3::new(0.0, 1.0, 0.0),
            edge_u: Vec3::new(1.0, 0.0, 1.0),
            edge_v: Vec3::new(-0.5, 1.0, 0.5),
        },
    ];
    let mut state = 17u32;
    let mut draw = || {
        state = crate::sample::mix32(state.wrapping_add(0x6d2b_79f5));
        crate::sample::unit(state) * 2.0 - 1.0
    };
    for shape in &shapes {
        let bounds = shape
            .bounds(Geometry {
                faces: &faces,
                fields: &[],
                prototypes: &[],
                lawns: &[],
                far_woods: &[],
                stands: &[],
                materials: &[],
                view: None,
            })
            .expect("bounded");
        for _ in 0..400 {
            let origin = Vec3::new(draw() * 8.0, draw() * 8.0, draw() * 8.0);
            let target = bounds.centre() + Vec3::new(draw(), draw(), draw());
            let probe = Ray::new(origin, (target - origin).normalized());
            if let Some((t, _)) = meet(shape, &probe, &faces) {
                let point = probe.at(t);
                let within = |low: f64, at: f64, high: f64| at >= low - 1e-7 && at <= high + 1e-7;
                assert!(
                    within(bounds.min.x, point.x, bounds.max.x)
                        && within(bounds.min.y, point.y, bounds.max.y)
                        && within(bounds.min.z, point.z, bounds.max.z),
                    "{shape:?}: {point:?} outside {bounds:?}"
                );
            }
        }
    }
}

#[test]
fn a_box_is_entered_where_the_slabs_say() {
    let bounds = Aabb {
        min: Vec3::new(-1.0, -1.0, -1.0),
        max: Vec3::new(1.0, 1.0, 1.0),
    };
    let inverse = |ray: &Ray| reciprocal(ray.dir);
    let straight = ray((0.0, 0.0, -5.0), (0.0, 0.0, 1.0));
    assert!(near(
        bounds
            .entry(&straight, inverse(&straight), FAR)
            .expect("entered"),
        4.0
    ));
    assert!(bounds.entry(&straight, inverse(&straight), 3.0).is_none());
    let inside = ray((0.2, 0.1, 0.0), (0.3, -0.2, 1.0));
    assert!(near(
        bounds
            .entry(&inside, inverse(&inside), FAR)
            .expect("within"),
        0.0
    ));
    // Exactly on a face and parallel to it is a graze of measure nought; the
    // hierarchy's padded boxes still let it in.
    let grazing = ray((1.0, 0.0, -5.0), (0.0, 0.0, 1.0));
    assert!(bounds
        .padded()
        .entry(&grazing, inverse(&grazing), FAR)
        .is_some());
    let outside = ray((1.000_001, 0.0, -5.0), (0.0, 0.0, 1.0));
    assert!(bounds.entry(&outside, inverse(&outside), FAR).is_none());
    let away = ray((0.0, 0.0, -5.0), (0.0, 0.0, -1.0));
    assert!(bounds.entry(&away, inverse(&away), FAR).is_none());
    assert!((bounds.half_area() - 12.0).abs() < 1e-12);
    assert_eq!(Aabb::EMPTY.union(bounds), bounds);
}

/// An arch is the ring's upper half alone: met over its crown, passed
/// beneath it where the lower half would have stood.
#[test]
fn an_arch_keeps_the_half_of_its_ring_its_arc_allows() {
    // Stood on end, the ring's own -z is the world's up.
    let upright = Frame::turned(0.0, core::f64::consts::FRAC_PI_2);
    let arch = Shape::Torus {
        pose: Pose::new(Vec3::ZERO, upright),
        major: 2.0,
        minor: 0.3,
        arc: 0.0,
    };
    let over = meet(&arch, &ray((0.0, 5.0, 0.0), (0.0, -1.0, 0.0)), &[]).expect("the crown");
    assert!(near(over.0, 5.0 - 2.3), "{over:?}");
    assert!(meet(&arch, &ray((0.0, -5.0, 0.0), (0.0, 1.0, 0.0)), &[])
        .is_some_and(|(t, _)| near(t, 5.0 + 1.7)));
    let under = ray((-5.0, -2.0, 0.0), (1.0, 0.0, 0.0));
    assert!(meet(&arch, &under, &[]).is_none(), "the lower half is gone");
    let whole = Shape::Torus {
        pose: Pose::new(Vec3::ZERO, upright),
        major: 2.0,
        minor: 0.3,
        arc: -1.0,
    };
    assert!(meet(&whole, &ray((0.0, -5.0, 0.0), (0.0, 1.0, 0.0)), &[])
        .is_some_and(|(t, _)| near(t, 5.0 - 2.3)));
}

#[test]
fn a_box_is_crossed_between_its_entry_and_its_exit() {
    let bounds = Aabb {
        min: Vec3::new(-1.0, -1.0, -1.0),
        max: Vec3::new(1.0, 1.0, 1.0),
    };
    let straight = ray((0.0, 0.0, -5.0), (0.0, 0.0, 1.0));
    let (enter, leave) = bounds
        .span(&straight, reciprocal(straight.dir), FAR)
        .expect("crossed");
    assert!(near(enter, 4.0) && near(leave, 6.0));
    let (_, cut) = bounds
        .span(&straight, reciprocal(straight.dir), 5.0)
        .expect("crossed");
    assert!(near(cut, 5.0), "no further than the reach");
    let inside = ray((0.0, 0.0, 0.0), (1.0, 0.0, 0.0));
    let (enter, leave) = bounds
        .span(&inside, reciprocal(inside.dir), FAR)
        .expect("within");
    assert!(near(enter, 0.0) && near(leave, 1.0));
}

#[test]
fn a_dome_is_met_outside_and_in_but_not_below_its_rim() {
    let dome = Shape::Dome {
        centre: Vec3::ZERO,
        radius: 2.0,
    };
    let from_above = meet(&dome, &ray((0.0, 5.0, 0.0), (0.0, -1.0, 0.0)), &[]).expect("its top");
    assert!(near(from_above.0, 3.0) && (from_above.1 - Vec3::UP).length() < 1e-9);
    let from_below = meet(&dome, &ray((0.0, -5.0, 0.0), (0.0, 1.0, 0.0)), &[]).expect("its inside");
    assert!(near(from_below.0, 7.0));
    assert!(
        meet(&dome, &ray((-5.0, -0.5, 0.0), (1.0, 0.0, 0.0)), &[]).is_none(),
        "open beneath"
    );
    let bounds = dome.bounds(NOTHING).expect("bounded");
    assert!(near(bounds.min.y, 0.0) && near(bounds.max.y, 2.0));
}
