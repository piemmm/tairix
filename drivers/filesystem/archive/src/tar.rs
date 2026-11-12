//! tar (`plans/ARCHIVES.md` AR5; POSIX.1-2001 `pax`): v7, ustar, pax and GNU
//! headers read — long names and links, extended records, times to the
//! nanosecond, owners — and members written as ustar, with a pax record only
//! where a field needs one. RISC OS's fields travel as the
//! `SCHILY.xattr.acorn.*` pax records. A tar may be plain, or the decoded
//! bytes of one gzip or bzip2 stream.
//!
//! A member nothing has changed is copied whole, headers and all — by the
//! kernel in a plain tar — and one whose bytes are unchanged gets new headers
//! over the same bytes, keeping the pax records this driver does not read and
//! its owner's names while the owner is unchanged.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::ops::Range;

use tairix_abi::driver::filesystem::{NameMatching, NodeTimes, WriteRefusal};
use tairix_abi::fs::FS_SYMLINK_MAX;
use tairix_abi::time::Time64;
use tairix_abi::volume::{ArchiveName, FormatName, ARCHIVE_REGION};
use tairix_abi::DriverError;
use tairix_fsmeta::preset::acorn;
use tairix_fsprobe::archive::{tar_header_holds, tar_number, TAR_BLOCK};
use tairix_fsserve::VolumeAccess;

use crate::data::{Archived, Check, Coding, Data, Lead};
use crate::format::{leaves_out, put_zeros, Content, Format, Out, Placed, Read};
use crate::name::{as_is, spelled, Charset, Spelled};
use crate::region::Regions;
use crate::stream::{Compression, Packing, Stream};
use crate::tree::{Body, Kept, Meta, Recorded, Tree, MAX_MEMBERS, ROOT};
use crate::wire::{le32, le64};
use crate::zip::directory_meta;

/// The longest extended record a member may carry: a fixed bound on what a
/// stranger's header can make the driver hold.
const PAX_MAX: u64 = 1 << 20;

/// The longest GNU long name or link a member may carry.
const LONG_NAME_MAX: u64 = 64 * 1024;

/// The record a tar written here is padded to: twenty blocks, as tar writes
/// one by default.
const RECORD: u64 = 20 * TAR_BLOCK as u64;

/// How far a header's scan reads ahead.
const SCAN_LEN: usize = 64 * 1024;

/// The most zeros after its end marker a tar written back may have held:
/// more than any record a tar writer pads to, so what lies past it is read
/// as something a rewrite would lose.
const TRAILER_MAX: u64 = 1 << 20;

/// The most global headers saying nothing of any member a tar written back
/// keeps: a fixed bound on what a stranger's archive makes the driver hold.
const GLOBALS_MAX: usize = 64;

/// The prefix of the pax records RISC OS's fields travel as.
const ACORN_XATTR: &[u8] = b"SCHILY.xattr.";

/// What a tar keeps of the archive as a whole: how its one stream is
/// written, where it is one, its global headers that say nothing of any
/// member, each copied ahead of every member, and whether it records its
/// root, as `./`.
pub(crate) struct Tar {
    packing: Option<Packing>,
    globals: Vec<Range<u64>>,
    root: bool,
}

impl Tar {
    /// How the archive's one stream is written, where it is one.
    pub(crate) const fn packing(&self) -> Option<&Packing> {
        self.packing.as_ref()
    }

    /// Record the root, as `./`, in every next archive.
    pub(crate) fn record_root(&mut self) {
        self.root = true;
    }

    /// What the mount table calls the archive.
    pub(crate) const fn format_name(&self) -> FormatName {
        match &self.packing {
            None => FormatName::Tar,
            Some(packing) => match packing.compression() {
                Compression::Gzip => FormatName::TarGzip,
                Compression::Bzip2 => FormatName::TarBzip2,
            },
        }
    }
}

/// The tar's bytes — the archive's own, or its stream's decoded — read ahead
/// a scan's worth at a time, the scan only ever moving on.
struct Scan<'r, 's, A> {
    regions: &'r Regions<A>,
    stream: Option<&'s mut Stream>,
    buf: Vec<u8>,
    /// Where `buf` starts in the tar, and how much of it is read.
    start: u64,
    filled: usize,
}

impl<'r, 's, A: VolumeAccess> Scan<'r, 's, A> {
    fn new(regions: &'r Regions<A>, stream: Option<&'s mut Stream>) -> Result<Self, DriverError> {
        Ok(Self {
            regions,
            stream,
            buf: tairix_util::fallible::filled(SCAN_LEN, 0u8).ok_or(DriverError::OutOfMemory)?,
            start: 0,
            filled: 0,
        })
    }

    /// Fill `out` from `at`, answering how much the tar holds there: less
    /// only at its end.
    fn read_at(&mut self, mut at: u64, out: &mut [u8]) -> Result<usize, DriverError> {
        let mut done = 0;
        while done < out.len() {
            let ahead = at
                .checked_sub(self.start)
                .and_then(|from| usize::try_from(from).ok());
            let from = match ahead {
                Some(from) if from < self.filled => from,
                _ => {
                    self.start = at;
                    self.filled = match self.stream.as_deref_mut() {
                        Some(stream) => stream.read(self.regions, at, &mut self.buf)?,
                        None => self.regions.read(ARCHIVE_REGION, at, &mut self.buf)?,
                    };
                    if self.filled == 0 {
                        break;
                    }
                    0
                }
            };
            let take = (self.filled - from).min(out.len() - done);
            out[done..done + take].copy_from_slice(&self.buf[from..from + take]);
            done += take;
            at += take as u64;
        }
        Ok(done)
    }

    /// The block at `at`, or `None` where the tar ends first.
    fn block(&mut self, at: u64) -> Result<Option<[u8; TAR_BLOCK]>, DriverError> {
        let mut block = [0u8; TAR_BLOCK];
        Ok((self.read_at(at, &mut block)? == TAR_BLOCK).then_some(block))
    }

    /// `len` bytes at `at`, at most `max`.
    fn bytes(&mut self, at: u64, len: u64, max: u64) -> Result<Vec<u8>, DriverError> {
        if len > max {
            return Err(DriverError::BadMagic);
        }
        let len = usize::try_from(len).map_err(|_| DriverError::BadMagic)?;
        let mut bytes = tairix_util::fallible::filled(len, 0u8).ok_or(DriverError::OutOfMemory)?;
        if self.read_at(at, &mut bytes)? != len {
            return Err(DriverError::BadMagic);
        }
        Ok(bytes)
    }
}

/// Whether the stream an archive is holds a tar: its first block a header,
/// or zeros where the archive's name says it is one — an empty tar, which
/// nothing else tells from a file of zeros.
pub(crate) fn holds_tar<A: VolumeAccess>(
    stream: &mut Stream,
    regions: &Regions<A>,
    name: Option<&ArchiveName>,
) -> Result<bool, DriverError> {
    let mut block = [0u8; TAR_BLOCK];
    if stream.read(regions, 0, &mut block)? < TAR_BLOCK {
        return Ok(false);
    }
    Ok(tar_header_holds(&block)
        || (block.iter().all(|&byte| byte == 0)
            && name.is_some_and(|name| crate::lone::names_tar(name.as_bytes()))))
}

/// What a pax record set says of the member it precedes, or of every member
/// after a global one.
#[derive(Default)]
struct Pax {
    path: Option<Vec<u8>>,
    linkpath: Option<Vec<u8>>,
    size: Option<u64>,
    mtime: Option<Time64>,
    atime: Option<Time64>,
    ctime: Option<Time64>,
    uid: Option<u32>,
    gid: Option<u32>,
    acorn: BTreeMap<Vec<u8>, Vec<u8>>,
    /// Whether it describes a member GNU tar stored sparse, whose bytes are
    /// a map of its data rather than its data.
    sparse: bool,
}

impl Pax {
    /// The records `bytes` holds, laid over `self`, answering whether every
    /// one is a comment, which says nothing of any member.
    fn read(&mut self, bytes: &[u8]) -> Result<bool, DriverError> {
        let mut inert = true;
        for record in pax_records(bytes) {
            let PaxRecord { key, value, .. } = record?;
            inert &= key == b"comment";
            match key {
                b"path" => self.path = Some(owned(value)?),
                b"linkpath" => self.linkpath = Some(owned(value)?),
                b"size" => self.size = Some(parse_decimal(value)?),
                b"mtime" => self.mtime = Some(pax_time(value)?),
                b"atime" => self.atime = Some(pax_time(value)?),
                b"ctime" => self.ctime = Some(pax_time(value)?),
                b"uid" => self.uid = u32::try_from(parse_decimal(value)?).ok(),
                b"gid" => self.gid = u32::try_from(parse_decimal(value)?).ok(),
                _ if key.starts_with(b"GNU.sparse.") => self.sparse = true,
                _ => {
                    if let Some(name) = acorn_key(key) {
                        self.acorn.insert(owned(name)?, owned(value)?);
                    }
                }
            }
        }
        Ok(inert)
    }
}

/// One pax record: its key and value, and the record whole as it is written.
struct PaxRecord<'b> {
    key: &'b [u8],
    value: &'b [u8],
    whole: &'b [u8],
}

/// Each record `bytes` holds — `<length> <key>=<value>\n`, its length
/// counting the whole record — up to the zeros padding its last block.
fn pax_records(bytes: &[u8]) -> impl Iterator<Item = Result<PaxRecord<'_>, DriverError>> {
    let mut rest = bytes;
    core::iter::from_fn(move || {
        if rest.iter().all(|&byte| byte == 0) {
            return None;
        }
        let record = pax_record_at(rest);
        rest = match &record {
            Ok(record) => &rest[record.whole.len()..],
            Err(_) => &[],
        };
        Some(record)
    })
}

/// The record `rest` starts with.
fn pax_record_at(rest: &[u8]) -> Result<PaxRecord<'_>, DriverError> {
    let space = rest
        .iter()
        .position(|&byte| byte == b' ')
        .ok_or(DriverError::BadMagic)?;
    let len: usize = core::str::from_utf8(&rest[..space])
        .ok()
        .and_then(|digits| digits.parse().ok())
        .ok_or(DriverError::BadMagic)?;
    if len <= space + 1 || len > rest.len() || rest[len - 1] != b'\n' {
        return Err(DriverError::BadMagic);
    }
    let record = &rest[space + 1..len - 1];
    let equals = record
        .iter()
        .position(|&byte| byte == b'=')
        .ok_or(DriverError::BadMagic)?;
    Ok(PaxRecord {
        key: &record[..equals],
        value: &record[equals + 1..],
        whole: &rest[..len],
    })
}

/// The `acorn.*` attribute a pax key carries a RISC OS field as, if it is one.
fn acorn_key(key: &[u8]) -> Option<&[u8]> {
    key.strip_prefix(ACORN_XATTR)
        .filter(|name| name.starts_with(b"acorn."))
}

/// Whether a writer spells the record keyed `key` from the tree, never
/// carrying it over from the record it read.
fn spelt_from_tree(key: &[u8]) -> bool {
    matches!(
        key,
        b"path" | b"linkpath" | b"size" | b"mtime" | b"atime" | b"ctime" | b"uid" | b"gid"
    ) || acorn_key(key).is_some()
}

/// `bytes`, copied, or [`DriverError::OutOfMemory`].
fn owned(bytes: &[u8]) -> Result<Vec<u8>, DriverError> {
    tairix_util::fallible::copied(bytes).ok_or(DriverError::OutOfMemory)
}

/// A decimal field.
fn parse_decimal(value: &[u8]) -> Result<u64, DriverError> {
    core::str::from_utf8(value)
        .ok()
        .and_then(|digits| digits.parse().ok())
        .ok_or(DriverError::BadMagic)
}

/// A pax time: seconds, optionally signed, with an optional fraction.
fn pax_time(value: &[u8]) -> Result<Time64, DriverError> {
    let text = core::str::from_utf8(value).map_err(|_| DriverError::BadMagic)?;
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    let secs: i64 = whole.parse().map_err(|_| DriverError::BadMagic)?;
    // Read from the text, as "-0.5" parses its whole seconds as zero.
    let negative = whole.starts_with('-');
    let mut nanos = 0u32;
    let mut scale = 100_000_000;
    for digit in fraction.bytes().take(9) {
        if !digit.is_ascii_digit() {
            return Err(DriverError::BadMagic);
        }
        nanos += u32::from(digit - b'0') * scale;
        scale /= 10;
    }
    // A fraction of a negative time counts back from its whole seconds.
    if negative && nanos > 0 {
        let secs = secs.checked_sub(1).ok_or(DriverError::BadMagic)?;
        return Time64::new(secs, 1_000_000_000 - nanos).map_err(|_| DriverError::BadMagic);
    }
    Time64::new(secs, nanos).map_err(|_| DriverError::BadMagic)
}

/// What a header's name field holds, NUL-ended or full.
fn field(bytes: &[u8]) -> &[u8] {
    let end = bytes
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(bytes.len());
    &bytes[..end]
}

/// Bytes in UTF-8 where they are, read as ISO 8859-1 otherwise: tar records
/// no character set.
fn text(bytes: &[u8]) -> Result<Vec<u8>, DriverError> {
    Charset::Utf8
        .decode(bytes)
        .or_else(|| Charset::Latin1.decode(bytes))
        .ok_or(DriverError::OutOfMemory)
}

/// Read the tar `regions` holds: the archive itself, or the decoded bytes of
/// its one stream, written as `packing` says.
///
/// # Errors
///
/// [`DriverError::BadMagic`] where the first header is not one;
/// [`DriverError::Unsupported`] past the members this driver opens; the
/// stream's refusal.
pub(crate) fn read<A: VolumeAccess>(
    regions: &Regions<A>,
    compressed: Option<(Packing, Stream)>,
) -> Result<Read, DriverError> {
    let (packing, mut stream) = compressed.unzip();
    let coding = if stream.is_some() {
        Coding::Stream
    } else {
        Coding::Stored
    };
    let mut scan = Scan::new(regions, stream.as_mut())?;
    let mut walk = Walk::new(coding);
    let mut at = 0u64;
    let mut end = None;
    while let Some(block) = scan.block(at)? {
        if block.iter().all(|&byte| byte == 0) {
            end = Some(at);
            break;
        }
        if !tar_header_holds(&block) {
            if walk.members == 0 && at == 0 {
                return Err(DriverError::BadMagic);
            }
            // A damaged header ends what can be believed, and what follows
            // it would be lost by writing the archive back.
            walk.refusal = Some(WriteRefusal::Damaged);
            break;
        }
        at = walk.header(&mut scan, at, &block)?;
    }
    drop(scan);
    // The rest of the stream is decoded and checked, and its places noted.
    let len = match stream.as_mut() {
        Some(stream) => stream.measure(regions)?,
        None => regions.len(),
    };
    if walk.record_start > len {
        // The last member's bytes run past the archive's end.
        walk.refusal = Some(WriteRefusal::Damaged);
    }
    if let Some(end) = end {
        // What follows the end marker is no member, and writing the archive
        // back would lose it.
        if !zeros_after(regions, stream.as_mut(), end, len)? {
            walk.refusal.get_or_insert(WriteRefusal::UnsupportedFeature);
        }
    }
    Ok(Read {
        tree: walk.tree,
        format: Format::Tar(Tar {
            packing,
            globals: walk.globals,
            root: walk.root,
        }),
        refusal: walk.refusal,
        stream,
        rar: None,
    })
}

/// Whether the tar holds nothing but zeros from `end` to its length `len`,
/// read no further than [`TRAILER_MAX`].
fn zeros_after<A: VolumeAccess>(
    regions: &Regions<A>,
    stream: Option<&mut Stream>,
    end: u64,
    len: u64,
) -> Result<bool, DriverError> {
    if len.saturating_sub(end) > TRAILER_MAX {
        return Ok(false);
    }
    let mut scan = Scan::new(regions, stream)?;
    let mut block = [0u8; TAR_BLOCK];
    let mut at = end;
    while at < len {
        let got = scan.read_at(at, &mut block)?;
        if got == 0 {
            break;
        }
        if block[..got].iter().any(|&byte| byte != 0) {
            return Ok(false);
        }
        at += got as u64;
    }
    Ok(true)
}

/// A tar being read: its tree so far, and the headers read for the member
/// about to be.
struct Walk {
    coding: Coding,
    tree: Tree,
    global: Pax,
    local: Pax,
    long_name: Option<Vec<u8>>,
    long_link: Option<Vec<u8>>,
    /// Where the extended header's records for the member lie.
    pax: Option<(u64, u64)>,
    /// Where the headers of the member about to be read began.
    record_start: u64,
    members: usize,
    globals: Vec<Range<u64>>,
    root: bool,
    refusal: Option<WriteRefusal>,
}

impl Walk {
    fn new(coding: Coding) -> Self {
        Self {
            coding,
            tree: Tree::new(NameMatching::Exact, directory_meta()),
            global: Pax::default(),
            local: Pax::default(),
            long_name: None,
            long_link: None,
            pax: None,
            record_start: 0,
            members: 0,
            globals: Vec::new(),
            root: false,
            refusal: None,
        }
    }

    /// Read the header `block` at `at`, answering where the next one is.
    fn header<A: VolumeAccess>(
        &mut self,
        scan: &mut Scan<'_, '_, A>,
        at: u64,
        block: &[u8; TAR_BLOCK],
    ) -> Result<u64, DriverError> {
        let kind = block[156];
        let size = match self.local.size.or(self.global.size) {
            Some(size) if !matches!(kind, b'x' | b'g') => size,
            _ => tar_number(&block[124..136]).ok_or(DriverError::BadMagic)?,
        };
        let data_at = at
            .checked_add(TAR_BLOCK as u64)
            .ok_or(DriverError::BadMagic)?;
        let next = size
            .checked_add(TAR_BLOCK as u64 - 1)
            .and_then(|padded| data_at.checked_add(padded / TAR_BLOCK as u64 * TAR_BLOCK as u64))
            .ok_or(DriverError::BadMagic)?;
        match kind {
            b'x' => {
                self.local.read(&scan.bytes(data_at, size, PAX_MAX)?)?;
                self.pax = Some((data_at, size));
            }
            b'g' => {
                let inert = self.global.read(&scan.bytes(data_at, size, PAX_MAX)?)?;
                // One saying nothing of any member is kept as it is, ahead of
                // them all; any other, written back, would come to say
                // something of members it did not, or be lost.
                if inert && self.record_start == at && self.globals.len() < GLOBALS_MAX {
                    self.globals
                        .try_reserve(1)
                        .map_err(|_| DriverError::OutOfMemory)?;
                    self.globals.push(at..next);
                    self.record_start = next;
                } else {
                    self.refusal.get_or_insert(WriteRefusal::UnsupportedFeature);
                }
            }
            b'L' => {
                self.long_name = Some(owned(field(&scan.bytes(data_at, size, LONG_NAME_MAX)?))?);
            }
            b'K' => {
                self.long_link = Some(owned(field(&scan.bytes(data_at, size, LONG_NAME_MAX)?))?);
            }
            _ => self.member(block, (at, data_at), size, next)?,
        }
        Ok(next)
    }

    /// Put the member whose ustar header `block` is at `at`, its `size`
    /// bytes at `data_at`, into the tree.
    fn member(
        &mut self,
        block: &[u8; TAR_BLOCK],
        (at, data_at): (u64, u64),
        size: u64,
        next: u64,
    ) -> Result<(), DriverError> {
        self.members += 1;
        if self.members > MAX_MEMBERS {
            return Err(DriverError::Unsupported);
        }
        let header = Header {
            block,
            global: &self.global,
            local: &self.local,
            long_name: self.long_name.take(),
            long_link: self.long_link.take(),
            size,
            at,
            data_at,
            record: self.record_start..next,
            pax: self.pax.take(),
        };
        match admit(&mut self.tree, &header, self.coding) {
            Ok(Admitted::Added) => {}
            Ok(Admitted::Root) => self.root = true,
            // A member left out, or one an earlier member's place was given
            // to, would be lost by writing the archive back.
            Ok(Admitted::Displaced) => {
                self.refusal.get_or_insert(WriteRefusal::UnsupportedFeature);
            }
            Err(err) if leaves_out(err) => {
                self.refusal.get_or_insert(WriteRefusal::UnsupportedFeature);
            }
            Err(err) => return Err(err),
        }
        self.local = Pax::default();
        self.record_start = next;
        Ok(())
    }
}

/// One member's headers, read.
struct Header<'b> {
    block: &'b [u8; TAR_BLOCK],
    global: &'b Pax,
    local: &'b Pax,
    long_name: Option<Vec<u8>>,
    long_link: Option<Vec<u8>>,
    size: u64,
    /// Where its ustar header is.
    at: u64,
    data_at: u64,
    record: Range<u64>,
    /// Where its extended header's records are.
    pax: Option<(u64, u64)>,
}

impl<'b> Header<'b> {
    /// What `pick` reads of its own extended header, else of the global one.
    fn pax<T>(&self, pick: impl Fn(&'b Pax) -> Option<T>) -> Option<T> {
        pick(self.local).or_else(|| pick(self.global))
    }

    /// Its path: the pax record's, a GNU long name, or ustar's prefix and
    /// name.
    fn path(&self) -> Result<Vec<u8>, DriverError> {
        if let Some(path) = self
            .pax(|pax| pax.path.as_ref())
            .or(self.long_name.as_ref())
        {
            return text(path);
        }
        let name = field(&self.block[..100]);
        let prefix = if &self.block[257..262] == b"ustar" {
            field(&self.block[345..500])
        } else {
            &[]
        };
        let mut path = Vec::new();
        path.try_reserve_exact(prefix.len() + 1 + name.len())
            .map_err(|_| DriverError::OutOfMemory)?;
        if !prefix.is_empty() {
            path.extend_from_slice(prefix);
            path.push(b'/');
        }
        path.extend_from_slice(name);
        text(&path)
    }

    fn link(&self) -> Result<Vec<u8>, DriverError> {
        match self
            .pax(|pax| pax.linkpath.as_ref())
            .or(self.long_link.as_ref())
        {
            Some(link) => text(link),
            None => text(field(&self.block[157..257])),
        }
    }

    fn meta(&self) -> Meta {
        let number = |range: Range<usize>| tar_number(&self.block[range]);
        let mtime = self.pax(|pax| pax.mtime).unwrap_or_else(|| {
            number(136..148)
                .and_then(|secs| i64::try_from(secs).ok())
                .map_or(Time64::UNIX_EPOCH, Time64::from_secs)
        });
        let uid = self
            .pax(|pax| pax.uid)
            .or_else(|| number(108..116).and_then(|uid| u32::try_from(uid).ok()));
        let gid = self
            .pax(|pax| pax.gid)
            .or_else(|| number(116..124).and_then(|gid| u32::try_from(gid).ok()));
        let mode = number(100..108).map_or(0o644, |mode| (mode & 0o7777) as u32);
        Meta {
            mode,
            owner: uid.zip(gid),
            times: NodeTimes {
                created: Time64::UNIX_EPOCH,
                modified: mtime,
                accessed: self.pax(|pax| pax.atime).unwrap_or(Time64::UNIX_EPOCH),
                changed: self.pax(|pax| pax.ctime).unwrap_or(Time64::UNIX_EPOCH),
            },
            acorn: acorn_fields(&self.local.acorn, &self.global.acorn),
            recorded: Recorded {
                accessed: self.pax(|pax| pax.atime).is_some(),
                changed: self.pax(|pax| pax.ctime).is_some(),
            },
        }
    }

    /// Where its headers keep what the tree does not hold.
    fn notes(&self, owner: Option<(u32, u32)>) -> TarNotes {
        TarNotes {
            pax: self.pax,
            header: self.at,
            owner,
        }
    }
}

/// The RISC OS fields the `acorn.*` records carry, a member's own laid over
/// the global ones: the raw load and exec words where it has them, else a
/// filetype and stamp, and the attributes.
fn acorn_fields(
    local: &BTreeMap<Vec<u8>, Vec<u8>>,
    global: &BTreeMap<Vec<u8>, Vec<u8>>,
) -> Option<acorn::Object> {
    if local.is_empty() && global.is_empty() {
        return None;
    }
    let mut object = acorn::Object {
        load: 0,
        exec: 0,
        attr: acorn::attr_from_fileswitch(0x03),
    };
    for key in [
        acorn::LOADADDR_KEY,
        acorn::EXECADDR_KEY,
        acorn::FILETYPE_KEY,
        acorn::DATESTAMP_KEY,
        acorn::ATTR_KEY,
    ] {
        if let Some(value) = local.get(key).or_else(|| global.get(key)) {
            // A record out of shape is passed over, as a reader of the
            // format would.
            let _ = object.set(key, value, acorn::ATTR_BITS & !acorn::ATTR_DIRECTORY);
        }
    }
    Some(object)
}

/// What putting a member into the tree did.
enum Admitted {
    /// It took a name of its own.
    Added,
    /// It was the record of the root, as `./` names it.
    Root,
    /// It took an earlier member's place, as extracting the archive
    /// overwrites that one with it.
    Displaced,
}

/// Put the member `header` reads into `tree`, its bytes kept as `coding`
/// says.
fn admit(tree: &mut Tree, header: &Header<'_>, coding: Coding) -> Result<Admitted, DriverError> {
    if header.local.sparse || header.global.sparse {
        return Err(DriverError::Unsupported);
    }
    let path = header.path()?;
    let kind = header.block[156];
    let meta = header.meta();
    let notes = header.notes(meta.owner).encode()?;
    let kept = Some(Kept {
        record: header.record.clone(),
        notes: Vec::new(),
    });
    let mut parts = match spelled(&path, b'/', as_is) {
        Spelled::Within(parts) => parts,
        Spelled::Root if kind == b'5' => {
            let root = tree.node_mut(ROOT)?;
            root.meta = meta;
            root.kept = kept;
            root.notes = notes;
            return Ok(Admitted::Root);
        }
        Spelled::Root | Spelled::Refused => return Err(DriverError::OutOfRange),
    };
    let leaf = parts.pop().ok_or(DriverError::OutOfRange)?;
    let dir = tree.directory(&parts, &directory_meta())?;
    let body = match kind {
        b'0' | b'\0' | b'7' => Body::File(Data::Archived(Archived {
            at: header.data_at,
            lead: Lead::None,
            len: header.size,
            coding,
            check: Check::None,
        })),
        b'5' => {
            let made = match tree.lookup(dir, &leaf) {
                Ok(made) if matches!(tree.node(made)?.body, Body::Dir(_)) => {
                    let node = tree.node_mut(made)?;
                    node.meta = meta;
                    node.kept = kept;
                    made
                }
                Ok(_) => return Err(DriverError::AlreadyExists),
                Err(_) => tree.add(dir, &leaf, Body::Dir(BTreeMap::new()), meta, kept)?,
            };
            tree.node_mut(made)?.notes = notes;
            return Ok(Admitted::Added);
        }
        b'2' => {
            let target = header.link()?;
            if target.is_empty() || target.len() > FS_SYMLINK_MAX || target.contains(&0) {
                return Err(DriverError::OutOfRange);
            }
            Body::Link(target)
        }
        b'1' => {
            // A hard link is read as a copy of what it names, which an
            // earlier member must be.
            let target = header.link()?;
            let Spelled::Within(target) = spelled(&target, b'/', as_is) else {
                return Err(DriverError::OutOfRange);
            };
            let mut at = ROOT;
            for part in &target {
                at = tree.lookup(at, part)?;
            }
            match &tree.node(at)?.body {
                Body::File(data) => Body::File(data.clone()),
                _ => return Err(DriverError::Unsupported),
            }
        }
        // Devices, pipes, sparse members and volume labels have no volume
        // node to be.
        _ => return Err(DriverError::Unsupported),
    };
    let kept = if kind == b'1' { None } else { kept };
    match tree.lookup(dir, &leaf) {
        Ok(earlier) if !matches!(tree.node(earlier)?.body, Body::Dir(_)) => {
            let content_gen = tree.fresh_gen();
            let node = tree.node_mut(earlier)?;
            node.body = body;
            node.meta = meta;
            node.kept = kept;
            node.notes = notes;
            node.content_gen = content_gen;
            Ok(Admitted::Displaced)
        }
        _ => {
            let id = tree.add(dir, &leaf, body, meta, kept)?;
            tree.node_mut(id)?.notes = notes;
            Ok(Admitted::Added)
        }
    }
}

/// Where a tar member's headers keep what the tree does not hold: the pax
/// records this driver does not read, and the names of its owner. A record
/// spelled afresh reads them back from there.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct TarNotes {
    /// Where its extended header's records are, and how many bytes.
    pax: Option<(u64, u64)>,
    /// Where its ustar header is.
    header: u64,
    /// The owner its header's names are the names of.
    owner: Option<(u32, u32)>,
}

impl TarNotes {
    const LEN: usize = 34;

    fn encode(&self) -> Result<Vec<u8>, DriverError> {
        let mut out = crate::wire::Out::with_capacity(Self::LEN)?;
        let (pax_at, pax_len) = self.pax.unwrap_or((0, 0));
        let (uid, gid) = self.owner.unwrap_or((0, 0));
        out.bytes(&[
            u8::from(self.pax.is_some()) | u8::from(self.owner.is_some()) << 1,
            0,
        ]);
        out.u64(pax_at);
        out.u64(pax_len);
        out.u64(self.header);
        out.u32(uid);
        out.u32(gid);
        out.finish()
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        let flags = *bytes.first()?;
        let pax = (le64(bytes, 2)?, le64(bytes, 10)?);
        let owner = (le32(bytes, 26)?, le32(bytes, 30)?);
        Some(Self {
            pax: (flags & 1 != 0).then_some(pax),
            header: le64(bytes, 18)?,
            owner: (flags & 2 != 0).then_some(owner),
        })
    }

    /// The notes of the record they describe, copied whole from `from` to
    /// `to`.
    fn moved(self, from: u64, to: u64) -> Result<Self, DriverError> {
        let shift = |at: u64| {
            at.checked_sub(from)
                .and_then(|offset| to.checked_add(offset))
                .ok_or(DriverError::DeviceFault)
        };
        Ok(Self {
            pax: self
                .pax
                .map(|(at, len)| shift(at).map(|at| (at, len)))
                .transpose()?,
            header: shift(self.header)?,
            owner: self.owner,
        })
    }

    /// What a fresh record of the member keeps of the record they describe,
    /// read from the tar: the pax records a writer does not spell, and the
    /// owner's names while `owner` is still the owner they name.
    fn carried<A: VolumeAccess>(
        &self,
        content: &mut Content<'_, A>,
        coding: Coding,
        owner: Option<(u32, u32)>,
    ) -> Result<Carried, DriverError> {
        let same_owner = owner == self.owner;
        let mut carried = Carried::default();
        if same_owner {
            let header = tar_bytes(content, coding, self.header, TAR_BLOCK as u64)?;
            if &header[257..262] == b"ustar" {
                carried.names.copy_from_slice(&header[265..329]);
            }
        }
        if let Some((at, len)) = self.pax {
            let records = tar_bytes(content, coding, at, len)?;
            for record in pax_records(&records) {
                let record = record?;
                match record.key {
                    b"uname" | b"gname" if !same_owner => {}
                    key if spelt_from_tree(key) => {}
                    _ => {
                        carried
                            .pax
                            .try_reserve(record.whole.len())
                            .map_err(|_| DriverError::OutOfMemory)?;
                        carried.pax.extend_from_slice(record.whole);
                    }
                }
            }
        }
        Ok(carried)
    }
}

/// What a member's record held that a fresh one keeps too.
struct Carried {
    /// Its pax records a writer does not spell, as they were written.
    pax: Vec<u8>,
    /// Its owner's user and group names, as its ustar header spelt them.
    names: [u8; 64],
}

impl Default for Carried {
    fn default() -> Self {
        Self {
            pax: Vec::new(),
            names: [0; 64],
        }
    }
}

/// `len` bytes at `at` of the tar: the archive's own, or its stream's
/// decoded.
fn tar_bytes<A: VolumeAccess>(
    content: &mut Content<'_, A>,
    coding: Coding,
    at: u64,
    len: u64,
) -> Result<Vec<u8>, DriverError> {
    if len > PAX_MAX {
        return Err(DriverError::DeviceFault);
    }
    let len = usize::try_from(len).map_err(|_| DriverError::DeviceFault)?;
    let mut bytes = tairix_util::fallible::filled(len, 0u8).ok_or(DriverError::OutOfMemory)?;
    let archived = Archived {
        at,
        lead: Lead::None,
        len: len as u64,
        coding,
        check: Check::None,
    };
    if content
        .cursors
        .read(content.regions, (0, 0), &archived, 0, &mut bytes)?
        != len
    {
        return Err(DriverError::MediumError);
    }
    Ok(bytes)
}

impl Tar {
    /// Write the next tar from `tree` into `out`.
    pub(crate) fn write<A: VolumeAccess, O: Out<A>>(
        &mut self,
        tree: &Tree,
        content: &mut Content<'_, A>,
        sink: &mut O,
    ) -> Result<Vec<Placed>, DriverError> {
        let coding = if self.packing.is_some() {
            Coding::Stream
        } else {
            Coding::Stored
        };
        let mut globals = Vec::new();
        globals
            .try_reserve_exact(self.globals.len())
            .map_err(|_| DriverError::OutOfMemory)?;
        for global in &self.globals {
            let start = sink.position();
            sink.copy(
                content,
                ARCHIVE_REGION,
                global.start,
                global.end - global.start,
            )?;
            globals.push(start..sink.position());
        }
        let order = tree.in_order()?;
        let mut placed = Vec::new();
        placed
            .try_reserve_exact(order.len() + 1)
            .map_err(|_| DriverError::OutOfMemory)?;
        for id in self.root.then_some(ROOT).into_iter().chain(order) {
            placed.push(put_member(tree, id, coding, content, sink)?);
        }
        // Two zero blocks end the archive, padded out to a record.
        put_zeros(sink, 2 * TAR_BLOCK as u64)?;
        put_zeros(
            sink,
            sink.position().next_multiple_of(RECORD) - sink.position(),
        )?;
        self.globals = globals;
        Ok(placed)
    }
}

/// Write node `id`'s record into `sink`: copied whole where nothing about it
/// has changed, else spelled afresh with what its old record carries.
fn put_member<A: VolumeAccess, O: Out<A>>(
    tree: &Tree,
    id: u64,
    coding: Coding,
    content: &mut Content<'_, A>,
    sink: &mut O,
) -> Result<Placed, DriverError> {
    let node = tree.node(id)?;
    let record_start = sink.position();
    if let Some(kept) = &node.kept {
        sink.copy(
            content,
            ARCHIVE_REGION,
            kept.record.start,
            kept.record.end - kept.record.start,
        )?;
        let data = match &node.body {
            Body::File(Data::Archived(archived)) => Some(Data::Archived(Archived {
                at: archived
                    .at
                    .checked_sub(kept.record.start)
                    .and_then(|offset| record_start.checked_add(offset))
                    .ok_or(DriverError::DeviceFault)?,
                ..*archived
            })),
            _ => None,
        };
        let notes = TarNotes::decode(&node.notes)
            .map(|notes| notes.moved(kept.record.start, record_start)?.encode())
            .transpose()?;
        return Ok(Placed {
            id,
            data,
            kept: Some(Kept {
                record: record_start..sink.position(),
                notes: Vec::new(),
            }),
            notes,
        });
    }
    let carried = match TarNotes::decode(&node.notes) {
        Some(notes) => notes.carried(content, coding, node.meta.owner)?,
        None => Carried::default(),
    };
    let path = if id == ROOT {
        alloc::vec![&b"."[..]]
    } else {
        tree.path(id)?
    };
    let (kind, size, link): (u8, u64, &[u8]) = match &node.body {
        Body::Dir(_) => (b'5', 0, &[]),
        Body::File(data) => (b'0', data.len(), &[]),
        Body::Link(target) => (b'2', 0, target),
    };
    let spelt = put_headers(sink, &path, (kind, size, link), &node.meta, &carried)?;
    let data_at = sink.position();
    let data = if let Body::File(data) = &node.body {
        content.copy_into((id, node.content_gen), data, sink)?;
        put_zeros(sink, padding(size))?;
        Some(Data::Archived(Archived {
            at: data_at,
            lead: Lead::None,
            len: size,
            coding,
            check: Check::None,
        }))
    } else {
        None
    };
    let notes = TarNotes {
        pax: spelt.pax,
        header: spelt.header,
        owner: node.meta.owner,
    };
    Ok(Placed {
        id,
        data,
        kept: Some(Kept {
            record: record_start..sink.position(),
            notes: Vec::new(),
        }),
        notes: Some(notes.encode()?),
    })
}

/// The zeros after `size` bytes that end its last block.
const fn padding(size: u64) -> u64 {
    (TAR_BLOCK as u64 - size % TAR_BLOCK as u64) % TAR_BLOCK as u64
}

/// Where a member's fresh headers put what its notes point at: its extended
/// header's records, and its ustar header.
struct Spelt {
    pax: Option<(u64, u64)>,
    header: u64,
}

/// A member's headers: a pax record set where a field needs one or its old
/// record carries one, then the ustar header.
fn put_headers<A: VolumeAccess, O: Out<A>>(
    sink: &mut O,
    path: &[&[u8]],
    (kind, size, link): (u8, u64, &[u8]),
    meta: &Meta,
    carried: &Carried,
) -> Result<Spelt, DriverError> {
    let mut joined = Vec::new();
    joined
        .try_reserve_exact(path.iter().map(|part| part.len() + 1).sum())
        .map_err(|_| DriverError::OutOfMemory)?;
    for (at, part) in path.iter().enumerate() {
        if at > 0 {
            joined.push(b'/');
        }
        joined.extend_from_slice(part);
    }
    if kind == b'5' {
        joined.push(b'/');
    }
    let split = ustar_split(&joined);
    let mut pax = Vec::new();
    if split.is_none() || !joined.is_ascii() {
        pax_record(&mut pax, b"path", &joined)?;
    }
    if link.len() > 100 || !link.is_ascii() {
        pax_record(&mut pax, b"linkpath", link)?;
    }
    let mut digits = [0u8; 20];
    if size >= 0o77_777_777_777 {
        pax_record(&mut pax, b"size", decimal(size, &mut digits))?;
    }
    let mut text = [0u8; 32];
    let mtime = meta.times.modified;
    if mtime.secs() < 0 || mtime.secs() >= 0o77_777_777_777 || mtime.subsec_nanos() != 0 {
        pax_record(&mut pax, b"mtime", time_text(mtime, &mut text))?;
    }
    if meta.recorded.accessed {
        pax_record(
            &mut pax,
            b"atime",
            time_text(meta.times.accessed, &mut text),
        )?;
    }
    if meta.recorded.changed {
        pax_record(&mut pax, b"ctime", time_text(meta.times.changed, &mut text))?;
    }
    let (uid, gid) = meta.owner.unwrap_or((0, 0));
    if uid > 0o7_777_777 {
        pax_record(&mut pax, b"uid", decimal(u64::from(uid), &mut digits))?;
    }
    if gid > 0o7_777_777 {
        pax_record(&mut pax, b"gid", decimal(u64::from(gid), &mut digits))?;
    }
    if let Some(object) = meta.acorn {
        let mut value = [0u8; acorn::ATTR_VALUE_MAX];
        for key in acorn::KEYS {
            if let Ok(Some(len)) = object.value(key, &mut value) {
                let mut name = Vec::new();
                name.try_reserve_exact(ACORN_XATTR.len() + key.len())
                    .map_err(|_| DriverError::OutOfMemory)?;
                name.extend_from_slice(ACORN_XATTR);
                name.extend_from_slice(key);
                pax_record(&mut pax, &name, &value[..len])?;
            }
        }
    }
    pax.try_reserve(carried.pax.len())
        .map_err(|_| DriverError::OutOfMemory)?;
    pax.extend_from_slice(&carried.pax);
    let mut spelt = Spelt {
        pax: None,
        header: 0,
    };
    if !pax.is_empty() {
        let mut header = ustar(
            b"PaxHeaders/entry",
            (b'x', pax.len() as u64, &[]),
            &directory_meta(),
            &[0; 64],
        );
        seal(&mut header);
        sink.put(&header)?;
        spelt.pax = Some((sink.position(), pax.len() as u64));
        sink.put(&pax)?;
        put_zeros(sink, padding(pax.len() as u64))?;
    }
    let (prefix, name) = split.unwrap_or((&[][..], &joined[joined.len().saturating_sub(100)..]));
    let mut header = ustar(
        name,
        (kind, size, &link[..link.len().min(100)]),
        meta,
        &carried.names,
    );
    header[345..345 + prefix.len()].copy_from_slice(prefix);
    seal(&mut header);
    spelt.header = sink.position();
    sink.put(&header)?;
    Ok(spelt)
}

/// `path` cut into ustar's prefix and name at a `/`, each within its field
/// and the name naming something — never only a directory's trailing `/` —
/// or `None` where it cannot be.
fn ustar_split(path: &[u8]) -> Option<(&[u8], &[u8])> {
    if path.len() <= 100 {
        return Some((&[], path));
    }
    let cut = path
        .iter()
        .enumerate()
        .rev()
        .find(|&(at, &byte)| {
            let name = &path[at + 1..];
            byte == b'/'
                && at > 0
                && at <= 155
                && name.len() <= 100
                && !name.is_empty()
                && name != b"/"
        })
        .map(|(at, _)| at)?;
    Some((&path[..cut], &path[cut + 1..]))
}

/// One pax record, `<length> <key>=<value>\n`, its length counting itself.
fn pax_record(out: &mut Vec<u8>, key: &[u8], value: &[u8]) -> Result<(), DriverError> {
    // Key, value, the space, the `=` and the newline, then the length's own
    // digits, which can carry it to one more digit.
    let body = key.len() + value.len() + 3;
    let mut len = body + 1;
    let mut digits = [0u8; 20];
    while len != body + decimal(len as u64, &mut digits).len() {
        len = body + decimal(len as u64, &mut digits).len();
    }
    out.try_reserve(len).map_err(|_| DriverError::OutOfMemory)?;
    out.extend_from_slice(decimal(len as u64, &mut digits));
    out.push(b' ');
    out.extend_from_slice(key);
    out.push(b'=');
    out.extend_from_slice(value);
    out.push(b'\n');
    Ok(())
}

/// `value` in decimal, written into `buf`.
fn decimal(value: u64, buf: &mut [u8; 20]) -> &[u8] {
    tairix_util::fmt::format_u64(value, buf).as_bytes()
}

/// `time` as a pax time, written into `buf`: seconds, and the fraction where
/// it has one, a negative time's fraction counting back towards zero.
fn time_text(time: Time64, buf: &mut [u8; 32]) -> &[u8] {
    let (secs, nanos) = if time.secs() < 0 && time.subsec_nanos() > 0 {
        (time.secs() + 1, 1_000_000_000 - time.subsec_nanos())
    } else {
        (time.secs(), time.subsec_nanos())
    };
    let mut len = 0;
    if time.secs() < 0 {
        buf[0] = b'-';
        len = 1;
    }
    let mut digits = [0u8; 20];
    let whole = decimal(secs.unsigned_abs(), &mut digits);
    buf[len..len + whole.len()].copy_from_slice(whole);
    len += whole.len();
    if nanos > 0 {
        buf[len] = b'.';
        len += 1;
        for at in 0..9 {
            buf[len] = b'0' + ((nanos / 10u32.pow(8 - at)) % 10) as u8;
            len += 1;
        }
    }
    &buf[..len]
}

/// A ustar header of `kind`, its checksum yet to be sealed, its owner named
/// by `names`: the user then the group, each NUL-padded.
fn ustar(
    name: &[u8],
    (kind, size, link): (u8, u64, &[u8]),
    meta: &Meta,
    names: &[u8; 64],
) -> [u8; TAR_BLOCK] {
    let mut block = [0u8; TAR_BLOCK];
    block[..name.len().min(100)].copy_from_slice(&name[..name.len().min(100)]);
    octal(&mut block[100..108], u64::from(meta.mode & 0o7777));
    let (uid, gid) = meta.owner.unwrap_or((0, 0));
    octal(&mut block[108..116], u64::from(uid).min(0o7_777_777));
    octal(&mut block[116..124], u64::from(gid).min(0o7_777_777));
    octal(&mut block[124..136], size.min(0o77_777_777_777));
    let secs = u64::try_from(meta.times.modified.secs()).unwrap_or(0);
    octal(&mut block[136..148], secs.min(0o77_777_777_777));
    block[156] = kind;
    block[157..157 + link.len()].copy_from_slice(link);
    block[257..263].copy_from_slice(b"ustar\0");
    block[263..265].copy_from_slice(b"00");
    block[265..329].copy_from_slice(names);
    block
}

/// `value` in a ustar numeric field: octal digits, ended by a NUL.
fn octal(field: &mut [u8], value: u64) {
    let digits = field.len() - 1;
    for (at, slot) in field[..digits].iter_mut().enumerate() {
        let shift = 3 * (digits - 1 - at);
        *slot = b'0' + ((value >> shift) & 7) as u8;
    }
    field[digits] = 0;
}

/// Write `block`'s checksum: the sum of its bytes, the field counted as
/// spaces.
fn seal(block: &mut [u8; TAR_BLOCK]) {
    block[148..156].fill(b' ');
    let sum: u64 = block.iter().map(|&byte| u64::from(byte)).sum();
    octal(&mut block[148..155], sum);
    block[155] = b' ';
}

#[cfg(test)]
#[path = "tar_tests.rs"]
mod tests;
