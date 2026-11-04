//! TAIRiX AmigaDOS filesystem driver: OFS and FFS volumes (read/write).
//!
//! Attaches an AmigaDOS volume — an 880 KiB or 1760 KiB floppy, or a bare
//! hard-disk partition of any block size the format allows — behind any
//! [`Block`] device and exposes it through [`FilesystemRead`],
//! [`FilesystemWrite`], [`FilesystemAttrs`], [`FilesystemStats`] and
//! [`FilesystemLabel`].
//!
//! # Supported volumes
//!
//! * `DOS\0`–`DOS\3`: OFS and FFS, with and without international mode, are
//!   read and written.
//! * `DOS\4`, `DOS\5`: directory-cache volumes are read; their caches are not
//!   kept, so they open read-only (`UnsupportedFeature`).
//! * `DOS\6`, `DOS\7`: long-filename volumes are refused (`Unsupported`).
//!
//! A volume whose bitmap was not left validated — written by a system that
//! stopped before it finished — opens read-only (`NeedsCheck`); AmigaDOS
//! rebuilds such a bitmap when it validates the volume.
//!
//! # Structure
//!
//! Every structure is checksummed and validated as it is read: a header's
//! type, key, secondary type, name and comment bounds and pointers, a data
//! block's owner and sequence number, a bitmap block's sum. Anything out of
//! shape is [`DriverError::BadMagic`]. Every chain is walked under a bound
//! with a loop check, so a damaged volume cannot hold the driver, and a change
//! that frees or fills what a file's extension chain names walks it on to its
//! end first, so one returning to a block it passed is refused before anything
//! is written. The root and the bitmap's own blocks are named once each, and no
//! change writes or frees one as anything else.
//!
//! # Writing
//!
//! A change opens a transaction at its first write by marking the root's
//! bitmap invalid on the medium; data reaches its blocks before a header
//! points at them, a header is written before a directory files it, and freed
//! blocks return to the bitmap. The commit writes the bitmap, flushes, and only
//! then marks the root valid again, so a crash in between leaves a volume
//! AmigaDOS revalidates. While an object the kernel holds has lost its last
//! name, or its free at the last unpin failed, the root stays marked invalid,
//! so AmigaDOS reclaims its blocks; a free the heap refuses at that unpin is
//! made when the mount's pins are released. Every refusal is decided before a
//! change's first write, so a refused change writes nothing — a heap running
//! short among them, as what a change takes once it has written is held before
//! it and the commit allocates nothing. One that fails part-way leaves the
//! volume for AmigaDOS to validate, and the driver writes nothing more to it.
//!
//! # Names and links
//!
//! Names are ISO 8859-1 on disk and UTF-8 above, at most 30 bytes, and match
//! as the volume's mode folds them ([`NameMatching`]). A hard link to a file
//! is the file under a second name; removing the file promotes its newest
//! link to take its place, as AmigaDOS does. A soft link reads in POSIX
//! spelling; a hard link to a directory reads as a soft link to it. The
//! volume's own name is its title, held to a new entry's rules, and a soft
//! link naming the volume follows it.
//!
//! # Metadata
//!
//! The protection bits' low byte and the comment surface as `amiga.protection`
//! and `amiga.comment` through `tairix_fsmeta`'s Amiga preset; the one date
//! stamp an object keeps is all four of its times, read as UTC.
//!
//! # Public surface
//!
//! The only public *function* is [`register`]. [`Affs`] is the type the
//! driver host opens with [`Affs::open`] (or formats with [`Affs::format`]);
//! it is reached only through the filesystem traits.
//!
//! # Capabilities
//!
//! Loading requires [`CapabilityId::DRV_LOAD`]. The driver runs in user
//! space; it does not request `CAP_DRV_KERNEL`.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

use alloc::vec::Vec;

use tairix_abi::blkio::BlkCommandCost;
use tairix_abi::driver::block::Block;
use tairix_abi::driver::filesystem::{
    DirEntry, DirVisit, FilesystemAttrs, FilesystemAttrsFs, FilesystemAttrsProvider,
    FilesystemLabel, FilesystemRead, FilesystemSecurity, FilesystemStats, FilesystemWrite,
    NameMatching, NodeId, NodeIdentity, NodeInfo, NodeKind, NodeSecurity, NodeTimes, Unpinned,
    VolumeHost, VolumeStats, VolumeTitle, WriteRefusal,
};
use tairix_abi::time::Time64;
use tairix_abi::{CapabilityId, DriverError, DriverHandle, DriverHost};
use tairix_bytedev::ByteDevice;
use tairix_collections::RadixTree;
use tairix_fsmeta::preset::amiga;
use tairix_fsprobe::affs::{
    be32, DateStamp, Layout, Offsets, BITMAP_PAGES, BITMAP_VALID, BOOT_BLOCKS, NAME_MAX,
};
use tairix_writeback::CommitScheduler;

mod bitmap;
mod chain;
mod dir;
mod file;
mod guard;
mod header;
mod mkfs;
mod name;
mod served;

#[cfg(test)]
mod tests;

use bitmap::{Bitmap, Pages};
use header::{Extension, Header, Kind};
use name::Name;

pub use mkfs::FormatOptions;
pub use served::{Served, BIND_KEYS};

/// Per-driver `DriverHandle` marker returned by [`register`].
const REGISTER_HANDLE_MARKER: u64 = 0x4146_4653_0000_0001; // "AFFS" + index

/// The hard-linked files whose name counts are kept between changes: a fixed
/// bound, past which the cache starts again rather than grow with the volume.
const LINK_COUNTS_KEPT: usize = 4096;

/// Driver entry point.
///
/// # Errors
///
/// [`DriverError::PermissionDenied`] if the host did not grant
/// [`CapabilityId::DRV_LOAD`].
pub fn register(host: &dyn DriverHost) -> Result<DriverHandle, DriverError> {
    if !host.has_capability(CapabilityId::DRV_LOAD) {
        return Err(DriverError::PermissionDenied);
    }
    DriverHandle::from_raw(REGISTER_HANDLE_MARKER)
}

/// The block buffers a change may take once it has written: the headers and
/// extensions it reads after its first write, a walk handing each step's back
/// and every header a change is done with handed back as it finishes with it.
/// A fixed bound on one change's working set; the deepest, a rename with its
/// buckets crowded, holds four at once.
const SPARE_BLOCKS: usize = 8;

/// The blocks of the volume, read and written whole.
struct Disk<B: Block> {
    dev: ByteDevice<B>,
    block_size: usize,
    blocks: u64,
    /// Buffers held for a change to take after its first write, so a heap
    /// that runs short refuses a change before it writes rather than
    /// stopping it part-way.
    spare: Vec<Vec<u8>>,
    /// The change under way has written, so a buffer comes from the spares.
    spending: bool,
}

impl<B: Block> Disk<B> {
    fn offset(&self, block: u64, len: usize) -> Result<u64, DriverError> {
        let count = (len / self.block_size) as u64;
        if block < BOOT_BLOCKS || block.checked_add(count).is_none_or(|end| end > self.blocks) {
            return Err(DriverError::BadMagic);
        }
        Ok(block * self.block_size as u64)
    }

    fn read(&mut self, block: u64, buf: &mut [u8]) -> Result<(), DriverError> {
        let at = self.offset(block, buf.len())?;
        self.dev.read(at, buf)
    }

    fn write(&mut self, block: u64, buf: &[u8]) -> Result<(), DriverError> {
        let at = self.offset(block, buf.len())?;
        self.dev.write(at, buf)
    }

    /// Zero the `count` blocks from `block`.
    fn zero(&mut self, block: u64, count: u64) -> Result<(), DriverError> {
        let len = usize::try_from(count)
            .ok()
            .and_then(|count| count.checked_mul(self.block_size))
            .ok_or(DriverError::OutOfRange)?;
        let at = self.offset(block, len)?;
        self.dev.zero(at, len as u64)
    }

    /// A zeroed block buffer: once a change has written, one of the spares.
    fn buffer(&mut self) -> Result<Vec<u8>, DriverError> {
        if self.spending {
            if let Some(mut buf) = self.spare.pop() {
                buf.fill(0);
                return Ok(buf);
            }
        }
        tairix_util::fallible::filled(self.block_size, 0).ok_or(DriverError::OutOfMemory)
    }

    /// Keep `buf`, a block buffer a change has read with and is done with, for
    /// the change's next take: only while it spends spares, into the room
    /// [`stock`](Self::stock) reserved before it wrote, so keeping one never
    /// allocates. Outside a change the buffer is let go.
    fn recycle(&mut self, buf: Vec<u8>) {
        if self.spending && self.spare.len() < SPARE_BLOCKS && buf.len() == self.block_size {
            self.spare.push(buf);
        }
    }

    /// Hold every spare a change may take, before it writes.
    ///
    /// # Errors
    ///
    /// [`DriverError::OutOfMemory`].
    fn stock(&mut self) -> Result<(), DriverError> {
        let wanted = SPARE_BLOCKS - self.spare.len();
        if !tairix_util::fallible::reserve(&mut self.spare, wanted) {
            return Err(DriverError::OutOfMemory);
        }
        while self.spare.len() < SPARE_BLOCKS {
            let buf = tairix_util::fallible::filled(self.block_size, 0)
                .ok_or(DriverError::OutOfMemory)?;
            self.spare.push(buf);
        }
        Ok(())
    }

    /// Extension block `block` of the file whose header is `file`.
    fn extension(&mut self, at: Offsets, file: u64, block: u64) -> Result<Extension, DriverError> {
        let mut buf = self.buffer()?;
        self.read(block, &mut buf)?;
        Extension::parse(block, buf, at, file, self.blocks)
    }
}

impl<B: Block> Pages for Disk<B> {
    fn read(&mut self, block: u64, buf: &mut [u8]) -> Result<(), DriverError> {
        Disk::read(self, block, buf)
    }

    fn write(&mut self, block: u64, buf: &[u8]) -> Result<(), DriverError> {
        Disk::write(self, block, buf)
    }
}

/// One hold of the kernel's on a node.
struct Hold {
    count: u32,
    /// The node lost its last name while held: its blocks are freed at the
    /// last unpin.
    unnamed: bool,
}

/// What a header records of its node, read once as a listing walks its
/// bucket so handing the entry over reads nothing more.
#[derive(Clone, Copy)]
struct Facts {
    block: u64,
    kind: Kind,
    /// A file's bytes, or a soft link's target's in POSIX spelling — or why
    /// those could not be had, answered for the entry alone.
    size: Result<u64, DriverError>,
    date: DateStamp,
    next_link: u32,
    real_entry: u32,
}

/// What a node is to the VFS.
fn node_kind(kind: Kind) -> Result<NodeKind, DriverError> {
    match kind {
        Kind::Root | Kind::Directory => Ok(NodeKind::Directory),
        Kind::File => Ok(NodeKind::RegularFile),
        Kind::SoftLink | Kind::DirLink => Ok(NodeKind::Symlink),
        // A hard link to a file is the file under another name, never a node.
        Kind::FileLink => Err(DriverError::BadMagic),
    }
}

/// The file the last read was of: its header, and the extension block its
/// last pointer came from, so a sequential read reads only its data. Dropped
/// by every change, which may rewrite either.
struct Reading {
    header: Header,
    held: Option<(u64, header::Extension)>,
}

/// A mounted AmigaDOS volume.
pub struct Affs<B: Block> {
    disk: Disk<B>,
    layout: Layout,
    at: Offsets,
    matching: NameMatching,
    /// The volume's name, which a soft link names its own volume by.
    volume: Name,
    /// Loaded as a change or the space count first needs it.
    bitmap: Option<Bitmap>,
    write_refusal: Option<WriteRefusal>,
    schedule: CommitScheduler,
    /// The root as the open transaction last wrote it, its bitmap marked
    /// invalid on the medium; none while no transaction is open. Kept so a
    /// commit publishes it with no read.
    transaction: Option<Header>,
    /// The change under way has written a block or moved one in the bitmap,
    /// so a failure leaves it part-way.
    mutated: bool,
    /// A transaction was dropped after the device accepted its writes, so the
    /// next flush must still force them out of the device's cache.
    unflushed: bool,
    held: RadixTree<Hold>,
    /// Objects that have lost their last name whose blocks are still
    /// allocated: held ones, freed at their last unpin, and any whose free
    /// failed, left for AmigaDOS's validator.
    orphans: usize,
    /// The names each hard-linked file was counted to have, kept until a
    /// change, so listing a directory of links walks each chain once.
    linked: RadixTree<u32>,
    /// The extension chains of the files most recently reached, kept in step
    /// with every change.
    chains: chain::Chains,
    reading: Option<Reading>,
    /// The bucket the last listing stopped in, so the next resumes there
    /// without walking its chain again. Dropped by every change.
    listing: Option<dir::Bucket>,
}

impl<B: Block> Affs<B> {
    /// Open the AmigaDOS volume on `device`.
    ///
    /// # Errors
    ///
    /// [`DriverError::BadMagic`] for no volume, or a root out of shape;
    /// [`DriverError::Unsupported`] for a long-filename volume, or one whose
    /// blocks a longword cannot number; the device's error.
    pub fn open(device: B) -> Result<Self, DriverError> {
        let cost = device.medium().map_or(
            BlkCommandCost::Cheap,
            tairix_abi::blkio::BlkMedium::command_cost,
        );
        let mut dev = ByteDevice::new(device)?;
        let layout = tairix_fsprobe::affs::identify(&mut dev)?.ok_or(DriverError::BadMagic)?;
        if layout.dos_type.has_long_names() || layout.blocks > u64::from(u32::MAX) + 1 {
            return Err(DriverError::Unsupported);
        }
        let block_size = layout.block_size as usize;
        let volume = Name::recorded(layout.name())?;
        let write_refusal = if layout.dos_type.has_dir_cache() {
            Some(WriteRefusal::UnsupportedFeature)
        } else if !layout.bitmap_valid {
            Some(WriteRefusal::NeedsCheck)
        } else {
            None
        };
        let mut fs = Self {
            disk: Disk {
                dev,
                block_size,
                blocks: layout.blocks,
                spare: Vec::new(),
                spending: false,
            },
            matching: if layout.dos_type.is_international() {
                NameMatching::Latin1CaseInsensitive
            } else {
                NameMatching::AsciiCaseInsensitive
            },
            at: Offsets::new(block_size),
            layout,
            volume,
            bitmap: None,
            write_refusal,
            schedule: CommitScheduler::new(cost),
            transaction: None,
            mutated: false,
            unflushed: false,
            held: RadixTree::new(),
            orphans: 0,
            linked: RadixTree::new(),
            chains: chain::Chains::default(),
            reading: None,
            listing: None,
        };
        let room = fs.extension_blocks(fs.data_blocks(u64::from(u32::MAX)));
        fs.chains =
            chain::Chains::new(usize::try_from(room).map_err(|_| DriverError::Unsupported)?)?;
        Ok(fs)
    }

    /// Lay an empty volume out on `device` as `options` asks, and open it.
    ///
    /// # Errors
    ///
    /// [`DriverError::LengthOutOfRange`] for a device too small for a volume,
    /// [`DriverError::OutOfRange`] for an option out of range; the device's
    /// error.
    pub fn format(device: B, options: &FormatOptions<'_>) -> Result<Self, DriverError> {
        let device = mkfs::format_volume(device, options)?;
        Self::open(device)
    }

    /// The device beneath the volume, which a test reads the image back from.
    pub fn into_device(self) -> B {
        self.disk.dev.into_inner()
    }

    /// Why the volume will not be written, if it will not.
    #[must_use]
    pub const fn write_refusal(&self) -> Option<WriteRefusal> {
        self.write_refusal
    }

    fn root_block(&self) -> u64 {
        self.layout.root
    }

    fn header(&mut self, block: u64) -> Result<Header, DriverError> {
        if !header::within(block, self.disk.blocks) {
            return Err(DriverError::BadMagic);
        }
        let mut buf = self.disk.buffer()?;
        self.disk.read(block, &mut buf)?;
        Header::parse(block, buf, self.at, self.layout.root, self.disk.blocks)
    }

    fn write_header(&mut self, header: &mut Header) -> Result<(), DriverError> {
        if header.kind != Kind::Root {
            return self.put(header.block, header.sealed());
        }
        // A copy read before the change's transaction opened still says the
        // bitmap is valid.
        header.set_long_at(self.at.bm_flag(), 0);
        self.mutate()?;
        self.disk.write(header.block, header.sealed())?;
        if let Some(kept) = self.transaction.as_mut() {
            kept.copy_from(header);
        }
        Ok(())
    }

    /// Write `buf` to `block` as part of a change.
    fn put(&mut self, block: u64, buf: &[u8]) -> Result<(), DriverError> {
        self.writing(block, (buf.len() / self.disk.block_size) as u64)?;
        self.disk.write(block, buf)
    }

    /// Zero the `count` blocks from `block` as part of a change.
    fn put_zeros(&mut self, block: u64, count: u64) -> Result<(), DriverError> {
        self.writing(block, count)?;
        self.disk.zero(block, count)
    }

    /// Ready the `count` blocks from `block` for a change's write, its
    /// transaction opened first. The root and the bitmap's blocks are refused:
    /// only damage names one as anything else, and the refusal comes before
    /// anything is written.
    fn writing(&mut self, block: u64, count: u64) -> Result<(), DriverError> {
        if self.with_bitmap(|bitmap, _| Ok(bitmap.reserves(block, count)))? {
            return Err(DriverError::BadMagic);
        }
        self.mutate()
    }

    /// Open the change's transaction, and count the change as having written:
    /// from here a buffer it needs is one of the spares.
    fn mutate(&mut self) -> Result<(), DriverError> {
        self.begin()?;
        self.mutated = true;
        self.disk.spending = true;
        Ok(())
    }

    /// The header of the node `id` names: the root, a directory, a file or a
    /// link that reads as a soft link. A hard link to a file is never a node
    /// itself, and an id outside the volume names nothing.
    fn node(&mut self, id: NodeId) -> Result<Header, DriverError> {
        let block = id.raw();
        if !header::within(block, self.disk.blocks) {
            return Err(DriverError::NotFound);
        }
        let header = self.header(block).map_err(|err| match err {
            DriverError::BadMagic => DriverError::NotFound,
            other => other,
        })?;
        if header.kind == Kind::FileLink {
            return Err(DriverError::NotFound);
        }
        Ok(header)
    }

    /// The header of the directory `id` names, which takes entries: a held
    /// directory that lost its name takes none.
    fn directory(&mut self, id: NodeId) -> Result<Header, DriverError> {
        let header = self.node(id)?;
        if !header.kind.is_directory() {
            return Err(DriverError::Unsupported);
        }
        if self.unnamed(id.raw()) {
            return Err(DriverError::NotFound);
        }
        Ok(header)
    }

    /// Free the orphan `block`, its last hold gone. It is counted until the
    /// free completes, so one that fails leaves the root marked invalid for
    /// AmigaDOS's validator to reclaim its blocks, as a crash would.
    fn free_orphan(&mut self, block: u64) -> Result<(), DriverError> {
        self.free_object(block)?;
        self.orphans = self.orphans.saturating_sub(1);
        Ok(())
    }

    fn unnamed(&self, block: u64) -> bool {
        self.held.get(block).is_some_and(|hold| hold.unnamed)
    }

    /// Leave the node `block`, which has lost its last name, to its last
    /// unpin, answering whether the kernel holds it.
    fn orphan(&mut self, block: u64) -> bool {
        let Some(hold) = self.held.get_mut(block) else {
            return false;
        };
        if !hold.unnamed {
            hold.unnamed = true;
            self.orphans += 1;
        }
        true
    }

    /// Free the object `block`, which has lost its last name, or leave it to
    /// its last unpin when the kernel holds it.
    fn unname(&mut self, block: u64) -> Result<(), DriverError> {
        if self.orphan(block) {
            return Ok(());
        }
        self.free_object(block)
    }

    /// The file a hard link naming `block` names.
    fn real_file(&mut self, block: u32) -> Result<Header, DriverError> {
        let real = self.header(u64::from(block))?;
        if real.kind != Kind::File {
            return Err(DriverError::BadMagic);
        }
        Ok(real)
    }

    /// The node an entry names: a hard link to a file names the file.
    fn entry_node(&mut self, entry: Header) -> Result<Header, DriverError> {
        if entry.kind == Kind::FileLink {
            self.real_file(entry.real_entry())
        } else {
            Ok(entry)
        }
    }

    fn now(&self) -> DateStamp {
        stamp(self.schedule.host().and_then(|(_, host)| host.wall_time()))
    }

    /// The time a date stamp records: the epoch for none, which a volume
    /// leaves when it knew no time.
    fn time(date: DateStamp) -> Time64 {
        if date == DateStamp::default() {
            return Time64::UNIX_EPOCH;
        }
        amiga::datestamp_to_time64(date.days, date.mins, date.ticks).unwrap_or(Time64::UNIX_EPOCH)
    }

    /// What `header` records of its node without reading anything else.
    fn facts(&self, header: &Header) -> Facts {
        let size = match header.kind {
            Kind::SoftLink => {
                name::link_target_to_posix(header.link_target(), &self.volume, self.matching)
                    .map(|target| target.len() as u64)
            }
            _ => Ok(u64::from(header.byte_size())),
        };
        Facts {
            block: header.block,
            kind: header.kind,
            size,
            date: header.date(),
            next_link: header.next_link(),
            real_entry: header.real_entry(),
        }
    }

    /// The structural reading of `header`'s node.
    fn info(&mut self, header: &Header) -> Result<NodeInfo, DriverError> {
        let mut info = self.info_of(&self.facts(header))?;
        if header.kind == Kind::Root {
            info.times.created = Self::time(header.created());
        }
        Ok(info)
    }

    /// The structural reading of the node `facts` describe, reading what a
    /// header alone does not answer: a file's other names, the directory a
    /// directory link names.
    fn info_of(&mut self, facts: &Facts) -> Result<NodeInfo, DriverError> {
        let kind = node_kind(facts.kind)?;
        let (size, allocated) = match facts.kind {
            Kind::File => {
                let size = facts.size?;
                (size, self.allocated(size))
            }
            Kind::SoftLink => (facts.size?, 0),
            Kind::DirLink => (self.linked_dir(facts.real_entry)?.len() as u64, 0),
            _ => (0, 0),
        };
        let nlink = if self.unnamed(facts.block) {
            0
        } else if facts.kind == Kind::File {
            self.link_count(facts.block, facts.next_link)?
        } else {
            NodeInfo::SINGLE_NAME
        };
        let changed = Self::time(facts.date);
        Ok(NodeInfo {
            kind,
            nlink,
            ino: facts.block,
            size,
            allocated,
            times: NodeTimes {
                created: changed,
                modified: changed,
                accessed: changed,
                changed,
            },
            content_gen: NodeInfo::NO_CONTENT_GEN,
        })
    }

    /// The names the file `file` has: its own, and one for each hard link
    /// the chain from its `next_link` holds.
    fn link_count(&mut self, file: u64, next_link: u32) -> Result<u32, DriverError> {
        let mut next = next_link;
        if next == 0 {
            return Ok(1);
        }
        if let Some(&count) = self.linked.get(file) {
            return Ok(count);
        }
        let mut count = 1u32;
        let mut guard = guard::Guard::new(file, self.disk.blocks);
        while next != 0 {
            guard.step(u64::from(next))?;
            let link = self.header(u64::from(next))?;
            if link.kind != Kind::FileLink || u64::from(link.real_entry()) != file {
                return Err(DriverError::BadMagic);
            }
            count = count.saturating_add(1);
            next = link.next_link();
        }
        if self.linked.len() >= LINK_COUNTS_KEPT {
            self.linked.clear();
        }
        // A count the cache has no room for is only counted again.
        let _ = self.linked.try_insert(file, count);
        Ok(count)
    }

    /// A link's target in POSIX spelling: a soft link's stored one, or the
    /// path from the volume's root to the directory a hard link names.
    fn link_target(&mut self, header: &Header) -> Result<alloc::string::String, DriverError> {
        if header.kind == Kind::SoftLink {
            return name::link_target_to_posix(header.link_target(), &self.volume, self.matching);
        }
        self.linked_dir(header.real_entry())
    }

    /// The path from the volume's root to the directory `block`, which a
    /// hard link names.
    fn linked_dir(&mut self, block: u32) -> Result<alloc::string::String, DriverError> {
        let dir = self.header(u64::from(block))?;
        if dir.kind != Kind::Directory {
            return Err(DriverError::BadMagic);
        }
        self.path_of(dir)
    }

    /// The absolute path, within the volume, of directory `dir`.
    fn path_of(&mut self, mut dir: Header) -> Result<alloc::string::String, DriverError> {
        let mut parts: Vec<Name> = Vec::new();
        let mut guard = guard::Guard::new(dir.block, self.disk.blocks);
        while dir.kind != Kind::Root {
            if !tairix_util::fallible::push(&mut parts, dir.name()?) {
                return Err(DriverError::OutOfMemory);
            }
            let parent = u64::from(dir.parent());
            guard.step(parent)?;
            dir = self.header(parent)?;
            if !dir.kind.is_directory() {
                return Err(DriverError::BadMagic);
            }
        }
        let mut path = alloc::string::String::new();
        let mut text = [0u8; 2 * tairix_fsprobe::affs::NAME_MAX];
        for part in parts.iter().rev() {
            let len = part.decode(&mut text);
            let part = core::str::from_utf8(&text[..len]).map_err(|_| DriverError::BadMagic)?;
            path.try_reserve(part.len() + 1)
                .map_err(|_| DriverError::OutOfMemory)?;
            path.push('/');
            path.push_str(part);
        }
        if path.is_empty() {
            path.push('/');
        }
        Ok(path)
    }

    /// Bytes of data and extension blocks a file of `size` bytes takes.
    fn allocated(&self, size: u64) -> u64 {
        let data = size.div_ceil(self.data_size() as u64);
        let table = self.at.table_len() as u64;
        let extensions = data.saturating_sub(table).div_ceil(table);
        (data + extensions) * self.disk.block_size as u64
    }

    /// Bytes of a file one data block carries: all of it on FFS, after the
    /// 24-byte header on OFS.
    fn data_size(&self) -> usize {
        if self.layout.dos_type.is_fast() {
            self.disk.block_size
        } else {
            self.disk.block_size - file::OFS_HEADER
        }
    }

    /// Run `op` on the bitmap, loaded on first use, and the blocks it lives
    /// in.
    fn with_bitmap<T>(
        &mut self,
        op: impl FnOnce(&mut Bitmap, &mut Disk<B>) -> Result<T, DriverError>,
    ) -> Result<T, DriverError> {
        if self.bitmap.is_none() {
            let loaded = self.load_bitmap()?;
            self.bitmap = Some(loaded);
        }
        match self.bitmap.as_mut() {
            Some(bitmap) => op(bitmap, &mut self.disk),
            None => Err(DriverError::OutOfMemory),
        }
    }

    /// The root's bitmap pointers and those its extension blocks chain.
    fn load_bitmap(&mut self) -> Result<Bitmap, DriverError> {
        let root = self.header(self.root_block())?;
        let blocks = self.disk.blocks;
        let pages = (blocks - BOOT_BLOCKS).div_ceil(bitmap::bits_per_page(self.disk.block_size));
        let mut pointers: Vec<u32> = Vec::new();
        pointers
            .try_reserve(usize::try_from(pages).map_err(|_| DriverError::OutOfMemory)?)
            .map_err(|_| DriverError::OutOfMemory)?;
        let mut extensions = Vec::new();
        // A page the volume needs with no block is damage, not a gap to close.
        let take = |pointers: &mut Vec<u32>, pointer: u32| {
            if (pointers.len() as u64) < pages {
                if pointer == 0 {
                    return Err(DriverError::BadMagic);
                }
                pointers.push(pointer);
            }
            Ok(())
        };
        for index in 0..BITMAP_PAGES {
            take(&mut pointers, root.long_at(self.at.bm_pages() + 4 * index))?;
        }
        let mut next = root.long_at(self.at.bm_ext());
        let mut guard = guard::Guard::new(root.block, blocks);
        let mut buf = self.disk.buffer()?;
        let per = self.at.longs() - 1;
        while next != 0 && (pointers.len() as u64) < pages {
            guard.step(u64::from(next))?;
            if !tairix_util::fallible::push(&mut extensions, next) {
                return Err(DriverError::OutOfMemory);
            }
            self.disk.read(u64::from(next), &mut buf)?;
            for index in 0..per {
                take(&mut pointers, be32(&buf, 4 * index))?;
            }
            next = be32(&buf, 4 * per);
        }
        Bitmap::new(
            pointers,
            &extensions,
            self.disk.block_size,
            blocks,
            self.root_block(),
        )
    }

    /// Refuse a change to a volume the driver will not write.
    fn writable(&self) -> Result<(), DriverError> {
        match self.write_refusal {
            Some(_) => Err(DriverError::ReadOnly),
            None => Ok(()),
        }
    }

    /// Run the change `op`, inside the open transaction or one it opens, and
    /// publish the transaction once it is due — at once with no host to
    /// publish it later.
    ///
    /// A change that fails after it wrote a block or moved one in the bitmap
    /// may have stopped part-way, and one that found a structure damaged has
    /// shown the volume unsafe to write: either way the volume is left as it
    /// is, its bitmap marked invalid on the medium for AmigaDOS to rebuild,
    /// and nothing more is written to it. Any other failure is a refusal and
    /// leaves the transaction as it was.
    fn change<T>(
        &mut self,
        op: impl FnOnce(&mut Self) -> Result<T, DriverError>,
    ) -> Result<T, DriverError> {
        self.writable()?;
        self.reading = None;
        self.listing = None;
        self.mutated = false;
        self.disk.stock()?;
        self.linked.clear();
        let done = op(self);
        self.disk.spending = false;
        self.linked.clear();
        match &done {
            Ok(_) if self.transaction.is_some() => self.schedule.joined(),
            Err(err) if self.mutated || *err == DriverError::BadMagic => {
                self.stop_writing(WriteRefusal::Damaged);
                return done;
            }
            Ok(_) | Err(_) => {}
        }
        if !self.schedule.defers() || self.schedule.expired() {
            self.commit()?;
        }
        done
    }

    /// Write nothing more to the volume, for `why`. The open transaction is
    /// dropped, leaving the root marked invalid on the medium, and so are the
    /// bitmap and the chains, which may hold a part-way change; the next flush
    /// still forces out of the device's cache what the device already accepted.
    fn stop_writing(&mut self, why: WriteRefusal) {
        self.write_refusal = Some(why);
        self.transaction = None;
        self.unflushed = true;
        self.bitmap = None;
        self.chains.clear();
        self.schedule.closed();
    }

    /// Open a transaction, if none is: the root marked invalid, and that on
    /// the medium before any change reaches it.
    fn begin(&mut self) -> Result<(), DriverError> {
        if self.transaction.is_some() {
            return Ok(());
        }
        self.with_bitmap(|_, _| Ok(()))?;
        let mut root = self.header(self.root_block())?;
        root.set_long_at(self.at.bm_flag(), 0);
        self.disk.write(root.block, root.sealed())?;
        self.disk.dev.flush()?;
        self.transaction = Some(root);
        self.schedule.opened();
        Ok(())
    }

    /// Publish the open transaction: the bitmap's changed pages and the
    /// device's cache, then the root stamped with the volume's alteration and
    /// marked valid — unless an orphan's blocks are still allocated — and the
    /// cache again. It reads nothing and allocates nothing, so a heap running
    /// short never stops a commit.
    fn commit(&mut self) -> Result<(), DriverError> {
        let Some(mut root) = self.transaction.take() else {
            return Ok(());
        };
        let published = self.publish(&mut root);
        if published.is_err() {
            self.stop_writing(WriteRefusal::CommitFailed);
        } else {
            self.schedule.closed();
        }
        published
    }

    fn publish(&mut self, root: &mut Header) -> Result<(), DriverError> {
        if let Some(bitmap) = self.bitmap.as_mut() {
            bitmap.commit(&mut self.disk)?;
        }
        // Nothing the root vouches for may land after it.
        self.disk.dev.flush()?;
        let flag = if self.orphans == 0 { BITMAP_VALID } else { 0 };
        root.set_long_at(self.at.bm_flag(), flag);
        root.set_volume_date(self.now());
        self.disk.write(root.block, root.sealed())?;
        self.disk.dev.flush()
    }
}

/// The date stamp of `time`, to the tick; none (all zero) for a time unknown
/// or before 1978, which a stamp cannot hold.
fn stamp(time: Option<Time64>) -> DateStamp {
    time.and_then(|time| amiga::time64_to_datestamp(amiga::to_tick(time)).ok())
        .map_or(DateStamp::default(), |(days, mins, ticks)| DateStamp {
            days,
            mins,
            ticks,
        })
}

impl<B: Block> FilesystemRead for Affs<B> {
    fn root(&self) -> NodeId {
        NodeId::from_raw(self.layout.root)
    }

    fn name_matching(&self) -> NameMatching {
        self.matching
    }

    fn node_identity(&self) -> NodeIdentity {
        NodeIdentity::Stable
    }

    fn node_info(&mut self, node: NodeId) -> Result<NodeInfo, DriverError> {
        let header = self.node(node)?;
        self.info(&header)
    }

    fn lookup(&mut self, dir: NodeId, name: &[u8]) -> Result<NodeId, DriverError> {
        let dir = self.directory(dir)?;
        let Ok(name) = Name::encode(name) else {
            return Err(DriverError::NotFound);
        };
        let entry = self.find(&dir, &name)?.ok_or(DriverError::NotFound)?;
        let node = self.entry_node(entry)?;
        Ok(NodeId::from_raw(node.block))
    }

    fn parent(&mut self, dir: NodeId) -> Result<NodeId, DriverError> {
        let header = self.node(dir)?;
        match header.kind {
            Kind::Root => Ok(dir),
            Kind::Directory if !self.unnamed(header.block) => {
                let parent = self.header(u64::from(header.parent()))?;
                if !parent.kind.is_directory() {
                    return Err(DriverError::BadMagic);
                }
                Ok(NodeId::from_raw(parent.block))
            }
            Kind::Directory => Err(DriverError::NotFound),
            _ => Err(DriverError::Unsupported),
        }
    }

    fn read_at(&mut self, file: NodeId, offset: u64, buf: &mut [u8]) -> Result<usize, DriverError> {
        let mut reading = match self.reading.take() {
            Some(reading) if reading.header.block == file.raw() => reading,
            _ => {
                let header = self.node(file)?;
                if header.kind != Kind::File {
                    return Err(DriverError::Unsupported);
                }
                Reading { header, held: None }
            }
        };
        let read = self.read_file(&reading.header, offset, buf, &mut reading.held);
        if read.is_ok() {
            self.reading = Some(reading);
        }
        read
    }

    fn read_link(&mut self, link: NodeId, out: &mut [u8]) -> Result<usize, DriverError> {
        let header = self.node(link)?;
        if !matches!(header.kind, Kind::SoftLink | Kind::DirLink) {
            return Err(DriverError::Unsupported);
        }
        let target = self.link_target(&header)?;
        let slot = out
            .get_mut(..target.len())
            .ok_or(DriverError::BufferTooSmall)?;
        slot.copy_from_slice(target.as_bytes());
        Ok(target.len())
    }

    fn read_dir(
        &mut self,
        dir: NodeId,
        cursor: u64,
        _after: &[u8],
        visit: &mut dyn FnMut(&DirEntry, &[u8]) -> DirVisit,
    ) -> Result<(), DriverError> {
        let header = self.node(dir)?;
        if !header.kind.is_directory() {
            return Err(DriverError::Unsupported);
        }
        if self.unnamed(header.block) {
            return Ok(());
        }
        self.list(&header, cursor, visit)
    }

    fn pin(&mut self, node: NodeId) -> Result<NodeId, DriverError> {
        if let Some(hold) = self.held.get_mut(node.raw()) {
            hold.count = hold.count.checked_add(1).ok_or(DriverError::OutOfMemory)?;
            return Ok(node);
        }
        self.node(node)?;
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
        if !hold.unnamed {
            self.held.remove(pinned.raw());
            return Ok(Unpinned::Released);
        }
        hold.count = 0;
        match self.change(|fs| fs.free_orphan(pinned.raw())) {
            Ok(()) => {
                self.held.remove(pinned.raw());
                Ok(Unpinned::Freed)
            }
            // Refused before it wrote: the orphan is owed its free, which
            // the release of the mount's pins gives it.
            Err(DriverError::OutOfMemory) if self.write_refusal.is_none() => {
                Err(DriverError::DeviceFault)
            }
            Err(_) => {
                self.held.remove(pinned.raw());
                Err(DriverError::DeviceFault)
            }
        }
    }

    fn release_pins(&mut self) -> Result<(), DriverError> {
        let held = core::mem::take(&mut self.held);
        let mut freed = Ok(());
        for (block, hold) in &held {
            if !hold.unnamed {
                continue;
            }
            if self.change(|fs| fs.free_orphan(block)).is_err() {
                freed = Err(DriverError::DeviceFault);
            }
        }
        freed
    }
}

impl<B: Block> FilesystemSecurity for Affs<B> {
    /// AmigaDOS records no owner, mode or ACL: every node reports the
    /// unrecorded default and the VFS presents the volume.
    fn security(&mut self, node: NodeId) -> Result<NodeSecurity, DriverError> {
        let header = self.node(node)?;
        Ok(NodeSecurity::unrecorded(node_kind(header.kind)?))
    }

    fn set_security(&mut self, node: NodeId, _security: NodeSecurity) -> Result<(), DriverError> {
        self.node(node)?;
        Err(DriverError::Unsupported)
    }

    fn records_ownership(&mut self, node: NodeId) -> Result<bool, DriverError> {
        self.node(node)?;
        Ok(false)
    }
}

impl<B: Block> FilesystemStats for Affs<B> {
    fn stats(&mut self) -> Result<VolumeStats, DriverError> {
        let total = self.disk.blocks - BOOT_BLOCKS;
        let free = match self.write_refusal {
            // An unvalidated bitmap's count is not to be trusted.
            Some(WriteRefusal::NeedsCheck) => 0,
            _ => self.with_bitmap(|bitmap, disk| bitmap.free(disk))?,
        };
        Ok(VolumeStats {
            block_size: self.layout.block_size,
            in_reserve: false,
            total_blocks: total,
            free_blocks: free,
            avail_blocks: if self.write_refusal.is_some() {
                0
            } else {
                free
            },
            files: 0,
            files_free: 0,
        })
    }
}

/// The volume's name is its title, held to a new entry's rules: ISO 8859-1,
/// one to 30 bytes, and never a control character or the `:` and `/`
/// AmigaDOS addresses a volume and separates a path with — a volume with no
/// name is one AmigaDOS cannot address.
impl<B: Block> FilesystemLabel for Affs<B> {
    fn label(&mut self) -> Result<VolumeTitle, DriverError> {
        let mut spelt = [0u8; 2 * NAME_MAX];
        let len = self.volume.decode(&mut spelt);
        let text = core::str::from_utf8(&spelt[..len]).map_err(|_| DriverError::OutOfRange)?;
        VolumeTitle::new(text).map_err(DriverError::from_errno)
    }

    fn set_label(&mut self, title: &VolumeTitle) -> Result<(), DriverError> {
        let name = Name::encode(title.as_str().as_bytes())?.given()?;
        self.change(|fs| {
            let mut root = fs.header(fs.root_block())?;
            root.set_name(&name);
            root.set_date(fs.now());
            fs.write_header(&mut root)
        })?;
        self.volume = name;
        Ok(())
    }
}

impl<B: Block> Affs<B> {
    /// What `node` surfaces as `amiga` attributes: nothing for the root and
    /// for a hard link to a directory, which keep no protection or comment.
    fn amiga_object(header: &Header) -> Option<amiga::Object<'_>> {
        matches!(header.kind, Kind::Directory | Kind::File | Kind::SoftLink).then(|| {
            amiga::Object {
                protection: Some(header.protection_byte()),
                comment: header.comment(),
            }
        })
    }
}

impl<B: Block> FilesystemAttrs for Affs<B> {
    fn get_attr(
        &mut self,
        node: NodeId,
        key: &[u8],
        value_out: &mut [u8],
    ) -> Result<Option<usize>, DriverError> {
        let key = tairix_fsmeta::AttrKey::parse(key).map_err(|_| DriverError::OutOfRange)?;
        let header = self.node(node)?;
        let Some(value) =
            Self::amiga_object(&header).and_then(|object| object.value(key.as_bytes()))
        else {
            return Ok(None);
        };
        let bytes = value.as_bytes();
        let out = value_out
            .get_mut(..bytes.len())
            .ok_or(DriverError::BufferTooSmall)?;
        out.copy_from_slice(bytes);
        Ok(Some(bytes.len()))
    }

    fn set_attr(&mut self, node: NodeId, key: &[u8], value: &[u8]) -> Result<(), DriverError> {
        let key = tairix_fsmeta::AttrKey::parse(key).map_err(|_| DriverError::OutOfRange)?;
        let header = self.node(node)?;
        if Self::amiga_object(&header).is_none() {
            return Err(DriverError::Unsupported);
        }
        let mut header = header;
        match key.as_bytes() {
            amiga::PROTECTION_KEY => {
                let low =
                    amiga::protection_from_value(value).map_err(|_| DriverError::OutOfRange)?;
                header.set_protect((header.protect() & !0xFF) | u32::from(low));
            }
            amiga::COMMENT_KEY => {
                amiga::validate_comment(value).map_err(|_| DriverError::LengthOutOfRange)?;
                header.set_comment(value);
            }
            _ => return Err(DriverError::Unsupported),
        }
        self.change(|fs| fs.write_header(&mut header))
    }

    fn list_attr(
        &mut self,
        node: NodeId,
        index: u64,
        key_out: &mut [u8],
    ) -> Result<Option<usize>, DriverError> {
        let header = self.node(node)?;
        let Some(key) = Self::amiga_object(&header).and_then(|object| object.key(index)) else {
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
        let header = self.node(node)?;
        let Some(object) = Self::amiga_object(&header) else {
            return Err(DriverError::NotFound);
        };
        match key.as_bytes() {
            // Every object keeps protection bits: they can be changed, never
            // taken away.
            amiga::PROTECTION_KEY => Err(DriverError::Unsupported),
            amiga::COMMENT_KEY if object.carries(amiga::COMMENT_KEY) => {
                let mut header = header;
                header.set_comment(&[]);
                self.change(|fs| fs.write_header(&mut header))
            }
            _ => Err(DriverError::NotFound),
        }
    }
}

impl<B: Block> FilesystemAttrsProvider for Affs<B> {
    fn attrs_fs(&mut self) -> Option<&mut dyn FilesystemAttrsFs> {
        Some(self)
    }
}

impl<B: Block> Affs<B> {
    /// Refuse every change for `why`, whatever the volume would allow: a
    /// volume served read-only.
    pub(crate) fn refuse_writes(&mut self, why: WriteRefusal) {
        self.write_refusal.get_or_insert(why);
    }
}

impl<B: Block> FilesystemWrite for Affs<B> {
    /// AmigaDOS records no owner or mode, so `record` is not kept.
    fn create(
        &mut self,
        dir: NodeId,
        name: &[u8],
        kind: NodeKind,
        _record: &NodeSecurity,
    ) -> Result<NodeId, DriverError> {
        let kind = match kind {
            NodeKind::Directory => Kind::Directory,
            NodeKind::RegularFile => Kind::File,
            NodeKind::Symlink => return Err(DriverError::Unsupported),
        };
        self.create_entry(dir, name, kind, None)
    }

    /// The target is stored in AmigaDOS's spelling; `record` is not kept.
    fn create_link(
        &mut self,
        dir: NodeId,
        name: &[u8],
        target: &[u8],
        _record: &NodeSecurity,
    ) -> Result<NodeId, DriverError> {
        let stored = name::link_target_to_amiga(target, Header::link_room(self.at))?;
        self.create_entry(dir, name, Kind::SoftLink, Some(&stored))
    }

    fn link(&mut self, dir: NodeId, name: &[u8], node: NodeId) -> Result<(), DriverError> {
        self.link_entry(dir, name, node)
    }

    fn write_at(&mut self, file: NodeId, offset: u64, data: &[u8]) -> Result<usize, DriverError> {
        self.write_file(file, offset, data)
    }

    fn truncate(&mut self, file: NodeId, size: u64) -> Result<(), DriverError> {
        self.truncate_file(file, size)
    }

    fn remove(&mut self, dir: NodeId, name: &[u8]) -> Result<(), DriverError> {
        self.remove_entry(dir, name)
    }

    fn rename(
        &mut self,
        src_dir: NodeId,
        src_name: &[u8],
        dst_dir: NodeId,
        dst_name: &[u8],
    ) -> Result<(), DriverError> {
        self.rename_entry(src_dir, src_name, dst_dir, dst_name)
    }

    /// Publish the open transaction; the commit itself flushes the device.
    fn flush(&mut self) -> Result<(), DriverError> {
        if self.transaction.is_some() {
            return self.commit();
        }
        if self.unflushed {
            self.disk.dev.flush()?;
            self.unflushed = false;
        }
        Ok(())
    }

    fn set_volume_host(&mut self, volume: DriverHandle, host: &'static dyn VolumeHost) {
        self.schedule.set_host(volume, host);
    }
}
