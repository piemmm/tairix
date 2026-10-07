use super::*;
use crate::route::Station;
use crate::testing::Hills;

const KEY: Key = Key::new(31);

fn lane(from: Point, to: Point) -> Line {
    let count = 120;
    Line {
        stations: (0..=count)
            .map(|index| Station {
                at: from.lerp(to, f64::from(index) / f64::from(count)),
                level: 0.0,
                width: 3.6,
                water: None,
            })
            .collect(),
    }
}

fn holding_parcels(holding: HoldingId, ways: &[(Rank, &Line)]) -> (Lattice, Parcels) {
    let lattice = Lattice::new(600.0, KEY);
    let surround = Surround {
        ways,
        plots: &[],
        ground: &Hills,
    };
    let parcels = parcels(KEY, &lattice, holding, &surround).expect("room");
    (lattice, parcels)
}

#[test]
fn a_holdings_fields_tile_its_land_and_each_is_big_enough_to_be_one() {
    for (i, j) in [(0, 0), (1, 0), (-2, 1), (3, -2)] {
        let holding = HoldingId::new(i, j);
        let lattice = Lattice::new(600.0, KEY);
        let middle = lattice.vertex(holding);
        let road = lane(
            middle + Point::new(-500.0, -90.0),
            middle + Point::new(500.0, 110.0),
        );
        let (_, parcels) = holding_parcels(holding, &[(Rank::Lane, &road)]);
        let mut land = alloc::vec![0.0; parcels.fields.len()];
        for row in 0..parcels.rows {
            for column in 0..parcels.columns {
                let Some(Cell::Land(block)) = parcels.cell(column, row) else {
                    continue;
                };
                let at = parcels.middle(column, row);
                let field = parcels
                    .field_in(block, at)
                    .expect("a field holds every cell of land");
                let field = &parcels.fields[field as usize];
                assert_eq!(field.block, block);
                let within = lattice.holds(holding, at);
                assert!(
                    !within || field.cell.contains(at),
                    "a field's land lies in its cell"
                );
                land[field.id.index as usize] += CELL * CELL;
            }
        }
        for (field, area) in parcels.fields.iter().zip(&land) {
            assert!((field.area - area).abs() < 1e-6);
            assert!(field.area >= LEAST_FIELD);
            assert!((field.along.length() - 1.0).abs() < 1e-9);
        }
        assert!(
            parcels.fields.len() > 3,
            "{holding:?} has {} fields",
            parcels.fields.len()
        );
    }
}

#[test]
fn no_field_takes_water_or_a_ways_corridor() {
    let lattice = Lattice::new(600.0, KEY);
    // The holdings the test land's river runs through.
    for holding in [
        HoldingId::new(0, 0),
        HoldingId::new(1, -1),
        HoldingId::new(0, 1),
    ] {
        let middle = lattice.vertex(holding);
        let road = lane(
            middle + Point::new(-400.0, 300.0),
            middle + Point::new(400.0, -300.0),
        );
        let (_, parcels) = holding_parcels(holding, &[(Rank::Lane, &road)]);
        for row in 0..parcels.rows {
            for column in 0..parcels.columns {
                let at = parcels.middle(column, row);
                if let Some(Cell::Land(_)) = parcels.cell(column, row) {
                    assert!(!ground::wet(&Hills, at));
                    let near = road.nearest(at).expect("a line");
                    assert!(near.0.distance > 0.5 * 3.6 + Rank::Lane.laying().verge);
                }
            }
        }
    }
}

#[test]
fn every_cut_runs_across_the_piece_it_cut() {
    let (lattice, parcels) = holding_parcels(HoldingId::new(2, 2), &[]);
    let outline = lattice.outline(HoldingId::new(2, 2)).expect("room");
    let mut cuts = 0;
    for node in &parcels.nodes {
        if let Node::Cut {
            at, normal, chord, ..
        } = *node
        {
            cuts += 1;
            for end in [chord.0, chord.1] {
                assert!(
                    (end - at).dot(normal).abs() < 1e-6,
                    "a chord's end lies on its line"
                );
                assert!(outline
                    .bounds()
                    .is_some_and(|bounds| bounds.grown(1e-6).contains(end)));
            }
            assert!((chord.1 - chord.0).length() > 0.0);
        }
    }
    assert!(cuts > 0);
}

#[test]
fn the_same_holding_is_cut_the_same_every_time() {
    let first = holding_parcels(HoldingId::new(-1, 3), &[]).1;
    let again = holding_parcels(HoldingId::new(-1, 3), &[]).1;
    assert_eq!(first.fields, again.fields);
    assert_eq!(first.nodes, again.nodes);
}
