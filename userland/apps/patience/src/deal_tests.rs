//! Deal tests: Microsoft's FreeCell numbering exactly, and seeded shuffles
//! that are permutations of their deck.

use alloc::string::String;
use alloc::vec::Vec;

use super::*;
use crate::card::{Deck, SpiderSuits};
use crate::variant::Draw;

/// A FreeCell deal written as its tableau rows, the way every list of
/// Microsoft's deals prints them.
fn rows(number: u32) -> Vec<String> {
    let deal = DealNumber::new(Variant::FreeCell, number).expect("a deal");
    let cards = shuffled(Variant::FreeCell, deal);
    cards
        .chunks(8)
        .map(|row| {
            row.iter()
                .map(|&card| crate::fixture::spell(Deck::Standard.face(card)))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

#[test]
fn freecell_deal_one_is_microsofts_deal_one() {
    assert_eq!(
        rows(1),
        [
            "JD 2D 9H JC 5D 7H 7C 5H",
            "KD KC 9S 5S AD QC KH 3H",
            "2S KS 9D QD JS AS AH 3C",
            "4C 5C TS QH 4H AC 4D 7S",
            "3S TD 4S TH 8H 2C JH 7D",
            "6D 8S 8D QS 6C 3D 8C TC",
            "6S 9C 2H 6H",
        ]
    );
}

#[test]
fn freecell_deals_617_and_11982_are_microsofts() {
    assert_eq!(
        rows(617),
        [
            "7D AD 5C 3S 5S 8C 2D AH",
            "TD 7S QD AC 6D 8H AS KH",
            "TH QC 3H 9D 6S 8D 3D TC",
            "KD 5H 9S 3C 8S 7H 4D JS",
            "4C QS 9C 9H 7C 6H 2C 2S",
            "4S TS 2H 5D JC 6C JH QH",
            "JD KS KC 4H",
        ]
    );
    assert_eq!(rows(11982)[0], "AH AS 4H AC 2D 6S TS JS");
}

fn is_permutation(deck: Deck, cards: &[CardId]) -> bool {
    let mut seen = alloc::vec![false; deck.count()];
    cards.len() == deck.count()
        && cards
            .iter()
            .all(|card| !core::mem::replace(&mut seen[card.index()], true))
}

#[test]
fn every_shuffle_deals_each_card_once() {
    for variant in Variant::ALL {
        for number in [1, 2, 9_999, DealNumber::most(variant)] {
            let deal = DealNumber::new(variant, number).expect("in range");
            assert!(
                is_permutation(variant.deck(), &shuffled(variant, deal)),
                "{variant:?} {number}"
            );
        }
    }
}

#[test]
fn a_number_is_the_same_deal_every_time_and_another_number_is_not() {
    let klondike = Variant::Klondike(Draw::One);
    let deal = |n| shuffled(klondike, DealNumber::new(klondike, n).expect("a deal"));
    assert_eq!(deal(42), deal(42));
    assert_ne!(deal(42), deal(43));
    let spider = Variant::Spider(SpiderSuits::Four);
    let one_suit = Variant::Spider(SpiderSuits::One);
    let n = DealNumber::new(spider, 42).expect("a deal");
    assert_ne!(
        shuffled(spider, n),
        shuffled(one_suit, n),
        "each variant's numbers are its own"
    );
}

#[test]
fn deal_numbers_stay_in_range() {
    assert_eq!(DealNumber::new(Variant::FreeCell, 0), None);
    assert_eq!(DealNumber::new(Variant::FreeCell, FREECELL_DEALS + 1), None);
    assert!(DealNumber::new(Variant::FreeCell, FREECELL_DEALS).is_some());
    let last = DealNumber::new(Variant::FreeCell, FREECELL_DEALS).expect("the last");
    assert_eq!(last.next(Variant::FreeCell).get(), 1);
    let mut rng = tairix_rng::NonCryptoRng::seed_from_u64(7);
    for variant in Variant::ALL {
        for _ in 0..200 {
            let number = DealNumber::random(variant, &mut rng).get();
            assert!((1..=DealNumber::most(variant)).contains(&number));
        }
    }
}
