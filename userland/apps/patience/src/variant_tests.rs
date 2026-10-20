//! Variant and pile-shape tests.

use super::*;

#[test]
fn every_variant_is_spelled_and_numbered_once() {
    for (index, variant) in Variant::ALL.into_iter().enumerate() {
        assert_eq!(variant.index(), index);
        assert_eq!(Variant::named(variant.name()), Some(variant));
        assert!(!variant.label().is_empty());
    }
    assert_eq!(Variant::named("pyramid"), None);
}

#[test]
fn each_shape_numbers_its_piles_without_gaps_or_overlaps() {
    for (variant, piles) in [
        (Variant::Klondike(Draw::One), 13),
        (Variant::FreeCell, 16),
        (Variant::Spider(SpiderSuits::Two), 19),
    ] {
        let shape = variant.shape();
        assert_eq!(shape.piles(), piles, "{variant:?}");
        assert!(shape.piles() <= PileId::MOST);
        for pile in shape.all() {
            let roles = [
                shape.stock == Some(pile),
                shape.waste == Some(pile),
                shape.cells.contains(pile),
                shape.foundations.contains(pile),
                shape.tableau.contains(pile),
            ];
            assert_eq!(roles.iter().filter(|&&r| r).count(), 1, "{pile:?}");
            assert!(shape.role(pile).is_some());
        }
    }
}

#[test]
fn a_run_of_piles_counts_from_its_first() {
    let columns = Variant::FreeCell.shape().tableau;
    assert_eq!(columns.len(), 8);
    let third = columns.nth(2).expect("a third column");
    assert_eq!(columns.position(third), Some(2));
    assert_eq!(columns.nth(8), None);
    assert!(Piles::EMPTY.is_empty());
    assert_eq!(columns.iter().count(), 8);
}

#[test]
fn a_waste_limit_counts_turns() {
    assert!(Recycles::Unlimited.allow(200));
    assert!(!Recycles::AtMost(0).allow(0));
    assert!(Recycles::AtMost(2).allow(1));
    assert!(!Recycles::AtMost(2).allow(2));
}
