//! The volume a served driver reaches through its grant, as the block device
//! its format is opened over, and for an archive the regions it reads and
//! writes by byte.

use tairix_abi::blkio::BlkMedium;
use tairix_abi::driver::block::{Block, BlockGeometry, DiscLayout, DiscardCapability};
use tairix_abi::driver::BufferClass;
use tairix_abi::volume::{VolumeCtl, VolumeInfo, VOLUME_IO_MAX};
use tairix_abi::{DriverError, Errno};

/// The kernel's operations on the one volume a driver's grant names: the
/// `volume_read`, `volume_write`, `volume_ctl`, `archive_read` and
/// `archive_write` calls.
pub trait VolumeAccess {
    /// Read whole blocks from `lba` into `buf`, at most [`VOLUME_IO_MAX`]
    /// bytes of them.
    ///
    /// # Errors
    ///
    /// The kernel's refusal.
    fn read(&self, lba: u64, buf: &mut [u8]) -> Result<(), Errno>;

    /// Write whole blocks from `buf` to `lba`, at most [`VOLUME_IO_MAX`]
    /// bytes of them.
    ///
    /// # Errors
    ///
    /// The kernel's refusal.
    fn write(&self, lba: u64, buf: &[u8]) -> Result<(), Errno>;

    /// Describe, flush, discard, copy within, truncate or commit the volume;
    /// a description answers [`Some`].
    ///
    /// # Errors
    ///
    /// The kernel's refusal.
    fn ctl(&self, ctl: &VolumeCtl) -> Result<Option<VolumeInfo>, Errno>;

    /// Read up to `buf.len()` bytes, at most [`VOLUME_IO_MAX`], at `offset`
    /// of an archive attachment's region `region`, answering how many: fewer
    /// only where the region ends.
    ///
    /// # Errors
    ///
    /// The kernel's refusal.
    fn region_read(&self, region: u8, offset: u64, buf: &mut [u8]) -> Result<usize, Errno>;

    /// Write `buf`, at most [`VOLUME_IO_MAX`] bytes, at `offset` of an archive
    /// attachment's work file `region`.
    ///
    /// # Errors
    ///
    /// The kernel's refusal.
    fn region_write(&self, region: u8, offset: u64, buf: &[u8]) -> Result<(), Errno>;
}

/// The granted volume as a block device.
pub struct GrantedVolume<A> {
    access: A,
    info: VolumeInfo,
    /// The most bytes one call moves: whole blocks within [`VOLUME_IO_MAX`].
    chunk: usize,
}

impl<A: VolumeAccess> GrantedVolume<A> {
    /// The volume `access` reaches, as the kernel describes it.
    ///
    /// # Errors
    ///
    /// The kernel's refusal, or [`DriverError::DeviceFault`] for an answer
    /// that is no description.
    pub fn open(access: A) -> Result<Self, DriverError> {
        let info = access
            .ctl(&VolumeCtl::Info)
            .map_err(DriverError::from_errno)?
            .ok_or(DriverError::DeviceFault)?;
        let block = usize::try_from(info.block_size).map_err(|_| DriverError::DeviceFault)?;
        let chunk = VOLUME_IO_MAX - VOLUME_IO_MAX % block;
        if chunk == 0 {
            return Err(DriverError::DeviceFault);
        }
        Ok(Self {
            access,
            info,
            chunk,
        })
    }

    /// What the kernel said the volume is.
    #[must_use]
    pub const fn info(&self) -> &VolumeInfo {
        &self.info
    }

    /// The kernel's calls on the volume, for what a block device does not
    /// say: an archive's regions.
    #[must_use]
    pub fn into_access(self) -> A {
        self.access
    }

    /// Run `op` over `len` bytes from `lba` a call's worth at a time, handing
    /// it each call's first block and byte range.
    fn each_call(
        &self,
        lba: u64,
        len: usize,
        mut op: impl FnMut(u64, core::ops::Range<usize>) -> Result<(), Errno>,
    ) -> Result<(), DriverError> {
        let block = self.info.block_size as usize;
        let mut done = 0;
        while done < len {
            let bytes = self.chunk.min(len - done);
            let at = lba
                .checked_add((done / block) as u64)
                .ok_or(DriverError::LengthOutOfRange)?;
            op(at, done..done + bytes).map_err(DriverError::from_errno)?;
            done += bytes;
        }
        Ok(())
    }

    fn ctl(&self, ctl: &VolumeCtl) -> Result<(), DriverError> {
        self.access
            .ctl(ctl)
            .map(|_| ())
            .map_err(DriverError::from_errno)
    }
}

impl<A: VolumeAccess> Block for GrantedVolume<A> {
    fn geometry(&self) -> Result<BlockGeometry, DriverError> {
        Ok(BlockGeometry {
            block_size: self.info.block_size,
            block_count: self.info.block_count,
        })
    }

    fn read_blocks(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), DriverError> {
        let len = buf.len();
        self.each_call(lba, len, |at, range| self.access.read(at, &mut buf[range]))
    }

    fn write_blocks(&mut self, lba: u64, buf: &[u8]) -> Result<(), DriverError> {
        self.each_call(lba, buf.len(), |at, range| {
            self.access.write(at, &buf[range])
        })
    }

    fn flush(&mut self) -> Result<(), DriverError> {
        self.ctl(&VolumeCtl::Flush)
    }

    // The kernel stages a served volume's transfers in buffers of its own,
    // which a payload of any class but the ordinary one may not be left in.
    fn read_blocks_with_class(
        &mut self,
        lba: u64,
        buf: &mut [u8],
        class: BufferClass,
    ) -> Result<(), DriverError> {
        match class {
            BufferClass::NonSensitive => self.read_blocks(lba, buf),
            _ => Err(DriverError::Unsupported),
        }
    }

    fn write_blocks_with_class(
        &mut self,
        lba: u64,
        buf: &[u8],
        class: BufferClass,
    ) -> Result<(), DriverError> {
        match class {
            BufferClass::NonSensitive => self.write_blocks(lba, buf),
            _ => Err(DriverError::Unsupported),
        }
    }

    fn discard_capability(&self) -> Result<DiscardCapability, DriverError> {
        Ok(self.info.discard)
    }

    fn discard(&mut self, lba: u64, blocks: u64) -> Result<(), DriverError> {
        self.ctl(&VolumeCtl::Discard { lba, blocks })
    }

    fn medium(&self) -> Option<BlkMedium> {
        self.info.medium
    }

    fn disc_layout(&self) -> Option<DiscLayout> {
        self.info.disc_layout
    }
}

#[cfg(test)]
#[path = "volume_tests.rs"]
mod tests;
