//! Filesystem recognition: which supported filesystem an extent of a block
//! device holds, and the identity, label and mutation evidence it carries
//! (`plans/DEVICES.md` D3c, `plans/FOREIGN-MEDIA.md`).
//!
//! [`recognise`] is the one decision the volume manager, the kernel's attach
//! path and the RAID administrator share. The filesystem drivers import the
//! constants and recognisers it reads by — the `ARXFS` header magic, the ext
//! superblock magic, the FAT32 parameter block and identity, and the
//! [`adfs`], [`affs`], [`iso9660`] and [`udf`] recognisers — so a probe and a
//! mounted driver never disagree about what a volume is or which identity it
//! carries. A disk an Amiga Rigid Disk Block partitions is told apart as a
//! partition table, not a volume.
//!
//! Everything read is untrusted removable-media content: every access is
//! bounds-checked, every field validated before use, and anything that does
//! not match a supported format exactly is `None` — never a guess.
//!
//! [`fingerprint`] renders an identity as the short display fingerprint
//! (`plans/ALIAS.md` §3.8), and [`twin_identity`] keys a volume attached
//! beside another of its identity.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

pub mod adfs;
pub mod affs;
pub mod archive;
pub mod iso9660;
pub mod name;
pub mod udf;

use alloc::boxed::Box;

use tairix_abi::blkio::BlkForm;
use tairix_abi::driver::block::{Block, DiscLayout};
use tairix_abi::driver::filesystem::{VolumeTitle, VOLUME_TITLE_MAX};
use tairix_abi::volume::VolumeFsType;
use tairix_abi::DriverError;
use tairix_bytedev::ByteDevice;
use tairix_crypto::Fingerprint;
use tairix_fat::boot::{at, Bpb, Kind, BOOT_SECTOR, LABEL_WINDOW};
use tairix_fat::dir::{slot, Slot, ENTRY};
use tairix_fat::name::NO_LABEL;

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

/// Maximum bytes of a recognised volume label: the longest title any
/// supported format records ([`VOLUME_TITLE_MAX`]).
pub const LABEL_MAX: usize = VOLUME_TITLE_MAX;

/// Characters of the full identity fingerprint: 16 identity bytes = 128
/// bits, at five bits per base32 character, rounded up.
pub const FINGERPRINT_CHARS: usize = 26;

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

/// Logical slots of the `ARXFS` superblock ring, each a mirrored pair whose
/// first block is its primary.
const ARXFS_RING_SLOTS: u64 = ARXFS_RING_BLOCKS / 2;

/// The `ARXFS` block type of a superblock-ring slot.
const ARXFS_SUPERBLOCK_TYPE: u32 = 1;

/// Offset, within an `ARXFS` metadata block, of the generation its header
/// carries: the transaction that wrote it.
pub const ARXFS_GENERATION_AT: usize = 40;

/// Offset, within an `ARXFS` superblock slot, of the volume's title field: its
/// length byte, then that many bytes of UTF-8, zero to the field's end
/// (`docs/src/filesystem/arxfs-spec.md` §4).
pub const ARXFS_TITLE_AT: usize = 256;

/// Bytes of an `ARXFS` superblock slot's title field: the length byte and the
/// longest title.
pub const ARXFS_TITLE_LEN: usize = 1 + VOLUME_TITLE_MAX;

/// How much of an `ARXFS` superblock slot its title is read from: its header
/// through its title field.
const ARXFS_SLOT_HEAD: usize = ARXFS_TITLE_AT + ARXFS_TITLE_LEN;

/// Offset, within an ext4 superblock, of its NUL-padded volume name,
/// `s_volume_name`.
pub const EXT4_VOLUME_NAME_AT: usize = 0x78;

/// Bytes of an ext4 superblock's volume name.
pub const EXT4_VOLUME_NAME_LEN: usize = 16;

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
    /// A disk an Amiga Rigid Disk Block partitions: it holds volumes this
    /// build does not attach, so it is neither attached nor blank.
    RigidDiskBlock,
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
    /// integrity sequence, the AmigaDOS root block. `None` where the format
    /// has no region every writer rewrites (ISO 9660 is never written; ADFS
    /// keeps none) or declares an implausible one — no evidence means no
    /// replay.
    pub evidence: Option<Evidence>,
    /// How far into the extent the volume's structures reach, for the
    /// formats whose structures do not open the extent (ISO 9660 and UDF), so
    /// a partition table found beside them can be weighed against them.
    pub span: Option<u64>,
    /// The medium the volume was laid out for, where its format says: a disc
    /// for ISO 9660 and for UDF's 2048-byte sectors, a floppy for an ADFS or
    /// AmigaDOS floppy format.
    pub form: BlkForm,
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
            form: BlkForm::Unspecified,
            label,
            label_len,
        }
    }

    const fn laid_out_for(self, form: BlkForm) -> Self {
        Self { form, ..self }
    }

    /// The volume's human-facing label, trimmed of the format's padding;
    /// empty when none is set, the field is blank, or it holds the format's
    /// "no label" placeholder. An `ARXFS` title, whose field has no padding,
    /// is kept exactly.
    #[must_use]
    pub fn label(&self) -> &[u8] {
        &self.label[..usize::from(self.label_len)]
    }
}

/// Recognise what the extent `dev` holds, in a fixed order: a RAID array
/// member first, then the formats whose signature opens the extent
/// (`ARXFS`, ext4, FAT32 — most specific first), then the disc formats whose
/// structures lie 32 KiB in (ISO 9660 and UDF), then AmigaDOS, whose `DOS`
/// boot block names a checksummed root at the volume's middle, then ADFS,
/// whose oldest layout carries the weakest signature, and last an Amiga Rigid
/// Disk Block, which partitions the extent rather than holding one volume.
/// `None` when nothing matches.
///
/// A FAT32 volume is labelled from its root directory's label entry, which a
/// relabel rewrites, over the boot sector's copy, and an `ARXFS` volume from
/// its superblock ring's newest slot. A disc carrying both ISO
/// 9660 and UDF over the same files (a bridge disc) is recognised as UDF,
/// whose names and metadata are the richer, unless its ISO 9660 tree carries
/// what UDF would lose: Rock Ridge's POSIX records or RISC OS filetypes.
///
/// # Errors
///
/// A device fault, or a refused allocation. A structure the extent is too
/// small to hold is simply absent.
pub fn recognise<B: Block>(dev: &mut ByteDevice<B>) -> Result<Option<Recognised>, DriverError> {
    let (head, len) = read_upto::<_, HEAD_LEN>(dev, 0)?;
    let head = &head[..len];
    if let Some(array) = raid_member(head) {
        return Ok(Some(Recognised::RaidMember(array)));
    }
    let found = match head_volume(head) {
        Some(mut volume) => {
            let found = match volume.fstype {
                VolumeFsType::Fat32 => directory_label(dev, head)?,
                VolumeFsType::ARXFS => arxfs_ring_label(dev, head)?,
                _ => None,
            };
            if let Some((label, label_len)) = found {
                volume.label = label;
                volume.label_len = label_len;
            }
            Some(volume)
        }
        None => match disc_volume(dev)? {
            Some(volume) => Some(volume),
            None => match affs_volume(dev)? {
                Some(volume) => Some(volume),
                None => adfs::identify(dev)?.map(|layout| {
                    let (label, len) = layout.label();
                    ProbedVolume::new(
                        VolumeFsType::Adfs,
                        layout.identity(),
                        &label[..len],
                        None,
                        None,
                    )
                    .laid_out_for(adfs_form(&layout))
                }),
            },
        },
    };
    if let Some(volume) = found {
        return Ok(Some(Recognised::Volume(volume)));
    }
    Ok(affs::rigid_disk_block(dev)?.then_some(Recognised::RigidDiskBlock))
}

/// An AmigaDOS volume, labelled with its name in UTF-8 and evidenced by its
/// root block, which every writer restamps with the volume's alteration date.
fn affs_volume<B: Block>(dev: &mut ByteDevice<B>) -> Result<Option<ProbedVolume>, DriverError> {
    let Some(layout) = affs::identify(dev)? else {
        return Ok(None);
    };
    let mut label = [0u8; 2 * affs::NAME_MAX];
    let mut len = 0;
    for &byte in layout.name() {
        len += char::from(byte).encode_utf8(&mut label[len..]).len();
    }
    let evidence = layout
        .root
        .checked_mul(u64::from(layout.block_size))
        .map(|offset| Evidence {
            offset,
            len: u64::from(layout.block_size),
        })
        .filter(|evidence| evidence.len <= EVIDENCE_MAX);
    Ok(Some(
        ProbedVolume::new(
            VolumeFsType::Affs,
            layout.identity(),
            &label[..len],
            evidence,
            None,
        )
        .laid_out_for(layout.form()),
    ))
}

/// The medium an ADFS volume was laid out for: a new-map disc record names a
/// density for a floppy and none for a hard disc; the old map's formats are
/// the floppies up to the D format's 800 KiB.
fn adfs_form(layout: &adfs::Layout) -> BlkForm {
    let floppy = match layout {
        adfs::Layout::NewMap { record, .. } => record.density != 0,
        adfs::Layout::OldMap { disc_sectors, .. } => {
            u64::from(*disc_sectors) * adfs::OLD_SECTOR_SIZE <= adfs::OLD_MAP_FLOPPY_MAX
        }
    };
    if floppy {
        BlkForm::FloppyDisk
    } else {
        BlkForm::Unspecified
    }
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
            return Ok(Some(
                ProbedVolume::new(
                    VolumeFsType::Udf,
                    udf.identity(),
                    &label[..len],
                    evidence,
                    Some(udf.span_bytes()),
                )
                .laid_out_for(udf_form(&udf)),
            ));
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
        .laid_out_for(BlkForm::OpticalDisc)
    }))
}

/// The medium a UDF volume was laid out for: a disc's 2048-byte sectors, or
/// unstated for the 512-byte blocks of a disk.
fn udf_form(volume: &udf::Volume) -> BlkForm {
    if volume.block_size() == 2048 {
        BlkForm::OpticalDisc
    } else {
        BlkForm::Unspecified
    }
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

/// The label a FAT32 volume's root directory records, read from the window
/// its boot sector `head` places ([`Bpb::label_window`]); `None` when the
/// window lies beyond the extent or records none.
fn directory_label<B: Block>(
    dev: &mut ByteDevice<B>,
    head: &[u8],
) -> Result<Option<([u8; LABEL_MAX], u8)>, DriverError> {
    let Some((root, window)) = fat32_bpb(head).and_then(|bpb| bpb.label_window()) else {
        return Ok(None);
    };
    let (entries, read) = read_upto::<_, LABEL_WINDOW>(dev, root)?;
    let take = read.min(window);
    if take == 0 {
        return Ok(None);
    }
    Ok(fat32_directory_label(&entries[..take]))
}

/// The FAT32 volume boot sector `head` opens, if it opens one.
#[must_use]
pub fn fat32_bpb(head: &[u8]) -> Option<Bpb> {
    let boot: &[u8; BOOT_SECTOR] = head.get(..BOOT_SECTOR)?.try_into().ok()?;
    Bpb::parse(boot).filter(|bpb| bpb.kind == Kind::Fat32)
}

/// A FAT32 volume's stable identity: a digest of its serial, where the
/// extended boot signature declares one, and of the geometry formatting
/// fixes. Neither a write nor a relabel changes any of them — a relabel
/// rewrites `BS_VolLab` beside the serial — so two volumes formatted alike
/// under one serial share it. FAT32 has no UUID;
/// `docs/src/filesystem/drives.md` §8 sanctions a content-derived identity
/// for formats without one.
#[must_use]
pub fn fat32_identity(bpb: &Bpb) -> [u8; 16] {
    let serial = bpb.serial.map(u32::to_le_bytes);
    content_identity(
        b"tairix.fat32",
        &[
            serial.as_ref().map_or(&[][..], |serial| &serial[..]),
            &u32::from(bpb.bytes_per_sector).to_le_bytes(),
            &u32::from(bpb.sectors_per_cluster).to_le_bytes(),
            &u64::from(bpb.reserved_sectors).to_le_bytes(),
            &u64::from(bpb.fats).to_le_bytes(),
            &u64::from(bpb.fat_sectors).to_le_bytes(),
            &bpb.root_cluster.to_le_bytes(),
        ],
    )
}

/// The label a FAT directory's `entries` record: the first live volume-label
/// entry before the directory's end, trimmed of padding, or `None` when there
/// is none or it holds the "no label" placeholder.
fn fat32_directory_label(entries: &[u8]) -> Option<([u8; LABEL_MAX], u8)> {
    for entry in entries.as_chunks::<ENTRY>().0 {
        match slot(entry) {
            Slot::End => return None,
            Slot::Label => return fat_label(&entry[..11]),
            Slot::Deleted | Slot::Long | Slot::Short => {}
        }
    }
    None
}

/// A FAT label field, trimmed, unless it is blank or the placeholder.
fn fat_label(field: &[u8]) -> Option<([u8; LABEL_MAX], u8)> {
    let (label, len) = take_label(field);
    let text = &label[..usize::from(len)];
    (!text.is_empty() && text != NO_LABEL).then_some((label, len))
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
/// identifier: its fingerprint under a domain tag over each length-prefixed
/// part, so no two layouts of parts feed the same message.
pub(crate) fn content_identity(domain: &[u8], parts: &[&[u8]]) -> [u8; 16] {
    let mut fingerprint = Fingerprint::new(domain);
    for part in parts {
        let len = u32::try_from(part.len()).unwrap_or(u32::MAX);
        fingerprint.field(&len.to_le_bytes());
        fingerprint.field(part);
    }
    fingerprint.finish()
}

/// The key a twin of the volume `identity` is published under while attached
/// as `attachment`: derived from both under a domain of its own, so
/// attachments numbered apart are keyed apart (`docs/src/filesystem/drives.md`
/// §8).
#[must_use]
pub fn twin_identity(identity: &[u8; 16], attachment: u64) -> [u8; 16] {
    content_identity(
        b"tairix.volume.twin",
        &[identity.as_slice(), &attachment.to_le_bytes()],
    )
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

/// Up to `N` bytes of `dev` from `offset`, in one read: all of them, or what
/// the extent holds past `offset`, nothing being read at or past its end.
/// Answers the buffer and how much of it was filled.
///
/// # Errors
///
/// A device fault, or a refused allocation.
pub(crate) fn read_upto<B: Block, const N: usize>(
    dev: &mut ByteDevice<B>,
    offset: u64,
) -> Result<(Box<[u8; N]>, usize), DriverError> {
    let mut buf = heap_buf::<N>()?;
    let len = usize::try_from(dev.capacity().saturating_sub(offset)).map_or(N, |left| left.min(N));
    if len > 0 {
        dev.read(offset, &mut buf[..len])?;
    }
    Ok((buf, len))
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
pub(crate) fn le_u64(bytes: &[u8], offset: usize) -> Option<u64> {
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
    let trimmed_len = field
        .iter()
        .rposition(|&b| b != 0 && b != b' ')
        .map_or(0, |last| last + 1);
    exact_label(&field[..trimmed_len])
}

/// A probe result's label buffer holding `text` as it is, cut to
/// [`LABEL_MAX`] bytes.
fn exact_label(text: &[u8]) -> ([u8; LABEL_MAX], u8) {
    let mut label = [0u8; LABEL_MAX];
    let take = text.len().min(LABEL_MAX);
    label[..take].copy_from_slice(&text[..take]);
    #[allow(clippy::cast_possible_truncation)] // `take <= LABEL_MAX` (252).
    (label, take as u8)
}

/// The title an `ARXFS` superblock slot's title `field` records, or `None` for
/// a field no writer leaves: a length past the field, a byte past the length,
/// or text that is no title.
#[must_use]
pub fn arxfs_title(field: &[u8; ARXFS_TITLE_LEN]) -> Option<VolumeTitle> {
    let (&len, text) = field.split_first()?;
    let (title, rest) = text.split_at_checked(usize::from(len))?;
    if rest.iter().any(|&byte| byte != 0) {
        return None;
    }
    VolumeTitle::new(core::str::from_utf8(title).ok()?).ok()
}

/// The title field an `ARXFS` superblock slot records `title` in.
#[must_use]
// A title holds at most `VOLUME_TITLE_MAX` (252) bytes, so its length fits a
// byte.
#[allow(clippy::cast_possible_truncation)]
pub fn arxfs_title_field(title: &VolumeTitle) -> [u8; ARXFS_TITLE_LEN] {
    let text = title.as_str().as_bytes();
    let mut field = [0u8; ARXFS_TITLE_LEN];
    field[0] = text.len() as u8;
    field[1..=text.len()].copy_from_slice(text);
    field
}

/// The block size an `ARXFS` head declares, where the format allows it.
/// Unauthenticated here, but it only places structures, and a lying value
/// fails the format's bounds and places none.
fn arxfs_block_size(head: &[u8]) -> Option<u32> {
    le_u32(head, 128).filter(|bs| bs.is_power_of_two() && (512..=4096).contains(bs))
}

/// The label the newest slot of the `ARXFS` ring opening with `head` titles
/// the volume with, kept exactly. Slots are read without their seals, as
/// every reading here is: one whose header names another volume, or whose
/// title field no writer leaves, is passed over, and a block size the format
/// does not allow places no slot.
fn arxfs_ring_label<B: Block>(
    dev: &mut ByteDevice<B>,
    head: &[u8],
) -> Result<Option<([u8; LABEL_MAX], u8)>, DriverError> {
    let (Some(block_size), Some(uuid)) = (arxfs_block_size(head), head.get(16..32)) else {
        return Ok(None);
    };
    let mut newest: Option<(u64, VolumeTitle)> = None;
    for slot in 0..ARXFS_RING_SLOTS {
        let (block, len) = read_upto::<_, ARXFS_SLOT_HEAD>(dev, slot * 2 * u64::from(block_size))?;
        let block = &block[..len];
        let ours = le_u64(block, 0) == Some(ARXFS_HEADER_MAGIC)
            && le_u32(block, 8) == Some(ARXFS_SUPERBLOCK_TYPE)
            && block.get(16..32) == Some(uuid);
        let generation = le_u64(block, ARXFS_GENERATION_AT).filter(|_| ours);
        let title = block
            .get(ARXFS_TITLE_AT..ARXFS_SLOT_HEAD)
            .and_then(|field| <&[u8; ARXFS_TITLE_LEN]>::try_from(field).ok())
            .and_then(arxfs_title);
        if let (Some(generation), Some(title)) = (generation, title) {
            if newest.is_none_or(|(held, _)| generation > held) {
                newest = Some((generation, title));
            }
        }
    }
    Ok(newest.map(|(_, title)| exact_label(title.as_str().as_bytes())))
}

/// `ARXFS`: every metadata block opens with the header magic, and block 0
/// of a volume is a superblock-ring slot, so the head of a `ARXFS` extent
/// carries the magic, the block-type word and the volume UUID
/// (`docs/src/filesystem/arxfs-spec.md` §8). The payload is sealed; only the
/// identity, and the plaintext block size the ring is sized by, are read.
fn probe_arxfs(head: &[u8]) -> Option<ProbedVolume> {
    if le_u64(head, 0)? != ARXFS_HEADER_MAGIC || le_u32(head, 8)? != ARXFS_SUPERBLOCK_TYPE {
        return None;
    }
    let identity: [u8; 16] = head.get(16..32)?.try_into().ok()?;
    // A nil UUID never identifies a real volume.
    if identity == [0u8; 16] {
        return None;
    }
    let evidence = arxfs_block_size(head).map(|bs| Evidence {
        offset: 0,
        len: ARXFS_RING_BLOCKS * u64::from(bs),
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
        sb.get(EXT4_VOLUME_NAME_AT..EXT4_VOLUME_NAME_AT + EXT4_VOLUME_NAME_LEN)?,
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
    let tag = head.get(at::TYPE_32..at::TYPE_32 + 8)?;
    if tag != Kind::Fat32.type_name() {
        return None;
    }
    let bpb = fat32_bpb(head)?;
    let identity = fat32_identity(&bpb);
    let label = bpb.label.and_then(|field| fat_label(&field));
    let (label, label_len) = label.unwrap_or(([0u8; LABEL_MAX], 0));
    // FSInfo lives in the reserved region; a boot sector that points
    // elsewhere is lying.
    let fsinfo = u64::from(bpb.fsinfo_sector);
    let evidence = (fsinfo != 0 && fsinfo < u64::from(bpb.reserved_sectors))
        .then(|| (fsinfo + 1) * bpb.sector_bytes())
        .filter(|&len| len <= EVIDENCE_MAX)
        .map(|len| Evidence { offset: 0, len });
    Some(ProbedVolume {
        label,
        label_len,
        ..ProbedVolume::new(VolumeFsType::Fat32, identity, &[], evidence, None)
    })
}
