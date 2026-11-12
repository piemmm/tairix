//! Directory entries: the 32-byte short entry every file and directory has,
//! and the VFAT long-name entries that may precede it, each carrying 13
//! UTF-16 units of the long name and a checksum binding them to the short
//! name they belong to.

use crate::stamp::{Stamp, Stamps};
use crate::{le16, le32, put16, put32};

/// The bytes of one directory entry.
pub const ENTRY: usize = 32;
/// A first name byte: the directory ends here.
pub const END: u8 = 0x00;
/// A first name byte: the entry was deleted.
pub const DELETED: u8 = 0xE5;
/// A first name byte standing for a name whose first byte is `E5h`, which
/// would otherwise read as deleted.
pub const KANJI: u8 = 0x05;
/// The most entries a directory may hold.
pub const MAX_ENTRIES: u32 = 65_536;
/// The units of the long name one long-name entry carries.
pub const LONG_UNITS: usize = 13;
/// The most units a long name has.
pub const LONG_NAME_UNITS: usize = 255;
/// The most bytes a long name takes as UTF-8: three for each unit, a
/// surrogate pair's two units making four.
pub const LONG_NAME_BYTES: usize = LONG_NAME_UNITS * 3;
/// The most long-name entries a name takes.
pub const LONG_FRAGMENTS: usize = LONG_NAME_UNITS.div_ceil(LONG_UNITS);
/// A long-name entry's order byte: it is the name's last, which a
/// directory lists first.
pub const LAST: u8 = 0x40;
/// A long-name entry's order byte: its place in the name, from 1.
pub const SEQUENCE: u8 = 0x1F;
/// Where a long-name entry's units lie.
pub const UNIT_AT: [usize; LONG_UNITS] = [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30];
/// The name of a directory's link to itself.
pub const DOT: [u8; 11] = *b".          ";
/// The name of a directory's link to its parent.
pub const DOTDOT: [u8; 11] = *b"..         ";

/// Where an entry's fields lie.
pub mod at {
    /// The 8.3 name; a long-name entry's order byte.
    pub const NAME: usize = 0;
    /// The attribute bits.
    pub const ATTRIBUTES: usize = 11;
    /// Windows NT's lower-case flags.
    pub const CASE: usize = 12;
    /// The creation stamp's hundredths; a long-name entry's checksum.
    pub const CREATED_CENTIS: usize = 13;
    /// The creation stamp's time.
    pub const CREATED_TIME: usize = 14;
    /// The creation stamp's date.
    pub const CREATED_DATE: usize = 16;
    /// The last-read date.
    pub const ACCESSED: usize = 18;
    /// The first cluster's high half.
    pub const CLUSTER_HIGH: usize = 20;
    /// The last-write stamp's time.
    pub const WRITTEN_TIME: usize = 22;
    /// The last-write stamp's date.
    pub const WRITTEN_DATE: usize = 24;
    /// The first cluster's low half.
    pub const CLUSTER_LOW: usize = 26;
    /// The size.
    pub const SIZE: usize = 28;
    /// A long-name entry's checksum of the short name it belongs to.
    pub const CHECKSUM: usize = CREATED_CENTIS;
}

/// An entry's attribute bits.
pub mod attr {
    /// Not to be written.
    pub const READ_ONLY: u8 = 0x01;
    /// Not listed.
    pub const HIDDEN: u8 = 0x02;
    /// The system's own.
    pub const SYSTEM: u8 = 0x04;
    /// The volume's label, not a file.
    pub const VOLUME_ID: u8 = 0x08;
    /// A directory.
    pub const DIRECTORY: u8 = 0x10;
    /// Changed since last archived.
    pub const ARCHIVE: u8 = 0x20;
    /// A long-name entry's attributes, which no short entry has.
    pub const LONG_NAME: u8 = 0x0F;
    /// The bits a long-name entry is told by.
    pub const LONG_NAME_MASK: u8 = 0x3F;
}

/// What a directory slot holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    /// The directory's end: no entry follows.
    End,
    /// A deleted entry.
    Deleted,
    /// A long-name entry.
    Long,
    /// The volume's label.
    Label,
    /// A file's or directory's short entry.
    Short,
}

/// What `raw` holds.
#[must_use]
pub fn slot(raw: &[u8; ENTRY]) -> Slot {
    let attributes = raw[at::ATTRIBUTES];
    match raw[at::NAME] {
        END => Slot::End,
        DELETED => Slot::Deleted,
        _ if attributes & attr::LONG_NAME_MASK == attr::LONG_NAME => Slot::Long,
        _ if attributes & attr::VOLUME_ID != 0 => Slot::Label,
        _ => Slot::Short,
    }
}

/// A short directory entry.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ShortEntry {
    /// The 8.3 name: eight bytes of base and three of extension, space
    /// padded.
    pub name: [u8; 11],
    /// Its [`attr`] bits.
    pub attributes: u8,
    /// `DIR_NTRes`: Windows NT's lower-case flags for the base and
    /// extension.
    pub case: u8,
    /// Its stamps.
    pub stamps: Stamps,
    /// Its first cluster; 0 for none.
    pub cluster: u32,
    /// Its size in bytes; 0 for a directory.
    pub size: u32,
}

impl ShortEntry {
    /// The entry `raw` holds.
    #[must_use]
    pub fn decode(raw: &[u8; ENTRY]) -> Self {
        let word = |offset| le16(raw, offset).unwrap_or(0);
        let mut name = [0; 11];
        name.copy_from_slice(&raw[at::NAME..at::NAME + 11]);
        Self {
            name,
            attributes: raw[at::ATTRIBUTES],
            case: raw[at::CASE],
            stamps: Stamps {
                created: Stamp {
                    date: word(at::CREATED_DATE),
                    time: word(at::CREATED_TIME),
                    centis: raw[at::CREATED_CENTIS],
                },
                accessed: word(at::ACCESSED),
                written: Stamp {
                    date: word(at::WRITTEN_DATE),
                    time: word(at::WRITTEN_TIME),
                    centis: 0,
                },
            },
            cluster: (u32::from(word(at::CLUSTER_HIGH)) << 16) | u32::from(word(at::CLUSTER_LOW)),
            size: le32(raw, at::SIZE).unwrap_or(0),
        }
    }

    /// The entry's 32 bytes.
    #[must_use]
    pub fn encode(&self) -> [u8; ENTRY] {
        let mut raw = [0; ENTRY];
        raw[at::NAME..at::NAME + 11].copy_from_slice(&self.name);
        raw[at::ATTRIBUTES] = self.attributes;
        raw[at::CASE] = self.case;
        raw[at::CREATED_CENTIS] = self.stamps.created.centis;
        put16(&mut raw, at::CREATED_TIME, self.stamps.created.time);
        put16(&mut raw, at::CREATED_DATE, self.stamps.created.date);
        put16(&mut raw, at::ACCESSED, self.stamps.accessed);
        let [low_0, low_1, high_0, high_1] = self.cluster.to_le_bytes();
        raw[at::CLUSTER_HIGH..at::CLUSTER_HIGH + 2].copy_from_slice(&[high_0, high_1]);
        put16(&mut raw, at::WRITTEN_TIME, self.stamps.written.time);
        put16(&mut raw, at::WRITTEN_DATE, self.stamps.written.date);
        raw[at::CLUSTER_LOW..at::CLUSTER_LOW + 2].copy_from_slice(&[low_0, low_1]);
        put32(&mut raw, at::SIZE, self.size);
        raw
    }

    /// Whether it is a directory's.
    #[must_use]
    pub const fn is_directory(&self) -> bool {
        self.attributes & attr::DIRECTORY != 0
    }

    /// Whether it is a directory's `.` or `..` link: no other name opens
    /// with a dot.
    #[must_use]
    pub const fn is_dot(&self) -> bool {
        self.name[0] == b'.'
    }
}

/// The checksum binding long-name entries to the short name `short`, over
/// its stored bytes as they are — a `05h` lead included.
#[must_use]
pub fn checksum(short: &[u8; 11]) -> u8 {
    short
        .iter()
        .fold(0u8, |sum, &byte| sum.rotate_right(1).wrapping_add(byte))
}

/// The long-name entry for place `sequence` (from 1) of a name of `units`,
/// flagged its last when `last`: past the name's end, one `0000h` and then
/// `FFFFh`.
#[must_use]
pub fn long_entry(units: &[u16], sequence: usize, last: bool, checksum: u8) -> [u8; ENTRY] {
    let mut raw = [0; ENTRY];
    raw[at::NAME] = u8::try_from(sequence).unwrap_or(0) | if last { LAST } else { 0 };
    raw[at::ATTRIBUTES] = attr::LONG_NAME;
    raw[at::CHECKSUM] = checksum;
    let base = sequence.saturating_sub(1) * LONG_UNITS;
    for (k, &at) in UNIT_AT.iter().enumerate() {
        let index = base + k;
        let unit = match index.cmp(&units.len()) {
            core::cmp::Ordering::Less => units[index],
            core::cmp::Ordering::Equal => 0x0000,
            core::cmp::Ordering::Greater => 0xFFFF,
        };
        put16(&mut raw, at, unit);
    }
    raw
}

/// The long-name entries a name of `units` takes, in the order a directory
/// holds them: its last place first.
pub fn long_entries(units: &[u16], checksum: u8) -> impl Iterator<Item = [u8; ENTRY]> + '_ {
    let count = units.len().div_ceil(LONG_UNITS);
    (1..=count)
        .rev()
        .map(move |sequence| long_entry(units, sequence, sequence == count, checksum))
}

/// A long name, reassembled from its entries as a directory lists them:
/// its last place first, each place one below the last, every checksum
/// alike.
#[derive(Clone, Debug)]
pub struct LongName {
    units: [u16; LONG_FRAGMENTS * LONG_UNITS],
    total: usize,
    next: u8,
    checksum: u8,
    started: bool,
    valid: bool,
}

impl Default for LongName {
    fn default() -> Self {
        Self::new()
    }
}

impl LongName {
    /// Nothing gathered.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            units: [0; LONG_FRAGMENTS * LONG_UNITS],
            total: 0,
            next: 0,
            checksum: 0,
            started: false,
            valid: false,
        }
    }

    /// Forgets what was gathered.
    pub fn reset(&mut self) {
        self.started = false;
        self.valid = false;
        self.total = 0;
        self.next = 0;
    }

    /// Takes in the long-name entry `raw`.
    pub fn push(&mut self, raw: &[u8; ENTRY]) {
        let order = raw[at::NAME];
        if order == DELETED {
            self.reset();
            return;
        }
        let sequence = order & SEQUENCE;
        if sequence == 0 || usize::from(sequence) > LONG_FRAGMENTS {
            self.reset();
            return;
        }
        if order & LAST != 0 {
            self.units = [0; LONG_FRAGMENTS * LONG_UNITS];
            self.total = usize::from(sequence) * LONG_UNITS;
            self.checksum = raw[at::CHECKSUM];
            self.next = sequence;
            self.started = true;
            self.valid = true;
        } else if !self.started
            || !self.valid
            || sequence != self.next
            || raw[at::CHECKSUM] != self.checksum
        {
            self.valid = false;
            return;
        }
        let base = (usize::from(sequence) - 1) * LONG_UNITS;
        for (slot, &at) in UNIT_AT.iter().enumerate() {
            self.units[base + slot] = le16(raw, at).unwrap_or(0);
        }
        self.next = sequence - 1;
    }

    /// The name, as UTF-8 in `out`, if a whole set binding to `short` was
    /// gathered: its length, never 0.
    #[must_use]
    pub fn finish(&self, short: &[u8; 11], out: &mut [u8]) -> Option<usize> {
        if !self.started || !self.valid || self.next != 0 || self.checksum != checksum(short) {
            return None;
        }
        decode_utf16(&self.units[..self.total], out).filter(|&len| len > 0)
    }
}

/// `units` as UTF-8 in `out`, up to the first `0000h`: its length, or
/// `None` for an unpaired surrogate or a name `out` cannot hold.
#[must_use]
pub fn decode_utf16(units: &[u16], out: &mut [u8]) -> Option<usize> {
    let end = units
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(units.len());
    let mut written = 0usize;
    for decoded in char::decode_utf16(units[..end].iter().copied()) {
        let mut scratch = [0u8; 4];
        let encoded = decoded.ok()?.encode_utf8(&mut scratch).as_bytes();
        let next = written.checked_add(encoded.len())?;
        out.get_mut(written..next)?.copy_from_slice(encoded);
        written = next;
    }
    Some(written)
}

/// UTF-8 `name` as UTF-16 in `units`: their count, or `None` for text that
/// is no UTF-8 or a name of more than [`LONG_NAME_UNITS`] units.
#[must_use]
pub fn encode_utf16(name: &[u8], units: &mut [u16; LONG_NAME_UNITS]) -> Option<usize> {
    let text = core::str::from_utf8(name).ok()?;
    let mut count = 0usize;
    for unit in text.encode_utf16() {
        *units.get_mut(count)? = unit;
        count += 1;
    }
    Some(count)
}

#[cfg(test)]
#[path = "dir_tests.rs"]
mod tests;
