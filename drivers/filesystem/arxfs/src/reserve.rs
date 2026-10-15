//! The reserve: free space ordinary allocation never takes, so a volume its
//! users have filled still takes the system's own writes and still lets a
//! delete free space (`docs/src/filesystem/arxfs-spec.md` §6).
//!
//! It is a set of free runs taken from the top of the volume, where metadata
//! lives, which the ordinary scans step over. Named runs rather than a count,
//! because metadata allocates in adjacent pairs and a count can be stranded as
//! single blocks by any pattern of frees.
//!
//! Once nothing else is free, three things may draw on it: a write the kernel
//! classes [`AllocationClass::System`], an operation that frees space —
//! admitted only when it frees at least what it still holds — and a commit's
//! own root. What a transaction takes from it goes back if the transaction is
//! abandoned, and a commit's frees top it up before ordinary allocation sees
//! them.

use core::ops::Range;

use tairix_abi::driver::filesystem::AllocationClass;
use tairix_abi::DriverError;
use tairix_collections::RangeSet;

use crate::allocator::Allocator;
use crate::btree::{internal_cap_of, leaf_cap_of};
use crate::superblock::RING_BLOCKS;
use crate::{data_capacity_of, extent_spec, Block, ARXFS};

/// What the running operation may take once ordinary space is gone.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub(crate) enum Draw {
    /// Nothing from the reserve.
    #[default]
    Ordinary,
    /// The whole reserve: the system's bounded write set, and a commit's root.
    System,
    /// The reserve, for an operation that frees at least what it holds.
    Release,
}

impl Draw {
    /// What an operation the kernel classed `class` draws.
    pub(crate) const fn of(class: AllocationClass) -> Self {
        match class {
            AllocationClass::Ordinary => Self::Ordinary,
            AllocationClass::System => Self::System,
        }
    }

    /// What the same operation draws when it frees space.
    pub(crate) const fn releasing(self) -> Self {
        match self {
            Self::System => Self::System,
            Self::Ordinary | Self::Release => Self::Release,
        }
    }
}

/// Bytes a transaction below the reserve allocates before it is published, so
/// a batched delete reuses its own frees rather than needing metadata for the
/// whole batch first.
pub(crate) const RELEASE_TXN_BYTES: u64 = 512 * 1024;

/// What a large system volume holds back: about a day and a half of audit at
/// one 1 KiB record a second, the time an operator has to act once ordinary
/// space is gone.
const RUNWAY_BYTES: u64 = 128 * 1024 * 1024;

/// Levels a costed write is allowed in each tree it descends: at half-full
/// 512-byte nodes, enough for a tree of 2^32 entries.
const PATH_LEVELS: u64 = 8;

/// Trees one file rewrite descends: the inode tree, its directory, the
/// pending-delete set, and its attributes.
const PATH_TREES: u64 = 4;

/// How far the system reserve's floor exceeds the write set it covers, so a
/// very large volume's deeper trees still fit.
const FLOOR_MARGIN: u64 = 4;

/// A metadata node's two mirrored copies.
const PAIR: u64 = 2;

/// The most one operation rewrites of the trees it descends.
pub(crate) const OPERATION_SLACK: u64 = PATH_TREES * PATH_LEVELS * PAIR;

/// `blocks`, rounded down to whole pairs.
const fn whole_pairs(blocks: u64) -> u64 {
    blocks & !1
}

/// Move `run` into `record`, except for the part `cancel` holds: that part
/// was the running operation's own opposite step, and the two cancel out.
fn shift(cancel: &mut RangeSet<u64>, record: &mut RangeSet<u64>, run: Range<u64>) {
    let mut pos = run.start;
    while pos < run.end {
        let Some(part) = cancel.first_overlap(pos..run.end) else {
            record.insert(pos..run.end);
            return;
        };
        if part.start > pos {
            record.insert(pos..part.start);
        }
        pos = part.end;
        cancel.remove(part);
    }
}

/// Blocks of `run` that `set` holds.
fn held_within(set: &RangeSet<u64>, run: Range<u64>) -> u64 {
    let mut held = 0;
    let mut pos = run.start;
    while let Some(part) = set.first_overlap(pos..run.end) {
        held += part.end - part.start;
        pos = part.end;
    }
    held
}

impl Allocator {
    /// Where `len` reserved blocks are taken: a data block from the low end
    /// and a metadata pair from the high end, as the ordinary allocator draws
    /// them, so a sequential write into the reserve still lands contiguous.
    fn reserve_draw_at(&self, len: u64) -> Option<u64> {
        if len == 1 {
            return self.reserve.iter().next().map(|held| held.start);
        }
        let mut below = u64::MAX;
        while let Some(held) = self.reserve.ending_at_or_below(below) {
            if held.end - held.start >= len {
                return Some(held.end - len);
            }
            below = held.start;
        }
        None
    }

    /// Blocks the reserve is short of its target.
    fn reserve_deficit(&self) -> u64 {
        self.reserve_target.saturating_sub(self.reserve.covered())
    }

    /// Hand back everything the open transaction took, as it is abandoned.
    pub(crate) fn abandon_reserve_draws(&mut self) {
        let taken = core::mem::take(&mut self.txn_reserve);
        for run in &taken {
            self.reserve.insert(run);
        }
        self.op_reserve_taken.clear();
        self.op_reserve_returned.clear();
    }

    /// Undo the running operation's draws and returns, as it is rolled back.
    pub(crate) fn undo_reserve_operation(&mut self) {
        let taken = core::mem::take(&mut self.op_reserve_taken);
        for run in &taken {
            self.txn_reserve.remove(run.clone());
            self.reserve.insert(run);
        }
        let returned = core::mem::take(&mut self.op_reserve_returned);
        for run in &returned {
            self.reserve.remove(run.clone());
            self.txn_reserve.insert(run);
        }
    }

    /// Whether a transaction below the reserve has allocated `threshold`
    /// blocks, and must be published so its frees can come back.
    pub(crate) fn reserve_commit_due(&self, free_count: u64, threshold: u64) -> bool {
        let below = free_count <= self.reserve.covered() || self.reserve_deficit() > 0;
        below && self.txn_private.covered() >= threshold
    }
}

/// The reserve every writable volume of `total_blocks` blocks of
/// `block_size` holds for operations that free space: one release
/// transaction, the operation that crosses it, and the commit that publishes
/// it, within an eighth of the volume.
#[must_use]
pub fn release_share_blocks(block_size: usize, total_blocks: u64) -> u64 {
    let wanted = units_of(block_size, RELEASE_TXN_BYTES)
        .saturating_add(OPERATION_SLACK)
        .saturating_add(PAIR);
    whole_pairs(wanted.min(total_blocks / 8))
}

/// The reserve a volume holding the system's writable grafts keeps, over
/// `write_set`, the largest size of each file the system rewrites on its own:
/// `min(volume / 8, max(floor, min(128 MiB, volume / 32)))`, the floor four
/// times the write set and the release share.
#[must_use]
pub fn system_reserve_blocks(block_size: usize, total_blocks: u64, write_set: &[u64]) -> u64 {
    let cost = write_set.iter().fold(0u64, |sum, &bytes| {
        sum.saturating_add(rewrite_cost(block_size, bytes))
    });
    system_reserve_over(block_size, total_blocks, cost)
}

fn system_reserve_over(block_size: usize, total_blocks: u64, cost: u64) -> u64 {
    let share = release_share_blocks(block_size, total_blocks);
    let floor = cost.saturating_add(share).saturating_mul(FLOOR_MARGIN);
    let runway = units_of(block_size, RUNWAY_BYTES).min(total_blocks / 32);
    whole_pairs(floor.max(runway).min(total_blocks / 8))
}

/// Blocks rewriting a file of `bytes` takes at worst: its data, an extent
/// tree at half fill over data fragmented a block to an extent, and a path
/// through every tree the rewrite descends, each node a mirrored pair.
fn rewrite_cost(block_size: usize, bytes: u64) -> u64 {
    let data = bytes.div_ceil(data_capacity_of(block_size));
    let leaf = (leaf_cap_of(block_size, extent_spec(0)) as u64 / 2).max(1);
    let fanout = (internal_cap_of(block_size) as u64 / 2).max(2);
    let mut nodes = data.div_ceil(leaf);
    let mut level = nodes;
    while level > 1 {
        level = level.div_ceil(fanout);
        nodes += level;
    }
    let pairs = nodes.saturating_add(PATH_TREES * PATH_LEVELS);
    data.saturating_add(pairs.saturating_mul(PAIR))
}

/// Allocation units `bytes` spans.
fn units_of(block_size: usize, bytes: u64) -> u64 {
    bytes.div_ceil(block_size as u64)
}

impl<B: Block> ARXFS<B> {
    /// Hold the system reserve on this volume, sized over `write_set`
    /// ([`system_reserve_blocks`]).
    ///
    /// For the volume that backs the system's writable grafts. Every writable
    /// volume already holds the release share, which this reserve includes.
    ///
    /// # Errors
    ///
    /// [`DriverError::PermissionDenied`] on a read-only handle, or the device's
    /// error while the reserve's runs are chosen.
    pub fn with_system_reserve(mut self, write_set: &[u64]) -> Result<Self, DriverError> {
        self.deny_if_read_only()?;
        let cost = write_set.iter().fold(0u64, |sum, &bytes| {
            sum.saturating_add(rewrite_cost(self.block_size, bytes))
        });
        self.system_write_cost = Some(cost);
        self.retarget_reserve()?;
        Ok(self)
    }

    /// The blocks the reserve is held at.
    #[must_use]
    pub fn reserve_blocks(&self) -> u64 {
        self.allocator().map_or(0, |alloc| alloc.reserve_target)
    }

    /// Blocks a transaction below the reserve allocates before it is
    /// published: the release share, less the operation that crosses it and
    /// the commit's own root.
    pub(crate) fn release_commit_blocks(&self) -> u64 {
        release_share_blocks(self.block_size, self.total_blocks)
            .saturating_sub(OPERATION_SLACK + PAIR)
            .max(PAIR)
    }

    /// The reserve's size for this volume as it now stands.
    fn reserve_target(&self) -> u64 {
        match self.system_write_cost {
            Some(cost) => system_reserve_over(self.block_size, self.total_blocks, cost),
            None => release_share_blocks(self.block_size, self.total_blocks),
        }
    }

    /// Size the reserve from the volume, fill it from the highest free runs,
    /// and tell the host if that leaves no space outside it.
    pub(crate) fn retarget_reserve(&mut self) -> Result<(), DriverError> {
        if self.read_only {
            return Ok(());
        }
        let target = self.reserve_target();
        self.allocator_mut()?.reserve_target = target;
        self.fill_reserve()?;
        self.report_reserve();
        Ok(())
    }

    /// Top the reserve up to its target from the highest free runs. A volume
    /// with less free space holds what there is: the reserve is then all that
    /// remains.
    fn fill_reserve(&mut self) -> Result<(), DriverError> {
        self.map_fold_pending()?;
        let floor = RING_BLOCKS + 1;
        let mut from = self.total_blocks.saturating_sub(1);
        loop {
            let deficit = self.allocator()?.reserve_deficit();
            if deficit == 0 {
                return Ok(());
            }
            let Some(top) = self.scan_down_outside_reserve(from, floor)? else {
                return Ok(());
            };
            let lowest = top.saturating_sub(deficit - 1).max(floor);
            let start = self
                .map_scan_used_down(top, lowest)?
                .map_or(lowest, |used| used + 1);
            self.allocator_mut()?.reserve.insert(start..top + 1);
            if start <= floor {
                return Ok(());
            }
            from = start - 1;
        }
    }

    /// Take `len` blocks — a data block, or a metadata pair — from the
    /// reserve, for an operation entitled to them.
    ///
    /// # Errors
    ///
    /// [`DriverError::NoSpace`] when the running operation may not draw on the
    /// reserve, or no reserve run holds `len` blocks.
    pub(crate) fn take_reserved(&mut self, len: u64) -> Result<u64, DriverError> {
        if self.draw == Draw::Ordinary {
            return Err(DriverError::NoSpace);
        }
        let start = self
            .allocator()?
            .reserve_draw_at(len)
            .ok_or(DriverError::NoSpace)?;
        let run = start..start + len;
        let alloc = self.allocator_mut()?;
        alloc.reserve.remove(run.clone());
        alloc.txn_reserve.insert(run.clone());
        shift(
            &mut alloc.op_reserve_returned,
            &mut alloc.op_reserve_taken,
            run,
        );
        self.claim_run(start, len);
        Ok(start)
    }

    /// Return to the reserve the part of `start..start + len` this transaction
    /// took from it, now free again.
    pub(crate) fn return_reserved(&mut self, start: u64, len: u64) {
        let Ok(alloc) = self.allocator_mut() else {
            return;
        };
        let end = start.saturating_add(len);
        let mut pos = start;
        while let Some(part) = alloc.txn_reserve.first_overlap(pos..end) {
            pos = part.end;
            alloc.txn_reserve.remove(part.clone());
            alloc.reserve.insert(part.clone());
            shift(
                &mut alloc.op_reserve_taken,
                &mut alloc.op_reserve_returned,
                part,
            );
        }
    }

    /// Top the reserve up from `run`, which has just become free.
    pub(crate) fn refill_reserve(&mut self, run: Range<u64>) {
        let Ok(alloc) = self.allocator_mut() else {
            return;
        };
        let wanted = alloc.reserve_deficit().min(run.end - run.start);
        if wanted > 0 {
            alloc.reserve.insert(run.end - wanted..run.end);
        }
    }

    /// Refuse an operation that freed space only in name: one that drew on the
    /// reserve as a release and frees less than it still holds.
    ///
    /// # Errors
    ///
    /// [`DriverError::NoSpace`] for such an operation, which the caller then
    /// rolls back.
    pub(crate) fn admit_release(&self) -> Result<(), DriverError> {
        if self.draw != Draw::Release {
            return Ok(());
        }
        let Ok(alloc) = self.allocator() else {
            return Ok(());
        };
        if alloc.op_reserve_taken.is_empty() {
            return Ok(());
        }
        let freed = alloc
            .op_deferred
            .covered()
            .saturating_add(alloc.op_released.covered());
        let held: u64 = alloc
            .op_claimed
            .iter()
            .map(|run| held_within(&alloc.txn_private, run))
            .sum();
        if freed >= held {
            Ok(())
        } else {
            Err(DriverError::NoSpace)
        }
    }

    /// Whether the open transaction must be published now so its frees come
    /// back: it runs below the reserve and has allocated a release
    /// transaction's worth.
    pub(crate) fn reserve_commit_due(&self) -> bool {
        let threshold = self.release_commit_blocks();
        self.allocator()
            .is_ok_and(|alloc| alloc.reserve_commit_due(self.free_count, threshold))
    }

    /// Whether the committed volume is down to its reserve: less than one
    /// operation's worth is left outside it, so the next ordinary write may
    /// be refused.
    pub(crate) fn in_reserve(&self) -> bool {
        self.allocator().is_ok_and(|alloc| {
            alloc.reserve_target > 0
                && self.committed_free_count() < alloc.reserve_target + OPERATION_SLACK
        })
    }

    /// Tell the host when the volume's space outside the reserve runs out or
    /// comes back.
    pub(crate) fn report_reserve(&mut self) {
        let in_reserve = self.in_reserve();
        if in_reserve == self.reported_in_reserve {
            return;
        }
        self.reported_in_reserve = in_reserve;
        if let Some((volume, host)) = self.schedule.host() {
            host.reserve_crossed(volume, in_reserve);
        }
    }
}
