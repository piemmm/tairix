//! The channel between the front and one connection's worker.
//!
//! The worker holds one connection: its `lib/smb` client, the session's keys,
//! and everything the server sent, parsed. The front holds the socket and
//! parses none of it: what the socket read crosses as it was read, and what
//! the worker answers crosses as facts — what the server negotiated, a share
//! it listed, a tree it connected, the error that ended a request — which the
//! front judges. Every frame is one layout behind a one-byte tag, and both
//! directions fail closed: a frame whose tag, length or any field is not what
//! an honest peer writes is refused whole.
//!
//! A frame stays small whatever crosses it: a listing crosses a share at a
//! time and the socket's bytes in pieces of one socket event, so the queues a
//! connection commits are small however large a listing or a transfer is.

use alloc::vec::Vec;
use core::fmt;

use tairix_abi::fs::FS_PATH_MAX;
use tairix_abi::net::SOCKET_MAX_DATAGRAM;
use tairix_abi::netshare_ipc::{
    is_host_name, is_share_name, ACCOUNT_NAME_MAX, HOST_MAX, REMARK_MAX, SHARE_NAME_BYTES,
};
use tairix_abi::time::Time64;
use tairix_sandbox::proto::FRAME_HEADER_LEN;
use tairix_sandbox::wire::{Reader, WireError, BYTES_PREFIX};
use tairix_smb::crypt::NONCE_SEED_LEN;
use tairix_smb::file::name::is_name;
use tairix_smb::negotiate::{Dialect, SALT_LEN};
use tairix_smb::status::NtStatus;
use tairix_smb::tree::ShareType;
use tairix_smb::{Refusal, SmbError};

/// The length of the key an account's password derives (NTOWFv2).
pub const KEY_LEN: usize = 16;

/// The most bytes one [`Order::Received`] or [`Report::Send`] carries: one
/// socket event's.
pub const CHUNK_MAX: usize = SOCKET_MAX_DATAGRAM;

const ORDER_OPEN: u8 = 1;
const ORDER_RECEIVED: u8 = 2;
const ORDER_SIGN_IN: u8 = 3;
const ORDER_RENEW: u8 = 4;
const ORDER_LIST: u8 = 5;
const ORDER_CONNECT: u8 = 6;
const ORDER_DISCONNECT: u8 = 7;
const ORDER_STAGE: u8 = 8;
const ORDER_FILE: u8 = 9;
const ORDER_ECHO: u8 = 10;

const FILE_STAT: u8 = 1;
const FILE_READ: u8 = 2;
const FILE_WRITE: u8 = 3;
const FILE_OPEN: u8 = 4;
const FILE_PAGE: u8 = 5;
const FILE_CLOSE: u8 = 6;
const FILE_CREATE: u8 = 7;
const FILE_REMOVE: u8 = 8;
const FILE_RENAME: u8 = 9;
const FILE_TRUNCATE: u8 = 10;
const FILE_SPACE: u8 = 11;
const FILE_HOLD: u8 = 12;
const FILE_READ_HELD: u8 = 13;
const FILE_WRITE_HELD: u8 = 14;
const FILE_SYNC: u8 = 15;
const FILE_RESIZE: u8 = 16;
const FILE_ACK: u8 = 17;

const REPORT_SEND: u8 = 1;
const REPORT_NEGOTIATED: u8 = 2;
const REPORT_ESTABLISHED: u8 = 3;
const REPORT_REJECTED: u8 = 4;
const REPORT_EXPIRED: u8 = 5;
const REPORT_RENEWED: u8 = 6;
const REPORT_SHARE: u8 = 7;
const REPORT_LISTED: u8 = 8;
const REPORT_CONNECTED: u8 = 9;
const REPORT_REFUSED: u8 = 10;
const REPORT_FAILED: u8 = 11;
const REPORT_STAT: u8 = 12;
const REPORT_DATA: u8 = 13;
const REPORT_ENTRY: u8 = 14;
const REPORT_PAGE: u8 = 15;
const REPORT_DONE: u8 = 16;
const REPORT_SPACE: u8 = 17;
const REPORT_HELD: u8 = 18;
const REPORT_BREAK: u8 = 19;

/// The lease bits a lease state is carried as: reads, the handle, writes.
const LEASE_BITS: u8 = 0b111;

/// The leases a server grants.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Leasing {
    /// None: nothing is cached past the moment it was read.
    None,
    /// On files.
    Files,
    /// On files and directories.
    Directories,
}

/// The most bytes one read or write moves: a lane's span of the served
/// window, which is what one call carries.
pub const IO_MAX: usize = 64 << 10;

/// The bytes a [`Stat`] takes: its flags, three sizes and four times.
const STAT_LEN: usize = 1 + 3 * 8 + 4 * 12;

/// An account and the key its password derives, as a sign-in carries it.
#[derive(Copy, Clone, Eq, PartialEq)]
pub struct Credential<'a> {
    /// The account's name, never empty.
    pub user: &'a str,
    /// Its domain, or empty for an account of the server's own.
    pub domain: &'a str,
    /// The key its password derives.
    pub key: &'a [u8; KEY_LEN],
    /// Eight bytes of the front's CSPRNG: NTLMv2's client challenge.
    pub client_challenge: [u8; 8],
    /// The wall clock, for a server that sends no time of its own.
    pub now: Time64,
}

impl fmt::Debug for Credential<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credential")
            .field("user", &self.user)
            .field("domain", &self.domain)
            .finish_non_exhaustive()
    }
}

/// What a node is, as the server reported it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Stat {
    /// Whether it is a directory.
    pub directory: bool,
    /// Whether the server marks it read-only.
    pub read_only: bool,
    /// Its length in bytes; zero for a directory.
    pub size: u64,
    /// The bytes allocated to it.
    pub allocated: u64,
    /// Its id on the server, or zero where the server keeps none.
    pub file_id: u64,
    /// When it was created.
    pub created: Time64,
    /// When its data was last written.
    pub modified: Time64,
    /// When it was last read.
    pub accessed: Time64,
    /// When it last changed at all.
    pub changed: Time64,
}

/// A share's space, as the server reported it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Space {
    /// The bytes in one allocation unit.
    pub unit: u32,
    /// Units in all.
    pub total: u64,
    /// Units free.
    pub free: u64,
    /// Units free to this account.
    pub available: u64,
}

/// One file operation, on a tree, by path.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FileOp<'a> {
    /// What the node at `path` is.
    Stat {
        /// The node.
        path: &'a str,
    },
    /// Read at most `len` bytes from `offset`.
    Read {
        /// The file.
        path: &'a str,
        /// Where reading starts.
        offset: u64,
        /// The most bytes read.
        len: u32,
    },
    /// Write the `len` bytes staged for this request at `offset`.
    Write {
        /// The file.
        path: &'a str,
        /// Where writing starts.
        offset: u64,
        /// The bytes staged.
        len: u32,
    },
    /// Open the directory at `path` for listing, as `handle`.
    Open {
        /// The directory.
        path: &'a str,
        /// The front's name for the open.
        handle: u32,
    },
    /// The next page of the listing `handle` names.
    Page {
        /// The open.
        handle: u32,
    },
    /// Close the listing `handle` names.
    Close {
        /// The open.
        handle: u32,
    },
    /// Make a file, or a directory when `directory` is set.
    Create {
        /// The new node.
        path: &'a str,
        /// Whether it is a directory.
        directory: bool,
    },
    /// Remove the file or the empty directory at `path`.
    Remove {
        /// The node.
        path: &'a str,
        /// The lease the front holds on it, so removing it breaks none of
        /// the front's own.
        lease: Option<[u8; 16]>,
    },
    /// Move `from` to `to`, replacing a file already there when `replace`
    /// is set.
    Rename {
        /// The node moved.
        from: &'a str,
        /// The path it takes.
        to: &'a str,
        /// Whether a file at `to` is replaced.
        replace: bool,
        /// The lease the front holds on the node moved.
        lease: Option<[u8; 16]>,
    },
    /// Set a file's length.
    Truncate {
        /// The file.
        path: &'a str,
        /// The new length.
        size: u64,
    },
    /// The share's space.
    Space,
    /// Open the node at `path` as `handle` and keep it open, asking a lease
    /// under `key`: reads, the handle and — on a file — writes.
    Hold {
        /// The node.
        path: &'a str,
        /// The front's name for the open.
        handle: u32,
        /// The lease's key.
        key: [u8; 16],
        /// Whether it is a directory.
        directory: bool,
        /// Whether to open it for writing too.
        write: bool,
        /// Whether the open is to outlive a dropped connection.
        durability: Durability,
    },
    /// Read at most `len` bytes from `offset` of the file `handle` holds.
    ReadHeld {
        /// The open.
        handle: u32,
        /// Where reading starts.
        offset: u64,
        /// The most bytes read.
        len: u32,
    },
    /// Write the `len` bytes staged for this request at `offset` of the
    /// file `handle` holds.
    WriteHeld {
        /// The open.
        handle: u32,
        /// Where writing starts.
        offset: u64,
        /// The bytes staged.
        len: u32,
        /// Whether the bytes may have been written before, by a write a
        /// dropped connection took the answer of.
        replay: bool,
    },
    /// Ask the server to commit what the file `handle` holds has had
    /// written.
    Sync {
        /// The open.
        handle: u32,
    },
    /// Set the length of the file `handle` holds.
    Resize {
        /// The open.
        handle: u32,
        /// The new length.
        size: u64,
    },
    /// Acknowledge the break of the lease `key`, keeping `state`'s caching.
    Ack {
        /// The lease's key.
        key: [u8; 16],
        /// The lease bits kept.
        state: u8,
    },
}

/// Whether a held open is to outlive a dropped connection.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Durability {
    /// It goes with the connection.
    None,
    /// A durable open, named `create_guid`; a `replay` asks again after the
    /// answer to the first ask was lost, and is answered with the open that
    /// ask made, if it made one.
    Ask {
        /// The open's name, the client's own.
        create_guid: [u8; 16],
        /// Whether this asks again.
        replay: bool,
    },
    /// The durable open `file_id`, named `create_guid`, taken back over a new
    /// connection.
    Reclaim {
        /// The open's name, as it was asked.
        create_guid: [u8; 16],
        /// The server's id for it.
        file_id: [u8; 16],
    },
}

const DURABILITY_NONE: u8 = 0;
const DURABILITY_ASK: u8 = 1;
const DURABILITY_REPLAY: u8 = 2;
const DURABILITY_RECLAIM: u8 = 3;

impl Durability {
    const fn encoded_len(self) -> usize {
        match self {
            Self::None => 1,
            Self::Ask { .. } => 1 + 16,
            Self::Reclaim { .. } => 1 + 16 + 16,
        }
    }

    fn encode(self, w: &mut Out<'_>) {
        match self {
            Self::None => w.u8(DURABILITY_NONE),
            Self::Ask {
                create_guid,
                replay,
            } => {
                w.u8(if replay {
                    DURABILITY_REPLAY
                } else {
                    DURABILITY_ASK
                });
                w.raw(&create_guid);
            }
            Self::Reclaim {
                create_guid,
                file_id,
            } => {
                w.u8(DURABILITY_RECLAIM);
                w.raw(&create_guid);
                w.raw(&file_id);
            }
        }
    }

    fn decode(r: &mut Reader<'_>) -> Result<Self, FrameError> {
        Ok(match r.u8()? {
            DURABILITY_NONE => Self::None,
            tag @ (DURABILITY_ASK | DURABILITY_REPLAY) => Self::Ask {
                create_guid: read_array(r)?,
                replay: tag == DURABILITY_REPLAY,
            },
            DURABILITY_RECLAIM => Self::Reclaim {
                create_guid: read_array(r)?,
                file_id: read_array(r)?,
            },
            _ => return Err(FrameError::Malformed),
        })
    }
}

/// What the front asks of a worker.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Order<'a> {
    /// Open the connection: its `NEGOTIATE` is owed to the socket.
    Open {
        /// The oldest dialect accepted.
        floor: Dialect,
        /// The client GUID, the same for every connection a reconnect
        /// replaces.
        client_guid: [u8; 16],
        /// The pre-auth integrity salt, fresh CSPRNG output.
        salt: [u8; SALT_LEN],
        /// The server's name as dialled.
        server: &'a str,
    },
    /// Bytes the socket read.
    Received(&'a [u8]),
    /// Authenticate the session.
    SignIn {
        /// The account.
        credential: Credential<'a>,
        /// The seed the session's nonces start from, fresh CSPRNG output.
        nonce_seed: [u8; NONCE_SEED_LEN],
        /// The session this one replaces after a dropped connection.
        previous: Option<u64>,
    },
    /// Authenticate the established session again, as an expired one must
    /// be.
    Renew(Credential<'a>),
    /// List the server's disk shares, the special ones when `special` is
    /// set.
    List {
        /// The front's name for the request.
        request: u32,
        /// Whether to list the special shares.
        special: bool,
    },
    /// Connect the tree of `share`.
    Connect {
        /// The front's name for the request.
        request: u32,
        /// The share.
        share: &'a str,
    },
    /// Disconnect a tree.
    Disconnect {
        /// The tree's id.
        tree: u32,
    },
    /// Part of the data a later [`FileOp::Write`] for `request` carries, in
    /// order.
    Stage {
        /// The write's request.
        request: u32,
        /// The bytes.
        chunk: &'a [u8],
    },
    /// Ask the server whether it still serves the connection.
    Echo,
    /// Carry out a file operation on `tree`.
    File {
        /// The front's name for the request.
        request: u32,
        /// The tree.
        tree: u32,
        /// What to do.
        op: FileOp<'a>,
    },
}

/// What a worker tells the front.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Report<'a> {
    /// Bytes owed to the socket, in order.
    Send(&'a [u8]),
    /// The negotiation settled.
    Negotiated {
        /// The server's GUID.
        server_guid: [u8; 16],
        /// The dialect.
        dialect: Dialect,
        /// The leases the server grants.
        leasing: Leasing,
    },
    /// The session is authenticated and keyed.
    Established {
        /// Its id.
        session: u64,
    },
    /// The server refused the credentials with this status.
    Rejected(NtStatus),
    /// The server says the session's authentication expired.
    Expired,
    /// A renewal completed.
    Renewed,
    /// One share a listing kept, ahead of the [`Report::Listed`] ending it.
    Share {
        /// The listing's request.
        request: u32,
        /// The share's name, one a tree connect can take.
        name: &'a str,
        /// Whether it is a special share.
        special: bool,
        /// The server's description of it.
        remark: &'a str,
    },
    /// A listing ended, after the shares it kept.
    Listed {
        /// Its request.
        request: u32,
        /// Special shares left out, since they were not asked for.
        hidden: u32,
        /// Shares no tree connect could name.
        unnamed: u32,
        /// Shares past the most a listing carries.
        unlisted: u32,
    },
    /// A tree connected.
    Connected {
        /// The request it answers.
        request: u32,
        /// The tree's id.
        tree: u32,
        /// What the share is.
        kind: ShareType,
    },
    /// A request failed.
    Refused {
        /// The request.
        request: u32,
        /// Why.
        error: SmbError,
    },
    /// The connection ended.
    Failed(SmbError),
    /// What a node is: a stat's answer, or a created node's.
    Stat {
        /// The request.
        request: u32,
        /// The node.
        stat: Stat,
    },
    /// Part of what a read found, in order, ahead of its [`Report::Done`].
    Data {
        /// The read's request.
        request: u32,
        /// The bytes.
        chunk: &'a [u8],
    },
    /// One entry of a listing's page, ahead of its [`Report::Page`].
    Entry {
        /// The page's request.
        request: u32,
        /// The entry's name, one both sides carry.
        name: &'a str,
        /// What it is.
        stat: Stat,
    },
    /// A listing's page ended.
    Page {
        /// Its request.
        request: u32,
        /// Whether the directory has more.
        more: bool,
    },
    /// A file operation completed.
    Done {
        /// Its request.
        request: u32,
        /// The bytes a read or write moved; zero otherwise.
        count: u32,
        /// The server's id for the node opened by path, or zero where it
        /// keeps none or none was opened.
        file_id: u64,
    },
    /// The share's space.
    Space {
        /// The request.
        request: u32,
        /// The space.
        space: Space,
    },
    /// A node is held open, with the lease the server granted.
    Held {
        /// The request.
        request: u32,
        /// What the node is.
        stat: Stat,
        /// The lease bits granted; none where the server granted no lease, or
        /// where the lease is being broken.
        lease: u8,
        /// The server's id for the open, which a reclaim names.
        file_id: [u8; 16],
        /// How long the open outlives a dropped connection, where the server
        /// made it durable.
        durable_ms: Option<u32>,
    },
    /// The server is breaking a lease: what it held may no longer be
    /// cached past `new`.
    Break {
        /// The lease's key.
        key: [u8; 16],
        /// The lease bits the client may keep.
        new: u8,
        /// Whether the server waits for an acknowledgment.
        ack: bool,
    },
}

/// Why a frame was refused.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FrameError {
    /// The frame is not one an honest peer writes.
    Malformed,
    /// No buffer could be had to encode it.
    OutOfMemory,
}

impl From<WireError> for FrameError {
    fn from(_: WireError) -> Self {
        Self::Malformed
    }
}

/// The largest [`Order`]: a rename between two paths at their bound, or a
/// chunk of bytes.
pub const ORDER_MAX: usize = {
    let sign_in = 1 + credential_len(ACCOUNT_NAME_MAX, ACCOUNT_NAME_MAX) + NONCE_SEED_LEN + 9;
    let received = 1 + BYTES_PREFIX + CHUNK_MAX;
    let rename = 1 + 4 + 4 + 1 + 2 * (BYTES_PREFIX + FS_PATH_MAX) + 1 + 1 + 16;
    let most = if sign_in > received {
        sign_in
    } else {
        received
    };
    if rename > most {
        rename
    } else {
        most
    }
};

/// The largest [`Report`]: a chunk of bytes, which is longer than a share or
/// a listing's entry at their bounds.
pub const REPORT_MAX: usize = 1 + 4 + BYTES_PREFIX + CHUNK_MAX;

const _: () = assert!(
    1 + 4 + (BYTES_PREFIX + SHARE_NAME_BYTES) + 1 + (BYTES_PREFIX + REMARK_MAX) <= REPORT_MAX
        && 1 + 4 + (BYTES_PREFIX + tairix_abi::fs::FS_NAME_MAX) + STAT_LEN <= REPORT_MAX
);

/// How many largest frames each direction of a connection's session holds:
/// enough that a transfer streams while the other side drains, and few
/// enough that a connection commits little.
pub const PIPELINE: usize = 8;

/// What the front's side of a connection's session commits for orders.
pub const ORDER_QUEUE: usize = PIPELINE * (FRAME_HEADER_LEN + ORDER_MAX);

/// What it commits for reports.
pub const REPORT_QUEUE: usize = PIPELINE * (FRAME_HEADER_LEN + REPORT_MAX);

const fn credential_len(user: usize, domain: usize) -> usize {
    (BYTES_PREFIX + user) + (BYTES_PREFIX + domain) + KEY_LEN + 8 + 8 + 4
}

/// A payload encoder over a buffer reserved whole before it is written, so
/// encoding never allocates past what was asked for and never fails half
/// written.
struct Out<'a>(&'a mut Vec<u8>);

impl<'a> Out<'a> {
    fn reserve(out: &'a mut Vec<u8>, len: usize) -> Result<Self, FrameError> {
        out.clear();
        out.try_reserve_exact(len)
            .map_err(|_| FrameError::OutOfMemory)?;
        Ok(Self(out))
    }

    fn u8(&mut self, value: u8) {
        self.0.push(value);
    }

    fn u32(&mut self, value: u32) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }

    fn raw(&mut self, bytes: &[u8]) {
        self.0.extend_from_slice(bytes);
    }

    fn bytes(&mut self, bytes: &[u8]) {
        self.raw(&tairix_sandbox::wire::bytes_prefix(bytes.len()));
        self.raw(bytes);
    }
}

/// Encode the [`Order::Stage`] carrying `parts`, joined, for the write
/// `request`, without joining them first.
///
/// # Errors
///
/// [`FrameError::Malformed`] for parts holding nothing or more than a chunk;
/// [`FrameError::OutOfMemory`].
pub fn encode_stage(request: u32, parts: &[&[u8]], out: &mut Vec<u8>) -> Result<(), FrameError> {
    let mut w = Out::reserve(out, stage_len(parts)?)?;
    put_stage(&mut w, request, parts);
    Ok(())
}

fn stage_len(parts: &[&[u8]]) -> Result<usize, FrameError> {
    let len = parts.iter().map(|part| part.len()).sum::<usize>();
    if len == 0 || len > CHUNK_MAX {
        return Err(FrameError::Malformed);
    }
    Ok(1 + 4 + BYTES_PREFIX + len)
}

fn put_stage(w: &mut Out<'_>, request: u32, parts: &[&[u8]]) {
    w.u8(ORDER_STAGE);
    w.u32(request);
    w.raw(&tairix_sandbox::wire::bytes_prefix(
        parts.iter().map(|part| part.len()).sum(),
    ));
    for part in parts {
        w.raw(part);
    }
}

fn dialect_code(dialect: Dialect) -> u8 {
    match dialect {
        Dialect::Smb300 => 0,
        Dialect::Smb302 => 1,
        Dialect::Smb311 => 2,
    }
}

fn read_dialect(r: &mut Reader<'_>) -> Result<Dialect, FrameError> {
    match r.u8()? {
        0 => Ok(Dialect::Smb300),
        1 => Ok(Dialect::Smb302),
        2 => Ok(Dialect::Smb311),
        _ => Err(FrameError::Malformed),
    }
}

fn read_flag(r: &mut Reader<'_>) -> Result<bool, FrameError> {
    match r.u8()? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(FrameError::Malformed),
    }
}

fn read_array<const N: usize>(r: &mut Reader<'_>) -> Result<[u8; N], FrameError> {
    let mut out = [0; N];
    out.copy_from_slice(r.take(N)?);
    Ok(out)
}

fn read_str<'a>(r: &mut Reader<'a>, cap: usize) -> Result<&'a str, FrameError> {
    core::str::from_utf8(r.bytes(cap)?).map_err(|_| FrameError::Malformed)
}

fn read_chunk<'a>(r: &mut Reader<'a>) -> Result<&'a [u8], FrameError> {
    let bytes = r.bytes(CHUNK_MAX)?;
    if bytes.is_empty() {
        return Err(FrameError::Malformed);
    }
    Ok(bytes)
}

/// Whether `path` is a share path: names a single `\` apart, each one both
/// sides carry, within the VFS's bound; empty for the root.
fn is_share_path(path: &str) -> bool {
    path.len() <= FS_PATH_MAX && (path.is_empty() || path.split('\\').all(is_name))
}

fn read_path<'a>(r: &mut Reader<'a>) -> Result<&'a str, FrameError> {
    let path = read_str(r, FS_PATH_MAX)?;
    if !is_share_path(path) {
        return Err(FrameError::Malformed);
    }
    Ok(path)
}

fn put_time(w: &mut Out<'_>, time: Time64) {
    w.u64(time.secs().cast_unsigned());
    w.u32(time.subsec_nanos());
}

fn read_time(r: &mut Reader<'_>) -> Result<Time64, FrameError> {
    let secs = r.u64()?.cast_signed();
    Time64::new(secs, r.u32()?).map_err(|_| FrameError::Malformed)
}

fn put_stat(w: &mut Out<'_>, stat: &Stat) {
    w.u8(u8::from(stat.directory) | (u8::from(stat.read_only) << 1));
    w.u64(stat.size);
    w.u64(stat.allocated);
    w.u64(stat.file_id);
    for time in [stat.created, stat.modified, stat.accessed, stat.changed] {
        put_time(w, time);
    }
}

fn read_stat(r: &mut Reader<'_>) -> Result<Stat, FrameError> {
    let flags = r.u8()?;
    if flags & !0b11 != 0 {
        return Err(FrameError::Malformed);
    }
    let stat = Stat {
        directory: flags & 1 != 0,
        read_only: flags & 0b10 != 0,
        size: r.u64()?,
        allocated: r.u64()?,
        file_id: r.u64()?,
        created: read_time(r)?,
        modified: read_time(r)?,
        accessed: read_time(r)?,
        changed: read_time(r)?,
    };
    if stat.directory && stat.size != 0 {
        return Err(FrameError::Malformed);
    }
    Ok(stat)
}

impl FileOp<'_> {
    /// The bytes the op takes, once every field is in shape.
    fn encoded_len(&self) -> Result<usize, FrameError> {
        let sized = |path: &str| {
            if is_share_path(path) {
                Ok(BYTES_PREFIX + path.len())
            } else {
                Err(FrameError::Malformed)
            }
        };
        let io = |len: u32| {
            if len == 0 || len as usize > IO_MAX {
                Err(FrameError::Malformed)
            } else {
                Ok(4 + 8 + 4)
            }
        };
        let len = match self {
            Self::Stat { path } => sized(path)?,
            Self::Remove { path, lease } => sized(path)? + key_len(*lease),
            Self::Read { path, len, .. } | Self::Write { path, len, .. } => {
                if *len == 0 || *len as usize > IO_MAX {
                    return Err(FrameError::Malformed);
                }
                sized(path)? + 8 + 4
            }
            Self::Truncate { path, .. } => sized(path)? + 8,
            Self::Open { path, .. } => sized(path)? + 4,
            Self::Page { .. } | Self::Close { .. } | Self::Sync { .. } => 4,
            Self::Create { path, .. } => sized(path)? + 1,
            Self::Rename {
                from, to, lease, ..
            } => sized(from)? + sized(to)? + 1 + key_len(*lease),
            Self::Space => 0,
            Self::Hold {
                path, durability, ..
            } => sized(path)? + 4 + 16 + 1 + durability.encoded_len(),
            Self::ReadHeld { len, .. } => io(*len)?,
            Self::WriteHeld { len, .. } => io(*len)? + 1,
            Self::Resize { .. } => 4 + 8,
            Self::Ack { state, .. } => {
                if state & !LEASE_BITS != 0 {
                    return Err(FrameError::Malformed);
                }
                16 + 1
            }
        };
        Ok(1 + len)
    }

    fn encode(&self, w: &mut Out<'_>) {
        match self {
            Self::Stat { path } => {
                w.u8(FILE_STAT);
                w.bytes(path.as_bytes());
            }
            Self::Read { path, offset, len } => {
                w.u8(FILE_READ);
                w.bytes(path.as_bytes());
                w.u64(*offset);
                w.u32(*len);
            }
            Self::Write { path, offset, len } => {
                w.u8(FILE_WRITE);
                w.bytes(path.as_bytes());
                w.u64(*offset);
                w.u32(*len);
            }
            Self::Open { path, handle } => {
                w.u8(FILE_OPEN);
                w.bytes(path.as_bytes());
                w.u32(*handle);
            }
            Self::Page { handle } => {
                w.u8(FILE_PAGE);
                w.u32(*handle);
            }
            Self::Close { handle } => {
                w.u8(FILE_CLOSE);
                w.u32(*handle);
            }
            Self::Create { path, directory } => {
                w.u8(FILE_CREATE);
                w.bytes(path.as_bytes());
                w.u8(u8::from(*directory));
            }
            Self::Remove { path, lease } => {
                w.u8(FILE_REMOVE);
                w.bytes(path.as_bytes());
                put_key(w, *lease);
            }
            Self::Rename {
                from,
                to,
                replace,
                lease,
            } => {
                w.u8(FILE_RENAME);
                w.bytes(from.as_bytes());
                w.bytes(to.as_bytes());
                w.u8(u8::from(*replace));
                put_key(w, *lease);
            }
            Self::Truncate { path, size } => {
                w.u8(FILE_TRUNCATE);
                w.bytes(path.as_bytes());
                w.u64(*size);
            }
            Self::Space => w.u8(FILE_SPACE),
            held => held.encode_held(w),
        }
    }

    /// Encode an operation on a node the front holds open, or its open.
    fn encode_held(&self, w: &mut Out<'_>) {
        match self {
            Self::Hold {
                path,
                handle,
                key,
                directory,
                write,
                durability,
            } => {
                w.u8(FILE_HOLD);
                w.bytes(path.as_bytes());
                w.u32(*handle);
                w.raw(key);
                w.u8(u8::from(*directory) | (u8::from(*write) << 1));
                durability.encode(w);
            }
            Self::ReadHeld {
                handle,
                offset,
                len,
            } => {
                w.u8(FILE_READ_HELD);
                w.u32(*handle);
                w.u64(*offset);
                w.u32(*len);
            }
            Self::WriteHeld {
                handle,
                offset,
                len,
                replay,
            } => {
                w.u8(FILE_WRITE_HELD);
                w.u32(*handle);
                w.u64(*offset);
                w.u32(*len);
                w.u8(u8::from(*replay));
            }
            Self::Sync { handle } => {
                w.u8(FILE_SYNC);
                w.u32(*handle);
            }
            Self::Resize { handle, size } => {
                w.u8(FILE_RESIZE);
                w.u32(*handle);
                w.u64(*size);
            }
            Self::Ack { key, state } => {
                w.u8(FILE_ACK);
                w.raw(key);
                w.u8(*state);
            }
            _ => {}
        }
    }
}

/// The bytes an optional lease key takes: its flag, and the key.
const fn key_len(key: Option<[u8; 16]>) -> usize {
    match key {
        Some(_) => 1 + 16,
        None => 1,
    }
}

fn put_key(w: &mut Out<'_>, key: Option<[u8; 16]>) {
    match key {
        Some(key) => {
            w.u8(1);
            w.raw(&key);
        }
        None => w.u8(0),
    }
}

fn read_key(r: &mut Reader<'_>) -> Result<Option<[u8; 16]>, FrameError> {
    if read_flag(r)? {
        Ok(Some(read_array(r)?))
    } else {
        Ok(None)
    }
}

fn read_lease(r: &mut Reader<'_>) -> Result<u8, FrameError> {
    let bits = r.u8()?;
    if bits & !LEASE_BITS != 0 {
        return Err(FrameError::Malformed);
    }
    Ok(bits)
}

fn read_file_op<'a>(r: &mut Reader<'a>) -> Result<FileOp<'a>, FrameError> {
    let tag = r.u8()?;
    let op = match tag {
        FILE_STAT => FileOp::Stat {
            path: read_path(r)?,
        },
        FILE_READ | FILE_WRITE => {
            let path = read_path(r)?;
            let offset = r.u64()?;
            let len = r.u32()?;
            if len == 0 || len as usize > IO_MAX {
                return Err(FrameError::Malformed);
            }
            if tag == FILE_READ {
                FileOp::Read { path, offset, len }
            } else {
                FileOp::Write { path, offset, len }
            }
        }
        FILE_OPEN => FileOp::Open {
            path: read_path(r)?,
            handle: r.u32()?,
        },
        FILE_PAGE => FileOp::Page { handle: r.u32()? },
        FILE_CLOSE => FileOp::Close { handle: r.u32()? },
        FILE_CREATE => FileOp::Create {
            path: read_path(r)?,
            directory: read_flag(r)?,
        },
        FILE_REMOVE => FileOp::Remove {
            path: read_path(r)?,
            lease: read_key(r)?,
        },
        FILE_RENAME => FileOp::Rename {
            from: read_path(r)?,
            to: read_path(r)?,
            replace: read_flag(r)?,
            lease: read_key(r)?,
        },
        FILE_TRUNCATE => FileOp::Truncate {
            path: read_path(r)?,
            size: r.u64()?,
        },
        FILE_SPACE => FileOp::Space,
        FILE_HOLD => {
            let path = read_path(r)?;
            let handle = r.u32()?;
            let key = read_array(r)?;
            let flags = r.u8()?;
            if flags & !0b11 != 0 {
                return Err(FrameError::Malformed);
            }
            FileOp::Hold {
                path,
                handle,
                key,
                directory: flags & 1 != 0,
                write: flags & 2 != 0,
                durability: Durability::decode(r)?,
            }
        }
        FILE_READ_HELD | FILE_WRITE_HELD => {
            let handle = r.u32()?;
            let offset = r.u64()?;
            let len = r.u32()?;
            if len == 0 || len as usize > IO_MAX {
                return Err(FrameError::Malformed);
            }
            if tag == FILE_READ_HELD {
                FileOp::ReadHeld {
                    handle,
                    offset,
                    len,
                }
            } else {
                FileOp::WriteHeld {
                    handle,
                    offset,
                    len,
                    replay: read_flag(r)?,
                }
            }
        }
        FILE_SYNC => FileOp::Sync { handle: r.u32()? },
        FILE_RESIZE => FileOp::Resize {
            handle: r.u32()?,
            size: r.u64()?,
        },
        FILE_ACK => FileOp::Ack {
            key: read_array(r)?,
            state: read_lease(r)?,
        },
        _ => return Err(FrameError::Malformed),
    };
    Ok(op)
}

fn put_credential(w: &mut Out<'_>, credential: &Credential<'_>) {
    w.bytes(credential.user.as_bytes());
    w.bytes(credential.domain.as_bytes());
    w.raw(credential.key);
    w.raw(&credential.client_challenge);
    w.u64(credential.now.secs().cast_unsigned());
    w.u32(credential.now.subsec_nanos());
}

fn read_credential<'a>(r: &mut Reader<'a>) -> Result<Credential<'a>, FrameError> {
    let user = read_str(r, ACCOUNT_NAME_MAX)?;
    let domain = read_str(r, ACCOUNT_NAME_MAX)?;
    let key: &[u8; KEY_LEN] = r
        .take(KEY_LEN)?
        .try_into()
        .map_err(|_| FrameError::Malformed)?;
    let client_challenge = read_array(r)?;
    let secs = r.u64()?.cast_signed();
    let now = Time64::new(secs, r.u32()?).map_err(|_| FrameError::Malformed)?;
    if user.is_empty() {
        return Err(FrameError::Malformed);
    }
    Ok(Credential {
        user,
        domain,
        key,
        client_challenge,
        now,
    })
}

impl<'a> Order<'a> {
    /// Encode the order into `out`, which it replaces.
    ///
    /// # Errors
    ///
    /// [`FrameError::Malformed`] for a field past its bound — a name out of
    /// shape, no bytes or too many to carry, an empty user — and
    /// [`FrameError::OutOfMemory`].
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<(), FrameError> {
        let len = self.encoded_len()?;
        let mut w = Out::reserve(out, len)?;
        match self {
            Self::Open {
                floor,
                client_guid,
                salt,
                server,
            } => {
                w.u8(ORDER_OPEN);
                w.u8(dialect_code(*floor));
                w.raw(client_guid);
                w.raw(salt);
                w.bytes(server.as_bytes());
            }
            Self::Received(bytes) => {
                w.u8(ORDER_RECEIVED);
                w.bytes(bytes);
            }
            Self::SignIn {
                credential,
                nonce_seed,
                previous,
            } => {
                w.u8(ORDER_SIGN_IN);
                put_credential(&mut w, credential);
                w.raw(nonce_seed);
                match previous {
                    None => {
                        w.u8(0);
                        w.u64(0);
                    }
                    Some(session) => {
                        w.u8(1);
                        w.u64(*session);
                    }
                }
            }
            Self::Renew(credential) => {
                w.u8(ORDER_RENEW);
                put_credential(&mut w, credential);
            }
            Self::List { request, special } => {
                w.u8(ORDER_LIST);
                w.u32(*request);
                w.u8(u8::from(*special));
            }
            Self::Connect { request, share } => {
                w.u8(ORDER_CONNECT);
                w.u32(*request);
                w.bytes(share.as_bytes());
            }
            Self::Echo => w.u8(ORDER_ECHO),
            Self::Disconnect { tree } => {
                w.u8(ORDER_DISCONNECT);
                w.u32(*tree);
            }
            Self::Stage { request, chunk } => put_stage(&mut w, *request, &[chunk]),
            Self::File { request, tree, op } => {
                w.u8(ORDER_FILE);
                w.u32(*request);
                w.u32(*tree);
                op.encode(&mut w);
            }
        }
        Ok(())
    }

    /// The bytes [`Self::encode`] writes, once every field is in shape.
    fn encoded_len(&self) -> Result<usize, FrameError> {
        let credential = |credential: &Credential<'_>| {
            if credential.user.is_empty()
                || credential.user.len() > ACCOUNT_NAME_MAX
                || credential.domain.len() > ACCOUNT_NAME_MAX
            {
                return Err(FrameError::Malformed);
            }
            Ok(credential_len(
                credential.user.len(),
                credential.domain.len(),
            ))
        };
        let len = match self {
            Self::Open { server, .. } => {
                if !is_host_name(server) {
                    return Err(FrameError::Malformed);
                }
                1 + 1 + 16 + SALT_LEN + BYTES_PREFIX + server.len()
            }
            Self::Received(bytes) => {
                if bytes.is_empty() || bytes.len() > CHUNK_MAX {
                    return Err(FrameError::Malformed);
                }
                1 + BYTES_PREFIX + bytes.len()
            }
            Self::SignIn {
                credential: c,
                previous,
                ..
            } => {
                if *previous == Some(0) {
                    return Err(FrameError::Malformed);
                }
                1 + credential(c)? + NONCE_SEED_LEN + 9
            }
            Self::Renew(c) => 1 + credential(c)?,
            Self::List { .. } => 1 + 4 + 1,
            Self::Connect { share, .. } => {
                if !is_share_name(share) {
                    return Err(FrameError::Malformed);
                }
                1 + 4 + BYTES_PREFIX + share.len()
            }
            Self::Disconnect { .. } => 1 + 4,
            Self::Echo => 1,
            Self::Stage { chunk, .. } => stage_len(&[chunk])?,
            Self::File { op, .. } => 1 + 4 + 4 + op.encoded_len()?,
        };
        Ok(len)
    }

    /// Decode the order in `bytes`.
    ///
    /// # Errors
    ///
    /// [`FrameError::Malformed`] for anything [`Self::encode`] could not have
    /// written.
    pub fn decode(bytes: &'a [u8]) -> Result<Self, FrameError> {
        let mut r = Reader::new(bytes);
        let order = match r.u8()? {
            ORDER_OPEN => {
                let floor = read_dialect(&mut r)?;
                let client_guid = read_array(&mut r)?;
                let salt = read_array(&mut r)?;
                let server = read_str(&mut r, HOST_MAX)?;
                if !is_host_name(server) {
                    return Err(FrameError::Malformed);
                }
                Self::Open {
                    floor,
                    client_guid,
                    salt,
                    server,
                }
            }
            ORDER_RECEIVED => Self::Received(read_chunk(&mut r)?),
            ORDER_SIGN_IN => {
                let credential = read_credential(&mut r)?;
                let nonce_seed = read_array(&mut r)?;
                let previous = match (r.u8()?, r.u64()?) {
                    (0, 0) => None,
                    (1, session) if session != 0 => Some(session),
                    _ => return Err(FrameError::Malformed),
                };
                Self::SignIn {
                    credential,
                    nonce_seed,
                    previous,
                }
            }
            ORDER_RENEW => Self::Renew(read_credential(&mut r)?),
            ORDER_LIST => Self::List {
                request: r.u32()?,
                special: read_flag(&mut r)?,
            },
            ORDER_CONNECT => {
                let request = r.u32()?;
                let share = read_str(&mut r, SHARE_NAME_BYTES)?;
                if !is_share_name(share) {
                    return Err(FrameError::Malformed);
                }
                Self::Connect { request, share }
            }
            ORDER_DISCONNECT => Self::Disconnect { tree: r.u32()? },
            ORDER_STAGE => Self::Stage {
                request: r.u32()?,
                chunk: read_chunk(&mut r)?,
            },
            ORDER_FILE => Self::File {
                request: r.u32()?,
                tree: r.u32()?,
                op: read_file_op(&mut r)?,
            },
            ORDER_ECHO => Self::Echo,
            _ => return Err(FrameError::Malformed),
        };
        if !r.is_exhausted() {
            return Err(FrameError::Malformed);
        }
        Ok(order)
    }
}

/// An [`SmbError`]'s kind and detail.
fn error_codes(error: SmbError) -> (u8, u32) {
    match error {
        SmbError::Malformed => (1, 0),
        SmbError::Protocol => (2, 0),
        SmbError::Refused(refusal) => (
            3,
            match refusal {
                Refusal::Dialect => 1,
                Refusal::Encryption => 2,
                Refusal::Signing => 3,
                Refusal::Guest => 4,
                Refusal::Integrity => 5,
                Refusal::Downgrade => 6,
                Refusal::Validation => 7,
                Refusal::Mechanism => 8,
                Refusal::Ntlm => 9,
            },
        ),
        SmbError::Integrity => (4, 0),
        SmbError::Status(status) => (5, status.code()),
        SmbError::OutOfMemory => (6, 0),
        SmbError::TooLarge => (7, 0),
        SmbError::NoCredit => (8, 0),
        SmbError::TimeOutOfRange => (9, 0),
        SmbError::Name => (10, 0),
        SmbError::Rpc(status) => (11, status),
    }
}

fn read_error(r: &mut Reader<'_>) -> Result<SmbError, FrameError> {
    let error = match (r.u8()?, r.u32()?) {
        (1, 0) => SmbError::Malformed,
        (2, 0) => SmbError::Protocol,
        (3, detail) => SmbError::Refused(match detail {
            1 => Refusal::Dialect,
            2 => Refusal::Encryption,
            3 => Refusal::Signing,
            4 => Refusal::Guest,
            5 => Refusal::Integrity,
            6 => Refusal::Downgrade,
            7 => Refusal::Validation,
            8 => Refusal::Mechanism,
            9 => Refusal::Ntlm,
            _ => return Err(FrameError::Malformed),
        }),
        (4, 0) => SmbError::Integrity,
        (5, status) => SmbError::Status(NtStatus::from_code(status)),
        (6, 0) => SmbError::OutOfMemory,
        (7, 0) => SmbError::TooLarge,
        (8, 0) => SmbError::NoCredit,
        (9, 0) => SmbError::TimeOutOfRange,
        (10, 0) => SmbError::Name,
        (11, status) => SmbError::Rpc(status),
        _ => return Err(FrameError::Malformed),
    };
    Ok(error)
}

impl Report<'_> {
    /// Append a file operation's report.
    fn encode_file(&self, w: &mut Out<'_>) {
        match self {
            Self::Stat { request, stat } => {
                w.u8(REPORT_STAT);
                w.u32(*request);
                put_stat(w, stat);
            }
            Self::Data { request, chunk } => {
                w.u8(REPORT_DATA);
                w.u32(*request);
                w.bytes(chunk);
            }
            Self::Entry {
                request,
                name,
                stat,
            } => {
                w.u8(REPORT_ENTRY);
                w.u32(*request);
                w.bytes(name.as_bytes());
                put_stat(w, stat);
            }
            Self::Page { request, more } => {
                w.u8(REPORT_PAGE);
                w.u32(*request);
                w.u8(u8::from(*more));
            }
            Self::Done {
                request,
                count,
                file_id,
            } => {
                w.u8(REPORT_DONE);
                w.u32(*request);
                w.u32(*count);
                w.u64(*file_id);
            }
            Self::Space { request, space } => {
                w.u8(REPORT_SPACE);
                w.u32(*request);
                w.u32(space.unit);
                w.u64(space.total);
                w.u64(space.free);
                w.u64(space.available);
            }
            Self::Held {
                request,
                stat,
                lease,
                file_id,
                durable_ms,
            } => {
                w.u8(REPORT_HELD);
                w.u32(*request);
                put_stat(w, stat);
                w.u8(*lease);
                w.raw(file_id);
                match durable_ms {
                    None => w.u8(0),
                    Some(ms) => {
                        w.u8(1);
                        w.u32(*ms);
                    }
                }
            }
            Self::Break { key, new, ack } => {
                w.u8(REPORT_BREAK);
                w.raw(key);
                w.u8(*new);
                w.u8(u8::from(*ack));
            }
            _ => {}
        }
    }
}

/// The file operation's report tagged `tag`.
fn read_file_report<'a>(tag: u8, r: &mut Reader<'a>) -> Result<Report<'a>, FrameError> {
    let report = match tag {
        REPORT_STAT => Report::Stat {
            request: r.u32()?,
            stat: read_stat(r)?,
        },
        REPORT_DATA => Report::Data {
            request: r.u32()?,
            chunk: read_chunk(r)?,
        },
        REPORT_ENTRY => {
            let request = r.u32()?;
            let name = read_str(r, tairix_abi::fs::FS_NAME_MAX)?;
            if !is_name(name) {
                return Err(FrameError::Malformed);
            }
            Report::Entry {
                request,
                name,
                stat: read_stat(r)?,
            }
        }
        REPORT_PAGE => Report::Page {
            request: r.u32()?,
            more: read_flag(r)?,
        },
        REPORT_DONE => Report::Done {
            request: r.u32()?,
            count: r.u32()?,
            file_id: r.u64()?,
        },
        REPORT_SPACE => Report::Space {
            request: r.u32()?,
            space: Space {
                unit: r.u32()?,
                total: r.u64()?,
                free: r.u64()?,
                available: r.u64()?,
            },
        },
        REPORT_HELD => Report::Held {
            request: r.u32()?,
            stat: read_stat(r)?,
            lease: read_lease(r)?,
            file_id: read_array(r)?,
            durable_ms: match r.u8()? {
                0 => None,
                1 => Some(r.u32()?),
                _ => return Err(FrameError::Malformed),
            },
        },
        REPORT_BREAK => Report::Break {
            key: read_array(r)?,
            new: read_lease(r)?,
            ack: read_flag(r)?,
        },
        _ => return Err(FrameError::Malformed),
    };
    Ok(report)
}

impl<'a> Report<'a> {
    /// The request a file operation's report, or a refusal, answers.
    #[must_use]
    pub const fn file_request(&self) -> Option<u32> {
        match *self {
            Self::Stat { request, .. }
            | Self::Data { request, .. }
            | Self::Entry { request, .. }
            | Self::Page { request, .. }
            | Self::Done { request, .. }
            | Self::Space { request, .. }
            | Self::Held { request, .. }
            | Self::Refused { request, .. } => Some(request),
            _ => None,
        }
    }

    /// Encode the report into `out`, which it replaces.
    ///
    /// # Errors
    ///
    /// [`FrameError::Malformed`] for a field past its bound — a share name
    /// out of shape, a remark too long, no bytes or too many to carry — and
    /// [`FrameError::OutOfMemory`].
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<(), FrameError> {
        let len = self.encoded_len()?;
        let mut w = Out::reserve(out, len)?;
        match self {
            Self::Send(bytes) => {
                w.u8(REPORT_SEND);
                w.bytes(bytes);
            }
            Self::Negotiated {
                server_guid,
                dialect,
                leasing,
            } => {
                w.u8(REPORT_NEGOTIATED);
                w.raw(server_guid);
                w.u8(dialect_code(*dialect));
                w.u8(match leasing {
                    Leasing::None => 0,
                    Leasing::Files => 1,
                    Leasing::Directories => 2,
                });
            }
            Self::Established { session } => {
                w.u8(REPORT_ESTABLISHED);
                w.u64(*session);
            }
            Self::Rejected(status) => {
                w.u8(REPORT_REJECTED);
                w.u32(status.code());
            }
            Self::Expired => w.u8(REPORT_EXPIRED),
            Self::Renewed => w.u8(REPORT_RENEWED),
            Self::Share {
                request,
                name,
                special,
                remark,
            } => {
                w.u8(REPORT_SHARE);
                w.u32(*request);
                w.bytes(name.as_bytes());
                w.u8(u8::from(*special));
                w.bytes(remark.as_bytes());
            }
            Self::Listed {
                request,
                hidden,
                unnamed,
                unlisted,
            } => {
                w.u8(REPORT_LISTED);
                w.u32(*request);
                w.u32(*hidden);
                w.u32(*unnamed);
                w.u32(*unlisted);
            }
            Self::Connected {
                request,
                tree,
                kind,
            } => {
                w.u8(REPORT_CONNECTED);
                w.u32(*request);
                w.u32(*tree);
                w.u8(kind.code());
            }
            Self::Refused { request, error } => {
                w.u8(REPORT_REFUSED);
                w.u32(*request);
                let (kind, detail) = error_codes(*error);
                w.u8(kind);
                w.u32(detail);
            }
            Self::Failed(error) => {
                w.u8(REPORT_FAILED);
                let (kind, detail) = error_codes(*error);
                w.u8(kind);
                w.u32(detail);
            }
            file => file.encode_file(&mut w),
        }
        Ok(())
    }

    /// The bytes [`Self::encode`] writes, once every field is in shape.
    fn encoded_len(&self) -> Result<usize, FrameError> {
        let len = match self {
            Self::Send(bytes) => {
                if bytes.is_empty() || bytes.len() > CHUNK_MAX {
                    return Err(FrameError::Malformed);
                }
                1 + BYTES_PREFIX + bytes.len()
            }
            Self::Negotiated { .. } => 1 + 16 + 1 + 1,
            Self::Established { .. } => 1 + 8,
            Self::Rejected(_) => 1 + 4,
            Self::Expired | Self::Renewed => 1,
            Self::Share { name, remark, .. } => {
                if !is_share_name(name) || remark.len() > REMARK_MAX {
                    return Err(FrameError::Malformed);
                }
                1 + 4 + BYTES_PREFIX + name.len() + 1 + BYTES_PREFIX + remark.len()
            }
            // A request, then three counts, or a count and a file id.
            Self::Listed { .. } | Self::Done { .. } => 1 + 4 + 12,
            // A request, then a tree and its kind, or an error's kind and
            // detail.
            Self::Connected { .. } | Self::Refused { .. } => 1 + 4 + 5,
            // An error's kind and detail, or a request and a flag.
            Self::Failed(_) | Self::Page { .. } => 1 + 5,
            Self::Stat { stat, .. } => {
                if stat.directory && stat.size != 0 {
                    return Err(FrameError::Malformed);
                }
                1 + 4 + STAT_LEN
            }
            Self::Data { chunk, .. } => {
                if chunk.is_empty() || chunk.len() > CHUNK_MAX {
                    return Err(FrameError::Malformed);
                }
                1 + 4 + BYTES_PREFIX + chunk.len()
            }
            Self::Entry { name, stat, .. } => {
                if !is_name(name) || (stat.directory && stat.size != 0) {
                    return Err(FrameError::Malformed);
                }
                1 + 4 + BYTES_PREFIX + name.len() + STAT_LEN
            }
            Self::Space { .. } => 1 + 4 + 4 + 3 * 8,
            Self::Held {
                stat,
                lease,
                durable_ms,
                ..
            } => {
                if (stat.directory && stat.size != 0) || lease & !LEASE_BITS != 0 {
                    return Err(FrameError::Malformed);
                }
                1 + 4 + STAT_LEN + 1 + 16 + 1 + if durable_ms.is_some() { 4 } else { 0 }
            }
            Self::Break { new, .. } => {
                if new & !LEASE_BITS != 0 {
                    return Err(FrameError::Malformed);
                }
                1 + 16 + 1 + 1
            }
        };
        Ok(len)
    }

    /// Decode the report in `bytes`.
    ///
    /// # Errors
    ///
    /// [`FrameError::Malformed`] for anything [`Self::encode`] could not have
    /// written.
    pub fn decode(bytes: &'a [u8]) -> Result<Self, FrameError> {
        let mut r = Reader::new(bytes);
        let report = match r.u8()? {
            REPORT_SEND => Self::Send(read_chunk(&mut r)?),
            REPORT_NEGOTIATED => Self::Negotiated {
                server_guid: read_array(&mut r)?,
                dialect: read_dialect(&mut r)?,
                leasing: match r.u8()? {
                    0 => Leasing::None,
                    1 => Leasing::Files,
                    2 => Leasing::Directories,
                    _ => return Err(FrameError::Malformed),
                },
            },
            REPORT_ESTABLISHED => Self::Established { session: r.u64()? },
            REPORT_REJECTED => Self::Rejected(NtStatus::from_code(r.u32()?)),
            REPORT_EXPIRED => Self::Expired,
            REPORT_RENEWED => Self::Renewed,
            REPORT_SHARE => {
                let request = r.u32()?;
                let name = read_str(&mut r, SHARE_NAME_BYTES)?;
                let special = read_flag(&mut r)?;
                let remark = read_str(&mut r, REMARK_MAX)?;
                if !is_share_name(name) {
                    return Err(FrameError::Malformed);
                }
                Self::Share {
                    request,
                    name,
                    special,
                    remark,
                }
            }
            REPORT_LISTED => Self::Listed {
                request: r.u32()?,
                hidden: r.u32()?,
                unnamed: r.u32()?,
                unlisted: r.u32()?,
            },
            REPORT_CONNECTED => Self::Connected {
                request: r.u32()?,
                tree: r.u32()?,
                kind: ShareType::from_code(r.u8()?).ok_or(FrameError::Malformed)?,
            },
            REPORT_REFUSED => Self::Refused {
                request: r.u32()?,
                error: read_error(&mut r)?,
            },
            REPORT_FAILED => Self::Failed(read_error(&mut r)?),
            tag => read_file_report(tag, &mut r)?,
        };
        if !r.is_exhausted() {
            return Err(FrameError::Malformed);
        }
        Ok(report)
    }
}

#[cfg(test)]
#[path = "wire_tests.rs"]
mod tests;
