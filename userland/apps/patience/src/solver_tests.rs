//! Solver tests: every win it claims is played out to the end, every loss it
//! claims is a position that truly has none, and it never guesses past its
//! budget.

use alloc::vec::Vec;

use super::*;
use crate::card::{CardId, Face, SpiderSuits, Suit};
use crate::deal::DealNumber;
use crate::score::Scoring;
use crate::table::Table;
use crate::variant::{Draw, Rules, Variant};

/// A budget every deal below is known to be proven within.
const SMALL: u32 = 4_000;

fn dealt(variant: Variant, number: u32) -> Table {
    let number = DealNumber::new(variant, number).expect("a deal");
    Table::deal(Scoring::Standard.rules(variant), number).0
}

/// Play `line` out on `table`, answering whether every step was allowed and
/// it ended won.
fn wins(table: &Table, line: &[Action]) -> bool {
    let mut table = table.clone();
    line.iter().all(|&action| table.apply(action).is_some()) && table.won()
}

#[test]
fn every_claimed_win_plays_out_to_a_won_table() {
    let mut solver = Solver::new();
    for (variant, numbers) in [
        (Variant::Klondike(Draw::One), [2, 8, 19]),
        (Variant::Klondike(Draw::Three), [3, 7, 13]),
        (Variant::FreeCell, [1, 3, 28]),
        (Variant::Spider(SpiderSuits::One), [12, 24, 30]),
    ] {
        for number in numbers {
            let table = dealt(variant, number);
            let Verdict::Won(line) = solver.solve(&table, SMALL) else {
                panic!("{variant:?} deal {number} was proven before");
            };
            assert!(wins(&table, &line), "{variant:?} deal {number}");
        }
    }
}

#[test]
fn vegas_limits_are_respected_by_the_lines_it_finds() {
    let variant = Variant::Klondike(Draw::Three);
    let number = DealNumber::new(variant, 3).expect("a deal");
    let table = Table::deal(Scoring::Vegas.rules(variant), number).0;
    if let Verdict::Won(line) = Solver::new().solve(&table, SMALL) {
        let recycles = line.iter().filter(|&&a| a == Action::Recycle).count();
        assert!(
            recycles <= 2,
            "drawing three at Vegas turns the waste twice"
        );
        assert!(wins(&table, &line));
    }
}

fn card(suit: Suit, rank: u8) -> CardId {
    crate::card::Deck::standard(Face {
        suit,
        rank: Rank::new(rank).expect("a rank"),
    })
}

/// A full FreeCell table with no move at all: four kings in the cells, and
/// eight black cards on top that cannot go home and cannot go on each other.
fn stuck() -> Table {
    let rules = Rules::of(Variant::FreeCell);
    let mut table = dealt(Variant::FreeCell, 1);
    let shape = table.shape();
    let mut rest: Vec<CardId> = crate::card::Deck::Standard.cards().collect();
    let kings = Suit::ALL.map(|suit| card(suit, 13));
    let tops = [
        card(Suit::Clubs, 2),
        card(Suit::Spades, 2),
        card(Suit::Clubs, 4),
        card(Suit::Spades, 4),
        card(Suit::Clubs, 6),
        card(Suit::Spades, 6),
        card(Suit::Clubs, 8),
        card(Suit::Spades, 8),
    ];
    rest.retain(|c| !kings.contains(c) && !tops.contains(c));
    let mut piles = alloc::vec![Vec::new(); shape.piles()];
    for (cell, king) in shape.cells.iter().zip(kings) {
        piles[cell.index()].push(Slot::new(king, true));
    }
    for (index, column) in shape.tableau.iter().enumerate() {
        let below: Vec<Slot> = rest
            .iter()
            .skip(index * 5)
            .take(5)
            .map(|&c| Slot::new(c, true))
            .collect();
        piles[column.index()].extend(below);
        piles[column.index()].push(Slot::new(tops[index], true));
    }
    table = Table::laid(rules, piles);
    table
}

#[test]
fn a_table_with_no_move_is_proven_lost() {
    let table = stuck();
    let mut actions = Vec::new();
    table.actions(&mut actions);
    assert!(actions.is_empty(), "the fixture has no move: {actions:?}");
    assert_eq!(Solver::new().solve(&table, SMALL), Verdict::Lost);
}

#[test]
fn a_budget_spent_answers_unknown_rather_than_guessing() {
    let table = dealt(Variant::Spider(SpiderSuits::Four), 1);
    assert_eq!(Solver::new().solve(&table, 50), Verdict::Unknown);
}

#[test]
fn the_same_position_gets_the_same_answer() {
    let table = dealt(Variant::FreeCell, 3);
    let mut solver = Solver::new();
    let first = solver.solve(&table, SMALL);
    assert!(matches!(first, Verdict::Won(_)));
    assert_eq!(
        solver.solve(&table, SMALL),
        first,
        "a reused solver forgets the last search"
    );
    assert_eq!(Solver::new().solve(&table, SMALL), first);
}

#[test]
fn a_won_table_needs_no_search() {
    let mut table = dealt(Variant::FreeCell, 3);
    let Verdict::Won(line) = Solver::new().solve(&table, SMALL) else {
        panic!("deal 3 was proven before");
    };
    for &action in &line {
        table.apply(action).expect("legal");
    }
    assert_eq!(Solver::new().solve(&table, 1), Verdict::Won(Vec::new()));
}

#[test]
fn a_pointless_swap_is_never_tried() {
    let table = dealt(Variant::FreeCell, 3);
    let shape = table.shape();
    let (first, second) = (
        shape.cells.nth(0).expect("a cell"),
        shape.cells.nth(1).expect("a cell"),
    );
    let swap = Action::Move {
        from: first,
        to: second,
        count: 1,
    };
    assert_eq!(promise(&table, swap), None);
}

#[test]
fn a_long_search_gives_its_memory_back() {
    let mut solver = Solver::new();
    let table = dealt(Variant::Spider(SpiderSuits::Four), 3);
    let _ = solver.solve(&table, 60_000);
    assert!(solver.seen.capacity() <= RETAINED);
    assert!(solver.pool.capacity() <= RETAINED && solver.frames.capacity() <= RETAINED);
    let short = dealt(Variant::FreeCell, 1);
    assert!(
        matches!(solver.solve(&short, SMALL), Verdict::Won(_)),
        "and searches as well after"
    );
}
