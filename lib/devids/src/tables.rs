//! The compiled ID-database table: format, fail-closed decoder, and
//! allocation-free binary-search lookups.
//!
//! `cargo xtask devids` compiles a vetted snapshot into one flat file per
//! database. The layout is little-endian throughout:
//!
//! ```text
//! header (36 bytes):
//!   magic        [u8; 8]   "RDEVIDS1"
//!   kind         u32       1 = PCI, 2 = USB, 3 = OUI
//!   vendors      u32       record count
//!   devices      u32       record count
//!   classes      u32       record count
//!   subclasses   u32       record count
//!   prog_ifs     u32       record count
//!   strings_len  u32       bytes of UTF-8 name data
//! five record tables, each `count × 12` bytes, sorted strictly
//! ascending by key:
//!   key          u32       see the per-table key layout below
//!   name_off     u32       byte offset into the strings blob
//!   name_len     u32       byte length of the name
//! strings blob (`strings_len` bytes): concatenated UTF-8 names.
//! ```
//!
//! Per-table keys: vendors `id` (for OUI, the organisation's 24-bit
//! assignment, and the only table it fills); devices `vendor << 16 | device`;
//! classes `class`; subclasses `class << 8 | subclass`; prog-ifs (USB:
//! protocols) `class << 16 | subclass << 8 | prog_if`.
//!
//! [`DevIds::parse`] validates the whole structure up front — magic, kind,
//! bounded counts, exact file length, strict key order, every name slice
//! in-bounds on UTF-8 character boundaries — so the lookups are infallible
//! slicing over a proven view. The file ships on the read-only system volume
//! but is still data, never trusted blindly.

use crate::{DbKind, MAX_NAME_BYTES, MAX_TABLE_ENTRIES};

/// Magic prefix of a compiled table file (format version 1).
pub const TABLE_MAGIC: [u8; 8] = *b"RDEVIDS1";

/// Header length in bytes.
const HEADER_LEN: usize = 36;

/// Record length in bytes (`key`, `name_off`, `name_len`, each `u32`).
const RECORD_LEN: usize = 12;

/// Why a compiled table was rejected.
///
/// Every variant is a whole-file rejection: a table that fails any check is
/// not partially usable.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TableError {
    /// The file is shorter than the fixed header.
    TruncatedHeader,
    /// The magic prefix is not [`TABLE_MAGIC`].
    BadMagic,
    /// The header's kind discriminant is not the expected database.
    WrongKind,
    /// A record count exceeds [`MAX_TABLE_ENTRIES`].
    CountTooLarge,
    /// The file length does not exactly match the header's counts.
    LengthMismatch,
    /// The strings blob is not valid UTF-8.
    StringsNotUtf8,
    /// A record table's keys are not strictly ascending.
    UnsortedRecords,
    /// A record's name slice is out of bounds, empty, over
    /// [`MAX_NAME_BYTES`], or not on UTF-8 character boundaries.
    BadNameSlice,
    /// The table carries records its kind has no place for: an `oui.ids`
    /// table holds organisations alone.
    WrongShape,
}

impl core::fmt::Display for TableError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::TruncatedHeader => "the table is shorter than its header",
            Self::BadMagic => "the file is not a compiled table",
            Self::WrongKind => "the table holds another database",
            Self::CountTooLarge => "a record count exceeds its bound",
            Self::LengthMismatch => "the table's length disagrees with its header",
            Self::StringsNotUtf8 => "the table's names are not UTF-8",
            Self::UnsortedRecords => "the table's records are out of order",
            Self::BadNameSlice => "a record's name lies outside the table",
            Self::WrongShape => "the table holds records its database has no place for",
        })
    }
}

/// A validated, allocation-free lookup table over one compiled ID database,
/// borrowing its bytes (`DevIds<&[u8]>`) or owning them (`DevIds<Vec<u8>>`).
///
/// Constructed by [`DevIds::parse`], which validates the whole file once, so a
/// long-lived owner never revalidates; every lookup is an O(log n) binary
/// search over the sorted record table, returning `None` for an id the
/// database does not name (the caller renders the numeric form).
#[derive(Debug)]
pub struct DevIds<B> {
    kind: DbKind,
    bytes: B,
    /// The byte spans of the vendor, device, class, subclass and prog-if
    /// record tables, in that order.
    tables: [Span; 5],
    strings: Span,
}

/// A validated byte range of the table file.
#[derive(Copy, Clone, Debug)]
struct Span {
    start: usize,
    end: usize,
}

impl Span {
    fn of(self, bytes: &[u8]) -> &[u8] {
        bytes.get(self.start..self.end).unwrap_or(&[])
    }
}

/// Which record table a lookup searches.
#[derive(Copy, Clone)]
enum Table {
    Vendors,
    Devices,
    Classes,
    Subclasses,
    ProgIfs,
}

/// The `u32` at `off` in `bytes` (little-endian). `off` is a compile-time
/// header offset the caller has already bounds-checked.
fn le32(bytes: &[u8], off: usize) -> Option<u32> {
    let b = bytes.get(off..off + 4)?;
    Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// The `(key, name_off, name_len)` of record `index` in a record table.
fn record(table: &[u8], index: usize) -> (u32, u32, u32) {
    let at = index * RECORD_LEN;
    // The slice length is a validated multiple of `RECORD_LEN`, so these
    // reads cannot fail; a defensive zero would mask a logic error, so the
    // impossible branch maps to an empty record instead of a panic.
    let field = |o: usize| le32(table, at + o).unwrap_or(0);
    (field(0), field(4), field(8))
}

impl<B: AsRef<[u8]>> DevIds<B> {
    /// Validate `bytes` as a compiled table of `kind` and return the lookup
    /// table over them.
    ///
    /// # Errors
    ///
    /// Any structural defect rejects the whole file; see [`TableError`].
    pub fn parse(kind: DbKind, bytes: B) -> Result<Self, TableError> {
        let all = bytes.as_ref();
        if all.len() < HEADER_LEN {
            return Err(TableError::TruncatedHeader);
        }
        if all[..8] != TABLE_MAGIC {
            return Err(TableError::BadMagic);
        }
        // The header reads below are within `HEADER_LEN`, checked above.
        let header = |off: usize| le32(all, off).unwrap_or(0);
        if header(8) != kind.code() {
            return Err(TableError::WrongKind);
        }
        let counts = [header(12), header(16), header(20), header(24), header(28)];
        if counts.iter().any(|&c| c > MAX_TABLE_ENTRIES) {
            return Err(TableError::CountTooLarge);
        }
        if kind == DbKind::Oui && counts[1..].iter().any(|&c| c != 0) {
            return Err(TableError::WrongShape);
        }
        let strings_len = header(32) as usize;
        let mut records_len = 0usize;
        for &c in &counts {
            records_len += c as usize * RECORD_LEN;
        }
        let expected = HEADER_LEN
            .checked_add(records_len)
            .and_then(|n| n.checked_add(strings_len))
            .ok_or(TableError::LengthMismatch)?;
        if all.len() != expected {
            return Err(TableError::LengthMismatch);
        }
        let strings = Span {
            start: HEADER_LEN + records_len,
            end: expected,
        };
        let text = core::str::from_utf8(strings.of(all)).map_err(|_| TableError::StringsNotUtf8)?;
        let mut at = HEADER_LEN;
        let mut tables = [Span { start: 0, end: 0 }; 5];
        for (slot, &count) in tables.iter_mut().zip(&counts) {
            let len = count as usize * RECORD_LEN;
            *slot = Span {
                start: at,
                end: at + len,
            };
            at += len;
            validate_table(slot.of(all), text)?;
        }
        Ok(Self {
            kind,
            bytes,
            tables,
            strings,
        })
    }

    /// Which database this table carries.
    #[must_use]
    pub fn kind(&self) -> DbKind {
        self.kind
    }

    /// The vendor name for `vendor`, if the database has one.
    #[must_use]
    pub fn vendor(&self, vendor: u16) -> Option<&str> {
        self.find(Table::Vendors, u32::from(vendor))
    }

    /// The organisation the IEEE assigned the 24-bit `oui` to, if an
    /// `oui.ids` table names one.
    #[must_use]
    pub fn organisation(&self, oui: u32) -> Option<&str> {
        if self.kind == DbKind::Oui {
            self.find(Table::Vendors, oui)
        } else {
            None
        }
    }

    /// The device (USB: product) name for `device` under `vendor`, if the
    /// database has one.
    #[must_use]
    pub fn device(&self, vendor: u16, device: u16) -> Option<&str> {
        self.find(Table::Devices, u32::from(vendor) << 16 | u32::from(device))
    }

    /// The class name for `class`, if the database has one.
    #[must_use]
    pub fn class(&self, class: u8) -> Option<&str> {
        self.find(Table::Classes, u32::from(class))
    }

    /// The subclass name for `subclass` under `class`, if the database has
    /// one.
    #[must_use]
    pub fn subclass(&self, class: u8, subclass: u8) -> Option<&str> {
        self.find(
            Table::Subclasses,
            u32::from(class) << 8 | u32::from(subclass),
        )
    }

    /// The programming-interface (USB: protocol) name for `prog_if` under
    /// `class:subclass`, if the database has one.
    #[must_use]
    pub fn prog_if(&self, class: u8, subclass: u8, prog_if: u8) -> Option<&str> {
        self.find(
            Table::ProgIfs,
            u32::from(class) << 16 | u32::from(subclass) << 8 | u32::from(prog_if),
        )
    }

    /// Binary-search `table` for `key` and slice its name out of the proven
    /// strings blob.
    fn find(&self, table: Table, key: u32) -> Option<&str> {
        let all = self.bytes.as_ref();
        let records = self.tables[table as usize].of(all);
        let count = records.len() / RECORD_LEN;
        let (mut lo, mut hi) = (0usize, count);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let (k, off, len) = record(records, mid);
            match k.cmp(&key) {
                core::cmp::Ordering::Less => lo = mid + 1,
                core::cmp::Ordering::Greater => hi = mid,
                core::cmp::Ordering::Equal => {
                    let (start, end) = (off as usize, off as usize + len as usize);
                    // Parse proved the name lies on character boundaries of
                    // valid UTF-8, so this check of one name cannot fail.
                    return core::str::from_utf8(self.strings.of(all).get(start..end)?).ok();
                }
            }
        }
        None
    }
}

/// Check one record table: keys strictly ascending, every name slice
/// in-bounds, non-empty, length-bounded, and on character boundaries.
fn validate_table(table: &[u8], strings: &str) -> Result<(), TableError> {
    let count = table.len() / RECORD_LEN;
    let mut previous: Option<u32> = None;
    for i in 0..count {
        let (key, off, len) = record(table, i);
        if previous.is_some_and(|p| p >= key) {
            return Err(TableError::UnsortedRecords);
        }
        previous = Some(key);
        let (start, end) = (off as usize, (off as usize).wrapping_add(len as usize));
        if len == 0 || len as usize > MAX_NAME_BYTES || strings.get(start..end).is_none() {
            return Err(TableError::BadNameSlice);
        }
    }
    Ok(())
}
