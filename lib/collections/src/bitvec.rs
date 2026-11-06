//! A bitmap of run-time length whose summary levels find the next or previous
//! set or clear bit in time proportional to their height, not to the bitmap.
//!
//! Above the words of bits stand two summaries, each a tower of levels with
//! one bit per word of the level beneath: one marks the words holding a set
//! bit, the other the words holding a clear bit within the length. A search
//! looks in its own word, climbs while each level shows nothing past where it
//! began, and descends to the first mark it meets, so it costs at most twice
//! the height — eleven levels for any length a `usize` can name.

use alloc::vec::Vec;
use core::fmt;
use core::ops::Range;

use crate::TryReserveError;

const WORD: usize = u64::BITS as usize;

/// The most summary levels any bitmap needs: 64^11 words exceeds what a
/// `usize` can count.
const MAX_LEVELS: usize = 11;

/// A bitmap whose length is chosen at run time.
///
/// Reads, writes, searches and walks allocate nothing; only
/// [`try_resize`](Self::try_resize), [`try_reserve`](Self::try_reserve) and
/// [`try_clone`](Self::try_clone) do, each fallibly and all or nothing.
pub struct BitVec {
    len: usize,
    ones: usize,
    /// The bits, low first; those past `len` are always clear.
    words: Vec<u64>,
    /// Which words hold a set bit.
    set: Summary,
    /// Which words hold a clear bit within `len`.
    clear: Summary,
}

/// A tower of levels over a bitmap's words, level after level in one vector:
/// one bit per word of the level beneath, set where that word is not zero.
struct Summary {
    levels: Vec<u64>,
    /// The level words searches have read, for the tests' cost gate.
    #[cfg(test)]
    reads: core::cell::Cell<usize>,
}

/// Where each summary level of a bitmap of `words` words lies in its vector.
#[derive(Clone, Copy)]
struct Spans {
    spans: [(usize, usize); MAX_LEVELS],
    count: usize,
}

impl Spans {
    fn of(words: usize) -> Self {
        let mut spans = [(0, 0); MAX_LEVELS];
        let (mut count, mut below, mut at) = (0, words, 0);
        while below > 1 && count < MAX_LEVELS {
            let len = below.div_ceil(WORD);
            spans[count] = (at, len);
            count += 1;
            at += len;
            below = len;
        }
        Self { spans, count }
    }

    fn total(self) -> usize {
        self.spans[..self.count].iter().map(|&(_, len)| len).sum()
    }

    fn level(self, level: usize) -> Option<(usize, usize)> {
        self.spans[..self.count].get(level).copied()
    }
}

impl Summary {
    const fn new() -> Self {
        Self::over(Vec::new())
    }

    const fn over(levels: Vec<u64>) -> Self {
        Self {
            levels,
            #[cfg(test)]
            reads: core::cell::Cell::new(0),
        }
    }

    /// Level word `at`, as a search reads it.
    fn read(&self, at: usize) -> Option<u64> {
        #[cfg(test)]
        self.reads.set(self.reads.get() + 1);
        self.levels.get(at).copied()
    }

    /// Mark word `index` of the bitmap beneath as `marked`, carrying the change
    /// up only as far as a level's word changes between zero and not.
    fn mark(&mut self, spans: Spans, mut index: usize, mut marked: bool) {
        for level in 0..spans.count {
            let Some((start, _)) = spans.level(level) else {
                return;
            };
            let Some(word) = self.levels.get_mut(start + index / WORD) else {
                return;
            };
            let was = *word != 0;
            let bit = 1u64 << (index % WORD);
            if marked {
                *word |= bit;
            } else {
                *word &= !bit;
            }
            if (*word != 0) == was {
                return;
            }
            marked = !was;
            index /= WORD;
        }
    }

    /// Mark every word of `words` by `marked`, each level built from the one
    /// beneath.
    fn rebuild(&mut self, spans: Spans, words: &[u64], marked: impl Fn(usize, u64) -> bool) {
        self.levels.clear();
        self.levels.resize(spans.total(), 0);
        let Some((base, _)) = spans.level(0) else {
            return;
        };
        for (index, &word) in words.iter().enumerate() {
            if marked(index, word) {
                if let Some(level) = self.levels.get_mut(base + index / WORD) {
                    *level |= 1 << (index % WORD);
                }
            }
        }
        for level in 1..spans.count {
            let (Some((below, below_len)), Some((start, _))) =
                (spans.level(level - 1), spans.level(level))
            else {
                return;
            };
            for index in 0..below_len {
                if self
                    .levels
                    .get(below + index)
                    .is_some_and(|&word| word != 0)
                {
                    if let Some(word) = self.levels.get_mut(start + index / WORD) {
                        *word |= 1 << (index % WORD);
                    }
                }
            }
        }
    }

    /// The first word at or past `from` that is marked.
    fn successor(&self, spans: Spans, from: usize) -> Option<usize> {
        let (mut level, mut at) = (0, from);
        let found = loop {
            let (start, len) = spans.level(level)?;
            let index = at / WORD;
            if index >= len {
                return None;
            }
            let bits = self.read(start + index)? & (u64::MAX << (at % WORD));
            if bits != 0 {
                break index * WORD + bits.trailing_zeros() as usize;
            }
            at = index + 1;
            level += 1;
        };
        self.descend(spans, level, found, |word| word.trailing_zeros() as usize)
    }

    /// The last word before `before` that is marked.
    fn predecessor(&self, spans: Spans, before: usize) -> Option<usize> {
        let (mut level, mut at) = (0, before.checked_sub(1)?);
        let found = loop {
            let (start, len) = spans.level(level)?;
            let index = at / WORD;
            if index >= len {
                return None;
            }
            let bits = self.read(start + index)? & (u64::MAX >> (WORD - 1 - at % WORD));
            if bits != 0 {
                break index * WORD + highest(bits);
            }
            at = index.checked_sub(1)?;
            level += 1;
        };
        self.descend(spans, level, found, highest)
    }

    /// From mark `at` of `level`, down to the word of the bitmap it leads to,
    /// taking at each level the bit `pick` chooses of the word it names.
    fn descend(
        &self,
        spans: Spans,
        mut level: usize,
        mut at: usize,
        pick: impl Fn(u64) -> usize,
    ) -> Option<usize> {
        while level > 0 {
            level -= 1;
            let (start, _) = spans.level(level)?;
            let word = self.read(start + at)?;
            at = at * WORD + pick(word);
        }
        Some(at)
    }
}

/// The index of the highest set bit of `word`, which is not zero.
const fn highest(word: u64) -> usize {
    WORD - 1 - word.leading_zeros() as usize
}

/// The bits of a word below position `bits`; all of them for 64 or more.
const fn below(bits: usize) -> u64 {
    if bits >= WORD {
        u64::MAX
    } else {
        (1 << bits) - 1
    }
}

/// Grow `vec`'s storage to hold `len` elements.
fn reserve_to(vec: &mut Vec<u64>, len: usize) -> Result<(), TryReserveError> {
    vec.try_reserve(len.saturating_sub(vec.len()))
        .map_err(|_| TryReserveError::AllocFailed)
}

impl BitVec {
    /// An empty bitmap.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            len: 0,
            ones: 0,
            words: Vec::new(),
            set: Summary::new(),
            clear: Summary::new(),
        }
    }

    /// A bitmap of `len` bits, each `value`.
    ///
    /// # Errors
    ///
    /// As [`try_resize`](Self::try_resize).
    pub fn try_with_len(len: usize, value: bool) -> Result<Self, TryReserveError> {
        let mut bits = Self::new();
        bits.try_resize(len, value)?;
        Ok(bits)
    }

    /// How many bits it holds.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether it holds no bits.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// How many of its bits are set.
    #[must_use]
    pub const fn count_ones(&self) -> usize {
        self.ones
    }

    /// How many of its bits are clear.
    #[must_use]
    pub const fn count_zeros(&self) -> usize {
        self.len - self.ones
    }

    /// Bit `index`, or `None` past the end.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<bool> {
        if index >= self.len {
            return None;
        }
        let word = self.words.get(index / WORD)?;
        Some(word >> (index % WORD) & 1 == 1)
    }

    /// Make bit `index` `value`, answering what it was, or `None`, changing
    /// nothing, past the end.
    pub fn set(&mut self, index: usize, value: bool) -> Option<bool> {
        if index >= self.len {
            return None;
        }
        let at = index / WORD;
        let bit = 1u64 << (index % WORD);
        let word = self.words.get_mut(at)?;
        let was = *word & bit != 0;
        if was == value {
            return Some(was);
        }
        if value {
            *word |= bit;
            self.ones += 1;
        } else {
            *word &= !bit;
            self.ones -= 1;
        }
        let word = *word;
        self.marked(Spans::of(self.words.len()), at, word);
        Some(was)
    }

    /// Make every bit of `range` `value`, answering whether it lies within
    /// the bitmap; one that does not changes nothing.
    pub fn set_range(&mut self, range: Range<usize>, value: bool) -> bool {
        if range.start > range.end || range.end > self.len {
            return false;
        }
        let spans = Spans::of(self.words.len());
        let mut at = range.start;
        while at < range.end {
            let index = at / WORD;
            let upto = range.end.min((index + 1) * WORD);
            let mask = below(upto - index * WORD) & !below(at % WORD);
            let Some(word) = self.words.get_mut(index) else {
                return false;
            };
            let held = (*word & mask).count_ones() as usize;
            if value {
                *word |= mask;
                self.ones += mask.count_ones() as usize - held;
            } else {
                *word &= !mask;
                self.ones -= held;
            }
            let word = *word;
            self.marked(spans, index, word);
            at = upto;
        }
        true
    }

    /// Make every bit `value`.
    pub fn fill(&mut self, value: bool) {
        self.set_range(0..self.len, value);
    }

    /// The first set bit at or past `from`.
    #[must_use]
    pub fn next_one(&self, from: usize) -> Option<usize> {
        self.next(from, |_, word| word, &self.set)
    }

    /// The first clear bit at or past `from`.
    #[must_use]
    pub fn next_zero(&self, from: usize) -> Option<usize> {
        self.next(from, |index, word| !word & self.valid(index), &self.clear)
    }

    /// The last set bit before `before`.
    #[must_use]
    pub fn prev_one(&self, before: usize) -> Option<usize> {
        self.prev(before, |_, word| word, &self.set)
    }

    /// The last clear bit before `before`.
    #[must_use]
    pub fn prev_zero(&self, before: usize) -> Option<usize> {
        self.prev(before, |index, word| !word & self.valid(index), &self.clear)
    }

    /// Its set bits, low first.
    #[must_use]
    pub fn iter_ones(&self) -> Ones<'_> {
        Ones {
            bits: self,
            index: 0,
            word: self.words.first().copied().unwrap_or(0),
        }
    }

    /// Make it `len` bits long, any bits it gains `value`.
    ///
    /// # Errors
    ///
    /// [`TryReserveError::AllocFailed`] when the storage cannot grow, leaving
    /// the bitmap as it was.
    pub fn try_resize(&mut self, len: usize, value: bool) -> Result<(), TryReserveError> {
        let words = len.div_ceil(WORD);
        let spans = Spans::of(words);
        reserve_to(&mut self.words, words)?;
        reserve_to(&mut self.set.levels, spans.total())?;
        reserve_to(&mut self.clear.levels, spans.total())?;
        if len < self.len {
            let partial = self
                .words
                .get(words.saturating_sub(1))
                .filter(|_| !len.is_multiple_of(WORD))
                .map_or(0, |word| (word & !below(len % WORD)).count_ones());
            let dropped: u32 = self
                .words
                .get(words..)
                .map_or(0, |gone| gone.iter().map(|word| word.count_ones()).sum());
            self.ones -= (partial + dropped) as usize;
            self.words.truncate(words);
        } else if len > self.len {
            if value && !self.len.is_multiple_of(WORD) {
                if let Some(last) = self.words.last_mut() {
                    *last |= !below(self.len % WORD);
                }
            }
            self.words.resize(words, if value { u64::MAX } else { 0 });
            if value {
                self.ones += len - self.len;
            }
        }
        if !len.is_multiple_of(WORD) {
            if let Some(last) = self.words.last_mut() {
                *last &= below(len % WORD);
            }
        }
        self.len = len;
        self.rebuild();
        Ok(())
    }

    /// Make room for `additional` more bits, so a growth to that length
    /// allocates nothing.
    ///
    /// # Errors
    ///
    /// [`TryReserveError::CapacityOverflow`] where the length overflows, and
    /// [`TryReserveError::AllocFailed`] where the storage cannot grow, leaving
    /// the bitmap as it was.
    pub fn try_reserve(&mut self, additional: usize) -> Result<(), TryReserveError> {
        let len = self
            .len
            .checked_add(additional)
            .ok_or(TryReserveError::CapacityOverflow)?;
        let words = len.div_ceil(WORD);
        let summaries = Spans::of(words).total();
        reserve_to(&mut self.words, words)?;
        reserve_to(&mut self.set.levels, summaries)?;
        reserve_to(&mut self.clear.levels, summaries)
    }

    /// A copy of it.
    ///
    /// # Errors
    ///
    /// [`TryReserveError::AllocFailed`] when the copy's storage cannot be
    /// allocated.
    pub fn try_clone(&self) -> Result<Self, TryReserveError> {
        let copy = |from: &Vec<u64>| -> Result<Vec<u64>, TryReserveError> {
            let mut to = Vec::new();
            reserve_to(&mut to, from.len())?;
            to.extend_from_slice(from);
            Ok(to)
        };
        Ok(Self {
            len: self.len,
            ones: self.ones,
            words: copy(&self.words)?,
            set: Summary::over(copy(&self.set.levels)?),
            clear: Summary::over(copy(&self.clear.levels)?),
        })
    }

    /// The bytes its storage holds.
    #[must_use]
    pub fn held_bytes(&self) -> usize {
        (self.words.capacity() + self.set.levels.capacity() + self.clear.levels.capacity())
            .saturating_mul(size_of::<u64>())
    }

    /// The bits of word `index` that lie within the length.
    fn valid(&self, index: usize) -> u64 {
        if index + 1 == self.words.len() {
            below(self.len - index * WORD)
        } else {
            u64::MAX
        }
    }

    /// Word `index` now holds `word`: mark it in both summaries.
    fn marked(&mut self, spans: Spans, index: usize, word: u64) {
        let holes = !word & self.valid(index) != 0;
        self.set.mark(spans, index, word != 0);
        self.clear.mark(spans, index, holes);
    }

    fn rebuild(&mut self) {
        let spans = Spans::of(self.words.len());
        self.set.rebuild(spans, &self.words, |_, word| word != 0);
        let last = self.words.len().saturating_sub(1);
        let tail = below(self.len.saturating_sub(last * WORD));
        self.clear.rebuild(spans, &self.words, |index, word| {
            let valid = if index == last { tail } else { u64::MAX };
            !word & valid != 0
        });
    }

    /// The first bit at or past `from` that `bits` picks out of its word,
    /// `summary` marking the words holding one.
    fn next(
        &self,
        from: usize,
        bits: impl Fn(usize, u64) -> u64,
        summary: &Summary,
    ) -> Option<usize> {
        if from >= self.len {
            return None;
        }
        let index = from / WORD;
        let here = bits(index, *self.words.get(index)?) & (u64::MAX << (from % WORD));
        if here != 0 {
            return Some(index * WORD + here.trailing_zeros() as usize);
        }
        let index = summary.successor(Spans::of(self.words.len()), index + 1)?;
        let word = bits(index, *self.words.get(index)?);
        Some(index * WORD + word.trailing_zeros() as usize)
    }

    /// The last bit before `before` that `bits` picks out of its word.
    fn prev(
        &self,
        before: usize,
        bits: impl Fn(usize, u64) -> u64,
        summary: &Summary,
    ) -> Option<usize> {
        let last = before.min(self.len).checked_sub(1)?;
        let index = last / WORD;
        let here = bits(index, *self.words.get(index)?) & below(last % WORD + 1);
        if here != 0 {
            return Some(index * WORD + highest(here));
        }
        let index = summary.predecessor(Spans::of(self.words.len()), index)?;
        let word = bits(index, *self.words.get(index)?);
        Some(index * WORD + highest(word))
    }
}

impl Default for BitVec {
    fn default() -> Self {
        Self::new()
    }
}

impl PartialEq for BitVec {
    fn eq(&self, other: &Self) -> bool {
        self.len == other.len && self.words == other.words
    }
}

impl Eq for BitVec {}

impl fmt::Debug for BitVec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BitVec")
            .field("len", &self.len)
            .field("ones", &self.ones)
            .finish_non_exhaustive()
    }
}

/// A bitmap's set bits, low first: a word's bits are taken in turn, and the
/// next word holding one is found through the summary.
pub struct Ones<'a> {
    bits: &'a BitVec,
    index: usize,
    word: u64,
}

impl Iterator for Ones<'_> {
    type Item = usize;

    fn next(&mut self) -> Option<usize> {
        while self.word == 0 {
            let spans = Spans::of(self.bits.words.len());
            self.index = self.bits.set.successor(spans, self.index + 1)?;
            self.word = *self.bits.words.get(self.index)?;
        }
        let bit = self.word.trailing_zeros() as usize;
        self.word &= self.word - 1;
        Some(self.index * WORD + bit)
    }
}

#[cfg(test)]
#[path = "bitvec_tests.rs"]
mod tests;
