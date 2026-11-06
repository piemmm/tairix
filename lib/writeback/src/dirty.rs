//! The transaction's dirty block set, ordered around the commit barrier.
//!
//! A filesystem rewrites the same metadata block many times inside one
//! transaction and only the last version survives, so the set holds one
//! entry per block keyed by address: the device is issued one write per block
//! per transaction rather than one per rewrite, and the drain hands it
//! ascending *runs* rather than blocks.
//!
//! A transaction spans operations, so a failed operation must undo itself
//! without disturbing the ones already reported successful: each block the
//! running operation changes is remembered as it stood before, once, in a
//! savepoint that undoing restores.
//!
//! A change the heap refuses leaves the set as it was, and each change keeps
//! room for what undoing it would put back, so the undo asks the heap for
//! nothing.

use alloc::vec::Vec;
use core::mem;
use core::num::NonZeroUsize;

use tairix_abi::DriverError;
use tairix_collections::{OrderedMap, TryReserveError};
use tairix_reclaim::MAP_ENTRY_OVERHEAD;
use zeroize::Zeroize;

use crate::RunWindow;

/// One staged block's bytes, wiped as they are dropped: drained, replaced,
/// discarded, undone or refused room, a block leaves the set through here.
struct Staged(Vec<u8>);

impl Drop for Staged {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// One block's staged state before the running operation first changed it.
struct Prior {
    phase: WritePhase,
    bytes: Staged,
}

/// Which side of the commit barrier may issue a staged block.
#[derive(Copy, Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum WritePhase {
    /// Authoritative blocks that must be durable before publication.
    BeforeBarrier,
    /// Rebuildable blocks protected by a durable invalidation marker. Their
    /// staging is not undone: dropping the only copy would lose state the
    /// owner no longer holds elsewhere.
    AfterBarrier,
}

impl WritePhase {
    const ALL: [Self; 2] = [Self::BeforeBarrier, Self::AfterBarrier];

    const fn other(self) -> Self {
        match self {
            Self::BeforeBarrier => Self::AfterBarrier,
            Self::AfterBarrier => Self::BeforeBarrier,
        }
    }
}

/// Blocks waiting for their ordered device drain.
pub struct DirtySet {
    entries: OrderedMap<(WritePhase, u64), Staged>,
    /// Each block the running operation has changed, as it stood before —
    /// `None` where the set held nothing. Absent between operations.
    savepoint: Option<OrderedMap<u64, Option<Prior>>>,
    block_size: usize,
    /// Buffers the savepoint holds; the staged half is the map's length.
    saved_priors: usize,
    /// Blocks the running operation took out of `entries` that its record
    /// holds as they stood: what undoing it puts back, so `entries` keeps room
    /// for that many. Never more than `saved_priors`.
    undo_inserts: usize,
}

/// `block` in a buffer of its own.
fn block_copy(block: &[u8]) -> Result<Staged, DriverError> {
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(block.len())
        .map_err(|_| DriverError::OutOfMemory)?;
    bytes.extend_from_slice(block);
    Ok(Staged(bytes))
}

/// The answer to a map refused the room a change needs.
fn out_of_memory(_: TryReserveError) -> DriverError {
    DriverError::OutOfMemory
}

impl DirtySet {
    /// An empty set over blocks of `block_size`. Allocates nothing until the
    /// first block is staged.
    ///
    /// A size of zero, which every later division would trip over, is not a
    /// value the type admits:
    ///
    /// ```compile_fail,E0308
    /// let _ = tairix_writeback::DirtySet::new(0usize);
    /// ```
    #[must_use]
    pub const fn new(block_size: NonZeroUsize) -> Self {
        Self {
            entries: OrderedMap::new(),
            savepoint: None,
            block_size: block_size.get(),
            saved_priors: 0,
            undo_inserts: 0,
        }
    }

    /// Block buffers the set pins: the staged ones plus the savepoint's.
    #[must_use]
    pub fn held(&self) -> usize {
        self.entries.len().saturating_add(self.saved_priors)
    }

    /// Bytes the set pins: each held buffer plus its map bookkeeping.
    #[must_use]
    pub fn pinned_bytes(&self) -> usize {
        self.held()
            .saturating_mul(self.block_size.saturating_add(MAP_ENTRY_OVERHEAD))
    }

    /// Start recording an operation's changes so they can be undone alone.
    pub fn begin_operation(&mut self) {
        self.drop_savepoint();
        self.savepoint = Some(OrderedMap::new());
    }

    /// The operation succeeded: its changes are the transaction's now.
    pub fn end_operation(&mut self) {
        self.drop_savepoint();
    }

    /// Undo the running operation's changes, leaving every block the rest of
    /// the transaction staged exactly as it was. It asks the heap for
    /// nothing: every change kept room for what this puts back.
    ///
    /// # Errors
    ///
    /// [`TryReserveError`] if a block could not be put back after all, which
    /// only a miscounted room can cause; the set then no longer holds what
    /// the transaction staged, and its owner must not publish it.
    pub fn undo_operation(&mut self) -> Result<(), TryReserveError> {
        let Some(savepoint) = self.savepoint.take() else {
            return Ok(());
        };
        self.saved_priors = 0;
        self.undo_inserts = 0;
        let mut undone = Ok(());
        for (phys, prior) in savepoint {
            let Some(Prior { phase, bytes }) = prior else {
                for phase in WritePhase::ALL {
                    self.entries.remove(&(phase, phys));
                }
                continue;
            };
            self.entries.remove(&(phase.other(), phys));
            if let Some(held) = self.entries.get_mut(&(phase, phys)) {
                *held = bytes;
            } else if let Err(refused) = self.entries.try_insert((phase, phys), bytes) {
                undone = Err(refused);
            }
        }
        undone
    }

    fn drop_savepoint(&mut self) {
        self.savepoint = None;
        self.saved_priors = 0;
        self.undo_inserts = 0;
    }

    /// Whether the running operation has yet to record `phys` as it stood.
    fn saves_first(&self, phys: u64) -> bool {
        self.savepoint
            .as_ref()
            .is_some_and(|savepoint| !savepoint.contains_key(&phys))
    }

    /// The phase `phys` is staged for, if the set holds it.
    fn phase_of(&self, phys: u64) -> Option<WritePhase> {
        WritePhase::ALL
            .into_iter()
            .find(|&phase| self.entries.contains_key(&(phase, phys)))
    }

    /// Make room for `inserts` new entries and `records` savepoint records,
    /// and for a change taking `removes` keys out — each of which undoing the
    /// running operation would put back — over the room it is already owed.
    /// Only a key taken out costs the undo room: one replaced in place is put
    /// back in place.
    fn make_room(
        &mut self,
        inserts: usize,
        records: usize,
        removes: usize,
    ) -> Result<(), DriverError> {
        let mut owed = 0;
        if let Some(savepoint) = self.savepoint.as_mut() {
            savepoint.try_reserve(records).map_err(out_of_memory)?;
            owed = self.undo_inserts.saturating_add(removes);
        }
        self.entries
            .try_reserve(inserts.saturating_add(owed))
            .map_err(out_of_memory)
    }

    /// Account for `(phase, phys)` having been taken out of `entries`: a block
    /// the running operation's record holds as it stood is one an undo puts
    /// back, and a record of a block the set did not hold is let go once no
    /// key for it is left, as the set is then as the operation found it.
    fn took_out(&mut self, phase: WritePhase, phys: u64) {
        let absent = self.phase_of(phys).is_none();
        let Some(savepoint) = self.savepoint.as_mut() else {
            return;
        };
        match savepoint.get(&phys) {
            Some(Some(prior)) if prior.phase == phase => {
                self.undo_inserts = self.undo_inserts.saturating_add(1).min(self.saved_priors);
            }
            Some(None) if absent => {
                savepoint.remove(&phys);
            }
            _ => {}
        }
    }

    /// Record `prior` — `phys` as it stood, absent when the set did not hold
    /// it — as the running operation's first sight of `phys`.
    fn record(&mut self, phys: u64, prior: Option<Prior>) -> Result<(), DriverError> {
        let Some(savepoint) = self.savepoint.as_mut() else {
            return Ok(());
        };
        let kept = prior.is_some();
        savepoint.try_insert(phys, prior).map_err(out_of_memory)?;
        if kept {
            self.saved_priors = self.saved_priors.saturating_add(1);
        }
        Ok(())
    }

    /// Put `prior` in the record opened for `phys`, once the change it
    /// records has been made.
    fn fill_record(&mut self, phys: u64, prior: Option<Prior>) {
        let Some(slot) = self
            .savepoint
            .as_mut()
            .and_then(|savepoint| savepoint.get_mut(&phys))
        else {
            return;
        };
        if prior.is_some() {
            self.saved_priors = self.saved_priors.saturating_add(1);
        }
        *slot = prior;
    }

    /// Whether staging `phys` for `phase` must be undoable.
    fn undoable(&self, phase: WritePhase, phys: u64) -> bool {
        phase == WritePhase::BeforeBarrier
            || self
                .entries
                .contains_key(&(WritePhase::BeforeBarrier, phys))
    }

    /// Put `bytes` at `phys` for `phase` in place of whatever the set holds
    /// there, recording what it replaces when `saves`.
    ///
    /// The record goes in first and a new key before the one it replaces
    /// comes out, so a refusal at any step leaves the set as it was.
    fn put(
        &mut self,
        phase: WritePhase,
        phys: u64,
        bytes: Staged,
        saves: bool,
    ) -> Result<(), DriverError> {
        let held = self.phase_of(phys);
        self.make_room(
            usize::from(held != Some(phase)),
            usize::from(saves),
            usize::from(held == Some(phase.other())),
        )?;
        if saves {
            self.record(phys, None)?;
        }
        let replaced = if let Some(slot) = self.entries.get_mut(&(phase, phys)) {
            Some(Prior {
                phase,
                bytes: mem::replace(slot, bytes),
            })
        } else {
            if let Err(refused) = self.entries.try_insert((phase, phys), bytes) {
                if saves {
                    if let Some(savepoint) = self.savepoint.as_mut() {
                        savepoint.remove(&phys);
                    }
                }
                return Err(out_of_memory(refused));
            }
            let other = phase.other();
            self.entries.remove(&(other, phys)).map(|bytes| Prior {
                phase: other,
                bytes,
            })
        };
        let moved = replaced.as_ref().is_some_and(|prior| prior.phase != phase);
        if saves {
            self.fill_record(phys, replaced);
        }
        if moved {
            self.took_out(phase.other(), phys);
        }
        Ok(())
    }

    /// File the block `phys` holds for the other phase under `phase`, its
    /// buffer moved rather than copied: [`Self::put`] for a change whose
    /// savepoint keeps nothing of the version it replaces.
    fn move_across(&mut self, phase: WritePhase, phys: u64) -> Result<(), DriverError> {
        let other = phase.other();
        self.make_room(1, 0, 1)?;
        // An empty buffer takes the key first, so a refusal leaves the block
        // where it was.
        self.entries
            .try_insert((phase, phys), Staged(Vec::new()))
            .map_err(out_of_memory)?;
        let moved = self
            .entries
            .get_mut(&(other, phys))
            .map(|held| mem::replace(held, Staged(Vec::new())));
        if let (Some(moved), Some(slot)) = (moved, self.entries.get_mut(&(phase, phys))) {
            *slot = moved;
        }
        self.entries.remove(&(other, phys));
        self.took_out(other, phys);
        Ok(())
    }

    /// Stage `bytes` at `phys` for `phase`, replacing any held version.
    ///
    /// # Errors
    ///
    /// [`DriverError::BufferTooSmall`] when `bytes` is shorter than a block,
    /// [`DriverError::OutOfMemory`] when the change cannot be had; the set is
    /// then as it was.
    pub fn stage(&mut self, phase: WritePhase, phys: u64, bytes: &[u8]) -> Result<(), DriverError> {
        let block = bytes
            .get(..self.block_size)
            .ok_or(DriverError::BufferTooSmall)?;
        let saves = self.undoable(phase, phys) && self.saves_first(phys);
        if !saves {
            if let Some(held) = self.entries.get_mut(&(phase, phys)) {
                held.0.copy_from_slice(block);
                return Ok(());
            }
        }
        // Replaced whole: a version the savepoint keeps moves there rather
        // than being copied first.
        self.put(phase, phys, block_copy(block)?, saves)
    }

    /// The block staged at `phys` for `phase`, made writable in place.
    ///
    /// A block the set does not hold yet is first filled by `load` — the
    /// caller's read of what the device has there — so an in-place edit
    /// starts from the current contents, and one it holds for the other phase
    /// moves across, copied only where the savepoint keeps it as it was. The
    /// savepoint is taken exactly as [`Self::stage`] takes it.
    ///
    /// # Errors
    ///
    /// Whatever `load` reports, or [`DriverError::OutOfMemory`] when the
    /// change cannot be had; either stages nothing.
    pub fn stage_with<F>(
        &mut self,
        phase: WritePhase,
        phys: u64,
        load: F,
    ) -> Result<&mut [u8], DriverError>
    where
        F: FnOnce(&mut [u8]) -> Result<(), DriverError>,
    {
        let saves = self.undoable(phase, phys) && self.saves_first(phys);
        if let Some(held) = self.entries.get(&(phase, phys)) {
            if saves {
                // Edited in place, so the version as it stands is copied.
                let prior = Prior {
                    phase,
                    bytes: block_copy(&held.0)?,
                };
                self.make_room(0, 1, 0)?;
                self.record(phys, Some(prior))?;
            }
        } else if !saves && self.entries.contains_key(&(phase.other(), phys)) {
            self.move_across(phase, phys)?;
        } else {
            let fresh = if let Some(held) = self.staged(phys) {
                block_copy(held)?
            } else {
                let zeroed = tairix_util::fallible::zeroed(self.block_size);
                let mut fresh = Staged(zeroed.ok_or(DriverError::OutOfMemory)?);
                load(&mut fresh.0)?;
                fresh
            };
            self.put(phase, phys, fresh, saves)?;
        }
        self.entries
            .get_mut(&(phase, phys))
            .map(|held| held.0.as_mut_slice())
            .ok_or(DriverError::OutOfMemory)
    }

    /// The bytes staged at `phys` in either phase, if the set holds it.
    #[must_use]
    pub fn staged(&self, phys: u64) -> Option<&[u8]> {
        WritePhase::ALL
            .iter()
            .find_map(|&phase| self.entries.get(&(phase, phys)))
            .map(|held| held.0.as_slice())
    }

    /// Every block staged for `phase`, in ascending address order, left
    /// staged.
    pub fn blocks(&self, phase: WritePhase) -> impl Iterator<Item = (u64, &[u8])> + '_ {
        self.entries
            .range((phase, 0)..=(phase, u64::MAX))
            .map(|(&(_, phys), bytes)| (phys, bytes.0.as_slice()))
    }

    /// How many blocks `phase` holds.
    #[must_use]
    pub fn count(&self, phase: WritePhase) -> usize {
        self.entries.range((phase, 0)..=(phase, u64::MAX)).count()
    }

    /// How many of the `blocks` addresses from `phys` are staged.
    #[must_use]
    pub fn staged_in(&self, phys: u64, blocks: u64) -> u64 {
        let Some(last) = blocks.checked_sub(1).map(|last| phys.saturating_add(last)) else {
            return 0;
        };
        WritePhase::ALL
            .iter()
            .map(|&phase| self.entries.range((phase, phys)..=(phase, last)).count() as u64)
            .sum()
    }

    /// Overlay every staged block of the run starting at `phys` onto `run`,
    /// which holds that run's bytes as the device has them.
    ///
    /// # Errors
    ///
    /// [`DriverError::BufferTooSmall`] when `run` is not a whole number of
    /// blocks.
    pub fn overlay(&self, phys: u64, run: &mut [u8]) -> Result<(), DriverError> {
        if !run.len().is_multiple_of(self.block_size) {
            return Err(DriverError::BufferTooSmall);
        }
        let blocks = (run.len() / self.block_size) as u64;
        let Some(last) = blocks.checked_sub(1).map(|last| phys.saturating_add(last)) else {
            return Ok(());
        };
        for phase in WritePhase::ALL {
            for ((_, at), bytes) in self.entries.range((phase, phys)..=(phase, last)) {
                let index =
                    usize::try_from(*at - phys).map_err(|_| DriverError::LengthOutOfRange)?;
                let off = index
                    .checked_mul(self.block_size)
                    .ok_or(DriverError::LengthOutOfRange)?;
                let slot = run
                    .get_mut(off..off + self.block_size)
                    .ok_or(DriverError::BufferTooSmall)?;
                slot.copy_from_slice(&bytes.0);
            }
        }
        Ok(())
    }

    /// Drain one phase in ascending physical runs.
    ///
    /// `write` receives a run's start address and its payload: blocks the set
    /// holds at consecutive addresses, never more than `run_bytes`. With no
    /// window to gather into — a machine too short of memory to reserve one —
    /// each block is written straight from the set, so a commit costs more
    /// requests rather than failing.
    ///
    /// # Errors
    ///
    /// Whatever `write` reports. The failed run and everything after it stay
    /// staged.
    pub fn drain<F>(
        &mut self,
        phase: WritePhase,
        run_bytes: usize,
        mut write: F,
    ) -> Result<(), DriverError>
    where
        F: FnMut(u64, &[u8]) -> Result<(), DriverError>,
    {
        let bound = run_bytes / self.block_size * self.block_size;
        let mut window = RunWindow::new(self.longest_run_bytes(phase, bound));
        while let Some((&(_, start), _)) = self.entries.range((phase, 0)..=(phase, u64::MAX)).next()
        {
            let (blocks, outcome) = match window.buf() {
                Some(buf) => {
                    let (blocks, run) = self.gather(phase, start, buf)?;
                    (blocks, write(start, run))
                }
                None => match self.entries.get(&(phase, start)) {
                    Some(block) => (1, write(start, &block.0)),
                    None => (1, Err(DriverError::BufferTooSmall)),
                },
            };
            outcome?;
            self.release(phase, start, blocks);
        }
        Ok(())
    }

    /// Drain one named block without disturbing other work in the phase.
    ///
    /// # Errors
    ///
    /// [`DriverError::DeviceFault`] when the set does not hold `phys`, or
    /// whatever `write` reports.
    pub fn drain_block<F>(
        &mut self,
        phase: WritePhase,
        phys: u64,
        write: F,
    ) -> Result<(), DriverError>
    where
        F: FnOnce(u64, &[u8]) -> Result<(), DriverError>,
    {
        let block = self
            .entries
            .get(&(phase, phys))
            .ok_or(DriverError::DeviceFault)?;
        write(phys, &block.0)?;
        self.release(phase, phys, 1);
        Ok(())
    }

    /// Whether the running operation has changed anything the set holds.
    #[must_use]
    pub fn operation_changed(&self) -> bool {
        self.savepoint
            .as_ref()
            .is_some_and(|savepoint| !savepoint.is_empty())
    }

    /// Whether `phase` holds at least one block.
    #[must_use]
    pub fn has(&self, phase: WritePhase) -> bool {
        self.entries
            .range((phase, 0)..=(phase, u64::MAX))
            .next()
            .is_some()
    }

    /// Bytes the longest run of adjacent staged blocks occupies, capped at
    /// `bound`, so a metadata-only transaction reserves a handful of blocks
    /// rather than the whole window.
    fn longest_run_bytes(&self, phase: WritePhase, bound: usize) -> usize {
        let mut longest = 0usize;
        let mut run = 0usize;
        let mut want = None;
        for &(_, at) in self
            .entries
            .range((phase, 0)..=(phase, u64::MAX))
            .map(|(key, _)| key)
        {
            run = if want == Some(at) { run + 1 } else { 1 };
            longest = longest.max(run);
            want = Some(at.saturating_add(1));
        }
        longest.saturating_mul(self.block_size).min(bound)
    }

    /// Copy the run of adjacent staged blocks starting at `start` into
    /// `window`, returning how many blocks it holds and their payload. One
    /// pass decides the length and lays the bytes out, so the two cannot
    /// disagree.
    fn gather<'w>(
        &self,
        phase: WritePhase,
        start: u64,
        window: &'w mut [u8],
    ) -> Result<(usize, &'w [u8]), DriverError> {
        let mut want = start;
        let mut span = 0usize;
        for ((_, at), bytes) in self.entries.range((phase, start)..=(phase, u64::MAX)) {
            if *at != want {
                break;
            }
            let Some(end) = span.checked_add(self.block_size) else {
                break;
            };
            let Some(slot) = window.get_mut(span..end) else {
                break;
            };
            let held = bytes
                .0
                .get(..self.block_size)
                .ok_or(DriverError::BufferTooSmall)?;
            slot.copy_from_slice(held);
            span = end;
            want = want.saturating_add(1);
        }
        if span == 0 {
            return Err(DriverError::BufferTooSmall);
        }
        let run = window.get(..span).ok_or(DriverError::BufferTooSmall)?;
        Ok((span / self.block_size, run))
    }

    fn release(&mut self, phase: WritePhase, start: u64, blocks: usize) {
        let mut at = start;
        for _ in 0..blocks {
            self.entries.remove(&(phase, at));
            at = at.saturating_add(1);
        }
        self.shrink_if_empty();
    }

    /// Give the node arena back once nothing is staged and no undo is owed
    /// room in it, as the transaction's blocks leave.
    fn shrink_if_empty(&mut self) {
        if self.entries.is_empty() && self.undo_inserts == 0 {
            self.entries.clear();
        }
    }

    /// Drop every block of `start..start + len` the set holds: nothing will
    /// reference them, so writing them out would cost a command for contents
    /// no reader can reach. Costs what is staged inside the run, nothing per
    /// block of it. A block the running operation has yet to change moves
    /// into its savepoint, so an undo restores it.
    ///
    /// # Errors
    ///
    /// [`DriverError::OutOfMemory`] when the savepoint cannot be given room
    /// for those blocks; nothing is dropped.
    pub fn discard_run(&mut self, start: u64, len: u64) -> Result<(), DriverError> {
        let Some(last) = len.checked_sub(1).map(|last| start.saturating_add(last)) else {
            return Ok(());
        };
        let (held, unsaved) = self.held_in(start, last);
        self.make_room(0, unsaved, held)?;
        for phase in WritePhase::ALL {
            while let Some(phys) = self
                .entries
                .range((phase, start)..=(phase, last))
                .next()
                .map(|(&(_, phys), _)| phys)
            {
                let saves = self.saves_first(phys);
                if saves {
                    self.record(phys, None)?;
                }
                let held = self.entries.remove(&(phase, phys));
                if saves {
                    self.fill_record(phys, held.map(|bytes| Prior { phase, bytes }));
                }
                self.took_out(phase, phys);
            }
        }
        Ok(())
    }

    /// Blocks of `start..=last` the set holds, and those of them the running
    /// operation has yet to record; none while no operation runs, as then no
    /// undo is owed room.
    fn held_in(&self, start: u64, last: u64) -> (usize, usize) {
        let Some(savepoint) = self.savepoint.as_ref() else {
            return (0, 0);
        };
        let mut counts = (0, 0);
        for phase in WritePhase::ALL {
            for ((_, phys), _) in self.entries.range((phase, start)..=(phase, last)) {
                counts.0 += 1;
                counts.1 += usize::from(!savepoint.contains_key(phys));
            }
        }
        counts
    }

    /// Drop and wipe every block in one phase.
    pub fn clear_phase(&mut self, phase: WritePhase) {
        self.drop_savepoint();
        while let Some((&(_, phys), _)) = self.entries.range((phase, 0)..=(phase, u64::MAX)).next()
        {
            self.entries.remove(&(phase, phys));
        }
        self.shrink_if_empty();
    }

    /// Drop and wipe every staged block.
    pub fn clear(&mut self) {
        self.drop_savepoint();
        self.entries.clear();
    }

    /// Blocks currently staged.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing is staged.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether an operation's savepoint is installed. Between operations it
    /// must not be: a stale one would record the next caller's changes
    /// against a snapshot nothing will unwind to.
    #[must_use]
    pub const fn operation_running(&self) -> bool {
        self.savepoint.is_some()
    }
}

#[cfg(test)]
#[path = "dirty_tests.rs"]
mod tests;
