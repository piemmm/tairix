//! File Entries and Extended File Entries (ECMA-167 4/14.9, 4/14.17), their
//! ICB tags, and the allocation descriptors that place a file's bytes.

use alloc::vec::Vec;

use tairix_abi::time::Time64;
use tairix_abi::DriverError;
use tairix_fsprobe::udf::{Tag, TAG_AED, TAG_EFE, TAG_FE, TAG_LEN};

use crate::fields::Fields;
use crate::layout::LbAddr;

/// ICB file type: a directory.
pub(crate) const TYPE_DIRECTORY: u8 = 4;
/// ICB file type: a file of bytes.
pub(crate) const TYPE_FILE: u8 = 5;
/// ICB file type: a file of extended attributes (ECMA-167 4/9.1).
pub(crate) const TYPE_ATTRIBUTES: u8 = 8;
/// ICB file type: a symbolic link.
pub(crate) const TYPE_SYMLINK: u8 = 12;
/// ICB file type: a stream directory (UDF 2.00 and later).
pub(crate) const TYPE_STREAMS: u8 = 13;
/// ICB file type: the virtual allocation table (UDF 2.00 and later).
pub(crate) const TYPE_VAT: u8 = 248;

/// ICB strategy: one direct entry.
pub(crate) const STRATEGY_4: u16 = 4;
/// ICB strategy: a chain of entries, the newest last (write-once media).
pub(crate) const STRATEGY_4096: u16 = 4096;

/// ICB flag bits naming how the allocation descriptors are recorded.
pub(crate) const FLAG_AD_MASK: u16 = 0b111;
/// ICB flag: set-user-id.
pub(crate) const FLAG_SETUID: u16 = 1 << 6;
/// ICB flag: set-group-id.
pub(crate) const FLAG_SETGID: u16 = 1 << 7;
/// ICB flag: sticky.
pub(crate) const FLAG_STICKY: u16 = 1 << 8;

/// Where a File Entry's variable part starts.
pub(crate) const FE_FIXED: usize = 176;
/// Where an Extended File Entry's variable part starts.
pub(crate) const EFE_FIXED: usize = 216;

/// Most extents one file's allocation descriptors may name: a validation
/// bound on untrusted media.
pub(crate) const MAX_EXTENTS: usize = 1 << 20;

/// Most allocation extent descriptors one file's chain may hop through.
const MAX_AED_HOPS: usize = 1 << 16;

/// How a file's allocation descriptors are recorded.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum AdForm {
    Short,
    Long,
    Extended,
    /// The file's bytes are the descriptor area itself.
    Embedded,
}

impl AdForm {
    fn of(flags: u16) -> Result<Self, DriverError> {
        match flags & FLAG_AD_MASK {
            0 => Ok(Self::Short),
            1 => Ok(Self::Long),
            2 => Ok(Self::Extended),
            3 => Ok(Self::Embedded),
            _ => Err(DriverError::DeviceFault),
        }
    }

    /// Bytes of one descriptor of this form.
    fn len(self) -> usize {
        match self {
            Self::Short => 8,
            Self::Long => 16,
            Self::Extended => 20,
            Self::Embedded => 0,
        }
    }
}

/// A decoded File Entry or Extended File Entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Entry {
    pub extended: bool,
    pub file_type: u8,
    pub flags: u16,
    pub uid: u32,
    pub gid: u32,
    pub permissions: u32,
    pub links: u16,
    pub size: u64,
    pub recorded_blocks: u64,
    pub accessed: Option<Time64>,
    pub modified: Option<Time64>,
    pub changed: Option<Time64>,
    pub created: Option<Time64>,
    pub unique_id: u64,
    /// Where the extended attributes lie in the entry's block.
    pub ea: core::ops::Range<usize>,
    /// The file holding the extended attributes beyond the entry's own.
    pub ea_file: Option<LbAddr>,
    /// The directory of the entry's named streams.
    pub streams: Option<LbAddr>,
    /// Where the allocation descriptors, or embedded bytes, lie.
    pub ad: core::ops::Range<usize>,
    pub form: AdForm,
}

impl Entry {
    /// The entry opening `block`, recorded at partition block `location`.
    ///
    /// # Errors
    ///
    /// [`DriverError::DeviceFault`] for a block holding no valid entry, or
    /// one whose areas run past the block.
    pub(crate) fn parse(block: &[u8], location: u32) -> Result<Self, DriverError> {
        let tag = Tag::verify(block, location).ok_or(DriverError::DeviceFault)?;
        let f = Fields(block);
        let extended = match tag.id {
            TAG_FE => false,
            TAG_EFE => true,
            _ => return Err(DriverError::DeviceFault),
        };
        let fixed = if extended { EFE_FIXED } else { FE_FIXED };
        let (lengths, unique) = if extended { (208, 200) } else { (168, 160) };
        let ea_len = usize::try_from(f.u32(lengths)?).map_err(|_| DriverError::DeviceFault)?;
        let ad_len = usize::try_from(f.u32(lengths + 4)?).map_err(|_| DriverError::DeviceFault)?;
        let ea_end = fixed.checked_add(ea_len).ok_or(DriverError::DeviceFault)?;
        let ad_end = ea_end.checked_add(ad_len).ok_or(DriverError::DeviceFault)?;
        if ad_end > block.len() {
            return Err(DriverError::DeviceFault);
        }
        let flags = f.u16(TAG_LEN + 18)?;
        let form = AdForm::of(flags)?;
        let size = f.u64(56)?;
        // Embedded bytes are the descriptor area itself.
        if form == AdForm::Embedded && size > ad_len as u64 {
            return Err(DriverError::DeviceFault);
        }
        let times = if extended {
            [80, 92, 116, 104]
        } else {
            [72, 84, 96, 0]
        };
        let icb = |at: usize| -> Result<Option<LbAddr>, DriverError> {
            let ad = f.long_ad(at)?;
            Ok((ad.bytes() != 0).then_some(LbAddr {
                block: ad.block,
                partition: ad.partition,
            }))
        };
        Ok(Self {
            extended,
            file_type: f.u8(TAG_LEN + 11)?,
            flags,
            uid: f.u32(36)?,
            gid: f.u32(40)?,
            permissions: f.u32(44)?,
            links: f.u16(48)?,
            size,
            recorded_blocks: f.u64(if extended { 72 } else { 64 })?,
            accessed: f.timestamp(times[0])?,
            modified: f.timestamp(times[1])?,
            changed: f.timestamp(times[2])?,
            created: if extended {
                f.timestamp(times[3])?
            } else {
                None
            },
            unique_id: f.u64(unique)?,
            ea: fixed..ea_end,
            ea_file: icb(if extended { 136 } else { 112 })?,
            streams: if extended { icb(152)? } else { None },
            ad: ea_end..ad_end,
            form,
        })
    }

    /// The ICB strategy the entry's tag names.
    pub(crate) fn strategy(block: &[u8]) -> Result<u16, DriverError> {
        Fields(block).u16(TAG_LEN + 4)
    }
}

/// What an extent of a file holds.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Holds {
    /// Recorded data.
    Data,
    /// Space allocated but never written, which reads as zeros.
    Unwritten,
    /// No space at all: a hole.
    Hole,
}

/// One extent of a file.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Extent {
    pub holds: Holds,
    pub bytes: u32,
    pub start: LbAddr,
}

/// A file's extents, read from its entry's descriptors and the allocation
/// extent descriptors they continue into. `read_block` reads the block a
/// continuation names.
///
/// # Errors
///
/// [`DriverError::DeviceFault`] for a descriptor that is malformed, a chain
/// past its bounds, or a continuation whose descriptor does not verify.
pub(crate) fn extents(
    block: &[u8],
    entry: &Entry,
    partition: u16,
    read_block: impl FnMut(LbAddr, &mut Vec<u8>) -> Result<(), DriverError>,
) -> Result<Vec<Extent>, DriverError> {
    allocation(block, entry, partition, read_block).map(|(extents, _)| extents)
}

/// A file's extents, as [`extents`] reads them, and the blocks of the
/// allocation extent descriptors they continued through.
pub(crate) fn allocation(
    block: &[u8],
    entry: &Entry,
    partition: u16,
    mut read_block: impl FnMut(LbAddr, &mut Vec<u8>) -> Result<(), DriverError>,
) -> Result<(Vec<Extent>, Vec<LbAddr>), DriverError> {
    let mut out = Vec::new();
    let mut chain = Vec::new();
    if entry.form == AdForm::Embedded {
        return Ok((out, chain));
    }
    let area = block
        .get(entry.ad.clone())
        .ok_or(DriverError::DeviceFault)?;
    let mut next = parse_area(area, entry.form, partition, &mut out)?;
    let mut continuation = Vec::new();
    while let Some(at) = next {
        if chain.len() >= MAX_AED_HOPS {
            return Err(DriverError::DeviceFault);
        }
        if !tairix_util::fallible::push(&mut chain, at) {
            return Err(DriverError::OutOfMemory);
        }
        read_block(at, &mut continuation)?;
        let tag = Tag::verify(&continuation, at.block).ok_or(DriverError::DeviceFault)?;
        if tag.id != TAG_AED {
            return Err(DriverError::DeviceFault);
        }
        let len = usize::try_from(Fields(&continuation).u32(20)?)
            .map_err(|_| DriverError::DeviceFault)?;
        let area = Fields(&continuation).bytes(AED_FIXED, len)?;
        next = parse_area(area, entry.form, partition, &mut out)?;
    }
    Ok((out, chain))
}

/// Where an allocation extent descriptor's descriptors start.
pub(crate) const AED_FIXED: usize = 24;

/// Append the extents the descriptors in `area` name to `out`, answering
/// where they continue. A zero length ends the list.
fn parse_area(
    area: &[u8],
    form: AdForm,
    partition: u16,
    out: &mut Vec<Extent>,
) -> Result<Option<LbAddr>, DriverError> {
    let width = form.len();
    if width == 0 {
        return Err(DriverError::DeviceFault);
    }
    for raw in area.chunks_exact(width) {
        let (length, start) = descriptor(raw, form, partition)?;
        let bytes = length & 0x3FFF_FFFF;
        if bytes == 0 {
            return Ok(None);
        }
        let holds = match length >> 30 {
            0 => Holds::Data,
            1 => Holds::Unwritten,
            2 => Holds::Hole,
            _ => return Ok(Some(start)),
        };
        if out.len() >= MAX_EXTENTS {
            return Err(DriverError::DeviceFault);
        }
        if !tairix_util::fallible::push(
            out,
            Extent {
                holds,
                bytes,
                start,
            },
        ) {
            return Err(DriverError::OutOfMemory);
        }
    }
    Ok(None)
}

/// The length field and start of one descriptor of `form`; a short
/// descriptor's start lies in the entry's own partition.
fn descriptor(raw: &[u8], form: AdForm, partition: u16) -> Result<(u32, LbAddr), DriverError> {
    let f = Fields(raw);
    match form {
        AdForm::Short => Ok((
            f.u32(0)?,
            LbAddr {
                block: f.u32(4)?,
                partition,
            },
        )),
        AdForm::Long => Ok((
            f.u32(0)?,
            LbAddr {
                block: f.u32(4)?,
                partition: f.u16(8)?,
            },
        )),
        AdForm::Extended => Ok((
            f.u32(0)?,
            LbAddr {
                block: f.u32(12)?,
                partition: f.u16(16)?,
            },
        )),
        AdForm::Embedded => Err(DriverError::DeviceFault),
    }
}
