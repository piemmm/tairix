//! Statistics: every game played, kept per variant in the game's own store.
//!
//! A finished or abandoned game reaches the store as an [`Outcome`] added to
//! what the store holds at the moment it is written, never as a total worked
//! out from what was read when the game started. A session whose store could
//! not be read at first therefore adds its games to the player's record rather
//! than replacing it.
//!
//! Every bound below is validation of a value that may arrive from a
//! hand-edited document, not a capacity.

use alloc::string::String;
use core::fmt::Write as _;

use tairix_appconf::{as_i64, as_u32, Keys, Registry};

use crate::store::kept;
use crate::variant::Variant;

/// The most games a count may hold.
pub const MOST_GAMES: u32 = 10_000_000;
/// The longest game a time may name, in seconds: the clock's widest reading,
/// ninety-nine hours.
pub const MOST_SECS: u32 = 359_999;
/// The most moves a record may name.
pub const MOST_MOVES: u32 = 100_000;
/// The largest score or Vegas bank, either way.
pub const MOST_SCORE: i64 = 1_000_000_000;

/// How one variant has gone.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct Record {
    /// Games begun.
    pub played: u32,
    /// Games won.
    pub won: u32,
    /// The current run: wins counted up, losses counted down.
    pub streak: i32,
    /// The longest run of wins.
    pub best_streak: u32,
    /// The fastest win, in seconds.
    pub fastest: Option<u32>,
    /// The win in fewest moves.
    pub fewest: Option<u32>,
    /// The highest score a win reached.
    pub best_score: Option<i64>,
}

impl Record {
    /// The share of games won, in whole percent.
    #[must_use]
    pub fn percent(&self) -> u32 {
        if self.played == 0 {
            return 0;
        }
        let share = u64::from(self.won) * 100 / u64::from(self.played);
        u32::try_from(share).unwrap_or(100)
    }
}

/// How one game ended, to add to its variant's record.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Outcome {
    /// The game.
    pub variant: Variant,
    /// Whether it was won.
    pub won: bool,
    /// How long it took, in seconds.
    pub secs: u32,
    /// How many moves it took.
    pub moves: u32,
    /// The score it ended on, where the variant keeps one.
    pub score: Option<i64>,
    /// What it won or lost at cumulative Vegas, carried into the bank.
    pub banked: Option<i64>,
}

/// Something to do to the statistics.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Tally {
    /// Add a game.
    Add(Outcome),
    /// Forget a variant's record, and the bank with Klondike's.
    Reset(Variant),
}

/// Every variant's record, and the cumulative Vegas bank.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct Stats {
    records: [Record; Variant::ALL.len()],
    bank: i64,
}

/// One field of a record.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Field {
    /// [`Record::played`].
    Played,
    /// [`Record::won`].
    Won,
    /// [`Record::streak`].
    Streak,
    /// [`Record::best_streak`].
    BestStreak,
    /// [`Record::fastest`].
    Fastest,
    /// [`Record::fewest`].
    Fewest,
    /// [`Record::best_score`].
    BestScore,
}

impl Field {
    const ALL: [Self; 7] = [
        Self::Played,
        Self::Won,
        Self::Streak,
        Self::BestStreak,
        Self::Fastest,
        Self::Fewest,
        Self::BestScore,
    ];
}

/// One stored statistic.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Stat {
    /// A field of the record of the variant at this index of
    /// [`Variant::ALL`].
    Of(usize, Field),
    /// The cumulative Vegas bank.
    Bank,
}

/// Every key's spelling, by variant and then field.
const NAMES: [[&str; Field::ALL.len()]; Variant::ALL.len()] = [
    [
        "stats.klondike-1.played",
        "stats.klondike-1.won",
        "stats.klondike-1.streak",
        "stats.klondike-1.best-streak",
        "stats.klondike-1.fastest",
        "stats.klondike-1.fewest",
        "stats.klondike-1.best-score",
    ],
    [
        "stats.klondike-3.played",
        "stats.klondike-3.won",
        "stats.klondike-3.streak",
        "stats.klondike-3.best-streak",
        "stats.klondike-3.fastest",
        "stats.klondike-3.fewest",
        "stats.klondike-3.best-score",
    ],
    [
        "stats.freecell.played",
        "stats.freecell.won",
        "stats.freecell.streak",
        "stats.freecell.best-streak",
        "stats.freecell.fastest",
        "stats.freecell.fewest",
        "stats.freecell.best-score",
    ],
    [
        "stats.spider-1.played",
        "stats.spider-1.won",
        "stats.spider-1.streak",
        "stats.spider-1.best-streak",
        "stats.spider-1.fastest",
        "stats.spider-1.fewest",
        "stats.spider-1.best-score",
    ],
    [
        "stats.spider-2.played",
        "stats.spider-2.won",
        "stats.spider-2.streak",
        "stats.spider-2.best-streak",
        "stats.spider-2.fastest",
        "stats.spider-2.fewest",
        "stats.spider-2.best-score",
    ],
    [
        "stats.spider-4.played",
        "stats.spider-4.won",
        "stats.spider-4.streak",
        "stats.spider-4.best-streak",
        "stats.spider-4.fastest",
        "stats.spider-4.fewest",
        "stats.spider-4.best-score",
    ],
];

impl Stats {
    /// How `variant` has gone.
    #[must_use]
    pub fn record(&self, variant: Variant) -> Record {
        self.records
            .get(variant.index())
            .copied()
            .unwrap_or_default()
    }

    /// Cumulative Vegas's winnings so far.
    #[must_use]
    pub const fn bank(&self) -> i64 {
        self.bank
    }

    /// Carry out `tally`, answering the statistics it changed.
    pub fn apply(&mut self, tally: Tally) -> Keys<Self> {
        match tally {
            Tally::Add(outcome) => self.add(&outcome),
            Tally::Reset(variant) => self.reset(variant),
        }
    }

    fn add(&mut self, outcome: &Outcome) -> Keys<Self> {
        let index = outcome.variant.index();
        let mut changed = Keys::EMPTY;
        if let Some(banked) = outcome.banked {
            self.bank = self
                .bank
                .saturating_add(banked)
                .clamp(-MOST_SCORE, MOST_SCORE);
            changed = changed.union(Keys::of(Stat::Bank));
        }
        let Some(record) = self.records.get_mut(index) else {
            return changed;
        };
        let mut touched = |field| changed = changed.union(Keys::of(Stat::Of(index, field)));
        record.played = (record.played + 1).min(MOST_GAMES);
        touched(Field::Played);
        touched(Field::Streak);
        if !outcome.won {
            record.streak = record.streak.min(0).saturating_sub(1);
            return changed;
        }
        record.won = (record.won + 1).min(record.played);
        record.streak = record.streak.max(0).saturating_add(1);
        let run = u32::try_from(record.streak).unwrap_or(0);
        record.best_streak = record.best_streak.max(run);
        touched(Field::Won);
        touched(Field::BestStreak);
        let secs = outcome.secs.clamp(1, MOST_SECS);
        if record.fastest.is_none_or(|best| secs < best) {
            record.fastest = Some(secs);
            touched(Field::Fastest);
        }
        let moves = outcome.moves.clamp(1, MOST_MOVES);
        if record.fewest.is_none_or(|best| moves < best) {
            record.fewest = Some(moves);
            touched(Field::Fewest);
        }
        if let Some(score) = outcome
            .score
            .map(|score| score.clamp(-MOST_SCORE, MOST_SCORE))
        {
            if record.best_score.is_none_or(|best| score > best) {
                record.best_score = Some(score);
                touched(Field::BestScore);
            }
        }
        changed
    }

    fn reset(&mut self, variant: Variant) -> Keys<Self> {
        let index = variant.index();
        let mut changed = Keys::EMPTY;
        if let Some(record) = self.records.get_mut(index) {
            *record = Record::default();
            for field in Field::ALL {
                changed = changed.union(Keys::of(Stat::Of(index, field)));
            }
        }
        if matches!(variant, Variant::Klondike(_)) {
            self.bank = 0;
            changed = changed.union(Keys::of(Stat::Bank));
        }
        changed
    }
}

impl Registry for Stats {
    type Key = Stat;
    const KEYS: &'static [Stat] = &[
        Stat::Of(0, Field::Played),
        Stat::Of(0, Field::Won),
        Stat::Of(0, Field::Streak),
        Stat::Of(0, Field::BestStreak),
        Stat::Of(0, Field::Fastest),
        Stat::Of(0, Field::Fewest),
        Stat::Of(0, Field::BestScore),
        Stat::Of(1, Field::Played),
        Stat::Of(1, Field::Won),
        Stat::Of(1, Field::Streak),
        Stat::Of(1, Field::BestStreak),
        Stat::Of(1, Field::Fastest),
        Stat::Of(1, Field::Fewest),
        Stat::Of(1, Field::BestScore),
        Stat::Of(2, Field::Played),
        Stat::Of(2, Field::Won),
        Stat::Of(2, Field::Streak),
        Stat::Of(2, Field::BestStreak),
        Stat::Of(2, Field::Fastest),
        Stat::Of(2, Field::Fewest),
        Stat::Of(2, Field::BestScore),
        Stat::Of(3, Field::Played),
        Stat::Of(3, Field::Won),
        Stat::Of(3, Field::Streak),
        Stat::Of(3, Field::BestStreak),
        Stat::Of(3, Field::Fastest),
        Stat::Of(3, Field::Fewest),
        Stat::Of(3, Field::BestScore),
        Stat::Of(4, Field::Played),
        Stat::Of(4, Field::Won),
        Stat::Of(4, Field::Streak),
        Stat::Of(4, Field::BestStreak),
        Stat::Of(4, Field::Fastest),
        Stat::Of(4, Field::Fewest),
        Stat::Of(4, Field::BestScore),
        Stat::Of(5, Field::Played),
        Stat::Of(5, Field::Won),
        Stat::Of(5, Field::Streak),
        Stat::Of(5, Field::BestStreak),
        Stat::Of(5, Field::Fastest),
        Stat::Of(5, Field::Fewest),
        Stat::Of(5, Field::BestScore),
        Stat::Bank,
    ];

    fn name(key: Stat) -> &'static str {
        match key {
            Stat::Of(index, field) => NAMES
                .get(index)
                .and_then(|names| names.get(field as usize))
                .copied()
                .unwrap_or("stats.unknown"),
            Stat::Bank => "vegas.bank",
        }
    }

    fn read(&mut self, key: Stat, text: &str) -> bool {
        let Stat::Of(index, field) = key else {
            return kept(
                &mut self.bank,
                as_i64(text).ok().filter(|bank| bank.abs() <= MOST_SCORE),
            );
        };
        let Some(record) = self.records.get_mut(index) else {
            return false;
        };
        let count = |most: u32| as_u32(text).ok().filter(|&n| n <= most);
        match field {
            Field::Played => kept(&mut record.played, count(MOST_GAMES)),
            Field::Won => kept(&mut record.won, count(MOST_GAMES)),
            Field::BestStreak => kept(&mut record.best_streak, count(MOST_GAMES)),
            Field::Streak => kept(
                &mut record.streak,
                text.parse::<i32>()
                    .ok()
                    .filter(|streak| streak.unsigned_abs() <= MOST_GAMES),
            ),
            Field::Fastest => kept(
                &mut record.fastest,
                count(MOST_SECS).filter(|&secs| secs > 0).map(Some),
            ),
            Field::Fewest => kept(
                &mut record.fewest,
                count(MOST_MOVES).filter(|&moves| moves > 0).map(Some),
            ),
            Field::BestScore => kept(
                &mut record.best_score,
                as_i64(text)
                    .ok()
                    .filter(|score| score.abs() <= MOST_SCORE)
                    .map(Some),
            ),
        }
    }

    fn spell(&self, key: Stat, out: &mut String) -> bool {
        let Stat::Of(index, field) = key else {
            return self.bank != 0 && write!(out, "{}", self.bank).is_ok();
        };
        let Some(record) = self.records.get(index) else {
            return false;
        };
        let value: Option<i64> = match field {
            Field::Played => Some(i64::from(record.played)).filter(|&n| n > 0),
            Field::Won => Some(i64::from(record.won)).filter(|&n| n > 0),
            Field::Streak => Some(i64::from(record.streak)).filter(|&n| n != 0),
            Field::BestStreak => Some(i64::from(record.best_streak)).filter(|&n| n > 0),
            Field::Fastest => record.fastest.map(i64::from),
            Field::Fewest => record.fewest.map(i64::from),
            Field::BestScore => record.best_score,
        };
        value.is_some_and(|value| write!(out, "{value}").is_ok())
    }

    /// A count the document contradicts is held to what the rest of the
    /// record allows: no more wins than games, no run longer than its kind.
    fn normalise(&mut self) {
        for record in &mut self.records {
            record.won = record.won.min(record.played);
            record.best_streak = record.best_streak.min(record.won);
            let wins = i32::try_from(record.won).unwrap_or(i32::MAX);
            let losses = i32::try_from(record.played - record.won).unwrap_or(i32::MAX);
            record.streak = record.streak.clamp(-losses, wins);
        }
    }
}

#[cfg(test)]
#[path = "stats_tests.rs"]
mod tests;
