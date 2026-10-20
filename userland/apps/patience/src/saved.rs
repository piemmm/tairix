//! The game in progress, kept between sessions so that closing the game never
//! loses one.
//!
//! What is kept is the deal and the line of play, not the cards: the line is
//! replayed through the rules from the deal when the game is resumed, so a
//! saved game is either exactly the game that was being played or refused
//! whole — never a position nobody reached. The line is split across a fixed
//! run of keys, each below the store's bound on one value.

use alloc::string::String;
use core::fmt::Write as _;

use tairix_appconf::{as_u32, Registry};

use crate::deal::DealNumber;
use crate::play::Play;
use crate::score::Scoring;
use crate::stats::MOST_SECS;
use crate::store::kept;
use crate::variant::Variant;

/// The most bytes of the line one key holds: below the store's own bound on
/// a value.
pub const CHUNK: usize = 1000;
/// How many keys the line may span.
pub const CHUNKS: usize = 16;

/// A game kept for later.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Saved {
    variant: Option<Variant>,
    scoring: Scoring,
    deal: Option<u32>,
    secs: u32,
    line: String,
    /// Chunks of the line read, so a missing one ends the line rather than
    /// splicing a later one onto it.
    chunks: usize,
}

impl Saved {
    /// `play`, `secs` into it, as it would be kept, or `None` for a line too
    /// long for the store to hold.
    #[must_use]
    pub fn of(play: &Play, secs: u32) -> Option<Self> {
        let mut line = String::new();
        play.spell_line(&mut line);
        if line.len() > CHUNK * CHUNKS {
            return None;
        }
        Some(Self {
            variant: Some(play.variant()),
            scoring: play.scoring(),
            deal: Some(play.number().get()),
            secs: secs.min(MOST_SECS),
            chunks: line.len().div_ceil(CHUNK),
            line,
        })
    }

    /// Whether a game is kept.
    #[must_use]
    pub const fn holds(&self) -> bool {
        self.variant.is_some()
    }

    /// The game kept, replayed from its deal, and how long it had been played,
    /// or `None` when nothing is kept or what is kept is not a game.
    #[must_use]
    pub fn resume(&self) -> Option<(Play, u32)> {
        let variant = self.variant?;
        let number = DealNumber::new(variant, self.deal?)?;
        let rules = self.scoring.rules(variant);
        let play = Play::replay(rules, self.scoring, number, &self.line)?;
        Some((play, self.secs))
    }

    fn chunk(&self, index: usize) -> Option<&str> {
        let start = index.checked_mul(CHUNK)?;
        let end = start.saturating_add(CHUNK).min(self.line.len());
        self.line.get(start..end).filter(|chunk| !chunk.is_empty())
    }
}

/// One stored part of the kept game.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Part {
    /// Which game.
    Variant,
    /// How it is scored.
    Scoring,
    /// The deal's number.
    Deal,
    /// Seconds played.
    Clock,
    /// A chunk of the line, by its place in it.
    Line(usize),
}

/// The line chunks' keys, in order.
const LINE_NAMES: [&str; CHUNKS] = [
    "saved.line.0",
    "saved.line.1",
    "saved.line.2",
    "saved.line.3",
    "saved.line.4",
    "saved.line.5",
    "saved.line.6",
    "saved.line.7",
    "saved.line.8",
    "saved.line.9",
    "saved.line.10",
    "saved.line.11",
    "saved.line.12",
    "saved.line.13",
    "saved.line.14",
    "saved.line.15",
];

/// Whether `text` is spelled only as a line is.
fn line_text(text: &str) -> bool {
    text.bytes().all(|byte| {
        byte.is_ascii_digit() || byte.is_ascii_lowercase() || matches!(byte, b'.' | b',' | b':')
    })
}

impl Registry for Saved {
    type Key = Part;
    const KEYS: &'static [Part] = &[
        Part::Variant,
        Part::Scoring,
        Part::Deal,
        Part::Clock,
        Part::Line(0),
        Part::Line(1),
        Part::Line(2),
        Part::Line(3),
        Part::Line(4),
        Part::Line(5),
        Part::Line(6),
        Part::Line(7),
        Part::Line(8),
        Part::Line(9),
        Part::Line(10),
        Part::Line(11),
        Part::Line(12),
        Part::Line(13),
        Part::Line(14),
        Part::Line(15),
    ];

    fn name(key: Part) -> &'static str {
        match key {
            Part::Variant => "saved.game",
            Part::Scoring => "saved.scoring",
            Part::Deal => "saved.deal",
            Part::Clock => "saved.clock",
            Part::Line(index) => LINE_NAMES.get(index).copied().unwrap_or("saved.line"),
        }
    }

    fn read(&mut self, key: Part, text: &str) -> bool {
        match key {
            Part::Variant => kept(&mut self.variant, Variant::named(text).map(Some)),
            Part::Scoring => kept(&mut self.scoring, Scoring::named(text)),
            Part::Deal => kept(&mut self.deal, as_u32(text).ok().map(Some)),
            Part::Clock => kept(
                &mut self.secs,
                as_u32(text).ok().filter(|&secs| secs <= MOST_SECS),
            ),
            Part::Line(index) => {
                if index != self.chunks || text.len() > CHUNK || !line_text(text) {
                    return false;
                }
                self.line.push_str(text);
                self.chunks += 1;
                true
            }
        }
    }

    fn spell(&self, key: Part, out: &mut String) -> bool {
        match key {
            Part::Variant => self
                .variant
                .is_some_and(|variant| write!(out, "{}", variant.name()).is_ok()),
            Part::Scoring => self.holds() && write!(out, "{}", self.scoring.name()).is_ok(),
            Part::Deal => self.deal.is_some_and(|deal| write!(out, "{deal}").is_ok()),
            Part::Clock => self.holds() && write!(out, "{}", self.secs).is_ok(),
            Part::Line(index) => self
                .chunk(index)
                .is_some_and(|chunk| write!(out, "{chunk}").is_ok()),
        }
    }
}

#[cfg(test)]
#[path = "saved_tests.rs"]
mod tests;
