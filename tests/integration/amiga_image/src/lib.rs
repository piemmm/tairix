//! AmigaDOS disks laid out for tests, once for every test that needs one: a
//! Fast File System volume holding one file, which the kernel's image
//! scenarios attach and the disk-image vertical plants, and a disk an Amiga
//! Rigid Disk Block partitions, which the volume probe and the image attach
//! refuse.
//!
//! The volume is laid out by the AmigaDOS driver's own formatter; the Rigid
//! Disk Block is written from its layout, apart from the probe that reads it.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::driver::block::Block;
use tairix_abi::driver::filesystem::{
    FilesystemRead as _, FilesystemWrite as _, NodeKind, NodeSecurity,
};
use tairix_abi::DriverError;
use tairix_drv_fs_affs::{Affs, FormatOptions};
/// The blocks of an 880 KiB double-density floppy, as the probe knows one.
pub use tairix_fsprobe::affs::DD_FLOPPY_BLOCKS as FLOPPY_BLOCKS;
use tairix_fsprobe::affs::{put_be32, seal, DateStamp};

/// The bytes of a block on every disk here.
pub const BLOCK_SIZE: u32 = 512;

/// The longs a Rigid Disk Block's checksum covers.
const RDB_SUMMED_LONGS: u32 = 64;

/// Lay out on `device` a Fast File System volume labelled `label` whose root
/// holds the file `file`, `text` in it, and answer the device.
///
/// # Errors
///
/// The formatter's refusal of the device or the label, or the driver's of the
/// file.
pub fn ffs_volume<B: Block>(
    device: B,
    label: &[u8],
    file: &[u8],
    text: &[u8],
) -> Result<B, DriverError> {
    let options = FormatOptions {
        dos_type: 1,
        block_size: BLOCK_SIZE,
        name: label,
        created: DateStamp::default(),
    };
    let mut fs = Affs::format(device, &options)?;
    let root = fs.root();
    let node = fs.create(
        root,
        file,
        NodeKind::RegularFile,
        &NodeSecurity::new(0o644, 0, 0),
    )?;
    fs.write_all(node, 0, text)?;
    fs.flush()?;
    Ok(fs.into_device())
}

/// A disk of `blocks` blocks whose Rigid Disk Block, the one an Amiga hard
/// disk opens with, lies in block `at`.
///
/// # Panics
///
/// Where block `at` is not on the disk.
#[must_use]
pub fn rdb_disk(blocks: usize, at: usize) -> Vec<u8> {
    let size = BLOCK_SIZE as usize;
    let mut disk = vec![0u8; blocks * size];
    let block = &mut disk[at * size..(at + 1) * size];
    block[..4].copy_from_slice(b"RDSK");
    put_be32(block, 4, RDB_SUMMED_LONGS);
    put_be32(block, 16, BLOCK_SIZE);
    seal(&mut block[..RDB_SUMMED_LONGS as usize * 4], 8);
    disk
}

#[cfg(test)]
mod tests;
