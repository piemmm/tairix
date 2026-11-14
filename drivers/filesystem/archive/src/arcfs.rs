//! RISC OS `ArcFS` archives (`plans/ARCHIVES.md` AR8): a header, a table of
//! entries — each directory's members after it and an end entry closing it —
//! and a data area the files' coded bytes lie in. Members stored, packed,
//! crunched and compressed are read; a member is written compressed, or
//! stored where compressing would not shrink it.
//!
//! The table is written afresh by every commit, a member nothing has changed
//! keeping its own entry with only its data's place moved; a file whose
//! bytes are unchanged keeps its coded bytes, copied by the kernel.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use tairix_abi::driver::filesystem::{NameMatching, NodeKind, WriteRefusal};
use tairix_abi::fs::Filetype;
use tairix_abi::volume::ARCHIVE_REGION;
use tairix_abi::DriverError;
use tairix_compress::arc::{self, Method};
use tairix_fsprobe::archive::{arcfs_header, ARCFS_ENTRY_LEN, ARCFS_HEADER_LEN, ARCFS_MAGIC};
use tairix_fsserve::VolumeAccess;

use crate::data::{Archived, Check, Coding, Data, Lead};
use crate::format::{leaves_out, Content, Format, Placed, Read};
use crate::name::{from_riscos, to_riscos};
use crate::region::{Regions, Sink};
use crate::riscos::{meta_of, words_of, Words};
use crate::spark::compress_into;
use crate::tree::{Body, Kept, Tree, MAX_DEPTH, MAX_MEMBERS, ROOT};
use crate::zip::directory_meta;

/// The longest name an entry holds, read.
const NAME_FIELD: usize = 11;
/// The longest name written: one RISC OS's filing systems of `ArcFS`'s day
/// hold, its NUL in the field.
const NAME_MAX: usize = NAME_FIELD - 1;

/// An entry's first byte: what it is, or how a file's bytes are coded.
const END: u8 = 0x00;
const DELETED: u8 = 0x01;
const STORED: u8 = 0x82;
const PACKED: u8 = 0x83;
const CRUNCHED: u8 = 0x88;
const COMPRESSED: u8 = 0xFF;

/// A directory entry's info word's mark, and a file's data offset's bound.
const DIRECTORY_BIT: u32 = 1 << 31;
/// The lengths a directory's entry gives.
const DIRECTORY_LEN: u32 = u32::MAX;

/// The version words an archive written here carries: the `ArcFS` that reads
/// it, and the one that writes it, each a hundredth.
const READ_VERSION: u32 = 40;
const WRITE_VERSION: u32 = 100;

/// The type a directory's load word carries.
const DIRECTORY_TYPE: Filetype = Filetype::from_low_bits(0xFFF);

/// Entries the table is read in at a time.
const TABLE_CHUNK: usize = 1820;

/// What an `ArcFS` archive keeps of itself beyond its members: nothing.
pub(crate) struct ArcFs;

/// Whether a member may be named `name`: RISC OS can spell it in the ten
/// bytes an entry holds with its NUL.
pub(crate) fn admits_name(name: &[u8]) -> Result<(), DriverError> {
    to_riscos(name, NAME_MAX)
        .map(|_| ())
        .ok_or(DriverError::LengthOutOfRange)
}

/// One entry, read.
struct Entry {
    info: u8,
    /// The name as the archive spells it.
    name: Vec<u8>,
    len: u32,
    words: Words,
    attr: u32,
    packed: u32,
    word: u32,
}

impl Entry {
    fn read(raw: &[u8]) -> Result<Self, DriverError> {
        let le32 = |at: usize| u32::from_le_bytes([raw[at], raw[at + 1], raw[at + 2], raw[at + 3]]);
        let field = &raw[1..=NAME_FIELD];
        let end = field
            .iter()
            .position(|&byte| byte < 0x20)
            .unwrap_or(NAME_FIELD);
        let mut name = Vec::new();
        name.try_reserve_exact(end)
            .map_err(|_| DriverError::OutOfMemory)?;
        name.extend_from_slice(&field[..end]);
        let attr = le32(24);
        Ok(Self {
            info: raw[0],
            name,
            len: le32(12),
            words: Words {
                load: le32(16),
                exec: le32(20),
                attr: attr.to_le_bytes()[0],
            },
            attr,
            packed: le32(28),
            word: le32(32),
        })
    }

    /// How its file's bytes are coded, by its info byte and the width its
    /// attribute word gives.
    fn coding(&self) -> Coding {
        let packed = u64::from(self.packed);
        let bits = self.attr.to_le_bytes()[1];
        let lzw = |rle| match bits {
            arc::LZW_MIN_BITS..=arc::LZW_MAX_BITS => Coding::Arc {
                packed,
                skip: 0,
                method: Method::Lzw {
                    max_bits: bits,
                    rle,
                },
            },
            _ => Coding::Refused {
                why: DriverError::BadMagic,
                packed,
            },
        };
        match self.info {
            STORED if self.packed == self.len => Coding::Stored,
            PACKED => Coding::Arc {
                packed,
                skip: 0,
                method: Method::Packed,
            },
            CRUNCHED => lzw(true),
            COMPRESSED => lzw(false),
            STORED => Coding::Refused {
                why: DriverError::BadMagic,
                packed,
            },
            _ => Coding::Refused {
                why: DriverError::Unsupported,
                packed,
            },
        }
    }

    /// The CRC its attribute word records, or none where it is zero.
    const fn check(&self) -> Check {
        let [_, _, low, high] = self.attr.to_le_bytes();
        match u16::from_le_bytes([low, high]) {
            0 => Check::None,
            crc => Check::Crc16(crc),
        }
    }
}

/// Read the `ArcFS` archive `regions` holds.
///
/// # Errors
///
/// [`DriverError::BadMagic`] for no header or a table out of shape;
/// [`DriverError::Unsupported`] for a format version this driver does not
/// read or more members than it opens.
pub(crate) fn read<A: VolumeAccess>(regions: &Regions<A>) -> Result<Read, DriverError> {
    let head = regions.archive_bytes(
        0,
        ARCFS_HEADER_LEN.min(usize::try_from(regions.len()).unwrap_or(0)),
    )?;
    let header = arcfs_header(&head, regions.len()).ok_or(DriverError::BadMagic)?;
    if head.len() < ARCFS_HEADER_LEN || head[24..28] != [0; 4] {
        return Err(DriverError::Unsupported);
    }
    let entries = header.entries_len as usize / ARCFS_ENTRY_LEN;
    // Every member and directory end takes an entry.
    if entries > 2 * MAX_MEMBERS + 1 {
        return Err(DriverError::Unsupported);
    }
    let data = u64::from(header.data);
    let mut tree = Tree::new(NameMatching::AsciiCaseInsensitive, directory_meta());
    let mut refusal = None;
    // The directories open, innermost last; `None` for one left out, whose
    // members go with it.
    let mut open: Vec<Option<u64>> = Vec::new();
    open.try_reserve(MAX_DEPTH + 1)
        .map_err(|_| DriverError::OutOfMemory)?;
    open.push(Some(ROOT));
    let mut members = 0usize;
    let mut chunk = tairix_util::fallible::filled(TABLE_CHUNK * ARCFS_ENTRY_LEN, 0u8)
        .ok_or(DriverError::OutOfMemory)?;
    let mut index = 0;
    'table: while index < entries {
        let count = (entries - index).min(TABLE_CHUNK);
        let bytes = &mut chunk[..count * ARCFS_ENTRY_LEN];
        regions.read_archive((ARCFS_HEADER_LEN + index * ARCFS_ENTRY_LEN) as u64, bytes)?;
        index += count;
        for raw in bytes.as_chunks::<ARCFS_ENTRY_LEN>().0 {
            let entry = Entry::read(raw)?;
            match entry.info {
                DELETED => continue,
                END => {
                    open.pop();
                    continue;
                }
                _ => {}
            }
            let Some(&parent) = open.last() else {
                // A member past the root's end, in no directory: writing the
                // archive back would lose it.
                refusal = Some(WriteRefusal::Damaged);
                break 'table;
            };
            members += 1;
            if members > MAX_MEMBERS {
                return Err(DriverError::Unsupported);
            }
            let directory = entry.word & DIRECTORY_BIT != 0;
            let admitted = match parent {
                Some(dir) => admit(&mut tree, dir, &entry, raw, data, regions),
                None => Err(DriverError::Unsupported),
            };
            let id = match admitted {
                Ok(id) => Some(id),
                Err(err) if leaves_out(err) => {
                    refusal = Some(WriteRefusal::UnsupportedFeature);
                    None
                }
                Err(err) => return Err(err),
            };
            if directory {
                open.try_reserve(1).map_err(|_| DriverError::OutOfMemory)?;
                open.push(id);
            }
        }
    }
    if !open.is_empty() {
        // The table ended inside a directory.
        refusal = Some(WriteRefusal::Damaged);
    }
    Ok(Read {
        tree,
        format: Format::ArcFs(ArcFs),
        refusal,
        stream: None,
        rar: None,
    })
}

/// Put the member `entry` describes, whose raw entry is `raw`, into `dir`,
/// answering its node; its data, if it is a file, lies `data` on from the
/// data area's start.
fn admit<A: VolumeAccess>(
    tree: &mut Tree,
    dir: u64,
    entry: &Entry,
    raw: &[u8],
    data: u64,
    regions: &Regions<A>,
) -> Result<u64, DriverError> {
    let name = from_riscos(&entry.name).ok_or(DriverError::OutOfRange)?;
    let mut notes = Vec::new();
    notes
        .try_reserve_exact(raw.len())
        .map_err(|_| DriverError::OutOfMemory)?;
    notes.extend_from_slice(raw);
    if entry.word & DIRECTORY_BIT != 0 {
        let meta = meta_of(Some(entry.words), None, NodeKind::Directory);
        return tree.add(dir, &name, Body::Dir(BTreeMap::new()), meta, None);
    }
    let at = data + u64::from(entry.word);
    let end = at
        .checked_add(u64::from(entry.packed))
        .filter(|&end| end <= regions.len())
        .ok_or(DriverError::OutOfRange)?;
    let body = Body::File(Data::Archived(Archived {
        at,
        lead: Lead::None,
        len: u64::from(entry.len),
        coding: entry.coding(),
        check: entry.check(),
    }));
    let meta = meta_of(Some(entry.words), None, NodeKind::RegularFile);
    let kept = Some(Kept {
        record: at..end,
        notes,
    });
    tree.add(dir, &name, body, meta, kept)
}

/// The name a member is written under, as RISC OS spells it, in the bytes an
/// entry holds: what one read held, or a new name already held to what it
/// holds with its NUL.
fn spelling(name: &[u8]) -> Result<Vec<u8>, DriverError> {
    to_riscos(name, NAME_FIELD).ok_or(DriverError::LengthOutOfRange)
}

/// One entry of the table being written.
enum Slot {
    File(u64),
    Directory(u64),
    End,
}

/// The table's entries for `dir`'s members and on down, pre-order, each
/// directory's members sorted as RISC OS sorts them and its end after them.
fn slots(tree: &Tree, dir: u64, out: &mut Vec<Slot>) -> Result<(), DriverError> {
    let mut members: Vec<(Vec<u8>, u64)> = Vec::new();
    for &id in tree.entries(dir)?.values() {
        let mut key = spelling(&tree.node(id)?.name)?;
        key.make_ascii_uppercase();
        members
            .try_reserve(1)
            .map_err(|_| DriverError::OutOfMemory)?;
        members.push((key, id));
    }
    members.sort_unstable();
    out.try_reserve(members.len() + 1)
        .map_err(|_| DriverError::OutOfMemory)?;
    for (_, id) in members {
        if matches!(tree.node(id)?.body, Body::Dir(_)) {
            out.push(Slot::Directory(id));
            slots(tree, id, out)?;
            out.push(Slot::End);
        } else {
            out.push(Slot::File(id));
        }
    }
    Ok(())
}

/// Write the next archive from `tree` into `sink`.
pub(crate) fn write<A: VolumeAccess>(
    tree: &Tree,
    content: &mut Content<'_, A>,
    sink: &mut Sink<'_, A>,
) -> Result<Vec<Placed>, DriverError> {
    let mut order = Vec::new();
    slots(tree, ROOT, &mut order)?;
    order.push(Slot::End);
    let table_len = order.len() * ARCFS_ENTRY_LEN;
    let data = ARCFS_HEADER_LEN + table_len;
    let mut header = [0u8; ARCFS_HEADER_LEN];
    header[..ARCFS_MAGIC.len()].copy_from_slice(ARCFS_MAGIC);
    let field = |value: usize| u32::try_from(value).map_err(|_| DriverError::LengthOutOfRange);
    header[8..12].copy_from_slice(&field(table_len)?.to_le_bytes());
    header[12..16].copy_from_slice(&field(data)?.to_le_bytes());
    header[16..20].copy_from_slice(&READ_VERSION.to_le_bytes());
    header[20..24].copy_from_slice(&WRITE_VERSION.to_le_bytes());
    sink.put(&header)?;
    let mut table =
        tairix_util::fallible::filled(table_len, 0u8).ok_or(DriverError::OutOfMemory)?;
    sink.put(&table)?;
    let mut placed = Vec::new();
    placed
        .try_reserve_exact(order.len())
        .map_err(|_| DriverError::OutOfMemory)?;
    let mut work = Vec::new();
    // Each directory's entry is written once its end's index is known.
    let mut opened: Vec<usize> = Vec::new();
    for (index, slot) in order.iter().enumerate() {
        let entry = &mut table[index * ARCFS_ENTRY_LEN..(index + 1) * ARCFS_ENTRY_LEN];
        match *slot {
            Slot::File(id) => {
                let start = sink.position();
                let offset = start - data as u64;
                if offset >= u64::from(DIRECTORY_BIT) {
                    // A file's data must lie within the reach its info
                    // word's offset spans.
                    return Err(DriverError::LengthOutOfRange);
                }
                let (data, kept) = file(tree, id, content, sink, &mut work, entry, offset)?;
                placed.push(Placed {
                    id,
                    data: Some(data),
                    kept: Some(Kept {
                        record: start..sink.position(),
                        notes: kept,
                    }),
                    notes: None,
                });
            }
            Slot::Directory(id) => {
                let node = tree.node(id)?;
                let words = words_of(&node.meta, DIRECTORY_TYPE)?;
                put_entry(
                    entry,
                    STORED,
                    &spelling(&node.name)?,
                    DIRECTORY_LEN,
                    words,
                    u32::from(words.attr),
                    DIRECTORY_LEN,
                    0,
                );
                opened
                    .try_reserve(1)
                    .map_err(|_| DriverError::OutOfMemory)?;
                opened.push(index);
                placed.push(Placed {
                    id,
                    data: None,
                    kept: None,
                    notes: None,
                });
            }
            Slot::End => {
                // An end entry is zeros; the directory it closes names
                // the entry after it.
                if let Some(dir) = opened.pop() {
                    let next = field((index + 1) * ARCFS_ENTRY_LEN)? | DIRECTORY_BIT;
                    table[dir * ARCFS_ENTRY_LEN + 32..dir * ARCFS_ENTRY_LEN + 36]
                        .copy_from_slice(&next.to_le_bytes());
                }
            }
        }
    }
    sink.patch(ARCFS_HEADER_LEN as u64, &table)?;
    Ok(placed)
}

/// Write file `id`'s coded bytes into the data area at `offset` and its
/// entry into `entry`, answering where its bytes now are and the entry, kept
/// as its notes.
fn file<A: VolumeAccess>(
    tree: &Tree,
    id: u64,
    content: &mut Content<'_, A>,
    sink: &mut Sink<'_, A>,
    work: &mut Vec<u32>,
    entry: &mut [u8],
    offset: u64,
) -> Result<(Data, Vec<u8>), DriverError> {
    let node = tree.node(id)?;
    let Body::File(data) = &node.body else {
        return Err(DriverError::DeviceFault);
    };
    let info_word = u32::try_from(offset).map_err(|_| DriverError::LengthOutOfRange)?;
    let at = sink.position();
    let size = |value: u64| u32::try_from(value).map_err(|_| DriverError::LengthOutOfRange);
    if let (Data::Archived(archived), Some(kept)) = (data, &node.kept) {
        // Nothing about it has changed: its own entry, its bytes moved.
        if kept.notes.len() == ARCFS_ENTRY_LEN {
            entry.copy_from_slice(&kept.notes);
            entry[32..36].copy_from_slice(&info_word.to_le_bytes());
            sink.copy(ARCHIVE_REGION, archived.at, archived.packed())?;
            return Ok((Data::Archived(Archived { at, ..*archived }), notes(entry)?));
        }
    }
    let words = words_of(&node.meta, Filetype::DATA)?;
    let spelt = spelling(&node.name)?;
    if let Data::Archived(archived) = data {
        if let Some((info, bits)) = arcfs_method(archived.coding) {
            let crc = match archived.check {
                Check::Crc16(crc) => crc,
                _ => 0,
            };
            let attr = u32::from(crc) << 16 | u32::from(bits) << 8 | u32::from(words.attr);
            put_entry(
                entry,
                info,
                &spelt,
                size(archived.len)?,
                words,
                attr,
                size(archived.packed())?,
                info_word,
            );
            sink.copy(ARCHIVE_REGION, archived.at, archived.packed())?;
            return Ok((Data::Archived(Archived { at, ..*archived }), notes(entry)?));
        }
    }
    let len = data.len();
    let crc = compress_into(content, sink, work, (id, node.content_gen), data)?;
    let packed = sink.position() - at;
    let (info, bits, coding, packed) = if packed < len {
        let method = Method::Lzw {
            max_bits: arc::LZW_MAX_BITS,
            rle: false,
        };
        (
            COMPRESSED,
            arc::LZW_MAX_BITS,
            Coding::Arc {
                packed,
                skip: 0,
                method,
            },
            packed,
        )
    } else {
        // Compressing would not shrink it.
        sink.rewind(at)?;
        content.copy_into((id, node.content_gen), data, sink)?;
        (STORED, 0, Coding::Stored, len)
    };
    let attr = u32::from(crc) << 16 | u32::from(bits) << 8 | u32::from(words.attr);
    put_entry(
        entry,
        info,
        &spelt,
        size(len)?,
        words,
        attr,
        size(packed)?,
        info_word,
    );
    let data = Data::Archived(Archived {
        at,
        lead: Lead::None,
        len,
        coding,
        check: Check::Crc16(crc),
    });
    Ok((data, notes(entry)?))
}

/// The `ArcFS` method and bit count an archived member's `coding` is kept
/// as, where the format names one.
fn arcfs_method(coding: Coding) -> Option<(u8, u8)> {
    match coding {
        Coding::Stored => Some((STORED, 0)),
        Coding::Arc {
            skip: 0,
            method: Method::Packed,
            ..
        } => Some((PACKED, 0)),
        Coding::Arc {
            skip: 0,
            method: Method::Lzw { max_bits, rle },
            ..
        } => Some((if rle { CRUNCHED } else { COMPRESSED }, max_bits)),
        _ => None,
    }
}

/// A table entry, copied to be kept as a member's notes.
fn notes(entry: &[u8]) -> Result<Vec<u8>, DriverError> {
    tairix_util::fallible::copied(entry).ok_or(DriverError::OutOfMemory)
}

/// Fill a 36-byte table entry.
#[allow(clippy::too_many_arguments)] // An entry is these eight fields, laid side by side.
fn put_entry(
    entry: &mut [u8],
    info: u8,
    spelt: &[u8],
    len: u32,
    words: Words,
    attr: u32,
    packed: u32,
    word: u32,
) {
    entry.fill(0);
    entry[0] = info;
    entry[1..=spelt.len()].copy_from_slice(spelt);
    entry[12..16].copy_from_slice(&len.to_le_bytes());
    entry[16..20].copy_from_slice(&words.load.to_le_bytes());
    entry[20..24].copy_from_slice(&words.exec.to_le_bytes());
    entry[24..28].copy_from_slice(&attr.to_le_bytes());
    entry[28..32].copy_from_slice(&packed.to_le_bytes());
    entry[32..36].copy_from_slice(&word.to_le_bytes());
}

#[cfg(test)]
#[path = "arcfs_tests.rs"]
mod tests;
