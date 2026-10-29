//! Filesystem recognition: which supported filesystem an extent of a block
//! device holds, and the identity, label and mutation evidence it carries
//! (`plans/DEVICES.md` D3c, `plans/FOREIGN-MEDIA.md`).
//!
//! [`recognise`] is the one decision the volume manager, the kernel's attach
//! path and the RAID administrator share. The filesystem drivers import the
//! constants and recognisers it reads by — the `ARXFS` header magic, the ext
//! superblock magic, the FAT32 parameter block and identity, and the
//! [`adfs`], [`iso9660`] and [`udf`] recognisers — so a probe and a mounted
//! driver never disagree about what a volume is or which identity it carries.
//!
//! Everything read is untrusted removable-media content: every access is
//! bounds-checked, every field validated before use, and anything that does
//! not match a supported format exactly is `None` — never a guess.
//!
//! [`fingerprint`] renders an identity as the short display fingerprint
//! (`plans/ALIAS.md` §3.8).

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

pub mod adfs;
pub mod iso9660;
pub mod name;
pub mod udf;

use alloc::boxed::Box;

use tairix_abi::driver::block::{Block, DiscLayout};
use tairix_abi::volume::VolumeFsType;
use tairix_abi::DriverError;
use tairix_bytedev::ByteDevice;
use tairix_crypto::hash::Sha256Stream;

/// Bytes of an extent's head that hold every signature opening an extent:
/// the FAT32 boot sector (512), the `ARXFS` metadata header (128), and the
/// ext4 superblock at bytes 1024..2048.
const HEAD_LEN: usize = 2048;

/// On-disk magic in the first eight bytes of every `ARXFS` metadata block:
/// `"ARXFSB\3"` little-endian (`docs/src/filesystem/arxfs-spec.md` §8).
pub const ARXFS_HEADER_MAGIC: u64 = 0x5255_5354_4653_4203;

/// ext4/ext3/ext2 superblock magic (`s_magic`), little-endian at byte `0x38`
/// of the superblock.
pub const EXT4_SUPERBLOCK_MAGIC: u16 = 0xEF53;

/// Maximum bytes of a recognised volume label: the longest any supported
/// format records is UDF's logical volume identifier, 126 Latin-1 characters
/// of up to two UTF-8 bytes each.
pub const LABEL_MAX: usize = 252;

/// Characters of the full identity fingerprint: 16 identity bytes = 128
/// bits, at five bits per base32 character, rounded up.
pub const FINGERPRINT_CHARS: usize = 26;

/// Bytes of one FAT directory entry, fixed by the format.
pub const FAT_DIR_ENTRY_LEN: usize = 32;
/// FAT attribute bit: the entry is the volume label, not a file.
pub const FAT_ATTR_VOLUME_ID: u8 = 0x08;
/// FAT attribute value of a long-file-name fragment.
pub const FAT_ATTR_LONG_NAME: u8 = 0x0F;
/// FAT first name byte: the directory ends here.
pub const FAT_END_OF_DIR: u8 = 0x00;
/// FAT first name byte: the entry was deleted.
pub const FAT_DELETED_ENTRY: u8 = 0xE5;

/// What FAT formatters write for a volume given no label: a placeholder, not
/// a label.
const FAT_NO_LABEL: &[u8] = b"NO NAME";

/// How much of a FAT32 root directory is read for its label entry, which
/// formatters write first: a bound on the read, not on the format.
const FAT32_LABEL_SCAN_LEN: usize = 4096;

/// Bytes at the start of an ISO 9660 or UDF volume their structures leave
/// alone: the system area, where a hybrid image keeps its partition table and
/// boot code.
pub const SYSTEM_AREA_LEN: u64 = 32 * 1024;

/// Ceiling on a mutation-evidence window, in bytes: the largest any
/// supported format's declaration can reach (the `ARXFS` superblock ring of
/// eight 4 KiB blocks). A larger declaration yields no evidence rather than
/// a clamped window, which could no longer prove non-mutation.
pub const EVIDENCE_MAX: u64 = 32 * 1024;

/// Physical blocks of the `ARXFS` superblock ring: four logical slots, each
/// a mirrored pair.
pub const ARXFS_RING_BLOCKS: u64 = 8;

const _: () = assert!(ARXFS_RING_BLOCKS * 4096 <= EVIDENCE_MAX);

/// What [`recognise`] found in an extent.
// One result per extent, returned once and never held in bulk.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Recognised {
    /// A member of a RAID array awaiting assembly, naming the array. It is
    /// never mounted bare: one copy of a mirror mounted read-write diverges
    /// the array, and a member that missed writes serves stale data.
    RaidMember([u8; 16]),
    /// A volume of a supported filesystem.
    Volume(ProbedVolume),
}

/// A region of an extent, in bytes from its start, whose content any
/// foreign mutation of the volume rewrites: what a verified re-insert
/// compares (`plans/DEVICES.md` D4c).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Evidence {
    /// Where the region starts.
    pub offset: u64,
    /// Its length, at most [`EVIDENCE_MAX`].
    pub len: u64,
}

/// A recognised volume.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ProbedVolume {
    /// The filesystem recognised.
    pub fstype: VolumeFsType,
    /// The volume's stable 16-byte identity, exactly as the filesystem's
    /// driver publishes it when the volume is attached.
    pub identity: [u8; 16],
    /// The volume's mutation evidence: the `ARXFS` superblock ring, the ext4
    /// primary superblock, FAT32's reserved sectors through `FSInfo`, UDF's
    /// integrity sequence. `None` where the format has no region every writer
    /// rewrites (ISO 9660 is never written; ADFS keeps none) or declares an
    /// implausible one — no evidence means no replay.
    pub evidence: Option<Evidence>,
    /// How far into the extent the volume's structures reach, for the
    /// formats whose structures do not open the extent (ISO 9660 and UDF), so
    /// a partition table found beside them can be weighed against them.
    pub span: Option<u64>,
    label: [u8; LABEL_MAX],
    label_len: u8,
}

impl ProbedVolume {
    fn new(
        fstype: VolumeFsType,
        identity: [u8; 16],
        label: &[u8],
        evidence: Option<Evidence>,
        span: Option<u64>,
    ) -> Self {
        let (label, label_len) = take_label(label);
        Self {
            fstype,
            identity,
            evidence,
            span,
            label,
            label_len,
        }
    }

    /// The volume's human-facing label, trimmed of the format's padding;
    /// empty when the format records none (`ARXFS`), the field is blank, or
    /// it holds the format's "no label" placeholder.
    #[must_use]
    pub fn label(&self) -> &[u8] {
        &self.label[..usize::from(self.label_len)]
    }
}

/// Recognise what the extent `dev` holds, in a fixed order: a RAID array
/// member first, then the formats whose signature opens the extent
/// (`ARXFS`, ext4, FAT32 — most specific first), then the disc formats whose
/// structures lie 32 KiB in (ISO 9660 and UDF), then ADFS, whose oldest
/// layout carries the weakest signature. `None` when nothing matches.
///
/// A FAT32 volume is labelled from its root directory's label entry, which a
/// relabel rewrites, over the boot sector's copy. A disc carrying both ISO
/// 9660 and UDF over the same files (a bridge disc) is recognised as UDF,
/// whose names and metadata are the richer, unless its ISO 9660 tree carries
/// what UDF would lose: Rock Ridge's POSIX records or RISC OS filetypes.
///
/// # Errors
///
/// A device fault, or a refused allocation. A structure the extent is too
/// small to hold is simply absent.
pub fn recognise<B: Block>(dev: &mut ByteDevice<B>) -> Result<Option<Recognised>, DriverError> {
    let mut head = heap_buf::<HEAD_LEN>()?;
    let len = usize::try_from(dev.capacity()).map_or(HEAD_LEN, |capacity| capacity.min(HEAD_LEN));
    let head = &mut head[..len];
    dev.read(0, head)?;
    if let Some(array) = raid_member(head) {
        return Ok(Some(Recognised::RaidMember(array)));
    }
    let found = match head_volume(head) {
        Some(mut volume) => {
            if volume.fstype == VolumeFsType::Fat32 {
                if let Some((label, label_len)) = directory_label(dev, head)? {
                    volume.label = label;
                    volume.label_len = label_len;
                }
            }
            Some(volume)
        }
        None => match disc_volume(dev)? {
            Some(volume) => Some(volume),
            None => adfs::identify(dev)?.map(|layout| {
                let (name, len) = layout.name();
                ProbedVolume::new(
                    VolumeFsType::Adfs,
                    layout.identity(),
                    &name[..len],
                    None,
                    None,
                )
            }),
        },
    };
    Ok(found.map(Recognised::Volume))
}

/// A RAID array member's array UUID, through the one superblock decode the
/// RAID composition driver assembles from.
fn raid_member(head: &[u8]) -> Option<[u8; 16]> {
    tairix_raidmeta::ArraySuperblock::decode(head)
        .ok()
        .map(|superblock| superblock.array_uuid)
}

/// The formats whose signature opens the extent.
fn head_volume(head: &[u8]) -> Option<ProbedVolume> {
    probe_arxfs(head)
        .or_else(|| probe_ext4(head))
        .or_else(|| probe_fat32(head))
}

/// The disc formats, from the disc's last session.
fn disc_volume<B: Block>(dev: &mut ByteDevice<B>) -> Result<Option<ProbedVolume>, DriverError> {
    let layout = dev.device().disc_layout();
    let iso = iso9660::identify(dev, layout)?;
    let iso_keeps_more = iso
        .as_ref()
        .is_some_and(|iso| iso.system_use.rock_ridge || iso.system_use.archimedes);
    if !iso_keeps_more {
        if let Some(udf) = udf::identify(dev, layout)? {
            let (label, len) = udf.label();
            let evidence = udf_evidence(&udf, dev.capacity());
            return Ok(Some(ProbedVolume::new(
                VolumeFsType::Udf,
                udf.identity(),
                &label[..len],
                evidence,
                Some(udf.span_bytes()),
            )));
        }
    }
    Ok(iso.map(|iso| {
        let (label, len) = iso.label();
        ProbedVolume::new(
            VolumeFsType::Iso9660,
            iso.identity(),
            &label[..len],
            None,
            Some(iso.span_bytes()),
        )
    }))
}

/// UDF's mutation evidence: the logical volume integrity sequence, which a
/// writer marks open before its first change and closed after its last.
fn udf_evidence(volume: &udf::Volume, capacity: u64) -> Option<Evidence> {
    let integrity = volume.logical().integrity;
    let offset = u64::from(integrity.location).checked_mul(u64::from(volume.block_size()))?;
    let len = u64::from(integrity.length);
    let fits = len != 0 && len <= EVIDENCE_MAX && offset.checked_add(len)? <= capacity;
    fits.then_some(Evidence { offset, len })
}

/// The bytes, from the device's start, of the session a disc's newest
/// structures are counted from, or `None` when that start is not a whole
/// number of `unit`-byte sectors.
pub(crate) fn session_bytes<B: Block>(
    dev: &ByteDevice<B>,
    layout: Option<DiscLayout>,
    unit: u64,
) -> Option<u64> {
    let start = layout.map_or(0, |layout| layout.last_session_start);
    let bytes = start.checked_mul(u64::from(dev.block_size()))?;
    bytes.is_multiple_of(unit).then_some(bytes)
}

/// The label a FAT32 volume's root directory records, read from where its
/// boot sector `head` places the directory; `None` when the directory lies
/// beyond the extent or records none.
fn directory_label<B: Block>(
    dev: &mut ByteDevice<B>,
    head: &[u8],
) -> Result<Option<([u8; LABEL_MAX], u8)>, DriverError> {
    let Some(root) = fat32_root_dir_byte(head) else {
        return Ok(None);
    };
    let take = usize::try_from(dev.capacity().saturating_sub(root))
        .map_or(FAT32_LABEL_SCAN_LEN, |left| left.min(FAT32_LABEL_SCAN_LEN));
    if take == 0 {
        return Ok(None);
    }
    let mut entries = heap_buf::<FAT32_LABEL_SCAN_LEN>()?;
    dev.read(root, &mut entries[..take])?;
    Ok(fat32_directory_label(&entries[..take]))
}

/// The fields of a FAT32 BIOS parameter block that place the volume's
/// regions, validated as the format bounds them: the one reading the probe
/// and the FAT32 driver share, so they cannot disagree about where a
/// volume's data lies.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Fat32Bpb {
    /// Bytes per sector: a power of two in `512..=4096`.
    pub bytes_per_sector: u32,
    /// Sectors per cluster: a power of two in `1..=128`.
    pub sectors_per_cluster: u32,
    /// Sectors before the first FAT, never zero.
    pub reserved_sectors: u64,
    /// Copies of the FAT: one or two.
    pub fat_count: u64,
    /// Sectors of one FAT, never zero.
    pub fat_sectors: u64,
    /// The root directory's first cluster, at least 2.
    pub root_cluster: u32,
}

impl Fat32Bpb {
    /// The parameter block `boot` records, or `None` for a boot sector no
    /// FAT32 volume carries: no `0x55AA` signature, a sector or cluster size
    /// outside the format, no reserved sectors, a FAT count other than one or
    /// two, the FAT12/16 root-entry or FAT-size fields set, an empty FAT, or
    /// a root below cluster 2.
    #[must_use]
    pub fn parse(boot: &[u8; 512]) -> Option<Self> {
        if boot[510] != 0x55 || boot[511] != 0xAA {
            return None;
        }
        let bytes_per_sector = u32::from(le_u16(boot, 11)?);
        let sectors_per_cluster = u32::from(boot[13]);
        let reserved_sectors = u64::from(le_u16(boot, 14)?);
        let fat_count = u64::from(boot[16]);
        let root_entries = le_u16(boot, 17)?;
        let fat_sectors_16 = le_u16(boot, 22)?;
        let fat_sectors = u64::from(le_u32(boot, 36)?);
        let root_cluster = le_u32(boot, 44)?;
        let valid = bytes_per_sector.is_power_of_two()
            && (512..=4096).contains(&bytes_per_sector)
            && sectors_per_cluster.is_power_of_two()
            && reserved_sectors != 0
            && (1..=2).contains(&fat_count)
            && root_entries == 0
            && fat_sectors_16 == 0
            && fat_sectors != 0
            && root_cluster >= 2;
        valid.then_some(Self {
            bytes_per_sector,
            sectors_per_cluster,
            reserved_sectors,
            fat_count,
            fat_sectors,
            root_cluster,
        })
    }

    /// Bytes of one cluster.
    #[must_use]
    pub const fn cluster_bytes(&self) -> u64 {
        self.sectors_per_cluster as u64 * self.bytes_per_sector as u64
    }

    /// The byte offset of the first FAT.
    #[must_use]
    pub const fn fat_start_byte(&self) -> u64 {
        self.reserved_sectors * self.bytes_per_sector as u64
    }

    /// The byte offset of the data region, where cluster 2 begins, or `None`
    /// when it overflows.
    #[must_use]
    pub fn data_start_byte(&self) -> Option<u64> {
        self.fat_count
            .checked_mul(self.fat_sectors)?
            .checked_add(self.reserved_sectors)?
            .checked_mul(u64::from(self.bytes_per_sector))
    }

    /// The byte offset of data cluster `cluster`, or `None` for a number
    /// below 2 or an offset that overflows.
    #[must_use]
    pub fn cluster_byte(&self, cluster: u32) -> Option<u64> {
        u64::from(cluster.checked_sub(2)?)
            .checked_mul(self.cluster_bytes())?
            .checked_add(self.data_start_byte()?)
    }
}

/// Where a FAT32 volume's root directory begins, in bytes from the start of
/// the volume, read from its boot sector: where its label entry lives.
fn fat32_root_dir_byte(head: &[u8]) -> Option<u64> {
    let boot: &[u8; 512] = head.get(..512)?.try_into().ok()?;
    let bpb = Fat32Bpb::parse(boot)?;
    bpb.cluster_byte(bpb.root_cluster)
}

/// The label a FAT directory's `entries` record: the first live volume-label
/// entry before the directory's end, trimmed of padding, or `None` when there
/// is none or it holds the "no label" placeholder.
fn fat32_directory_label(entries: &[u8]) -> Option<([u8; LABEL_MAX], u8)> {
    for entry in entries.as_chunks::<FAT_DIR_ENTRY_LEN>().0 {
        match entry[0] {
            FAT_END_OF_DIR => return None,
            FAT_DELETED_ENTRY => continue,
            _ => {}
        }
        let attributes = entry[11];
        if attributes & FAT_ATTR_LONG_NAME != FAT_ATTR_LONG_NAME
            && attributes & FAT_ATTR_VOLUME_ID != 0
        {
            return fat_label(&entry[..11]);
        }
    }
    None
}

/// A FAT label field, trimmed, unless it is blank or the placeholder.
fn fat_label(field: &[u8]) -> Option<([u8; LABEL_MAX], u8)> {
    let (label, len) = take_label(field);
    let text = &label[..usize::from(len)];
    (!text.is_empty() && text != FAT_NO_LABEL).then_some((label, len))
}

/// The stable FAT32 identity derived from a boot sector: the BPB volume
/// serial (4 bytes) + label (11 bytes) when the extended boot signature
/// (`0x29` at byte 66) declares them, else zeros, with a trailing tag byte
/// keeping the identity non-nil either way. FAT32 has no UUID;
/// `docs/src/filesystem/drives.md` §8 sanctions a content-derived identity
/// for formats without one.
#[must_use]
pub fn fat32_identity_from_boot(boot: &[u8; 512]) -> [u8; 16] {
    let mut identity = [0u8; 16];
    if boot[66] == 0x29 {
        identity[..4].copy_from_slice(&boot[67..71]);
        identity[4..15].copy_from_slice(&boot[71..82]);
    }
    identity[15] = 0xF3;
    identity
}

/// Render `identity` as its full display fingerprint: lowercase Crockford
/// base32 (digits and letters, minus the look-alikes `i`, `l`, `o`, `u`),
/// most-significant bits first. A caller takes the prefix it needs; the
/// full [`FINGERPRINT_CHARS`] characters encode every identity bit, so two
/// distinct identities always have distinct full fingerprints.
#[must_use]
pub fn fingerprint(identity: &[u8; 16]) -> [u8; FINGERPRINT_CHARS] {
    const ALPHABET: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";
    let mut out = [0u8; FINGERPRINT_CHARS];
    let mut acc: u32 = 0;
    let mut acc_bits: u32 = 0;
    let mut next = 0usize;
    for &byte in identity {
        acc = (acc << 8) | u32::from(byte);
        acc_bits += 8;
        while acc_bits >= 5 {
            acc_bits -= 5;
            out[next] = ALPHABET[usize::try_from((acc >> acc_bits) & 0x1F).unwrap_or(0)];
            next += 1;
        }
    }
    // 128 bits leave a 3-bit tail; pad it to a final character so every
    // identity bit is encoded.
    out[next] = ALPHABET[usize::try_from((acc << (5 - acc_bits)) & 0x1F).unwrap_or(0)];
    out
}

/// A content-derived 16-byte identity for a format that records no unique
/// identifier: the leading bytes of a SHA-256 over a domain tag and each
/// length-prefixed part, so no two layouts of parts digest the same message.
pub(crate) fn content_identity(domain: &[u8], parts: &[&[u8]]) -> [u8; 16] {
    let mut digest = Sha256Stream::new();
    digest.update(domain);
    for part in parts {
        let len = u32::try_from(part.len()).unwrap_or(u32::MAX);
        digest.update(&len.to_le_bytes());
        digest.update(part);
    }
    let mut identity = [0u8; 16];
    identity.copy_from_slice(&digest.finalize()[..16]);
    identity
}

/// Fill `buf` from byte `offset` of `dev`, answering `false` when the range
/// lies beyond the extent: a structure a device is too small to hold is
/// absent, not a fault.
///
/// # Errors
///
/// A device fault.
pub(crate) fn read_present<B: Block>(
    dev: &mut ByteDevice<B>,
    offset: u64,
    buf: &mut [u8],
) -> Result<bool, DriverError> {
    match dev.read(offset, buf) {
        Ok(()) => Ok(true),
        Err(DriverError::LengthOutOfRange) => Ok(false),
        Err(err) => Err(err),
    }
}

/// A zeroed `N`-byte buffer on the heap, refused rather than aborting when
/// memory is short: recognition runs beneath a filesystem attach, on a small
/// kernel stack.
pub(crate) fn heap_buf<const N: usize>() -> Result<Box<[u8; N]>, DriverError> {
    tairix_util::fallible::boxed_array(0u8).ok_or(DriverError::OutOfMemory)
}

/// Little-endian `u16` at `offset`, or `None` past the end.
pub(crate) fn le_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    let raw = bytes.get(offset..offset.checked_add(2)?)?;
    Some(u16::from_le_bytes([raw[0], raw[1]]))
}

/// Little-endian `u32` at `offset`, or `None` past the end.
pub(crate) fn le_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let raw = bytes.get(offset..offset.checked_add(4)?)?;
    Some(u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]))
}

/// Little-endian `u64` at `offset`, or `None` past the end.
fn le_u64(bytes: &[u8], offset: usize) -> Option<u64> {
    let raw = bytes.get(offset..offset.checked_add(8)?)?;
    let mut buf = [0u8; 8];
    buf.copy_from_slice(raw);
    Some(u64::from_le_bytes(buf))
}

/// Copy a fixed-width, padding-trimmed label field into a probe result's
/// label buffer. Trailing NULs and spaces are padding in every format that
/// records a label; interior bytes are kept verbatim (the naming policy above
/// this crate decides what is renderable).
fn take_label(field: &[u8]) -> ([u8; LABEL_MAX], u8) {
    let mut label = [0u8; LABEL_MAX];
    let trimmed_len = field
        .iter()
        .rposition(|&b| b != 0 && b != b' ')
        .map_or(0, |last| last + 1);
    let take = trimmed_len.min(LABEL_MAX);
    label[..take].copy_from_slice(&field[..take]);
    #[allow(clippy::cast_possible_truncation)] // `take <= LABEL_MAX` (252).
    (label, take as u8)
}

/// `ARXFS`: every metadata block opens with the header magic, and block 0
/// of a volume is a superblock-ring slot, so the head of a `ARXFS` extent
/// carries the magic, the block-type word and the volume UUID
/// (`docs/src/filesystem/arxfs-spec.md` §8). The payload is sealed; only the
/// identity, and the plaintext block size the ring is sized by, are read.
fn probe_arxfs(head: &[u8]) -> Option<ProbedVolume> {
    const ARXFS_SUPERBLOCK_TYPE: u32 = 1;
    if le_u64(head, 0)? != ARXFS_HEADER_MAGIC || le_u32(head, 8)? != ARXFS_SUPERBLOCK_TYPE {
        return None;
    }
    let identity: [u8; 16] = head.get(16..32)?.try_into().ok()?;
    // A nil UUID never identifies a real volume.
    if identity == [0u8; 16] {
        return None;
    }
    // Unauthenticated here, but it only sizes the window, and a lying value
    // fails the format's bounds and leaves no evidence.
    let evidence = le_u32(head, 128)
        .map(u64::from)
        .filter(|bs| bs.is_power_of_two() && (512..=4096).contains(bs))
        .map(|bs| Evidence {
            offset: 0,
            len: ARXFS_RING_BLOCKS * bs,
        });
    Some(ProbedVolume::new(
        VolumeFsType::ARXFS,
        identity,
        &[],
        evidence,
        None,
    ))
}

/// ext4 (and its ext2/ext3 ancestors): the superblock occupies bytes
/// 1024..2048 with `s_magic` at superblock offset `0x38`, `s_uuid` at
/// `0x68`, and `s_volume_name` at `0x78`. The magic alone is two bytes, so
/// the structural fields around it are sanity-checked before the match is
/// accepted. Every read-write mount rewrites the superblock's write time,
/// mount count and checksum, so it is the volume's mutation evidence.
fn probe_ext4(head: &[u8]) -> Option<ProbedVolume> {
    const SUPERBLOCK_OFFSET: usize = 1024;
    let sb = head.get(SUPERBLOCK_OFFSET..SUPERBLOCK_OFFSET + 1024)?;
    if le_u16(sb, 0x38)? != EXT4_SUPERBLOCK_MAGIC {
        return None;
    }
    // s_inodes_count and s_blocks_count_lo are never zero on a real volume,
    // and s_log_block_size beyond 6 (64 KiB blocks) is outside the format.
    if le_u32(sb, 0x00)? == 0 || le_u32(sb, 0x04)? == 0 || le_u32(sb, 0x18)? > 6 {
        return None;
    }
    let identity: [u8; 16] = sb.get(0x68..0x78)?.try_into().ok()?;
    let evidence = Some(Evidence {
        offset: 0,
        len: 2048,
    });
    Some(ProbedVolume::new(
        VolumeFsType::Ext4,
        identity,
        sb.get(0x78..0x88)?,
        evidence,
        None,
    ))
}

/// FAT32: the boot sector ends in `0x55AA`, carries the `"FAT32   "`
/// extended-BPB tag at byte 82, and its geometry fields must be structurally
/// sane — the tag alone is advisory. A writer updates `FSInfo`'s free-cluster
/// fields, so the reserved sectors through it are the mutation evidence:
/// honestly weaker than the other formats', and the strongest FAT offers.
fn probe_fat32(head: &[u8]) -> Option<ProbedVolume> {
    let boot: &[u8; 512] = head.get(..512)?.try_into().ok()?;
    if &boot[82..90] != b"FAT32   " {
        return None;
    }
    let bpb = Fat32Bpb::parse(boot)?;
    let identity = fat32_identity_from_boot(boot);
    // The label field exists only under the 0x29 extended boot signature.
    let label = if boot[66] == 0x29 {
        fat_label(&boot[71..82])
    } else {
        None
    };
    let (label, label_len) = label.unwrap_or(([0u8; LABEL_MAX], 0));
    // FSInfo lives in the reserved region; a boot sector that points
    // elsewhere is lying.
    let fsinfo = u64::from(le_u16(boot, 48)?);
    let evidence = (fsinfo != 0 && fsinfo < bpb.reserved_sectors)
        .then(|| (fsinfo + 1) * u64::from(bpb.bytes_per_sector))
        .filter(|&len| len <= EVIDENCE_MAX)
        .map(|len| Evidence { offset: 0, len });
    Some(ProbedVolume {
        label,
        label_len,
        ..ProbedVolume::new(VolumeFsType::Fat32, identity, &[], evidence, None)
    })
}
