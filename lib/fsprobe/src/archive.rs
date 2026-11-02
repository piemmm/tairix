//! Archive recognition (`plans/ARCHIVES.md` AR1): which archive format a
//! file's bytes hold, by signature alone and never by decompressing.
//!
//! An archive is a file, so it is read as its exact `len` bytes, wherever the
//! device's last block runs past them, and nothing at or past `len` is read.
//! The archive driver reads each format's opening structures through these
//! same readers ([`ZipEnd`], [`tar_header_holds`], [`GzipHeader`],
//! [`bzip2_level`], [`rar_version`], [`arcfs_header`], [`arc_entry_holds`]),
//! so the probe and the driver never disagree about what an archive is.
//!
//! Everything read is a stranger's file: every offset, length and count is
//! checked before it is used, and anything short of a format's own signature
//! and checks is `None`.

use alloc::vec::Vec;
use core::ops::Range;

use tairix_abi::driver::block::Block;
use tairix_abi::volume::VolumeFsType;
use tairix_abi::DriverError;
use tairix_bytedev::ByteDevice;

use crate::{content_identity, le_u16, le_u32, le_u64, ProbedVolume};

/// Bytes of an archive's head the identity is taken over, and that hold
/// every signature opening an archive.
const HEAD_LEN: usize = 4096;

/// The domain an archive's content identity is digested under.
const IDENTITY_DOMAIN: &[u8] = b"tairix-archive/v1";

/// The archive the file `dev` holds as its first `len` bytes, recognised in a
/// fixed order: zip by the end record that closes it, then the formats whose
/// signature opens the file — RAR, `ArcFS`, the ARC family (Spark), gzip,
/// bzip2 — and last tar, whose header carries only a checksum. `None` when
/// nothing matches.
///
/// # Errors
///
/// A device fault, or a refused allocation.
pub fn recognise<B: Block>(
    dev: &mut ByteDevice<B>,
    len: u64,
) -> Result<Option<ProbedVolume>, DriverError> {
    if len == 0 || len > dev.capacity() {
        return Ok(None);
    }
    let head_len = usize::try_from(len).map_or(HEAD_LEN, |len| len.min(HEAD_LEN));
    let mut head = tairix_util::fallible::filled(head_len, 0u8).ok_or(DriverError::OutOfMemory)?;
    dev.read(0, &mut head)?;
    let fstype = if ZipEnd::find(dev, len)?.is_some() {
        Some(VolumeFsType::Zip)
    } else if rar_version(&head).is_some() {
        Some(VolumeFsType::Rar)
    } else if arcfs_header(&head, len).is_some() {
        Some(VolumeFsType::ArcFs)
    } else if arc_entry_holds(&head, len) {
        Some(VolumeFsType::Spark)
    } else if GzipHeader::parse(&head).is_some() && len >= GZIP_MIN_LEN {
        Some(VolumeFsType::Gzip)
    } else if bzip2_level(&head).is_some() {
        Some(VolumeFsType::Bzip2)
    } else if head
        .get(..TAR_BLOCK)
        .and_then(|block| <&[u8; TAR_BLOCK]>::try_from(block).ok())
        .is_some_and(tar_header_holds)
    {
        Some(VolumeFsType::Tar)
    } else {
        None
    };
    Ok(fstype.map(|fstype| {
        let identity = content_identity(
            IDENTITY_DOMAIN,
            &[&[fstype.as_u8()], &len.to_le_bytes(), &head],
        );
        ProbedVolume::new(fstype, identity, &[], None, None)
    }))
}

/// Read `buf` from `offset` of an archive of `len` bytes, or answer `false`
/// where the range runs past them.
fn read_within<B: Block>(
    dev: &mut ByteDevice<B>,
    len: u64,
    offset: u64,
    buf: &mut [u8],
) -> Result<bool, DriverError> {
    match offset.checked_add(buf.len() as u64) {
        Some(end) if end <= len => {
            dev.read(offset, buf)?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// The zip end-of-central-directory record's signature.
pub const ZIP_END_SIG: u32 = 0x0605_4b50;
/// The ZIP64 end-of-central-directory locator's signature.
pub const ZIP64_LOCATOR_SIG: u32 = 0x0706_4b50;
/// The ZIP64 end-of-central-directory record's signature.
pub const ZIP64_END_SIG: u32 = 0x0606_4b50;
/// A zip central directory header's signature.
pub const ZIP_CENTRAL_SIG: u32 = 0x0201_4b50;
/// The end record's fixed length, before its comment.
pub const ZIP_END_LEN: usize = 22;
/// The ZIP64 locator's length.
pub const ZIP64_LOCATOR_LEN: usize = 20;
/// The ZIP64 end record's fixed length.
pub const ZIP64_END_LEN: usize = 56;
/// The longest archive comment the end record's length field can state.
const ZIP_COMMENT_MAX: usize = 0xFFFF;

/// Where a zip archive's central directory is, read from the end record that
/// closes the archive (and its ZIP64 record, where it has one).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ZipEnd {
    /// Bytes ahead of the archive proper — a self-extractor's program —
    /// which every offset the archive records is counted after.
    pub base: u64,
    /// Where the central directory starts in the file.
    pub directory: u64,
    /// The central directory's length.
    pub directory_len: u64,
    /// How many entries it holds.
    pub entries: u64,
    /// Whether the count and places are a ZIP64 end record's, in 64 bits,
    /// rather than the end record's sixteen and thirty-two.
    pub zip64: bool,
    /// Whether the archive says it spans several disks.
    pub split: bool,
    /// Where the archive's end records start in the file: the ZIP64 record
    /// where there is one, else the end record.
    pub end: u64,
    /// The archive comment, in the file.
    pub comment: Range<u64>,
}

impl ZipEnd {
    /// The end of the zip archive the first `len` bytes of `dev` hold, or
    /// `None` where they hold none: an end record whose comment ends exactly
    /// at `len`, naming a central directory that lies before it and opens
    /// with a central header.
    ///
    /// # Errors
    ///
    /// A device fault, or a refused allocation.
    pub fn find<B: Block>(dev: &mut ByteDevice<B>, len: u64) -> Result<Option<Self>, DriverError> {
        let window = usize::try_from(len).map_or(ZIP_END_LEN + ZIP_COMMENT_MAX, |len| {
            len.min(ZIP_END_LEN + ZIP_COMMENT_MAX)
        });
        if window < ZIP_END_LEN {
            return Ok(None);
        }
        let start = len - window as u64;
        let mut tail =
            tairix_util::fallible::filled(window, 0u8).ok_or(DriverError::OutOfMemory)?;
        dev.read(start, &mut tail)?;
        // The record whose comment ends the file; scanning from the end finds
        // the last such record, so a comment holding a record's bytes is not
        // taken for one.
        let Some(at) = (0..=window - ZIP_END_LEN).rev().find(|&at| {
            le_u32(&tail, at) == Some(ZIP_END_SIG)
                && le_u16(&tail, at + 20).map(usize::from) == Some(window - at - ZIP_END_LEN)
        }) else {
            return Ok(None);
        };
        let record = start + at as u64;
        let field16 = |offset| le_u16(&tail, at + offset).map_or(0, u64::from);
        let field32 = |offset| le_u32(&tail, at + offset).map_or(0, u64::from);
        let disk = field16(4);
        let directory_disk = field16(6);
        let entries_here = field16(8);
        let mut entries = field16(10);
        let mut directory_len = field32(12);
        let mut directory_at = field32(16);
        let comment_len = field16(20);
        let mut split = disk != 0 || directory_disk != 0 || entries_here != entries;
        let mut end = record;
        let wide = entries == 0xFFFF || directory_len == 0xFFFF_FFFF || directory_at == 0xFFFF_FFFF;
        // A field at its widest defers to the ZIP64 record; with none it
        // means what it says, as a writer counting exactly 65535 entries may.
        let zip64 = if wide {
            Self::zip64(dev, len, record)?
        } else {
            None
        };
        if let Some(zip64) = &zip64 {
            entries = zip64.entries;
            directory_len = zip64.directory_len;
            directory_at = zip64.directory;
            split |= zip64.split;
            end = zip64.end;
        }
        // The directory ends where the end records start; what it says of
        // its own start, against that, is how much stands ahead of the
        // archive.
        let Some(directory) = end.checked_sub(directory_len) else {
            return Ok(None);
        };
        let Some(base) = directory.checked_sub(directory_at) else {
            return Ok(None);
        };
        if entries == 0 {
            if directory_len != 0 {
                return Ok(None);
            }
        } else {
            let mut sig = [0u8; 4];
            if !read_within(dev, len, directory, &mut sig)?
                || u32::from_le_bytes(sig) != ZIP_CENTRAL_SIG
            {
                return Ok(None);
            }
        }
        Ok(Some(Self {
            base,
            directory,
            directory_len,
            entries,
            zip64: zip64.is_some(),
            split,
            end,
            comment: record + ZIP_END_LEN as u64..record + ZIP_END_LEN as u64 + comment_len,
        }))
    }

    /// The ZIP64 end record the locator just before the end record at
    /// `record` names, as an end; `None` where either is absent or out of
    /// shape. The record ends where the locator starts: it lies just before
    /// it unless an extensible data sector lengthens it, when it lies where
    /// the locator says, counted from the file's start.
    fn zip64<B: Block>(
        dev: &mut ByteDevice<B>,
        len: u64,
        record: u64,
    ) -> Result<Option<Self>, DriverError> {
        let Some(locator_at) = record.checked_sub(ZIP64_LOCATOR_LEN as u64) else {
            return Ok(None);
        };
        let mut locator = [0u8; ZIP64_LOCATOR_LEN];
        if !read_within(dev, len, locator_at, &mut locator)?
            || le_u32(&locator, 0) != Some(ZIP64_LOCATOR_SIG)
        {
            return Ok(None);
        }
        let (Some(disk), Some(recorded_at), Some(disks)) = (
            le_u32(&locator, 4),
            le_u64(&locator, 8),
            le_u32(&locator, 16),
        ) else {
            return Ok(None);
        };
        let adjacent = locator_at.checked_sub(ZIP64_END_LEN as u64);
        for at in [adjacent, Some(recorded_at)].into_iter().flatten() {
            let mut fixed = [0u8; ZIP64_END_LEN];
            if !read_within(dev, len, at, &mut fixed)? || le_u32(&fixed, 0) != Some(ZIP64_END_SIG) {
                continue;
            }
            // The size field counts everything after its own first twelve
            // bytes.
            let ends = le_u64(&fixed, 4).and_then(|size| at.checked_add(12)?.checked_add(size));
            if ends == Some(locator_at) {
                return Ok(Self::zip64_record(&fixed, at, disk, disks));
            }
        }
        Ok(None)
    }

    /// A ZIP64 end record's fields at `at`.
    fn zip64_record(fixed: &[u8; ZIP64_END_LEN], at: u64, disk: u32, disks: u32) -> Option<Self> {
        let entries_here = le_u64(fixed, 24)?;
        let entries = le_u64(fixed, 32)?;
        let directory_len = le_u64(fixed, 40)?;
        let directory = le_u64(fixed, 48)?;
        let split = disk != 0
            || disks > 1
            || le_u32(fixed, 16)? != 0
            || le_u32(fixed, 20)? != 0
            || entries_here != entries;
        Some(Self {
            base: 0,
            directory,
            directory_len,
            entries,
            zip64: true,
            split,
            end: at,
            comment: 0..0,
        })
    }
}

/// A tar block: every header and every member's data is whole blocks.
pub const TAR_BLOCK: usize = 512;

/// The value of a tar header's numeric `field`: octal digits, led by spaces
/// and ended by a space or a NUL as writers spell them, or the base-256 form
/// a GNU or star writer uses for a value too large for octal. `None` for a
/// field holding anything else.
#[must_use]
pub fn tar_number(field: &[u8]) -> Option<u64> {
    match field.first() {
        Some(0x80) => field[1..].iter().try_fold(0u64, |value, &byte| {
            value.checked_mul(256)?.checked_add(u64::from(byte))
        }),
        Some(_) => {
            let digits = field.iter().skip_while(|&&byte| byte == b' ');
            let mut value = 0u64;
            let mut ended = false;
            for &byte in digits {
                match byte {
                    b'0'..=b'7' if !ended => {
                        value = value.checked_mul(8)?.checked_add(u64::from(byte - b'0'))?;
                    }
                    b' ' | 0 => ended = true,
                    _ => return None,
                }
            }
            Some(value)
        }
        None => None,
    }
}

/// Whether `block` is a tar header: it names something, its type is one a
/// tar writer defines, and its checksum holds — summed unsigned as POSIX
/// asks, or signed as some old writers summed it, the checksum field counted
/// as spaces.
#[must_use]
pub fn tar_header_holds(block: &[u8; TAR_BLOCK]) -> bool {
    const TYPES: &[u8] = b"01234567xgLKSDMVN\0";
    if block[0] == 0 || !TYPES.contains(&block[156]) {
        return false;
    }
    let Some(stored) = tar_number(&block[148..156]) else {
        return false;
    };
    let (unsigned, signed) =
        block
            .iter()
            .enumerate()
            .fold((0u64, 0i64), |(unsigned, signed), (at, &byte)| {
                let byte = if (148..156).contains(&at) { b' ' } else { byte };
                (
                    unsigned + u64::from(byte),
                    signed + i64::from(byte.cast_signed()),
                )
            });
    stored == unsigned || i64::try_from(stored).is_ok_and(|stored| stored == signed)
}

/// gzip's magic bytes.
pub const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];
/// The longest gzip header the probe and the driver read: a member whose
/// extra field, name and comment run past it is not one this system opens.
pub const GZIP_HEADER_MAX: usize = 1 << 17;
/// The shortest gzip stream: a ten-byte header, an empty deflate block and
/// the eight-byte trailer.
const GZIP_MIN_LEN: u64 = 20;

/// A gzip member's header (RFC 1952 §2.3).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GzipHeader {
    /// Its length: where the compressed data starts.
    pub len: usize,
    /// The original file name it records, within the header's bytes, without
    /// its terminating NUL.
    pub name: Option<Range<usize>>,
    /// The modification time it records, in seconds since the Unix epoch;
    /// zero for none.
    pub mtime: u32,
}

impl GzipHeader {
    /// The header `bytes` open with, or `None` for bytes that open no gzip
    /// member this system reads: no magic, a method other than deflate, a
    /// reserved flag set, a field running past the bytes or
    /// [`GZIP_HEADER_MAX`], or a header checksum that does not hold.
    #[must_use]
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        const FHCRC: u8 = 0x02;
        const FEXTRA: u8 = 0x04;
        const FNAME: u8 = 0x08;
        const FCOMMENT: u8 = 0x10;
        let bytes = bytes.get(..bytes.len().min(GZIP_HEADER_MAX))?;
        if bytes.get(..2)? != GZIP_MAGIC || *bytes.get(2)? != 8 {
            return None;
        }
        let flags = *bytes.get(3)?;
        if flags & 0xE0 != 0 {
            return None;
        }
        let mtime = le_u32(bytes, 4)?;
        let mut at = 10usize;
        if flags & FEXTRA != 0 {
            let extra = usize::from(le_u16(bytes, at)?);
            at = at.checked_add(2 + extra)?;
        }
        let terminated = |at: usize| -> Option<(Range<usize>, usize)> {
            let field = bytes.get(at..)?;
            let nul = field.iter().position(|&byte| byte == 0)?;
            Some((at..at + nul, at + nul + 1))
        };
        let mut name = None;
        if flags & FNAME != 0 {
            let (field, next) = terminated(at)?;
            name = Some(field);
            at = next;
        }
        if flags & FCOMMENT != 0 {
            at = terminated(at)?.1;
        }
        if flags & FHCRC != 0 {
            let stored = le_u16(bytes, at)?;
            let crc = tairix_crc32::checksum(bytes.get(..at)?);
            if u32::from(stored) != crc & 0xFFFF {
                return None;
            }
            at += 2;
        }
        if at > bytes.len() {
            return None;
        }
        Some(Self {
            len: at,
            name,
            mtime,
        })
    }
}

/// A bzip2 block's magic, the digits of pi.
pub const BZIP2_BLOCK_MAGIC: u64 = 0x3141_5926_5359;
/// A bzip2 stream's end magic, the square root of pi.
pub const BZIP2_END_MAGIC: u64 = 0x1772_4538_5090;

/// The block size, in hundreds of kilobytes, of the bzip2 stream `head`
/// opens: `BZh`, the level digit, then a block's magic or, for an empty
/// stream, the end's.
#[must_use]
pub fn bzip2_level(head: &[u8]) -> Option<u8> {
    let prefix = head.get(..10)?;
    if &prefix[..3] != b"BZh" || !(b'1'..=b'9').contains(&prefix[3]) {
        return None;
    }
    let magic = prefix[4..10]
        .iter()
        .fold(0u64, |magic, &byte| magic << 8 | u64::from(byte));
    (magic == BZIP2_BLOCK_MAGIC || magic == BZIP2_END_MAGIC).then_some(prefix[3] - b'0')
}

/// A RAR archive's generation.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum RarVersion {
    /// RAR 1.5 to 4.x.
    Rar4,
    /// RAR 5.
    Rar5,
}

/// The signature every RAR 1.5–4 archive opens with.
pub const RAR4_SIGNATURE: &[u8; 7] = b"Rar!\x1a\x07\x00";
/// The length of a RAR 1.5–4 main header's fixed fields, which its checksum
/// covers.
pub const RAR4_MAIN_LEN: usize = 13;
/// The signature every RAR 5 archive opens with.
pub const RAR5_SIGNATURE: &[u8; 8] = b"Rar!\x1a\x07\x01\x00";
/// A RAR 5 main header's block type.
pub const RAR5_MAIN: u64 = 1;
/// A RAR 5 archive encryption header's block type.
pub const RAR5_ENCRYPTION: u64 = 4;

/// The generation of the RAR archive `head` opens, or `None`: its signature,
/// then a main archive header whose checksum holds.
#[must_use]
pub fn rar_version(head: &[u8]) -> Option<RarVersion> {
    if head.starts_with(RAR5_SIGNATURE) {
        let at = RAR5_SIGNATURE.len();
        let stored = le_u32(head, at)?;
        let (size, size_len) = rar5_vint(head.get(at + 4..)?)?;
        let body = at + 4 + size_len;
        let end = body.checked_add(usize::try_from(size).ok()?)?;
        let checked = head.get(at + 4..end)?;
        let (kind, _) = rar5_vint(head.get(body..end)?)?;
        // An archive whose headers are encrypted opens with its encryption
        // header in place of the main one.
        let opening = kind == RAR5_MAIN || kind == RAR5_ENCRYPTION;
        (opening && tairix_crc32::checksum(checked) == stored).then_some(RarVersion::Rar5)
    } else if head.starts_with(RAR4_SIGNATURE) {
        let at = RAR4_SIGNATURE.len();
        let stored = le_u16(head, at)?;
        let size = usize::from(le_u16(head, at + 5)?);
        if *head.get(at + 2)? != 0x73 || size < RAR4_MAIN_LEN {
            return None;
        }
        // The checksum covers the header's fixed fields, never the archive
        // comment RAR 1.5 and 2 embed after them.
        let checked = head.get(at + 2..at + RAR4_MAIN_LEN)?;
        (tairix_crc32::checksum(checked) & 0xFFFF == u32::from(stored)).then_some(RarVersion::Rar4)
    } else {
        None
    }
}

/// A RAR 5 variable-length integer at the start of `bytes`: seven bits a
/// byte, low first, each byte but the last with its top bit set, at most ten
/// bytes. Answers the value and how many bytes it took.
#[must_use]
pub fn rar5_vint(bytes: &[u8]) -> Option<(u64, usize)> {
    let mut value = 0u64;
    for (at, &byte) in bytes.iter().take(10).enumerate() {
        let shift = 7 * u32::try_from(at).ok()?;
        let part = u64::from(byte & 0x7F);
        if shift == 63 && part > 1 {
            return None;
        }
        value |= part << shift;
        if byte & 0x80 == 0 {
            return Some((value, at + 1));
        }
    }
    None
}

/// The magic an `ArcFS` archive opens with.
pub const ARCFS_MAGIC: &[u8; 8] = b"Archive\0";
/// The length of an `ArcFS` archive's header, ahead of its entries.
pub const ARCFS_HEADER_LEN: usize = 96;
/// The length of one `ArcFS` entry.
pub const ARCFS_ENTRY_LEN: usize = 36;

/// Where an `ArcFS` archive keeps its entries and its data.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ArcFsHeader {
    /// The entries' length in bytes, after the header.
    pub entries_len: u32,
    /// Where the data starts in the file.
    pub data: u32,
}

/// The `ArcFS` header `head` opens with, for an archive of `len` bytes: its
/// magic, then an entry table of whole entries and a data area within the
/// file and after the table.
#[must_use]
pub fn arcfs_header(head: &[u8], len: u64) -> Option<ArcFsHeader> {
    if !head.starts_with(ARCFS_MAGIC) || head.len() < 24 {
        return None;
    }
    let entries_len = le_u32(head, 8)?;
    let data = le_u32(head, 12)?;
    let table_end = (ARCFS_HEADER_LEN as u64).checked_add(u64::from(entries_len))?;
    let whole = (entries_len as usize).is_multiple_of(ARCFS_ENTRY_LEN);
    (whole && table_end <= u64::from(data) && u64::from(data) <= len)
        .then_some(ArcFsHeader { entries_len, data })
}

/// The byte every ARC header opens with.
pub const ARC_MARKER: u8 = 0x1A;
/// The bit an ARC header's method byte carries in a Spark archive, saying the
/// RISC OS load, exec and attribute words follow the header.
pub const ARC_RISCOS_BIT: u8 = 0x80;
/// The longest ARC member name, with its terminating NUL.
pub const ARC_NAME_LEN: usize = 13;
/// The length of an ARC header past the original type, before Spark's
/// RISC OS words: marker, method, name, compressed size, date, time, CRC and
/// original size.
pub const ARC_HEADER_LEN: usize = 29;

/// Whether `head`, the start of a file of `len` bytes, opens with an ARC
/// header — the Spark archive's, or the plain ARC one it extends: the
/// marker, a method the family defines (or the end of an empty archive), a
/// name ended within its field, and a member that fits in the file.
#[must_use]
pub fn arc_entry_holds(head: &[u8], len: u64) -> bool {
    let (Some(&marker), Some(&kind)) = (head.first(), head.get(1)) else {
        return false;
    };
    if marker != ARC_MARKER {
        return false;
    }
    let method = kind & !ARC_RISCOS_BIT;
    if method == 0 {
        // An empty archive is its end marker alone.
        return len == 2;
    }
    if !matches!(method, 1..=9 | 0x7F) {
        return false;
    }
    let Some(name) = head.get(2..2 + ARC_NAME_LEN) else {
        return false;
    };
    let Some(nul) = name.iter().position(|&byte| byte == 0) else {
        return false;
    };
    if nul == 0 || name[..nul].iter().any(|&byte| byte < 0x20 || byte == 0x7F) {
        return false;
    }
    let header = if method == 1 {
        ARC_HEADER_LEN - 4
    } else {
        ARC_HEADER_LEN
    } + if kind & ARC_RISCOS_BIT == 0 { 0 } else { 12 };
    le_u32(head, 15).is_some_and(|stored| {
        (header as u64)
            .checked_add(u64::from(stored))
            .is_some_and(|end| end <= len)
    })
}

/// The bytes of `range` of the first `len` bytes of `dev`, read whole;
/// `None` where the range runs past them.
///
/// # Errors
///
/// A device fault, or a refused allocation.
pub fn read_range<B: Block>(
    dev: &mut ByteDevice<B>,
    len: u64,
    range: Range<u64>,
) -> Result<Option<Vec<u8>>, DriverError> {
    if range.end > len || range.start > range.end {
        return Ok(None);
    }
    let size = usize::try_from(range.end - range.start).map_err(|_| DriverError::OutOfMemory)?;
    let mut bytes = tairix_util::fallible::filled(size, 0u8).ok_or(DriverError::OutOfMemory)?;
    dev.read(range.start, &mut bytes)?;
    Ok(Some(bytes))
}

#[cfg(test)]
#[path = "archive_tests.rs"]
mod tests;
