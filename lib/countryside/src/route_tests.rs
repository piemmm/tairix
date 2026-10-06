use alloc::collections::BinaryHeap;
use core::cmp::Reverse;

use super::*;
use crate::ground::{self, Lie};
use crate::testing::Flat;

const KEY: Key = Key::new(23);

fn routed(rank: Rank, ends: (Point, Point), (greater, barred): (&[(Rank, &Line)], &[Convex]), ground: &dyn Ground) -> Line {
    let mut routing = Routing::new(rank, ends).expect("a lattice");
    routing.prepare(greater, barred).expect("room");
    loop {
        if let Some(line) = routing.step(KEY, ground).expect("a route") {
            return line;
        }
    }
}

/// Flat dry land with a round lake.
struct Lake;

impl Ground for Lake {
    fn height(&self, _: Point) -> f64 {
        10.0
    }

    fn water(&self, at: Point) -> Option<f64> {
        ((at - Point::new(500.0, 0.0)).length() < 120.0).then_some(11.0)
    }

    fn lie(&self, _: Point) -> Lie {
        Lie::default()
    }
}

#[test]
fn over_level_open_ground_a_road_runs_almost_straight_between_its_ends() {
    let ends = (Point::new(0.0, 0.0), Point::new(900.0, 300.0));
    let line = routed(Rank::Road, ends, (&[], &[]), &Flat);
    assert_eq!(line.stations.first().map(|station| station.at), Some(ends.0));
    assert_eq!(line.stations.last().map(|station| station.at), Some(ends.1));
    let length = plane::length(&line.places().collect::<Vec<_>>());
    let direct = (ends.1 - ends.0).length();
    assert!(length < 1.03 * direct, "{length} against {direct}");
    assert!(line.stations.iter().all(|station| (station.width - 5.5).abs() < 1.0));
}

#[test]
fn a_road_goes_round_a_lake_it_can_pass_cheaply() {
    let line = routed(Rank::Road, (Point::new(0.0, 0.0), Point::new(1000.0, 0.0)), (&[], &[]), &Lake);
    assert!(line.stations.iter().all(|station| !ground::wet(&Lake, station.at)));
    assert!(line.stations.iter().all(|station| station.water.is_none()));
}

#[test]
fn a_route_keeps_clear_of_what_bars_it_but_about_its_ends() {
    let barred = Convex {
        corners: alloc::vec![
            Point::new(200.0, -80.0),
            Point::new(320.0, -80.0),
            Point::new(320.0, 80.0),
            Point::new(200.0, 80.0),
        ],
    };
    let line = routed(
        Rank::Lane,
        (Point::new(0.0, 0.0), Point::new(520.0, 0.0)),
        (&[], core::slice::from_ref(&barred)),
        &Flat,
    );
    assert!(line.stations.iter().all(|station| !barred.contains(station.at)));
}

#[test]
fn a_lane_follows_the_road_that_leads_its_way() {
    let road = routed(Rank::Road, (Point::new(-100.0, 0.0), Point::new(1100.0, 0.0)), (&[], &[]), &Flat);
    let lane = routed(
        Rank::Lane,
        (Point::new(0.0, 40.0), Point::new(1000.0, 40.0)),
        (&[(Rank::Road, &road)], &[]),
        &Flat,
    );
    let beside = lane
        .stations
        .iter()
        .filter(|station| road.nearest(station.at).is_some_and(|(near, _)| near.distance < 6.0))
        .count();
    assert!(4 * beside > 3 * lane.stations.len(), "{beside} of {} beside the road", lane.stations.len());
}

#[test]
fn a_route_comes_out_the_same_whenever_it_is_routed() {
    let ends = (Point::new(0.0, 0.0), Point::new(700.0, -450.0));
    let first = routed(Rank::Track, ends, (&[], &[]), &crate::testing::Hills);
    let again = routed(Rank::Track, ends, (&[], &[]), &crate::testing::Hills);
    assert_eq!(first, again);
}

#[test]
fn the_guide_never_overestimates_the_rest_of_a_route_nor_falls_by_more_than_a_step() {
    let road = Line {
        stations: (0..=40)
            .map(|index| Station {
                at: Point::new(f64::from(index) * 5.0, 60.0 + f64::from(index)),
                level: 10.0,
                width: 5.0,
                water: None,
            })
            .collect(),
    };
    let mut routing = Routing::new(Rank::Lane, (Point::new(0.0, 0.0), Point::new(200.0, 30.0))).expect("a lattice");
    routing.prepare(&[(Rank::Road, &road)], &[]).expect("room");
    let view = View {
        rank: routing.rank,
        square: routing.square,
        samples: &routing.samples,
        ground: &crate::testing::Hills,
    };
    let columns = routing.square.columns();
    let goal = routing.square.index_of(routing.ends.1);
    let reach = routing.goal_reach;
    let neighbours = |index: usize| {
        let (x, y) = (index % columns, index / columns);
        (-1isize..=1).flat_map(move |dy| (-1isize..=1).map(move |dx| (dx, dy))).filter_map(move |(dx, dy)| {
            let (nx, ny) = (x.checked_add_signed(dx)?, y.checked_add_signed(dy)?);
            ((dx, dy) != (0, 0) && nx < columns && ny < columns).then_some((ny * columns + nx, dx != 0 && dy != 0))
        })
    };
    // The least cost of the rest of the way from every point: a search out
    // from the goal over every step reversed.
    let mut rest = alloc::vec![u64::MAX; routing.square.area()];
    let mut open = BinaryHeap::new();
    rest[goal] = 0;
    open.push(Reverse((0u64, goal)));
    while let Some(Reverse((cost, to))) = open.pop() {
        if cost > rest[to] {
            continue;
        }
        for (from, diagonal) in neighbours(to) {
            let Some(step) = view.price(from, to, diagonal) else {
                continue;
            };
            let through = cost + u64::from(step);
            if through < rest[from] {
                rest[from] = through;
                open.push(Reverse((through, from)));
            }
        }
    }
    for (index, &least) in rest.iter().enumerate() {
        let guide = u64::from(view.guide(index, (goal, reach)));
        assert!(guide <= least, "{index}: {guide} over {least}");
        for (next, diagonal) in neighbours(index) {
            if let Some(step) = view.price(index, next, diagonal) {
                let onward = u64::from(view.guide(next, (goal, reach)));
                assert!(guide <= u64::from(step) + onward, "{index} to {next}");
            }
        }
    }
}
