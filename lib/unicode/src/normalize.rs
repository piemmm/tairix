//! Unicode normalization (UAX #15): the canonical and compatibility
//! decompositions, canonical ordering and canonical composition, written into
//! a buffer the caller holds, so nothing is allocated.

use crate::in_ranges;
use crate::tables::{
    COMBINING_CLASSES, COMPOSITIONS, DECOMPOSED_IN_3_2, DECOMPOSITIONS, DECOMPOSITION_POOL,
    LEAST_COMBINING, LEAST_COMPOSING, SYLLABLES, UNASSIGNED_3_2,
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
    /// Whether it knows nothing of `ch`.
    fn ignores(self, ch: char) -> bool {
        self == Self::Stringprep && in_ranges(UNASSIGNED_3_2, u32::from(ch))
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

/// `input` in normalization form `form`, written to the start of `out`: how
/// many characters it takes there.
///
/// No character fully decomposes to more than
/// [`MAX_DECOMPOSITION`](crate::MAX_DECOMPOSITION), so a buffer that many
/// times the input's length always suffices. The work is linear in the input
/// but for canonical ordering, which moves each combining mark past the marks
/// of a higher class before it in its run.
///
/// # Errors
///
/// [`Overflow`] where the text is longer than `out`, whose contents are then
/// unspecified.
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
    let compatibility = matches!(form, Form::Nfkd | Form::Nfkc);
    let mut written = Written {
        out,
        len: 0,
        run: 0,
        repertoire,
    };
    for ch in input {
        written.decompose(ch, compatibility)?;
    }
    if matches!(form, Form::Nfc | Form::Nfkc) {
        written.compose();
    }
    Ok(written.len)
}

/// `ch`'s full canonical decomposition, where it has one by mapping: a
/// Hangul syllable decomposes by arithmetic and is not listed.
pub(crate) fn canonical_decomposition(ch: char) -> Option<&'static [char]> {
    mapped(ch, false)
}

/// `ch`'s full decomposition by mapping, a compatibility one where
/// `compatibility`, if it has one.
fn mapped(ch: char, compatibility: bool) -> Option<&'static [char]> {
    if ch.is_ascii() {
        return None;
    }
    let code = u32::from(ch);
    let index = DECOMPOSITIONS
        .binary_search_by_key(&code, |&(listed, ..)| listed)
        .ok()?;
    let &(_, canonical_at, canonical_len, compatibility_at, compatibility_len) =
        DECOMPOSITIONS.get(index)?;
    let (at, len) = if compatibility {
        (compatibility_at, compatibility_len)
    } else {
        (canonical_at, canonical_len)
    };
    let at = usize::from(at);
    DECOMPOSITION_POOL
        .get(at..at + usize::from(len))
        .filter(|parts| !parts.is_empty())
}

/// `ch`'s canonical combining class in `repertoire`.
fn class_of(ch: char, repertoire: Repertoire) -> u8 {
    let code = u32::from(ch);
    if code < LEAST_COMBINING || repertoire.ignores(ch) {
        return 0;
    }
    let after = COMBINING_CLASSES.partition_point(|&(first, _, _)| first <= code);
    after
        .checked_sub(1)
        .and_then(|index| COMBINING_CLASSES.get(index))
        .filter(|&&(_, last, _)| code <= last)
        .map_or(0, |&(_, _, class)| class)
}

/// The primary composite `first` and `second` are canonically equivalent to
/// in `repertoire`, where there is one.
fn composite(first: char, second: char, repertoire: Repertoire) -> Option<char> {
    let (a, b) = (u32::from(first), u32::from(second));
    if b < LEAST_COMPOSING.min(V_BASE) {
        return None;
    }
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
    if repertoire.ignores(first) || repertoire.ignores(second) {
        return None;
    }
    let index = COMPOSITIONS
        .binary_search_by_key(&(a, b), |&(x, y, _)| (x, y))
        .ok()?;
    let composite = char::from_u32(COMPOSITIONS.get(index)?.2)?;
    (!repertoire.ignores(composite)).then_some(composite)
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
/// `out`, the run of non-starters ending them beginning at `run`.
struct Written<'a> {
    out: &'a mut [char],
    len: usize,
    run: usize,
    repertoire: Repertoire,
}

impl Written<'_> {
    /// Append `ch`'s full decomposition, a compatibility one where
    /// `compatibility`.
    fn decompose(&mut self, ch: char, compatibility: bool) -> Result<(), Overflow> {
        if ch.is_ascii() || self.repertoire.ignores(ch) {
            return self.push(ch);
        }
        let code = u32::from(ch);
        if let Some(index) = code
            .checked_sub(SYLLABLES.0)
            .filter(|&index| index < S_COUNT)
        {
            return jamo(index).try_for_each(|part| self.push(part));
        }
        if self.repertoire == Repertoire::Stringprep {
            if let Ok(index) = DECOMPOSED_IN_3_2.binary_search_by_key(&code, |&(listed, _)| listed)
            {
                let original = DECOMPOSED_IN_3_2
                    .get(index)
                    .and_then(|&(_, original)| char::from_u32(original))
                    .unwrap_or(ch);
                return self.push(original);
            }
        }
        match mapped(ch, compatibility) {
            Some(parts) => parts.iter().try_for_each(|&part| self.push(part)),
            None => self.push(ch),
        }
    }

    /// Append `ch` in canonical order (UAX #15 D108): after every
    /// non-starter of its run whose class is no higher than its own.
    fn push(&mut self, ch: char) -> Result<(), Overflow> {
        *self.out.get_mut(self.len).ok_or(Overflow)? = ch;
        self.len += 1;
        let class = class_of(ch, self.repertoire);
        if class == 0 {
            self.run = self.len;
            return Ok(());
        }
        let repertoire = self.repertoire;
        if let Some((_, before)) = self
            .out
            .get_mut(self.run..self.len)
            .and_then(<[char]>::split_last_mut)
        {
            let at = before.partition_point(|&held| class_of(held, repertoire) <= class);
            if let Some(moved) = self.out.get_mut(self.run + at..self.len) {
                moved.rotate_right(1);
            }
        }
        Ok(())
    }

    /// Compose what was written in place (UAX #15 D117): each character
    /// joined to the last starter before it where nothing between blocks it
    /// and the two have a primary composite.
    fn compose(&mut self) {
        let repertoire = self.repertoire;
        let mut starter: Option<usize> = None;
        let mut last_class = 0;
        let mut kept = 0;
        for read in 0..self.len {
            let Some(&ch) = self.out.get(read) else {
                break;
            };
            let class = class_of(ch, repertoire);
            if let Some(at) = starter {
                // What stands between them is a run of non-starters in
                // canonical order, so the last of them is its highest.
                let blocked = kept > at + 1 && last_class >= class;
                let joined = self
                    .out
                    .get(at)
                    .filter(|_| !blocked)
                    .and_then(|&held| composite(held, ch, repertoire));
                if let (Some(joined), Some(slot)) = (joined, self.out.get_mut(at)) {
                    *slot = joined;
                    continue;
                }
            }
            if class == 0 {
                starter = Some(kept);
            }
            last_class = class;
            if let Some(slot) = self.out.get_mut(kept) {
                *slot = ch;
            }
            kept += 1;
        }
        self.len = kept;
    }
}

#[cfg(test)]
#[path = "normalize_tests.rs"]
mod tests;
