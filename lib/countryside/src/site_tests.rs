use super::*;
use crate::testing::Hills;

const TIER: Villages = Villages {
    spacing: 1500.0,
    exclusion: 0.8,
    gathers: 0.25,
};

fn key() -> Key {
    Key::new(77)
}

#[test]
fn no_two_villages_stand_nearer_than_their_exclusion_and_none_on_water() {
    let villages = villages(key(), &TIER, &Hills, Rect::around(Point::new(0.0, 0.0), 12_000.0)).expect("room");
    assert!(villages.len() > 20, "{} villages", villages.len());
    for (index, a) in villages.iter().enumerate() {
        assert!(!ground::wet(&Hills, a.at));
        for b in &villages[index + 1..] {
            assert!((a.at - b.at).length() >= TIER.exclusion * TIER.spacing, "{a:?} {b:?}");
        }
    }
}

#[test]
fn villages_laid_in_pieces_are_those_laid_whole() {
    let whole = villages(key(), &TIER, &Hills, Rect::around(Point::new(0.0, 0.0), 6000.0)).expect("room");
    let mut pieces = Vec::new();
    for (x, y) in [(-3000.0, -3000.0), (3000.0, -3000.0), (-3000.0, 3000.0), (3000.0, 3000.0)] {
        let piece = Rect::around(Point::new(x, y), 3000.0);
        for village in villages(key(), &TIER, &Hills, piece).expect("room") {
            if !pieces.contains(&village) {
                pieces.push(village);
            }
        }
    }
    pieces.sort_unstable_by_key(|village| village.settled);
    let mut whole = whole;
    whole.sort_unstable_by_key(|village| village.settled);
    assert_eq!(pieces, whole);
}

#[test]
fn a_holding_a_village_gathers_has_no_farmstead_and_any_other_stands_in_its_own_land() {
    let lattice = Lattice::new(350.0, key());
    let rect = Rect::around(Point::new(0.0, 0.0), 5000.0);
    let villages = villages(key(), &TIER, &Hills, rect.grown(2000.0)).expect("room");
    let gathers = TIER.gathers * TIER.spacing;
    let (mut farms, mut gathered) = (0, 0);
    for holding in lattice.holdings_over(rect).expect("room") {
        let middle = lattice.vertex(holding);
        let farm = farmstead(key(), &lattice, &Hills, (holding, &villages, gathers));
        if gatherer(&villages, middle, gathers).is_some() {
            assert_eq!(farm, None);
            gathered += 1;
        } else if let Some(farm) = farm {
            assert_eq!(farm.settled, Settled::Farmstead(holding));
            assert!(lattice.outline(holding).contains(farm.at));
            assert!(!ground::wet(&Hills, farm.at));
            farms += 1;
        }
    }
    assert!(gathered > 0 && farms > 100, "{gathered} gathered, {farms} farms");
}
