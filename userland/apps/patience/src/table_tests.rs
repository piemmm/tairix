//! Table rule tests.

use alloc::vec::Vec;

use tairix_rng::{NonCryptoRng, RandU64};

use super::*;
use crate::card::SpiderSuits;
use crate::fixture::position;
use crate::variant::{Draw, Recycles};

fn pile(index: usize) -> PileId {
    PileId::new(index).expect("a pile")
}

fn moving(from: usize, to: usize, count: u8) -> Action {
    Action::Move {
        from: pile(from),
        to: pile(to),
        count,
    }
}

const KLONDIKE: Variant = Variant::Klondike(Draw::One);
const SPIDER: Variant = Variant::Spider(SpiderSuits::Four);

#[test]
fn klondike_deals_a_staircase_of_seven_columns_and_a_stock_of_24() {
    let (table, order) = Table::deal(
        Rules::of(KLONDIKE),
        DealNumber::new(KLONDIKE, 5).expect("deal"),
    );
    let shape = table.shape();
    for (index, column) in shape.tableau.iter().enumerate() {
        let cards = table.pile(column);
        assert_eq!(cards.len(), index + 1);
        assert!(cards.last().expect("a top").up());
        assert!(cards.iter().rev().skip(1).all(|slot| !slot.up()));
    }
    let stock = table.pile(shape.stock.expect("a stock"));
    assert_eq!(stock.len(), 24);
    assert!(stock.iter().all(|slot| !slot.up()));
    assert_eq!(order.len(), 28, "the deal order names every card dealt out");
}

#[test]
fn freecell_deals_every_card_face_up_into_eight_columns() {
    let (table, _) = Table::deal(
        Rules::of(Variant::FreeCell),
        DealNumber::new(Variant::FreeCell, 1).expect("deal"),
    );
    let lengths: Vec<usize> = table
        .shape()
        .tableau
        .iter()
        .map(|c| table.pile(c).len())
        .collect();
    assert_eq!(lengths, [7, 7, 7, 7, 6, 6, 6, 6]);
    assert_eq!(table.face_down(), 0);
}

#[test]
fn spider_deals_fifty_four_cards_and_keeps_fifty_back() {
    let (table, order) = Table::deal(Rules::of(SPIDER), DealNumber::new(SPIDER, 3).expect("deal"));
    let shape = table.shape();
    let lengths: Vec<usize> = shape.tableau.iter().map(|c| table.pile(c).len()).collect();
    assert_eq!(lengths, [6, 6, 6, 6, 5, 5, 5, 5, 5, 5]);
    assert_eq!(table.pile(shape.stock.expect("a stock")).len(), 50);
    assert_eq!(table.face_down(), 50 + 44);
    assert_eq!(order.len(), 54);
}

/// Every action taken back leaves the table exactly as it was, across long
/// random games of every variant, completions and recycles included.
#[test]
fn every_action_is_undone_exactly() {
    let mut rng = NonCryptoRng::seed_from_u64(0x9a7_1e9c);
    let mut actions = Vec::new();
    for variant in Variant::ALL {
        for number in 1..=6 {
            let rules = crate::score::Scoring::Vegas.rules(variant);
            let (mut table, _) =
                Table::deal(rules, DealNumber::new(variant, number).expect("deal"));
            let start = table.clone();
            let mut done = Vec::new();
            let mut positions = alloc::vec![table.clone()];
            for _ in 0..300 {
                table.actions(&mut actions);
                if actions.is_empty() {
                    break;
                }
                let pick = usize::try_from(rng.next_below(actions.len() as u64)).expect("index");
                let action = actions[pick];
                assert!(table.allows(action), "{variant:?} offered {action:?}");
                let applied = table.apply(action).expect("an offered action applies");
                done.push(applied);
                positions.push(table.clone());
            }
            while let Some(applied) = done.pop() {
                positions.pop();
                table.undo(&applied);
                assert_eq!(
                    &table,
                    positions.last().expect("a position"),
                    "{variant:?} {number}"
                );
            }
            assert_eq!(table, start);
        }
    }
}

#[test]
fn a_refused_action_leaves_the_table_untouched() {
    let mut table = position(KLONDIKE, &[(6, "KS"), (7, "QS")]);
    let before = table.clone();
    assert_eq!(table.apply(moving(7, 6, 1)), None, "same colour");
    assert_eq!(table.apply(moving(6, 6, 1)), None, "onto itself");
    assert_eq!(table.apply(moving(7, 6, 2)), None, "more than it holds");
    assert_eq!(
        table.apply(moving(18, 6, 1)),
        None,
        "not one of Klondike's piles"
    );
    assert_eq!(table, before);
}

#[test]
fn klondike_builds_down_in_alternating_colours_and_opens_a_column_only_to_a_king() {
    let mut table = position(KLONDIKE, &[(6, "#2C QH"), (7, "JS"), (8, "")]);
    assert!(table.allows(moving(7, 6, 1)), "black jack on red queen");
    assert!(!table.allows(moving(7, 8, 1)), "only a king opens a column");
    let applied = table.apply(moving(6, 8, 1));
    assert_eq!(applied, None, "a queen does not open one either");
    let mut kings = position(KLONDIKE, &[(6, "#2C KH"), (8, "")]);
    let applied = kings
        .apply(moving(6, 8, 1))
        .expect("a king moves to an empty column");
    assert!(applied.turned, "the card it uncovered is turned");
    assert!(kings.top(pile(6)).expect("a card").up());
}

#[test]
fn klondike_draws_three_with_the_last_turned_on_top_and_turns_the_waste_back_reversed() {
    let mut table = position(Variant::Klondike(Draw::Three), &[(0, "#AC #2C #3C #4C")]);
    table.rules.recycles = Recycles::AtMost(1);
    let applied = table.apply(Action::Draw).expect("a draw");
    assert_eq!(applied.drawn, 3);
    let waste = table.pile(pile(1));
    let shown: Vec<u8> = waste
        .iter()
        .map(|s| table.face(s.card()).rank.get())
        .collect();
    assert_eq!(shown, [4, 3, 2], "the third card turned lies on top");
    assert!(waste.iter().all(|s| s.up()));
    let _ = table.apply(Action::Draw).expect("the last card");
    assert!(table.allows(Action::Recycle));
    let _ = table.apply(Action::Recycle).expect("turned back");
    let stock: Vec<u8> = table
        .pile(pile(0))
        .iter()
        .map(|s| table.face(s.card()).rank.get())
        .collect();
    assert_eq!(stock, [1, 2, 3, 4], "the first card drawn is on top again");
    let _ = table.apply(Action::Draw);
    let _ = table.apply(Action::Draw);
    assert!(!table.allows(Action::Recycle), "the limit is reached");
}

#[test]
fn klondike_may_bring_a_card_back_from_home_but_freecell_may_not() {
    let klondike = position(KLONDIKE, &[(2, "AH 2H"), (6, "3S")]);
    assert!(klondike.allows(moving(2, 6, 1)));
    let freecell = position(Variant::FreeCell, &[(4, "AH 2H"), (8, "3S")]);
    assert!(!freecell.allows(moving(4, 8, 1)));
}

#[test]
fn freecell_moves_as_many_cards_as_its_free_cells_and_columns_allow() {
    // Four free cells and six empty columns besides the target: 5 × 2⁶.
    let roomy = position(Variant::FreeCell, &[(8, "9S 8H 7C 6D"), (9, "TH")]);
    assert_eq!(roomy.capacity(pile(9)), 5 * 64);
    assert!(
        roomy.allows(moving(8, 9, 4)),
        "the nine leads the run onto the ten"
    );
    // One free cell and no empty column: two cards at a time.
    let crowded = position(
        Variant::FreeCell,
        &[
            (0, "AC"),
            (1, "AD"),
            (2, "AS"),
            (8, "9S 8H 7C 6D"),
            (9, "TH"),
            (10, "2C"),
            (11, "2D"),
            (12, "2S"),
            (13, "2H"),
            (14, "3C"),
            (15, "3D"),
        ],
    );
    assert_eq!(crowded.capacity(pile(9)), 2);
    assert!(
        !crowded.allows(moving(8, 9, 4)),
        "four cards need more room"
    );
}

#[test]
fn a_freecell_cell_holds_one_card() {
    let mut table = position(Variant::FreeCell, &[(8, "KS QH")]);
    assert!(table.apply(moving(8, 0, 1)).is_some());
    assert!(!table.allows(moving(8, 0, 1)), "the cell is taken");
    assert!(!table.allows(moving(8, 1, 2)), "a cell takes one card");
}

#[test]
fn spider_moves_only_runs_of_one_suit_but_builds_on_any() {
    let broken = position(SPIDER, &[(9, "8S 7S 6H"), (10, "9D"), (11, "7H")]);
    assert_eq!(broken.movable(pile(9)), 1, "the heart breaks the spade run");
    assert!(broken.allows(moving(9, 11, 1)), "a six lands on any seven");
    assert!(
        !broken.allows(moving(9, 10, 3)),
        "a broken run does not move as one"
    );
    let run = position(SPIDER, &[(9, "8S 7S 6S"), (10, "9D")]);
    assert_eq!(run.movable(pile(9)), 3);
    assert!(run.allows(moving(9, 10, 3)), "any suit takes the run");
}

#[test]
fn a_completed_spider_run_goes_home_and_uncovers_its_column() {
    let mut table = position(
        SPIDER,
        &[(9, "#4D KS QS JS TS 9S 8S 7S 6S 5S 4S 3S 2S"), (10, "AS")],
    );
    let applied = table
        .apply(moving(10, 9, 1))
        .expect("the ace completes the run");
    let completed: Vec<Completion> = applied.completions().collect();
    assert_eq!(completed.len(), 1);
    assert!(completed[0].turned);
    assert_eq!(table.pile(completed[0].foundation).len(), 13);
    assert_eq!(table.pile(pile(9)).len(), 1);
    assert!(table.top(pile(9)).expect("the four").up());
    let mut undone = table.clone();
    undone.undo(&applied);
    assert_eq!(undone.pile(pile(9)).len(), 13);
    assert!(!undone.pile(pile(9))[0].up(), "turned back down");
}

#[test]
fn spider_will_not_deal_onto_an_empty_column() {
    let mut piles = alloc::vec![(0, "#AS #2S #3S #4S #5S #6S #7S #8S #9S #TS")];
    let tops = ["KH", "KD", "KS", "KC", "QH", "QD", "QS", "QC", "JH"];
    for (column, top) in (9..18).zip(tops) {
        piles.push((column, top));
    }
    let gap = position(SPIDER, &piles);
    assert!(!gap.allows(Action::Draw));
    piles.push((18, "JD"));
    let full = position(SPIDER, &piles);
    assert!(full.allows(Action::Draw));
}

#[test]
fn only_a_card_no_later_play_could_want_goes_home_unasked() {
    let safe = position(
        KLONDIKE,
        &[(2, "AS 2S"), (3, "AC 2C"), (4, "AH"), (6, "3H")],
    );
    assert_eq!(
        safe.safe_home(true),
        None,
        "the two of hearts is not home yet"
    );
    let ready = position(
        KLONDIKE,
        &[(2, "AS 2S"), (3, "AC 2C"), (4, "AH 2H"), (6, "3H")],
    );
    assert_eq!(ready.safe_home(true), Some(moving(6, 4, 1)));
    let waste = position(Variant::Klondike(Draw::Three), &[(1, "AD")]);
    assert_eq!(
        waste.safe_home(false),
        None,
        "the waste is left alone when told"
    );
    assert!(waste.safe_home(true).is_some());
}

#[test]
fn a_position_with_everything_uncovered_and_ordered_is_settled() {
    let settled = position(KLONDIKE, &[(6, "KS QH"), (7, "KH QS")]);
    assert!(settled.settled());
    let buried = position(KLONDIKE, &[(6, "#KS QH")]);
    assert!(!buried.settled());
    let stocked = position(KLONDIKE, &[(0, "#AS"), (6, "KS QH")]);
    assert!(!stocked.settled());
}

#[test]
fn a_fingerprint_ignores_which_column_is_which_and_nothing_else() {
    let one = position(Variant::FreeCell, &[(8, "KS QH"), (9, "KD"), (0, "5C")]);
    let swapped = position(Variant::FreeCell, &[(9, "KS QH"), (12, "KD"), (3, "5C")]);
    let other = position(Variant::FreeCell, &[(8, "KS"), (9, "KD QH"), (0, "5C")]);
    assert_eq!(one.fingerprint(), swapped.fingerprint());
    assert_ne!(one.fingerprint(), other.fingerprint());
}

#[test]
fn every_action_offered_is_allowed_and_every_allowed_move_is_offered() {
    let mut actions = Vec::new();
    for variant in [KLONDIKE, Variant::FreeCell, SPIDER] {
        let (table, _) = Table::deal(
            Rules::of(variant),
            DealNumber::new(variant, 77).expect("deal"),
        );
        table.actions(&mut actions);
        for &action in &actions {
            assert!(table.allows(action));
        }
        for from in table.shape().all() {
            for to in table.shape().all() {
                for count in 1..=13 {
                    let action = Action::Move { from, to, count };
                    assert_eq!(
                        table.allows(action),
                        actions.contains(&action),
                        "{variant:?} {action:?}"
                    );
                }
            }
        }
    }
}
