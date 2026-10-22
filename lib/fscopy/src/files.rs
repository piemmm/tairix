//! The files a copy reaches by path.

use tairix_abi::fs::SeekRegion;
use tairix_abi::Errno;

use crate::data::{Source, Target};

/// Files reached by path: what a copying tool's filesystem offers a copy,
/// which reads one of them and writes another ([`PathFile`]).
pub trait PathFiles {
    /// Read up to `buf.len()` bytes of the file at `path` from `offset`,
    /// answering how many; `0` only at its end.
    ///
    /// # Errors
    ///
    /// The open's or the read's refusal.
    fn read(&self, path: &str, offset: u64, buf: &mut [u8]) -> Result<usize, Errno>;

    /// The length of the file at `path`.
    ///
    /// # Errors
    ///
    /// The open's or the stat's refusal.
    fn size(&self, path: &str) -> Result<u64, Errno>;

    /// Where the next region of `region`'s kind begins at or after `offset`
    /// in the file at `path`, `None` when none does before its end.
    ///
    /// # Errors
    ///
    /// The open's or the seek's refusal; [`Errno::NotSupported`] where the
    /// backing keeps no word on its holes.
    fn next_region(
        &self,
        path: &str,
        offset: u64,
        region: SeekRegion,
    ) -> Result<Option<u64>, Errno>;

    /// Write every byte of `bytes` to the file at `path` from `offset`.
    ///
    /// # Errors
    ///
    /// The open's or the write's refusal; a partial write is one.
    fn write(&self, path: &str, offset: u64, bytes: &[u8]) -> Result<(), Errno>;

    /// Set the length of the file at `path` to `len`: bytes past what was
    /// written read as zeros.
    ///
    /// # Errors
    ///
    /// The open's or the truncate's refusal.
    fn set_len(&self, path: &str, len: u64) -> Result<(), Errno>;
}

/// One file of a [`PathFiles`], as a copy reads or writes it.
pub struct PathFile<'a, F: ?Sized> {
    files: &'a F,
    path: &'a str,
}

impl<'a, F: ?Sized> PathFile<'a, F> {
    /// The file at `path` among `files`.
    #[must_use]
    pub const fn new(files: &'a F, path: &'a str) -> Self {
        Self { files, path }
    }
}

impl<F: PathFiles + ?Sized> Source for PathFile<'_, F> {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize, Errno> {
        self.files.read(self.path, offset, buf)
    }

    fn next_region(&mut self, offset: u64, region: SeekRegion) -> Result<Option<u64>, Errno> {
        self.files.next_region(self.path, offset, region)
    }

    fn size(&mut self) -> Result<u64, Errno> {
        self.files.size(self.path)
    }
}

impl<F: PathFiles + ?Sized> Target for PathFile<'_, F> {
    fn write_all_at(&mut self, offset: u64, bytes: &[u8]) -> Result<(), Errno> {
        self.files.write(self.path, offset, bytes)
    }

    fn set_len(&mut self, len: u64) -> Result<(), Errno> {
        self.files.set_len(self.path, len)
    }
}
