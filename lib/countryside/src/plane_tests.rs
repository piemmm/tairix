use super::*;

fn square(side: f64) -> Convex {
    Convex {
        corners: alloc::vec![
            Point::new(0.0, 0.0),
            Point::new(side, 0.0),
            Point::new(side, side),
            Point::new(0.0, side),
        ],
    }
}

#[test]
fn a_squares_area_and_middle() {
    let square = square(4.0);
    assert!((square.area() - 16.0).abs() < 1e-12);
    assert_eq!(square.centroid(), Point::new(2.0, 2.0));
    assert!(
        square.contains(Point::new(4.0, 2.0)),
        "its edges are its own"
    );
    assert!(!square.contains(Point::new(4.1, 2.0)));
}

#[test]
fn a_point_lies_as_far_within_a_polygon_as_its_nearest_edge() {
    let square = square(4.0);
    assert!((square.inset(Point::new(1.0, 2.5)) - 1.0).abs() < 1e-12);
    assert!((square.inset(Point::new(2.0, 2.0)) - 2.0).abs() < 1e-12);
    assert!(square.inset(Point::new(4.0, 2.0)).abs() < 1e-12);
    assert!(square.inset(Point::new(5.0, 2.0)) < 0.0);
    let line = Convex {
        corners: alloc::vec![Point::new(0.0, 0.0), Point::new(1.0, 0.0)],
    };
    assert!(
        line.inset(Point::new(0.5, 0.0)) < 0.0,
        "a line has no inside"
    );
}

#[test]
fn a_cut_parts_a_polygon_into_the_two_sides_of_its_line() {
    let square = square(4.0);
    let normal = Point::new(1.0, 1.0).normalized();
    let behind = square.behind(Point::new(2.0, 2.0), normal).expect("room");
    let ahead = square.behind(Point::new(2.0, 2.0), -normal).expect("room");
    assert!((behind.area() - 8.0).abs() < 1e-9);
    assert!((ahead.area() - 8.0).abs() < 1e-9);
    assert!(behind.contains(Point::new(0.5, 0.5)) && !behind.contains(Point::new(3.5, 3.5)));
}

#[test]
fn a_chord_runs_from_edge_to_edge_along_its_line() {
    let square = square(4.0);
    let (from, to) = square
        .chord(Point::new(1.0, 1.0), Point::new(1.0, 0.0))
        .expect("it crosses");
    assert_eq!((from, to), (Point::new(1.0, 0.0), Point::new(1.0, 4.0)));
    assert_eq!(
        square.chord(Point::new(9.0, 0.0), Point::new(1.0, 0.0)),
        None
    );
}

#[test]
fn polygons_overlap_only_where_they_share_more_than_an_edge() {
    let a = square(4.0);
    let shifted = |dx: f64| Convex {
        corners: a
            .corners
            .iter()
            .map(|&corner| corner + Point::new(dx, 1.0))
            .collect(),
    };
    assert!(a.overlaps(&shifted(3.0)));
    assert!(!a.overlaps(&shifted(4.0)), "touching edges");
    assert!(!a.overlaps(&shifted(6.0)));
}

#[test]
fn a_polyline_is_walked_and_its_nearest_point_found() {
    let line = [
        Point::new(0.0, 0.0),
        Point::new(10.0, 0.0),
        Point::new(10.0, 10.0),
    ];
    assert!((length(&line) - 20.0).abs() < 1e-12);
    let (at, way) = at(&line, 15.0).expect("on it");
    assert_eq!((at, way), (Point::new(10.0, 5.0), Point::new(0.0, 1.0)));
    let near = nearest(&line, Point::new(12.0, 4.0)).expect("a segment");
    assert!((near.distance - 2.0).abs() < 1e-12);
    assert!((near.along - 14.0).abs() < 1e-12);
    assert_eq!(near.segment, 1);
    assert!(near.side < 0.0, "to the right of its way");
}

/// The place `along` `line`, looked up afresh from its start.
fn looked_up(line: &[Point], along: f64) -> Option<(Point, Point)> {
    let mut walked = 0.0;
    let last = line.len().checked_sub(2)?;
    for (segment, pair) in line.windows(2).enumerate() {
        let (a, b) = (pair[0], pair[1]);
        let span = (b - a).length();
        if walked + span >= along || segment == last {
            let t = if span > 0.0 {
                ((along - walked) / span).clamp(0.0, 1.0)
            } else {
                0.0
            };
            return Some((a.lerp(b, t), (b - a).normalized()));
        }
        walked += span;
    }
    None
}

#[test]
fn a_walk_reads_a_line_as_looking_each_place_up_from_its_start_does() {
    let line = [
        Point::new(0.0, 0.0),
        Point::new(3.0, 4.0),
        Point::new(3.0, 4.0),
        Point::new(9.0, 4.0),
        Point::new(9.5, -2.0),
    ];
    let total = length(&line);
    // Forward and back, through every corner exactly and past both ends.
    let mut places: Vec<f64> = (-4..=60)
        .map(|step| f64::from(step) * total / 50.0)
        .collect();
    places.extend([5.0, 5.0, 11.0, 2.0, 11.0, 0.0, total, total + 3.0, 4.9]);
    let mut walk = Walk::new(&line);
    for along in places {
        let want = looked_up(&line, along);
        assert_eq!(walk.at(along), want, "walked to {along}");
        assert_eq!(at(&line, along), want, "looked up at {along}");
    }
    assert_eq!(Walk::new(&line[..1]).at(1.0), None);
}

#[test]
fn segments_cross_where_both_reach() {
    let crossed = crossing(
        (Point::new(0.0, 0.0), Point::new(4.0, 4.0)),
        (Point::new(0.0, 4.0), Point::new(4.0, 0.0)),
    );
    assert_eq!(crossed, Some((0.5, 0.5)));
    let short = crossing(
        (Point::new(0.0, 0.0), Point::new(1.0, 1.0)),
        (Point::new(0.0, 4.0), Point::new(4.0, 0.0)),
    );
    assert_eq!(short, None);
}
