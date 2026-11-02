//! RAR 1.5 to 4's headers: a block per header, each held to its CRC, a
//! member's fields, its name in the host's code page or RAR's Unicode
//! encoding, and its extended times, read into members.

use alloc::vec::Vec;

use tairix_abi::time::{DosTime, Time64};
use tairix_abi::DriverError;
use tairix_fsprobe::archive::RAR4_SIGNATURE;
use tairix_fsserve::VolumeAccess;

use super::{Check, Entry, Kind, Member, Method};
use crate::name::Charset;
use crate::region::Regions;

/// Block types.
const MAIN: u8 = 0x73;
const FILE: u8 = 0x74;
const SUB: u8 = 0x77;
const PROTECT: u8 = 0x78;
const NEWSUB: u8 = 0x7A;
const END: u8 = 0x7B;
const COMMENT: u8 = 0x75;
const MARK: u8 = 0x72;

/// A block carrying a data area's size.
const LONG_BLOCK: u16 = 0x8000;

/// Main header flags.
const VOLUME: u16 = 0x0001;
const SOLID: u16 = 0x0008;
const ENCRYPTED_HEADERS: u16 = 0x0080;

/// File header flags.
const SPLIT_BEFORE: u16 = 0x0001;
const SPLIT_AFTER: u16 = 0x0002;
const ENCRYPTED: u16 = 0x0004;
const HAS_COMMENT: u16 = 0x0008;
const SOLID_FILE: u16 = 0x0010;
const DIRECTORY: u16 = 0x00E0;
const LARGE: u16 = 0x0100;
const UNICODE: u16 = 0x0200;
const SALT: u16 = 0x0400;
const EXT_TIME: u16 = 0x1000;

/// Hosts.
const HOST_DOS: u8 = 0;
const HOST_UNIX: u8 = 3;

/// The stored method.
const STORED: u8 = 0x30;

/// One block's header.
struct Block {
    kind: u8,
    flags: u16,
    /// The header's bytes.
    bytes: Vec<u8>,
    data_start: u64,
    data_end: u64,
    stored_crc: u16,
}

/// The CRC-16 a header records: CRC-32's low half.
fn crc16(bytes: &[u8]) -> u16 {
    u16::try_from(tairix_crc32::checksum(bytes) & 0xFFFF).unwrap_or(0)
}

fn le16(bytes: &[u8], at: usize) -> Result<u16, DriverError> {
    Ok(u16::from_le_bytes([
        *bytes.get(at).ok_or(DriverError::BadMagic)?,
        *bytes.get(at + 1).ok_or(DriverError::BadMagic)?,
    ]))
}

fn le32(bytes: &[u8], at: usize) -> Result<u32, DriverError> {
    let slice = bytes.get(at..at + 4).ok_or(DriverError::BadMagic)?;
    Ok(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

/// The block whose header is at `pos`.
fn block<A: VolumeAccess>(regions: &Regions<A>, pos: u64) -> Result<Block, DriverError> {
    let mut fixed = [0u8; 7];
    regions.read_archive(pos, &mut fixed)?;
    let stored_crc = le16(&fixed, 0)?;
    let kind = fixed[2];
    let flags = le16(&fixed, 3)?;
    let size = le16(&fixed, 5)?;
    if size < 7 {
        return Err(DriverError::BadMagic);
    }
    let bytes = regions.archive_bytes(pos, usize::from(size))?;
    let data_len = match kind {
        FILE | NEWSUB => {
            let low = u64::from(le32(&bytes, 7)?);
            let high = if flags & LARGE != 0 {
                u64::from(le32(&bytes, 32)?)
            } else {
                0
            };
            high << 32 | low
        }
        SUB | PROTECT => u64::from(le32(&bytes, 7)?),
        MAIN | COMMENT | END | MARK => 0,
        _ if flags & LONG_BLOCK != 0 => u64::from(le32(&bytes, 7)?),
        _ => 0,
    };
    let data_start = pos + u64::from(size);
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
        data_start,
        data_end,
        stored_crc,
    })
}

/// List the members of the RAR 1.5 to 4 archive `regions` holds.
///
/// # Errors
///
/// [`DriverError::BadMagic`] for a main header out of shape;
/// [`DriverError::PermissionDenied`] for encrypted headers;
/// [`DriverError::Unsupported`] for one volume of several.
pub(super) fn list<A: VolumeAccess>(regions: &Regions<A>) -> Result<Vec<Member>, DriverError> {
    let mut members = Vec::new();
    let mut pos = RAR4_SIGNATURE.len() as u64;
    let mut main_flags = None;
    let mut first = true;
    while pos + 7 <= regions.len() {
        // A header damaged ends what can be believed of the archive.
        let Ok(block) = block(regions, pos) else {
            break;
        };
        match block.kind {
            MAIN => {
                if block.bytes.len() < 13 || crc16(&block.bytes[2..13]) != block.stored_crc {
                    return Err(DriverError::BadMagic);
                }
                if block.flags & ENCRYPTED_HEADERS != 0 {
                    return Err(DriverError::PermissionDenied);
                }
                if block.flags & VOLUME != 0 {
                    return Err(DriverError::Unsupported);
                }
                main_flags = Some(block.flags);
            }
            FILE => {
                let Some(main) = main_flags else {
                    return Err(DriverError::BadMagic);
                };
                let member = match file(&block, main, first) {
                    Ok(Some(member)) => member,
                    // A header out of shape ends what can be believed, so no
                    // member after it joins a run across it.
                    Err(_) => break,
                    // A directory no volume can name, which holds no bytes.
                    Ok(None) => {
                        first = false;
                        pos = block.data_end;
                        continue;
                    }
                };
                members
                    .try_reserve(1)
                    .map_err(|_| DriverError::OutOfMemory)?;
                members.push(member);
                first = false;
            }
            END => break,
            _ => {}
        }
        pos = block.data_end;
    }
    if main_flags.is_none() {
        return Err(DriverError::BadMagic);
    }
    Ok(members)
}

/// The member a file header records, or `None` for one with no name a
/// volume serves; [`DriverError::BadMagic`] for a header failing its CRC.
fn file(block: &Block, main_flags: u16, first: bool) -> Result<Option<Member>, DriverError> {
    let h = &block.bytes;
    if h.len() < 32 {
        return Err(DriverError::BadMagic);
    }
    let flags = block.flags;
    let mut packed_size = u64::from(le32(h, 7)?);
    let mut len = u64::from(le32(h, 11)?);
    let host = h[15];
    let crc = le32(h, 16)?;
    let dos = le32(h, 20)?;
    let version = h[24];
    let method = h[25];
    let name_len = usize::from(le16(h, 26)?);
    let attributes = le32(h, 28)?;
    let mut at = 32;
    if flags & LARGE != 0 {
        packed_size |= u64::from(le32(h, 32)?) << 32;
        len |= u64::from(le32(h, 36)?) << 32;
        at = 40;
    }
    let raw = h.get(at..at + name_len).ok_or(DriverError::BadMagic)?;
    at += name_len;
    // RAR before 3.0 checks a header with a comment only to its name.
    let checked = if flags & HAS_COMMENT != 0 && crc16(&h[2..at]) == block.stored_crc {
        true
    } else {
        crc16(&h[2..]) == block.stored_crc
    };
    if !checked {
        return Err(DriverError::BadMagic);
    }
    if flags & SALT != 0 {
        at += 8;
    }
    if flags & HAS_COMMENT != 0 {
        let size = usize::from(le16(h, at + 5)?);
        at += size;
    }
    let modified = modification_time(h, at, flags, dos);
    let directory = flags & DIRECTORY == DIRECTORY
        || (version == 15 && host == HOST_DOS && attributes & 0x10 != 0);
    let name = decode_name(raw, flags, host).map(|mut name| {
        for byte in &mut name {
            if *byte == b'\\' {
                *byte = b'/';
            }
        }
        name
    });
    let mode = mode_of(host, directory, attributes);
    if directory {
        // One spelling no name a volume serves holds no bytes either.
        return Ok(name.map(|name| Member {
            name,
            kind: Kind::Dir,
            mode,
            modified,
        }));
    }
    let (coding, refused) = coding_of(method, version, flags);
    let solid = if version < 20 {
        main_flags & SOLID != 0 && !first
    } else {
        flags & SOLID_FILE != 0
    };
    let entry = Entry {
        at: block.data_start,
        packed: packed_size.min(block.data_end - block.data_start),
        len,
        method: coding,
        dict: 0x1_0000u64 << ((flags >> 5) & 7),
        solid,
        check: Check::Crc32(crc),
        refused,
    };
    let link = host == HOST_UNIX && attributes & 0o170_000 == 0o120_000;
    let kind = match &name {
        // Spelling no name a volume serves, its bytes still keep their place
        // in its run.
        None => Kind::Unlisted(entry),
        Some(_) if link => Kind::LinkData(entry),
        Some(_) => Kind::File(entry),
    };
    Ok(Some(Member {
        name: name.unwrap_or_default(),
        kind,
        mode,
        modified,
    }))
}

/// A member's mode: a Unix host's own permission bits, else one read from
/// its DOS attributes.
fn mode_of(host: u8, directory: bool, attributes: u32) -> u32 {
    if host == HOST_UNIX {
        attributes & 0o7777
    } else if directory {
        0o755
    } else if attributes & 1 != 0 {
        0o444
    } else {
        0o644
    }
}

/// The decoder a file header's method and version name, and why its member
/// cannot be read, where it cannot.
fn coding_of(method: u8, version: u8, flags: u16) -> (Method, Option<DriverError>) {
    let coding = match (method, version) {
        (STORED, _) => Method::Stored,
        (_, 15) => Method::V15,
        (_, 20 | 26) => Method::V20,
        _ => Method::V29,
    };
    let refused = if flags & ENCRYPTED != 0 {
        Some(DriverError::PermissionDenied)
    } else if flags & (SPLIT_BEFORE | SPLIT_AFTER) != 0
        || !matches!(version, 15 | 20 | 26 | 29 | 36) && method != STORED
    {
        Some(DriverError::Unsupported)
    } else {
        None
    };
    (coding, refused)
}

/// The modification time: the header's MS-DOS stamp, read as UTC as the
/// format records no zone, refined by its extended time where it has one.
fn modification_time(h: &[u8], at: usize, flags: u16, dos: u32) -> Time64 {
    let stamp = DosTime {
        date: u16::try_from(dos >> 16).unwrap_or(0),
        time: u16::try_from(dos & 0xFFFF).unwrap_or(0),
    };
    let base = stamp.to_time64().unwrap_or(Time64::UNIX_EPOCH);
    if flags & EXT_TIME == 0 {
        return base;
    }
    // Some writers set the flag with no field; then there is none.
    let Ok(ext) = le16(h, at) else {
        return base;
    };
    let mode = ext >> 12;
    if mode & 8 == 0 {
        return base;
    }
    let count = usize::from(mode & 3);
    let Some(bytes) = h.get(at + 2..at + 2 + count) else {
        return base;
    };
    let mut units = 0u32;
    for (j, &byte) in bytes.iter().enumerate() {
        units |= u32::from(byte) << (8 * (j + 3 - count));
    }
    let secs = base.secs() + i64::from(mode & 4 != 0);
    Time64::new(secs, units.saturating_mul(100).min(999_999_999)).unwrap_or(base)
}

/// A member name in UTF-8: RAR's Unicode encoding where the header says,
/// a Unix host's bytes as UTF-8 where they are, else the host's code page.
fn decode_name(raw: &[u8], flags: u16, host: u8) -> Option<Vec<u8>> {
    if flags & UNICODE != 0 {
        let Some(nul) = raw.iter().position(|&byte| byte == 0) else {
            return Charset::Utf8.decode(raw);
        };
        return unicode_name(&raw[..nul], &raw[nul + 1..], raw.len());
    }
    if host == HOST_UNIX {
        return Charset::Utf8
            .decode(raw)
            .or_else(|| Charset::Latin1.decode(raw));
    }
    Charset::Cp437.decode(raw)
}

/// RAR's Unicode name: `encoded` spelled against the code-page `ascii`
/// spelling, in at most `limit` code units, as UTF-16.
fn unicode_name(ascii: &[u8], encoded: &[u8], limit: usize) -> Option<Vec<u8>> {
    let mut units: Vec<u16> = Vec::new();
    units.try_reserve(limit).ok()?;
    let Some((&high, mut rest)) = encoded.split_first() else {
        return Charset::Latin1.decode(ascii);
    };
    let high = u16::from(high) << 8;
    let mut flags = 0u8;
    let mut flag_bits = 0;
    while !rest.is_empty() && units.len() < limit {
        if flag_bits == 0 {
            flags = rest[0];
            rest = &rest[1..];
            flag_bits = 8;
        }
        flag_bits -= 2;
        match (flags >> flag_bits) & 3 {
            0 => {
                let (&low, tail) = rest.split_first()?;
                units.push(u16::from(low));
                rest = tail;
            }
            1 => {
                let (&low, tail) = rest.split_first()?;
                units.push(u16::from(low) | high);
                rest = tail;
            }
            2 => {
                let [low, up, tail @ ..] = rest else {
                    break;
                };
                units.push(u16::from_le_bytes([*low, *up]));
                rest = tail;
            }
            _ => {
                let (&count, tail) = rest.split_first()?;
                rest = tail;
                if count & 0x80 != 0 {
                    let (&correction, tail) = rest.split_first()?;
                    rest = tail;
                    for _ in 0..(count & 0x7F) + 2 {
                        let Some(&byte) = ascii.get(units.len()) else {
                            break;
                        };
                        units.push(u16::from(byte.wrapping_add(correction)) | high);
                    }
                } else {
                    for _ in 0..count + 2 {
                        let Some(&byte) = ascii.get(units.len()) else {
                            break;
                        };
                        units.push(u16::from(byte));
                    }
                }
            }
        }
    }
    let mut out = Vec::new();
    out.try_reserve(units.len() * 3).ok()?;
    for ch in char::decode_utf16(units.iter().copied()) {
        let mut utf8 = [0u8; 4];
        out.extend_from_slice(ch.ok()?.encode_utf8(&mut utf8).as_bytes());
    }
    Some(out)
}
