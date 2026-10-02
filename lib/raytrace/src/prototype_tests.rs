//! Host tests of prototypes: each part met where it lies, the nearest first,
//! and a placed prototype met where its placing puts it.

use alloc::vec;

use super::*;
use crate::shape::{Geometry, Shape};
use crate::vector::{Frame, Pose};

fn tube(a: Vec3, b: Vec3, radii: (f64, f64)) -> Part {
    Part::Tube(Tube::new(
        (a, b),
        (radii, (0.0, (b - a).length())),
        (3, 9),
        Vec3::new(1.0, 0.0, 0.0),
    ))
}

#[test]
fn a_limb_is_met_on_its_body_and_its_rounded_ends() {
    let limb = Prototype::new(
        vec![tube(Vec3::ZERO, Vec3::UP * 2.0, (0.5, 0.25))],
        vec![],
        vec![],
    )
    .expect("a limb");
    let side = Ray::new(Vec3::new(-5.0, 1.0, 0.0), Vec3::new(1.0, 0.0, 0.0));
    let hit = limb.intersect(&side, 1e-9, f64::INFINITY).expect("met");
    // The radius halfway up a taper from 0.5 to 0.25, less the slope's lean.
    assert!((hit.t - (5.0 - 0.375)).abs() < 0.02, "{}", hit.t);
    assert!(hit.normal.x < -0.9 && hit.material == Some(3) && hit.mark == 9);
    assert!((hit.tangent - Vec3::UP).length() < 1e-6);
    let top = Ray::new(Vec3::new(0.0, 5.0, 0.0), -Vec3::UP);
    let cap = limb
        .intersect(&top, 1e-9, f64::INFINITY)
        .expect("the top end");
    assert!((cap.t - (5.0 - 2.25)).abs() < 1e-3, "{}", cap.t);
    let beside = Ray::new(Vec3::new(-5.0, 1.0, 0.6), Vec3::new(1.0, 0.0, 0.0));
    assert!(limb.intersect(&beside, 1e-9, f64::INFINITY).is_none());
}

#[test]
fn a_bent_chain_of_limbs_leaves_no_gap_at_its_joint() {
    let bend = Vec3::new(0.0, 1.0, 0.0);
    let chain = Prototype::new(
        vec![
            tube(Vec3::ZERO, bend, (0.2, 0.2)),
            tube(bend, bend + Vec3::new(0.7, 0.7, 0.0), (0.2, 0.2)),
        ],
        vec![],
        vec![],
    )
    .expect("a chain");
    // Straight at the outside of the bend, where two frusta would part.
    let outside = Ray::new(Vec3::new(-3.0, 1.2, 0.0), Vec3::new(1.0, 0.0, 0.0));
    let hit = chain
        .intersect(&outside, 1e-9, f64::INFINITY)
        .expect("the joint is closed");
    assert!(hit.t < 3.0);
    assert!(chain.occludes(&outside, 1e-9, f64::INFINITY));
}

#[test]
fn a_leaf_is_met_within_its_outline_and_missed_outside_it() {
    let leaf = Prototype::new(
        vec![Part::Leaf(Blade {
            base: stored(Vec3::ZERO),
            normal: stored(Vec3::UP),
            axis: stored(Vec3::new(1.0, 0.0, 0.0)),
            length: 0.1,
            width: 0.04,
            outline: Outline::Ovate { teeth: 0 },
            fold: 0.3,
            material: 5,
            key: 1,
        })],
        vec![],
        vec![],
    )
    .expect("a leaf");
    let down = |x: f64, z: f64| Ray::new(Vec3::new(x, 1.0, z), -Vec3::UP);
    let hit = leaf
        .intersect(&down(0.05, 0.005), 1e-9, 2.0)
        .expect("on the blade");
    assert!((hit.t - 1.0).abs() < 1e-6 && hit.material == Some(5));
    assert!((hit.uv.0 - 0.5).abs() < 1e-3);
    assert!(
        hit.shading.dot(Vec3::UP) > 0.8 && hit.shading.dot(Vec3::UP) < 1.0,
        "folded"
    );
    assert!(
        leaf.intersect(&down(0.05, 0.039), 1e-9, 2.0).is_none()
            || leaf.intersect(&down(0.099, 0.03), 1e-9, 2.0).is_none()
    );
    assert!(
        leaf.intersect(&down(0.12, 0.0), 1e-9, 2.0).is_none(),
        "past its tip"
    );
}

#[test]
fn a_facet_is_shaded_by_its_corners_normals_blended() {
    let rock = Prototype::new(
        vec![Part::Facet(Facet {
            corners: [0, 1, 2],
            material: 2,
        })],
        vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]],
        vec![[0.0, 1.0, 0.0], [0.6, 0.8, 0.0], [0.0, 0.8, 0.6]],
    )
    .expect("a facet");
    let hit = rock
        .intersect(&Ray::new(Vec3::new(0.25, 2.0, 0.25), -Vec3::UP), 1e-9, 10.0)
        .expect("on the facet");
    assert!((hit.t - 2.0).abs() < 1e-9);
    assert!(hit.normal.y.abs() > 0.999);
    assert!(
        hit.shading.x > 0.0 && hit.shading.z > 0.0,
        "{:?}",
        hit.shading
    );
    assert!(
        Prototype::new(
            vec![Part::Facet(Facet {
                corners: [0, 1, 7],
                material: 0
            })],
            vec![[0.0; 3]; 3],
            vec![[0.0, 1.0, 0.0]; 3]
        )
        .is_none(),
        "a facet naming a vertex it lacks is refused"
    );
}

#[test]
fn a_placed_prototype_is_met_where_its_placing_puts_it() {
    let prototypes =
        vec![
            Prototype::new(vec![tube(Vec3::ZERO, Vec3::UP, (0.1, 0.1))], vec![], vec![])
                .expect("a post"),
        ];
    let geometry = Geometry {
        faces: &[],
        fields: &[],
        prototypes: &prototypes,
        lawns: &[],
    };
    let placed = Shape::Instance {
        prototype: 0,
        pose: Pose::new(Vec3::new(10.0, 0.0, 0.0), Frame::turned(0.3, 0.0)),
        scale: 3.0,
        key: 0x55,
    };
    let bounds = placed.bounds(geometry).expect("bounded");
    assert!(bounds.max.y > 3.0 && bounds.min.x > 9.0);
    let ray = Ray::new(Vec3::new(0.0, 1.5, 0.0), Vec3::new(1.0, 0.0, 0.0));
    let hit = placed
        .intersect(&ray, 1e-9, f64::INFINITY, geometry)
        .expect("met");
    assert!((hit.t - (10.0 - 0.3)).abs() < 0.01, "{}", hit.t);
    assert_eq!(hit.mark, 9 ^ 0x55);
    assert!(placed.occludes(&ray, 1e-9, f64::INFINITY, geometry));
    assert!(
        !placed.occludes(&ray, 1e-9, 9.0, geometry),
        "not before its reach"
    );
}

#[test]
fn a_scaled_limb_is_met_at_its_placed_girth_and_its_placed_way_along_its_stem() {
    let prototypes =
        vec![
            Prototype::new(vec![tube(Vec3::ZERO, Vec3::UP, (0.1, 0.1))], vec![], vec![])
                .expect("a post"),
        ];
    let geometry = Geometry {
        faces: &[],
        fields: &[],
        prototypes: &prototypes,
        lawns: &[],
    };
    let placed = |scale: f64| Shape::Instance {
        prototype: 0,
        pose: Pose::new(Vec3::new(10.0, 0.0, 0.0), Frame::WORLD),
        scale,
        key: 0,
    };
    for scale in [0.5, 1.0, 3.0] {
        // Half way up the post as it stands, whatever its scale.
        let ray = Ray::new(Vec3::new(0.0, 0.5 * scale, 0.0), Vec3::new(1.0, 0.0, 0.0));
        let hit = placed(scale)
            .intersect(&ray, 1e-9, f64::INFINITY, geometry)
            .expect("met");
        assert!(
            (hit.girth - 0.1 * scale).abs() < 1e-6,
            "{scale}: {}",
            hit.girth
        );
        assert!(
            (hit.uv.0 - 0.5 * scale).abs() < 1e-6,
            "{scale}: {}",
            hit.uv.0
        );
    }
}

#[test]
fn a_bending_limbs_bark_starts_round_it_alike_either_side_of_a_joint() {
    // Two segments of one limb, either side of where the world's own start
    // for the angle round a limb changes axis, both begun from the side
    // their stem carries: square to the plane they bend in.
    let side = Vec3::new(0.0, 0.0, 1.0);
    let joint = Vec3::new(0.88, mathf::sqrt(1.0 - 0.88 * 0.88), 0.0);
    let beyond = joint + Vec3::new(0.92, mathf::sqrt(1.0 - 0.92 * 0.92), 0.0);
    let segments = [(Vec3::ZERO, joint), (joint, beyond)]
        .map(|(a, b)| Tube::new((a, b), ((0.1, 0.1), (0.0, 1.0)), (0, 0), side));
    let middle = ((beyond - joint).normalized() + joint.normalized()).normalized();
    let across = side.cross(middle);
    for step in 0..12u32 {
        let around = f64::from(step) * core::f64::consts::TAU / 12.0;
        let normal = side * mathf::cos(around) + across * mathf::sin(around);
        let [first, second] = segments.map(|tube| limb_hit(1.0, normal, (1.0, 0.5), &tube).uv.1);
        let apart = (first - second).abs();
        let apart = apart.min(core::f64::consts::TAU - apart);
        assert!(apart < 0.03, "{step}: {first} against {second}");
    }
}
