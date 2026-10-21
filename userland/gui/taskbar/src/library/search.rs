//! Finding a program by what is typed.
//!
//! Each program is folded once, when the catalog or its manifest facts
//! arrive ([`Indexed::new`]), so a keystroke compares characters and builds
//! only what it finds: the places a match took. A query is split into terms,
//! every one of which must match the
//! program's name, its manifest's one-line purpose, or its folder; the name
//! ranks above the purpose, and within the name a whole match, a prefix, a
//! word's start, the words' initials, a run anywhere, and the characters in
//! order rank in that order ([`Tier`]). Ties go to the more used program and
//! then to the name, so a result list never reorders itself between two
//! identical keystrokes.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::cmp::Reverse;
use core::ops::Range;

use tairix_proglib::{EntryId, LibraryCategory, LibraryEntry};
use tairix_util::utf8::fold;

/// The most terms a query is read as, and the most characters of each: the
/// bound on what one keystroke can ask of the search, well past anything a
/// person types into a launcher.
pub(crate) const MAX_TERMS: usize = 8;

/// See [`MAX_TERMS`].
pub(crate) const MAX_TERM_CHARS: usize = 64;

/// How well one term matched a program, best first.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) enum Tier {
    /// The query is the whole name.
    NameWhole,
    /// The name begins with the term.
    NamePrefix,
    /// A word of the name begins with the term.
    NameWord,
    /// The term is the initials of consecutive words of the name.
    NameInitials,
    /// The term appears inside the name.
    NameInside,
    /// The term's characters appear in the name in order, from a word's start.
    NameScattered,
    /// A word of the purpose begins with the term.
    PurposeWord,
    /// The term appears inside the purpose.
    PurposeInside,
    /// A word of the folder's name begins with the term.
    Folder,
}

impl Tier {
    const fn weight(self) -> u32 {
        self as u32
    }
}

/// Text as the search reads it: each character folded, the characters that
/// begin a word, and the byte each starts at in the text it came from.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct Folded {
    chars: Vec<char>,
    /// Where each word begins, in order.
    words: Vec<usize>,
    /// One more entry than `chars`: the text's length, so a run's end maps too.
    bytes: Vec<usize>,
}

impl Folded {
    fn new(text: &str) -> Self {
        let mut folded = Self::default();
        let mut previous = None;
        for (index, (at, ch)) in text.char_indices().enumerate() {
            folded.chars.push(fold(ch));
            if begins_word(previous, ch) {
                folded.words.push(index);
            }
            folded.bytes.push(at);
            previous = Some(ch);
        }
        folded.bytes.push(text.len());
        folded
    }

    fn prefix_at(&self, term: &[char], at: usize) -> bool {
        self.chars
            .get(at..)
            .is_some_and(|rest| rest.starts_with(term))
    }

    fn word_starts(&self) -> impl Iterator<Item = usize> + '_ {
        self.words.iter().copied()
    }

    fn find(&self, term: &[char]) -> Option<usize> {
        (0..=self.chars.len().checked_sub(term.len())?).find(|&at| self.prefix_at(term, at))
    }

    /// The word starts whose characters spell `term`, consecutively from any
    /// word: `fm` in "File Manager".
    fn initials(&self, term: &[char]) -> Option<Vec<usize>> {
        if term.len() < 2 {
            return None;
        }
        let last = self.words.len().checked_sub(term.len())?;
        (0..=last).find_map(|first| {
            let words = self.words.get(first..first + term.len())?;
            words
                .iter()
                .zip(term)
                .all(|(&at, want)| self.chars.get(at) == Some(want))
                .then(|| words.to_vec())
        })
    }

    /// The first placement of `term`'s characters in order from a word's
    /// start, the earliest characters taken: `tedit` in `TextEdit`.
    fn scattered(&self, term: &[char]) -> Option<Vec<usize>> {
        let (first, rest) = term.split_first()?;
        if rest.is_empty() {
            return None;
        }
        self.word_starts()
            .filter(|&at| self.chars.get(at) == Some(first))
            .find_map(|start| {
                let mut found = Vec::with_capacity(term.len());
                found.push(start);
                let mut next = start + 1;
                for want in rest {
                    let at = self.chars.get(next..)?.iter().position(|ch| ch == want)? + next;
                    found.push(at);
                    next = at + 1;
                }
                Some(found)
            })
    }
}

/// Whether `ch` begins a word after `previous`: after a non-alphanumeric, at a
/// lower-to-upper case step (`TextEdit`), or where letters meet digits.
fn begins_word(previous: Option<char>, ch: char) -> bool {
    if !ch.is_alphanumeric() {
        return false;
    }
    match previous {
        None => true,
        Some(prev) if !prev.is_alphanumeric() => true,
        Some(prev) => {
            (prev.is_lowercase() && ch.is_uppercase())
                || (prev.is_alphabetic() != ch.is_alphabetic())
        }
    }
}

/// One program as the search reads it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Indexed {
    id: EntryId,
    category: LibraryCategory,
    name: Folded,
    purpose: Folded,
    folder: Folded,
}

impl Indexed {
    /// Fold `entry`, with its manifest's `purpose` where one is known, filed
    /// under the folder named `folder`.
    pub(crate) fn new(entry: &LibraryEntry, purpose: Option<&str>, folder: &str) -> Self {
        Self {
            id: entry.id().clone(),
            category: entry.category(),
            name: Folded::new(entry.name().as_str()),
            purpose: purpose.map(Folded::new).unwrap_or_default(),
            folder: Folded::new(folder),
        }
    }

    /// The program this reads.
    pub(crate) const fn id(&self) -> &EntryId {
        &self.id
    }

    /// The folder it is filed under.
    pub(crate) const fn category(&self) -> LibraryCategory {
        self.category
    }

    /// How well `term` matches, and which of the name's characters it took.
    fn matched(&self, term: &[char], whole_query: bool) -> Option<(Tier, Vec<usize>)> {
        let span = |at: usize| (at..at + term.len()).collect::<Vec<usize>>();
        let name = &self.name;
        if whole_query && name.chars == term {
            return Some((Tier::NameWhole, span(0)));
        }
        if name.prefix_at(term, 0) {
            return Some((Tier::NamePrefix, span(0)));
        }
        if let Some(at) = name.word_starts().find(|&at| name.prefix_at(term, at)) {
            return Some((Tier::NameWord, span(at)));
        }
        if let Some(found) = name.initials(term) {
            return Some((Tier::NameInitials, found));
        }
        if let Some(at) = name.find(term) {
            return Some((Tier::NameInside, span(at)));
        }
        if let Some(found) = name.scattered(term) {
            return Some((Tier::NameScattered, found));
        }
        let purpose = &self.purpose;
        if purpose.word_starts().any(|at| purpose.prefix_at(term, at)) {
            return Some((Tier::PurposeWord, Vec::new()));
        }
        if term.len() >= 3 && purpose.find(term).is_some() {
            return Some((Tier::PurposeInside, Vec::new()));
        }
        let folder = &self.folder;
        folder
            .word_starts()
            .any(|at| folder.prefix_at(term, at))
            .then(|| (Tier::Folder, Vec::new()))
    }

    /// The name's byte ranges covering the characters at `positions`, merged
    /// into runs, in order.
    fn emphasis(&self, mut positions: Vec<usize>) -> Vec<Range<usize>> {
        positions.sort_unstable();
        positions.dedup();
        let mut runs: Vec<Range<usize>> = Vec::new();
        for at in positions {
            let (Some(&start), Some(&end)) = (self.name.bytes.get(at), self.name.bytes.get(at + 1))
            else {
                continue;
            };
            match runs.last_mut() {
                Some(run) if run.end == start => run.end = end,
                _ => runs.push(start..end),
            }
        }
        runs
    }
}

/// One program a query found.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Hit {
    /// Its place in the index searched.
    pub(crate) index: usize,
    /// The name's bytes the query matched.
    pub(crate) emphasis: Vec<Range<usize>>,
}

/// The query's terms, folded and bounded.
pub(crate) fn terms(query: &str) -> Vec<Vec<char>> {
    query
        .split_whitespace()
        .take(MAX_TERMS)
        .map(|term| term.chars().map(fold).take(MAX_TERM_CHARS).collect())
        .collect()
}

/// Every program of `index` matching all of `query`'s terms, best first.
///
/// `weights` says how much each program has been used; it ranks programs that
/// matched equally well. A query of no terms finds nothing.
pub(crate) fn search(index: &[Indexed], query: &str, weights: &BTreeMap<EntryId, u32>) -> Vec<Hit> {
    let terms = terms(query);
    if terms.is_empty() {
        return Vec::new();
    }
    let whole_query = terms.len() == 1;
    let mut scored: Vec<(u32, &Indexed, Hit)> = index
        .iter()
        .enumerate()
        .filter_map(|(at, program)| {
            let mut score = 0u32;
            let mut taken = Vec::new();
            for term in &terms {
                let (tier, found) = program.matched(term, whole_query)?;
                score = score.saturating_add(tier.weight());
                taken.extend(found);
            }
            let emphasis = program.emphasis(taken);
            Some((
                score,
                program,
                Hit {
                    index: at,
                    emphasis,
                },
            ))
        })
        .collect();
    let weight = |program: &Indexed| Reverse(weights.get(&program.id).copied().unwrap_or(0));
    scored.sort_by(|(a_score, a, _), (b_score, b, _)| {
        a_score
            .cmp(b_score)
            .then_with(|| weight(a).cmp(&weight(b)))
            .then_with(|| a.name.chars.cmp(&b.name.chars))
            .then_with(|| a.id.cmp(&b.id))
    });
    scored.into_iter().map(|(_, _, hit)| hit).collect()
}

#[cfg(test)]
#[path = "search_tests.rs"]
mod tests;
