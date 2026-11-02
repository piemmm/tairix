//! zip (`plans/ARCHIVES.md` AR6; PKWARE `APPNOTE` 6.3): the central directory
//! read into the tree, members stored, deflated or bzip2'd, ZIP64, UTF-8 and
//! code page 437 names, Unix modes, the extended timestamp, and RISC OS's
//! load, exec and attributes in the `SparkFS` field.
//!
//! Every member is written with a fresh local header and central record, so
//! a member's record always says what its data is; its compressed bytes are
//! copied by the kernel whenever they are unchanged, and a member whose bytes
//! changed is deflated, or stored where deflating would not shrink it.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use tairix_abi::driver::block::{Block, BlockGeometry};
use tairix_abi::driver::filesystem::{NameMatching, NodeTimes, WriteRefusal};
use tairix_abi::fs::FS_SYMLINK_MAX;
use tairix_abi::time::{DosTime, Time64};
use tairix_abi::volume::ARCHIVE_REGION;
use tairix_abi::DriverError;
use tairix_bytedev::ByteDevice;
use tairix_compress::deflate::{Deflate, Flush};
use tairix_fsmeta::preset::acorn;
use tairix_fsprobe::archive::{
    ZipEnd, ZIP64_END_LEN, ZIP64_END_SIG, ZIP64_LOCATOR_SIG, ZIP_CENTRAL_SIG, ZIP_END_SIG,
};
use tairix_fsserve::VolumeAccess;

use crate::data::{Archived, Check, Coding, Cursors, Data, Heap, Lead};
use crate::format::{leaves_out, Content, Format, Placed, Read};
use crate::name::{as_is, spelled, Charset, Spelled};
use crate::region::{Regions, Sink};
use crate::tree::{Body, Meta, Node, Tree, MAX_MEMBERS};
use crate::wire::{le16, le32, le64, Out};

/// A local file header's signature.
const LOCAL_SIG: u32 = 0x0403_4b50;
/// A local header's fixed length.
const LOCAL_LEN: usize = 30;
/// A central record's fixed length.
const CENTRAL_LEN: usize = 46;

/// General-purpose flag: the member is encrypted.
const FLAG_ENCRYPTED: u16 = 1 << 0;
/// General-purpose flag: a data descriptor follows the member's bytes.
const FLAG_DESCRIPTOR: u16 = 1 << 3;
/// A data descriptor's signature.
const DESCRIPTOR_SIG: u32 = 0x0807_4B50;
/// General-purpose flag: strong encryption.
const FLAG_STRONG: u16 = 1 << 6;
/// General-purpose flag: the name and comment are UTF-8.
const FLAG_UTF8: u16 = 1 << 11;

/// Compression method: stored.
const METHOD_STORED: u16 = 0;
/// Compression method: deflate.
const METHOD_DEFLATE: u16 = 8;
/// Compression method: bzip2.
const METHOD_BZIP2: u16 = 12;

/// The ZIP64 extra field's id.
const EXTRA_ZIP64: u16 = 0x0001;
/// The extended timestamp's id.
const EXTRA_TIME: u16 = 0x5455;
/// The `SparkFS` RISC OS field's id, `AC`.
const EXTRA_ACORN: u16 = 0x4341;
/// The Info-ZIP Unicode path's id.
const EXTRA_UNICODE_PATH: u16 = 0x7075;
/// The `SparkFS` field's own signature.
const ACORN_SIGNATURE: &[u8; 4] = b"ARC0";

/// The host a central record says made it: Unix, whose external attributes
/// carry a mode.
const HOST_UNIX: u16 = 3;
/// The versions needed to extract a member: 2.0 for what this driver
/// writes, 4.5 for ZIP64 sizes, 4.6 for bzip2.
const VERSION: u16 = 20;
const VERSION_ZIP64: u16 = 45;
const VERSION_BZIP2: u16 = 46;

/// A Unix mode's file-type field and the types a member can be.
const S_IFMT: u32 = 0o170_000;
const S_IFDIR: u32 = 0o040_000;
const S_IFLNK: u32 = 0o120_000;
const S_IFREG: u32 = 0o100_000;

/// The MS-DOS attribute marking a directory.
const DOS_DIRECTORY: u32 = 0x10;

/// The largest value a narrow zip field holds; that value itself says the
/// ZIP64 field holds the real one.
const WIDE: u64 = 0xFFFF_FFFF;

/// What a zip keeps of the archive as a whole.
pub(crate) struct Zip {
    /// Bytes ahead of the first member — a self-extractor's program — which
    /// every next archive keeps as they are.
    stub: u64,
    /// The archive comment.
    comment: Vec<u8>,
}

/// Extra fields carrying a member's times, which a change of its times
/// leaves telling the old ones: NTFS, PKWARE Unix, and Info-ZIP's first Unix
/// field.
const TIMED_EXTRAS: [u16; 3] = [0x000a, 0x000d, 0x5855];

/// What a member's central record holds that the tree does not: its maker,
/// its attributes, the extra fields this driver neither reads nor writes,
/// and its comment.
struct Notes {
    made_by: u16,
    external: u32,
    internal: u16,
    extra: Vec<u8>,
    comment: Vec<u8>,
    /// The flags, method, stamp and version needed its own record gave:
    /// what a member this driver cannot read is written with again, its
    /// encryption checking the stamp where a descriptor follows it.
    flags: u16,
    method: u16,
    dos_time: u32,
    needed: u16,
    /// The modification time the record gives, which its extra fields
    /// carrying times agree with.
    mtime: Time64,
}

impl Notes {
    fn encode(&self) -> Result<Vec<u8>, DriverError> {
        let mut out = Out::with_capacity(34 + self.extra.len() + self.comment.len())?;
        out.u16(self.flags);
        out.u16(self.method);
        out.u32(self.dos_time);
        out.u16(self.needed);
        out.u64(self.mtime.secs().cast_unsigned());
        out.u32(self.mtime.subsec_nanos());
        out.u16(self.made_by);
        out.u32(self.external);
        out.u16(self.internal);
        out.u16(u16_len(self.extra.len())?);
        out.bytes(&self.extra);
        out.u16(u16_len(self.comment.len())?);
        out.bytes(&self.comment);
        out.finish()
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        let (flags, method, dos_time) = (le16(bytes, 0)?, le16(bytes, 2)?, le32(bytes, 4)?);
        let needed = le16(bytes, 8)?;
        let mtime = Time64::new(le64(bytes, 10)?.cast_signed(), le32(bytes, 18)?).ok()?;
        let bytes = bytes.get(22..)?;
        let made_by = le16(bytes, 0)?;
        let external = le32(bytes, 2)?;
        let internal = le16(bytes, 6)?;
        let extra_len = usize::from(le16(bytes, 8)?);
        let extra = bytes.get(10..10 + extra_len)?.to_vec();
        let at = 10 + extra_len;
        let comment_len = usize::from(le16(bytes, at)?);
        let comment = bytes.get(at + 2..at + 2 + comment_len)?.to_vec();
        Some(Self {
            made_by,
            external,
            internal,
            extra,
            comment,
            flags,
            method,
            dos_time,
            needed,
            mtime,
        })
    }

    /// The notes a member made since the archive was read starts with: made
    /// on Unix, its mode in its external attributes.
    fn fresh(node: &Node) -> Self {
        let (kind, dos) = kind_bits(&node.body);
        Self {
            made_by: HOST_UNIX << 8 | VERSION,
            external: (kind | (node.meta.mode & 0o7777)) << 16 | dos,
            internal: 0,
            extra: Vec::new(),
            comment: Vec::new(),
            flags: FLAG_UTF8,
            method: METHOD_STORED,
            dos_time: 0,
            needed: VERSION,
            mtime: node.meta.times.modified,
        }
    }

    /// The permission bits the attributes give, or those a member whose
    /// host records none is served with.
    fn mode(&self, directory: bool) -> u32 {
        unix_mode(self.made_by, self.external)
            .map_or(if directory { 0o755 } else { 0o644 }, |mode| mode & 0o7777)
    }

    /// What they say of `node` as it now is: its mode where it is no longer
    /// the one they give, and none of the extra fields carrying times its
    /// own no longer are.
    fn reconciled(mut self, node: &Node) -> Result<Self, DriverError> {
        let mode = node.meta.mode & 0o7777;
        if mode != self.mode(matches!(node.body, Body::Dir(_))) {
            let (kind, dos) = kind_bits(&node.body);
            self.made_by = HOST_UNIX << 8 | (self.made_by & 0xFF);
            self.external = (kind | mode) << 16 | (self.external & 0xFFFF & !DOS_DIRECTORY) | dos;
        }
        if node.meta.times.modified != self.mtime {
            let mut kept = Out::with_capacity(self.extra.len())?;
            for (id, data) in extra_fields(&self.extra) {
                if !TIMED_EXTRAS.contains(&id) {
                    kept.u16(id);
                    kept.u16(u16_len(data.len())?);
                    kept.bytes(data);
                }
            }
            self.extra = kept.finish()?;
            self.mtime = node.meta.times.modified;
        }
        Ok(self)
    }
}

/// The type bits a Unix mode gives `body`, and the MS-DOS attribute it has.
const fn kind_bits(body: &Body) -> (u32, u32) {
    match body {
        Body::Dir(_) => (S_IFDIR, DOS_DIRECTORY),
        Body::Link(_) => (S_IFLNK, 0),
        Body::File(_) => (S_IFREG, 0),
    }
}

/// The Unix mode external attributes carry, where their host is Unix and
/// they carry one.
const fn unix_mode(made_by: u16, external: u32) -> Option<u32> {
    if made_by >> 8 == HOST_UNIX && external >> 16 != 0 {
        Some(external >> 16)
    } else {
        None
    }
}

/// `len` as a 16-bit length field.
fn u16_len(len: usize) -> Result<u16, DriverError> {
    u16::try_from(len).map_err(|_| DriverError::LengthOutOfRange)
}

/// The archive, as the block device the probe's readers read.
struct ArchiveBlock<'r, A> {
    regions: &'r Regions<A>,
}

impl<A: VolumeAccess> Block for ArchiveBlock<'_, A> {
    fn geometry(&self) -> Result<BlockGeometry, DriverError> {
        Ok(BlockGeometry {
            block_size: 512,
            block_count: self.regions.len().div_ceil(512),
        })
    }

    fn read_blocks(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), DriverError> {
        let at = lba.checked_mul(512).ok_or(DriverError::LengthOutOfRange)?;
        let got = self.regions.read(ARCHIVE_REGION, at, buf)?;
        buf[got..].fill(0);
        Ok(())
    }

    fn write_blocks(&mut self, _lba: u64, _buf: &[u8]) -> Result<(), DriverError> {
        Err(DriverError::Unsupported)
    }

    fn flush(&mut self) -> Result<(), DriverError> {
        Ok(())
    }
}

/// Read the zip archive `regions` holds.
///
/// # Errors
///
/// [`DriverError::BadMagic`] for no end record or a central directory out of
/// shape; [`DriverError::Unsupported`] for an archive split across disks or
/// holding more members than this driver opens.
pub(crate) fn read<A: VolumeAccess>(regions: &Regions<A>) -> Result<Read, DriverError> {
    let len = regions.len();
    let mut dev = ByteDevice::new(ArchiveBlock { regions })?;
    let end = ZipEnd::find(&mut dev, len)?.ok_or(DriverError::BadMagic)?;
    if end.split || end.entries > MAX_MEMBERS as u64 {
        return Err(DriverError::Unsupported);
    }
    let directory = regions.archive_bytes(
        end.directory,
        usize::try_from(end.directory_len).map_err(|_| DriverError::Unsupported)?,
    )?;
    let comment = regions.archive_bytes(
        end.comment.start,
        usize::try_from(end.comment.end - end.comment.start).map_err(|_| DriverError::BadMagic)?,
    )?;
    let mut tree = Tree::new(NameMatching::Exact, directory_meta());
    let mut cursors = Cursors::default();
    let mut at = 0;
    let mut refusal = None;
    let mut records = 0u64;
    let mut stub = end.directory;
    // Read by the directory's own length, which a count may disagree with.
    while at < directory.len() {
        let Ok(record) = Record::parse(&directory, at, end.base) else {
            // What follows a record out of shape cannot be believed, and
            // writing the archive back would lose it.
            refusal = Some(WriteRefusal::Damaged);
            break;
        };
        at = record.next;
        records += 1;
        if records > MAX_MEMBERS as u64 {
            return Err(DriverError::Unsupported);
        }
        stub = stub.min(record.local);
        // A member whose path leaves the archive, that another member's path
        // already holds, whose data lies past the archive's end, or whose
        // link cannot be read is left out; writing the archive back would
        // lose it.
        if let Err(err) = admit(&mut tree, &record, regions, &mut cursors) {
            if !leaves_out(err) {
                return Err(err);
            }
            refusal.get_or_insert(WriteRefusal::UnsupportedFeature);
        }
    }
    // Sixteen bits count a directory past 65535 records modulo 65536.
    let counted = if end.zip64 { records } else { records & 0xFFFF };
    if counted != end.entries {
        refusal = Some(WriteRefusal::Damaged);
    }
    Ok(Read {
        tree,
        format: Format::Zip(Zip { stub, comment }),
        refusal,
        stream: None,
        rar: None,
    })
}

/// One central record, read.
struct Record {
    /// Where the next record starts in the directory.
    next: usize,
    flags: u16,
    method: u16,
    crc: u32,
    packed: u64,
    len: u64,
    /// Where its local header is in the file.
    local: u64,
    name: Vec<u8>,
    notes: Notes,
    directory: bool,
    acorn: Option<acorn::Object>,
}

impl Record {
    /// The central record at `at` of `directory`, its offsets counted after
    /// `base`.
    fn parse(directory: &[u8], at: usize, base: u64) -> Result<Self, DriverError> {
        let fixed = directory
            .get(at..at + CENTRAL_LEN)
            .ok_or(DriverError::BadMagic)?;
        if le32(fixed, 0) != Some(ZIP_CENTRAL_SIG) {
            return Err(DriverError::BadMagic);
        }
        let field16 = |offset| le16(fixed, offset).unwrap_or(0);
        let field32 = |offset| le32(fixed, offset).unwrap_or(0);
        let made_by = field16(4);
        let needed = field16(6);
        let flags = field16(8);
        let method = field16(10);
        let dos_time = u32::from(field16(12)) | u32::from(field16(14)) << 16;
        let crc = field32(16);
        let name_len = usize::from(field16(28));
        let extra_len = usize::from(field16(30));
        let comment_len = usize::from(field16(32));
        let internal = field16(36);
        let external = field32(38);
        let body = at + CENTRAL_LEN;
        let end = body + name_len + extra_len + comment_len;
        let raw_name = directory
            .get(body..body + name_len)
            .ok_or(DriverError::BadMagic)?;
        let extra = directory
            .get(body + name_len..body + name_len + extra_len)
            .ok_or(DriverError::BadMagic)?;
        let comment = directory
            .get(body + name_len + extra_len..end)
            .ok_or(DriverError::BadMagic)?;
        let mut len = u64::from(field32(24));
        let mut packed = u64::from(field32(20));
        let mut local = u64::from(field32(42));
        let mut mtime = None;
        let mut acorn_fields = None;
        let mut unicode = None;
        let mut kept_extra = Out::with_capacity(extra.len())?;
        for (id, data) in extra_fields(extra) {
            match id {
                EXTRA_ZIP64 => {
                    // Only the fields the fixed record could not hold are
                    // here, in this order.
                    let mut cursor = 0;
                    for field in [&mut len, &mut packed, &mut local] {
                        if *field == WIDE {
                            *field = le64(data, cursor).ok_or(DriverError::BadMagic)?;
                            cursor += 8;
                        }
                    }
                }
                EXTRA_TIME => {
                    // Flags, then the modification time where bit 0 says.
                    if data.first().is_some_and(|flags| flags & 1 != 0) {
                        mtime = le32(data, 1).map(|word| ut_time(word, dos_time));
                    }
                }
                EXTRA_ACORN => acorn_fields = acorn_extra(data),
                EXTRA_UNICODE_PATH => unicode = unicode_path(data, raw_name),
                _ => {
                    kept_extra.u16(id);
                    kept_extra.u16(u16_len(data.len())?);
                    kept_extra.bytes(data);
                }
            }
        }
        let name = unicode.unwrap_or_else(|| record_name(raw_name, flags));
        let local = base.checked_add(local).ok_or(DriverError::BadMagic)?;
        let directory = name.ends_with(b"/")
            || match unix_mode(made_by, external) {
                Some(mode) => mode & S_IFMT == S_IFDIR,
                None => external & DOS_DIRECTORY != 0,
            };
        let mtime = mtime.unwrap_or_else(|| dos_to_time(dos_time));
        Ok(Self {
            next: end,
            flags,
            method,
            crc,
            packed,
            len,
            local,
            name,
            notes: Notes {
                made_by,
                external,
                internal,
                extra: kept_extra.finish()?,
                comment: comment.to_vec(),
                flags,
                method,
                dos_time,
                needed,
                mtime,
            },
            directory,
            acorn: acorn_fields,
        })
    }

    fn is_link(&self) -> bool {
        unix_mode(self.notes.made_by, self.notes.external)
            .is_some_and(|mode| mode & S_IFMT == S_IFLNK)
    }

    fn coding(&self) -> Coding {
        let refused = |why| Coding::Refused {
            why,
            packed: self.packed,
        };
        if self.flags & (FLAG_ENCRYPTED | FLAG_STRONG) != 0 {
            return refused(DriverError::PermissionDenied);
        }
        match self.method {
            METHOD_STORED if self.packed == self.len => Coding::Stored,
            METHOD_STORED => refused(DriverError::MediumError),
            METHOD_DEFLATE => Coding::Deflate {
                packed: self.packed,
            },
            METHOD_BZIP2 => Coding::Bzip2 {
                packed: self.packed,
            },
            _ => refused(DriverError::Unsupported),
        }
    }

    fn meta(&self) -> Meta {
        Meta {
            mode: self.notes.mode(self.directory),
            owner: None,
            times: NodeTimes {
                created: Time64::UNIX_EPOCH,
                modified: self.notes.mtime,
                accessed: Time64::UNIX_EPOCH,
                changed: Time64::UNIX_EPOCH,
            },
            acorn: self.acorn,
        }
    }
}

/// Put the member `record` reads into `tree`, a link's target read from the
/// archive.
fn admit<A: VolumeAccess>(
    tree: &mut Tree,
    record: &Record,
    regions: &Regions<A>,
    cursors: &mut Cursors,
) -> Result<(), DriverError> {
    let Spelled::Within(mut parts) = spelled(&record.name, b'/', as_is) else {
        return Err(DriverError::OutOfRange);
    };
    let leaf = parts.pop().ok_or(DriverError::OutOfRange)?;
    let meta = record.meta();
    let dir = tree.directory(&parts, &directory_meta())?;
    let notes = record.notes.encode()?;
    if record.directory {
        let made = match tree.lookup(dir, &leaf) {
            // A directory a member's path made first takes the record.
            Ok(made) if matches!(tree.node(made)?.body, Body::Dir(_)) => {
                tree.node_mut(made)?.meta = meta;
                made
            }
            Ok(_) => return Err(DriverError::AlreadyExists),
            Err(_) => tree.add(dir, &leaf, Body::Dir(BTreeMap::new()), meta, None)?,
        };
        tree.node_mut(made)?.notes = notes;
        return Ok(());
    }
    if record
        .local
        .checked_add(record.packed)
        .is_none_or(|end| end > regions.len())
    {
        return Err(DriverError::OutOfRange);
    }
    let archived = Archived {
        at: record.local,
        lead: Lead::ZipLocal,
        len: record.len,
        coding: record.coding(),
        check: Check::Crc32(record.crc),
    };
    let body = if record.is_link() {
        // A link whose target cannot be read — encrypted, damaged, or out of
        // shape — is left out rather than refusing the archive.
        let target = match link_target(regions, cursors, archived) {
            Ok(target) => target,
            Err(DriverError::OutOfMemory) => return Err(DriverError::OutOfMemory),
            Err(_) => return Err(DriverError::Unsupported),
        };
        Body::Link(target)
    } else {
        Body::File(Data::Archived(archived))
    };
    let id = tree.add(dir, &leaf, body, meta, None)?;
    tree.node_mut(id)?.notes = notes;
    Ok(())
}

/// The target a link member's bytes hold.
fn link_target<A: VolumeAccess>(
    regions: &Regions<A>,
    cursors: &mut Cursors,
    mut archived: Archived,
) -> Result<Vec<u8>, DriverError> {
    let len = usize::try_from(archived.len)
        .ok()
        .filter(|&len| len > 0 && len <= FS_SYMLINK_MAX)
        .ok_or(DriverError::OutOfRange)?;
    archived = located(regions, archived)?;
    let mut target = tairix_util::fallible::filled(len, 0u8).ok_or(DriverError::OutOfMemory)?;
    // A link is read once, never by a cursor a file's read could resume.
    let got = cursors.read(regions, (0, 0), &archived, 0, &mut target)?;
    cursors.forget(0);
    if got != len || target.contains(&0) {
        return Err(DriverError::OutOfRange);
    }
    Ok(target)
}

/// `archived`, placed where its bytes start.
pub(crate) fn located<A: VolumeAccess>(
    regions: &Regions<A>,
    archived: Archived,
) -> Result<Archived, DriverError> {
    match archived.lead {
        Lead::None => Ok(archived),
        Lead::ZipLocal => {
            let mut header = [0u8; LOCAL_LEN];
            regions.read_archive(archived.at, &mut header)?;
            if le32(&header, 0) != Some(LOCAL_SIG) {
                return Err(DriverError::MediumError);
            }
            let name = u64::from(le16(&header, 26).unwrap_or(0));
            let extra = u64::from(le16(&header, 28).unwrap_or(0));
            let at = archived
                .at
                .checked_add(LOCAL_LEN as u64 + name + extra)
                .ok_or(DriverError::MediumError)?;
            // Its data lies within the archive, as its record says.
            if at
                .checked_add(archived.packed())
                .is_none_or(|end| end > regions.len())
            {
                return Err(DriverError::MediumError);
            }
            Ok(Archived {
                at,
                lead: Lead::None,
                ..archived
            })
        }
    }
}

/// The record a directory the archive does not name itself is given.
pub(crate) fn directory_meta() -> Meta {
    Meta {
        mode: 0o755,
        owner: None,
        times: NodeTimes {
            created: Time64::UNIX_EPOCH,
            modified: Time64::UNIX_EPOCH,
            accessed: Time64::UNIX_EPOCH,
            changed: Time64::UNIX_EPOCH,
        },
        acorn: None,
    }
}

/// The `(id, data)` extra fields of `extra`, as far as they are whole.
fn extra_fields(extra: &[u8]) -> impl Iterator<Item = (u16, &[u8])> {
    let mut at = 0;
    core::iter::from_fn(move || {
        let id = le16(extra, at)?;
        let len = usize::from(le16(extra, at + 2)?);
        let data = extra.get(at + 4..at + 4 + len)?;
        at += 4 + len;
        Some((id, data))
    })
}

/// The name `raw` spells in the character set a record's `flags` give it;
/// none for one it misspells, which leaves its member out.
fn record_name(raw: &[u8], flags: u16) -> Vec<u8> {
    let charset = if flags & FLAG_UTF8 != 0 {
        Charset::Utf8
    } else {
        Charset::Cp437
    };
    charset.decode(raw).unwrap_or_default()
}

/// The time an extended timestamp's word gives: signed, as the field's
/// definition says, but unsigned where the MS-DOS stamp beside it, which
/// spells no year before 1980, puts it past 2038, as a writer whose clock
/// ran past the signed range wrote it.
fn ut_time(word: u32, dos_time: u32) -> Time64 {
    let signed = i64::from(word.cast_signed());
    let dos_year = 1980 + (dos_time >> 25);
    Time64::from_secs(if signed < 0 && dos_year >= 2038 {
        i64::from(word)
    } else {
        signed
    })
}

/// The extended timestamp's word for `time`, where one holds it: negative
/// times signed, those past 2038 unsigned, which the MS-DOS stamp written
/// beside it tells apart.
fn ut_word(time: Time64) -> Option<u32> {
    u32::try_from(time.secs())
        .ok()
        .or_else(|| i32::try_from(time.secs()).ok().map(i32::cast_unsigned))
}

/// The `SparkFS` field's RISC OS words: its signature, then load, exec and
/// attributes.
fn acorn_extra(data: &[u8]) -> Option<acorn::Object> {
    if data.get(..4)? != ACORN_SIGNATURE {
        return None;
    }
    Some(acorn::Object {
        load: le32(data, 4)?,
        exec: le32(data, 8)?,
        attr: acorn::attr_from_fileswitch(data.get(12).copied()?),
    })
}

/// The Info-ZIP Unicode path, where its checksum of the record's own name
/// holds.
fn unicode_path(data: &[u8], raw_name: &[u8]) -> Option<Vec<u8>> {
    if *data.first()? != 1 || le32(data, 1)? != tairix_crc32::checksum(raw_name) {
        return None;
    }
    Charset::Utf8.decode(data.get(5..)?)
}

/// A zip record's MS-DOS date and time, the date in the high half: read as
/// UTC, the format recording no zone.
fn dos_to_time(dos: u32) -> Time64 {
    let [t0, t1, d0, d1] = dos.to_le_bytes();
    let stamp = DosTime {
        date: u16::from_le_bytes([d0, d1]),
        time: u16::from_le_bytes([t0, t1]),
    };
    stamp.to_time64().unwrap_or(Time64::UNIX_EPOCH)
}

/// `time` as a zip record's MS-DOS date and time, held to the years the
/// field spells; the extended timestamp beside it keeps the exact time.
fn time_to_dos(time: Time64) -> u32 {
    let held = time
        .secs()
        .clamp(DosTime::EARLIEST.secs(), DosTime::LATEST.secs());
    DosTime::from_time64(Time64::from_secs(held)).map_or(0, |stamp| {
        u32::from(stamp.date) << 16 | u32::from(stamp.time)
    })
}

/// A member of the next archive as its records will say.
struct Central {
    flags: u16,
    method: u16,
    /// The version its method needs to extract it, short of what ZIP64
    /// sizes need.
    needed: u16,
    dos_time: u32,
    crc: u32,
    packed: u64,
    len: u64,
    local: u64,
    name: Vec<u8>,
    notes: Notes,
    mtime: Time64,
    acorn: Option<acorn::Object>,
}

impl Central {
    /// The version needed to extract it, ZIP64's at least where `wide`.
    fn version_needed(&self, wide: bool) -> u16 {
        if wide {
            self.needed.max(VERSION_ZIP64)
        } else {
            self.needed
        }
    }

    /// The extra fields this driver writes, the ZIP64 one holding `zip64`.
    fn extra(&self, zip64: &[u64]) -> Result<Vec<u8>, DriverError> {
        let mut out = Out::with_capacity(64 + self.notes.extra.len())?;
        if !zip64.is_empty() {
            out.u16(EXTRA_ZIP64);
            out.u16(u16_len(zip64.len() * 8)?);
            for &value in zip64 {
                out.u64(value);
            }
        }
        if let Some(word) = ut_word(self.mtime) {
            out.u16(EXTRA_TIME);
            out.u16(5);
            out.bytes(&[1]);
            out.u32(word);
        }
        if let Some(object) = self.acorn {
            out.u16(EXTRA_ACORN);
            out.u16(20);
            out.bytes(ACORN_SIGNATURE);
            out.u32(object.load);
            out.u32(object.exec);
            out.u32(u32::from(acorn::attr_to_fileswitch(object.attr)));
            out.u32(0);
        }
        out.bytes(&self.notes.extra);
        u16_len(out.len())?;
        out.finish()
    }

    /// The local header, its sizes in a ZIP64 field where `wide`.
    fn local_header(&self, wide: bool) -> Result<Vec<u8>, DriverError> {
        let zip64: &[u64] = if wide { &[self.len, self.packed] } else { &[] };
        let extra = self.extra(zip64)?;
        let narrow = |value: u64| {
            if wide {
                u32::MAX
            } else {
                u32::try_from(value).unwrap_or(u32::MAX)
            }
        };
        let mut out = Out::with_capacity(LOCAL_LEN + self.name.len() + extra.len())?;
        out.u32(LOCAL_SIG);
        out.u16(self.version_needed(wide));
        out.u16(self.flags);
        out.u16(self.method);
        out.u32(self.dos_time);
        out.u32(self.crc);
        out.u32(narrow(self.packed));
        out.u32(narrow(self.len));
        out.u16(u16_len(self.name.len())?);
        out.u16(u16_len(extra.len())?);
        out.bytes(&self.name);
        out.bytes(&extra);
        out.finish()
    }

    /// The data descriptor that follows the bytes of a member whose flags
    /// say one does.
    fn descriptor(&self, wide: bool) -> Result<Vec<u8>, DriverError> {
        let mut out = Out::with_capacity(24)?;
        out.u32(DESCRIPTOR_SIG);
        out.u32(self.crc);
        if wide {
            out.u64(self.packed);
            out.u64(self.len);
        } else {
            out.u32(u32::try_from(self.packed).map_err(|_| DriverError::LengthOutOfRange)?);
            out.u32(u32::try_from(self.len).map_err(|_| DriverError::LengthOutOfRange)?);
        }
        out.finish()
    }

    /// The central record.
    fn record(&self) -> Result<Vec<u8>, DriverError> {
        let mut zip64 = Vec::new();
        let mut narrow = |value: u64| match u32::try_from(value) {
            Ok(narrow) if value < WIDE => narrow,
            _ => {
                zip64.push(value);
                u32::MAX
            }
        };
        let len = narrow(self.len);
        let packed = narrow(self.packed);
        let local = narrow(self.local);
        let extra = self.extra(&zip64)?;
        let needed = self.version_needed(!zip64.is_empty());
        let mut out = Out::with_capacity(
            CENTRAL_LEN + self.name.len() + extra.len() + self.notes.comment.len(),
        )?;
        out.u32(ZIP_CENTRAL_SIG);
        out.u16(self.notes.made_by);
        out.u16(needed);
        out.u16(self.flags);
        out.u16(self.method);
        out.u32(self.dos_time);
        out.u32(self.crc);
        out.u32(packed);
        out.u32(len);
        out.u16(u16_len(self.name.len())?);
        out.u16(u16_len(extra.len())?);
        out.u16(u16_len(self.notes.comment.len())?);
        out.u16(0);
        out.u16(self.notes.internal);
        out.u32(self.notes.external);
        out.u32(local);
        out.bytes(&self.name);
        out.bytes(&extra);
        out.bytes(&self.notes.comment);
        out.finish()
    }
}

/// Whether a member of `len` bytes may need ZIP64 sizes: its stored length,
/// or a deflated one, which can run a little past it.
const fn wide_sizes(len: u64) -> bool {
    len >= WIDE - (WIDE >> 6)
}

impl Zip {
    /// Write the next archive from `tree` into `sink`.
    pub(crate) fn write<A: VolumeAccess>(
        &mut self,
        tree: &Tree,
        content: &mut Content<'_, A>,
        sink: &mut Sink<'_, A>,
    ) -> Result<Vec<Placed>, DriverError> {
        sink.copy(ARCHIVE_REGION, 0, self.stub)?;
        let order = tree.in_order()?;
        let mut placed = Vec::new();
        placed
            .try_reserve_exact(order.len())
            .map_err(|_| DriverError::OutOfMemory)?;
        let mut centrals = Vec::new();
        centrals
            .try_reserve_exact(order.len())
            .map_err(|_| DriverError::OutOfMemory)?;
        for id in order {
            let node = tree.node(id)?;
            let mut name = join(&tree.path(id)?)?;
            if matches!(node.body, Body::Dir(_)) {
                name.push(b'/');
            }
            let notes = match Notes::decode(&node.notes) {
                Some(notes) => notes.reconciled(node)?,
                None => Notes::fresh(node),
            };
            let mut central = Central {
                flags: FLAG_UTF8,
                method: METHOD_STORED,
                needed: VERSION,
                dos_time: time_to_dos(node.meta.times.modified),
                crc: 0,
                packed: 0,
                len: 0,
                local: sink.position(),
                name,
                notes,
                mtime: node.meta.times.modified,
                acorn: node.meta.acorn,
            };
            let data = match &node.body {
                Body::Dir(_) => {
                    sink.put(&central.local_header(false)?)?;
                    None
                }
                Body::Link(target) => {
                    central.crc = tairix_crc32::checksum(target);
                    central.len = target.len() as u64;
                    central.packed = central.len;
                    sink.put(&central.local_header(false)?)?;
                    sink.put(target)?;
                    None
                }
                Body::File(data) => Some(Self::put_file(
                    content,
                    (id, node.content_gen),
                    data,
                    &mut central,
                    sink,
                )?),
            };
            placed.push(Placed {
                id,
                data,
                kept: None,
                notes: Some(central.notes.encode()?),
            });
            centrals.push(central);
        }
        self.put_directory(&centrals, sink)?;
        Ok(placed)
    }

    /// Write a file member's header and bytes, answering where its bytes now
    /// are: unchanged ones keep their coding and are copied by the kernel.
    fn put_file<A: VolumeAccess>(
        content: &mut Content<'_, A>,
        node: (u64, u64),
        data: &Data,
        central: &mut Central,
        sink: &mut Sink<'_, A>,
    ) -> Result<Data, DriverError> {
        if let Data::Archived(archived) = data {
            let method = match archived.coding {
                Coding::Stored => Some((METHOD_STORED, VERSION)),
                Coding::Deflate { .. } => Some((METHOD_DEFLATE, VERSION)),
                Coding::Bzip2 { .. } => Some((METHOD_BZIP2, VERSION_BZIP2)),
                // A member this driver cannot read keeps its record's own
                // method, flags, stamp and version over the same coded bytes.
                Coding::Refused { .. } => {
                    // Its name is written as the tree spells it, in UTF-8.
                    central.flags = central.notes.flags | FLAG_UTF8;
                    central.dos_time = central.notes.dos_time;
                    Some((central.notes.method, central.notes.needed))
                }
                Coding::Arc { .. } | Coding::Stream | Coding::Rar { .. } => None,
            };
            if let (Some((method, needed)), Check::Crc32(crc)) = (method, archived.check) {
                let source = located(content.regions, *archived)?;
                central.method = method;
                central.needed = needed;
                central.crc = crc;
                central.len = archived.len;
                central.packed = archived.packed();
                let wide = wide_sizes(central.len.max(central.packed));
                sink.put(&central.local_header(wide)?)?;
                let at = sink.position();
                sink.copy(ARCHIVE_REGION, source.at, central.packed)?;
                if central.flags & FLAG_DESCRIPTOR != 0 {
                    sink.put(&central.descriptor(wide)?)?;
                }
                return Ok(Data::Archived(Archived { at, ..source }));
            }
        }
        Self::deflate_file(content, node, data, central, sink)
    }

    /// Deflate a member's bytes into the next archive, or store them where
    /// deflating would not shrink them, patching the local header once its
    /// sizes and CRC are known.
    fn deflate_file<A: VolumeAccess>(
        content: &mut Content<'_, A>,
        node: (u64, u64),
        data: &Data,
        central: &mut Central,
        sink: &mut Sink<'_, A>,
    ) -> Result<Data, DriverError> {
        const CHUNK: usize = 32 * 1024;
        let len = data.len();
        let wide = wide_sizes(len);
        central.len = len;
        central.packed = len;
        central.method = METHOD_DEFLATE;
        let header_at = sink.position();
        sink.put(&central.local_header(wide)?)?;
        let start = sink.position();
        let mut heap = Heap::new(Deflate::new())?;
        let encoder = heap.get()?;
        let mut input =
            tairix_util::fallible::filled(CHUNK, 0u8).ok_or(DriverError::OutOfMemory)?;
        let mut output = tairix_util::fallible::filled(encoder.bound(CHUNK), 0u8)
            .ok_or(DriverError::OutOfMemory)?;
        let mut crc = tairix_crc32::Crc32::new();
        let mut offset = 0;
        loop {
            let got = if offset < len {
                content.read(node, data, offset, &mut input)?
            } else {
                0
            };
            if offset < len && got == 0 {
                return Err(DriverError::MediumError);
            }
            crc.update(&input[..got]);
            offset += got as u64;
            let flush = if offset >= len {
                Flush::Finish
            } else {
                Flush::None
            };
            let made = encoder
                .deflate(&input[..got], &mut output, flush)
                .map_err(|_| DriverError::DeviceFault)?;
            sink.put(&output[..made])?;
            if flush == Flush::Finish {
                break;
            }
        }
        central.crc = crc.finish();
        central.packed = sink.position() - start;
        let coding = if central.packed < len {
            Coding::Deflate {
                packed: central.packed,
            }
        } else {
            // Deflating did not shrink it: stored instead.
            sink.rewind(header_at)?;
            central.method = METHOD_STORED;
            central.packed = len;
            sink.put(&central.local_header(wide)?)?;
            content.copy_into(node, data, sink)?;
            Coding::Stored
        };
        sink.patch(header_at, &central.local_header(wide)?)?;
        Ok(Data::Archived(Archived {
            at: sink.position() - central.packed,
            lead: Lead::None,
            len,
            coding,
            check: Check::Crc32(central.crc),
        }))
    }

    /// The central directory and the end records.
    fn put_directory<A: VolumeAccess>(
        &self,
        centrals: &[Central],
        sink: &mut Sink<'_, A>,
    ) -> Result<(), DriverError> {
        let start = sink.position();
        for central in centrals {
            sink.put(&central.record()?)?;
        }
        let size = sink.position() - start;
        let entries = centrals.len() as u64;
        if entries >= 0xFFFF || size >= WIDE || start >= WIDE {
            let zip64_at = sink.position();
            let mut record = Out::with_capacity(ZIP64_END_LEN + 20)?;
            record.u32(ZIP64_END_SIG);
            record.u64((ZIP64_END_LEN - 12) as u64);
            record.u16(HOST_UNIX << 8 | VERSION_ZIP64);
            record.u16(VERSION_ZIP64);
            record.u32(0);
            record.u32(0);
            record.u64(entries);
            record.u64(entries);
            record.u64(size);
            record.u64(start);
            record.u32(ZIP64_LOCATOR_SIG);
            record.u32(0);
            record.u64(zip64_at);
            record.u32(1);
            sink.put(&record.finish()?)?;
        }
        let mut end = Out::with_capacity(22 + self.comment.len())?;
        end.u32(ZIP_END_SIG);
        end.u16(0);
        end.u16(0);
        end.u16(u16::try_from(entries).unwrap_or(u16::MAX));
        end.u16(u16::try_from(entries).unwrap_or(u16::MAX));
        end.u32(u32::try_from(size).unwrap_or(u32::MAX));
        end.u32(u32::try_from(start).unwrap_or(u32::MAX));
        end.u16(u16_len(self.comment.len())?);
        end.bytes(&self.comment);
        sink.put(&end.finish()?)
    }
}

/// `parts` joined with `/`.
fn join(parts: &[&[u8]]) -> Result<Vec<u8>, DriverError> {
    let len = parts.iter().map(|part| part.len() + 1).sum::<usize>();
    let mut out = Vec::new();
    out.try_reserve_exact(len)
        .map_err(|_| DriverError::OutOfMemory)?;
    for (at, part) in parts.iter().enumerate() {
        if at > 0 {
            out.push(b'/');
        }
        out.extend_from_slice(part);
    }
    u16_len(out.len())?;
    Ok(out)
}

#[cfg(test)]
#[path = "zip_tests.rs"]
mod tests;
