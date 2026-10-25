//! TAIRiX UDF filesystem driver.
//!
//! Mounts an OSTA UDF 1.02 to 2.60 volume behind any [`Block`] device —
//! physical, sparable, virtual (write-once, through its allocation table)
//! and metadata partitions — through [`FilesystemRead`],
//! [`FilesystemWrite`](tairix_abi::driver::filesystem::FilesystemWrite),
//! [`FilesystemSecurity`], [`FilesystemAttrs`] and
//! [`FilesystemStats`]. A rewritable or overwritable volume is written in
//! place; [`Udf::write_refusal`] names why any other is mounted read-only.
//! [`Udf::format`] lays a new volume out. `docs/src/filesystem/udf.md`
//! describes the supported structures.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

mod dir;
mod ea;
mod entry;
mod fields;
mod integrity;
mod layout;
mod mkfs;
mod space;
mod write;

#[cfg(test)]
mod tests;

use alloc::vec::Vec;

use tairix_abi::driver::block::{Block, BlockClass};
use tairix_abi::driver::filesystem::{
    AclModel, DirEntry, DirVisit, FilesystemAttrs, FilesystemAttrsFs, FilesystemAttrsProvider,
    FilesystemRead, FilesystemSecurity, FilesystemStats, NameMatching, NodeId, NodeIdentity,
    NodeInfo, NodeKind, NodeSecurity, NodeTimes, Unpinned, VolumeHost, VolumeStats,
};
use tairix_abi::fs::{SeekRegion, FS_SYMLINK_MAX};
use tairix_abi::time::Time64;
use tairix_abi::{CapabilityId, DriverError, DriverHandle, DriverHost};
use tairix_bytedev::ByteDevice;
use tairix_collections::RadixTree;
use tairix_fsmeta::preset::mac::{self, FinderInfo};
use tairix_fsprobe::udf::{self, LongAd, Tag, TAG_FSD, TAG_IE};

use dir::{Fid, Name, CHAR_DELETED, CHAR_PARENT, FID_FIXED};
use entry::{AdForm, Entry, Extent, Holds, FLAG_SETGID, FLAG_SETUID, FLAG_STICKY};
use fields::Fields;
use integrity::Integrity;
use layout::{LbAddr, Map, Partitions, Physical, Run};

pub use mkfs::FormatOptions;

/// Per-driver `DriverHandle` marker returned by [`register`].
const REGISTER_HANDLE_MARKER: u64 = 0x5544_4600_0000_0001; // "UDF" + index

/// Most indirect entries one ICB is followed through.
const MAX_ICB_HOPS: usize = 64;

/// Most blocks a file set descriptor sequence is read across.
const MAX_FSD_BLOCKS: u32 = 16;

/// Most blocks scanned back from a write-once disc's last recorded block for
/// its virtual allocation table.
const VAT_SEARCH: u32 = 32;

/// Most bytes of an extended attribute file read: a validation bound on
/// untrusted media.
const MAX_ATTRIBUTE_FILE: usize = 1 << 20;

/// The identifier a virtual allocation table of UDF 1.50 ends in.
const VAT_150_ID: &[u8] = b"*UDF Virtual Alloc Tbl";

/// Bytes of a sparing table before its entries.
const SPARING_HEADER: usize = 56;

/// The UDF revision this driver writes: the newest it knows.
pub(crate) const WRITE_REVISION: u16 = 0x0260;

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

/// A mounted UDF volume.
pub struct Udf<B: Block> {
    dev: ByteDevice<B>,
    volume: udf::Volume,
    partitions: Partitions,
    block_size: u32,
    root: LbAddr,
    /// Where the file set descriptor lives, for the volume's identity.
    file_set: LongAd,
    integrity: Integrity,
    /// Why the volume is not written, or `None` when it is.
    refusal: Option<&'static str>,
    /// The file most recently read, its extents mapped.
    file: Option<(NodeId, Mapped)>,
    write: write::State,
    host: Option<&'static dyn VolumeHost>,
    /// The nodes the kernel holds, by id.
    held: RadixTree<Hold>,
}

/// The kernel's hold on one node.
struct Hold {
    /// Holds outstanding: never none while the hold is recorded.
    count: u32,
    /// The node lost its last name while held, so its last unpin frees it.
    unnamed: bool,
}

/// A file's extents and the offset each starts at.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Mapped {
    pub extents: Vec<Extent>,
    pub starts: Vec<u64>,
}

impl Mapped {
    fn of(extents: Vec<Extent>) -> Result<Self, DriverError> {
        let mut starts = Vec::new();
        if !tairix_util::fallible::reserve(&mut starts, extents.len()) {
            return Err(DriverError::OutOfMemory);
        }
        let mut at = 0u64;
        for extent in &extents {
            starts.push(at);
            at += u64::from(extent.bytes);
        }
        Ok(Self { extents, starts })
    }

    /// The extent byte `offset` lies in, and its place in it.
    fn find(&self, offset: u64) -> Option<(usize, u64)> {
        let index = self
            .starts
            .partition_point(|&start| start <= offset)
            .checked_sub(1)?;
        let within = offset - self.starts[index];
        (within < u64::from(self.extents[index].bytes)).then_some((index, within))
    }
}

/// What [`Udf::walk_dir`] hands each live entry to: the entry's descriptor,
/// its name and where the next begins.
type DirVisitor<'a, B> =
    dyn FnMut(&mut Udf<B>, &Fid, &Name, u64) -> Result<DirVisit, DriverError> + 'a;

/// A directory's stream being read, a block at a time.
pub(crate) struct DirReader {
    mapped: Mapped,
    size: u64,
    /// Which block of the stream [`Self::block`] holds.
    held: Option<u64>,
    block: Vec<u8>,
}

/// A resolved node: its entry, the block it was read from, and where.
pub(crate) struct Resolved {
    pub entry: Entry,
    pub block: Vec<u8>,
    pub addr: LbAddr,
}

/// A node's id, from the address of its ICB.
pub(crate) fn node_of(addr: LbAddr) -> NodeId {
    NodeId::from_raw(((u64::from(addr.partition) + 1) << 32) | u64::from(addr.block))
}

/// The ICB address a node id names, `None` for one no node has.
pub(crate) fn addr_of(node: NodeId) -> Option<LbAddr> {
    let partition = u16::try_from((node.raw() >> 32).checked_sub(1)?).ok()?;
    Some(LbAddr {
        block: u32::try_from(node.raw() & 0xFFFF_FFFF).ok()?,
        partition,
    })
}

/// The kind a file type is served as, `None` for one this system has no
/// node for: a device, pipe, socket or stream directory.
pub(crate) fn kind_of(file_type: u8) -> Option<NodeKind> {
    match file_type {
        entry::TYPE_DIRECTORY => Some(NodeKind::Directory),
        // Unspecified, a file of bytes, a real-time file, the allocation
        // table: bytes all.
        0 | entry::TYPE_FILE | 249 | entry::TYPE_VAT => Some(NodeKind::RegularFile),
        entry::TYPE_SYMLINK => Some(NodeKind::Symlink),
        _ => None,
    }
}

impl<B: Block> Udf<B> {
    /// Mount the volume `device` holds, from the session its disc layout
    /// names last.
    ///
    /// # Errors
    ///
    /// * [`DriverError::BadMagic`] if the device holds no UDF volume, or its
    ///   file set or root does not decode.
    /// * [`DriverError::Unsupported`] for a partition map of a kind no UDF
    ///   revision defines, or a device block size outside the storage
    ///   stack's bounds.
    /// * [`DriverError::OutOfMemory`] if a working buffer is refused.
    /// * Whatever the device reports for a read.
    pub fn open(device: B) -> Result<Self, DriverError> {
        let mut dev = ByteDevice::new(device)?;
        let layout = dev.device().disc_layout();
        let cost = dev.device().command_cost();
        let volume = udf::identify(&mut dev, layout)?.ok_or(DriverError::BadMagic)?;
        let block_size = volume.block_size();
        let mut fs = Self {
            dev,
            partitions: Partitions { maps: Vec::new() },
            block_size,
            root: LbAddr {
                block: 0,
                partition: 0,
            },
            file_set: volume.logical().file_set,
            integrity: Integrity::default(),
            refusal: None,
            file: None,
            write: write::State::new(cost),
            host: None,
            held: RadixTree::new(),
            volume,
        };
        fs.partitions = fs.build_partitions(layout)?;
        fs.root = fs.file_set_root()?;
        fs.integrity = integrity::read(&mut fs)?;
        fs.refusal = fs.refusal_cause();
        match kind_of(fs.resolve(fs.root)?.entry.file_type) {
            Some(NodeKind::Directory) => Ok(fs),
            _ => Err(DriverError::BadMagic),
        }
    }

    /// The volume's stable identity, as the volume probe derives it.
    #[must_use]
    pub fn volume_identity(&self) -> [u8; 16] {
        self.volume.identity()
    }

    /// Why the volume is mounted read-only, `None` when it is written: the
    /// media or the volume forbid it, or it uses a structure this driver
    /// reads but does not write.
    #[must_use]
    pub fn write_refusal(&self) -> Option<&'static str> {
        self.refusal
    }

    /// Hand the device back.
    pub fn into_device(self) -> B {
        self.dev.into_inner()
    }

    fn block_bytes(&self) -> u64 {
        u64::from(self.block_size)
    }

    /// Read logical block `addr` into `buf`, a block long.
    pub(crate) fn read_block(&mut self, addr: LbAddr, buf: &mut [u8]) -> Result<(), DriverError> {
        let sector = self.partitions.sector(addr)?;
        self.read_sector(sector, buf)
    }

    pub(crate) fn read_sector(&mut self, sector: u64, buf: &mut [u8]) -> Result<(), DriverError> {
        let at = sector
            .checked_mul(self.block_bytes())
            .ok_or(DriverError::DeviceFault)?;
        match self.dev.read(at, buf) {
            Err(DriverError::LengthOutOfRange) => Err(DriverError::DeviceFault),
            other => other,
        }
    }

    /// Resolve the partition maps the logical volume declares, in order.
    fn build_partitions(
        &mut self,
        layout: Option<tairix_abi::driver::block::DiscLayout>,
    ) -> Result<Partitions, DriverError> {
        let logical = self.volume.logical().clone();
        let table = logical.maps();
        let mut maps = Vec::new();
        let mut numbers = Vec::new();
        let mut pending = Vec::new();
        let mut at = 0usize;
        for _ in 0..logical.map_count {
            let kind = *table.get(at).ok_or(DriverError::BadMagic)?;
            let len = usize::from(*table.get(at + 1).ok_or(DriverError::BadMagic)?);
            let map = table
                .get(at..at + len)
                .filter(|_| len >= 6)
                .ok_or(DriverError::BadMagic)?;
            let number = Fields(map).u16(if kind == 1 { 4 } else { 38 })?;
            let physical = self
                .volume
                .partitions()
                .iter()
                .find(|partition| partition.number == number)
                .map(Physical::of)
                .ok_or(DriverError::BadMagic)?;
            if !tairix_util::fallible::reserve(&mut maps, 1)
                || !tairix_util::fallible::push(&mut numbers, number)
                || !tairix_util::fallible::reserve(&mut pending, 1)
            {
                return Err(DriverError::OutOfMemory);
            }
            match (kind, map.get(5..28)) {
                (1, _) => maps.push(Map::Physical(physical)),
                (2, Some(id)) if id.starts_with(b"*UDF Sparable Partition") => {
                    let table = self.sparing_table(map, &physical)?;
                    let packet = u32::from(Fields(map).u16(40)?);
                    if packet == 0 {
                        return Err(DriverError::BadMagic);
                    }
                    maps.push(Map::Sparable {
                        physical,
                        packet,
                        table,
                    });
                }
                (2, Some(id))
                    if id.starts_with(b"*UDF Virtual Partition")
                        || id.starts_with(b"*UDF Metadata Partition") =>
                {
                    // Resolved once every map they may sit on is.
                    maps.push(Map::Physical(physical));
                    pending.push((maps.len() - 1, map.to_vec()));
                }
                _ => return Err(DriverError::Unsupported),
            }
            at += len;
        }
        let mut partitions = Partitions { maps };
        let awaiting: Vec<usize> = pending.iter().map(|&(index, _)| index).collect();
        for virtual_first in [true, false] {
            for (index, map) in &pending {
                let virtual_map = map
                    .get(5..28)
                    .is_some_and(|id| id.starts_with(b"*UDF Virtual Partition"));
                if virtual_map != virtual_first {
                    continue;
                }
                let beneath = beneath_of(
                    &partitions.maps,
                    &numbers,
                    &awaiting,
                    numbers[*index],
                    virtual_map,
                )
                .ok_or(DriverError::BadMagic)?;
                let resolved = if virtual_map {
                    let table = self.virtual_table(&partitions, beneath, layout)?;
                    Map::Virtual { beneath, table }
                } else {
                    self.metadata_map(&partitions, beneath, map)?
                };
                partitions.maps[*index] = resolved;
            }
        }
        Ok(partitions)
    }

    /// The remappings a sparable partition's first readable sparing table
    /// records.
    fn sparing_table(
        &mut self,
        map: &[u8],
        physical: &Physical,
    ) -> Result<Vec<(u32, u32)>, DriverError> {
        let f = Fields(map);
        let tables = usize::from(f.u8(42)?).min(4);
        let size = usize::try_from(f.u32(44)?)
            .map_err(|_| DriverError::BadMagic)?
            .min(SPARING_HEADER + layout::MAX_SPARING_ENTRIES * 8);
        let blocks = size.div_ceil(self.block_size as usize).max(1);
        let mut bytes = zeroed_len(blocks * self.block_size as usize)?;
        for index in 0..tables {
            let sector = f.u32(48 + index * 4)?;
            if self.read_sector(u64::from(sector), &mut bytes).is_err() {
                continue;
            }
            let Some(tag) = Tag::verify(&bytes, sector) else {
                continue;
            };
            if tag.id != 0 || bytes.get(17..35) != Some(b"*UDF Sparing Table") {
                continue;
            }
            let t = Fields(&bytes);
            let entries = usize::from(t.u16(48)?);
            if entries > layout::MAX_SPARING_ENTRIES || SPARING_HEADER + entries * 8 > bytes.len() {
                continue;
            }
            let mut out = Vec::new();
            if !tairix_util::fallible::reserve(&mut out, entries) {
                return Err(DriverError::OutOfMemory);
            }
            for entry in 0..entries {
                let original = t.u32(SPARING_HEADER + entry * 8)?;
                let mapped = t.u32(SPARING_HEADER + 4 + entry * 8)?;
                // Available and defective spares remap nothing.
                if original < 0xFFFF_FFF0 && original < physical.length {
                    out.push((original, mapped));
                }
            }
            // The first entry naming a packet is the one that remaps it.
            out.sort_by_key(|&(original, _)| original);
            out.dedup_by_key(|&mut (original, _)| original);
            return Ok(out);
        }
        Err(DriverError::BadMagic)
    }

    /// A write-once partition's virtual allocation table: the newest, found
    /// by scanning back from the last block the disc recorded.
    fn virtual_table(
        &mut self,
        partitions: &Partitions,
        beneath: usize,
        layout: Option<tairix_abi::driver::block::DiscLayout>,
    ) -> Result<Vec<u32>, DriverError> {
        let physical = *partitions
            .physical(u16::try_from(beneath).map_err(|_| DriverError::BadMagic)?)
            .ok_or(DriverError::BadMagic)?;
        let device_block = u64::from(self.dev.block_size());
        let last_sector = layout
            .and_then(|layout| layout.last_recorded)
            .and_then(|last| (last + 1).checked_mul(device_block))
            .unwrap_or(self.dev.capacity())
            / self.block_bytes();
        let last = last_sector
            .checked_sub(1 + u64::from(physical.start))
            .ok_or(DriverError::BadMagic)?;
        let partition = u16::try_from(beneath).map_err(|_| DriverError::BadMagic)?;
        let saved = core::mem::replace(&mut self.partitions, partitions.clone());
        let found = self.find_vat(last, partition);
        self.partitions = saved;
        found
    }

    fn find_vat(&mut self, last: u64, partition: u16) -> Result<Vec<u32>, DriverError> {
        for back in 0..u64::from(VAT_SEARCH) {
            let Some(block) = last.checked_sub(back).and_then(|b| u32::try_from(b).ok()) else {
                break;
            };
            let addr = LbAddr { block, partition };
            let Ok(resolved) = self.resolve(addr) else {
                continue;
            };
            let entry = &resolved.entry;
            if entry.file_type != entry::TYPE_VAT && entry.file_type != 0 {
                continue;
            }
            let Ok(content) = self.whole_content(&resolved, layout::MAX_VAT_ENTRIES * 4 + 512)
            else {
                continue;
            };
            if entry.file_type == entry::TYPE_VAT {
                let header = usize::from(Fields(&content).u16(0)?);
                return words(content.get(header..).ok_or(DriverError::BadMagic)?);
            }
            if entry.file_type == 0
                && content.len() >= 36
                && content[content.len() - 36 + 1..].starts_with(VAT_150_ID)
            {
                return words(&content[..content.len() - 36]);
            }
        }
        Err(DriverError::BadMagic)
    }

    /// A metadata partition: its metadata file's extents, and its mirror's.
    fn metadata_map(
        &mut self,
        partitions: &Partitions,
        beneath: usize,
        map: &[u8],
    ) -> Result<Map, DriverError> {
        let f = Fields(map);
        let partition = u16::try_from(beneath).map_err(|_| DriverError::BadMagic)?;
        let saved = core::mem::replace(&mut self.partitions, partitions.clone());
        let main = self.metadata_runs(LbAddr {
            block: f.u32(40)?,
            partition,
        });
        let mirror = self.metadata_runs(LbAddr {
            block: f.u32(44)?,
            partition,
        });
        self.partitions = saved;
        // The mirror stands in for a main file that cannot be read.
        let file = main.or(mirror)?;
        Ok(Map::Metadata { beneath, file })
    }

    /// The runs of blocks the metadata file whose entry lies at `addr`
    /// occupies.
    fn metadata_runs(&mut self, addr: LbAddr) -> Result<Vec<Run>, DriverError> {
        let resolved = self.resolve(addr)?;
        let extents = self.extents_of(&resolved)?;
        let mut runs = Vec::new();
        if !tairix_util::fallible::reserve(&mut runs, extents.len()) {
            return Err(DriverError::OutOfMemory);
        }
        let mut first = 0u32;
        for extent in extents {
            if extent.holds != Holds::Data || extent.start.partition != addr.partition {
                return Err(DriverError::BadMagic);
            }
            let blocks = extent.bytes / self.block_size;
            runs.push(Run {
                first,
                block: extent.start.block,
                blocks,
            });
            first = first.checked_add(blocks).ok_or(DriverError::BadMagic)?;
        }
        Ok(runs)
    }

    /// The root directory the prevailing file set descriptor names.
    fn file_set_root(&mut self) -> Result<LbAddr, DriverError> {
        let start = self.file_set;
        let blocks = start
            .bytes()
            .div_ceil(self.block_size)
            .clamp(1, MAX_FSD_BLOCKS);
        let mut block = zeroed(self.block_size)?;
        let mut best: Option<(u32, LongAd)> = None;
        for index in 0..blocks {
            let addr = LbAddr {
                block: start
                    .block
                    .checked_add(index)
                    .ok_or(DriverError::BadMagic)?,
                partition: start.partition,
            };
            if self.read_block(addr, &mut block).is_err() {
                break;
            }
            match Tag::verify(&block, addr.block) {
                Some(tag) if tag.id == TAG_FSD => {
                    let f = Fields(&block);
                    let number = f.u32(44)?;
                    if best.is_none_or(|(seen, _)| number >= seen) {
                        best = Some((number, f.long_ad(400)?));
                    }
                }
                _ => break,
            }
        }
        let (_, root) = best.ok_or(DriverError::BadMagic)?;
        Ok(LbAddr {
            block: root.block,
            partition: root.partition,
        })
    }

    /// The entry the ICB at `addr` holds: followed through indirect entries,
    /// and on write-once media to the newest in its chain.
    pub(crate) fn resolve(&mut self, addr: LbAddr) -> Result<Resolved, DriverError> {
        let mut at = addr;
        let mut block = zeroed(self.block_size)?;
        for _ in 0..MAX_ICB_HOPS {
            self.read_block(at, &mut block)?;
            if Tag::verify(&block, at.block).is_some_and(|tag| tag.id == TAG_IE) {
                at = indirect(&block)?;
                continue;
            }
            let entry = Entry::parse(&block, at.block)?;
            if Entry::strategy(&block)? == entry::STRATEGY_4096 {
                let next = LbAddr {
                    block: at.block.checked_add(1).ok_or(DriverError::DeviceFault)?,
                    partition: at.partition,
                };
                let mut probe = zeroed(self.block_size)?;
                if self.read_block(next, &mut probe).is_ok()
                    && Tag::verify(&probe, next.block).is_some_and(|tag| tag.id == TAG_IE)
                {
                    at = indirect(&probe)?;
                    continue;
                }
            }
            return Ok(Resolved {
                entry,
                block,
                addr: at,
            });
        }
        Err(DriverError::DeviceFault)
    }

    /// The extents of a resolved node.
    pub(crate) fn extents_of(&mut self, resolved: &Resolved) -> Result<Vec<Extent>, DriverError> {
        let block_size = self.block_size;
        entry::extents(
            &resolved.block,
            &resolved.entry,
            resolved.addr.partition,
            |addr, out| {
                if out.len() != block_size as usize {
                    *out = zeroed(block_size)?;
                }
                self.read_block(addr, out)
            },
        )
    }

    /// A node's whole content, up to `bound` bytes: a directory's or a
    /// link's, which are read whole.
    fn whole_content(&mut self, resolved: &Resolved, bound: usize) -> Result<Vec<u8>, DriverError> {
        let len = usize::try_from(resolved.entry.size).map_err(|_| DriverError::DeviceFault)?;
        if len > bound {
            return Err(DriverError::DeviceFault);
        }
        let mut out = zeroed_len(len)?;
        let mapped = Mapped::of(self.extents_of(resolved)?)?;
        self.read_content(resolved, &mapped, 0, &mut out)?;
        Ok(out)
    }

    /// Read `buf.len()` bytes of the content of `resolved`, its extents
    /// `mapped`, from `offset`: embedded bytes from its entry, an extent that
    /// was never written, and whatever lies past the last extent, as zeros.
    pub(crate) fn read_content(
        &mut self,
        resolved: &Resolved,
        mapped: &Mapped,
        mut offset: u64,
        mut buf: &mut [u8],
    ) -> Result<(), DriverError> {
        if resolved.entry.form == AdForm::Embedded {
            let bytes = usize::try_from(offset)
                .ok()
                .and_then(|offset| resolved.entry.ad.start.checked_add(offset))
                .and_then(|start| Some(start..start.checked_add(buf.len())?))
                .filter(|range| range.end <= resolved.entry.ad.end)
                .and_then(|range| resolved.block.get(range))
                .ok_or(DriverError::DeviceFault)?;
            buf.copy_from_slice(bytes);
            return Ok(());
        }
        while !buf.is_empty() {
            let Some((index, within)) = mapped.find(offset) else {
                buf.fill(0);
                return Ok(());
            };
            let extent = mapped.extents[index];
            let take = usize::try_from((u64::from(extent.bytes) - within).min(buf.len() as u64))
                .unwrap_or(buf.len());
            let (now, rest) = buf.split_at_mut(take);
            match extent.holds {
                Holds::Data => self.read_extent(extent.start, within, now)?,
                Holds::Unwritten | Holds::Hole => now.fill(0),
            }
            buf = rest;
            offset += take as u64;
        }
        Ok(())
    }

    /// Read `buf.len()` bytes from byte `within` of the extent starting at
    /// `start`, in the longest runs of consecutive sectors.
    fn read_extent(
        &mut self,
        start: LbAddr,
        mut within: u64,
        mut buf: &mut [u8],
    ) -> Result<(), DriverError> {
        let bs = self.block_bytes();
        while !buf.is_empty() {
            let block = start
                .block
                .checked_add(u32::try_from(within / bs).map_err(|_| DriverError::DeviceFault)?)
                .ok_or(DriverError::DeviceFault)?;
            let addr = LbAddr {
                block,
                partition: start.partition,
            };
            let skip = within % bs;
            let wanted = (skip + buf.len() as u64).div_ceil(bs);
            let run = self
                .partitions
                .contiguous(addr, u32::try_from(wanted).unwrap_or(u32::MAX))?;
            let span = (u64::from(run) * bs - skip).min(buf.len() as u64);
            let take = usize::try_from(span).unwrap_or(buf.len());
            let sector = self.partitions.sector(addr)?;
            let at = sector
                .checked_mul(bs)
                .and_then(|at| at.checked_add(skip))
                .ok_or(DriverError::DeviceFault)?;
            let (now, rest) = buf.split_at_mut(take);
            match self.dev.read(at, now) {
                Err(DriverError::LengthOutOfRange) => return Err(DriverError::DeviceFault),
                other => other?,
            }
            buf = rest;
            within += take as u64;
        }
        Ok(())
    }

    /// The node `node` names, resolved: none once its entry is freed, which
    /// leaves the entry's bytes in place.
    pub(crate) fn node(&mut self, node: NodeId) -> Result<Resolved, DriverError> {
        let addr = addr_of(node).ok_or(DriverError::NotFound)?;
        if self.freed(addr)? {
            return Err(DriverError::NotFound);
        }
        self.resolve(addr)
    }

    /// Whether the node `node` lost its last name while the kernel held it.
    pub(crate) fn unnamed(&self, node: NodeId) -> bool {
        self.held.get(node.raw()).is_some_and(|hold| hold.unnamed)
    }

    /// The directory `dir`, resolved to have its entries read: `None` for one
    /// removed while held, which takes no entries.
    fn listable(&mut self, dir: NodeId) -> Result<Option<Resolved>, DriverError> {
        let resolved = self.node(dir)?;
        if kind_of(resolved.entry.file_type) != Some(NodeKind::Directory) {
            return Err(DriverError::Unsupported);
        }
        Ok((!self.unnamed(dir)).then_some(resolved))
    }

    /// The extended attribute file of a resolved node: its entry, its bytes,
    /// and the block its header was recorded in. `None` for a node with none.
    pub(crate) fn attribute_file(
        &mut self,
        resolved: &Resolved,
    ) -> Result<Option<(Resolved, Vec<u8>, u32)>, DriverError> {
        let Some(addr) = resolved.entry.ea_file else {
            return Ok(None);
        };
        let file = self.resolve(addr)?;
        let len = usize::try_from(file.entry.size)
            .ok()
            .filter(|&len| {
                len <= MAX_ATTRIBUTE_FILE && file.entry.file_type == entry::TYPE_ATTRIBUTES
            })
            .ok_or(DriverError::DeviceFault)?;
        let mapped = Mapped::of(self.extents_of(&file)?)?;
        // A header lies in the block its first byte does.
        let location = match (file.entry.form, mapped.extents.first()) {
            (AdForm::Embedded, _) => file.addr.block,
            (_, Some(extent)) if extent.holds == Holds::Data => extent.start.block,
            _ => return Err(DriverError::DeviceFault),
        };
        let mut content = zeroed_len(len)?;
        self.read_content(&file, &mapped, 0, &mut content)?;
        Ok(Some((file, content, location)))
    }

    /// The Finder information a resolved node records: in its entry, else in
    /// its attribute file. Damage to either reads as none, so a damaged
    /// attribute never stands in the way of the node itself.
    fn finder(&mut self, resolved: &Resolved) -> Option<FinderInfo> {
        let is_dir = kind_of(resolved.entry.file_type) == Some(NodeKind::Directory);
        let space = resolved.block.get(resolved.entry.ea.clone())?;
        if let Ok(Some(at)) = ea::find_finder(space, resolved.addr.block) {
            return ea::finder_at(space, at, is_dir).ok();
        }
        let (_, content, location) = self.attribute_file(resolved).ok()??;
        let at = ea::find_finder(&content, location).ok()??;
        ea::finder_at(&content, at, is_dir).ok()
    }

    /// The structural metadata of the node `node`, resolved as `resolved`.
    fn info(&mut self, node: NodeId, resolved: &Resolved) -> Result<NodeInfo, DriverError> {
        let entry = &resolved.entry;
        let kind = kind_of(entry.file_type).ok_or(DriverError::Unsupported)?;
        let size = match kind {
            NodeKind::Directory => 0,
            NodeKind::RegularFile => entry.size,
            NodeKind::Symlink => self.link_target(resolved)?.len() as u64,
        };
        let stamp = |time: Option<Time64>| time.unwrap_or(Time64::UNIX_EPOCH);
        let allocated = if entry.form == AdForm::Embedded {
            0
        } else {
            entry.recorded_blocks.saturating_mul(self.block_bytes())
        };
        // UDF counts the names that identify a node; a directory's own `.` is
        // not one.
        let nlink = if self.unnamed(node) {
            0
        } else {
            u32::from(entry.links).max(1) + u32::from(kind == NodeKind::Directory)
        };
        Ok(NodeInfo {
            kind,
            nlink,
            ino: node.raw(),
            size,
            allocated,
            times: NodeTimes {
                created: stamp(entry.created),
                modified: stamp(entry.modified),
                accessed: stamp(entry.accessed),
                changed: stamp(entry.changed),
            },
            content_gen: NodeInfo::NO_CONTENT_GEN,
        })
    }

    /// The path a symbolic link records (ECMA-167 4/14.16): its components
    /// joined, a root component opening it with `/`.
    fn link_target(&mut self, resolved: &Resolved) -> Result<Vec<u8>, DriverError> {
        let content = self.whole_content(resolved, FS_SYMLINK_MAX * 2)?;
        let mut target = Vec::new();
        let mut at = 0;
        while at < content.len() {
            let f = Fields(&content);
            let kind = f.u8(at)?;
            let len = usize::from(f.u8(at + 1)?);
            let identifier = f.bytes(at + 4, len)?;
            let part: Name;
            let piece: &[u8] = match kind {
                1 | 2 => {
                    target.clear();
                    at += 4 + len;
                    push(&mut target, b"/")?;
                    continue;
                }
                3 => b"..",
                4 => b".",
                5 => {
                    part = dir::decode(identifier).ok_or(DriverError::DeviceFault)?;
                    part.as_bytes()
                }
                _ => return Err(DriverError::DeviceFault),
            };
            if !target.is_empty() && target.last() != Some(&b'/') {
                push(&mut target, b"/")?;
            }
            push(&mut target, piece)?;
            at += 4 + len;
        }
        if target.len() > FS_SYMLINK_MAX {
            return Err(DriverError::DeviceFault);
        }
        Ok(target)
    }

    /// A reader of the directory `dir`'s stream.
    pub(crate) fn dir_reader(&mut self, dir: &Resolved) -> Result<DirReader, DriverError> {
        if kind_of(dir.entry.file_type) != Some(NodeKind::Directory) {
            return Err(DriverError::Unsupported);
        }
        self.descriptor_reader(dir)
    }

    /// A reader over the descriptors of a directory or a stream directory.
    pub(crate) fn descriptor_reader(&mut self, dir: &Resolved) -> Result<DirReader, DriverError> {
        Ok(DirReader {
            mapped: Mapped::of(self.extents_of(dir)?)?,
            size: dir.entry.size,
            held: None,
            block: zeroed(self.block_size)?,
        })
    }

    /// Fill `out` from byte `at` of a directory's stream, through the one
    /// block the reader holds.
    fn dir_bytes(
        &mut self,
        dir: &Resolved,
        reader: &mut DirReader,
        at: u64,
        out: &mut [u8],
    ) -> Result<(), DriverError> {
        let bs = self.block_bytes();
        let mut done = 0;
        while done < out.len() {
            let pos = at + done as u64;
            let index = pos / bs;
            if reader.held != Some(index) {
                reader.held = None;
                let start = index * bs;
                let len = usize::try_from((reader.size - start).min(bs))
                    .map_err(|_| DriverError::DeviceFault)?;
                self.read_content(dir, &reader.mapped, start, &mut reader.block[..len])?;
                reader.held = Some(index);
            }
            let within = usize::try_from(pos % bs).map_err(|_| DriverError::DeviceFault)?;
            let take = (self.block_size as usize - within).min(out.len() - done);
            out[done..done + take].copy_from_slice(&reader.block[within..within + take]);
            done += take;
        }
        Ok(())
    }

    /// The descriptor at byte `at` of a directory's stream, and where the
    /// next begins; `None` at the stream's end.
    pub(crate) fn next_fid(
        &mut self,
        dir: &Resolved,
        reader: &mut DirReader,
        at: u64,
    ) -> Result<Option<(Fid, u64)>, DriverError> {
        if at >= reader.size {
            return Ok(None);
        }
        if reader.size - at < FID_FIXED as u64 {
            return Err(DriverError::DeviceFault);
        }
        let mut header = [0u8; FID_FIXED];
        self.dir_bytes(dir, reader, at, &mut header)?;
        let len = Fid::length(&header)?;
        let next = at + len as u64;
        if next > reader.size {
            return Err(DriverError::DeviceFault);
        }
        let mut bytes = zeroed_len(len)?;
        self.dir_bytes(dir, reader, at, &mut bytes)?;
        Ok(Some((
            Fid::parse(&bytes, self.stream_block(dir, &reader.mapped, at)?)?,
            next,
        )))
    }

    /// The block byte `at` of the stream of `node`, its extents `mapped`,
    /// lies in: its entry's own for embedded bytes.
    pub(crate) fn stream_block(
        &self,
        node: &Resolved,
        mapped: &Mapped,
        at: u64,
    ) -> Result<u32, DriverError> {
        if node.entry.form == AdForm::Embedded {
            return Ok(node.addr.block);
        }
        let (index, within) = mapped.find(at).ok_or(DriverError::DeviceFault)?;
        let offset =
            u32::try_from(within / self.block_bytes()).map_err(|_| DriverError::DeviceFault)?;
        mapped.extents[index]
            .start
            .block
            .checked_add(offset)
            .ok_or(DriverError::DeviceFault)
    }

    /// Hand `visit` each live entry of the directory `dir` from byte
    /// `cursor` of its stream on, with where the next begins, until it
    /// stops.
    fn walk_dir(
        &mut self,
        dir: &Resolved,
        cursor: u64,
        visit: &mut DirVisitor<'_, B>,
    ) -> Result<(), DriverError> {
        let mut reader = self.dir_reader(dir)?;
        let mut at = cursor;
        while let Some((fid, next)) = self.next_fid(dir, &mut reader, at)? {
            at = next;
            if fid.characteristics & (CHAR_DELETED | CHAR_PARENT) != 0 {
                continue;
            }
            let Some(name) = dir::decode(&fid.identifier) else {
                continue;
            };
            if visit(self, &fid, &name, next)? == DirVisit::Stop {
                return Ok(());
            }
        }
        Ok(())
    }

    /// Why the volume cannot be written, `None` when it can.
    fn refusal_cause(&self) -> Option<&'static str> {
        let logical = self.volume.logical();
        if logical.domain_flags & (udf::DOMAIN_HARD_WRITE_PROTECT | udf::DOMAIN_SOFT_WRITE_PROTECT)
            != 0
        {
            return Some("write_protected");
        }
        if !(0x0102..=WRITE_REVISION).contains(&logical.revision) {
            return Some("revision_unwritten");
        }
        if self.integrity.min_write_revision > WRITE_REVISION {
            return Some("revision_unwritten");
        }
        if !self.integrity.closed {
            return Some("not_closed");
        }
        for map in &self.partitions.maps {
            match map {
                Map::Physical(physical) => {
                    if !matches!(
                        physical.access,
                        udf::ACCESS_OVERWRITABLE | udf::ACCESS_REWRITABLE
                    ) {
                        return Some("media_not_rewritable");
                    }
                    if physical.bitmap.bytes() == 0 {
                        return Some("no_space_bitmap");
                    }
                }
                Map::Sparable { .. } => return Some("sparable_media"),
                Map::Virtual { .. } => return Some("write_once_media"),
                Map::Metadata { .. } => return Some("metadata_partition"),
            }
        }
        if self.partitions.maps.len() != 1 {
            return Some("several_partitions");
        }
        None
    }

    /// The extents of the file `file`, resolved as `resolved`: the ones the
    /// last read or seek of it mapped, while no write has intervened.
    fn mapped(&mut self, file: NodeId, resolved: &Resolved) -> Result<Mapped, DriverError> {
        match self.file.take() {
            Some((id, mapped)) if id == file => Ok(mapped),
            _ => Mapped::of(self.extents_of(resolved)?),
        }
    }

    /// Where a seek for `region` from `offset` of the file `resolved` lands.
    fn region_after(
        resolved: &Resolved,
        mapped: &Mapped,
        offset: u64,
        region: SeekRegion,
    ) -> Option<u64> {
        let size = resolved.entry.size;
        if offset >= size {
            return None;
        }
        if resolved.entry.form == AdForm::Embedded {
            return region.in_solid_file(offset, size);
        }
        let mut at = offset;
        while at < size {
            let Some((index, _)) = mapped.find(at) else {
                // Past the last extent the file reads as a hole.
                return match region {
                    SeekRegion::Hole => Some(at),
                    SeekRegion::Data => None,
                };
            };
            let extent = mapped.extents[index];
            let data = extent.holds == Holds::Data;
            if data == (region == SeekRegion::Data) {
                return Some(at);
            }
            at = mapped.starts[index] + u64::from(extent.bytes);
        }
        match region {
            SeekRegion::Hole => Some(size),
            SeekRegion::Data => None,
        }
    }
}

/// The map a virtual or metadata partition of partition `number` sits on,
/// among the maps no other awaits: a virtual partition on the physical or
/// sparable map of its number, a metadata partition on the virtual one of its
/// number where there is one, else on that physical or sparable map. Neither
/// sits on a map that sits on another of its kind, so no chain of maps is
/// longer than two, and none is a cycle.
fn beneath_of(
    maps: &[Map],
    numbers: &[u16],
    awaiting: &[usize],
    number: u16,
    virtual_map: bool,
) -> Option<usize> {
    let physical = || {
        maps.iter().enumerate().position(|(at, map)| {
            !awaiting.contains(&at)
                && numbers.get(at) == Some(&number)
                && matches!(map, Map::Physical(_) | Map::Sparable { .. })
        })
    };
    if virtual_map {
        return physical();
    }
    maps.iter()
        .enumerate()
        .position(|(at, map)| {
            matches!(map, Map::Virtual { .. }) && numbers.get(at) == Some(&number)
        })
        .or_else(physical)
}

/// The target of the indirect entry `block` holds.
fn indirect(block: &[u8]) -> Result<LbAddr, DriverError> {
    let target = Fields(block).long_ad(36)?;
    Ok(LbAddr {
        block: target.block,
        partition: target.partition,
    })
}

/// `bytes` read as little-endian words.
fn words(bytes: &[u8]) -> Result<Vec<u32>, DriverError> {
    let count = bytes.len() / 4;
    if count > layout::MAX_VAT_ENTRIES {
        return Err(DriverError::BadMagic);
    }
    tairix_util::fallible::collected(
        count,
        bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|&word| u32::from_le_bytes(word)),
    )
    .ok_or(DriverError::OutOfMemory)
}

fn push(target: &mut Vec<u8>, bytes: &[u8]) -> Result<(), DriverError> {
    if target.len() + bytes.len() > FS_SYMLINK_MAX
        || !tairix_util::fallible::reserve(target, bytes.len())
    {
        return Err(DriverError::DeviceFault);
    }
    target.extend_from_slice(bytes);
    Ok(())
}

/// A zeroed block-sized buffer.
pub(crate) fn zeroed(block_size: u32) -> Result<Vec<u8>, DriverError> {
    zeroed_len(block_size as usize)
}

/// `len` zero bytes, reserved fallibly.
pub(crate) fn zeroed_len(len: usize) -> Result<Vec<u8>, DriverError> {
    tairix_util::fallible::filled(len, 0u8).ok_or(DriverError::OutOfMemory)
}

impl<B: Block> FilesystemRead for Udf<B> {
    fn root(&self) -> NodeId {
        node_of(self.root)
    }

    fn name_matching(&self) -> NameMatching {
        NameMatching::Exact
    }

    fn node_identity(&self) -> NodeIdentity {
        NodeIdentity::Stable
    }

    fn node_info(&mut self, node: NodeId) -> Result<NodeInfo, DriverError> {
        let resolved = self.node(node)?;
        self.info(node, &resolved).map_err(|err| match err {
            DriverError::Unsupported => DriverError::NotFound,
            other => other,
        })
    }

    fn lookup(&mut self, dir: NodeId, name: &[u8]) -> Result<NodeId, DriverError> {
        let dir = self.listable(dir)?.ok_or(DriverError::NotFound)?;
        let mut found = None;
        self.walk_dir(&dir, 0, &mut |fs, fid, listed, _| {
            if listed.as_bytes() != name {
                return Ok(DirVisit::Take);
            }
            let addr = fid.node();
            // A name for a node this system has no kind for is not one.
            if fs
                .resolve(addr)
                .is_ok_and(|child| kind_of(child.entry.file_type).is_some())
            {
                found = Some(node_of(addr));
                return Ok(DirVisit::Stop);
            }
            Ok(DirVisit::Take)
        })?;
        found.ok_or(DriverError::NotFound)
    }

    fn read_at(&mut self, file: NodeId, offset: u64, buf: &mut [u8]) -> Result<usize, DriverError> {
        let resolved = self.node(file)?;
        if kind_of(resolved.entry.file_type) != Some(NodeKind::RegularFile) {
            return Err(DriverError::Unsupported);
        }
        let Some(left) = resolved
            .entry
            .size
            .checked_sub(offset)
            .filter(|&left| left > 0)
        else {
            return Ok(0);
        };
        let take = usize::try_from(left).map_or(buf.len(), |left| left.min(buf.len()));
        let mapped = self.mapped(file, &resolved)?;
        let result = self.read_content(&resolved, &mapped, offset, &mut buf[..take]);
        self.file = Some((file, mapped));
        result.map(|()| take)
    }

    fn read_link(&mut self, link: NodeId, out: &mut [u8]) -> Result<usize, DriverError> {
        let resolved = self.node(link)?;
        if kind_of(resolved.entry.file_type) != Some(NodeKind::Symlink) {
            return Err(DriverError::Unsupported);
        }
        let target = self.link_target(&resolved)?;
        let slot = out
            .get_mut(..target.len())
            .ok_or(DriverError::BufferTooSmall)?;
        slot.copy_from_slice(&target);
        Ok(target.len())
    }

    fn seek_region(
        &mut self,
        file: NodeId,
        offset: u64,
        region: SeekRegion,
    ) -> Result<Option<u64>, DriverError> {
        let resolved = self.node(file)?;
        if kind_of(resolved.entry.file_type) != Some(NodeKind::RegularFile) {
            return Err(DriverError::Unsupported);
        }
        let mapped = self.mapped(file, &resolved)?;
        let answer = Self::region_after(&resolved, &mapped, offset, region);
        self.file = Some((file, mapped));
        Ok(answer)
    }

    fn read_dir(
        &mut self,
        dir: NodeId,
        cursor: u64,
        _after: &[u8],
        visit: &mut dyn FnMut(&DirEntry, &[u8]) -> DirVisit,
    ) -> Result<(), DriverError> {
        let Some(dir) = self.listable(dir)? else {
            return Ok(());
        };
        self.walk_dir(&dir, cursor, &mut |fs, fid, name, next| {
            let addr = fid.node();
            let child = fs.resolve(addr)?;
            if kind_of(child.entry.file_type).is_none() {
                return Ok(DirVisit::Take);
            }
            let node = node_of(addr);
            let entry = DirEntry {
                node,
                info: fs.info(node, &child)?,
                next_cursor: next,
            };
            Ok(visit(&entry, name.as_bytes()))
        })
    }

    fn parent(&mut self, dir: NodeId) -> Result<NodeId, DriverError> {
        if addr_of(dir) == Some(self.root) {
            return Ok(dir);
        }
        let resolved = self.node(dir)?;
        match kind_of(resolved.entry.file_type) {
            Some(NodeKind::Directory) if !self.unnamed(dir) => {
                self.parent_addr(&resolved).map(node_of)
            }
            // A removed directory's parent descriptor names where it was.
            Some(NodeKind::Directory) | None => Err(DriverError::NotFound),
            Some(_) => Err(DriverError::Unsupported),
        }
    }

    /// A held node's entry stays where it lies and is never freed, so the
    /// node's own id names it while it is held.
    fn pin(&mut self, node: NodeId) -> Result<NodeId, DriverError> {
        if let Some(hold) = self.held.get_mut(node.raw()) {
            hold.count = hold.count.checked_add(1).ok_or(DriverError::OutOfMemory)?;
            return Ok(node);
        }
        if kind_of(self.node(node)?.entry.file_type).is_none() {
            return Err(DriverError::NotFound);
        }
        self.held
            .try_insert(
                node.raw(),
                Hold {
                    count: 1,
                    unnamed: false,
                },
            )
            .map_err(|_| DriverError::OutOfMemory)?;
        Ok(node)
    }

    fn unpin(&mut self, pinned: NodeId) -> Result<Unpinned, DriverError> {
        let hold = self
            .held
            .get_mut(pinned.raw())
            .ok_or(DriverError::NotFound)?;
        if hold.count > 1 {
            hold.count -= 1;
            return Ok(Unpinned::Held);
        }
        let unnamed = hold.unnamed;
        self.held.remove(pinned.raw());
        if !unnamed {
            return Ok(Unpinned::Released);
        }
        self.free_unnamed(pinned).map(|()| Unpinned::Freed)
    }

    fn release_pins(&mut self) -> Result<(), DriverError> {
        let held = core::mem::take(&mut self.held);
        let mut freed = Ok(());
        for (id, hold) in &held {
            if hold.unnamed && self.free_unnamed(NodeId::from_raw(id)).is_err() {
                freed = Err(DriverError::DeviceFault);
            }
        }
        freed
    }
}

/// The POSIX mode a permission word and ICB flags record.
pub(crate) fn mode_of(permissions: u32, flags: u16) -> u32 {
    let class = |shift: u32| (permissions >> shift) & 0b111;
    let mut mode = (class(10) << 6) | (class(5) << 3) | class(0);
    for (flag, bit) in [
        (FLAG_SETUID, 0o4000),
        (FLAG_SETGID, 0o2000),
        (FLAG_STICKY, 0o1000),
    ] {
        if flags & flag != 0 {
            mode |= bit;
        }
    }
    mode
}

/// The owner and group a UDF entry may leave unspecified.
pub(crate) const UNSPECIFIED_ID: u32 = u32::MAX;

/// UDF records a POSIX owner, group and mode; an entry that leaves its owner
/// or group unspecified reports the record an ownerless format stands in
/// with.
impl<B: Block> FilesystemSecurity for Udf<B> {
    fn security(&mut self, node: NodeId) -> Result<NodeSecurity, DriverError> {
        let resolved = self.node(node)?;
        let entry = &resolved.entry;
        let kind = kind_of(entry.file_type).ok_or(DriverError::NotFound)?;
        if entry.uid == UNSPECIFIED_ID || entry.gid == UNSPECIFIED_ID {
            return Ok(NodeSecurity::unrecorded(kind));
        }
        Ok(NodeSecurity::new(
            mode_of(entry.permissions, entry.flags),
            entry.uid,
            entry.gid,
        ))
    }

    /// A file entry has no place for ACL entries or a capability gate, so a
    /// record asking for either is refused whole rather than stored in part.
    fn set_security(&mut self, node: NodeId, security: NodeSecurity) -> Result<(), DriverError> {
        if security.required_cap.is_some()
            || !security.acl().is_empty()
            || security.acl_model != AclModel::Additive
        {
            return Err(DriverError::Unsupported);
        }
        self.change(|fs| fs.set_owner_mode(node, security))
    }

    fn records_ownership(&mut self, node: NodeId) -> Result<bool, DriverError> {
        let resolved = self.node(node)?;
        Ok(resolved.entry.uid != UNSPECIFIED_ID && resolved.entry.gid != UNSPECIFIED_ID)
    }
}

/// UDF keeps the Macintosh Finder information, as the `mac` type, creator
/// and Finder flags; it has nowhere to keep any other attribute.
impl<B: Block> FilesystemAttrs for Udf<B> {
    fn get_attr(
        &mut self,
        node: NodeId,
        key: &[u8],
        value_out: &mut [u8],
    ) -> Result<Option<usize>, DriverError> {
        let key = tairix_fsmeta::AttrKey::parse(key).map_err(|_| DriverError::OutOfRange)?;
        let resolved = self.node(node)?;
        let finder = self.finder(&resolved).unwrap_or_default();
        let mut value = [0u8; mac::FINDER_VALUE_MAX];
        let Some(len) = finder.value(key.as_bytes(), &mut value) else {
            return Ok(None);
        };
        let slot = value_out
            .get_mut(..len)
            .ok_or(DriverError::BufferTooSmall)?;
        slot.copy_from_slice(&value[..len]);
        Ok(Some(len))
    }

    fn set_attr(&mut self, node: NodeId, key: &[u8], value: &[u8]) -> Result<(), DriverError> {
        let key = tairix_fsmeta::AttrKey::parse(key).map_err(|_| DriverError::OutOfRange)?;
        let key = key.as_bytes();
        if !mac::KEYS.contains(&key) {
            return Err(DriverError::Unsupported);
        }
        self.change(|fs| {
            fs.update_finder(node, |current, is_dir| {
                if is_dir && key != b"mac.finderflags" {
                    // A directory has no type or creator to record.
                    return Err(DriverError::Unsupported);
                }
                let mut finder = current.unwrap_or_default();
                finder
                    .set(key, value)
                    .map_err(|_| DriverError::OutOfRange)?;
                Ok(finder)
            })
        })
    }

    fn list_attr(
        &mut self,
        node: NodeId,
        index: u64,
        key_out: &mut [u8],
    ) -> Result<Option<usize>, DriverError> {
        let resolved = self.node(node)?;
        let Some(key) = self.finder(&resolved).unwrap_or_default().key(index) else {
            return Ok(None);
        };
        let out = key_out
            .get_mut(..key.len())
            .ok_or(DriverError::BufferTooSmall)?;
        out.copy_from_slice(key);
        Ok(Some(key.len()))
    }

    fn remove_attr(&mut self, node: NodeId, key: &[u8]) -> Result<(), DriverError> {
        let key = tairix_fsmeta::AttrKey::parse(key).map_err(|_| DriverError::OutOfRange)?;
        let key = key.as_bytes();
        if !mac::KEYS.contains(&key) {
            return Err(DriverError::NotFound);
        }
        self.change(|fs| {
            fs.update_finder(node, |current, _| {
                let mut finder = current.ok_or(DriverError::NotFound)?;
                if finder.clear(key) {
                    Ok(finder)
                } else {
                    Err(DriverError::NotFound)
                }
            })
        })
    }
}

impl<B: Block> FilesystemAttrsProvider for Udf<B> {
    fn attrs_fs(&mut self) -> Option<&mut dyn FilesystemAttrsFs> {
        Some(self)
    }
}

impl<B: Block> FilesystemStats for Udf<B> {
    fn stats(&mut self) -> Result<VolumeStats, DriverError> {
        let total = self
            .partitions
            .maps
            .iter()
            .filter_map(|map| match map {
                Map::Physical(physical) | Map::Sparable { physical, .. } => {
                    Some(u64::from(physical.length))
                }
                Map::Virtual { .. } | Map::Metadata { .. } => None,
            })
            .sum();
        let free = self
            .write
            .free_blocks()
            .unwrap_or(self.integrity.free_blocks);
        let avail = if self.refusal.is_none() { free } else { 0 };
        Ok(VolumeStats {
            block_size: self.block_size,
            in_reserve: false,
            total_blocks: total,
            free_blocks: free,
            avail_blocks: avail,
            files: 0,
            files_free: 0,
        })
    }
}
