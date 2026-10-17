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

/// How far past a course's ends a place lies, along its line there, and
/// nought anywhere alongside it.
#[test]
fn a_place_past_a_courses_end_knows_how_far() {
    let courses =
        Courses::new(&[straight()], ((-200.0, -200.0), 400.0), reach(20.0)).expect("courses");
    let past = |x: f64, z: f64| courses.nearest(x, z).expect("near").past;
    assert!(past(2.0, 50.0).abs() < 1e-9, "alongside its middle");
    assert!(past(2.0, 99.0).abs() < 1e-9, "alongside its last stretch");
    assert!((past(1.0, 104.0) - 4.0).abs() < 1e-9, "past its last mark");
    assert!(
        (past(-1.0, -6.0) - 6.0).abs() < 1e-9,
        "before its first mark"
    );
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

/// The index finds the nearest point of any course within its reach, and
/// half a course's breadth more, as a search of every segment of every
/// course finds it, and none where none lies that near.
#[test]
fn the_index_finds_what_a_search_of_every_segment_finds() {
    let reaching = 30.0 + 0.5 * mark(0.0, 0.0, 0.0).width;
    let wiggly: Vec<Vec<Mark>> = (0..6u32)
        .map(|course| {
            (0..24u32)
                .map(|at| {
                    let wander = 30.0 * mathf::sin(0.4 * f64::from(at) + f64::from(course));
                    let x = -280.0 + 90.0 * f64::from(course) + wander;
                    mark(x, -300.0 + 25.0 * f64::from(at), 0.0)
                })
                .collect()
        })
        .collect();
    let courses = Courses::new(&wiggly, ((-320.0, -320.0), 640.0), reach(30.0)).expect("courses");
    let searched = |x: f64, z: f64| {
        (0..courses.len())
            .flat_map(|course| courses.course(course).windows(2))
            .map(|pair| {
                let (a, b) = (pair[0], pair[1]);
                let (dx, dz) = (b.x - a.x, b.z - a.z);
                let t = (((x - a.x) * dx + (z - a.z) * dz) / (dx * dx + dz * dz)).clamp(0.0, 1.0);
                mathf::hypot(x - (a.x + t * dx), z - (a.z + t * dz))
            })
            .filter(|&distance| distance <= reaching)
            .min_by(f64::total_cmp)
    };
    let (mut found, mut missed) = (0u32, 0u32);
    for draw in 0..4000u32 {
        let place = |salt: u32| -300.0 + 600.0 * tairix_mix::unit(tairix_mix::mix32(draw ^ salt));
        let (x, z) = (place(0x3a1), place(0x97c));
        let nearest = courses.nearest(x, z).map(|nearest| nearest.distance);
        match (nearest, searched(x, z)) {
            (Some(by_index), Some(by_search)) => {
                assert!(
                    (by_index - by_search).abs() < 1e-9,
                    "({x}, {z}): {by_index} against {by_search}"
                );
                found += 1;
            }
            (None, None) => missed += 1,
            other => panic!("({x}, {z}): {other:?}"),
        }
    }
    assert!(
        found > 500 && missed > 500,
        "{found} found, {missed} beyond reach"
    );
}

/// A course past the last, however far past, has no marks, place or way,
/// and asking for one never overflows.
#[test]
fn a_course_past_the_last_has_nothing() {
    let courses =
        Courses::new(&[straight()], ((-200.0, -200.0), 400.0), reach(20.0)).expect("courses");
    for index in [1, usize::MAX] {
        assert!(courses.course(index).is_empty(), "{index}");
        assert!(courses.at(index, 10.0).is_none(), "{index}");
        assert!(courses.way(index, 10.0).is_none(), "{index}");
    }
    assert!(!courses.course(0).is_empty());
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

/// A course whose length is not finite has no place to hold within it, and
/// its way is answered rather than refused with a panic.
#[test]
fn a_course_of_no_finite_length_answers_its_way() {
    let mut courses =
        Courses::new(&[straight()], ((-200.0, -200.0), 400.0), reach(20.0)).expect("courses");
    if let Some(length) = courses.along.last_mut() {
        *length = f64::NAN;
    }
    assert!(courses.way(0, 3.0).is_some());
}

/// The way a course runs holds right to its ends and past them, where its
/// marks are held: a piece lodged beyond the end of a stream still lies
/// across the stream rather than along nothing.
#[test]
fn a_courses_way_holds_to_its_ends_and_past_them() {
    let courses =
        Courses::new(&[straight()], ((-200.0, -200.0), 400.0), reach(20.0)).expect("courses");
    let length = *courses.along.last().expect("a length");
    for along in [
        -10.0,
        0.0,
        0.5,
        0.5 * length,
        length - 0.3,
        length,
        length + 30.0,
    ] {
        let (down, left) = courses.way(0, along).expect("a way");
        assert!(
            (mathf::hypot(down.0, down.1) - 1.0).abs() < 1e-9,
            "{along}: {down:?}"
        );
        assert!((down.0 * left.0 + down.1 * left.1).abs() < 1e-9, "{along}");
        let (dx, dz) = {
            let marks = courses.course(0);
            let (first, last) = (marks[0], marks[marks.len() - 1]);
            (last.x - first.x, last.z - first.z)
        };
        let along_course = (down.0 * dx + down.1 * dz) / mathf::hypot(dx, dz);
        assert!(along_course > 0.99, "{along}: {down:?} against the course");
    }
}
