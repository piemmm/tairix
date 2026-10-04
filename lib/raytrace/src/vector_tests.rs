//! Host tests of the tracer's rays, and the scalars it counts, narrows and
//! shapes with.

use super::{byte, power, real, tanh, whole, Frame, Pose, Ray, Vec3};

fn close(a: Vec3, b: Vec3) -> bool {
    (a - b).length() < 1e-9
}

#[test]
fn a_pose_takes_rays_into_its_frame_and_back() {
    let pose = Pose::new(Vec3::new(1.0, 2.0, -3.0), Frame::turned(0.8, -0.3));
    let ray = Ray::new(
        Vec3::new(4.0, 0.5, 2.0),
        Vec3::new(-0.3, 0.2, -0.9).normalized(),
    );
    let local = ray.to_local(&pose);
    assert!((local.dir.length() - 1.0).abs() < 1e-12);
    for t in [0.0, 1.5, 7.25] {
        let world = ray.at(t);
        assert!(close(world, pose.point_to_world(local.at(t))), "t = {t}");
        assert!(close(pose.point_to_local(world), local.at(t)));
    }
}

#[test]
fn a_count_is_its_real_value_and_saturates_past_what_a_scene_holds() {
    assert!((real(0) - 0.0).abs() < f64::EPSILON);
    assert!((real(12_345) - 12_345.0).abs() < f64::EPSILON);
    assert!((real(usize::MAX) - f64::from(u32::MAX)).abs() < f64::EPSILON);
}

/// A real rounds to the nearest count, nought for one below nought or none.
#[test]
fn a_real_rounds_to_its_nearest_count() {
    assert_eq!(whole(2.4), 2);
    assert_eq!(whole(2.6), 3);
    assert_eq!(whole(-3.0), 0);
    assert_eq!(whole(f64::NAN), 0);
}

/// A share is a byte rounded to the nearest, held to the byte's range.
#[test]
fn a_share_narrows_to_a_byte() {
    assert_eq!(byte(0.0), 0);
    assert_eq!(byte(0.5), 128);
    assert_eq!(byte(1.0), 255);
    assert_eq!((byte(-2.0), byte(7.0)), (0, 255));
}

/// A power of a positive number is its power, and of none or less nought.
#[test]
fn a_positive_number_takes_its_power() {
    assert!((power(8.0, 2.0 / 3.0) - 4.0).abs() < 1e-12);
    assert!((power(0.25, 0.5) - 0.5).abs() < 1e-12);
    assert!(power(0.0, 1.5).abs() < f64::EPSILON && power(-2.0, 0.5).abs() < f64::EPSILON);
}

/// The hyperbolic tangent is odd and bounded however far out it is asked
/// for: no overflow to `NaN` far below nought.
#[test]
fn the_hyperbolic_tangent_is_odd_and_bounded() {
    for x in [0.0, 1e-3, 0.5, 2.0, 30.0, 1e3, 1e300] {
        let (up, down) = (tanh(x), tanh(-x));
        assert!((up + down).abs() < 1e-15, "{x}: {up} and {down}");
        assert!((0.0..=1.0).contains(&up), "{x}: {up}");
    }
    assert!((tanh(0.5) - 0.462_117_157_260_009_8).abs() < 1e-15);
    assert!((tanh(-1e3) + 1.0).abs() < f64::EPSILON);
}
