//! Acorn / RISC OS (ADFS, `FileCore`) preset conversions.
//!
//! A RISC OS object is typed by the top of its 32-bit load address: when the
//! high 12 bits are `&FFF`, bits `19..8` hold a 12-bit filetype (e.g. `&FFF` =
//! Text) and the remaining 40 bits (`load & 0xFF` as the high byte, then the
//! whole exec word) hold a centisecond timestamp since 1900. An untyped object
//! instead uses load/exec as literal load and execution addresses.
//!
//! The registry stores the decoded `acorn.filetype` *and* preserves the raw
//! load/exec words (`acorn.loadaddr` / `acorn.execaddr`), so a copy back to
//! ADFS reproduces the native fields byte-for-byte.

use tairix_abi::driver::filesystem::StampField;
use tairix_abi::time::Time64;
use tairix_abi::DriverError;

use crate::MetadataError;

/// Seconds between the RISC OS epoch (1900-01-01) and the Unix epoch
/// (1970-01-01). RISC OS counts centiseconds from 1900.
const RISC_OS_EPOCH_TO_UNIX_SECS: i64 = 2_208_988_800;

/// Centiseconds in one second.
const CENTIS_PER_SEC: u64 = 100;

/// Nanoseconds in one centisecond.
const NANOS_PER_CENTI: u32 = 10_000_000;

/// The RISC OS timestamp is a 40-bit (5-byte) centisecond count.
const CENTIS_LIMIT: u64 = 1 << 40;

/// What a RISC OS stamp holds: every centisecond its 40-bit count reaches
/// from 1900 but the zero, which RISC OS reads as a clock never set.
pub const STAMP_FIELD: StampField = StampField::new(
    Time64::from_secs(-RISC_OS_EPOCH_TO_UNIX_SECS),
    NANOS_PER_CENTI as u64,
    1..CENTIS_LIMIT as u128,
);

/// High 12 bits of a load address that mark a filetyped object.
const TYPED_MARKER: u32 = 0xFFF;

pub use tairix_abi::fs::Filetype;

/// The bytes RISC OS keeps out of a name: its path separator and the
/// characters its filing systems give a meaning of their own — the root `$`,
/// the user root `&`, the library `%`, the current `@` and previous `\`
/// directories, the parent `^`, the filing-system and path-variable `:`, the
/// wildcards `#` and `*`, and the quote and bar the command line reads.
pub const RESERVED_NAME_BYTES: &[u8] = b"$&%@\\^:.#*\"|";

/// The byte a name holds in place of `byte` once it has crossed between RISC
/// OS and TAIRiX. RISC OS separates a path with `.` and writes a dot within a
/// name as `/`, the reverse of TAIRiX, so the two trade places — `ReadMe/txt`
/// there is `ReadMe.txt` here — and every other byte crosses as itself. The
/// exchange is its own inverse, so it carries a name either way.
#[must_use]
pub const fn crossed_name_byte(byte: u8) -> u8 {
    match byte {
        b'.' => b'/',
        b'/' => b'.',
        other => other,
    }
}

/// The characters RISC OS's Latin-1 gives the bytes `0x80..=0x9F`, which ISO
/// 8859-1 leaves to control codes; `None` where it gives none.
const LATIN1_EXTRAS: [Option<char>; 32] = [
    Some('\u{20AC}'),
    Some('\u{0174}'),
    Some('\u{0175}'),
    None,
    None,
    Some('\u{0176}'),
    Some('\u{0177}'),
    None,
    None,
    None,
    None,
    None,
    Some('\u{2026}'),
    Some('\u{2122}'),
    Some('\u{2030}'),
    Some('\u{2022}'),
    Some('\u{2018}'),
    Some('\u{2019}'),
    Some('\u{2039}'),
    Some('\u{203A}'),
    Some('\u{201C}'),
    Some('\u{201D}'),
    Some('\u{201E}'),
    Some('\u{2013}'),
    Some('\u{2014}'),
    Some('\u{2212}'),
    Some('\u{0152}'),
    Some('\u{0153}'),
    Some('\u{2020}'),
    Some('\u{2021}'),
    Some('\u{FB01}'),
    Some('\u{FB02}'),
];

/// The character `byte` spells in RISC OS's Latin-1 — ISO 8859-1 with
/// characters of its own in `0x80..=0x9F`, the alphabet of RISC OS's names
/// and text alike — or `None` for a byte no name or text holds: a control
/// code, or one of those it leaves undefined.
#[must_use]
pub fn latin1_char(byte: u8) -> Option<char> {
    match byte {
        0..=0x1F | 0x7F => None,
        0x80..=0x9F => LATIN1_EXTRAS[usize::from(byte - 0x80)],
        _ => Some(char::from(byte)),
    }
}

/// The byte RISC OS's Latin-1 spells `ch` as, or `None` for a character it
/// has none for.
#[must_use]
pub fn latin1_byte(ch: char) -> Option<u8> {
    match u8::try_from(u32::from(ch)) {
        Ok(byte @ (0x20..=0x7E | 0xA0..=0xFF)) => Some(byte),
        _ => LATIN1_EXTRAS
            .iter()
            .position(|&extra| extra == Some(ch))
            .and_then(|at| u8::try_from(0x80 + at).ok()),
    }
}

/// A decoded RISC OS load/exec pair.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum LoadExec {
    /// A filetyped object: a 12-bit filetype and a 40-bit centisecond stamp.
    Typed {
        /// The filetype.
        filetype: Filetype,
        /// The 40-bit centisecond timestamp since 1900.
        centiseconds: u64,
    },
    /// An untyped object with literal load and execution addresses.
    Untyped {
        /// The literal 32-bit load address.
        load: u32,
        /// The literal 32-bit execution address.
        exec: u32,
    },
}

/// Decode a raw load/exec pair into a filetyped or untyped object.
#[must_use]
pub fn decode_load_exec(load: u32, exec: u32) -> LoadExec {
    if load >> 20 == TYPED_MARKER {
        let [low, high, ..] = (load >> 8).to_le_bytes();
        let filetype = Filetype::from_low_bits(u16::from_le_bytes([low, high]));
        let centiseconds = (u64::from(load & 0xFF) << 32) | u64::from(exec);
        LoadExec::Typed {
            filetype,
            centiseconds,
        }
    } else {
        LoadExec::Untyped { load, exec }
    }
}

/// Encode a filetype and a 40-bit centisecond stamp back into the raw
/// load/exec words.
///
/// # Errors
///
/// [`MetadataError::NotRepresentable`] if `centiseconds` exceeds 40 bits.
pub fn encode_typed(filetype: Filetype, centiseconds: u64) -> Result<(u32, u32), MetadataError> {
    if centiseconds >= CENTIS_LIMIT {
        return Err(MetadataError::NotRepresentable);
    }
    let high_byte = u32::try_from(centiseconds >> 32).unwrap_or(0) & 0xFF;
    let load = (TYPED_MARKER << 20) | (u32::from(filetype.get()) << 8) | high_byte;
    let exec = u32::try_from(centiseconds & 0xFFFF_FFFF).unwrap_or(0);
    Ok((load, exec))
}

/// Encode a filetype as three lowercase hex digits (e.g. `&FFF` → `b"fff"`),
/// the canonical `acorn.filetype` value.
#[must_use]
pub fn filetype_to_value(filetype: Filetype) -> [u8; 3] {
    let raw = filetype.get();
    [
        hex_digit(u8::try_from((raw >> 8) & 0xF).unwrap_or(0)),
        hex_digit(u8::try_from((raw >> 4) & 0xF).unwrap_or(0)),
        hex_digit(u8::try_from(raw & 0xF).unwrap_or(0)),
    ]
}

/// Parse an `acorn.filetype` value (three hex digits, upper or lower case)
/// back into a filetype.
///
/// # Errors
///
/// [`MetadataError::NotRepresentable`] if the value is not exactly three hex
/// digits.
pub fn filetype_from_value(value: &[u8]) -> Result<Filetype, MetadataError> {
    if value.len() != 3 {
        return Err(MetadataError::NotRepresentable);
    }
    let mut out: u16 = 0;
    for &byte in value {
        let nibble = hex_value(byte).ok_or(MetadataError::NotRepresentable)?;
        out = (out << 4) | u16::from(nibble);
    }
    Filetype::new(out).ok_or(MetadataError::NotRepresentable)
}

/// Encode a 32-bit load or exec address as eight lowercase hex digits, the
/// canonical `acorn.loadaddr` / `acorn.execaddr` value.
#[must_use]
pub fn addr_to_value(addr: u32) -> [u8; 8] {
    let mut out = [0u8; 8];
    for (i, slot) in out.iter_mut().enumerate() {
        let shift = 28 - 4 * u32::try_from(i).unwrap_or(0);
        *slot = hex_digit(u8::try_from((addr >> shift) & 0xF).unwrap_or(0));
    }
    out
}

/// Parse an `acorn.loadaddr` / `acorn.execaddr` value (eight hex digits, upper
/// or lower case) back into a 32-bit address.
///
/// # Errors
///
/// [`MetadataError::NotRepresentable`] if the value is not exactly eight hex
/// digits.
pub fn addr_from_value(value: &[u8]) -> Result<u32, MetadataError> {
    if value.len() != 8 {
        return Err(MetadataError::NotRepresentable);
    }
    let mut out: u32 = 0;
    for &byte in value {
        let nibble = hex_value(byte).ok_or(MetadataError::NotRepresentable)?;
        out = (out << 4) | u32::from(nibble);
    }
    Ok(out)
}

/// Convert a RISC OS 40-bit centisecond timestamp to a [`Time64`].
///
/// # Errors
///
/// [`MetadataError::TimestampOutOfRange`] if `centiseconds` exceeds 40 bits or
/// the resulting instant is not representable.
pub fn centiseconds_to_time64(centiseconds: u64) -> Result<Time64, MetadataError> {
    if centiseconds >= CENTIS_LIMIT {
        return Err(MetadataError::TimestampOutOfRange);
    }
    let secs_since_1900 = i64::try_from(centiseconds / CENTIS_PER_SEC)
        .map_err(|_| MetadataError::TimestampOutOfRange)?;
    let unix_secs = secs_since_1900 - RISC_OS_EPOCH_TO_UNIX_SECS;
    let rem_centis = u32::try_from(centiseconds % CENTIS_PER_SEC).unwrap_or(0);
    Time64::new(unix_secs, rem_centis * NANOS_PER_CENTI)
        .map_err(|_| MetadataError::TimestampOutOfRange)
}

/// Convert a [`Time64`] to a RISC OS 40-bit centisecond timestamp, checked.
///
/// # Errors
///
/// [`MetadataError::TimestampOutOfRange`] if the instant is before 1900, is
/// beyond the 40-bit centisecond range, or carries sub-centisecond precision
/// (which RISC OS cannot represent and this never silently drops).
pub fn time64_to_centiseconds(time: Time64) -> Result<u64, MetadataError> {
    if !time.subsec_nanos().is_multiple_of(NANOS_PER_CENTI) {
        return Err(MetadataError::TimestampOutOfRange);
    }
    let since_1900 = time
        .secs()
        .checked_add(RISC_OS_EPOCH_TO_UNIX_SECS)
        .ok_or(MetadataError::TimestampOutOfRange)?;
    if since_1900 < 0 {
        return Err(MetadataError::TimestampOutOfRange);
    }
    let sub_centis = u64::from(time.subsec_nanos() / NANOS_PER_CENTI);
    let centiseconds = u64::try_from(since_1900)
        .map_err(|_| MetadataError::TimestampOutOfRange)?
        .checked_mul(CENTIS_PER_SEC)
        .and_then(|c| c.checked_add(sub_centis))
        .ok_or(MetadataError::TimestampOutOfRange)?;
    if centiseconds >= CENTIS_LIMIT {
        return Err(MetadataError::TimestampOutOfRange);
    }
    Ok(centiseconds)
}

/// Canonical attribute bit: owner read (`R`).
pub const ATTR_OWNER_READ: u16 = 1 << 0;
/// Canonical attribute bit: owner write (`W`).
pub const ATTR_OWNER_WRITE: u16 = 1 << 1;
/// Canonical attribute bit: locked against deletion (`L`).
pub const ATTR_LOCKED: u16 = 1 << 2;
/// Canonical attribute bit: the object is a directory (`D`).
pub const ATTR_DIRECTORY: u16 = 1 << 3;
/// Canonical attribute bit: owner execute-only (`E`).
pub const ATTR_EXECUTE_ONLY: u16 = 1 << 4;
/// Canonical attribute bit: public read (`r`).
pub const ATTR_PUBLIC_READ: u16 = 1 << 5;
/// Canonical attribute bit: public write (`w`).
pub const ATTR_PUBLIC_WRITE: u16 = 1 << 6;
/// Canonical attribute bit: public execute (`e`, 8-bit ADFS only).
pub const ATTR_PUBLIC_EXECUTE: u16 = 1 << 7;
/// Canonical attribute bit: private (`P`, 8-bit ADFS only).
pub const ATTR_PRIVATE: u16 = 1 << 8;

/// Every canonical attribute bit: the 8-bit ADFS access bits, in their
/// order there, which every later Acorn layout is a subset of.
pub const ATTR_BITS: u16 = 0x01FF;

/// The canonical bits `FileCore`'s on-disc attribute byte can hold.
pub const FILECORE_ATTR_BITS: u16 = ATTR_OWNER_READ
    | ATTR_OWNER_WRITE
    | ATTR_LOCKED
    | ATTR_DIRECTORY
    | ATTR_PUBLIC_READ
    | ATTR_PUBLIC_WRITE;

/// `FileCore`'s on-disc attribute byte (`NewDirAtts`, RISC OS PRM vol. 2
/// ch. 28) beside the canonical bit each holds: owner read, owner write,
/// locked, directory, public read, public write. Bits 6 and 7 are reserved.
const FILECORE_BITS: [(u8, u16); 6] = [
    (1 << 0, ATTR_OWNER_READ),
    (1 << 1, ATTR_OWNER_WRITE),
    (1 << 2, ATTR_LOCKED),
    (1 << 3, ATTR_DIRECTORY),
    (1 << 4, ATTR_PUBLIC_READ),
    (1 << 5, ATTR_PUBLIC_WRITE),
];

/// A `FileSwitch` attribute byte (`OS_File`, RISC OS PRM vol. 2 ch. 27)
/// beside the canonical bit each holds: owner read, owner write, owner
/// execute-only, locked, public read, public write. Bits 6 and 7 are
/// reserved; `FileSwitch` has no directory bit, a directory being an
/// object's kind.
const FILESWITCH_BITS: [(u8, u16); 6] = [
    (1 << 0, ATTR_OWNER_READ),
    (1 << 1, ATTR_OWNER_WRITE),
    (1 << 2, ATTR_EXECUTE_ONLY),
    (1 << 3, ATTR_LOCKED),
    (1 << 4, ATTR_PUBLIC_READ),
    (1 << 5, ATTR_PUBLIC_WRITE),
];

/// The canonical bits `FileCore`'s on-disc attribute `byte` records; its
/// reserved bits carry nothing.
#[must_use]
pub fn attr_from_filecore(byte: u8) -> u16 {
    map_bits(u16::from(byte), &FILECORE_BITS, false)
}

/// `FileCore`'s on-disc attribute byte for canonical bits `attr`, of which
/// only [`FILECORE_ATTR_BITS`] have a place; a caller that may not drop the
/// others checks `attr` against that mask first.
#[must_use]
pub fn attr_to_filecore(attr: u16) -> u8 {
    map_bits(attr, &FILECORE_BITS, true).to_le_bytes()[0]
}

/// The canonical bits a `FileSwitch` attribute `byte` records; its reserved
/// bits carry nothing.
#[must_use]
pub fn attr_from_fileswitch(byte: u8) -> u16 {
    map_bits(u16::from(byte), &FILESWITCH_BITS, false)
}

/// The canonical bits a `FileSwitch` attribute byte can hold.
pub const FILESWITCH_ATTR_BITS: u16 = ATTR_OWNER_READ
    | ATTR_OWNER_WRITE
    | ATTR_EXECUTE_ONLY
    | ATTR_LOCKED
    | ATTR_PUBLIC_READ
    | ATTR_PUBLIC_WRITE;

/// The `FileSwitch` attribute byte for canonical bits `attr`, of which only
/// [`FILESWITCH_ATTR_BITS`] have a place; a caller that may not drop the
/// others checks `attr` against that mask first.
#[must_use]
pub fn attr_to_fileswitch(attr: u16) -> u8 {
    map_bits(attr, &FILESWITCH_BITS, true).to_le_bytes()[0]
}

/// Translate `bits` through `table`, from its native side to the canonical
/// one, or back when `to_native`.
fn map_bits(bits: u16, table: &[(u8, u16)], to_native: bool) -> u16 {
    table.iter().fold(0, |out, &(native, canonical)| {
        let (from, to) = if to_native {
            (canonical, u16::from(native))
        } else {
            (u16::from(native), canonical)
        };
        if bits & from != 0 {
            out | to
        } else {
            out
        }
    })
}

/// Owner-part attribute letters and their bit positions.
const ATTR_OWNER: [(u8, u16); 6] = [
    (b'R', ATTR_OWNER_READ),
    (b'W', ATTR_OWNER_WRITE),
    (b'L', ATTR_LOCKED),
    (b'D', ATTR_DIRECTORY),
    (b'E', ATTR_EXECUTE_ONLY),
    (b'P', ATTR_PRIVATE),
];

/// Public-part attribute letters and their bit positions.
const ATTR_PUBLIC: [(u8, u16); 3] = [
    (b'r', ATTR_PUBLIC_READ),
    (b'w', ATTR_PUBLIC_WRITE),
    (b'e', ATTR_PUBLIC_EXECUTE),
];

/// Longest canonical `acorn.attr` value (`RWLDEP/rwe`).
pub const ATTR_VALUE_MAX: usize = 10;

/// Encode `FileCore` attribute bits as the canonical `acorn.attr` value:
/// the owner letters in `RWLDEP` order, a `/`, then the public letters
/// in `rwe` order (a locked, publicly readable directory encodes as
/// `b"RLD/r"`).
///
/// # Errors
///
/// [`MetadataError::NotRepresentable`] if `attr` carries bits outside
/// [`ATTR_BITS`].
pub fn attr_to_value(attr: u16) -> Result<([u8; ATTR_VALUE_MAX], usize), MetadataError> {
    if attr & !ATTR_BITS != 0 {
        return Err(MetadataError::NotRepresentable);
    }
    let mut out = [0u8; ATTR_VALUE_MAX];
    let mut len = 0;
    for (letter, bit) in ATTR_OWNER {
        if attr & bit != 0 {
            out[len] = letter;
            len += 1;
        }
    }
    out[len] = b'/';
    len += 1;
    for (letter, bit) in ATTR_PUBLIC {
        if attr & bit != 0 {
            out[len] = letter;
            len += 1;
        }
    }
    Ok((out, len))
}

/// Parse an `acorn.attr` value back into `FileCore` attribute bits. The
/// letters may appear in any order around the single `/`; duplicate or
/// unknown letters are rejected.
///
/// # Errors
///
/// [`MetadataError::NotRepresentable`] on any malformed value.
pub fn attr_from_value(value: &[u8]) -> Result<u16, MetadataError> {
    let slash = value
        .iter()
        .position(|&b| b == b'/')
        .ok_or(MetadataError::NotRepresentable)?;
    let (owner, public) = (&value[..slash], &value[slash + 1..]);
    if public.contains(&b'/') {
        return Err(MetadataError::NotRepresentable);
    }
    let mut attr = 0u16;
    for &letter in owner {
        let (_, bit) = ATTR_OWNER
            .iter()
            .find(|(l, _)| *l == letter)
            .ok_or(MetadataError::NotRepresentable)?;
        if attr & bit != 0 {
            return Err(MetadataError::NotRepresentable);
        }
        attr |= bit;
    }
    for &letter in public {
        let (_, bit) = ATTR_PUBLIC
            .iter()
            .find(|(l, _)| *l == letter)
            .ok_or(MetadataError::NotRepresentable)?;
        if attr & bit != 0 {
            return Err(MetadataError::NotRepresentable);
        }
        attr |= bit;
    }
    Ok(attr)
}

/// The key of an object's raw load word.
pub const LOADADDR_KEY: &[u8] = b"acorn.loadaddr";
/// The key of an object's raw execution word.
pub const EXECADDR_KEY: &[u8] = b"acorn.execaddr";
/// The key of an object's canonical attribute bits.
pub const ATTR_KEY: &[u8] = b"acorn.attr";
/// The key of a typed object's filetype.
pub const FILETYPE_KEY: &[u8] = b"acorn.filetype";
/// The key of a typed object's centisecond datestamp.
pub const DATESTAMP_KEY: &[u8] = b"acorn.datestamp";

/// The `acorn` keys an object's fields are surfaced as, in listing order.
pub const KEYS: [&[u8]; 5] = [
    LOADADDR_KEY,
    EXECADDR_KEY,
    ATTR_KEY,
    FILETYPE_KEY,
    DATESTAMP_KEY,
];

/// What RISC OS records of one object: its load and execution words and its
/// canonical attribute bits. The one reading every format that keeps these
/// fields surfaces them through, so ADFS and a RISC OS CD present an object
/// identically.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Object {
    /// The raw load word.
    pub load: u32,
    /// The raw execution word.
    pub exec: u32,
    /// The canonical attribute bits ([`ATTR_BITS`]).
    pub attr: u16,
}

impl Object {
    fn typed(&self) -> bool {
        matches!(
            decode_load_exec(self.load, self.exec),
            LoadExec::Typed { .. }
        )
    }

    /// Whether the object carries `key`: the addresses and the attributes
    /// always, the filetype and datestamp only when its load word types it.
    #[must_use]
    pub fn carries(&self, key: &[u8]) -> bool {
        match key {
            LOADADDR_KEY | EXECADDR_KEY | ATTR_KEY => true,
            FILETYPE_KEY | DATESTAMP_KEY => self.typed(),
            _ => false,
        }
    }

    /// The `index`-th key the object carries, in [`KEYS`] order.
    #[must_use]
    pub fn key(&self, index: u64) -> Option<&'static [u8]> {
        KEYS.into_iter()
            .filter(|key| self.carries(key))
            .nth(usize::try_from(index).ok()?)
    }

    /// The canonical value of `key`, written into `out`, or `None` when the
    /// object does not carry it.
    ///
    /// # Errors
    ///
    /// [`MetadataError::NotRepresentable`] when the attribute bits carry a bit
    /// outside [`ATTR_BITS`].
    pub fn value(
        &self,
        key: &[u8],
        out: &mut [u8; ATTR_VALUE_MAX],
    ) -> Result<Option<usize>, MetadataError> {
        let decoded = decode_load_exec(self.load, self.exec);
        let len = match (key, decoded) {
            (LOADADDR_KEY, _) => put(out, &addr_to_value(self.load)),
            (EXECADDR_KEY, _) => put(out, &addr_to_value(self.exec)),
            (ATTR_KEY, _) => {
                let (encoded, len) = attr_to_value(self.attr)?;
                put(out, &encoded[..len])
            }
            (FILETYPE_KEY, LoadExec::Typed { filetype, .. }) => {
                put(out, &filetype_to_value(filetype))
            }
            (DATESTAMP_KEY, LoadExec::Typed { centiseconds, .. }) => {
                put(out, &datestamp_to_value(centiseconds)?)
            }
            _ => return Ok(None),
        };
        Ok(Some(len))
    }
}

impl Object {
    /// Set the field `key` names to its canonical `value`, as RISC OS would:
    /// a filetype keeps the stamp it had, a stamp needs the filetype it lives
    /// inside, and the attribute bits keep the directory bit the object's
    /// kind is and stay within `storable`, the bits its format can hold.
    ///
    /// # Errors
    ///
    /// [`DriverError::OutOfRange`] for a value out of shape or bits the
    /// format cannot store; [`DriverError::Unsupported`] for a stamp on an
    /// untyped object, or a key no RISC OS object holds.
    pub fn set(&mut self, key: &[u8], value: &[u8], storable: u16) -> Result<(), DriverError> {
        let shape = |_| DriverError::OutOfRange;
        let decoded = decode_load_exec(self.load, self.exec);
        match key {
            LOADADDR_KEY => self.load = addr_from_value(value).map_err(shape)?,
            EXECADDR_KEY => self.exec = addr_from_value(value).map_err(shape)?,
            ATTR_KEY => {
                let attr = attr_from_value(value).map_err(shape)?;
                // The directory bit is the object's kind, not a free bit.
                if attr & ATTR_DIRECTORY != self.attr & ATTR_DIRECTORY || attr & !storable != 0 {
                    return Err(DriverError::OutOfRange);
                }
                self.attr = attr;
            }
            FILETYPE_KEY => {
                let filetype = filetype_from_value(value).map_err(shape)?;
                let centiseconds = match decoded {
                    LoadExec::Typed { centiseconds, .. } => centiseconds,
                    LoadExec::Untyped { .. } => 0,
                };
                (self.load, self.exec) = encode_typed(filetype, centiseconds).map_err(shape)?;
            }
            DATESTAMP_KEY => {
                let centiseconds = datestamp_from_value(value).map_err(shape)?;
                // The stamp lives inside the filetype encoding.
                let LoadExec::Typed { filetype, .. } = decoded else {
                    return Err(DriverError::Unsupported);
                };
                (self.load, self.exec) = encode_typed(filetype, centiseconds).map_err(shape)?;
            }
            _ => return Err(DriverError::Unsupported),
        }
        Ok(())
    }

    /// Remove the field `key` names: a filetype takes the stamp inside it
    /// with it, and a stamp leaves the filetype dated zero.
    ///
    /// # Errors
    ///
    /// [`DriverError::NotFound`] for a field the object does not carry;
    /// [`DriverError::Unsupported`] for the addresses and attribute bits,
    /// which every object carries.
    pub fn remove(&mut self, key: &[u8]) -> Result<(), DriverError> {
        let decoded = decode_load_exec(self.load, self.exec);
        match (key, decoded) {
            (FILETYPE_KEY, LoadExec::Typed { .. }) => (self.load, self.exec) = (0, 0),
            (DATESTAMP_KEY, LoadExec::Typed { filetype, .. }) => {
                (self.load, self.exec) =
                    encode_typed(filetype, 0).map_err(|_| DriverError::OutOfRange)?;
            }
            (LOADADDR_KEY | EXECADDR_KEY | ATTR_KEY, _) => return Err(DriverError::Unsupported),
            _ => return Err(DriverError::NotFound),
        }
        Ok(())
    }
}

/// `value` written to the head of `out`, answering its length; every
/// canonical value fits [`ATTR_VALUE_MAX`].
fn put(out: &mut [u8; ATTR_VALUE_MAX], value: &[u8]) -> usize {
    out[..value.len()].copy_from_slice(value);
    value.len()
}

/// Encode a 40-bit centisecond datestamp as the canonical
/// `acorn.datestamp` value: ten lowercase hex digits, so the raw stamp
/// round-trips exactly.
///
/// # Errors
///
/// [`MetadataError::NotRepresentable`] if `centiseconds` exceeds 40 bits.
pub fn datestamp_to_value(centiseconds: u64) -> Result<[u8; 10], MetadataError> {
    if centiseconds >= CENTIS_LIMIT {
        return Err(MetadataError::NotRepresentable);
    }
    let mut out = [0u8; 10];
    for (i, slot) in out.iter_mut().enumerate() {
        let shift = 36 - 4 * u32::try_from(i).unwrap_or(0);
        *slot = hex_digit(u8::try_from((centiseconds >> shift) & 0xF).unwrap_or(0));
    }
    Ok(out)
}

/// Parse an `acorn.datestamp` value (ten hex digits, upper or lower
/// case) back into a 40-bit centisecond stamp.
///
/// # Errors
///
/// [`MetadataError::NotRepresentable`] if the value is not exactly ten
/// hex digits.
pub fn datestamp_from_value(value: &[u8]) -> Result<u64, MetadataError> {
    if value.len() != 10 {
        return Err(MetadataError::NotRepresentable);
    }
    let mut out: u64 = 0;
    for &byte in value {
        let nibble = hex_value(byte).ok_or(MetadataError::NotRepresentable)?;
        out = (out << 4) | u64::from(nibble);
    }
    Ok(out)
}

/// The lowercase-hex character for a nibble `0..=15`.
fn hex_digit(nibble: u8) -> u8 {
    match nibble {
        0..=9 => b'0' + nibble,
        _ => b'a' + (nibble - 10),
    }
}

/// The nibble value of a hex character (upper or lower case), or `None`.
fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
