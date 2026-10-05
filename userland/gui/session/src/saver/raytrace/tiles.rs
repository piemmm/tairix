//! The tiles of a picture a change touches, and the cover of them a compositor
//! is told to repaint.
//!
//! A change is marked in tiles of the finest size. Its cover lists them as
//! runs along each row, the tiles doubled until the runs fit a budget: a
//! compositor merges each rectangle against the rest, so a scattered change's
//! own rectangles would cost more than whole tiles.

use alloc::vec::Vec;
use core::ops::Range;

use tairix_util::fallible;
use tairix_wm::{Rect, Region};

/// The side of the finest tile, in pixels.
pub(super) const TILE: u32 = 16;

/// The most rectangles a cover lists; past this the tiles double.
pub(super) const COVER_BUDGET: usize = 128;

/// The finest tiles of a picture, each marked or not.
pub(super) struct Tiles {
    size: (u32, u32),
    across: usize,
    marked: Vec<bool>,
}

impl Tiles {
    /// None of a `size` picture's tiles marked; `None` when the heap will not
    /// hold them.
    pub(super) fn new((width, height): (u32, u32)) -> Option<Self> {
        let across = width.div_ceil(TILE) as usize;
        let down = height.div_ceil(TILE) as usize;
        Some(Self {
            size: (width, height),
            across,
            marked: fallible::filled(across.checked_mul(down)?, false)?,
        })
    }

    /// How many rows of tiles the picture holds.
    pub(super) fn rows(&self) -> usize {
        self.marked.len() / self.across.max(1)
    }

    /// How many tiles are marked.
    pub(super) fn count(&self) -> usize {
        self.marked.iter().filter(|marked| **marked).count()
    }

    /// Mark the tiles the part of `rect` within the picture touches.
    pub(super) fn mark(&mut self, rect: Rect) {
        let rect = rect.intersection(&Rect::new(0, 0, self.size.0, self.size.1));
        if rect.is_empty() {
            return;
        }
        let tile = |edge: i32| (u32::try_from(edge).unwrap_or(0) / TILE) as usize;
        let (left, right) = (tile(rect.left()), tile(rect.right() - 1));
        for row in tile(rect.top())..=tile(rect.bottom() - 1) {
            let start = row * self.across;
            if let Some(run) = self.marked.get_mut(start + left..=start + right) {
                run.fill(true);
            }
        }
    }

    /// Mark the tiles `other`, of the same picture, marks, and no others.
    pub(super) fn copy_from(&mut self, other: &Self) {
        for (mine, theirs) in self.marked.iter_mut().zip(&other.marked) {
            *mine = *theirs;
        }
    }

    /// Unmark every tile.
    pub(super) fn clear(&mut self) {
        self.marked.fill(false);
    }

    /// The pixels of the marked tiles in tile rows `rows`, a run of them at a
    /// time: its columns and its tile row's lines, within the picture.
    pub(super) fn spans(
        &self,
        rows: Range<usize>,
    ) -> impl Iterator<Item = (Range<u32>, Range<u32>)> + '_ {
        let (width, height) = self.size;
        let edge = |at: usize, end: u32| {
            u32::try_from(at).map_or(end, |at| at.saturating_mul(TILE).min(end))
        };
        runs(&self.marked, self.across, rows).map(move |(row, columns)| {
            (
                edge(columns.start, width)..edge(columns.end, width),
                edge(row, height)..edge(row + 1, height),
            )
        })
    }

    /// Add a cover of the marked tiles to `damage`, worked out in `room`, the
    /// same picture's tiles: whole tiles, their side doubled until their runs
    /// fit the budget. The marks themselves are left as they were.
    pub(super) fn cover(&self, room: &mut Self, damage: &mut Region) {
        room.copy_from(self);
        let (width, height) = self.size;
        let mut tile = TILE;
        let (mut across, mut down) = (self.across, self.rows());
        while runs(&room.marked, across, 0..down).count() > COVER_BUDGET && (across > 1 || down > 1)
        {
            coarsen(&mut room.marked, across, down);
            (across, down) = (across.div_ceil(2), down.div_ceil(2));
            tile = tile.saturating_mul(2);
        }
        let edge = |at: usize| u32::try_from(at).unwrap_or(u32::MAX).saturating_mul(tile);
        for (row, columns) in runs(&room.marked, across, 0..down) {
            let (left, top) = (edge(columns.start), edge(row));
            damage.add(Rect::new(
                i32::try_from(left).unwrap_or(i32::MAX),
                i32::try_from(top).unwrap_or(i32::MAX),
                edge(columns.end).min(width).saturating_sub(left),
                top.saturating_add(tile).min(height).saturating_sub(top),
            ));
        }
    }
}

/// The runs of marked tiles in rows `rows` of an `across`-wide grid of them,
/// row by row, each as its row and its columns.
fn runs(
    tiles: &[bool],
    across: usize,
    rows: Range<usize>,
) -> impl Iterator<Item = (usize, Range<usize>)> + '_ {
    rows.flat_map(move |row| {
        let line = tiles.get(row * across..(row + 1) * across).unwrap_or(&[]);
        let mut at = 0;
        core::iter::from_fn(move || {
            let first = at + line.get(at..)?.iter().position(|marked| *marked)?;
            let end = line
                .get(first..)
                .and_then(|rest| rest.iter().position(|marked| !*marked))
                .map_or(line.len(), |length| first + length);
            at = end;
            Some((row, first..end))
        })
    })
}

/// Merge each two by two of an `across` by `down` grid of tiles into one, in
/// place: a merged tile's index is never past those it reads.
fn coarsen(tiles: &mut [bool], across: usize, down: usize) {
    let half = across.div_ceil(2);
    for row in 0..down.div_ceil(2) {
        for column in 0..half {
            let marked = (2 * row..(2 * row + 2).min(down)).any(|y| {
                (2 * column..(2 * column + 2).min(across))
                    .any(|x| tiles.get(y * across + x).copied().unwrap_or(false))
            });
            if let Some(tile) = tiles.get_mut(row * half + column) {
                *tile = marked;
            }
        }
    }
}

#[cfg(test)]
#[path = "tiles_tests.rs"]
mod tests;
