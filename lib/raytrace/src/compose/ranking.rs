//! A ranking of places, largest first, sorted a run a core and merged a place
//! at a time as they are taken: how a stream's bed thins its stones, each
//! laid where none larger already lies.

use alloc::collections::BinaryHeap;
use alloc::vec::Vec;

use tairix_parallel::JobRunner;
use tairix_util::fallible;

use crate::vector::single;

/// Places in a run of a ranking one core sorts: a unit of the ranking is a
/// run a core.
const RUN: usize = 1 << 14;

/// Items kept in runs of `RUN` that never move as more are added, so a band's
/// worth more costs only its own room: no copy of what came before, and no
/// room held past the last run's.
#[derive(Debug)]
pub(super) struct Runs<T> {
    runs: Vec<Vec<T>>,
    len: usize,
}

impl<T> Default for Runs<T> {
    fn default() -> Self {
        Self {
            runs: Vec::new(),
            len: 0,
        }
    }
}

impl<T> Runs<T> {
    pub(super) fn len(&self) -> usize {
        self.len
    }

    /// Room for `count` more; `false` when the heap will not hold it.
    pub(super) fn reserve(&mut self, count: usize) -> bool {
        let room = self.runs.len() * RUN;
        let wanted = (self.len + count).saturating_sub(room).div_ceil(RUN);
        if self.runs.try_reserve(wanted).is_err() {
            return false;
        }
        for _ in 0..wanted {
            let mut run = Vec::new();
            if run.try_reserve_exact(RUN).is_err() {
                return false;
            }
            self.runs.push(run);
        }
        true
    }

    /// Add `item`, within the room reserved.
    pub(super) fn push(&mut self, item: T) {
        if let Some(run) = self.runs.get_mut(self.len / RUN) {
            run.push(item);
            self.len += 1;
        }
    }

    pub(super) fn get(&self, index: usize) -> Option<&T> {
        self.runs.get(index / RUN)?.get(index % RUN)
    }
}

/// Places ranked largest first, ties in the order they were added: each run
/// sorted by a core once all are added, then merged a place at a time as they
/// are taken, so neither the sort nor the merge is ever one long unit.
#[derive(Debug, Default)]
pub(super) struct Ranking {
    /// Each place's size, as the grids hold one, and its index.
    order: Runs<(f32, u32)>,
    /// How many runs are sorted.
    sorted: usize,
    /// The next place of each run not yet taken.
    next: Vec<usize>,
    /// The largest place each run has left, largest of all on top.
    heads: BinaryHeap<Head>,
}

/// The largest place a run of a ranking has left.
#[derive(Copy, Clone, Debug)]
struct Head {
    size: f32,
    index: u32,
    run: u32,
}

impl Ord for Head {
    /// Larger first, and of two alike, the one added first.
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.size
            .total_cmp(&other.size)
            .then(other.index.cmp(&self.index))
    }
}

impl PartialOrd for Head {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Head {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other).is_eq()
    }
}

impl Eq for Head {}

impl Ranking {
    /// Room for `count` more places; `false` when the heap will not hold it.
    pub(super) fn reserve(&mut self, count: usize) -> bool {
        self.order.reserve(count)
    }

    /// Add place `index` of size `size`, within the room reserved.
    pub(super) fn add(&mut self, size: f64, index: u32) {
        self.order.push((single(size), index));
    }

    /// Whether every run is sorted and laid out to be merged.
    pub(super) fn ranked(&self) -> bool {
        let runs = self.order.runs.len();
        self.sorted >= runs && self.next.len() == runs
    }

    /// Sort the next runs, one a core across `runner`, and ready them to be
    /// merged once all are; `None` when the heap will not hold the merge.
    pub(super) fn rank(&mut self, runner: &dyn JobRunner) -> Option<()> {
        let count = self.order.runs.len();
        let end = (self.sorted + runner.width().max(1)).min(count);
        if let Some(runs) = self.order.runs.get_mut(self.sorted..end) {
            tairix_parallel::for_each(runner, runs, &|run| {
                run.sort_unstable_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
            });
        }
        self.sorted = end;
        if self.sorted < count {
            return Some(());
        }
        if !fallible::reserve(&mut self.next, count) || self.heads.try_reserve_exact(count).is_err()
        {
            return None;
        }
        for run in 0..count {
            self.next.push(0);
            self.push_head(run)?;
        }
        Some(())
    }

    /// The largest place not yet taken; `None` once all are.
    pub(super) fn next(&mut self) -> Option<u32> {
        let head = self.heads.pop()?;
        self.push_head(head.run as usize)?;
        Some(head.index)
    }

    /// Whether every place has been taken.
    pub(super) fn exhausted(&self) -> bool {
        self.heads.is_empty()
    }

    /// Move run `run` on to its next place, if it has one left, and offer
    /// that place to the merge; `None` only for a run that is not there.
    fn push_head(&mut self, run: usize) -> Option<()> {
        let next = self.next.get_mut(run)?;
        if let Some(&(size, index)) = self.order.runs.get(run)?.get(*next) {
            *next += 1;
            self.heads.push(Head {
                size,
                index,
                run: u32::try_from(run).ok()?,
            });
        }
        Some(())
    }
}

#[cfg(test)]
#[path = "ranking_tests.rs"]
mod tests;
