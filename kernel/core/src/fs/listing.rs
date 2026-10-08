//! Where a directory listing stands between `fs_readdir` batches.
//!
//! A listing is the directory's own entries, read through the driver's
//! resumable cursor, and then the covered mount points beneath it that the
//! volume holds no entry for, in name order. [`Listing`] records which of the
//! two it is in, how far it has got, and the directory its first batch read.

use alloc::vec::Vec;

use tairix_sync::SpinLock;
use tairix_util::secret::{wipe, Wiped};

use super::path::MAX_COMPONENT_LEN;
use super::VfsError;

/// The directories open listings are bound to on one mounted filesystem.
///
/// A listing resolves its path afresh on every batch, so a node number alone
/// cannot tell its directory from a successor made at the same path: FAT hands
/// a removed directory's first cluster to the next one it makes, ext4 its
/// inode. Removing a directory marks its record here, and every listing bound
/// to it is stale from then on, whatever number the next directory takes.
///
/// A record lives exactly as long as some listing holds it, so the registry
/// is bounded by the open listings, never by the volume.
#[derive(Debug, Default)]
pub struct ListingRegistry {
    bound: SpinLock<Vec<Bound>>,
}

/// One directory a listing is bound to: the driver it was read through and
/// the node it was.
#[derive(Copy, Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct BoundDir {
    driver: u64,
    node: u64,
}

/// A bound directory's record: how many listings hold it, and how many times
/// it has been removed since the record was made.
#[derive(Debug)]
struct Bound {
    dir: BoundDir,
    holders: usize,
    removals: u64,
}

impl ListingRegistry {
    /// An empty registry, `const` so it can sit in the boot's static mount.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            bound: SpinLock::new(Vec::new()),
        }
    }

    /// Node `node` on driver `driver` was removed: every listing bound to it
    /// is stale.
    pub fn removed(&self, driver: u64, node: u64) {
        let dir = BoundDir { driver, node };
        let mut bound = self.bound.lock();
        if let Ok(at) = bound.binary_search_by_key(&dir, |held| held.dir) {
            bound[at].removals = bound[at].removals.wrapping_add(1);
        }
    }

    /// Bind a listing to node `node` on driver `driver`.
    fn bind(&'static self, driver: u64, node: u64) -> Result<ListingTicket, VfsError> {
        let dir = BoundDir { driver, node };
        let mut bound = self.bound.lock();
        let removals = match bound.binary_search_by_key(&dir, |held| held.dir) {
            Ok(at) => {
                bound[at].holders = bound[at].holders.saturating_add(1);
                bound[at].removals
            }
            Err(at) => {
                bound.try_reserve(1).map_err(|_| VfsError::OutOfMemory)?;
                bound.insert(
                    at,
                    Bound {
                        dir,
                        holders: 1,
                        removals: 0,
                    },
                );
                0
            }
        };
        Ok(ListingTicket {
            registry: self,
            dir,
            removals,
        })
    }

    /// Whether `ticket`'s directory is unremoved since it was bound.
    fn is_current(&self, ticket: &ListingTicket) -> bool {
        let bound = self.bound.lock();
        bound
            .binary_search_by_key(&ticket.dir, |held| held.dir)
            .is_ok_and(|at| bound[at].removals == ticket.removals)
    }

    /// One more listing holds `dir`'s record.
    fn retain(&self, dir: BoundDir) {
        let mut bound = self.bound.lock();
        if let Ok(at) = bound.binary_search_by_key(&dir, |held| held.dir) {
            bound[at].holders = bound[at].holders.saturating_add(1);
        }
    }

    /// One fewer listing holds `dir`'s record, which goes with its last.
    fn release(&self, dir: BoundDir) {
        let mut bound = self.bound.lock();
        if let Ok(at) = bound.binary_search_by_key(&dir, |held| held.dir) {
            bound[at].holders = bound[at].holders.saturating_sub(1);
            if bound[at].holders == 0 {
                bound.remove(at);
            }
        }
    }

    /// How many directories are bound, for the tests' bound on the registry.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.bound.lock().len()
    }
}

/// A listing's hold on its directory's record in a [`ListingRegistry`],
/// released when the listing restarts or goes.
pub struct ListingTicket {
    registry: &'static ListingRegistry,
    dir: BoundDir,
    removals: u64,
}

/// Shows only this listing's own record, never the registry it is held in.
impl core::fmt::Debug for ListingTicket {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ListingTicket")
            .field("dir", &self.dir)
            .field("removals", &self.removals)
            .finish_non_exhaustive()
    }
}

impl ListingTicket {
    /// Whether the directory is unremoved since the listing bound it.
    fn is_current(&self) -> bool {
        self.registry.is_current(self)
    }
}

impl Clone for ListingTicket {
    fn clone(&self) -> Self {
        self.registry.retain(self.dir);
        Self {
            registry: self.registry,
            dir: self.dir,
            removals: self.removals,
        }
    }
}

impl Drop for ListingTicket {
    fn drop(&mut self) {
        self.registry.release(self.dir);
    }
}

/// The name a listing resumes after: the last entry it took. A directory
/// entry's name is decrypted user data, so it is wiped when replaced or
/// dropped, and a debug print shows only its length.
#[derive(Default)]
pub struct ResumeName {
    bytes: Wiped<MAX_COMPONENT_LEN>,
    len: usize,
}

impl ResumeName {
    /// The name, empty before the first entry.
    #[must_use]
    pub fn get(&self) -> &[u8] {
        &self.bytes[..self.len]
    }

    /// Resume after `name` instead.
    ///
    /// # Errors
    ///
    /// [`VfsError::Io`] for a name longer than a path component, which only
    /// a driver breaking its own name bound can produce.
    pub fn set(&mut self, name: &[u8]) -> Result<(), VfsError> {
        self.bytes
            .get_mut(..name.len())
            .ok_or(VfsError::Io)?
            .copy_from_slice(name);
        if let Some(rest) = self.bytes.get_mut(name.len()..self.len) {
            wipe(rest);
        }
        self.len = name.len();
        Ok(())
    }
}

impl core::fmt::Debug for ResumeName {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ResumeName")
            .field("len", &self.len)
            .finish_non_exhaustive()
    }
}

impl Clone for ResumeName {
    fn clone(&self) -> Self {
        let mut copy = Self::default();
        copy.bytes[..self.len].copy_from_slice(self.get());
        copy.len = self.len;
        copy
    }
}

/// A place among one directory's own entries: the directory, the driver
/// cursor the next entry is read at, and the name of the entry that cursor
/// follows.
#[derive(Clone, Debug, Default)]
pub struct DirPosition {
    /// The directory's node, fixed by the batch that first read it: a later
    /// batch that finds another node at the path is refused rather than
    /// resumed inside it.
    pub dir: Option<u64>,
    /// The driver's resume token; `0` is the first entry.
    pub cursor: u64,
    /// The entry `cursor` follows, which a driver whose entries shift resumes
    /// from.
    pub after: ResumeName,
}

/// How a batch over a directory's entries ended.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ListEnd {
    /// The directory has no entries past the position.
    Exhausted,
    /// The visitor stopped, and the position is at the entry it left.
    Stopped,
}

/// Which part of the listing comes next.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
enum Phase {
    /// The directory's own entries.
    #[default]
    Entries,
    /// The covered mount points after the one the position names.
    Mounts,
    /// Nothing is left.
    Done,
}

/// One open description's directory listing, carried across `fs_readdir`
/// calls.
#[derive(Clone, Debug, Default)]
pub struct Listing {
    /// The volume the first batch read; the position fixes the node.
    volume: Option<[u8; 16]>,
    phase: Phase,
    at: DirPosition,
    /// The hold on the directory's record, once a batch has fixed it.
    ticket: Option<ListingTicket>,
}

impl Listing {
    /// Back to the first entry, of whatever the path names now.
    pub fn restart(&mut self) {
        *self = Self::default();
    }

    /// Whether every entry and mount point has been handed over.
    #[must_use]
    pub fn is_done(&self) -> bool {
        self.phase == Phase::Done
    }

    /// Fix the listing to `volume` on its first batch.
    ///
    /// # Errors
    ///
    /// [`VfsError::Stale`] when the listing began on another volume.
    pub fn on_volume(&mut self, volume: [u8; 16]) -> Result<(), VfsError> {
        match self.volume {
            Some(bound) if bound != volume => Err(VfsError::Stale),
            _ => {
                self.volume = Some(volume);
                Ok(())
            }
        }
    }

    /// Hold the listing to its directory's record in `registry`, read through
    /// driver `driver`: bind it once a batch has fixed the directory, and
    /// check it on every batch after.
    ///
    /// # Errors
    ///
    /// [`VfsError::Stale`] when the directory has been removed since the
    /// listing bound it, and [`VfsError::OutOfMemory`] when its record cannot
    /// be made.
    pub fn hold(
        &mut self,
        registry: &'static ListingRegistry,
        driver: u64,
    ) -> Result<(), VfsError> {
        match (&self.ticket, self.at.dir) {
            (Some(ticket), _) if !ticket.is_current() => Err(VfsError::Stale),
            (Some(_), _) | (None, None) => Ok(()),
            (None, Some(node)) => {
                self.ticket = Some(registry.bind(driver, node)?);
                Ok(())
            }
        }
    }

    /// The directory node a batch fixed the listing to, if one has.
    #[must_use]
    pub fn bound_dir(&self) -> Option<u64> {
        self.at.dir
    }

    /// The position among the directory's own entries, while they remain.
    pub fn entries(&mut self) -> Option<&mut DirPosition> {
        (self.phase == Phase::Entries).then_some(&mut self.at)
    }

    /// The directory's own entries are exhausted: the mount points follow.
    pub fn finish_entries(&mut self) {
        self.phase = Phase::Mounts;
        self.at = DirPosition {
            dir: self.at.dir,
            ..DirPosition::default()
        };
    }

    /// The last mount point handed over, while mount points remain.
    pub fn mounts(&mut self) -> Option<&mut ResumeName> {
        (self.phase == Phase::Mounts).then_some(&mut self.at.after)
    }

    /// Nothing is left to hand over.
    pub fn finish(&mut self) {
        self.phase = Phase::Done;
        self.at.after = ResumeName::default();
    }
}

/// Serve the fixed `entries` through `at` as a mounted service does: from
/// the position's cursor, read as an index, to the end. For the service test
/// doubles.
#[cfg(test)]
pub(crate) fn serve_fixed(
    entries: &[super::service::ReaddirEntry],
    at: &mut Listing,
    each: &mut dyn FnMut(&tairix_abi::DirEntry<'_>) -> tairix_abi::driver::filesystem::DirVisit,
) {
    use tairix_abi::driver::filesystem::DirVisit;
    if let Some(position) = at.entries() {
        let from = usize::try_from(position.cursor).unwrap_or(usize::MAX);
        for (index, entry) in entries.iter().enumerate().skip(from) {
            if each(&entry.wire()) == DirVisit::Stop {
                return;
            }
            position.cursor = index as u64 + 1;
            position
                .after
                .set(entry.name.as_bytes())
                .expect("a fixture name fits a component");
        }
        at.finish_entries();
    }
    at.finish();
}

#[cfg(test)]
#[path = "listing_tests.rs"]
mod tests;
