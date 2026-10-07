use super::*;

const SPACING: f64 = 400.0;

fn lattice() -> Lattice {
    Lattice::new(SPACING, Key::new(41))
}

fn located(lattice: &Lattice, at: Point) -> HoldingId {
    lattice.locate(at, &|holding| {
        (lattice.holds(holding, at), lattice.vertex(holding))
    })
}

#[test]
fn every_holding_is_convex_and_holds_its_vertex() {
    let lattice = lattice();
    for j in -6..6 {
        for i in -6..6 {
            let holding = HoldingId::new(i, j);
            let outline = lattice.outline(holding).expect("room");
            assert_eq!(outline.corners.len(), 6);
            assert!(outline.area() > 0.0);
            for (a, b) in outline.edges() {
                for &corner in &outline.corners {
                    assert!(
                        (b - a).cross(corner - a) >= -1e-6,
                        "{holding:?} is not convex"
                    );
                }
            }
            assert!(outline.contains(lattice.vertex(holding)));
        }
    }
}

#[test]
fn neighbouring_holdings_share_their_edges_to_the_bit() {
    let lattice = lattice();
    for j in -4..4 {
        for i in -4..4 {
            let holding = HoldingId::new(i, j);
            let outline = lattice.outline(holding).expect("room");
            for edge in 0..6 {
                let (di, dj) = AROUND[(edge + 1) % 6];
                let beyond = lattice
                    .outline(HoldingId::new(i + di, j + dj))
                    .expect("room");
                let (a, b) = (outline.corners[edge], outline.corners[(edge + 1) % 6]);
                let shared = beyond.edges().any(|(c, d)| (c, d) == (b, a));
                assert!(shared, "{holding:?}'s edge {edge} is not its neighbour's");
            }
        }
    }
}

#[test]
fn the_holdings_tile_the_land_and_a_place_is_found_in_the_one_holding_it_lies_in() {
    let lattice = lattice();
    let mut draws = Key::new(3).draws(Stage::Holding, (0, 0));
    for _ in 0..4000 {
        let at = Point::new(draws.range(-2000.0, 2000.0), draws.range(-2000.0, 2000.0));
        let found = located(&lattice, at);
        assert!(lattice.holds(found, at));
        let holding_count = (-1..=1)
            .flat_map(|dj| (-1..=1).map(move |di| (di, dj)))
            .map(|(di, dj)| HoldingId::new(found.i + di, found.j + dj))
            .filter(|&holding| holding != found && lattice.holds(holding, at))
            .count();
        assert!(
            holding_count <= 1,
            "{at:?} lies inside two holdings' interiors"
        );
    }
}

#[test]
fn the_holdings_over_a_rectangle_cover_it_and_each_reaches_it() {
    let lattice = lattice();
    let rect = Rect::around(Point::new(150.0, -40.0), 900.0);
    let holdings = lattice.holdings_over(rect).expect("room");
    let mut draws = Key::new(5).draws(Stage::Holding, (0, 0));
    for _ in 0..2000 {
        let at = Point::new(
            draws.range(rect.low.x, rect.high.x),
            draws.range(rect.low.y, rect.high.y),
        );
        assert!(
            holdings.contains(&located(&lattice, at)),
            "{at:?} uncovered"
        );
    }
    assert!(holdings.iter().all(|&holding| lattice
        .bounds(holding)
        .is_some_and(|bounds| bounds.overlaps(rect))));
}
