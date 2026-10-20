//! A game in progress: its deal, its table, and the line of play that led
//! here, with what the line says — the moves made, the score, and the text it
//! is kept as between sessions.
//!
//! # Moves the game makes for the player
//!
//! A card sent home automatically after a move is recorded as following that
//! move, and an undo takes the whole group back. Undoing only the automatic
//! half would hand the position straight back to the rule that made it, which
//! would make it again.
//!
//! # The line as text
//!
//! A move is three base-36 digits — the pile it leaves, the pile it lands on,
//! how many cards — a draw is `.`, turning the waste back is `,`, and a step
//! made automatically is marked by a leading `:`. Every character is one the
//! store keeps unquoted. A line read back is replayed through the rules from
//! the deal, so text that does not describe a legal game is refused whole.

use alloc::string::String;
use alloc::vec::Vec;

use crate::card::CardId;
use crate::deal::DealNumber;
use crate::score::{
    standard_points, time_bonus, time_penalty, Score, Scoring, SPIDER_RUN, SPIDER_START,
    VEGAS_PAYOUT, VEGAS_STAKE,
};
use crate::table::{Action, Applied, Table};
use crate::variant::{PileId, Rules, Variant};

/// The digits a move is spelled in.
const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
/// A draw's spelling.
const DRAW: u8 = b'.';
/// The spelling of turning the waste back.
const RECYCLE: u8 = b',';
/// The mark of a step the game made itself.
const FOLLOWS: u8 = b':';

/// One step of the line.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Step {
    /// What it did.
    pub applied: Applied,
    /// Whether the game made it, following the player's move before it.
    pub automatic: bool,
}

/// A game in progress.
#[derive(Clone, Debug)]
pub struct Play {
    scoring: Scoring,
    number: DealNumber,
    table: Table,
    line: Vec<Step>,
    /// Groups taken back, latest last, each the player's action and what
    /// followed it.
    undone: Vec<Vec<Action>>,
    /// Standard points the line has earned, kept as it changes.
    standard: i64,
}

impl Play {
    /// Deal `number` under `rules` scored by `scoring`, answering the game and
    /// the cards in the order they were dealt.
    #[must_use]
    pub fn deal(rules: Rules, scoring: Scoring, number: DealNumber) -> (Self, Vec<CardId>) {
        let (table, order) = Table::deal(rules, number);
        let play = Self {
            scoring,
            number,
            table,
            line: Vec::new(),
            undone: Vec::new(),
            standard: 0,
        };
        (play, order)
    }

    /// The rules in force.
    #[must_use]
    pub const fn rules(&self) -> Rules {
        self.table.rules()
    }

    /// The variant played.
    #[must_use]
    pub const fn variant(&self) -> Variant {
        self.table.rules().variant
    }

    /// How the game is scored.
    #[must_use]
    pub const fn scoring(&self) -> Scoring {
        self.scoring
    }

    /// The deal.
    #[must_use]
    pub const fn number(&self) -> DealNumber {
        self.number
    }

    /// The table as it stands.
    #[must_use]
    pub const fn table(&self) -> &Table {
        &self.table
    }

    /// The line of play so far.
    #[must_use]
    pub fn line(&self) -> &[Step] {
        &self.line
    }

    /// Moves made: every step of the line.
    #[must_use]
    pub fn moves(&self) -> usize {
        self.line.len()
    }

    /// Whether any move has been made.
    #[must_use]
    pub fn started(&self) -> bool {
        !self.line.is_empty()
    }

    /// Make the player's `action`, answering what it did, or `None` for one
    /// the rules refuse. Anything taken back is forgotten.
    pub fn act(&mut self, action: Action) -> Option<Applied> {
        let applied = self.step(action, false)?;
        self.undone.clear();
        Some(applied)
    }

    /// Make `action` on the player's behalf, following their last move.
    pub fn follow(&mut self, action: Action) -> Option<Applied> {
        self.step(action, true)
    }

    fn step(&mut self, action: Action, automatic: bool) -> Option<Applied> {
        let applied = self.table.apply(action)?;
        self.standard += self.points(&applied);
        self.line.push(Step { applied, automatic });
        Some(applied)
    }

    fn points(&self, applied: &Applied) -> i64 {
        match self.variant() {
            Variant::Klondike(draw) => standard_points(self.table.shape(), draw, applied),
            Variant::FreeCell | Variant::Spider(_) => 0,
        }
    }

    /// Whether there is a move to take back.
    #[must_use]
    pub fn can_undo(&self) -> bool {
        !self.line.is_empty()
    }

    /// Whether there is a move taken back to make again.
    #[must_use]
    pub fn can_redo(&self) -> bool {
        !self.undone.is_empty()
    }

    /// Take back the player's last move and what followed it, answering the
    /// steps reversed, latest first.
    pub fn undo(&mut self) -> Vec<Applied> {
        let mut reversed = Vec::new();
        while let Some(step) = self.line.pop() {
            self.table.undo(&step.applied);
            self.standard -= self.points(&step.applied);
            reversed.push(step.applied);
            if !step.automatic {
                break;
            }
        }
        if !reversed.is_empty() {
            self.undone.push(
                reversed
                    .iter()
                    .rev()
                    .map(|applied| applied.action)
                    .collect(),
            );
        }
        reversed
    }

    /// Make the last move taken back again, with what followed it, answering
    /// the steps made, earliest first.
    pub fn redo(&mut self) -> Vec<Applied> {
        let Some(group) = self.undone.pop() else {
            return Vec::new();
        };
        let mut made = Vec::with_capacity(group.len());
        for (index, action) in group.into_iter().enumerate() {
            match self.step(action, index > 0) {
                Some(applied) => made.push(applied),
                None => break,
            }
        }
        made
    }

    /// The score after `secs` of play, or `None` for a game that keeps none.
    /// `bank` is cumulative Vegas's winnings from the games before.
    #[must_use]
    pub fn score(&self, secs: u64, bank: i64) -> Option<Score> {
        match self.variant() {
            Variant::Klondike(_) => Some(match self.scoring {
                Scoring::Standard => {
                    let earned = (self.standard - time_penalty(secs)).max(0);
                    let bonus = if self.table.won() {
                        time_bonus(secs)
                    } else {
                        0
                    };
                    Score::Points(earned + bonus)
                }
                Scoring::Vegas => Score::Dollars(self.vegas()),
                Scoring::Cumulative => Score::Dollars(bank.saturating_add(self.vegas())),
            }),
            Variant::Spider(_) => {
                let runs = i64::try_from(self.table.home() / 13).unwrap_or(0);
                let moves = i64::try_from(self.moves()).unwrap_or(i64::MAX);
                Some(Score::Points(
                    (SPIDER_START + SPIDER_RUN * runs)
                        .saturating_sub(moves)
                        .max(0),
                ))
            }
            Variant::FreeCell => None,
        }
    }

    /// What this game alone wins or loses at Vegas.
    #[must_use]
    pub fn vegas(&self) -> i64 {
        let home = i64::try_from(self.table.home()).unwrap_or(0);
        VEGAS_PAYOUT * home - VEGAS_STAKE
    }

    /// The line, spelled as the store keeps it, onto `out`.
    pub fn spell_line(&self, out: &mut String) {
        for step in &self.line {
            if step.automatic {
                out.push(char::from(FOLLOWS));
            }
            match step.applied.action {
                Action::Move { from, to, count } => {
                    for digit in [from.index(), to.index(), usize::from(count)] {
                        out.push(char::from(DIGITS[digit % DIGITS.len()]));
                    }
                }
                Action::Draw => out.push(char::from(DRAW)),
                Action::Recycle => out.push(char::from(RECYCLE)),
            }
        }
    }

    /// Deal `number` under `rules` and `scoring` again and replay `line` on
    /// it, or `None` — nothing half-played — for a line that does not
    /// describe a legal game of that deal.
    #[must_use]
    pub fn replay(rules: Rules, scoring: Scoring, number: DealNumber, line: &str) -> Option<Self> {
        let (mut play, _) = Self::deal(rules, scoring, number);
        let mut bytes = line.bytes();
        while let Some(byte) = bytes.next() {
            let (automatic, first) = if byte == FOLLOWS {
                (true, bytes.next()?)
            } else {
                (false, byte)
            };
            let action = match first {
                DRAW => Action::Draw,
                RECYCLE => Action::Recycle,
                digit => {
                    let from = PileId::new(value(digit)?)?;
                    let to = PileId::new(value(bytes.next()?)?)?;
                    let count = u8::try_from(value(bytes.next()?)?).ok()?;
                    Action::Move { from, to, count }
                }
            };
            if automatic {
                play.follow(action)?;
            } else {
                play.act(action)?;
            }
        }
        Some(play)
    }
}

/// The value of one base-36 digit.
fn value(digit: u8) -> Option<usize> {
    DIGITS.iter().position(|&d| d == digit)
}

#[cfg(test)]
impl Play {
    /// A game of `number` standing at `table` with no move made, for another
    /// module's tests to play on.
    pub(crate) fn laid(table: Table, scoring: Scoring, number: DealNumber) -> Self {
        Self {
            scoring,
            number,
            table,
            line: Vec::new(),
            undone: Vec::new(),
            standard: 0,
        }
    }
}

#[cfg(test)]
#[path = "play_tests.rs"]
mod tests;
