//! The volumes user-space filesystem drivers serve (`plans/FSSERVE.md` §4):
//! the object a volume grant names, through which its driver reads and
//! writes its own partition's blocks and nothing else.
//!
//! Each one owns the volume's window: a device volume's extent of the shared
//! device, behind the uncommitted-write journal, so every block the driver
//! writes reaches the journal exactly as an in-kernel driver's does and an
//! unplug or a re-insert treats it the same way; or the disk-image file an
//! image volume is held in; or an archive attachment's file and work files,
//! read and written by byte (`plans/ARCHIVES.md` §2).

use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

use alloc::sync::Arc;

use tairix_abi::driver::block::{Block, DiscardCapability};
use tairix_abi::volume::{ArchiveName, VolumeCtl, VolumeInfo};
use tairix_abi::{DriverError, Errno};
use tairix_kernel_core::fs::served::ServerActivity;
use tairix_kernel_core::fs::{ChunkSink, ChunkSource};
use tairix_kernel_core::SleepLock;

use crate::volume_service::VolumeWindow;

/// How much of a transfer is staged in the kernel at once: whole blocks of
/// it, copied to or from the driver a chunk at a time.
const BOUNCE_LEN: usize = 64 * 1024;

/// A served volume as its grant reaches it, whatever keeps its blocks: a
/// device's extent, a disk-image file, or an archive attachment.
pub(crate) trait Granted: Send + Sync {
    /// The id its grant names.
    fn id(&self) -> u64;

    /// Read `len` bytes of whole blocks from `lba`, handing `deliver` each
    /// chunk at its offset into the transfer.
    fn read(&self, lba: u64, len: usize, deliver: &mut ChunkSink<'_>) -> Result<(), Errno>;

    /// Write `len` bytes of whole blocks to `lba`, `fill` supplying each
    /// chunk at its offset into the transfer.
    fn write(&self, lba: u64, len: usize, fill: &mut ChunkSource<'_>) -> Result<(), Errno>;

    /// Describe, flush, discard, copy within, truncate or commit the volume.
    fn ctl(&self, ctl: &VolumeCtl) -> Result<Option<VolumeInfo>, Errno>;

    /// Read up to `len` bytes at `offset` of an archive attachment's region
    /// `region`, handing `deliver` each chunk at its offset into the
    /// transfer, answering how many it read.
    fn archive_read(
        &self,
        region: u8,
        offset: u64,
        len: usize,
        deliver: &mut ChunkSink<'_>,
    ) -> Result<usize, Errno>;

    /// Write `len` bytes at `offset` of an archive attachment's work file
    /// `region`, `fill` supplying each chunk at its offset into the transfer.
    fn archive_write(
        &self,
        region: u8,
        offset: u64,
        len: usize,
        fill: &mut ChunkSource<'_>,
    ) -> Result<(), Errno>;
}

/// What a served volume's window is beyond its blocks: for an archive
/// attachment, its exact length and its regions, read and written by byte.
/// Every operation a window of whole blocks lacks is refused
/// [`Errno::NotSupported`].
pub(crate) trait Window: Block {
    /// The bytes the volume holds where they are not its whole blocks.
    fn byte_len(&self) -> Option<u64> {
        None
    }

    /// How many work files the attachment has.
    fn work_files(&self) -> u8 {
        0
    }

    /// The name of the file an archive was attached from.
    fn archive_name(&self) -> Option<ArchiveName> {
        None
    }

    /// Read up to `buf.len()` bytes at `offset` of region `region`,
    /// answering how many: fewer only where the region ends.
    fn region_read(&mut self, _region: u8, _offset: u64, _buf: &mut [u8]) -> Result<usize, Errno> {
        Err(Errno::NotSupported)
    }

    /// Write `data` at `offset` of work file `region`.
    fn region_write(&mut self, _region: u8, _offset: u64, _data: &[u8]) -> Result<(), Errno> {
        Err(Errno::NotSupported)
    }

    /// Cut work file `region` to `len` bytes, never past its length.
    fn region_truncate(&mut self, _region: u8, _len: u64) -> Result<(), Errno> {
        Err(Errno::NotSupported)
    }

    /// The length of region `region`.
    fn region_len(&self, _region: u8) -> Result<u64, Errno> {
        Err(Errno::NotSupported)
    }

    /// Make the next-archive work file, cut to `len` bytes, the archive.
    fn commit(&mut self, _len: u64) -> Result<(), Errno> {
        Err(Errno::NotSupported)
    }

    /// Let the volume's work files go uncommitted, where parking is allowed:
    /// as it leaves service, or as a driver serves it in place of one lost.
    fn discard_work(&mut self) {}
}

impl Window for VolumeWindow {}

/// The volume's window and the buffer its transfers are staged in.
struct Disk<W> {
    window: W,
    bounce: Vec<u8>,
}

impl<W: Window> Disk<W> {
    /// Copy `len` bytes of region `from` at `from_offset` to work file `to`
    /// at `to_offset`, through the staging buffer; a range past the source's
    /// end is refused before anything is written.
    fn copy(
        &mut self,
        (from, from_offset): (u8, u64),
        (to, to_offset): (u8, u64),
        len: u64,
    ) -> Result<(), Errno> {
        let end = from_offset
            .checked_add(len)
            .ok_or(Errno::LengthOutOfRange)?;
        to_offset.checked_add(len).ok_or(Errno::LengthOutOfRange)?;
        if end > self.window.region_len(from)? {
            return Err(Errno::LengthOutOfRange);
        }
        let mut done = 0;
        while done < len {
            let step = usize::try_from(len - done).map_or(BOUNCE_LEN, |left| left.min(BOUNCE_LEN));
            let chunk = &mut self.bounce[..step];
            if self.window.region_read(from, from_offset + done, chunk)? != step {
                // The source shrank under the copy.
                return Err(Errno::DeviceFault);
            }
            self.window.region_write(to, to_offset + done, chunk)?;
            done += step as u64;
        }
        Ok(())
    }
}

/// One volume a driver serves.
///
/// Its window goes with its last holder: the registry lets go as the volume
/// leaves service, and a transfer under way then holds its own reference
/// until it ends.
pub(crate) struct ServedVolume<W = VolumeWindow> {
    id: u64,
    /// The block size and what the device can discard, which never change;
    /// the rest of a description is read from the window as it answers.
    info: VolumeInfo,
    /// Whole blocks of the staging buffer: what one step of a transfer moves.
    chunk: usize,
    activity: Arc<ServerActivity>,
    /// The volume left service; every later transfer fails closed.
    retired: AtomicBool,
    /// It takes no more writes: set at construction for a read-only volume,
    /// and only ever set after.
    writes_refused: AtomicBool,
    disk: SleepLock<Disk<W>>,
}

impl<W: Window> ServedVolume<W> {
    /// The volume `id` names, over `window`, refusing writes when
    /// `read_only`.
    ///
    /// # Errors
    ///
    /// The window's geometry error, [`Errno::OutOfRange`] for a block the
    /// staging buffer cannot hold, or [`Errno::OutOfMemory`] if that buffer
    /// cannot be allocated.
    pub(crate) fn new(id: u64, window: W, read_only: bool) -> Result<Self, Errno> {
        let geometry = window.geometry().map_err(DriverError::as_errno)?;
        let block = usize::try_from(geometry.block_size).map_err(|_| Errno::OutOfRange)?;
        if block == 0 || block > BOUNCE_LEN {
            return Err(Errno::OutOfRange);
        }
        let info = VolumeInfo {
            block_size: geometry.block_size,
            block_count: geometry.block_count,
            read_only,
            discard: window
                .discard_capability()
                .unwrap_or_else(|_| DiscardCapability::unsupported()),
            disc_layout: window.disc_layout(),
            medium: window.medium(),
            byte_len: 0,
            work_files: window.work_files(),
            archive_name: window.archive_name(),
        };
        let mut bounce = Vec::new();
        bounce
            .try_reserve_exact(BOUNCE_LEN)
            .map_err(|_| Errno::OutOfMemory)?;
        bounce.resize(BOUNCE_LEN, 0);
        Ok(Self {
            id,
            info,
            chunk: BOUNCE_LEN - BOUNCE_LEN % block,
            activity: Arc::try_new(ServerActivity::default()).map_err(|_| Errno::OutOfMemory)?,
            retired: AtomicBool::new(false),
            writes_refused: AtomicBool::new(read_only),
            disk: SleepLock::new(Disk { window, bounce }),
        })
    }

    /// Whether its driver may not write it.
    pub(crate) fn read_only(&self) -> bool {
        self.writes_refused.load(Ordering::Acquire)
    }

    /// Take no more writes. A transfer already moving completes first, so
    /// none lands once this returns. Parks.
    pub(crate) fn refuse_writes(&self) {
        let _disk = self.disk.lock();
        self.writes_refused.store(true, Ordering::Release);
    }

    /// Whether it has left service.
    pub(crate) fn retired(&self) -> bool {
        self.retired.load(Ordering::Acquire)
    }

    /// What its driver is doing with it, for the proxy to judge the driver
    /// by.
    pub(crate) fn activity(&self) -> Arc<ServerActivity> {
        Arc::clone(&self.activity)
    }

    /// Take the volume out of service: every later transfer fails closed.
    /// Never parks, so the unplug path may call it.
    pub(crate) fn retire(&self) {
        self.retired.store(true, Ordering::Release);
    }

    /// Let its window's work files go uncommitted. Parks on the disk, so only
    /// a path that may park calls it.
    pub(crate) fn discard_work(&self) {
        self.disk.lock().window.discard_work();
    }

    /// The volume as its window describes it now: an archive's commit changes
    /// its length.
    fn described(&self, window: &W) -> Result<VolumeInfo, Errno> {
        let geometry = window.geometry().map_err(DriverError::as_errno)?;
        let whole = u64::from(geometry.block_size)
            .checked_mul(geometry.block_count)
            .ok_or(Errno::OutOfRange)?;
        Ok(VolumeInfo {
            block_count: geometry.block_count,
            read_only: self.read_only(),
            byte_len: window.byte_len().unwrap_or(whole),
            ..self.info
        })
    }

    /// Refuse a range of `blocks` from `lba` that runs past the volume.
    fn within(window: &W, lba: u64, blocks: u64) -> Result<(), Errno> {
        let count = window
            .geometry()
            .map_err(DriverError::as_errno)?
            .block_count;
        match lba.checked_add(blocks) {
            Some(end) if blocks > 0 && end <= count => Ok(()),
            _ => Err(Errno::LengthOutOfRange),
        }
    }

    /// Run `op` against the volume's disk while it is in service, marking
    /// the driver busy meanwhile.
    fn with_disk<T>(&self, op: impl FnOnce(&mut Disk<W>) -> Result<T, Errno>) -> Result<T, Errno> {
        let _busy = self.activity.enter();
        let mut disk = self.disk.lock();
        if self.retired() {
            return Err(Errno::DeviceOffline);
        }
        op(&mut disk)
    }

    /// As [`Self::with_disk`], for an operation that writes: refused once
    /// the volume takes no writes, judged under the disk so a refusal settles
    /// every write either side of it.
    fn with_disk_writing<T>(
        &self,
        op: impl FnOnce(&mut Disk<W>) -> Result<T, Errno>,
    ) -> Result<T, Errno> {
        self.with_disk(|disk| {
            if self.read_only() {
                return Err(Errno::ReadOnly);
            }
            op(disk)
        })
    }

    /// Move `len` bytes of whole blocks from `lba` a staged chunk at a time,
    /// `step` moving each: its first block, its offset into the transfer,
    /// and its length.
    fn transfer(
        &self,
        lba: u64,
        len: usize,
        writes: bool,
        mut step: impl FnMut(&mut Disk<W>, u64, usize, usize) -> Result<(), Errno>,
    ) -> Result<(), Errno> {
        let block = self.info.block_size as usize;
        if len == 0 || !len.is_multiple_of(block) {
            return Err(Errno::LengthOutOfRange);
        }
        let moved = |disk: &mut Disk<W>| {
            Self::within(&disk.window, lba, (len / block) as u64)?;
            let mut offset = 0;
            while offset < len {
                let bytes = self.chunk.min(len - offset);
                step(disk, lba + (offset / block) as u64, offset, bytes)?;
                offset += bytes;
            }
            Ok(())
        };
        if writes {
            self.with_disk_writing(moved)
        } else {
            self.with_disk(moved)
        }
    }
}

impl<W: Window + Send> Granted for ServedVolume<W> {
    fn id(&self) -> u64 {
        self.id
    }

    fn read(&self, lba: u64, len: usize, deliver: &mut ChunkSink<'_>) -> Result<(), Errno> {
        self.transfer(lba, len, false, |disk, at, offset, bytes| {
            let chunk = &mut disk.bounce[..bytes];
            disk.window
                .read_blocks(at, chunk)
                .map_err(DriverError::as_errno)?;
            deliver(offset, chunk)
        })
    }

    fn write(&self, lba: u64, len: usize, fill: &mut ChunkSource<'_>) -> Result<(), Errno> {
        self.transfer(lba, len, true, |disk, at, offset, bytes| {
            let chunk = &mut disk.bounce[..bytes];
            fill(offset, chunk)?;
            disk.window
                .write_blocks(at, chunk)
                .map_err(DriverError::as_errno)
        })
    }

    fn ctl(&self, ctl: &VolumeCtl) -> Result<Option<VolumeInfo>, Errno> {
        match *ctl {
            VolumeCtl::Info => self
                .with_disk(|disk| self.described(&disk.window))
                .map(Some),
            VolumeCtl::Flush => self
                .with_disk(|disk| match disk.window.flush() {
                    Ok(()) | Err(DriverError::Unsupported) => Ok(()),
                    Err(err) => Err(err.as_errno()),
                })
                .map(|()| None),
            VolumeCtl::Discard { lba, blocks } => self
                .with_disk_writing(|disk| {
                    Self::within(&disk.window, lba, blocks)?;
                    disk.window
                        .discard(lba, blocks)
                        .map_err(DriverError::as_errno)
                })
                .map(|()| None),
            VolumeCtl::Copy {
                from,
                from_offset,
                to,
                to_offset,
                len,
            } => self
                .with_disk_writing(|disk| disk.copy((from, from_offset), (to, to_offset), len))
                .map(|()| None),
            VolumeCtl::Truncate { region, len } => self
                .with_disk_writing(|disk| disk.window.region_truncate(region, len))
                .map(|()| None),
            VolumeCtl::Commit { len } => self
                .with_disk_writing(|disk| disk.window.commit(len))
                .map(|()| None),
        }
    }

    fn archive_read(
        &self,
        region: u8,
        offset: u64,
        len: usize,
        deliver: &mut ChunkSink<'_>,
    ) -> Result<usize, Errno> {
        self.with_disk(|disk| {
            let mut done = 0;
            while done < len {
                let ask = BOUNCE_LEN.min(len - done);
                let at = offset
                    .checked_add(done as u64)
                    .ok_or(Errno::LengthOutOfRange)?;
                let chunk = &mut disk.bounce[..ask];
                let got = disk.window.region_read(region, at, chunk)?;
                if got == 0 {
                    break;
                }
                deliver(done, &chunk[..got])?;
                done += got;
                if got < ask {
                    break;
                }
            }
            Ok(done)
        })
    }

    fn archive_write(
        &self,
        region: u8,
        offset: u64,
        len: usize,
        fill: &mut ChunkSource<'_>,
    ) -> Result<(), Errno> {
        offset
            .checked_add(len as u64)
            .ok_or(Errno::LengthOutOfRange)?;
        self.with_disk_writing(|disk| {
            let mut done = 0;
            while done < len {
                let step = BOUNCE_LEN.min(len - done);
                let chunk = &mut disk.bounce[..step];
                fill(done, chunk)?;
                disk.window
                    .region_write(region, offset + done as u64, chunk)?;
                done += step;
            }
            Ok(())
        })
    }
}

#[cfg(test)]
#[path = "served_volume_tests.rs"]
mod tests;
