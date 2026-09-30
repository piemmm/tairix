//! Host tests of the tracer's vector arithmetic and frames.

use core::f64::consts::FRAC_PI_2;

use tairix_util::mathf;

use super::{Frame, Pose, Ray, Vec3};

fn close(a: Vec3, b: Vec3) -> bool {
    (a - b).length() < 1e-9
}

fn orthonormal(frame: &Frame) -> bool {
    let unit = |v: Vec3| (v.length() - 1.0).abs() < 1e-9;
    unit(frame.x)
        && unit(frame.y)
        && unit(frame.z)
        && frame.x.dot(frame.y).abs() < 1e-9
        && frame.y.dot(frame.z).abs() < 1e-9
        && frame.z.dot(frame.x).abs() < 1e-9
}

/// Directions spread over the whole sphere, including the poles the
/// branch-free basis is most delicate at.
fn directions() -> impl Iterator<Item = Vec3> {
    let poles = [
        Vec3::new(0.0, 0.0, 1.0),
        Vec3::new(0.0, 0.0, -1.0),
        Vec3::new(0.0, 0.0, -0.999_999_9).normalized() + Vec3::new(1e-4, 0.0, 0.0),
        Vec3::UP,
        -Vec3::UP,
    ];
    let spread = (0..200u32).map(|i| {
        let t = f64::from(i) / 200.0;
        let z = 1.0 - 2.0 * t;
        let r = mathf::sqrt((1.0 - z * z).max(0.0));
        let a = 2.399_963 * f64::from(i);
        Vec3::new(r * mathf::cos(a), r * mathf::sin(a), z)
    });
    poles.into_iter().map(Vec3::normalized).chain(spread)
}

#[test]
fn the_products_are_the_textbook_ones() {
    let (a, b) = (Vec3::new(1.0, 2.0, 3.0), Vec3::new(-4.0, 0.5, 2.0));
    assert!((a.dot(b) - 3.0).abs() < 1e-12);
    let c = a.cross(b);
    assert!(close(c, Vec3::new(2.5, -14.0, 8.5)));
    assert!(c.dot(a).abs() < 1e-12 && c.dot(b).abs() < 1e-12);
    assert!(close(a * b, Vec3::new(-4.0, 1.0, 6.0)));
}

#[test]
fn normalising_keeps_direction_and_leaves_zero_alone() {
    let v = Vec3::new(3.0, -4.0, 12.0).normalized();
    assert!((v.length() - 1.0).abs() < 1e-12);
    assert!(close(v * 13.0, Vec3::new(3.0, -4.0, 12.0)));
    assert!(close(Vec3::ZERO.normalized(), Vec3::ZERO));
}

#[test]
fn a_reflection_mirrors_the_normal_part_and_keeps_the_rest() {
    let normal = Vec3::UP;
    let incoming = Vec3::new(0.6, -0.8, 0.0);
    let out = incoming.reflect(normal);
    assert!(close(out, Vec3::new(0.6, 0.8, 0.0)));
    assert!((out.length() - 1.0).abs() < 1e-12);
}

#[test]
fn a_basis_around_any_normal_is_orthonormal_with_that_normal_as_z() {
    for normal in directions() {
        let frame = Frame::around(normal);
        assert!(orthonormal(&frame), "{normal:?}");
        assert!(close(frame.z, normal));
        let local = Vec3::new(0.3, -0.5, 0.81);
        assert!(close(frame.to_local(frame.to_world(local)), local));
    }
}

#[test]
fn a_turned_basis_stays_orthonormal_and_level_when_untilted() {
    for step in 0..24u32 {
        let yaw = f64::from(step) * 0.29;
        let level = Frame::turned(yaw, 0.0);
        assert!(orthonormal(&level));
        assert!(close(level.y, Vec3::UP));
        let tilted = Frame::turned(yaw, 0.4);
        assert!(orthonormal(&tilted));
        assert!((tilted.y.dot(Vec3::UP) - mathf::cos(0.4)).abs() < 1e-12);
    }
    assert!(close(Frame::turned(0.0, 0.0).x, Frame::WORLD.x));
    assert!(close(Frame::turned(0.0, FRAC_PI_2).y, Frame::WORLD.z));
}

#[test]
fn aligning_carries_one_direction_onto_another_rigidly() {
    for from in directions().step_by(7) {
        for to in directions().step_by(11) {
            let turned = Frame::WORLD.aligning(from, to);
            assert!(orthonormal(&turned), "{from:?} -> {to:?}");
            assert!(close(turned.to_world(from), to), "{from:?} -> {to:?}");
            // A rotation, never a reflection.
            assert!(turned.x.cross(turned.y).dot(turned.z) > 0.999);
        }
    }
    let antipodal = Frame::WORLD.aligning(Vec3::UP, -Vec3::UP);
    assert!(orthonormal(&antipodal));
    assert!(close(antipodal.to_world(Vec3::UP), -Vec3::UP));
}

#[test]
fn rotating_a_basis_composes_the_two_turns() {
    let first = Frame::turned(0.7, 0.3);
    let second = Frame::turned(-1.1, 0.5);
    let both = first.rotated_by(second);
    assert!(orthonormal(&both));
    let v = Vec3::new(0.2, 0.9, -0.4);
    assert!(close(both.to_world(v), second.to_world(first.to_world(v))));
}

#[test]
fn a_pose_takes_rays_into_its_frame_and_back() {
    let pose = Pose::new(Vec3::new(1.0, 2.0, -3.0), Frame::turned(0.8, -0.3));
    let ray = Ray::new(
        Vec3::new(4.0, 0.5, 2.0),
        Vec3::new(-0.3, 0.2, -0.9).normalized(),
    );
    let local = pose.ray_to_local(&ray);
    assert!((local.dir.length() - 1.0).abs() < 1e-12);
    for t in [0.0, 1.5, 7.25] {
        let world = ray.at(t);
        let back = pose.at + pose.frame.to_world(local.at(t));
        assert!(close(world, back), "t = {t}");
        assert!(close(pose.point_to_local(world), local.at(t)));
    }
}

#[test]
fn exp_is_componentwise_and_finiteness_is_every_component() {
    let v = Vec3::new(0.0, -1.0, 2.0).exp();
    assert!(close(v, Vec3::new(1.0, mathf::exp(-1.0), mathf::exp(2.0))));
    assert!(Vec3::ONE.is_finite());
    assert!(!Vec3::new(1.0, f64::INFINITY, 0.0).is_finite());
    assert!(!Vec3::new(f64::NAN, 0.0, 0.0).is_finite());
    assert!((Vec3::new(1.0, 5.0, 3.0).max_element() - 5.0).abs() < 1e-12);
    assert!((Vec3::new(1.0, 5.0, 3.0).along(2) - 3.0).abs() < 1e-12);
}
