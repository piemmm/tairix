//! Host tests of courses and the index that finds the nearest of them.

use alloc::vec;
use alloc::vec::Vec;

use super::*;

fn mark(x: f64, z: f64, level: f64) -> Mark {
    Mark {
        x,
        z,
        level,
        width: 4.0,
        depth: 1.0,
        ..Mark::default()
    }
}

/// A reach of `beyond` whatever a course's breadth.
fn reach(beyond: f64) -> Reach {
    Reach {
        per_width: 0.0,
        beyond,
    }
}

/// A course running along +z from the origin, falling as it goes.
fn straight() -> Vec<Mark> {
    vec![
        mark(0.0, 0.0, 10.0),
        mark(0.0, 50.0, 5.0),
        mark(0.0, 100.0, 0.0),
    ]
}

#[test]
fn the_nearest_point_is_found_with_its_distance_side_and_blended_level() {
    let courses =
        Courses::new(&[straight()], ((-200.0, -200.0), 400.0), reach(20.0)).expect("courses");
    let near = courses.nearest(3.0, 25.0).expect("near");
    assert!((near.distance - 3.0).abs() < 1e-9, "{near:?}");
    assert!((near.along - 25.0).abs() < 1e-9, "{near:?}");
    assert!((near.level - 7.5).abs() < 1e-9, "{near:?}");
    let other = courses.nearest(-3.0, 25.0).expect("near");
    assert!(near.side * other.side < 0.0, "either side of the course");
    assert_eq!(courses.len(), 1);
    assert_eq!(courses.course(0).len(), 3);
    assert!(courses.course(1).is_empty());
}

#[test]
fn nothing_is_found_beyond_the_reach_or_outside_the_index() {
    let courses =
        Courses::new(&[straight()], ((-200.0, -200.0), 400.0), reach(20.0)).expect("courses");
    assert!(
        courses.nearest(0.0, 500.0).is_none(),
        "outside the indexed square"
    );
    assert!(
        courses.nearest(150.0, 50.0).is_none(),
        "far beyond the reach"
    );
    assert!(Courses::none().nearest(0.0, 0.0).is_none());
}

#[test]
fn of_two_courses_the_nearer_is_found() {
    let across = vec![mark(-60.0, 60.0, 3.0), mark(60.0, 60.0, 3.0)];
    let courses = Courses::new(
        &[straight(), across],
        ((-200.0, -200.0), 400.0),
        reach(30.0),
    )
    .expect("courses");
    let near = courses.nearest(10.0, 58.0).expect("near");
    assert_eq!(near.course, 1, "{near:?}");
    let near = courses.nearest(1.0, 20.0).expect("near");
    assert_eq!(near.course, 0, "{near:?}");
}

#[test]
fn smoothing_keeps_the_ends_and_rounds_the_corners() {
    let corner = vec![
        mark(0.0, 0.0, 0.0),
        mark(10.0, 0.0, 0.0),
        mark(10.0, 10.0, 0.0),
    ];
    let smooth = smoothed(&corner, 3).expect("smoothed");
    assert!(smooth.len() > corner.len() * 4);
    assert_eq!(smooth.first(), corner.first());
    assert_eq!(smooth.last(), corner.last());
    // No point of the smoothed course reaches the corner it cut.
    assert!(smooth
        .iter()
        .all(|at| mathf::hypot(at.x - 10.0, at.z) > 1.0));
}
