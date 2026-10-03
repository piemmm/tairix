//! The ZIP container OpenRaster is carried in (APPNOTE 6.3.x, the subset
//! OpenRaster uses).
//!
//! Writing stores every entry: what OpenRaster carries is PNG, already
//! compressed, and an XML stack small enough not to matter. Reading takes an
//! entry stored or deflated, found through the central directory and checked
//! against its CRC-32; an encrypted entry, a ZIP64 archive, a size past the
//! file or past what the caller allows, and any structural damage are all
//! refused rather than guessed at.

use alloc::vec::Vec;

use tairix_compress::inflate::inflate_into;
use tairix_crc32::checksum as crc32;
use tairix_util::fallible;

/// Local file header, central directory header and end of central directory
/// signatures (APPNOTE 4.3.7, 4.3.12, 4.3.16).
const LOCAL: u32 = 0x0403_4b50;
const CENTRAL: u32 = 0x0201_4b50;
const END: u32 = 0x0605_4b50;

const LOCAL_LEN: usize = 30;
const CENTRAL_LEN: usize = 46;
const END_LEN: usize = 22;

/// The version needed to extract what is written here: stored and deflated
/// entries, no ZIP64 (APPNOTE 4.4.3.2).
const VERSION: u16 = 20;

const STORED: u16 = 0;
const DEFLATED: u16 = 8;

/// 1980-01-01 00:00, the earliest an MS-DOS date holds: written for every
/// entry, so the same document always writes the same bytes.
const DOS_DATE: u16 = 0x0021;

/// The most entries one archive is read with: a fixed defence against a
/// directory that claims millions, not a capacity.
pub(crate) const MOST_ENTRIES: usize = 4096;

/// Why an archive could not be read.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum ZipError {
    /// Structurally damaged, truncated, or past the archive's own bounds.
    Malformed,
    /// Encrypted, ZIP64, or a compression this reader does not take.
    Unsupported,
    /// An entry larger than the caller allows.
    TooLarge,
    /// No room to hold an entry.
    OutOfMemory,
}

/// An archive being written, entry by entry.
pub(crate) struct Writer {
    bytes: Vec<u8>,
    central: Vec<u8>,
    count: u16,
}

impl Writer {
    pub(crate) const fn new() -> Self {
        Self {
            bytes: Vec::new(),
            central: Vec::new(),
            count: 0,
        }
    }

    /// Store `data` as entry `name`.
    pub(crate) fn store(&mut self, name: &str, data: &[u8]) -> Result<(), ZipError> {
        let offset = u32::try_from(self.bytes.len()).map_err(|_| ZipError::TooLarge)?;
        let size = u32::try_from(data.len()).map_err(|_| ZipError::TooLarge)?;
        let name_len = u16::try_from(name.len()).map_err(|_| ZipError::TooLarge)?;
        self.count = self.count.checked_add(1).ok_or(ZipError::TooLarge)?;
        let crc = crc32(data);
        let mut local = [0u8; LOCAL_LEN];
        put32(&mut local, 0, LOCAL);
        put16(&mut local, 4, VERSION);
        put16(&mut local, 8, STORED);
        put16(&mut local, 12, DOS_DATE);
        put32(&mut local, 14, crc);
        put32(&mut local, 18, size);
        put32(&mut local, 22, size);
        put16(&mut local, 26, name_len);
        let mut central = [0u8; CENTRAL_LEN];
        put32(&mut central, 0, CENTRAL);
        put16(&mut central, 4, VERSION);
        put16(&mut central, 6, VERSION);
        put16(&mut central, 10, STORED);
        put16(&mut central, 14, DOS_DATE);
        put32(&mut central, 16, crc);
        put32(&mut central, 20, size);
        put32(&mut central, 24, size);
        put16(&mut central, 28, name_len);
        put32(&mut central, 42, offset);
        let more = LOCAL_LEN + name.len() + data.len();
        if !fallible::reserve(&mut self.bytes, more)
            || !fallible::reserve(&mut self.central, CENTRAL_LEN + name.len())
        {
            return Err(ZipError::OutOfMemory);
        }
        self.bytes.extend_from_slice(&local);
        self.bytes.extend_from_slice(name.as_bytes());
        self.bytes.extend_from_slice(data);
        self.central.extend_from_slice(&central);
        self.central.extend_from_slice(name.as_bytes());
        Ok(())
    }

    /// The archive, its central directory and end record appended.
    pub(crate) fn finish(mut self) -> Result<Vec<u8>, ZipError> {
        let offset = u32::try_from(self.bytes.len()).map_err(|_| ZipError::TooLarge)?;
        let size = u32::try_from(self.central.len()).map_err(|_| ZipError::TooLarge)?;
        let mut end = [0u8; END_LEN];
        put32(&mut end, 0, END);
        put16(&mut end, 8, self.count);
        put16(&mut end, 10, self.count);
        put32(&mut end, 12, size);
        put32(&mut end, 16, offset);
        if !fallible::reserve(&mut self.bytes, self.central.len() + END_LEN) {
            return Err(ZipError::OutOfMemory);
        }
        self.bytes.extend_from_slice(&self.central);
        self.bytes.extend_from_slice(&end);
        Ok(self.bytes)
    }
}

fn put16(out: &mut [u8], at: usize, value: u16) {
    out[at..at + 2].copy_from_slice(&value.to_le_bytes());
}

fn put32(out: &mut [u8], at: usize, value: u32) {
    out[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

fn get16(bytes: &[u8], at: usize) -> Result<u16, ZipError> {
    bytes
        .get(at..at + 2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .ok_or(ZipError::Malformed)
}

fn get32(bytes: &[u8], at: usize) -> Result<u32, ZipError> {
    bytes
        .get(at..at + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or(ZipError::Malformed)
}

/// One entry, as the central directory describes it.
#[derive(Copy, Clone, Debug)]
struct Entry {
    name: (usize, usize),
    method: u16,
    crc: u32,
    packed: u32,
    size: u32,
    local: u32,
}

/// An archive read through its central directory.
pub(crate) struct Archive<'a> {
    bytes: &'a [u8],
    entries: Vec<Entry>,
}

impl<'a> Archive<'a> {
    /// Read `bytes`' central directory, refusing anything this reader cannot
    /// vouch for.
    pub(crate) fn open(bytes: &'a [u8]) -> Result<Self, ZipError> {
        let end_at = find_end(bytes)?;
        let count = usize::from(get16(bytes, end_at + 10)?);
        let directory =
            usize::try_from(get32(bytes, end_at + 12)?).map_err(|_| ZipError::Malformed)?;
        let offset =
            usize::try_from(get32(bytes, end_at + 16)?).map_err(|_| ZipError::Malformed)?;
        // A second disk is a spanned archive.
        if get16(bytes, end_at + 4)? != 0 || usize::from(get16(bytes, end_at + 8)?) != count {
            return Err(ZipError::Unsupported);
        }
        let directory_end = offset.checked_add(directory).ok_or(ZipError::Malformed)?;
        if count > MOST_ENTRIES || directory_end > end_at {
            return Err(ZipError::Malformed);
        }
        let mut entries = Vec::new();
        if entries.try_reserve_exact(count).is_err() {
            return Err(ZipError::OutOfMemory);
        }
        let mut at = offset;
        for _ in 0..count {
            if get32(bytes, at)? != CENTRAL {
                return Err(ZipError::Malformed);
            }
            let flags = get16(bytes, at + 8)?;
            let packed = get32(bytes, at + 20)?;
            let size = get32(bytes, at + 24)?;
            let local = get32(bytes, at + 42)?;
            // Bit 0 is encryption; a size of all ones is a ZIP64 entry.
            if flags & 1 != 0 || [packed, size, local].contains(&u32::MAX) {
                return Err(ZipError::Unsupported);
            }
            let name_len = usize::from(get16(bytes, at + 28)?);
            let extra_len = usize::from(get16(bytes, at + 30)?);
            let comment_len = usize::from(get16(bytes, at + 32)?);
            let name = (at + CENTRAL_LEN, name_len);
            if bytes.get(name.0..name.0 + name_len).is_none() {
                return Err(ZipError::Malformed);
            }
            entries.push(Entry {
                name,
                method: get16(bytes, at + 10)?,
                crc: get32(bytes, at + 16)?,
                packed,
                size,
                local,
            });
            at = name.0 + name_len + extra_len + comment_len;
            if at > directory_end {
                return Err(ZipError::Malformed);
            }
        }
        Ok(Self { bytes, entries })
    }

    fn name(&self, entry: &Entry) -> &'a [u8] {
        &self.bytes[entry.name.0..entry.name.0 + entry.name.1]
    }

    /// The names its entries carry, in directory order.
    pub(crate) fn names(&self) -> impl Iterator<Item = &'a [u8]> + '_ {
        self.entries.iter().map(|entry| self.name(entry))
    }

    /// The bytes of the entry named `name`, no more than `most` of them,
    /// checked against its CRC-32; `None` where no entry is so named.
    pub(crate) fn read(&self, name: &str, most: usize) -> Result<Option<Vec<u8>>, ZipError> {
        let Some(entry) = self
            .entries
            .iter()
            .find(|entry| self.name(entry) == name.as_bytes())
        else {
            return Ok(None);
        };
        let size = usize::try_from(entry.size).map_err(|_| ZipError::TooLarge)?;
        if size > most {
            return Err(ZipError::TooLarge);
        }
        let local = usize::try_from(entry.local).map_err(|_| ZipError::Malformed)?;
        if get32(self.bytes, local)? != LOCAL {
            return Err(ZipError::Malformed);
        }
        let start = local
            + LOCAL_LEN
            + usize::from(get16(self.bytes, local + 26)?)
            + usize::from(get16(self.bytes, local + 28)?);
        let packed = usize::try_from(entry.packed).map_err(|_| ZipError::Malformed)?;
        let data = self
            .bytes
            .get(start..start.checked_add(packed).ok_or(ZipError::Malformed)?)
            .ok_or(ZipError::Malformed)?;
        let out = match entry.method {
            STORED if packed == size => {
                fallible::collected(size, data.iter().copied()).ok_or(ZipError::OutOfMemory)?
            }
            DEFLATED => {
                let mut out = fallible::filled(size, 0u8).ok_or(ZipError::OutOfMemory)?;
                let made = inflate_into(data, &mut out).map_err(|_| ZipError::Malformed)?;
                if made != size {
                    return Err(ZipError::Malformed);
                }
                out
            }
            STORED => return Err(ZipError::Malformed),
            _ => return Err(ZipError::Unsupported),
        };
        if crc32(&out) != entry.crc {
            return Err(ZipError::Malformed);
        }
        Ok(Some(out))
    }
}

/// Where the end of central directory record starts: the last one in the
/// final 64 KiB plus its own length, the most a trailing comment can push it
/// back.
fn find_end(bytes: &[u8]) -> Result<usize, ZipError> {
    if bytes.len() < END_LEN {
        return Err(ZipError::Malformed);
    }
    let earliest = bytes.len().saturating_sub(END_LEN + usize::from(u16::MAX));
    (earliest..=bytes.len() - END_LEN)
        .rev()
        .find(|&at| get32(bytes, at).is_ok_and(|signature| signature == END))
        .ok_or(ZipError::Malformed)
}

#[cfg(test)]
#[path = "zip_tests.rs"]
mod tests;
