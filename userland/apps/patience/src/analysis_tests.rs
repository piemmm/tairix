//! Analysis tests: the answers name the position they are about, a rewind
//! never goes back to a position it has not proven, and a deal search moves
//! past a deal it cannot win.

use alloc::vec::Vec;

use super::*;
use crate::score::Scoring;
use crate::variant::Variant;

fn freecell(number: u32) -> (Rules, DealNumber, Table) {
    let rules = Scoring::Standard.rules(Variant::FreeCell);
    let number = DealNumber::new(Variant::FreeCell, number).expect("a deal");
    (rules, number, Table::deal(rules, number).0)
}

#[test]
fn halving_finds_the_last_step_that_holds() {
    for len in 0..40 {
        for boundary in 0..=len {
            let mut probes = 0;
            let found = last_holding(len, false, |steps| {
                probes += 1;
                steps <= boundary
            });
            assert_eq!(found, Some(boundary), "{len}, {boundary}");
            // One probe of the start, then one per halving.
            let halvings = usize::BITS - len.leading_zeros();
            assert!(
                probes <= 2 + usize::try_from(halvings).expect("small"),
                "{probes} for {len}"
            );
        }
    }
}

#[test]
fn halving_keeps_nothing_when_even_the_start_fails() {
    assert_eq!(last_holding(10, false, |_| false), None);
    let mut asked = Vec::new();
    let kept = last_holding(10, true, |steps| {
        asked.push(steps);
        false
    });
    assert_eq!(kept, Some(0), "a start known to hold is kept");
    assert!(!asked.contains(&0), "and is not asked about again");
}

#[test]
fn a_probe_that_fails_spuriously_only_keeps_fewer_steps() {
    // Whatever holds is truly winnable; a probe that fails where the truth
    // holds can move the answer down, never past the true boundary.
    let truth = 13;
    let kept = last_holding(20, false, |steps| steps <= truth && steps != 7);
    assert!(kept.is_some_and(|kept| kept <= truth));
}

#[test]
fn a_position_answer_names_the_position_asked_about() {
    let (_, _, table) = freecell(1);
    let mut solver = Solver::new();
    let mut question = Question::Position {
        table: table.clone(),
        budget: HINT_BUDGET,
    };
    let Finding::Position {
        fingerprint,
        verdict,
    } = study(&mut solver, &mut question)
    else {
        panic!("a position's answer");
    };
    assert_eq!(fingerprint, table.fingerprint());
    let Verdict::Won(line) = verdict else {
        panic!("deal 1 is won");
    };
    let mut played = table;
    assert!(line.iter().all(|&action| played.apply(action).is_some()));
    assert!(played.won());
}

#[test]
fn a_rewind_along_a_winning_line_keeps_all_of_it() {
    let (_, _, table) = freecell(1);
    let mut solver = Solver::new();
    let Verdict::Won(winning) = solver.solve(&table, HINT_BUDGET) else {
        panic!("deal 1 is won");
    };
    let line: Vec<Action> = winning.iter().copied().take(winning.len() / 2).collect();
    let mut ended = table.clone();
    for &action in &line {
        let _ = ended.apply(action);
    }
    let mut question = Question::Rewind {
        start: table,
        line: line.clone(),
        winnable: true,
    };
    assert_eq!(
        study(&mut solver, &mut question),
        Finding::Rewind {
            fingerprint: ended.fingerprint(),
            keep: Some(line.len()),
        }
    );
}

#[test]
fn a_rewind_from_a_deal_no_one_can_win_keeps_nothing() {
    // Microsoft's deal 11982 has no winning line at all.
    let (_, _, table) = freecell(11_982);
    let mut actions = Vec::new();
    table.actions(&mut actions);
    let line: Vec<Action> = actions.into_iter().take(1).collect();
    let mut solver = Solver::new();
    let mut question = Question::Rewind {
        start: table,
        line,
        winnable: false,
    };
    let Finding::Rewind { keep, .. } = study(&mut solver, &mut question) else {
        panic!("a rewind's answer");
    };
    assert_eq!(keep, None);
}

#[test]
fn a_deal_search_keeps_a_winnable_deal_and_passes_one_it_cannot_win() {
    let mut solver = Solver::new();
    let (rules, first, _) = freecell(1);
    let mut search = DealSearch {
        rules,
        first,
        waited: true,
    };
    assert_eq!(
        find_deal(&mut solver, &mut search),
        DealFound {
            rules,
            number: first,
            proven: true,
        }
    );
    let (rules, first, _) = freecell(11_982);
    let mut search = DealSearch {
        rules,
        first,
        waited: true,
    };
    let found = find_deal(&mut solver, &mut search);
    assert!(found.proven);
    assert_eq!(found.number, first.next(Variant::FreeCell));
    assert_eq!(found.rules, rules);
}

/// A search over FreeCell from deal 1, whose deals answer `verdicts` in turn
/// and are otherwise unsettled.
fn settled(waited: bool, verdicts: &[Verdict]) -> (u32, bool, usize) {
    let (rules, first, _) = freecell(1);
    let search = DealSearch {
        rules,
        first,
        waited,
    };
    let mut asked = 0;
    let (number, proven) = settle_on(&search, |_| {
        asked += 1;
        verdicts.get(asked - 1).cloned().unwrap_or(Verdict::Unknown)
    });
    (number.get(), proven, asked)
}

#[test]
fn a_deal_search_never_settles_on_a_deal_proven_lost() {
    let won = Verdict::Won(Vec::new());
    assert_eq!(settled(true, &[Verdict::Lost, won.clone()]), (2, true, 2));
    assert_eq!(
        settled(
            true,
            &[
                Verdict::Lost,
                Verdict::Unknown,
                Verdict::Lost,
                Verdict::Unknown
            ]
        ),
        (2, false, 4)
    );
    let lost: Vec<Verdict> = (0..DEAL_ATTEMPTS).map(|_| Verdict::Lost).collect();
    let (number, proven, asked) = settled(true, &lost);
    assert!(
        !proven && number == DEAL_ATTEMPTS + 1,
        "past every deal proven lost"
    );
    assert_eq!(asked, usize::try_from(DEAL_ATTEMPTS).expect("small"));
}

#[test]
fn a_waited_search_gives_up_sooner_than_one_made_ahead() {
    let (number, proven, asked) = settled(true, &[]);
    let count = |n: u32| usize::try_from(n).expect("small");
    assert_eq!((number, proven, asked), (1, false, count(UNSETTLED_WAITED)));
    let (number, proven, asked) = settled(false, &[]);
    assert_eq!((number, proven, asked), (1, false, count(UNSETTLED_AHEAD)));
    let won = Verdict::Won(Vec::new());
    let late = [Verdict::Unknown, Verdict::Unknown, Verdict::Unknown, won];
    assert_eq!(
        settled(false, &late),
        (4, true, 4),
        "ahead of time, a fourth deal is still tried"
    );
}
