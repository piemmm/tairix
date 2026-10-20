//! Card vocabulary tests.

use super::*;

#[test]
fn suits_alternate_red_and_black_in_deal_order() {
    let colours = Suit::ALL.map(Suit::colour);
    assert_eq!(
        colours,
        [Colour::Black, Colour::Red, Colour::Red, Colour::Black]
    );
    for (index, suit) in Suit::ALL.into_iter().enumerate() {
        assert_eq!(suit.index(), index);
    }
}

#[test]
fn a_rank_is_ace_to_king_and_nothing_else() {
    assert_eq!(Rank::new(0), None);
    assert_eq!(Rank::new(14), None);
    assert_eq!(Rank::new(1), Some(Rank::ACE));
    assert_eq!(Rank::new(13), Some(Rank::KING));
    assert!(Rank::ACE.is_below(Rank::new(2).expect("two")));
    assert!(!Rank::KING.is_below(Rank::ACE));
}

fn face(suit: Suit, rank: u8) -> Face {
    Face {
        suit,
        rank: Rank::new(rank).expect("a rank"),
    }
}

#[test]
fn the_three_ways_a_card_lies_on_another() {
    let red_six = face(Suit::Hearts, 6);
    let black_seven = face(Suit::Spades, 7);
    let red_seven = face(Suit::Diamonds, 7);
    assert!(red_six.alternates_on(black_seven));
    assert!(!red_six.alternates_on(red_seven), "same colour");
    assert!(!black_seven.alternates_on(red_six), "upwards");
    assert!(red_six.descends_on(red_seven), "any suit descends");
    assert!(face(Suit::Hearts, 6).follows_suit_on(face(Suit::Hearts, 7)));
    assert!(!red_six.follows_suit_on(red_seven));
}

#[test]
fn a_foundation_starts_at_the_ace_and_climbs_by_suit() {
    assert!(face(Suit::Clubs, 1).builds_on(None));
    assert!(!face(Suit::Clubs, 2).builds_on(None));
    assert!(face(Suit::Clubs, 2).builds_on(Some(face(Suit::Clubs, 1))));
    assert!(!face(Suit::Clubs, 2).builds_on(Some(face(Suit::Spades, 1))));
    assert!(!face(Suit::Clubs, 3).builds_on(Some(face(Suit::Clubs, 1))));
}

#[test]
fn a_slot_keeps_its_card_and_its_side() {
    for card in Deck::Spider(SpiderSuits::Four).cards() {
        for up in [false, true] {
            let slot = Slot::new(card, up);
            assert_eq!((slot.card(), slot.up()), (card, up));
            assert_eq!(slot.turned(!up).up(), !up);
            assert_eq!(slot.turned(!up).card(), card);
        }
    }
    assert_eq!(CardId::new(CardId::MOST), None);
}

#[test]
fn every_deck_holds_each_of_its_faces_equally_often() {
    for (deck, suits, copies) in [
        (Deck::Standard, 4, 1),
        (Deck::Spider(SpiderSuits::One), 1, 8),
        (Deck::Spider(SpiderSuits::Two), 2, 4),
        (Deck::Spider(SpiderSuits::Four), 4, 2),
    ] {
        let mut seen = [[0_u32; 13]; 4];
        for card in deck.cards() {
            let face = deck.face(card);
            seen[face.suit.index()][usize::from(face.rank.get() - 1)] += 1;
        }
        let used = seen
            .iter()
            .filter(|ranks| ranks.iter().any(|&n| n > 0))
            .count();
        assert_eq!(used, suits, "{deck:?}");
        for ranks in seen.iter().filter(|ranks| ranks[0] > 0) {
            assert!(ranks.iter().all(|&n| n == copies), "{deck:?} {ranks:?}");
        }
    }
}

#[test]
fn a_standard_face_names_its_own_card() {
    for card in Deck::Standard.cards() {
        assert_eq!(Deck::standard(Deck::Standard.face(card)), card);
    }
}
