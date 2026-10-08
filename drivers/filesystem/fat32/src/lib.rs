//! TAIRiX FAT32 filesystem driver (read/write).
//!
//! Reads and writes a FAT32 volume sitting behind any
//! [`tairix_abi::driver::block::Block`] device and exposes it through
//! the versioned [`tairix_abi::driver::filesystem::FilesystemRead`] and
//! [`tairix_abi::driver::filesystem::FilesystemWrite`] surfaces
//! (new behaviour ships as a new trait, never by
//! widening the frozen mount/unmount
//! [`Filesystem`](tairix_abi::driver::filesystem::Filesystem)).
//!
//! FAT32 has no per-inode owner, mode, ACL, or capability gate; those
//! live in the VFS metadata layer that mounts this
//! driver. The driver therefore makes **no** permission decisions
//! (the VFS is the policy point, this is raw structural I/O).
//!
//! # Public surface
//!
//! Per the only public *function* is [`register`].
//! [`Fat32`] is a public *type* the driver host instantiates with
//! [`Fat32::open`]; the host reaches into it only through the
//! [`FilesystemRead`] and [`FilesystemWrite`] traits.
//!
//! # Scope
//!
//! Read and write. Long file names (VFAT) are reconstructed on read:
//! each entry exposes a single name — its long name when a valid,
//! checksum-matching long-name set precedes the 8.3 short entry, and
//! otherwise the short name (so a volume written without long names is
//! still fully readable). When a long name is present the internal 8.3
//! alias is *not* separately resolvable; the long name is the entry's
//! name. Names are returned as UTF-8 — UTF-16LE long names are decoded,
//! and the driver falls back to the short name on any malformed set
//! rather than surfacing a partial name.
//!
//! Writing creates files and directories, extends/overwrites file data
//! (allocating and chaining clusters, zero-filling sparse gaps),
//! truncates (shrinking frees the tail chain, growing zero-extends), and
//! unlinks files and empty directories. Every created entry is written
//! as a VFAT long-name set bound to a generated, directory-unique 8.3
//! short alias (so arbitrary, case-preserving names round-trip), and
//! every FAT mutation is mirrored across all FAT copies. No
//! `unwrap`/`expect`/`panic!` and no `unsafe`.
//!
//! # Capabilities
//!
//! Loading requires
//! [`CapabilityId::DRV_LOAD`](tairix_abi::CapabilityId::DRV_LOAD). The
//! driver runs in user space; it does not request `CAP_DRV_KERNEL`.

#![no_std]
#![forbid(unsafe_op_in_unsafe_fn)]
#![deny(missing_docs)]

use tairix_abi::driver::block::Block;
use tairix_abi::driver::filesystem::{
    DirEntry, DirVisit, FilesystemAttrsProvider, FilesystemRead, FilesystemSecurity,
    FilesystemStats, FilesystemWrite, NameMatching, NodeId, NodeInfo, NodeKind, NodeSecurity,
    NodeTimes, VolumeStats,
};
use tairix_abi::fs::FS_NAME_MAX;
use tairix_abi::time::{CivilTime, Time64};
use tairix_abi::{CapabilityId, DriverError, DriverHandle, DriverHost};

/// Per-driver `DriverHandle` marker returned by [`register`].
const REGISTER_HANDLE_MARKER: u64 = 0x4641_5433_3200_0001; // "FAT32" + index

/// Driver entry point.
///
/// # Errors
///
/// * [`DriverError::PermissionDenied`] if the host did not grant
///   [`CapabilityId::DRV_LOAD`].
///
/// # Capabilities
///
/// Requires [`CapabilityId::DRV_LOAD`].
pub fn register(host: &dyn DriverHost) -> Result<DriverHandle, DriverError> {
    if !host.has_capability(CapabilityId::DRV_LOAD) {
        return Err(DriverError::PermissionDenied);
    }
    DriverHandle::from_raw(REGISTER_HANDLE_MARKER)
}

/// Largest device logical-block size the driver stages through its
/// on-stack scratch buffer. FAT volumes never use a sector larger than
/// 4096 bytes, and no Tier-1 block device exceeds it either.
const MAX_BLOCK_SIZE: u32 = 4096;

/// On-disk directory-entry size, frozen by the FAT specification.
const DIR_ENTRY_LEN: usize = 32;

/// The most 32-byte entries a FAT directory may hold (the specification's
/// 65,536-entry limit). A walk that would read past it is on a corrupt or
/// cyclic chain and fails closed rather than looping; a create that would
/// grow a directory past it is refused.
const MAX_DIR_SLOTS: u32 = 65_536;

/// The alias tails one walk of a directory examines. A search past them,
/// which a thousand live aliases of one stem force, walks again.
const TAIL_WINDOW: u32 = 1024;

/// Attribute byte: the entry describes a subdirectory.
const ATTR_DIRECTORY: u8 = 0x10;
/// Attribute byte: the entry is the volume label, not a file.
const ATTR_VOLUME_ID: u8 = 0x08;
/// Attribute byte value marking a long-file-name fragment (skipped).
const ATTR_LONG_NAME: u8 = 0x0F;

/// First name byte marking the end of the directory.
const END_OF_DIR: u8 = 0x00;
/// First name byte marking a deleted (free) directory entry.
const DELETED_ENTRY: u8 = 0xE5;
/// First name byte `0x05` stands in for a leading `0xE5` (Kanji).
const KANJI_LEAD: u8 = 0x05;

/// 28-bit mask applied to raw FAT32 cluster values.
const FAT32_CLUSTER_MASK: u32 = 0x0FFF_FFFF;
/// Smallest end-of-chain marker; `value >= EOC` terminates a chain.
const FAT32_EOC: u32 = 0x0FFF_FFF8;
/// End-of-chain value written when allocating the last cluster of a
/// chain (the canonical all-ones marker, `>= FAT32_EOC`).
const FAT32_EOC_WRITE: u32 = 0x0FFF_FFFF;
/// The single "bad cluster" sentinel.
const FAT32_BAD: u32 = 0x0FFF_FFF7;

/// Minimum number of data clusters a genuine FAT32 volume carries. Fewer
/// clusters than this is, by the FAT specification, a FAT12/FAT16 volume;
/// [`Fat32::format`] refuses a device too small to reach it.
const MIN_FAT32_CLUSTERS: u64 = 65_525;

/// Maximum number of UTF-16 code units in a long file name, frozen by
/// the VFAT specification.
const MAX_LONG_NAME_UNITS: usize = 255;

/// Number of UTF-16 code-unit slots a single long-name entry carries
/// (5 + 6 + 2).
const LFN_UNITS_PER_ENTRY: usize = 13;

/// Maximum number of long-name entries in a single set. A 255-unit name
/// needs 20 entries (the last is partially filled); a higher sequence
/// number is malformed.
const LFN_MAX_FRAGMENTS: usize = 20;

/// Number of UTF-16 code-unit slots reserved while reassembling a set,
/// covering the partially-filled final fragment.
const LFN_BUFFER_UNITS: usize = LFN_MAX_FRAGMENTS * LFN_UNITS_PER_ENTRY;

/// Maximum number of UTF-8 bytes a reconstructed long name can occupy:
/// every code unit decodes to at most 3 UTF-8 bytes (a surrogate pair
/// spends two units on a single 4-byte sequence, which is fewer bytes
/// per unit, so this bound holds).
const MAX_NAME_BYTES: usize = MAX_LONG_NAME_UNITS * 3;

/// `order` byte bit marking the last logical (first physical) long-name
/// entry of a set.
const LFN_LAST_FLAG: u8 = 0x40;

/// Mask isolating the 1-based sequence number from a long-name `order`
/// byte.
const LFN_SEQUENCE_MASK: u8 = 0x1F;

/// Byte offsets within a long-name entry holding UTF-16 code units.
const LFN_CHAR_OFFSETS: [usize; LFN_UNITS_PER_ENTRY] =
    [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30];

/// `NodeId` bit carrying the directory flag (cluster numbers are 28-bit,
/// so bit 28 is free).
const NODE_DIR_FLAG: u64 = 1 << 28;
/// `NodeId` validity bit, set on every live node so that no live node
/// ever equals [`NodeId::NONE`] (`0`) — an empty file has cluster 0.
const NODE_VALID_FLAG: u64 = 1 << 29;
/// Bit position at which a regular file's size is packed into a
/// `NodeId`.
const NODE_SIZE_SHIFT: u64 = 32;

/// Pack a directory entry's identity into a self-describing [`NodeId`].
fn pack_node(cluster: u32, is_dir: bool, size: u32) -> NodeId {
    let mut raw = u64::from(cluster & FAT32_CLUSTER_MASK) | NODE_VALID_FLAG;
    if is_dir {
        raw |= NODE_DIR_FLAG;
    } else {
        raw |= u64::from(size) << NODE_SIZE_SHIFT;
    }
    NodeId::from_raw(raw)
}

/// First cluster encoded in a [`NodeId`].
fn node_cluster(node: NodeId) -> u32 {
    // The masked value spans at most 28 bits, so it always fits in `u32`.
    u32::try_from(node.raw() & u64::from(FAT32_CLUSTER_MASK)).unwrap_or(0)
}

/// Whether a [`NodeId`] denotes a directory.
fn node_is_dir(node: NodeId) -> bool {
    node.raw() & NODE_DIR_FLAG != 0
}

/// File size encoded in a [`NodeId`] (`0` for directories).
fn node_size(node: NodeId) -> u32 {
    // The high 32 bits of a `u64` always fit in `u32`.
    u32::try_from(node.raw() >> NODE_SIZE_SHIFT).unwrap_or(0)
}

/// Result of following one FAT chain link.
enum ChainStep {
    /// The chain continues at this cluster.
    Next(u32),
    /// The cluster was the last in its chain.
    End,
    /// The link is the reserved "bad cluster" value or otherwise
    /// structurally invalid.
    Bad,
}

/// Classify a raw FAT32 table value as a chain step.
fn classify_chain(value: u32) -> ChainStep {
    let masked = value & FAT32_CLUSTER_MASK;
    if masked >= FAT32_EOC {
        ChainStep::End
    } else if masked == FAT32_BAD || masked < 2 {
        ChainStep::Bad
    } else {
        ChainStep::Next(masked)
    }
}

/// Computed geometry of a validated FAT32 volume, in bytes.
struct Layout {
    bytes_per_cluster: u64,
    fat_start_byte: u64,
    data_start_byte: u64,
    root_cluster: u32,
    /// Size of one FAT, in bytes (each of [`Layout::num_fats`] copies).
    fat_size_bytes: u64,
    /// Number of FAT copies; every FAT mutation is mirrored across all
    /// of them.
    num_fats: u64,
    /// Highest valid data-cluster number. Data clusters are numbered
    /// `2..=max_cluster`; allocation never hands out a number above it.
    max_cluster: u32,
}

/// A single decoded directory entry. `name` holds the file name as
/// UTF-8 bytes — the reconstructed long name when one is present and
/// valid, otherwise the 8.3 short name.
struct ParsedEntry {
    name: [u8; MAX_NAME_BYTES],
    name_len: usize,
    cluster: u32,
    size: u32,
    is_dir: bool,
    /// The entry's timestamps decoded from the short entry's DOS date/time
    /// fields: `created` from the creation date+time, `modified` from the
    /// last-write date+time, `accessed` from the last-access *date* (FAT
    /// stores no access time-of-day, so it is that date at midnight UTC).
    /// `changed` (ctime) is [`Time64::UNIX_EPOCH`]: FAT keeps no
    /// metadata-change stamp. A field that is not a decodable calendar date
    /// is [`Time64::UNIX_EPOCH`] (the documented "no stamp" value — never a
    /// clamped or guessed date).
    times: NodeTimes,
    /// Device byte offset of the 8.3 short entry (the one carrying the
    /// cluster and size); the write path patches metadata here.
    short_offset: u64,
    /// The first slot of this logical entry: its first long-name fragment,
    /// or the short entry when none precede it.
    first: SlotPos,
    /// Number of 32-byte slots this logical entry occupies (long-name
    /// fragments plus the short entry).
    slot_span: u64,
}

/// A slot's place in its directory: the cluster holding it, its byte within
/// that cluster, and its index counted from the directory's first slot.
#[derive(Clone, Copy)]
struct SlotPos {
    cluster: u32,
    intra: u64,
    slot: u64,
}

impl SlotPos {
    /// The first slot of the directory starting at `cluster`.
    const fn at(cluster: u32) -> Self {
        Self {
            cluster,
            intra: 0,
            slot: 0,
        }
    }
}

/// Where a directory seek ended: the directory's first cluster, the index of
/// a cluster in its chain, and that cluster.
#[derive(Clone, Copy)]
struct SeekHint {
    dir: u32,
    index: u64,
    cluster: u32,
}

/// Cursor walking a directory's cluster chain, 32 bytes at a time.
struct DirCursor {
    /// The next slot to read.
    pos: SlotPos,
    /// The device block the last slot came from, so a walk reads each block
    /// once rather than once per slot it holds.
    held: Option<u64>,
    block: [u8; MAX_BLOCK_SIZE as usize],
}

impl DirCursor {
    /// A walk from the first slot of the directory starting at `cluster`.
    fn at(cluster: u32) -> Self {
        Self {
            pos: SlotPos::at(cluster),
            held: None,
            block: [0; MAX_BLOCK_SIZE as usize],
        }
    }
}

/// A FAT32 volume backed by a [`Block`] device.
pub struct Fat32<B: Block> {
    block: B,
    block_size: u32,
    block_count: u64,
    layout: Layout,
    /// Forward search hint for the cluster allocator: the cluster to try
    /// first on the next [`Fat32::alloc_cluster`]. It only moves forward
    /// (wrapping once at `max_cluster`) and is reset downward whenever a
    /// lower-numbered cluster is freed, so the allocator amortises to a
    /// single forward step per allocation while still finding every free
    /// cluster — turning a sequential fill from O(n²) into O(n).
    next_free: u32,
    /// Live count of free data clusters, established by one FAT scan at
    /// open and maintained by the allocator and the chain-free path, so
    /// [`FilesystemStats::stats`] never re-scans the FAT.
    free_clusters: u64,
    /// The volume's stable 16-byte identity, derived from the BPB volume
    /// serial and label (see [`Fat32::volume_identity`]).
    identity: [u8; 16],
    /// Where the last listing batch stopped in its directory's chain, so the
    /// next batch walks on from there rather than from the first cluster:
    /// reading a directory a batch at a time costs one walk of its chain. A
    /// live directory's chain only grows, so the hint holds until a chain is
    /// freed.
    seek: Option<SeekHint>,
}

/// The volume's stable 16-byte identity, derived from the boot sector:
/// the BPB volume serial + label when the extended boot signature declares
/// them, else zeros. The one derivation lives in `lib/fsprobe`
/// ([`tairix_fsprobe::fat32_identity_from_boot`]), which the volume
/// manager's signature probe shares, so a probe-side fingerprint always
/// names the identity this driver publishes.
use tairix_fsprobe::fat32_identity_from_boot as identity_from_boot;

/// One linear FAT scan counting the free data clusters, run once at open;
/// the allocator and the chain-free path maintain the count from there,
/// so space accounting never re-scans the FAT.
fn count_free_clusters<B: Block>(
    block: &mut B,
    block_size: u32,
    block_count: u64,
    fat_start_byte: u64,
    max_cluster: u32,
) -> Result<u64, DriverError> {
    let mut free_clusters = 0u64;
    let mut chunk = [0u8; MAX_BLOCK_SIZE as usize];
    let mut cluster = 2u32;
    while cluster <= max_cluster {
        let remaining = (u64::from(max_cluster) - u64::from(cluster) + 1) * 4;
        let take = remaining.min(chunk.len() as u64);
        let take_usize = usize::try_from(take).map_err(|_| DriverError::DeviceFault)?;
        device_read(
            block,
            block_size,
            block_count,
            fat_start_byte + u64::from(cluster) * 4,
            &mut chunk[..take_usize],
        )?;
        for entry in chunk[..take_usize].as_chunks::<4>().0 {
            if le32(entry, 0) & FAT32_CLUSTER_MASK == 0 {
                free_clusters += 1;
            }
        }
        // `take` is a whole number of 4-byte entries by construction
        // (`MAX_BLOCK_SIZE` and `remaining` are both multiples of 4).
        cluster += u32::try_from(take / 4).map_err(|_| DriverError::DeviceFault)?;
    }
    Ok(free_clusters)
}

/// Read `u16` little-endian from `buf` at `offset`.
fn le16(buf: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([buf[offset], buf[offset + 1]])
}

/// Read `u32` little-endian from `buf` at `offset`.
fn le32(buf: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        buf[offset],
        buf[offset + 1],
        buf[offset + 2],
        buf[offset + 3],
    ])
}

/// Write `value` little-endian into `buf` at `offset`.
fn put_le16(buf: &mut [u8], offset: usize, value: u16) {
    buf[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

/// Write `value` little-endian into `buf` at `offset`.
fn put_le32(buf: &mut [u8], offset: usize, value: u32) {
    buf[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

/// Choose a FAT32 cluster size (in bytes) for a volume of `total_bytes`,
/// mirroring the size thresholds a conventional `mkfs.fat` applies. The
/// result is clamped to a whole number of `bps`-byte sectors in the
/// `1..=128` sectors-per-cluster range the BPB can express.
fn pick_cluster_bytes(total_bytes: u64, bps: u64) -> u64 {
    const MIB: u64 = 1 << 20;
    const GIB: u64 = 1 << 30;
    let target: u64 = if total_bytes <= 64 * MIB {
        512
    } else if total_bytes <= 128 * MIB {
        1024
    } else if total_bytes <= 256 * MIB {
        2048
    } else if total_bytes <= 8 * GIB {
        4096
    } else if total_bytes <= 16 * GIB {
        8192
    } else if total_bytes <= 32 * GIB {
        16384
    } else {
        32768
    };
    target.max(bps).min(bps * 128)
}

/// `FSInfo` sector (BPB offset 48): the format's customary sector 1.
const FSINFO_SECTOR: u16 = 1;

/// Backup boot sector (BPB offset 50): the format's customary sector 6,
/// with the `FSInfo` copy at the following sector.
const BACKUP_BOOT_SECTOR: u16 = 6;

/// Write the reserved-region structures the FAT32 format requires around
/// the finished `boot` sector: the `FSInfo` structure, the backup
/// boot/`FSInfo` pair the BPB points at, and finally the primary boot
/// sector itself.
///
/// This driver derives its free-space accounting from the FAT itself (the
/// open-time scan), so both `FSInfo` hint fields carry the format's
/// documented "unknown" value — deterministic, so images stay
/// bit-reproducible — and are left untouched at runtime.
fn write_reserved_structures<B: Block>(
    block: &mut B,
    block_size: u32,
    block_count: u64,
    boot: &[u8; 512],
) -> Result<(), DriverError> {
    let mut fsinfo = [0u8; 512];
    put_le32(&mut fsinfo, 0, 0x4161_5252); // FSI_LeadSig
    put_le32(&mut fsinfo, 484, 0x6141_7272); // FSI_StrucSig
    put_le32(&mut fsinfo, 488, 0xFFFF_FFFF); // FSI_Free_Count: unknown
    put_le32(&mut fsinfo, 492, 0xFFFF_FFFF); // FSI_Nxt_Free: unknown
    put_le32(&mut fsinfo, 508, 0xAA55_0000); // FSI_TrailSig
    let bps64 = u64::from(block_size);
    for (sector, image) in [
        (u64::from(FSINFO_SECTOR), &fsinfo),
        (u64::from(BACKUP_BOOT_SECTOR), boot),
        (u64::from(BACKUP_BOOT_SECTOR) + 1, &fsinfo),
    ] {
        device_write(block, block_size, block_count, sector * bps64, image)?;
    }
    device_write(block, block_size, block_count, 0, boot)
}

/// Write `len` zero bytes to `block` starting at device byte `offset`,
/// staged through a stack scratch buffer one chunk at a time.
fn write_zeros<B: Block>(
    block: &mut B,
    block_size: u32,
    block_count: u64,
    offset: u64,
    len: u64,
) -> Result<(), DriverError> {
    let zeros = [0u8; MAX_BLOCK_SIZE as usize];
    let mut at = offset;
    let mut remaining = len;
    while remaining > 0 {
        let chunk = remaining.min(zeros.len() as u64);
        let chunk_usize = usize::try_from(chunk).map_err(|_| DriverError::DeviceFault)?;
        device_write(block, block_size, block_count, at, &zeros[..chunk_usize])?;
        at += chunk;
        remaining -= chunk;
    }
    Ok(())
}

/// Number of leading bytes of `field` that are not the ASCII padding
/// space `0x20`, counting from the end.
fn trimmed_len(field: &[u8]) -> usize {
    let mut len = field.len();
    while len > 0 && field[len - 1] == b' ' {
        len -= 1;
    }
    len
}

/// Decode a 32-byte short-name directory entry.
fn parse_short_entry(raw: &[u8; DIR_ENTRY_LEN]) -> ParsedEntry {
    let mut name = [0u8; MAX_NAME_BYTES];
    let mut len = 0;

    let base_len = trimmed_len(&raw[0..8]);
    for (i, &raw_byte) in raw[..8].iter().enumerate().take(base_len) {
        let byte = if i == 0 && raw_byte == KANJI_LEAD {
            DELETED_ENTRY
        } else {
            raw_byte
        };
        name[len] = byte;
        len += 1;
    }

    let ext_len = trimmed_len(&raw[8..11]);
    if ext_len > 0 {
        name[len] = b'.';
        len += 1;
        for &raw_byte in &raw[8..8 + ext_len] {
            name[len] = raw_byte;
            len += 1;
        }
    }

    let cluster = (u32::from(le16(raw, 20)) << 16) | u32::from(le16(raw, 26));
    ParsedEntry {
        name,
        name_len: len,
        cluster,
        size: le32(raw, 28),
        is_dir: raw[11] & ATTR_DIRECTORY != 0,
        times: NodeTimes {
            // Creation date (0x10) + time (0x0E); last-write date (0x18) +
            // time (0x16); last-access date (0x12), no time-of-day. FAT has
            // no metadata-change (ctime) stamp, so `changed` is the epoch.
            created: dos_datetime_to_time64(le16(raw, 16), le16(raw, 14)),
            modified: dos_datetime_to_time64(le16(raw, 24), le16(raw, 22)),
            accessed: dos_datetime_to_time64(le16(raw, 18), 0),
            changed: Time64::UNIX_EPOCH,
        },
        short_offset: 0,
        first: SlotPos::at(0),
        slot_span: 0,
    }
}

/// Decode a DOS (FAT) date/time pair into a [`Time64`], UTC.
///
/// `date` packs `year-1980` (bits 9..16), month `1..=12` (bits 5..9), and
/// day `1..=31` (bits 0..5); `time` packs hours (bits 11..16), minutes
/// (bits 5..11), and two-second units (bits 0..5). FAT keeps no timezone,
/// so the stored local wall time is reported as-is — the format's own
/// declared precision and range limit, not a TAIRiX one. Every decodable
/// pair (1980..=2107) is representable in [`Time64`], so the conversion
/// never truncates; a field combination that is not a real calendar
/// date/time is not decodable and yields [`Time64::UNIX_EPOCH`], the
/// documented "no stamp" value — never a clamped or guessed date.
fn dos_datetime_to_time64(date: u16, time: u16) -> Time64 {
    let civil = CivilTime {
        year: i64::from(date >> 9) + 1980,
        month: u32::from((date >> 5) & 0x0F),
        day: u32::from(date & 0x1F),
        hour: u32::from(time >> 11),
        minute: u32::from((time >> 5) & 0x3F),
        second: u32::from(time & 0x1F) * 2,
    };
    civil.to_time64().unwrap_or(Time64::UNIX_EPOCH)
}

/// VFAT short-name checksum binding a long-name set to its 8.3 entry.
///
/// Computed over the raw 11-byte on-disk short-name field (base +
/// extension, space-padded), exactly as the bytes are stored — the
/// `0x05` Kanji-lead substitution is *not* undone here, because the
/// generating implementation checksums the stored bytes.
fn short_name_checksum(short: &[u8; 11]) -> u8 {
    let mut sum = 0u8;
    for &byte in short {
        sum = sum.rotate_right(1).wrapping_add(byte);
    }
    sum
}

/// Whether `raw` is a long-name fragment of an entry that exists.
fn is_live_long_fragment(raw: &[u8; DIR_ENTRY_LEN]) -> bool {
    raw[0] != END_OF_DIR && raw[0] != DELETED_ENTRY && raw[11] == ATTR_LONG_NAME
}

/// Map a name byte to a valid 8.3 short-name byte: ASCII letters are
/// upper-cased, digits and a small safe set pass through, and everything
/// else (including non-ASCII) becomes `_`.
fn sanitize_short_char(byte: u8) -> u8 {
    const SAFE: &[u8] = b"$%'-_@~`!(){}^#&";
    if byte.is_ascii_alphanumeric() {
        byte.to_ascii_uppercase()
    } else if SAFE.contains(&byte) {
        byte
    } else {
        b'_'
    }
}

/// Split `name` into its base and extension at the last interior `.`
/// (a leading dot is part of the base). The extension excludes the dot.
fn split_name(name: &[u8]) -> (&[u8], &[u8]) {
    let mut dot = None;
    for (i, &b) in name.iter().enumerate() {
        if b == b'.' && i != 0 {
            dot = Some(i);
        }
    }
    match dot {
        Some(i) => (&name[..i], &name[i + 1..]),
        None => (name, &[]),
    }
}

/// Write the decimal form of `value` into `out`, returning its length.
fn u32_to_decimal(value: u32, out: &mut [u8; 7]) -> usize {
    if value == 0 {
        out[0] = b'0';
        return 1;
    }
    let mut tmp = [0u8; 7];
    let mut n = value;
    let mut len = 0;
    while n > 0 {
        tmp[len] = b'0' + u8::try_from(n % 10).unwrap_or(0);
        n /= 10;
        len += 1;
    }
    for i in 0..len {
        out[i] = tmp[len - 1 - i];
    }
    len
}

/// The part of a new entry's 8.3 alias its `~N` tail leaves alone: the
/// sanitised base and extension of its name.
struct AliasStem {
    base: [u8; 8],
    base_len: usize,
    ext: [u8; 3],
}

impl AliasStem {
    fn of(name: &[u8]) -> Self {
        let kept = |&&b: &&u8| b != b' ' && b != b'.';
        let (base_src, ext_src) = split_name(name);
        let mut ext = [b' '; 3];
        for (slot, &b) in ext.iter_mut().zip(ext_src.iter().filter(kept)) {
            *slot = sanitize_short_char(b);
        }
        let mut base = [b'_'; 8];
        let mut base_len = 0;
        for (slot, &b) in base.iter_mut().zip(base_src.iter().filter(kept)) {
            *slot = sanitize_short_char(b);
            base_len += 1;
        }
        Self {
            base,
            base_len: base_len.max(1),
            ext,
        }
    }

    /// The alias carrying `tail`, its base cut short to make room.
    fn alias(&self, tail: u32) -> Option<[u8; 11]> {
        let mut digits = [0u8; 7];
        let digit_len = u32_to_decimal(tail, &mut digits);
        if digit_len > 6 {
            return None;
        }
        let keep = self.base_len.min(7 - digit_len);
        let mut field = [b' '; 11];
        field[..keep].copy_from_slice(&self.base[..keep]);
        field[keep] = b'~';
        field[keep + 1..=keep + digit_len].copy_from_slice(&digits[..digit_len]);
        field[8..].copy_from_slice(&self.ext);
        Some(field)
    }

    /// The tail `raw` holds, when its name is this stem's alias for one.
    fn tail_of(&self, raw: &RawEntry) -> Option<u32> {
        let tilde = raw[..8].iter().rposition(|&b| b == b'~')?;
        let tail = raw[tilde + 1..8]
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .fold(0, |tail, &digit| tail * 10 + u32::from(digit - b'0'));
        (self.alias(tail)? == raw[..11]).then_some(tail)
    }
}

/// Which of [`TAIL_WINDOW`] consecutive alias tails a directory's live
/// entries hold.
struct TailWindow {
    first: u32,
    held: [u64; TAIL_WINDOW as usize / 64],
}

impl TailWindow {
    fn starting_at(first: u32) -> Self {
        Self {
            first,
            held: [0; TAIL_WINDOW as usize / 64],
        }
    }

    fn hold(&mut self, tail: u32) {
        if let Some(index) = tail.checked_sub(self.first).filter(|&i| i < TAIL_WINDOW) {
            self.held[index as usize / 64] |= 1 << (index % 64);
        }
    }

    fn lowest_free(&self) -> Option<u32> {
        self.held
            .iter()
            .zip((self.first..).step_by(64))
            .find_map(|(word, base)| (*word != u64::MAX).then(|| base + word.trailing_ones()))
    }
}

/// A run of free slots a new entry can take.
#[derive(Clone, Copy)]
struct FreeRun {
    start: SlotPos,
    /// The run begins at the directory's end-of-entries mark, which the
    /// writer moves past it.
    at_end: bool,
}

/// Decode UTF-16LE code `units` into UTF-8 `out`, stopping at the first
/// `0x0000` terminator.
///
/// Returns the number of bytes written, or `None` if the units contain
/// an unpaired surrogate, an invalid scalar value, or would overflow
/// `out` (callers fall back to the 8.3 short name on `None`).
fn decode_utf16le(units: &[u16], out: &mut [u8]) -> Option<usize> {
    const HIGH_SURROGATES: core::ops::RangeInclusive<u16> = 0xD800..=0xDBFF;
    const LOW_SURROGATES: core::ops::RangeInclusive<u16> = 0xDC00..=0xDFFF;
    const SURROGATE_BASE: u32 = 0x1_0000;

    let mut written: usize = 0;
    let mut index: usize = 0;
    while index < units.len() {
        let unit = units[index];
        if unit == 0 {
            break;
        }
        let scalar = if HIGH_SURROGATES.contains(&unit) {
            index += 1;
            let low = *units.get(index)?;
            if !LOW_SURROGATES.contains(&low) {
                return None;
            }
            SURROGATE_BASE
                + ((u32::from(unit - *HIGH_SURROGATES.start()) << 10)
                    | u32::from(low - *LOW_SURROGATES.start()))
        } else if LOW_SURROGATES.contains(&unit) {
            return None;
        } else {
            u32::from(unit)
        };
        let decoded = char::from_u32(scalar)?;
        let mut scratch = [0u8; 4];
        let encoded = decoded.encode_utf8(&mut scratch);
        let end = written.checked_add(encoded.len())?;
        if end > out.len() {
            return None;
        }
        out[written..end].copy_from_slice(encoded.as_bytes());
        written = end;
        index += 1;
    }
    Some(written)
}

/// Encode UTF-8 `name` into UTF-16 code `units`, returning the unit
/// count. `None` if `name` is not valid UTF-8 or needs more than
/// [`MAX_LONG_NAME_UNITS`] units.
fn encode_utf16le(name: &[u8], units: &mut [u16; MAX_LONG_NAME_UNITS]) -> Option<usize> {
    let text = core::str::from_utf8(name).ok()?;
    let mut count = 0usize;
    let mut scratch = [0u16; 2];
    for ch in text.chars() {
        for &unit in ch.encode_utf16(&mut scratch).iter() {
            if count >= units.len() {
                return None;
            }
            units[count] = unit;
            count += 1;
        }
    }
    Some(count)
}

/// Reassembles a VFAT long-name set from its physical directory
/// entries, which precede the short entry in reverse sequence order
/// (the entry flagged [`LFN_LAST_FLAG`] appears first).
struct LongName {
    units: [u16; LFN_BUFFER_UNITS],
    total_units: usize,
    next_sequence: u8,
    checksum: u8,
    started: bool,
    valid: bool,
}

impl LongName {
    fn new() -> Self {
        Self {
            units: [0u16; LFN_BUFFER_UNITS],
            total_units: 0,
            next_sequence: 0,
            checksum: 0,
            started: false,
            valid: false,
        }
    }

    fn reset(&mut self) {
        self.started = false;
        self.valid = false;
        self.total_units = 0;
        self.next_sequence = 0;
    }

    /// Absorb one long-name directory entry.
    fn push(&mut self, raw: &[u8; DIR_ENTRY_LEN]) {
        let order = raw[0];
        if order == DELETED_ENTRY {
            self.reset();
            return;
        }
        let sequence = order & LFN_SEQUENCE_MASK;
        let is_last = order & LFN_LAST_FLAG != 0;
        if sequence == 0 || usize::from(sequence) > LFN_MAX_FRAGMENTS {
            self.reset();
            return;
        }

        if is_last {
            self.units = [0u16; LFN_BUFFER_UNITS];
            self.total_units = usize::from(sequence) * LFN_UNITS_PER_ENTRY;
            self.checksum = raw[13];
            self.next_sequence = sequence;
            self.started = true;
            self.valid = true;
        } else if !self.started
            || !self.valid
            || sequence != self.next_sequence
            || raw[13] != self.checksum
        {
            self.valid = false;
            return;
        }

        let base = (usize::from(sequence) - 1) * LFN_UNITS_PER_ENTRY;
        for (slot, &offset) in LFN_CHAR_OFFSETS.iter().enumerate() {
            self.units[base + slot] = le16(raw, offset);
        }
        self.next_sequence = sequence - 1;
    }

    /// Reconstruct the name into `out` if a complete, checksum-matching
    /// set was accumulated for the short entry `short`.
    fn finish(&self, short: &[u8; 11], out: &mut [u8]) -> Option<usize> {
        if !self.started || !self.valid || self.next_sequence != 0 {
            return None;
        }
        if self.checksum != short_name_checksum(short) {
            return None;
        }
        let len = decode_utf16le(&self.units[..self.total_units], out)?;
        if len == 0 {
            return None;
        }
        Some(len)
    }
}

impl<B: Block> Fat32<B> {
    /// Validate the FAT32 boot sector on `block` and bring the volume
    /// online read-only.
    ///
    /// FAT type is identified by the FAT32 boot-sector shape — a zero
    /// 16-bit FAT size and a zero root-entry count — rather than by
    /// re-deriving the cluster-count threshold: a FAT12/FAT16 volume
    /// has non-zero values in those fields and is rejected, so the
    /// distinction is exact for the volumes this driver accepts.
    ///
    /// # Errors
    ///
    /// * [`DriverError::DeviceFault`] if the device geometry is
    ///   degenerate or a block read fails.
    /// * [`DriverError::BadMagic`] if the boot sector is not a valid
    ///   FAT32 BPB (bad signature, non-power-of-two sector/cluster
    ///   size, missing FAT, or non-FAT32 markers).
    ///
    /// # Capabilities
    ///
    /// Caller must already hold the driver's [`DriverHandle`].
    pub fn open(mut block: B) -> Result<Self, DriverError> {
        let geometry = block.geometry()?;
        let block_size = geometry.block_size;
        if block_size == 0 || block_size > MAX_BLOCK_SIZE || !block_size.is_power_of_two() {
            return Err(DriverError::DeviceFault);
        }
        let block_count = geometry.block_count;
        let total_bytes = u64::from(block_size)
            .checked_mul(block_count)
            .ok_or(DriverError::DeviceFault)?;

        let mut boot = [0u8; 512];
        device_read(&mut block, block_size, block_count, 0, &mut boot)?;

        if le16(&boot, 510) != 0xAA55 {
            return Err(DriverError::BadMagic);
        }
        let bytes_per_sector = u32::from(le16(&boot, 11));
        let sectors_per_cluster = u32::from(boot[13]);
        let reserved_sectors = u64::from(le16(&boot, 14));
        let num_fats = u64::from(boot[16]);
        let root_entry_count = le16(&boot, 17);
        let fat_size_16 = le16(&boot, 22);
        let fat_size_32 = u64::from(le32(&boot, 36));
        let root_cluster = le32(&boot, 44);

        let sector_ok = (512..=MAX_BLOCK_SIZE).contains(&bytes_per_sector)
            && bytes_per_sector.is_power_of_two();
        let cluster_ok =
            (1..=128).contains(&sectors_per_cluster) && sectors_per_cluster.is_power_of_two();
        let fat32_markers = root_entry_count == 0 && fat_size_16 == 0;
        if !sector_ok
            || !cluster_ok
            || num_fats < 1
            || !fat32_markers
            || fat_size_32 == 0
            || root_cluster < 2
        {
            return Err(DriverError::BadMagic);
        }

        let fat_start_byte = reserved_sectors
            .checked_mul(u64::from(bytes_per_sector))
            .ok_or(DriverError::BadMagic)?;
        let data_start_sectors = reserved_sectors
            .checked_add(
                num_fats
                    .checked_mul(fat_size_32)
                    .ok_or(DriverError::BadMagic)?,
            )
            .ok_or(DriverError::BadMagic)?;
        let data_start_byte = data_start_sectors
            .checked_mul(u64::from(bytes_per_sector))
            .ok_or(DriverError::BadMagic)?;
        let bytes_per_cluster = u64::from(sectors_per_cluster) * u64::from(bytes_per_sector);
        if data_start_byte >= total_bytes {
            return Err(DriverError::BadMagic);
        }
        let fat_size_bytes = fat_size_32
            .checked_mul(u64::from(bytes_per_sector))
            .ok_or(DriverError::BadMagic)?;
        let data_clusters = (total_bytes - data_start_byte) / bytes_per_cluster;
        // Data clusters are numbered from 2, so the last valid number is
        // `data_clusters + 1`. Clamp to the 28-bit cluster space.
        let max_cluster = u32::try_from((data_clusters + 1).min(u64::from(FAT32_CLUSTER_MASK)))
            .map_err(|_| DriverError::BadMagic)?;
        if max_cluster < 2 || root_cluster > max_cluster {
            return Err(DriverError::BadMagic);
        }

        let identity = identity_from_boot(&boot);
        let free_clusters = count_free_clusters(
            &mut block,
            block_size,
            block_count,
            fat_start_byte,
            max_cluster,
        )?;

        Ok(Self {
            block,
            block_size,
            block_count,
            layout: Layout {
                bytes_per_cluster,
                fat_start_byte,
                data_start_byte,
                root_cluster,
                fat_size_bytes,
                num_fats,
                max_cluster,
            },
            // The first allocatable cluster; the allocator advances it.
            next_free: 2,
            free_clusters,
            identity,
            seek: None,
        })
    }

    /// The volume's stable 16-byte identity.
    ///
    /// FAT32 has no UUID, so this is content-derived: the BPB volume
    /// serial (4 bytes) and label (11 bytes) when the extended boot
    /// signature declares them, zero otherwise, closed by a constant tag
    /// byte so the identity is never the reserved all-zero value. Stable
    /// across re-inserts (the serial and label live on the medium);
    /// honestly weaker than a real UUID, as the format is.
    #[must_use]
    pub fn volume_identity(&self) -> [u8; 16] {
        self.identity
    }

    /// Lay down a fresh, empty FAT32 volume on `block` and bring it
    /// online read-write.
    ///
    /// `serial` is the caller-minted BPB volume serial — the four bytes
    /// FAT32's content-derived identity is built on
    /// ([`Fat32::volume_identity`]). The caller mints it (`mkfs.fat`
    /// derives one from the clock; TAIRiX callers draw from their entropy
    /// or provenance source) because two volumes formatted with one
    /// serial are indistinguishable to the volume forest, so a duplicate
    /// can never mount while its twin is attached. Zero is refused: the
    /// all-zero serial is the "none recorded" value and would collide
    /// every freshly formatted volume with every other.
    ///
    /// The geometry is derived from the device size: a sectors-per-cluster
    /// is chosen (mirroring `mkfs.fat` thresholds) to keep the cluster count inside
    /// the FAT32 range, the two mirrored FATs are sized so every data
    /// cluster is addressable, and the root directory (cluster 2) is
    /// created empty. The boot sector this function writes is then handed
    /// straight to [`Fat32::open`], which is the single source of truth for
    /// the on-disk layout — so a volume `format`
    /// produces is, by construction, one the driver mounts.
    ///
    /// # Errors
    ///
    /// * [`DriverError::BadMagic`] if the device logical-block size is not
    ///   a power of two in `512..=4096` (a valid FAT32 sector size), or if
    ///   the bytes written somehow fail [`Fat32::open`]'s validation.
    /// * [`DriverError::OutOfRange`] if `serial` is zero, if the device is
    ///   too small to host a valid FAT32 volume (fewer than the FAT32
    ///   minimum of 65525 data clusters), or if it has more sectors than
    ///   the 32-bit BPB count addresses.
    /// * [`DriverError::DeviceFault`] if a block read or write fails.
    ///
    /// # Capabilities
    ///
    /// Reached only through the driver's [`DriverHandle`].
    pub fn format(mut block: B, serial: u32) -> Result<Self, DriverError> {
        const RESERVED_SECTORS: u16 = 32;
        const NUM_FATS: u8 = 2;
        const _: () = assert!(BACKUP_BOOT_SECTOR + 1 < RESERVED_SECTORS);

        if serial == 0 {
            return Err(DriverError::OutOfRange);
        }
        let geometry = block.geometry()?;
        let bps = geometry.block_size;
        if !(512..=MAX_BLOCK_SIZE).contains(&bps) || !bps.is_power_of_two() {
            return Err(DriverError::BadMagic);
        }
        let block_count = geometry.block_count;
        let total_sectors = u32::try_from(block_count).map_err(|_| DriverError::OutOfRange)?;
        let bps64 = u64::from(bps);
        let total_bytes = bps64
            .checked_mul(block_count)
            .ok_or(DriverError::OutOfRange)?;

        let cluster_bytes = pick_cluster_bytes(total_bytes, bps64);
        let bytes_per_cluster = cluster_bytes;
        let spc = cluster_bytes / bps64;
        let spc_u8 = u8::try_from(spc).map_err(|_| DriverError::OutOfRange)?;

        let reserved = u64::from(RESERVED_SECTORS);
        let num_fats = u64::from(NUM_FATS);
        let entries_per_fat_sector = bps64 / 4;

        // Grow the FAT until it maps every data cluster that fits after it.
        // `needed` only shrinks as `fat_sectors` grows, so the loop rises
        // monotonically and terminates.
        let mut fat_sectors = 1u64;
        let (clusters, fat_sectors) = loop {
            let metadata = reserved + num_fats * fat_sectors;
            let data_sectors = block_count
                .checked_sub(metadata)
                .ok_or(DriverError::OutOfRange)?;
            let clusters = data_sectors / spc;
            let needed = (clusters + 2).div_ceil(entries_per_fat_sector);
            if needed <= fat_sectors {
                break (clusters, fat_sectors);
            }
            fat_sectors = needed;
        };

        if clusters < MIN_FAT32_CLUSTERS {
            return Err(DriverError::OutOfRange);
        }
        let fat_size_32 = u32::try_from(fat_sectors).map_err(|_| DriverError::OutOfRange)?;

        let fat_start_byte = reserved * bps64;
        let fat_size_bytes = fat_sectors * bps64;
        let data_start_byte = (reserved + num_fats * fat_sectors) * bps64;

        // Boot sector / BIOS parameter block.
        let mut boot = [0u8; 512];
        boot[0] = 0xEB;
        boot[1] = 0x58;
        boot[2] = 0x90;
        boot[3..11].copy_from_slice(b"TAIRIX  ");
        let bps_u16 = u16::try_from(bps).map_err(|_| DriverError::BadMagic)?;
        put_le16(&mut boot, 11, bps_u16);
        boot[13] = spc_u8;
        put_le16(&mut boot, 14, RESERVED_SECTORS);
        boot[16] = NUM_FATS;
        put_le16(&mut boot, 17, 0); // root entry count (0 for FAT32)
        put_le16(&mut boot, 19, 0); // 16-bit total sectors (0 for FAT32)
        boot[21] = 0xF8; // media descriptor (non-removable)
        put_le16(&mut boot, 22, 0); // 16-bit FAT size (0 for FAT32)
        put_le32(&mut boot, 32, total_sectors);
        put_le32(&mut boot, 36, fat_size_32);
        put_le32(&mut boot, 44, 2); // root directory first cluster
        put_le16(&mut boot, 48, FSINFO_SECTOR);
        put_le16(&mut boot, 50, BACKUP_BOOT_SECTOR);
        boot[66] = 0x29; // extended boot signature
        boot[67..71].copy_from_slice(&serial.to_le_bytes());
        boot[71..82].copy_from_slice(b"NO NAME    ");
        boot[82..90].copy_from_slice(b"FAT32   ");
        boot[510] = 0x55;
        boot[511] = 0xAA;
        write_reserved_structures(&mut block, bps, block_count, &boot)?;

        // Mirrored FATs: zero every entry, then plant the two reserved
        // entries and the root directory's end-of-chain marker.
        for fat in 0..num_fats {
            let base = fat_start_byte + fat * fat_size_bytes;
            write_zeros(&mut block, bps, block_count, base, fat_size_bytes)?;
            let mut head = [0u8; 12];
            head[0..4].copy_from_slice(&0x0FFF_FFF8u32.to_le_bytes()); // media | reserved
            head[4..8].copy_from_slice(&FAT32_EOC_WRITE.to_le_bytes()); // entry 1
            head[8..12].copy_from_slice(&FAT32_EOC_WRITE.to_le_bytes()); // root EOC
            device_write(&mut block, bps, block_count, base, &head)?;
        }

        // Empty root directory: a zeroed cluster reads as end-of-directory.
        write_zeros(
            &mut block,
            bps,
            block_count,
            data_start_byte,
            bytes_per_cluster,
        )?;

        Self::open(block)
    }

    /// Consume the driver, returning the underlying block device.
    pub fn into_block(self) -> B {
        self.block
    }

    /// Read an arbitrary byte range from the backing device.
    fn read_bytes(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), DriverError> {
        device_read(
            &mut self.block,
            self.block_size,
            self.block_count,
            offset,
            buf,
        )
    }

    /// Byte offset of cluster `cluster`'s data on the device.
    fn cluster_byte(&self, cluster: u32) -> u64 {
        self.layout.data_start_byte + (u64::from(cluster) - 2) * self.layout.bytes_per_cluster
    }

    /// Follow one link of the FAT chain starting at `cluster`.
    fn next_cluster(&mut self, cluster: u32) -> Result<ChainStep, DriverError> {
        let offset = self.layout.fat_start_byte + u64::from(cluster) * 4;
        let mut raw = [0u8; 4];
        self.read_bytes(offset, &mut raw)?;
        Ok(match classify_chain(u32::from_le_bytes(raw)) {
            // A link past the last data cluster points outside the volume.
            ChainStep::Next(next) if next > self.layout.max_cluster => ChainStep::Bad,
            step => step,
        })
    }

    /// Write an arbitrary byte range to the backing device.
    fn write_bytes(&mut self, offset: u64, buf: &[u8]) -> Result<(), DriverError> {
        device_write(
            &mut self.block,
            self.block_size,
            self.block_count,
            offset,
            buf,
        )
    }

    /// Read the masked 28-bit FAT entry for `cluster` from the first FAT.
    fn fat_entry(&mut self, cluster: u32) -> Result<u32, DriverError> {
        let offset = self.layout.fat_start_byte + u64::from(cluster) * 4;
        let mut raw = [0u8; 4];
        self.read_bytes(offset, &mut raw)?;
        Ok(u32::from_le_bytes(raw) & FAT32_CLUSTER_MASK)
    }

    /// Write the 28-bit `value` into `cluster`'s entry in every FAT copy,
    /// preserving each copy's reserved top 4 bits.
    fn set_fat(&mut self, cluster: u32, value: u32) -> Result<(), DriverError> {
        let value = value & FAT32_CLUSTER_MASK;
        for fat in 0..self.layout.num_fats {
            let offset = self.layout.fat_start_byte
                + fat * self.layout.fat_size_bytes
                + u64::from(cluster) * 4;
            let mut raw = [0u8; 4];
            self.read_bytes(offset, &mut raw)?;
            let reserved = u32::from_le_bytes(raw) & !FAT32_CLUSTER_MASK;
            self.write_bytes(offset, &(reserved | value).to_le_bytes())?;
        }
        Ok(())
    }

    /// Zero the entire data region of `cluster`.
    fn zero_cluster(&mut self, cluster: u32) -> Result<(), DriverError> {
        let zeros = [0u8; MAX_BLOCK_SIZE as usize];
        let mut at = self.cluster_byte(cluster);
        let mut remaining = self.layout.bytes_per_cluster;
        while remaining > 0 {
            let chunk = remaining.min(zeros.len() as u64);
            let chunk_usize = usize::try_from(chunk).map_err(|_| DriverError::DeviceFault)?;
            self.write_bytes(at, &zeros[..chunk_usize])?;
            at += chunk;
            remaining -= chunk;
        }
        Ok(())
    }

    /// Allocate one free data cluster, mark it end-of-chain, optionally
    /// zero it, and return its number. Fails with [`DriverError::NoSpace`]
    /// when no free cluster remains (the volume is full).
    fn alloc_cluster(&mut self, zero: bool) -> Result<u32, DriverError> {
        let max = self.layout.max_cluster;
        // Clusters 2..=max are allocatable; scan from the hint, wrapping
        // once, so no free cluster is ever missed.
        let total = u64::from(max) - 1;
        let mut candidate = self.next_free.clamp(2, max);
        let mut scanned = 0u64;
        while scanned < total {
            if self.fat_entry(candidate)? == 0 {
                self.set_fat(candidate, FAT32_EOC_WRITE)?;
                if zero {
                    self.zero_cluster(candidate)?;
                }
                self.next_free = if candidate >= max { 2 } else { candidate + 1 };
                self.free_clusters = self.free_clusters.saturating_sub(1);
                return Ok(candidate);
            }
            candidate = if candidate >= max { 2 } else { candidate + 1 };
            scanned += 1;
        }
        Err(DriverError::NoSpace)
    }

    /// Free an entire cluster chain starting at `first`.
    fn free_chain(&mut self, first: u32) -> Result<(), DriverError> {
        self.seek = None;
        let mut cluster = first;
        let mut min_freed = u32::MAX;
        while (2..=self.layout.max_cluster).contains(&cluster) {
            let next = self.fat_entry(cluster)?;
            self.set_fat(cluster, 0)?;
            self.free_clusters += 1;
            min_freed = min_freed.min(cluster);
            match classify_chain(next) {
                ChainStep::Next(n) => cluster = n,
                _ => break,
            }
        }
        // Reuse the freed clusters first: rewind the allocator hint to the
        // lowest cluster just released.
        if min_freed != u32::MAX {
            self.next_free = self.next_free.min(min_freed);
        }
        Ok(())
    }

    /// Return the next valid entry at or after `cursor`, advancing the
    /// cursor past it. `Ok(None)` marks end-of-directory.
    ///
    /// The entry's name is the reconstructed VFAT long name when a
    /// valid, checksum-matching long-name set precedes the short entry,
    /// and otherwise the 8.3 short name. Deleted entries, orphaned
    /// long-name fragments, volume labels, and the `.`/`..` self/parent
    /// links are skipped (the VFS resolves `.`/`..` itself).
    fn next_entry(&mut self, cursor: &mut DirCursor) -> Result<Option<ParsedEntry>, DriverError> {
        let mut long = LongName::new();
        let mut run_start: Option<SlotPos> = None;
        while let Some((at, raw)) = self.next_slot(cursor)? {
            let first = raw[0];
            if first == END_OF_DIR {
                return Ok(None);
            }
            if first == DELETED_ENTRY {
                long.reset();
                run_start = None;
                continue;
            }
            let attr = raw[11];
            if attr == ATTR_LONG_NAME {
                run_start.get_or_insert(at);
                long.push(&raw);
                continue;
            }
            if attr & ATTR_VOLUME_ID != 0 || first == b'.' {
                long.reset();
                run_start = None;
                continue;
            }

            let mut entry = parse_short_entry(&raw);
            let mut short = [0u8; 11];
            short.copy_from_slice(&raw[0..11]);
            // A long name past the VFS's name limit (one written by another
            // system in a script whose characters are three UTF-8 bytes) is
            // listed and reached by its short alias, so it neither hides the
            // file nor makes the directory unlistable.
            if let Some(long_len) = long.finish(&short, &mut entry.name[..FS_NAME_MAX]) {
                entry.name_len = long_len;
            }
            entry.short_offset = self.slot_byte(at);
            entry.first = run_start.unwrap_or(at);
            entry.slot_span = at.slot - entry.first.slot + 1;
            return Ok(Some(entry));
        }
        Ok(None)
    }

    /// The slot `walk` stands at, read through its held block, with its
    /// position; `None` once the directory's chain ends.
    fn next_slot(
        &mut self,
        walk: &mut DirCursor,
    ) -> Result<Option<(SlotPos, RawEntry)>, DriverError> {
        // A directory recording no cluster holds no slots, as Linux reads one.
        if walk.pos.cluster < 2 {
            return Ok(None);
        }
        if walk.pos.cluster > self.layout.max_cluster {
            return Err(DriverError::DeviceFault);
        }
        if !self.settle(&mut walk.pos)? {
            return Ok(None);
        }
        if walk.pos.slot >= u64::from(MAX_DIR_SLOTS) {
            return Err(DriverError::DeviceFault);
        }
        let at = walk.pos;
        let raw = self.walk_slot(walk, self.slot_byte(at))?;
        walk.pos.intra += DIR_ENTRY_LEN as u64;
        walk.pos.slot += 1;
        Ok(Some((at, raw)))
    }

    /// Bring `pos` onto a slot of its chain, following the chain once its
    /// cluster's slots are used up; `false` once the chain ends.
    fn settle(&mut self, pos: &mut SlotPos) -> Result<bool, DriverError> {
        if pos.intra < self.layout.bytes_per_cluster {
            return Ok(true);
        }
        match self.next_cluster(pos.cluster)? {
            ChainStep::Next(next) => {
                pos.cluster = next;
                pos.intra = 0;
                Ok(true)
            }
            ChainStep::End => Ok(false),
            ChainStep::Bad => Err(DriverError::DeviceFault),
        }
    }

    /// Device byte offset of the slot at `pos`.
    fn slot_byte(&self, pos: SlotPos) -> u64 {
        self.cluster_byte(pos.cluster) + pos.intra
    }

    /// Write `raw` at `pos` and step past it, chaining a zeroed cluster on
    /// when the directory's chain ends first.
    fn put_slot(&mut self, pos: &mut SlotPos, raw: &RawEntry) -> Result<(), DriverError> {
        if !(2..=self.layout.max_cluster).contains(&pos.cluster) {
            return Err(DriverError::DeviceFault);
        }
        if !self.settle(pos)? {
            let fresh = self.alloc_cluster(true)?;
            self.set_fat(pos.cluster, fresh)?;
            pos.cluster = fresh;
            pos.intra = 0;
        }
        self.write_bytes(self.slot_byte(*pos), raw)?;
        pos.intra += DIR_ENTRY_LEN as u64;
        pos.slot += 1;
        Ok(())
    }

    /// The 32-byte slot at device byte `byte`, read through the walk's held
    /// block. A slot never straddles two blocks: blocks are a multiple of 32
    /// bytes and clusters start on block boundaries.
    fn walk_slot(
        &mut self,
        cursor: &mut DirCursor,
        byte: u64,
    ) -> Result<[u8; DIR_ENTRY_LEN], DriverError> {
        let bs = u64::from(self.block_size);
        let lba = byte / bs;
        if cursor.held != Some(lba) {
            if lba >= self.block_count {
                return Err(DriverError::DeviceFault);
            }
            cursor.held = None;
            self.block
                .read_blocks(lba, &mut cursor.block[..self.block_size as usize])?;
            cursor.held = Some(lba);
        }
        let within = usize::try_from(byte % bs).map_err(|_| DriverError::DeviceFault)?;
        let mut raw = [0u8; DIR_ENTRY_LEN];
        raw.copy_from_slice(
            cursor
                .block
                .get(within..within + DIR_ENTRY_LEN)
                .ok_or(DriverError::DeviceFault)?,
        );
        Ok(raw)
    }

    /// Position `walk`, standing at its directory's first slot, at logical
    /// slot `slot` by following the directory's own chain — from where the
    /// last batch stopped when that is on the way — answering `false` when
    /// the chain ends first.
    fn seek_slot(&mut self, walk: &mut DirCursor, slot: u64) -> Result<bool, DriverError> {
        let per_cluster = self.slots_per_cluster();
        let dir = walk.pos.cluster;
        let target = slot / per_cluster;
        let (mut index, mut cluster) = match self.seek {
            Some(hint) if hint.dir == dir && hint.index <= target => (hint.index, hint.cluster),
            _ => (0, dir),
        };
        while index < target {
            match self.next_cluster(cluster)? {
                ChainStep::Next(next) => cluster = next,
                ChainStep::End => return Ok(false),
                ChainStep::Bad => return Err(DriverError::DeviceFault),
            }
            index += 1;
        }
        // A listing's batches start ever further on, so the next one seeks
        // on from here.
        self.seek = Some(SeekHint {
            dir,
            index,
            cluster,
        });
        walk.pos.cluster = cluster;
        walk.pos.intra = (slot % per_cluster) * DIR_ENTRY_LEN as u64;
        walk.pos.slot = slot;
        Ok(true)
    }

    /// Step `walk`, standing on the slot a batch's last entry took, onto the
    /// next, past any entry whose long name now runs through that slot: it
    /// was written after the listing passed, so it was not present throughout
    /// and is skipped rather than listed by its 8.3 alias. `false` once the
    /// directory ends.
    fn step_past_resume(&mut self, walk: &mut DirCursor) -> Result<bool, DriverError> {
        let Some((_, before)) = self.next_slot(walk)? else {
            return Ok(false);
        };
        if !is_live_long_fragment(&before) {
            return Ok(true);
        }
        let checksum = before[13];
        loop {
            let mark = walk.pos;
            let Some((_, raw)) = self.next_slot(walk)? else {
                return Ok(false);
            };
            if is_live_long_fragment(&raw) {
                continue;
            }
            let mut short = [0u8; 11];
            short.copy_from_slice(&raw[..11]);
            // Anything but the run's own short entry — the end, a free slot,
            // an unrelated entry — is listed as found.
            if raw[0] == END_OF_DIR
                || raw[0] == DELETED_ENTRY
                || short_name_checksum(&short) != checksum
            {
                walk.pos = mark;
            }
            return Ok(true);
        }
    }

    /// The bytes a node's data occupies: its chain, for a directory (whose
    /// entry records no size), and the clusters its size needs, for a file,
    /// as Linux reports it — walking a large file's chain per listed entry
    /// would cost the listing the file's size.
    fn allocation(&mut self, cluster: u32, is_dir: bool, size: u32) -> Result<u64, DriverError> {
        let per_cluster = self.layout.bytes_per_cluster;
        if cluster < 2 {
            Ok(0)
        } else if is_dir {
            Ok(self.chain_len(cluster)?.0 * per_cluster)
        } else {
            Ok(u64::from(size).div_ceil(per_cluster) * per_cluster)
        }
    }

    /// Build the [`NodeId`] for a decoded directory entry.
    fn entry_node(entry: &ParsedEntry) -> NodeId {
        let size = if entry.is_dir { 0 } else { entry.size };
        pack_node(entry.cluster, entry.is_dir, size)
    }
}

/// Read `buf.len()` bytes starting at device byte `offset`, staging
/// through one logical block at a time.
fn device_read<B: Block>(
    block: &mut B,
    block_size: u32,
    block_count: u64,
    offset: u64,
    buf: &mut [u8],
) -> Result<(), DriverError> {
    let bs = u64::from(block_size);
    let bs_usize = block_size as usize;
    let mut scratch = [0u8; MAX_BLOCK_SIZE as usize];
    let mut done: usize = 0;
    while done < buf.len() {
        let cursor = offset + done as u64;
        let lba = cursor / bs;
        let within = usize::try_from(cursor % bs).map_err(|_| DriverError::DeviceFault)?;
        if lba >= block_count {
            return Err(DriverError::DeviceFault);
        }
        block.read_blocks(lba, &mut scratch[..bs_usize])?;
        let take = core::cmp::min(bs_usize - within, buf.len() - done);
        buf[done..done + take].copy_from_slice(&scratch[within..within + take]);
        done += take;
    }
    Ok(())
}

/// Write `buf.len()` bytes starting at device byte `offset`, staging
/// through one logical block at a time.
///
/// A block touched only partially is read-modified-written so the
/// untouched bytes of that block are preserved; a fully covered block is
/// written directly.
fn device_write<B: Block>(
    block: &mut B,
    block_size: u32,
    block_count: u64,
    offset: u64,
    buf: &[u8],
) -> Result<(), DriverError> {
    let bs = u64::from(block_size);
    let bs_usize = block_size as usize;
    let mut scratch = [0u8; MAX_BLOCK_SIZE as usize];
    let mut done: usize = 0;
    while done < buf.len() {
        let cursor = offset + done as u64;
        let lba = cursor / bs;
        let within = usize::try_from(cursor % bs).map_err(|_| DriverError::DeviceFault)?;
        if lba >= block_count {
            return Err(DriverError::DeviceFault);
        }
        let take = core::cmp::min(bs_usize - within, buf.len() - done);
        if within == 0 && take == bs_usize {
            scratch[..bs_usize].copy_from_slice(&buf[done..done + bs_usize]);
        } else {
            block.read_blocks(lba, &mut scratch[..bs_usize])?;
            scratch[within..within + take].copy_from_slice(&buf[done..done + take]);
        }
        block.write_blocks(lba, &scratch[..bs_usize])?;
        done += take;
    }
    Ok(())
}

/// One created directory entry pending write (a long-name fragment or
/// the 8.3 short entry).
type RawEntry = [u8; DIR_ENTRY_LEN];

impl<B: Block> Fat32<B> {
    /// Number of 32-byte directory slots in one cluster.
    fn slots_per_cluster(&self) -> u64 {
        self.layout.bytes_per_cluster / DIR_ENTRY_LEN as u64
    }

    /// Look up child `name` in directory `dir_cluster`, returning its
    /// parsed entry (with on-disk slot/offset metadata) if present.
    fn find_child(
        &mut self,
        dir_cluster: u32,
        name: &[u8],
    ) -> Result<Option<ParsedEntry>, DriverError> {
        let mut cursor = DirCursor::at(dir_cluster);
        while let Some(entry) = self.next_entry(&mut cursor)? {
            if entry.name[..entry.name_len].eq_ignore_ascii_case(name) {
                return Ok(Some(entry));
            }
        }
        Ok(None)
    }

    /// Patch the first-cluster and size fields of the short entry at
    /// `short_offset`.
    fn set_entry_meta(
        &mut self,
        short_offset: u64,
        cluster: u32,
        size: u32,
    ) -> Result<(), DriverError> {
        let cb = cluster.to_le_bytes();
        self.write_bytes(short_offset + 20, &cb[2..4])?;
        self.write_bytes(short_offset + 26, &cb[0..2])?;
        self.write_bytes(short_offset + 28, &size.to_le_bytes())
    }

    /// Choose a new entry's alias, the lowest `~N` tail no live alias holds,
    /// and the first free run of `count` slots for it. One walk of the
    /// directory decides both unless its first window of tails is all held.
    fn place_entry(
        &mut self,
        dir_cluster: u32,
        stem: &AliasStem,
        count: u64,
    ) -> Result<([u8; 11], FreeRun), DriverError> {
        let mut run: Option<FreeRun> = None;
        // A tail for every slot a directory holds: a directory with room for
        // the entry always leaves one of them free.
        let mut first_tail = 1;
        while first_tail <= MAX_DIR_SLOTS {
            let mut held = TailWindow::starting_at(first_tail);
            let mut walk = DirCursor::at(dir_cluster);
            let mut free: Option<(SlotPos, u64)> = None;
            let end = loop {
                let Some((at, raw)) = self.next_slot(&mut walk)? else {
                    break walk.pos;
                };
                if raw[0] == END_OF_DIR {
                    break at;
                }
                if raw[0] == DELETED_ENTRY {
                    let (start, len) = free.get_or_insert((at, 0));
                    *len += 1;
                    if *len == count && run.is_none() {
                        run = Some(FreeRun {
                            start: *start,
                            at_end: false,
                        });
                    }
                    continue;
                }
                free = None;
                if raw[11] != ATTR_LONG_NAME && raw[11] & ATTR_VOLUME_ID == 0 {
                    if let Some(tail) = stem.tail_of(&raw) {
                        held.hold(tail);
                    }
                }
            };
            let run = *run.get_or_insert(FreeRun {
                start: free.map_or(end, |(start, _)| start),
                at_end: true,
            });
            if let Some(tail) = held.lowest_free() {
                return Ok((stem.alias(tail).ok_or(DriverError::NoSpace)?, run));
            }
            first_tail += TAIL_WINDOW;
        }
        Err(DriverError::NoSpace)
    }

    /// Build one long-name fragment for sequence `seq` (1-based), covering
    /// `units[(seq-1)*13 ..]`, flagged last when `is_last`.
    fn build_lfn_entry(units: &[u16], seq: usize, is_last: bool, checksum: u8) -> RawEntry {
        let mut raw = [0u8; DIR_ENTRY_LEN];
        let mut order = u8::try_from(seq).unwrap_or(0);
        if is_last {
            order |= LFN_LAST_FLAG;
        }
        raw[0] = order;
        raw[11] = ATTR_LONG_NAME;
        raw[13] = checksum;
        let base = (seq - 1) * LFN_UNITS_PER_ENTRY;
        for (k, &offset) in LFN_CHAR_OFFSETS.iter().enumerate() {
            let idx = base + k;
            let unit = match idx.cmp(&units.len()) {
                core::cmp::Ordering::Less => units[idx],
                core::cmp::Ordering::Equal => 0x0000,
                core::cmp::Ordering::Greater => 0xFFFF,
            };
            raw[offset..offset + 2].copy_from_slice(&unit.to_le_bytes());
        }
        raw
    }

    /// Build a short directory entry with the given raw name field,
    /// attribute, first cluster, and size.
    fn build_short_entry(field: &[u8; 11], attr: u8, cluster: u32, size: u32) -> RawEntry {
        let mut raw = [0u8; DIR_ENTRY_LEN];
        raw[0..11].copy_from_slice(field);
        raw[11] = attr;
        let cb = cluster.to_le_bytes();
        raw[20..22].copy_from_slice(&cb[2..4]);
        raw[26..28].copy_from_slice(&cb[0..2]);
        raw[28..32].copy_from_slice(&size.to_le_bytes());
        raw
    }

    /// Initialise a freshly allocated directory cluster with its `.` and
    /// `..` links. `parent_cluster` is `0` when the parent is the root.
    fn init_dir_cluster(
        &mut self,
        child_cluster: u32,
        parent_cluster: u32,
    ) -> Result<(), DriverError> {
        let dot = {
            let mut f = [b' '; 11];
            f[0] = b'.';
            Self::build_short_entry(&f, ATTR_DIRECTORY, child_cluster, 0)
        };
        let dotdot = {
            let mut f = [b' '; 11];
            f[0] = b'.';
            f[1] = b'.';
            let pc = if parent_cluster == self.layout.root_cluster {
                0
            } else {
                parent_cluster
            };
            Self::build_short_entry(&f, ATTR_DIRECTORY, pc, 0)
        };
        let base = self.cluster_byte(child_cluster);
        self.write_bytes(base, &dot)?;
        self.write_bytes(base + DIR_ENTRY_LEN as u64, &dotdot)
    }

    /// Length (in clusters) and last cluster of the chain at `first`.
    fn chain_len(&mut self, first: u32) -> Result<(u64, u32), DriverError> {
        // No chain is longer than the volume has clusters; a longer walk is
        // going round a cycle, and fails closed rather than looping.
        let most = u64::from(self.layout.max_cluster) - 1;
        let mut cluster = first;
        let mut len = 1u64;
        loop {
            match self.next_cluster(cluster)? {
                ChainStep::Next(next) if len < most => {
                    cluster = next;
                    len += 1;
                }
                ChainStep::End => return Ok((len, cluster)),
                ChainStep::Next(_) | ChainStep::Bad => return Err(DriverError::DeviceFault),
            }
        }
    }

    /// Ensure the file whose first cluster is `first` (0 if empty) has at
    /// least `needed` clusters, allocating zeroed clusters as required.
    /// Returns the (possibly newly allocated) first cluster.
    fn ensure_chain(&mut self, first: u32, needed: u64) -> Result<u32, DriverError> {
        if needed == 0 {
            return Ok(first);
        }
        let (head, mut have, mut last) = if first < 2 {
            let fresh = self.alloc_cluster(true)?;
            (fresh, 1u64, fresh)
        } else {
            let (len, last) = self.chain_len(first)?;
            (first, len, last)
        };
        while have < needed {
            let fresh = self.alloc_cluster(true)?;
            self.set_fat(last, fresh)?;
            last = fresh;
            have += 1;
        }
        Ok(head)
    }

    /// Write `buf` into the data chain `first` starting at byte
    /// `byte_offset`. The chain must already be long enough.
    fn write_data(&mut self, first: u32, byte_offset: u64, buf: &[u8]) -> Result<(), DriverError> {
        if buf.is_empty() {
            return Ok(());
        }
        let bpc = self.layout.bytes_per_cluster;
        let mut cluster = first;
        let mut skip = byte_offset / bpc;
        while skip > 0 {
            match self.next_cluster(cluster)? {
                ChainStep::Next(next) => cluster = next,
                _ => return Err(DriverError::DeviceFault),
            }
            skip -= 1;
        }
        let mut intra = byte_offset % bpc;
        let mut done = 0usize;
        while done < buf.len() {
            if cluster < 2 {
                return Err(DriverError::DeviceFault);
            }
            let room = usize::try_from(bpc - intra).map_err(|_| DriverError::LengthOutOfRange)?;
            let take = core::cmp::min(room, buf.len() - done);
            let at = self.cluster_byte(cluster) + intra;
            self.write_bytes(at, &buf[done..done + take])?;
            done += take;
            intra = 0;
            if done < buf.len() {
                match self.next_cluster(cluster)? {
                    ChainStep::Next(next) => cluster = next,
                    _ => return Err(DriverError::DeviceFault),
                }
            }
        }
        Ok(())
    }

    /// Zero `len` bytes of the data chain `first` starting at `start`.
    fn zero_range(&mut self, first: u32, start: u64, len: u64) -> Result<(), DriverError> {
        let zeros = [0u8; MAX_BLOCK_SIZE as usize];
        let mut remaining = len;
        let mut at = start;
        while remaining > 0 {
            let chunk = remaining.min(zeros.len() as u64);
            let chunk_usize = usize::try_from(chunk).map_err(|_| DriverError::DeviceFault)?;
            self.write_data(first, at, &zeros[..chunk_usize])?;
            at += chunk;
            remaining -= chunk;
        }
        Ok(())
    }

    /// Shared implementation of [`FilesystemWrite::create`].
    fn create_child(
        &mut self,
        dir: NodeId,
        name: &[u8],
        kind: NodeKind,
    ) -> Result<NodeId, DriverError> {
        if !node_is_dir(dir) {
            return Err(DriverError::Unsupported);
        }
        if name.is_empty() || name.len() > MAX_NAME_BYTES {
            return Err(DriverError::LengthOutOfRange);
        }
        let dir_cluster = node_cluster(dir);
        if self.find_child(dir_cluster, name)?.is_some() {
            return Err(DriverError::AlreadyExists);
        }

        let is_dir = matches!(kind, NodeKind::Directory);
        let child_cluster = if is_dir {
            let fresh = self.alloc_cluster(true)?;
            self.init_dir_cluster(fresh, dir_cluster)?;
            fresh
        } else {
            0
        };
        let attr = if is_dir { ATTR_DIRECTORY } else { 0x20 };
        if let Err(e) = self.write_dir_entry(dir_cluster, name, attr, child_cluster, 0) {
            // Reclaim the directory cluster if naming the entry failed, so a
            // rejected create leaves no orphaned chain behind.
            if is_dir && child_cluster >= 2 {
                let _ = self.free_chain(child_cluster);
            }
            return Err(e);
        }
        Ok(pack_node(child_cluster, is_dir, 0))
    }

    /// Encode `name` as a run of directory slots — the long-name fragments
    /// followed by the 8.3 short entry pointing at `cluster`/`size` with
    /// attribute `attr` — in a free run of `dir_cluster`. Shared by
    /// [`Self::create_child`] and [`Self::rename_child`] so the long-name
    /// encoding lives in one place.
    fn write_dir_entry(
        &mut self,
        dir_cluster: u32,
        name: &[u8],
        attr: u8,
        cluster: u32,
        size: u32,
    ) -> Result<(), DriverError> {
        let mut units = [0u16; MAX_LONG_NAME_UNITS];
        let unit_count = encode_utf16le(name, &mut units).ok_or(DriverError::LengthOutOfRange)?;
        if unit_count == 0 {
            return Err(DriverError::LengthOutOfRange);
        }
        let frag_count = unit_count.div_ceil(LFN_UNITS_PER_ENTRY);
        if frag_count > LFN_MAX_FRAGMENTS {
            return Err(DriverError::LengthOutOfRange);
        }

        let total_slots = frag_count as u64 + 1;
        let (short, run) = self.place_entry(dir_cluster, &AliasStem::of(name), total_slots)?;
        if run.start.slot + total_slots > u64::from(MAX_DIR_SLOTS) {
            return Err(DriverError::NoSpace);
        }
        let checksum = short_name_checksum(&short);

        // The end-of-entries mark moves to the slot after the entry. It is
        // found before anything is written, so a broken chain fails the
        // create cleanly, and written first, so the old mark ends the
        // directory until the entry is whole. Past the chain's end no mark is
        // needed: the clusters the entry grows into are zeroed.
        if run.at_end {
            let mut after = run.start;
            let mut in_chain = true;
            for _ in 0..total_slots {
                in_chain = self.settle(&mut after)?;
                if !in_chain {
                    break;
                }
                after.intra += DIR_ENTRY_LEN as u64;
                after.slot += 1;
            }
            if in_chain && after.slot < u64::from(MAX_DIR_SLOTS) && self.settle(&mut after)? {
                self.write_bytes(self.slot_byte(after), &[0u8; DIR_ENTRY_LEN])?;
            }
        }

        // Physical order: the highest sequence (flagged last-logical) is
        // written first, descending to sequence 1, then the short entry.
        let mut pos = run.start;
        for phys in 0..frag_count {
            let seq = frag_count - phys;
            let entry = Self::build_lfn_entry(&units[..unit_count], seq, phys == 0, checksum);
            self.put_slot(&mut pos, &entry)?;
        }
        self.put_slot(
            &mut pos,
            &Self::build_short_entry(&short, attr, cluster, size),
        )
    }

    /// Shared implementation of [`FilesystemWrite::write_at`].
    fn write_file(
        &mut self,
        dir: NodeId,
        name: &[u8],
        offset: u64,
        data: &[u8],
    ) -> Result<usize, DriverError> {
        if !node_is_dir(dir) {
            return Err(DriverError::Unsupported);
        }
        let dir_cluster = node_cluster(dir);
        let entry = self
            .find_child(dir_cluster, name)?
            .ok_or(DriverError::NotFound)?;
        if entry.is_dir {
            return Err(DriverError::Unsupported);
        }
        if data.is_empty() {
            return Ok(0);
        }
        let end = offset
            .checked_add(data.len() as u64)
            .ok_or(DriverError::LengthOutOfRange)?;
        let old_size = u64::from(entry.size);
        let bpc = self.layout.bytes_per_cluster;
        let needed = end.div_ceil(bpc);
        let first = self.ensure_chain(entry.cluster, needed)?;
        if offset > old_size {
            self.zero_range(first, old_size, offset - old_size)?;
        }
        self.write_data(first, offset, data)?;
        let new_size =
            u32::try_from(old_size.max(end)).map_err(|_| DriverError::LengthOutOfRange)?;
        self.set_entry_meta(entry.short_offset, first, new_size)?;
        Ok(data.len())
    }

    /// Shared implementation of [`FilesystemWrite::truncate`].
    fn truncate_file(&mut self, dir: NodeId, name: &[u8], size: u64) -> Result<(), DriverError> {
        if !node_is_dir(dir) {
            return Err(DriverError::Unsupported);
        }
        let dir_cluster = node_cluster(dir);
        let entry = self
            .find_child(dir_cluster, name)?
            .ok_or(DriverError::NotFound)?;
        if entry.is_dir {
            return Err(DriverError::Unsupported);
        }
        let old_size = u64::from(entry.size);
        if size == old_size {
            return Ok(());
        }
        let bpc = self.layout.bytes_per_cluster;
        let new_size = u32::try_from(size).map_err(|_| DriverError::LengthOutOfRange)?;

        if size < old_size {
            let needed = size.div_ceil(bpc);
            if needed == 0 {
                if entry.cluster >= 2 {
                    self.free_chain(entry.cluster)?;
                }
                self.set_entry_meta(entry.short_offset, 0, 0)?;
                return Ok(());
            }
            // Walk to the new last cluster, sever and free the remainder.
            let mut cluster = entry.cluster;
            for _ in 0..needed - 1 {
                match self.next_cluster(cluster)? {
                    ChainStep::Next(next) => cluster = next,
                    _ => return Err(DriverError::DeviceFault),
                }
            }
            if let ChainStep::Next(tail) = self.next_cluster(cluster)? {
                self.free_chain(tail)?;
            }
            self.set_fat(cluster, FAT32_EOC_WRITE)?;
            self.set_entry_meta(entry.short_offset, entry.cluster, new_size)?;
        } else {
            let needed = size.div_ceil(bpc);
            let first = self.ensure_chain(entry.cluster, needed)?;
            self.zero_range(first, old_size, size - old_size)?;
            self.set_entry_meta(entry.short_offset, first, new_size)?;
        }
        Ok(())
    }

    /// Shared implementation of [`FilesystemWrite::remove`].
    fn remove_child(&mut self, dir: NodeId, name: &[u8]) -> Result<(), DriverError> {
        if !node_is_dir(dir) {
            return Err(DriverError::Unsupported);
        }
        let dir_cluster = node_cluster(dir);
        let entry = self
            .find_child(dir_cluster, name)?
            .ok_or(DriverError::NotFound)?;
        if entry.is_dir {
            let mut child = DirCursor::at(entry.cluster);
            if self.next_entry(&mut child)?.is_some() {
                return Err(DriverError::DirectoryNotEmpty);
            }
        }
        if entry.cluster >= 2 {
            self.free_chain(entry.cluster)?;
        }
        self.delete_entry_slots(entry.first, entry.slot_span)
    }

    /// Mark the `span` slots from `first` deleted: one logical entry's
    /// long-name fragments and its short entry.
    fn delete_entry_slots(&mut self, mut pos: SlotPos, span: u64) -> Result<(), DriverError> {
        for _ in 0..span {
            if !self.settle(&mut pos)? {
                return Err(DriverError::DeviceFault);
            }
            self.write_bytes(self.slot_byte(pos), &[DELETED_ENTRY])?;
            pos.intra += DIR_ENTRY_LEN as u64;
            pos.slot += 1;
        }
        Ok(())
    }

    /// The cluster of `dir_cluster`'s parent, read from its `..` entry
    /// (which stores `0` for the root).
    fn dir_parent_cluster(&mut self, dir_cluster: u32) -> Result<u32, DriverError> {
        let mut raw = [0u8; DIR_ENTRY_LEN];
        self.read_bytes(
            self.cluster_byte(dir_cluster) + DIR_ENTRY_LEN as u64,
            &mut raw,
        )?;
        if raw[..11] != *b"..         " || raw[11] & ATTR_DIRECTORY == 0 {
            return Err(DriverError::DeviceFault);
        }
        match (u32::from(le16(&raw, 20)) << 16) | u32::from(le16(&raw, 26)) {
            0 => Ok(self.layout.root_cluster),
            cluster if (2..=self.layout.max_cluster).contains(&cluster) => Ok(cluster),
            _ => Err(DriverError::DeviceFault),
        }
    }

    /// Whether directory `candidate` is `ancestor` itself or lives anywhere
    /// beneath it, walking `..` links up to the root. Refuses moving a
    /// directory into its own subtree (which would detach the cycle).
    fn is_subdir_of(&mut self, mut candidate: u32, ancestor: u32) -> Result<bool, DriverError> {
        if !(2..=self.layout.max_cluster).contains(&candidate) {
            return Err(DriverError::DeviceFault);
        }
        // Brent's cycle detection: a `..` chain that comes back to a
        // directory it has passed never reaches the root.
        let mut mark = candidate;
        let mut lap = 1u32;
        let mut steps = 0u32;
        loop {
            if candidate == ancestor {
                return Ok(true);
            }
            if candidate == self.layout.root_cluster {
                return Ok(false);
            }
            candidate = self.dir_parent_cluster(candidate)?;
            if candidate == mark {
                return Err(DriverError::DeviceFault);
            }
            steps += 1;
            if steps == lap {
                mark = candidate;
                lap = lap.saturating_mul(2);
                steps = 0;
            }
        }
    }

    /// Shared implementation of [`FilesystemWrite::rename`].
    ///
    /// FAT has no inode and no journal: the move re-encodes the source's
    /// long-name + short entry under the destination name (preserving its
    /// first cluster, size, and attribute byte verbatim), then deletes the
    /// source entry, so the file's data clusters are never touched. Across
    /// directories a moved directory's `..` is repointed at the new parent.
    /// Replacement of an existing destination is therefore best-effort
    /// rather than atomic, matching the non-transactional create/remove
    /// paths the on-disk format allows.
    fn rename_child(
        &mut self,
        src_dir: NodeId,
        src_name: &[u8],
        dst_dir: NodeId,
        dst_name: &[u8],
    ) -> Result<(), DriverError> {
        if !node_is_dir(src_dir) || !node_is_dir(dst_dir) {
            return Err(DriverError::Unsupported);
        }
        if dst_name.is_empty() || dst_name.len() > MAX_NAME_BYTES {
            return Err(DriverError::LengthOutOfRange);
        }
        if dst_name == b"." || dst_name == b".." {
            return Err(DriverError::Unsupported);
        }
        let src_cluster = node_cluster(src_dir);
        let dst_cluster = node_cluster(dst_dir);
        let src_entry = self
            .find_child(src_cluster, src_name)?
            .ok_or(DriverError::NotFound)?;
        let moving_dir = src_entry.is_dir;

        // Preserve the source's attribute byte (read-only/hidden/system/
        // archive/directory) verbatim across the move.
        let mut attr_buf = [0u8; 1];
        self.read_bytes(src_entry.short_offset + 11, &mut attr_buf)?;
        let attr = attr_buf[0];

        let dst_existing = self.find_child(dst_cluster, dst_name)?;
        if let Some(d) = &dst_existing {
            if d.short_offset == src_entry.short_offset {
                // Source and destination resolve to the same entry already.
                return Ok(());
            }
        }

        // Refuse moving a directory into itself or its own subtree.
        if moving_dir && self.is_subdir_of(dst_cluster, src_entry.cluster)? {
            return Err(DriverError::DirectoryCycle);
        }

        // Replace an existing destination, subject to kind compatibility.
        if let Some(d) = dst_existing {
            if d.is_dir != moving_dir {
                return Err(DriverError::Unsupported);
            }
            if d.is_dir {
                let mut cur = DirCursor::at(d.cluster);
                if self.next_entry(&mut cur)?.is_some() {
                    return Err(DriverError::DirectoryNotEmpty);
                }
            }
            if d.cluster >= 2 {
                self.free_chain(d.cluster)?;
            }
            self.delete_entry_slots(d.first, d.slot_span)?;
        }

        // Link the moved node under its new name, then unlink the source.
        self.write_dir_entry(
            dst_cluster,
            dst_name,
            attr,
            src_entry.cluster,
            src_entry.size,
        )?;
        self.delete_entry_slots(src_entry.first, src_entry.slot_span)?;

        // Repoint the moved directory's `..` at its new parent cluster.
        if moving_dir && src_cluster != dst_cluster && src_entry.cluster >= 2 {
            let parent_field = if dst_cluster == self.layout.root_cluster {
                0
            } else {
                dst_cluster
            };
            let dotdot_offset = self.cluster_byte(src_entry.cluster) + DIR_ENTRY_LEN as u64;
            self.set_entry_meta(dotdot_offset, parent_field, 0)?;
        }
        Ok(())
    }
}

impl<B: Block> FilesystemRead for Fat32<B> {
    fn root(&self) -> NodeId {
        pack_node(self.layout.root_cluster, true, 0)
    }

    fn name_matching(&self) -> NameMatching {
        NameMatching::AsciiCaseInsensitive
    }

    fn node_info(&mut self, node: NodeId) -> Result<NodeInfo, DriverError> {
        let allocated = self.allocation(node_cluster(node), node_is_dir(node), node_size(node))?;
        // FAT stores timestamps only in the *parent's* directory entry, not
        // in anything addressable by the packed node identity, so a stat by
        // node cannot report them: `read_dir` is the one path that carries a
        // FAT node's real stamps. Reporting the epoch here is the honest
        // "not addressable by node" answer, never a fabricated wall time.
        if node_is_dir(node) {
            Ok(NodeInfo {
                kind: NodeKind::Directory,
                nlink: NodeInfo::SINGLE_NAME,
                size: 0,
                allocated,
                times: NodeTimes::default(),
            })
        } else {
            Ok(NodeInfo {
                kind: NodeKind::RegularFile,
                nlink: NodeInfo::SINGLE_NAME,
                size: u64::from(node_size(node)),
                allocated,
                times: NodeTimes::default(),
            })
        }
    }

    fn lookup(&mut self, dir: NodeId, name: &[u8]) -> Result<NodeId, DriverError> {
        if !node_is_dir(dir) {
            return Err(DriverError::Unsupported);
        }
        if name.is_empty() || name.len() > MAX_NAME_BYTES {
            return Err(DriverError::NotFound);
        }
        let mut cursor = DirCursor::at(node_cluster(dir));
        while let Some(entry) = self.next_entry(&mut cursor)? {
            if entry.name[..entry.name_len].eq_ignore_ascii_case(name) {
                return Ok(Self::entry_node(&entry));
            }
        }
        Err(DriverError::NotFound)
    }

    fn read_at(&mut self, file: NodeId, offset: u64, buf: &mut [u8]) -> Result<usize, DriverError> {
        if node_is_dir(file) {
            return Err(DriverError::Unsupported);
        }
        let size = u64::from(node_size(file));
        if buf.is_empty() || offset >= size {
            return Ok(0);
        }
        let want = core::cmp::min(buf.len() as u64, size - offset);
        let want = usize::try_from(want).map_err(|_| DriverError::LengthOutOfRange)?;

        let first = node_cluster(file);
        if first < 2 {
            return Err(DriverError::DeviceFault);
        }
        let bytes_per_cluster = self.layout.bytes_per_cluster;

        let mut cluster = first;
        let mut to_skip = offset / bytes_per_cluster;
        while to_skip > 0 {
            match self.next_cluster(cluster)? {
                ChainStep::Next(next) => cluster = next,
                _ => return Err(DriverError::DeviceFault),
            }
            to_skip -= 1;
        }

        let mut intra = usize::try_from(offset % bytes_per_cluster)
            .map_err(|_| DriverError::LengthOutOfRange)?;
        let cluster_len =
            usize::try_from(bytes_per_cluster).map_err(|_| DriverError::LengthOutOfRange)?;
        let mut produced = 0;
        while produced < want {
            if cluster < 2 {
                return Err(DriverError::DeviceFault);
            }
            let take = core::cmp::min(cluster_len - intra, want - produced);
            let start = self.cluster_byte(cluster) + intra as u64;
            self.read_bytes(start, &mut buf[produced..produced + take])?;
            produced += take;
            intra = 0;
            if produced < want {
                match self.next_cluster(cluster)? {
                    ChainStep::Next(next) => cluster = next,
                    ChainStep::End => break,
                    ChainStep::Bad => return Err(DriverError::DeviceFault),
                }
            }
        }
        Ok(produced)
    }

    fn read_dir(
        &mut self,
        dir: NodeId,
        cursor: u64,
        _after: &[u8],
        visit: &mut dyn FnMut(&DirEntry, &[u8]) -> DirVisit,
    ) -> Result<(), DriverError> {
        if !node_is_dir(dir) {
            return Err(DriverError::Unsupported);
        }
        // The cursor is a logical slot index, resolved by walking this
        // directory's own chain, so no cursor reaches outside it. Slots never
        // move while in use — a deletion marks its slots in place and an
        // insertion takes a free run — so an entry present throughout a
        // listing is reached exactly once.
        if cursor >= u64::from(MAX_DIR_SLOTS) {
            return Ok(());
        }
        let mut walk = DirCursor::at(node_cluster(dir));
        // A resumed batch starts on the slot its last entry took, to see
        // what holds it now.
        let resumed = cursor > 0;
        if !self.seek_slot(&mut walk, cursor - u64::from(resumed))? {
            return Ok(());
        }
        if resumed && !self.step_past_resume(&mut walk)? {
            return Ok(());
        }
        self.list_from(&mut walk, visit)
    }
}

impl<B: Block> Fat32<B> {
    /// Hand `visit` the entries from where `walk` stands until it stops or
    /// the directory ends.
    fn list_from(
        &mut self,
        walk: &mut DirCursor,
        visit: &mut dyn FnMut(&DirEntry, &[u8]) -> DirVisit,
    ) -> Result<(), DriverError> {
        while let Some(entry) = self.next_entry(walk)? {
            let info = NodeInfo {
                kind: if entry.is_dir {
                    NodeKind::Directory
                } else {
                    NodeKind::RegularFile
                },
                nlink: NodeInfo::SINGLE_NAME,
                size: if entry.is_dir {
                    0
                } else {
                    u64::from(entry.size)
                },
                allocated: self.allocation(entry.cluster, entry.is_dir, entry.size)?,
                times: entry.times,
            };
            let listed = DirEntry {
                node: Self::entry_node(&entry),
                info,
                next_cursor: walk.pos.slot,
            };
            if visit(&listed, &entry.name[..entry.name_len]) == DirVisit::Stop {
                break;
            }
        }
        Ok(())
    }
}

impl<B: Block> FilesystemWrite for Fat32<B> {
    fn create(&mut self, dir: NodeId, name: &[u8], kind: NodeKind) -> Result<NodeId, DriverError> {
        self.create_child(dir, name, kind)
    }

    fn write_at(
        &mut self,
        dir: NodeId,
        name: &[u8],
        offset: u64,
        data: &[u8],
    ) -> Result<usize, DriverError> {
        self.write_file(dir, name, offset, data)
    }

    fn truncate(&mut self, dir: NodeId, name: &[u8], size: u64) -> Result<(), DriverError> {
        self.truncate_file(dir, name, size)
    }

    fn remove(&mut self, dir: NodeId, name: &[u8]) -> Result<(), DriverError> {
        self.remove_child(dir, name)
    }

    fn rename(
        &mut self,
        src_dir: NodeId,
        src_name: &[u8],
        dst_dir: NodeId,
        dst_name: &[u8],
    ) -> Result<(), DriverError> {
        self.rename_child(src_dir, src_name, dst_dir, dst_name)
    }

    fn flush(&mut self) -> Result<(), DriverError> {
        // All mutations are written straight through to the block device.
        Ok(())
    }
}

impl<B: Block> FilesystemSecurity for Fat32<B> {
    fn security(&mut self, node: NodeId) -> Result<NodeSecurity, DriverError> {
        // FAT32 stores no owner, mode, ACL, or capability gate, so every
        // node reports one uniform, restrictive default record: owned by
        // the system principal, group-writable by nobody. The volume
        // manager's mount policy is what grants ordinary users access to
        // a foreign volume; the driver itself never fabricates per-file
        // ownership the format cannot hold.
        let info = self.node_info(node)?;
        let mode = match info.kind {
            NodeKind::Directory => 0o755,
            NodeKind::RegularFile => 0o644,
            // FAT32 stores no symbolic links, so `node_info` never reports
            // one; refuse rather than invent a mode for a node this format
            // cannot hold.
            NodeKind::Symlink => return Err(DriverError::Unsupported),
        };
        Ok(NodeSecurity::new(mode, 0, 0))
    }

    fn set_security(&mut self, _node: NodeId, _security: NodeSecurity) -> Result<(), DriverError> {
        // The FAT32 on-disk format cannot store any part of a TAIRiX
        // security record. Storing a silently-lossy record is forbidden,
        // so the write is refused whole (fail closed).
        Err(DriverError::Unsupported)
    }
}

/// The on-disk format has nowhere to store TAIRiX extended attributes, so
/// the default facet answer stands: a mounted volume refuses the
/// `fs_attr_*` surface with the typed unsupported-backing error.
impl<B: Block> FilesystemAttrsProvider for Fat32<B> {}

impl<B: Block> FilesystemStats for Fat32<B> {
    fn stats(&mut self) -> Result<VolumeStats, DriverError> {
        // A pure read of the counter established by the open-time FAT scan
        // and maintained by the allocator — no device I/O. The allocation
        // unit FAT32 accounts in is the cluster, so the figures are
        // cluster-denominated; FAT32 has no inode table, and the zero pair
        // reports that honestly rather than fabricating a capacity.
        let bytes_per_cluster =
            u32::try_from(self.layout.bytes_per_cluster).map_err(|_| DriverError::DeviceFault)?;
        Ok(VolumeStats {
            block_size: bytes_per_cluster,
            total_blocks: u64::from(self.layout.max_cluster) - 1,
            free_blocks: self.free_clusters,
            avail_blocks: self.free_clusters,
            files: 0,
            files_free: 0,
        })
    }
}

#[cfg(test)]
mod tests;
