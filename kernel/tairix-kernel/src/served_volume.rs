//! The volumes user-space filesystem drivers serve (`plans/FSSERVE.md` §4):
//! the object a volume grant names, through which its driver reads and
//! writes its own partition's blocks and nothing else.
//!
//! Each one owns the volume's window over the shared device — its extent,
//! behind the uncommitted-write journal — so every block the driver writes
//! reaches the journal exactly as an in-kernel driver's does, and an unplug
//! or a re-insert treats it the same way.

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
            disk: SleepLock::new(Disk { window, bounce }),
        })
    }

    /// The id its grant names.
    pub(crate) const fn id(&self) -> u64 {
        self.id
    }

    /// Whether its driver may not write it.
    pub(crate) const fn read_only(&self) -> bool {
        self.info.read_only
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

    /// Read `len` bytes of whole blocks from `lba`, handing `deliver` each
    /// chunk at its offset into the transfer.
    pub(crate) fn read(
        &self,
        lba: u64,
        len: usize,
        deliver: &mut ChunkSink<'_>,
    ) -> Result<(), Errno> {
        self.transfer(lba, len, |disk, at, offset, bytes| {
            let chunk = &mut disk.bounce[..bytes];
            disk.window
                .read_blocks(at, chunk)
                .map_err(DriverError::as_errno)?;
            deliver(offset, chunk)
        })
    }

    /// Write `len` bytes of whole blocks to `lba`, `fill` supplying each
    /// chunk at its offset into the transfer.
    pub(crate) fn write(
        &self,
        lba: u64,
        len: usize,
        fill: &mut ChunkSource<'_>,
    ) -> Result<(), Errno> {
        if self.info.read_only {
            return Err(Errno::ReadOnly);
        }
        self.transfer(lba, len, |disk, at, offset, bytes| {
            let chunk = &mut disk.bounce[..bytes];
            fill(offset, chunk)?;
            disk.window
                .write_blocks(at, chunk)
                .map_err(DriverError::as_errno)
        })
    }

    /// Describe, flush or discard the volume.
    pub(crate) fn ctl(&self, ctl: &VolumeCtl) -> Result<Option<VolumeInfo>, Errno> {
        match ctl.op {
            VolumeCtlOp::Info => Ok(Some(self.info)),
            VolumeCtlOp::Flush => self
                .with_disk(|disk| match disk.window.flush() {
                    Ok(()) | Err(DriverError::Unsupported) => Ok(()),
                    Err(err) => Err(err.as_errno()),
                })
                .map(|()| None),
            VolumeCtlOp::Discard => {
                if self.info.read_only {
                    return Err(Errno::ReadOnly);
                }
                self.within(ctl.lba, ctl.blocks)?;
                self.with_disk(|disk| {
                    disk.window
                        .discard(ctl.lba, ctl.blocks)
                        .map_err(DriverError::as_errno)
                })
                .map(|()| None)
            }
        }
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
        mut step: impl FnMut(&mut Disk<W>, u64, usize, usize) -> Result<(), Errno>,
    ) -> Result<(), Errno> {
        let block = self.info.block_size as usize;
        if len == 0 || !len.is_multiple_of(block) {
            return Err(Errno::LengthOutOfRange);
        }
        self.within(lba, (len / block) as u64)?;
        self.with_disk(|disk| {
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

#[cfg(test)]
#[path = "served_volume_tests.rs"]
mod tests;
