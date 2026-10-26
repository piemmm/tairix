//! The volume a served driver reaches through its grant, as the block device
//! its format is opened over.

use tairix_abi::blkio::BlkMedium;
use tairix_abi::driver::block::{Block, BlockGeometry, DiscLayout, DiscardCapability};
use tairix_abi::driver::BufferClass;
use tairix_abi::volume::{VolumeCtl, VolumeCtlOp, VolumeInfo, VOLUME_IO_MAX};
use tairix_abi::{DriverError, Errno};

/// The kernel's operations on the one volume a driver's grant names: the
/// `volume_read`, `volume_write` and `volume_ctl` calls.
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

    /// Describe, flush or discard the volume; a description answers
    /// [`Some`].
    ///
    /// # Errors
    ///
    /// The kernel's refusal.
    fn ctl(&self, ctl: &VolumeCtl) -> Result<Option<VolumeInfo>, Errno>;
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
            .ctl(&VolumeCtl {
                op: VolumeCtlOp::Info,
                lba: 0,
                blocks: 0,
            })
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

    fn ctl(&self, op: VolumeCtlOp, lba: u64, blocks: u64) -> Result<(), DriverError> {
        self.access
            .ctl(&VolumeCtl { op, lba, blocks })
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
        self.ctl(VolumeCtlOp::Flush, 0, 0)
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
        self.ctl(VolumeCtlOp::Discard, lba, blocks)
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
