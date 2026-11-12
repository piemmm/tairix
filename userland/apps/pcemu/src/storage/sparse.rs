//! Media that cost only what they hold: a disk that keeps just the sectors
//! with data in them, and a region of a medium — a partition — addressed
//! as a medium of its own.

use super::{BlockDevice, MediaError, SECTOR};
use alloc::boxed::Box;
use alloc::collections::BTreeMap;

/// A medium holding only its sectors with data: one never written, or
/// written as zeros, reads as zeros and takes no memory, so a large disk
/// costs what is on it.
#[derive(Clone, Debug, Default)]
pub struct SparseDisk {
    sectors: u64,
    read_only: bool,
    held: BTreeMap<u64, Box<[u8; SECTOR]>>,
}

impl SparseDisk {
    /// A blank medium of `sectors` sectors.
    #[must_use]
    pub fn new(sectors: u64) -> Self {
        Self {
            sectors,
            read_only: false,
            held: BTreeMap::new(),
        }
    }

    /// The same, writes refused.
    #[must_use]
    pub fn protected(self) -> Self {
        Self {
            read_only: true,
            ..self
        }
    }

    /// The sectors it holds data for.
    #[must_use]
    pub fn held(&self) -> usize {
        self.held.len()
    }

    /// Each sector holding data, in order, with its number: what its owner
    /// keeps of it.
    pub fn sectors_held(&self) -> impl Iterator<Item = (u64, &[u8; SECTOR])> {
        self.held.iter().map(|(&sector, data)| (sector, &**data))
    }

    fn check(&self, sector: u64) -> Result<(), MediaError> {
        if sector < self.sectors {
            Ok(())
        } else {
            Err(MediaError::OutOfRange)
        }
    }
}

impl BlockDevice for SparseDisk {
    fn sectors(&self) -> u64 {
        self.sectors
    }

    fn read_only(&self) -> bool {
        self.read_only
    }

    fn read(&mut self, sector: u64, out: &mut [u8; SECTOR]) -> Result<(), MediaError> {
        self.check(sector)?;
        match self.held.get(&sector) {
            Some(data) => out.copy_from_slice(&**data),
            None => out.fill(0),
        }
        Ok(())
    }

    fn write(&mut self, sector: u64, data: &[u8; SECTOR]) -> Result<(), MediaError> {
        if self.read_only {
            return Err(MediaError::WriteProtected);
        }
        self.check(sector)?;
        if data.iter().all(|&byte| byte == 0) {
            self.held.remove(&sector);
        } else {
            self.held.insert(sector, Box::new(*data));
        }
        Ok(())
    }
}

/// `sectors` sectors of `device` from `first`, addressed from 0: a
/// partition seen as the medium its volume is laid out on.
pub struct Region<'a> {
    /// The medium the region lies on.
    pub device: &'a mut dyn BlockDevice,
    /// Its first sector there.
    pub first: u64,
    /// Its sectors.
    pub sectors: u64,
}

impl Region<'_> {
    fn place(&self, sector: u64) -> Result<u64, MediaError> {
        if sector < self.sectors {
            self.first.checked_add(sector).ok_or(MediaError::OutOfRange)
        } else {
            Err(MediaError::OutOfRange)
        }
    }
}

impl BlockDevice for Region<'_> {
    fn sectors(&self) -> u64 {
        self.sectors
    }

    fn read_only(&self) -> bool {
        self.device.read_only()
    }

    fn read(&mut self, sector: u64, out: &mut [u8; SECTOR]) -> Result<(), MediaError> {
        let at = self.place(sector)?;
        self.device.read(at, out)
    }

    fn write(&mut self, sector: u64, data: &[u8; SECTOR]) -> Result<(), MediaError> {
        let at = self.place(sector)?;
        self.device.write(at, data)
    }
}

#[cfg(test)]
#[path = "sparse_tests.rs"]
mod tests;
