//! RISC OS `FileCore`'s disc format: the disc record and boot block, the old
//! map and the new map's zones and fragments, the fixed and big directories,
//! a new disc's layout, and a disc's objects and directories over them — the
//! one definition the ADFS driver and TAIRiX Bridge's `FileCore` share.
//!
//! - [`record`]: the disc record, its geometry, the boot block, the
//!   checksums, and the old map's checks.
//! - [`oldmap`], [`newmap`]: allocation in each map.
//! - [`dir`], [`bigdir`]: the fixed and the big directories.
//! - [`format`]: a new disc's layout.
//! - [`identify`]: where a disc's structures lie.
//! - [`volume`]: a disc's objects and directories.
//!
//! Every access reaches the disc through [`Disc`], whose failures pass
//! through untouched as [`Error::Disc`]; what the format refuses is a
//! [`Fault`], one for each condition RISC OS reports apart.
//!
//! References: the RISC OS PRM's `FileCore` chapter; J.G. Harston, "Acorn
//! 8-Bit ADFS Filesystem Structure" (mdfs.net).

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

pub mod bigdir;
mod bytes;
pub mod dir;
pub mod format;
pub mod identify;
pub mod newmap;
pub mod oldmap;
pub mod record;
pub mod volume;

use tairix_abi::driver::block::Block;
use tairix_abi::DriverError;
use tairix_bytedev::ByteDevice;

/// What the format refuses.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Fault {
    /// No free space for what was asked.
    DiscFull,
    /// The old map's table of free areas has no room for another: the disc
    /// must be compacted first.
    Compaction,
    /// The new map has no object id free.
    MapFull,
    /// A directory holds as many entries as its format allows.
    DirectoryFull,
    /// An object would pass the 4 GiB an entry records.
    TooLarge,
    /// A name longer than its directory's format holds.
    NameTooLong,
    /// A name holding a character its directory's format cannot.
    BadName,
    /// An entry of the name exists.
    Exists,
    /// A disc address naming no fragment the map holds.
    NoFragment,
    /// A structure that fails its checks.
    Broken(Structure),
    /// A disc or a layout the format cannot describe.
    Unsupported,
    /// The host had no memory for the work.
    NoMemory,
}

/// The structure a [`Fault::Broken`] names.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Structure {
    /// The disc record.
    Record,
    /// The old map, or a zone of the new map.
    Map,
    /// A directory.
    Directory,
    /// An object, read past what its map allocates.
    Object,
}

/// A failure on a disc: what the format refused, or what the disc failed
/// with.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Error<E> {
    /// The format refused.
    Fault(Fault),
    /// The disc failed.
    Disc(E),
}

impl<E> From<Fault> for Error<E> {
    fn from(fault: Fault) -> Self {
        Self::Fault(fault)
    }
}

/// The bytes a disc holds, from 0 to its capacity.
pub trait Disc {
    /// What an access fails with.
    type Error;

    /// The bytes it holds.
    fn capacity(&self) -> u64;

    /// `out.len()` bytes from `at`.
    ///
    /// # Errors
    ///
    /// The disc's failure.
    fn read(&mut self, at: u64, out: &mut [u8]) -> core::result::Result<(), Self::Error>;

    /// `bytes` written at `at`.
    ///
    /// # Errors
    ///
    /// The disc's failure.
    fn write(&mut self, at: u64, bytes: &[u8]) -> core::result::Result<(), Self::Error>;

    /// `length` zero bytes written at `at`.
    ///
    /// # Errors
    ///
    /// The disc's failure.
    fn zero(&mut self, at: u64, length: u64) -> core::result::Result<(), Self::Error>;

    /// What has been written made durable.
    ///
    /// # Errors
    ///
    /// The disc's failure.
    fn flush(&mut self) -> core::result::Result<(), Self::Error>;
}

/// The result of an operation on `D`.
pub type Result<T, D> = core::result::Result<T, Error<<D as Disc>::Error>>;

/// A disc read and written as a byte range over a TAIRiX block device.
impl<B: Block> Disc for ByteDevice<B> {
    type Error = DriverError;

    fn capacity(&self) -> u64 {
        ByteDevice::capacity(self)
    }

    fn read(&mut self, at: u64, out: &mut [u8]) -> core::result::Result<(), DriverError> {
        ByteDevice::read(self, at, out)
    }

    fn write(&mut self, at: u64, bytes: &[u8]) -> core::result::Result<(), DriverError> {
        ByteDevice::write(self, at, bytes)
    }

    fn zero(&mut self, at: u64, length: u64) -> core::result::Result<(), DriverError> {
        ByteDevice::zero(self, at, length)
    }

    fn flush(&mut self) -> core::result::Result<(), DriverError> {
        ByteDevice::flush(self)
    }
}

/// A refusal as a TAIRiX filesystem driver reports it.
impl From<Fault> for DriverError {
    fn from(fault: Fault) -> Self {
        match fault {
            Fault::DiscFull
            | Fault::Compaction
            | Fault::MapFull
            | Fault::DirectoryFull
            | Fault::TooLarge => Self::NoSpace,
            Fault::NameTooLong => Self::LengthOutOfRange,
            Fault::BadName => Self::OutOfRange,
            Fault::Exists => Self::AlreadyExists,
            Fault::NoFragment => Self::NotFound,
            Fault::Broken(_) => Self::BadMagic,
            Fault::Unsupported => Self::Unsupported,
            Fault::NoMemory => Self::OutOfMemory,
        }
    }
}

impl From<Error<DriverError>> for DriverError {
    fn from(error: Error<DriverError>) -> Self {
        match error {
            Error::Fault(fault) => fault.into(),
            Error::Disc(error) => error,
        }
    }
}

#[cfg(test)]
mod testkit;
#[cfg(test)]
mod tests;
