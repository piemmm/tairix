use super::*;
use crate::network::{Joins, Placed};
use crate::route::Station;
use crate::testing::Flat;

const KEY: Key = Key::new(88);

fn street(from: Point, to: Point) -> Line {
    let count = 160;
    Line {
        stations: (0..=count)
            .map(|index| Station {
                at: from.lerp(to, f64::from(index) / f64::from(count)),
                level: 10.0,
                width: 5.5,
                water: None,
            })
            .collect(),
    }
}

fn id(a: i32) -> WayId {
    WayId {
        rank: Rank::Road,
        joins: Joins::Ends(
            Placed::Settled(Settled::Village(a, 0)),
            Placed::Settled(Settled::Village(a + 1, 0)),
        ),
    }
}

#[test]
fn a_villages_plots_line_its_streets_and_keep_clear_of_each_other_and_what_bars_them() {
    let village = Settlement {
        settled: Settled::Village(0, 0),
        at: Point::new(0.0, 0.0),
    };
    let main = street(Point::new(-400.0, 0.0), Point::new(400.0, 0.0));
    let cross = street(Point::new(30.0, -400.0), Point::new(-20.0, 400.0));
    let streets = [(id(0), &main), (id(5), &cross)];
    let barred = [crate::farm::rectangle(Point::new(120.0, 40.0), Point::new(1.0, 0.0), (40.0, 40.0))];
    let laid = lay_out(KEY, &village, (&streets, &barred), &Flat).expect("room");
    assert!(laid.plots.len() > 12, "{} plots", laid.plots.len());
    for (index, plot) in laid.plots.iter().enumerate() {
        for other in &laid.plots[index + 1..] {
            assert!(!plot.outline.overlaps(&other.outline));
        }
        assert!(!plot.outline.overlaps(&barred[0]));
        assert!(laid.green.as_ref().is_none_or(|green| !green.overlaps(&plot.outline)));
        let house = plot.house.outline();
        assert!(house.corners.iter().all(|&corner| plot.outline.contains(corner)));
        let line = if plot.street == id(0) { &main } else { &cross };
        let fronting = line.nearest(plot.gate).expect("a street").0.distance;
        assert!(fronting < 0.5 * 5.5 + 1.5, "{fronting} from its street");
        for (other, other_line) in streets {
            if other != plot.street {
                assert!(plot
                    .outline
                    .corners
                    .iter()
                    .all(|&corner| other_line.nearest(corner).expect("a street").0.distance > 0.5 * 5.5));
            }
        }
    }
}
