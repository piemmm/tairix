use super::*;
use crate::holding::HoldingId;
use crate::plane;
use crate::testing::{Flat, Hills};

const KEY: Key = Key::new(66);

fn farm(index: i32) -> Settlement {
    Settlement {
        settled: Settled::Farmstead(HoldingId::new(index, -index)),
        at: Point::new(f64::from(index) * 431.0, f64::from(index) * -257.0),
    }
}

#[test]
fn a_farmsteads_buildings_stand_apart_about_its_yard_and_within_its_plot() {
    for ground in [&Flat as &dyn Ground, &Hills] {
        for index in 0..400 {
            let toward = Point::toward(f64::from(index));
            let laid = lay_out(KEY, &farm(index), toward, ground).expect("room");
            assert_eq!(laid.at, farm(index).at);
            assert_eq!(laid.buildings[0].standing, Standing::Dwelling);
            let outlines: Vec<Convex> = laid.buildings.iter().map(Footprint::outline).collect();
            for (n, outline) in outlines.iter().enumerate() {
                assert!(!outline.overlaps(&laid.yard), "{index}: building {n} stands in the yard");
                for other in &outlines[n + 1..] {
                    assert!(!outline.overlaps(other), "{index}: buildings overlap");
                }
                assert!(outline.corners.iter().all(|&corner| laid.plot.contains(corner)));
            }
            assert!(!outlines[0].overlaps(&laid.garden), "the garden lies before the house");
            assert!(laid.garden.corners.iter().all(|&corner| laid.plot.contains(corner)));
            let on_edge = laid
                .yard
                .edges()
                .any(|(a, b)| plane::onto_segment(laid.gate, a, b).1 < 1e-9);
            assert!(on_edge, "{index}: the gate is not on the yard's edge");
        }
    }
}

#[test]
fn a_hull_is_convex_and_holds_every_point() {
    let mut draws = KEY.draws(Stage::Yard, (0, 0));
    let points: Vec<Point> = (0..200)
        .map(|_| Point::new(draws.range(-50.0, 50.0), draws.range(-20.0, 80.0)))
        .collect();
    let hull = hull(&points).expect("room");
    for (a, b) in hull.edges() {
        for &corner in &hull.corners {
            assert!((b - a).cross(corner - a) >= -1e-9);
        }
    }
    assert!(points.iter().all(|&point| hull.contains(point)));
}
