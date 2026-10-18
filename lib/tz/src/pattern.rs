//! A region's numeric date and time-of-day patterns: Unicode CLDR's notation,
//! reduced to the fields a clock shows.
//!
//! A date pattern holds `d` or `dd`, `M` or `MM`, and `y` or `yyyy` once
//! each; a time pattern an hour — `H` or `HH` on a 24-hour clock, `h` or `hh`
//! with `a` on a 12-hour one — and `mm`. A doubled field is padded to two
//! digits, `y` is the whole year and `yyyy` the year in at least four digits
//! after its sign, as ISO 8601 writes it, and `a` is `AM` or `PM`. Everything
//! else is a literal of ASCII punctuation and spaces.

use core::fmt::{self, Write};

use tairix_abi::time::CivilTime;

use crate::layout::MAX_PATTERN;

/// A validated date or time pattern.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Pattern<'a>(&'a str);

/// The numeric date where no region decides: ISO 8601's.
pub(crate) const ISO_DATE: Pattern<'static> = Pattern("yyyy-MM-dd");

/// The time of day where no region decides: a 24-hour clock.
pub(crate) const ISO_TIME: Pattern<'static> = Pattern("HH:mm");

/// Local times at which every field a pattern holds is at its widest: a
/// two-digit day and month, a four-digit year, and a two-digit hour both
/// before noon and after, so a display that keeps room for these never moves
/// as the time does.
pub const WIDEST: [CivilTime; 2] = [
    CivilTime {
        year: 2088,
        month: 12,
        day: 28,
        hour: 10,
        minute: 58,
        second: 0,
    },
    CivilTime {
        year: 2088,
        month: 12,
        day: 28,
        hour: 22,
        minute: 58,
        second: 0,
    },
];

/// One element of a pattern.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Token<'a> {
    Field(char, usize),
    Literal(&'a str),
}

/// The tokens of `text`, or `None` for a character no pattern may hold.
fn tokens(text: &str) -> impl Iterator<Item = Option<Token<'_>>> {
    let mut rest = text;
    core::iter::from_fn(move || {
        let first = rest.chars().next()?;
        let run = if first.is_ascii_alphabetic() {
            rest.find(|c| c != first).unwrap_or(rest.len())
        } else {
            rest.find(|c: char| c.is_ascii_alphabetic())
                .unwrap_or(rest.len())
        };
        let (token, after) = rest.split_at(run);
        rest = after;
        Some(if first.is_ascii_alphabetic() {
            Some(Token::Field(first, run))
        } else if token
            .bytes()
            .all(|byte| byte == b' ' || byte.is_ascii_punctuation())
        {
            Some(Token::Literal(token))
        } else {
            None
        })
    })
}

/// How many of each field a pattern holds.
#[derive(Default)]
struct Census {
    day: u8,
    month: u8,
    year: u8,
    hour24: u8,
    hour12: u8,
    minute: u8,
    period: u8,
}

impl Census {
    fn of(text: &str) -> Option<Self> {
        if text.is_empty() || text.len() > MAX_PATTERN {
            return None;
        }
        let mut census = Self::default();
        for token in tokens(text) {
            let Token::Field(field, width) = token? else {
                continue;
            };
            let slot = match (field, width) {
                ('d', 1 | 2) => &mut census.day,
                ('M', 1 | 2) => &mut census.month,
                ('y', 1 | 4) => &mut census.year,
                ('H', 1 | 2) => &mut census.hour24,
                ('h', 1 | 2) => &mut census.hour12,
                ('m', 2) => &mut census.minute,
                ('a', 1) => &mut census.period,
                _ => return None,
            };
            *slot += 1;
        }
        Some(census)
    }
}

impl<'a> Pattern<'a> {
    /// `text` as a date pattern, or `None` unless it holds a day, a month
    /// and a year once each and nothing of a time.
    #[must_use]
    pub fn date(text: &'a str) -> Option<Self> {
        let c = Census::of(text)?;
        let fields = (
            c.day,
            c.month,
            c.year,
            c.hour24 + c.hour12 + c.minute + c.period,
        );
        (fields == (1, 1, 1, 0)).then_some(Self(text))
    }

    /// `text` as a time pattern, or `None` unless it holds one hour, the
    /// minutes, the period exactly when the hour is on a 12-hour clock, and
    /// nothing of a date.
    #[must_use]
    pub fn time(text: &'a str) -> Option<Self> {
        let c = Census::of(text)?;
        let twelve = c.hour12 == 1 && c.hour24 == 0 && c.period == 1;
        let twenty_four = c.hour24 == 1 && c.hour12 == 0 && c.period == 0;
        let dateless = c.day + c.month + c.year == 0;
        ((twelve || twenty_four) && c.minute == 1 && dateless).then_some(Self(text))
    }

    /// The pattern's text.
    #[must_use]
    pub const fn as_str(&self) -> &'a str {
        self.0
    }

    /// Write `civil` as this pattern spells it.
    ///
    /// # Errors
    ///
    /// The writer's own failure.
    pub fn write(&self, civil: &CivilTime, out: &mut dyn Write) -> fmt::Result {
        for token in tokens(self.0).flatten() {
            match token {
                Token::Literal(text) => out.write_str(text)?,
                Token::Field('y', 4) if civil.year < 0 => {
                    write!(out, "-{:04}", civil.year.unsigned_abs())?;
                }
                Token::Field('y', 4) => write!(out, "{:04}", civil.year)?,
                Token::Field('y', _) => write!(out, "{}", civil.year)?,
                Token::Field('M', width) => number(out, civil.month, width)?,
                Token::Field('d', width) => number(out, civil.day, width)?,
                Token::Field('H', width) => number(out, civil.hour, width)?,
                Token::Field('h', width) => number(out, twelve_hour(civil.hour), width)?,
                Token::Field('m', width) => number(out, civil.minute, width)?,
                Token::Field('a', _) => out.write_str(if civil.hour < 12 { "AM" } else { "PM" })?,
                Token::Field(..) => {}
            }
        }
        Ok(())
    }
}

/// `hour` on a 12-hour clock, which names midnight and noon 12.
const fn twelve_hour(hour: u32) -> u32 {
    match hour % 12 {
        0 => 12,
        hour => hour,
    }
}

/// `value` in at least `width` digits.
fn number(out: &mut dyn Write, value: u32, width: usize) -> fmt::Result {
    write!(out, "{value:0width$}")
}

#[cfg(test)]
#[path = "pattern_tests.rs"]
mod tests;
