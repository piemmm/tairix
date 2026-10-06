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
    assert!(serial.parcels().len() > 40, "{} fields", serial.parcels().len());
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
            let found = whole.parcels().iter().find(|other| other.field.id == parcel.field.id);
            assert_eq!(found, Some(parcel), "{:?}", parcel.field.id);
        }
        for boundary in piece.boundaries() {
            let found = whole.boundaries().iter().find(|other| other.id == boundary.id);
            assert_eq!(found, Some(boundary), "{:?}", boundary.id);
        }
        for way in piece.ways() {
            let found = whole.ways().find(|other| other.id == way.id);
            assert_eq!(found.map(|way| &way.line), Some(&way.line), "{:?}", way.id);
        }
        for farm in piece.farmsteads() {
            assert!(whole.farmsteads().contains(farm));
        }
        assert!(!piece.parcels().is_empty());
    }
}

#[test]
fn no_field_lies_on_water_or_in_a_ways_corridor() {
    let layout = laid(region(), &Threaded::new(4));
    let corridors: Vec<&Way> = layout.ways.iter().filter(|way| way.id.rank.bounded()).collect();
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
        let (left, right) = (layout.side_at(middle + off, &Hills), layout.side_at(middle - off, &Hills));
        assert!(agrees(left, boundary.left), "{:?}: {left:?} against {:?}", boundary.id, boundary.left);
        assert!(agrees(right, boundary.right), "{:?}: {right:?} against {:?}", boundary.id, boundary.right);
        assert!(matches!(boundary.left, Side::Field(_)) || matches!(boundary.right, Side::Field(_)));
        for gap in &boundary.gaps {
            assert!(gap.along >= 0.0 && gap.along <= length + 1e-9);
        }
    }
    assert!(kinds.iter().filter(|&&count| count > 0).count() >= 3, "{kinds:?}");
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
            .filter(|boundary| boundary.left == Side::Field(field) || boundary.right == Side::Field(field))
            .collect();
        let entered = touching.iter().any(|boundary| {
            matches!(boundary.id.owner, Owner::End(..))
                || boundary.gaps.iter().any(|gap| gap.through == Through::Gateway)
        });
        let enterable = touching.iter().any(|boundary| plane::length(&boundary.line) > 6.0);
        assert!(entered || !enterable, "{field:?} is never entered");
    }
}

#[test]
fn every_track_ends_at_a_gateway_in_its_own_holding_and_no_way_runs_through_a_building() {
    let layout = laid(region(), &Threaded::new(4));
    let lattice = Lattice::new(countryside().spacing, countryside().key);
    for way in layout.ways() {
        if let (Rank::Track, Joins::Ends(_, Placed::Gateway(holding, _))) = (way.id.rank, way.id.joins) {
            let end = way.line.stations.last().expect("a line").at;
            assert!(lattice.outline(holding).contains(end));
        }
        let (first, last) = (way.line.stations[0].at, way.line.stations[way.line.stations.len() - 1].at);
        for farm in layout.farmsteads() {
            for building in &farm.buildings {
                let outline = building.outline();
                for station in &way.line.stations {
                    let clear = (station.at - first).length() < 12.0 || (station.at - last).length() < 12.0;
                    assert!(clear || !outline.contains(station.at), "{:?} runs through a building", way.id);
                }
            }
        }
    }
}

/// A level dale between steep fells nobody farms.
struct Dale;

impl Ground for Dale {
    fn height(&self, at: Point) -> f64 {
        let across = (at.y - 0.25 * at.x).abs();
        if across < 250.0 { 30.0 } else { 30.0 + 0.6 * (across - 250.0) }
    }

    fn water(&self, _: Point) -> Option<f64> {
        None
    }

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
    assert!(open > 300 && farmed > 300, "{open} open, {farmed} in fields");
    let dykes = layout
        .boundaries()
        .iter()
        .filter(|boundary| matches!(boundary.id.owner, Owner::Edge(..)))
        .filter(|boundary| boundary.left == Side::Waste || boundary.right == Side::Waste)
        .count();
    assert!(dykes > 3, "{dykes} boundaries against the open fell");
}
