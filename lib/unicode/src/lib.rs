//! Unicode character properties: what showing text a stranger authored asks of
//! a character and of a string — whether a character hides, whether it is a
//! nonspacing mark, whether a combining sequence carries too many marks or one
//! twice, and how a string's scripts mix (UTS #39 §5, Unicode Security
//! Mechanisms) — answered from tables generated out of the Unicode Character
//! Database in the crate's `assets/`.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

// Generated, so laid out by its generator.
#[rustfmt::skip]
mod tables;

pub use tables::VERSION;

use tables::{
    BOPO, CYRL, DECOMPOSED, DECOMPOSED_MARKS, DEFAULT_IGNORABLE, EXTENSIONS, EXTENSION_SETS, GREK,
    HANB, HANG, HANI, HIRA, HNTL, JPAN, KANA, KORE, LATN, NONSPACING_MARKS, RECOMMENDED, SCRIPTS,
    SET_WORDS, ZINH, ZYYY,
};

/// Whether `ch` draws nothing where it stands yet can hide, reorder or break
/// the text around it: a C0 or C1 control, a line or paragraph separator, an
/// interlinear-annotation control, or a `Default_Ignorable_Code_Point` — the
/// bidirectional controls, the zero-width characters, the fillers, the
/// variation selectors and the tags.
#[must_use]
pub const fn is_hidden(ch: char) -> bool {
    matches!(
        ch,
        '\u{0}'..='\u{1f}' | '\u{7f}'..='\u{9f}' | '\u{2028}' | '\u{2029}' | '\u{fff9}'..='\u{fffb}'
    ) || is_default_ignorable(ch)
}

/// Whether `ch` is a `Default_Ignorable_Code_Point`: a character a renderer
/// that does not support it draws as nothing.
#[must_use]
pub const fn is_default_ignorable(ch: char) -> bool {
    !ch.is_ascii() && in_ranges(DEFAULT_IGNORABLE, ch as u32)
}

/// Whether `ch` is a nonspacing mark — `General_Category` `Mn` or `Me` — drawn
/// over or around the character before it.
#[must_use]
pub const fn is_nonspacing_mark(ch: char) -> bool {
    !ch.is_ascii() && in_ranges(NONSPACING_MARKS, ch as u32)
}

/// The most nonspacing marks one character may carry (UTS #39 §5.4): more
/// stack over the text around them.
pub const MAX_MARKS: usize = 4;

/// The nonspacing marks of the combining sequence being read, judged as UTS
/// #39 §5.4 judges them in its canonical decomposition: at most [`MAX_MARKS`],
/// and none that an earlier character of the sequence already carries, since
/// a repeated mark draws over itself — so `é` followed by U+0301 repeats its
/// acute. One character's own decomposition may repeat a mark: it draws as
/// the single glyph it encodes.
#[derive(Copy, Clone, Debug, Default)]
pub struct MarkRun {
    marks: [char; MAX_MARKS],
    len: usize,
}

impl MarkRun {
    /// Read `ch`, answering whether the sequence still passes. A character
    /// that is no nonspacing mark begins a sequence carrying the marks it
    /// decomposes to; a mark joins the sequence being read.
    pub fn admit(&mut self, ch: char) -> bool {
        let mark = is_nonspacing_mark(ch);
        if !mark {
            self.len = 0;
        }
        let earlier = self.len;
        match decomposed_marks(ch) {
            Some(carried) => carried.iter().all(|&carried| self.push(carried, earlier)),
            None => !mark || self.push(ch, earlier),
        }
    }

    fn push(&mut self, mark: char, earlier: usize) -> bool {
        if self.marks.iter().take(earlier).any(|&held| held == mark) {
            return false;
        }
        let Some(slot) = self.marks.get_mut(self.len) else {
            return false;
        };
        *slot = mark;
        self.len += 1;
        true
    }
}

/// Which characters of text from elsewhere a faithful rendering — one that
/// escapes a character rather than replacing it — must escape, read a
/// character at a time: each [`is_hidden`] character, and each mark that
/// breaks its combining sequence's [`MarkRun`] limits, so no two texts render
/// alike.
#[derive(Copy, Clone, Debug, Default)]
pub struct Faithful {
    run: MarkRun,
}

impl Faithful {
    /// Read `ch`, answering whether it must be escaped.
    pub fn escapes(&mut self, ch: char) -> bool {
        if is_hidden(ch) {
            self.run = MarkRun::default();
            return true;
        }
        !self.run.admit(ch)
    }
}

/// The nonspacing marks `ch`'s full canonical decomposition is written with,
/// where it has any.
fn decomposed_marks(ch: char) -> Option<&'static [char]> {
    if ch.is_ascii() {
        return None;
    }
    let code = u32::from(ch);
    let index = DECOMPOSED
        .binary_search_by_key(&code, |&(decomposed, _, _)| decomposed)
        .ok()?;
    let &(_, at, len) = DECOMPOSED.get(index)?;
    let at = usize::from(at);
    DECOMPOSED_MARKS.get(at..at + usize::from(len))
}

const fn in_ranges(ranges: &[(u32, u32)], code: u32) -> bool {
    let (mut low, mut high) = (0, ranges.len());
    while low < high {
        let mid = low + (high - low) / 2;
        let (first, last) = ranges[mid];
        if code < first {
            high = mid;
        } else if code > last {
            low = mid + 1;
        } else {
            return true;
        }
    }
    false
}

/// How freely a string mixes scripts (UTS #39 §5.2), strictest first. Where a
/// string mixes scripts it may be impersonating another written in one.
///
/// With no identifier profile in force, as for a name shown rather than one
/// parsed, levels 5 and 6 of the standard coincide as
/// [`RestrictionLevel::MinimallyRestrictive`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum RestrictionLevel {
    /// ASCII alone.
    AsciiOnly,
    /// One script, besides the characters every script shares.
    SingleScript,
    /// Latin with the scripts of Japanese, of Chinese with Bopomofo, or of
    /// Korean.
    HighlyRestrictive,
    /// Latin with one other Recommended script, neither Cyrillic nor Greek.
    ModeratelyRestrictive,
    /// Any other mixture — Latin with Cyrillic or Greek among them.
    MinimallyRestrictive,
}

/// The restriction level of `text` (UTS #39 §5.2).
#[must_use]
pub fn restriction_level(text: &str) -> RestrictionLevel {
    if text.is_ascii() {
        return RestrictionLevel::AsciiOnly;
    }
    let mut resolved = Scripts::ALL;
    let mut beside_latin = Scripts::ALL;
    let (mut korean, mut bopomofo, mut japanese) = (true, true, true);
    for set in text.chars().filter_map(augmented) {
        resolved = resolved.and(set);
        if !set.contains(LATN) {
            beside_latin = beside_latin.and(set);
            korean &= set.contains(KORE);
            bopomofo &= set.contains(HANB);
            japanese &= set.contains(JPAN);
        }
    }
    if !resolved.is_empty() {
        RestrictionLevel::SingleScript
    } else if korean || bopomofo || japanese {
        RestrictionLevel::HighlyRestrictive
    } else if !beside_latin.and(MODERATE_PARTNERS).is_empty() {
        RestrictionLevel::ModeratelyRestrictive
    } else {
        RestrictionLevel::MinimallyRestrictive
    }
}

/// What Latin may stand beside in a moderately restrictive string.
const MODERATE_PARTNERS: Scripts = Scripts(RECOMMENDED).without(CYRL).without(GREK);

/// A set of scripts, by the generated tables' script numbers.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Scripts([u64; SET_WORDS]);

impl Scripts {
    const ALL: Self = Self([u64::MAX; SET_WORDS]);

    const fn of(script: u8) -> Self {
        Self([0; SET_WORDS]).with(script)
    }

    const fn with(mut self, script: u8) -> Self {
        self.0[script as usize / 64] |= 1 << (script % 64);
        self
    }

    const fn without(mut self, script: u8) -> Self {
        self.0[script as usize / 64] &= !(1 << (script % 64));
        self
    }

    const fn contains(self, script: u8) -> bool {
        self.0[script as usize / 64] & (1 << (script % 64)) != 0
    }

    fn and(self, other: Self) -> Self {
        let mut out = self;
        for (word, other) in out.0.iter_mut().zip(other.0) {
            *word &= other;
        }
        out
    }

    fn is_empty(self) -> bool {
        self.0.iter().all(|&word| word == 0)
    }
}

/// `ch`'s `Script_Extensions` as UTS #39 §5.1 augments them with the writing
/// systems they belong to, or `None` for a character every script shares —
/// Common or Inherited — which constrains nothing.
fn augmented(ch: char) -> Option<Scripts> {
    let code = u32::from(ch);
    let set = extensions(code).unwrap_or_else(|| Scripts::of(script(code)));
    if set.contains(ZYYY) || set.contains(ZINH) {
        return None;
    }
    let mut augmented = set;
    if set.contains(HANI) {
        augmented = augmented.with(HANB).with(HNTL).with(JPAN).with(KORE);
    }
    if set.contains(HIRA) || set.contains(KANA) {
        augmented = augmented.with(JPAN);
    }
    if set.contains(HANG) {
        augmented = augmented.with(KORE);
    }
    if set.contains(BOPO) {
        augmented = augmented.with(HANB);
    }
    if set.contains(LATN) {
        augmented = augmented.with(HNTL);
    }
    Some(augmented)
}

/// The Script property of `code`.
fn script(code: u32) -> u8 {
    let after = SCRIPTS.partition_point(|&(first, _)| first <= code);
    after
        .checked_sub(1)
        .and_then(|index| SCRIPTS.get(index))
        .map_or(tables::ZZZZ, |&(_, script)| script)
}

/// `code`'s explicit `Script_Extensions`, where it has them.
fn extensions(code: u32) -> Option<Scripts> {
    let after = EXTENSIONS.partition_point(|&(first, _, _)| first <= code);
    let &(_, last, set) = EXTENSIONS.get(after.checked_sub(1)?)?;
    if code > last {
        return None;
    }
    EXTENSION_SETS.get(usize::from(set)).copied().map(Scripts)
}

#[cfg(test)]
mod tests;
