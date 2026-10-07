use tairix_parallel::{Reversed, Threaded, SERIAL};

use super::*;
use crate::boundary::{Owner, Through};
use crate::field::FieldId;
use crate::testing::Hills;

fn countryside() -> Countryside {
    Countryside {
        key: Key::new(2024),
        spacing: 300.0,
        villages: Villages {
            spacing: 1200.0,
            exclusion: 0.8,
            gathers: 0.25,
        },
        style: Style {
            hedge: 1.0,
            wall: 0.6,
            fence: 0.5,
            ditch: 0.4,
            open: 0.1,
        },
        mix: Mix {
            arable: 1.0,
            pasture: 1.0,
            meadow: 0.5,
            orchard: 0.3,
            vineyard: 0.2,
            woodlot: 0.3,
            overgrown: 0.2,
        },
        warm: Point::new(0.0, -1.0),
        highways: Vec::new(),
    }
}

fn laid(region: Rect, runner: &dyn JobRunner) -> Layout {
    Laying::new(countryside(), region)
        .expect("a sane countryside")
        .run(&Hills, runner)
        .expect("room")
}

fn region() -> Rect {
    Rect::around(Point::new(200.0, 100.0), 700.0)
}

/// Every feature a layout reports.
#[derive(Debug, PartialEq)]
struct Features<'a> {
    ways: Vec<&'a Way>,
    parcels: &'a [Parcel],
    boundaries: &'a [Boundary],
    farmsteads: &'a [Farmstead],
    villages: &'a [Village],
}

fn features(layout: &Layout) -> Features<'_> {
    Features {
        ways: layout.ways().collect(),
        parcels: layout.parcels(),
        boundaries: layout.boundaries(),
        farmsteads: layout.farmsteads(),
        villages: layout.villages(),
    }
}

#[test]
fn a_layout_comes_out_the_same_however_its_work_is_shared() {
    let serial = laid(region(), &SERIAL);
    let threaded = laid(region(), &Threaded::new(4));
    let reversed = laid(region(), &Reversed::new(4));
    assert_eq!(features(&serial), features(&threaded));
    assert_eq!(features(&serial), features(&reversed));
    assert!(
        serial.parcels().len() > 40,
        "{} fields",
        serial.parcels().len()
    );
    assert!(serial.ways().any(|way| way.id.rank == Rank::Lane));
    assert!(serial.ways().any(|way| way.id.rank == Rank::Track));
    assert!(!serial.farmsteads().is_empty());
}

#[test]
fn a_region_laid_out_in_halves_lays_every_feature_as_the_whole_does() {
    let runner = Threaded::new(4);
    let whole = laid(region(), &runner);
    let (low, high) = (region().low, region().high);
    let middle = f64::midpoint(low.x, high.x);
    for half in [
        Rect {
            low,
            high: Point::new(middle, high.y),
        },
        Rect {
            low: Point::new(middle, low.y),
            high,
        },
    ] {
        let piece = laid(half, &runner);
        for parcel in piece.parcels() {
            let found = whole
                .parcels()
                .iter()
                .find(|other| other.field.id == parcel.field.id);
            assert_eq!(found, Some(parcel), "{:?}", parcel.field.id);
        }
        for boundary in piece.boundaries() {
            let found = whole
                .boundaries()
                .iter()
                .find(|other| other.id == boundary.id);
            assert_eq!(found, Some(boundary), "{:?}", boundary.id);
        }
        for way in piece.ways() {
            let found = whole.ways().find(|other| other.id == way.id);
            assert_eq!(found.map(|way| &way.line), Some(&way.line), "{:?}", way.id);
        }
        for farm in piece.farmsteads() {
            assert!(whole.farmsteads().contains(farm));
        }
        for village in piece.villages() {
            let found = whole
                .villages()
                .iter()
                .find(|other| other.settled == village.settled);
            assert_eq!(found, Some(village), "{:?}", village.settled);
        }
        for settlement in piece.settlements() {
            assert!(
                whole.settlements().contains(settlement),
                "{:?}",
                settlement.settled
            );
        }
        assert!(!piece.parcels().is_empty());
    }
    assert!(
        whole
            .villages()
            .iter()
            .any(|village| !village.plots.is_empty()),
        "no village's plots to compare"
    );
}

#[test]
fn no_field_lies_on_water_or_in_a_ways_corridor() {
    let layout = laid(region(), &Threaded::new(4));
    let corridors: Vec<&Way> = layout
        .ways
        .iter()
        .filter(|way| way.id.rank.bounded())
        .collect();
    let mut draws = Key::new(1).draws(Stage::Use, (0, 0));
    let mut checked = 0;
    for _ in 0..6000 {
        let at = Point::new(
            draws.range(region().low.x, region().high.x),
            draws.range(region().low.y, region().high.y),
        );
        let Some(parcel) = layout.parcel_at(at, &Hills) else {
            continue;
        };
        checked += 1;
        assert!(!ground::wet(&Hills, at), "{at:?} is a field under water");
        assert!(parcel.field.cell.contains(at));
        for way in &corridors {
            let (near, station) = way.line.nearest(at).expect("a line");
            assert!(
                near.distance >= 0.5 * station.width + way.id.rank.laying().verge - 0.5,
                "{at:?} is a field in {:?}'s corridor",
                way.id
            );
        }
    }
    assert!(checked > 2000, "{checked} places in fields");
}

#[test]
fn every_boundary_parts_what_lies_either_side_of_it() {
    let layout = laid(region(), &Threaded::new(4));
    assert!(layout.boundaries().len() > 60);
    let mut kinds = [0usize; 5];
    for boundary in layout.boundaries() {
        kinds[boundary.kind as usize] += 1;
        let length = plane::length(&boundary.line);
        let (middle, way) = plane::at(&boundary.line, 0.5 * length).expect("a line");
        let off = way.left() * 0.5;
        // Where ways run together any of them lines a way's boundary; a field
        // is the one field.
        let agrees = |read: Side, kept: Side| match kept {
            Side::Way(_) => matches!(read, Side::Way(_)),
            _ => read == kept,
        };
        let (left, right) = (
            layout.side_at(middle + off, &Hills),
            layout.side_at(middle - off, &Hills),
        );
        assert!(
            agrees(left, boundary.left),
            "{:?}: {left:?} against {:?}",
            boundary.id,
            boundary.left
        );
        assert!(
            agrees(right, boundary.right),
            "{:?}: {right:?} against {:?}",
            boundary.id,
            boundary.right
        );
        assert!(
            matches!(boundary.left, Side::Field(_)) || matches!(boundary.right, Side::Field(_))
        );
        for gap in &boundary.gaps {
            assert!(gap.along >= 0.0 && gap.along <= length + 1e-9);
        }
    }
    assert!(
        kinds.iter().filter(|&&count| count > 0).count() >= 3,
        "{kinds:?}"
    );
}

#[test]
fn every_field_with_a_way_or_neighbour_is_entered_by_a_gate() {
    let layout = laid(region(), &Threaded::new(4));
    // A field wholly within the region has every boundary it has reported.
    let within: Vec<FieldId> = layout
        .parcels()
        .iter()
        .filter(|parcel| {
            parcel.field.cell.bounds().is_some_and(|bounds| {
                region().contains(bounds.low) && region().contains(bounds.high)
            })
        })
        .map(|parcel| parcel.field.id)
        .collect();
    assert!(within.len() > 20, "{} fields within", within.len());
    for field in within {
        let touching: Vec<&Boundary> = layout
            .boundaries()
            .iter()
            .filter(|boundary| {
                boundary.left == Side::Field(field) || boundary.right == Side::Field(field)
            })
            .collect();
        let entered = touching.iter().any(|boundary| {
            matches!(boundary.id.owner, Owner::End(..))
                || boundary
                    .gaps
                    .iter()
                    .any(|gap| gap.through == Through::Gateway)
        });
        let enterable = touching
            .iter()
            .any(|boundary| plane::length(&boundary.line) > 6.0);
        assert!(entered || !enterable, "{field:?} is never entered");
    }
}

#[test]
fn every_track_ends_at_a_gateway_in_its_own_holding_and_no_way_runs_through_a_building() {
    let layout = laid(region(), &Threaded::new(4));
    let lattice = Lattice::new(countryside().spacing, countryside().key);
    let buildings: Vec<[Point; 4]> = layout
        .farmsteads()
        .iter()
        .flat_map(|farm| farm.buildings.iter().map(crate::farm::Footprint::corners))
        .chain(
            layout
                .villages()
                .iter()
                .flat_map(|village| village.plots.iter().map(|plot| plot.house.corners())),
        )
        .collect();
    assert!(buildings.len() > 20, "{} buildings", buildings.len());
    for way in layout.ways() {
        if let (Rank::Track, Joins::Ends(_, Placed::Gateway(holding, _))) =
            (way.id.rank, way.id.joins)
        {
            let end = way.line.stations.last().expect("a line").at;
            assert!(lattice.holds(holding, end));
        }
        // Every half metre of the line, ends and all.
        for pair in way.line.stations.windows(2) {
            let (a, b) = (pair[0].at, pair[1].at);
            let steps = u32::try_from(mathf::round_i32(
                mathf::ceil((b - a).length() / 0.5).max(1.0),
            ))
            .expect("a short line");
            for step in 0..=steps {
                let at = a.lerp(b, f64::from(step) / f64::from(steps));
                for corners in &buildings {
                    assert!(
                        !plane::contains(corners, at),
                        "{:?} runs through a building at {at:?}",
                        way.id
                    );
                }
            }
        }
    }
}

/// A level dale between steep fells nobody farms.
struct Dale;

impl Waters for Dale {
    fn height(&self, at: Point) -> f64 {
        let across = (at.y - 0.25 * at.x).abs();
        if across < 250.0 {
            30.0
        } else {
            30.0 + 0.6 * (across - 250.0)
        }
    }

    fn water(&self, _: Point) -> Option<f64> {
        None
    }
}

impl Ground for Dale {
    fn lie(&self, at: Point) -> Lie {
        let fell = (at.y - 0.25 * at.x).abs() > 250.0;
        Lie {
            wet: 0.1,
            stony: if fell { 0.8 } else { 0.2 },
            wooded: 0.2,
            fertile: if fell { 0.1 } else { 0.8 },
        }
    }
}

#[test]
fn the_fields_end_at_a_boundary_where_the_land_nobody_farms_begins() {
    let layout = Laying::new(countryside(), region())
        .expect("a sane countryside")
        .run(&Dale, &Threaded::new(4))
        .expect("room");
    let mut draws = Key::new(4).draws(Stage::Use, (0, 0));
    let (mut open, mut farmed) = (0, 0);
    for _ in 0..3000 {
        let at = Point::new(
            draws.range(region().low.x, region().high.x),
            draws.range(region().low.y, region().high.y),
        );
        match layout.side_at(at, &Dale) {
            Side::Field(_) => farmed += 1,
            Side::Waste if (at.y - 0.25 * at.x).abs() > 550.0 => open += 1,
            _ => {}
        }
    }
    assert!(
        open > 300 && farmed > 300,
        "{open} open, {farmed} in fields"
    );
    let dykes = layout
        .boundaries()
        .iter()
        .filter(|boundary| matches!(boundary.id.owner, Owner::Edge(..)))
        .filter(|boundary| boundary.left == Side::Waste || boundary.right == Side::Waste)
        .count();
    assert!(dykes > 3, "{dykes} boundaries against the open fell");
}

#[test]
fn a_laying_refuses_a_region_beyond_reach_of_the_origin() {
    let far = Rect::around(Point::new(2.0e6, 0.0), 100.0);
    let unbounded = Rect {
        low: Point::new(f64::NEG_INFINITY, 0.0),
        high: Point::new(0.0, 10.0),
    };
    let unknown = Rect {
        low: Point::new(f64::NAN, 0.0),
        high: Point::new(0.0, 10.0),
    };
    for region in [far, unbounded, unknown] {
        assert!(
            matches!(Laying::new(countryside(), region), Err(Error::Shape)),
            "{region:?}"
        );
    }
}

/// Every unit hands each core at most its phase's share of jobs, so a unit
/// stays a fixed amount of work however large the region.
#[test]
fn a_laying_never_hands_a_core_more_than_a_units_jobs() {
    let runner = Reversed::new(2);
    let mut laying = Laying::new(countryside(), region()).expect("a sane countryside");
    let mut units = 0;
    while laying.step(&Hills, &runner).expect("room").is_none() {
        units += 1;
    }
    let most = [
        FARMS_EACH,
        YARDS_EACH,
        NODES_EACH,
        TRACKS_EACH,
        VILLAGES_EACH,
        PARCELS_EACH,
        BOUNDS_EACH,
        GATEWAYS_EACH,
        STILES_EACH,
        USES_EACH,
    ]
    .into_iter()
    .max()
    .expect("phases");
    assert!(
        runner.widest() <= 2 * most,
        "{} jobs in one dispatch",
        runner.widest()
    );
    assert!(units > 100, "{units} units");
}

#[test]
fn a_layings_progress_climbs_and_is_whole_only_once_it_is_laid() {
    assert!(
        (SHARES.iter().sum::<f64>() - 1.0).abs() < 1e-9,
        "shares sum to {}",
        SHARES.iter().sum::<f64>()
    );
    let mut laying = Laying::new(countryside(), region()).expect("a sane countryside");
    let mut last = laying.done();
    assert!(last < 1e-9);
    loop {
        let laid = laying.step(&Hills, &SERIAL).expect("room");
        let now = laying.done();
        assert!(now >= last - 1e-12, "progress fell from {last} to {now}");
        if laid.is_some() {
            assert!((now - 1.0).abs() < 1e-12);
            break;
        }
        assert!(now < 1.0, "whole before the layout is laid");
        last = now;
    }
}
