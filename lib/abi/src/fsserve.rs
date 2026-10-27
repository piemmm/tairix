//! `fsserve-v1`: the protocol a user-space filesystem driver serves its volume
//! to the kernel over (`plans/FSSERVE.md` §1).
//!
//! The kernel posts one [`Request`] on the driver's call endpoint and the
//! driver answers one reply. Both travel inline; file data alone moves
//! through the shared data window, at the [`Span`] a [`Request::ReadAt`] or
//! [`Request::WriteAt`] names. The requests are the operations of the
//! filesystem driver traits, so a served volume answers exactly what an
//! in-kernel driver would.
//!
//! Each side decodes what the other sent as untrusted. The kernel holds a
//! reply to the request it asked ([`decode_reply`]): a count no larger than
//! the span, a name that is a path component, a security record within its
//! bounds. The driver refuses a request the kernel could not have sent
//! ([`decode_request`]).

use core::num::NonZeroU64;

use crate::driver::filesystem::{
    AclModel, AllocationClass, DirEntry, NameMatching, NodeId, NodeIdentity, NodeInfo, NodeKind,
    NodeSecurity, NodeTimes, SecurityAcl, SecuritySubject, Unpinned, VolumeStats, WriteRefusal,
    MAX_ACL_ENTRIES,
};
use crate::fs::{
    is_component, SeekRegion, FS_ATTR_KEY_MAX, FS_ATTR_VALUE_MAX, FS_MODE_MASK, FS_NAME_MAX,
    FS_SYMLINK_MAX,
};
use crate::le::{
    exact_len, put_i32, put_u16, put_u32, put_u64, read_i32, read_u16, read_u32, read_u64, Reader,
    Writer,
};
use crate::time::Time64;
use crate::volume::FormatName;
use crate::{CapabilityId, DriverError, Errno};

/// The protocol version every frame carries.
pub const FSSERVE_VERSION: u16 = 1;

/// Length of the shared data window.
///
/// A transfer granule, not a capacity: a larger read or write is split into
/// calls of at most this much, and a quarter mebibyte is enough that the
/// per-call cost vanishes beside a removable medium's own transfer time.
pub const FSSERVE_WINDOW_LEN: usize = 256 * 1024;

/// The first call-endpoint id the kernel creates for the drivers serving
/// volumes: one fresh id per accepted serve, never reused in a boot. No
/// process creates an endpoint in this range itself, so a served volume's
/// endpoint is always one the kernel made for it.
pub const SERVED_ENDPOINT_BASE: u64 = 0x4653_0000_0000_0000;

/// How many ids the [`SERVED_ENDPOINT_BASE`] range spans.
const SERVED_ENDPOINT_SPAN: u64 = 1 << u32::BITS;

/// The `serial`-th endpoint id the kernel creates for a served volume.
#[must_use]
pub const fn served_endpoint(serial: u32) -> u64 {
    SERVED_ENDPOINT_BASE + serial as u64
}

/// Whether `id` is one the kernel creates for a served volume.
#[must_use]
pub const fn is_served_endpoint(id: u64) -> bool {
    id >= SERVED_ENDPOINT_BASE && id < SERVED_ENDPOINT_BASE + SERVED_ENDPOINT_SPAN
}

/// The kernel's name for a network volume a server has offered to one
/// principal, which that principal attaches (`plans/FSSERVE.md` FSS6).
/// Never zero, and never reused in a boot.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct OfferId(NonZeroU64);

impl OfferId {
    /// The offer `raw` names, or `None` for zero, which names none.
    #[must_use]
    pub const fn new(raw: u64) -> Option<Self> {
        match NonZeroU64::new(raw) {
            Some(raw) => Some(Self(raw)),
            None => None,
        }
    }

    /// Its raw value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Request header: version (2), operation (1), flags (1).
const REQUEST_HEAD_LEN: usize = 4;

/// Reply header: version (2), operation (1), server state (1), status (4).
const REPLY_HEAD_LEN: usize = 8;

/// The largest request: a [`Request::CreateLink`], whose name, target and
/// record are each at their bounds.
pub const FSSERVE_REQUEST_MAX: usize =
    REQUEST_HEAD_LEN + 8 + 2 + FS_NAME_MAX + 2 + FS_SYMLINK_MAX + SECURITY_MAX_LEN;

/// The largest reply: a listing batch, sized so a directory of a few hundred
/// entries is answered in one call.
pub const FSSERVE_REPLY_MAX: usize = 1 << 16;

/// The request flag carrying [`AllocationClass::System`].
const FLAG_SYSTEM: u8 = 1 << 0;

/// The reply state bit for [`ServerState::unpublished`].
const STATE_UNPUBLISHED: u8 = 1 << 0;

/// The reply state bit for [`ServerState::in_reserve`].
const STATE_IN_RESERVE: u8 = 1 << 1;

/// Wire length of a [`NodeInfo`].
const NODE_INFO_LEN: usize = 1 + 4 + 8 + 8 + 8 + 4 * Time64::WIRE_LEN + 8;

/// Wire length of a listing entry less its name.
const ENTRY_FIXED_LEN: usize = 8 + NODE_INFO_LEN + 8 + 2;

/// Wire length of a listing batch's trailer: how it ended, and the error.
const BATCH_TRAILER_LEN: usize = 1 + 4;

/// Wire length of a security record holding every entry it can: mode, owner,
/// group, the capability gate, the ACL model, the entry count, then a tag, an
/// id and a triad per entry.
const SECURITY_MAX_LEN: usize = 4 + 4 + 4 + 1 + 2 + 3 + 1 + MAX_ACL_ENTRIES * (1 + 4 + 1);

/// An `rwx` triad.
const TRIAD_MAX: u8 = 0o7;

/// One operation of the protocol, as its frames name it.
#[repr(u8)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Op {
    /// [`Request::NodeInfo`].
    NodeInfo = 1,
    /// [`Request::Lookup`].
    Lookup = 2,
    /// [`Request::Parent`].
    Parent = 3,
    /// [`Request::ReadAt`].
    ReadAt = 4,
    /// [`Request::ReadLink`].
    ReadLink = 5,
    /// [`Request::SeekRegion`].
    SeekRegion = 6,
    /// [`Request::ReadDir`].
    ReadDir = 7,
    /// [`Request::Pin`].
    Pin = 8,
    /// [`Request::Unpin`].
    Unpin = 9,
    /// [`Request::ReleasePins`].
    ReleasePins = 10,
    /// [`Request::Create`].
    Create = 11,
    /// [`Request::CreateLink`].
    CreateLink = 12,
    /// [`Request::Link`].
    Link = 13,
    /// [`Request::WriteAt`].
    WriteAt = 14,
    /// [`Request::Truncate`].
    Truncate = 15,
    /// [`Request::Punch`].
    Punch = 16,
    /// [`Request::Remove`].
    Remove = 17,
    /// [`Request::Rename`].
    Rename = 18,
    /// [`Request::Flush`].
    Flush = 19,
    /// [`Request::Security`].
    Security = 20,
    /// [`Request::SetSecurity`].
    SetSecurity = 21,
    /// [`Request::RecordsOwnership`].
    RecordsOwnership = 22,
    /// [`Request::Stats`].
    Stats = 23,
    /// [`Request::GetAttr`].
    GetAttr = 24,
    /// [`Request::SetAttr`].
    SetAttr = 25,
    /// [`Request::ListAttr`].
    ListAttr = 26,
    /// [`Request::RemoveAttr`].
    RemoveAttr = 27,
    /// [`Request::Release`].
    Release = 28,
}

impl Op {
    const fn from_u8(raw: u8) -> Result<Self, Errno> {
        Ok(match raw {
            1 => Self::NodeInfo,
            2 => Self::Lookup,
            3 => Self::Parent,
            4 => Self::ReadAt,
            5 => Self::ReadLink,
            6 => Self::SeekRegion,
            7 => Self::ReadDir,
            8 => Self::Pin,
            9 => Self::Unpin,
            10 => Self::ReleasePins,
            11 => Self::Create,
            12 => Self::CreateLink,
            13 => Self::Link,
            14 => Self::WriteAt,
            15 => Self::Truncate,
            16 => Self::Punch,
            17 => Self::Remove,
            18 => Self::Rename,
            19 => Self::Flush,
            20 => Self::Security,
            21 => Self::SetSecurity,
            22 => Self::RecordsOwnership,
            23 => Self::Stats,
            24 => Self::GetAttr,
            25 => Self::SetAttr,
            26 => Self::ListAttr,
            27 => Self::RemoveAttr,
            28 => Self::Release,
            _ => return Err(Errno::OutOfRange),
        })
    }
}

/// Where in the shared data window a request's file data lies: never empty,
/// and never past the window's end.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Span {
    offset: u32,
    len: u32,
}

impl Span {
    /// The `len` bytes at `offset`.
    ///
    /// # Errors
    ///
    /// [`Errno::LengthOutOfRange`] for an empty span or one reaching past
    /// [`FSSERVE_WINDOW_LEN`].
    pub fn new(offset: usize, len: usize) -> Result<Self, Errno> {
        let end = offset.checked_add(len).ok_or(Errno::LengthOutOfRange)?;
        if len == 0 || end > FSSERVE_WINDOW_LEN {
            return Err(Errno::LengthOutOfRange);
        }
        Ok(Self {
            offset: u32::try_from(offset).map_err(|_| Errno::LengthOutOfRange)?,
            len: u32::try_from(len).map_err(|_| Errno::LengthOutOfRange)?,
        })
    }

    /// The span's first byte within the window.
    #[must_use]
    pub const fn offset(self) -> usize {
        self.offset as usize
    }

    /// How many bytes the span covers: never none.
    #[must_use]
    pub const fn size(self) -> usize {
        self.len as usize
    }

    /// The window bytes the span covers.
    #[must_use]
    pub const fn range(self) -> core::ops::Range<usize> {
        self.offset()..self.offset() + self.size()
    }
}

/// One call the kernel makes of a served volume: an operation of the
/// filesystem driver traits, carrying exactly that operation's arguments.
///
/// Every name is a path component ([`is_component`]), every node is a live
/// id rather than [`NodeId::NONE`], and every bound the trait states holds;
/// neither side can build one that breaks them.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Request<'a> {
    /// `FilesystemRead::node_info`.
    NodeInfo {
        /// The node.
        node: NodeId,
    },
    /// `FilesystemRead::lookup`.
    Lookup {
        /// The directory.
        dir: NodeId,
        /// The component looked up.
        name: &'a [u8],
    },
    /// `FilesystemRead::parent`.
    Parent {
        /// The directory.
        dir: NodeId,
    },
    /// `FilesystemRead::read_at`, into the window at `span`.
    ReadAt {
        /// The file.
        file: NodeId,
        /// The first byte read.
        offset: u64,
        /// Where the bytes land, and how many are wanted.
        span: Span,
    },
    /// `FilesystemRead::read_link` into a buffer of `cap` bytes.
    ReadLink {
        /// The link.
        link: NodeId,
        /// The caller's buffer length, clamped to [`FS_SYMLINK_MAX`].
        cap: u32,
    },
    /// `FilesystemRead::seek_region`.
    SeekRegion {
        /// The file.
        file: NodeId,
        /// Where the search starts.
        offset: u64,
        /// The kind of region sought.
        region: SeekRegion,
    },
    /// `FilesystemRead::read_dir`, one batch from `cursor`.
    ReadDir {
        /// The directory.
        dir: NodeId,
        /// The resume token; `0` starts the listing.
        cursor: u64,
        /// The name of the entry `cursor` came from; empty at the start.
        after: &'a [u8],
    },
    /// `FilesystemRead::pin`.
    Pin {
        /// The node held.
        node: NodeId,
    },
    /// `FilesystemRead::unpin`.
    Unpin {
        /// The id a pin answered.
        pinned: NodeId,
    },
    /// `FilesystemRead::release_pins`.
    ReleasePins,
    /// `FilesystemWrite::create`.
    Create {
        /// The parent directory.
        dir: NodeId,
        /// The new child's name.
        name: &'a [u8],
        /// The new child's kind: never a link, which carries a target.
        kind: NodeKind,
        /// The record the child is born with.
        record: NodeSecurity,
    },
    /// `FilesystemWrite::create_link`.
    CreateLink {
        /// The parent directory.
        dir: NodeId,
        /// The link's name.
        name: &'a [u8],
        /// The stored target.
        target: &'a [u8],
        /// The record the link is born with.
        record: NodeSecurity,
    },
    /// `FilesystemWrite::link`.
    Link {
        /// The directory gaining the name.
        dir: NodeId,
        /// The new name.
        name: &'a [u8],
        /// The node it names.
        node: NodeId,
    },
    /// `FilesystemWrite::write_at`, from the window at `span`.
    WriteAt {
        /// The file.
        file: NodeId,
        /// The first byte written.
        offset: u64,
        /// Where the staged bytes lie.
        span: Span,
    },
    /// `FilesystemWrite::truncate`.
    Truncate {
        /// The file.
        file: NodeId,
        /// The new length.
        size: u64,
    },
    /// `FilesystemWrite::punch`.
    Punch {
        /// The file.
        file: NodeId,
        /// The hole's first byte.
        offset: u64,
        /// The hole's length.
        len: u64,
    },
    /// `FilesystemWrite::remove`.
    Remove {
        /// The parent directory.
        dir: NodeId,
        /// The name unlinked.
        name: &'a [u8],
    },
    /// `FilesystemWrite::rename`.
    Rename {
        /// The source directory.
        src_dir: NodeId,
        /// The name moved.
        src_name: &'a [u8],
        /// The destination directory.
        dst_dir: NodeId,
        /// The name it takes.
        dst_name: &'a [u8],
    },
    /// `FilesystemWrite::flush`.
    Flush,
    /// `FilesystemSecurity::security`.
    Security {
        /// The node.
        node: NodeId,
    },
    /// `FilesystemSecurity::set_security`.
    SetSecurity {
        /// The node.
        node: NodeId,
        /// The record stored.
        security: NodeSecurity,
    },
    /// `FilesystemSecurity::records_ownership`.
    RecordsOwnership {
        /// The node.
        node: NodeId,
    },
    /// `FilesystemStats::stats`.
    Stats,
    /// `FilesystemAttrs::get_attr` into a buffer of `cap` bytes.
    GetAttr {
        /// The node.
        node: NodeId,
        /// The attribute's key.
        key: &'a [u8],
        /// The caller's buffer length, clamped to [`FS_ATTR_VALUE_MAX`].
        cap: u32,
    },
    /// `FilesystemAttrs::set_attr`.
    SetAttr {
        /// The node.
        node: NodeId,
        /// The attribute's key.
        key: &'a [u8],
        /// The value stored.
        value: &'a [u8],
    },
    /// `FilesystemAttrs::list_attr` into a buffer of `cap` bytes.
    ListAttr {
        /// The node.
        node: NodeId,
        /// Which key, in the driver's order.
        index: u64,
        /// The caller's buffer length, clamped to [`FS_ATTR_KEY_MAX`].
        cap: u32,
    },
    /// `FilesystemAttrs::remove_attr`.
    RemoveAttr {
        /// The node.
        node: NodeId,
        /// The attribute's key.
        key: &'a [u8],
    },
    /// No operation: answered once the driver has finished with every
    /// request it received before this one, so the kernel may use the window
    /// again after abandoning a call.
    Release,
}

impl Request<'_> {
    /// The operation this request is.
    #[must_use]
    pub const fn op(&self) -> Op {
        match self {
            Self::NodeInfo { .. } => Op::NodeInfo,
            Self::Lookup { .. } => Op::Lookup,
            Self::Parent { .. } => Op::Parent,
            Self::ReadAt { .. } => Op::ReadAt,
            Self::ReadLink { .. } => Op::ReadLink,
            Self::SeekRegion { .. } => Op::SeekRegion,
            Self::ReadDir { .. } => Op::ReadDir,
            Self::Pin { .. } => Op::Pin,
            Self::Unpin { .. } => Op::Unpin,
            Self::ReleasePins => Op::ReleasePins,
            Self::Create { .. } => Op::Create,
            Self::CreateLink { .. } => Op::CreateLink,
            Self::Link { .. } => Op::Link,
            Self::WriteAt { .. } => Op::WriteAt,
            Self::Truncate { .. } => Op::Truncate,
            Self::Punch { .. } => Op::Punch,
            Self::Remove { .. } => Op::Remove,
            Self::Rename { .. } => Op::Rename,
            Self::Flush => Op::Flush,
            Self::Security { .. } => Op::Security,
            Self::SetSecurity { .. } => Op::SetSecurity,
            Self::RecordsOwnership { .. } => Op::RecordsOwnership,
            Self::Stats => Op::Stats,
            Self::GetAttr { .. } => Op::GetAttr,
            Self::SetAttr { .. } => Op::SetAttr,
            Self::ListAttr { .. } => Op::ListAttr,
            Self::RemoveAttr { .. } => Op::RemoveAttr,
            Self::Release => Op::Release,
        }
    }

    /// The window span the request moves data through, if it moves any.
    #[must_use]
    pub const fn span(&self) -> Option<Span> {
        match self {
            Self::ReadAt { span, .. } | Self::WriteAt { span, .. } => Some(*span),
            _ => None,
        }
    }
}

/// Encode `request`, to run under `class`, into `out`, returning its length.
///
/// # Errors
///
/// [`Errno::BufferTooSmall`] if `out` cannot hold it, and whatever
/// [`decode_request`] would refuse it for: a request that could not be
/// decoded is never sent.
#[allow(clippy::too_many_lines)] // One arm per operation; splitting it would scatter the layout.
pub fn encode_request(
    request: &Request<'_>,
    class: AllocationClass,
    out: &mut [u8],
) -> Result<usize, Errno> {
    let mut w = Writer::new(out);
    w.u16(FSSERVE_VERSION)?;
    w.u8(request.op() as u8)?;
    w.u8(match class {
        AllocationClass::Ordinary => 0,
        AllocationClass::System => FLAG_SYSTEM,
    })?;
    match *request {
        Request::NodeInfo { node }
        | Request::Parent { dir: node }
        | Request::Pin { node }
        | Request::Unpin { pinned: node }
        | Request::Security { node }
        | Request::RecordsOwnership { node } => put_node(&mut w, node)?,
        Request::Lookup { dir, name } | Request::Remove { dir, name } => {
            put_node(&mut w, dir)?;
            put_name(&mut w, name)?;
        }
        Request::ReadLink { link, cap } => {
            put_node(&mut w, link)?;
            w.u32(check_cap(cap, FS_SYMLINK_MAX)?)?;
        }
        Request::SeekRegion {
            file,
            offset,
            region,
        } => {
            put_node(&mut w, file)?;
            w.u64(offset)?;
            w.u32(region.as_u32())?;
        }
        Request::ReadDir { dir, cursor, after } => {
            put_node(&mut w, dir)?;
            w.u64(cursor)?;
            if !after.is_empty() && !is_component(after) {
                return Err(Errno::OutOfRange);
            }
            w.bytes16(after)?;
        }
        Request::Create {
            dir,
            name,
            kind,
            record,
        } => {
            put_node(&mut w, dir)?;
            put_name(&mut w, name)?;
            if kind == NodeKind::Symlink {
                return Err(Errno::OutOfRange);
            }
            w.u8(kind as u8)?;
            put_security(&mut w, &record)?;
        }
        Request::CreateLink {
            dir,
            name,
            target,
            record,
        } => {
            put_node(&mut w, dir)?;
            put_name(&mut w, name)?;
            w.bytes16(check_target(target)?)?;
            put_security(&mut w, &record)?;
        }
        Request::Link { dir, name, node } => {
            put_node(&mut w, dir)?;
            put_name(&mut w, name)?;
            put_node(&mut w, node)?;
        }
        Request::ReadAt { file, offset, span } | Request::WriteAt { file, offset, span } => {
            put_node(&mut w, file)?;
            w.u64(offset)?;
            put_span(&mut w, span)?;
        }
        Request::Truncate { file, size } => {
            put_node(&mut w, file)?;
            w.u64(size)?;
        }
        Request::Punch { file, offset, len } => {
            put_node(&mut w, file)?;
            w.u64(offset)?;
            w.u64(len)?;
        }
        Request::Rename {
            src_dir,
            src_name,
            dst_dir,
            dst_name,
        } => {
            put_node(&mut w, src_dir)?;
            put_name(&mut w, src_name)?;
            put_node(&mut w, dst_dir)?;
            put_name(&mut w, dst_name)?;
        }
        Request::ReleasePins | Request::Flush | Request::Stats | Request::Release => {}
        Request::SetSecurity { node, security } => {
            put_node(&mut w, node)?;
            put_security(&mut w, &security)?;
        }
        Request::GetAttr { node, key, cap } => {
            put_node(&mut w, node)?;
            w.bytes16(check_key(key)?)?;
            w.u32(check_cap(cap, FS_ATTR_VALUE_MAX)?)?;
        }
        Request::SetAttr { node, key, value } => {
            put_node(&mut w, node)?;
            w.bytes16(check_key(key)?)?;
            w.bytes16(check_value(value)?)?;
        }
        Request::ListAttr { node, index, cap } => {
            put_node(&mut w, node)?;
            w.u64(index)?;
            w.u32(check_cap(cap, FS_ATTR_KEY_MAX)?)?;
        }
        Request::RemoveAttr { node, key } => {
            put_node(&mut w, node)?;
            w.bytes16(check_key(key)?)?;
        }
    }
    Ok(w.written())
}

/// The operation a request frame names, even one that fails to decode, so
/// the refusal can answer it; `None` when the frame does not name one.
#[must_use]
pub fn request_op(bytes: &[u8]) -> Option<Op> {
    if bytes.get(..2)? != FSSERVE_VERSION.to_le_bytes() {
        return None;
    }
    Op::from_u8(*bytes.get(2)?).ok()
}

/// Decode a request frame, returning the class it runs under and the request.
///
/// # Errors
///
/// [`Errno::LengthOutOfRange`] for a frame over [`FSSERVE_REQUEST_MAX`], a
/// truncated one, or trailing bytes; [`Errno::AbiVersionUnsupported`] for
/// another version; [`Errno::OutOfRange`] for an unknown operation, a
/// reserved flag, [`NodeId::NONE`], a name that is not a component, or any
/// field outside its bound.
#[allow(clippy::too_many_lines)] // One arm per operation; splitting it would scatter the layout.
pub fn decode_request(bytes: &[u8]) -> Result<(AllocationClass, Request<'_>), Errno> {
    if bytes.len() > FSSERVE_REQUEST_MAX {
        return Err(Errno::LengthOutOfRange);
    }
    let mut r = Reader::new(bytes);
    if r.u16()? != FSSERVE_VERSION {
        return Err(Errno::AbiVersionUnsupported);
    }
    let op = Op::from_u8(r.u8()?)?;
    let class = match r.u8()? {
        0 => AllocationClass::Ordinary,
        FLAG_SYSTEM => AllocationClass::System,
        _ => return Err(Errno::OutOfRange),
    };
    let request = match op {
        Op::NodeInfo => Request::NodeInfo {
            node: node(&mut r)?,
        },
        Op::Lookup => Request::Lookup {
            dir: node(&mut r)?,
            name: name(&mut r)?,
        },
        Op::Parent => Request::Parent { dir: node(&mut r)? },
        Op::ReadAt => Request::ReadAt {
            file: node(&mut r)?,
            offset: r.u64()?,
            span: span(&mut r)?,
        },
        Op::ReadLink => Request::ReadLink {
            link: node(&mut r)?,
            cap: check_cap(r.u32()?, FS_SYMLINK_MAX)?,
        },
        Op::SeekRegion => Request::SeekRegion {
            file: node(&mut r)?,
            offset: r.u64()?,
            region: SeekRegion::from_raw(r.u32()?)?,
        },
        Op::ReadDir => Request::ReadDir {
            dir: node(&mut r)?,
            cursor: r.u64()?,
            after: {
                let after = r.bytes16()?;
                if !after.is_empty() && !is_component(after) {
                    return Err(Errno::OutOfRange);
                }
                after
            },
        },
        Op::Pin => Request::Pin {
            node: node(&mut r)?,
        },
        Op::Unpin => Request::Unpin {
            pinned: node(&mut r)?,
        },
        Op::ReleasePins => Request::ReleasePins,
        Op::Create => Request::Create {
            dir: node(&mut r)?,
            name: name(&mut r)?,
            kind: match node_kind(r.u8()?)? {
                NodeKind::Symlink => return Err(Errno::OutOfRange),
                kind => kind,
            },
            record: security(&mut r)?,
        },
        Op::CreateLink => Request::CreateLink {
            dir: node(&mut r)?,
            name: name(&mut r)?,
            target: check_target(r.bytes16()?)?,
            record: security(&mut r)?,
        },
        Op::Link => Request::Link {
            dir: node(&mut r)?,
            name: name(&mut r)?,
            node: node(&mut r)?,
        },
        Op::WriteAt => Request::WriteAt {
            file: node(&mut r)?,
            offset: r.u64()?,
            span: span(&mut r)?,
        },
        Op::Truncate => Request::Truncate {
            file: node(&mut r)?,
            size: r.u64()?,
        },
        Op::Punch => Request::Punch {
            file: node(&mut r)?,
            offset: r.u64()?,
            len: r.u64()?,
        },
        Op::Remove => Request::Remove {
            dir: node(&mut r)?,
            name: name(&mut r)?,
        },
        Op::Rename => Request::Rename {
            src_dir: node(&mut r)?,
            src_name: name(&mut r)?,
            dst_dir: node(&mut r)?,
            dst_name: name(&mut r)?,
        },
        Op::Flush => Request::Flush,
        Op::Security => Request::Security {
            node: node(&mut r)?,
        },
        Op::SetSecurity => Request::SetSecurity {
            node: node(&mut r)?,
            security: security(&mut r)?,
        },
        Op::RecordsOwnership => Request::RecordsOwnership {
            node: node(&mut r)?,
        },
        Op::Stats => Request::Stats,
        Op::GetAttr => Request::GetAttr {
            node: node(&mut r)?,
            key: check_key(r.bytes16()?)?,
            cap: check_cap(r.u32()?, FS_ATTR_VALUE_MAX)?,
        },
        Op::SetAttr => Request::SetAttr {
            node: node(&mut r)?,
            key: check_key(r.bytes16()?)?,
            value: check_value(r.bytes16()?)?,
        },
        Op::ListAttr => Request::ListAttr {
            node: node(&mut r)?,
            index: r.u64()?,
            cap: check_cap(r.u32()?, FS_ATTR_KEY_MAX)?,
        },
        Op::RemoveAttr => Request::RemoveAttr {
            node: node(&mut r)?,
            key: check_key(r.bytes16()?)?,
        },
        Op::Release => Request::Release,
    };
    if !r.exhausted() {
        return Err(Errno::LengthOutOfRange);
    }
    Ok((class, request))
}

/// What the driver tells the kernel about itself with every reply.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct ServerState {
    /// The driver holds writes it has not yet published to the volume: lost
    /// if the medium leaves now.
    pub unpublished: bool,
    /// The volume's free space is down to its reserve.
    pub in_reserve: bool,
}

impl ServerState {
    const fn bits(self) -> u8 {
        let mut bits = 0;
        if self.unpublished {
            bits |= STATE_UNPUBLISHED;
        }
        if self.in_reserve {
            bits |= STATE_IN_RESERVE;
        }
        bits
    }

    const fn from_bits(bits: u8) -> Result<Self, Errno> {
        if bits & !(STATE_UNPUBLISHED | STATE_IN_RESERVE) != 0 {
            return Err(Errno::OutOfRange);
        }
        Ok(Self {
            unpublished: bits & STATE_UNPUBLISHED != 0,
            in_reserve: bits & STATE_IN_RESERVE != 0,
        })
    }
}

/// A successful answer: the value the trait method returned.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Answer<'a> {
    /// The node's structural metadata.
    Info(NodeInfo),
    /// A node: the one found, created or climbed to, or the id a pin names
    /// its node by while held.
    Node(NodeId),
    /// How many bytes were read or written.
    Count(u32),
    /// A link's target.
    Target(&'a [u8]),
    /// Where the next region begins, if one does.
    Region(Option<u64>),
    /// A listing batch.
    Entries(DirBatch<'a>),
    /// The operation returns nothing.
    Done,
    /// A node's security record.
    Security(NodeSecurity),
    /// What letting a hold go left behind.
    Unpinned(Unpinned),
    /// Whether the format keeps the node's ownership.
    Owned(bool),
    /// The volume's space accounting.
    Stats(VolumeStats),
    /// An attribute value, or a listed key, if there is one.
    Attr(Option<&'a [u8]>),
}

/// How a listing batch ended.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DirEnd {
    /// The batch filled: the listing continues from its last entry.
    More,
    /// The directory has no more entries.
    Ended,
    /// The driver failed after the batch's entries, which it had read.
    Failed(DriverError),
}

/// A validated listing batch: its entries are walked once at decode, so
/// iterating them cannot fail.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct DirBatch<'a> {
    entries: &'a [u8],
    count: u16,
    end: DirEnd,
}

impl<'a> DirBatch<'a> {
    /// How the batch ended.
    #[must_use]
    pub const fn end(&self) -> DirEnd {
        self.end
    }

    /// How many entries the batch holds.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.count as usize
    }

    /// Whether the batch holds no entry.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// The entries in the driver's order, each with its name.
    #[must_use]
    pub const fn iter(&self) -> DirBatchIter<'a> {
        DirBatchIter {
            r: Reader::new(self.entries),
            left: self.count,
        }
    }
}

impl<'a> IntoIterator for &DirBatch<'a> {
    type Item = (DirEntry, &'a [u8]);
    type IntoIter = DirBatchIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// The entries of a [`DirBatch`].
pub struct DirBatchIter<'a> {
    r: Reader<'a>,
    left: u16,
}

impl<'a> Iterator for DirBatchIter<'a> {
    type Item = (DirEntry, &'a [u8]);

    fn next(&mut self) -> Option<Self::Item> {
        if self.left == 0 {
            return None;
        }
        self.left -= 1;
        // Validated at decode; a short read ends the walk rather than panics.
        entry(&mut self.r).ok()
    }
}

/// A decoded reply: the driver's state and what the call returned.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Reply<'a> {
    /// What the driver says of itself.
    pub state: ServerState,
    /// The trait method's result.
    pub result: Result<Answer<'a>, DriverError>,
}

/// Encode the reply to a call of `op` into `out`, returning its length.
///
/// A listing is answered with a [`DirReply`] instead, which streams its
/// entries into the frame.
///
/// # Errors
///
/// [`Errno::BufferTooSmall`] if `out` cannot hold it; [`Errno::OutOfRange`]
/// for an answer of the wrong kind for `op`, or one [`decode_reply`] would
/// refuse.
pub fn encode_reply(
    op: Op,
    state: ServerState,
    result: Result<Answer<'_>, DriverError>,
    out: &mut [u8],
) -> Result<usize, Errno> {
    let mut w = Writer::new(out);
    w.u16(FSSERVE_VERSION)?;
    w.u8(op as u8)?;
    w.u8(state.bits())?;
    let answer = match result {
        Ok(answer) => answer,
        Err(err) => {
            w.i32(err.as_i32())?;
            return Ok(w.written());
        }
    };
    w.i32(0)?;
    match (op, answer) {
        (Op::NodeInfo, Answer::Info(info)) => put_info(&mut w, &info)?,
        (Op::Lookup | Op::Parent | Op::Pin | Op::Create | Op::CreateLink, Answer::Node(node)) => {
            put_node(&mut w, node)?;
        }
        (Op::Unpin, Answer::Unpinned(left)) => w.u8(unpinned_code(left))?,
        (Op::ReadAt | Op::WriteAt, Answer::Count(count)) => w.u32(count)?,
        (Op::ReadLink, Answer::Target(target)) => w.bytes16(check_target(target)?)?,
        (Op::SeekRegion, Answer::Region(at)) => put_option_u64(&mut w, at)?,
        (Op::Security, Answer::Security(security)) => put_security(&mut w, &security)?,
        (Op::RecordsOwnership, Answer::Owned(owned)) => w.u8(u8::from(owned))?,
        (Op::Stats, Answer::Stats(space)) => put_stats(&mut w, &space)?,
        (Op::GetAttr, Answer::Attr(value)) => {
            put_attr(&mut w, value.map(check_value).transpose()?)?;
        }
        (Op::ListAttr, Answer::Attr(key)) => {
            put_attr(&mut w, key.map(check_key).transpose()?)?;
        }
        (
            Op::ReleasePins
            | Op::Link
            | Op::Truncate
            | Op::Punch
            | Op::Remove
            | Op::Rename
            | Op::Flush
            | Op::SetSecurity
            | Op::SetAttr
            | Op::RemoveAttr
            | Op::Release,
            Answer::Done,
        ) => {}
        _ => return Err(Errno::OutOfRange),
    }
    Ok(w.written())
}

/// A listing reply under construction: the driver pushes each entry its
/// visitor is handed until one no longer fits, then finishes the frame.
pub struct DirReply<'a> {
    w: Writer<'a>,
    count: u16,
}

impl<'a> DirReply<'a> {
    /// Start a listing reply in `out`.
    ///
    /// # Errors
    ///
    /// [`Errno::BufferTooSmall`] if `out` cannot hold the header and trailer.
    pub fn new(out: &'a mut [u8]) -> Result<Self, Errno> {
        if out.len() < REPLY_HEAD_LEN + 2 + BATCH_TRAILER_LEN {
            return Err(Errno::BufferTooSmall);
        }
        let mut w = Writer::new(out);
        w.u16(FSSERVE_VERSION)?;
        w.u8(Op::ReadDir as u8)?;
        w.u8(0)?;
        w.i32(0)?;
        w.u16(0)?;
        Ok(Self { w, count: 0 })
    }

    /// Append one entry and its name.
    ///
    /// # Errors
    ///
    /// [`Errno::BufferTooSmall`] when it does not fit beside the trailer, and
    /// the batch stops before it; [`Errno::OutOfRange`] for an entry the
    /// kernel would refuse.
    pub fn push(&mut self, dir_entry: &DirEntry, name: &[u8]) -> Result<(), Errno> {
        let need = ENTRY_FIXED_LEN + name.len() + BATCH_TRAILER_LEN;
        if self.w.room() < need {
            return Err(Errno::BufferTooSmall);
        }
        let count = self.count.checked_add(1).ok_or(Errno::BufferTooSmall)?;
        put_entry(&mut self.w, dir_entry, name)?;
        self.count = count;
        Ok(())
    }

    /// How many entries the batch holds.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.count as usize
    }

    /// Whether the batch holds no entry.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Close the batch as `end`, with the driver's `state`, returning the
    /// frame's length.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] for a batch the kernel would refuse: one that
    /// says the listing continues but holds no entry to continue after.
    pub fn finish(mut self, end: DirEnd, state: ServerState) -> Result<usize, Errno> {
        if end == DirEnd::More && self.count == 0 {
            return Err(Errno::OutOfRange);
        }
        let (tag, err) = match end {
            DirEnd::More => (0, 0),
            DirEnd::Ended => (1, 0),
            DirEnd::Failed(err) => (2, err.as_i32()),
        };
        self.w.patch_u8(3, state.bits());
        self.w.patch_u16(REPLY_HEAD_LEN, self.count);
        self.w.u8(tag)?;
        self.w.i32(err)?;
        Ok(self.w.written())
    }
}

/// Decode the reply to `request`, holding it to what was asked.
///
/// # Errors
///
/// [`Errno::LengthOutOfRange`] for a frame over [`FSSERVE_REPLY_MAX`], a
/// truncated one, or trailing bytes; [`Errno::AbiVersionUnsupported`] for
/// another version; [`Errno::OutOfRange`] for an answer to another
/// operation, an unknown status, or any value the request's bounds or the
/// trait's contract refuse.
pub fn decode_reply<'a>(bytes: &'a [u8], request: &Request<'_>) -> Result<Reply<'a>, Errno> {
    if bytes.len() > FSSERVE_REPLY_MAX {
        return Err(Errno::LengthOutOfRange);
    }
    let mut r = Reader::new(bytes);
    if r.u16()? != FSSERVE_VERSION {
        return Err(Errno::AbiVersionUnsupported);
    }
    if Op::from_u8(r.u8()?)? != request.op() {
        return Err(Errno::OutOfRange);
    }
    let state = ServerState::from_bits(r.u8()?)?;
    let status = r.i32()?;
    if status != 0 {
        let err = driver_error(status)?;
        if !r.exhausted() {
            return Err(Errno::LengthOutOfRange);
        }
        return Ok(Reply {
            state,
            result: Err(err),
        });
    }
    let answer = match *request {
        Request::NodeInfo { .. } => Answer::Info(info(&mut r)?),
        Request::Lookup { .. }
        | Request::Parent { .. }
        | Request::Pin { .. }
        | Request::Create { .. }
        | Request::CreateLink { .. } => Answer::Node(node(&mut r)?),
        Request::Unpin { .. } => Answer::Unpinned(unpinned(r.u8()?)?),
        Request::ReadAt { span, .. } | Request::WriteAt { span, .. } => {
            let count = r.u32()?;
            if count as usize > span.size() {
                return Err(Errno::OutOfRange);
            }
            Answer::Count(count)
        }
        Request::ReadLink { cap, .. } => {
            let target = check_target(r.bytes16()?)?;
            if target.len() > cap as usize {
                return Err(Errno::OutOfRange);
            }
            Answer::Target(target)
        }
        Request::SeekRegion { offset, .. } => {
            let at = option_u64(&mut r)?;
            if at.is_some_and(|at| at < offset) {
                return Err(Errno::OutOfRange);
            }
            Answer::Region(at)
        }
        Request::ReadDir { .. } => Answer::Entries(batch(&mut r)?),
        Request::Security { .. } => Answer::Security(security(&mut r)?),
        Request::RecordsOwnership { .. } => Answer::Owned(flag(&mut r)?),
        Request::Stats => Answer::Stats(stats(&mut r)?),
        Request::GetAttr { cap, .. } => Answer::Attr(attr(&mut r, cap, false)?),
        Request::ListAttr { cap, .. } => Answer::Attr(attr(&mut r, cap, true)?),
        Request::ReleasePins
        | Request::Link { .. }
        | Request::Truncate { .. }
        | Request::Punch { .. }
        | Request::Remove { .. }
        | Request::Rename { .. }
        | Request::Flush
        | Request::SetSecurity { .. }
        | Request::SetAttr { .. }
        | Request::RemoveAttr { .. }
        | Request::Release => Answer::Done,
    };
    if !r.exhausted() {
        return Err(Errno::LengthOutOfRange);
    }
    Ok(Reply {
        state,
        result: Ok(answer),
    })
}

/// What a filesystem driver offers when it serves its volume.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ServeOffer {
    /// The root directory.
    pub root: NodeId,
    /// How the format matches a looked-up name.
    pub name_matching: NameMatching,
    /// How long a node keeps its id.
    pub node_identity: NodeIdentity,
    /// The format keeps extended attributes.
    pub attrs: bool,
    /// Why the format will not write the volume, which is then served
    /// read-only.
    pub write_refusal: Option<WriteRefusal>,
    /// What the mount table calls the volume.
    pub format: FormatName,
}

/// What a filesystem driver tells the kernel when it opens its volume.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ServeFrame {
    /// The volume opened: serve it.
    Serve(ServeOffer),
    /// The volume could not be opened, for this reason.
    Refuse(DriverError),
}

/// Wire length of a [`ServeFrame`].
pub const SERVE_FRAME_LEN: usize = 16;

const SERVE_KIND_SERVE: u8 = 1;
const SERVE_KIND_REFUSE: u8 = 2;

impl ServeFrame {
    /// Encode the frame: version (2), kind (1), then for a serve the name
    /// rule (1), the id lifetime (1), attributes (1), the write refusal (1),
    /// the format name (1) and the root (8); for a refusal a pad (1), the
    /// error (4) and a pad (8).
    #[must_use]
    pub fn encode(&self) -> [u8; SERVE_FRAME_LEN] {
        let mut out = [0u8; SERVE_FRAME_LEN];
        put_u16(&mut out, 0, FSSERVE_VERSION);
        match *self {
            Self::Serve(offer) => {
                out[2] = SERVE_KIND_SERVE;
                out[3] = match offer.name_matching {
                    NameMatching::Exact => 0,
                    NameMatching::AsciiCaseInsensitive => 1,
                };
                out[4] = match offer.node_identity {
                    NodeIdentity::Stable => 0,
                    NodeIdentity::ContentDerived => 1,
                };
                out[5] = u8::from(offer.attrs);
                out[6] = offer.write_refusal.map_or(0, WriteRefusal::as_u8);
                out[7] = offer.format.as_u8();
                put_u64(&mut out, 8, offer.root.raw());
            }
            Self::Refuse(err) => {
                out[2] = SERVE_KIND_REFUSE;
                put_i32(&mut out, 4, err.as_i32());
            }
        }
        out
    }

    /// Decode a frame.
    ///
    /// # Errors
    ///
    /// [`Errno::LengthOutOfRange`] for any length but [`SERVE_FRAME_LEN`];
    /// [`Errno::AbiVersionUnsupported`] for another version;
    /// [`Errno::OutOfRange`] for an unknown kind, code or format name, a
    /// root of [`NodeId::NONE`], or a nonzero pad.
    pub fn decode(bytes: &[u8]) -> Result<Self, Errno> {
        exact_len(bytes, SERVE_FRAME_LEN).map_err(|_| Errno::LengthOutOfRange)?;
        if read_u16(bytes, 0) != FSSERVE_VERSION {
            return Err(Errno::AbiVersionUnsupported);
        }
        match bytes[2] {
            SERVE_KIND_SERVE => {
                let name_matching = match bytes[3] {
                    0 => NameMatching::Exact,
                    1 => NameMatching::AsciiCaseInsensitive,
                    _ => return Err(Errno::OutOfRange),
                };
                let node_identity = match bytes[4] {
                    0 => NodeIdentity::Stable,
                    1 => NodeIdentity::ContentDerived,
                    _ => return Err(Errno::OutOfRange),
                };
                let attrs = match bytes[5] {
                    0 => false,
                    1 => true,
                    _ => return Err(Errno::OutOfRange),
                };
                let write_refusal = match bytes[6] {
                    0 => None,
                    raw => Some(WriteRefusal::from_u8(raw).map_err(|_| Errno::OutOfRange)?),
                };
                let format = FormatName::from_u8(bytes[7])?;
                let root = match read_u64(bytes, 8) {
                    0 => return Err(Errno::OutOfRange),
                    raw => NodeId::from_raw(raw),
                };
                Ok(Self::Serve(ServeOffer {
                    root,
                    name_matching,
                    node_identity,
                    attrs,
                    write_refusal,
                    format,
                }))
            }
            SERVE_KIND_REFUSE => {
                if bytes[3] != 0 || bytes[8..].iter().any(|&b| b != 0) {
                    return Err(Errno::OutOfRange);
                }
                Ok(Self::Refuse(driver_error(read_i32(bytes, 4))?))
            }
            _ => Err(Errno::OutOfRange),
        }
    }
}

/// What the kernel gives a driver it accepted to serve: the endpoint to
/// receive its calls on, and where the data window is mapped.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ServeGrant {
    /// The call endpoint the driver serves.
    pub endpoint: u64,
    /// The window's address in the driver's address space.
    pub window: u64,
    /// The window's length, [`FSSERVE_WINDOW_LEN`].
    pub window_len: u32,
}

/// Wire length of a [`ServeGrant`].
pub const SERVE_GRANT_LEN: usize = 24;

impl ServeGrant {
    /// Encode the grant: endpoint (8), window address (8), window length (4),
    /// a pad (4).
    #[must_use]
    pub fn encode(&self) -> [u8; SERVE_GRANT_LEN] {
        let mut out = [0u8; SERVE_GRANT_LEN];
        put_u64(&mut out, 0, self.endpoint);
        put_u64(&mut out, 8, self.window);
        put_u32(&mut out, 16, self.window_len);
        out
    }

    /// Decode a grant.
    ///
    /// # Errors
    ///
    /// [`Errno::LengthOutOfRange`] for any length but [`SERVE_GRANT_LEN`], or
    /// a window of any length but [`FSSERVE_WINDOW_LEN`];
    /// [`Errno::OutOfRange`] for a nonzero pad.
    pub fn decode(bytes: &[u8]) -> Result<Self, Errno> {
        exact_len(bytes, SERVE_GRANT_LEN).map_err(|_| Errno::LengthOutOfRange)?;
        if read_u32(bytes, 20) != 0 {
            return Err(Errno::OutOfRange);
        }
        let grant = Self {
            endpoint: read_u64(bytes, 0),
            window: read_u64(bytes, 8),
            window_len: read_u32(bytes, 16),
        };
        if grant.window_len as usize != FSSERVE_WINDOW_LEN {
            return Err(Errno::LengthOutOfRange);
        }
        Ok(grant)
    }
}

fn put_node(w: &mut Writer<'_>, node: NodeId) -> Result<(), Errno> {
    if node == NodeId::NONE {
        return Err(Errno::OutOfRange);
    }
    w.u64(node.raw())
}

fn node(r: &mut Reader<'_>) -> Result<NodeId, Errno> {
    match r.u64()? {
        0 => Err(Errno::OutOfRange),
        raw => Ok(NodeId::from_raw(raw)),
    }
}

fn put_name(w: &mut Writer<'_>, name: &[u8]) -> Result<(), Errno> {
    if !is_component(name) {
        return Err(Errno::OutOfRange);
    }
    w.bytes16(name)
}

fn name<'a>(r: &mut Reader<'a>) -> Result<&'a [u8], Errno> {
    let name = r.bytes16()?;
    if !is_component(name) {
        return Err(Errno::OutOfRange);
    }
    Ok(name)
}

fn put_span(w: &mut Writer<'_>, span: Span) -> Result<(), Errno> {
    w.u32(span.offset)?;
    w.u32(span.len)
}

fn span(r: &mut Reader<'_>) -> Result<Span, Errno> {
    let offset = r.u32()? as usize;
    let len = r.u32()? as usize;
    Span::new(offset, len).map_err(|_| Errno::OutOfRange)
}

/// A link target: stored verbatim, so any bytes but NUL, within the bound.
fn check_target(target: &[u8]) -> Result<&[u8], Errno> {
    if target.is_empty() || target.len() > FS_SYMLINK_MAX || target.contains(&0) {
        return Err(Errno::OutOfRange);
    }
    Ok(target)
}

fn check_key(key: &[u8]) -> Result<&[u8], Errno> {
    if key.is_empty() || key.len() > FS_ATTR_KEY_MAX {
        return Err(Errno::OutOfRange);
    }
    Ok(key)
}

fn check_value(value: &[u8]) -> Result<&[u8], Errno> {
    if value.len() > FS_ATTR_VALUE_MAX {
        return Err(Errno::OutOfRange);
    }
    Ok(value)
}

/// A caller's buffer length, no larger than the most a value of its kind
/// can need. An empty buffer is a call the driver answers too: with
/// `BufferTooSmall` for anything it would have written.
fn check_cap(cap: u32, max: usize) -> Result<u32, Errno> {
    if cap as usize > max {
        return Err(Errno::OutOfRange);
    }
    Ok(cap)
}

fn flag(r: &mut Reader<'_>) -> Result<bool, Errno> {
    match r.u8()? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(Errno::OutOfRange),
    }
}

fn put_option_u64(w: &mut Writer<'_>, value: Option<u64>) -> Result<(), Errno> {
    w.u8(u8::from(value.is_some()))?;
    w.u64(value.unwrap_or(0))
}

fn option_u64(r: &mut Reader<'_>) -> Result<Option<u64>, Errno> {
    let present = flag(r)?;
    let value = r.u64()?;
    match (present, value) {
        (true, value) => Ok(Some(value)),
        (false, 0) => Ok(None),
        (false, _) => Err(Errno::OutOfRange),
    }
}

/// A status code: a [`DriverError`], never a success spelt as one.
fn driver_error(code: i32) -> Result<DriverError, Errno> {
    DriverError::from_i32(code).map_err(|_| Errno::OutOfRange)
}

const fn unpinned_code(left: Unpinned) -> u8 {
    match left {
        Unpinned::Held => 0,
        Unpinned::Released => 1,
        Unpinned::Freed => 2,
    }
}

const fn unpinned(raw: u8) -> Result<Unpinned, Errno> {
    Ok(match raw {
        0 => Unpinned::Held,
        1 => Unpinned::Released,
        2 => Unpinned::Freed,
        _ => return Err(Errno::OutOfRange),
    })
}

const fn node_kind(raw: u8) -> Result<NodeKind, Errno> {
    Ok(match raw {
        0 => NodeKind::Directory,
        1 => NodeKind::RegularFile,
        2 => NodeKind::Symlink,
        _ => return Err(Errno::OutOfRange),
    })
}

fn put_info(w: &mut Writer<'_>, info: &NodeInfo) -> Result<(), Errno> {
    w.u8(info.kind as u8)?;
    w.u32(info.nlink)?;
    w.u64(info.ino)?;
    w.u64(info.size)?;
    w.u64(info.allocated)?;
    for time in [
        info.times.created,
        info.times.modified,
        info.times.accessed,
        info.times.changed,
    ] {
        w.bytes(&time.to_le_bytes())?;
    }
    w.u64(info.content_gen)
}

fn info(r: &mut Reader<'_>) -> Result<NodeInfo, Errno> {
    let kind = node_kind(r.u8()?)?;
    let nlink = r.u32()?;
    let ino = r.u64()?;
    let size = r.u64()?;
    let allocated = r.u64()?;
    let mut time = || Time64::from_bytes(r.take(Time64::WIRE_LEN)?).map_err(|_| Errno::OutOfRange);
    let times = NodeTimes {
        created: time()?,
        modified: time()?,
        accessed: time()?,
        changed: time()?,
    };
    Ok(NodeInfo {
        kind,
        nlink,
        ino,
        size,
        allocated,
        times,
        content_gen: r.u64()?,
    })
}

fn put_entry(w: &mut Writer<'_>, dir_entry: &DirEntry, name: &[u8]) -> Result<(), Errno> {
    put_node(w, dir_entry.node)?;
    put_info(w, &dir_entry.info)?;
    w.u64(dir_entry.next_cursor)?;
    put_name(w, name)
}

fn entry<'a>(r: &mut Reader<'a>) -> Result<(DirEntry, &'a [u8]), Errno> {
    let dir_entry = DirEntry {
        node: node(r)?,
        info: info(r)?,
        next_cursor: r.u64()?,
    };
    Ok((dir_entry, name(r)?))
}

fn batch<'a>(r: &mut Reader<'a>) -> Result<DirBatch<'a>, Errno> {
    let count = r.u16()?;
    let from = r.position();
    for _ in 0..count {
        entry(r)?;
    }
    let entries = r.since(from)?;
    let end = match (r.u8()?, r.i32()?) {
        (0, 0) => DirEnd::More,
        (1, 0) => DirEnd::Ended,
        (2, code) if code != 0 => DirEnd::Failed(driver_error(code)?),
        _ => return Err(Errno::OutOfRange),
    };
    // A batch that ends neither the directory nor the call has to carry an
    // entry to resume after, or the listing could never advance.
    if end == DirEnd::More && count == 0 {
        return Err(Errno::OutOfRange);
    }
    Ok(DirBatch {
        entries,
        count,
        end,
    })
}

fn put_security(w: &mut Writer<'_>, security: &NodeSecurity) -> Result<(), Errno> {
    if security.mode & !FS_MODE_MASK != 0 {
        return Err(Errno::OutOfRange);
    }
    w.u32(security.mode)?;
    w.u32(security.uid)?;
    w.u32(security.gid)?;
    match security.required_cap {
        None => {
            w.u8(0)?;
            w.u16(0)?;
        }
        Some(cap) => {
            w.u8(1)?;
            w.u16(cap.as_u16())?;
        }
    }
    match security.acl_model {
        AclModel::Additive => {
            w.u8(0)?;
            w.u8(0)?;
            w.u8(0)?;
        }
        AclModel::Posix { group_obj, mask } => {
            if group_obj > TRIAD_MAX || mask > TRIAD_MAX {
                return Err(Errno::OutOfRange);
            }
            w.u8(1)?;
            w.u8(group_obj)?;
            w.u8(mask)?;
        }
    }
    let acl = security.acl();
    w.u8(u8::try_from(acl.len()).map_err(|_| Errno::OutOfRange)?)?;
    for ace in acl {
        if ace.perms > TRIAD_MAX {
            return Err(Errno::OutOfRange);
        }
        let (tag, id) = match ace.subject {
            SecuritySubject::User(uid) => (0, uid),
            SecuritySubject::Group(gid) => (1, gid),
        };
        w.u8(tag)?;
        w.u32(id)?;
        w.u8(ace.perms)?;
    }
    Ok(())
}

fn security(r: &mut Reader<'_>) -> Result<NodeSecurity, Errno> {
    let mode = r.u32()?;
    if mode & !FS_MODE_MASK != 0 {
        return Err(Errno::OutOfRange);
    }
    let mut record = NodeSecurity::new(mode, r.u32()?, r.u32()?);
    record.required_cap = match (flag(r)?, r.u16()?) {
        (false, 0) => None,
        (true, raw) => Some(CapabilityId::from_raw(raw)?),
        (false, _) => return Err(Errno::OutOfRange),
    };
    record.acl_model = match (r.u8()?, r.u8()?, r.u8()?) {
        (0, 0, 0) => AclModel::Additive,
        (1, group_obj, mask) if group_obj <= TRIAD_MAX && mask <= TRIAD_MAX => {
            AclModel::Posix { group_obj, mask }
        }
        _ => return Err(Errno::OutOfRange),
    };
    let entries = usize::from(r.u8()?);
    if entries > MAX_ACL_ENTRIES {
        return Err(Errno::OutOfRange);
    }
    for _ in 0..entries {
        let tag = r.u8()?;
        let id = r.u32()?;
        let perms = r.u8()?;
        let subject = match tag {
            0 => SecuritySubject::User(id),
            1 => SecuritySubject::Group(id),
            _ => return Err(Errno::OutOfRange),
        };
        if perms > TRIAD_MAX {
            return Err(Errno::OutOfRange);
        }
        record
            .push_acl(SecurityAcl { subject, perms })
            .map_err(|_| Errno::OutOfRange)?;
    }
    Ok(record)
}

fn put_stats(w: &mut Writer<'_>, stats: &VolumeStats) -> Result<(), Errno> {
    check_stats(stats)?;
    w.u32(stats.block_size)?;
    w.u8(u8::from(stats.in_reserve))?;
    w.u64(stats.total_blocks)?;
    w.u64(stats.free_blocks)?;
    w.u64(stats.avail_blocks)?;
    w.u64(stats.files)?;
    w.u64(stats.files_free)
}

fn stats(r: &mut Reader<'_>) -> Result<VolumeStats, Errno> {
    let stats = VolumeStats {
        block_size: r.u32()?,
        in_reserve: flag(r)?,
        total_blocks: r.u64()?,
        free_blocks: r.u64()?,
        avail_blocks: r.u64()?,
        files: r.u64()?,
        files_free: r.u64()?,
    };
    check_stats(&stats)?;
    Ok(stats)
}

/// Space accounting that adds up: a power-of-two block size, and never more
/// free than total or more available than free.
fn check_stats(stats: &VolumeStats) -> Result<(), Errno> {
    if !stats.block_size.is_power_of_two()
        || stats.free_blocks > stats.total_blocks
        || stats.avail_blocks > stats.free_blocks
        || stats.files_free > stats.files
    {
        return Err(Errno::OutOfRange);
    }
    Ok(())
}

fn put_attr(w: &mut Writer<'_>, bytes: Option<&[u8]>) -> Result<(), Errno> {
    match bytes {
        None => w.u8(0),
        Some(bytes) => {
            w.u8(1)?;
            w.bytes16(bytes)
        }
    }
}

/// An attribute value (`key == false`) or a listed key (`key == true`), if
/// the driver has one, no longer than the caller's buffer.
fn attr<'a>(r: &mut Reader<'a>, cap: u32, key: bool) -> Result<Option<&'a [u8]>, Errno> {
    if !flag(r)? {
        return Ok(None);
    }
    let bytes = r.bytes16()?;
    let bytes = if key {
        check_key(bytes)?
    } else {
        check_value(bytes)?
    };
    if bytes.len() > cap as usize {
        return Err(Errno::OutOfRange);
    }
    Ok(Some(bytes))
}

#[cfg(test)]
#[path = "fsserve_tests.rs"]
mod tests;
