//! TAIRiX ISO 9660 filesystem driver (read-only).
//!
//! Mounts the tree [`tairix_fsprobe::iso9660::identify`] chooses from a
//! disc's last session — the primary tree when it carries Rock Ridge or RISC
//! OS records, else Joliet, else the ISO 9660:1999 enhanced tree — behind
//! any [`Block`] device, through [`FilesystemRead`], [`FilesystemSecurity`],
//! [`FilesystemAttrs`] and [`FilesystemStats`]. Every [`FilesystemWrite`]
//! mutation is refused as read-only. `docs/src/filesystem/iso9660.md`
//! describes what each extension contributes.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

mod name;
mod susp;
mod time;
mod zisofs;

#[cfg(test)]
mod tests;

use alloc::boxed::Box;
use alloc::vec::Vec;

use tairix_abi::driver::block::Block;
use tairix_abi::driver::filesystem::{
    DirEntry, DirVisit, FilesystemAttrs, FilesystemAttrsFs, FilesystemAttrsProvider,
    FilesystemRead, FilesystemSecurity, FilesystemStats, FilesystemWrite, NameMatching, NodeId,
    NodeIdentity, NodeInfo, NodeKind, NodeSecurity, NodeTimes, Unpinned, VolumeStats,
};
use tairix_abi::fs::SeekRegion;
use tairix_abi::time::Time64;
use tairix_abi::{CapabilityId, DriverError, DriverHandle, DriverHost};
use tairix_bytedev::ByteDevice;
use tairix_fsmeta::preset::{acorn, mac};
use tairix_fsprobe::iso9660::{
    self, both_u16, Flavour, Record, Volume, FLAG_ASSOCIATED, FLAG_MULTI_EXTENT, FLAG_PROTECTION,
    SECTOR, SECTOR_LEN,
};

use name::Name;
use susp::{Next, Posix, Sparse, SystemUse, S_IFDIR, S_IFLNK, S_IFMT, S_IFREG};

/// Per-driver `DriverHandle` marker returned by [`register`].
const REGISTER_HANDLE_MARKER: u64 = 0x4953_4F39_0000_0001; // "ISO9" + index

/// The longest directory record: its length is one byte.
const RECORD_MAX: usize = 255;

/// Most continuation areas one record's system use is followed through: a
/// validation bound on untrusted media.
const MAX_CONTINUATIONS: usize = 16;

/// Most records one multi-extent file is assembled from: a validation bound
/// a volume of 2^32 logical blocks can never need more than.
const MAX_PARTS: usize = 4096;

/// Bytes of an extended attribute record's fields this driver reads: the
/// owner, group, permissions and the four dates.
const XAR_READ_LEN: usize = 78;

/// Driver entry point.
///
/// # Errors
///
/// * [`DriverError::PermissionDenied`] if the host did not grant
///   [`CapabilityId::DRV_LOAD`].
///
/// # Capabilities
///
/// Requires [`CapabilityId::DRV_LOAD`].
pub fn register(host: &dyn DriverHost) -> Result<DriverHandle, DriverError> {
    if !host.has_capability(CapabilityId::DRV_LOAD) {
        return Err(DriverError::PermissionDenied);
    }
    DriverHandle::from_raw(REGISTER_HANDLE_MARKER)
}

/// A mounted ISO 9660 or High Sierra volume.
pub struct Iso9660<B: Block> {
    dev: ByteDevice<B>,
    volume: Volume,
    /// The root directory's `.` record: its node.
    root: NodeId,
    /// The sector [`Self::sector`] holds, by its byte address.
    sector_at: Option<u64>,
    sector: Box<[u8; SECTOR_LEN]>,
    /// The sector [`Self::area`] holds, by its byte address, while it holds
    /// one whole: continuation areas are packed many to a sector.
    area_at: Option<u64>,
    area: Box<[u8; SECTOR_LEN]>,
    /// The file most recently read, mapped.
    file: Option<(NodeId, FileMap)>,
    /// The zisofs block most recently decompressed.
    block: Option<Decompressed>,
}

/// One extent of a file: a directory record's share of it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Part {
    extent: u32,
    xar_blocks: u8,
    unit_blocks: u8,
    gap_blocks: u8,
    len: u64,
}

/// Where a file's bytes are stored, and how they are read.
#[derive(Clone, Debug, Eq, PartialEq)]
struct FileMap {
    parts: Vec<Part>,
    /// Bytes stored across the parts.
    stored: u64,
    /// The file's bytes as read: the uncompressed length of a zisofs file.
    size: u64,
    compression: Compression,
    /// A Rock Ridge sparse file, whose layout is not read: its size is the
    /// one its `SF` entry records, and its bytes are refused.
    sparse: bool,
}

/// How a file's stored bytes become the bytes it reads as.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Compression {
    /// They are its bytes.
    None,
    /// zisofs, its header not yet read: a listing needs only the `ZF`
    /// record's length.
    Zisofs(susp::Zisofs),
    /// zisofs, its header read and its layout checked against the record.
    Mapped(zisofs::Layout),
}

/// A zisofs block, decompressed.
struct Decompressed {
    file: NodeId,
    index: u64,
    bytes: Vec<u8>,
}

/// What a node is, resolved from its directory record.
struct Node {
    info: NodeInfo,
    security: Option<NodeSecurity>,
    acorn: Option<acorn::Object>,
    finder: Option<mac::FinderInfo>,
    amiga: Option<susp::Amiga>,
    content: Content,
}

/// The attribute keys a node's foreign records surface, in listing order.
fn keys(node: &Node) -> impl Iterator<Item = &'static [u8]> + '_ {
    let acorn = node
        .acorn
        .iter()
        .flat_map(|object| (0..).map_while(move |index| object.key(index)));
    let finder = node
        .finder
        .iter()
        .flat_map(|finder| (0..).map_while(move |index| finder.key(index)));
    let amiga = node.amiga.iter().flat_map(|amiga| {
        amiga
            .protection
            .map(|_| &b"amiga.protection"[..])
            .into_iter()
            .chain((!amiga.comment.is_empty()).then_some(&b"amiga.comment"[..]))
    });
    acorn.chain(finder).chain(amiga)
}

/// The value of attribute `key` on `node`, written into `out`.
fn value(node: &Node, key: &[u8], out: &mut [u8]) -> Result<Option<usize>, DriverError> {
    let mut staging = [0u8; acorn::ATTR_VALUE_MAX];
    let mut code = [0u8; mac::FINDER_VALUE_MAX];
    let protection;
    let bytes: &[u8] = match (key, &node.amiga) {
        (
            b"amiga.protection",
            Some(susp::Amiga {
                protection: Some(bits),
                ..
            }),
        ) => {
            protection = tairix_fsmeta::preset::amiga::protection_to_value(*bits);
            &protection
        }
        (b"amiga.comment", Some(amiga)) if !amiga.comment.is_empty() => &amiga.comment,
        _ => {
            if let Some(len) = node.finder.and_then(|finder| finder.value(key, &mut code)) {
                &code[..len]
            } else if let Some(object) = node.acorn {
                let Some(len) = object
                    .value(key, &mut staging)
                    .map_err(|_| DriverError::DeviceFault)?
                else {
                    return Ok(None);
                };
                &staging[..len]
            } else {
                return Ok(None);
            }
        }
    };
    let slot = out
        .get_mut(..bytes.len())
        .ok_or(DriverError::BufferTooSmall)?;
    slot.copy_from_slice(bytes);
    Ok(Some(bytes.len()))
}

/// Where a node's content lies.
enum Content {
    /// A directory's records, by byte address.
    Directory {
        start: u64,
        end: u64,
    },
    File(FileMap),
    Link(Vec<u8>),
}

/// One directory record, copied out of its sector, and its byte address.
#[derive(Clone)]
struct Raw {
    address: u64,
    bytes: [u8; RECORD_MAX],
    record: Record,
}

impl Raw {
    /// The record opening `available`, recorded at byte `address`: one that
    /// does not decode is damage.
    fn parse(address: u64, available: &[u8], flavour: Flavour) -> Result<Self, DriverError> {
        let record = Record::parse(available, flavour).ok_or(DriverError::DeviceFault)?;
        let mut bytes = [0u8; RECORD_MAX];
        bytes[..record.len].copy_from_slice(&available[..record.len]);
        Ok(Self {
            address,
            bytes,
            record,
        })
    }

    fn bytes(&self) -> &[u8] {
        &self.bytes[..self.record.len]
    }

    fn identifier(&self) -> &[u8] {
        self.record.name(self.bytes())
    }
}

/// A walk over the records of a directory's bytes `pos..end`.
struct Walk {
    pos: u64,
    end: u64,
}

/// Which entries [`Iso9660::listed`] resolves: a listing's every one, a
/// lookup's by name, a climb's by the directory it holds.
#[derive(Clone, Copy)]
enum Wanted<'a> {
    All,
    Named(&'a [u8]),
    /// The directory whose `.` record opens this extent.
    Directory(u32),
}

/// What [`Iso9660::listed`] makes of the records at a walk's position.
// One per record walked, returned once and never held in bulk.
#[allow(clippy::large_enum_variant)]
enum Listed {
    /// A node to list.
    Entry { name: Name, node: Node, id: NodeId },
    /// A record the directory keeps but does not list.
    Skipped,
}

impl<B: Block> Iso9660<B> {
    /// Mount the volume `device` holds, reading the descriptor set of the
    /// session its disc layout names last.
    ///
    /// # Errors
    ///
    /// * [`DriverError::BadMagic`] if the device holds no ISO 9660 or High
    ///   Sierra volume, or its root directory does not decode.
    /// * [`DriverError::Unsupported`] for a device block size outside the
    ///   bounds the storage stack defines.
    /// * [`DriverError::OutOfMemory`] if a working buffer is refused.
    /// * Whatever the device reports for a read.
    pub fn open(device: B) -> Result<Self, DriverError> {
        let mut dev = ByteDevice::new(device)?;
        let layout = dev.device().disc_layout();
        let volume = iso9660::identify(&mut dev, layout)?.ok_or(DriverError::BadMagic)?;
        let root = volume.mounted().root(volume.flavour)?;
        let root_at = volume.block_offset(root.extent)
            + u64::from(root.xar_blocks) * u64::from(volume.block_size);
        let mut fs = Self {
            dev,
            volume,
            root: node_id(root_at),
            sector_at: None,
            sector: heap_sector()?,
            area_at: None,
            area: heap_sector()?,
            file: None,
            block: None,
        };
        match fs.node(fs.root)?.content {
            Content::Directory { .. } => Ok(fs),
            Content::File(_) | Content::Link(_) => Err(DriverError::BadMagic),
        }
    }

    /// The volume's stable identity, as the volume probe derives it.
    #[must_use]
    pub fn volume_identity(&self) -> [u8; 16] {
        self.volume.identity()
    }

    /// Hand the device back.
    pub fn into_device(self) -> B {
        self.dev.into_inner()
    }

    fn rock_ridge(&self) -> bool {
        self.volume.system_use.rock_ridge
    }

    fn block_bytes(&self) -> u64 {
        u64::from(self.volume.block_size)
    }

    /// Make [`Self::sector`] hold the 2048-byte sector at byte `at`.
    fn load_sector(&mut self, at: u64) -> Result<(), DriverError> {
        if self.sector_at != Some(at) {
            self.sector_at = None;
            read_whole(&mut self.dev, at, &mut self.sector[..])?;
            self.sector_at = Some(at);
        }
        Ok(())
    }

    /// The record at byte `address`, copied out.
    fn record_at(&mut self, address: u64) -> Result<Raw, DriverError> {
        let sector = address - address % SECTOR;
        self.load_sector(sector)?;
        let within = usize::try_from(address - sector).map_err(|_| DriverError::DeviceFault)?;
        Raw::parse(address, &self.sector[within..], self.volume.flavour)
    }

    /// The record at byte `address`, read alone: the sector of the
    /// directory being walked stays held.
    fn record_alone(&mut self, address: u64) -> Result<Raw, DriverError> {
        let mut bytes = [0u8; RECORD_MAX];
        let len = usize::try_from(SECTOR - address % SECTOR)
            .map_or(RECORD_MAX, |left| left.min(RECORD_MAX));
        read_whole(&mut self.dev, address, &mut bytes[..len])?;
        Raw::parse(address, &bytes[..len], self.volume.flavour)
    }

    /// The next record of `walk`, `None` at the directory's end. A record
    /// never spans a sector: a zero length byte pads the rest of one.
    fn next_record(&mut self, walk: &mut Walk) -> Result<Option<Raw>, DriverError> {
        while walk.pos < walk.end {
            let sector = walk.pos - walk.pos % SECTOR;
            self.load_sector(sector)?;
            let within =
                usize::try_from(walk.pos - sector).map_err(|_| DriverError::DeviceFault)?;
            let len = u64::from(self.sector[within]);
            if len == 0 {
                walk.pos = sector + SECTOR;
                continue;
            }
            let end = walk.pos + len;
            if end > (sector + SECTOR).min(walk.end) {
                return Err(DriverError::DeviceFault);
            }
            let raw = self.record_at(walk.pos)?;
            walk.pos = end;
            return Ok(Some(raw));
        }
        Ok(None)
    }

    /// The system use `raw` declares: the RISC OS field opening it, then
    /// the SUSP entries after the skip, followed through their continuation
    /// areas.
    fn system_use(&mut self, raw: &Raw) -> Result<SystemUse, DriverError> {
        let mut found = SystemUse::new();
        let field = raw.record.system_use(raw.bytes());
        let mounted = self.volume.system_use;
        if mounted.archimedes {
            found.read_archimedes(field, raw.record.is_dir());
        }
        let Some(skip) = mounted.susp_skip else {
            found.read_apple(field);
            return Ok(found);
        };
        let Some(entries) = field.get(usize::from(skip)..) else {
            return Ok(found);
        };
        let mut next = found.read_entries(entries)?;
        let mut hops = 0;
        while let Next::Continue(area) = next {
            hops += 1;
            let len = usize::try_from(area.len).map_err(|_| DriverError::DeviceFault)?;
            if hops > MAX_CONTINUATIONS || len > SECTOR_LEN {
                return Err(DriverError::DeviceFault);
            }
            let at = self
                .volume
                .block_offset(area.block)
                .checked_add(u64::from(area.offset))
                .ok_or(DriverError::DeviceFault)?;
            let bytes = self.continuation(at, len)?;
            next = found.read_entries(bytes)?;
        }
        Ok(found)
    }

    /// The `len` bytes of the continuation area at byte `at`: from the held
    /// sector where they lie in one.
    fn continuation(&mut self, at: u64, len: usize) -> Result<&[u8], DriverError> {
        let sector = at - at % SECTOR;
        let within = usize::try_from(at - sector).map_err(|_| DriverError::DeviceFault)?;
        if within + len > SECTOR_LEN {
            self.area_at = None;
            read_whole(&mut self.dev, at, &mut self.area[..len])?;
            return Ok(&self.area[..len]);
        }
        if self.area_at != Some(sector) {
            self.area_at = None;
            read_whole(&mut self.dev, sector, &mut self.area[..])?;
            self.area_at = Some(sector);
        }
        Ok(&self.area[within..within + len])
    }

    /// The node the record `raw` and the `parts` of its file are, `None` for
    /// one this driver does not serve: a device, pipe or socket Rock Ridge
    /// records, or a sparse file whose `SF` entry is malformed.
    fn resolve(
        &mut self,
        raw: &Raw,
        found: SystemUse,
        parts: Vec<Part>,
    ) -> Result<Option<Node>, DriverError> {
        let flavour = self.volume.flavour;
        let xar = self.xar(raw)?;
        let kind = match (found.posix, found.child, &found.link) {
            (_, Some(_), _) => NodeKind::Directory,
            (Some(posix), None, _) => match posix.mode & S_IFMT {
                S_IFDIR => NodeKind::Directory,
                S_IFREG => NodeKind::RegularFile,
                S_IFLNK => NodeKind::Symlink,
                _ => return Ok(None),
            },
            (None, None, Some(_)) => NodeKind::Symlink,
            (None, None, None) if raw.record.is_dir() => NodeKind::Directory,
            (None, None, None) => NodeKind::RegularFile,
        };
        // Rock Ridge's stamps, then the extended attribute record's, then the
        // instant the record was written; an instant none keeps is the epoch.
        let recorded = record_date(raw, flavour);
        let stamp = |kept: [Option<Time64>; 2]| {
            kept.into_iter()
                .flatten()
                .next()
                .or(recorded)
                .unwrap_or(Time64::UNIX_EPOCH)
        };
        let times = NodeTimes {
            created: stamp([found.stamps.created, xar.created]),
            modified: stamp([found.stamps.modified, xar.modified]),
            accessed: stamp([found.stamps.accessed, None]),
            changed: stamp([found.stamps.changed, None]),
        };
        let nlink = found
            .posix
            .map_or(NodeInfo::SINGLE_NAME, |posix| posix.links.max(1));
        let security = found
            .posix
            .map(|Posix { mode, uid, gid, .. }| NodeSecurity::new(mode & 0o7777, uid, gid))
            .or(xar.security);
        let (content, size, allocated) = match kind {
            NodeKind::Directory => {
                let (start, end, allocated) = if let Some(child) = found.child {
                    self.directory_at(child)?
                } else {
                    let start = self.data_start(&raw.record);
                    let len = u64::from(raw.record.data_len);
                    (start, start + len, self.allocation(&raw.record))
                };
                (Content::Directory { start, end }, 0, allocated)
            }
            NodeKind::Symlink => {
                let target = match &found.link {
                    Some(target) => copied(target.bytes())?,
                    None => Vec::new(),
                };
                let len = target.len() as u64;
                (Content::Link(target), len, 0)
            }
            NodeKind::RegularFile => {
                let Some(map) = file_map(parts, &found) else {
                    return Ok(None);
                };
                let allocated = map
                    .parts
                    .iter()
                    .map(|part| {
                        let blocks =
                            u64::from(part.xar_blocks) + part.len.div_ceil(self.block_bytes());
                        blocks * self.block_bytes()
                    })
                    .sum();
                let size = map.size;
                (Content::File(map), size, allocated)
            }
        };
        Ok(Some(Node {
            info: NodeInfo {
                kind,
                nlink,
                ino: node_id(raw.address).raw(),
                size,
                allocated,
                times,
                content_gen: NodeInfo::NO_CONTENT_GEN,
            },
            security,
            acorn: found.acorn,
            finder: found.finder,
            amiga: found.amiga,
            content,
        }))
    }

    /// The byte address a record's data starts at, past its extended
    /// attribute record.
    fn data_start(&self, record: &Record) -> u64 {
        self.volume.block_offset(record.extent) + u64::from(record.xar_blocks) * self.block_bytes()
    }

    /// The bytes of whole blocks a record's extent occupies.
    fn allocation(&self, record: &Record) -> u64 {
        let blocks =
            u64::from(record.xar_blocks) + u64::from(record.data_len).div_ceil(self.block_bytes());
        blocks * self.block_bytes()
    }

    /// The directory whose `.` record opens `extent`, as a Rock Ridge `CL` or
    /// `PL` names one: its records' byte range and allocation.
    fn directory_at(&mut self, extent: u32) -> Result<(u64, u64, u64), DriverError> {
        let dot = self.record_alone(self.volume.block_offset(extent))?;
        if dot.identifier() != [0] || !dot.record.is_dir() || dot.record.extent != extent {
            return Err(DriverError::DeviceFault);
        }
        let start = self.data_start(&dot.record);
        Ok((
            start,
            start + u64::from(dot.record.data_len),
            self.allocation(&dot.record),
        ))
    }

    /// What a record's extended attribute record says of its owner, group,
    /// permissions and dates, when it has one.
    fn xar(&mut self, raw: &Raw) -> Result<Xar, DriverError> {
        if raw.record.xar_blocks == 0 {
            return Ok(Xar::default());
        }
        let mut bytes = [0u8; XAR_READ_LEN];
        read_whole(
            &mut self.dev,
            self.volume.block_offset(raw.record.extent),
            &mut bytes,
        )?;
        Ok(Xar::parse(&bytes, raw.record.flags & FLAG_PROTECTION != 0))
    }

    /// The parts of the file whose first record is `first`, its later
    /// records read from `walk`: every one but the last says another follows,
    /// and each repeats the file's identifier.
    fn parts(&mut self, first: &Raw, walk: &mut Walk) -> Result<Vec<Part>, DriverError> {
        let mut parts = Vec::new();
        let mut record = first.record;
        loop {
            if parts.len() >= MAX_PARTS {
                return Err(DriverError::DeviceFault);
            }
            let part = Part {
                extent: record.extent,
                xar_blocks: record.xar_blocks,
                unit_blocks: record.unit_blocks,
                gap_blocks: record.gap_blocks,
                len: u64::from(record.data_len),
            };
            if !tairix_util::fallible::push(&mut parts, part) {
                return Err(DriverError::OutOfMemory);
            }
            if record.flags & FLAG_MULTI_EXTENT == 0 {
                return Ok(parts);
            }
            let next = self.next_record(walk)?.ok_or(DriverError::DeviceFault)?;
            if next.identifier() != first.identifier() {
                return Err(DriverError::DeviceFault);
            }
            record = next.record;
        }
    }

    /// The node `id` names. A multi-extent file's later records follow its
    /// first, as far as its most parts reach: the listing that named the
    /// node found them within its directory.
    fn node(&mut self, id: NodeId) -> Result<Node, DriverError> {
        let raw = self.record_at(record_address(id).ok_or(DriverError::NotFound)?)?;
        if id != self.root && raw.record.is_self_or_parent(raw.bytes()) {
            return Err(DriverError::NotFound);
        }
        let found = self.system_use(&raw)?;
        let after = raw.address + raw.record.len as u64;
        let mut walk = Walk {
            pos: after,
            end: after - after % SECTOR + MAX_PARTS as u64 * SECTOR,
        };
        let parts = self.parts(&raw, &mut walk)?;
        self.resolve(&raw, found, parts)?
            .ok_or(DriverError::NotFound)
    }

    /// The directory `dir` names, as its records' byte range.
    fn directory(&mut self, dir: NodeId) -> Result<Walk, DriverError> {
        match self.node(dir)?.content {
            Content::Directory { start, end } => Ok(Walk { pos: start, end }),
            Content::File(_) | Content::Link(_) => Err(DriverError::Unsupported),
        }
    }

    /// The records of the directory in `extent`: the root's where its record
    /// places them, any other's from the `.` record opening the extent.
    fn walk_of(&mut self, extent: u32) -> Result<Walk, DriverError> {
        if extent == self.root_extent()? {
            return self.directory(self.root);
        }
        let (pos, end, _) = self.directory_at(extent)?;
        Ok(Walk { pos, end })
    }

    /// The extent the root directory's record names.
    fn root_extent(&self) -> Result<u32, DriverError> {
        Ok(self.volume.mounted().root(self.volume.flavour)?.extent)
    }

    /// The extent of the parent of the directory whose records `walk` spans:
    /// the one its `..` record names or, for a directory Rock Ridge
    /// relocated, the one the `PL` entry there names.
    fn parent_extent(&mut self, mut walk: Walk) -> Result<u32, DriverError> {
        let dot = self
            .next_record(&mut walk)?
            .ok_or(DriverError::DeviceFault)?;
        let dotdot = self
            .next_record(&mut walk)?
            .ok_or(DriverError::DeviceFault)?;
        if dot.identifier() != [0] || dotdot.identifier() != [1] || !dotdot.record.is_dir() {
            return Err(DriverError::DeviceFault);
        }
        let found = self.system_use(&dotdot)?;
        Ok(found.parent.unwrap_or(dotdot.record.extent))
    }

    /// What the records at `walk`'s position list as, consuming the parts of
    /// a multi-extent file — a node resolved only where it is the one
    /// `wanted`. A record sharing the name `previous` listed is that file's
    /// older version, which ECMA-119 orders after the newest.
    fn listed(
        &mut self,
        walk: &mut Walk,
        previous: &mut Option<Name>,
        wanted: Wanted<'_>,
    ) -> Result<Option<Listed>, DriverError> {
        let Some(raw) = self.next_record(walk)? else {
            return Ok(None);
        };
        let parts = self.parts(&raw, walk)?;
        if raw.record.is_self_or_parent(raw.bytes()) || raw.record.flags & FLAG_ASSOCIATED != 0 {
            return Ok(Some(Listed::Skipped));
        }
        let found = self.system_use(&raw)?;
        if found.relocated {
            return Ok(Some(Listed::Skipped));
        }
        let name = self.listed_name(&raw, &found);
        let shadowed = !self.rock_ridge()
            && previous
                .as_ref()
                .is_some_and(|previous| previous.as_bytes() == name.as_bytes());
        if shadowed {
            return Ok(Some(Listed::Skipped));
        }
        *previous = Some(name.clone());
        let unwanted = match wanted {
            Wanted::All => false,
            Wanted::Named(wanted) => !self.name_matching().matches(name.as_bytes(), wanted),
            Wanted::Directory(extent) => {
                let holds = found
                    .child
                    .or_else(|| raw.record.is_dir().then_some(raw.record.extent));
                holds != Some(extent)
            }
        };
        if unwanted {
            return Ok(Some(Listed::Skipped));
        }
        let Some(node) = self.resolve(&raw, found, parts)? else {
            return Ok(Some(Listed::Skipped));
        };
        Ok(Some(Listed::Entry {
            name,
            node,
            id: node_id(raw.address),
        }))
    }

    /// The name a record lists under: Rock Ridge's own where it records
    /// one, else the tree's, with RISC OS's `!` restored.
    fn listed_name(&self, raw: &Raw, found: &SystemUse) -> Name {
        let alternate = found
            .name
            .as_ref()
            .and_then(susp::AlternateName::get)
            .map(name::alternate_name);
        let mut name = alternate.unwrap_or_else(|| {
            name::tree_name(raw.identifier(), self.volume.tree, raw.record.is_dir())
        });
        if found.bang {
            name.restore_bang();
        }
        name
    }

    /// Read `buf.len()` stored bytes of `map` from stored offset `offset`.
    fn read_stored(
        &mut self,
        map: &FileMap,
        mut offset: u64,
        mut buf: &mut [u8],
    ) -> Result<(), DriverError> {
        for part in &map.parts {
            if buf.is_empty() {
                break;
            }
            if offset >= part.len {
                offset -= part.len;
                continue;
            }
            let take =
                usize::try_from((part.len - offset).min(buf.len() as u64)).unwrap_or(buf.len());
            let (now, rest) = buf.split_at_mut(take);
            self.read_part(part, offset, now)?;
            buf = rest;
            offset = 0;
        }
        if buf.is_empty() {
            Ok(())
        } else {
            Err(DriverError::DeviceFault)
        }
    }

    /// Read `buf.len()` bytes of `part` from `offset` into it: one span, or
    /// file unit by file unit when it is interleaved.
    fn read_part(
        &mut self,
        part: &Part,
        mut offset: u64,
        mut buf: &mut [u8],
    ) -> Result<(), DriverError> {
        let bs = self.block_bytes();
        if part.unit_blocks == 0 {
            let at = (u64::from(part.extent) + u64::from(part.xar_blocks)) * bs + offset;
            return read_whole(&mut self.dev, at, buf);
        }
        // The extended attribute record fills the first file unit of its own,
        // and the data's units follow, each after a gap.
        let unit = u64::from(part.unit_blocks) * bs;
        let stride = u64::from(part.unit_blocks) + u64::from(part.gap_blocks);
        let first = u64::from(part.xar_blocks != 0);
        while !buf.is_empty() {
            let index = offset / unit;
            let within = offset % unit;
            let block = u64::from(part.extent) + (index + first) * stride;
            let take = usize::try_from((unit - within).min(buf.len() as u64)).unwrap_or(buf.len());
            let (now, rest) = buf.split_at_mut(take);
            read_whole(&mut self.dev, block * bs + within, now)?;
            buf = rest;
            offset += take as u64;
        }
        Ok(())
    }

    /// Read decompressed bytes of the zisofs file `id` from `offset`.
    fn read_compressed(
        &mut self,
        id: NodeId,
        map: &FileMap,
        layout: zisofs::Layout,
        mut offset: u64,
        mut buf: &mut [u8],
    ) -> Result<(), DriverError> {
        while !buf.is_empty() {
            let index = offset >> layout.log2_block;
            let within = usize::try_from(offset & (layout.block_bytes() - 1)).unwrap_or(0);
            if !self
                .block
                .as_ref()
                .is_some_and(|b| b.file == id && b.index == index)
            {
                self.block = None;
                let bytes = self.decompress(map, layout, index)?;
                self.block = Some(Decompressed {
                    file: id,
                    index,
                    bytes,
                });
            }
            let block = self.block.as_ref().ok_or(DriverError::DeviceFault)?;
            let available = block.bytes.get(within..).ok_or(DriverError::DeviceFault)?;
            let take = available.len().min(buf.len());
            if take == 0 {
                return Err(DriverError::DeviceFault);
            }
            let (now, rest) = buf.split_at_mut(take);
            now.copy_from_slice(&available[..take]);
            buf = rest;
            offset += take as u64;
        }
        Ok(())
    }

    /// Block `index` of a zisofs file, decompressed: its pointers must lie
    /// in order within the stored bytes, and a block they give no bytes is
    /// all zero.
    fn decompress(
        &mut self,
        map: &FileMap,
        layout: zisofs::Layout,
        index: u64,
    ) -> Result<Vec<u8>, DriverError> {
        if index >= layout.blocks {
            return Err(DriverError::DeviceFault);
        }
        let mut pointers = [0u8; 8];
        self.read_stored(
            map,
            layout.table + index * zisofs::POINTER_LEN,
            &mut pointers,
        )?;
        let start = u64::from(u32::from_le_bytes([
            pointers[0],
            pointers[1],
            pointers[2],
            pointers[3],
        ]));
        let end = u64::from(u32::from_le_bytes([
            pointers[4],
            pointers[5],
            pointers[6],
            pointers[7],
        ]));
        let len = layout.block_len(index);
        let compressed = end.checked_sub(start).ok_or(DriverError::DeviceFault)?;
        if end > map.stored || compressed > zisofs::Layout::compressed_bound(len) {
            return Err(DriverError::DeviceFault);
        }
        let len = usize::try_from(len).map_err(|_| DriverError::DeviceFault)?;
        let mut out = tairix_util::fallible::filled(len, 0u8).ok_or(DriverError::OutOfMemory)?;
        if compressed != 0 {
            let compressed = usize::try_from(compressed).map_err(|_| DriverError::DeviceFault)?;
            let mut input =
                tairix_util::fallible::filled(compressed, 0u8).ok_or(DriverError::OutOfMemory)?;
            self.read_stored(map, start, &mut input)?;
            zisofs::inflate(&input, &mut out)?;
        }
        Ok(out)
    }
}

/// What an extended attribute record says of its file.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
struct Xar {
    security: Option<NodeSecurity>,
    created: Option<Time64>,
    modified: Option<Time64>,
}

impl Xar {
    /// The owner, group and permissions of a record whose protection flag
    /// is set, and the record's creation and modification dates.
    ///
    /// The permissions (ECMA-119 9.5.3) are most significant byte first, a
    /// set bit refusing what it names: 4 and 6 the owner's read and execute,
    /// 8 and 10 the group's, 12 and 14 everyone's. Nothing on a read-only
    /// medium is writable.
    fn parse(bytes: &[u8; XAR_READ_LEN], protected: bool) -> Self {
        let security = protected
            .then(|| {
                let owner = both_u16(bytes, 0)?;
                let group = both_u16(bytes, 4)?;
                let permissions = u16::from_be_bytes([bytes[8], bytes[9]]);
                let mode = [
                    (4, 0o400),
                    (6, 0o100),
                    (8, 0o040),
                    (10, 0o010),
                    (12, 0o004),
                    (14, 0o001),
                ]
                .into_iter()
                .filter(|&(bit, _)| permissions & (1 << bit) == 0)
                .fold(0u32, |mode, (_, granted)| mode | granted);
                Some(NodeSecurity::new(mode, u32::from(owner), u32::from(group)))
            })
            .flatten();
        let date = |at: usize| -> Option<Time64> {
            let raw: &[u8; 17] = bytes.get(at..at + 17)?.try_into().ok()?;
            time::long_date(raw)
        };
        Self {
            security,
            created: date(10),
            modified: date(27),
        }
    }
}

/// A record's recording date, in its flavour's layout.
fn record_date(raw: &Raw, flavour: Flavour) -> Option<Time64> {
    let bytes = raw.bytes();
    match flavour {
        Flavour::Iso9660 => bytes
            .get(18..25)?
            .try_into()
            .ok()
            .and_then(time::record_date),
        Flavour::HighSierra => bytes
            .get(18..24)?
            .try_into()
            .ok()
            .and_then(time::high_sierra_date),
    }
}

/// Fill `buf` from byte `at`: a range the device does not hold is damage,
/// since every structure this driver reads is one the volume points it at.
fn read_whole<B: Block>(
    dev: &mut ByteDevice<B>,
    at: u64,
    buf: &mut [u8],
) -> Result<(), DriverError> {
    match dev.read(at, buf) {
        Err(DriverError::LengthOutOfRange) => Err(DriverError::DeviceFault),
        other => other,
    }
}

/// A sector-sized buffer on the heap, off the kernel stack this driver may
/// run on.
fn heap_sector() -> Result<Box<[u8; SECTOR_LEN]>, DriverError> {
    tairix_util::fallible::boxed_array(0u8).ok_or(DriverError::OutOfMemory)
}

/// `bytes`, copied fallibly.
fn copied(bytes: &[u8]) -> Result<Vec<u8>, DriverError> {
    tairix_util::fallible::collected(bytes.len(), bytes.iter().copied())
        .ok_or(DriverError::OutOfMemory)
}

impl<B: Block> FilesystemRead for Iso9660<B> {
    fn root(&self) -> NodeId {
        self.root
    }

    fn name_matching(&self) -> NameMatching {
        // Rock Ridge names are POSIX names; every other tree is case-blind
        // as the systems that write it are.
        if self.rock_ridge() {
            NameMatching::Exact
        } else {
            NameMatching::AsciiCaseInsensitive
        }
    }

    fn node_identity(&self) -> NodeIdentity {
        NodeIdentity::Stable
    }

    fn node_info(&mut self, node: NodeId) -> Result<NodeInfo, DriverError> {
        self.node(node).map(|node| node.info)
    }

    fn lookup(&mut self, dir: NodeId, name: &[u8]) -> Result<NodeId, DriverError> {
        let mut walk = self.directory(dir)?;
        let mut previous = None;
        while let Some(listed) = self.listed(&mut walk, &mut previous, Wanted::Named(name))? {
            if let Listed::Entry { id, .. } = listed {
                return Ok(id);
            }
        }
        Err(DriverError::NotFound)
    }

    fn read_at(&mut self, file: NodeId, offset: u64, buf: &mut [u8]) -> Result<usize, DriverError> {
        let mut map = match self.file.take() {
            Some((id, map)) if id == file => map,
            _ => match self.node(file)?.content {
                Content::File(map) => map,
                Content::Directory { .. } | Content::Link(_) => {
                    return Err(DriverError::Unsupported)
                }
            },
        };
        let result = self.read_mapped(file, &mut map, offset, buf);
        self.file = Some((file, map));
        result
    }

    fn seek_region(
        &mut self,
        file: NodeId,
        offset: u64,
        region: SeekRegion,
    ) -> Result<Option<u64>, DriverError> {
        match self.node(file)?.content {
            Content::File(map) if !map.sparse => Ok(region.in_solid_file(offset, map.size)),
            Content::File(_) | Content::Directory { .. } | Content::Link(_) => {
                Err(DriverError::Unsupported)
            }
        }
    }

    fn read_link(&mut self, link: NodeId, out: &mut [u8]) -> Result<usize, DriverError> {
        let Content::Link(target) = self.node(link)?.content else {
            return Err(DriverError::Unsupported);
        };
        let slot = out
            .get_mut(..target.len())
            .ok_or(DriverError::BufferTooSmall)?;
        slot.copy_from_slice(&target);
        Ok(target.len())
    }

    fn read_dir(
        &mut self,
        dir: NodeId,
        cursor: u64,
        after: &[u8],
        visit: &mut dyn FnMut(&DirEntry, &[u8]) -> DirVisit,
    ) -> Result<(), DriverError> {
        let directory = self.directory(dir)?;
        let start = directory.pos;
        let mut walk = Walk {
            pos: start.saturating_add(cursor).min(directory.end),
            end: directory.end,
        };
        // A listing resumed after a name skips that name's older versions.
        let mut previous = if cursor == 0 {
            None
        } else {
            Name::from_bytes(after)
        };
        loop {
            let Some(listed) = self.listed(&mut walk, &mut previous, Wanted::All)? else {
                return Ok(());
            };
            let Listed::Entry { name, node, id } = listed else {
                continue;
            };
            let entry = DirEntry {
                node: id,
                info: node.info,
                next_cursor: walk.pos - start,
            };
            if visit(&entry, name.as_bytes()) == DirVisit::Stop {
                return Ok(());
            }
        }
    }

    fn parent(&mut self, dir: NodeId) -> Result<NodeId, DriverError> {
        if dir == self.root {
            return Ok(dir);
        }
        let walk = self.directory(dir)?;
        let up = self.parent_extent(walk)?;
        if up == self.root_extent()? {
            return Ok(self.root);
        }
        let walk = self.walk_of(up)?;
        let grandparent = self.parent_extent(walk)?;
        let mut walk = self.walk_of(grandparent)?;
        let mut previous = None;
        while let Some(listed) = self.listed(&mut walk, &mut previous, Wanted::Directory(up))? {
            if let Listed::Entry { id, .. } = listed {
                return Ok(id);
            }
        }
        // No entry of the grandparent holds the parent: the tree disagrees
        // with itself.
        Err(DriverError::DeviceFault)
    }

    /// A finished disc changes under no one, so a hold needs no record: the
    /// node's own id names it for as long as the disc is mounted.
    fn pin(&mut self, node: NodeId) -> Result<NodeId, DriverError> {
        self.node(node).map(|_| node)
    }

    fn unpin(&mut self, _pinned: NodeId) -> Result<Unpinned, DriverError> {
        Ok(Unpinned::Released)
    }

    fn release_pins(&mut self) -> Result<(), DriverError> {
        Ok(())
    }
}

impl<B: Block> Iso9660<B> {
    /// Read the file `file`, mapped as `map`, from `offset`: a zisofs file's
    /// header is read, and checked against its record, on its first read.
    fn read_mapped(
        &mut self,
        file: NodeId,
        map: &mut FileMap,
        offset: u64,
        buf: &mut [u8],
    ) -> Result<usize, DriverError> {
        if map.sparse {
            return Err(DriverError::Unsupported);
        }
        let Some(left) = map.size.checked_sub(offset).filter(|&left| left > 0) else {
            return Ok(0);
        };
        let take = usize::try_from(left).map_or(buf.len(), |left| left.min(buf.len()));
        if let Compression::Zisofs(zf) = map.compression {
            let mut header = [0u8; zisofs::HEADER_LEN];
            self.read_stored(map, 0, &mut header)?;
            let layout =
                zisofs::Layout::parse(&header, zf, map.stored).ok_or(DriverError::DeviceFault)?;
            map.compression = Compression::Mapped(layout);
        }
        match map.compression {
            Compression::Mapped(layout) => {
                self.read_compressed(file, map, layout, offset, &mut buf[..take])?;
            }
            Compression::None | Compression::Zisofs(_) => {
                self.read_stored(map, offset, &mut buf[..take])?;
            }
        }
        Ok(take)
    }
}

/// How the file `parts` hold is read, as its record's system use declares;
/// `None` for a sparse file whose `SF` entry is malformed.
fn file_map(parts: Vec<Part>, found: &SystemUse) -> Option<FileMap> {
    let stored = parts.iter().map(|part| part.len).sum();
    let (size, compression) = match (found.sparse, found.zisofs) {
        (Sparse::Sized(size), _) => (size, Compression::None),
        (Sparse::Malformed, _) => return None,
        (Sparse::No, Some(zf)) => (u64::from(zf.size), Compression::Zisofs(zf)),
        (Sparse::No, None) => (stored, Compression::None),
    };
    Some(FileMap {
        parts,
        stored,
        size,
        compression,
        sparse: found.sparse != Sparse::No,
    })
}

/// The id of the record at byte `address`: one past it, since no node may
/// be [`NodeId::NONE`].
fn node_id(address: u64) -> NodeId {
    NodeId::from_raw(address + 1)
}

/// The byte address of the record `id` names.
fn record_address(id: NodeId) -> Option<u64> {
    id.raw().checked_sub(1)
}

impl<B: Block> FilesystemWrite for Iso9660<B> {
    fn create(
        &mut self,
        _dir: NodeId,
        _name: &[u8],
        _kind: NodeKind,
        _record: &NodeSecurity,
    ) -> Result<NodeId, DriverError> {
        Err(DriverError::ReadOnly)
    }

    fn create_link(
        &mut self,
        _dir: NodeId,
        _name: &[u8],
        _target: &[u8],
        _record: &NodeSecurity,
    ) -> Result<NodeId, DriverError> {
        Err(DriverError::ReadOnly)
    }

    fn link(&mut self, _dir: NodeId, _name: &[u8], _node: NodeId) -> Result<(), DriverError> {
        Err(DriverError::ReadOnly)
    }

    fn write_at(
        &mut self,
        _file: NodeId,
        _offset: u64,
        _data: &[u8],
    ) -> Result<usize, DriverError> {
        Err(DriverError::ReadOnly)
    }

    fn truncate(&mut self, _file: NodeId, _size: u64) -> Result<(), DriverError> {
        Err(DriverError::ReadOnly)
    }

    fn punch(&mut self, _file: NodeId, _offset: u64, _len: u64) -> Result<(), DriverError> {
        Err(DriverError::ReadOnly)
    }

    fn remove(&mut self, _dir: NodeId, _name: &[u8]) -> Result<(), DriverError> {
        Err(DriverError::ReadOnly)
    }

    fn rename(
        &mut self,
        _src_dir: NodeId,
        _src_name: &[u8],
        _dst_dir: NodeId,
        _dst_name: &[u8],
    ) -> Result<(), DriverError> {
        Err(DriverError::ReadOnly)
    }

    fn flush(&mut self) -> Result<(), DriverError> {
        Ok(())
    }
}

/// Rock Ridge records POSIX owners and modes, and an extended attribute
/// record an owner, group and permissions; a node with neither reports the
/// record an ownerless format stands in with.
impl<B: Block> FilesystemSecurity for Iso9660<B> {
    fn security(&mut self, node: NodeId) -> Result<NodeSecurity, DriverError> {
        let node = self.node(node)?;
        Ok(node
            .security
            .unwrap_or(NodeSecurity::unrecorded(node.info.kind)))
    }

    fn set_security(&mut self, _node: NodeId, _security: NodeSecurity) -> Result<(), DriverError> {
        Err(DriverError::ReadOnly)
    }

    fn records_ownership(&mut self, node: NodeId) -> Result<bool, DriverError> {
        Ok(self.node(node)?.security.is_some())
    }
}

/// The foreign records a disc keeps beside a file: RISC OS's load and
/// execution words, filetype, datestamp and attributes, as the `acorn` keys
/// ADFS presents them under; Apple's type, creator and Finder flags; the
/// Amiga protection bits and comment.
impl<B: Block> FilesystemAttrs for Iso9660<B> {
    fn get_attr(
        &mut self,
        node: NodeId,
        key: &[u8],
        value_out: &mut [u8],
    ) -> Result<Option<usize>, DriverError> {
        let key = tairix_fsmeta::AttrKey::parse(key).map_err(|_| DriverError::OutOfRange)?;
        value(&self.node(node)?, key.as_bytes(), value_out)
    }

    fn set_attr(&mut self, _node: NodeId, _key: &[u8], _value: &[u8]) -> Result<(), DriverError> {
        Err(DriverError::ReadOnly)
    }

    fn list_attr(
        &mut self,
        node: NodeId,
        index: u64,
        key_out: &mut [u8],
    ) -> Result<Option<usize>, DriverError> {
        let node = self.node(node)?;
        let Some(key) = keys(&node).nth(usize::try_from(index).unwrap_or(usize::MAX)) else {
            return Ok(None);
        };
        let out = key_out
            .get_mut(..key.len())
            .ok_or(DriverError::BufferTooSmall)?;
        out.copy_from_slice(key);
        Ok(Some(key.len()))
    }

    fn remove_attr(&mut self, _node: NodeId, _key: &[u8]) -> Result<(), DriverError> {
        Err(DriverError::ReadOnly)
    }
}

impl<B: Block> FilesystemAttrsProvider for Iso9660<B> {
    fn attrs_fs(&mut self) -> Option<&mut dyn FilesystemAttrsFs> {
        Some(self)
    }
}

impl<B: Block> FilesystemStats for Iso9660<B> {
    fn stats(&mut self) -> Result<VolumeStats, DriverError> {
        Ok(VolumeStats {
            block_size: self.volume.block_size,
            in_reserve: false,
            total_blocks: u64::from(self.volume.volume_blocks),
            free_blocks: 0,
            avail_blocks: 0,
            files: 0,
            files_free: 0,
        })
    }
}
