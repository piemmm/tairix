//! TAIRiX FAT32 filesystem driver (read/write).
//!
//! Reads and writes a FAT32 volume sitting behind any
//! [`tairix_abi::driver::block::Block`] device and exposes it through
//! the versioned [`tairix_abi::driver::filesystem::FilesystemRead`] and
//! [`tairix_abi::driver::filesystem::FilesystemWrite`] surfaces
//! (new behaviour ships as a new trait, never by
//! widening the frozen mount/unmount
//! [`Filesystem`](tairix_abi::driver::filesystem::Filesystem)).
//!
//! FAT32 has no per-inode owner, mode, ACL, or capability gate; those
//! live in the VFS metadata layer that mounts this
//! driver. The driver therefore makes **no** permission decisions
//! (the VFS is the policy point, this is raw structural I/O).
//!
//! # Public surface
//!
//! Per the only public *function* is [`register`].
//! [`Fat32`] is a public *type* the driver host instantiates with
//! [`Fat32::open`]; the host reaches into it only through the
//! [`FilesystemRead`] and [`FilesystemWrite`] traits.
//!
//! # Scope
//!
//! Read and write. Long file names (VFAT) are reconstructed on read:
//! each entry exposes a single name — its long name when a valid,
//! checksum-matching long-name set precedes the 8.3 short entry, and
//! otherwise the short name (so a volume written without long names is
//! still fully readable). When a long name is present the internal 8.3
//! alias is *not* separately resolvable; the long name is the entry's
//! name. Names are returned as UTF-8 — UTF-16LE long names are decoded,
//! and the driver falls back to the short name on any malformed set
//! rather than surfacing a partial name.
//!
//! Writing creates files and directories, extends/overwrites file data
//! (allocating and chaining clusters, zero-filling sparse gaps),
//! truncates (shrinking frees the tail chain, growing zero-extends), and
//! unlinks files and empty directories. Every created entry is written
//! as a VFAT long-name set bound to a generated, directory-unique 8.3
//! short alias (so arbitrary, case-preserving names round-trip), and
//! every FAT mutation is mirrored across all FAT copies. No
//! `unwrap`/`expect`/`panic!` and no `unsafe`.
//!
//! The volume label ([`FilesystemLabel`]) is read as the volume probe reads
//! it and written to both places FAT keeps it: the root directory's label
//! entry, where the probe reads it, and the boot sector's `BS_VolLab`, with
//! its backup's.
//!
//! A file the kernel pins ([`FilesystemRead::pin`]) is reported under a token
//! until its last unpin, since its own id is where its entry lies and a rename
//! moves that; a removal deletes a pinned file's entry but keeps its chain
//! until then. FAT records no owner or mode, so a create ignores the record it
//! is given.
//!
//! # Capabilities
//!
//! Loading requires
//! [`CapabilityId::DRV_LOAD`](tairix_abi::CapabilityId::DRV_LOAD). The
//! driver runs in user space; it does not request `CAP_DRV_KERNEL`.

#![no_std]
#![forbid(unsafe_op_in_unsafe_fn)]
#![deny(missing_docs)]

use tairix_abi::driver::block::{Block, MAX_BLOCK_LEN};
use tairix_abi::driver::filesystem::unkept_stamp;
use tairix_abi::driver::filesystem::{
    DirEntry, DirVisit, FilesystemAttrsProvider, FilesystemLabel, FilesystemRead,
    FilesystemSecurity, FilesystemStats, FilesystemWrite, NameMatching, NodeId, NodeIdentity,
    NodeInfo, NodeKind, NodeSecurity, NodeTimes, Unpinned, VolumeHost, VolumeStats, VolumeTitle,
    WriteRefusal,
};
use tairix_abi::fs::{StampChange, TimesChange, TimesKept, FS_NAME_MAX};
use tairix_abi::time::Time64;
use tairix_abi::volume::VolumeFsType;
use tairix_abi::{CapabilityId, DriverError, DriverHandle, DriverHost};
use tairix_bytedev::ByteDevice;
use tairix_collections::RadixTree;
use tairix_fat::boot::{at, Bpb, Kind, BOOT_SECTOR};
use tairix_fat::dir::{
    self, attr, LongName, ShortEntry, Slot, DELETED, DOT, DOTDOT, ENTRY, LONG_NAME_BYTES,
    LONG_NAME_UNITS, LONG_UNITS, MAX_ENTRIES,
};
use tairix_fat::format::{self, FormatError, Medium, Shape};
use tairix_fat::name::{self, AliasStem, LabelError, TailWindow, LABEL_LEN, NO_LABEL, TAIL_WINDOW};
use tairix_fat::stamp::{self, Stamp, Stamps};
use tairix_fat::table::{self, Link};
use tairix_fsprobe::{fat32_bpb, fat32_identity, Recognised};

mod served;

pub use served::{Served, BIND_KEYS};

/// Per-driver `DriverHandle` marker returned by [`register`].
const REGISTER_HANDLE_MARKER: u64 = 0x4641_5433_3200_0001; // "FAT32" + index

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

/// The one kind of FAT volume the driver serves.
const KIND: Kind = Kind::Fat32;

/// The bytes of one allocation-table entry.
const FAT_ENTRY: usize = (KIND.entry_bits() / 8) as usize;

/// The name the driver's format writes into a boot sector.
const OEM: &[u8; 8] = b"TAIRIX  ";

/// Bit of a directory's id carrying the directory flag: cluster numbers are
/// 28-bit, so bit 28 is free.
const NODE_DIR_FLAG: u64 = 1 << 28;
/// Bit set on every directory's id, so none is [`NodeId::NONE`] whatever
/// cluster a damaged entry records.
const NODE_VALID_FLAG: u64 = 1 << 29;
/// Bit set on every file's id, whose other bits are its short entry's slot
/// on the device; no directory's id sets it.
const NODE_FILE_TAG: u64 = 1 << 63;
/// Bit set on every pinned file's token, whose other bits are a number the
/// mount never gives twice; no file's or directory's own id sets it.
const NODE_TOKEN_TAG: u64 = 1 << 62;

/// The id of the directory whose first cluster is `cluster`.
fn dir_node(cluster: u32) -> NodeId {
    NodeId::from_raw(NODE_VALID_FLAG | NODE_DIR_FLAG | u64::from(cluster & table::mask(KIND)))
}

/// The id of the file whose short entry lies at device byte `short_offset`.
/// The entry holds the file's size, first cluster and stamps, so a write
/// keeps the id, and a rename or a removal, which free the entry, do not.
fn file_node(short_offset: u64) -> NodeId {
    NodeId::from_raw(NODE_FILE_TAG | entry_slot(short_offset))
}

/// The slot of the device the 32-byte entry at device byte `short_offset`
/// takes.
fn entry_slot(short_offset: u64) -> u64 {
    short_offset / ENTRY as u64
}

/// The token numbered `number`.
fn token_node(number: u64) -> NodeId {
    NodeId::from_raw(NODE_TOKEN_TAG | number)
}

/// What an id names, before the device is consulted.
#[derive(Clone, Copy)]
enum Named {
    /// A directory, by its first cluster.
    Dir(u32),
    /// A file, by the device byte offset of its short entry.
    File(u64),
    /// A pinned file, by its token's number.
    Pinned(u64),
}

/// What `node` names; `None` for an id no directory or file is given.
fn named(node: NodeId) -> Option<Named> {
    let raw = node.raw();
    if raw & NODE_FILE_TAG != 0 {
        return (raw & !NODE_FILE_TAG)
            .checked_mul(ENTRY as u64)
            .map(Named::File);
    }
    if raw & NODE_TOKEN_TAG != 0 {
        return Some(Named::Pinned(raw & !NODE_TOKEN_TAG));
    }
    let flags = NODE_VALID_FLAG | NODE_DIR_FLAG;
    let cluster = u32::try_from(raw & !flags)
        .ok()
        .filter(|&cluster| cluster <= table::mask(KIND))?;
    (raw & flags == flags).then_some(Named::Dir(cluster))
}

/// The first cluster of the directory `dir` names.
///
/// # Errors
///
/// [`DriverError::Unsupported`] for an id naming anything but a directory.
fn cluster_of(dir: NodeId) -> Result<u32, DriverError> {
    match named(dir) {
        Some(Named::Dir(cluster)) => Ok(cluster),
        _ => Err(DriverError::Unsupported),
    }
}

/// Computed geometry of a validated FAT32 volume, in bytes.
struct Layout {
    bytes_per_cluster: u64,
    fat_start_byte: u64,
    data_start_byte: u64,
    root_cluster: u32,
    /// Size of one FAT, in bytes (each of [`Layout::num_fats`] copies).
    fat_size_bytes: u64,
    /// Number of FAT copies; every FAT mutation is mirrored across all
    /// of them.
    num_fats: u64,
    /// Highest valid data-cluster number. Data clusters are numbered
    /// `2..=max_cluster`; allocation never hands out a number above it.
    max_cluster: u32,
}

/// The instants `stamps` hold, read as UTC. FAT keeps no metadata-change
/// stamp, so `changed` is the epoch.
fn node_times(stamps: &Stamps) -> NodeTimes {
    NodeTimes {
        created: stamps.created(),
        modified: stamps.written(),
        accessed: stamps.accessed(),
        changed: Time64::UNIX_EPOCH,
    }
}

/// A single decoded directory entry. `name` holds the file name as
/// UTF-8 bytes — the reconstructed long name when one is present and
/// valid, otherwise the 8.3 short name.
struct ParsedEntry {
    name: [u8; LONG_NAME_BYTES],
    name_len: usize,
    cluster: u32,
    size: u32,
    attributes: u8,
    /// The stamps as stored, for rewriting the entry without loss.
    stamps: Stamps,
    /// Device byte offset of the 8.3 short entry (the one carrying the
    /// cluster and size); the write path patches metadata here.
    short_offset: u64,
    /// The first slot of this logical entry: its first long-name fragment,
    /// or the short entry when none precede it.
    first: SlotPos,
    /// Number of 32-byte slots this logical entry occupies (long-name
    /// fragments plus the short entry).
    slot_span: u64,
}

impl ParsedEntry {
    /// The entry `short` records, named by its 8.3 name, its short entry at
    /// device byte `short_offset` and its run `slot_span` slots from `first`.
    fn of(short: &ShortEntry, short_offset: u64, first: SlotPos, slot_span: u64) -> Self {
        let mut shown = [0u8; 12];
        let name_len = name::display(&short.name, &mut shown);
        let mut name = [0u8; LONG_NAME_BYTES];
        name[..name_len].copy_from_slice(&shown[..name_len]);
        Self {
            name,
            name_len,
            cluster: short.cluster,
            size: short.size,
            attributes: short.attributes,
            stamps: short.stamps,
            short_offset,
            first,
            slot_span,
        }
    }

    fn is_dir(&self) -> bool {
        self.attributes & attr::DIRECTORY != 0
    }
}

/// A slot's place in its directory: the cluster holding it, its byte within
/// that cluster, and its index counted from the directory's first slot.
#[derive(Clone, Copy)]
struct SlotPos {
    cluster: u32,
    intra: u64,
    slot: u64,
}

impl SlotPos {
    /// The first slot of the directory starting at `cluster`.
    const fn at(cluster: u32) -> Self {
        Self {
            cluster,
            intra: 0,
            slot: 0,
        }
    }
}

/// Where a directory seek ended: the directory's first cluster, the index of
/// a cluster in its chain, and that cluster.
#[derive(Clone, Copy)]
struct SeekHint {
    dir: u32,
    index: u64,
    cluster: u32,
}

/// Cursor walking a directory's cluster chain, 32 bytes at a time.
struct DirCursor {
    /// The next slot to read.
    pos: SlotPos,
    /// The device block the last slot came from, so a walk reads each block
    /// once rather than once per slot it holds.
    held: Option<u64>,
    block: [u8; MAX_BLOCK_LEN],
}

impl DirCursor {
    /// A walk from the first slot of the directory starting at `cluster`.
    fn at(cluster: u32) -> Self {
        Self {
            pos: SlotPos::at(cluster),
            held: None,
            block: [0; MAX_BLOCK_LEN],
        }
    }
}

/// A FAT32 volume backed by a [`Block`] device.
pub struct Fat32<B: Block> {
    dev: ByteDevice<B>,
    layout: Layout,
    /// Forward search hint for the cluster allocator: the cluster to try
    /// first on the next [`Fat32::alloc_cluster`]. It only moves forward
    /// (wrapping once at `max_cluster`) and is reset downward whenever a
    /// lower-numbered cluster is freed, so the allocator amortises to a
    /// single forward step per allocation while still finding every free
    /// cluster — turning a sequential fill from O(n²) into O(n).
    next_free: u32,
    /// Live count of free data clusters, established by one FAT scan at
    /// open and maintained by the allocator and the chain-free path, so
    /// [`FilesystemStats::stats`] never re-scans the FAT.
    free_clusters: u64,
    /// The volume's stable 16-byte identity (see [`Fat32::volume_identity`]).
    identity: [u8; 16],
    /// Where the last listing batch stopped in its directory's chain, so the
    /// next batch walks on from there rather than from the first cluster:
    /// reading a directory a batch at a time costs one walk of its chain. A
    /// live directory's chain only grows, so the hint holds until a chain is
    /// freed.
    seek: Option<SeekHint>,
    /// The host the mount's stamps take their time from, once it has one.
    host: Option<&'static dyn VolumeHost>,
    /// The kernel's holds on the mount's nodes.
    pins: Pins,
    /// Why the volume takes no writes, once a reason is found: it is served
    /// read-only, or a retitle failed part-way and may have left the copies
    /// of its label disagreeing.
    refusal: Option<WriteRefusal>,
}

/// The kernel's holds on a mount's nodes.
///
/// A directory keeps its id, its first cluster, while it is held, since a
/// held directory's cluster is not freed. A file's own id is where its entry
/// lies, which a rename moves, so a held file is reported under a token.
/// Every table grows fallibly, so a hold that cannot be recorded is refused.
#[derive(Default)]
struct Pins {
    /// Each pinned file, by its token's number.
    files: RadixTree<PinnedFile>,
    /// The number of each pinned file's token, by the device slot of the
    /// entry naming it.
    by_entry: RadixTree<u64>,
    /// Each pinned directory, by its first cluster.
    dirs: RadixTree<PinnedDir>,
    /// The number the next token takes: none the mount has given before.
    next: u64,
}

/// A pinned file: its holds, and where its facts are kept.
struct PinnedFile {
    holds: u32,
    kept: KeptFile,
}

/// Where a pinned file's first cluster, size and stamps are kept.
#[derive(Clone, Copy)]
enum KeptFile {
    /// In its short entry, at this device byte.
    Entry(u64),
    /// In the pin: a removal deleted its entry.
    Nameless(Nameless),
}

/// What a pinned file's deleted entry held.
#[derive(Clone, Copy)]
struct Nameless {
    /// Where the entry lay, which `stat` goes on reporting as its number.
    last: u64,
    cluster: u32,
    size: u32,
    stamps: Stamps,
}

/// A pinned directory: its holds and, once a removal deleted its entry, the
/// stamps that entry held.
struct PinnedDir {
    holds: u32,
    removed: Option<NodeTimes>,
}

impl Pins {
    /// One more hold of the file whose short entry lies at `short_offset`.
    fn pin_file(&mut self, short_offset: u64) -> Result<NodeId, DriverError> {
        let slot = entry_slot(short_offset);
        if let Some(&number) = self.by_entry.get(slot) {
            return self.hold_file(number);
        }
        let number = self.next;
        if number >= NODE_TOKEN_TAG {
            return Err(DriverError::OutOfMemory);
        }
        self.files
            .try_reserve_key(number)
            .map_err(|_| DriverError::OutOfMemory)?;
        self.by_entry
            .try_reserve_key(slot)
            .map_err(|_| DriverError::OutOfMemory)?;
        let pin = PinnedFile {
            holds: 1,
            kept: KeptFile::Entry(short_offset),
        };
        self.files
            .try_insert(number, pin)
            .map_err(|_| DriverError::OutOfMemory)?;
        if self.by_entry.try_insert(slot, number).is_err() {
            self.files.remove(number);
            return Err(DriverError::OutOfMemory);
        }
        self.next = number + 1;
        Ok(token_node(number))
    }

    /// One more hold of the pinned file numbered `number`.
    fn hold_file(&mut self, number: u64) -> Result<NodeId, DriverError> {
        let pin = self.files.get_mut(number).ok_or(DriverError::NotFound)?;
        pin.holds = pin.holds.checked_add(1).ok_or(DriverError::OutOfMemory)?;
        Ok(token_node(number))
    }

    /// One more hold of the directory at `cluster`.
    fn pin_dir(&mut self, cluster: u32) -> Result<NodeId, DriverError> {
        let key = u64::from(cluster);
        if let Some(held) = self.dirs.get_mut(key) {
            held.holds = held.holds.checked_add(1).ok_or(DriverError::OutOfMemory)?;
        } else {
            let held = PinnedDir {
                holds: 1,
                removed: None,
            };
            self.dirs
                .try_insert(key, held)
                .map_err(|_| DriverError::OutOfMemory)?;
        }
        Ok(dir_node(cluster))
    }

    /// Where the pinned file numbered `number` keeps its facts.
    fn kept(&self, number: u64) -> Result<KeptFile, DriverError> {
        self.files
            .get(number)
            .map(|pin| pin.kept)
            .ok_or(DriverError::NotFound)
    }

    /// Release one hold of the pinned file numbered `number`: `None` while
    /// others remain, else where the last left its facts.
    fn unpin_file(&mut self, number: u64) -> Result<Option<KeptFile>, DriverError> {
        let pin = self.files.get_mut(number).ok_or(DriverError::NotFound)?;
        if pin.holds > 1 {
            pin.holds -= 1;
            return Ok(None);
        }
        let kept = pin.kept;
        self.files.remove(number);
        if let KeptFile::Entry(short_offset) = kept {
            self.by_entry.remove(entry_slot(short_offset));
        }
        Ok(Some(kept))
    }

    /// Release one hold of the directory at `cluster`: [`Unpinned::Freed`]
    /// asks for its chain to be freed.
    fn unpin_dir(&mut self, cluster: u32) -> Result<Unpinned, DriverError> {
        let key = u64::from(cluster);
        let held = self.dirs.get_mut(key).ok_or(DriverError::NotFound)?;
        if held.holds > 1 {
            held.holds -= 1;
            return Ok(Unpinned::Held);
        }
        let removed = held.removed.is_some();
        self.dirs.remove(key);
        Ok(if removed {
            Unpinned::Freed
        } else {
            Unpinned::Released
        })
    }

    /// Whether a pin holds what `entry` names.
    fn holds(&self, entry: &ParsedEntry) -> bool {
        if entry.is_dir() {
            self.dirs.contains_key(u64::from(entry.cluster))
        } else {
            self.by_entry.contains_key(entry_slot(entry.short_offset))
        }
    }

    /// The stamps of the directory at `cluster` while a pin keeps it after a
    /// removal.
    fn removed(&self, cluster: u32) -> Option<NodeTimes> {
        self.dirs
            .get(u64::from(cluster))
            .and_then(|held| held.removed)
    }

    /// The stamps a removal left with the nameless pinned file `number`, or
    /// the removed directory at `cluster`, become `stamps`.
    fn restamp(&mut self, at: StampsAt, stamps: Stamps) {
        match at {
            StampsAt::Nameless(number) => {
                if let Some(PinnedFile {
                    kept: KeptFile::Nameless(kept),
                    ..
                }) = self.files.get_mut(number)
                {
                    kept.stamps = stamps;
                }
            }
            StampsAt::Removed(cluster) => {
                if let Some(held) = self.dirs.get_mut(u64::from(cluster)) {
                    held.removed = Some(node_times(&stamps));
                }
            }
            StampsAt::Entry(_) => {}
        }
    }

    /// The entry at `from` naming a pinned file has been written afresh at
    /// `to`: its pin follows it, or, when that cannot be recorded, stays.
    fn moved(&mut self, from: u64, to: u64) -> Result<(), DriverError> {
        let Some(&number) = self.by_entry.get(entry_slot(from)) else {
            return Ok(());
        };
        self.by_entry
            .try_insert(entry_slot(to), number)
            .map_err(|_| DriverError::OutOfMemory)?;
        self.by_entry.remove(entry_slot(from));
        if let Some(pin) = self.files.get_mut(number) {
            pin.kept = KeptFile::Entry(to);
        }
        Ok(())
    }

    /// A removal deleted `entry`: the pin of what it named keeps that.
    fn unnamed(&mut self, entry: &ParsedEntry) {
        if entry.is_dir() {
            if let Some(held) = self.dirs.get_mut(u64::from(entry.cluster)) {
                held.removed = Some(node_times(&entry.stamps));
            }
        } else if let Some(number) = self.by_entry.remove(entry_slot(entry.short_offset)) {
            if let Some(pin) = self.files.get_mut(number) {
                pin.kept = KeptFile::Nameless(Nameless {
                    last: entry.short_offset,
                    cluster: entry.cluster,
                    size: entry.size,
                    stamps: entry.stamps,
                });
            }
        }
    }

    /// The nameless pinned file numbered `number` now has `cluster`, `size`
    /// and `written` as its last-write stamp.
    fn renew(&mut self, number: u64, cluster: u32, size: u32, written: Stamp) {
        if let Some(PinnedFile {
            kept: KeptFile::Nameless(kept),
            ..
        }) = self.files.get_mut(number)
        {
            kept.cluster = cluster;
            kept.size = size;
            kept.stamps.written = written;
        }
    }
}

/// Where a file's first cluster, size and stamps are kept.
#[derive(Clone, Copy)]
enum Holder {
    /// Its short entry, at this device byte.
    Entry(u64),
    /// The pin of a file a removal left nameless, by its token's number.
    Pin(u64),
}

/// Where a node's stamps are kept: its short entry, or a pin's copy of the
/// one a removal deleted.
#[derive(Clone, Copy)]
enum StampsAt {
    /// The short entry at this device byte.
    Entry(u64),
    /// The nameless pinned file by its token's number.
    Nameless(u64),
    /// The removed directory a pin keeps, by its first cluster.
    Removed(u32),
}

/// A file's first cluster and size, and where they are kept.
#[derive(Clone, Copy)]
struct FileFacts {
    cluster: u32,
    size: u32,
    holder: Holder,
}

/// One linear FAT scan counting the free data clusters, run once at open;
/// the allocator and the chain-free path maintain the count from there,
/// so space accounting never re-scans the FAT.
fn count_free_clusters<B: Block>(
    dev: &mut ByteDevice<B>,
    layout: &Layout,
) -> Result<u64, DriverError> {
    let width = FAT_ENTRY as u64;
    let mut free_clusters = 0u64;
    let mut chunk = [0u8; MAX_BLOCK_LEN];
    let mut cluster = 2u32;
    while cluster <= layout.max_cluster {
        let remaining = (u64::from(layout.max_cluster) - u64::from(cluster) + 1) * width;
        let take = remaining.min(chunk.len() as u64);
        let take_usize = usize::try_from(take).map_err(|_| DriverError::DeviceFault)?;
        let (offset, _) = table::span(KIND, cluster);
        dev.read(layout.fat_start_byte + offset, &mut chunk[..take_usize])?;
        for entry in chunk[..take_usize].as_chunks::<FAT_ENTRY>().0 {
            let value = u32::from_le_bytes(*entry);
            if table::classify(KIND, value, layout.max_cluster) == Link::Free {
                free_clusters += 1;
            }
        }
        // `take` is a whole number of entries: `MAX_BLOCK_LEN` and
        // `remaining` are both multiples of an entry's width.
        cluster += u32::try_from(take / width).map_err(|_| DriverError::DeviceFault)?;
    }
    Ok(free_clusters)
}

/// Where an entry was written: its first slot, the slots it took, and the
/// device byte of its short entry.
#[derive(Clone, Copy)]
struct Placed {
    first: SlotPos,
    span: u64,
    short_offset: u64,
}

/// A run of free slots a new entry can take.
#[derive(Clone, Copy)]
struct FreeRun {
    start: SlotPos,
    /// The run begins at the directory's end-of-entries mark, which the
    /// writer moves past it.
    at_end: bool,
}

impl<B: Block> Fat32<B> {
    /// Validate the FAT32 boot sector on `block` and bring the volume
    /// online.
    ///
    /// FAT type is identified by the FAT32 boot-sector shape — a zero
    /// 16-bit FAT size and a zero root-entry count — rather than by
    /// re-deriving the cluster-count threshold: a FAT12/FAT16 volume
    /// has non-zero values in those fields and is rejected, so the
    /// distinction is exact for the volumes this driver accepts.
    ///
    /// # Errors
    ///
    /// * [`DriverError::Unsupported`] if the device's block size is not one
    ///   [`ByteDevice`] can stage.
    /// * [`DriverError::BadMagic`] if the boot sector is not a valid
    ///   FAT32 BPB (bad signature, non-power-of-two sector/cluster
    ///   size, missing FAT, or non-FAT32 markers).
    /// * Whatever the device reports for a read.
    ///
    /// # Capabilities
    ///
    /// Caller must already hold the driver's [`DriverHandle`].
    pub fn open(block: B) -> Result<Self, DriverError> {
        Self::mount(ByteDevice::new(block)?)
    }

    /// Read the volume on `dev`: the one reading of the on-disk layout, which
    /// [`Fat32::format`] ends in too. The volume's clusters are those its
    /// sectors hold, its table describes and the device reaches, so neither a
    /// table smaller than the volume nor a device smaller than it is followed
    /// past its end.
    fn mount(mut dev: ByteDevice<B>) -> Result<Self, DriverError> {
        let mut boot = [0u8; BOOT_SECTOR];
        dev.read(0, &mut boot)?;
        let bpb = fat32_bpb(&boot).ok_or(DriverError::BadMagic)?;
        let clusters = bpb.clusters_within(dev.capacity());
        let max_cluster = clusters + 1;
        if clusters == 0 || bpb.root_cluster > max_cluster {
            return Err(DriverError::BadMagic);
        }
        let layout = Layout {
            bytes_per_cluster: bpb.cluster_bytes(),
            fat_start_byte: bpb.fat_start(),
            data_start_byte: bpb.data_start(),
            root_cluster: bpb.root_cluster,
            fat_size_bytes: bpb.fat_bytes(),
            num_fats: u64::from(bpb.fats),
            max_cluster,
        };
        let free_clusters = count_free_clusters(&mut dev, &layout)?;

        Ok(Self {
            dev,
            layout,
            // The first allocatable cluster; the allocator advances it.
            next_free: 2,
            free_clusters,
            identity: fat32_identity(&bpb),
            seek: None,
            host: None,
            pins: Pins::default(),
            refusal: None,
        })
    }

    /// The volume's stable 16-byte identity, exactly as the volume probe
    /// derives it ([`tairix_fsprobe::fat32_identity`]).
    ///
    /// FAT32 has no UUID, so this is content-derived: a digest of the BPB
    /// volume serial, where the extended boot signature declares one, and
    /// the geometry formatting fixes, neither of which a write or a relabel
    /// changes. Stable across re-inserts, since both live on the medium;
    /// honestly weaker than a real UUID, as the format is.
    #[must_use]
    pub fn volume_identity(&self) -> [u8; 16] {
        self.identity
    }

    /// Refuse every change for `why`, whatever the volume would allow: a
    /// volume served read-only.
    pub(crate) fn refuse_writes(&mut self, why: WriteRefusal) {
        self.refusal.get_or_insert(why);
    }

    /// Refuse a change once the volume takes no writes.
    fn writable(&self) -> Result<(), DriverError> {
        match self.refusal {
            Some(_) => Err(DriverError::ReadOnly),
            None => Ok(()),
        }
    }

    /// Lay down a fresh, empty FAT32 volume on `block` and bring it
    /// online read-write.
    ///
    /// `serial` is the caller-minted BPB volume serial — the four bytes
    /// FAT32's content-derived identity is built on
    /// ([`Fat32::volume_identity`]). The caller mints it (`mkfs.fat`
    /// derives one from the clock; TAIRiX callers draw from their entropy
    /// or provenance source) because two volumes formatted with one
    /// serial are indistinguishable to the volume forest, so a duplicate
    /// can never mount while its twin is attached. Zero is refused: the
    /// all-zero serial is the "none recorded" value and would collide
    /// every freshly formatted volume with every other.
    ///
    /// The volume fills the device, laid out as [`tairix_fat::format::plan`]
    /// plans a FAT32 volume of its size: its cluster size by the thresholds
    /// `mkfs.fat` applies, its two FATs sized to map every cluster, its root
    /// directory empty in cluster 2. The whole system area is zeroed and the
    /// boot sector written last, so a format cut short leaves no volume a
    /// reader would take for one. The volume is then read back through the
    /// mount [`Fat32::open`] uses, the single reading of the on-disk layout —
    /// so a volume `format` produces is, by construction, one the driver
    /// mounts.
    ///
    /// # Errors
    ///
    /// * [`DriverError::Unsupported`] if the device's block size is not one
    ///   [`ByteDevice`] can stage or no FAT volume has.
    /// * [`DriverError::BadMagic`] if the bytes written somehow fail the
    ///   mount's validation.
    /// * [`DriverError::OutOfRange`] if `serial` is zero, if the device is
    ///   too small to host a valid FAT32 volume (fewer than the FAT32
    ///   minimum of 65525 data clusters), or if it has more sectors or
    ///   clusters than FAT32 counts.
    /// * Whatever the device reports for a transfer.
    ///
    /// # Capabilities
    ///
    /// Reached only through the driver's [`DriverHandle`].
    pub fn format(block: B, serial: u32) -> Result<Self, DriverError> {
        if serial == 0 {
            return Err(DriverError::OutOfRange);
        }
        let mut dev = ByteDevice::new(block)?;
        let medium = Medium {
            sectors: u32::try_from(dev.block_count()).map_err(|_| DriverError::OutOfRange)?,
            bytes_per_sector: u16::try_from(dev.block_size())
                .map_err(|_| DriverError::Unsupported)?,
            sectors_per_track: 0,
            heads: 0,
            hidden_sectors: 0,
            media: format::FIXED_MEDIA,
        };
        let shape = Shape {
            kind: Some(KIND),
            sectors_per_cluster: None,
            root_entries: 0,
            serial,
            label: None,
        };
        let bpb = format::plan(&medium, &shape).map_err(|refused| match refused {
            FormatError::SectorSize | FormatError::ClusterSize => DriverError::Unsupported,
            FormatError::TooSmall | FormatError::TooLarge => DriverError::OutOfRange,
        })?;
        let boot = format::boot_sector(&bpb, OEM, &[]);
        let sector_bytes = bpb.sector_bytes();
        dev.zero(0, format::system_sectors(&bpb) * sector_bytes)?;
        let mut sector = [0u8; MAX_BLOCK_LEN];
        let sector = sector
            .get_mut(..usize::from(bpb.bytes_per_sector))
            .ok_or(DriverError::Unsupported)?;
        for at in format::marked_sectors(&bpb) {
            format::system_sector(&bpb, &boot, at, sector);
            dev.write(at * sector_bytes, sector)?;
        }
        dev.write(0, &boot)?;
        Self::mount(dev)
    }

    /// Consume the driver, returning the underlying block device.
    pub fn into_block(self) -> B {
        self.dev.into_inner()
    }

    /// Byte offset of cluster `cluster`'s data on the device.
    fn cluster_byte(&self, cluster: u32) -> u64 {
        self.layout.data_start_byte + (u64::from(cluster) - 2) * self.layout.bytes_per_cluster
    }

    /// Whether `cluster` is one of the volume's data clusters.
    fn in_volume(&self, cluster: u32) -> bool {
        (2..=self.layout.max_cluster).contains(&cluster)
    }

    /// Follow one link of the FAT chain starting at `cluster`. A link to a
    /// free cluster is as broken as one outside the volume.
    fn next_cluster(&mut self, cluster: u32) -> Result<Link, DriverError> {
        Ok(
            match table::classify(KIND, self.fat_entry(cluster)?, self.layout.max_cluster) {
                Link::Free => Link::Bad,
                link => link,
            },
        )
    }

    /// Where data cluster `cluster`'s entry lies in the first FAT, and its
    /// width; a cluster outside the volume has none, so a damaged entry
    /// naming one is never followed out of the table.
    fn fat_span(&self, cluster: u32) -> Result<(u64, usize), DriverError> {
        if !self.in_volume(cluster) {
            return Err(DriverError::DeviceFault);
        }
        let (offset, width) = table::span(KIND, cluster);
        Ok((self.layout.fat_start_byte + offset, width))
    }

    /// Read `cluster`'s entry from the first FAT.
    fn fat_entry(&mut self, cluster: u32) -> Result<u32, DriverError> {
        let (offset, width) = self.fat_span(cluster)?;
        let mut raw = [0u8; FAT_ENTRY];
        let raw = &mut raw[..width];
        self.dev.read(offset, raw)?;
        table::read(KIND, cluster, raw).ok_or(DriverError::DeviceFault)
    }

    /// Write `value` into `cluster`'s entry in every FAT copy, keeping the
    /// bits each copy's entry reserves.
    fn set_fat(&mut self, cluster: u32, value: u32) -> Result<(), DriverError> {
        let (first, width) = self.fat_span(cluster)?;
        for fat in 0..self.layout.num_fats {
            let offset = first + fat * self.layout.fat_size_bytes;
            let mut raw = [0u8; FAT_ENTRY];
            let raw = &mut raw[..width];
            self.dev.read(offset, raw)?;
            if !table::write(KIND, cluster, raw, value) {
                return Err(DriverError::DeviceFault);
            }
            self.dev.write(offset, raw)?;
        }
        Ok(())
    }

    /// Zero the entire data region of `cluster`.
    fn zero_cluster(&mut self, cluster: u32) -> Result<(), DriverError> {
        self.dev
            .zero(self.cluster_byte(cluster), self.layout.bytes_per_cluster)
    }

    /// Allocate one free data cluster, mark it end-of-chain, optionally
    /// zero it, and return its number. Fails with [`DriverError::NoSpace`]
    /// when no free cluster remains (the volume is full).
    fn alloc_cluster(&mut self, zero: bool) -> Result<u32, DriverError> {
        let max = self.layout.max_cluster;
        // Clusters 2..=max are allocatable; scan from the hint, wrapping
        // once, so no free cluster is ever missed.
        let total = u64::from(max) - 1;
        let mut candidate = self.next_free.clamp(2, max);
        let mut scanned = 0u64;
        while scanned < total {
            if self.fat_entry(candidate)? == 0 {
                self.set_fat(candidate, table::end(KIND))?;
                if zero {
                    self.zero_cluster(candidate)?;
                }
                self.next_free = if candidate >= max { 2 } else { candidate + 1 };
                self.free_clusters = self.free_clusters.saturating_sub(1);
                return Ok(candidate);
            }
            candidate = if candidate >= max { 2 } else { candidate + 1 };
            scanned += 1;
        }
        Err(DriverError::NoSpace)
    }

    /// Free an entire cluster chain starting at `first`.
    fn free_chain(&mut self, first: u32) -> Result<(), DriverError> {
        self.seek = None;
        let mut cluster = first;
        let mut min_freed = u32::MAX;
        while self.in_volume(cluster) {
            let next = self.fat_entry(cluster)?;
            self.set_fat(cluster, 0)?;
            self.free_clusters += 1;
            min_freed = min_freed.min(cluster);
            match table::classify(KIND, next, self.layout.max_cluster) {
                Link::Next(n) => cluster = n,
                _ => break,
            }
        }
        // Reuse the freed clusters first: rewind the allocator hint to the
        // lowest cluster just released.
        if min_freed != u32::MAX {
            self.next_free = self.next_free.min(min_freed);
        }
        Ok(())
    }

    /// Return the next valid entry at or after `cursor`, advancing the
    /// cursor past it. `Ok(None)` marks end-of-directory.
    ///
    /// The entry's name is the reconstructed VFAT long name when a
    /// valid, checksum-matching long-name set precedes the short entry,
    /// and otherwise the 8.3 short name. Deleted entries, orphaned
    /// long-name fragments, volume labels, and the `.`/`..` self/parent
    /// links are skipped (the VFS resolves `.`/`..` itself).
    fn next_entry(&mut self, cursor: &mut DirCursor) -> Result<Option<ParsedEntry>, DriverError> {
        let mut long = LongName::new();
        let mut run_start: Option<SlotPos> = None;
        while let Some((at, raw)) = self.next_slot(cursor)? {
            let short = match dir::slot(&raw) {
                Slot::End => return Ok(None),
                Slot::Long => {
                    run_start.get_or_insert(at);
                    long.push(&raw);
                    continue;
                }
                Slot::Short => Some(ShortEntry::decode(&raw)).filter(|short| !short.is_dot()),
                Slot::Deleted | Slot::Label => None,
            };
            let Some(short) = short else {
                long.reset();
                run_start = None;
                continue;
            };
            let first = run_start.unwrap_or(at);
            let mut entry =
                ParsedEntry::of(&short, self.slot_byte(at), first, at.slot - first.slot + 1);
            // A long name past the VFS's name limit (one written by another
            // system in a script whose characters are three UTF-8 bytes) is
            // listed and reached by its short alias, so it neither hides the
            // file nor makes the directory unlistable.
            if let Some(long_len) = long.finish(&short.name, &mut entry.name[..FS_NAME_MAX]) {
                entry.name_len = long_len;
            }
            return Ok(Some(entry));
        }
        Ok(None)
    }

    /// The slot `walk` stands at, read through its held block, with its
    /// position; `None` once the directory's chain ends.
    fn next_slot(
        &mut self,
        walk: &mut DirCursor,
    ) -> Result<Option<(SlotPos, RawEntry)>, DriverError> {
        // A directory recording no data cluster holds no slots: data clusters
        // start at 2.
        if walk.pos.cluster < 2 {
            return Ok(None);
        }
        if walk.pos.cluster > self.layout.max_cluster {
            return Err(DriverError::DeviceFault);
        }
        if !self.settle(&mut walk.pos)? {
            return Ok(None);
        }
        if walk.pos.slot >= u64::from(MAX_ENTRIES) {
            return Err(DriverError::DeviceFault);
        }
        let at = walk.pos;
        let raw = self.walk_slot(walk, self.slot_byte(at))?;
        walk.pos.intra += ENTRY as u64;
        walk.pos.slot += 1;
        Ok(Some((at, raw)))
    }

    /// Bring `pos` onto a slot of its chain, following the chain once its
    /// cluster's slots are used up; `false` once the chain ends.
    fn settle(&mut self, pos: &mut SlotPos) -> Result<bool, DriverError> {
        if pos.intra < self.layout.bytes_per_cluster {
            return Ok(true);
        }
        match self.next_cluster(pos.cluster)? {
            Link::Next(next) => {
                pos.cluster = next;
                pos.intra = 0;
                Ok(true)
            }
            Link::End => Ok(false),
            Link::Free | Link::Bad => Err(DriverError::DeviceFault),
        }
    }

    /// Device byte offset of the slot at `pos`.
    fn slot_byte(&self, pos: SlotPos) -> u64 {
        self.cluster_byte(pos.cluster) + pos.intra
    }

    /// Write `raw` at `pos` and step past it, chaining a zeroed cluster on
    /// when the directory's chain ends first; the device byte written at.
    fn put_slot(&mut self, pos: &mut SlotPos, raw: &RawEntry) -> Result<u64, DriverError> {
        if !self.in_volume(pos.cluster) {
            return Err(DriverError::DeviceFault);
        }
        if !self.settle(pos)? {
            let fresh = self.alloc_cluster(true)?;
            self.set_fat(pos.cluster, fresh)?;
            pos.cluster = fresh;
            pos.intra = 0;
        }
        let at = self.slot_byte(*pos);
        self.dev.write(at, raw)?;
        pos.intra += ENTRY as u64;
        pos.slot += 1;
        Ok(at)
    }

    /// The 32-byte slot at device byte `byte`, read through the walk's held
    /// block. A slot never straddles two blocks: blocks are a multiple of 32
    /// bytes and clusters start on block boundaries.
    fn walk_slot(&mut self, cursor: &mut DirCursor, byte: u64) -> Result<[u8; ENTRY], DriverError> {
        let bs = self.dev.block_size() as usize;
        let lba = byte / bs as u64;
        if cursor.held != Some(lba) {
            cursor.held = None;
            self.dev.read(lba * bs as u64, &mut cursor.block[..bs])?;
            cursor.held = Some(lba);
        }
        let within = usize::try_from(byte % bs as u64).map_err(|_| DriverError::DeviceFault)?;
        let mut raw = [0u8; ENTRY];
        raw.copy_from_slice(
            cursor
                .block
                .get(within..within + ENTRY)
                .ok_or(DriverError::DeviceFault)?,
        );
        Ok(raw)
    }

    /// Position `walk`, standing at its directory's first slot, at logical
    /// slot `slot` by following the directory's own chain — from where the
    /// last batch stopped when that is on the way — answering `false` when
    /// the chain ends first.
    fn seek_slot(&mut self, walk: &mut DirCursor, slot: u64) -> Result<bool, DriverError> {
        let per_cluster = self.slots_per_cluster();
        let dir = walk.pos.cluster;
        let target = slot / per_cluster;
        let (mut index, mut cluster) = match self.seek {
            Some(hint) if hint.dir == dir && hint.index <= target => (hint.index, hint.cluster),
            _ => (0, dir),
        };
        while index < target {
            match self.next_cluster(cluster)? {
                Link::Next(next) => cluster = next,
                Link::End => return Ok(false),
                Link::Free | Link::Bad => return Err(DriverError::DeviceFault),
            }
            index += 1;
        }
        // A listing's batches start ever further on, so the next one seeks
        // on from here.
        self.seek = Some(SeekHint {
            dir,
            index,
            cluster,
        });
        walk.pos.cluster = cluster;
        walk.pos.intra = (slot % per_cluster) * ENTRY as u64;
        walk.pos.slot = slot;
        Ok(true)
    }

    /// Step `walk`, standing on the slot a batch's last entry took, onto the
    /// next, past any entry whose long name now runs through that slot: it
    /// was written after the listing passed, so it was not present throughout
    /// and is skipped rather than listed by its 8.3 alias. `false` once the
    /// directory ends.
    fn step_past_resume(&mut self, walk: &mut DirCursor) -> Result<bool, DriverError> {
        let Some((_, before)) = self.next_slot(walk)? else {
            return Ok(false);
        };
        if dir::slot(&before) != Slot::Long {
            return Ok(true);
        }
        let checksum = before[dir::at::CHECKSUM];
        loop {
            let mark = walk.pos;
            let Some((_, raw)) = self.next_slot(walk)? else {
                return Ok(false);
            };
            // Anything but the run's own short entry — the end, a free slot,
            // an unrelated entry — is listed as found.
            let own = match dir::slot(&raw) {
                Slot::Long => continue,
                Slot::Short | Slot::Label => {
                    dir::checksum(&ShortEntry::decode(&raw).name) == checksum
                }
                Slot::End | Slot::Deleted => false,
            };
            if !own {
                walk.pos = mark;
            }
            return Ok(true);
        }
    }

    /// The bytes a node's data occupies: its chain, for a directory (whose
    /// entry records no size), and the clusters its size needs, for a file —
    /// walking a large file's chain per listed entry
    /// would cost the listing the file's size.
    fn allocation(&mut self, cluster: u32, is_dir: bool, size: u32) -> Result<u64, DriverError> {
        let per_cluster = self.layout.bytes_per_cluster;
        if !self.in_volume(cluster) {
            Ok(0)
        } else if is_dir {
            Ok(self.chain_len(cluster)?.0 * per_cluster)
        } else {
            Ok(u64::from(size).div_ceil(per_cluster) * per_cluster)
        }
    }

    /// The id of a decoded directory entry.
    fn entry_node(entry: &ParsedEntry) -> NodeId {
        if entry.is_dir() {
            dir_node(entry.cluster)
        } else {
            file_node(entry.short_offset)
        }
    }

    /// The structural metadata of a decoded directory entry, its stamps
    /// among them.
    fn entry_info(&mut self, entry: &ParsedEntry) -> Result<NodeInfo, DriverError> {
        Ok(NodeInfo {
            kind: if entry.is_dir() {
                NodeKind::Directory
            } else {
                NodeKind::RegularFile
            },
            nlink: NodeInfo::SINGLE_NAME,
            ino: Self::entry_node(entry).raw(),
            size: if entry.is_dir() {
                0
            } else {
                u64::from(entry.size)
            },
            allocated: self.allocation(entry.cluster, entry.is_dir(), entry.size)?,
            times: node_times(&entry.stamps),
            content_gen: NodeInfo::NO_CONTENT_GEN,
        })
    }
}

/// One created directory entry pending write (a long-name fragment or
/// the 8.3 short entry).
type RawEntry = [u8; ENTRY];

impl<B: Block> Fat32<B> {
    /// Number of 32-byte directory slots in one cluster.
    fn slots_per_cluster(&self) -> u64 {
        self.layout.bytes_per_cluster / ENTRY as u64
    }

    /// Look up child `name` in directory `dir_cluster`, returning its
    /// parsed entry (with on-disk slot/offset metadata) if present.
    fn find_child(
        &mut self,
        dir_cluster: u32,
        name: &[u8],
    ) -> Result<Option<ParsedEntry>, DriverError> {
        let mut cursor = DirCursor::at(dir_cluster);
        while let Some(entry) = self.next_entry(&mut cursor)? {
            if entry.name[..entry.name_len].eq_ignore_ascii_case(name) {
                return Ok(Some(entry));
            }
        }
        Ok(None)
    }

    /// The live file whose short entry lies at device byte `short_offset`.
    ///
    /// # Errors
    ///
    /// * [`DriverError::NotFound`] unless the place is a slot of the data
    ///   region holding a file's short entry: an id outlives the entry a
    ///   removal or a rename frees, and the slot may since hold anything.
    /// * Whatever the device reports for a read.
    fn file_at(&mut self, short_offset: u64) -> Result<ParsedEntry, DriverError> {
        let data_end = self.cluster_byte(self.layout.max_cluster) + self.layout.bytes_per_cluster;
        if short_offset < self.layout.data_start_byte || short_offset > data_end - ENTRY as u64 {
            return Err(DriverError::NotFound);
        }
        let mut raw = [0u8; ENTRY];
        self.dev.read(short_offset, &mut raw)?;
        let short = ShortEntry::decode(&raw);
        if dir::slot(&raw) != Slot::Short || short.is_dot() || short.is_directory() {
            return Err(DriverError::NotFound);
        }
        Ok(ParsedEntry::of(&short, short_offset, SlotPos::at(0), 0))
    }

    /// The file `file` names: a live one, or a pinned one a removal left
    /// nameless.
    ///
    /// # Errors
    ///
    /// * [`DriverError::Unsupported`] if `file` names a directory.
    /// * [`DriverError::NotFound`] if it names no live file and no held one.
    /// * [`DriverError::DeviceFault`] if its first cluster is none of the
    ///   volume's, which no access may follow out of the volume.
    fn file(&mut self, file: NodeId) -> Result<FileFacts, DriverError> {
        let facts = match named(file).ok_or(DriverError::NotFound)? {
            Named::Dir(_) => return Err(DriverError::Unsupported),
            Named::File(short_offset) => self.live_file(short_offset)?,
            Named::Pinned(number) => match self.pins.kept(number)? {
                KeptFile::Entry(short_offset) => self.live_file(short_offset)?,
                KeptFile::Nameless(kept) => FileFacts {
                    cluster: kept.cluster,
                    size: kept.size,
                    holder: Holder::Pin(number),
                },
            },
        };
        if facts.cluster != 0 && !self.in_volume(facts.cluster) {
            return Err(DriverError::DeviceFault);
        }
        Ok(facts)
    }

    /// The facts of the live file whose short entry lies at `short_offset`.
    fn live_file(&mut self, short_offset: u64) -> Result<FileFacts, DriverError> {
        let entry = self.file_at(short_offset)?;
        Ok(FileFacts {
            cluster: entry.cluster,
            size: entry.size,
            holder: Holder::Entry(short_offset),
        })
    }

    /// Record `cluster` and `size` as the file's where `holder` keeps them,
    /// and `written` as its last-write stamp.
    fn set_extent(
        &mut self,
        holder: Holder,
        cluster: u32,
        size: u32,
        written: Stamp,
    ) -> Result<(), DriverError> {
        match holder {
            Holder::Entry(short_offset) => {
                self.set_entry_meta(short_offset, cluster, size, written)
            }
            Holder::Pin(number) => {
                self.pins.renew(number, cluster, size, written);
                Ok(())
            }
        }
    }

    /// Refuse the directory whose first cluster is `cluster` once that is no
    /// longer allocated, as a removed directory's is: its id outlives it.
    fn live_dir(&mut self, cluster: u32) -> Result<(), DriverError> {
        if self.in_volume(cluster) && self.fat_entry(cluster)? != 0 {
            Ok(())
        } else {
            Err(DriverError::NotFound)
        }
    }

    /// The first cluster of the directory `dir` names, which takes entries.
    ///
    /// # Errors
    ///
    /// * [`DriverError::Unsupported`] for an id naming anything but a
    ///   directory.
    /// * [`DriverError::NotFound`] for a removed directory, its cluster freed
    ///   or kept by a pin.
    fn entries_dir(&mut self, dir: NodeId) -> Result<u32, DriverError> {
        let cluster = cluster_of(dir)?;
        if self.pins.removed(cluster).is_some() {
            return Err(DriverError::NotFound);
        }
        self.live_dir(cluster)?;
        Ok(cluster)
    }

    /// The kind of the node `node` names, live or held.
    fn node_kind(&mut self, node: NodeId) -> Result<NodeKind, DriverError> {
        match named(node).ok_or(DriverError::NotFound)? {
            Named::Dir(cluster) => {
                if self.pins.removed(cluster).is_none() {
                    self.live_dir(cluster)?;
                }
                Ok(NodeKind::Directory)
            }
            Named::File(_) | Named::Pinned(_) => self.file(node).map(|_| NodeKind::RegularFile),
        }
    }

    /// The structural metadata of the directory at `cluster`: one a pin keeps
    /// after a removal has no name, and the stamps its entry held.
    fn dir_info(&mut self, cluster: u32) -> Result<NodeInfo, DriverError> {
        let (nlink, times) = if let Some(times) = self.pins.removed(cluster) {
            (0, times)
        } else {
            self.live_dir(cluster)?;
            (NodeInfo::SINGLE_NAME, self.dir_times(cluster)?)
        };
        Ok(NodeInfo {
            kind: NodeKind::Directory,
            nlink,
            ino: dir_node(cluster).raw(),
            size: 0,
            allocated: self.allocation(cluster, true, 0)?,
            times,
            content_gen: NodeInfo::NO_CONTENT_GEN,
        })
    }

    /// The structural metadata of a pinned file a removal left nameless.
    fn nameless_info(&mut self, kept: &Nameless) -> Result<NodeInfo, DriverError> {
        Ok(NodeInfo {
            kind: NodeKind::RegularFile,
            nlink: 0,
            ino: file_node(kept.last).raw(),
            size: u64::from(kept.size),
            allocated: self.allocation(kept.cluster, false, kept.size)?,
            times: node_times(&kept.stamps),
            content_gen: NodeInfo::NO_CONTENT_GEN,
        })
    }

    /// Patch the first-cluster, size and last-write fields of the short
    /// entry at `short_offset`. They close the entry together, so a content
    /// change is one write.
    fn set_entry_meta(
        &mut self,
        short_offset: u64,
        cluster: u32,
        size: u32,
        written: Stamp,
    ) -> Result<(), DriverError> {
        let fields = ShortEntry {
            cluster,
            size,
            stamps: Stamps {
                written,
                ..Stamps::NONE
            },
            ..ShortEntry::default()
        }
        .encode();
        let from = dir::at::CLUSTER_HIGH;
        self.dev.write(short_offset + from as u64, &fields[from..])
    }

    /// The instant an operation stamps, or `None` while the host has no time
    /// to give.
    fn now(&self) -> Option<Time64> {
        self.host.and_then(VolumeHost::wall_time)
    }

    /// The last-write stamp a content change records now.
    fn written_now(&self) -> Stamp {
        Stamps::at(self.now()).written
    }

    /// Choose a new entry's alias, the lowest `~N` tail no live alias holds,
    /// and the first free run of `count` slots for it. One walk of the
    /// directory decides both unless its first window of tails is all held.
    fn place_entry(
        &mut self,
        dir_cluster: u32,
        stem: &AliasStem,
        count: u64,
    ) -> Result<([u8; 11], FreeRun), DriverError> {
        let mut run: Option<FreeRun> = None;
        // A tail for every slot a directory holds: a directory with room for
        // the entry always leaves one of them free.
        let mut first_tail = 1;
        while first_tail <= MAX_ENTRIES {
            let mut held = TailWindow::starting_at(first_tail);
            let mut walk = DirCursor::at(dir_cluster);
            let mut free: Option<(SlotPos, u64)> = None;
            let end = loop {
                let Some((at, raw)) = self.next_slot(&mut walk)? else {
                    break walk.pos;
                };
                match dir::slot(&raw) {
                    Slot::End => break at,
                    Slot::Deleted => {
                        let (start, len) = free.get_or_insert((at, 0));
                        *len += 1;
                        if *len == count && run.is_none() {
                            run = Some(FreeRun {
                                start: *start,
                                at_end: false,
                            });
                        }
                        continue;
                    }
                    Slot::Short => {
                        if let Some(tail) = stem.tail_of(&ShortEntry::decode(&raw).name) {
                            held.hold(tail);
                        }
                    }
                    Slot::Long | Slot::Label => {}
                }
                free = None;
            };
            let run = *run.get_or_insert(FreeRun {
                start: free.map_or(end, |(start, _)| start),
                at_end: true,
            });
            if let Some(tail) = held.lowest_free() {
                return Ok((stem.alias(tail).ok_or(DriverError::NoSpace)?, run));
            }
            first_tail += TAIL_WINDOW;
        }
        Err(DriverError::NoSpace)
    }

    /// Initialise a freshly allocated directory cluster with its `.` and
    /// `..` links, each stamped as the directory is. `parent_cluster` is `0`
    /// when the parent is the root.
    fn init_dir_cluster(
        &mut self,
        child_cluster: u32,
        parent_cluster: u32,
        stamps: Stamps,
    ) -> Result<(), DriverError> {
        let link = |name, cluster| {
            ShortEntry {
                name,
                attributes: attr::DIRECTORY,
                stamps,
                cluster,
                ..ShortEntry::default()
            }
            .encode()
        };
        let mut links = [0u8; 2 * ENTRY];
        links[..ENTRY].copy_from_slice(&link(DOT, child_cluster));
        links[ENTRY..].copy_from_slice(&link(DOTDOT, self.parent_link(parent_cluster)));
        self.dev.write(self.cluster_byte(child_cluster), &links)
    }

    /// What a `..` entry records for the parent at `cluster`: `0` for the
    /// root.
    fn parent_link(&self, cluster: u32) -> u32 {
        if cluster == self.layout.root_cluster {
            0
        } else {
            cluster
        }
    }

    /// Length (in clusters) and last cluster of the chain at `first`.
    fn chain_len(&mut self, first: u32) -> Result<(u64, u32), DriverError> {
        // No chain is longer than the volume has clusters; a longer walk is
        // going round a cycle, and fails closed rather than looping.
        let most = u64::from(self.layout.max_cluster) - 1;
        let mut cluster = first;
        let mut len = 1u64;
        loop {
            match self.next_cluster(cluster)? {
                Link::Next(next) if len < most => {
                    cluster = next;
                    len += 1;
                }
                Link::End => return Ok((len, cluster)),
                Link::Next(_) | Link::Free | Link::Bad => return Err(DriverError::DeviceFault),
            }
        }
    }

    /// Ensure the file whose first cluster is `first` (0 if empty) has at
    /// least `needed` clusters, allocating zeroed clusters as required.
    /// Returns the (possibly newly allocated) first cluster.
    fn ensure_chain(&mut self, first: u32, needed: u64) -> Result<u32, DriverError> {
        if needed == 0 {
            return Ok(first);
        }
        let (head, mut have, mut last) = if first < 2 {
            let fresh = self.alloc_cluster(true)?;
            (fresh, 1u64, fresh)
        } else {
            let (len, last) = self.chain_len(first)?;
            (first, len, last)
        };
        while have < needed {
            let fresh = self.alloc_cluster(true)?;
            self.set_fat(last, fresh)?;
            last = fresh;
            have += 1;
        }
        Ok(head)
    }

    /// Write `buf` into the data chain `first` starting at byte
    /// `byte_offset`. The chain must already be long enough.
    fn write_data(&mut self, first: u32, byte_offset: u64, buf: &[u8]) -> Result<(), DriverError> {
        if buf.is_empty() {
            return Ok(());
        }
        let bpc = self.layout.bytes_per_cluster;
        let mut cluster = first;
        let mut skip = byte_offset / bpc;
        while skip > 0 {
            match self.next_cluster(cluster)? {
                Link::Next(next) => cluster = next,
                _ => return Err(DriverError::DeviceFault),
            }
            skip -= 1;
        }
        let mut intra = byte_offset % bpc;
        let mut done = 0usize;
        while done < buf.len() {
            if cluster < 2 {
                return Err(DriverError::DeviceFault);
            }
            let room = usize::try_from(bpc - intra).map_err(|_| DriverError::LengthOutOfRange)?;
            let take = core::cmp::min(room, buf.len() - done);
            let at = self.cluster_byte(cluster) + intra;
            self.dev.write(at, &buf[done..done + take])?;
            done += take;
            intra = 0;
            if done < buf.len() {
                match self.next_cluster(cluster)? {
                    Link::Next(next) => cluster = next,
                    _ => return Err(DriverError::DeviceFault),
                }
            }
        }
        Ok(())
    }

    /// Zero `len` bytes of the data chain `first` starting at `start`.
    fn zero_range(&mut self, first: u32, start: u64, len: u64) -> Result<(), DriverError> {
        let zeros = [0u8; MAX_BLOCK_LEN];
        let mut remaining = len;
        let mut at = start;
        while remaining > 0 {
            let chunk = remaining.min(zeros.len() as u64);
            let chunk_usize = usize::try_from(chunk).map_err(|_| DriverError::DeviceFault)?;
            self.write_data(first, at, &zeros[..chunk_usize])?;
            at += chunk;
            remaining -= chunk;
        }
        Ok(())
    }

    /// Shared implementation of [`FilesystemWrite::create`].
    fn create_child(
        &mut self,
        dir: NodeId,
        name: &[u8],
        kind: NodeKind,
    ) -> Result<NodeId, DriverError> {
        let dir_cluster = self.entries_dir(dir)?;
        if name.is_empty() || name.len() > LONG_NAME_BYTES {
            return Err(DriverError::LengthOutOfRange);
        }
        if self.find_child(dir_cluster, name)?.is_some() {
            return Err(DriverError::AlreadyExists);
        }

        let is_dir = matches!(kind, NodeKind::Directory);
        let stamps = Stamps::at(self.now());
        let child_cluster = if is_dir {
            let fresh = self.alloc_cluster(true)?;
            self.init_dir_cluster(fresh, dir_cluster, stamps)?;
            fresh
        } else {
            0
        };
        let attributes = if is_dir {
            attr::DIRECTORY
        } else {
            attr::ARCHIVE
        };
        match self.write_dir_entry(dir_cluster, name, attributes, child_cluster, 0, stamps) {
            Ok(_) if is_dir => Ok(dir_node(child_cluster)),
            Ok(placed) => Ok(file_node(placed.short_offset)),
            Err(e) => {
                // Reclaim the directory cluster if naming the entry failed, so
                // a rejected create leaves no orphaned chain behind.
                if is_dir && child_cluster >= 2 {
                    let _ = self.free_chain(child_cluster);
                }
                Err(e)
            }
        }
    }

    /// Encode `name` as a run of directory slots — the long-name fragments
    /// followed by the 8.3 short entry pointing at `cluster`/`size` with
    /// `attributes` — in a free run of `dir_cluster`, answering where it
    /// lies. Shared by [`Self::create_child`] and [`Self::rename_child`] so
    /// the long-name encoding lives in one place.
    fn write_dir_entry(
        &mut self,
        dir_cluster: u32,
        name: &[u8],
        attributes: u8,
        cluster: u32,
        size: u32,
        stamps: Stamps,
    ) -> Result<Placed, DriverError> {
        let mut units = [0u16; LONG_NAME_UNITS];
        let count = dir::encode_utf16(name, &mut units)
            .filter(|&count| count > 0)
            .ok_or(DriverError::LengthOutOfRange)?;
        let units = &units[..count];
        let slots = count.div_ceil(LONG_UNITS) as u64 + 1;
        let (short, run) = self.place_entry(dir_cluster, &AliasStem::of(name), slots)?;
        if run.start.slot + slots > u64::from(MAX_ENTRIES) {
            return Err(DriverError::NoSpace);
        }
        self.mark_end_after(run, slots)?;

        let mut pos = run.start;
        for long in dir::long_entries(units, dir::checksum(&short)) {
            self.put_slot(&mut pos, &long)?;
        }
        let entry = ShortEntry {
            name: short,
            attributes,
            case: 0,
            stamps,
            cluster,
            size,
        };
        let short_offset = self.put_slot(&mut pos, &entry.encode())?;
        Ok(Placed {
            first: run.start,
            span: slots,
            short_offset,
        })
    }

    /// Move the end-of-entries mark from `run`'s first slot, where an entry
    /// of `slots` slots is about to go, to the slot after it. The mark is
    /// found before anything is written, so a broken chain fails the change
    /// cleanly, and written first, so the old mark ends the directory until
    /// the entry is whole. Past the chain's end no mark is needed: the
    /// clusters an entry grows into are zeroed.
    fn mark_end_after(&mut self, run: FreeRun, slots: u64) -> Result<(), DriverError> {
        if !run.at_end {
            return Ok(());
        }
        let mut after = run.start;
        let mut in_chain = true;
        for _ in 0..slots {
            in_chain = self.settle(&mut after)?;
            if !in_chain {
                break;
            }
            after.intra += ENTRY as u64;
            after.slot += 1;
        }
        if in_chain && after.slot < u64::from(MAX_ENTRIES) && self.settle(&mut after)? {
            self.dev.write(self.slot_byte(after), &[0u8; ENTRY])?;
        }
        Ok(())
    }

    /// Shared implementation of [`FilesystemWrite::write_at`].
    fn write_file(&mut self, file: NodeId, offset: u64, data: &[u8]) -> Result<usize, DriverError> {
        let facts = self.file(file)?;
        if data.is_empty() {
            return Ok(0);
        }
        let end = offset
            .checked_add(data.len() as u64)
            .ok_or(DriverError::LengthOutOfRange)?;
        let old_size = u64::from(facts.size);
        let bpc = self.layout.bytes_per_cluster;
        let needed = end.div_ceil(bpc);
        let first = self.ensure_chain(facts.cluster, needed)?;
        if offset > old_size {
            self.zero_range(first, old_size, offset - old_size)?;
        }
        self.write_data(first, offset, data)?;
        let new_size =
            u32::try_from(old_size.max(end)).map_err(|_| DriverError::LengthOutOfRange)?;
        self.set_extent(facts.holder, first, new_size, self.written_now())?;
        Ok(data.len())
    }

    /// Shared implementation of [`FilesystemWrite::truncate`].
    fn truncate_file(&mut self, file: NodeId, size: u64) -> Result<(), DriverError> {
        let facts = self.file(file)?;
        let old_size = u64::from(facts.size);
        if size == old_size {
            // A truncation that keeps the length is still the open that
            // truncates a file, and is stamped as one.
            return self.set_extent(facts.holder, facts.cluster, facts.size, self.written_now());
        }
        let bpc = self.layout.bytes_per_cluster;
        let new_size = u32::try_from(size).map_err(|_| DriverError::LengthOutOfRange)?;

        if size < old_size {
            let needed = size.div_ceil(bpc);
            if needed == 0 {
                if facts.cluster >= 2 {
                    self.free_chain(facts.cluster)?;
                }
                return self.set_extent(facts.holder, 0, 0, self.written_now());
            }
            // Walk to the new last cluster, sever and free the remainder.
            let mut cluster = facts.cluster;
            for _ in 0..needed - 1 {
                match self.next_cluster(cluster)? {
                    Link::Next(next) => cluster = next,
                    _ => return Err(DriverError::DeviceFault),
                }
            }
            if let Link::Next(tail) = self.next_cluster(cluster)? {
                self.free_chain(tail)?;
            }
            self.set_fat(cluster, table::end(KIND))?;
            self.set_extent(facts.holder, facts.cluster, new_size, self.written_now())?;
        } else {
            let needed = size.div_ceil(bpc);
            let first = self.ensure_chain(facts.cluster, needed)?;
            self.zero_range(first, old_size, size - old_size)?;
            self.set_extent(facts.holder, first, new_size, self.written_now())?;
        }
        Ok(())
    }

    /// Shared implementation of [`FilesystemWrite::remove`].
    fn remove_child(&mut self, dir: NodeId, name: &[u8]) -> Result<(), DriverError> {
        let dir_cluster = self.entries_dir(dir)?;
        let entry = self
            .find_child(dir_cluster, name)?
            .ok_or(DriverError::NotFound)?;
        if entry.is_dir() {
            let mut child = DirCursor::at(entry.cluster);
            if self.next_entry(&mut child)?.is_some() {
                return Err(DriverError::DirectoryNotEmpty);
            }
        }
        self.unlink(&entry)
    }

    /// Delete `entry` and free what it names, unless a pin holds that: then
    /// it is kept, nameless, until the last unpin frees it.
    fn unlink(&mut self, entry: &ParsedEntry) -> Result<(), DriverError> {
        let held = self.pins.holds(entry);
        if !held && entry.cluster >= 2 {
            self.free_chain(entry.cluster)?;
        }
        self.delete_entry_slots(entry.first, entry.slot_span)?;
        if held {
            self.pins.unnamed(entry);
        }
        Ok(())
    }

    /// Mark the `span` slots from `first` deleted: one logical entry's
    /// long-name fragments and its short entry.
    fn delete_entry_slots(&mut self, mut pos: SlotPos, span: u64) -> Result<(), DriverError> {
        for _ in 0..span {
            if !self.settle(&mut pos)? {
                return Err(DriverError::DeviceFault);
            }
            self.dev.write(self.slot_byte(pos), &[DELETED])?;
            pos.intra += ENTRY as u64;
            pos.slot += 1;
        }
        Ok(())
    }

    /// The cluster of `dir_cluster`'s parent, read from its `..` entry
    /// (which stores `0` for the root).
    fn dir_parent_cluster(&mut self, dir_cluster: u32) -> Result<u32, DriverError> {
        self.dotdot(dir_cluster)?.ok_or(DriverError::DeviceFault)
    }

    /// The parent `dir_cluster`'s `..` entry names, or `None` where that
    /// entry is no well-formed `..`.
    fn dotdot(&mut self, dir_cluster: u32) -> Result<Option<u32>, DriverError> {
        let link = self.dotdot_entry(dir_cluster)?;
        if link.name != DOTDOT || !link.is_directory() {
            return Ok(None);
        }
        Ok(match link.cluster {
            0 => Some(self.layout.root_cluster),
            cluster if self.in_volume(cluster) => Some(cluster),
            _ => None,
        })
    }

    /// Where the `..` entry of the directory at `dir_cluster` lies: its
    /// second slot.
    fn dotdot_byte(&self, dir_cluster: u32) -> u64 {
        self.cluster_byte(dir_cluster) + ENTRY as u64
    }

    /// The entry in the `..` slot of the directory at `dir_cluster`.
    fn dotdot_entry(&mut self, dir_cluster: u32) -> Result<ShortEntry, DriverError> {
        let mut raw = [0u8; ENTRY];
        self.dev.read(self.dotdot_byte(dir_cluster), &mut raw)?;
        Ok(ShortEntry::decode(&raw))
    }

    /// The stamps of directory `cluster`, which its entry in its parent
    /// holds, as a listing of the parent reports them. The root, which no
    /// entry names, has none; nor has a directory whose `..` is damaged or
    /// names a parent not holding it, since a stat degrades rather than fails
    /// over stamps it cannot reach.
    fn dir_times(&mut self, cluster: u32) -> Result<NodeTimes, DriverError> {
        if cluster == self.layout.root_cluster {
            return Ok(NodeTimes::default());
        }
        let Some(parent) = self.dotdot(cluster)? else {
            return Ok(NodeTimes::default());
        };
        let mut walk = DirCursor::at(parent);
        while let Some((_, raw)) = self.next_slot(&mut walk)? {
            match dir::slot(&raw) {
                Slot::End => break,
                Slot::Short => {
                    let entry = ShortEntry::decode(&raw);
                    if entry.is_directory() && !entry.is_dot() && entry.cluster == cluster {
                        return Ok(node_times(&entry.stamps));
                    }
                }
                Slot::Deleted | Slot::Long | Slot::Label => {}
            }
        }
        Ok(NodeTimes::default())
    }

    /// Where `node`'s stamps are kept, with what they are now; `None` for the
    /// root, which no entry names.
    fn stamps_of(&mut self, node: NodeId) -> Result<Option<(StampsAt, Stamps)>, DriverError> {
        let file_entry = |fs: &mut Self, short_offset| {
            fs.file_at(short_offset)
                .map(|entry| Some((StampsAt::Entry(short_offset), entry.stamps)))
        };
        match named(node).ok_or(DriverError::NotFound)? {
            Named::File(short_offset) => file_entry(self, short_offset),
            Named::Pinned(number) => match self.pins.kept(number)? {
                KeptFile::Entry(short_offset) => file_entry(self, short_offset),
                KeptFile::Nameless(kept) => Ok(Some((StampsAt::Nameless(number), kept.stamps))),
            },
            Named::Dir(cluster) if cluster == self.layout.root_cluster => Ok(None),
            Named::Dir(cluster) => {
                if let Some(times) = self.pins.removed(cluster) {
                    let stamps = Stamps::NONE.changed(
                        StampChange::At(times.accessed),
                        StampChange::At(times.modified),
                        None,
                    );
                    return Ok(Some((StampsAt::Removed(cluster), stamps)));
                }
                self.live_dir(cluster)?;
                let parent = self.dotdot(cluster)?.ok_or(DriverError::DeviceFault)?;
                let mut walk = DirCursor::at(parent);
                while let Some((pos, raw)) = self.next_slot(&mut walk)? {
                    match dir::slot(&raw) {
                        Slot::End => break,
                        Slot::Short => {
                            let entry = ShortEntry::decode(&raw);
                            if entry.is_directory() && !entry.is_dot() && entry.cluster == cluster {
                                return Ok(Some((
                                    StampsAt::Entry(self.slot_byte(pos)),
                                    entry.stamps,
                                )));
                            }
                        }
                        Slot::Deleted | Slot::Long | Slot::Label => {}
                    }
                }
                Err(DriverError::DeviceFault)
            }
        }
    }

    /// [`FilesystemWrite::set_times`]: the last-read date and last-write
    /// stamp, as [`tairix_fat::stamp::ACCESSED`] and
    /// [`tairix_fat::stamp::WRITTEN`] hold them. FAT keeps no change time, and
    /// the root no stamps at all.
    fn set_entry_times(
        &mut self,
        node: NodeId,
        change: TimesChange,
    ) -> Result<TimesKept, DriverError> {
        let Some((at, stamps)) = self.stamps_of(node)? else {
            let accessed = unkept_stamp(change.accessed, change.nearest)?;
            return Ok(accessed.and(unkept_stamp(change.modified, change.nearest)?));
        };
        let (accessed, a) = stamp::ACCESSED.fit_change(change.accessed, change.nearest)?;
        let (modified, m) = stamp::WRITTEN.fit_change(change.modified, change.nearest)?;
        if change.is_empty() {
            return Ok(TimesKept::Exactly);
        }
        self.writable()?;
        let restamped = stamps.changed(accessed, modified, self.now());
        match at {
            StampsAt::Entry(short_offset) => {
                let fields = ShortEntry {
                    stamps: restamped,
                    ..ShortEntry::default()
                }
                .encode();
                let from = dir::at::ACCESSED;
                self.dev.write(
                    short_offset + from as u64,
                    &fields[from..dir::at::CLUSTER_HIGH],
                )?;
                let from = dir::at::WRITTEN_TIME;
                self.dev.write(
                    short_offset + from as u64,
                    &fields[from..dir::at::CLUSTER_LOW],
                )?;
            }
            StampsAt::Nameless(_) | StampsAt::Removed(_) => self.pins.restamp(at, restamped),
        }
        Ok(a.and(m))
    }

    /// Whether directory `candidate` is `ancestor` itself or lives anywhere
    /// beneath it, walking `..` links up to the root. Refuses moving a
    /// directory into its own subtree (which would detach the cycle).
    fn is_subdir_of(&mut self, mut candidate: u32, ancestor: u32) -> Result<bool, DriverError> {
        if !self.in_volume(candidate) {
            return Err(DriverError::DeviceFault);
        }
        // Brent's cycle detection: a `..` chain that comes back to a
        // directory it has passed never reaches the root.
        let mut mark = candidate;
        let mut lap = 1u32;
        let mut steps = 0u32;
        loop {
            if candidate == ancestor {
                return Ok(true);
            }
            if candidate == self.layout.root_cluster {
                return Ok(false);
            }
            candidate = self.dir_parent_cluster(candidate)?;
            if candidate == mark {
                return Err(DriverError::DeviceFault);
            }
            steps += 1;
            if steps == lap {
                mark = candidate;
                lap = lap.saturating_mul(2);
                steps = 0;
            }
        }
    }

    /// Shared implementation of [`FilesystemWrite::rename`].
    ///
    /// FAT has no inode and no journal: the move re-encodes the source's
    /// long-name + short entry under the destination name (preserving its
    /// first cluster, size, attribute byte, and stamps verbatim), then deletes
    /// the source entry, so the file's data clusters are never touched. A
    /// destination matching the source's own name is that entry re-spelled.
    /// Across directories a moved directory's `..` is repointed at the new
    /// parent, and a directory with no well-formed `..` to repoint is
    /// refused before anything is written. A moved file takes the id of its
    /// new entry's place, and a pin
    /// of it follows it there; a directory keeps its first cluster, and so its
    /// id. A replaced destination a pin holds is kept, nameless, as a removal
    /// keeps it.
    /// Replacement of an existing destination is therefore best-effort
    /// rather than atomic, matching the non-transactional create/remove
    /// paths the on-disk format allows.
    fn rename_child(
        &mut self,
        src_dir: NodeId,
        src_name: &[u8],
        dst_dir: NodeId,
        dst_name: &[u8],
    ) -> Result<(), DriverError> {
        let src_cluster = self.entries_dir(src_dir)?;
        let dst_cluster = self.entries_dir(dst_dir)?;
        if dst_name.is_empty() || dst_name.len() > LONG_NAME_BYTES {
            return Err(DriverError::LengthOutOfRange);
        }
        if dst_name == b"." || dst_name == b".." {
            return Err(DriverError::Unsupported);
        }
        let src_entry = self
            .find_child(src_cluster, src_name)?
            .ok_or(DriverError::NotFound)?;
        let moving_dir = src_entry.is_dir();
        let reparenting = moving_dir && src_cluster != dst_cluster;

        // A destination resolving to the source's own entry is that entry
        // re-spelled, since names match ignoring case: it takes the spelling
        // asked for, and is never the replaced entry, whose chain the move is.
        let dst_existing = self
            .find_child(dst_cluster, dst_name)?
            .filter(|d| d.short_offset != src_entry.short_offset);
        if dst_existing.is_none()
            && src_cluster == dst_cluster
            && src_entry.name[..src_entry.name_len] == *dst_name
        {
            return Ok(());
        }

        // Refuse moving a directory into itself or its own subtree.
        if moving_dir && self.is_subdir_of(dst_cluster, src_entry.cluster)? {
            return Err(DriverError::DirectoryCycle);
        }
        if reparenting
            && (!self.in_volume(src_entry.cluster) || self.dotdot(src_entry.cluster)?.is_none())
        {
            return Err(DriverError::DeviceFault);
        }

        // Replace an existing destination, subject to kind compatibility.
        if let Some(d) = dst_existing {
            if d.is_dir() != moving_dir {
                return Err(DriverError::Unsupported);
            }
            if d.is_dir() {
                let mut cur = DirCursor::at(d.cluster);
                if self.next_entry(&mut cur)?.is_some() {
                    return Err(DriverError::DirectoryNotEmpty);
                }
            }
            self.unlink(&d)?;
        }

        // Link the moved node under its new name, keeping its attributes and
        // stamps, then unlink the source.
        let placed = self.write_dir_entry(
            dst_cluster,
            dst_name,
            src_entry.attributes,
            src_entry.cluster,
            src_entry.size,
            src_entry.stamps,
        )?;
        if let Err(err) = self.pins.moved(src_entry.short_offset, placed.short_offset) {
            // The file's pin cannot follow it, so it keeps its old entry.
            self.delete_entry_slots(placed.first, placed.span)?;
            return Err(err);
        }
        self.delete_entry_slots(src_entry.first, src_entry.slot_span)?;

        if reparenting {
            let link = ShortEntry {
                cluster: self.parent_link(dst_cluster),
                ..self.dotdot_entry(src_entry.cluster)?
            };
            self.dev
                .write(self.dotdot_byte(src_entry.cluster), &link.encode())?;
        }
        Ok(())
    }
}

impl<B: Block> FilesystemRead for Fat32<B> {
    fn root(&self) -> NodeId {
        dir_node(self.layout.root_cluster)
    }

    fn name_matching(&self) -> NameMatching {
        NameMatching::AsciiCaseInsensitive
    }

    fn node_identity(&self) -> NodeIdentity {
        // A pin, and its last unpin, change the id a file is reported under,
        // and a rename moves its entry.
        NodeIdentity::ContentDerived
    }

    fn node_info(&mut self, node: NodeId) -> Result<NodeInfo, DriverError> {
        let short_offset = match named(node).ok_or(DriverError::NotFound)? {
            Named::Dir(cluster) => return self.dir_info(cluster),
            Named::File(short_offset) => short_offset,
            Named::Pinned(number) => match self.pins.kept(number)? {
                KeptFile::Entry(short_offset) => short_offset,
                KeptFile::Nameless(kept) => return self.nameless_info(&kept),
            },
        };
        let entry = self.file_at(short_offset)?;
        self.entry_info(&entry)
    }

    fn lookup(&mut self, dir: NodeId, name: &[u8]) -> Result<NodeId, DriverError> {
        let dir_cluster = cluster_of(dir)?;
        if name.is_empty()
            || name.len() > LONG_NAME_BYTES
            || self.pins.removed(dir_cluster).is_some()
        {
            return Err(DriverError::NotFound);
        }
        self.find_child(dir_cluster, name)?
            .map(|entry| self.reported(&entry))
            .ok_or(DriverError::NotFound)
    }

    fn read_at(&mut self, file: NodeId, offset: u64, buf: &mut [u8]) -> Result<usize, DriverError> {
        let facts = self.file(file)?;
        let size = u64::from(facts.size);
        if buf.is_empty() || offset >= size {
            return Ok(0);
        }
        let want = core::cmp::min(buf.len() as u64, size - offset);
        let want = usize::try_from(want).map_err(|_| DriverError::LengthOutOfRange)?;

        let first = facts.cluster;
        if first < 2 {
            return Err(DriverError::DeviceFault);
        }
        let bytes_per_cluster = self.layout.bytes_per_cluster;

        let mut cluster = first;
        let mut to_skip = offset / bytes_per_cluster;
        while to_skip > 0 {
            match self.next_cluster(cluster)? {
                Link::Next(next) => cluster = next,
                _ => return Err(DriverError::DeviceFault),
            }
            to_skip -= 1;
        }

        let mut intra = usize::try_from(offset % bytes_per_cluster)
            .map_err(|_| DriverError::LengthOutOfRange)?;
        let cluster_len =
            usize::try_from(bytes_per_cluster).map_err(|_| DriverError::LengthOutOfRange)?;
        let mut produced = 0;
        while produced < want {
            if cluster < 2 {
                return Err(DriverError::DeviceFault);
            }
            let take = core::cmp::min(cluster_len - intra, want - produced);
            let start = self.cluster_byte(cluster) + intra as u64;
            self.dev.read(start, &mut buf[produced..produced + take])?;
            produced += take;
            intra = 0;
            if produced < want {
                match self.next_cluster(cluster)? {
                    Link::Next(next) => cluster = next,
                    Link::End => break,
                    Link::Free | Link::Bad => return Err(DriverError::DeviceFault),
                }
            }
        }
        Ok(produced)
    }

    fn read_dir(
        &mut self,
        dir: NodeId,
        cursor: u64,
        _after: &[u8],
        visit: &mut dyn FnMut(&DirEntry, &[u8]) -> DirVisit,
    ) -> Result<(), DriverError> {
        let dir_cluster = cluster_of(dir)?;
        // The cursor is a logical slot index, resolved by walking this
        // directory's own chain, so no cursor reaches outside it. Slots never
        // move while in use — a deletion marks its slots in place and an
        // insertion takes a free run — so an entry present throughout a
        // listing is reached exactly once.
        if cursor >= u64::from(MAX_ENTRIES) || self.pins.removed(dir_cluster).is_some() {
            return Ok(());
        }
        let mut walk = DirCursor::at(dir_cluster);
        // A resumed batch starts on the slot its last entry took, to see
        // what holds it now.
        let resumed = cursor > 0;
        if !self.seek_slot(&mut walk, cursor - u64::from(resumed))? {
            return Ok(());
        }
        if resumed && !self.step_past_resume(&mut walk)? {
            return Ok(());
        }
        self.list_from(&mut walk, visit)
    }

    fn parent(&mut self, dir: NodeId) -> Result<NodeId, DriverError> {
        let cluster = match named(dir).ok_or(DriverError::NotFound)? {
            Named::Dir(cluster) => cluster,
            Named::File(_) | Named::Pinned(_) => return Err(DriverError::Unsupported),
        };
        if cluster == self.layout.root_cluster {
            return Ok(self.root());
        }
        if self.pins.removed(cluster).is_some() {
            return Err(DriverError::NotFound);
        }
        self.live_dir(cluster)?;
        self.dir_parent_cluster(cluster).map(dir_node)
    }

    fn pin(&mut self, node: NodeId) -> Result<NodeId, DriverError> {
        match named(node).ok_or(DriverError::NotFound)? {
            Named::Dir(cluster) => {
                if !self.pins.dirs.contains_key(u64::from(cluster)) {
                    self.live_dir(cluster)?;
                }
                self.pins.pin_dir(cluster)
            }
            Named::File(short_offset) => {
                if !self.pins.by_entry.contains_key(entry_slot(short_offset)) {
                    self.file_at(short_offset)?;
                }
                self.pins.pin_file(short_offset)
            }
            Named::Pinned(number) => self.pins.hold_file(number),
        }
    }

    fn unpin(&mut self, pinned: NodeId) -> Result<Unpinned, DriverError> {
        match named(pinned).ok_or(DriverError::NotFound)? {
            Named::Dir(cluster) => {
                let unpinned = self.pins.unpin_dir(cluster)?;
                if unpinned == Unpinned::Freed {
                    self.free_chain(cluster)?;
                }
                Ok(unpinned)
            }
            Named::File(_) => Err(DriverError::NotFound),
            Named::Pinned(number) => match self.pins.unpin_file(number)? {
                None => Ok(Unpinned::Held),
                Some(KeptFile::Entry(_)) => Ok(Unpinned::Released),
                Some(KeptFile::Nameless(kept)) => {
                    if kept.cluster >= 2 {
                        self.free_chain(kept.cluster)?;
                    }
                    Ok(Unpinned::Freed)
                }
            },
        }
    }

    fn release_pins(&mut self) -> Result<(), DriverError> {
        self.pins.by_entry.clear();
        let files = core::mem::take(&mut self.pins.files);
        let dirs = core::mem::take(&mut self.pins.dirs);
        let mut freed = Ok(());
        for (_, pin) in &files {
            if let KeptFile::Nameless(kept) = pin.kept {
                if kept.cluster >= 2 {
                    freed = freed.and(self.free_chain(kept.cluster));
                }
            }
        }
        for (key, held) in &dirs {
            if held.removed.is_some() {
                // Every key is a cluster `pin_dir` widened.
                let cluster = u32::try_from(key).map_err(|_| DriverError::DeviceFault);
                freed = freed.and(cluster.and_then(|cluster| self.free_chain(cluster)));
            }
        }
        freed
    }
}

impl<B: Block> Fat32<B> {
    /// Hand `visit` the entries from where `walk` stands until it stops or
    /// the directory ends.
    fn list_from(
        &mut self,
        walk: &mut DirCursor,
        visit: &mut dyn FnMut(&DirEntry, &[u8]) -> DirVisit,
    ) -> Result<(), DriverError> {
        while let Some(entry) = self.next_entry(walk)? {
            let listed = DirEntry {
                node: self.reported(&entry),
                info: self.entry_info(&entry)?,
                next_cursor: walk.pos.slot,
            };
            if visit(&listed, &entry.name[..entry.name_len]) == DirVisit::Stop {
                break;
            }
        }
        Ok(())
    }

    /// The id `entry` is reported under: the token of a pinned file, else its
    /// own.
    fn reported(&self, entry: &ParsedEntry) -> NodeId {
        match self.pins.by_entry.get(entry_slot(entry.short_offset)) {
            Some(&number) if !entry.is_dir() => token_node(number),
            _ => Self::entry_node(entry),
        }
    }
}

impl<B: Block> FilesystemWrite for Fat32<B> {
    fn create(
        &mut self,
        dir: NodeId,
        name: &[u8],
        kind: NodeKind,
        _record: &NodeSecurity,
    ) -> Result<NodeId, DriverError> {
        self.writable()?;
        // FAT keeps no owner or mode, so the record has nowhere to go.
        self.create_child(dir, name, kind)
    }

    fn write_at(&mut self, file: NodeId, offset: u64, data: &[u8]) -> Result<usize, DriverError> {
        self.writable()?;
        self.write_file(file, offset, data)
    }

    fn truncate(&mut self, file: NodeId, size: u64) -> Result<(), DriverError> {
        self.writable()?;
        self.truncate_file(file, size)
    }

    fn set_times(&mut self, node: NodeId, change: TimesChange) -> Result<TimesKept, DriverError> {
        self.set_entry_times(node, change)
    }

    fn remove(&mut self, dir: NodeId, name: &[u8]) -> Result<(), DriverError> {
        self.writable()?;
        self.remove_child(dir, name)
    }

    fn rename(
        &mut self,
        src_dir: NodeId,
        src_name: &[u8],
        dst_dir: NodeId,
        dst_name: &[u8],
    ) -> Result<(), DriverError> {
        self.writable()?;
        self.rename_child(src_dir, src_name, dst_dir, dst_name)
    }

    fn flush(&mut self) -> Result<(), DriverError> {
        // All mutations are written straight through to the block device.
        Ok(())
    }

    fn set_volume_host(&mut self, _volume: DriverHandle, host: &'static dyn VolumeHost) {
        self.host = Some(host);
    }
}

impl<B: Block> FilesystemSecurity for Fat32<B> {
    fn security(&mut self, node: NodeId) -> Result<NodeSecurity, DriverError> {
        // FAT32 stores no owner, mode, ACL, or capability gate, so every
        // node reports the one record an ownerless format stands in with;
        // the mount policy is what grants a foreign volume's users access.
        Ok(NodeSecurity::unrecorded(self.node_kind(node)?))
    }

    fn records_ownership(&mut self, node: NodeId) -> Result<bool, DriverError> {
        self.node_kind(node).map(|_| false)
    }

    fn set_security(&mut self, _node: NodeId, _security: NodeSecurity) -> Result<(), DriverError> {
        // The FAT32 on-disk format cannot store any part of a TAIRiX
        // security record. Storing a silently-lossy record is forbidden,
        // so the write is refused whole (fail closed).
        Err(DriverError::Unsupported)
    }
}

/// The on-disk format has nowhere to store TAIRiX extended attributes, so
/// the default facet answer stands: a mounted volume refuses the
/// `fs_attr_*` surface with the typed unsupported-backing error.
impl<B: Block> FilesystemAttrsProvider for Fat32<B> {}

impl<B: Block> FilesystemStats for Fat32<B> {
    fn stats(&mut self) -> Result<VolumeStats, DriverError> {
        // A pure read of the counter established by the open-time FAT scan
        // and maintained by the allocator — no device I/O. The allocation
        // unit FAT32 accounts in is the cluster, so the figures are
        // cluster-denominated; FAT32 has no inode table, and the zero pair
        // reports that honestly rather than fabricating a capacity.
        let bytes_per_cluster =
            u32::try_from(self.layout.bytes_per_cluster).map_err(|_| DriverError::DeviceFault)?;
        Ok(VolumeStats {
            block_size: bytes_per_cluster,
            in_reserve: false,
            total_blocks: u64::from(self.layout.max_cluster) - 1,
            free_blocks: self.free_clusters,
            avail_blocks: self.free_clusters,
            files: 0,
            files_free: 0,
        })
    }
}

/// What a retitle writes in the root directory, decided before anything is
/// written.
#[derive(Clone, Copy)]
enum EntryWrite {
    /// These bytes over the label entry at this device byte: the new label,
    /// or the entry deleted.
    Over(u64, RawEntry),
    /// A new label entry, these bytes, in this free run.
    New(FreeRun, RawEntry),
    /// Nothing: there is no label entry to delete.
    Nothing,
}

/// `title` as a label field, or `None` for the empty title, which is no
/// label ([`name::label`]): a lower-case letter is refused rather than
/// folded.
fn label_of(title: &VolumeTitle) -> Result<Option<[u8; LABEL_LEN]>, DriverError> {
    name::label(title.as_str().as_bytes()).map_err(|refused| match refused {
        LabelError::Spelling => DriverError::OutOfRange,
        LabelError::Length => DriverError::LengthOutOfRange,
    })
}

impl<B: Block> Fat32<B> {
    /// What retitling the volume to `field` writes in its root directory,
    /// within the first `window` bytes the volume probe reads the label from
    /// ([`Bpb::label_window`]), so the probe, and
    /// [`FilesystemLabel::label`] through it, read back what is written: the
    /// first label entry before the entries end, rewritten or, for `None`,
    /// deleted; with none, a new one in the first deleted slot or at the end
    /// of the entries.
    ///
    /// # Errors
    ///
    /// [`DriverError::NoSpace`] for a new label entry the window has no slot
    /// for.
    fn entry_write(
        &mut self,
        window: usize,
        field: Option<[u8; LABEL_LEN]>,
    ) -> Result<EntryWrite, DriverError> {
        let slots = (window / ENTRY) as u64;
        let mut walk = DirCursor::at(self.layout.root_cluster);
        let mut free = None;
        while walk.pos.slot < slots {
            let Some((at, raw)) = self.next_slot(&mut walk)? else {
                break;
            };
            match dir::slot(&raw) {
                Slot::End => {
                    free.get_or_insert(FreeRun {
                        start: at,
                        at_end: true,
                    });
                    break;
                }
                Slot::Deleted => {
                    free.get_or_insert(FreeRun {
                        start: at,
                        at_end: false,
                    });
                }
                Slot::Label => {
                    let at = self.slot_byte(at);
                    return Ok(EntryWrite::Over(at, self.relabelled(raw, field)));
                }
                Slot::Long | Slot::Short => {}
            }
        }
        match (free, field) {
            (_, None) => Ok(EntryWrite::Nothing),
            (Some(run), Some(field)) => {
                let label = ShortEntry {
                    name: field,
                    attributes: attr::VOLUME_ID,
                    stamps: Stamps::at(self.now()),
                    ..ShortEntry::default()
                };
                Ok(EntryWrite::New(run, label.encode()))
            }
            (None, Some(_)) => Err(DriverError::NoSpace),
        }
    }

    /// The label entry `raw` holding `field`, its last-write stamp now, or
    /// deleted for `None`.
    fn relabelled(&self, mut raw: RawEntry, field: Option<[u8; LABEL_LEN]>) -> RawEntry {
        let Some(field) = field else {
            raw[dir::at::NAME] = DELETED;
            return raw;
        };
        let label = ShortEntry::decode(&raw);
        ShortEntry {
            name: field,
            attributes: label.attributes,
            stamps: Stamps {
                written: self.written_now(),
                ..label.stamps
            },
            ..ShortEntry::default()
        }
        .encode()
    }

    /// Where the boot sector `bpb` was read from and its backup keep the
    /// label (`BS_VolLab`), each only where its extended boot signature
    /// declares the field; the backup is read here.
    fn boot_label_copies(&mut self, bpb: &Bpb) -> Result<[Option<u64>; 2], DriverError> {
        let label_at = at::LABEL_32 as u64;
        let primary = bpb.label.is_some().then_some(label_at);
        let backup = u64::from(bpb.backup_boot_sector);
        if backup == 0 || backup >= u64::from(bpb.reserved_sectors) {
            return Ok([primary, None]);
        }
        let start = backup * bpb.sector_bytes();
        let mut copy = [0u8; BOOT_SECTOR];
        self.dev.read(start, &mut copy)?;
        let kept = fat32_bpb(&copy).is_some_and(|copy| copy.label.is_some());
        Ok([primary, kept.then_some(start + label_at)])
    }

    /// Write `kept` where `copies` say the boot sectors keep the label, then
    /// `entry`: the root's label entry, which readers prefer, last.
    fn write_label(
        &mut self,
        entry: EntryWrite,
        copies: [Option<u64>; 2],
        kept: &[u8; LABEL_LEN],
    ) -> Result<(), DriverError> {
        for at in copies.into_iter().flatten() {
            self.dev.write(at, kept)?;
        }
        match entry {
            EntryWrite::Over(at, raw) => self.dev.write(at, &raw),
            EntryWrite::New(run, raw) => {
                self.mark_end_after(run, 1)?;
                let mut pos = run.start;
                self.put_slot(&mut pos, &raw)?;
                Ok(())
            }
            EntryWrite::Nothing => Ok(()),
        }
    }
}

/// FAT keeps a volume's label twice: in the root directory's label entry,
/// which readers prefer, and in the boot sector and its backup. A retitle
/// reads and refuses before it writes anything, then writes the boot
/// sectors' copies and the entry last; one failing once it has begun
/// writing leaves the volume refusing writes, damaged.
impl<B: Block> FilesystemLabel for Fat32<B> {
    fn label(&mut self) -> Result<VolumeTitle, DriverError> {
        let Some(Recognised::Volume(volume)) = tairix_fsprobe::recognise(&mut self.dev)? else {
            return Err(DriverError::BadMagic);
        };
        if volume.fstype != VolumeFsType::Fat32 {
            return Err(DriverError::BadMagic);
        }
        // FAT records no code page, so a byte past ASCII names no character.
        if !volume.label().is_ascii() {
            return Err(DriverError::OutOfRange);
        }
        let text = core::str::from_utf8(volume.label()).map_err(|_| DriverError::OutOfRange)?;
        VolumeTitle::new(text).map_err(DriverError::from_errno)
    }

    fn set_label(&mut self, title: &VolumeTitle) -> Result<(), DriverError> {
        let field = label_of(title)?;
        self.writable()?;
        let mut boot = [0u8; BOOT_SECTOR];
        self.dev.read(0, &mut boot)?;
        let bpb = fat32_bpb(&boot).ok_or(DriverError::BadMagic)?;
        let (_, window) = bpb.label_window().ok_or(DriverError::BadMagic)?;
        let entry = self.entry_write(window, field)?;
        let copies = self.boot_label_copies(&bpb)?;
        let kept = field.unwrap_or_else(|| name::label_field(NO_LABEL));
        let written = self.write_label(entry, copies, &kept);
        if written.is_err() {
            self.refusal = Some(WriteRefusal::Damaged);
        }
        written
    }
}

#[cfg(test)]
mod tests;
