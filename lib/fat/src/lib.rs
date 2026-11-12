//! The FAT file system's on-disk format — FAT12, FAT16 and FAT32 — as
//! Microsoft's FAT specification and the VFAT long-name extension define
//! it: the one definition the FAT32 driver, the volume probe and the test
//! images built for them read and write volumes through.
//!
//! - [`boot`] — the boot sector, its parameter block, and the layout of the
//!   volume's regions.
//! - [`table`] — the allocation table's entries, 12, 16 or 28 bits wide.
//! - [`dir`] — short and long directory entries, and long names.
//! - [`name`] — 8.3 names, the aliases long names are given, and labels.
//! - [`stamp`] — a directory entry's creation, access and write stamps.
//! - [`fsinfo`] — FAT32's free-space hint sector.
//! - [`format`] — a new volume's parameters and what its system area holds.
//!
//! Everything is computation over bytes the caller supplies, total over any
//! input: a structure that breaks the format is refused, never trusted.

#![no_std]

pub mod boot;
pub mod dir;
pub mod format;
pub mod fsinfo;
pub mod name;
pub mod stamp;
pub mod table;

/// The little-endian word at `at`, if `bytes` holds it.
fn le16(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

/// The little-endian double word at `at`, if `bytes` holds it.
fn le32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

/// Writes `value` at `at`, if `bytes` holds it.
fn put16(bytes: &mut [u8], at: usize, value: u16) {
    if let Some(slot) = bytes.get_mut(at..at + 2) {
        slot.copy_from_slice(&value.to_le_bytes());
    }
}

/// Writes `value` at `at`, if `bytes` holds it.
fn put32(bytes: &mut [u8], at: usize, value: u32) {
    if let Some(slot) = bytes.get_mut(at..at + 4) {
        slot.copy_from_slice(&value.to_le_bytes());
    }
}
