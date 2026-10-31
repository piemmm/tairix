//! The volumes user-space filesystem drivers serve (`plans/FSSERVE.md` §4):
//! the object a volume grant names, through which its driver reads and
//! writes its own partition's blocks and nothing else.
//!
//! Each one owns the volume's window: a device volume's extent of the shared
//! device, behind the uncommitted-write journal, so every block the driver
//! writes reaches the journal exactly as an in-kernel driver's does and an
//! unplug or a re-insert treats it the same way; or the disk-image file an
//! image volume is held in.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

use alloc::sync::Arc;

use tairix_abi::driver::block::{Block, DiscardCapability};
use tairix_abi::volume::{VolumeCtl, VolumeCtlOp, VolumeInfo};
use tairix_abi::{DriverError, Errno};
use tairix_kernel_core::fs::served::ServerActivity;
use tairix_kernel_core::fs::{ChunkSink, ChunkSource};
use tairix_kernel_core::SleepLock;

use crate::volume_service::VolumeWindow;

/// How much of a transfer is staged in the kernel at once: whole blocks of
/// it, copied to or from the driver a chunk at a time.
const BOUNCE_LEN: usize = 64 * 1024;

/// A served volume as its grant reaches it, whatever keeps its blocks: a
/// device's extent or a disk-image file.
pub(crate) trait Granted: Send + Sync {
    /// The id its grant names.
    fn id(&self) -> u64;

    /// Read `len` bytes of whole blocks from `lba`, handing `deliver` each
    /// chunk at its offset into the transfer.
    fn read(&self, lba: u64, len: usize, deliver: &mut ChunkSink<'_>) -> Result<(), Errno>;

    /// Write `len` bytes of whole blocks to `lba`, `fill` supplying each
    /// chunk at its offset into the transfer.
    fn write(&self, lba: u64, len: usize, fill: &mut ChunkSource<'_>) -> Result<(), Errno>;

    /// Describe, flush or discard the volume.
    fn ctl(&self, ctl: &VolumeCtl) -> Result<Option<VolumeInfo>, Errno>;
}

/// The volume's window and the buffer its transfers are staged in.
struct Disk<W> {
    window: W,
    bounce: Vec<u8>,
}

/// One volume a driver serves.
///
/// Its window goes with its last holder: the registry lets go as the volume
/// leaves service, and a transfer under way then holds its own reference
/// until it ends.
pub(crate) struct ServedVolume<W = VolumeWindow> {
    id: u64,
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

impl<W: Block> ServedVolume<W> {
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

    /// Refuse a range of `blocks` from `lba` that runs past the volume.
    fn within(&self, lba: u64, blocks: u64) -> Result<(), Errno> {
        match lba.checked_add(blocks) {
            Some(end) if blocks > 0 && end <= self.info.block_count => Ok(()),
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
        self.within(lba, (len / block) as u64)?;
        self.with_disk(|disk| {
            // Judged under the disk, so a refusal of writes settles every
            // transfer either side of it.
            if writes && self.read_only() {
                return Err(Errno::ReadOnly);
            }
            let mut offset = 0;
            while offset < len {
                let bytes = self.chunk.min(len - offset);
                step(disk, lba + (offset / block) as u64, offset, bytes)?;
                offset += bytes;
            }
            Ok(())
        })
    }
}

impl<W: Block + Send> Granted for ServedVolume<W> {
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
        match ctl.op {
            VolumeCtlOp::Info => Ok(Some(VolumeInfo {
                read_only: self.read_only(),
                ..self.info
            })),
            VolumeCtlOp::Flush => self
                .with_disk(|disk| match disk.window.flush() {
                    Ok(()) | Err(DriverError::Unsupported) => Ok(()),
                    Err(err) => Err(err.as_errno()),
                })
                .map(|()| None),
            VolumeCtlOp::Discard => {
                self.within(ctl.lba, ctl.blocks)?;
                self.with_disk(|disk| {
                    if self.read_only() {
                        return Err(Errno::ReadOnly);
                    }
                    disk.window
                        .discard(ctl.lba, ctl.blocks)
                        .map_err(DriverError::as_errno)
                })
                .map(|()| None)
            }
        }
    }
}

#[cfg(test)]
#[path = "served_volume_tests.rs"]
mod tests;
