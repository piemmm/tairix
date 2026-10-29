//! The front's half of a mounted share: the kernel's `fsserve-v1` calls on
//! its volume, each answered by a file operation on its tree.
//!
//! Every byte a call moves crosses the shared window: a read's data lands in
//! its span as the worker reports it, a write's is staged to the worker from
//! its span before the write is ordered. Once a call is answered — or has
//! outlived its deadline — nothing more is written to its span, so the kernel
//! may hand the span to the next call at once. While calls are in hand the
//! volume tells the kernel it is working, well within the kernel's idle
//! bound, so a call is abandoned only by its own deadline or its caller.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;

use core::fmt::Write as _;
use tairix_abi::driver::filesystem::{
    DirEntry, NodeId, NodeInfo, NodeKind, NodeSecurity, NodeTimes, Unpinned, VolumeStats,
};
use tairix_abi::fs::{SeekRegion, FS_PATH_MAX};
use tairix_abi::fsserve::{
    decode_request, encode_reply, request_op, Answer, DirEnd, DirReply, Op, Request, ServerState,
    Span, SERVED_IDLE_NS,
};
use tairix_abi::netattach::OfferId;
use tairix_abi::netshare_ipc::MountState;
use tairix_abi::time::NANOS_PER_SEC;
use tairix_abi::DriverError;

use tairix_inline::{ArrayString, ArrayVec};
use tairix_log::Sink;
use tairix_smb::file::name::SharePath;
use tairix_smb::status::NtStatus;
use tairix_smb::SmbError;

use super::held::{Effect, Held, HeldOp, Holding};
use super::meta;
use super::{Connection, Front, Lie, CALL_DEADLINE_NS};
use crate::host::{ConnId, Host};
use crate::nodes::{same, within, Node, Nodes, ROOT};
use crate::wire::{Durability, FileOp, Order, Report, Space, Stat, CHUNK_MAX, IO_MAX};

/// How often a volume with calls in hand tells the kernel it is working on
/// them.
pub const ALIVE_EVERY_NS: u64 = SERVED_IDLE_NS / 4;

/// How long a listing nothing reads keeps its directory open.
pub const LISTING_IDLE_NS: u64 = 30 * NANOS_PER_SEC as u64;

/// The most listings one volume keeps open at once.
pub const LISTINGS_MAX: usize = 8;

/// A call in hand, and what its answer waits on.
pub(super) struct Served {
    ticket: u64,
    op: Op,
    /// The worker's name for the operation it waits on, if it waits on one.
    request: Option<u32>,
    deadline: u64,
    work: Work,
    /// The held file whose work the call is part of.
    held: Option<NodeId>,
    /// What a call on a held file asked, kept while it goes to the server
    /// so a dropped connection hands it back to the file's work.
    asked: Option<HeldOp>,
}

impl Served {
    /// The call `ticket` asked at `now`, answered by `work`.
    const fn new(now: u64, ticket: u64, op: Op, work: Work) -> Self {
        Self {
            ticket,
            op,
            request: None,
            deadline: now.saturating_add(CALL_DEADLINE_NS),
            work,
            held: None,
            asked: None,
        }
    }
}

/// What a held call hands the server, on the file's open or by path.
#[derive(Copy, Clone)]
pub(super) enum Through {
    Read(u64, Span),
    Write(u64, Span),
    Resize(u64),
}

/// Where a read, a write or a resize reaches its file: by path, or on the
/// open the front holds.
enum Via {
    Path(SharePath),
    Held(u32),
}

/// What a call's answer is made from, and the file operation it waits on.
enum Work {
    Lookup(SharePath),
    Info(NodeId, SharePath),
    Read {
        node: NodeId,
        at: Via,
        offset: u64,
        span: Span,
        got: usize,
    },
    /// A write of the bytes its span holds: its node, where, and offset.
    Write(NodeId, Via, u64, Span),
    Create(SharePath, bool),
    /// A remove, under the lease the node is held with.
    Remove(SharePath, Option<[u8; 16]>),
    /// A move, replacing a file at the target when set, under the lease
    /// the node moved is held with.
    Rename(SharePath, SharePath, bool, Option<[u8; 16]>),
    /// A directory moved over another: the empty one removed, then the move.
    Replace(SharePath, SharePath),
    /// A file the front holds open removed: moved aside to a hidden name,
    /// then that name removed, so the name is free at once while the open
    /// serves on until its last hold goes.
    Unlink(SharePath, SharePath, [u8; 16]),
    Truncate(NodeId, Via, u64),
    /// A call on a held file, waiting its turn in the file's work.
    Held(NodeId, HeldOp),
    /// A flush, waiting on its held files' syncs.
    Flushing(u32),
    Seek {
        path: SharePath,
        offset: u64,
        region: SeekRegion,
    },
    Space,
    /// A listing batch, waiting on its listing's next page.
    List(u32),
    /// A span's release, waiting on the reads still writing into it.
    Release(Span),
}

impl Work {
    /// Whether asking again what a dropped connection took the answer of
    /// does no more than asking once: a change by name might already have
    /// been made, and making it again would fail or undo another's.
    const fn repeatable(&self) -> bool {
        !matches!(
            self,
            Self::Create(..)
                | Self::Remove(..)
                | Self::Rename(..)
                | Self::Replace(..)
                | Self::Unlink(..)
        )
    }

    /// The file operation this work orders, if it orders one itself.
    fn file_op(&self) -> Option<FileOp<'_>> {
        Some(match self {
            Self::Lookup(path) | Self::Info(_, path) | Self::Seek { path, .. } => FileOp::Stat {
                path: path.as_str(),
            },
            Self::Read {
                at, offset, span, ..
            } => {
                let len = u32::try_from(span.size()).ok()?;
                match at {
                    Via::Held(handle) => FileOp::ReadHeld {
                        handle: *handle,
                        offset: *offset,
                        len,
                    },
                    Via::Path(path) => FileOp::Read {
                        path: path.as_str(),
                        offset: *offset,
                        len,
                    },
                }
            }
            Self::Write(_, at, offset, span) => {
                let len = u32::try_from(span.size()).ok()?;
                match at {
                    Via::Held(handle) => FileOp::WriteHeld {
                        handle: *handle,
                        offset: *offset,
                        len,
                        replay: false,
                    },
                    Via::Path(path) => FileOp::Write {
                        path: path.as_str(),
                        offset: *offset,
                        len,
                    },
                }
            }
            Self::Create(path, directory) => FileOp::Create {
                path: path.as_str(),
                directory: *directory,
            },
            Self::Remove(path, lease) => FileOp::Remove {
                path: path.as_str(),
                lease: *lease,
            },
            Self::Replace(_, path) => FileOp::Remove {
                path: path.as_str(),
                lease: None,
            },
            Self::Unlink(path, hidden, lease) => FileOp::Rename {
                from: path.as_str(),
                to: hidden.as_str(),
                replace: false,
                lease: Some(*lease),
            },
            Self::Rename(from, to, replace, lease) => FileOp::Rename {
                from: from.as_str(),
                to: to.as_str(),
                replace: *replace,
                lease: *lease,
            },
            Self::Truncate(_, at, size) => match at {
                Via::Held(handle) => FileOp::Resize {
                    handle: *handle,
                    size: *size,
                },
                Via::Path(path) => FileOp::Truncate {
                    path: path.as_str(),
                    size: *size,
                },
            },
            Self::Space => FileOp::Space,
            Self::List(_) | Self::Release(_) | Self::Held(..) | Self::Flushing(_) => return None,
        })
    }
}

/// One directory open for listing, and what of it the kernel has been
/// handed.
struct Listing {
    dir: NodeId,
    path: SharePath,
    handle: u32,
    /// Entries handed to the kernel: the cursor that continues the listing.
    position: u64,
    /// Entries still to pass over before any is handed, after a restart.
    skip: u64,
    /// Entries received and not yet handed.
    page: VecDeque<(String, Stat)>,
    /// Whether the directory is open on the server.
    opened: bool,
    /// Whether the server has more pages.
    more: bool,
    /// The worker's name for the open or page in flight.
    busy: Option<u32>,
    used: u64,
    /// Every entry received, kept to be remembered once the listing ends,
    /// while it is one read whole from its start and no longer than one
    /// remembered.
    kept: Option<Vec<(String, Stat)>>,
    /// The lease the directory is opened under, where the server leases
    /// directories, and whether it was granted with reads.
    lease: Option<([u8; 16], bool)>,
}

/// A directory the front keeps open under a lease, so what it read of the
/// directory stands until the lease breaks.
pub(super) struct DirLease {
    pub(super) dir: NodeId,
    handle: u32,
    pub(super) key: [u8; 16],
    used: u64,
}

/// The most directories one volume keeps leased: each holds an open on the
/// server.
pub const DIR_LEASES_MAX: usize = 16;

/// How long a directory lease is kept unused: CIFS's `dir_cache_timeout`
/// default, so a mount left idle holds nothing open and costs nothing.
pub const DIR_LEASE_IDLE_NS: u64 = 30 * NANOS_PER_SEC as u64;

/// One mounted share's volume.
pub(super) struct Volume {
    pub(super) conn: ConnId,
    pub(super) tree: u32,
    pub(super) read_only: bool,
    pub(super) nodes: Nodes,
    pub(super) calls: Vec<Served>,
    listings: Vec<Listing>,
    pub(super) held: Holding,
    /// The volume is going, once its held files are let go.
    pub(super) retiring: Option<Retiring>,
    /// Whether its server leases directories.
    pub(super) dir_leasing: bool,
    pub(super) dir_leases: Vec<DirLease>,
    alive_at: Option<u64>,
}

/// A volume going: what its held files hold is sent and their opens let go
/// before it is withdrawn.
pub(super) struct Retiring {
    pub(super) cause: &'static str,
    /// When it is withdrawn whether or not they are.
    pub(super) by: u64,
    /// The unmounts to answer once it is.
    pub(super) unmounts: Vec<u64>,
}

impl Volume {
    /// The volume of the share connected as `tree` on `conn`.
    pub(super) fn new(
        conn: ConnId,
        tree: u32,
        read_only: bool,
        nodes: Nodes,
        held: Holding,
    ) -> Self {
        Self {
            conn,
            tree,
            read_only,
            nodes,
            calls: Vec::new(),
            listings: Vec::new(),
            held,
            retiring: None,
            dir_leasing: false,
            dir_leases: Vec::new(),
            alive_at: None,
        }
    }

    /// The key of the lease the front holds on `dir`, if it holds one.
    pub(super) fn dir_lease(&self, dir: NodeId) -> Option<[u8; 16]> {
        self.dir_leases
            .iter()
            .find(|lease| lease.dir == dir)
            .map(|lease| lease.key)
    }

    /// What was kept under the lease on `dir` answered a call at `now`.
    pub(super) fn used_dir_lease(&mut self, dir: NodeId, now: u64) {
        if let Some(lease) = self.dir_leases.iter_mut().find(|lease| lease.dir == dir) {
            lease.used = now;
        }
    }

    /// Whether it rests on its connection: a call waits on the server, a file
    /// is held open, or a directory is being listed or kept leased.
    pub(super) fn rests_on_server(&self) -> bool {
        !(self.calls.is_empty()
            && self.listings.is_empty()
            && self.dir_leases.is_empty()
            && self.held.files.is_empty())
    }

    /// The earliest instant something on the volume is due.
    pub(super) fn next_deadline(&self) -> Option<u64> {
        let calls = self.calls.iter().map(|call| call.deadline).min();
        let listings = self
            .listings
            .iter()
            .filter(|listing| listing.busy.is_none())
            .map(|listing| listing.used.saturating_add(LISTING_IDLE_NS))
            .min();
        let leases = self
            .dir_leases
            .iter()
            .map(|lease| lease.used.saturating_add(DIR_LEASE_IDLE_NS))
            .min();
        [
            calls,
            listings,
            leases,
            self.alive_at.filter(|_| !self.calls.is_empty()),
            self.held.next_deadline(),
            self.retiring.as_ref().map(|retiring| retiring.by),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// Hold the call `ticket` in the held file `node`'s work, its turn to
    /// come; room was made by the caller.
    pub(super) fn hold_call(&mut self, now: u64, ticket: u64, node: NodeId, op: HeldOp) {
        let call_op = match op {
            HeldOp::Read { .. } => Op::ReadAt,
            HeldOp::Write { .. } => Op::WriteAt,
            HeldOp::Truncate(_) => Op::Truncate,
        };
        self.calls.push(Served {
            held: Some(node),
            asked: Some(op),
            ..Served::new(now, ticket, call_op, Work::Held(node, op))
        });
    }

    /// What the held call `ticket` asks, if it still waits its turn.
    pub(super) fn held_op(&self, ticket: u64) -> Option<HeldOp> {
        self.calls.iter().find_map(|call| match call.work {
            Work::Held(_, op) if call.ticket == ticket => Some(op),
            _ => None,
        })
    }

    /// Take the call `ticket` off the volume, to be answered.
    pub(super) fn take_call(&mut self, ticket: u64) -> Option<(u64, Op)> {
        let at = self.calls.iter().position(|call| call.ticket == ticket)?;
        let call = self.calls.swap_remove(at);
        Some((call.ticket, call.op))
    }

    /// Hold the flush call `ticket` on the syncs of the flush `flush`; room
    /// was made by the caller.
    pub(super) fn flush_call(&mut self, now: u64, ticket: u64, flush: u32) {
        self.calls
            .push(Served::new(now, ticket, Op::Flush, Work::Flushing(flush)));
    }

    /// Take the flush call waiting on `flush` off the volume.
    pub(super) fn take_flush(&mut self, flush: u32) -> Option<(u64, Op)> {
        let at = self
            .calls
            .iter()
            .position(|call| matches!(call.work, Work::Flushing(held) if held == flush))?;
        let call = self.calls.swap_remove(at);
        Some((call.ticket, call.op))
    }

    fn listing_mut(&mut self, handle: u32) -> Option<&mut Listing> {
        self.listings
            .iter_mut()
            .find(|listing| listing.handle == handle)
    }

    /// Whether a read in hand still writes into `span`.
    fn reading_into(&self, span: Span) -> bool {
        self.calls.iter().any(|call| match call.work {
            Work::Read { span: held, .. } | Work::Held(_, HeldOp::Read { span: held, .. }) => {
                overlaps(held, span)
            }
            _ => false,
        })
    }
}

/// The driver error a failed file operation answers.
pub(super) fn driver_error(error: SmbError) -> DriverError {
    match error {
        SmbError::Status(
            NtStatus::OBJECT_NAME_NOT_FOUND
            | NtStatus::OBJECT_PATH_NOT_FOUND
            | NtStatus::OBJECT_PATH_SYNTAX_BAD
            | NtStatus::NO_SUCH_FILE
            | NtStatus::NOT_FOUND
            | NtStatus::OBJECT_NAME_INVALID,
        )
        | SmbError::Name => DriverError::NotFound,
        SmbError::Status(NtStatus::OBJECT_NAME_COLLISION) => DriverError::AlreadyExists,
        SmbError::Status(
            NtStatus::ACCESS_DENIED | NtStatus::NETWORK_ACCESS_DENIED | NtStatus::CANNOT_DELETE,
        ) => DriverError::PermissionDenied,
        SmbError::Status(NtStatus::DIRECTORY_NOT_EMPTY) => DriverError::DirectoryNotEmpty,
        SmbError::Status(NtStatus::DISK_FULL) => DriverError::NoSpace,
        SmbError::Status(NtStatus::MEDIA_WRITE_PROTECTED) => DriverError::ReadOnly,
        SmbError::Status(
            NtStatus::SHARING_VIOLATION | NtStatus::DELETE_PENDING | NtStatus::FILE_LOCK_CONFLICT,
        ) => DriverError::Busy,
        SmbError::Status(NtStatus::NAME_TOO_LONG) => DriverError::LengthOutOfRange,
        SmbError::Status(NtStatus::IO_TIMEOUT) => DriverError::TimedOut,
        SmbError::Status(
            NtStatus::NETWORK_NAME_DELETED
            | NtStatus::USER_SESSION_DELETED
            | NtStatus::NETWORK_SESSION_EXPIRED,
        ) => DriverError::DeviceOffline,
        SmbError::Status(
            NtStatus::FILE_IS_A_DIRECTORY | NtStatus::NOT_A_DIRECTORY | NtStatus::NOT_SUPPORTED,
        ) => DriverError::Unsupported,
        SmbError::OutOfMemory => DriverError::OutOfMemory,
        _ => DriverError::DeviceFault,
    }
}

/// What a node is, as the kernel is told it.
fn node_info(node: NodeId, stat: &Stat) -> NodeInfo {
    NodeInfo {
        kind: if stat.directory {
            NodeKind::Directory
        } else {
            NodeKind::RegularFile
        },
        nlink: NodeInfo::SINGLE_NAME,
        ino: if stat.file_id == 0 {
            node.raw()
        } else {
            stat.file_id
        },
        size: stat.size,
        allocated: stat.allocated,
        times: NodeTimes {
            created: stat.created,
            modified: stat.modified,
            accessed: stat.accessed,
            changed: stat.changed,
        },
        content_gen: NodeInfo::NO_CONTENT_GEN,
    }
}

/// Whether what the server now says of a node is another node than the one
/// recorded: both ids known, and different.
const fn replaced(recorded: u64, reported: u64) -> bool {
    recorded != 0 && reported != 0 && recorded != reported
}

/// Refuse a change to a read-only volume.
const fn writable(volume: &Volume) -> Result<(), DriverError> {
    if volume.read_only {
        Err(DriverError::ReadOnly)
    } else {
        Ok(())
    }
}

/// The node `id` names, or stale.
fn known(volume: &mut Volume, id: NodeId) -> Result<Node, DriverError> {
    volume.nodes.get(id).ok_or(DriverError::Stale)
}

/// The directory `id` names.
fn dir_of(volume: &mut Volume, id: NodeId) -> Result<Node, DriverError> {
    let node = known(volume, id)?;
    if node.directory {
        Ok(node)
    } else {
        Err(DriverError::Unsupported)
    }
}

/// The file `id` names.
fn file_of(volume: &mut Volume, id: NodeId) -> Result<Node, DriverError> {
    let node = known(volume, id)?;
    if node.directory {
        Err(DriverError::Unsupported)
    } else {
        Ok(node)
    }
}

/// The path of `name` beneath the directory `dir`, when both sides can
/// carry it.
fn child(volume: &mut Volume, dir: NodeId, name: &[u8]) -> Result<SharePath, DriverError> {
    let dir = dir_of(volume, dir)?;
    let name = core::str::from_utf8(name).map_err(|_| DriverError::NotFound)?;
    let path = dir.path.join(name).map_err(|error| match error {
        SmbError::OutOfMemory => DriverError::OutOfMemory,
        _ => DriverError::NotFound,
    })?;
    if path.as_str().len() > FS_PATH_MAX {
        return Err(DriverError::LengthOutOfRange);
    }
    Ok(path)
}

/// The path of `path`'s parent.
pub(super) fn parent_of(path: &SharePath) -> Result<SharePath, DriverError> {
    let text = path.as_str();
    let parent = text.rfind('\\').map_or("", |at| &text[..at]);
    SharePath::parse(parent).map_err(|_| DriverError::OutOfMemory)
}

/// Whether two spans of the window share a byte.
const fn overlaps(a: Span, b: Span) -> bool {
    a.offset() < b.offset() + b.size() && b.offset() < a.offset() + a.size()
}

/// The parent of the directory `dir`.
fn parent(volume: &mut Volume, dir: NodeId) -> Result<Local, DriverError> {
    let found = dir_of(volume, dir)?;
    if dir == ROOT {
        return Ok(Local::Node(ROOT));
    }
    volume
        .nodes
        .name(&parent_of(&found.path)?, 0, true)
        .map(Local::Node)
        .map_err(|_| DriverError::OutOfMemory)
}

/// Hold `node`.
fn pin(volume: &mut Volume, node: NodeId) -> Result<Local, DriverError> {
    match volume.nodes.pin(node) {
        Ok(true) => Ok(Local::Node(node)),
        Ok(false) => Err(DriverError::Stale),
        Err(_) => Err(DriverError::OutOfMemory),
    }
}

/// The security a node is answered with: none is recorded on the share.
fn security(volume: &mut Volume, node: NodeId) -> Result<Local, DriverError> {
    let kind = if known(volume, node)?.directory {
        NodeKind::Directory
    } else {
        NodeKind::RegularFile
    };
    Ok(Local::Security(NodeSecurity::unrecorded(kind)))
}

/// The work reading `span`'s worth of `file` from `offset`.
fn read(volume: &mut Volume, file: NodeId, offset: u64, span: Span) -> Result<Work, DriverError> {
    let path = file_of(volume, file)?.path;
    if span.size() > IO_MAX {
        return Err(DriverError::OutOfRange);
    }
    Ok(Work::Read {
        node: file,
        at: Via::Path(path),
        offset,
        span,
        got: 0,
    })
}

/// The lease the front holds `path`'s node open under, if it holds it.
fn lease_on(volume: &mut Volume, path: &SharePath) -> Option<[u8; 16]> {
    let id = volume.nodes.id_of(path)?;
    volume
        .held
        .files
        .get(&id.raw())
        .filter(|held| held.is_open())
        .map(|held| held.key)
}

/// The hidden name a held file removed waits under, beside its own, for its
/// last hold to go: the front's name for its open makes it one no other
/// held file takes.
fn hidden(path: &SharePath, handle: u32) -> Result<SharePath, DriverError> {
    let mut name = ArrayString::<16>::new();
    write!(name, ".smbfs-{handle:08x}").map_err(|_| DriverError::OutOfMemory)?;
    parent_of(path)?
        .join(&name)
        .map_err(|_| DriverError::OutOfMemory)
}

/// What a rename does: nothing, a move, or a move once the front has let
/// go of its own open on the file the move replaces, which no share
/// replaces while it is open.
enum Moving {
    Nothing,
    Now(Work),
    AfterClose(NodeId, Work),
}

/// The work making `name` in `dir`.
fn create(
    volume: &mut Volume,
    dir: NodeId,
    name: &[u8],
    kind: NodeKind,
) -> Result<Work, DriverError> {
    writable(volume)?;
    let path = child(volume, dir, name)?;
    match kind {
        NodeKind::Directory => Ok(Work::Create(path, true)),
        NodeKind::RegularFile => Ok(Work::Create(path, false)),
        NodeKind::Symlink => Err(DriverError::Unsupported),
    }
}

/// The work moving `from` to `to`, each a directory and a name, or `None`
/// for a move onto itself, which changes nothing.
///
/// The kernel has found both ends and refused a move between kinds, so a
/// directory moved over another is one over an empty directory: the share
/// replaces no directory, so the empty one is removed first.
fn rename(
    volume: &mut Volume,
    from: (NodeId, &[u8]),
    to: (NodeId, &[u8]),
) -> Result<Moving, DriverError> {
    writable(volume)?;
    let source = child(volume, from.0, from.1)?;
    let target = child(volume, to.0, to.1)?;
    if from == to {
        return Ok(Moving::Nothing);
    }
    let moving_dir = volume.nodes.at(&source).is_some_and(|node| node.directory);
    let onto_itself = same(&source, &target);
    if moving_dir && !onto_itself && within(&target, &source) {
        return Err(DriverError::DirectoryCycle);
    }
    let over_dir = !onto_itself && volume.nodes.at(&target).is_some_and(|node| node.directory);
    if moving_dir && over_dir {
        return Ok(Moving::Now(Work::Replace(source, target)));
    }
    let lease = lease_on(volume, &source);
    let replaced = (!onto_itself)
        .then(|| volume.nodes.id_of(&target))
        .flatten()
        .filter(|id| volume.held.files.get(&id.raw()).is_some_and(Held::is_open));
    let work = Work::Rename(source, target, true, lease);
    Ok(match replaced {
        Some(id) => Moving::AfterClose(id, work),
        None => Moving::Now(work),
    })
}

/// An answer made without the server.
enum Local {
    Node(NodeId),
    Done,
    Unpinned(Unpinned),
    Security(NodeSecurity),
    Owned(bool),
    Info(NodeInfo),
    Region(Option<u64>),
}

impl Local {
    const fn as_answer(&self) -> Answer<'static> {
        match *self {
            Self::Node(node) => Answer::Node(node),
            Self::Done => Answer::Done,
            Self::Unpinned(left) => Answer::Unpinned(left),
            Self::Security(security) => Answer::Security(security),
            Self::Owned(owned) => Answer::Owned(owned),
            Self::Info(info) => Answer::Info(info),
            Self::Region(region) => Answer::Region(region),
        }
    }
}

/// Where the data or the hole at or after `offset` starts, in a file of
/// `size` bytes stored without holes.
fn region(offset: u64, size: u64, region: SeekRegion) -> Option<u64> {
    let within = offset < size;
    match region {
        SeekRegion::Data => within.then_some(offset),
        SeekRegion::Hole => within.then_some(size),
    }
}

impl From<Space> for VolumeStats {
    fn from(space: Space) -> Self {
        Self {
            block_size: space.unit,
            in_reserve: false,
            total_blocks: space.total,
            free_blocks: space.free,
            avail_blocks: space.available,
            files: 0,
            files_free: 0,
        }
    }
}

/// Where a call goes once a file operation's report settles it.
enum Settled {
    /// It is answered.
    Answer(Result<Owned, DriverError>),
    /// It goes on to further work.
    Next(Work),
}

/// An answer carrying no borrowed bytes.
enum Owned {
    Node(NodeId),
    Info(NodeInfo),
    Count(u32),
    Done,
    Region(Option<u64>),
    Stats(VolumeStats),
}

impl Owned {
    const fn as_answer(&self) -> Answer<'static> {
        match *self {
            Self::Node(node) => Answer::Node(node),
            Self::Info(info) => Answer::Info(info),
            Self::Count(count) => Answer::Count(count),
            Self::Done => Answer::Done,
            Self::Region(region) => Answer::Region(region),
            Self::Stats(stats) => Answer::Stats(stats),
        }
    }
}

/// Whether the node `id` is still the one the server reports as `file_id`;
/// a replaced one is forgotten and answered stale.
fn checked(volume: &mut Volume, id: NodeId, file_id: u64) -> Result<(), DriverError> {
    let recorded = volume.nodes.get(id).ok_or(DriverError::Stale)?.file_id;
    if replaced(recorded, file_id) {
        volume.nodes.forget(id.raw());
        return Err(DriverError::Stale);
    }
    Ok(())
}

/// What `report` makes of the call `volume.calls[at]` holds, or `None` while
/// it waits on more; `window` lends the span of the volume's shared window a
/// read's data lands in.
fn settle<'w>(
    volume: &mut Volume,
    at: usize,
    window: impl FnOnce(Span) -> Option<&'w mut [u8]>,
    report: &Report<'_>,
) -> Result<Option<Settled>, Lie> {
    let named = |volume: &mut Volume, path: &SharePath, stat: &Stat| {
        volume
            .nodes
            .name(path, stat.file_id, stat.directory)
            .map(Owned::Node)
            .map_err(|_| DriverError::OutOfMemory)
    };
    let settled = match (&mut volume.calls[at].work, report) {
        // A server that would not move a removed open file aside removes
        // it where it is.
        (Work::Unlink(path, _, key), Report::Refused { error, .. })
            if driver_error(*error) != DriverError::NotFound =>
        {
            Settled::Next(Work::Remove(path.clone(), Some(*key)))
        }
        (_, Report::Refused { error, .. }) => Settled::Answer(Err(driver_error(*error))),
        (Work::Lookup(path) | Work::Create(path, _), Report::Stat { stat, .. }) => {
            let path = path.clone();
            Settled::Answer(named(volume, &path, stat))
        }
        (Work::Info(id, _), Report::Stat { stat, .. }) => {
            let id = *id;
            Settled::Answer(
                checked(volume, id, stat.file_id).map(|()| Owned::Info(node_info(id, stat))),
            )
        }
        (
            Work::Seek {
                offset,
                region: asked,
                ..
            },
            Report::Stat { stat, .. },
        ) => Settled::Answer(Ok(Owned::Region(region(*offset, stat.size, *asked)))),
        (Work::Read { span, got, .. }, Report::Data { chunk, .. }) => {
            if *got + chunk.len() > span.size() {
                return Err(Lie);
            }
            window(*span)
                .and_then(|window| window.get_mut(*got..*got + chunk.len()))
                .ok_or(Lie)?
                .copy_from_slice(chunk);
            *got += chunk.len();
            return Ok(None);
        }
        (Work::Read { node, got, .. }, Report::Done { count, file_id, .. }) => {
            if usize::try_from(*count).ok() != Some(*got) {
                return Err(Lie);
            }
            let node = *node;
            Settled::Answer(checked(volume, node, *file_id).map(|()| Owned::Count(*count)))
        }
        (Work::Write(node, .., span), Report::Done { count, file_id, .. }) => {
            if *count as usize > span.size() {
                return Err(Lie);
            }
            let node = *node;
            Settled::Answer(checked(volume, node, *file_id).map(|()| Owned::Count(*count)))
        }
        (Work::Truncate(node, ..), Report::Done { file_id, .. }) => {
            let node = *node;
            Settled::Answer(checked(volume, node, *file_id).map(|()| Owned::Done))
        }
        (Work::Remove(path, _), Report::Done { .. }) => {
            let path = path.clone();
            volume.nodes.forget_at(&path);
            Settled::Answer(Ok(Owned::Done))
        }
        (Work::Unlink(path, aside, key), Report::Done { .. }) => {
            let next = Work::Remove(aside.clone(), Some(*key));
            let (path, aside) = (path.clone(), aside.clone());
            if volume.nodes.moved(&path, &aside).is_err() {
                volume.nodes.forget_at(&path);
            }
            Settled::Next(next)
        }
        (Work::Replace(from, to), Report::Done { .. }) => {
            let next = Work::Rename(from.clone(), to.clone(), false, None);
            volume.nodes.forget_at(to);
            Settled::Next(next)
        }
        (Work::Rename(from, to, ..), Report::Done { .. }) => {
            let (from, to) = (from.clone(), to.clone());
            Settled::Answer(
                volume
                    .nodes
                    .moved(&from, &to)
                    .map(|()| Owned::Done)
                    .map_err(|_| DriverError::OutOfMemory),
            )
        }
        (Work::Space, Report::Space { space, .. }) => {
            Settled::Answer(Ok(Owned::Stats(VolumeStats::from(*space))))
        }
        _ => return Err(Lie),
    };
    Ok(Some(settled))
}

impl<H: Host, S: Sink + Clone> Front<H, S> {
    /// Calls are waiting on `offer`'s volume.
    pub fn served(&mut self, now: u64, offer: OfferId) {
        let mut request = core::mem::take(&mut self.served_request);
        loop {
            match self.host.served_take(offer, &mut request) {
                Ok(Some((ticket, len))) => {
                    let len = len.min(request.len());
                    self.serve_call(now, offer, ticket, &request[..len]);
                }
                Ok(None) => break,
                // The kernel took the volume's endpoint down: its owner
                // detached it.
                Err(_) => {
                    self.withdraw(now, offer, "detached");
                    break;
                }
            }
        }
        self.served_request = request;
        if let Some(conn) = self.volumes.get(&offer.get()).map(|volume| volume.conn) {
            self.settled(now, conn);
        }
    }

    pub(super) fn served_answer(
        &mut self,
        offer: OfferId,
        ticket: u64,
        op: Op,
        result: Result<Answer<'_>, DriverError>,
    ) {
        let state = ServerState {
            unpublished: self
                .volumes
                .get(&offer.get())
                .is_some_and(|volume| volume.held.unpublished()),
            in_reserve: false,
        };
        let out = &mut self.served_reply;
        // A value the codec refuses is a server reporting what no volume
        // holds: answered as the fault it is.
        let len = encode_reply(op, state, result, out)
            .or_else(|_| encode_reply(op, state, Err(DriverError::DeviceFault), out))
            .unwrap_or(0);
        self.host.served_reply(offer, ticket, &out[..len]);
    }

    fn serve_call(&mut self, now: u64, offer: OfferId, ticket: u64, frame: &[u8]) {
        let Ok((_, request)) = decode_request(frame) else {
            match request_op(frame) {
                Some(op) => self.served_answer(offer, ticket, op, Err(DriverError::OutOfRange)),
                None => self.host.served_reply(offer, ticket, &[]),
            }
            return;
        };
        let op = request.op();
        match self.dispatch_served(now, offer, ticket, &request) {
            Ok(None) => {}
            Ok(Some(answer)) => self.served_answer(offer, ticket, op, Ok(answer.as_answer())),
            Err(error) => self.served_answer(offer, ticket, op, Err(error)),
        }
    }

    /// Answer `request`, which the volume answers without the server, or
    /// holds behind its own held files.
    fn answer_local(
        &mut self,
        now: u64,
        offer: OfferId,
        ticket: u64,
        request: &Request<'_>,
    ) -> Result<Option<Local>, DriverError> {
        let volume = self
            .volumes
            .get_mut(&offer.get())
            .ok_or(DriverError::DeviceOffline)?;
        match *request {
            Request::Parent { dir } => parent(volume, dir).map(Some),
            Request::Pin { node } => pin(volume, node).map(Some),
            Request::Unpin { pinned } => {
                let left = volume.nodes.unpin(pinned).ok_or(DriverError::OutOfRange)?;
                if left != Unpinned::Held {
                    self.unheld(now, offer, pinned);
                }
                Ok(Some(Local::Unpinned(left)))
            }
            Request::ReleasePins => {
                let mut held = Vec::new();
                held.try_reserve_exact(volume.held.files.len())
                    .map_err(|_| DriverError::OutOfMemory)?;
                held.extend(volume.held.files.keys().copied());
                volume.nodes.release_pins();
                for node in held {
                    self.unheld(now, offer, NodeId::from_raw(node));
                }
                Ok(Some(Local::Done))
            }
            Request::Flush => self
                .flush_held(now, offer, ticket)
                .map(|waiting| waiting.map_or(Some(Local::Done), |()| None)),
            Request::Security { node } => security(volume, node).map(Some),
            Request::RecordsOwnership { node } => {
                known(volume, node).map(|_| Some(Local::Owned(false)))
            }
            _ => Err(DriverError::Unsupported),
        }
    }

    /// Answer `request` at once, or hold it on a file operation: `None` when
    /// it is held.
    fn dispatch_served(
        &mut self,
        now: u64,
        offer: OfferId,
        ticket: u64,
        request: &Request<'_>,
    ) -> Result<Option<Local>, DriverError> {
        if let Some(answer) = self.dispatch_held(now, offer, ticket, request) {
            return answer;
        }
        if let Some(answer) = self.dispatch_kept(now, offer, request) {
            return answer;
        }
        let volume = self
            .volumes
            .get_mut(&offer.get())
            .ok_or(DriverError::DeviceOffline)?;
        let work = match *request {
            Request::ReadDir { dir, cursor, .. } => {
                return self.list(now, offer, ticket, dir, cursor)
            }
            Request::WriteAt { file, offset, span } => {
                return self.write_at(now, offer, ticket, file, offset, span);
            }
            Request::Release { span } => return self.hold_release(now, offer, ticket, span),
            Request::Parent { .. }
            | Request::Pin { .. }
            | Request::Unpin { .. }
            | Request::ReleasePins
            | Request::Flush
            | Request::Security { .. }
            | Request::RecordsOwnership { .. } => {
                return self.answer_local(now, offer, ticket, request)
            }
            Request::ReadLink { .. }
            | Request::CreateLink { .. }
            | Request::Link { .. }
            | Request::Punch { .. }
            | Request::SetSecurity { .. }
            | Request::GetAttr { .. }
            | Request::SetAttr { .. }
            | Request::ListAttr { .. }
            | Request::RemoveAttr { .. } => return Err(DriverError::Unsupported),
            Request::NodeInfo { node } => Work::Info(node, known(volume, node)?.path),
            Request::Lookup { dir, name } => Work::Lookup(child(volume, dir, name)?),
            Request::ReadAt { file, offset, span } => read(volume, file, offset, span)?,
            Request::SeekRegion {
                file,
                offset,
                region,
            } => Work::Seek {
                path: file_of(volume, file)?.path,
                offset,
                region,
            },
            Request::Create {
                dir, name, kind, ..
            } => create(volume, dir, name, kind)?,
            Request::Truncate { file, size } => {
                writable(volume)?;
                Work::Truncate(file, Via::Path(file_of(volume, file)?.path), size)
            }
            Request::Remove { dir, name } => {
                writable(volume)?;
                let path = child(volume, dir, name)?;
                match volume.nodes.id_of(&path).and_then(|id| {
                    let held = volume
                        .held
                        .files
                        .get(&id.raw())
                        .filter(|held| held.is_open())?;
                    Some((held.handle, held.key))
                }) {
                    Some((handle, key)) => {
                        let aside = hidden(&path, handle)?;
                        Work::Unlink(path, aside, key)
                    }
                    None => Work::Remove(path, None),
                }
            }
            Request::Rename {
                src_dir,
                src_name,
                dst_dir,
                dst_name,
            } => match rename(volume, (src_dir, src_name), (dst_dir, dst_name))? {
                Moving::Now(work) => work,
                Moving::Nothing => return Ok(Some(Local::Done)),
                Moving::AfterClose(replaced, work) => {
                    return self
                        .after_close(now, offer, ticket, replaced, work)
                        .map(|()| None);
                }
            },
            Request::Stats => Work::Space,
        };
        self.hold_on(now, offer, Served::new(now, ticket, request.op(), work))
    }

    /// Answer, or take into a held file's work, a call the files the kernel
    /// holds open answer differently; `None` for one they do not.
    fn dispatch_held(
        &mut self,
        now: u64,
        offer: OfferId,
        ticket: u64,
        request: &Request<'_>,
    ) -> Option<Result<Option<Local>, DriverError>> {
        let volume = self.volumes.get_mut(&offer.get())?;
        let (node, op) = match *request {
            Request::ReadAt { file, offset, span } => (file, HeldOp::Read { offset, span }),
            Request::WriteAt { file, offset, span } => {
                if let Err(error) = writable(volume) {
                    return Some(Err(error));
                }
                if span.size() > IO_MAX {
                    return Some(Err(DriverError::OutOfRange));
                }
                (file, HeldOp::Write { offset, span })
            }
            Request::Truncate { file, size } => {
                if let Err(error) = writable(volume) {
                    return Some(Err(error));
                }
                (file, HeldOp::Truncate(size))
            }
            Request::NodeInfo { node } => {
                let stat = volume.held.files.get(&node.raw())?.known()?;
                return Some(Ok(Some(Local::Info(node_info(node, &stat)))));
            }
            Request::SeekRegion {
                file,
                offset,
                region: asked,
            } => {
                let stat = volume.held.files.get(&file.raw())?.known()?;
                return Some(Ok(Some(Local::Region(region(offset, stat.size, asked)))));
            }
            _ => return None,
        };
        if let HeldOp::Read { span, .. } = op {
            if span.size() > IO_MAX {
                return Some(Err(DriverError::OutOfRange));
            }
        }
        self.held_call(now, offer, ticket, node, op)
            .map(|taken| taken.map(|()| None))
    }

    /// Answer from what the server said within the second, where it did.
    fn dispatch_kept(
        &mut self,
        now: u64,
        offer: OfferId,
        request: &Request<'_>,
    ) -> Option<Result<Option<Local>, DriverError>> {
        match *request {
            Request::NodeInfo { node } => {
                self.volumes.get_mut(&offer.get())?.nodes.get(node)?;
                let stat = self.fresh_attr(now, offer, node)?;
                Some(Ok(Some(Local::Info(node_info(node, &stat)))))
            }
            Request::SeekRegion {
                file,
                offset,
                region: asked,
            } => {
                self.volumes.get_mut(&offer.get())?.nodes.get(file)?;
                let stat = self
                    .fresh_attr(now, offer, file)
                    .filter(|stat| !stat.directory)?;
                Some(Ok(Some(Local::Region(region(offset, stat.size, asked)))))
            }
            Request::Lookup { dir, name } => {
                let volume = self.volumes.get_mut(&offer.get())?;
                let path = child(volume, dir, name).ok()?;
                let id = volume.nodes.id_of(&path)?;
                self.fresh_attr(now, offer, id)?;
                Some(Ok(Some(Local::Node(id))))
            }
            _ => None,
        }
    }

    /// Remember what a settled call's report said, and forget what the call
    /// changed.
    fn learned(
        &mut self,
        now: u64,
        offer: OfferId,
        work: &Work,
        result: &Result<Owned, DriverError>,
        report: &Report<'_>,
    ) {
        match (work, result, report) {
            (Work::Lookup(_), Ok(Owned::Node(id)), Report::Stat { stat, .. })
            | (Work::Info(id, _), Ok(Owned::Info(_)), Report::Stat { stat, .. }) => {
                self.keep_attr(now, offer, *id, *stat);
            }
            (Work::Create(path, _), Ok(Owned::Node(id)), Report::Stat { stat, .. }) => {
                self.stale_path(offer, path);
                self.keep_attr(now, offer, *id, *stat);
            }
            (Work::Remove(path, _), Ok(_), _) => self.stale_path(offer, path),
            (Work::Rename(from, to, ..), Ok(_), _) => {
                self.stale_path(offer, from);
                self.stale_path(offer, to);
            }
            (Work::Write(node, ..) | Work::Truncate(node, ..), _, _) => {
                self.stale_node(offer, *node);
            }
            _ => {}
        }
    }

    /// Hand the held call `ticket` to the server as `through`, on the open
    /// `via` or by path, the call keeping its place.
    pub(super) fn order_through(
        &mut self,
        now: u64,
        offer: OfferId,
        ticket: u64,
        via: Option<u32>,
        through: Through,
    ) -> Result<(), DriverError> {
        let volume = self
            .volumes
            .get_mut(&offer.get())
            .ok_or(DriverError::DeviceOffline)?;
        let node = volume
            .calls
            .iter()
            .find_map(|call| match call.work {
                Work::Held(node, _) if call.ticket == ticket => Some(node),
                _ => None,
            })
            .ok_or(DriverError::DeviceFault)?;
        let at = match via {
            Some(handle) => Via::Held(handle),
            None => Via::Path(file_of(volume, node)?.path),
        };
        let request = self.next_request_id();
        let work = match through {
            Through::Read(offset, span) => Work::Read {
                node,
                at,
                offset,
                span,
                got: 0,
            },
            Through::Write(offset, span) => Work::Write(node, at, offset, span),
            Through::Resize(size) => Work::Truncate(node, at, size),
        };
        let volume = self
            .volumes
            .get_mut(&offer.get())
            .ok_or(DriverError::DeviceOffline)?;
        if let Some(call) = volume.calls.iter_mut().find(|call| call.ticket == ticket) {
            call.work = work;
        }
        self.order_call(now, offer, ticket, request)
    }

    /// Order the call `ticket`'s work as `request`, the call keeping its
    /// place among the volume's.
    fn order_call(
        &mut self,
        now: u64,
        offer: OfferId,
        ticket: u64,
        request: u32,
    ) -> Result<(), DriverError> {
        self.routes
            .try_reserve(1)
            .map_err(|_| DriverError::OutOfMemory)?;
        let staged = self
            .volumes
            .get(&offer.get())
            .and_then(|volume| volume.calls.iter().find(|call| call.ticket == ticket))
            .and_then(|call| match call.work {
                Work::Write(.., span) => Some(span),
                _ => None,
            });
        if let Some(span) = staged {
            self.stage(offer, request, span)?;
        }
        let Self {
            volumes,
            conns,
            routes,
            ..
        } = self;
        let volume = volumes
            .get_mut(&offer.get())
            .ok_or(DriverError::DeviceOffline)?;
        let tree = volume.tree;
        let call = volume
            .calls
            .iter_mut()
            .find(|call| call.ticket == ticket)
            .ok_or(DriverError::DeviceFault)?;
        let op = call.work.file_op().ok_or(DriverError::DeviceFault)?;
        conns
            .get_mut(volume.conn)
            .and_then(|conn| conn.link.as_mut())
            .ok_or(DriverError::DeviceOffline)?
            .order(&Order::File { request, tree, op })
            .map_err(|_| DriverError::DeviceOffline)?;
        call.request = Some(request);
        volume
            .alive_at
            .get_or_insert(now.saturating_add(ALIVE_EVERY_NS));
        // Room was reserved above.
        let _ = routes.try_insert(request, offer.get());
        Ok(())
    }

    /// Hold the call `ticket`, whose `work` replaces the file `replaced`,
    /// until the front has let go of its own open on that file.
    fn after_close(
        &mut self,
        now: u64,
        offer: OfferId,
        ticket: u64,
        replaced: NodeId,
        work: Work,
    ) -> Result<(), DriverError> {
        let volume = self
            .volumes
            .get_mut(&offer.get())
            .ok_or(DriverError::DeviceOffline)?;
        volume
            .calls
            .try_reserve(1)
            .map_err(|_| DriverError::OutOfMemory)?;
        volume.calls.push(Served {
            held: Some(replaced),
            ..Served::new(now, ticket, Op::Rename, work)
        });
        self.park_after_close(now, offer, replaced, ticket)
    }

    /// Order the held call `ticket`, prepared when it was asked.
    pub(super) fn order_prepared(
        &mut self,
        now: u64,
        offer: OfferId,
        ticket: u64,
    ) -> Result<(), DriverError> {
        let request = self.next_request_id();
        self.order_call(now, offer, ticket, request)
    }

    /// Whether the held call `ticket` waits its turn as asked — `Some(true)`
    /// — or was prepared when it was asked; `None` once answered.
    pub(super) fn held_kind(&self, offer: OfferId, ticket: u64) -> Option<bool> {
        let call = self
            .volumes
            .get(&offer.get())?
            .calls
            .iter()
            .find(|call| call.ticket == ticket)?;
        Some(matches!(call.work, Work::Held(..)))
    }

    /// Stage a write's bytes from `span` of the window, and order the write.
    fn write_at(
        &mut self,
        now: u64,
        offer: OfferId,
        ticket: u64,
        file: NodeId,
        offset: u64,
        span: Span,
    ) -> Result<Option<Local>, DriverError> {
        let volume = self
            .volumes
            .get_mut(&offer.get())
            .ok_or(DriverError::DeviceOffline)?;
        writable(volume)?;
        let path = file_of(volume, file)?.path;
        if span.size() > IO_MAX {
            return Err(DriverError::OutOfRange);
        }
        let call = Served::new(
            now,
            ticket,
            Op::WriteAt,
            Work::Write(file, Via::Path(path), offset, span),
        );
        self.hold_on(now, offer, call)
    }

    /// Answer a release once no read in hand still writes into its span.
    fn hold_release(
        &mut self,
        now: u64,
        offer: OfferId,
        ticket: u64,
        span: Span,
    ) -> Result<Option<Local>, DriverError> {
        let volume = self
            .volumes
            .get_mut(&offer.get())
            .ok_or(DriverError::DeviceOffline)?;
        if !volume.reading_into(span) {
            return Ok(Some(Local::Done));
        }
        volume
            .calls
            .try_reserve(1)
            .map_err(|_| DriverError::OutOfMemory)?;
        volume
            .calls
            .push(Served::new(now, ticket, Op::Release, Work::Release(span)));
        Ok(None)
    }

    /// Make room to hold one more call on `offer`'s volume.
    fn reserve_call(&mut self, offer: OfferId) -> Result<(), DriverError> {
        self.routes
            .try_reserve(1)
            .map_err(|_| DriverError::OutOfMemory)?;
        self.volumes
            .get_mut(&offer.get())
            .ok_or(DriverError::DeviceOffline)?
            .calls
            .try_reserve(1)
            .map_err(|_| DriverError::OutOfMemory)
    }

    /// Hand a write's bytes, from `span` of the window, to the worker ahead of
    /// the write `request` orders.
    fn stage(&mut self, offer: OfferId, request: u32, span: Span) -> Result<(), DriverError> {
        let conn = self
            .volumes
            .get(&offer.get())
            .map(|volume| volume.conn)
            .ok_or(DriverError::DeviceOffline)?;
        let Self { host, conns, .. } = self;
        let staged = host.window(offer, span).ok_or(DriverError::OutOfRange)?;
        let link = conns
            .get_mut(conn)
            .and_then(|conn| conn.link.as_mut())
            .ok_or(DriverError::DeviceOffline)?;
        for chunk in staged.chunks(CHUNK_MAX) {
            link.order(&Order::Stage { request, chunk })
                .map_err(|_| DriverError::DeviceOffline)?;
        }
        Ok(())
    }

    /// Hold `call` on the file operation it waits on, ordered now — or,
    /// while the volume's connection is being made again, once it is.
    fn hold_on(
        &mut self,
        now: u64,
        offer: OfferId,
        call: Served,
    ) -> Result<Option<Local>, DriverError> {
        self.reserve_call(offer)?;
        let reach = self.reach(offer);
        if reach == MountState::Lost {
            return Err(DriverError::DeviceOffline);
        }
        let volume = self
            .volumes
            .get_mut(&offer.get())
            .ok_or(DriverError::DeviceOffline)?;
        let ticket = call.ticket;
        // Room was reserved above.
        volume.calls.push(call);
        volume
            .alive_at
            .get_or_insert(now.saturating_add(ALIVE_EVERY_NS));
        if reach == MountState::Reconnecting {
            return Ok(None);
        }
        let request = self.next_request_id();
        if let Err(error) = self.order_call(now, offer, ticket, request) {
            if let Some(volume) = self.volumes.get_mut(&offer.get()) {
                volume.take_call(ticket);
            }
            return Err(error);
        }
        Ok(None)
    }

    /// How the volume `offer` names reaches its share: served, waiting for
    /// its connection to be made again, or past waiting.
    pub(super) fn reach(&self, offer: OfferId) -> MountState {
        self.volumes
            .get(&offer.get())
            .and_then(|volume| self.conns.get(volume.conn))
            .map_or(MountState::Lost, Connection::state)
    }

    /// The volume a file operation's report is for, if it is for one.
    pub(super) fn route(&self, request: u32) -> Option<OfferId> {
        self.routes.get(&request).copied().and_then(OfferId::new)
    }

    /// Act on a file operation's report.
    ///
    /// # Errors
    ///
    /// A worker that reported what no honest worker does.
    pub(super) fn on_file_report(
        &mut self,
        now: u64,
        offer: OfferId,
        report: &Report<'_>,
    ) -> Result<(), Lie> {
        let request = report.file_request().ok_or(Lie)?;
        // An entry or a chunk of data is not an operation's last report.
        let last = !matches!(report, Report::Data { .. } | Report::Entry { .. });
        if let Some(&node) = self
            .volumes
            .get(&offer.get())
            .and_then(|volume| volume.held.ops.get(&request))
        {
            return self.held_report(now, offer, NodeId::from_raw(node), request, report);
        }
        if let Some(handle) = self.listing_of(offer, request) {
            return self.on_listing_report(now, offer, handle, request, report);
        }
        let Self {
            volumes,
            host,
            routes,
            ..
        } = self;
        let Some(volume) = volumes.get_mut(&offer.get()) else {
            return Ok(());
        };
        let Some(at) = volume
            .calls
            .iter()
            .position(|call| call.request == Some(request))
        else {
            // An operation whose call or listing went first: it is reported
            // to no one.
            if last {
                routes.remove(&request);
            }
            return Ok(());
        };
        let Some(settled) = settle(volume, at, |span| host.window(offer, span), report)? else {
            return Ok(());
        };
        let mut call = volume.calls.swap_remove(at);
        routes.remove(&request);
        match settled {
            Settled::Answer(result) => {
                let effect = match (&call.work, &result) {
                    (Work::Write(_, _, offset, _), Ok(Owned::Count(count))) => Effect::Wrote {
                        offset: *offset,
                        count: u64::from(*count),
                    },
                    (Work::Truncate(_, _, size), Ok(_)) => Effect::Resized(*size),
                    _ => Effect::Nothing,
                };
                let answer = result
                    .as_ref()
                    .map(Owned::as_answer)
                    .map_err(|error| *error);
                self.served_answer(offer, call.ticket, call.op, answer);
                self.learned(now, offer, &call.work, &result, report);
                self.release_spans(offer);
                if let Some(node) = call.held {
                    self.held_call_done(now, offer, node, call.ticket, effect);
                }
            }
            Settled::Next(work) => {
                let (ticket, op) = (call.ticket, call.op);
                call.work = work;
                if let Err(error) = self.hold_on(now, offer, call) {
                    self.served_answer(offer, ticket, op, Err(error));
                }
            }
        }
        Ok(())
    }

    /// Answer every release no read still writes into.
    pub(super) fn release_spans(&mut self, offer: OfferId) {
        let mut at = 0;
        while let Some(volume) = self.volumes.get_mut(&offer.get()) {
            let Some(call) = volume.calls.get(at) else {
                return;
            };
            if !matches!(call.work, Work::Release(span) if !volume.reading_into(span)) {
                at += 1;
                continue;
            }
            let ticket = volume.calls.swap_remove(at).ticket;
            self.served_answer(offer, ticket, Op::Release, Ok(Answer::Done));
        }
    }

    /// Answer every call of `offer`'s past its deadline as timed out,
    /// allocating nothing, so a machine out of memory still sees each one
    /// answered. One a held file's work answers meanwhile is gone by the time
    /// it would be reached.
    fn time_out_calls(&mut self, now: u64, offer: OfferId) {
        let mut at = 0;
        while let Some(volume) = self.volumes.get_mut(&offer.get()) {
            let Some(call) = volume.calls.get(at) else {
                return;
            };
            if call.deadline > now {
                at += 1;
                continue;
            }
            let call = volume.calls.swap_remove(at);
            if let Some(request) = call.request {
                self.routes.remove(&request);
            }
            self.served_answer(offer, call.ticket, call.op, Err(DriverError::TimedOut));
            if let Some(node) = call.held {
                self.held_call_done(now, offer, node, call.ticket, Effect::Nothing);
            }
        }
    }

    /// Let go of each of `offer`'s directory leases left unused past
    /// [`DIR_LEASE_IDLE_NS`].
    fn let_idle_dirs_go(&mut self, now: u64, offer: OfferId) {
        while let Some(volume) = self.volumes.get_mut(&offer.get()) {
            let Some(at) = volume
                .dir_leases
                .iter()
                .position(|lease| lease.used.saturating_add(DIR_LEASE_IDLE_NS) <= now)
            else {
                return;
            };
            let lease = volume.dir_leases.swap_remove(at);
            self.let_dir_go(offer, lease.handle, None);
        }
    }

    /// The listing an open or a page in flight as `request` is for.
    fn listing_of(&self, offer: OfferId, request: u32) -> Option<u32> {
        self.volumes
            .get(&offer.get())?
            .listings
            .iter()
            .find(|listing| listing.busy == Some(request))
            .map(|listing| listing.handle)
    }

    /// Answer a listing batch from `cursor`, or hold it on its listing's
    /// next page.
    fn list(
        &mut self,
        now: u64,
        offer: OfferId,
        ticket: u64,
        dir: NodeId,
        cursor: u64,
    ) -> Result<Option<Local>, DriverError> {
        let volume = self
            .volumes
            .get_mut(&offer.get())
            .ok_or(DriverError::DeviceOffline)?;
        let found = dir_of(volume, dir)?;
        volume
            .calls
            .try_reserve(1)
            .map_err(|_| DriverError::OutOfMemory)?;
        let reusable = volume.listings.iter().position(|listing| {
            listing.dir == dir && listing.position == cursor && listing.busy.is_none()
        });
        let handle = if let Some(at) = reusable {
            volume.listings[at].handle
        } else {
            // A cursor no open listing stands at: the listing starts again and
            // passes over what was handed out already.
            if volume.listings.len() >= LISTINGS_MAX {
                let idle = volume
                    .listings
                    .iter()
                    .enumerate()
                    .filter(|(_, listing)| listing.busy.is_none())
                    .min_by_key(|(_, listing)| listing.used)
                    .map(|(at, _)| at)
                    .ok_or(DriverError::Busy)?;
                let closed = volume.listings.swap_remove(idle);
                self.close_listing(offer, &closed);
            }
            // The worker names an open on its connection by this, across
            // every volume the connection serves.
            let handle = self.next_handle();
            // A listing read whole within the second answers one from its
            // start without the server.
            let kept = if cursor == 0 {
                self.kept_listing(now, offer, dir)
            } else {
                None
            };
            let keeping = cursor == 0 && kept.is_none() && self.snapshots.is_some();
            let lease = self.listing_lease(offer, dir);
            let volume = self
                .volumes
                .get_mut(&offer.get())
                .ok_or(DriverError::DeviceOffline)?;
            volume
                .listings
                .try_reserve(1)
                .map_err(|_| DriverError::OutOfMemory)?;
            let from_kept = kept.is_some();
            volume.listings.push(Listing {
                dir,
                path: found.path,
                handle,
                position: 0,
                skip: cursor,
                page: kept.map(VecDeque::from).unwrap_or_default(),
                opened: false,
                more: !from_kept,
                busy: None,
                used: now,
                kept: keeping.then(Vec::new),
                lease: lease.map(|key| (key, false)),
            });
            handle
        };
        let volume = self
            .volumes
            .get_mut(&offer.get())
            .ok_or(DriverError::DeviceOffline)?;
        // Room was reserved above.
        volume
            .calls
            .push(Served::new(now, ticket, Op::ReadDir, Work::List(handle)));
        volume
            .alive_at
            .get_or_insert(now.saturating_add(ALIVE_EVERY_NS));
        self.advance_listing(now, offer, handle);
        Ok(None)
    }

    /// Answer the batch waiting on listing `handle` when it can be answered;
    /// otherwise ask for what it waits on.
    fn advance_listing(&mut self, now: u64, offer: OfferId, handle: u32) {
        let Some(volume) = self.volumes.get_mut(&offer.get()) else {
            return;
        };
        let Some(listing) = volume.listing_mut(handle) else {
            return;
        };
        listing.used = now;
        while listing.skip > 0 && !listing.page.is_empty() {
            listing.page.pop_front();
            listing.skip -= 1;
            listing.position += 1;
        }
        if listing.busy.is_some() {
            return;
        }
        // A cursor past the end, as a directory others shrank leaves one,
        // ends the listing with whatever is left to skip.
        if (listing.skip == 0 && !listing.page.is_empty()) || !listing.more {
            self.answer_listing(now, offer, handle);
            return;
        }
        match self.reach(offer) {
            MountState::Serving => {}
            MountState::Reconnecting => return,
            MountState::Lost => {
                self.fail_listing(offer, handle, DriverError::DeviceOffline);
                return;
            }
        }
        let request = self.next_request_id();
        let Self {
            volumes,
            conns,
            routes,
            ..
        } = self;
        let Some(volume) = volumes.get_mut(&offer.get()) else {
            return;
        };
        let (tree, conn) = (volume.tree, volume.conn);
        let Some(listing) = volume.listing_mut(handle) else {
            return;
        };
        let op = match (listing.opened, listing.lease) {
            (true, _) => FileOp::Page { handle },
            (false, Some((key, _))) => FileOp::Hold {
                path: listing.path.as_str(),
                handle,
                key,
                directory: true,
                write: false,
                durability: Durability::None,
            },
            (false, None) => FileOp::Open {
                path: listing.path.as_str(),
                handle,
            },
        };
        let ordered = routes.try_reserve(1).is_ok()
            && conns
                .get_mut(conn)
                .and_then(|conn| conn.link.as_mut())
                .is_some_and(|link| link.order(&Order::File { request, tree, op }).is_ok());
        if ordered {
            listing.busy = Some(request);
            // Room was reserved above.
            let _ = routes.try_insert(request, offer.get());
        } else {
            self.fail_listing(offer, handle, DriverError::DeviceOffline);
        }
    }

    /// Hand the batch waiting on listing `handle` what its page holds.
    fn answer_listing(&mut self, now: u64, offer: OfferId, handle: u32) {
        let Self {
            volumes,
            served_reply,
            host,
            attrs,
            ..
        } = self;
        let Some(volume) = volumes.get_mut(&offer.get()) else {
            return;
        };
        let (Some(listing_at), Some(at)) = (
            volume
                .listings
                .iter()
                .position(|listing| listing.handle == handle),
            volume
                .calls
                .iter()
                .position(|call| matches!(call.work, Work::List(held) if held == handle)),
        ) else {
            return;
        };
        let ticket = volume.calls.swap_remove(at).ticket;
        let Ok(mut reply) = DirReply::new(served_reply) else {
            self.served_answer(offer, ticket, Op::ReadDir, Err(DriverError::DeviceFault));
            return;
        };
        let (nodes, listings) = (&mut volume.nodes, &mut volume.listings);
        let listing = &mut listings[listing_at];
        // What the listing read under a lease granted with reads stands
        // until the lease breaks.
        let under = listing
            .lease
            .filter(|(_, granted)| *granted)
            .map(|(key, _)| key);
        let mut failed = None;
        while let Some((name, stat)) = listing.page.front() {
            let Ok(path) = listing.path.join(name) else {
                listing.page.pop_front();
                listing.position += 1;
                continue;
            };
            let Ok(id) = nodes.name(&path, stat.file_id, stat.directory) else {
                failed = Some(DriverError::OutOfMemory);
                break;
            };
            let entry = DirEntry {
                node: id,
                info: node_info(id, stat),
                next_cursor: listing.position + 1,
            };
            if reply.push(&entry, name.as_bytes()).is_err() {
                break;
            }
            if let Some(cache) = attrs.as_mut() {
                let under = under.map(|key| (listing.dir, key));
                cache.retain(
                    &(),
                    meta::NodeKey::of(offer, id),
                    meta::attr(*stat, now, under),
                );
            }
            listing.page.pop_front();
            listing.position += 1;
        }
        let end = match failed {
            Some(error) if reply.is_empty() => DirEnd::Failed(error),
            _ if listing.page.is_empty() && !listing.more => DirEnd::Ended,
            _ if reply.is_empty() => DirEnd::Failed(DriverError::DeviceFault),
            _ => DirEnd::More,
        };
        let finished = matches!(end, DirEnd::Ended | DirEnd::Failed(_));
        let state = ServerState {
            unpublished: volume.held.unpublished(),
            in_reserve: false,
        };
        let len = reply.finish(end, state).unwrap_or(0);
        host.served_reply(offer, ticket, &served_reply[..len]);
        if finished {
            let mut closed = volume.listings.swap_remove(listing_at);
            let ended = matches!(end, DirEnd::Ended);
            if let (true, Some(kept)) = (ended, closed.kept.take()) {
                self.keep_listing(now, offer, closed.dir, kept, under);
            }
            match under {
                Some(key) if ended && self.hold_dir_lease(now, offer, &closed, key) => {}
                _ => self.close_listing(offer, &closed),
            }
        }
    }

    /// The lease a new listing of `dir` opens it under: the one the front
    /// holds on it already, or a fresh one where the server leases
    /// directories.
    fn listing_lease(&mut self, offer: OfferId, dir: NodeId) -> Option<[u8; 16]> {
        let volume = self.volumes.get(&offer.get())?;
        if !volume.dir_leasing {
            return None;
        }
        if let Some(key) = volume.dir_lease(dir) {
            return Some(key);
        }
        let mut key = [0; 16];
        self.host.fill_random(&mut key).ok()?;
        Some(key)
    }

    /// Keep `listing`'s open as the holder of the lease `key` on its
    /// directory; whether it is kept, rather than closed, since another open
    /// already holds the lease.
    fn hold_dir_lease(
        &mut self,
        now: u64,
        offer: OfferId,
        listing: &Listing,
        key: [u8; 16],
    ) -> bool {
        let Some(volume) = self.volumes.get_mut(&offer.get()) else {
            return false;
        };
        if volume.dir_lease(listing.dir).is_some() || volume.dir_leases.try_reserve(1).is_err() {
            return false;
        }
        let evicted = (volume.dir_leases.len() >= DIR_LEASES_MAX)
            .then(|| {
                volume
                    .dir_leases
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, lease)| lease.used)
                    .map(|(at, _)| at)
            })
            .flatten()
            .map(|at| volume.dir_leases.swap_remove(at));
        volume.dir_leases.push(DirLease {
            dir: listing.dir,
            handle: listing.handle,
            key,
            used: now,
        });
        if let Some(evicted) = evicted {
            self.let_dir_go(offer, evicted.handle, None);
        }
        true
    }

    /// Close the open `handle` held a directory's lease with, acknowledging
    /// the lease's break first when `ack` names it.
    fn let_dir_go(&mut self, offer: OfferId, handle: u32, ack: Option<([u8; 16], u8)>) {
        let Some((conn, tree)) = self
            .volumes
            .get(&offer.get())
            .map(|volume| (volume.conn, volume.tree))
        else {
            return;
        };
        let mut ops: ArrayVec<FileOp<'_>, 2> = ArrayVec::new();
        if let Some((key, state)) = ack {
            let _ = ops.try_push(FileOp::Ack { key, state });
        }
        let _ = ops.try_push(FileOp::Close { handle });
        for op in ops {
            let request = self.next_request_id();
            if let Some(link) = self.conns.get_mut(conn).and_then(|conn| conn.link.as_mut()) {
                // Orders the connection cannot take are moot: its opens and
                // leases go with it.
                let _ = link.order(&Order::File { request, tree, op });
            }
        }
    }

    /// The server is breaking the lease `key` on one of `offer`'s
    /// directories; whether it is one of the volume's. What was read under
    /// it no longer stands, and its open is let go.
    pub(super) fn dir_break(&mut self, offer: OfferId, key: [u8; 16], new: u8, ack: bool) -> bool {
        let Some(volume) = self.volumes.get_mut(&offer.get()) else {
            return false;
        };
        let Some(at) = volume.dir_leases.iter().position(|lease| lease.key == key) else {
            return false;
        };
        let lease = volume.dir_leases.swap_remove(at);
        self.let_dir_go(offer, lease.handle, ack.then_some((key, new)));
        true
    }

    /// Fail the batch waiting on listing `handle`, and let the listing go.
    fn fail_listing(&mut self, offer: OfferId, handle: u32, error: DriverError) {
        let Some(volume) = self.volumes.get_mut(&offer.get()) else {
            return;
        };
        let waiting = volume
            .calls
            .iter()
            .position(|call| matches!(call.work, Work::List(held) if held == handle))
            .map(|at| volume.calls.swap_remove(at).ticket);
        let closed = volume
            .listings
            .iter()
            .position(|listing| listing.handle == handle)
            .map(|at| volume.listings.swap_remove(at));
        if let Some(ticket) = waiting {
            self.served_answer(offer, ticket, Op::ReadDir, Err(error));
        }
        if let Some(closed) = closed {
            self.close_listing(offer, &closed);
        }
    }

    /// Close `listing`'s directory on the server, if it was opened or its
    /// open is in flight: the worker closes it once it is open.
    fn close_listing(&mut self, offer: OfferId, listing: &Listing) {
        if !listing.opened && listing.busy.is_none() {
            return;
        }
        let Some((conn, tree)) = self
            .volumes
            .get(&offer.get())
            .map(|volume| (volume.conn, volume.tree))
        else {
            return;
        };
        let request = self.next_request_id();
        if let Some(link) = self.conns.get_mut(conn).and_then(|conn| conn.link.as_mut()) {
            // A close the connection cannot take is moot: the directory is
            // closed with the connection.
            let _ = link.order(&Order::File {
                request,
                tree,
                op: FileOp::Close {
                    handle: listing.handle,
                },
            });
        }
    }

    /// Act on a listing's open or page report.
    fn on_listing_report(
        &mut self,
        now: u64,
        offer: OfferId,
        handle: u32,
        request: u32,
        report: &Report<'_>,
    ) -> Result<(), Lie> {
        let Some(volume) = self.volumes.get_mut(&offer.get()) else {
            return Ok(());
        };
        let Some(listing) = volume.listing_mut(handle) else {
            return Ok(());
        };
        match *report {
            Report::Done { .. } if !listing.opened => {
                listing.opened = true;
                listing.busy = None;
                self.routes.remove(&request);
            }
            Report::Held { lease, .. } if !listing.opened && listing.lease.is_some() => {
                listing.opened = true;
                listing.busy = None;
                listing.lease = listing.lease.map(|(key, _)| (key, lease & 0b001 != 0));
                self.routes.remove(&request);
            }
            Report::Entry { name, stat, .. } if listing.opened => {
                if listing.page.len() >= crate::files::PAGE_ENTRIES_MAX {
                    return Err(Lie);
                }
                let mut held = String::new();
                if held.try_reserve_exact(name.len()).is_err()
                    || listing.page.try_reserve(1).is_err()
                {
                    self.fail_listing(offer, handle, DriverError::OutOfMemory);
                    return Ok(());
                }
                held.push_str(name);
                if let Some(kept) = listing.kept.as_mut() {
                    let mut copy = String::new();
                    let room = kept.len() < meta::SNAPSHOT_MAX
                        && kept.try_reserve(1).is_ok()
                        && copy.try_reserve_exact(name.len()).is_ok();
                    if room {
                        copy.push_str(name);
                        kept.push((copy, stat));
                    } else {
                        listing.kept = None;
                    }
                }
                listing.page.push_back((held, stat));
                return Ok(());
            }
            Report::Page { more, .. } if listing.opened => {
                listing.more = more;
                listing.busy = None;
                self.routes.remove(&request);
            }
            Report::Refused { error, .. } => {
                // Nothing was opened, so nothing is closed.
                listing.busy = None;
                self.routes.remove(&request);
                self.fail_listing(offer, handle, driver_error(error));
                return Ok(());
            }
            _ => return Err(Lie),
        }
        self.advance_listing(now, offer, handle);
        Ok(())
    }

    /// Act on what is due on `offer`'s volume by `now`: calls past their
    /// deadline answered timed out, listings and directory leases left idle
    /// let go, and the kernel told the volume is working.
    pub(super) fn expire_volume(&mut self, now: u64, offer: OfferId) {
        self.time_out_calls(now, offer);
        self.let_idle_dirs_go(now, offer);
        let Some(volume) = self.volumes.get_mut(&offer.get()) else {
            return;
        };
        let mut idle: ArrayVec<u32, LISTINGS_MAX> = ArrayVec::new();
        for listing in &volume.listings {
            if listing.busy.is_none() && listing.used.saturating_add(LISTING_IDLE_NS) <= now {
                // As many as there are listings, so all fit.
                let _ = idle.try_push(listing.handle);
            }
        }
        let due = volume.alive_at.is_some_and(|at| at <= now);
        let alive = due && !volume.calls.is_empty();
        volume.alive_at = if volume.calls.is_empty() {
            None
        } else if due || volume.alive_at.is_none() {
            Some(now.saturating_add(ALIVE_EVERY_NS))
        } else {
            volume.alive_at
        };
        self.expire_held(now, offer);
        if self
            .volumes
            .get(&offer.get())
            .and_then(|volume| volume.retiring.as_ref())
            .is_some_and(|retiring| retiring.by <= now)
        {
            self.retired(now, offer);
            return;
        }
        for handle in idle {
            self.fail_listing(offer, handle, DriverError::TimedOut);
        }
        if alive {
            self.host.alive(offer);
        }
        self.release_spans(offer);
    }

    /// `offer`'s connection dropped: what it had in flight waits to be
    /// asked again once the connection is made again, but for a change by
    /// name, which might have been made and is answered offline. What the
    /// connection held open on the server — listings, directory leases —
    /// went with it.
    pub(super) fn suspend_volume(&mut self, offer: OfferId) {
        let mut at = self
            .volumes
            .get(&offer.get())
            .map_or(0, |volume| volume.calls.len());
        while at > 0 {
            at -= 1;
            let Self {
                volumes, routes, ..
            } = self;
            let Some(volume) = volumes.get_mut(&offer.get()) else {
                return;
            };
            let call = &mut volume.calls[at];
            let Some(request) = call.request.take() else {
                continue;
            };
            routes.remove(&request);
            if let (Some(node), Some(op)) = (call.held, call.asked) {
                call.work = Work::Held(node, op);
            } else if !call.work.repeatable() {
                let call = volume.calls.swap_remove(at);
                self.served_answer(offer, call.ticket, call.op, Err(DriverError::DeviceOffline));
            } else if let Work::Read { got, .. } = &mut call.work {
                // Asked again from its start.
                *got = 0;
            }
        }
        let Self {
            volumes, routes, ..
        } = self;
        let Some(volume) = volumes.get_mut(&offer.get()) else {
            return;
        };
        let waiting = |listing: &Listing| {
            volume
                .calls
                .iter()
                .any(|call| matches!(call.work, Work::List(held) if held == listing.handle))
        };
        let mut kept = 0;
        for at in 0..volume.listings.len() {
            if let Some(request) = volume.listings[at].busy.take() {
                routes.remove(&request);
            }
            if waiting(&volume.listings[at]) {
                volume.listings.swap(kept, at);
                kept += 1;
            }
        }
        volume.listings.truncate(kept);
        for listing in &mut volume.listings {
            // Read again from its start, past what was handed out already.
            listing.skip = listing.position;
            listing.position = 0;
            listing.page.clear();
            listing.opened = false;
            listing.more = true;
            listing.kept = None;
            listing.lease = listing.lease.map(|(key, _)| (key, false));
        }
        volume.dir_leases.clear();
        self.suspend_held(offer);
    }

    /// `offer`'s connection is made again: its held files are taken back
    /// before their work goes on, and what waited is asked.
    pub(super) fn resume_volume(&mut self, now: u64, offer: OfferId) {
        while let Some(ticket) = self.volumes.get(&offer.get()).and_then(|volume| {
            volume
                .calls
                .iter()
                .find(|call| call.request.is_none() && call.work.file_op().is_some())
                .map(|call| call.ticket)
        }) {
            let request = self.next_request_id();
            if let Err(error) = self.order_call(now, offer, ticket, request) {
                let taken = self
                    .volumes
                    .get_mut(&offer.get())
                    .and_then(|volume| volume.take_call(ticket));
                if let Some((ticket, op)) = taken {
                    self.served_answer(offer, ticket, op, Err(error));
                }
            }
        }
        let mut listings: ArrayVec<u32, LISTINGS_MAX> = ArrayVec::new();
        if let Some(volume) = self.volumes.get(&offer.get()) {
            for listing in &volume.listings {
                // As many as there are listings, so all fit.
                let _ = listings.try_push(listing.handle);
            }
        }
        for handle in listings {
            self.advance_listing(now, offer, handle);
        }
        self.resume_held(now, offer);
    }

    /// `offer`'s connection was not made again within its grace window:
    /// what its files hold written is lost, and every call it holds is
    /// answered offline, as each one after it is until the connection is.
    pub(super) fn lose_volume(&mut self, offer: OfferId) {
        self.lose_held(offer);
        let Some(volume) = self.volumes.get_mut(&offer.get()) else {
            return;
        };
        let calls = core::mem::take(&mut volume.calls);
        let listings = core::mem::take(&mut volume.listings);
        volume.held.forget_flushes();
        for call in &calls {
            if let Some(request) = call.request {
                self.routes.remove(&request);
            }
        }
        for listing in &listings {
            if let Some(request) = listing.busy {
                self.routes.remove(&request);
            }
        }
        let mut releases = Vec::new();
        for call in calls {
            if let Work::Release(_) = call.work {
                if releases.try_reserve(1).is_ok() {
                    releases.push(call.ticket);
                    continue;
                }
            }
            self.served_answer(offer, call.ticket, call.op, Err(DriverError::DeviceOffline));
        }
        // No read is left to write into a released span.
        for ticket in releases {
            self.served_answer(offer, ticket, Op::Release, Ok(Answer::Done));
        }
    }

    /// Answer every call `offer`'s volume holds as offline, its listings let
    /// go with it. Bytes it holds written are lost, and every answer says
    /// so, so the kernel keeps the volume as lost rather than letting it go.
    pub(super) fn abandon_volume(&mut self, offer: OfferId) {
        let unsent = self
            .volumes
            .get(&offer.get())
            .map_or(0, |volume| volume.held.unsent_bytes());
        if unsent > 0 {
            self.lost_bytes(offer, unsent);
        }
        let Some(volume) = self.volumes.remove(&offer.get()) else {
            return;
        };
        let state = ServerState {
            unpublished: unsent > 0,
            in_reserve: false,
        };
        for call in &volume.calls {
            if let Some(request) = call.request {
                self.routes.remove(&request);
            }
            let out = &mut self.served_reply;
            let len =
                encode_reply(call.op, state, Err(DriverError::DeviceOffline), out).unwrap_or(0);
            self.host.served_reply(offer, call.ticket, &out[..len]);
        }
        for listing in &volume.listings {
            if let Some(request) = listing.busy {
                self.routes.remove(&request);
            }
        }
        for request in volume.held.ops.keys() {
            self.routes.remove(request);
        }
    }
}

#[cfg(test)]
#[path = "served_tests.rs"]
pub(super) mod tests;
