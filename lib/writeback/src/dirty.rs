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

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::num::NonZeroUsize;

use tairix_abi::DriverError;
use tairix_reclaim::MAP_ENTRY_OVERHEAD;
use zeroize::Zeroize;

use crate::RunWindow;

/// One block's staged state before the running operation first changed it.
struct Prior {
    phase: WritePhase,
    bytes: Vec<u8>,
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
    entries: BTreeMap<(WritePhase, u64), Vec<u8>>,
    /// Each block the running operation has changed, as it stood before —
    /// `None` where the set held nothing. Absent between operations.
    savepoint: Option<BTreeMap<u64, Option<Prior>>>,
    block_size: usize,
    /// Buffers the savepoint holds; the staged half is the map's length.
    saved_priors: usize,
}

/// `block` in a buffer of its own.
fn block_copy(block: &[u8]) -> Result<Vec<u8>, DriverError> {
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(block.len())
        .map_err(|_| DriverError::NoSpace)?;
    bytes.extend_from_slice(block);
    Ok(bytes)
}

impl DirtySet {
    /// An empty set over blocks of `block_size`. Allocates nothing until the
    /// first block is staged.
    ///
    /// A size of zero, which every later division would trip over, is not a
    /// value the type admits:
    ///
    /// ```compile_fail
    /// let _ = tairix_writeback::DirtySet::new(0usize);
    /// ```
    #[must_use]
    pub const fn new(block_size: NonZeroUsize) -> Self {
        Self {
            entries: BTreeMap::new(),
            savepoint: None,
            block_size: block_size.get(),
            saved_priors: 0,
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
        self.savepoint = Some(BTreeMap::new());
    }

    /// The operation succeeded: its changes are the transaction's now.
    pub fn end_operation(&mut self) {
        self.drop_savepoint();
    }

    /// Undo the running operation's changes, leaving every block the rest of
    /// the transaction staged exactly as it was.
    pub fn undo_operation(&mut self) {
        let Some(savepoint) = self.savepoint.take() else {
            return;
        };
        self.saved_priors = 0;
        for (phys, prior) in savepoint {
            for phase in WritePhase::ALL {
                if let Some(mut bytes) = self.entries.remove(&(phase, phys)) {
                    bytes.zeroize();
                }
            }
            if let Some(prior) = prior {
                self.entries.insert((prior.phase, phys), prior.bytes);
            }
        }
    }

    fn drop_savepoint(&mut self) {
        let Some(savepoint) = self.savepoint.take() else {
            return;
        };
        self.saved_priors = 0;
        for prior in savepoint.into_values().flatten() {
            let mut bytes = prior.bytes;
            bytes.zeroize();
        }
    }

    /// Whether the running operation has yet to record `phys` as it stood.
    fn saves_first(&self, phys: u64) -> bool {
        self.savepoint
            .as_ref()
            .is_some_and(|savepoint| !savepoint.contains_key(&phys))
    }

    /// Record `prior` — `phys` as it stood, absent when the set did not hold
    /// it — in the running operation's savepoint.
    fn record_prior(&mut self, phys: u64, prior: Option<Prior>) {
        if prior.is_some() {
            self.saved_priors = self.saved_priors.saturating_add(1);
        }
        if let Some(savepoint) = self.savepoint.as_mut() {
            savepoint.insert(phys, prior);
        }
    }

    /// Copy `phys` as it stands into the running operation's savepoint, the
    /// first time that operation changes it. A block the set does not hold is
    /// recorded as absent, so undoing removes whatever the operation staged.
    fn save_prior(&mut self, phys: u64) -> Result<(), DriverError> {
        if !self.saves_first(phys) {
            return Ok(());
        }
        let mut prior = None;
        for phase in WritePhase::ALL {
            if let Some(held) = self.entries.get(&(phase, phys)) {
                prior = Some(Prior {
                    phase,
                    bytes: block_copy(held)?,
                });
                break;
            }
        }
        self.record_prior(phys, prior);
        Ok(())
    }

    /// Whether staging `phys` for `phase` must be undoable.
    fn undoable(&self, phase: WritePhase, phys: u64) -> bool {
        phase == WritePhase::BeforeBarrier
            || self
                .entries
                .contains_key(&(WritePhase::BeforeBarrier, phys))
    }

    /// Stage `bytes` at `phys` for `phase`, replacing any held version.
    ///
    /// # Errors
    ///
    /// [`DriverError::BufferTooSmall`] when `bytes` is shorter than a block,
    /// [`DriverError::NoSpace`] when a first-time entry cannot be allocated.
    pub fn stage(&mut self, phase: WritePhase, phys: u64, bytes: &[u8]) -> Result<(), DriverError> {
        let block = bytes
            .get(..self.block_size)
            .ok_or(DriverError::BufferTooSmall)?;
        if self.undoable(phase, phys) && self.saves_first(phys) {
            // Replaced whole, the held version moves into the savepoint
            // rather than being copied there and then overwritten.
            let fresh = block_copy(block)?;
            let prior = WritePhase::ALL.into_iter().find_map(|held| {
                self.entries
                    .remove(&(held, phys))
                    .map(|bytes| Prior { phase: held, bytes })
            });
            self.record_prior(phys, prior);
            self.entries.insert((phase, phys), fresh);
            return Ok(());
        }
        self.stage_with(phase, phys, |_| Ok(()))?
            .copy_from_slice(block);
        Ok(())
    }

    /// The block staged at `phys` for `phase`, made writable in place.
    ///
    /// A block the set does not hold yet is first filled by `load` — the
    /// caller's read of what the device has there — so an in-place edit
    /// starts from the current contents. The savepoint is taken exactly as
    /// [`Self::stage`] takes it.
    ///
    /// # Errors
    ///
    /// Whatever `load` reports, or [`DriverError::NoSpace`] when a first-time
    /// entry cannot be allocated. A failed `load` stages nothing.
    pub fn stage_with<F>(
        &mut self,
        phase: WritePhase,
        phys: u64,
        load: F,
    ) -> Result<&mut [u8], DriverError>
    where
        F: FnOnce(&mut [u8]) -> Result<(), DriverError>,
    {
        if self.undoable(phase, phys) {
            self.save_prior(phys)?;
        }
        if !self.entries.contains_key(&(phase, phys)) {
            let held = if let Some(held) = self.entries.remove(&(phase.other(), phys)) {
                held
            } else {
                let mut held = Vec::new();
                held.try_reserve_exact(self.block_size)
                    .map_err(|_| DriverError::NoSpace)?;
                held.resize(self.block_size, 0);
                if let Err(err) = load(&mut held) {
                    held.zeroize();
                    return Err(err);
                }
                held
            };
            self.entries.insert((phase, phys), held);
        }
        self.entries
            .get_mut(&(phase, phys))
            .map(Vec::as_mut_slice)
            .ok_or(DriverError::NoSpace)
    }

    /// The bytes staged at `phys` in either phase, if the set holds it.
    #[must_use]
    pub fn staged(&self, phys: u64) -> Option<&[u8]> {
        WritePhase::ALL
            .iter()
            .find_map(|&phase| self.entries.get(&(phase, phys)))
            .map(Vec::as_slice)
    }

    /// Every block staged for `phase`, in ascending address order, left
    /// staged.
    pub fn blocks(&self, phase: WritePhase) -> impl Iterator<Item = (u64, &[u8])> + '_ {
        self.entries
            .range((phase, 0)..=(phase, u64::MAX))
            .map(|(&(_, phys), bytes)| (phys, bytes.as_slice()))
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
                slot.copy_from_slice(bytes);
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
                    Some(block) => (1, write(start, block)),
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
        write(phys, block)?;
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
            slot.copy_from_slice(
                bytes
                    .get(..self.block_size)
                    .ok_or(DriverError::BufferTooSmall)?,
            );
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
            if let Some(mut bytes) = self.entries.remove(&(phase, at)) {
                bytes.zeroize();
            }
            at = at.saturating_add(1);
        }
    }

    /// Drop every block of `start..start + len` the set holds: nothing will
    /// reference them, so writing them out would cost a command for contents
    /// no reader can reach. Costs what is staged inside the run, nothing per
    /// block of it.
    pub fn discard_run(&mut self, start: u64, len: u64) {
        let Some(last) = len.checked_sub(1).map(|last| start.saturating_add(last)) else {
            return;
        };
        for phase in WritePhase::ALL {
            while let Some(phys) = self
                .entries
                .range((phase, start)..=(phase, last))
                .next()
                .map(|(&(_, phys), _)| phys)
            {
                if let Some(bytes) = self.entries.remove(&(phase, phys)) {
                    self.retire(phase, phys, bytes);
                }
            }
        }
    }

    /// Retire `bytes`, just taken from `phys` in `phase`: moved into the
    /// running operation's savepoint the first time that operation changes
    /// `phys`, so an undo restores it, and wiped otherwise.
    fn retire(&mut self, phase: WritePhase, phys: u64, mut bytes: Vec<u8>) {
        match self.savepoint.as_mut() {
            Some(savepoint) if !savepoint.contains_key(&phys) => {
                self.saved_priors = self.saved_priors.saturating_add(1);
                savepoint.insert(phys, Some(Prior { phase, bytes }));
            }
            _ => bytes.zeroize(),
        }
    }

    /// Drop and wipe every block in one phase.
    pub fn clear_phase(&mut self, phase: WritePhase) {
        self.drop_savepoint();
        while let Some((&(_, phys), _)) = self.entries.range((phase, 0)..=(phase, u64::MAX)).next()
        {
            if let Some(mut bytes) = self.entries.remove(&(phase, phys)) {
                bytes.zeroize();
            }
        }
    }

    /// Drop and wipe every staged block.
    pub fn clear(&mut self) {
        self.drop_savepoint();
        while let Some((_, mut bytes)) = self.entries.pop_first() {
            bytes.zeroize();
        }
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

impl Drop for DirtySet {
    fn drop(&mut self) {
        self.clear();
    }
}

#[cfg(test)]
#[path = "dirty_tests.rs"]
mod tests;
