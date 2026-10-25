//! Where a directory listing stands between `fs_readdir` batches.
//!
//! A listing is the directory's own entries, read through the driver's
//! resumable cursor, and then, for a projection's floor, the projections
//! attached beneath it that the volume holds no entry for, in name order.
//! [`Listing`] records which of the two it is in and how far it has got; the
//! open description it belongs to holds the directory, pinned, so a later
//! batch can only read the directory the first one did.

use tairix_util::secret::{wipe, Wiped};

use super::path::MAX_COMPONENT_LEN;
use super::VfsError;

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

/// A place among one directory's own entries: the driver cursor the next
/// entry is read at, and the name of the entry that cursor follows.
#[derive(Clone, Debug, Default)]
pub struct DirPosition {
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
    phase: Phase,
    at: DirPosition,
}

impl Listing {
    /// Back to the first entry.
    pub fn restart(&mut self) {
        *self = Self::default();
    }

    /// Whether every entry and mount point has been handed over.
    #[must_use]
    pub fn is_done(&self) -> bool {
        self.phase == Phase::Done
    }

    /// The position among the directory's own entries, while they remain.
    pub fn entries(&mut self) -> Option<&mut DirPosition> {
        (self.phase == Phase::Entries).then_some(&mut self.at)
    }

    /// The directory's own entries are exhausted: the mount points follow.
    pub fn finish_entries(&mut self) {
        self.phase = Phase::Mounts;
        self.at = DirPosition::default();
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
