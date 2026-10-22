//! The shared copy engine's seams (`lib/fscopy`) over the running system's
//! files, their extended attributes, and the directories a tree copy must
//! not land in.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::fs::{OpenFlags, SeekRegion};
use tairix_abi::{Errno, FileId, RealpathMode};
use tairix_fscopy::{Ancestry, AttributeStore, PathFiles, Source, Target};

use crate::File;

/// The system's own resolution of a path and identity of a node.
pub struct RtAncestry;

impl Ancestry for RtAncestry {
    fn canonical(&self, path: &str) -> Result<String, Errno> {
        crate::realpath(path, RealpathMode::Final)
    }

    fn identity(&self, path: &str) -> Result<FileId, Errno> {
        // A resolve-only open: no read authority is asked for, and the handle
        // closes on drop.
        let file =
            File::open(path.as_bytes(), OpenFlags::NO_FOLLOW).map_err(Errno::from_syscall)?;
        Ok(file.stat().map_err(Errno::from_syscall)?.id)
    }
}

/// The extended attributes of the system's filesystem, by path.
pub struct RtAttributes;

impl AttributeStore for RtAttributes {
    fn attr_keys(&self, path: &str) -> Result<Vec<String>, Errno> {
        crate::fs_attr_keys(path.as_bytes()).map_err(Errno::from_syscall)
    }

    fn attr_value(&self, path: &str, key: &str) -> Result<Vec<u8>, Errno> {
        crate::fs_attr_value(path.as_bytes(), key.as_bytes()).map_err(Errno::from_syscall)
    }

    fn attr_set(&self, path: &str, key: &str, value: &[u8]) -> Result<(), Errno> {
        match crate::fs_attr_set(path.as_bytes(), key.as_bytes(), value) {
            0 => Ok(()),
            ret => Err(Errno::from_syscall(ret)),
        }
    }
}

/// An open file a copy reads or writes.
pub struct RtFile<'a>(pub &'a File);

impl Source for RtFile<'_> {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize, Errno> {
        self.0.read_at(offset, buf).map_err(Errno::from_syscall)
    }

    fn next_region(&mut self, offset: u64, region: SeekRegion) -> Result<Option<u64>, Errno> {
        self.0
            .seek_region(offset, region)
            .map_err(Errno::from_syscall)
    }

    fn size(&mut self) -> Result<u64, Errno> {
        self.0
            .stat()
            .map(|stat| stat.size)
            .map_err(Errno::from_syscall)
    }
}

impl Target for RtFile<'_> {
    fn write_all_at(&mut self, offset: u64, bytes: &[u8]) -> Result<(), Errno> {
        let mut written = 0usize;
        while written < bytes.len() {
            let n = self
                .0
                .write_at(offset + written as u64, &bytes[written..])
                .map_err(Errno::from_syscall)?;
            if n == 0 {
                // A write taking nothing would never finish.
                return Err(Errno::LengthOutOfRange);
            }
            written += n;
        }
        Ok(())
    }

    fn set_len(&mut self, len: u64) -> Result<(), Errno> {
        self.0.truncate(len).map_err(Errno::from_syscall)
    }
}

/// The files a copying tool reads and writes by path, kept open between
/// calls — one source and one destination — so a file copied a chunk at a
/// time is opened once rather than once a chunk.
#[derive(Default)]
pub struct OpenFiles {
    reader: core::cell::RefCell<Option<(String, File)>>,
    writer: core::cell::RefCell<Option<(String, File)>>,
}

impl PathFiles for OpenFiles {
    fn read(&self, path: &str, offset: u64, buf: &mut [u8]) -> Result<usize, Errno> {
        Self::with(&self.reader, path, OpenFlags::READ, |file| {
            RtFile(file).read_at(offset, buf)
        })
    }

    fn size(&self, path: &str) -> Result<u64, Errno> {
        Self::with(&self.reader, path, OpenFlags::READ, |file| {
            RtFile(file).size()
        })
    }

    fn next_region(
        &self,
        path: &str,
        offset: u64,
        region: SeekRegion,
    ) -> Result<Option<u64>, Errno> {
        Self::with(&self.reader, path, OpenFlags::READ, |file| {
            RtFile(file).next_region(offset, region)
        })
    }

    fn write(&self, path: &str, offset: u64, bytes: &[u8]) -> Result<(), Errno> {
        Self::with(&self.writer, path, OpenFlags::WRITE, |file| {
            RtFile(file).write_all_at(offset, bytes)
        })
    }

    fn set_len(&self, path: &str, len: u64) -> Result<(), Errno> {
        Self::with(&self.writer, path, OpenFlags::WRITE, |file| {
            RtFile(file).set_len(len)
        })
    }
}

impl OpenFiles {
    /// Close whatever is open on `path`: a name created, renamed or removed
    /// since names another file.
    pub fn forget(&self, path: &str) {
        for slot in [&self.reader, &self.writer] {
            let mut open = slot.borrow_mut();
            if matches!(&*open, Some((name, _)) if name == path) {
                *open = None;
            }
        }
    }

    fn with<R>(
        slot: &core::cell::RefCell<Option<(String, File)>>,
        path: &str,
        flags: OpenFlags,
        op: impl FnOnce(&File) -> Result<R, Errno>,
    ) -> Result<R, Errno> {
        let mut open = slot.borrow_mut();
        if !matches!(&*open, Some((name, _)) if name == path) {
            let file = File::open(path.as_bytes(), flags).map_err(Errno::from_syscall)?;
            *open = Some((String::from(path), file));
        }
        match &*open {
            Some((_, file)) => op(file),
            // The handle was installed just above.
            None => Err(Errno::NotFound),
        }
    }
}
