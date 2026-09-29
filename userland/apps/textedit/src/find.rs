//! Finding text or bytes: a pattern compiled once, matched in one linear
//! pass over any [`Source`], and a search that runs a bounded stretch at a
//! time so a worker that carries it can be handed a save between steps.
//!
//! An exact text pattern is matched byte for byte: UTF-8 synchronises
//! itself, so a byte match of a whole-character needle always lies on
//! character boundaries. Ignoring case or asking for whole words matches
//! characters instead, decoded as the grid decodes them.

use alloc::vec::Vec;
use core::ops::{ControlFlow, Range};

use crate::document::Source;
use crate::text::{is_word, last_unit, Decoder, Glyph};

/// Longest pattern, in bytes.
///
/// A containment bound on the table a pattern compiles to and the lookahead
/// a scan reads past its range, not a capacity.
pub const MAX_PATTERN: usize = 1024;

/// Most matches one Replace All takes in: a bound on the one edit it makes
/// and the history it leaves. More are replaced by asking again.
pub const MAX_REPLACEMENTS: usize = 10_000;

/// Why a pattern cannot be searched for.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PatternError {
    /// It is empty.
    Empty,
    /// It is longer than [`MAX_PATTERN`].
    TooLong,
    /// A hex pattern holds something other than pairs of hex digits.
    NotHex,
}

/// How a text pattern matches.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Options {
    /// Letters match only in the same case.
    pub match_case: bool,
    /// A match is neither preceded nor followed by a word character.
    pub whole_word: bool,
}

/// A compiled pattern.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Pattern {
    /// Bytes when matching bytes, folded characters when matching units.
    needle: Vec<u32>,
    /// The KMP failure function of `needle`.
    fail: Vec<usize>,
    units: bool,
    fold: bool,
    whole_word: bool,
    /// The most bytes a match can span.
    span: usize,
}

/// The key a character is compared by, folded when case is ignored.
fn fold_char(ch: char, fold: bool) -> u32 {
    if !fold {
        return u32::from(ch);
    }
    if ch.is_ascii() {
        return u32::from(ch.to_ascii_lowercase());
    }
    u32::from(ch.to_lowercase().next().unwrap_or(ch))
}

/// The key a decoded unit is compared by; an invalid byte never equals a
/// character.
fn unit_key(glyph: Glyph, fold: bool) -> u32 {
    match glyph {
        Glyph::Char(ch) | Glyph::Hidden(ch) => fold_char(ch, fold),
        Glyph::Tab => u32::from(b'\t'),
        Glyph::Control(byte) => u32::from(byte),
        Glyph::Invalid(byte) => 0x11_0000 | u32::from(byte),
    }
}

impl Pattern {
    /// A pattern for `needle` as text.
    ///
    /// # Errors
    ///
    /// [`PatternError::Empty`] or [`PatternError::TooLong`].
    pub fn text(needle: &str, options: Options) -> Result<Self, PatternError> {
        let units = !options.match_case || options.whole_word;
        let keys: Vec<u32> = if units {
            needle
                .chars()
                .map(|ch| fold_char(ch, !options.match_case))
                .collect()
        } else {
            needle.bytes().map(u32::from).collect()
        };
        // A folded character may be stored in more bytes than its needle's.
        let span = if units { keys.len() * 4 } else { keys.len() };
        Self::compile(
            keys,
            needle.len(),
            units,
            !options.match_case,
            options.whole_word,
            span,
        )
    }

    /// A pattern for the bytes `digits` spells in hex: pairs of digits,
    /// optionally separated by white space.
    ///
    /// # Errors
    ///
    /// [`PatternError::NotHex`] for anything but pairs of hex digits, and
    /// [`PatternError::Empty`] or [`PatternError::TooLong`].
    pub fn hex(digits: &str) -> Result<Self, PatternError> {
        let bytes = parse_hex(digits)?;
        let len = bytes.len();
        Self::compile(
            bytes.into_iter().map(u32::from).collect(),
            len,
            false,
            false,
            false,
            len,
        )
    }

    fn compile(
        needle: Vec<u32>,
        bytes: usize,
        units: bool,
        fold: bool,
        whole_word: bool,
        span: usize,
    ) -> Result<Self, PatternError> {
        if needle.is_empty() {
            return Err(PatternError::Empty);
        }
        if bytes > MAX_PATTERN {
            return Err(PatternError::TooLong);
        }
        let mut fail = alloc::vec![0; needle.len()];
        let mut k = 0;
        for at in 1..needle.len() {
            while k > 0 && needle[at] != needle[k] {
                k = fail[k - 1];
            }
            if needle[at] == needle[k] {
                k += 1;
            }
            fail[at] = k;
        }
        Ok(Self {
            needle,
            fail,
            units,
            fold,
            whole_word,
            span,
        })
    }

    /// Whether `range` of `source` is exactly a match.
    #[must_use]
    pub fn is_match(&self, source: &impl Source, range: Range<usize>) -> bool {
        let mut found = None;
        scan(
            source,
            self,
            range.start..range.start + 1,
            range.start,
            false,
            |hit| {
                found = Some(hit);
                ControlFlow::Break(())
            },
        );
        found == Some(range)
    }
}

/// The bytes `digits` spells in hex.
///
/// # Errors
///
/// [`PatternError::NotHex`] for anything but pairs of hex digits.
pub fn parse_hex(digits: &str) -> Result<Vec<u8>, PatternError> {
    let mut bytes = Vec::new();
    let mut high: Option<u8> = None;
    for ch in digits.chars() {
        if ch.is_whitespace() {
            if high.is_some() {
                return Err(PatternError::NotHex);
            }
            continue;
        }
        let value = crate::hex::digit_value(ch).ok_or(PatternError::NotHex)?;
        match high.take() {
            Some(first) => bytes.push((first << 4) | value),
            None => high = Some(value),
        }
    }
    if high.is_some() {
        return Err(PatternError::NotHex);
    }
    Ok(bytes)
}

/// Every match of `pattern` starting in `range` of `source` and no earlier
/// than `min_start`, in order, for as long as `visit` continues. With
/// `apart`, a match never overlaps the one before it.
pub fn scan(
    source: &impl Source,
    pattern: &Pattern,
    range: Range<usize>,
    min_start: usize,
    apart: bool,
    visit: impl FnMut(Range<usize>) -> ControlFlow<()>,
) {
    let limit = range.end.saturating_add(pattern.span + 4).min(source.len());
    if range.start >= range.end || range.start >= source.len() {
        return;
    }
    if pattern.units {
        scan_units(source, pattern, range, min_start, apart, limit, visit);
    } else {
        scan_bytes(source, pattern, range, min_start, apart, limit, visit);
    }
}

fn scan_bytes(
    source: &impl Source,
    pattern: &Pattern,
    range: Range<usize>,
    min_start: usize,
    apart: bool,
    limit: usize,
    mut visit: impl FnMut(Range<usize>) -> ControlFlow<()>,
) {
    let m = pattern.needle.len();
    let mut k = 0;
    let mut at = range.start;
    source.walk(range.start, |slice| {
        for &byte in &slice[..slice.len().min(limit - at)] {
            let key = u32::from(byte);
            while k > 0 && pattern.needle[k] != key {
                k = pattern.fail[k - 1];
            }
            if pattern.needle[k] == key {
                k += 1;
            }
            at += 1;
            if k == m {
                let start = at - m;
                if start >= range.end {
                    return ControlFlow::Break(());
                }
                // Only a reported match ends where the next may start; one
                // before `min_start` was an earlier step's, and may overlap.
                let reported = start >= min_start;
                k = if apart && reported {
                    0
                } else {
                    pattern.fail[m - 1]
                };
                if reported && visit(start..at).is_break() {
                    return ControlFlow::Break(());
                }
            }
        }
        if at >= limit {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    });
}

/// One decoded unit a unit scan remembers: where it starts and whether it
/// is part of a word.
#[derive(Copy, Clone, Default)]
struct Seen {
    offset: usize,
    word: bool,
}

/// A scan over decoded units.
struct Units<'a, V> {
    pattern: &'a Pattern,
    range: Range<usize>,
    min_start: usize,
    apart: bool,
    visit: V,
    /// The last units seen, by unit number modulo their count; unit 0 is the
    /// one before the range.
    seen: Vec<Seen>,
    n: usize,
    k: usize,
    /// A whole-word match waiting on the unit after it.
    pending: Option<Range<usize>>,
    done: bool,
}

impl<V: FnMut(Range<usize>) -> ControlFlow<()>> Units<'_, V> {
    fn report(&mut self, hit: Range<usize>) -> ControlFlow<()> {
        if self.apart {
            self.k = 0;
        }
        let flow = (self.visit)(hit);
        self.done = flow.is_break();
        flow
    }

    fn unit(&mut self, offset: usize, len: usize, glyph: Glyph) -> ControlFlow<()> {
        let word = is_word(glyph);
        if let Some(hit) = self.pending.take() {
            if !word && self.report(hit).is_break() {
                return ControlFlow::Break(());
            }
        }
        if offset >= self.range.end && self.k == 0 {
            self.done = true;
            return ControlFlow::Break(());
        }
        let m = self.pattern.needle.len();
        self.n += 1;
        self.seen[self.n % (m + 1)] = Seen { offset, word };
        let key = unit_key(glyph, self.pattern.fold);
        while self.k > 0 && self.pattern.needle[self.k] != key {
            self.k = self.pattern.fail[self.k - 1];
        }
        if self.pattern.needle[self.k] == key {
            self.k += 1;
        }
        if self.k < m {
            return ControlFlow::Continue(());
        }
        let start = self.seen[(self.n + 1 - m) % (m + 1)].offset;
        let before = self.seen[(self.n - m) % (m + 1)].word;
        self.k = self.pattern.fail[m - 1];
        if start >= self.range.end {
            self.done = true;
            return ControlFlow::Break(());
        }
        let hit = start..offset + len;
        if start < self.min_start {
            ControlFlow::Continue(())
        } else if !self.pattern.whole_word {
            self.report(hit)
        } else {
            if !before {
                self.pending = Some(hit);
            }
            ControlFlow::Continue(())
        }
    }
}

fn scan_units(
    source: &impl Source,
    pattern: &Pattern,
    range: Range<usize>,
    min_start: usize,
    apart: bool,
    limit: usize,
    visit: impl FnMut(Range<usize>) -> ControlFlow<()>,
) {
    let mut seen = alloc::vec![Seen::default(); pattern.needle.len() + 1];
    seen[0].word = word_before(source, range.start);
    let start = range.start;
    let mut units = Units {
        pattern,
        range,
        min_start,
        apart,
        visit,
        seen,
        n: 0,
        k: 0,
        pending: None,
        done: false,
    };
    let mut decoder = Decoder::new(start);
    let mut at = start;
    let mut step = |offset, len, glyph| units.unit(offset, len, glyph);
    source.walk(start, |slice| {
        let slice = &slice[..slice.len().min(limit - at)];
        at += slice.len();
        if decoder.feed(slice, &mut step).is_break() || at >= limit {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    });
    if !units.done && at == source.len() {
        decoder.finish(&mut |offset, len, glyph| units.unit(offset, len, glyph));
        // The end of the text is a word boundary.
        if let Some(hit) = units.pending.take().filter(|_| !units.done) {
            let _ = units.report(hit);
        }
    }
}

/// Whether the unit ending at `at` is part of a word.
fn word_before(source: &impl Source, at: usize) -> bool {
    let from = at.saturating_sub(4);
    let mut bytes = [0u8; 4];
    let mut len = 0;
    source.walk(from, |slice| {
        let take = slice.len().min(at - from - len);
        bytes[len..len + take].copy_from_slice(&slice[..take]);
        len += take;
        if len == at - from {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    });
    last_unit(&bytes[..len]).is_some_and(|(glyph, _)| is_word(glyph))
}

/// What a search is after.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Goal {
    /// The first match at or after `origin`, wrapping to the start.
    Next,
    /// The last match starting before `origin`, wrapping to the end.
    Previous,
    /// Every match, apart, up to [`MAX_REPLACEMENTS`].
    All(Vec<Range<usize>>),
}

/// What one step of a search came to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Step {
    /// The match found.
    Found(Range<usize>),
    /// There is no match anywhere.
    Missing,
    /// Every match, and whether more lie past [`MAX_REPLACEMENTS`].
    All {
        /// The matches, in order and apart.
        matches: Vec<Range<usize>>,
        /// Whether more matches lie past the ones answered.
        more: bool,
    },
    /// Not yet: run another step.
    Partial,
}

/// A search in progress, carried from one step to the next.
#[derive(Clone, Debug)]
pub struct Search {
    pattern: Pattern,
    goal: Goal,
    origin: usize,
    /// Next: where the next step starts. Previous: where it ends. All: where
    /// it starts.
    at: usize,
    wrapped: bool,
    /// Bytes read so far, for progress.
    read: usize,
}

impl Search {
    /// Look for the first match at or after `origin`.
    #[must_use]
    pub fn next(pattern: Pattern, origin: usize) -> Self {
        Self::new(pattern, Goal::Next, origin, origin)
    }

    /// Look for the last match starting before `origin`.
    #[must_use]
    pub fn previous(pattern: Pattern, origin: usize) -> Self {
        Self::new(pattern, Goal::Previous, origin, origin)
    }

    /// Look for every match.
    #[must_use]
    pub fn all(pattern: Pattern) -> Self {
        Self::new(pattern, Goal::All(Vec::new()), 0, 0)
    }

    const fn new(pattern: Pattern, goal: Goal, origin: usize, at: usize) -> Self {
        Self {
            pattern,
            goal,
            origin,
            at,
            wrapped: false,
            read: 0,
        }
    }

    /// The pattern searched for.
    #[must_use]
    pub const fn pattern(&self) -> &Pattern {
        &self.pattern
    }

    /// Bytes read so far.
    #[must_use]
    pub const fn read(&self) -> usize {
        self.read
    }

    /// Read at most `budget` more bytes of `source`.
    pub fn step(&mut self, source: &impl Source, budget: usize) -> Step {
        let len = source.len();
        let budget = budget.max(1);
        self.origin = self.origin.min(len);
        match &mut self.goal {
            Goal::Next => {
                let end = if self.wrapped { self.origin } else { len };
                let stop = self.at.saturating_add(budget).min(end);
                let mut found = None;
                scan(source, &self.pattern, self.at..stop, 0, false, |hit| {
                    found = Some(hit);
                    ControlFlow::Break(())
                });
                self.read += stop - self.at;
                if let Some(hit) = found {
                    return Step::Found(hit);
                }
                self.at = stop;
                if self.at >= end {
                    if self.wrapped || self.origin == 0 {
                        return Step::Missing;
                    }
                    self.wrapped = true;
                    self.at = 0;
                }
                Step::Partial
            }
            Goal::Previous => {
                let floor = if self.wrapped { self.origin } else { 0 };
                let start = self.at.saturating_sub(budget).max(floor);
                let mut found = None;
                scan(source, &self.pattern, start..self.at, 0, false, |hit| {
                    found = Some(hit);
                    ControlFlow::Continue(())
                });
                self.read += self.at - start;
                if let Some(hit) = found {
                    return Step::Found(hit);
                }
                self.at = start;
                if self.at <= floor {
                    if self.wrapped || self.origin == len {
                        return Step::Missing;
                    }
                    self.wrapped = true;
                    self.at = len;
                }
                Step::Partial
            }
            Goal::All(matches) => {
                let stop = self.at.saturating_add(budget).min(len);
                let min_start = matches.last().map_or(0, |last| last.end);
                let mut more = false;
                scan(
                    source,
                    &self.pattern,
                    self.at..stop,
                    min_start,
                    true,
                    |hit| {
                        if matches.len() == MAX_REPLACEMENTS {
                            more = true;
                            return ControlFlow::Break(());
                        }
                        matches.push(hit);
                        ControlFlow::Continue(())
                    },
                );
                self.read += stop - self.at;
                self.at = stop;
                if more || self.at >= len {
                    return Step::All {
                        matches: core::mem::take(matches),
                        more,
                    };
                }
                Step::Partial
            }
        }
    }
}

#[cfg(test)]
#[path = "find_tests.rs"]
mod tests;
