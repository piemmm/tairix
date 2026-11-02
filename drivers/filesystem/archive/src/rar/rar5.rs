//! RAR 5's headers (also what RAR 6 and 7 write): a CRC-checked header per
//! block, a member's fields and its extra records — hash, times,
//! redirection — read into members.

use alloc::vec::Vec;

use tairix_abi::time::Time64;
use tairix_abi::DriverError;
use tairix_fsprobe::archive::{rar5_vint, RAR5_ENCRYPTION, RAR5_MAIN, RAR5_SIGNATURE};
use tairix_fsserve::VolumeAccess;

use super::{Check, Entry, Kind, Member, Method};
use crate::region::Regions;

/// The largest header a block may have.
const HEADER_MAX: u64 = 2 * 1024 * 1024;

/// Block types.
const MAIN: u64 = RAR5_MAIN;
const FILE: u64 = 2;
const ENCRYPTION: u64 = RAR5_ENCRYPTION;
const END: u64 = 5;

/// Block flags.
const HAS_EXTRA: u64 = 0x01;
const HAS_DATA: u64 = 0x02;
const SPLIT_BEFORE: u64 = 0x08;
const SPLIT_AFTER: u64 = 0x10;

/// Main header flags.
const VOLUME: u64 = 0x01;

/// File header flags.
const DIRECTORY: u64 = 0x01;
const UNIX_TIME: u64 = 0x02;
const HAS_CRC: u64 = 0x04;
const SIZE_UNKNOWN: u64 = 0x08;

/// Host systems.
const HOST_WINDOWS: u64 = 0;
const HOST_UNIX: u64 = 1;

/// Extra record types.
const EXTRA_ENCRYPTION: u64 = 1;
const EXTRA_HASH: u64 = 2;
const EXTRA_TIME: u64 = 3;
const EXTRA_REDIRECTION: u64 = 5;

/// Redirection types.
const REDIRECT_UNIX_LINK: u64 = 1;
const REDIRECT_WINDOWS_LINK: u64 = 2;
const REDIRECT_HARD_LINK: u64 = 4;
const REDIRECT_COPY: u64 = 5;

/// Seconds from 1601, FILETIME's epoch, to 1970.
const FILETIME_TO_UNIX: i64 = 11_644_473_600;

/// A header's bytes, read as RAR 5 lays them out.
struct Fields<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Fields<'_> {
    fn vint(&mut self) -> Result<u64, DriverError> {
        let (value, len) =
            rar5_vint(self.bytes.get(self.at..).unwrap_or(&[])).ok_or(DriverError::BadMagic)?;
        self.at += len;
        Ok(value)
    }

    fn take(&mut self, len: u64) -> Result<&[u8], DriverError> {
        let len = usize::try_from(len).map_err(|_| DriverError::BadMagic)?;
        let end = self.at.checked_add(len).ok_or(DriverError::BadMagic)?;
        let taken = self.bytes.get(self.at..end).ok_or(DriverError::BadMagic)?;
        self.at = end;
        Ok(taken)
    }

    fn u32(&mut self) -> Result<u32, DriverError> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn u64(&mut self) -> Result<u64, DriverError> {
        let bytes = self.take(8)?;
        let mut word = [0u8; 8];
        word.copy_from_slice(bytes);
        Ok(u64::from_le_bytes(word))
    }
}

/// List the members of the RAR 5 archive `regions` holds.
///
/// # Errors
///
/// [`DriverError::BadMagic`] for a main header out of shape;
/// [`DriverError::PermissionDenied`] for encrypted headers;
/// [`DriverError::Unsupported`] for one volume of several.
pub(super) fn list<A: VolumeAccess>(regions: &Regions<A>) -> Result<Vec<Member>, DriverError> {
    let len = regions.len();
    let mut members = Vec::new();
    let mut pos = RAR5_SIGNATURE.len() as u64;
    let mut seen_main = false;
    while pos < len {
        // A header damaged ends what can be believed of the archive.
        let Ok(block) = header(regions, pos) else {
            if seen_main {
                break;
            }
            return Err(DriverError::BadMagic);
        };
        let mut fields = Fields {
            bytes: &block.bytes,
            at: block.body,
        };
        match block.kind {
            MAIN => {
                if fields.vint()? & VOLUME != 0 {
                    return Err(DriverError::Unsupported);
                }
                seen_main = true;
            }
            ENCRYPTION => return Err(DriverError::PermissionDenied),
            FILE if seen_main => {
                let member = match file(&block, fields) {
                    Ok(member) => member,
                    // Its bytes keep their place in a run it may carry on.
                    Err(_) if block.data_end > block.data_start => Member::unlisted(Entry::lost(
                        block.data_start,
                        block.data_end - block.data_start,
                        Method::V50 { v1: false },
                    )),
                    Err(_) => {
                        pos = block.data_end;
                        continue;
                    }
                };
                members
                    .try_reserve(1)
                    .map_err(|_| DriverError::OutOfMemory)?;
                members.push(member);
            }
            END => break,
            _ => {}
        }
        if !seen_main {
            return Err(DriverError::BadMagic);
        }
        pos = block.data_end;
    }
    Ok(members)
}

/// One block's header.
struct Block {
    kind: u64,
    flags: u64,
    /// The header's bytes from its type on, where its type-specific fields
    /// start, and where its extra area starts.
    bytes: Vec<u8>,
    body: usize,
    extra: usize,
    data_start: u64,
    data_end: u64,
}

/// The block whose header is at `pos`.
fn header<A: VolumeAccess>(regions: &Regions<A>, pos: u64) -> Result<Block, DriverError> {
    let mut fixed = [0u8; 7];
    let got = regions.read(tairix_abi::volume::ARCHIVE_REGION, pos, &mut fixed)?;
    let stored = u32::from_le_bytes([fixed[0], fixed[1], fixed[2], fixed[3]]);
    let (size, size_len) = rar5_vint(&fixed[4..got.max(4)]).ok_or(DriverError::BadMagic)?;
    if size_len > 3 || size == 0 || size > HEADER_MAX {
        return Err(DriverError::BadMagic);
    }
    let start = pos + 4;
    let total = size_len as u64 + size;
    let bytes = regions.archive_bytes(
        start,
        usize::try_from(total).map_err(|_| DriverError::BadMagic)?,
    )?;
    if tairix_crc32::checksum(&bytes) != stored {
        return Err(DriverError::BadMagic);
    }
    let mut fields = Fields {
        bytes: &bytes,
        at: size_len,
    };
    let kind = fields.vint()?;
    let flags = fields.vint()?;
    let extra_len = if flags & HAS_EXTRA != 0 {
        fields.vint()?
    } else {
        0
    };
    let data_len = if flags & HAS_DATA != 0 {
        fields.vint()?
    } else {
        0
    };
    let body = fields.at;
    let extra = usize::try_from(extra_len)
        .ok()
        .and_then(|extra_len| bytes.len().checked_sub(extra_len))
        .filter(|&extra| extra >= body)
        .ok_or(DriverError::BadMagic)?;
    let data_start = start + total;
    let data_end = data_start
        .checked_add(data_len)
        .ok_or(DriverError::BadMagic)?;
    if data_end > regions.len() {
        return Err(DriverError::BadMagic);
    }
    Ok(Block {
        kind,
        flags,
        bytes,
        body,
        extra,
        data_start,
        data_end,
    })
}

/// The member a file header records.
fn file(block: &Block, mut fields: Fields<'_>) -> Result<Member, DriverError> {
    let flags = fields.vint()?;
    let len = fields.vint()?;
    let attributes = fields.vint()?;
    let mut modified = None;
    if flags & UNIX_TIME != 0 {
        modified = Some(Time64::from_secs(i64::from(fields.u32()?)));
    }
    let crc = if flags & HAS_CRC != 0 {
        Some(fields.u32()?)
    } else {
        None
    };
    let info = fields.vint()?;
    let host = fields.vint()?;
    let name_len = fields.vint()?;
    let name = fields.take(name_len)?.to_vec();
    if fields.at > block.extra {
        return Err(DriverError::BadMagic);
    }
    // No volume serves the name of a Windows member holding `\`.
    let unservable = host == HOST_WINDOWS && name.contains(&b'\\');
    let records = Records::read(&block.bytes[block.extra..])?;
    if let Some(time) = records.modified {
        modified = Some(time);
    }
    let modified = modified.unwrap_or(Time64::UNIX_EPOCH);
    let directory = flags & DIRECTORY != 0;
    let mode = if host == HOST_UNIX {
        u32::try_from(attributes & 0o7777).unwrap_or(0o644)
    } else if directory {
        0o755
    } else if attributes & 1 != 0 {
        0o444
    } else {
        0o644
    };
    let kind = if directory {
        Kind::Dir
    } else if let Some((redirect, mut target)) = records.redirect {
        if host == HOST_WINDOWS {
            for byte in &mut target {
                if *byte == b'\\' {
                    *byte = b'/';
                }
            }
        }
        match redirect {
            REDIRECT_UNIX_LINK | REDIRECT_WINDOWS_LINK => Kind::Link(target),
            REDIRECT_HARD_LINK | REDIRECT_COPY => Kind::Copy(target),
            _ => return Err(DriverError::Unsupported),
        }
    } else {
        Kind::File(entry(block, flags, len, info, crc, &records))
    };
    let kind = match kind {
        Kind::File(entry) if unservable => Kind::Unlisted(entry),
        _ if unservable => return Err(DriverError::OutOfRange),
        kind => kind,
    };
    Ok(Member {
        name,
        kind,
        mode,
        modified,
    })
}

/// A file member's bytes, as its header and records describe them.
fn entry(
    block: &Block,
    flags: u64,
    len: u64,
    info: u64,
    crc: Option<u32>,
    records: &Records,
) -> Entry {
    let algorithm = info & 0x3F;
    let compression = (info >> 7) & 7;
    let code = (info >> 10) & 0x1F;
    let mut dict = 0x2_0000u64 << code;
    let v1 = match algorithm {
        0 if code <= 15 => false,
        1 => {
            dict += dict / 32 * ((info >> 15) & 0x1F);
            info & 0x10_0000 == 0
        }
        _ => true,
    };
    let unsupported = algorithm > 1
        || (algorithm == 0 && code > 15)
        || flags & SIZE_UNKNOWN != 0
        || block.flags & (SPLIT_BEFORE | SPLIT_AFTER) != 0;
    let refused = if records.encrypted {
        Some(DriverError::PermissionDenied)
    } else if unsupported {
        Some(DriverError::Unsupported)
    } else {
        None
    };
    let method = if compression == 0 {
        Method::Stored
    } else {
        Method::V50 { v1 }
    };
    let check = match (crc, records.hash) {
        (Some(crc), _) => Check::Crc32(crc),
        (None, Some(hash)) => Check::Blake2sp(hash),
        (None, None) => Check::None,
    };
    Entry {
        at: block.data_start,
        packed: block.data_end - block.data_start,
        len,
        method,
        dict,
        solid: info & 0x40 != 0,
        check,
        refused,
    }
}

/// What a file header's extra records say.
#[derive(Default)]
struct Records {
    encrypted: bool,
    hash: Option<[u8; 32]>,
    modified: Option<Time64>,
    redirect: Option<(u64, Vec<u8>)>,
}

impl Records {
    fn read(area: &[u8]) -> Result<Self, DriverError> {
        let mut records = Self::default();
        let mut fields = Fields { bytes: area, at: 0 };
        while fields.at < area.len() {
            let size = fields.vint()?;
            let body = fields.take(size)?;
            if size == 0 {
                return Err(DriverError::BadMagic);
            }
            let mut record = Fields { bytes: body, at: 0 };
            match record.vint()? {
                EXTRA_ENCRYPTION => records.encrypted = true,
                EXTRA_HASH => {
                    if record.vint()? == 0 {
                        let digest = record.take(32)?;
                        let mut hash = [0u8; 32];
                        hash.copy_from_slice(digest);
                        records.hash = Some(hash);
                    }
                }
                EXTRA_TIME => records.modified = time(&mut record)?,
                EXTRA_REDIRECTION => {
                    let kind = record.vint()?;
                    let _flags = record.vint()?;
                    let len = record.vint()?;
                    records.redirect = Some((kind, record.take(len)?.to_vec()));
                }
                _ => {}
            }
        }
        Ok(records)
    }
}

/// The modification time a time record holds, where it holds one.
fn time(record: &mut Fields<'_>) -> Result<Option<Time64>, DriverError> {
    const UNIX: u64 = 0x01;
    const MODIFIED: u64 = 0x02;
    const NANOS: u64 = 0x10;
    let flags = record.vint()?;
    if flags & MODIFIED == 0 {
        return Ok(None);
    }
    if flags & UNIX == 0 {
        let ticks = record.u64()?;
        let secs = i64::try_from(ticks / 10_000_000).map_err(|_| DriverError::BadMagic)?
            - FILETIME_TO_UNIX;
        let nanos = u32::try_from(ticks % 10_000_000).map_err(|_| DriverError::BadMagic)? * 100;
        return Ok(Some(Time64::new(secs, nanos).unwrap_or(Time64::UNIX_EPOCH)));
    }
    let secs = i64::from(record.u32()?);
    // The nanoseconds follow every time the record holds, in its order.
    let others = [0x04u64, 0x08]
        .iter()
        .filter(|&&bit| flags & bit != 0)
        .count();
    let nanos = if flags & NANOS != 0 {
        record.take(4 * others as u64)?;
        let nanos = record.u32()?;
        nanos.min(999_999_999)
    } else {
        0
    };
    Ok(Some(
        Time64::new(secs, nanos).unwrap_or(Time64::from_secs(secs)),
    ))
}
