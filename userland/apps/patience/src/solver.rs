//! The solver: whether a position can still be won, and how.
//!
//! A depth-first search, every move followed by the safe moves home the game
//! would make anyway, with each position reached remembered by its
//! [`Table::canonical`] fingerprint so that none is searched twice. From each
//! position the moves are tried best first, judged by the position each leads
//! to, so a winnable game is usually proven long before the space is
//! exhausted; the search is bounded by a budget of positions, past which it
//! answers that it does not know rather than spending without end.
//!
//! Klondike's stock is searched a card at a time rather than a draw at a
//! time: every card the stock and waste can bring to hand becomes one move —
//! the turns that reach it, then its play — so the search never wanders the
//! stock's cycle without playing anything.
//!
//! It sees every card, face down or not. That is what lets it say a deal
//! *can* be won before the player has seen it, and it is the game's own
//! knowledge offered as advice: nothing it says is shown as a card.
//!
//! # What an answer means
//!
//! - [`Verdict::Won`] carries a winning line, which is proof.
//! - [`Verdict::Lost`] means every reachable position was searched and none
//!   wins. Positions are told apart by a 64-bit fingerprint, so two that
//!   collide count as one: at a budget's worth of positions the chance of a
//!   collision misleading the search is below one in ten billion.
//! - [`Verdict::Unknown`] means the budget ran out first.

use alloc::vec::Vec;

use tairix_collections::HashSet;
use tairix_hash::BuildFastHash;
use tairix_rng::{NonCryptoRng, RandU64};

use crate::card::{Rank, Slot};
use crate::table::{Action, Applied, Table};
use crate::variant::{Draw, Role, Variant};

/// What a search concluded.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Verdict {
    /// The position wins by this line, every step of it, automatic moves
    /// home and turns of the stock included.
    Won(Vec<Action>),
    /// No line wins.
    Lost,
    /// The budget ran out first.
    Unknown,
}

/// One way on from a position: some turns of the stock, then an action.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Plan {
    turns: u8,
    action: Action,
}

/// One position on the search's path, and the plans from it still to try.
#[derive(Copy, Clone, Debug)]
struct Frame {
    /// Where this position's plans begin in the shared pool.
    first: usize,
    /// How many there are.
    len: usize,
    /// How many have been tried.
    tried: usize,
    /// How long the path was on reaching this position, so backing out of it
    /// undoes exactly what led here.
    entry: usize,
}

/// A search's reusable working memory: kept between searches up to what the
/// short ones need, so a worker searching after every move allocates only for
/// a long search and holds little once it is idle.
#[derive(Debug)]
pub struct Solver {
    seen: HashSet<u64, BuildFastHash>,
    frames: Vec<Frame>,
    pool: Vec<Plan>,
    path: Vec<Applied>,
    actions: Vec<Action>,
    plans: Vec<Plan>,
    ranked: Vec<(i32, Plan)>,
    stock: Vec<Slot>,
    waste: Vec<Slot>,
    /// What breaks near-ties differently on a restart.
    noise: Option<NonCryptoRng>,
}

/// Searches after the first, sharing the budget's second half.
const RESTARTS: u32 = 4;

/// Positions a solver keeps room for between searches.
const RETAINED: usize = 4_096;

/// The most a restart's tie-break moves a plan's rank: a few points of
/// judgement, enough to reorder near-ties and never to put a poor move first.
const JITTER: u64 = 48;

impl Default for Solver {
    fn default() -> Self {
        Self::new()
    }
}

impl Solver {
    /// A solver holding nothing yet.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            seen: HashSet::with_hasher(BuildFastHash::new()),
            frames: Vec::new(),
            pool: Vec::new(),
            path: Vec::new(),
            actions: Vec::new(),
            plans: Vec::new(),
            ranked: Vec::new(),
            stock: Vec::new(),
            waste: Vec::new(),
            noise: None,
        }
    }

    /// Search `table` for a win, visiting at most `budget` positions.
    ///
    /// Half the budget goes on one search ordered by judgement alone, which is
    /// the one that can run to the end and prove a loss. The rest goes on
    /// shorter searches whose near-ties are broken differently: a search that
    /// has wandered into one bad corner of a game is usually quick from
    /// another.
    pub fn solve(&mut self, table: &Table, budget: u32) -> Verdict {
        let verdict = self.restarted(table, budget);
        self.give_back();
        verdict
    }

    fn restarted(&mut self, table: &Table, budget: u32) -> Verdict {
        let first = self.search(table, budget / 2, None);
        if first != Verdict::Unknown {
            return first;
        }
        let share = (budget / 2 / RESTARTS).max(1);
        for seed in 1..=u64::from(RESTARTS) {
            let verdict = self.search(table, share, Some(seed));
            if matches!(verdict, Verdict::Won(_)) {
                return verdict;
            }
        }
        Verdict::Unknown
    }

    /// Give back what a long search grew past [`RETAINED`].
    fn give_back(&mut self) {
        if self.seen.capacity() > RETAINED {
            self.seen = HashSet::with_hasher(BuildFastHash::new());
        }
        self.frames.clear();
        self.frames.shrink_to(RETAINED);
        self.pool.clear();
        self.pool.shrink_to(RETAINED);
        self.path.clear();
        self.path.shrink_to(RETAINED);
    }

    fn search(&mut self, table: &Table, budget: u32, noise: Option<u64>) -> Verdict {
        self.noise = noise.map(NonCryptoRng::seed_from_u64);
        let mut table = table.clone();
        self.seen.clear();
        self.frames.clear();
        self.pool.clear();
        self.path.clear();

        self.settle(&mut table);
        if table.won() {
            return Verdict::Won(self.line());
        }
        if self.seen.try_insert(table.canonical()).is_err() {
            return Verdict::Unknown;
        }
        self.expand(&mut table, 0);
        let mut visited = 1_u32;

        while let Some(frame) = self.frames.last_mut() {
            if frame.tried == frame.len {
                let (entry, first) = (frame.entry, frame.first);
                self.frames.pop();
                self.pool.truncate(first);
                self.unwind(&mut table, entry);
                continue;
            }
            let Some(&plan) = self.pool.get(frame.first + frame.tried) else {
                return Verdict::Unknown;
            };
            frame.tried += 1;
            let entry = self.path.len();
            if !self.make(&mut table, plan) {
                self.unwind(&mut table, entry);
                continue;
            }
            if table.won() {
                return Verdict::Won(self.line());
            }
            if visited >= budget {
                return Verdict::Unknown;
            }
            match self.seen.try_insert(table.canonical()) {
                Ok(true) => {}
                Ok(false) => {
                    self.unwind(&mut table, entry);
                    continue;
                }
                Err(_) => return Verdict::Unknown,
            }
            visited += 1;
            self.expand(&mut table, entry);
        }
        Verdict::Lost
    }

    /// Carry `plan` out and settle what follows, answering whether every step
    /// of it was allowed.
    fn make(&mut self, table: &mut Table, plan: Plan) -> bool {
        for _ in 0..plan.turns {
            let turn = if table.allows(Action::Draw) {
                Action::Draw
            } else {
                Action::Recycle
            };
            let Some(applied) = table.apply(turn) else {
                return false;
            };
            self.path.push(applied);
        }
        let Some(applied) = table.apply(plan.action) else {
            return false;
        };
        self.path.push(applied);
        self.settle(table);
        true
    }

    /// Make every safe move home there is, as the game itself would, but for
    /// a waste card drawing three, whose move can change the line.
    fn settle(&mut self, table: &mut Table) {
        let waste = table.rules().variant != Variant::Klondike(Draw::Three);
        while let Some(action) = table.safe_home(waste) {
            match table.apply(action) {
                Some(applied) => self.path.push(applied),
                None => break,
            }
        }
    }

    /// Take the path back to `entry` steps.
    fn unwind(&mut self, table: &mut Table, entry: usize) {
        while self.path.len() > entry {
            if let Some(applied) = self.path.pop() {
                table.undo(&applied);
            }
        }
    }

    /// The path so far as a line of actions.
    fn line(&self) -> Vec<Action> {
        self.path.iter().map(|applied| applied.action).collect()
    }

    /// Push the frame for `table`'s position, its plans ranked best first by
    /// the position each leads to.
    fn expand(&mut self, table: &mut Table, entry: usize) {
        self.collect_plans(table);
        self.ranked.clear();
        let plans = core::mem::take(&mut self.plans);
        for &plan in &plans {
            let Some(bias) = promise(table, plan.action) else {
                continue;
            };
            let mark = self.path.len();
            if self.make(table, plan) {
                let jitter = self
                    .noise
                    .as_mut()
                    .map_or(0, |rng| i32::try_from(rng.next_below(JITTER)).unwrap_or(0));
                let rank = evaluate(table)
                    .saturating_mul(4)
                    .saturating_add(bias)
                    .saturating_add(jitter)
                    .saturating_sub(i32::from(plan.turns));
                self.ranked.push((rank, plan));
            }
            self.unwind(table, mark);
        }
        self.plans = plans;
        // Stable, so equally promising plans keep the table's own order and a
        // search is reproducible.
        self.ranked
            .sort_by_key(|&(rank, _)| core::cmp::Reverse(rank));
        let first = self.pool.len();
        self.pool.extend(self.ranked.iter().map(|&(_, plan)| plan));
        self.frames.push(Frame {
            first,
            len: self.ranked.len(),
            tried: 0,
            entry,
        });
    }

    /// Every plan from `table`'s position into `self.plans`.
    fn collect_plans(&mut self, table: &Table) {
        table.actions(&mut self.actions);
        self.plans.clear();
        let klondike = matches!(table.rules().variant, Variant::Klondike(_));
        for &action in &self.actions {
            // Klondike's turns of the stock are folded into the plans below.
            if klondike && matches!(action, Action::Draw | Action::Recycle) {
                continue;
            }
            self.plans.push(Plan { turns: 0, action });
        }
        if klondike {
            self.stock_plans(table);
        }
    }

    /// A plan for every card the stock and waste can bring to the waste's
    /// top within one cycle — as far as the limit on turning the waste back
    /// allows — and every pile it could then be played to.
    fn stock_plans(&mut self, table: &Table) {
        let shape = table.shape();
        let (Some(stock), Some(waste)) = (shape.stock, shape.waste) else {
            return;
        };
        let Variant::Klondike(draw) = table.rules().variant else {
            return;
        };
        self.stock.clear();
        self.stock.extend_from_slice(table.pile(stock));
        self.waste.clear();
        self.waste.extend_from_slice(table.pile(waste));
        let start = (self.stock.len(), self.waste.len());
        let mut recycled = table.recycled();
        let mut cycled = false;
        let most = u8::try_from(self.stock.len() + self.waste.len() + 2).unwrap_or(u8::MAX);
        for turns in 1..=most {
            if !self.stock.is_empty() {
                for _ in 0..draw.count() {
                    if let Some(slot) = self.stock.pop() {
                        self.waste.push(slot);
                    }
                }
            } else if !self.waste.is_empty() && table.rules().recycles.allow(recycled) {
                while let Some(slot) = self.waste.pop() {
                    self.stock.push(slot);
                }
                recycled = recycled.saturating_add(1);
                cycled = true;
            } else {
                break;
            }
            if cycled && (self.stock.len(), self.waste.len()) == start {
                break;
            }
            let Some(top) = self.waste.last().map(|slot| table.face(slot.card())) else {
                continue;
            };
            for to in shape.foundations.iter().chain(shape.tableau.iter()) {
                if table.accepts(to, top, 1) {
                    self.plans.push(Plan {
                        turns,
                        action: Action::Move {
                            from: waste,
                            to,
                            count: 1,
                        },
                    });
                }
            }
        }
    }
}

/// How promising `action` looks as a move, a nudge on top of the position it
/// leads to, or `None` for a move that can never matter: one that leaves the
/// same position with two piles swapped.
fn promise(table: &Table, action: Action) -> Option<i32> {
    match action {
        Action::Draw => Some(-40),
        Action::Recycle => Some(-60),
        Action::Move { from, to, count } => {
            let count = usize::from(count);
            let source = table.pile(from);
            let leaves = source.len().saturating_sub(count);
            let from_role = table.role(from)?;
            let to_role = table.role(to)?;
            let target_empty = table.pile(to).is_empty();
            if from_role == to_role && target_empty && leaves == 0 {
                return None;
            }
            let mut bias = match to_role {
                Role::Foundation => 30,
                Role::Tableau if target_empty => 0,
                Role::Tableau => 10,
                Role::Cell => -20,
                Role::Stock | Role::Waste => return None,
            };
            if from_role == Role::Foundation {
                bias -= 40;
            }
            if to_role == Role::Tableau && target_empty {
                let king = source
                    .get(leaves)
                    .is_some_and(|slot| table.face(slot.card()).rank == Rank::KING);
                let buried = source.iter().take(leaves).any(|slot| !slot.up());
                // A king that leaves face-down cards behind is the move that
                // opens a Klondike column for good.
                if king && buried {
                    bias += 20;
                }
            }
            Some(bias)
        }
    }
}

/// How close `table` looks to won: cards home, cards uncovered, room to move,
/// and — Spider's whole game — runs kept to one suit.
fn evaluate(table: &Table) -> i32 {
    let shape = table.shape();
    let variant = table.rules().variant;
    let count = |n: usize| i32::try_from(n).unwrap_or(i32::MAX);
    let mut score = count(table.home()) * 50;
    let mut face_down = 0;
    let mut empty = 0;
    let mut links = 0;
    let mut loose = 0;
    let mut disorder = 0;
    for column in shape.tableau.iter() {
        let cards = table.pile(column);
        if cards.is_empty() {
            empty += 1;
        }
        let mut sorted = true;
        for (index, pair) in cards.windows(2).enumerate() {
            let (lower, upper) = (pair[0], pair[1]);
            if !lower.up() {
                face_down += 1;
                continue;
            }
            let (lower, upper) = (table.face(lower.card()), table.face(upper.card()));
            if upper.follows_suit_on(lower) {
                links += 1;
            } else if upper.descends_on(lower) {
                loose += 1;
            }
            if !upper.alternates_on(lower) && sorted {
                sorted = false;
                disorder += count(cards.len() - index - 1);
            }
        }
    }
    match variant {
        Variant::Klondike(_) => {
            score -= 30 * face_down;
            score += 8 * empty;
            score -= count(
                [shape.stock, shape.waste]
                    .into_iter()
                    .flatten()
                    .map(|pile| table.pile(pile).len())
                    .sum(),
            );
        }
        Variant::FreeCell => {
            let free = shape
                .cells
                .iter()
                .filter(|&cell| table.pile(cell).is_empty())
                .count();
            score += 20 * count(free) + 45 * empty - 6 * disorder;
        }
        Variant::Spider(_) => {
            score += 8 * links + 2 * loose + 40 * empty - 25 * face_down;
            score -= shape
                .stock
                .map_or(0, |stock| count(table.pile(stock).len()));
            score += 300 * count(table.home() / 13);
        }
    }
    score - 3 * buried_next(table)
}

/// How many cards lie on top of the cards each foundation needs next.
fn buried_next(table: &Table) -> i32 {
    if matches!(table.rules().variant, Variant::Spider(_)) {
        return 0;
    }
    let shape = table.shape();
    let mut buried = 0;
    for column in shape.tableau.iter() {
        let cards = table.pile(column);
        for (depth, slot) in cards.iter().enumerate() {
            let face = table.face(slot.card());
            if table.foundation_for(face).is_some() {
                buried += i32::try_from(cards.len() - depth - 1).unwrap_or(0);
            }
        }
    }
    buried
}

#[cfg(test)]
#[path = "solver_tests.rs"]
mod tests;
