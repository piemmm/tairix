//! RISC OS Spark archives (`plans/ARCHIVES.md` AR7), and the plain ARC
//! archives Spark extends: ARC headers, the RISC OS load, exec and attribute
//! words Spark adds, and each directory an archive nested in its member.
//! Every ARC method is read; a member is written compressed — the dynamic
//! LZW Spark calls that — or stored where compressing would not shrink it.
//!
//! A member nothing has changed is copied whole, header and all, by the
//! kernel, and so is a directory nothing beneath has changed; a member whose
//! bytes are unchanged gets a new header over the same coded bytes, copied
//! likewise. Bytes a self-extracting stub put ahead of the first header, and
//! whatever follows the end, are copied as they are.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::ops::Range;

use tairix_abi::driver::filesystem::{NameMatching, NodeKind, WriteRefusal};
use tairix_abi::fs::Filetype;
use tairix_abi::time::{DosTime, Time64};
use tairix_abi::volume::ARCHIVE_REGION;
use tairix_abi::DriverError;
use tairix_compress::arc::{self, Hash, Method};
use tairix_fsmeta::preset::acorn;
use tairix_fsprobe::archive::{ARC_HEADER_LEN, ARC_MARKER, ARC_NAME_LEN, ARC_RISCOS_BIT};
use tairix_fsserve::VolumeAccess;

use crate::data::{Archived, Check, Coding, Data, Lead};
use crate::format::{leaves_out, Content, Format, Placed, Read};
use crate::name::{from_riscos, to_riscos, Charset};
use crate::region::{Regions, Sink, SinkOutput};
use crate::riscos::{meta_of, words_of, Words};
use crate::tree::{Body, Kept, Tree, MAX_DEPTH, MAX_MEMBERS, ROOT};
use crate::zip::directory_meta;

/// The longest name a header holds, its NUL aside.
const NAME_MAX: usize = ARC_NAME_LEN - 1;

/// The bytes ahead of the first header a self-extracting archive may carry.
const LEAD_MAX: u64 = 3;

/// The length of a header with Spark's RISC OS words.
const HEADER_LEN: usize = ARC_HEADER_LEN + 12;

/// The end of an archive, or of a directory nested in one, as Spark writes
/// it.
const END: [u8; 2] = [ARC_MARKER, ARC_RISCOS_BIT];

/// The type a Spark directory's load word carries.
const DIRECTORY_TYPE: Filetype = Filetype::from_low_bits(0xDDC);

/// A stored member's method byte, with the RISC OS words.
const STORED: u8 = ARC_RISCOS_BIT | 2;
/// A compressed member's: dynamic LZW, its width in its first byte.
const COMPRESSED: u8 = ARC_RISCOS_BIT | 0x7F;
/// The width a member written here is compressed to: the widest Spark
/// reads.
const WRITE_BITS: u8 = arc::LZW_MAX_BITS;

/// What a Spark archive keeps of itself beyond its members: the bytes ahead
/// of its first header and after its end, each copied as it is.
pub(crate) struct Spark {
    prefix: u64,
    suffix: Range<u64>,
}

/// Whether a member may be named `name`: RISC OS can spell it in the twelve
/// bytes a header holds.
pub(crate) fn admits_name(name: &[u8]) -> Result<(), DriverError> {
    to_riscos(name, NAME_MAX)
        .map(|_| ())
        .ok_or(DriverError::LengthOutOfRange)
}

/// One header, read.
struct Header {
    kind: u8,
    /// The name as the archive spells it.
    name: Vec<u8>,
    packed: u64,
    dos: DosTime,
    crc: u16,
    len: u64,
    words: Option<Words>,
    /// The header's own length.
    size: u64,
}

impl Header {
    const fn method(&self) -> u8 {
        self.kind & !ARC_RISCOS_BIT
    }

    /// Whether it opens a nested directory: stored, typed as Spark's
    /// directory type.
    fn names_directory(&self) -> bool {
        self.kind == STORED
            && self.words.is_some_and(|words| {
                matches!(
                    acorn::decode_load_exec(words.load, words.exec),
                    acorn::LoadExec::Typed { filetype, .. } if filetype == DIRECTORY_TYPE
                )
            })
    }
}

/// What lies at a place in an archive's headers.
enum Record {
    Member(Header),
    End,
}

/// The record at `at`, which must end by `end`, or `None` where the bytes
/// there open none.
fn record<A: VolumeAccess>(
    regions: &Regions<A>,
    at: u64,
    end: u64,
) -> Result<Option<Record>, DriverError> {
    if end.saturating_sub(at) < 2 {
        return Ok(None);
    }
    let lead = regions.archive_bytes(at, 2)?;
    if lead[0] != ARC_MARKER {
        return Ok(None);
    }
    let kind = lead[1];
    let method = kind & !ARC_RISCOS_BIT;
    if method == 0 {
        return Ok(Some(Record::End));
    }
    let size = if method == 1 {
        ARC_HEADER_LEN - 4
    } else {
        ARC_HEADER_LEN
    } + if kind & ARC_RISCOS_BIT == 0 { 0 } else { 12 };
    if end - at < size as u64 {
        return Ok(None);
    }
    let bytes = regions.archive_bytes(at, size)?;
    let field = &bytes[2..2 + ARC_NAME_LEN];
    let Some(nul) = field.iter().position(|&byte| byte == 0) else {
        return Ok(None);
    };
    let le16 = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]);
    let le32 =
        |at: usize| u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    let packed = u64::from(le32(15));
    let mut name = Vec::new();
    name.try_reserve_exact(nul)
        .map_err(|_| DriverError::OutOfMemory)?;
    name.extend_from_slice(&field[..nul]);
    let words = (kind & ARC_RISCOS_BIT != 0).then(|| {
        let at = size - 12;
        Words {
            load: le32(at),
            exec: le32(at + 4),
            attr: bytes[at + 8],
        }
    });
    Ok(Some(Record::Member(Header {
        kind,
        name,
        packed,
        dos: DosTime {
            date: le16(19),
            time: le16(21),
        },
        crc: le16(23),
        len: if method == 1 {
            packed
        } else {
            u64::from(le32(25))
        },
        words,
        size: size as u64,
    })))
}

/// Whether `range` holds one archive's headers, ended by its end marker
/// exactly at the range's end: what a directory's member must hold.
fn holds_archive<A: VolumeAccess>(
    regions: &Regions<A>,
    range: Range<u64>,
) -> Result<bool, DriverError> {
    let mut at = range.start;
    // Each record takes at least a header, so the walk is bounded.
    while at < range.end {
        match record(regions, at, range.end)? {
            Some(Record::End) => return Ok(at + 2 == range.end),
            Some(Record::Member(header)) => {
                at = match (at + header.size).checked_add(header.packed) {
                    Some(next) if next <= range.end => next,
                    _ => return Ok(false),
                };
            }
            None => return Ok(false),
        }
    }
    Ok(false)
}

/// How a member coded as `header` says is read, its width byte read where
/// its stream opens with one.
fn coding<A: VolumeAccess>(
    regions: &Regions<A>,
    header: &Header,
    at: u64,
) -> Result<Coding, DriverError> {
    let packed = header.packed;
    let arc = |skip, method| Coding::Arc {
        packed,
        skip,
        method,
    };
    let refused = |why| Coding::Refused { why, packed };
    let width = || -> Result<Option<u8>, DriverError> {
        Ok(if packed == 0 {
            None
        } else {
            Some(regions.archive_bytes(at, 1)?[0])
        })
    };
    Ok(match header.method() {
        1 | 2 if packed == header.len => Coding::Stored,
        1 | 2 => refused(DriverError::BadMagic),
        3 => arc(0, Method::Packed),
        4 => arc(0, Method::Squeezed),
        5 => arc(
            0,
            Method::Crunched {
                hash: Hash::Square,
                rle: false,
            },
        ),
        6 => arc(
            0,
            Method::Crunched {
                hash: Hash::Square,
                rle: true,
            },
        ),
        7 => arc(
            0,
            Method::Crunched {
                hash: Hash::Product,
                rle: true,
            },
        ),
        // ARC 5's crunching names its width, always twelve bits; another
        // first byte is a member garbled with a password.
        8 => match width()? {
            Some(12) => arc(
                1,
                Method::Lzw {
                    max_bits: 12,
                    rle: true,
                },
            ),
            _ => refused(DriverError::PermissionDenied),
        },
        9 => arc(
            0,
            Method::Lzw {
                max_bits: 13,
                rle: false,
            },
        ),
        0x7F => match width()?.map(|byte| byte & 0x1F) {
            Some(bits @ arc::LZW_MIN_BITS..=arc::LZW_MAX_BITS) => arc(
                1,
                Method::Lzw {
                    max_bits: bits,
                    rle: false,
                },
            ),
            _ => refused(DriverError::BadMagic),
        },
        // PAK's crushing and distilling, which no RISC OS archiver writes.
        _ => refused(DriverError::Unsupported),
    })
}

/// The reading of one archive: its tree, how many members it has, and why
/// it may not be written back whole.
struct Reading {
    tree: Tree,
    members: usize,
    refusal: Option<WriteRefusal>,
}

impl Reading {
    /// Note that the archive cannot be written back whole, for `why`.
    fn refuse(&mut self, why: WriteRefusal) {
        self.refusal.get_or_insert(why);
    }

    /// Read the archive `range` holds into directory `dir`, answering where
    /// it ended: its end marker's end, or where what can be believed of it
    /// stopped.
    fn level<A: VolumeAccess>(
        &mut self,
        regions: &Regions<A>,
        dir: u64,
        range: Range<u64>,
        depth: usize,
    ) -> Result<u64, DriverError> {
        let mut at = range.start;
        loop {
            let header = match record(regions, at, range.end)? {
                Some(Record::End) => return Ok(at + 2),
                Some(Record::Member(header)) => header,
                // A damaged header ends what can be believed.
                None => {
                    self.refuse(WriteRefusal::Damaged);
                    return Ok(at);
                }
            };
            let data = at + header.size;
            let Some(next) = data
                .checked_add(header.packed)
                .filter(|&next| next <= range.end)
            else {
                self.refuse(WriteRefusal::Damaged);
                return Ok(at);
            };
            self.members += 1;
            if self.members > MAX_MEMBERS {
                return Err(DriverError::Unsupported);
            }
            if matches!(header.method(), 20..=22 | 30 | 31) {
                // ARC 6's notes and subdirectories, which no member is.
                self.refuse(WriteRefusal::UnsupportedFeature);
            } else if let Err(err) = self.admit(regions, dir, &header, at..next, depth) {
                if !leaves_out(err) {
                    return Err(err);
                }
                // A member no volume can serve is left out; writing the
                // archive back would lose it.
                self.refuse(WriteRefusal::UnsupportedFeature);
            }
            at = next;
        }
    }

    /// Put the member `header` heads, whose record is `record`, into `dir`.
    fn admit<A: VolumeAccess>(
        &mut self,
        regions: &Regions<A>,
        dir: u64,
        header: &Header,
        record: Range<u64>,
        depth: usize,
    ) -> Result<(), DriverError> {
        // A plain ARC member's name is DOS's, a dot in it its own.
        let name = match header.words {
            Some(_) => from_riscos(&header.name),
            None => Charset::Cp437
                .decode(&header.name)
                .filter(|name| crate::name::servable(name)),
        }
        .ok_or(DriverError::OutOfRange)?;
        let data = record.start + header.size;
        let kept = Some(Kept {
            record: record.clone(),
            notes: Vec::new(),
        });
        let dated = header.dos.to_time64();
        if header.names_directory()
            && depth < MAX_DEPTH
            && holds_archive(regions, data..record.end)?
        {
            let meta = meta_of(header.words, dated, NodeKind::Directory);
            let id = self
                .tree
                .add(dir, &name, Body::Dir(BTreeMap::new()), meta, kept)?;
            self.level(regions, id, data..record.end, depth + 1)?;
            return Ok(());
        }
        let body = Body::File(Data::Archived(Archived {
            at: data,
            lead: Lead::None,
            len: header.len,
            coding: coding(regions, header, data)?,
            check: Check::Crc16(header.crc),
        }));
        let meta = meta_of(header.words, dated, NodeKind::RegularFile);
        self.tree.add(dir, &name, body, meta, kept).map(|_| ())
    }
}

/// Read the Spark or ARC archive `regions` holds.
///
/// # Errors
///
/// [`DriverError::BadMagic`] where no header opens the archive;
/// [`DriverError::Unsupported`] past the members this driver opens.
pub(crate) fn read<A: VolumeAccess>(regions: &Regions<A>) -> Result<Read, DriverError> {
    let head = regions.archive_bytes(
        0,
        usize::try_from(regions.len().min(LEAD_MAX + 1)).unwrap_or(0),
    )?;
    let prefix = head
        .iter()
        .position(|&byte| byte == ARC_MARKER)
        .ok_or(DriverError::BadMagic)? as u64;
    let mut reading = Reading {
        tree: Tree::new(NameMatching::AsciiCaseInsensitive, directory_meta()),
        members: 0,
        refusal: None,
    };
    let end = reading.level(regions, ROOT, prefix..regions.len(), 0)?;
    if end == prefix {
        return Err(DriverError::BadMagic);
    }
    Ok(Read {
        tree: reading.tree,
        format: Format::Spark(Spark {
            prefix,
            suffix: end..regions.len(),
        }),
        refusal: reading.refusal,
        stream: None,
        rar: None,
    })
}

/// The method byte a member coded as `coding` is written with, or `None`
/// for a coding no Spark method is.
const fn method_byte(coding: Coding) -> Option<u8> {
    let method = match coding {
        Coding::Stored => 2,
        Coding::Arc { method, skip, .. } => match (method, skip) {
            (Method::Packed, 0) => 3,
            (Method::Squeezed, 0) => 4,
            (
                Method::Crunched {
                    hash: Hash::Square,
                    rle: false,
                },
                0,
            ) => 5,
            (
                Method::Crunched {
                    hash: Hash::Square,
                    rle: true,
                },
                0,
            ) => 6,
            (
                Method::Crunched {
                    hash: Hash::Product,
                    rle: true,
                },
                0,
            ) => 7,
            (
                Method::Lzw {
                    max_bits: 12,
                    rle: true,
                },
                1,
            ) => 8,
            (
                Method::Lzw {
                    max_bits: 13,
                    rle: false,
                },
                0,
            ) => 9,
            (Method::Lzw { rle: false, .. }, 1) => 0x7F,
            _ => return None,
        },
        _ => return None,
    };
    Some(ARC_RISCOS_BIT | method)
}

/// A header with Spark's RISC OS words.
fn header_bytes(
    kind: u8,
    spelt: &[u8],
    (packed, len): (u64, u64),
    modified: Time64,
    crc: u16,
    words: Words,
) -> Result<[u8; HEADER_LEN], DriverError> {
    let size = |value: u64| u32::try_from(value).map_err(|_| DriverError::LengthOutOfRange);
    // The legacy stamp is held to the years it spells; the words keep the
    // exact time.
    let held = modified
        .secs()
        .clamp(DosTime::EARLIEST.secs(), DosTime::LATEST.secs());
    let dos = DosTime::from_time64(Time64::from_secs(held)).ok_or(DriverError::OutOfRange)?;
    let mut bytes = [0u8; HEADER_LEN];
    bytes[0] = ARC_MARKER;
    bytes[1] = kind;
    bytes[2..2 + spelt.len()].copy_from_slice(spelt);
    bytes[15..19].copy_from_slice(&size(packed)?.to_le_bytes());
    bytes[19..21].copy_from_slice(&dos.date.to_le_bytes());
    bytes[21..23].copy_from_slice(&dos.time.to_le_bytes());
    bytes[23..25].copy_from_slice(&crc.to_le_bytes());
    bytes[25..29].copy_from_slice(&size(len)?.to_le_bytes());
    bytes[29..33].copy_from_slice(&words.load.to_le_bytes());
    bytes[33..37].copy_from_slice(&words.exec.to_le_bytes());
    bytes[37] = words.attr;
    Ok(bytes)
}

/// The name a member is written under: its own, as RISC OS spells it, and
/// held to what a header holds.
fn spelling(name: &[u8]) -> Result<Vec<u8>, DriverError> {
    to_riscos(name, NAME_MAX).ok_or(DriverError::LengthOutOfRange)
}

/// The members of `dir`, in the order a writer keeps.
fn children(tree: &Tree, dir: u64) -> Result<Vec<u64>, DriverError> {
    let entries = tree.entries(dir)?;
    let mut ids = Vec::new();
    ids.try_reserve_exact(entries.len())
        .map_err(|_| DriverError::OutOfMemory)?;
    ids.extend(entries.values().copied());
    ids.sort_unstable_by_key(|&id| tree.node(id).map_or(u64::MAX, |node| node.order));
    Ok(ids)
}

/// The record `id` was read from, where it can be copied whole: nothing
/// about it has changed, and for a directory nothing beneath it either — its
/// members' records still lie end to end through its nested archive.
fn whole(tree: &Tree, id: u64) -> Result<Option<Range<u64>>, DriverError> {
    let node = tree.node(id)?;
    let Some(kept) = &node.kept else {
        return Ok(None);
    };
    if !matches!(node.body, Body::Dir(_)) {
        return Ok(Some(kept.record.clone()));
    }
    let mut at = kept.record.start + HEADER_LEN as u64;
    for child in children(tree, id)? {
        match whole(tree, child)? {
            Some(record) if record.start == at => at = record.end,
            _ => return Ok(None),
        }
    }
    Ok((at + END.len() as u64 == kept.record.end).then(|| kept.record.clone()))
}

impl Spark {
    /// Write the next archive from `tree` into `sink`.
    pub(crate) fn write<A: VolumeAccess>(
        &mut self,
        tree: &Tree,
        content: &mut Content<'_, A>,
        sink: &mut Sink<'_, A>,
    ) -> Result<Vec<Placed>, DriverError> {
        let mut placed = Vec::new();
        placed
            .try_reserve_exact(tree.len())
            .map_err(|_| DriverError::OutOfMemory)?;
        sink.copy(ARCHIVE_REGION, 0, self.prefix)?;
        let mut work = Vec::new();
        Writer {
            tree,
            content,
            sink,
            placed: &mut placed,
            work: &mut work,
        }
        .directory(ROOT)?;
        sink.put(&END)?;
        let suffix = self.suffix.end - self.suffix.start;
        let at = sink.position();
        sink.copy(ARCHIVE_REGION, self.suffix.start, suffix)?;
        self.suffix = at..at + suffix;
        Ok(placed)
    }
}

/// A Spark archive being written.
struct Writer<'w, 'c, 's, 'r, A> {
    tree: &'w Tree,
    content: &'w mut Content<'c, A>,
    sink: &'s mut Sink<'r, A>,
    placed: &'w mut Vec<Placed>,
    /// The compressor's dictionary, kept from member to member.
    work: &'w mut Vec<u32>,
}

impl<A: VolumeAccess> Writer<'_, '_, '_, '_, A> {
    /// Write the members of `dir`, each record in turn.
    fn directory(&mut self, dir: u64) -> Result<(), DriverError> {
        for id in children(self.tree, dir)? {
            let start = self.sink.position();
            if let Some(record) = whole(self.tree, id)? {
                self.sink
                    .copy(ARCHIVE_REGION, record.start, record.end - record.start)?;
                self.moved(id, start, record.start)?;
                continue;
            }
            let node = self.tree.node(id)?;
            let spelt = spelling(&node.name)?;
            match &node.body {
                Body::Dir(_) => {
                    let words = words_of(&node.meta, DIRECTORY_TYPE)?;
                    let header =
                        header_bytes(STORED, &spelt, (0, 0), node.meta.times.modified, 0, words)?;
                    self.sink.put(&header)?;
                    self.directory(id)?;
                    self.sink.put(&END)?;
                    let len = self.sink.position() - start - HEADER_LEN as u64;
                    let header = header_bytes(
                        STORED,
                        &spelt,
                        (len, len),
                        node.meta.times.modified,
                        0,
                        words,
                    )?;
                    self.sink.patch(start, &header)?;
                    self.placed.push(Placed {
                        id,
                        data: None,
                        kept: Some(Kept {
                            record: start..self.sink.position(),
                            notes: Vec::new(),
                        }),
                        notes: None,
                    });
                }
                Body::File(data) => {
                    let words = words_of(&node.meta, Filetype::DATA)?;
                    let modified = node.meta.times.modified;
                    let data = self.file((id, node.content_gen), data, &spelt, modified, words)?;
                    self.placed.push(Placed {
                        id,
                        data: Some(data),
                        kept: Some(Kept {
                            record: start..self.sink.position(),
                            notes: Vec::new(),
                        }),
                        notes: None,
                    });
                }
                Body::Link(_) => return Err(DriverError::Unsupported),
            }
        }
        Ok(())
    }

    /// Write a file member: its coded bytes copied where they are unchanged,
    /// else compressed or stored afresh. Answers where its bytes now are.
    fn file(
        &mut self,
        node: (u64, u64),
        data: &Data,
        spelt: &[u8],
        modified: Time64,
        words: Words,
    ) -> Result<Data, DriverError> {
        let start = self.sink.position();
        let at = start + HEADER_LEN as u64;
        if let Data::Archived(archived) = data {
            if let (Some(kind), Check::Crc16(crc)) = (method_byte(archived.coding), archived.check)
            {
                let header = header_bytes(
                    kind,
                    spelt,
                    (archived.packed(), archived.len),
                    modified,
                    crc,
                    words,
                )?;
                self.sink.put(&header)?;
                self.sink
                    .copy(ARCHIVE_REGION, archived.at, archived.packed())?;
                return Ok(Data::Archived(Archived { at, ..*archived }));
            }
        }
        let len = data.len();
        self.sink.put(&header_bytes(
            COMPRESSED,
            spelt,
            (0, len),
            modified,
            0,
            words,
        )?)?;
        self.sink.put(&[WRITE_BITS])?;
        let (crc, packed) = self.compress(node, data)?;
        let (kind, coding) = if packed < len {
            let method = Method::Lzw {
                max_bits: WRITE_BITS,
                rle: false,
            };
            (
                COMPRESSED,
                Coding::Arc {
                    packed,
                    skip: 1,
                    method,
                },
            )
        } else {
            // Compressing would not shrink it.
            self.sink.rewind(at)?;
            self.content.copy_into(node, data, &mut *self.sink)?;
            (STORED, Coding::Stored)
        };
        let packed = if kind == STORED { len } else { packed };
        self.sink.patch(
            start,
            &header_bytes(kind, spelt, (packed, len), modified, crc, words)?,
        )?;
        Ok(Data::Archived(Archived {
            at,
            lead: Lead::None,
            len,
            coding,
            check: Check::Crc16(crc),
        }))
    }

    /// Compress `data`'s bytes onto the sink after its width byte, answering
    /// their CRC and the coded length, that byte counted.
    fn compress(&mut self, node: (u64, u64), data: &Data) -> Result<(u16, u64), DriverError> {
        let start = self.sink.position() - 1;
        compress_into(self.content, self.sink, self.work, node, data)
            .map(|crc| (crc, self.sink.position() - start))
    }

    /// Note that the subtree `id` heads, copied whole, moved from `from` to
    /// `to` in the archive.
    fn moved(&mut self, id: u64, to: u64, from: u64) -> Result<(), DriverError> {
        let node = self.tree.node(id)?;
        let shift = |at: u64| at - from + to;
        let data = match &node.body {
            Body::File(Data::Archived(archived)) => Some(Data::Archived(Archived {
                at: shift(archived.at),
                ..*archived
            })),
            _ => None,
        };
        let kept = node.kept.as_ref().map(|kept| Kept {
            record: shift(kept.record.start)..shift(kept.record.end),
            notes: Vec::new(),
        });
        self.placed.push(Placed {
            id,
            data,
            kept,
            notes: None,
        });
        if matches!(node.body, Body::Dir(_)) {
            for child in children(self.tree, id)? {
                self.moved(child, to, from)?;
            }
        }
        Ok(())
    }
}

/// Compress `data`'s bytes onto `sink` with dynamic LZW of the widest codes,
/// answering their CRC-16: what a Spark or `ArcFS` member written afresh holds.
pub(crate) fn compress_into<A: VolumeAccess>(
    content: &mut Content<'_, A>,
    sink: &mut Sink<'_, A>,
    work: &mut Vec<u32>,
    node: (u64, u64),
    data: &Data,
) -> Result<u16, DriverError> {
    let len = data.len();
    let need = arc::encoder_work(len);
    if work.len() < need {
        work.try_reserve(need - work.len())
            .map_err(|_| DriverError::OutOfMemory)?;
        work.resize(need, 0);
    }
    let mut encoder =
        arc::Encoder::new(&mut work[..need], WRITE_BITS).map_err(|_| DriverError::DeviceFault)?;
    let mut buf = tairix_util::fallible::filled(64 * 1024, 0u8).ok_or(DriverError::OutOfMemory)?;
    let mut crc = 0;
    let mut offset = 0;
    while offset < len {
        let got = content.read(node, data, offset, &mut buf)?;
        if got == 0 {
            return Err(DriverError::MediumError);
        }
        crc = tairix_crc16::update(crc, &buf[..got]);
        let mut out = SinkOutput::new(sink);
        let wrote = encoder.write(&buf[..got], &mut out);
        if let Some(fault) = out.fault() {
            return Err(fault);
        }
        wrote.map_err(|_| DriverError::DeviceFault)?;
        offset += got as u64;
    }
    let mut out = SinkOutput::new(sink);
    let finished = encoder.finish(&mut out);
    if let Some(fault) = out.fault() {
        return Err(fault);
    }
    finished.map_err(|_| DriverError::DeviceFault)?;
    Ok(crc)
}

#[cfg(test)]
#[path = "spark_tests.rs"]
mod tests;
