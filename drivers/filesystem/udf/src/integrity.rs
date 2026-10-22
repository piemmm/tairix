//! The Logical Volume Integrity Descriptor (ECMA-167 3/10.10, OSTA UDF 2.60
//! §2.2.6): whether the volume was closed cleanly, its free space, its next
//! unique id, and the counts and revisions its implementation use keeps. A
//! writer marks it open before its first change and closed after its last.

use alloc::vec::Vec;

use tairix_abi::driver::block::Block;
use tairix_abi::time::Time64;
use tairix_abi::DriverError;
use tairix_fsprobe::udf::{ExtentAd, Tag, TAG_LEN, TAG_LVID};

use crate::fields::{encode_timestamp, Fields};
use crate::{zeroed, Udf};

/// Most integrity extents a sequence is followed through.
const MAX_EXTENTS: usize = 16;

/// Most sectors one integrity extent is read across.
const MAX_SECTORS: u32 = 64;

/// Integrity type: the volume may be mid-change.
const OPEN: u32 = 0;
/// Integrity type: the volume was left consistent.
const CLOSE: u32 = 1;

/// The first unique id a file other than the root takes (UDF 2.60 §3.2.1.1).
pub(crate) const FIRST_UNIQUE_ID: u64 = 16;

/// Where the LVID's fixed fields end and its tables begin.
const TABLES_AT: usize = 80;

/// The prevailing integrity descriptor, decoded, and where it lies.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Integrity {
    /// The sector the prevailing descriptor was read from, if any was.
    pub sector: Option<u64>,
    pub closed: bool,
    pub free_blocks: u64,
    /// One entry per partition: its free blocks, `u32::MAX` when unknown.
    pub free_table: Vec<u32>,
    pub next_unique_id: u64,
    pub files: u32,
    pub directories: u32,
    pub min_write_revision: u16,
    /// The descriptor's bytes, for a writer to update.
    raw: Vec<u8>,
}

/// The integrity descriptor the sequence at the logical volume's integrity
/// extent ends with: the last that verifies, following continuation extents.
/// A volume with none reads as not closed.
pub(crate) fn read<B: Block>(fs: &mut Udf<B>) -> Result<Integrity, DriverError> {
    let mut extent = fs.volume.logical().integrity;
    let mut block = zeroed(fs.block_size)?;
    let mut found: Option<(u64, Vec<u8>)> = None;
    for _ in 0..MAX_EXTENTS {
        let sectors = (extent.length / fs.block_size).min(MAX_SECTORS);
        let mut next = None;
        for index in 0..sectors {
            let sector = u64::from(extent.location) + u64::from(index);
            if fs.read_sector(sector, &mut block).is_err() {
                break;
            }
            let Ok(location) = u32::try_from(sector) else {
                break;
            };
            match Tag::verify(&block, location).map(|tag| tag.id) {
                Some(TAG_LVID) => {
                    let continuation = ExtentAd::at(&block, 32).ok_or(DriverError::DeviceFault)?;
                    next = (continuation.length != 0).then_some(continuation);
                    found = Some((
                        sector,
                        core::mem::replace(&mut block, zeroed(fs.block_size)?),
                    ));
                }
                _ => break,
            }
        }
        match next {
            Some(continuation) => extent = continuation,
            None => break,
        }
    }
    let Some((sector, raw)) = found else {
        return Ok(Integrity::default());
    };
    decode(sector, raw)
}

fn decode(sector: u64, raw: Vec<u8>) -> Result<Integrity, DriverError> {
    let f = Fields(&raw);
    let partitions = usize::try_from(f.u32(72)?).map_err(|_| DriverError::DeviceFault)?;
    let implementation = usize::try_from(f.u32(76)?).map_err(|_| DriverError::DeviceFault)?;
    let iu = TABLES_AT + partitions.checked_mul(8).ok_or(DriverError::DeviceFault)?;
    f.bytes(iu, implementation)?;
    let mut free_table = Vec::new();
    if !tairix_util::fallible::reserve(&mut free_table, partitions) {
        return Err(DriverError::OutOfMemory);
    }
    for index in 0..partitions {
        free_table.push(f.u32(TABLES_AT + index * 4)?);
    }
    let free_blocks = free_table
        .iter()
        .filter(|&&free| free != u32::MAX)
        .map(|&free| u64::from(free))
        .sum();
    // The implementation use: an identifier, the file and directory counts,
    // then the revisions.
    let counts = |at: usize| {
        if implementation >= at + 4 {
            f.u32(iu + at)
        } else {
            Ok(0)
        }
    };
    Ok(Integrity {
        sector: Some(sector),
        closed: f.u32(28)? == CLOSE,
        free_blocks,
        free_table,
        next_unique_id: f.u64(40)?.max(FIRST_UNIQUE_ID),
        files: counts(32)?,
        directories: counts(36)?,
        min_write_revision: if implementation >= 44 {
            f.u16(iu + 42)?
        } else {
            0
        },
        raw,
    })
}

impl Integrity {
    /// The descriptor, marked open or closed at `now`, with the state a
    /// writer keeps: each partition's free blocks, the next unique id, the
    /// file and directory counts, and the revision it was written to.
    pub(crate) fn encoded(
        &self,
        open: bool,
        now: Option<Time64>,
        revision: u16,
    ) -> Result<(u64, Vec<u8>), DriverError> {
        let sector = self.sector.ok_or(DriverError::ReadOnly)?;
        let mut raw = self.raw.clone();
        let f = Fields(&raw);
        let partitions = usize::try_from(f.u32(72)?).map_err(|_| DriverError::DeviceFault)?;
        let implementation = usize::try_from(f.u32(76)?).map_err(|_| DriverError::DeviceFault)?;
        let iu = TABLES_AT + partitions * 8;
        let end = iu + implementation;
        if let Some(stamp) = now.and_then(encode_timestamp) {
            raw[16..28].copy_from_slice(&stamp);
        }
        raw[28..32].copy_from_slice(&(if open { OPEN } else { CLOSE }).to_le_bytes());
        raw[40..48].copy_from_slice(&self.next_unique_id.to_le_bytes());
        for (index, free) in self.free_table.iter().enumerate().take(partitions) {
            raw[TABLES_AT + index * 4..TABLES_AT + index * 4 + 4]
                .copy_from_slice(&free.to_le_bytes());
        }
        if implementation >= 46 {
            raw[iu + 32..iu + 36].copy_from_slice(&self.files.to_le_bytes());
            raw[iu + 36..iu + 40].copy_from_slice(&self.directories.to_le_bytes());
            // Minimum read and write revisions stay; the maximum written is
            // this writer's when newer.
            let max_written = u16::from_le_bytes([raw[iu + 44], raw[iu + 45]]).max(revision);
            raw[iu + 44..iu + 46].copy_from_slice(&max_written.to_le_bytes());
        }
        let tag = Tag {
            id: TAG_LVID,
            version: Fields(&raw).u16(2)?,
            serial: Fields(&raw).u16(6)?,
            location: u32::try_from(sector).map_err(|_| DriverError::DeviceFault)?,
        };
        tag.seal(&mut raw, end - TAG_LEN)?;
        Ok((sector, raw))
    }
}
