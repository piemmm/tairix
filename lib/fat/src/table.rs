//! The allocation table: an entry per cluster — 12, 16 or 28 bits — saying
//! whether its cluster is free, bad, the last of its chain, or which cluster
//! follows it.
//!
//! An entry is read and written through the bytes it spans, so a caller
//! holding only a sector or two of the table reaches it as well as one
//! holding the whole table. A FAT12 entry shares a byte with its neighbour,
//! and FAT32's top four bits are reserved; writing an entry keeps both.

use crate::boot::Kind;
use crate::{le16, le32, put16, put32};

/// What an entry says of its cluster.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    /// The cluster is free.
    Free,
    /// The chain continues at this cluster.
    Next(u32),
    /// The cluster is its chain's last.
    End,
    /// The cluster is marked bad, or the entry names no cluster the volume
    /// has.
    Bad,
}

/// The bits an entry holds.
#[must_use]
pub const fn mask(kind: Kind) -> u32 {
    match kind {
        Kind::Fat12 => 0x0FFF,
        Kind::Fat16 => 0xFFFF,
        Kind::Fat32 => 0x0FFF_FFFF,
    }
}

/// The value a chain's last entry is written with.
#[must_use]
pub const fn end(kind: Kind) -> u32 {
    mask(kind)
}

/// The value that marks a cluster bad.
#[must_use]
pub const fn bad(kind: Kind) -> u32 {
    mask(kind) - 8
}

/// The first of the values that end a chain.
const fn end_from(kind: Kind) -> u32 {
    mask(kind) - 7
}

/// What entries 0 and 1 hold on a new volume: the media descriptor, and
/// the end value.
#[must_use]
pub const fn head(kind: Kind, media: u8) -> [u32; 2] {
    [(mask(kind) & !0xFF) | media as u32, end(kind)]
}

/// Where cluster `cluster`'s entry lies in a table: its first byte, and
/// the bytes it spans.
#[must_use]
pub const fn span(kind: Kind, cluster: u32) -> (u64, usize) {
    let cluster = cluster as u64;
    match kind {
        Kind::Fat12 => (cluster + cluster / 2, 2),
        Kind::Fat16 => (cluster * 2, 2),
        Kind::Fat32 => (cluster * 4, 4),
    }
}

/// The entry cluster `cluster` holds, from the bytes its [`span`] covers.
#[must_use]
pub fn read(kind: Kind, cluster: u32, bytes: &[u8]) -> Option<u32> {
    match kind {
        Kind::Fat12 => {
            let pair = u32::from(le16(bytes, 0)?);
            Some(if cluster % 2 == 1 {
                pair >> 4
            } else {
                pair & 0x0FFF
            })
        }
        Kind::Fat16 => le16(bytes, 0).map(u32::from),
        Kind::Fat32 => le32(bytes, 0).map(|value| value & mask(kind)),
    }
}

/// Sets cluster `cluster`'s entry to `value` in the bytes its [`span`]
/// covers, keeping the bits it shares; `false` when `bytes` is too short.
pub fn write(kind: Kind, cluster: u32, bytes: &mut [u8], value: u32) -> bool {
    let value = value & mask(kind);
    match kind {
        Kind::Fat12 => {
            let Some(pair) = le16(bytes, 0) else {
                return false;
            };
            let value = u16::try_from(value).unwrap_or(0);
            let merged = if cluster % 2 == 1 {
                (pair & 0x000F) | (value << 4)
            } else {
                (pair & 0xF000) | value
            };
            put16(bytes, 0, merged);
        }
        Kind::Fat16 => {
            if bytes.len() < 2 {
                return false;
            }
            put16(bytes, 0, u16::try_from(value).unwrap_or(0));
        }
        Kind::Fat32 => {
            let Some(old) = le32(bytes, 0) else {
                return false;
            };
            put32(bytes, 0, (old & !mask(kind)) | value);
        }
    }
    true
}

/// The entry for `cluster` in a whole table `fat`.
#[must_use]
pub fn get(kind: Kind, fat: &[u8], cluster: u32) -> Option<u32> {
    let (at, len) = span(kind, cluster);
    let at = usize::try_from(at).ok()?;
    read(kind, cluster, fat.get(at..at.checked_add(len)?)?)
}

/// Sets the entry for `cluster` in a whole table `fat`; `false` when the
/// table does not reach it.
pub fn set(kind: Kind, fat: &mut [u8], cluster: u32, value: u32) -> bool {
    let (at, len) = span(kind, cluster);
    let Some(bytes) = usize::try_from(at)
        .ok()
        .and_then(|at| fat.get_mut(at..at.checked_add(len)?))
    else {
        return false;
    };
    write(kind, cluster, bytes, value)
}

/// What an entry's `value` says, on a volume whose highest cluster is
/// `max_cluster`.
#[must_use]
pub fn classify(kind: Kind, value: u32, max_cluster: u32) -> Link {
    let value = value & mask(kind);
    match value {
        0 => Link::Free,
        v if v >= end_from(kind) => Link::End,
        v if (2..=max_cluster).contains(&v) && v != bad(kind) => Link::Next(v),
        _ => Link::Bad,
    }
}

#[cfg(test)]
#[path = "table_tests.rs"]
mod tests;
