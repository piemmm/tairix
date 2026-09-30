//! Host tests of the tracer's rays.

use super::{real, Frame, Pose, Ray, Vec3};

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
