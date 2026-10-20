//! What the game asks of the solver, on a worker rather than the loop that
//! owes the window a frame: whether the position in play can still be won and
//! how, the last position of a lost game that could, and a deal that can be.

use alloc::vec::Vec;

use crate::deal::DealNumber;
use crate::solver::{Solver, Verdict};
use crate::table::{Action, Table};
use crate::variant::Rules;

/// Positions a search after each move may visit: enough to prove most
/// positions of a game in play either way, few enough to cost a fraction of a
/// second.
pub const MOVE_BUDGET: u32 = 60_000;
/// Positions a search for a hint asked for may visit.
pub const HINT_BUDGET: u32 = 160_000;
/// Positions each probe of a rewind may visit.
pub const REWIND_BUDGET: u32 = 40_000;
/// Positions each candidate deal may visit.
pub const DEAL_BUDGET: u32 = 120_000;
/// The most deals a search tries, however many it proves lost.
pub const DEAL_ATTEMPTS: u32 = 24;
/// Deals a search the player is waiting on leaves unsettled before it deals
/// the first of them, unchecked.
pub const UNSETTLED_WAITED: u32 = 2;
/// The same for a search made ahead of time, which no one is waiting on.
pub const UNSETTLED_AHEAD: u32 = 6;

/// A question for the solver.
#[derive(Clone, Debug)]
pub enum Question {
    /// Can `table` still be won, and how?
    Position {
        /// The position.
        table: Table,
        /// The positions the search may visit.
        budget: u32,
    },
    /// How far along `line`, played from `start`, does the game stay
    /// winnable?
    Rewind {
        /// The deal as dealt.
        start: Table,
        /// The line played since.
        line: Vec<Action>,
        /// Whether the deal was proven winnable when it was dealt, so the
        /// search need not prove it again.
        winnable: bool,
    },
}

/// What the solver found.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Finding {
    /// The verdict on the position with this fingerprint.
    Position {
        /// Which position.
        fingerprint: u64,
        /// What the search concluded.
        verdict: Verdict,
    },
    /// The longest prefix of the line, by steps, from whose end the game can
    /// still be won, or `None` when no step of it could be proven so.
    Rewind {
        /// The fingerprint of the position the line ended on.
        fingerprint: u64,
        /// How many steps to keep.
        keep: Option<usize>,
    },
}

/// Answer `question`: what the analysis worker runs.
pub fn study(solver: &mut Solver, question: &mut Question) -> Finding {
    match question {
        Question::Position { table, budget } => Finding::Position {
            fingerprint: table.fingerprint(),
            verdict: solver.solve(table, *budget),
        },
        Question::Rewind {
            start,
            line,
            winnable,
        } => rewind(solver, start, line, *winnable),
    }
}

/// The longest proven-winnable prefix of `line` from `start`.
fn rewind(solver: &mut Solver, start: &Table, line: &[Action], winnable: bool) -> Finding {
    let at = |steps: usize| {
        let mut table = start.clone();
        for &action in line.iter().take(steps) {
            let _ = table.apply(action);
        }
        table
    };
    let fingerprint = at(line.len()).fingerprint();
    let proven = |steps: usize| matches!(solver.solve(&at(steps), REWIND_BUDGET), Verdict::Won(_));
    Finding::Rewind {
        fingerprint,
        keep: last_holding(line.len(), winnable, proven),
    }
}

/// The most steps in `0..=len` for which `holds`, or `None` when it holds
/// for none; `at_start` says it is already known to hold for none taken.
///
/// Whatever can be won after some steps of a line could be won after every
/// fewer, so the boundary is found by halving. A probe the budget cannot
/// settle counts as failing, which can only ever keep fewer steps than the
/// true boundary, never more.
fn last_holding(len: usize, at_start: bool, mut holds: impl FnMut(usize) -> bool) -> Option<usize> {
    if !at_start && !holds(0) {
        return None;
    }
    let (mut low, mut high) = (0, len);
    while low < high {
        let mid = low + (high - low).div_ceil(2);
        if holds(mid) {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    Some(low)
}

/// A search for a deal the solver can show is winnable.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct DealSearch {
    /// The rules it is to be played by.
    pub rules: Rules,
    /// The deal tried first; the ones after it follow in turn.
    pub first: DealNumber,
    /// Whether the player is waiting for it.
    pub waited: bool,
}

/// The deal a search settled on.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct DealFound {
    /// The rules it was searched under.
    pub rules: Rules,
    /// The deal.
    pub number: DealNumber,
    /// Whether the solver proved it winnable; a search that settled none
    /// deals the first it could not settle, and never one proven lost.
    pub proven: bool,
}

/// Find a winnable deal: what the dealing worker runs.
pub fn find_deal(solver: &mut Solver, search: &mut DealSearch) -> DealFound {
    let rules = search.rules;
    let (number, proven) = settle_on(search, |number| {
        let (table, _) = Table::deal(rules, number);
        solver.solve(&table, DEAL_BUDGET)
    });
    DealFound {
        rules,
        number,
        proven,
    }
}

/// The deal `search` settles on, given each deal's `verdict`, and whether it
/// was proven: the first won, else the first unsettled once as many are as
/// the search waits for, else the first not proven lost.
fn settle_on(
    search: &DealSearch,
    mut verdict: impl FnMut(DealNumber) -> Verdict,
) -> (DealNumber, bool) {
    let variant = search.rules.variant;
    let patience = if search.waited {
        UNSETTLED_WAITED
    } else {
        UNSETTLED_AHEAD
    };
    let mut number = search.first;
    let mut unsettled: Option<DealNumber> = None;
    let mut left = patience;
    for _ in 0..DEAL_ATTEMPTS {
        match verdict(number) {
            Verdict::Won(_) => return (number, true),
            Verdict::Unknown => {
                let first = *unsettled.get_or_insert(number);
                left = left.saturating_sub(1);
                if left == 0 {
                    return (first, false);
                }
            }
            Verdict::Lost => {}
        }
        number = number.next(variant);
    }
    (unsettled.unwrap_or(number), false)
}

#[cfg(test)]
#[path = "analysis_tests.rs"]
mod tests;
