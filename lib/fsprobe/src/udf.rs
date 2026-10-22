//! UDF (ECMA-167 3rd edition, OSTA UDF 1.02–2.60) recognition, and the
//! descriptor handling the probe and the UDF driver share: the volume
//! recognition sequence, the anchor and the block size it fixes, descriptor
//! tags and their CRC, the prevailing volume descriptors, OSTA Compressed
//! Unicode, and the identity and label a volume carries.
//!
//! References: ECMA-167 3rd edition (1997) parts 2–4; OSTA UDF 2.60 §2 and
//! §6.11; OSTA UDF 1.50 for the earlier revisions' differences.

use alloc::boxed::Box;
use alloc::vec::Vec;

use tairix_abi::driver::block::{Block, DiscLayout};
use tairix_abi::DriverError;
use tairix_bytedev::ByteDevice;

use crate::{
    content_identity, heap_buf, le_u16, le_u32, read_present, session_bytes, LABEL_MAX,
    SYSTEM_AREA_LEN,
};

/// Primary Volume Descriptor.
pub const TAG_PVD: u16 = 1;
/// Anchor Volume Descriptor Pointer.
pub const TAG_AVDP: u16 = 2;
/// Volume Descriptor Pointer.
pub const TAG_VDP: u16 = 3;
/// Implementation Use Volume Descriptor.
pub const TAG_IUVD: u16 = 4;
/// Partition Descriptor.
pub const TAG_PD: u16 = 5;
/// Logical Volume Descriptor.
pub const TAG_LVD: u16 = 6;
/// Unallocated Space Descriptor.
pub const TAG_USD: u16 = 7;
/// Terminating Descriptor.
pub const TAG_TD: u16 = 8;
/// Logical Volume Integrity Descriptor.
pub const TAG_LVID: u16 = 9;
/// File Set Descriptor.
pub const TAG_FSD: u16 = 256;
/// File Identifier Descriptor.
pub const TAG_FID: u16 = 257;
/// Allocation Extent Descriptor.
pub const TAG_AED: u16 = 258;
/// Indirect Entry.
pub const TAG_IE: u16 = 259;
/// Terminal Entry.
pub const TAG_TE: u16 = 260;
/// File Entry.
pub const TAG_FE: u16 = 261;
/// Extended Attribute Header Descriptor.
pub const TAG_EAHD: u16 = 262;
/// Space Bitmap Descriptor.
pub const TAG_SBD: u16 = 264;
/// Extended File Entry.
pub const TAG_EFE: u16 = 266;

/// Bytes of a descriptor tag.
pub const TAG_LEN: usize = 16;

/// Bytes of the volume descriptors kept whole, all of whose fields lie in
/// their first 512 bytes: the smallest logical block UDF allows.
pub const DESCRIPTOR_LEN: usize = 512;

/// Where a Logical Volume Descriptor's partition map table begins.
pub const LVD_MAPS_AT: usize = 440;

/// Byte offset, from a session's start, where the volume recognition
/// sequence begins: past the system area.
pub const VRS_START: u64 = SYSTEM_AREA_LEN;

/// Most volume structure descriptors read before a recognition sequence is
/// given up on: a validation bound on untrusted media, not a capacity.
pub const MAX_VRS: u64 = 64;

/// The sector, counted from a session's start, where the first anchor lies.
pub const ANCHOR_SECTOR: u64 = 256;

/// The anchor an unclosed write-once disc may carry instead.
pub const UNCLOSED_ANCHOR_SECTOR: u64 = 512;

/// Most sectors read along one volume descriptor sequence, its continuations
/// included: a validation bound on untrusted media.
pub const MAX_VDS_SECTORS: u64 = 256;

/// Most partition descriptors one volume may prevail with: UDF allows one,
/// or a read-only and a writable pair.
pub const MAX_PARTITIONS: usize = 4;

/// The domain a UDF logical volume declares.
pub const DOMAIN_ID: &[u8] = b"*OSTA UDF Compliant";

/// Logical block sizes a UDF volume may be recorded with.
const BLOCK_SIZES: [u32; 4] = [512, 1024, 2048, 4096];

/// CRC-ITU-T (x^16 + x^12 + x^5 + 1) over `data`: initial value zero,
/// most significant bit first, no final inversion — ECMA-167 3/7.2.6, whose
/// test vector `CRC(70 6A 77) = 3299` the tests hold it to.
#[must_use]
pub fn crc16(data: &[u8]) -> u16 {
    data.iter().fold(0u16, |crc, &byte| {
        let index = usize::from(crc.to_be_bytes()[0] ^ byte);
        (crc << 8) ^ CRC_TABLE[index]
    })
}

/// The CRC-ITU-T byte table: the polynomial applied to every high byte.
const CRC_TABLE: [u16; 256] = {
    let mut table = [0u16; 256];
    let mut high: u16 = 0;
    while high < 256 {
        let mut crc = high << 8;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
            bit += 1;
        }
        // `From` is not callable in a const initializer; `high` is below 256.
        #[allow(clippy::cast_lossless)]
        {
            table[high as usize] = crc;
        }
        high += 1;
    }
    table
};

/// A verified descriptor tag.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Tag {
    /// The descriptor's kind.
    pub id: u16,
    /// 2 for the ECMA-167 2nd-edition structures (`NSR02`), 3 for the 3rd.
    pub version: u16,
    /// The serial number a set of descriptors was recorded with.
    pub serial: u16,
    /// The block the tag says it was recorded in.
    pub location: u32,
}

/// The checksum of a tag: its bytes but the checksum itself, summed modulo
/// 256.
#[must_use]
pub fn tag_checksum(tag: &[u8; TAG_LEN]) -> u8 {
    tag[..4]
        .iter()
        .chain(&tag[5..])
        .fold(0u8, |sum, &b| sum.wrapping_add(b))
}

impl Tag {
    /// Verify the descriptor opening `bytes`, recorded at block `location`:
    /// [`Self::checked`]'s checks and the location it claims. `None` when
    /// any check fails; a descriptor is never half-trusted.
    #[must_use]
    pub fn verify(bytes: &[u8], location: u32) -> Option<Self> {
        Self::checked(bytes).filter(|tag| tag.location == location)
    }

    /// The tag opening `bytes`, its checksum, version and the CRC over the
    /// body bytes it covers — which must lie within `bytes` — verified, for
    /// a descriptor whose location is checked by its caller, or that has
    /// none to check.
    #[must_use]
    pub fn checked(bytes: &[u8]) -> Option<Self> {
        let tag: &[u8; TAG_LEN] = bytes.get(..TAG_LEN)?.try_into().ok()?;
        if tag_checksum(tag) != tag[4] {
            return None;
        }
        let word = |at: usize| u16::from_le_bytes([tag[at], tag[at + 1]]);
        let version = word(2);
        if version != 2 && version != 3 {
            return None;
        }
        let crc_len = usize::from(word(10));
        let body = bytes.get(TAG_LEN..TAG_LEN + crc_len)?;
        if crc16(body) != word(8) {
            return None;
        }
        Some(Self {
            id: word(0),
            version,
            serial: word(6),
            location: u32::from_le_bytes([tag[12], tag[13], tag[14], tag[15]]),
        })
    }

    /// Write this tag over the first [`TAG_LEN`] bytes of `bytes`, its CRC
    /// covering the `crc_len` body bytes that follow and its checksum last —
    /// the one encoding [`Self::verify`] accepts.
    ///
    /// # Errors
    ///
    /// [`DriverError::LengthOutOfRange`] when the body does not fit `bytes`
    /// or a CRC length field.
    pub fn seal(&self, bytes: &mut [u8], crc_len: usize) -> Result<(), DriverError> {
        let len_field = u16::try_from(crc_len).map_err(|_| DriverError::LengthOutOfRange)?;
        let body = bytes
            .get(TAG_LEN..TAG_LEN + crc_len)
            .ok_or(DriverError::LengthOutOfRange)?;
        let crc = crc16(body);
        let tag: &mut [u8; TAG_LEN] = bytes
            .get_mut(..TAG_LEN)
            .and_then(|tag| tag.try_into().ok())
            .ok_or(DriverError::LengthOutOfRange)?;
        tag[0..2].copy_from_slice(&self.id.to_le_bytes());
        tag[2..4].copy_from_slice(&self.version.to_le_bytes());
        tag[5] = 0;
        tag[6..8].copy_from_slice(&self.serial.to_le_bytes());
        tag[8..10].copy_from_slice(&crc.to_le_bytes());
        tag[10..12].copy_from_slice(&len_field.to_le_bytes());
        tag[12..16].copy_from_slice(&self.location.to_le_bytes());
        tag[4] = tag_checksum(tag);
        Ok(())
    }
}

/// An extent in bytes from a sector, as volume structures record it
/// (`extent_ad`, ECMA-167 3/7.1).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct ExtentAd {
    /// The extent's length, in bytes.
    pub length: u32,
    /// The extent's first sector.
    pub location: u32,
}

impl ExtentAd {
    /// The `extent_ad` at `at` in `bytes`, `None` past the end.
    #[must_use]
    pub fn at(bytes: &[u8], at: usize) -> Option<Self> {
        Some(Self {
            length: le_u32(bytes, at)?,
            location: le_u32(bytes, at + 4)?,
        })
    }
}

/// An extent of a partition, with the flags its length's top bits carry
/// (`short_ad`, ECMA-167 4/14.14.1).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct ShortAd {
    /// The length field, its two type bits included.
    pub length: u32,
    /// The extent's first block, in its partition.
    pub position: u32,
}

impl ShortAd {
    /// The `short_ad` at `at` in `bytes`, `None` past the end.
    #[must_use]
    pub fn at(bytes: &[u8], at: usize) -> Option<Self> {
        Some(Self {
            length: le_u32(bytes, at)?,
            position: le_u32(bytes, at + 4)?,
        })
    }

    /// The extent's length in bytes, its type bits cleared.
    #[must_use]
    pub const fn bytes(&self) -> u32 {
        self.length & 0x3FFF_FFFF
    }
}

/// An extent anywhere in the logical volume (`long_ad`, ECMA-167 4/14.14.2).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct LongAd {
    /// The length field, its two type bits included.
    pub length: u32,
    /// The extent's first block, in its partition.
    pub block: u32,
    /// The partition reference number of that partition.
    pub partition: u16,
    /// Implementation use: UDF keeps flags and a unique id here.
    pub impl_use: [u8; 6],
}

impl LongAd {
    /// The `long_ad` at `at` in `bytes`, `None` past the end.
    #[must_use]
    pub fn at(bytes: &[u8], at: usize) -> Option<Self> {
        Some(Self {
            length: le_u32(bytes, at)?,
            block: le_u32(bytes, at + 4)?,
            partition: le_u16(bytes, at + 8)?,
            impl_use: bytes.get(at + 10..at + 16)?.try_into().ok()?,
        })
    }

    /// The extent's length in bytes, its type bits cleared.
    #[must_use]
    pub const fn bytes(&self) -> u32 {
        self.length & 0x3FFF_FFFF
    }
}

/// A verified anchor: the block size it fixed and the two sequences it names.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Anchor {
    /// Bytes per logical sector, which UDF makes the logical block too.
    pub block_size: u32,
    /// The sector the anchor was found at.
    pub sector: u64,
    /// The main volume descriptor sequence.
    pub main: ExtentAd,
    /// Its reserve copy.
    pub reserve: ExtentAd,
}

/// The prevailing Logical Volume Descriptor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LogicalVolume {
    /// Bytes per logical block.
    pub block_size: u32,
    /// The UDF revision the domain names, as BCD (`0x0260` for 2.60), or
    /// zero for a volume that names no domain.
    pub revision: u16,
    /// The domain flags: bit 0 a hard write-protect, bit 1 a soft one.
    pub domain_flags: u8,
    /// Where the File Set Descriptor is.
    pub file_set: LongAd,
    /// The Logical Volume Integrity Descriptor sequence.
    pub integrity: ExtentAd,
    /// How many partition maps the table holds.
    pub map_count: u32,
    identifier: [u8; 128],
    maps: Vec<u8>,
}

impl LogicalVolume {
    /// The partition map table.
    #[must_use]
    pub fn maps(&self) -> &[u8] {
        &self.maps
    }

    /// The logical volume identifier, a `dstring`.
    #[must_use]
    pub fn identifier(&self) -> &[u8; 128] {
        &self.identifier
    }
}

/// Domain flag: the volume is permanently write-protected.
pub const DOMAIN_HARD_WRITE_PROTECT: u8 = 1 << 0;
/// Domain flag: the volume is write-protected until a user lifts it.
pub const DOMAIN_SOFT_WRITE_PROTECT: u8 = 1 << 1;

/// A prevailing Partition Descriptor.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Partition {
    /// The partition number the maps refer to it by.
    pub number: u16,
    /// How the partition may be recorded: 1 read-only, 2 write-once, 3
    /// rewritable, 4 overwritable, 0 pseudo-overwritable.
    pub access: u32,
    /// Its first sector.
    pub start: u32,
    /// Its length in sectors.
    pub length: u32,
    /// The unallocated space table, when the partition keeps one.
    pub unallocated_table: ShortAd,
    /// The unallocated space bitmap, when the partition keeps one.
    pub unallocated_bitmap: ShortAd,
    /// The freed space table, when the partition keeps one.
    pub freed_table: ShortAd,
    /// The freed space bitmap, when the partition keeps one.
    pub freed_bitmap: ShortAd,
}

/// Partition access type: recorded once and never changed.
pub const ACCESS_READ_ONLY: u32 = 1;
/// Partition access type: each block written at most once.
pub const ACCESS_WRITE_ONCE: u32 = 2;
/// Partition access type: blocks may be rewritten, a limited number of times.
pub const ACCESS_REWRITABLE: u32 = 3;
/// Partition access type: blocks may be rewritten without limit.
pub const ACCESS_OVERWRITABLE: u32 = 4;

impl Partition {
    fn parse(pd: &[u8]) -> Option<Self> {
        Some(Self {
            number: le_u16(pd, 22)?,
            access: le_u32(pd, 184)?,
            start: le_u32(pd, 188)?,
            length: le_u32(pd, 192)?,
            unallocated_table: ShortAd::at(pd, 56)?,
            unallocated_bitmap: ShortAd::at(pd, 64)?,
            freed_table: ShortAd::at(pd, 80)?,
            freed_bitmap: ShortAd::at(pd, 88)?,
        })
    }
}

/// A recognised UDF volume.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Volume {
    /// The sector the session's recognition sequence and anchor are counted
    /// from.
    pub session_start: u64,
    /// `NSR02` (2) or `NSR03` (3).
    pub nsr: u8,
    /// The anchor.
    pub anchor: Anchor,
    /// The furthest sector an anchor verified at: the volume's last.
    furthest_anchor: u64,
    pvd: Box<[u8; DESCRIPTOR_LEN]>,
    logical: LogicalVolume,
    partitions: Vec<Partition>,
}

impl Volume {
    /// Bytes per logical block.
    #[must_use]
    pub fn block_size(&self) -> u32 {
        self.anchor.block_size
    }

    /// The prevailing logical volume.
    #[must_use]
    pub fn logical(&self) -> &LogicalVolume {
        &self.logical
    }

    /// The prevailing partitions, one per partition number.
    #[must_use]
    pub fn partitions(&self) -> &[Partition] {
        &self.partitions
    }

    /// The volume's stable identity: a digest of the volume set identifier,
    /// whose leading characters UDF makes unique, and the instant the volume
    /// was recorded — neither of which a file's writes or a relabel change.
    #[must_use]
    pub fn identity(&self) -> [u8; 16] {
        content_identity(b"tairix.udf", &[&self.pvd[72..200], &self.pvd[376..388]])
    }

    /// The logical volume's identifier as UTF-8, or the volume's when the
    /// logical volume records none.
    #[must_use]
    pub fn label(&self) -> ([u8; LABEL_MAX], usize) {
        let mut out = [0u8; LABEL_MAX];
        let mut len = cs0_label(dstring(&self.logical.identifier), &mut out);
        if len == 0 {
            len = cs0_label(dstring(&self.pvd[24..56]), &mut out);
        }
        (out, len)
    }

    /// The bytes the volume spans from the device's start: to the end of its
    /// last partition or anchor, whichever is further.
    #[must_use]
    pub fn span_bytes(&self) -> u64 {
        let partitions = self
            .partitions
            .iter()
            .map(|pd| u64::from(pd.start) + u64::from(pd.length))
            .max()
            .unwrap_or(0);
        partitions
            .max(self.furthest_anchor + 1)
            .saturating_mul(u64::from(self.block_size()))
    }
}

/// Recognise a UDF volume on `dev`: a recognition sequence naming `NSR02` or
/// `NSR03`, an anchor that fixes the block size, and a volume descriptor
/// sequence whose logical volume agrees with it. `layout` places a
/// multisession or write-once disc's newest session. `None` when any of them
/// fails — never a guess.
///
/// # Errors
///
/// A device fault, or a refused allocation.
pub fn identify<B: Block>(
    dev: &mut ByteDevice<B>,
    layout: Option<DiscLayout>,
) -> Result<Option<Volume>, DriverError> {
    let device_block = dev.block_size();
    // UDF's block is the medium's sector, so the device's own size is tried
    // first; an image copied between devices may carry any of the others.
    let others = BLOCK_SIZES.into_iter().filter(|&size| size != device_block);
    // The recognition sequence depends only on its descriptors' stride, which
    // every block size up to 2048 shares.
    let mut recognised: [Option<Option<u8>>; 2] = [None, None];
    for block_size in core::iter::once(device_block).chain(others) {
        let bs = u64::from(block_size);
        let Some(session) = session_bytes(dev, layout, bs) else {
            continue;
        };
        let stride = bs.max(2048);
        let cached = &mut recognised[usize::from(stride > 2048)];
        let nsr = match *cached {
            Some(nsr) => nsr,
            None => *cached.insert(recognition(dev, session, stride)?),
        };
        let Some(nsr) = nsr else {
            continue;
        };
        let last_recorded = layout
            .and_then(|layout| layout.last_recorded)
            .and_then(|block| block.checked_add(1)?.checked_mul(u64::from(device_block)))
            .and_then(|end| (end / bs).checked_sub(1));
        let Some((anchor, furthest_anchor)) =
            find_anchor(dev, session / bs, last_recorded, block_size)?
        else {
            continue;
        };
        let Some(volume) = prevailing(dev, &anchor)? else {
            continue;
        };
        let (pvd, logical, partitions) = volume;
        return Ok(Some(Volume {
            session_start: session / bs,
            nsr,
            anchor,
            furthest_anchor,
            pvd,
            logical,
            partitions,
        }));
    }
    Ok(None)
}

/// The `NSR` revision a recognition sequence of descriptors `stride` bytes
/// apart, from byte `session` of the device, names inside its extended area,
/// or `None` when it names none.
fn recognition<B: Block>(
    dev: &mut ByteDevice<B>,
    session: u64,
    stride: u64,
) -> Result<Option<u8>, DriverError> {
    let mut extended = false;
    let mut nsr = None;
    for index in 0..MAX_VRS {
        let mut vsd = [0u8; 7];
        let Some(at) = index
            .checked_mul(stride)
            .and_then(|offset| session.checked_add(VRS_START)?.checked_add(offset))
        else {
            return Ok(None);
        };
        if !read_present(dev, at, &mut vsd)? {
            return Ok(None);
        }
        // ECMA-119's descriptors, a terminator among them, may precede the
        // extended area with types and versions of their own; the extended
        // area's descriptors are type 0, version 1.
        let fixed = vsd[0] == 0 && vsd[6] == 1;
        match &vsd[1..6] {
            b"CD001" | b"CDW02" | b"BOOT2" => {}
            b"BEA01" if fixed => extended = true,
            b"NSR02" if fixed && extended => nsr = Some(2),
            b"NSR03" if fixed && extended => nsr = Some(3),
            b"TEA01" if fixed && extended => return Ok(nsr),
            _ => return Ok(None),
        }
    }
    Ok(None)
}

/// The first anchor of `block_size`-byte sectors found where UDF places one —
/// sector 256 of the session, then the disc's last recorded sector and 256
/// before it, then the device's last sector and 256 before that, then
/// sector 512 of an unclosed disc — and the furthest sector any of them
/// verified at.
fn find_anchor<B: Block>(
    dev: &mut ByteDevice<B>,
    session: u64,
    last_recorded: Option<u64>,
    block_size: u32,
) -> Result<Option<(Anchor, u64)>, DriverError> {
    let bs = u64::from(block_size);
    let last = (dev.capacity() / bs).checked_sub(1);
    let candidates = [
        Some(session + ANCHOR_SECTOR),
        last_recorded,
        last_recorded.and_then(|n| n.checked_sub(ANCHOR_SECTOR)),
        last,
        last.and_then(|n| n.checked_sub(ANCHOR_SECTOR)),
        Some(session + UNCLOSED_ANCHOR_SECTOR),
    ];
    let mut sector = heap_buf::<DESCRIPTOR_LEN>()?;
    let mut first = None;
    let mut furthest = 0;
    for candidate in candidates.into_iter().flatten() {
        let Ok(location) = u32::try_from(candidate) else {
            continue;
        };
        if !read_present(dev, candidate * bs, &mut sector[..])?
            || Tag::verify(&sector[..], location).is_none_or(|tag| tag.id != TAG_AVDP)
        {
            continue;
        }
        let (Some(main), Some(reserve)) =
            (ExtentAd::at(&sector[..], 16), ExtentAd::at(&sector[..], 24))
        else {
            continue;
        };
        furthest = furthest.max(candidate);
        first.get_or_insert(Anchor {
            block_size,
            sector: candidate,
            main,
            reserve,
        });
    }
    Ok(first.map(|anchor| (anchor, furthest)))
}

/// The descriptors a usable volume prevails with.
type Prevailing = (Box<[u8; DESCRIPTOR_LEN]>, LogicalVolume, Vec<Partition>);

/// The prevailing descriptors of the anchor's main sequence, or of its
/// reserve when the main one does not hold a usable volume.
fn prevailing<B: Block>(
    dev: &mut ByteDevice<B>,
    anchor: &Anchor,
) -> Result<Option<Prevailing>, DriverError> {
    for extent in [anchor.main, anchor.reserve] {
        if let Some(found) = walk_sequence(dev, extent, anchor.block_size)? {
            return Ok(Some(found));
        }
    }
    Ok(None)
}

/// One volume descriptor sequence from `extent`, following its continuation
/// pointers, keeping the descriptor of highest sequence number in each
/// class.
fn walk_sequence<B: Block>(
    dev: &mut ByteDevice<B>,
    mut extent: ExtentAd,
    block_size: u32,
) -> Result<Option<Prevailing>, DriverError> {
    let bs = u64::from(block_size);
    let Ok(sector_len) = usize::try_from(bs) else {
        return Ok(None);
    };
    let mut sector =
        tairix_util::fallible::filled(sector_len, 0u8).ok_or(DriverError::OutOfMemory)?;
    let mut pvd: Option<(u32, Box<[u8; DESCRIPTOR_LEN]>)> = None;
    let mut lvd: Option<(u32, LogicalVolume)> = None;
    let mut partitions: Vec<(u32, Partition)> = Vec::new();
    if !tairix_util::fallible::reserve(&mut partitions, MAX_PARTITIONS) {
        return Err(DriverError::OutOfMemory);
    }
    let mut read = 0u64;
    'sequence: loop {
        for index in 0..u64::from(extent.length) / bs {
            read += 1;
            if read > MAX_VDS_SECTORS {
                break 'sequence;
            }
            let at = u64::from(extent.location) + index;
            let Ok(location) = u32::try_from(at) else {
                break 'sequence;
            };
            if !read_present(dev, at * bs, &mut sector)? {
                break 'sequence;
            }
            let Some(tag) = Tag::verify(&sector, location) else {
                break 'sequence;
            };
            let Some(vdsn) = le_u32(&sector, 16) else {
                break 'sequence;
            };
            match tag.id {
                TAG_PVD if pvd.as_ref().is_none_or(|(seen, _)| vdsn >= *seen) => {
                    let mut kept = heap_buf::<DESCRIPTOR_LEN>()?;
                    kept.copy_from_slice(&sector[..DESCRIPTOR_LEN]);
                    pvd = Some((vdsn, kept));
                }
                TAG_LVD if lvd.as_ref().is_none_or(|(seen, _)| vdsn >= *seen) => {
                    let Some(logical) = parse_lvd(&sector)? else {
                        return Ok(None);
                    };
                    lvd = Some((vdsn, logical));
                }
                TAG_PD => {
                    let Some(partition) = Partition::parse(&sector) else {
                        return Ok(None);
                    };
                    let held = partitions.len();
                    match partitions
                        .iter_mut()
                        .find(|(_, p)| p.number == partition.number)
                    {
                        Some(slot) if vdsn >= slot.0 => *slot = (vdsn, partition),
                        Some(_) => {}
                        None if held < MAX_PARTITIONS => partitions.push((vdsn, partition)),
                        None => return Ok(None),
                    }
                }
                TAG_VDP => {
                    let Some(next) = ExtentAd::at(&sector, 20) else {
                        break 'sequence;
                    };
                    extent = next;
                    continue 'sequence;
                }
                TAG_TD => break 'sequence,
                _ => {}
            }
        }
        break;
    }
    let (Some((_, pvd)), Some((_, logical))) = (pvd, lvd) else {
        return Ok(None);
    };
    if partitions.is_empty() || logical.block_size != block_size {
        return Ok(None);
    }
    Ok(Some((
        pvd,
        logical,
        partitions
            .into_iter()
            .map(|(_, partition)| partition)
            .collect(),
    )))
}

/// The Logical Volume Descriptor in `sector`, `None` for one naming a
/// foreign domain or a map table its sector cannot hold.
fn parse_lvd(sector: &[u8]) -> Result<Option<LogicalVolume>, DriverError> {
    let (Some(block_size), Some(map_len), Some(map_count)) = (
        le_u32(sector, 212),
        le_u32(sector, 264),
        le_u32(sector, 268),
    ) else {
        return Ok(None);
    };
    let Some(domain) = sector.get(217..240) else {
        return Ok(None);
    };
    let names_domain = domain.iter().any(|&b| b != 0);
    if names_domain && !domain.starts_with(DOMAIN_ID) {
        return Ok(None);
    }
    let Some(table) = usize::try_from(map_len)
        .ok()
        .and_then(|len| sector.get(LVD_MAPS_AT..LVD_MAPS_AT.checked_add(len)?))
    else {
        return Ok(None);
    };
    let (Some(file_set), Some(integrity), Some(identifier)) = (
        LongAd::at(sector, 248),
        ExtentAd::at(sector, 432),
        sector.get(84..212).and_then(|id| id.try_into().ok()),
    ) else {
        return Ok(None);
    };
    let maps = tairix_util::fallible::collected(table.len(), table.iter().copied())
        .ok_or(DriverError::OutOfMemory)?;
    Ok(Some(LogicalVolume {
        block_size,
        revision: if names_domain {
            le_u16(sector, 240).unwrap_or(0)
        } else {
            0
        },
        domain_flags: if names_domain {
            sector.get(242).copied().unwrap_or(0)
        } else {
            0
        },
        file_set,
        integrity,
        map_count,
        identifier,
        maps,
    }))
}

/// The OSTA Compressed Unicode characters of a `dstring` field: its last
/// byte holds the length used, the compression ID included. An empty field
/// has none.
#[must_use]
pub fn dstring(field: &[u8]) -> &[u8] {
    let Some((&used, body)) = field.split_last() else {
        return &[];
    };
    &body[..usize::from(used).min(body.len())]
}

/// The characters of an OSTA Compressed Unicode string (its compression ID
/// first): one byte per character for ID 8, two big-endian bytes for 16,
/// and the same for the deleted-name IDs 254 and 255. `None` for any other
/// ID; an `Err` item for a 16-bit unit that is not a scalar value.
#[must_use]
pub fn cs0_chars(cs0: &[u8]) -> Option<impl Iterator<Item = Result<char, u16>> + '_> {
    let (&id, body) = cs0.split_first()?;
    let wide = match id {
        8 | 254 => false,
        16 | 255 => true,
        _ => return None,
    };
    let narrow = (!wide).then(|| body.iter().map(|&b| Ok(char::from(b))));
    let wide = wide.then(|| {
        char::decode_utf16(
            body.as_chunks::<2>()
                .0
                .iter()
                .map(|&pair| u16::from_be_bytes(pair)),
        )
        .map(|decoded| decoded.map_err(|err| err.unpaired_surrogate()))
    });
    Some(
        narrow
            .into_iter()
            .flatten()
            .chain(wide.into_iter().flatten()),
    )
}

/// A CS0 label decoded into UTF-8 in `out`, trimmed of trailing spaces; an
/// unknown compression or an unpaired surrogate ends it there.
fn cs0_label(cs0: &[u8], out: &mut [u8; LABEL_MAX]) -> usize {
    let Some(chars) = cs0_chars(cs0) else {
        return 0;
    };
    let mut len = 0;
    let mut end = 0;
    for ch in chars {
        let Ok(ch) = ch else {
            break;
        };
        if ch == '\0' {
            break;
        }
        let mut buf = [0u8; 4];
        let encoded = ch.encode_utf8(&mut buf);
        let Some(slot) = out.get_mut(len..len + encoded.len()) else {
            break;
        };
        slot.copy_from_slice(encoded.as_bytes());
        len += encoded.len();
        if ch != ' ' {
            end = len;
        }
    }
    end
}
