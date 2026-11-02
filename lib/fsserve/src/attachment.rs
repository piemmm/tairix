//! An archive attachment in memory, as the kernel serves one, for the tests
//! of the drivers that serve archives: the archive, its staging file and its
//! next archive, the copies and commits a driver asks of it, counted.

use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};

use tairix_abi::driver::block::DiscardCapability;
use tairix_abi::volume::{
    ArchiveName, VolumeCtl, VolumeInfo, ARCHIVE_NEXT, ARCHIVE_REGION, ARCHIVE_STAGING,
    ARCHIVE_WORK_FILES,
};
use tairix_abi::Errno;

use crate::VolumeAccess;

/// The block size an attachment describes itself in.
const BLOCK: u64 = 512;

/// What an attachment was asked.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Asked {
    /// Bytes the kernel copied rather than the driver wrote.
    pub copied: u64,
    /// Bytes the driver wrote into a work file.
    pub written: u64,
    /// Commits that made the next archive the archive.
    pub commits: u32,
}

/// One archive attachment: the archive and its two work files.
pub struct MemoryAttachment {
    regions: RefCell<[Vec<u8>; 3]>,
    work_files: u8,
    name: Option<ArchiveName>,
    asked: RefCell<Asked>,
    /// Whether a commit is refused, as a volume out of room refuses one.
    full: Cell<bool>,
}

impl MemoryAttachment {
    /// An attachment of `archive`, attached by `name`, writable where
    /// `writable`.
    #[must_use]
    pub fn new(archive: &[u8], writable: bool, name: &[u8]) -> Rc<Self> {
        Rc::new(Self {
            regions: RefCell::new([archive.to_vec(), Vec::new(), Vec::new()]),
            work_files: if writable { ARCHIVE_WORK_FILES } else { 0 },
            name: ArchiveName::new(name),
            asked: RefCell::new(Asked::default()),
            full: Cell::new(false),
        })
    }

    /// Refuse every commit from now on, leaving the archive as it is, as a
    /// volume with no room for the next archive does.
    pub fn fill(&self) {
        self.full.set(true);
    }

    /// The archive as it now stands.
    #[must_use]
    pub fn archive(&self) -> Vec<u8> {
        self.regions.borrow()[usize::from(ARCHIVE_REGION)].clone()
    }

    /// The staging file's length.
    #[must_use]
    pub fn staging_len(&self) -> usize {
        self.regions.borrow()[usize::from(ARCHIVE_STAGING)].len()
    }

    /// What the attachment has been asked so far.
    #[must_use]
    pub fn asked(&self) -> Asked {
        *self.asked.borrow()
    }

    /// The calls a driver of the attachment makes.
    #[must_use]
    pub fn access(self: &Rc<Self>) -> MemoryAccess {
        MemoryAccess(Rc::clone(self))
    }
}

/// An attachment as its driver's calls reach it.
#[derive(Clone)]
pub struct MemoryAccess(Rc<MemoryAttachment>);

/// `value` as an index into an attachment's bytes, refused where no buffer
/// could reach it.
fn index(value: u64) -> Result<usize, Errno> {
    usize::try_from(value).map_err(|_| Errno::LengthOutOfRange)
}

impl VolumeAccess for MemoryAccess {
    fn read(&self, lba: u64, buf: &mut [u8]) -> Result<(), Errno> {
        let regions = self.0.regions.borrow();
        let at = index(lba.checked_mul(BLOCK).ok_or(Errno::LengthOutOfRange)?)?;
        let archive = &regions[usize::from(ARCHIVE_REGION)];
        for (offset, byte) in buf.iter_mut().enumerate() {
            *byte = archive.get(at + offset).copied().unwrap_or(0);
        }
        Ok(())
    }

    fn write(&self, _lba: u64, _buf: &[u8]) -> Result<(), Errno> {
        Err(Errno::ReadOnly)
    }

    fn ctl(&self, ctl: &VolumeCtl) -> Result<Option<VolumeInfo>, Errno> {
        let mut regions = self.0.regions.borrow_mut();
        let writable = |region: u8| match region {
            ARCHIVE_STAGING | ARCHIVE_NEXT if self.0.work_files > 0 => Ok(usize::from(region)),
            _ => Err(Errno::OutOfRange),
        };
        match *ctl {
            VolumeCtl::Info => {
                let len = regions[usize::from(ARCHIVE_REGION)].len() as u64;
                Ok(Some(VolumeInfo {
                    block_size: 512,
                    block_count: len.div_ceil(BLOCK).max(1),
                    read_only: self.0.work_files == 0,
                    discard: DiscardCapability::unsupported(),
                    disc_layout: None,
                    medium: None,
                    byte_len: len,
                    work_files: self.0.work_files,
                    archive_name: self.0.name,
                }))
            }
            VolumeCtl::Flush => Ok(None),
            VolumeCtl::Discard { .. } => Err(Errno::NotSupported),
            VolumeCtl::Copy {
                from,
                from_offset,
                to,
                to_offset,
                len,
            } => {
                let to = writable(to)?;
                let (from_offset, to_offset, len) =
                    (index(from_offset)?, index(to_offset)?, index(len)?);
                let source = regions
                    .get(usize::from(from))
                    .ok_or(Errno::OutOfRange)?
                    .get(from_offset..from_offset + len)
                    .ok_or(Errno::LengthOutOfRange)?
                    .to_vec();
                let target = &mut regions[to];
                if target.len() < to_offset + len {
                    target.resize(to_offset + len, 0);
                }
                target[to_offset..to_offset + len].copy_from_slice(&source);
                self.0.asked.borrow_mut().copied += len as u64;
                Ok(None)
            }
            VolumeCtl::Truncate { region, len } => {
                let region = writable(region)?;
                regions[region].resize(index(len)?, 0);
                Ok(None)
            }
            VolumeCtl::Commit { len } => {
                writable(ARCHIVE_NEXT)?;
                if self.0.full.get() {
                    return Err(Errno::NoSpace);
                }
                let mut next = core::mem::take(&mut regions[usize::from(ARCHIVE_NEXT)]);
                next.truncate(index(len)?);
                regions[usize::from(ARCHIVE_REGION)] = next;
                self.0.asked.borrow_mut().commits += 1;
                Ok(None)
            }
        }
    }

    fn region_read(&self, region: u8, offset: u64, buf: &mut [u8]) -> Result<usize, Errno> {
        let regions = self.0.regions.borrow();
        let data = regions.get(usize::from(region)).ok_or(Errno::OutOfRange)?;
        let at = index(offset)?.min(data.len());
        let len = (data.len() - at).min(buf.len());
        buf[..len].copy_from_slice(&data[at..at + len]);
        Ok(len)
    }

    fn region_write(&self, region: u8, offset: u64, buf: &[u8]) -> Result<(), Errno> {
        if self.0.work_files == 0 || !matches!(region, ARCHIVE_STAGING | ARCHIVE_NEXT) {
            return Err(Errno::OutOfRange);
        }
        let mut regions = self.0.regions.borrow_mut();
        let data = &mut regions[usize::from(region)];
        let at = index(offset)?;
        if data.len() < at + buf.len() {
            data.resize(at + buf.len(), 0);
        }
        data[at..at + buf.len()].copy_from_slice(buf);
        self.0.asked.borrow_mut().written += buf.len() as u64;
        Ok(())
    }
}
