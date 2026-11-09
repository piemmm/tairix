//! Unicode normalization (UAX #15): the canonical and compatibility
//! decompositions, canonical ordering and canonical composition, written into
//! a buffer the caller holds, so nothing is allocated.

use zeroize::Zeroize;

use crate::in_ranges;
use crate::tables::{
    CANONICAL, COMBINING_CLASSES, COMPATIBILITY, COMPOSING_SECONDS, COMPOSITIONS,
    DECOMPOSED_IN_3_2, LEAST_COMBINING, MAX_CANONICAL_DECOMPOSITION, SYLLABLES, UNASSIGNED_3_2,
};

/// A normalization form (UAX #15).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Form {
    /// Canonical decomposition.
    Nfd,
    /// Canonical decomposition, then canonical composition.
    Nfc,
    /// Compatibility decomposition.
    Nfkd,
    /// Compatibility decomposition, then canonical composition.
    Nfkc,
}

/// The normalized text is longer than the buffer it is written into.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Overflow;

/// The characters a normalization knows.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Repertoire {
    /// This release's.
    Current,
    /// Unicode 3.2's, which stringprep fixes (RFC 3454): a code point 3.2
    /// does not assign has no decomposition, class or composite, and the five
    /// decompositions a later release corrected are 3.2's own.
    Stringprep,
}

impl Repertoire {
    /// Whether it knows nothing of `ch`. The generator refuses tables that
    /// take a character it knows to one it does not, so this is asked only of
    /// the characters read, never of their parts or composites.
    fn ignores(self, ch: char) -> bool {
        self == Self::Stringprep && in_ranges(UNASSIGNED_3_2, u32::from(ch))
    }

    /// `ch`'s decomposition in this repertoire, where a later release
    /// corrected it.
    fn original(self, ch: char) -> Option<char> {
        if self != Self::Stringprep {
            return None;
        }
        let code = u32::from(ch);
        let index = DECOMPOSED_IN_3_2
            .binary_search_by_key(&code, |&(listed, _)| listed)
            .ok()?;
        DECOMPOSED_IN_3_2
            .get(index)
            .map(|&(_, original)| char::from_u32(original).unwrap_or(ch))
    }
}

/// The full decompositions of one kind, as generated: that of `keys[i]` is
/// `parts[starts[i]..starts[i + 1]]`.
pub(crate) struct Decompositions {
    /// The characters decomposed, sorted.
    pub(crate) keys: &'static [u32],
    /// Where each one's decomposition begins in `parts`, then where the last
    /// ends.
    pub(crate) starts: &'static [u16],
    /// The decompositions, laid end to end.
    pub(crate) parts: &'static [char],
}

impl Decompositions {
    /// `ch`'s full decomposition, where it has one of this kind.
    pub(crate) fn of(&self, ch: char) -> Option<&'static [char]> {
        let code = u32::from(ch);
        // Below every key, as ASCII is, without a search.
        if self.keys.first().is_none_or(|&least| code < least) {
            return None;
        }
        let index = self.keys.binary_search(&code).ok()?;
        let start = usize::from(*self.starts.get(index)?);
        let end = usize::from(*self.starts.get(index + 1)?);
        self.parts.get(start..end)
    }
}

/// The Hangul jamo bases and counts the syllables are composed by (Unicode
/// §3.12).
const L_BASE: u32 = 0x1100;
const V_BASE: u32 = 0x1161;
const T_BASE: u32 = 0x11a7;
const L_COUNT: u32 = 19;
const V_COUNT: u32 = 21;
const T_COUNT: u32 = 28;
const N_COUNT: u32 = V_COUNT * T_COUNT;
const S_COUNT: u32 = L_COUNT * N_COUNT;

const _: () = assert!(SYLLABLES.1 - SYLLABLES.0 + 1 == S_COUNT);

/// How far past `out` the text may run while it can still fit once composed:
/// a starter absorbs at most this many characters into its composite.
const SPILL: usize = MAX_CANONICAL_DECOMPOSITION - 1;

/// `input` in normalization form `form`, written to the start of `out`: how
/// many characters it takes there.
///
/// `out` need only be as long as the normalized text, and as no character
/// normalizes to more than [`MAX_DECOMPOSITION`](crate::MAX_DECOMPOSITION)
/// characters, a buffer that many times the input's length always is. Each
/// part of a character's decomposition lands in canonical order and, in NFC
/// and NFKC, composes as it lands, so the work is linear in the input but
/// where a combining mark follows one of a higher class in its sequence: it
/// is moved before them, and NFC and NFKC compose the sequence again from its
/// starter's decomposition, so a long run of marks out of order costs time
/// quadratic in its length.
///
/// # Errors
///
/// [`Overflow`] where the normalized text is longer than `out`, whose
/// contents are then unspecified.
pub fn normalize(
    input: impl IntoIterator<Item = char>,
    form: Form,
    out: &mut [char],
) -> Result<usize, Overflow> {
    normalize_in(input, form, Repertoire::Current, out)
}

/// [`normalize`] over `repertoire`.
pub(crate) fn normalize_in(
    input: impl IntoIterator<Item = char>,
    form: Form,
    repertoire: Repertoire,
    out: &mut [char],
) -> Result<usize, Overflow> {
    let mut written = Written::new(out, form, repertoire);
    for ch in input {
        written.decompose(ch)?;
    }
    written.finish()
}

/// `ch`'s canonical combining class.
fn class_of(ch: char) -> u8 {
    let code = u32::from(ch);
    if code < LEAST_COMBINING {
        return 0;
    }
    let after = COMBINING_CLASSES.partition_point(|&(first, _, _)| first <= code);
    after
        .checked_sub(1)
        .and_then(|index| COMBINING_CLASSES.get(index))
        .filter(|&&(_, last, _)| code <= last)
        .map_or(0, |&(_, _, class)| class)
}

/// The primary composite `first` and `second` are canonically equivalent to,
/// where there is one.
fn composite(first: char, second: char) -> Option<char> {
    let (a, b) = (u32::from(first), u32::from(second));
    let lead = a.checked_sub(L_BASE).filter(|&lead| lead < L_COUNT);
    let vowel = b.checked_sub(V_BASE).filter(|&vowel| vowel < V_COUNT);
    if let (Some(lead), Some(vowel)) = (lead, vowel) {
        return char::from_u32(SYLLABLES.0 + (lead * V_COUNT + vowel) * T_COUNT);
    }
    let syllable = a
        .checked_sub(SYLLABLES.0)
        .filter(|&index| index < S_COUNT && index % T_COUNT == 0);
    let trail = b
        .checked_sub(T_BASE)
        .filter(|&trail| 0 < trail && trail < T_COUNT);
    if let (Some(_), Some(trail)) = (syllable, trail) {
        return char::from_u32(a + trail);
    }
    if !in_ranges(COMPOSING_SECONDS, b) {
        return None;
    }
    let index = COMPOSITIONS
        .binary_search_by_key(&(a, b), |&(x, y, _)| (x, y))
        .ok()?;
    char::from_u32(COMPOSITIONS.get(index)?.2)
}

/// The jamo Hangul syllable `index` (counted from the first) is written
/// with: its leading consonant, its vowel, and its trailing consonant where
/// it has one.
fn jamo(index: u32) -> impl Iterator<Item = char> {
    let trail = index % T_COUNT;
    [
        Some(L_BASE + index / N_COUNT),
        Some(V_BASE + index % N_COUNT / T_COUNT),
        (trail != 0).then_some(T_BASE + trail),
    ]
    .into_iter()
    .flatten()
    .filter_map(char::from_u32)
}

/// A normalization's output as it is written: the first `len` characters of
/// `out` and, past its end, of `spill`, the run of non-starters ending them
/// beginning at `run`.
struct Written<'a> {
    out: &'a mut [char],
    spill: [char; SPILL],
    len: usize,
    run: usize,
    /// The highest class in the run, counting the marks its starter absorbed.
    highest: u8,
    /// The class of the run's last mark, 0 while it holds none.
    last: u8,
    /// Whether the run follows a starter that may still compose.
    joins: bool,
    composes: bool,
    compatibility: bool,
    repertoire: Repertoire,
}

impl Drop for Written<'_> {
    fn drop(&mut self) {
        self.wipe_spill();
    }
}

impl<'a> Written<'a> {
    /// A normalization to `form` over `repertoire` about to be written to
    /// `out`.
    fn new(out: &'a mut [char], form: Form, repertoire: Repertoire) -> Self {
        Self {
            out,
            spill: ['\0'; SPILL],
            len: 0,
            run: 0,
            highest: 0,
            last: 0,
            joins: false,
            composes: matches!(form, Form::Nfc | Form::Nfkc),
            compatibility: matches!(form, Form::Nfkd | Form::Nfkc),
            repertoire,
        }
    }

    /// Wipe what was written past `out`: what is normalized may be a
    /// password, and the caller can wipe only what it lent.
    fn wipe_spill(&mut self) {
        self.spill.zeroize();
    }
}

impl Written<'_> {
    /// Append `ch`'s full decomposition — a compatibility one where the form
    /// asks — in canonical order (UAX #15 D108), each part composed as it
    /// lands where the form composes (D117).
    fn decompose(&mut self, ch: char) -> Result<(), Overflow> {
        if ch.is_ascii() {
            return self.extend(ch, 0);
        }
        if self.repertoire.ignores(ch) {
            // What the repertoire does not know composes with nothing.
            return self.start(ch, false);
        }
        let code = u32::from(ch);
        if let Some(index) = code
            .checked_sub(SYLLABLES.0)
            .filter(|&index| index < S_COUNT)
        {
            return jamo(index).try_for_each(|part| self.extend(part, 0));
        }
        if let Some(original) = self.repertoire.original(ch) {
            return self.push(original, class_of(original));
        }
        match CANONICAL.of(ch) {
            Some(parts) => parts.iter().try_for_each(|&part| self.push_part(part)),
            None => self.push_part(ch),
        }
    }

    /// Append `ch`, which has no canonical decomposition, decomposed by
    /// compatibility where the form asks.
    fn push_part(&mut self, ch: char) -> Result<(), Overflow> {
        let class = class_of(ch);
        // The generator refuses a compatibility mapping on a combining mark.
        let parts = if self.compatibility && class == 0 {
            COMPATIBILITY.of(ch)
        } else {
            None
        };
        match parts {
            Some(parts) => parts
                .iter()
                .try_for_each(|&part| self.push(part, class_of(part))),
            None => self.push(ch, class),
        }
    }

    /// Append `ch`, of combining class `class`, in canonical order.
    fn push(&mut self, ch: char, class: u8) -> Result<(), Overflow> {
        if class == 0 || class >= self.highest {
            self.extend(ch, class)
        } else if self.joins {
            self.recompose(ch, class)
        } else {
            self.place(ch, class)
        }
    }

    /// Append `ch` of class `class`, which no mark of the run outranks,
    /// joining it to the run's starter where the two have a primary composite
    /// and nothing blocks them: a starter must follow that one directly, and a
    /// mark must have no mark of its class between them.
    fn extend(&mut self, ch: char, class: u8) -> Result<(), Overflow> {
        let unblocked = if class == 0 {
            self.len == self.run
        } else {
            self.last < class
        };
        if self.joins && unblocked {
            let starter = self.run.saturating_sub(1);
            if let Some(joined) = self.get(starter).and_then(|held| composite(held, ch)) {
                self.highest = class;
                return self.set(starter, joined);
            }
        }
        if class == 0 {
            self.start(ch, self.composes)
        } else {
            self.place(ch, class)
        }
    }

    /// Append the starter `ch`, which begins a run that may compose with it
    /// where `joins`.
    fn start(&mut self, ch: char, joins: bool) -> Result<(), Overflow> {
        self.append(ch)?;
        self.run = self.len;
        self.highest = 0;
        self.last = 0;
        self.joins = joins;
        Ok(())
    }

    /// Append the mark `ch` of class `class` after every mark of the run whose
    /// class is no higher, composing nothing.
    fn place(&mut self, ch: char, class: u8) -> Result<(), Overflow> {
        if self.last <= class {
            self.last = class;
            self.highest = self.highest.max(class);
            return self.append(ch);
        }
        let at = self.first_above(class);
        self.shift(at, 1)?;
        self.set(at, ch)
    }

    /// Where the first mark of the run whose class is above `class` stands.
    fn first_above(&self, class: u8) -> usize {
        let (mut low, mut high) = (self.run, self.len);
        while low < high {
            let mid = low + (high - low) / 2;
            if self.get(mid).map_or(0, class_of) <= class {
                low = mid + 1;
            } else {
                high = mid;
            }
        }
        low
    }

    /// Put the mark `ch` of class `class`, which a mark the run's starter
    /// absorbed outranks, in canonical order: the starter's decomposition, the
    /// run's marks and `ch` are ordered anew, then composed as they land.
    fn recompose(&mut self, ch: char, class: u8) -> Result<(), Overflow> {
        let starter = self.run.saturating_sub(1);
        let held = self.get(starter).ok_or(Overflow)?;
        let alone = [held];
        let parts = CANONICAL.of(held).unwrap_or(&alone);
        let grow = parts.len().saturating_sub(1);
        let marks = self.run + grow..self.len + grow;
        self.shift(self.run, grow)?;
        self.restart(starter);
        for &part in parts {
            self.order(part, class_of(part))?;
        }
        for at in marks {
            if let Some(mark) = self.get(at) {
                self.order(mark, class_of(mark))?;
            }
        }
        self.order(ch, class)?;
        let ordered = starter..self.len;
        self.restart(starter);
        for at in ordered {
            if let Some(part) = self.get(at) {
                self.extend(part, class_of(part))?;
            }
        }
        Ok(())
    }

    /// Write again from `at`, where a starter stands.
    fn restart(&mut self, at: usize) {
        self.len = at;
        self.run = at;
        self.highest = 0;
        self.last = 0;
        self.joins = false;
    }

    /// Append `ch` of class `class` in canonical order, composing nothing.
    fn order(&mut self, ch: char, class: u8) -> Result<(), Overflow> {
        if class == 0 {
            self.start(ch, false)
        } else {
            self.place(ch, class)
        }
    }

    /// Move what stands from `from` on `by` places along.
    fn shift(&mut self, from: usize, by: usize) -> Result<(), Overflow> {
        let end = self.len;
        for _ in 0..by {
            self.append('\0')?;
        }
        for at in (from..end).rev() {
            if let Some(moved) = self.get(at) {
                self.set(at + by, moved)?;
            }
        }
        Ok(())
    }

    /// The `at`th character written: in `out` while it lasts, then in the
    /// spill.
    fn get(&self, at: usize) -> Option<char> {
        let held = match at.checked_sub(self.out.len()) {
            None => self.out.get(at),
            Some(past) => self.spill.get(past),
        };
        held.copied()
    }

    /// Write `ch` as the `at`th character.
    fn set(&mut self, at: usize, ch: char) -> Result<(), Overflow> {
        let slot = match at.checked_sub(self.out.len()) {
            None => self.out.get_mut(at),
            Some(past) => self.spill.get_mut(past),
        };
        *slot.ok_or(Overflow)? = ch;
        Ok(())
    }

    fn append(&mut self, ch: char) -> Result<(), Overflow> {
        self.set(self.len, ch)?;
        self.len += 1;
        Ok(())
    }

    /// How many characters were written, where `out` holds them all.
    fn finish(self) -> Result<usize, Overflow> {
        if self.len > self.out.len() {
            Err(Overflow)
        } else {
            Ok(self.len)
        }
    }
}

#[cfg(test)]
#[path = "normalize_tests.rs"]
mod tests;
