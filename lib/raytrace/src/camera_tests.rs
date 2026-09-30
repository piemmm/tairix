//! Host tests of the camera.

use tairix_util::mathf;

use super::super::vector::Vec3;
use super::Camera;

#[test]
fn the_middle_of_the_film_looks_at_the_target_and_the_edges_at_the_field() {
    let eye = Vec3::new(1.0, 2.0, -6.0);
    let target = Vec3::new(0.0, 1.0, 0.0);
    let fov = 0.9;
    let camera = Camera::looking(eye, target, fov, 16.0 / 9.0, (0.0, 1.0));
    let middle = camera.ray((0.0, 0.0), (0.0, 0.0));
    assert!((middle.origin - eye).length() < 1e-12);
    assert!((middle.dir - (target - eye).normalized()).length() < 1e-12);
    let top = camera.ray((0.0, 1.0), (0.0, 0.0));
    assert!((mathf::acos(top.dir.dot(middle.dir)) - fov / 2.0).abs() < 1e-9);
    assert!(
        top.dir.y > middle.dir.y,
        "up on the film is up in the world"
    );
    let right = camera.ray((1.0, 0.0), (0.0, 0.0));
    let across = mathf::atan(mathf::tan(fov / 2.0) * 16.0 / 9.0);
    assert!((mathf::acos(right.dir.dot(middle.dir)) - across).abs() < 1e-9);
}

#[test]
fn a_point_projects_back_onto_the_film_it_was_seen_through() {
    let camera = Camera::looking(Vec3::new(0.0, 3.0, -8.0), Vec3::ZERO, 0.7, 1.5, (0.0, 1.0));
    for film in [(0.0, 0.0), (0.5, -0.25), (-0.9, 0.8)] {
        let ray = camera.ray(film, (0.0, 0.0));
        let (x, y) = camera.project(ray.at(5.0)).expect("in front");
        assert!((x - film.0).abs() < 1e-9 && (y - film.1).abs() < 1e-9);
    }
    assert!(
        camera.project(Vec3::new(0.0, 3.0, -20.0)).is_none(),
        "behind the camera"
    );
}

/// Through any part of the lens, a film point sees the same point at the
/// focal distance, and blurs everything nearer and farther.
#[test]
fn the_lens_is_sharp_at_its_focus_alone() {
    let focus = 4.0;
    let camera = Camera::looking(Vec3::ZERO, Vec3::new(0.0, 0.0, 1.0), 0.8, 1.0, (0.2, focus));
    let film = (0.3, -0.2);
    let sharp = camera.ray(film, (0.0, 0.0));
    let depth = |t: f64, ray: &super::super::vector::Ray| ray.at(t / ray.dir.z);
    for lens in [(1.0, 0.0), (-0.5, 0.5), (0.0, -1.0)] {
        let through = camera.ray(film, lens);
        assert!((depth(focus, &through) - depth(focus, &sharp)).length() < 1e-9);
        assert!((depth(1.0, &through) - depth(1.0, &sharp)).length() > 1e-3);
    }
}

#[test]
fn looking_straight_down_is_still_a_camera() {
    let camera = Camera::looking(Vec3::new(0.0, 5.0, 0.0), Vec3::ZERO, 0.8, 1.0, (0.0, 1.0));
    for film in [(0.0, 0.0), (1.0, 1.0), (-1.0, 0.5)] {
        let ray = camera.ray(film, (0.0, 0.0));
        assert!(ray.dir.is_finite() && (ray.dir.length() - 1.0).abs() < 1e-12);
        assert!(ray.dir.y < 0.0);
    }
    assert!((camera.eye() - Vec3::new(0.0, 5.0, 0.0)).length() < 1e-12);
    assert!(camera.pixel_angle(1000) > 0.0);
}
