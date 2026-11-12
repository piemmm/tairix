//! The document `EDIT` holds: its bytes in a gap buffer over the start of
//! the store it is given, the gap kept where the last change was made, so
//! typing moves nothing. Lines end with CR LF, as the files it reads and
//! writes do; a lone LF ends one too. What lies past the document's
//! capacity, at the store's end, is the caller's: `EDIT` keeps its
//! clipboard and a dialog's directory listing there, the capacity resized
//! to make the room.

use alloc::vec::Vec;
use core::ops::Range;

pub const CR: u8 = 0x0D;
pub const LF: u8 = 0x0A;

/// What the bytes are kept in.
pub trait Store {
    /// The bytes it holds.
    fn capacity(&self) -> u32;
    fn read(&mut self, at: u32, out: &mut [u8]);
    fn write(&mut self, at: u32, data: &[u8]);
}

/// Where the gap lies in the store.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Gap {
    pub start: u32,
    pub end: u32,
}

/// A change the store has no room for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Full;

/// The bytes moved or searched at a time.
const CHUNK: u32 = 512;

/// A document over the first `capacity` bytes of `store`, its gap at `gap`.
pub struct Text<'s, S: Store> {
    store: &'s mut S,
    gap: Gap,
    capacity: u32,
}

/// `n` as an index; every count here is of bytes in memory, which a `u32`
/// holds and a `usize` holds on every target.
fn index(n: u32) -> usize {
    usize::try_from(n).unwrap_or(usize::MAX)
}

/// The length of `bytes`, which lie in a store a `u32` measures.
fn length(bytes: &[u8]) -> u32 {
    u32::try_from(bytes.len()).unwrap_or(u32::MAX)
}

impl<'s, S: Store> Text<'s, S> {
    /// The document over `store`'s first `capacity` bytes with its gap at
    /// `gap`; a layout that does not fit the store is taken as an empty
    /// document over all of it.
    pub fn new(store: &'s mut S, gap: Gap, capacity: u32) -> Self {
        let fits = gap.start <= gap.end && gap.end <= capacity && capacity <= store.capacity();
        let (gap, capacity) = if fits {
            (gap, capacity)
        } else {
            (
                Gap {
                    start: 0,
                    end: store.capacity(),
                },
                store.capacity(),
            )
        };
        Self {
            store,
            gap,
            capacity,
        }
    }

    #[must_use]
    pub const fn gap(&self) -> Gap {
        self.gap
    }

    #[must_use]
    pub const fn capacity(&self) -> u32 {
        self.capacity
    }

    const fn gap_len(&self) -> u32 {
        self.gap.end - self.gap.start
    }

    /// The document's bytes.
    #[must_use]
    pub const fn len(&self) -> u32 {
        self.capacity - self.gap_len()
    }

    /// The bytes the store holds, the document's and those past it.
    #[must_use]
    pub fn store_len(&self) -> u32 {
        self.store.capacity()
    }

    /// The room the document has to grow into.
    #[must_use]
    pub const fn room(&self) -> u32 {
        self.gap_len()
    }

    /// Where byte `at` of the document lies in the store.
    const fn physical(&self, at: u32) -> u32 {
        if at < self.gap.start {
            at
        } else {
            at + self.gap_len()
        }
    }

    /// Its bytes `range`, clamped to the document.
    pub fn read(&mut self, range: Range<u32>) -> Vec<u8> {
        let end = range.end.min(self.len());
        let start = range.start.min(end);
        let mut out = alloc::vec![0u8; index(end - start)];
        self.read_into(start, &mut out);
        out
    }

    /// `out` filled from byte `at`, as much of it as the document reaches:
    /// the bytes it filled.
    fn read_into(&mut self, at: u32, out: &mut [u8]) -> usize {
        let end = at.saturating_add(length(out)).min(self.len());
        let start = at.min(end);
        let filled = index(end - start);
        let before = self.gap.start.clamp(start, end);
        let split = index(before - start);
        self.store.read(start, &mut out[..split]);
        self.store
            .read(before + self.gap_len(), &mut out[split..filled]);
        filled
    }

    /// Its byte at `at`.
    pub fn byte(&mut self, at: u32) -> Option<u8> {
        if at >= self.len() {
            return None;
        }
        let mut out = [0u8];
        let physical = self.physical(at);
        self.store.read(physical, &mut out);
        Some(out[0])
    }

    /// The store's bytes `from..to` carried to begin at `at`, a piece at a
    /// time in the order that never overwrites one not yet carried.
    fn carry(&mut self, from: u32, to: u32, at: u32) {
        let mut chunk = [0u8; CHUNK as usize];
        let len = to - from;
        let mut done = 0;
        while done < len {
            let n = (len - done).min(CHUNK);
            let offset = if at > from { len - done - n } else { done };
            let piece = &mut chunk[..index(n)];
            self.store.read(from + offset, piece);
            self.store.write(at + offset, piece);
            done += n;
        }
    }

    /// The gap moved to `to`, the bytes between carried across it.
    fn move_gap(&mut self, to: u32) {
        let gap = self.gap;
        if to < gap.start {
            self.carry(to, gap.start, gap.end - (gap.start - to));
        } else if to > gap.start {
            self.carry(gap.end, gap.end + (to - gap.start), gap.start);
        }
        self.gap = Gap {
            start: to,
            end: to + (gap.end - gap.start),
        };
    }

    /// `bytes` put in at `at`.
    ///
    /// # Errors
    ///
    /// [`Full`] where the store has no room for them, nothing changed.
    pub fn insert(&mut self, at: u32, bytes: &[u8]) -> Result<(), Full> {
        let n = u32::try_from(bytes.len()).map_err(|_| Full)?;
        if n > self.gap_len() {
            return Err(Full);
        }
        self.move_gap(at.min(self.len()));
        self.store.write(self.gap.start, bytes);
        self.gap.start += n;
        Ok(())
    }

    /// `n` bytes written at the gap's start by another — a file read into
    /// it — made the document's.
    ///
    /// # Errors
    ///
    /// [`Full`] where the gap is smaller, nothing changed.
    pub fn admit(&mut self, n: u32) -> Result<(), Full> {
        if n > self.gap_len() {
            return Err(Full);
        }
        self.gap.start += n;
        Ok(())
    }

    /// Bytes `range` taken out.
    pub fn delete(&mut self, range: Range<u32>) {
        let end = range.end.min(self.len());
        let start = range.start.min(end);
        self.move_gap(start);
        self.gap.end += end - start;
    }

    /// The document's capacity made `capacity`, the bytes past its gap
    /// carried to its new end.
    ///
    /// # Errors
    ///
    /// [`Full`] where the document would not fit or the store is smaller,
    /// nothing changed.
    pub fn resize(&mut self, capacity: u32) -> Result<(), Full> {
        if capacity < self.len() || capacity > self.store.capacity() {
            return Err(Full);
        }
        let end = if capacity < self.capacity {
            self.gap.end - (self.capacity - capacity)
        } else {
            self.gap.end + (capacity - self.capacity)
        };
        self.carry(self.gap.end, self.capacity, end);
        self.gap.end = end;
        self.capacity = capacity;
        Ok(())
    }

    /// The bytes `back` from the store's end, past the document: `out`
    /// filled from there; nothing read where they would reach into it.
    pub fn read_end(&mut self, back: u32, out: &mut [u8]) {
        let Some(at) = self.beyond(back, length(out)) else {
            return;
        };
        self.store.read(at, out);
    }

    /// `data` written `back` from the store's end, past the document;
    /// nothing written where it would reach into it.
    pub fn write_end(&mut self, back: u32, data: &[u8]) {
        let Some(at) = self.beyond(back, length(data)) else {
            return;
        };
        self.store.write(at, data);
    }

    /// Where `len` bytes `back` from the store's end begin, if they lie
    /// wholly past the document.
    fn beyond(&self, back: u32, len: u32) -> Option<u32> {
        let at = self.store.capacity().checked_sub(back)?;
        (at >= self.capacity && back >= len).then_some(at)
    }

    /// The line ends among bytes `range`.
    pub fn breaks_in(&mut self, range: Range<u32>) -> u32 {
        let end = range.end.min(self.len());
        let mut start = range.start.min(end);
        let mut chunk = [0u8; CHUNK as usize];
        let mut lines = 0;
        while start < end {
            let n = (end - start).min(CHUNK);
            let piece = &mut chunk[..index(n)];
            self.read_into(start, piece);
            lines += line_ends(piece);
            start += n;
        }
        lines
    }

    /// Where the line holding `at` begins.
    pub fn line_start(&mut self, at: u32) -> u32 {
        let mut end = at.min(self.len());
        let mut chunk = [0u8; CHUNK as usize];
        while end > 0 {
            let start = end.saturating_sub(CHUNK);
            let piece = &mut chunk[..index(end - start)];
            self.read_into(start, piece);
            if let Some(found) = piece.iter().rposition(|&byte| byte == LF) {
                return start + length(&piece[..found]) + 1;
            }
            end = start;
        }
        0
    }

    /// Where the line holding `at` ends: at the CR of its CR LF, at its
    /// lone LF, or at the document's end; a lone CR is a character.
    pub fn line_end(&mut self, at: u32) -> u32 {
        let len = self.len();
        let mut start = at.min(len);
        let mut chunk = [0u8; CHUNK as usize];
        while start < len {
            let piece = &mut chunk[..index((len - start).min(CHUNK))];
            self.read_into(start, piece);
            if let Some(found) = piece.iter().position(|&byte| byte == LF) {
                let lf = start + length(&piece[..found]);
                let cr = lf > 0 && self.byte(lf - 1) == Some(CR);
                return if cr { lf - 1 } else { lf };
            }
            start += length(piece);
        }
        len
    }

    /// Where the line after the one holding `at` begins, none for the last.
    pub fn next_line(&mut self, at: u32) -> Option<u32> {
        let end = self.line_end(at);
        self.after_end(end)
    }

    /// Where the line after one ending at `end` begins, none for the last.
    pub fn after_end(&mut self, end: u32) -> Option<u32> {
        match self.byte(end)? {
            CR if self.byte(end + 1) == Some(LF) => Some(end + 2),
            _ => Some(end + 1),
        }
    }

    /// Where the line before the one holding `at` begins, none for the
    /// first.
    pub fn previous_line(&mut self, at: u32) -> Option<u32> {
        let start = self.line_start(at);
        (start > 0).then(|| self.line_start(start - 1))
    }

    /// Where `needle` next occurs at or after `from`, compared without
    /// regard to case unless `exact`, and not inside a word when `whole`
    /// asks for whole words: the document searched to its end, then from its
    /// start.
    pub fn find(&mut self, needle: &[u8], from: u32, exact: bool, whole: bool) -> Option<u32> {
        let from = from.min(self.len());
        let wanted = Wanted {
            needle,
            exact,
            whole,
        };
        self.scan(&wanted, from..self.len())
            .or_else(|| self.scan(&wanted, 0..from))
    }

    /// Where `needle` first occurs starting in `range`, as [`Self::find`]
    /// compares it.
    pub fn find_in(
        &mut self,
        needle: &[u8],
        range: Range<u32>,
        exact: bool,
        whole: bool,
    ) -> Option<u32> {
        self.scan(
            &Wanted {
                needle,
                exact,
                whole,
            },
            range,
        )
    }

    fn scan(&mut self, wanted: &Wanted<'_>, range: Range<u32>) -> Option<u32> {
        let width = u32::try_from(wanted.needle.len())
            .ok()
            .filter(|&width| width > 0)?;
        let end = range.end.min(self.len().checked_sub(width)? + 1);
        let mut chunk = alloc::vec![0u8; index(CHUNK + width + 1)];
        let mut at = range.start;
        while at < end {
            let stop = (at + CHUNK).min(end);
            let context = u32::from(at > 0);
            let filled = self.read_into(at - context, &mut chunk);
            let piece = &chunk[..filled];
            if let Some(found) =
                (at..stop).find(|&start| wanted.at(piece, index(start - at + context)))
            {
                return Some(found);
            }
            at = stop;
        }
        None
    }
}

/// What a search looks for.
struct Wanted<'a> {
    needle: &'a [u8],
    exact: bool,
    whole: bool,
}

/// The line ends `bytes` holds.
#[must_use]
pub fn line_ends(bytes: &[u8]) -> u32 {
    bytes
        .iter()
        .fold(0, |lines, &byte| lines + u32::from(byte == LF))
}

/// Whether `byte` is part of a word, as a whole-word search reckons it.
#[must_use]
pub fn in_word(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

impl Wanted<'_> {
    /// Whether it occurs at `offset` in `piece`, the bytes either side
    /// there as context.
    fn at(&self, piece: &[u8], offset: usize) -> bool {
        let Some(found) = piece.get(offset..offset + self.needle.len()) else {
            return false;
        };
        let same = if self.exact {
            found == self.needle
        } else {
            found.eq_ignore_ascii_case(self.needle)
        };
        let word = |byte: Option<&u8>| byte.is_some_and(|&byte| in_word(byte));
        let before = offset.checked_sub(1).and_then(|before| piece.get(before));
        same && (!self.whole || (!word(before) && !word(piece.get(offset + self.needle.len()))))
    }
}

#[cfg(test)]
#[path = "text_tests.rs"]
mod tests;
