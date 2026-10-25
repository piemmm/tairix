//! Operations and the transactions they join.
//!
//! An operation runs against the open transaction and is undone alone if it
//! fails. The transaction is published when its window closes, when it nears
//! the memory bound or the journal's share, or on an explicit sync:
//!
//! * journaled — the superblock's `needs_recovery` is set, the blocks are
//!   logged and committed behind barriers, written home, the log emptied, and
//!   `needs_recovery` cleared, so a quiet volume is clean and a crash at any
//!   point leaves either the old state or one replay makes whole;
//! * unjournaled (ext2) — the superblock is marked not clean, the blocks are
//!   written home behind a barrier, and the mark is lifted, so a crash
//!   mid-write sends the volume to a checker rather than passing as clean.

use tairix_abi::driver::block::Block;
use tairix_abi::DriverError;
use tairix_writeback::{WritePhase, RUN_BYTES};

use crate::bytes::{high32, le16, le32, low32, put_le16, put_le32, scratch};
use crate::journal::{CommitError, JournalIo};
use crate::superblock::{
    self, NoWrite, INCOMPAT_RECOVER, SB_FEATURE_INCOMPAT, SB_MNT_COUNT, SB_MTIME, SB_MTIME_HI,
    SB_STATE, SB_WTIME, SB_WTIME_HI, STATE_VALID, SUPERBLOCK_LEN, SUPERBLOCK_OFFSET,
};
use crate::{Ext4, JournalMap};

/// What undoing the running operation restores beside the dirty set and the
/// superblock image the mark copies into `Ext4::mark_sb`.
pub(crate) struct OpMark {
    freed: usize,
    freed_cost: usize,
    generation: u32,
    /// The orphan index's undo log, as long as it was.
    orphans: usize,
}

/// Blocks a commit stages beyond the transaction's own: the superblock's.
const COMMIT_OVERHEAD: usize = 2;

/// The journal as the device sees it: journal blocks through the journal
/// file's map, home blocks in place.
pub(crate) struct JournalDev<'a, B: Block> {
    pub(crate) dev: &'a mut crate::io::Device<B>,
    pub(crate) map: &'a JournalMap,
}

impl<B: Block> JournalIo for JournalDev<'_, B> {
    fn read_journal(&mut self, n: u32, buf: &mut [u8]) -> Result<(), DriverError> {
        let block = self.map.resolve(n).ok_or(DriverError::DeviceFault)?;
        self.dev.read_blocks(block, buf)
    }

    fn write_journal(&mut self, n: u32, buf: &[u8]) -> Result<(), DriverError> {
        let block = self.map.resolve(n).ok_or(DriverError::DeviceFault)?;
        self.dev.write_blocks(block, buf)
    }

    fn write_home(&mut self, block: u64, buf: &[u8]) -> Result<(), DriverError> {
        self.dev.write_blocks(block, buf)
    }

    fn flush(&mut self) -> Result<(), DriverError> {
        self.dev.flush()
    }
}

/// Where a failed publication stopped.
enum Stopped {
    /// Before anything it wrote could be taken for the new state.
    Clean(DriverError),
    /// After the medium began to carry the new state.
    Torn(DriverError),
}

impl<B: Block> Ext4<B> {
    /// Refuse a write this handle may not make.
    pub(crate) fn ensure_writable(&self) -> Result<(), DriverError> {
        match self.layout.no_write {
            None => Ok(()),
            Some(_) => Err(DriverError::ReadOnly),
        }
    }

    /// Run `body` as one operation, undone alone if it fails. A transaction
    /// that has reached its bound, or whose window has closed, is published
    /// first.
    pub(crate) fn operation<T>(
        &mut self,
        body: impl FnOnce(&mut Self) -> Result<T, DriverError>,
    ) -> Result<T, DriverError> {
        self.ensure_writable()?;
        if self.pending() && (self.needs_publishing() || self.schedule.expired()) {
            self.commit(false)?;
        }
        self.run(body)
    }

    fn run<T>(
        &mut self,
        body: impl FnOnce(&mut Self) -> Result<T, DriverError>,
    ) -> Result<T, DriverError> {
        self.begin();
        let outcome = body(self);
        let fault = self.release_fault.take();
        match (outcome, fault) {
            (Ok(value), None) => {
                self.end()?;
                Ok(value)
            }
            (Ok(_), Some(err)) | (Err(err), _) => {
                self.undo();
                Err(err)
            }
        }
    }

    fn mark(&mut self) -> OpMark {
        self.mark_sb.copy_from_slice(&self.sb);
        OpMark {
            freed: self.freed.len(),
            freed_cost: self.freed_cost,
            generation: self.generation,
            orphans: self.orphan_undo.len(),
        }
    }

    fn begin(&mut self) {
        if let Some(bound) = &self.bound {
            let reading = bound.reading(self.dirty.pinned_bytes());
            self.schedule.set_band(reading.band);
            self.reading = Some(reading);
        }
        self.dirty.begin_operation();
        self.op = Some(self.mark());
    }

    /// Keep what the running operation did so far; a later failure undoes
    /// only what follows.
    pub(crate) fn savepoint(&mut self) {
        self.dirty.end_operation();
        self.dirty.begin_operation();
        self.op = Some(self.mark());
    }

    fn undo(&mut self) {
        self.dirty.undo_operation();
        self.hints.forget_full();
        if let Some(mark) = self.op.take() {
            self.sb.copy_from_slice(&self.mark_sb);
            self.freed.truncate(mark.freed);
            self.freed_cost = mark.freed_cost;
            self.generation = mark.generation;
            self.rewind_orphans(mark.orphans);
        }
    }

    /// Undo what the running operation did since its last savepoint, and
    /// carry on from there.
    pub(crate) fn undo_to_savepoint(&mut self) {
        self.undo();
        self.dirty.begin_operation();
        self.op = Some(self.mark());
    }

    fn end(&mut self) -> Result<(), DriverError> {
        self.dirty.end_operation();
        self.op = None;
        if self.pending() {
            self.schedule.opened();
            if self.schedule.defers() {
                // The host's timer publishes the window's end, and the next
                // operation a transaction that has reached its bound.
                self.schedule.joined();
            } else {
                // Published before the operation is reported done, so a
                // commit that fails cleanly fails this operation alone.
                self.commit(false)?;
            }
        }
        self.publish_footprint();
        Ok(())
    }

    /// Whether the open transaction changed anything.
    pub(crate) fn pending(&self) -> bool {
        !self.dirty.is_empty() || !self.freed.is_empty() || self.sb != self.disk_sb
    }

    /// Whether the transaction has reached the memory bound or the journal's
    /// share, so it must be published before it grows further.
    fn needs_publishing(&self) -> bool {
        let over_bound = self
            .reading
            .is_some_and(|reading| self.dirty.pinned_bytes() > reading.ceiling);
        let journal_full = self.journal.as_ref().is_some_and(|journal| {
            self.dirty.len() + self.freed_cost + COMMIT_OVERHEAD >= journal.capacity() / 2
        });
        over_bound || journal_full
    }

    /// Whether a long operation should stop here and report what it has
    /// done, so its transaction stays within the bound and the journal.
    pub(crate) fn should_yield(&self) -> bool {
        self.needs_publishing()
    }

    /// Blocks the open transaction may still stage before it must be
    /// published, or `None` when neither a journal nor a bound limits it.
    pub(crate) fn room(&self) -> Option<usize> {
        let staged = self.dirty.len() + self.freed_cost + COMMIT_OVERHEAD;
        let journal = self
            .journal
            .as_ref()
            .map(|journal| (journal.capacity() / 2).saturating_sub(staged));
        let memory = self.reading.map(|reading| {
            let free = reading.ceiling.saturating_sub(self.dirty.pinned_bytes());
            (free / self.layout.block_size as usize).saturating_sub(self.freed_cost)
        });
        match (journal, memory) {
            (Some(journal), Some(memory)) => Some(journal.min(memory)),
            (journal, memory) => journal.or(memory),
        }
    }

    /// Publish in the middle of an operation that spans transactions — one
    /// the orphan list keeps whole across a crash.
    pub(crate) fn commit_midway(&mut self) -> Result<(), DriverError> {
        self.dirty.end_operation();
        self.schedule.opened();
        let published = self.commit(false);
        self.dirty.begin_operation();
        self.op = Some(self.mark());
        published
    }

    pub(crate) fn publish_footprint(&self) {
        if let Some(bound) = &self.bound {
            bound.publish(self.dirty.pinned_bytes(), self.dirty.held() as u64);
        }
    }

    /// Report a crossing of the reserve to the host, once per crossing.
    pub(crate) fn note_reserve(&mut self, in_reserve: bool) {
        if self.in_reserve == in_reserve {
            return;
        }
        self.in_reserve = in_reserve;
        if let Some((volume, host)) = self.schedule.host() {
            host.reserve_crossed(volume, in_reserve);
        }
    }

    /// The wall clock as the superblock's split stamp records it.
    fn stamp_now(&self, image: &mut [u8], lo: usize, hi: usize) {
        if let Some(now) = self.now() {
            let secs = u64::try_from(now.secs()).unwrap_or(0);
            put_le32(image, lo, low32(secs));
            image[hi] = high32(secs).to_le_bytes()[0];
        }
    }

    /// Publish the open transaction, and with `durable` make every write
    /// before it durable and leave the volume clean.
    pub(crate) fn commit(&mut self, durable: bool) -> Result<(), DriverError> {
        if self.pending() {
            let acknowledged = self.schedule.acknowledged();
            match self.publish() {
                Ok(()) => {
                    self.data_written = false;
                    self.orphan_undo.clear();
                    self.schedule.closed();
                    if let Some(bound) = &self.bound {
                        bound.note_released();
                    }
                    self.publish_footprint();
                }
                Err(Stopped::Clean(err)) => {
                    // Nothing reached the medium as the new state: the
                    // transaction is lost whole. Operations already told they
                    // succeeded lose their work, so the handle stops writing.
                    self.dirty.clear();
                    self.freed.clear();
                    self.freed_cost = 0;
                    self.hints.forget_full();
                    self.sb.copy_from_slice(&self.disk_sb);
                    self.rewind_orphans(0);
                    self.schedule.closed();
                    if acknowledged > 0 {
                        self.layout.no_write = Some(NoWrite::Halted);
                    }
                    self.publish_footprint();
                    return Err(err);
                }
                Err(Stopped::Torn(err)) => {
                    // The medium is part-way: the journal's replay, a checker,
                    // or a later explicit flush finishes it. The staged state
                    // stays as the read view, and no timer is left to retry.
                    self.layout.no_write = Some(NoWrite::Halted);
                    self.schedule.closed();
                    return Err(err);
                }
            }
        }
        if durable && matches!(self.layout.no_write, None | Some(NoWrite::Halted)) {
            self.settle()?;
        }
        Ok(())
    }

    /// Make every write so far durable, and leave the volume clean: on a
    /// journaled one, once every home write is durable, the log is emptied
    /// durably and only then `needs_recovery` cleared, so the mark never
    /// reads clear while the log holds a transaction.
    fn settle(&mut self) -> Result<(), DriverError> {
        if let (Some(journal), Some(map)) = (self.journal.as_mut(), self.journal_map.as_ref()) {
            let mut io = JournalDev {
                dev: &mut self.dev,
                map,
            };
            journal.quiesce(&mut io)?;
        }
        self.mark_disk(|sb| {
            let incompat = le32(sb, SB_FEATURE_INCOMPAT) & !INCOMPAT_RECOVER;
            put_le32(sb, SB_FEATURE_INCOMPAT, incompat);
        })?;
        self.sb.copy_from_slice(&self.disk_sb);
        self.dev.flush()?;
        self.data_written = false;
        Ok(())
    }

    fn publish(&mut self) -> Result<(), Stopped> {
        self.apply_frees().map_err(Stopped::Clean)?;
        let mut image = scratch(SUPERBLOCK_LEN).map_err(Stopped::Clean)?;
        image.copy_from_slice(&self.sb);
        self.stamp_now(&mut image, SB_WTIME, SB_WTIME_HI);
        if !self.session_counted {
            let mounts = le16(&image, SB_MNT_COUNT).wrapping_add(1);
            put_le16(&mut image, SB_MNT_COUNT, mounts);
            self.stamp_now(&mut image, SB_MTIME, SB_MTIME_HI);
        }
        if self.journal.is_some() {
            self.publish_journaled(&mut image)?;
        } else {
            self.publish_unjournaled(&mut image)?;
        }
        self.disk_sb.copy_from_slice(&image);
        self.sb.copy_from_slice(&image);
        self.session_counted = true;
        Ok(())
    }

    /// Log the transaction, commit it behind its barriers, and write it home.
    /// `needs_recovery` is set before anything is logged and stays set while
    /// the volume is busy; home writes are made durable by the next commit's
    /// first barrier or by settling, before anything moves the log past them.
    fn publish_journaled(&mut self, image: &mut [u8]) -> Result<(), Stopped> {
        let incompat = le32(image, SB_FEATURE_INCOMPAT) | INCOMPAT_RECOVER;
        put_le32(image, SB_FEATURE_INCOMPAT, incompat);
        if self.layout.metadata_csum {
            superblock::stamp(image);
        }
        self.stage_superblock(image).map_err(Stopped::Clean)?;
        self.mark_disk(|sb| {
            let incompat = le32(sb, SB_FEATURE_INCOMPAT) | INCOMPAT_RECOVER;
            put_le32(sb, SB_FEATURE_INCOMPAT, incompat);
        })
        .map_err(Stopped::Clean)?;
        let now = self.now().map_or((0, 0), |t| {
            (u64::try_from(t.secs()).unwrap_or(0), t.subsec_nanos())
        });
        let count = self.dirty.count(WritePhase::BeforeBarrier);
        let (Some(journal), Some(map)) = (self.journal.as_mut(), self.journal_map.as_ref()) else {
            return Err(Stopped::Clean(DriverError::DeviceFault));
        };
        let mut io = JournalDev {
            dev: &mut self.dev,
            map,
        };
        journal
            .commit(
                &mut io,
                self.dirty.blocks(WritePhase::BeforeBarrier),
                count,
                now,
            )
            .map_err(|err| match err {
                CommitError::Unwritten(err) => Stopped::Clean(err),
                CommitError::Uncertain(err) => Stopped::Torn(err),
            })?;
        let dev = &mut self.dev;
        self.dirty
            .drain(WritePhase::BeforeBarrier, RUN_BYTES, |block, run| {
                dev.write_blocks(block, run)
            })
            .map_err(Stopped::Torn)
    }

    /// Mark the volume not clean behind a barrier, write the blocks home
    /// behind another, and mark it clean: a crash part-way leaves it for a
    /// checker rather than passing as clean.
    fn publish_unjournaled(&mut self, image: &mut [u8]) -> Result<(), Stopped> {
        if self.data_written {
            self.dev.flush().map_err(Stopped::Clean)?;
        }
        self.mark_disk(|sb| {
            let state = le16(sb, SB_STATE) & !STATE_VALID;
            put_le16(sb, SB_STATE, state);
        })
        .map_err(Stopped::Clean)?;
        self.dev.flush().map_err(Stopped::Clean)?;
        let dev = &mut self.dev;
        self.dirty
            .drain(WritePhase::BeforeBarrier, RUN_BYTES, |block, run| {
                dev.write_blocks(block, run)
            })
            .map_err(Stopped::Torn)?;
        self.dev.flush().map_err(Stopped::Torn)?;
        let state = le16(image, SB_STATE) | STATE_VALID;
        put_le16(image, SB_STATE, state);
        if self.layout.metadata_csum {
            superblock::stamp(image);
        }
        self.dev
            .write_bytes(SUPERBLOCK_OFFSET, image)
            .map_err(Stopped::Torn)
    }

    /// Stage superblock `image` into the block that holds it.
    fn stage_superblock(&mut self, image: &[u8]) -> Result<(), DriverError> {
        let block = self.layout.sb_block;
        let off = self.layout.sb_offset;
        let dev = &mut self.dev;
        let staged = self
            .dirty
            .stage_with(WritePhase::BeforeBarrier, block, |buf| {
                dev.read_blocks(block, buf)
            })?;
        staged[off..off + SUPERBLOCK_LEN].copy_from_slice(image);
        Ok(())
    }

    /// Write the published superblock again with `change` made, straight to
    /// the medium: the marks that bracket a publication.
    fn mark_disk(&mut self, change: impl FnOnce(&mut [u8])) -> Result<(), DriverError> {
        let mut sb = scratch(SUPERBLOCK_LEN)?;
        sb.copy_from_slice(&self.disk_sb);
        change(&mut sb);
        if sb == self.disk_sb {
            return Ok(());
        }
        if self.layout.metadata_csum {
            superblock::stamp(&mut sb);
        }
        self.dev.write_bytes(SUPERBLOCK_OFFSET, &sb)?;
        // Only once the medium holds it: a refused write leaves the record
        // of what the medium holds true.
        self.disk_sb.copy_from_slice(&sb);
        Ok(())
    }
}
