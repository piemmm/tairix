//! The seam a share's server reaches the folder through: a directory held
//! open, and the files opened from it. The front-end's binary implements it
//! over the directory capability the picker handed it; the tests over an
//! in-memory tree.
//!
//! Every name is one entry of the directory it is asked of, never a path, and
//! is followed through a link where it names one, except where it is removed
//! or renamed, which acts on the entry itself: the capability keeps every link
//! within the folder.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::driver::filesystem::VolumeStats;
use tairix_abi::{Errno, FileId, FileStat, TimesChange};

use super::{Access, Creation};

/// One entry of a directory, as its listing gave it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostEntry {
    /// Its name.
    pub name: String,
    /// The identity it was listed with.
    pub id: FileId,
}

/// A directory of the folder, held open.
pub trait HostDir: Sized {
    /// A file opened from it.
    type File: HostFile;

    /// Every entry, read once in full.
    ///
    /// # Errors
    ///
    /// The listing's refusal.
    fn list(&self) -> Result<Vec<HostEntry>, Errno>;

    /// What the directory itself is.
    ///
    /// # Errors
    ///
    /// The host's refusal.
    fn stat(&self) -> Result<FileStat, Errno>;

    /// What `name` is.
    ///
    /// # Errors
    ///
    /// The host's refusal: [`Errno::NotFound`] where it names nothing.
    fn look(&self, name: &str) -> Result<FileStat, Errno>;

    /// Whether the user may change `name` — its bytes, or a directory's
    /// entries — or, for `None`, this directory's entries.
    fn may_write(&self, name: Option<&str>) -> bool;

    /// The directory `name` names.
    ///
    /// # Errors
    ///
    /// The host's refusal: [`Errno::NotADirectory`] where it names something
    /// else.
    fn enter(&self, name: &str) -> Result<Self, Errno>;

    /// The file `name` names, opened for `access` as `creation` asks.
    ///
    /// # Errors
    ///
    /// The host's refusal: [`Errno::NotFound`] for [`Creation::Existing`]
    /// where it names nothing, [`Errno::AlreadyExists`] for
    /// [`Creation::New`] where it names something.
    fn open(&self, name: &str, access: Access, creation: Creation) -> Result<Self::File, Errno>;

    /// Make the directory `name`.
    ///
    /// # Errors
    ///
    /// The host's refusal.
    fn make_dir(&self, name: &str) -> Result<(), Errno>;

    /// Remove the entry `name`: a file, or where `directory` an empty
    /// directory.
    ///
    /// # Errors
    ///
    /// The host's refusal: [`Errno::NotEmpty`] for a directory holding
    /// entries.
    fn remove(&self, name: &str, directory: bool) -> Result<(), Errno>;

    /// Give the entry `from` the name `to` in `to_dir`, which nothing holds.
    ///
    /// # Errors
    ///
    /// The host's refusal: [`Errno::AlreadyExists`] where `to` is taken.
    fn rename(&self, from: &str, to_dir: &Self, to: &str) -> Result<(), Errno>;

    /// The space of the volume the directory lies on.
    ///
    /// # Errors
    ///
    /// The host's refusal.
    fn space(&self) -> Result<VolumeStats, Errno>;
}

/// A file of the folder, held open.
pub trait HostFile {
    /// Read from `offset` into `buf`; the bytes read, `0` at the end.
    ///
    /// # Errors
    ///
    /// The host's refusal.
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize, Errno>;

    /// Write `bytes` at `offset`; the bytes written, fewer only where the
    /// volume filled.
    ///
    /// # Errors
    ///
    /// The host's refusal: [`Errno::NoSpace`] where nothing could be written.
    fn write_at(&mut self, offset: u64, bytes: &[u8]) -> Result<usize, Errno>;

    /// Cut or extend it to `len` bytes.
    ///
    /// # Errors
    ///
    /// The host's refusal.
    fn set_len(&mut self, len: u64) -> Result<(), Errno>;

    /// What it is now.
    ///
    /// # Errors
    ///
    /// The host's refusal.
    fn stat(&self) -> Result<FileStat, Errno>;

    /// Change its times as `change` asks.
    ///
    /// # Errors
    ///
    /// The host's refusal.
    fn set_times(&self, change: &TimesChange) -> Result<(), Errno>;

    /// Put what was written to it on its medium.
    ///
    /// # Errors
    ///
    /// The host's refusal.
    fn sync(&self) -> Result<(), Errno>;
}
