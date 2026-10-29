//! A held file's bytes written and not yet confirmed, a block at a time.
//!
//! Each block keeps which of its bytes are held, so a write lands at a cost
//! bounded by the blocks it spans, however scattered what the file holds,
//! and a wave sends each stretch of held bytes as writes of at most one
//! call's worth. A block's bytes stay until the server confirms them, so a
//! read meanwhile sees them and a short write leaves its rest to send again.

use alloc::boxed::Box;
use core::ops::RangeInclusive;

use tairix_collections::RadixTree;

/// Bytes one block holds.
pub(super) const BLOCK: usize = 4096;

const BLOCK_LEN: u64 = BLOCK as u64;

const WORDS: usize = BLOCK / 64;

/// One block's bytes, and which of them are held.
struct Block {
    bytes: [u8; BLOCK],
    held: [u64; WORDS],
}

/// What one held block pins, beside its place in the index.
pub(super) const BLOCK_PINNED: usize = core::mem::size_of::<Block>();

/// Why bytes could not be kept.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) struct NoRoom;

/// The word masks of bits `[from, to)`, `from` below `to`.
fn masks(from: usize, to: usize) -> impl Iterator<Item = (usize, u64)> {
    (from / 64..=(to - 1) / 64).map(move |word| {
        let lo = from.max(word * 64) - word * 64;
        let hi = to.min(word * 64 + 64) - word * 64;
        let ones = if hi - lo == 64 {
            u64::MAX
        } else {
            ((1 << (hi - lo)) - 1) << lo
        };
        (word, ones)
    })
}

/// The first bit at or after `from` that is set, or that is clear when
/// `clear`.
fn scan(words: &[u64; WORDS], from: usize, clear: bool) -> Option<usize> {
    let flip = if clear { u64::MAX } else { 0 };
    let mut word = from / 64;
    let mut bits = (*words.get(word)? ^ flip) & (u64::MAX << (from % 64));
    loop {
        if bits != 0 {
            return Some(word * 64 + bits.trailing_zeros() as usize);
        }
        word += 1;
        bits = *words.get(word)? ^ flip;
    }
}

impl Block {
    const EMPTY: Self = Self {
        bytes: [0; BLOCK],
        held: [0; WORDS],
    };

    /// Hold bytes `[from, to)`: how many were not held.
    fn hold(&mut self, from: usize, to: usize) -> usize {
        let mut added = 0;
        for (word, ones) in masks(from, to) {
            added += (ones & !self.held[word]).count_ones() as usize;
            self.held[word] |= ones;
        }
        added
    }

    /// Let bytes `[from, to)` go: how many were held.
    fn let_go(&mut self, from: usize, to: usize) -> usize {
        let mut gone = 0;
        for (word, ones) in masks(from, to) {
            gone += (ones & self.held[word]).count_ones() as usize;
            self.held[word] &= !ones;
        }
        gone
    }

    fn is_empty(&self) -> bool {
        self.held.iter().all(|&word| word == 0)
    }

    /// The end of the stretch of held bytes from `from`, which is held.
    fn held_until(&self, from: usize) -> usize {
        scan(&self.held, from, true).unwrap_or(BLOCK)
    }

    /// Each stretch of held bytes within `[from, to)`.
    fn stretches(&self, from: usize, to: usize) -> impl Iterator<Item = (usize, usize)> + '_ {
        let mut at = from;
        core::iter::from_fn(move || {
            let start = scan(&self.held, at, false).filter(|&start| start < to)?;
            let end = self.held_until(start).min(to);
            at = end;
            Some((start, end))
        })
    }
}

/// The blocks bytes `[offset, end)` touch, `end` past `offset`.
const fn touched(offset: u64, end: u64) -> RangeInclusive<u64> {
    offset / BLOCK_LEN..=(end - 1) / BLOCK_LEN
}

/// Where bytes `[offset, end)` lie within block `key`, as offsets into it.
fn within(key: u64, offset: u64, end: u64) -> (usize, usize) {
    let base = key * BLOCK_LEN;
    // Within the block, so it fits.
    let clip = |at: u64| usize::try_from(at.saturating_sub(base).min(BLOCK_LEN)).unwrap_or(BLOCK);
    (clip(offset), clip(end))
}

/// A file's written bytes.
#[derive(Default)]
pub(super) struct Written {
    blocks: RadixTree<Box<Block>>,
    /// Bytes held.
    bytes: usize,
    /// When the oldest byte held was written.
    since: Option<u64>,
}

impl Written {
    pub(super) const fn is_empty(&self) -> bool {
        self.bytes == 0
    }

    /// Bytes held.
    pub(super) const fn bytes(&self) -> usize {
        self.bytes
    }

    /// Blocks holding them.
    pub(super) const fn blocks(&self) -> usize {
        self.blocks.len()
    }

    /// Memory holding them: the blocks, and the index over them.
    pub(super) fn pinned(&self) -> usize {
        self.blocks.len() * BLOCK_PINNED + self.blocks.allocated_bytes()
    }

    /// When the oldest byte held was written.
    pub(super) const fn since(&self) -> Option<u64> {
        self.since
    }

    /// The end of the last byte held, or zero with none.
    pub(super) fn end(&self) -> u64 {
        self.blocks
            .prev(u64::MAX)
            .and_then(|(key, block)| {
                let (word, bits) = block
                    .held
                    .iter()
                    .enumerate()
                    .rev()
                    .find(|(_, &bits)| bits != 0)?;
                let last = word * 64 + (63 - bits.leading_zeros() as usize);
                Some(key * BLOCK_LEN + last as u64 + 1)
            })
            .unwrap_or(0)
    }

    /// Blocks a write of `len` bytes at `offset` would add.
    pub(super) fn new_blocks(&self, offset: u64, len: usize) -> usize {
        match offset.checked_add(len as u64) {
            Some(end) if len > 0 => touched(offset, end)
                .filter(|&key| !self.blocks.contains_key(key))
                .count(),
            _ => 0,
        }
    }

    /// Hold `data`, written at `offset` at `now`, over whatever was held
    /// there: the bytes it newly holds.
    ///
    /// # Errors
    ///
    /// [`NoRoom`], holding what was held before.
    pub(super) fn write(&mut self, offset: u64, data: &[u8], now: u64) -> Result<usize, NoRoom> {
        if data.is_empty() {
            return Ok(0);
        }
        let end = offset.checked_add(data.len() as u64).ok_or(NoRoom)?;
        // Every block the write needs is had before any byte lands, so a
        // write is held whole or not at all.
        for key in touched(offset, end) {
            if self.blocks.contains_key(key) {
                continue;
            }
            let added = Box::try_new(Block::EMPTY)
                .map_err(|_| NoRoom)
                .and_then(|block| self.blocks.try_insert(key, block).map_err(|_| NoRoom));
            if added.is_err() {
                self.drop_empty(offset, end);
                return Err(NoRoom);
            }
        }
        let (mut added, mut laid) = (0, 0);
        for key in touched(offset, end) {
            let (from, to) = within(key, offset, end);
            let piece = &data[laid..laid + (to - from)];
            laid += piece.len();
            if let Some(block) = self.blocks.get_mut(key) {
                block.bytes[from..to].copy_from_slice(piece);
                added += block.hold(from, to);
            }
        }
        self.bytes += added;
        self.since.get_or_insert(now);
        Ok(added)
    }

    /// Lay what is held of `[offset, offset + out.len())` over `out`.
    pub(super) fn overlay(&self, offset: u64, out: &mut [u8]) {
        if out.is_empty() || self.bytes == 0 {
            return;
        }
        let end = offset.saturating_add(out.len() as u64);
        let last = (end - 1) / BLOCK_LEN;
        for (key, block) in self.blocks.iter_from(offset / BLOCK_LEN) {
            if key > last {
                break;
            }
            let (from, to) = within(key, offset, end);
            for (start, stop) in block.stretches(from, to) {
                // Within the window asked, so it fits.
                let Ok(at) = usize::try_from(key * BLOCK_LEN + start as u64 - offset) else {
                    continue;
                };
                out[at..at + (stop - start)].copy_from_slice(&block.bytes[start..stop]);
            }
        }
    }

    /// The first stretch of held bytes at or after `from`, at most `max`
    /// long: its offset and length.
    pub(super) fn stretch(&self, from: u64, max: usize) -> Option<(u64, usize)> {
        let mut blocks = self.blocks.iter_from(from / BLOCK_LEN);
        let (mut key, start, mut until) = blocks.find_map(|(key, block)| {
            let (first, _) = within(key, from, u64::MAX);
            let start = scan(&block.held, first, false)?;
            Some((key, start, block.held_until(start)))
        })?;
        let offset = key * BLOCK_LEN + start as u64;
        let mut len = until - start;
        while until == BLOCK && len < max {
            match blocks.next() {
                Some((next, block)) if next == key + 1 && block.held[0] & 1 != 0 => {
                    key = next;
                    until = block.held_until(0);
                    len += until;
                }
                _ => break,
            }
        }
        Some((offset, len.min(max)))
    }

    /// The bytes of the held stretch `[offset, offset + len)`, block by
    /// block.
    pub(super) fn parts(&self, offset: u64, len: usize) -> impl Iterator<Item = &[u8]> {
        let end = offset.saturating_add(len as u64);
        let last = end.saturating_sub(1) / BLOCK_LEN;
        self.blocks
            .iter_from(offset / BLOCK_LEN)
            .take_while(move |&(key, _)| len > 0 && key <= last)
            .map(move |(key, block)| {
                let (from, to) = within(key, offset, end);
                &block.bytes[from..to]
            })
    }

    /// Let go of what is held of `[offset, offset + len)`, sent or lost: how
    /// many bytes were held.
    pub(super) fn let_go(&mut self, offset: u64, len: usize) -> usize {
        let Some(end) = offset.checked_add(len as u64).filter(|_| len > 0) else {
            return 0;
        };
        let mut gone = 0;
        for key in touched(offset, end) {
            let Some(block) = self.blocks.get_mut(key) else {
                continue;
            };
            let (from, to) = within(key, offset, end);
            gone += block.let_go(from, to);
            if block.is_empty() {
                self.blocks.remove(key);
            }
        }
        self.bytes -= gone;
        if self.bytes == 0 {
            *self = Self::default();
        }
        gone
    }

    /// Let go of everything held: how many bytes were.
    pub(super) fn let_all_go(&mut self) -> usize {
        core::mem::take(self).bytes
    }

    /// Take out the blocks of `[offset, end)` holding nothing.
    fn drop_empty(&mut self, offset: u64, end: u64) {
        for key in touched(offset, end) {
            if self.blocks.get(key).is_some_and(|block| block.is_empty()) {
                self.blocks.remove(key);
            }
        }
    }
}

#[cfg(test)]
#[path = "written_tests.rs"]
mod tests;
