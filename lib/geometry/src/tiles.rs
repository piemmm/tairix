//! Which tiles of a picture a change touches, and the cover of them a
//! compositor is told to repaint.
//!
//! A change is marked in tiles of one side, a bit each, at a cost per tile
//! however many changes there are. Its cover lists the marked tiles as runs
//! along each row, the same run in the rows beneath taken as one rectangle, the
//! tiles doubled until the rectangles fit a budget: a compositor merges each
//! rectangle against the rest, so a scattered change's own rectangles would
//! cost more than the whole tiles around them.

use alloc::vec::Vec;
use core::ops::Range;

use crate::{Rect, Region};

/// Bits in a word of tiles.
const WORD: usize = u64::BITS as usize;

/// The tiles of a picture, each marked or not.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Tiles {
    size: (u32, u32),
    grid: Grid,
    /// Row-major: bit `c % 64` of word `c / 64` of a row marks column `c`. No
    /// bit past the last column is ever set.
    bits: Vec<u64>,
}

/// The shape of a grid of tiles over a picture.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Grid {
    side: u32,
    across: usize,
    down: usize,
    /// Words a row of tiles takes.
    stride: usize,
}

impl Tiles {
    /// None of a `size` picture's tiles, `tile` pixels a side, marked; `None`
    /// for a tile of nought, or when the heap will not hold them.
    #[must_use]
    pub fn new(size: (u32, u32), tile: u32) -> Option<Self> {
        if tile == 0 {
            return None;
        }
        let across = usize::try_from(size.0.div_ceil(tile)).ok()?;
        let down = usize::try_from(size.1.div_ceil(tile)).ok()?;
        let stride = across.div_ceil(WORD);
        let len = stride.checked_mul(down)?;
        let mut bits = Vec::new();
        bits.try_reserve_exact(len).ok()?;
        bits.resize(len, 0);
        Some(Self {
            size,
            grid: Grid {
                side: tile,
                across,
                down,
                stride,
            },
            bits,
        })
    }

    /// The side of a tile, in pixels.
    #[must_use]
    pub const fn side(&self) -> u32 {
        self.grid.side
    }

    /// How many rows of tiles the picture holds.
    #[must_use]
    pub const fn rows(&self) -> usize {
        self.grid.down
    }

    /// How many tiles are marked.
    #[must_use]
    pub fn count(&self) -> usize {
        self.bits
            .iter()
            .map(|word| word.count_ones() as usize)
            .sum()
    }

    /// Whether no tile is marked.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bits.iter().all(|word| *word == 0)
    }

    /// Unmark every tile.
    pub fn clear(&mut self) {
        self.bits.fill(0);
    }

    /// Mark the tiles the part of `rect` within the picture touches.
    pub fn mark(&mut self, rect: Rect) {
        let rect = rect.intersection(&Rect::new(0, 0, self.size.0, self.size.1));
        if rect.is_empty() {
            return;
        }
        let side = usize::try_from(self.grid.side).unwrap_or(usize::MAX);
        let tile = |edge: i32| usize::try_from(edge).unwrap_or(0) / side;
        let columns = tile(rect.left())..tile(rect.right() - 1) + 1;
        for row in tile(rect.top())..=tile(rect.bottom() - 1) {
            if let Some(line) = self.grid.row_mut(&mut self.bits, row) {
                set_run(line, &columns);
            }
        }
    }

    /// Mark every tile `other`, of the same picture, marks as well.
    pub fn include(&mut self, other: &Self) {
        if self.grid == other.grid {
            for (mine, theirs) in self.bits.iter_mut().zip(&other.bits) {
                *mine |= *theirs;
            }
        }
    }

    /// Mark the tiles `other`, of the same picture, marks, and no others.
    pub fn copy_from(&mut self, other: &Self) {
        if self.grid == other.grid {
            self.bits.copy_from_slice(&other.bits);
        }
    }

    /// The pixels of the marked tiles in tile rows `rows`, a run of them at a
    /// time: its columns and its tile row's lines, within the picture.
    pub fn spans(&self, rows: Range<usize>) -> impl Iterator<Item = (Range<u32>, Range<u32>)> + '_ {
        let (width, height) = self.size;
        let side = self.grid.side;
        let rows = rows.start.min(self.grid.down)..rows.end.min(self.grid.down);
        rows.flat_map(move |row| {
            let line = self.grid.row(&self.bits, row).unwrap_or_default();
            runs(line).map(move |columns| {
                (
                    edge(columns.start, side, width)..edge(columns.end, side, width),
                    edge(row, side, height)..edge(row + 1, side, height),
                )
            })
        })
    }

    /// Add a cover of the marked tiles to `damage`, worked out in `room`, the
    /// same picture's tiles: whole tiles in the band form a region holds them
    /// in — each tile row's runs, joined with the rows beneath that mark the
    /// same tiles — the tiles' side doubled until the rectangles number no
    /// more than `budget`. The marks themselves are left as they were.
    ///
    /// A `room` of another picture is no room to work in, and the whole
    /// picture is covered instead, which over-covers but loses nothing.
    pub fn cover(&self, budget: usize, room: &mut Self, damage: &mut Region) {
        if room.grid != self.grid || room.size != self.size {
            if !self.is_empty() {
                damage.add(Rect::new(0, 0, self.size.0, self.size.1));
            }
            return;
        }
        room.bits.copy_from_slice(&self.bits);
        let mut grid = self.grid;
        while grid.rects(&room.bits).nth(budget.max(1)).is_some()
            && (grid.across > 1 || grid.down > 1)
        {
            grid = grid.coarsen(&mut room.bits);
        }
        let (width, height) = self.size;
        for (columns, rows) in grid.rects(&room.bits) {
            let (left, top) = (
                edge(columns.start, grid.side, width),
                edge(rows.start, grid.side, height),
            );
            let (Ok(x), Ok(y)) = (i32::try_from(left), i32::try_from(top)) else {
                continue;
            };
            damage.add(Rect::new(
                x,
                y,
                edge(columns.end, grid.side, width) - left,
                edge(rows.end, grid.side, height) - top,
            ));
        }
    }
}

impl Grid {
    /// Row `row`'s words of `bits`.
    fn row<'a>(&self, bits: &'a [u64], row: usize) -> Option<&'a [u64]> {
        let start = row.checked_mul(self.stride)?;
        bits.get(start..start.checked_add(self.stride)?)
    }

    /// Row `row`'s words of `bits`, to mark.
    fn row_mut<'a>(&self, bits: &'a mut [u64], row: usize) -> Option<&'a mut [u64]> {
        let start = row.checked_mul(self.stride)?;
        bits.get_mut(start..start.checked_add(self.stride)?)
    }

    /// The rectangles of tiles `bits` marks, as their columns and rows, band
    /// by band: each row's runs, joined with the rows beneath marking the same
    /// tiles, so a region holds exactly as many as are counted here.
    fn rects<'a>(
        &'a self,
        bits: &'a [u64],
    ) -> impl Iterator<Item = (Range<usize>, Range<usize>)> + 'a {
        let line = move |row: usize| self.row(bits, row).unwrap_or_default();
        (0..self.down)
            .filter(move |row| *row == 0 || line(*row) != line(*row - 1))
            .flat_map(move |top| {
                let end = (top + 1..self.down)
                    .find(|beneath| line(*beneath) != line(top))
                    .unwrap_or(self.down);
                runs(line(top)).map(move |columns| (columns, top..end))
            })
    }

    /// Merge each two by two tiles of `bits` into one, in place, answering
    /// the coarser grid. A merged word's index is never past the words it
    /// reads, so no word is overwritten before it is read.
    fn coarsen(&self, bits: &mut [u64]) -> Self {
        let across = self.across.div_ceil(2);
        let coarse = Self {
            side: self.side.saturating_mul(2),
            across,
            down: self.down.div_ceil(2),
            stride: across.div_ceil(WORD),
        };
        let word = |bits: &[u64], row: usize, index: usize| {
            if row >= self.down || index >= self.stride {
                return 0;
            }
            bits.get(row * self.stride + index).copied().unwrap_or(0)
        };
        for row in 0..coarse.down {
            for index in 0..coarse.stride {
                let pair = |half: usize| {
                    let at = 2 * index + half;
                    halve(word(bits, 2 * row, at) | word(bits, 2 * row + 1, at))
                };
                let merged = pair(0) | (pair(1) << 32);
                if let Some(slot) = bits.get_mut(row * coarse.stride + index) {
                    *slot = merged;
                }
            }
        }
        coarse
    }
}

/// Tile `at`'s edge in pixels along an axis of `end` pixels.
fn edge(at: usize, side: u32, end: u32) -> u32 {
    u32::try_from(at).map_or(end, |at| at.saturating_mul(side).min(end))
}

/// Bits `low..=high` of a word.
const fn span(low: usize, high: usize) -> u64 {
    (u64::MAX >> (WORD - 1 - high)) & (u64::MAX << low)
}

/// Mark `columns` of a row's words.
fn set_run(line: &mut [u64], columns: &Range<usize>) {
    let Some(last) = columns.end.checked_sub(1) else {
        return;
    };
    let (first_word, last_word) = (columns.start / WORD, last / WORD);
    for (index, word) in line
        .iter_mut()
        .enumerate()
        .take(last_word + 1)
        .skip(first_word)
    {
        let low = if index == first_word {
            columns.start % WORD
        } else {
            0
        };
        let high = if index == last_word {
            last % WORD
        } else {
            WORD - 1
        };
        *word |= span(low, high);
    }
}

/// The runs of marked columns in a row's words.
fn runs(line: &[u64]) -> impl Iterator<Item = Range<usize>> + '_ {
    let mut at = 0;
    core::iter::from_fn(move || {
        let start = next(line, at, true)?;
        let end = next(line, start, false).unwrap_or(line.len() * WORD);
        at = end;
        Some(start..end)
    })
}

/// The first column from `from` on that is marked, or that is not.
fn next(line: &[u64], from: usize, marked: bool) -> Option<usize> {
    let read = |index: usize| {
        line.get(index)
            .map(|word| if marked { *word } else { !*word })
    };
    let mut index = from / WORD;
    let mut word = read(index)? & (u64::MAX << (from % WORD));
    loop {
        if word != 0 {
            return Some(index * WORD + word.trailing_zeros() as usize);
        }
        index += 1;
        word = read(index)?;
    }
}

/// A word's 64 tiles merged in pairs into the low 32 bits.
const fn halve(word: u64) -> u64 {
    let mut merged = (word | (word >> 1)) & 0x5555_5555_5555_5555;
    merged = (merged | (merged >> 1)) & 0x3333_3333_3333_3333;
    merged = (merged | (merged >> 2)) & 0x0F0F_0F0F_0F0F_0F0F;
    merged = (merged | (merged >> 4)) & 0x00FF_00FF_00FF_00FF;
    merged = (merged | (merged >> 8)) & 0x0000_FFFF_0000_FFFF;
    (merged | (merged >> 16)) & 0x0000_0000_FFFF_FFFF
}

#[cfg(test)]
#[path = "tiles_tests.rs"]
mod tests;
