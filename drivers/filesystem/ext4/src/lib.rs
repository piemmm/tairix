//! TAIRiX ext2/ext3/ext4 filesystem driver (read/write, journaled).
//!
//! Serves an ext2, ext3 or ext4 volume behind any
//! [`tairix_abi::driver::block::Block`] device through [`FilesystemRead`],
//! [`FilesystemWrite`], [`FilesystemSecurity`] and [`FilesystemStats`]. It
//! makes no permission decision: owners, modes and ACLs are the VFS's to
//! apply.
//!
//! Metadata changes batch into transactions, committed through the volume's
//! jbd2 journal where it has one and checkpointed home, so a crash leaves the
//! volume as one commit or the next. Opening a volume writable replays a
//! journal a crash left and finishes its orphans. File data is written in
//! place ahead of the commit that maps it. An unjournaled volume is marked
//! not clean while a commit is in flight, and one found unclean is served
//! read-only until a checker has seen it.
//!
//! The only public function is [`register`]; [`Ext4`] is the type the host
//! opens a volume with.
//!
//! # Capabilities
//!
//! Loading requires
//! [`CapabilityId::DRV_LOAD`](tairix_abi::CapabilityId::DRV_LOAD).

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_abi::driver::block::Block;
use tairix_abi::driver::filesystem::{
    AclModel, AllocationClass, DirEntry, DirVisit, FilesystemAttrsProvider, FilesystemRead,
    FilesystemSecurity, FilesystemStats, FilesystemWrite, NameMatching, NodeId, NodeIdentity,
    NodeInfo, NodeKind, NodeSecurity, NodeTimes, VolumeHost, VolumeStats,
};
use tairix_abi::fs::{SeekRegion, FS_SYMLINK_MAX};
use tairix_abi::time::Time64;
use tairix_abi::{CapabilityId, DriverError, DriverHandle, DriverHost};
use tairix_reclaim::{CacheBudget, PinnedAccounting, PressureGauge};
use tairix_writeback::{CommitScheduler, DirtySet, WritebackBound, WritebackReading};

mod blockmap;
mod bytes;
mod extent;
mod file;
mod format;
mod group;
mod htree;
mod indexed;
mod inode;
mod io;
mod journal;
mod map;
mod orphan;
mod superblock;
mod txn;
mod xattr;

use bytes::{
    align4, le16, le32, put_le16, put_le32, put_split_id, scratch, split_id, u16_of, u32_of,
};
use extent::Mapping;
use group::{Hints, Releases};
use indexed::IndexUse;
use io::Device;
use journal::Journal;
pub use superblock::Generation;
use superblock::{
    Layout, NoWrite, INCOMPAT_RECOVER, RO_COMPAT_METADATA_CSUM, SB_FEATURE_INCOMPAT,
    SB_FEATURE_RO_COMPAT, SB_FREE_INODES, SB_STATE, SB_WTIME, STATE_VALID, SUPERBLOCK_LEN,
    SUPERBLOCK_OFFSET,
};
use txn::OpMark;

/// Per-driver `DriverHandle` marker returned by [`register`].
const REGISTER_HANDLE_MARKER: u64 = 0x4558_5434_0000_0001; // "EXT4" + index

/// Driver entry point.
///
/// # Errors
///
/// * [`DriverError::PermissionDenied`] if the host did not grant
///   [`CapabilityId::DRV_LOAD`].
pub fn register(host: &dyn DriverHost) -> Result<DriverHandle, DriverError> {
    if !host.has_capability(CapabilityId::DRV_LOAD) {
        return Err(DriverError::PermissionDenied);
    }
    DriverHandle::from_raw(REGISTER_HANDLE_MARKER)
}

/// The largest device or filesystem block the driver stages.
const MAX_BLOCK_SIZE: u32 = superblock::MAX_FS_BLOCK;

const INODE_SIZE_LO: usize = 0x04;
const INODE_DTIME: usize = 0x14;
const INODE_LINKS: usize = 0x1A;
const INODE_UID_LO: usize = 0x02;
const INODE_UID_HI: usize = 0x78;
const INODE_GID_LO: usize = 0x18;
const INODE_GID_HI: usize = 0x7A;
const INODE_BLOCKS_LO: usize = 0x1C;
const INODE_FLAGS: usize = 0x20;
const INODE_FILE_ACL_LO: usize = 0x68;
const INODE_SIZE_HI: usize = 0x6C;
const INODE_BLOCKS_HI: usize = 0x74;
const INODE_FILE_ACL_HI: usize = 0x76;

/// The node is never changed, linked, renamed or removed (`chattr +i`).
const INODE_FLAG_IMMUTABLE: u32 = 0x0000_0010;
/// The file only grows at its end and is never renamed or removed; the
/// directory only gains entries (`chattr +a`).
const INODE_FLAG_APPEND: u32 = 0x0000_0020;
const INODE_FLAG_ENCRYPT: u32 = 0x0000_0800;
const INODE_FLAG_INDEX: u32 = 0x0000_1000;
const INODE_FLAG_HUGE_FILE: u32 = 0x0004_0000;
const INODE_FLAG_EXTENTS: u32 = 0x0008_0000;
const INODE_FLAG_INLINE_DATA: u32 = 0x1000_0000;
const INODE_FLAG_CASEFOLD: u32 = 0x4000_0000;

/// `i_dtime` for a deleted inode when the time it went is unknown: non-zero
/// marks it deleted, and it is above any inode number, so a checker never
/// reads it as an orphan-chain link.
const DELETED_DTIME: u32 = 1_700_000_000;

/// The checksum tail closing a directory leaf: a twelve-byte empty record.
const DIR_TAIL_LEN: usize = 12;
const DIR_TAIL_FT: u8 = 0xDE;

/// `i_extra_isize` for the inodes this driver creates, the mke2fs default.
const NEW_EXTRA_ISIZE: u16 = 32;

const S_IFMT: u16 = 0xF000;
const S_IFDIR: u16 = 0x4000;
const S_IFREG: u16 = 0x8000;
const S_IFLNK: u16 = 0xA000;
const S_IFIFO: u16 = 0x1000;
const S_IFCHR: u16 = 0x2000;
const S_IFBLK: u16 = 0x6000;
const S_IFSOCK: u16 = 0xC000;

const FT_REG: u8 = 1;
const FT_DIR: u8 = 2;

/// The directory-entry `file_type` an inode of `mode` is listed with.
fn dirent_file_type(mode: u16) -> u8 {
    match mode & S_IFMT {
        S_IFREG => FT_REG,
        S_IFDIR => FT_DIR,
        S_IFCHR => 3,
        S_IFBLK => 4,
        S_IFIFO => 5,
        S_IFSOCK => 6,
        S_IFLNK => 7,
        _ => 0,
    }
}

/// The root directory is always inode 2.
const ROOT_INODE: u32 = 2;

/// `i_block`: 60 bytes at offset 40.
const I_BLOCK_LEN: usize = 60;
const I_BLOCK_OFFSET: usize = 40;

/// The bytes of a record a decoded [`Inode`] keeps: the classic record and
/// every extended-timestamp field.
const INODE_HEAD: usize = 160;

/// The most links a directory records before `dir_nlink` stops counting.
const DIR_LINK_MAX: u16 = 65_000;
/// The most links a directory may have on a volume without `dir_nlink`.
const DIR_LINK_MAX_COUNTED: u16 = 32_000;

/// A journal must hold a transaction of at least this many blocks to be
/// written through; a smaller one could not take one operation.
const MIN_JOURNAL_CAPACITY: usize = 64;

/// Longest name the format stores.
const MAX_NAME_LEN: usize = 255;
/// Modes new nodes take; the VFS stamps the owner.
const NEW_FILE_MODE: u16 = S_IFREG | 0o644;
const NEW_DIR_MODE: u16 = S_IFDIR | 0o755;

/// One of the four instants an inode records: a classic seconds field and,
/// in an enlarged record, the `_extra` field that extends it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Stamp {
    Access,
    Change,
    Modify,
    /// Birth time, which only an enlarged record has room for.
    Create,
}

impl Stamp {
    /// Every stamp, as a new inode takes them.
    const ALL: [Self; 4] = [Self::Access, Self::Change, Self::Modify, Self::Create];

    /// The offsets of the stamp's seconds field and its `_extra` field.
    const fn fields(self) -> (usize, usize) {
        match self {
            Self::Access => (0x08, 0x8C),
            Self::Change => (0x0C, 0x84),
            Self::Modify => (0x10, 0x88),
            Self::Create => (0x90, 0x94),
        }
    }
}

/// Whether record `raw` holds the four-byte enlarged-record field at `off`.
fn has_extra(raw: &[u8], off: usize) -> bool {
    raw.len() >= off + 4
        && inode::EXTRA_ISIZE + usize::from(le16(raw, inode::EXTRA_ISIZE)) >= off + 4
}

/// Decode an inode timestamp: signed 32-bit seconds, extended by the
/// `_extra` field's two epoch bits and 30-bit nanoseconds (1901 to 2446). A
/// nanosecond count of a second or more is damage.
fn decode_inode_time(secs: u32, extra: Option<u32>) -> Result<Time64, DriverError> {
    let signed_secs = i64::from(i32::from_le_bytes(secs.to_le_bytes()));
    let Some(extra) = extra else {
        return Ok(Time64::from_secs(signed_secs));
    };
    let seconds = signed_secs + (i64::from(extra & 0x3) << 32);
    Time64::new(seconds, extra >> 2).map_err(|_| DriverError::DeviceFault)
}

/// Encode `time` as [`decode_inode_time`] reads it back, or `None` for an
/// instant the fields cannot hold, so no stamp is recorded rather than a
/// wrapped one. Without an `_extra` field whole seconds are the precision.
fn encode_inode_time(time: Time64, extended: bool) -> Option<(u32, u32)> {
    let secs = time.secs();
    let base = u32::try_from(secs.rem_euclid(1 << 32)).ok()?;
    let epoch = secs.checked_sub(i64::from(i32::from_le_bytes(base.to_le_bytes())))? >> 32;
    let epoch = u32::try_from(epoch)
        .ok()
        .filter(|&epoch| epoch <= if extended { 3 } else { 0 })?;
    Some((base, epoch | (time.subsec_nanos() << 2)))
}

/// Record `now` as each of `stamps`, or no stamp — zero fields — while the
/// time is unknown or beyond what the record holds.
fn put_stamps(raw: &mut [u8], now: Option<Time64>, stamps: &[Stamp]) {
    for &stamp in stamps {
        let (secs_at, extra_at) = stamp.fields();
        if stamp == Stamp::Create && !has_extra(raw, secs_at) {
            continue;
        }
        let extended = has_extra(raw, extra_at);
        let (secs, extra) = now
            .and_then(|time| encode_inode_time(time, extended))
            .unwrap_or((0, 0));
        put_le32(raw, secs_at, secs);
        if extended {
            put_le32(raw, extra_at, extra);
        }
    }
}

/// The node kind an `i_mode` names, or `None` for a special file.
fn kind_of(mode: u16) -> Option<NodeKind> {
    match mode & S_IFMT {
        S_IFDIR => Some(NodeKind::Directory),
        S_IFREG => Some(NodeKind::RegularFile),
        S_IFLNK => Some(NodeKind::Symlink),
        _ => None,
    }
}

/// A decoded inode: the fields the read surface needs, and the head of its
/// record, which its map is read from.
struct Inode {
    ino: u32,
    mode: u16,
    times: NodeTimes,
    size: u64,
    flags: u32,
    links: u16,
    /// Allocated storage, in 512-byte sectors.
    blocks: u64,
    generation: u32,
    head: [u8; INODE_HEAD],
}

impl Inode {
    fn kind(&self) -> Option<NodeKind> {
        kind_of(self.mode)
    }

    /// The [`NodeInfo`] `node_info` and a listing both report.
    fn info(&self) -> Result<NodeInfo, DriverError> {
        let kind = self.kind().ok_or(DriverError::NotFound)?;
        let size = match kind {
            NodeKind::Directory => 0,
            NodeKind::RegularFile | NodeKind::Symlink => self.size,
        };
        Ok(NodeInfo {
            kind,
            nlink: u32::from(self.links),
            size,
            allocated: self.blocks.saturating_mul(512),
            times: self.times,
            content_gen: NodeInfo::NO_CONTENT_GEN,
        })
    }
}

/// The journal file's blocks as runs of `(first journal block, physical
/// block, length)`, in order.
struct JournalMap {
    runs: Vec<(u32, u64, u32)>,
}

impl JournalMap {
    /// The map of `runs`, in journal order. Two journal blocks that share a
    /// physical block are no log, and refused.
    fn new(runs: Vec<(u32, u64, u32)>) -> Result<Self, DriverError> {
        let mut extents: Vec<(u64, u64)> = Vec::new();
        extents
            .try_reserve_exact(runs.len())
            .map_err(|_| DriverError::NoSpace)?;
        extents.extend(runs.iter().map(|&(_, phys, len)| (phys, u64::from(len))));
        extents.sort_unstable();
        if extents
            .windows(2)
            .any(|pair| pair[0].0 + pair[0].1 > pair[1].0)
        {
            return Err(DriverError::DeviceFault);
        }
        Ok(Self { runs })
    }

    /// The physical block of journal block `n`.
    fn resolve(&self, n: u32) -> Option<u64> {
        let at = self
            .runs
            .partition_point(|&(start, _, _)| start <= n)
            .checked_sub(1)?;
        let (start, phys, len) = self.runs[at];
        (n - start < len).then(|| phys + u64::from(n - start))
    }
}

/// The ext4 driver over a [`Block`] device.
pub struct Ext4<B: Block> {
    dev: Device<B>,
    layout: Layout,
    /// The superblock as the open transaction has it.
    sb: Vec<u8>,
    /// The superblock as last written to the medium.
    disk_sb: Vec<u8>,
    /// The superblock as the running operation found it.
    mark_sb: Vec<u8>,
    /// Metadata blocks the open transaction changed.
    dirty: DirtySet,
    /// Runs the open transaction freed, released when it commits.
    freed: Releases,
    /// Blocks applying `freed` will stage, counted against the journal.
    freed_cost: usize,
    /// The running operation's undo point.
    op: Option<OpMark>,
    journal: Option<Journal>,
    journal_map: Option<JournalMap>,
    hints: Hints,
    schedule: CommitScheduler,
    bound: Option<WritebackBound>,
    reading: Option<WritebackReading>,
    /// File data was written in place since the last commit.
    data_written: bool,
    class: AllocationClass,
    in_reserve: bool,
    /// A release a map could not report, failing its operation.
    release_fault: Option<DriverError>,
    /// The source of new inodes' generations.
    generation: u32,
    /// Whether this mount's first commit has counted the mount.
    session_counted: bool,
}

impl<B: Block> Ext4<B> {
    /// Open the volume on `block` for reading and writing, replaying a
    /// journal a crash left and finishing its orphans. A volume this driver
    /// cannot safely write is opened all the same and serves reads; see
    /// [`Self::write_refusal`].
    ///
    /// # Errors
    ///
    /// * [`DriverError::BadMagic`] for a superblock that is not ext2/3/4 or
    ///   is structurally invalid.
    /// * [`DriverError::Unsupported`] for a feature the read path does not
    ///   implement, or a journal needing a replay this driver cannot make.
    /// * [`DriverError::DeviceFault`] for an I/O failure or a checksum that
    ///   does not match.
    pub fn open(block: B) -> Result<Self, DriverError> {
        Self::mount(block, true)
    }

    /// Open the volume on `block` read-only. It is never written, so a
    /// journal needing replay refuses the open rather than serve the
    /// half-checkpointed state beneath it.
    ///
    /// # Errors
    ///
    /// As [`Self::open`].
    pub fn open_read_only(block: B) -> Result<Self, DriverError> {
        Self::mount(block, false)
    }

    fn mount(block: B, writable: bool) -> Result<Self, DriverError> {
        Self::assemble(Device::new(block)?, writable, true)
    }

    /// Read the volume on `dev` and bring its journal up. When `may_replay`,
    /// a log that needs replaying is replayed and the volume read again from
    /// what the replay left — it may have rewritten the superblock, geometry
    /// and all — and that second reading refuses a log still needing one.
    fn assemble(mut dev: Device<B>, writable: bool, may_replay: bool) -> Result<Self, DriverError> {
        let bytes = dev.bytes();
        if bytes < SUPERBLOCK_OFFSET + SUPERBLOCK_LEN as u64 {
            return Err(DriverError::BadMagic);
        }
        let mut sb = scratch(SUPERBLOCK_LEN)?;
        dev.read_bytes(SUPERBLOCK_OFFSET, &mut sb)?;
        let mut layout = superblock::parse(&sb, bytes)?;
        let mut disk_sb = scratch(SUPERBLOCK_LEN)?;
        disk_sb.copy_from_slice(&sb);
        if !writable {
            layout.no_write = Some(NoWrite::ReadOnly);
        }
        dev.set_fs_block(layout.block_size);
        let block_size = usize::try_from(layout.block_size)
            .ok()
            .and_then(core::num::NonZeroUsize::new)
            .ok_or(DriverError::Unsupported)?;
        let cost = dev.command_cost();
        let generation = tairix_crc32c::update(le32(&sb, SB_WTIME), &layout.uuid);
        let mut fs = Self {
            dev,
            dirty: DirtySet::new(block_size),
            layout,
            sb,
            disk_sb,
            mark_sb: scratch(SUPERBLOCK_LEN)?,
            freed: Releases::default(),
            freed_cost: 0,
            op: None,
            journal: None,
            journal_map: None,
            hints: Hints::default(),
            schedule: CommitScheduler::new(cost),
            bound: None,
            reading: None,
            data_written: false,
            class: AllocationClass::Ordinary,
            in_reserve: false,
            release_fault: None,
            generation,
            session_counted: false,
        };
        if fs.attach_journal(writable, may_replay)? {
            return Self::assemble(fs.dev, writable, false);
        }
        if fs.layout.no_write.is_none() {
            let clean = le16(&fs.sb, SB_STATE) & STATE_VALID != 0;
            // An unjournaled volume a crash left mid-write, orphans that
            // cannot be finished, or totals the groups cannot account for,
            // need a checker before anything is written.
            if (fs.journal.is_none() && !clean)
                || fs.finish_orphans().is_err()
                || fs.operation(Self::recount_free).is_err()
            {
                fs.layout.no_write.get_or_insert(NoWrite::Unclean);
            }
        }
        Ok(fs)
    }

    /// Load the journal, answering whether it was replayed — in which case
    /// nothing read before the replay describes the volume any more.
    ///
    /// A log left under a clear mark is replayed all the same, as e2fsck
    /// does: whatever it commits is applied before anything new is logged
    /// after it. Read-only, such a volume is refused rather than served from
    /// beneath a log it cannot apply.
    fn attach_journal(&mut self, writable: bool, may_replay: bool) -> Result<bool, DriverError> {
        let recover = le32(&self.sb, SB_FEATURE_INCOMPAT) & INCOMPAT_RECOVER != 0;
        let Some(ino) = self.layout.journal_inum else {
            return if recover {
                Err(DriverError::Unsupported)
            } else {
                Ok(false)
            };
        };
        let (mut journal, map) = match self.load_journal(ino) {
            Ok(loaded) => loaded,
            Err(_) if !recover => {
                self.layout.no_write.get_or_insert(NoWrite::Journal);
                return Ok(false);
            }
            Err(_) => return Err(DriverError::Unsupported),
        };
        if recover || journal.has_log() {
            // A replay is a write, which a volume another host may be
            // writing must not be given.
            let shared = le32(&self.sb, SB_FEATURE_INCOMPAT) & superblock::INCOMPAT_MMP != 0;
            if !writable || !may_replay || shared {
                return Err(DriverError::Unsupported);
            }
            self.replay(&mut journal, &map)?;
            return Ok(true);
        }
        if journal.errno() != 0 || journal.capacity() < MIN_JOURNAL_CAPACITY {
            self.layout.no_write.get_or_insert(NoWrite::Journal);
        }
        journal.adopt_features(self.layout.wide_desc(), self.layout.metadata_csum);
        self.journal = Some(journal);
        self.journal_map = Some(map);
        Ok(false)
    }

    /// Replay `journal` home, then clear the volume's `needs_recovery`.
    fn replay(&mut self, journal: &mut Journal, map: &JournalMap) -> Result<(), DriverError> {
        let mut io = txn::JournalDev {
            dev: &mut self.dev,
            map,
        };
        journal.recover(&mut io)?;
        let mut sb = scratch(SUPERBLOCK_LEN)?;
        self.dev.read_bytes(SUPERBLOCK_OFFSET, &mut sb)?;
        let incompat = le32(&sb, SB_FEATURE_INCOMPAT) & !INCOMPAT_RECOVER;
        put_le32(&mut sb, SB_FEATURE_INCOMPAT, incompat);
        if le32(&sb, SB_FEATURE_RO_COMPAT) & RO_COMPAT_METADATA_CSUM != 0 {
            superblock::stamp(&mut sb);
        }
        self.dev.write_bytes(SUPERBLOCK_OFFSET, &sb)?;
        self.dev.flush()
    }

    /// Parse the journal inode `ino` and map its blocks.
    /// Parse the journal inode `ino` and map the blocks its log uses: as
    /// many as its superblock names, none of them twice.
    fn load_journal(&mut self, ino: u32) -> Result<(Journal, JournalMap), DriverError> {
        let mut raw = self.inode_scratch()?;
        self.read_inode_raw(ino, &mut raw)?;
        let bs = u64::from(self.layout.block_size);
        let file_blocks = file::inode_size_bytes(&raw, &self.layout) / bs;
        let (first, _) = self.journal_run(ino, &raw, 0, file_blocks)?;
        let mut block = self.block_scratch()?;
        self.dev.read_blocks(first, &mut block)?;
        let journal = Journal::parse(block, self.layout.block_size as usize, file_blocks)
            .map_err(|_| DriverError::Unsupported)?;
        let blocks = u64::from(journal.maxlen());
        let mut runs = Vec::new();
        let mut logical = 0u64;
        while logical < blocks {
            let (phys, len) = self.journal_run(ino, &raw, logical, blocks)?;
            runs.try_reserve(1).map_err(|_| DriverError::NoSpace)?;
            runs.push((u32_of(logical)?, phys, u32_of(len)?));
            logical += len;
        }
        Ok((journal, JournalMap::new(runs)?))
    }

    /// The written run of journal inode `ino` (`raw`) from file block
    /// `logical`, inside the volume and no longer than to file block `end`.
    fn journal_run(
        &mut self,
        ino: u32,
        raw: &[u8],
        logical: u64,
        end: u64,
    ) -> Result<(u64, u64), DriverError> {
        let Mapping::Mapped {
            phys,
            len,
            unwritten: false,
        } = self.map_lookup(ino, raw, logical)?
        else {
            return Err(DriverError::DeviceFault);
        };
        let len = len.min(end.saturating_sub(logical));
        if len == 0
            || phys
                .checked_add(len)
                .is_none_or(|run_end| run_end > self.layout.blocks_count)
        {
            return Err(DriverError::DeviceFault);
        }
        Ok((phys, len))
    }

    /// The volume's stable identity: its superblock `s_uuid`.
    #[must_use]
    pub fn volume_uuid(&self) -> [u8; 16] {
        self.layout.uuid
    }

    /// The ext generation the volume's features make it.
    #[must_use]
    pub fn generation(&self) -> Generation {
        self.layout.generation
    }

    /// Why this handle refuses writes, as a stable cause for the host's
    /// audit record, or `None` when it accepts them.
    #[must_use]
    pub fn write_refusal(&self) -> Option<&'static str> {
        self.layout.no_write.map(|reason| match reason {
            NoWrite::Feature => "unsupported_feature",
            NoWrite::Errors => "errors_recorded",
            NoWrite::Hurd => "hurd_layout",
            NoWrite::Journal => "journal_unusable",
            NoWrite::Unclean => "needs_check",
            NoWrite::ReadOnly => "opened_read_only",
            NoWrite::Halted => "commit_failed",
        })
    }

    /// Lay a fresh, empty volume onto `block` and open it. `uuid` is the
    /// volume's identity, minted by the caller; all-zero is refused.
    ///
    /// # Errors
    ///
    /// [`DriverError::OutOfRange`] for a device too small for one group, a
    /// zero `inode_count`, or the all-zero `uuid`;
    /// [`DriverError::DeviceFault`] for a write failure.
    pub fn format(block: B, inode_count: u32, uuid: [u8; 16]) -> Result<Self, DriverError> {
        Self::open(format::write_volume(block, inode_count, uuid)?)
    }

    /// Bound this volume's open transaction to the machine: a share of the
    /// RAM-derived `backing` budget under `gauge`, its footprint published
    /// through `pinned`.
    ///
    /// # Errors
    ///
    /// [`DriverError::NoSpace`] when the machine cannot spare one coalesced
    /// device transfer for it; the mount is refused rather than left to
    /// commit after every operation.
    pub fn with_writeback_bound(
        mut self,
        backing: usize,
        gauge: &'static dyn PressureGauge,
        pinned: Arc<PinnedAccounting>,
    ) -> Result<Self, DriverError> {
        self.bound = Some(WritebackBound::new(
            CacheBudget::from_backing(backing),
            gauge,
            pinned,
        )?);
        self.publish_footprint();
        Ok(self)
    }

    /// Publish what is open, make it durable, and hand the device back.
    ///
    /// # Errors
    ///
    /// The closing commit's failure: the device is not handed back with an
    /// image older than the operations reported done.
    pub fn into_block(mut self) -> Result<B, DriverError> {
        self.commit(true)?;
        Ok(self.dev.into_inner())
    }

    /// The instant an operation stamps, or `None` while the host has none.
    fn now(&self) -> Option<Time64> {
        self.schedule.host().and_then(|(_, host)| host.wall_time())
    }

    /// Read and decode inode `ino`.
    fn read_inode(&mut self, ino: u32) -> Result<Inode, DriverError> {
        let mut raw = self.inode_scratch()?;
        self.read_inode_raw(ino, &mut raw)?;
        let raw = &raw[..];
        let stamp = |stamp: Stamp| {
            let (secs, extra) = stamp.fields();
            decode_inode_time(
                le32(raw, secs),
                has_extra(raw, extra).then(|| le32(raw, extra)),
            )
        };
        let created = if has_extra(raw, Stamp::Create.fields().0) {
            stamp(Stamp::Create)?
        } else {
            Time64::UNIX_EPOCH
        };
        let times = NodeTimes {
            created,
            modified: stamp(Stamp::Modify)?,
            accessed: stamp(Stamp::Access)?,
            changed: stamp(Stamp::Change)?,
        };
        let mut head = [0u8; INODE_HEAD];
        let take = raw.len().min(INODE_HEAD);
        head[..take].copy_from_slice(&raw[..take]);
        Ok(Inode {
            ino,
            mode: le16(raw, 0),
            times,
            size: file::inode_size_bytes(raw, &self.layout),
            flags: le32(raw, INODE_FLAGS),
            links: le16(raw, INODE_LINKS),
            blocks: file::sectors(raw, &self.layout),
            generation: le32(raw, inode::GENERATION),
            head,
        })
    }

    /// Read metadata block `block` into `buf[..block_size]`.
    fn read_fs_block(&mut self, block: u64, buf: &mut [u8]) -> Result<(), DriverError> {
        self.read_meta(block, buf).map(drop)
    }

    /// Stage `buf[..block_size]` as metadata block `block`.
    fn write_fs_block(&mut self, block: u64, buf: &[u8]) -> Result<(), DriverError> {
        self.put_meta(block, buf)
    }

    /// The physical block behind logical block `logical` of directory
    /// `inode`, or `None` for a hole. A directory holding its entries
    /// inline or encrypted is one this driver does not read, and one with
    /// unwritten blocks is damage.
    fn map_block(&mut self, inode: &Inode, logical: u64) -> Result<Option<u64>, DriverError> {
        check_block_mapped(inode)?;
        match self.map_lookup(inode.ino, &inode.head, logical)? {
            Mapping::Mapped {
                phys,
                unwritten: false,
                ..
            } => Ok(Some(phys)),
            Mapping::Mapped { .. } => Err(DriverError::DeviceFault),
            Mapping::Hole { .. } => Ok(None),
        }
    }

    /// The next block `walk` reaches in directory `dir`: its logical and
    /// physical numbers, or `None` past the last mapped one.
    fn next_dir_block(
        &mut self,
        dir: &Inode,
        walk: &mut DirBlocks,
    ) -> Result<Option<(u64, u64)>, DriverError> {
        check_block_mapped(dir)?;
        while walk.run_left == 0 {
            if walk.next >= walk.end {
                return Ok(None);
            }
            match self.map_lookup(dir.ino, &dir.head, walk.next)? {
                Mapping::Mapped {
                    phys,
                    len,
                    unwritten: false,
                } => (walk.run_phys, walk.run_left) = (phys, len.min(walk.end - walk.next)),
                Mapping::Mapped { .. } => return Err(DriverError::DeviceFault),
                Mapping::Hole { len: Some(len) } => {
                    walk.next = walk.next.saturating_add(len.max(1));
                }
                Mapping::Hole { len: None } => return Ok(None),
            }
        }
        walk.budget = walk.budget.checked_sub(1).ok_or(DriverError::DeviceFault)?;
        let at = (walk.next, walk.run_phys);
        walk.next += 1;
        walk.run_phys += 1;
        walk.run_left -= 1;
        Ok(Some(at))
    }

    /// Heap scratch for one filesystem block.
    fn block_scratch(&self) -> Result<Vec<u8>, DriverError> {
        scratch(self.layout.block_size as usize)
    }

    /// Heap scratch for one inode record.
    fn inode_scratch(&self) -> Result<Vec<u8>, DriverError> {
        scratch(self.layout.inode_size as usize)
    }
}

/// One directory record, parsed in place from its block.
struct Record {
    /// The inode it names; `0` for an unused record.
    ino: u32,
    /// Bytes to the next record.
    rec_len: usize,
    name_len: usize,
}

impl Record {
    /// The name of the child this record lists, or `None` for an unused
    /// record and for `.` and `..`, which the VFS resolves itself.
    fn child_name<'b>(&self, block: &'b [u8], at: usize) -> Option<&'b [u8]> {
        if self.ino == 0 || self.name_len == 0 || DIRENT_HEADER + self.name_len > self.rec_len {
            return None;
        }
        let name = block.get(at + DIRENT_HEADER..at + DIRENT_HEADER + self.name_len)?;
        (name != b"." && name != b"..").then_some(name)
    }
}

/// The fixed directory-entry header ahead of the name: inode, record
/// length, name length and file type.
const DIRENT_HEADER: usize = 8;

/// A named record found in a directory block: where it lies, the record
/// before it, and the inode it names.
struct Located {
    at: usize,
    prev: Option<usize>,
    ino: u32,
}

/// What a new directory entry carries.
#[derive(Copy, Clone)]
struct Child<'a> {
    name: &'a [u8],
    ino: u32,
    file_type: u8,
}

/// A walk over a directory's mapped blocks in logical order. A hole is
/// stepped over in one move and a run of blocks read from one lookup, and no
/// more blocks are visited than the volume holds, so neither a size nor a
/// tree crafted to promise more can hold the walk.
struct DirBlocks {
    next: u64,
    end: u64,
    run_phys: u64,
    run_left: u64,
    budget: u64,
}

impl DirBlocks {
    /// Directory `dir`'s blocks, from logical block `from`.
    fn of(layout: &Layout, dir: &Inode, from: u64) -> Self {
        Self {
            next: from,
            end: dir.size.div_ceil(u64::from(layout.block_size)),
            run_phys: 0,
            run_left: 0,
            budget: layout.blocks_count,
        }
    }
}

/// Refuse to change a node whose inode flags are `flags` and that `chattr`
/// protects: what the volume holds fixed holds against every principal, its
/// owner included.
fn check_changeable(flags: u32) -> Result<(), DriverError> {
    if flags & (INODE_FLAG_IMMUTABLE | INODE_FLAG_APPEND) == 0 {
        Ok(())
    } else {
        Err(DriverError::PermissionDenied)
    }
}

/// Refuse an inode whose contents are not reached through its block map.
fn check_block_mapped(inode: &Inode) -> Result<(), DriverError> {
    if inode.flags & (INODE_FLAG_INLINE_DATA | INODE_FLAG_ENCRYPT) == 0 {
        Ok(())
    } else {
        Err(DriverError::Unsupported)
    }
}

impl<B: Block> Ext4<B> {
    /// The child of directory `dir_ino` named exactly `name`: through its
    /// index when it has one this driver uses, else by a walk of its blocks.
    fn find_name(
        &mut self,
        dir_ino: u32,
        dir: &Inode,
        name: &[u8],
    ) -> Result<Option<u32>, DriverError> {
        if name == b"." || name == b".." {
            return Ok(None);
        }
        let bs = self.layout.block_size as usize;
        let mut block = self.block_scratch()?;
        let kind = self.index_use(dir_ino, dir, &mut block[..bs])?;
        if let IndexUse::Indexed(ix) = &kind {
            return self.find_indexed(dir, ix, name, &mut block[..bs]);
        }
        let (scan, checked) = (self.scan_end(&kind), self.scan_seed(dir_ino, dir, &kind));
        let mut walk = DirBlocks::of(&self.layout, dir, 0);
        while let Some((_, phys)) = self.next_dir_block(dir, &mut walk)? {
            self.read_dir_block(checked, phys, &mut block[..bs])?;
            if let Some(found) = self.locate(&block[..scan], name)? {
                return self.named_child(found.ino).map(Some);
            }
        }
        Ok(None)
    }

    /// Inode `ino` as a directory entry names it, or a device fault where no
    /// entry may: past the volume's inodes, a reserved one below the first
    /// ordinary inode — the root among them, which only `.` and `..` name —
    /// or the journal or the orphan file, which nothing names.
    fn named_child(&self, ino: u32) -> Result<u32, DriverError> {
        let named = ino >= self.layout.first_ino
            && ino <= self.layout.inodes_count
            && Some(ino) != self.layout.journal_inum
            && Some(ino) != self.layout.orphan_file_inum;
        if named {
            Ok(ino)
        } else {
            Err(DriverError::DeviceFault)
        }
    }

    /// The listing entry for child `ino`, or `None` for a device, pipe or
    /// socket, which has no node kind here and is left out of a listing.
    fn child_entry(&mut self, ino: u32, next_cursor: u64) -> Result<Option<DirEntry>, DriverError> {
        let child = self.read_inode(ino)?;
        if child.kind().is_none() {
            return Ok(None);
        }
        Ok(Some(DirEntry {
            node: NodeId::from_raw(u64::from(ino)),
            info: child.info()?,
            next_cursor,
        }))
    }

    /// Hand `visit` the children of `dir`, read linearly, from byte offset
    /// `cursor` on, its blocks checked under `seed` when it names one.
    ///
    /// A cursor is the offset of a record, and in a directory read linearly
    /// a record never moves while it is in use: removal folds it into its
    /// predecessor and insertion splits a record's slack, so an entry present
    /// throughout a listing is reached exactly once. Parsing starts at the
    /// block's first record, so a cursor that is not a record boundary can
    /// only skip, never misparse.
    fn list_from(
        &mut self,
        dir: &Inode,
        seed: Option<u32>,
        cursor: u64,
        visit: &mut dyn FnMut(&DirEntry, &[u8]) -> DirVisit,
    ) -> Result<(), DriverError> {
        let block_size = u64::from(self.layout.block_size);
        let bs = self.layout.block_size as usize;
        let mut block = self.block_scratch()?;
        let mut walk = DirBlocks::of(&self.layout, dir, cursor / block_size);
        while let Some((logical, phys)) = self.next_dir_block(dir, &mut walk)? {
            self.read_dir_block(seed, phys, &mut block[..bs])?;
            let base = logical * block_size;
            let mut pos = 0usize;
            while pos + DIRENT_HEADER <= bs {
                let record = self.record_at(&block[..bs], pos)?;
                let at = pos;
                pos += record.rec_len;
                let Some(name) = record.child_name(&block, at) else {
                    continue;
                };
                if base + (at as u64) < cursor {
                    continue;
                }
                let ino = self.named_child(record.ino)?;
                let Some(entry) = self.child_entry(ino, base + pos as u64)? else {
                    continue;
                };
                if visit(&entry, name) == DirVisit::Stop {
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    /// Usable directory-block bytes: the whole block, less the checksum tail
    /// on a `metadata_csum` volume.
    fn dir_data_end(&self) -> usize {
        self.layout.block_size as usize
            - if self.layout.metadata_csum {
                DIR_TAIL_LEN
            } else {
                0
            }
    }

    /// The checksum seed of directory `ino`'s blocks on a `metadata_csum`
    /// volume: its inode's.
    fn dir_seed(&self, ino: u32, dir: &Inode) -> Option<u32> {
        self.layout
            .metadata_csum
            .then(|| self.inode_csum_seed(ino, dir.generation))
    }

    /// Check the tail closing leaf `block` under `seed`: the empty record the
    /// format reserves for it, holding the checksum of the block before it.
    fn check_leaf(seed: u32, block: &[u8]) -> Result<(), DriverError> {
        let tail = block
            .len()
            .checked_sub(DIR_TAIL_LEN)
            .ok_or(DriverError::DeviceFault)?;
        let intact = le32(block, tail) == 0
            && usize::from(le16(block, tail + 4)) == DIR_TAIL_LEN
            && block[tail + 6] == 0
            && block[tail + 7] == DIR_TAIL_FT
            && le32(block, tail + 8) == tairix_crc32c::update(seed, &block[..tail]);
        if intact {
            Ok(())
        } else {
            Err(DriverError::DeviceFault)
        }
    }

    /// Lay the tail closing leaf `block` and stamp its checksum under `seed`.
    fn seal_leaf(seed: u32, block: &mut [u8]) -> Result<(), DriverError> {
        let tail = block
            .len()
            .checked_sub(DIR_TAIL_LEN)
            .ok_or(DriverError::DeviceFault)?;
        put_le32(block, tail, 0);
        put_le16(block, tail + 4, u16_of(DIR_TAIL_LEN)?);
        block[tail + 6] = 0;
        block[tail + 7] = DIR_TAIL_FT;
        let csum = tairix_crc32c::update(seed, &block[..tail]);
        put_le32(block, tail + 8, csum);
        Ok(())
    }

    /// Stage leaf `block` at `phys`, sealed under `seed` on a
    /// `metadata_csum` volume.
    fn write_leaf(
        &mut self,
        seed: Option<u32>,
        phys: u64,
        block: &mut [u8],
    ) -> Result<(), DriverError> {
        if let Some(seed) = seed {
            Self::seal_leaf(seed, block)?;
        }
        self.write_fs_block(phys, block)
    }

    /// Read directory block `phys`; a leaf of a linear directory has its
    /// tail checked under `seed`. An index this driver cannot use mixes index
    /// blocks among the leaves, so `seed` is `None` there.
    fn read_dir_block(
        &mut self,
        seed: Option<u32>,
        phys: u64,
        block: &mut [u8],
    ) -> Result<(), DriverError> {
        self.read_fs_block(phys, block)?;
        match seed {
            Some(seed) => Self::check_leaf(seed, block),
            None => Ok(()),
        }
    }

    /// Where a linear scan of a directory read as `kind` finds records: a
    /// leaf's data area, or the whole block where index blocks lie among the
    /// leaves.
    fn scan_end(&self, kind: &IndexUse) -> usize {
        match kind {
            IndexUse::Linear => self.dir_data_end(),
            IndexUse::Indexed(_) | IndexUse::Unusable(_) => self.layout.block_size as usize,
        }
    }

    /// The seed a linear scan of directory `ino` checks its blocks under:
    /// only a linear directory's blocks are all leaves.
    fn scan_seed(&self, ino: u32, dir: &Inode, kind: &IndexUse) -> Option<u32> {
        match kind {
            IndexUse::Linear => self.dir_seed(ino, dir),
            IndexUse::Indexed(_) | IndexUse::Unusable(_) => None,
        }
    }

    /// The record naming `name` in `block`: where it is, the record before
    /// it, and the inode it names.
    fn locate(&self, block: &[u8], name: &[u8]) -> Result<Option<Located>, DriverError> {
        let (mut pos, mut prev) = (0usize, None);
        while pos + DIRENT_HEADER <= block.len() {
            let record = self.record_at(block, pos)?;
            if record.ino != 0
                && record.name_len > 0
                && DIRENT_HEADER + record.name_len <= record.rec_len
                && block.get(pos + DIRENT_HEADER..pos + DIRENT_HEADER + record.name_len)
                    == Some(name)
            {
                return Ok(Some(Located {
                    at: pos,
                    prev,
                    ino: record.ino,
                }));
            }
            prev = Some(pos);
            pos += record.rec_len;
        }
        Ok(None)
    }

    /// Drop record `found` from `block`: folded into the record before it,
    /// or freed in place when it is the first.
    fn unlink_record(&self, block: &mut [u8], found: &Located) -> Result<(), DriverError> {
        match found.prev {
            Some(prev) => {
                let merged =
                    self.record_at(block, prev)?.rec_len + self.record_at(block, found.at)?.rec_len;
                put_le16(block, prev + 4, u16_of(merged)?);
            }
            None => put_le32(block, found.at, 0),
        }
        Ok(())
    }

    /// Write an entry header and name at `pos` of `block`, in the layout the
    /// `filetype` feature selects.
    fn write_dirent(
        &self,
        block: &mut [u8],
        pos: usize,
        ino: u32,
        rec_len: u16,
        name: &[u8],
        file_type: u8,
    ) -> Result<(), DriverError> {
        put_le32(block, pos, ino);
        put_le16(block, pos + 4, rec_len);
        if self.layout.filetype {
            block[pos + 6] = u8::try_from(name.len()).map_err(|_| DriverError::LengthOutOfRange)?;
            block[pos + 7] = file_type;
        } else {
            put_le16(block, pos + 6, u16_of(name.len())?);
        }
        block[pos + DIRENT_HEADER..pos + DIRENT_HEADER + name.len()].copy_from_slice(name);
        Ok(())
    }

    /// Read the record at `pos`, its length validated against the block.
    fn record_at(&self, block: &[u8], pos: usize) -> Result<Record, DriverError> {
        let ino = le32(block, pos);
        let rec_len = usize::from(le16(block, pos + 4));
        if rec_len < DIRENT_HEADER || rec_len % 4 != 0 || pos + rec_len > block.len() {
            return Err(DriverError::DeviceFault);
        }
        let name_len = if self.layout.filetype {
            usize::from(block[pos + 6])
        } else {
            usize::from(le16(block, pos + 6))
        };
        Ok(Record {
            ino,
            rec_len,
            name_len,
        })
    }

    /// Place a `needed`-byte entry into `block`, reusing an unused slot or
    /// splitting one with slack. Answers whether it fit.
    fn place_in_block(
        &self,
        block: &mut [u8],
        needed: usize,
        ino: u32,
        name: &[u8],
        file_type: u8,
    ) -> Result<bool, DriverError> {
        let bs = block.len();
        let mut pos = 0usize;
        while pos + DIRENT_HEADER <= bs {
            let Record {
                ino: slot_ino,
                rec_len,
                name_len,
            } = self.record_at(block, pos)?;
            let used = if slot_ino == 0 {
                0
            } else {
                align4(DIRENT_HEADER + name_len)
            };
            if rec_len >= used + needed {
                if slot_ino == 0 {
                    self.write_dirent(block, pos, ino, u16_of(rec_len)?, name, file_type)?;
                } else {
                    put_le16(block, pos + 4, u16_of(used)?);
                    let np = pos + used;
                    self.write_dirent(block, np, ino, u16_of(rec_len - used)?, name, file_type)?;
                }
                return Ok(true);
            }
            pos += rec_len;
        }
        Ok(false)
    }

    /// Add `child` to directory `dir_ino`, refused for a directory whose
    /// index this driver cannot use.
    fn insert_child(&mut self, dir_ino: u32, child: Child<'_>) -> Result<(), DriverError> {
        let dir = self.read_inode(dir_ino)?;
        if !self.layout.dir_index && dir.flags & INODE_FLAG_INDEX != 0 {
            // Without `dir_index` a flagged directory is read linearly and a
            // linear insert may land where the index was, so the stale flag
            // goes rather than be trusted later.
            let mut raw = self.inode_scratch()?;
            self.read_inode_raw(dir_ino, &mut raw)?;
            let flags = le32(&raw, INODE_FLAGS) & !INODE_FLAG_INDEX;
            put_le32(&mut raw, INODE_FLAGS, flags);
            self.write_inode_raw(dir_ino, &mut raw)?;
        }
        let bs = self.layout.block_size as usize;
        let mut index = self.block_scratch()?;
        match self.index_use(dir_ino, &dir, &mut index[..bs])? {
            IndexUse::Linear => self.insert_linear(dir_ino, &dir, child),
            IndexUse::Indexed(ix) => {
                self.insert_indexed(dir_ino, &dir, ix, child, &mut index[..bs])
            }
            IndexUse::Unusable(err) => Err(err),
        }
    }

    /// Refuse, before anything changes, a directory no name can be added to.
    fn check_insertable(&mut self, dir_ino: u32, dir: &Inode) -> Result<(), DriverError> {
        if dir.flags & INODE_FLAG_IMMUTABLE != 0 {
            return Err(DriverError::PermissionDenied);
        }
        let bs = self.layout.block_size as usize;
        let mut index = self.block_scratch()?;
        match self.index_use(dir_ino, dir, &mut index[..bs])? {
            IndexUse::Unusable(err) => Err(err),
            IndexUse::Linear | IndexUse::Indexed(_) => Ok(()),
        }
    }

    /// Add `child` to linear directory `dir_ino`: into the first block with
    /// room, else a block appended for it. Records never move, which a linear
    /// listing's cursor relies on.
    fn insert_linear(
        &mut self,
        dir_ino: u32,
        dir: &Inode,
        child: Child<'_>,
    ) -> Result<(), DriverError> {
        let bs = self.layout.block_size as usize;
        let end = self.dir_data_end();
        let needed = align4(DIRENT_HEADER + child.name.len());
        let seed = self.dir_seed(dir_ino, dir);
        let mut blocks = dir.size.div_ceil(u64::from(self.layout.block_size));
        let mut block = self.block_scratch()?;
        let mut walk = DirBlocks::of(&self.layout, dir, 0);
        while let Some((_, phys)) = self.next_dir_block(dir, &mut walk)? {
            self.read_dir_block(seed, phys, &mut block[..bs])?;
            if self.place_in_block(
                &mut block[..end],
                needed,
                child.ino,
                child.name,
                child.file_type,
            )? {
                return self.write_leaf(seed, phys, &mut block[..bs]);
            }
        }
        if blocks == 1 && self.can_index(dir) {
            return self.index_directory(dir_ino, dir, child);
        }
        block.fill(0);
        self.write_dirent(
            &mut block,
            0,
            child.ino,
            u16_of(end)?,
            child.name,
            child.file_type,
        )?;
        if let Some(seed) = seed {
            Self::seal_leaf(seed, &mut block[..bs])?;
        }
        self.append_dir_block(dir_ino, &mut blocks, &block[..bs])
            .map(drop)
    }

    /// Append `block` to directory `dir_ino` as logical block `blocks`, which
    /// then counts it. Answers where it lives.
    fn append_dir_block(
        &mut self,
        dir_ino: u32,
        blocks: &mut u64,
        block: &[u8],
    ) -> Result<u64, DriverError> {
        // A directory's size is 32 bits wide without `largedir`.
        let size = (*blocks + 1)
            .checked_mul(u64::from(self.layout.block_size))
            .filter(|&size| self.layout.largedir || size >> 32 == 0)
            .ok_or(DriverError::NoSpace)?;
        let mut raw = self.inode_scratch()?;
        self.read_inode_raw(dir_ino, &mut raw)?;
        // The block past a directory's end is a hole; one already mapped
        // there is damage, never storage to write over.
        if !matches!(
            self.map_lookup(dir_ino, &raw, *blocks)?,
            Mapping::Hole { .. }
        ) {
            return Err(DriverError::DeviceFault);
        }
        let got = self.map_alloc(dir_ino, &mut raw, *blocks, 1, Some(1))?;
        self.write_fs_block(got.phys, block)?;
        file::set_size(&mut raw, size);
        let added = 1 + i64::try_from(got.nodes).map_err(|_| DriverError::DeviceFault)?;
        file::adjust_blocks(&mut raw, &self.layout, added)?;
        self.write_inode_raw(dir_ino, &mut raw)?;
        *blocks += 1;
        Ok(got.phys)
    }

    /// Remove the entry named `name` from directory `dir_ino`, answering the
    /// inode it named.
    fn remove_dirent(&mut self, dir_ino: u32, name: &[u8]) -> Result<u32, DriverError> {
        let dir = self.read_inode(dir_ino)?;
        let bs = self.layout.block_size as usize;
        let mut block = self.block_scratch()?;
        let kind = self.index_use(dir_ino, &dir, &mut block[..bs])?;
        if let IndexUse::Indexed(ix) = &kind {
            return self.remove_indexed(&dir, ix, name, &mut block[..bs]);
        }
        let end = self.dir_data_end();
        let seed = self.dir_seed(dir_ino, &dir);
        let (scan, checked) = (self.scan_end(&kind), self.scan_seed(dir_ino, &dir, &kind));
        let mut walk = DirBlocks::of(&self.layout, &dir, 0);
        while let Some((_, phys)) = self.next_dir_block(&dir, &mut walk)? {
            self.read_dir_block(checked, phys, &mut block[..bs])?;
            let Some(found) = self.locate(&block[..scan], name)? else {
                continue;
            };
            let found = if scan == end {
                found
            } else {
                // Among index blocks, the one naming a child is a leaf:
                // checked, and parsed, as one before it changes.
                if let Some(seed) = seed {
                    Self::check_leaf(seed, &block[..bs])?;
                }
                self.locate(&block[..end], name)?
                    .ok_or(DriverError::DeviceFault)?
            };
            self.unlink_record(&mut block[..bs], &found)?;
            self.write_leaf(seed, phys, &mut block[..bs])?;
            return Ok(found.ino);
        }
        Err(DriverError::NotFound)
    }

    /// Whether directory `dir_ino` names no child.
    fn dir_is_empty(&mut self, dir_ino: u32) -> Result<bool, DriverError> {
        let dir = self.read_inode(dir_ino)?;
        let bs = self.layout.block_size as usize;
        let mut block = self.block_scratch()?;
        let kind = self.index_use(dir_ino, &dir, &mut block[..bs])?;
        if let IndexUse::Indexed(ix) = &kind {
            return self.indexed_is_empty(&dir, ix, &mut block[..bs]);
        }
        let (scan, checked) = (self.scan_end(&kind), self.scan_seed(dir_ino, &dir, &kind));
        let mut walk = DirBlocks::of(&self.layout, &dir, 0);
        while let Some((_, phys)) = self.next_dir_block(&dir, &mut walk)? {
            self.read_dir_block(checked, phys, &mut block[..bs])?;
            let mut pos = 0usize;
            while pos + DIRENT_HEADER <= scan {
                let record = self.record_at(&block[..scan], pos)?;
                if record.child_name(&block[..scan], pos).is_some() {
                    return Ok(false);
                }
                pos += record.rec_len;
            }
        }
        Ok(true)
    }

    /// Resolve a child of directory `dir_ino` by name.
    fn lookup_child(&mut self, dir_ino: u32, name: &[u8]) -> Result<u32, DriverError> {
        let dir = self.read_inode(dir_ino)?;
        if dir.kind() != Some(NodeKind::Directory) {
            return Err(DriverError::Unsupported);
        }
        self.find_name(dir_ino, &dir, name)?
            .ok_or(DriverError::NotFound)
    }

    /// Record that directory `dir_ino`'s entries changed at `now`, adding
    /// `link_delta` for the child directories, each a `..` back-link, that
    /// moved in or out. With `dir_nlink` a count past the format's limit
    /// becomes `1`, "too many to count", and stays there.
    fn update_dir(
        &mut self,
        dir_ino: u32,
        link_delta: i16,
        now: Option<Time64>,
    ) -> Result<(), DriverError> {
        let mut raw = self.inode_scratch()?;
        self.read_inode_raw(dir_ino, &mut raw)?;
        let links = le16(&raw, INODE_LINKS);
        let magnitude = link_delta.unsigned_abs();
        let limit = if self.layout.dir_nlink {
            DIR_LINK_MAX
        } else {
            DIR_LINK_MAX_COUNTED
        };
        let new = match link_delta.signum() {
            0 => links,
            _ if links == 1 && self.layout.dir_nlink => 1,
            1 => match links.checked_add(magnitude).filter(|&n| n <= limit) {
                Some(n) => n,
                None if self.layout.dir_nlink => 1,
                None => return Err(DriverError::TooManyLinks),
            },
            _ => links.saturating_sub(magnitude).max(2),
        };
        put_le16(&mut raw, INODE_LINKS, new);
        put_stamps(&mut raw, now, &[Stamp::Modify, Stamp::Change]);
        self.write_inode_raw(dir_ino, &mut raw)
    }

    /// Drop one name from inode `ino` (record `raw`), deleting it when that
    /// was the last. Linux makes hard links freely, so an unlink frees only
    /// at zero; a directory cannot carry a second name, so it goes outright.
    fn drop_one_name(
        &mut self,
        ino: u32,
        raw: &mut [u8],
        is_dir: bool,
        now: Option<Time64>,
    ) -> Result<(), DriverError> {
        let links = le16(raw, INODE_LINKS);
        let remaining = if is_dir { 0 } else { links.saturating_sub(1) };
        if remaining > 0 {
            put_le16(raw, INODE_LINKS, remaining);
            put_stamps(raw, now, &[Stamp::Change]);
            return self.write_inode_raw(ino, raw);
        }
        self.release_inode(ino, raw, false)
    }

    /// The `i_dtime` a deleted inode is stamped with: the time it went, where
    /// the field holds it and it cannot be read as an inode number.
    fn deletion_time(&self, now: Option<Time64>) -> u32 {
        let inodes = u64::from(self.layout.inodes_count);
        now.and_then(|time| u32::try_from(time.secs()).ok())
            .filter(|&secs| u64::from(secs) > inodes)
            .unwrap_or(DELETED_DTIME)
    }

    /// Directory `dir_ino`'s block 0, read into `block` and checked as what
    /// it is — an index root or a leaf — with where it lives and how the
    /// directory is read.
    fn first_block(
        &mut self,
        dir_ino: u32,
        dir: &Inode,
        block: &mut [u8],
    ) -> Result<(u64, IndexUse), DriverError> {
        let kind = self.index_use(dir_ino, dir, block)?;
        let phys = self.map_block(dir, 0)?.ok_or(DriverError::DeviceFault)?;
        if let IndexUse::Linear = kind {
            self.read_dir_block(self.dir_seed(dir_ino, dir), phys, block)?;
        }
        Ok((phys, kind))
    }

    /// Where the `..` record lies in a directory's block 0, and the parent it
    /// names. Every directory puts `.` first and `..` second.
    fn dot_dot(&self, block: &[u8]) -> Result<(usize, u32), DriverError> {
        let named = |at: usize, want: &[u8]| -> Result<Record, DriverError> {
            let record = self.record_at(block, at)?;
            let fits = record.ino != 0 && DIRENT_HEADER + record.name_len <= record.rec_len;
            if fits
                && block.get(at + DIRENT_HEADER..at + DIRENT_HEADER + record.name_len) == Some(want)
            {
                Ok(record)
            } else {
                Err(DriverError::DeviceFault)
            }
        };
        let at = named(0, b".")?.rec_len;
        Ok((at, named(at, b"..")?.ino))
    }

    /// The parent of directory `dir_ino`, from its `..` record.
    fn dir_parent_ino(&mut self, dir_ino: u32) -> Result<u32, DriverError> {
        let dir = self.read_inode(dir_ino)?;
        let bs = self.layout.block_size as usize;
        let mut block = self.block_scratch()?;
        self.first_block(dir_ino, &dir, &mut block[..bs])?;
        Ok(self.dot_dot(&block[..bs])?.1)
    }

    /// Point directory `dir_ino`'s `..` at `parent`, resealing block 0 as the
    /// root or leaf it is.
    fn set_parent(&mut self, dir_ino: u32, parent: u32) -> Result<(), DriverError> {
        let dir = self.read_inode(dir_ino)?;
        let bs = self.layout.block_size as usize;
        let mut block = self.block_scratch()?;
        let (phys, kind) = self.first_block(dir_ino, &dir, &mut block[..bs])?;
        let (at, _) = self.dot_dot(&block[..bs])?;
        put_le32(&mut block, at, parent);
        match kind {
            IndexUse::Linear => {
                self.write_leaf(self.dir_seed(dir_ino, &dir), phys, &mut block[..bs])
            }
            IndexUse::Indexed(ix) => self.rewrite_root(&ix, &mut block[..bs]),
            IndexUse::Unusable(err) => Err(err),
        }
    }

    /// Whether directory `candidate` is `ancestor` or lies beneath it, by its
    /// `..` links. A chain that loops on a damaged volume is found within
    /// steps proportional to its own length (Brent's cycle detection), never
    /// followed for ever.
    fn is_subdir_of(&mut self, mut candidate: u32, ancestor: u32) -> Result<bool, DriverError> {
        let (mut saved, mut steps, mut stride) = (candidate, 0u32, 1u32);
        loop {
            if candidate == ancestor {
                return Ok(true);
            }
            if candidate == ROOT_INODE {
                return Ok(false);
            }
            let parent = self.dir_parent_ino(candidate)?;
            if parent == candidate {
                return Ok(false);
            }
            candidate = parent;
            if candidate == saved {
                return Err(DriverError::DeviceFault);
            }
            steps += 1;
            if steps == stride {
                (saved, steps, stride) = (candidate, 0, stride.saturating_mul(2));
            }
        }
    }

    /// [`FilesystemWrite::rename`] as one operation: the moved node keeps its
    /// inode, a replaced destination loses its name and, with it, its last
    /// link, and a directory moved across parents is repointed.
    fn rename_inner(
        &mut self,
        src_dir: NodeId,
        src_name: &[u8],
        dst_dir: NodeId,
        dst_name: &[u8],
    ) -> Result<(), DriverError> {
        validate_name(dst_name)?;
        let src_dir_ino = node_inode(src_dir)?;
        let dst_dir_ino = node_inode(dst_dir)?;
        let src_dir_inode = self.read_inode(src_dir_ino)?;
        let dst_dir_inode = self.read_inode(dst_dir_ino)?;
        if src_dir_inode.kind() != Some(NodeKind::Directory)
            || dst_dir_inode.kind() != Some(NodeKind::Directory)
        {
            return Err(DriverError::Unsupported);
        }
        let dst_dir_flags = dst_dir_inode.flags;
        self.check_insertable(dst_dir_ino, &dst_dir_inode)?;
        if src_dir_ino == dst_dir_ino && src_name == dst_name {
            return Ok(());
        }
        check_changeable(src_dir_inode.flags)?;
        let src_ino = self.lookup_child(src_dir_ino, src_name)?;
        let mut src_raw = self.inode_scratch()?;
        self.read_inode_raw(src_ino, &mut src_raw)?;
        check_changeable(le32(&src_raw, INODE_FLAGS))?;
        let src_mode = le16(&src_raw, 0);
        let moving_dir = src_mode & S_IFMT == S_IFDIR;
        let reparenting = moving_dir && src_dir_ino != dst_dir_ino;
        if moving_dir && self.is_subdir_of(dst_dir_ino, src_ino)? {
            return Err(DriverError::DirectoryCycle);
        }
        if reparenting {
            let moved = self.read_inode(src_ino)?;
            self.check_insertable(src_ino, &moved)?;
        }
        let dst_existing = match self.lookup_child(dst_dir_ino, dst_name) {
            Ok(ino) => Some(ino),
            Err(DriverError::NotFound) => None,
            Err(e) => return Err(e),
        };
        let now = self.now();
        let mut replaced = None;
        if let Some(dst_ino) = dst_existing {
            if dst_ino == src_ino {
                return Ok(());
            }
            let mut dst_raw = self.inode_scratch()?;
            self.read_inode_raw(dst_ino, &mut dst_raw)?;
            // Replacing a name removes the node it named.
            check_changeable(dst_dir_flags)?;
            check_changeable(le32(&dst_raw, INODE_FLAGS))?;
            let dst_is_dir = le16(&dst_raw, 0) & S_IFMT == S_IFDIR;
            if dst_is_dir != moving_dir {
                return Err(DriverError::Unsupported);
            }
            if dst_is_dir && !self.dir_is_empty(dst_ino)? {
                return Err(DriverError::DirectoryNotEmpty);
            }
            self.remove_dirent(dst_dir_ino, dst_name)?;
            replaced = Some((dst_ino, dst_raw, dst_is_dir));
        }
        put_stamps(&mut src_raw, now, &[Stamp::Change]);
        self.write_inode_raw(src_ino, &mut src_raw)?;
        self.insert_child(
            dst_dir_ino,
            Child {
                name: dst_name,
                ino: src_ino,
                file_type: dirent_file_type(src_mode),
            },
        )?;
        self.remove_dirent(src_dir_ino, src_name)?;
        if reparenting {
            self.set_parent(src_ino, dst_dir_ino)?;
        }
        let moved_link = i16::from(reparenting);
        self.update_dir(src_dir_ino, -moved_link, now)?;
        if dst_dir_ino != src_dir_ino {
            self.update_dir(dst_dir_ino, moved_link, now)?;
        }
        // Only once the rename is whole is the node it replaced let go: a
        // large one is let go across transactions, and a crash between them
        // must find the new name in place, the replaced node on the orphan
        // chain to be finished at mount.
        if let Some((dst_ino, mut dst_raw, dst_is_dir)) = replaced {
            if dst_is_dir {
                self.update_dir(dst_dir_ino, -1, now)?;
            }
            self.drop_one_name(dst_ino, &mut dst_raw, dst_is_dir, now)?;
        }
        Ok(())
    }

    /// Read file or link `inode`'s contents from byte `offset` into `buf`.
    fn read_contents(
        &mut self,
        inode: &Inode,
        offset: u64,
        buf: &mut [u8],
    ) -> Result<usize, DriverError> {
        if inode.flags & INODE_FLAG_ENCRYPT != 0 {
            return Err(DriverError::Unsupported);
        }
        if inode.flags & INODE_FLAG_INLINE_DATA != 0 {
            // Inline contents on a volume that never enabled them are damage.
            if !self.layout.inline_data {
                return Err(DriverError::DeviceFault);
            }
            let mut raw = self.inode_scratch()?;
            self.read_inode_raw(inode.ino, &mut raw)?;
            return xattr::read_inline(&raw, inode.size, offset, buf);
        }
        self.read_file(inode.ino, &inode.head, inode.size, offset, buf)
    }

    /// Where the next region of `region`'s kind begins at or after `offset`
    /// of `inode`, a mapped run at a time. An unwritten extent reads as zeros
    /// and holds no data, so it is a hole; inline contents are data
    /// throughout.
    fn region_after(
        &mut self,
        inode: &Inode,
        offset: u64,
        region: SeekRegion,
    ) -> Result<Option<u64>, DriverError> {
        if inode.flags & INODE_FLAG_ENCRYPT != 0 {
            return Err(DriverError::Unsupported);
        }
        if inode.flags & INODE_FLAG_INLINE_DATA != 0 {
            return Ok(region.in_solid_file(offset, inode.size));
        }
        if offset >= inode.size {
            return Ok(None);
        }
        let bs = u64::from(self.layout.block_size);
        let last = (inode.size - 1) / bs;
        let mut block = offset / bs;
        while block <= last {
            let (data, run) = match self.map_lookup(inode.ino, &inode.head, block)? {
                Mapping::Mapped {
                    len,
                    unwritten: false,
                    ..
                } => (true, Some(len)),
                Mapping::Mapped { len, .. } => (false, Some(len)),
                Mapping::Hole { len } => (false, len),
            };
            if data == (region == SeekRegion::Data) {
                return Ok(Some((block * bs).max(offset)));
            }
            match run {
                Some(len) if len > 0 => block = block.saturating_add(len),
                _ => break,
            }
        }
        Ok(match region {
            SeekRegion::Data => None,
            SeekRegion::Hole => Some(inode.size),
        })
    }
}

impl<B: Block> FilesystemRead for Ext4<B> {
    fn root(&self) -> NodeId {
        NodeId::from_raw(u64::from(ROOT_INODE))
    }

    fn name_matching(&self) -> NameMatching {
        NameMatching::Exact
    }

    fn node_identity(&self) -> NodeIdentity {
        NodeIdentity::Stable
    }

    fn node_info(&mut self, node: NodeId) -> Result<NodeInfo, DriverError> {
        let inode = self.read_inode(node_inode(node)?)?;
        inode.info()
    }

    fn lookup(&mut self, dir: NodeId, name: &[u8]) -> Result<NodeId, DriverError> {
        let ino = self.lookup_child(node_inode(dir)?, name)?;
        Ok(NodeId::from_raw(u64::from(ino)))
    }

    fn read_at(&mut self, file: NodeId, offset: u64, buf: &mut [u8]) -> Result<usize, DriverError> {
        let inode = self.read_inode(node_inode(file)?)?;
        match inode.kind() {
            Some(NodeKind::RegularFile) => self.read_contents(&inode, offset, buf),
            Some(NodeKind::Directory | NodeKind::Symlink) => Err(DriverError::Unsupported),
            None => Err(DriverError::NotFound),
        }
    }

    fn seek_region(
        &mut self,
        file: NodeId,
        offset: u64,
        region: SeekRegion,
    ) -> Result<Option<u64>, DriverError> {
        let ino = node_inode(file)?;
        let inode = self.read_inode(ino)?;
        if inode.kind() != Some(NodeKind::RegularFile) {
            return Err(DriverError::Unsupported);
        }
        self.region_after(&inode, offset, region)
    }

    fn read_link(&mut self, link: NodeId, out: &mut [u8]) -> Result<usize, DriverError> {
        let inode = self.read_inode(node_inode(link)?)?;
        if inode.kind() != Some(NodeKind::Symlink) {
            return Err(DriverError::Unsupported);
        }
        let len = usize::try_from(inode.size).map_err(|_| DriverError::DeviceFault)?;
        if len == 0 || len > FS_SYMLINK_MAX {
            return Err(DriverError::DeviceFault);
        }
        if out.len() < len {
            return Err(DriverError::BufferTooSmall);
        }
        if file::is_fast_symlink(&inode.head, &self.layout) {
            let inline = inode
                .head
                .get(I_BLOCK_OFFSET..I_BLOCK_OFFSET + len)
                .ok_or(DriverError::DeviceFault)?;
            out[..len].copy_from_slice(inline);
            return Ok(len);
        }
        if self.read_contents(&inode, 0, &mut out[..len])? != len {
            return Err(DriverError::DeviceFault);
        }
        Ok(len)
    }

    fn read_dir(
        &mut self,
        dir: NodeId,
        cursor: u64,
        after: &[u8],
        visit: &mut dyn FnMut(&DirEntry, &[u8]) -> DirVisit,
    ) -> Result<(), DriverError> {
        let ino = node_inode(dir)?;
        let inode = self.read_inode(ino)?;
        if inode.kind() != Some(NodeKind::Directory) {
            return Err(DriverError::Unsupported);
        }
        let bs = self.layout.block_size as usize;
        let mut index = self.block_scratch()?;
        match self.index_use(ino, &inode, &mut index[..bs])? {
            IndexUse::Indexed(ix) => self.list_indexed(&inode, &ix, after, &mut index[..bs], visit),
            kind => {
                let seed = self.scan_seed(ino, &inode, &kind);
                self.list_from(&inode, seed, cursor, visit)
            }
        }
    }
}

/// The owning group's ACL entry, which a mode change never touches.
fn group_obj(sec: &NodeSecurity) -> Option<u8> {
    match sec.acl_model {
        AclModel::Additive => None,
        AclModel::Posix { group_obj, .. } => Some(group_obj),
    }
}

impl<B: Block> FilesystemSecurity for Ext4<B> {
    fn security(&mut self, node: NodeId) -> Result<NodeSecurity, DriverError> {
        let ino = node_inode(node)?;
        let mut raw = self.inode_scratch()?;
        self.read_inode_raw(ino, &mut raw)?;
        let mode = le16(&raw, 0);
        if kind_of(mode).is_none() {
            return Err(DriverError::NotFound);
        }
        let mut sec = NodeSecurity::new(
            u32::from(mode) & 0o7777,
            split_id(&raw, INODE_UID_LO, INODE_UID_HI),
            split_id(&raw, INODE_GID_LO, INODE_GID_HI),
        );
        self.decode_acl(&raw, &mut sec)?;
        Ok(sec)
    }

    /// Store `security`'s mode and owner. The format has no place for a
    /// capability gate, and this driver writes ACL entries only as a mode
    /// change moves them, so a record asking for either is refused whole
    /// rather than stored in part.
    fn set_security(&mut self, node: NodeId, security: NodeSecurity) -> Result<(), DriverError> {
        let ino = node_inode(node)?;
        self.operation(|fs| {
            let current = fs.security(node)?;
            if security.required_cap.is_some()
                || security.acl() != current.acl()
                || group_obj(&security) != group_obj(&current)
            {
                return Err(DriverError::Unsupported);
            }
            let mode = security.mode & 0o7777;
            let mut raw = fs.inode_scratch()?;
            fs.read_inode_raw(ino, &mut raw)?;
            check_changeable(le32(&raw, INODE_FLAGS))?;
            let typed = (le16(&raw, 0) & S_IFMT)
                | u16::try_from(mode).map_err(|_| DriverError::OutOfRange)?;
            put_le16(&mut raw, 0, typed);
            put_split_id(&mut raw, INODE_UID_LO, INODE_UID_HI, security.uid);
            put_split_id(&mut raw, INODE_GID_LO, INODE_GID_HI, security.gid);
            fs.mirror_acl(&mut raw, mode)?;
            put_stamps(&mut raw, fs.now(), &[Stamp::Change]);
            fs.write_inode_raw(ino, &mut raw)
        })
    }

    /// Joins the transaction the node's creation opened, so a crash never
    /// leaves the new node with its default owner.
    fn stamp_owner(&mut self, node: NodeId, uid: u32, gid: u32) -> Result<(), DriverError> {
        let ino = node_inode(node)?;
        self.continuation(|fs| {
            let mut raw = fs.inode_scratch()?;
            fs.read_inode_raw(ino, &mut raw)?;
            if kind_of(le16(&raw, 0)).is_none() {
                return Err(DriverError::NotFound);
            }
            put_split_id(&mut raw, INODE_UID_LO, INODE_UID_HI, uid);
            put_split_id(&mut raw, INODE_GID_LO, INODE_GID_HI, gid);
            fs.write_inode_raw(ino, &mut raw)
        })
    }
}

/// The format has nowhere to keep TAIRiX's own attributes, so the facet
/// stays absent and the `fs_attr_*` surface answers the unsupported-backing
/// error.
impl<B: Block> FilesystemAttrsProvider for Ext4<B> {}

impl<B: Block> FilesystemStats for Ext4<B> {
    fn stats(&mut self) -> Result<VolumeStats, DriverError> {
        let free_blocks = self.free_blocks_count();
        let reserved = superblock::reserved_blocks(&self.sb, self.layout.wide_desc());
        Ok(VolumeStats {
            block_size: self.layout.block_size,
            in_reserve: self.in_reserve,
            total_blocks: self.layout.blocks_count,
            free_blocks,
            // Blocks reserved for the system are free but not available to
            // an ordinary allocation, the POSIX `f_bavail` distinction.
            avail_blocks: free_blocks.saturating_sub(reserved),
            files: u64::from(self.layout.inodes_count),
            files_free: u64::from(le32(&self.sb, SB_FREE_INODES)),
        })
    }
}

/// Validate a path component the write surface will store.
fn validate_name(name: &[u8]) -> Result<(), DriverError> {
    if name.is_empty() || name.len() > MAX_NAME_LEN {
        return Err(DriverError::LengthOutOfRange);
    }
    if name == b"." || name == b".." || name.iter().any(|&b| b == b'/' || b == 0) {
        return Err(DriverError::LengthOutOfRange);
    }
    Ok(())
}

impl<B: Block> Ext4<B> {
    /// Read a regular file's record for writing, refusing anything else and
    /// a file whose contents this driver does not write.
    fn open_regular_for_write(&mut self, ino: u32) -> Result<Vec<u8>, DriverError> {
        let mut raw = self.inode_scratch()?;
        self.read_inode_raw(ino, &mut raw)?;
        if le16(&raw, 0) & S_IFMT != S_IFREG
            || le32(&raw, INODE_FLAGS) & (INODE_FLAG_INLINE_DATA | INODE_FLAG_ENCRYPT) != 0
        {
            return Err(DriverError::Unsupported);
        }
        Ok(raw)
    }

    /// Lay down new inode `ino` of `mode` in `raw`, a child of `parent`: a
    /// fresh generation, extent-mapped where the volume has extents, and a
    /// directory's first block holding `.` and `..`.
    fn lay_new_inode(
        &mut self,
        parent: u32,
        ino: u32,
        mode: u16,
        raw: &mut [u8],
        now: Option<Time64>,
    ) -> Result<(), DriverError> {
        let is_dir = mode & S_IFMT == S_IFDIR;
        raw.fill(0);
        put_le16(raw, 0, mode);
        put_le16(raw, INODE_LINKS, if is_dir { 2 } else { 1 });
        if self.layout.inode_size > 128 {
            put_le16(raw, inode::EXTRA_ISIZE, NEW_EXTRA_ISIZE);
        }
        put_stamps(raw, now, &Stamp::ALL);
        self.new_generation(raw);
        if self.layout.extents {
            put_le32(raw, INODE_FLAGS, INODE_FLAG_EXTENTS);
            extent::init_root(&mut raw[I_BLOCK_OFFSET..I_BLOCK_OFFSET + I_BLOCK_LEN])?;
        }
        if is_dir {
            let bs = self.layout.block_size as usize;
            let got = self.map_alloc(ino, raw, 0, 1, Some(1))?;
            let end = self.dir_data_end();
            let mut first = self.block_scratch()?;
            self.write_dirent(&mut first, 0, ino, 12, b".", FT_DIR)?;
            self.write_dirent(&mut first, 12, parent, u16_of(end - 12)?, b"..", FT_DIR)?;
            let seed = self
                .layout
                .metadata_csum
                .then(|| self.inode_csum_seed(ino, le32(raw, inode::GENERATION)));
            self.write_leaf(seed, got.phys, &mut first[..bs])?;
            file::set_size(raw, u64::from(self.layout.block_size));
            let added = 1 + i64::try_from(got.nodes).map_err(|_| DriverError::DeviceFault)?;
            file::adjust_blocks(raw, &self.layout, added)?;
        }
        self.write_inode_raw(ino, raw)
    }
}

impl<B: Block> FilesystemWrite for Ext4<B> {
    fn create(&mut self, dir: NodeId, name: &[u8], kind: NodeKind) -> Result<NodeId, DriverError> {
        if kind == NodeKind::Symlink {
            return Err(DriverError::Unsupported);
        }
        validate_name(name)?;
        let dir_ino = node_inode(dir)?;
        self.operation(|fs| {
            let dir_inode = fs.read_inode(dir_ino)?;
            if dir_inode.kind() != Some(NodeKind::Directory) {
                return Err(DriverError::Unsupported);
            }
            fs.check_insertable(dir_ino, &dir_inode)?;
            if fs.find_name(dir_ino, &dir_inode, name)?.is_some() {
                return Err(DriverError::AlreadyExists);
            }
            let is_dir = kind == NodeKind::Directory;
            let mode = if is_dir { NEW_DIR_MODE } else { NEW_FILE_MODE };
            let now = fs.now();
            let new_ino = fs.alloc_inode(dir_ino, is_dir)?;
            let mut raw = fs.inode_scratch()?;
            fs.lay_new_inode(dir_ino, new_ino, mode, &mut raw, now)?;
            fs.insert_child(
                dir_ino,
                Child {
                    name,
                    ino: new_ino,
                    file_type: dirent_file_type(mode),
                },
            )?;
            fs.update_dir(dir_ino, i16::from(is_dir), now)?;
            Ok(NodeId::from_raw(u64::from(new_ino)))
        })
    }

    fn write_at(
        &mut self,
        dir: NodeId,
        name: &[u8],
        offset: u64,
        data: &[u8],
    ) -> Result<usize, DriverError> {
        let dir_ino = node_inode(dir)?;
        self.operation(|fs| {
            let child = fs.lookup_child(dir_ino, name)?;
            let mut raw = fs.open_regular_for_write(child)?;
            let flags = le32(&raw, INODE_FLAGS);
            let appending = offset == file::inode_size_bytes(&raw, &fs.layout);
            if flags & INODE_FLAG_IMMUTABLE != 0 || (flags & INODE_FLAG_APPEND != 0 && !appending) {
                return Err(DriverError::PermissionDenied);
            }
            if data.is_empty() {
                return Ok(0);
            }
            fs.write_file(child, &mut raw, offset, data)
        })
    }

    fn truncate(&mut self, dir: NodeId, name: &[u8], size: u64) -> Result<(), DriverError> {
        let dir_ino = node_inode(dir)?;
        self.operation(|fs| {
            let child = fs.lookup_child(dir_ino, name)?;
            let mut raw = fs.open_regular_for_write(child)?;
            check_changeable(le32(&raw, INODE_FLAGS))?;
            fs.truncate_file(child, &mut raw, size)
        })
    }

    fn remove(&mut self, dir: NodeId, name: &[u8]) -> Result<(), DriverError> {
        let dir_ino = node_inode(dir)?;
        self.operation(|fs| {
            check_changeable(fs.read_inode(dir_ino)?.flags)?;
            let child = fs.lookup_child(dir_ino, name)?;
            let mut raw = fs.inode_scratch()?;
            fs.read_inode_raw(child, &mut raw)?;
            check_changeable(le32(&raw, INODE_FLAGS))?;
            let is_dir = le16(&raw, 0) & S_IFMT == S_IFDIR;
            if is_dir && !fs.dir_is_empty(child)? {
                return Err(DriverError::DirectoryNotEmpty);
            }
            let now = fs.now();
            fs.remove_dirent(dir_ino, name)?;
            fs.update_dir(dir_ino, -i16::from(is_dir), now)?;
            fs.drop_one_name(child, &mut raw, is_dir, now)
        })
    }

    fn rename(
        &mut self,
        src_dir: NodeId,
        src_name: &[u8],
        dst_dir: NodeId,
        dst_name: &[u8],
    ) -> Result<(), DriverError> {
        self.operation(|fs| fs.rename_inner(src_dir, src_name, dst_dir, dst_name))
    }

    fn flush(&mut self) -> Result<(), DriverError> {
        self.commit(true)
    }

    fn set_volume_host(&mut self, volume: DriverHandle, host: &'static dyn VolumeHost) {
        self.schedule.set_host(volume, host);
    }

    fn set_allocation_class(&mut self, class: AllocationClass) {
        self.class = class;
    }
}

/// Decode a [`NodeId`] into an inode number.
fn node_inode(node: NodeId) -> Result<u32, DriverError> {
    if node == NodeId::NONE {
        return Err(DriverError::NotFound);
    }
    u32::try_from(node.raw()).map_err(|_| DriverError::NotFound)
}

#[cfg(test)]
mod tests;
