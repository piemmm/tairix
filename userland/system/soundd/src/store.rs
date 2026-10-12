//! The files the authority reads: the theme store, the machine's sound
//! document and the replacements it names under the authority's own identity,
//! and the files a session hands over open, through an injected seam so the
//! authority is host-tested with no filesystem.

use alloc::vec::Vec;

use tairix_abi::fs::FileId;
use tairix_abi::origin::ProcId;
use tairix_abi::time::Time64;
use tairix_abi::Errno;

/// One version of a file: one rewritten in place, or replaced under its name,
/// reads as another.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct FileIdentity {
    /// The node.
    pub id: FileId,
    /// Its content generation, where the volume keeps one.
    pub content_gen: u64,
    /// Its length in bytes.
    pub len: u64,
    /// When its contents were last written, for a volume that keeps no
    /// generation.
    pub modified: Time64,
}

/// The authority's read of the volume.
pub trait SoundStore {
    /// A file a session handed over, held open and closed when dropped.
    type Handed;

    /// The identity of the regular file at `path`, or `None` when nothing is
    /// there.
    ///
    /// # Errors
    ///
    /// The store's refusal to look.
    fn identify(&mut self, path: &str) -> Result<Option<FileIdentity>, Errno>;

    /// The whole of the file at `path`.
    ///
    /// # Errors
    ///
    /// [`Errno::LengthOutOfRange`] past `max` bytes, [`Errno::Stale`] when it
    /// is no longer the version `expected`, or the store's own refusal.
    fn read(&mut self, path: &str, expected: FileIdentity, max: usize) -> Result<Vec<u8>, Errno>;

    /// Redeem `handle`, a delegation the process instance `grantor` minted to
    /// this one, as an open file read under the grantor's identity.
    ///
    /// # Errors
    ///
    /// [`Errno::NotFound`] for a handle `grantor` did not mint to this
    /// process, or the redemption's own refusal.
    fn redeem(&mut self, handle: u64, grantor: ProcId) -> Result<Self::Handed, Errno>;

    /// The identity of `file`.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] for anything but a regular file, or the store's
    /// refusal to look.
    fn identify_handed(&mut self, file: &Self::Handed) -> Result<FileIdentity, Errno>;

    /// The whole of `file`.
    ///
    /// # Errors
    ///
    /// As [`Self::read`].
    fn read_handed(
        &mut self,
        file: &Self::Handed,
        expected: FileIdentity,
        max: usize,
    ) -> Result<Vec<u8>, Errno>;
}
