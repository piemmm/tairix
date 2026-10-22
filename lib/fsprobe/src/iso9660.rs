//! ISO 9660 (ECMA-119) and High Sierra recognition, shared by the probe and
//! the ISO 9660 driver so the two agree on which tree of a disc is mounted
//! and which identity and label it carries.
//!
//! A disc's volume descriptor set starts at logical sector 16 of its last
//! session, one 2048-byte descriptor per sector: a primary descriptor, any
//! supplementary (Joliet) and enhanced (ISO 9660:1999) ones and boot records,
//! then a terminator. High Sierra, ECMA-119's 1986 predecessor, keeps the
//! same set in a different layout.
//!
//! References: ECMA-119 4th edition (2019) §6.7, §8, §9; High Sierra §11,
//! §13 (the working paper of 28 May 1986); ECMA-119 Annex C (Joliet); IEEE
//! P1281 (SUSP) for the system use markers.

use alloc::boxed::Box;

use tairix_abi::driver::block::{Block, DiscLayout};
use tairix_abi::DriverError;
use tairix_bytedev::ByteDevice;

use crate::{content_identity, heap_buf, read_present, session_bytes, LABEL_MAX, SYSTEM_AREA_LEN};

/// Bytes of one logical sector: the unit descriptors and directories are
/// laid out in, whatever the device's own block size.
pub const SECTOR: u64 = 2048;

/// Bytes of one logical sector, as a buffer length.
pub const SECTOR_LEN: usize = 2048;

/// The logical sector, counted from a session's start, the descriptor set
/// begins at: the first after the system area.
pub const DESCRIPTOR_SET_START: u64 = SYSTEM_AREA_LEN / SECTOR;

/// Most descriptors read before a set is given up on: a validation bound on
/// untrusted media, not a capacity. Real discs carry a handful.
pub const MAX_DESCRIPTORS: u64 = 64;

/// Length of a directory record's fixed part, before its identifier.
pub const RECORD_FIXED_LEN: usize = 33;

/// Length of the root directory record a volume descriptor holds.
pub const ROOT_RECORD_LEN: usize = 34;

/// The signature opening a RISC OS directory record's system use.
pub const ARCHIMEDES: &[u8; 10] = b"ARCHIMEDES";
/// Bytes of the RISC OS system-use record.
pub const ARCHIMEDES_LEN: usize = 32;

/// Which of ECMA-119 and its High Sierra predecessor a volume follows.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Flavour {
    /// ECMA-119 / ISO 9660.
    Iso9660,
    /// The 1986 High Sierra format.
    HighSierra,
}

impl Flavour {
    /// Offset of the file-flags byte in a directory record: High Sierra's
    /// six-byte date carries no timezone, which moves it up one.
    #[must_use]
    pub const fn flags_at(self) -> usize {
        match self {
            Self::Iso9660 => 25,
            Self::HighSierra => 24,
        }
    }
}

/// Which directory hierarchy of a disc is mounted.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Tree {
    /// The primary tree, its names d-characters.
    Primary,
    /// A Joliet supplementary tree, its names UCS-2 big-endian.
    Joliet,
    /// The ISO 9660:1999 enhanced tree, its names as the preparer agreed.
    Enhanced,
}

/// What the system use fields of the mounted tree hold.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct SystemUse {
    /// The System Use Sharing Protocol is in force, its entries starting
    /// this many bytes into each record's system use.
    pub susp_skip: Option<u8>,
    /// SUSP carries Rock Ridge entries: POSIX names, owners, modes, links
    /// and times.
    pub rock_ridge: bool,
    /// Records carry the 32-byte RISC OS `ARCHIMEDES` field: load and
    /// execution addresses, filetype and attributes.
    pub archimedes: bool,
}

/// The fixed fields of one directory record, decoded from its bytes.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Record {
    /// The record's length, its identifier and system use included.
    pub len: usize,
    /// Logical blocks of extended attribute record at the extent's head.
    pub xar_blocks: u8,
    /// First logical block of the extent.
    pub extent: u32,
    /// Bytes of data in the extent, the extended attribute record excluded.
    pub data_len: u32,
    /// The file flags.
    pub flags: u8,
    /// Logical blocks per file unit of an interleaved file, else zero.
    pub unit_blocks: u8,
    /// Logical blocks between file units of an interleaved file.
    pub gap_blocks: u8,
    /// The identifier's length; it follows the fixed part.
    pub name_len: usize,
    /// Where the system use field lies in the record.
    pub system_use_at: usize,
}

/// File flag: the existence of the file need not be made known.
pub const FLAG_HIDDEN: u8 = 1 << 0;
/// File flag: the record identifies a directory.
pub const FLAG_DIRECTORY: u8 = 1 << 1;
/// File flag: the file is an associated file (a resource fork).
pub const FLAG_ASSOCIATED: u8 = 1 << 2;
/// File flag: the extended attribute record carries a record format.
pub const FLAG_RECORD: u8 = 1 << 3;
/// File flag: owner, group and permissions are in the extended attribute
/// record.
pub const FLAG_PROTECTION: u8 = 1 << 4;
/// File flag: further records of the same file follow this one.
pub const FLAG_MULTI_EXTENT: u8 = 1 << 7;

impl Record {
    /// Decode the record opening `bytes`, `None` for one whose lengths do not
    /// fit together: an identifier or system use running past the record, a
    /// record running past `bytes`, or one shorter than its fixed part. The
    /// both-endian fields must agree, as a well-formed disc's always do.
    #[must_use]
    pub fn parse(bytes: &[u8], flavour: Flavour) -> Option<Self> {
        let len = usize::from(*bytes.first()?);
        if len <= RECORD_FIXED_LEN || len > bytes.len() {
            return None;
        }
        let record = &bytes[..len];
        let name_len = usize::from(record[32]);
        let name_end = RECORD_FIXED_LEN + name_len;
        if name_len == 0 || name_end > len {
            return None;
        }
        // A pad byte after an even-length identifier keeps the system use on
        // an even offset.
        let system_use_at = (name_end + usize::from(name_len % 2 == 0)).min(len);
        Some(Self {
            len,
            xar_blocks: record[1],
            extent: both_u32(record, 2)?,
            data_len: both_u32(record, 10)?,
            flags: record[flavour.flags_at()],
            unit_blocks: record[26],
            gap_blocks: record[27],
            name_len,
            system_use_at,
        })
    }

    /// Whether the record identifies a directory.
    #[must_use]
    pub const fn is_dir(&self) -> bool {
        self.flags & FLAG_DIRECTORY != 0
    }

    /// The record's identifier, within `record`, the bytes it was parsed
    /// from.
    #[must_use]
    pub fn name<'r>(&self, record: &'r [u8]) -> &'r [u8] {
        record
            .get(RECORD_FIXED_LEN..RECORD_FIXED_LEN + self.name_len)
            .unwrap_or_default()
    }

    /// The record's system use field, within `record`, the bytes it was
    /// parsed from.
    #[must_use]
    pub fn system_use<'r>(&self, record: &'r [u8]) -> &'r [u8] {
        record.get(self.system_use_at..self.len).unwrap_or_default()
    }

    /// Whether this is the `.` or `..` record a directory opens with.
    #[must_use]
    pub fn is_self_or_parent(&self, record: &[u8]) -> bool {
        matches!(self.name(record), [0 | 1])
    }
}

/// One volume descriptor of a set, kept whole for the driver's reading.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Descriptor {
    /// The descriptor's logical sector, from the device's start.
    pub sector: u64,
    bytes: Box<[u8; SECTOR_LEN]>,
}

impl Descriptor {
    /// The descriptor's bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8; SECTOR_LEN] {
        &self.bytes
    }

    /// The root directory record's bytes.
    #[must_use]
    pub fn root_bytes(&self, flavour: Flavour) -> &[u8] {
        let at = match flavour {
            Flavour::Iso9660 => 156,
            Flavour::HighSierra => 180,
        };
        &self.bytes[at..at + ROOT_RECORD_LEN]
    }

    /// The root directory record, decoded.
    ///
    /// # Errors
    ///
    /// [`DriverError::BadMagic`] for one that does not decode as a directory.
    pub fn root(&self, flavour: Flavour) -> Result<Record, DriverError> {
        Record::parse(self.root_bytes(flavour), flavour)
            .filter(Record::is_dir)
            .ok_or(DriverError::BadMagic)
    }

    /// The logical block size the descriptor records.
    #[must_use]
    pub fn block_size(&self, flavour: Flavour) -> Option<u32> {
        let at = match flavour {
            Flavour::Iso9660 => 128,
            Flavour::HighSierra => 136,
        };
        both_u16(&self.bytes[..], at).map(u32::from)
    }

    /// The volume's size, in logical blocks, the descriptor records.
    #[must_use]
    pub fn volume_blocks(&self, flavour: Flavour) -> Option<u32> {
        let at = match flavour {
            Flavour::Iso9660 => 80,
            Flavour::HighSierra => 88,
        };
        both_u32(&self.bytes[..], at)
    }

    /// The raw volume identifier field.
    #[must_use]
    pub fn volume_id(&self, flavour: Flavour) -> &[u8] {
        match flavour {
            Flavour::Iso9660 => &self.bytes[40..72],
            Flavour::HighSierra => &self.bytes[48..80],
        }
    }
}

/// The Joliet level a supplementary descriptor's escape sequences name,
/// `None` for a descriptor that is not Joliet.
fn joliet_level(descriptor: &[u8; SECTOR_LEN]) -> Option<u8> {
    let escapes = &descriptor[88..120];
    let level = match &escapes[..3] {
        b"%/@" => 1,
        b"%/C" => 2,
        b"%/E" => 3,
        _ => return None,
    };
    escapes[3..].iter().all(|&b| b == 0).then_some(level)
}

/// The descriptor in `scratch`, kept, with a fresh buffer left in its place.
fn keep(scratch: &mut Box<[u8; SECTOR_LEN]>, sector: u64) -> Result<Descriptor, DriverError> {
    Ok(Descriptor {
        sector,
        bytes: core::mem::replace(scratch, heap_buf()?),
    })
}

/// A recognised ISO 9660 or High Sierra volume: the descriptors that
/// describe it and the tree mounted from them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Volume {
    /// ECMA-119 or High Sierra.
    pub flavour: Flavour,
    /// Bytes per logical block.
    pub block_size: u32,
    /// The volume's size in logical blocks.
    pub volume_blocks: u32,
    /// The primary descriptor, which every volume has.
    pub primary: Descriptor,
    /// The supplementary or enhanced descriptor of the tree mounted, `None`
    /// when the primary tree is.
    supplementary: Option<Descriptor>,
    /// Which tree is mounted.
    pub tree: Tree,
    /// The Joliet level of a Joliet tree; zero otherwise.
    pub joliet_level: u8,
    /// What the mounted tree's system use fields hold.
    pub system_use: SystemUse,
}

impl Volume {
    /// The descriptor of the tree mounted.
    #[must_use]
    pub fn mounted(&self) -> &Descriptor {
        self.supplementary.as_ref().unwrap_or(&self.primary)
    }

    /// The volume's stable identity: a digest of its primary descriptor,
    /// which a disc is mastered with and only a new session rewrites.
    #[must_use]
    pub fn identity(&self) -> [u8; 16] {
        let domain: &[u8] = match self.flavour {
            Flavour::Iso9660 => b"tairix.iso9660",
            Flavour::HighSierra => b"tairix.highsierra",
        };
        content_identity(domain, &[&self.primary.bytes[..]])
    }

    /// The mounted tree's volume label as UTF-8, trimmed of padding: Joliet
    /// records it in UCS-2, the other trees in single bytes.
    #[must_use]
    pub fn label(&self) -> ([u8; LABEL_MAX], usize) {
        let field = self.mounted().volume_id(self.flavour);
        let mut out = [0u8; LABEL_MAX];
        let len = match self.tree {
            Tree::Joliet => ucs2_label(field, &mut out),
            Tree::Primary | Tree::Enhanced => {
                let len = field
                    .iter()
                    .rposition(|&b| b != b' ' && b != 0)
                    .map_or(0, |last| last + 1);
                out[..len].copy_from_slice(&field[..len]);
                len
            }
        };
        (out, len)
    }

    /// The byte offset of logical block `block`.
    #[must_use]
    pub fn block_offset(&self, block: u32) -> u64 {
        u64::from(block) * u64::from(self.block_size)
    }

    /// The bytes the volume spans from the device's start.
    #[must_use]
    pub fn span_bytes(&self) -> u64 {
        self.block_offset(self.volume_blocks)
    }
}

/// Recognise an ISO 9660 or High Sierra volume on `dev` from the descriptor
/// set of the session `layout` names last (the disc's start without one),
/// and choose the tree it is mounted from: the primary tree when it carries
/// Rock Ridge or RISC OS records, else the highest-level Joliet tree, else
/// the enhanced tree, else the primary tree. `None` when the set holds no
/// valid primary descriptor — never a guess.
///
/// # Errors
///
/// A device fault, or a refused allocation.
pub fn identify<B: Block>(
    dev: &mut ByteDevice<B>,
    layout: Option<DiscLayout>,
) -> Result<Option<Volume>, DriverError> {
    let Some(session) = session_bytes(dev, layout, SECTOR) else {
        return Ok(None);
    };
    let first = session / SECTOR + DESCRIPTOR_SET_START;
    let mut primary: Option<Descriptor> = None;
    let mut joliet: Option<(u8, Descriptor)> = None;
    let mut enhanced: Option<Descriptor> = None;
    let mut flavour = None;
    let mut scratch = heap_buf::<SECTOR_LEN>()?;
    for sector in first..first.saturating_add(MAX_DESCRIPTORS) {
        let Some(at) = sector.checked_mul(SECTOR) else {
            break;
        };
        if !read_present(dev, at, &mut scratch[..])? {
            break;
        }
        let (kind, version, this) = if &scratch[1..6] == b"CD001" {
            (scratch[0], scratch[6], Flavour::Iso9660)
        } else if &scratch[9..14] == b"CDROM" {
            (scratch[8], scratch[14], Flavour::HighSierra)
        } else {
            break;
        };
        // A set that changes format part-way is not a set.
        if *flavour.get_or_insert(this) != this {
            return Ok(None);
        }
        if kind == 255 {
            break;
        }
        match (kind, version, this) {
            (1, 1, _) if primary.is_none() => primary = Some(keep(&mut scratch, sector)?),
            (2, 1, Flavour::Iso9660) => {
                let better = joliet_level(&scratch)
                    .filter(|&level| joliet.as_ref().is_none_or(|(best, _)| level > *best));
                if let Some(level) = better {
                    joliet = Some((level, keep(&mut scratch, sector)?));
                }
            }
            (2, 2, Flavour::Iso9660) if enhanced.is_none() => {
                enhanced = Some(keep(&mut scratch, sector)?);
            }
            _ => {}
        }
    }
    let (Some(flavour), Some(primary)) = (flavour, primary) else {
        return Ok(None);
    };
    let (Some(block_size), Some(volume_blocks)) =
        (primary.block_size(flavour), primary.volume_blocks(flavour))
    else {
        return Ok(None);
    };
    // A logical block is 2^(n+9) bytes and no larger than a sector.
    if !block_size.is_power_of_two() || !(512..=2048).contains(&block_size) || volume_blocks == 0 {
        return Ok(None);
    }
    let Ok(root) = primary.root(flavour) else {
        return Ok(None);
    };
    let Some(primary_use) = root_system_use(dev, flavour, block_size, &root, &mut scratch)? else {
        return Ok(None);
    };
    let mut volume = Volume {
        flavour,
        block_size,
        volume_blocks,
        primary,
        supplementary: None,
        tree: Tree::Primary,
        joliet_level: 0,
        system_use: primary_use,
    };
    if primary_use.rock_ridge || primary_use.archimedes || flavour == Flavour::HighSierra {
        return Ok(Some(volume));
    }
    let candidates = [
        joliet.map(|(level, descriptor)| (Tree::Joliet, level, descriptor)),
        enhanced.map(|descriptor| (Tree::Enhanced, 0, descriptor)),
    ];
    for (tree, level, descriptor) in candidates.into_iter().flatten() {
        let Ok(root) = descriptor.root(flavour) else {
            continue;
        };
        if let Some(system_use) = root_system_use(dev, flavour, block_size, &root, &mut scratch)? {
            volume.supplementary = Some(descriptor);
            volume.tree = tree;
            volume.joliet_level = level;
            volume.system_use = system_use;
            break;
        }
    }
    Ok(Some(volume))
}

/// What a tree's system use fields hold, read from the first sector of its
/// root directory: SUSP and Rock Ridge from the `.` record, and RISC OS
/// records from any record there. `None` for a root whose first sector holds
/// no well-formed `.` record.
fn root_system_use<B: Block>(
    dev: &mut ByteDevice<B>,
    flavour: Flavour,
    block_size: u32,
    root: &Record,
    sector: &mut [u8; SECTOR_LEN],
) -> Result<Option<SystemUse>, DriverError> {
    let block = u64::from(root.extent) + u64::from(root.xar_blocks);
    if !read_present(dev, block * u64::from(block_size), &mut sector[..])? {
        return Ok(None);
    }
    let Some(dot) = Record::parse(&sector[..], flavour) else {
        return Ok(None);
    };
    if dot.name(&sector[..]) != [0] {
        return Ok(None);
    }
    let mut found = SystemUse::default();
    if flavour == Flavour::Iso9660 {
        let dot_use = dot.system_use(&sector[..]);
        // SUSP's indicator opens the root's first record: `SP`, length 7,
        // version 1, the check bytes `BE EF`, then the skip length.
        if let [b'S', b'P', 7, 1, 0xBE, 0xEF, skip, entries @ ..] = dot_use {
            found.susp_skip = Some(*skip);
            found.rock_ridge = names_rock_ridge(entries);
        }
    }
    let skip = found.susp_skip.map_or(0, usize::from);
    let mut pos = 0;
    while let Some(record) = sector
        .get(pos..)
        .and_then(|rest| Record::parse(rest, flavour))
    {
        let bytes = &sector[pos..pos + record.len];
        // RISC OS's field precedes SUSP's entries, in the bytes SUSP is told
        // to skip.
        let field = record.system_use(bytes);
        if field.starts_with(ARCHIMEDES)
            && field.len() >= ARCHIMEDES_LEN
            && (found.susp_skip.is_none() || skip >= ARCHIMEDES_LEN)
        {
            found.archimedes = true;
            break;
        }
        pos += record.len;
    }
    Ok(Some(found))
}

/// Whether the SUSP entries in `entries` (those after the `SP` indicator in
/// the root's `.` record) declare Rock Ridge: an `ER` naming an RRIP
/// edition, or the RRIP entries themselves. A disc that defers its `ER` to a
/// continuation area still records `PX` here, as every Rock Ridge record
/// does.
fn names_rock_ridge(entries: &[u8]) -> bool {
    let mut pos = 0;
    while let Some(header) = entries.get(pos..pos + 4) {
        let len = usize::from(header[2]);
        let Some(entry) = entries.get(pos..pos + len).filter(|_| len >= 4) else {
            return false;
        };
        match &entry[..2] {
            b"ER" => {
                let id_len = usize::from(entry.get(4).copied().unwrap_or(0));
                if let Some(id) = entry.get(8..8 + id_len) {
                    if matches!(id, b"RRIP_1991A" | b"IEEE_P1282" | b"IEEE_1282") {
                        return true;
                    }
                }
            }
            b"PX" | b"RR" | b"NM" | b"TF" | b"SL" => return true,
            b"ST" => return false,
            _ => {}
        }
        pos += len;
    }
    false
}

/// Decode a Joliet UCS-2 big-endian label into UTF-8, trimmed of padding. A
/// code unit that is not a valid scalar value (an unpaired surrogate) ends
/// the label rather than being guessed at.
fn ucs2_label(field: &[u8], out: &mut [u8; LABEL_MAX]) -> usize {
    let units = field
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&pair| u16::from_be_bytes(pair));
    let mut len = 0;
    let mut end = 0;
    for decoded in char::decode_utf16(units) {
        let Ok(ch) = decoded else {
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

/// A both-endian (ECMA-119 7.3.3) `u32` at `at`, `None` past the end or when
/// its halves disagree.
#[must_use]
pub fn both_u32(bytes: &[u8], at: usize) -> Option<u32> {
    let raw = bytes.get(at..at.checked_add(8)?)?;
    let le = u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
    let be = u32::from_be_bytes([raw[4], raw[5], raw[6], raw[7]]);
    (le == be).then_some(le)
}

/// A both-endian (ECMA-119 7.2.3) `u16` at `at`, `None` past the end or when
/// its halves disagree.
#[must_use]
pub fn both_u16(bytes: &[u8], at: usize) -> Option<u16> {
    let raw = bytes.get(at..at.checked_add(4)?)?;
    let le = u16::from_le_bytes([raw[0], raw[1]]);
    let be = u16::from_be_bytes([raw[2], raw[3]]);
    (le == be).then_some(le)
}
