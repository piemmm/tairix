//! The scripted server's disk share, held in memory.
//!
//! It keeps what a server keeps across requests and connections: the tree,
//! the opens on it, the leases they hold, the watches outstanding, the creates
//! waiting on a break, and the durable opens a dropped connection leaves to be
//! reclaimed. A [`Share`] outlives the connection serving it, so a test can
//! drop one connection and reconnect another to the same share.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::time::Time64;

use super::{behind_header, empty_error, DISK_SHARE, MAXIMAL_ACCESS};
use crate::file::create::{
    Access, CreateAction, CreateOptions, CreateRequest, CreateResponse, DiskId, Disposition,
    Durable, MaximalAccess, OplockLevel,
};
use crate::file::dir::{
    write_entries, EntryRecord, ListingFlags, QueryDirectoryRequest, QueryDirectoryResponse,
};
use crate::file::info::{
    BasicInfo, BasicUpdate, Class, DispositionInfo, EndOfFileInfo, FsAbilities, FsAttribute,
    FsFullSize, FsVolume, InternalInfo, NetworkOpenInfo, QueryInfoRequest, QueryInfoResponse,
    RenameInfo, SetInfoRequest, SetInfoResponse, StandardInfo,
};
use crate::file::io::{
    CloseRequest, CloseResponse, ClosedAttributes, FlushRequest, ReadRequest, ReadResponse,
    WriteRequest, WriteResponse,
};
use crate::file::lease::{BreakNotice, Lease, LeaseAck, LeaseBreak, LeaseKey, LeaseState};
use crate::file::name::SharePath;
use crate::file::notify::{
    write_changes, Action, Change, ChangeNotifyRequest, ChangeNotifyResponse, NotifyFilter,
};
use crate::file::{FileAttributes, FileId, FileTimes, NO_FILE};
use crate::header::{Address, Command, Flags, Header};
use crate::simple::encode_empty;
use crate::status::NtStatus;
use crate::wire::{owned, utf16le_len, Encoder, Guid};
use crate::SmbError;

/// The root directory's node.
const ROOT: usize = 0;

/// When the share's clock starts; every change moves it a second on.
const EPOCH_SECS: i64 = 1_700_000_000;

/// The volume the share is on, as an on-disk id names it.
const VOLUME_ID: u64 = 0x7a1e_0000_0000_0001;

/// The volume's serial number.
const SERIAL: u32 = 0x5eed_0001;

/// The durable timeout granted a create asking the server's default, and the
/// longest granted any (MS-SMB2 §3.3.5.9.10).
const DEFAULT_DURABLE_MS: u32 = 60_000;
const MAX_DURABLE_MS: u32 = 300_000;

/// The largest file the share holds.
const MAX_FILE: u64 = 64 << 20;

/// The fixed part of a listed entry, which its name follows.
const ENTRY_FIXED: usize = 104;

/// The lease states a server grants (MS-SMB2 §3.3.5.9.8): reads, then handle
/// or write caching or both.
const GRANTABLE: [u32; 4] = [0x1, 0x3, 0x5, 0x7];

/// Who a request comes from, which a durable open is reclaimed against.
#[derive(Copy, Clone, Debug)]
pub(super) struct Caller<'a> {
    /// The session's user.
    pub user: &'a str,
    /// The connection's client GUID.
    pub client_guid: Guid,
}

/// A message the share has for the client.
#[derive(Debug)]
pub(super) enum Sent {
    /// The response to `request`, an interim one when its status is pending.
    /// The body stands behind a header-sized gap.
    Response {
        request: Header,
        address: Address,
        status: NtStatus,
        body: Vec<u8>,
    },
    /// An unsolicited break, behind a header-sized gap.
    Break(Vec<u8>),
}

/// What a compound's related requests take from the create before them: its
/// open, or the status it failed with.
#[derive(Debug, Default)]
pub(super) struct Chain {
    file_id: Option<FileId>,
    failed: Option<NtStatus>,
}

#[derive(Debug)]
struct Node {
    parent: usize,
    name: String,
    /// A file's data; `None` for a directory.
    data: Option<Vec<u8>>,
    times: FileTimes,
    attributes: FileAttributes,
    delete_pending: bool,
}

#[derive(Debug)]
struct DurableGrant {
    create_guid: Guid,
    client_guid: Guid,
    owner: String,
}

#[derive(Debug)]
struct Open {
    file_id: FileId,
    node: usize,
    path: SharePath,
    access: Access,
    delete_on_close: bool,
    lease: Option<LeaseKey>,
    durable: Option<DurableGrant>,
    /// Whether a connection holds it: a durable open outlives its own.
    attached: bool,
    /// How far an enumeration of the directory has reached.
    listed: usize,
}

#[derive(Debug)]
struct LeaseEntry {
    key: LeaseKey,
    node: usize,
    state: LeaseState,
    epoch: u16,
    breaking_to: Option<LeaseState>,
}

/// An operation answered with an interim response, its final one owed.
#[derive(Debug)]
struct Deferred {
    request: Header,
    async_id: u64,
    waiting: Waiting,
}

#[derive(Debug)]
enum Waiting {
    /// A create held until the lease it breaks is acknowledged.
    Create {
        message: Vec<u8>,
        breaking: LeaseKey,
    },
    /// A watch on a directory.
    Watch {
        file_id: FileId,
        node: usize,
        tree: bool,
        filter: NotifyFilter,
        output_length: u32,
    },
}

/// What a request is answered with.
enum Answer {
    /// A final response.
    Final(NtStatus, Vec<u8>),
    /// A create's success, naming the open it made.
    Opened { file_id: FileId, body: Vec<u8> },
    /// A break's acknowledgment, whose held creates may now go on.
    Acked { key: LeaseKey, body: Vec<u8> },
    /// An interim response under the id the final one will carry.
    Interim(u64),
    /// Nothing yet: a resumed operation still waiting.
    Held,
}

/// Why a request fails: answered with a status, or the connection dropped.
enum Fail {
    Status(NtStatus),
    Drop(SmbError),
}

impl From<NtStatus> for Fail {
    fn from(status: NtStatus) -> Self {
        Self::Status(status)
    }
}

impl From<SmbError> for Fail {
    fn from(err: SmbError) -> Self {
        Self::Drop(err)
    }
}

/// What a path names: an object, or the directory a new one would go in.
enum Located {
    Existing(usize),
    Absent { parent: usize, name: String },
}

/// An entry changing in a directory, as a watch reports it.
struct Event {
    parent: usize,
    name: String,
    action: Action,
    what: NotifyFilter,
}

fn done(encode: impl FnOnce(&mut Encoder)) -> Result<Answer, Fail> {
    Ok(Answer::Final(NtStatus::SUCCESS, behind_header(encode)?))
}

fn opened(response: &CreateResponse) -> Result<Answer, Fail> {
    Ok(Answer::Opened {
        file_id: response.file_id,
        body: behind_header(|enc| response.encode(enc))?,
    })
}

fn require(access: Access, any_of: Access) -> Result<(), Fail> {
    if access.bits() & any_of.bits() == 0 {
        return Err(NtStatus::ACCESS_DENIED.into());
    }
    Ok(())
}

fn open_id(persistent: u64, volatile: u64) -> FileId {
    let mut id = [0; 16];
    id[..8].copy_from_slice(&persistent.to_le_bytes());
    id[8..].copy_from_slice(&volatile.to_le_bytes());
    id
}

const fn allocated(len: u64) -> u64 {
    len.next_multiple_of(4096)
}

/// The scripted server's one disk share.
#[derive(Debug)]
pub struct Share {
    nodes: Vec<Option<Node>>,
    opens: Vec<Open>,
    leases: Vec<LeaseEntry>,
    deferred: Vec<Deferred>,
    sent: Vec<Sent>,
    clock: i64,
    next_id: u64,
}

impl Default for Share {
    fn default() -> Self {
        Self::new()
    }
}

impl Share {
    /// An empty share: its root directory and nothing in it.
    #[must_use]
    pub fn new() -> Self {
        let epoch = Time64::from_secs(EPOCH_SECS);
        let root = Node {
            parent: ROOT,
            name: String::new(),
            data: None,
            times: FileTimes {
                created: epoch,
                accessed: epoch,
                written: epoch,
                changed: epoch,
            },
            attributes: FileAttributes::DIRECTORY,
            delete_pending: false,
        };
        Self {
            nodes: alloc::vec![Some(root)],
            opens: Vec::new(),
            leases: Vec::new(),
            deferred: Vec::new(),
            sent: Vec::new(),
            clock: EPOCH_SECS,
            next_id: 1,
        }
    }

    /// The data of the file `names` reach from the root, if one is there.
    #[must_use]
    pub fn contents(&self, names: &[&str]) -> Option<&[u8]> {
        let node = names
            .iter()
            .try_fold(ROOT, |at, name| self.child(at, name))?;
        self.nodes[node].as_ref()?.data.as_deref()
    }

    /// How many opens a dropped connection left for reclaiming.
    #[must_use]
    pub fn detached_opens(&self) -> usize {
        self.opens.iter().filter(|open| !open.attached).count()
    }

    /// What the share has for the client, taken.
    pub(super) fn take_sent(&mut self) -> Vec<Sent> {
        core::mem::take(&mut self.sent)
    }

    /// The connection serving the share dropped: every open but the durable
    /// ones closes, and nothing in flight is ever answered.
    pub(super) fn detach(&mut self) -> Result<(), SmbError> {
        self.deferred.clear();
        let (durable, closing): (Vec<Open>, Vec<Open>) = core::mem::take(&mut self.opens)
            .into_iter()
            .partition(|open| open.durable.is_some());
        self.opens = durable;
        for open in &mut self.opens {
            open.attached = false;
        }
        for open in &closing {
            self.release(open)?;
        }
        self.sent.clear();
        Ok(())
    }

    /// Answer one request on the share.
    pub(super) fn request(
        &mut self,
        caller: Caller<'_>,
        header: &Header,
        message: &[u8],
        chain: &mut Chain,
    ) -> Result<(), SmbError> {
        if !header.flags.contains(Flags::RELATED_OPERATIONS) {
            *chain = Chain::default();
        }
        let outcome = match header.command {
            Command::Create => self.create(caller, header, message, None),
            Command::Close => self.close(header, message, chain),
            Command::Read => self.read(header, message, chain),
            Command::Write => self.write(header, message, chain),
            Command::Flush => self.flush(header, message, chain),
            Command::QueryDirectory => self.query_directory(header, message, chain),
            Command::QueryInfo => self.query_info(header, message, chain),
            Command::SetInfo => self.set_info(header, message, chain),
            Command::ChangeNotify => self.change_notify(header, message, chain),
            Command::OplockBreak => self.lease_ack(message),
            Command::Cancel => return self.cancel(header),
            _ => Err(NtStatus::NOT_SUPPORTED.into()),
        };
        let Address::Sync { tree_id } = header.address else {
            return Err(SmbError::Protocol);
        };
        match &outcome {
            Ok(Answer::Opened { file_id, .. }) => {
                chain.file_id = Some(*file_id);
                chain.failed = None;
            }
            Err(Fail::Status(status)) if header.command == Command::Create => {
                chain.file_id = None;
                chain.failed = Some(*status);
            }
            _ => {}
        }
        let acked = match &outcome {
            Ok(Answer::Acked { key, .. }) => Some(*key),
            _ => None,
        };
        self.finish(header, Address::Sync { tree_id }, outcome)?;
        match acked {
            Some(key) => self.resume(caller, key),
            None => Ok(()),
        }
    }

    fn finish(
        &mut self,
        request: &Header,
        address: Address,
        outcome: Result<Answer, Fail>,
    ) -> Result<(), SmbError> {
        let (address, status, body) = match outcome {
            Ok(Answer::Final(status, body)) => (address, status, body),
            Ok(Answer::Opened { body, .. } | Answer::Acked { body, .. }) => {
                (address, NtStatus::SUCCESS, body)
            }
            Ok(Answer::Interim(async_id)) => (
                Address::Async { async_id },
                NtStatus::PENDING,
                behind_header(empty_error)?,
            ),
            Ok(Answer::Held) => return Ok(()),
            Err(Fail::Status(status)) => (address, status, behind_header(empty_error)?),
            Err(Fail::Drop(err)) => return Err(err),
        };
        self.push(Sent::Response {
            request: *request,
            address,
            status,
            body,
        })
    }

    fn push(&mut self, sent: Sent) -> Result<(), SmbError> {
        self.sent
            .try_reserve(1)
            .map_err(|_| SmbError::OutOfMemory)?;
        self.sent.push(sent);
        Ok(())
    }

    fn node(&self, node: usize) -> Result<&Node, SmbError> {
        self.nodes
            .get(node)
            .and_then(Option::as_ref)
            .ok_or(SmbError::Protocol)
    }

    fn node_mut(&mut self, node: usize) -> Result<&mut Node, SmbError> {
        self.nodes
            .get_mut(node)
            .and_then(Option::as_mut)
            .ok_or(SmbError::Protocol)
    }

    fn child(&self, parent: usize, name: &str) -> Option<usize> {
        self.children(parent)
            .find(|&id| self.nodes[id].as_ref().is_some_and(|n| n.name == name))
    }

    fn children(&self, parent: usize) -> impl Iterator<Item = usize> + '_ {
        self.nodes
            .iter()
            .enumerate()
            .filter(move |(id, node)| {
                *id != ROOT && node.as_ref().is_some_and(|n| n.parent == parent)
            })
            .map(|(id, _)| id)
    }

    fn tick(&mut self) -> Time64 {
        self.clock += 1;
        Time64::from_secs(self.clock)
    }

    fn fresh_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    /// The open `file_id` names, or a related request's create's.
    fn resolve(&self, header: &Header, file_id: FileId, chain: &Chain) -> Result<usize, Fail> {
        let file_id = if header.flags.contains(Flags::RELATED_OPERATIONS) && file_id == NO_FILE {
            match (chain.file_id, chain.failed) {
                (Some(id), _) => id,
                (None, Some(status)) => return Err(status.into()),
                (None, None) => return Err(NtStatus::INVALID_PARAMETER.into()),
            }
        } else {
            file_id
        };
        self.opens
            .iter()
            .position(|open| open.attached && open.file_id == file_id)
            .ok_or_else(|| NtStatus::FILE_CLOSED.into())
    }

    fn locate(&self, path: &SharePath) -> Result<Located, Fail> {
        let mut names = path.names().peekable();
        let mut at = ROOT;
        while let Some(name) = names.next() {
            if self.node(at)?.data.is_some() {
                return Err(NtStatus::OBJECT_PATH_NOT_FOUND.into());
            }
            match self.child(at, name) {
                Some(child) => at = child,
                None if names.peek().is_some() => {
                    return Err(NtStatus::OBJECT_PATH_NOT_FOUND.into());
                }
                None => {
                    return Ok(Located::Absent {
                        parent: at,
                        name: String::from(name),
                    });
                }
            }
        }
        Ok(Located::Existing(at))
    }

    fn path_of(&self, node: usize) -> Result<SharePath, SmbError> {
        let mut names = Vec::new();
        let mut at = node;
        while at != ROOT {
            let n = self.node(at)?;
            names.push(n.name.as_str());
            at = n.parent;
        }
        names
            .iter()
            .rev()
            .try_fold(SharePath::root(), |path, name| path.join(name))
    }

    fn create(
        &mut self,
        caller: Caller<'_>,
        header: &Header,
        message: &[u8],
        resumed: Option<u64>,
    ) -> Result<Answer, Fail> {
        let request = CreateRequest::decode(message)?;
        if let Some(Durable::Reconnect {
            file_id,
            create_guid,
        }) = request.durable
        {
            return self.reconnect(caller, &request, file_id, create_guid);
        }
        let options = request.options;
        if options.contains(CreateOptions::DELETE_ON_CLOSE)
            && !request.access.contains(Access::DELETE)
            && !request.access.contains(Access::MAXIMUM_ALLOWED)
        {
            return Err(NtStatus::INVALID_PARAMETER.into());
        }
        let located = self.locate(&request.path)?;
        if let Located::Existing(node) = located {
            let own = request.lease.map(|lease| lease.key);
            if let Some(key) = self.conflicting_lease(node, own) {
                return self.defer_create(header, message, key, resumed);
            }
        }
        let (node, action) = self.apply(located, &request)?;
        let access = if request.access.contains(Access::MAXIMUM_ALLOWED) {
            Access::from_bits(MAXIMAL_ACCESS)
        } else {
            request.access
        };
        let lease = match request.lease {
            Some(asked) => self.grant_lease(node, asked)?,
            None => None,
        };
        let durable = match request.durable {
            Some(Durable::Request {
                timeout_ms,
                create_guid,
            }) if lease.is_some_and(|lease| lease.state.contains(LeaseState::HANDLE)) => {
                let timeout = if timeout_ms == 0 {
                    DEFAULT_DURABLE_MS
                } else {
                    timeout_ms.min(MAX_DURABLE_MS)
                };
                Some((
                    timeout,
                    DurableGrant {
                        create_guid,
                        client_guid: caller.client_guid,
                        owner: String::from(caller.user),
                    },
                ))
            }
            _ => None,
        };
        let persistent = self.fresh_id();
        let id = open_id(persistent, self.fresh_id());
        let durable_timeout_ms = durable.as_ref().map(|(timeout, _)| *timeout);
        self.opens
            .try_reserve(1)
            .map_err(|_| SmbError::OutOfMemory)?;
        self.opens.push(Open {
            file_id: id,
            node,
            path: request.path.clone(),
            access,
            delete_on_close: options.contains(CreateOptions::DELETE_ON_CLOSE),
            lease: lease.map(|lease| lease.key),
            durable: durable.map(|(_, grant)| grant),
            attached: true,
            listed: 0,
        });
        let response = CreateResponse {
            durable_timeout_ms,
            ..self.create_response(node, id, action, lease, &request)?
        };
        opened(&response)
    }

    fn create_response(
        &self,
        node: usize,
        file_id: FileId,
        action: CreateAction,
        lease: Option<Lease>,
        request: &CreateRequest,
    ) -> Result<CreateResponse, SmbError> {
        let n = self.node(node)?;
        let len = n.data.as_ref().map_or(0, |data| data.len() as u64);
        Ok(CreateResponse {
            oplock: if lease.is_some() {
                OplockLevel::Lease
            } else {
                OplockLevel::None
            },
            reparse_point: false,
            action,
            times: n.times,
            allocation_size: allocated(len),
            end_of_file: len,
            attributes: n.attributes,
            file_id,
            lease,
            durable_timeout_ms: None,
            maximal_access: request.maximal_access.then_some(MaximalAccess {
                status: NtStatus::SUCCESS,
                access: Access::from_bits(MAXIMAL_ACCESS),
            }),
            disk_id: request.disk_id.then_some(DiskId {
                file: node as u64,
                volume: VOLUME_ID,
            }),
        })
    }

    /// Carry out a create's disposition on what its path names.
    fn apply(
        &mut self,
        located: Located,
        request: &CreateRequest,
    ) -> Result<(usize, CreateAction), Fail> {
        let directory = request.options.contains(CreateOptions::DIRECTORY_FILE);
        match located {
            Located::Existing(node) => {
                let n = self.node(node)?;
                let is_directory = n.data.is_none();
                if directory && !is_directory {
                    return Err(NtStatus::NOT_A_DIRECTORY.into());
                }
                if request.options.contains(CreateOptions::NON_DIRECTORY_FILE) && is_directory {
                    return Err(NtStatus::FILE_IS_A_DIRECTORY.into());
                }
                if n.delete_pending {
                    return Err(NtStatus::DELETE_PENDING.into());
                }
                let action = match request.disposition {
                    Disposition::Create => return Err(NtStatus::OBJECT_NAME_COLLISION.into()),
                    Disposition::Open | Disposition::OpenIf => {
                        return Ok((node, CreateAction::Opened))
                    }
                    Disposition::Overwrite | Disposition::OverwriteIf => CreateAction::Overwritten,
                    Disposition::Supersede => CreateAction::Superseded,
                };
                if is_directory {
                    return Err(NtStatus::INVALID_PARAMETER.into());
                }
                let now = self.tick();
                let n = self.node_mut(node)?;
                if let Some(data) = &mut n.data {
                    data.clear();
                }
                n.times.written = now;
                n.times.changed = now;
                let event = Event {
                    parent: n.parent,
                    name: n.name.clone(),
                    action: Action::Modified,
                    what: NotifyFilter::SIZE.with(NotifyFilter::LAST_WRITE),
                };
                self.notify(&[event])?;
                Ok((node, action))
            }
            Located::Absent { parent, name } => match request.disposition {
                Disposition::Open | Disposition::Overwrite => {
                    Err(NtStatus::OBJECT_NAME_NOT_FOUND.into())
                }
                Disposition::Create
                | Disposition::OpenIf
                | Disposition::OverwriteIf
                | Disposition::Supersede => {
                    let node = self.make(parent, name, directory, request.attributes)?;
                    Ok((node, CreateAction::Created))
                }
            },
        }
    }

    fn make(
        &mut self,
        parent: usize,
        name: String,
        directory: bool,
        attributes: FileAttributes,
    ) -> Result<usize, SmbError> {
        let now = self.tick();
        let attributes = if directory {
            attributes.with(FileAttributes::DIRECTORY)
        } else {
            attributes
                .without(FileAttributes::DIRECTORY)
                .with(FileAttributes::ARCHIVE)
        };
        let event = Event {
            parent,
            name: name.clone(),
            action: Action::Added,
            what: if directory {
                NotifyFilter::DIR_NAME
            } else {
                NotifyFilter::FILE_NAME
            },
        };
        self.nodes
            .try_reserve(1)
            .map_err(|_| SmbError::OutOfMemory)?;
        self.nodes.push(Some(Node {
            parent,
            name,
            data: (!directory).then(Vec::new),
            times: FileTimes {
                created: now,
                accessed: now,
                written: now,
                changed: now,
            },
            attributes,
            delete_pending: false,
        }));
        self.notify(&[event])?;
        Ok(self.nodes.len() - 1)
    }

    /// A lease on `node` under another key than `own` that caches more than
    /// reads, which another open must break first.
    fn conflicting_lease(&self, node: usize, own: Option<LeaseKey>) -> Option<LeaseKey> {
        self.leases
            .iter()
            .find(|lease| {
                lease.node == node
                    && Some(lease.key) != own
                    && (lease.breaking_to.is_some()
                        || lease.state.bits() & !LeaseState::READ.bits() != 0)
            })
            .map(|lease| lease.key)
    }

    /// Break `key` to reads, and hold the create until the break is
    /// acknowledged.
    fn defer_create(
        &mut self,
        header: &Header,
        message: &[u8],
        key: LeaseKey,
        resumed: Option<u64>,
    ) -> Result<Answer, Fail> {
        let entry = self
            .leases
            .iter_mut()
            .find(|lease| lease.key == key)
            .ok_or(SmbError::Protocol)?;
        if entry.breaking_to.is_none() {
            entry.breaking_to = Some(LeaseState::READ);
            entry.epoch = entry.epoch.wrapping_add(1);
            let notice = BreakNotice::Lease(LeaseBreak {
                epoch: entry.epoch,
                ack_required: true,
                key,
                current: entry.state,
                new: LeaseState::READ,
            });
            let body = behind_header(|enc| notice.encode(enc))?;
            self.push(Sent::Break(body))?;
        }
        let async_id = match resumed {
            Some(async_id) => async_id,
            None => self.fresh_id(),
        };
        self.deferred
            .try_reserve(1)
            .map_err(|_| SmbError::OutOfMemory)?;
        self.deferred.push(Deferred {
            request: *header,
            async_id,
            waiting: Waiting::Create {
                message: owned(message)?,
                breaking: key,
            },
        });
        Ok(match resumed {
            Some(_) => Answer::Held,
            None => Answer::Interim(async_id),
        })
    }

    /// The lease `asked` for, as far as `node` allows it: none on a
    /// directory, since the share offers no directory leasing, and reads
    /// alone beside another key's lease.
    fn grant_lease(&mut self, node: usize, asked: Lease) -> Result<Option<Lease>, Fail> {
        if self.node(node)?.data.is_none() || !GRANTABLE.contains(&asked.state.bits()) {
            return Ok(None);
        }
        let shared = self
            .leases
            .iter()
            .any(|lease| lease.node == node && lease.key != asked.key);
        let granted = if shared {
            LeaseState::from_bits(asked.state.bits() & LeaseState::READ.bits())
        } else {
            asked.state
        };
        let at = match self.leases.iter().position(|lease| lease.key == asked.key) {
            Some(at) if self.leases[at].node != node => {
                return Err(NtStatus::INVALID_PARAMETER.into());
            }
            Some(at) => {
                let entry = &mut self.leases[at];
                entry.state = LeaseState::from_bits(entry.state.bits() | granted.bits());
                at
            }
            None => {
                self.leases
                    .try_reserve(1)
                    .map_err(|_| SmbError::OutOfMemory)?;
                self.leases.push(LeaseEntry {
                    key: asked.key,
                    node,
                    state: granted,
                    epoch: 1,
                    breaking_to: None,
                });
                self.leases.len() - 1
            }
        };
        let entry = &self.leases[at];
        Ok(Some(Lease {
            key: entry.key,
            state: entry.state,
            parent: asked.parent,
            epoch: entry.epoch,
            breaking: entry.breaking_to.is_some(),
        }))
    }

    fn reconnect(
        &mut self,
        caller: Caller<'_>,
        request: &CreateRequest,
        file_id: FileId,
        create_guid: Guid,
    ) -> Result<Answer, Fail> {
        let at = self
            .opens
            .iter()
            .position(|open| !open.attached && open.file_id[..8] == file_id[..8])
            .ok_or(NtStatus::OBJECT_NAME_NOT_FOUND)?;
        let open = &self.opens[at];
        let grant = open
            .durable
            .as_ref()
            .ok_or(NtStatus::OBJECT_NAME_NOT_FOUND)?;
        let asked = request.lease.map(|lease| lease.key);
        if grant.create_guid != create_guid
            || open.lease != asked
            || (open.lease.is_some()
                && (grant.client_guid != caller.client_guid || open.path != request.path))
        {
            return Err(NtStatus::OBJECT_NAME_NOT_FOUND.into());
        }
        if grant.owner != caller.user {
            return Err(NtStatus::ACCESS_DENIED.into());
        }
        let node = open.node;
        let lease = match open.lease {
            Some(key) => {
                let entry = self
                    .leases
                    .iter()
                    .find(|lease| lease.key == key)
                    .ok_or(SmbError::Protocol)?;
                Some(Lease {
                    key,
                    state: entry.state,
                    parent: request.lease.and_then(|lease| lease.parent),
                    epoch: entry.epoch,
                    breaking: entry.breaking_to.is_some(),
                })
            }
            None => None,
        };
        let persistent =
            u64::from_le_bytes(file_id[..8].try_into().map_err(|_| SmbError::Protocol)?);
        let id = open_id(persistent, self.fresh_id());
        let open = &mut self.opens[at];
        open.file_id = id;
        open.attached = true;
        opened(&self.create_response(node, id, CreateAction::Opened, lease, request)?)
    }

    fn close(&mut self, header: &Header, message: &[u8], chain: &Chain) -> Result<Answer, Fail> {
        let request = CloseRequest::decode(message)?;
        let at = self.resolve(header, request.file_id, chain)?;
        let open = self.opens.swap_remove(at);
        let attributes = if request.post_query {
            let n = self.node(open.node)?;
            let len = n.data.as_ref().map_or(0, |data| data.len() as u64);
            Some(ClosedAttributes {
                times: n.times,
                allocation_size: allocated(len),
                end_of_file: len,
                attributes: n.attributes,
            })
        } else {
            None
        };
        self.release(&open)?;
        done(|enc| CloseResponse { attributes }.encode(enc))
    }

    /// Let `open` go: its watches end, its lease with the last open under
    /// its key, and its object with its last open once deletion is pending.
    fn release(&mut self, open: &Open) -> Result<(), SmbError> {
        let mut at = 0;
        while at < self.deferred.len() {
            match &self.deferred[at].waiting {
                Waiting::Watch { file_id, .. } if *file_id == open.file_id => {
                    let ended = self.deferred.remove(at);
                    let body =
                        behind_header(|enc| ChangeNotifyResponse { output: &[] }.encode(enc))?;
                    self.push(Sent::Response {
                        request: ended.request,
                        address: Address::Async {
                            async_id: ended.async_id,
                        },
                        status: NtStatus::NOTIFY_CLEANUP,
                        body,
                    })?;
                }
                _ => at += 1,
            }
        }
        if let Some(key) = open.lease {
            if !self.opens.iter().any(|other| other.lease == Some(key)) {
                self.leases.retain(|lease| lease.key != key);
            }
        }
        if open.delete_on_close {
            self.node_mut(open.node)?.delete_pending = true;
        }
        let node = open.node;
        if self.node(node)?.delete_pending && !self.opens.iter().any(|other| other.node == node) {
            self.remove(node)?;
        }
        Ok(())
    }

    /// Remove `node`, a file or an empty directory.
    fn remove(&mut self, node: usize) -> Result<(), SmbError> {
        if self.children(node).next().is_some() {
            self.node_mut(node)?.delete_pending = false;
            return Ok(());
        }
        let removed = self
            .nodes
            .get_mut(node)
            .and_then(Option::take)
            .ok_or(SmbError::Protocol)?;
        self.notify(&[Event {
            parent: removed.parent,
            name: removed.name,
            action: Action::Removed,
            what: if removed.data.is_some() {
                NotifyFilter::FILE_NAME
            } else {
                NotifyFilter::DIR_NAME
            },
        }])
    }

    fn read(&mut self, header: &Header, message: &[u8], chain: &Chain) -> Result<Answer, Fail> {
        let request = ReadRequest::decode(message)?;
        let at = self.resolve(header, request.file_id, chain)?;
        require(self.opens[at].access, Access::READ_DATA)?;
        let n = self.node(self.opens[at].node)?;
        let data = n.data.as_ref().ok_or(NtStatus::INVALID_DEVICE_REQUEST)?;
        let start = usize::try_from(request.offset).unwrap_or(usize::MAX);
        if start >= data.len() {
            return Err(NtStatus::END_OF_FILE.into());
        }
        let len = usize::try_from(request.length).map_err(|_| SmbError::Protocol)?;
        let end = data.len().min(start.saturating_add(len));
        if end - start < usize::try_from(request.minimum).map_err(|_| SmbError::Protocol)? {
            return Err(NtStatus::END_OF_FILE.into());
        }
        let read = &data[start..end];
        done(|enc| ReadResponse { data: read }.encode(enc))
    }

    fn write(&mut self, header: &Header, message: &[u8], chain: &Chain) -> Result<Answer, Fail> {
        let request = WriteRequest::decode(message)?;
        let at = self.resolve(header, request.file_id, chain)?;
        require(
            self.opens[at].access,
            Access::WRITE_DATA.with(Access::APPEND_DATA),
        )?;
        let node = self.opens[at].node;
        let len = request.data.len() as u64;
        let end = request
            .offset
            .checked_add(len)
            .ok_or(NtStatus::INVALID_PARAMETER)?;
        if end > MAX_FILE {
            return Err(NtStatus::DISK_FULL.into());
        }
        let now = self.tick();
        let n = self.node_mut(node)?;
        let data = n.data.as_mut().ok_or(NtStatus::INVALID_DEVICE_REQUEST)?;
        let (start, end) = (
            usize::try_from(request.offset).map_err(|_| SmbError::Protocol)?,
            usize::try_from(end).map_err(|_| SmbError::Protocol)?,
        );
        if data.len() < end {
            data.resize(end, 0);
        }
        data[start..end].copy_from_slice(request.data);
        n.times.written = now;
        n.times.changed = now;
        let event = Event {
            parent: n.parent,
            name: n.name.clone(),
            action: Action::Modified,
            what: NotifyFilter::SIZE.with(NotifyFilter::LAST_WRITE),
        };
        self.notify(&[event])?;
        let count = u32::try_from(request.data.len()).map_err(|_| SmbError::Protocol)?;
        done(|enc| WriteResponse { count }.encode(enc))
    }

    fn flush(&mut self, header: &Header, message: &[u8], chain: &Chain) -> Result<Answer, Fail> {
        let request = FlushRequest::decode(message)?;
        self.resolve(header, request.file_id, chain)?;
        done(encode_empty)
    }

    fn query_directory(
        &mut self,
        header: &Header,
        message: &[u8],
        chain: &Chain,
    ) -> Result<Answer, Fail> {
        let request = QueryDirectoryRequest::decode(message)?;
        let at = self.resolve(header, request.file_id, chain)?;
        require(self.opens[at].access, Access::READ_DATA)?;
        if request.flags.contains(ListingFlags::RESTART_SCANS)
            || request.flags.contains(ListingFlags::REOPEN)
        {
            self.opens[at].listed = 0;
        }
        let node = self.opens[at].node;
        let n = self.node(node)?;
        if n.data.is_some() {
            return Err(NtStatus::INVALID_PARAMETER.into());
        }
        let mut listed: Vec<(usize, &str)> = Vec::from([(node, "."), (n.parent, "..")]);
        for child in self.children(node) {
            listed.push((child, self.node(child)?.name.as_str()));
        }
        let start = self.opens[at].listed;
        if start >= listed.len() {
            return Err(NtStatus::NO_MORE_FILES.into());
        }
        let room = usize::try_from(request.output_length).map_err(|_| SmbError::Protocol)?;
        let (mut used, mut count) = (0_usize, 0_usize);
        for (_, name) in &listed[start..] {
            let need = used.next_multiple_of(8) + ENTRY_FIXED + utf16le_len(name);
            if need > room
                || (count == 1 && request.flags.contains(ListingFlags::RETURN_SINGLE_ENTRY))
            {
                break;
            }
            used = need;
            count += 1;
        }
        if count == 0 {
            return Err(NtStatus::INVALID_PARAMETER.into());
        }
        let mut records = Vec::new();
        for (entry, name) in &listed[start..start + count] {
            let n = self.node(*entry)?;
            let len = n.data.as_ref().map_or(0, |data| data.len() as u64);
            records.push(EntryRecord {
                file_index: 0,
                times: n.times,
                end_of_file: len,
                allocation_size: allocated(len),
                attributes: n.attributes,
                ea_size: 0,
                file_id: *entry as u64,
                name,
            });
        }
        let mut enc = Encoder::new();
        write_entries(&mut enc, &records);
        let output = enc.finish()?;
        self.opens[at].listed = start + count;
        done(|enc| QueryDirectoryResponse { output: &output }.encode(enc))
    }

    fn query_info(
        &mut self,
        header: &Header,
        message: &[u8],
        chain: &Chain,
    ) -> Result<Answer, Fail> {
        let request = QueryInfoRequest::decode(message)?;
        let at = self.resolve(header, request.file_id, chain)?;
        let node = self.opens[at].node;
        let n = self.node(node)?;
        let len = n.data.as_ref().map_or(0, |data| data.len() as u64);
        let mut enc = Encoder::new();
        match request.class {
            Class::Basic => BasicInfo {
                times: n.times,
                attributes: n.attributes,
            }
            .encode(&mut enc),
            Class::Standard => StandardInfo {
                allocation_size: allocated(len),
                end_of_file: len,
                links: 1,
                delete_pending: n.delete_pending,
                directory: n.data.is_none(),
            }
            .encode(&mut enc),
            Class::Internal => InternalInfo { index: node as u64 }.encode(&mut enc),
            Class::NetworkOpen => NetworkOpenInfo {
                times: n.times,
                allocation_size: allocated(len),
                end_of_file: len,
                attributes: n.attributes,
            }
            .encode(&mut enc),
            Class::FsVolume => FsVolume {
                created: Time64::from_secs(EPOCH_SECS),
                serial: SERIAL,
                label: String::from(DISK_SHARE),
            }
            .encode(&mut enc),
            Class::FsAttribute => FsAttribute {
                abilities: FsAbilities::CASE_SENSITIVE_SEARCH
                    .with(FsAbilities::CASE_PRESERVED_NAMES)
                    .with(FsAbilities::UNICODE_ON_DISK),
                max_name_length: 255,
                name: String::from("NTFS"),
            }
            .encode(&mut enc),
            Class::FsFullSize => FsFullSize {
                total_units: 1 << 20,
                caller_available_units: 1 << 19,
                available_units: 1 << 19,
                sectors_per_unit: 8,
                bytes_per_sector: 512,
            }
            .encode(&mut enc),
            Class::Rename | Class::Disposition | Class::EndOfFile => {
                return Err(NtStatus::INVALID_INFO_CLASS.into());
            }
        }
        let output = enc.finish()?;
        if output.len() > usize::try_from(request.output_length).map_err(|_| SmbError::Protocol)? {
            return Err(NtStatus::INVALID_PARAMETER.into());
        }
        done(|enc| QueryInfoResponse { output: &output }.encode(enc))
    }

    fn set_info(&mut self, header: &Header, message: &[u8], chain: &Chain) -> Result<Answer, Fail> {
        let request = SetInfoRequest::decode(message)?;
        let at = self.resolve(header, request.file_id, chain)?;
        let (node, access) = (self.opens[at].node, self.opens[at].access);
        match request.class {
            Class::Basic => {
                require(access, Access::WRITE_ATTRIBUTES)?;
                let update = BasicUpdate::decode(&request.buffer)?;
                let n = self.node_mut(node)?;
                for (field, value) in [
                    (&mut n.times.created, update.created),
                    (&mut n.times.accessed, update.accessed),
                    (&mut n.times.written, update.written),
                    (&mut n.times.changed, update.changed),
                ] {
                    if let Some(value) = value {
                        *field = value;
                    }
                }
                if let Some(attributes) = update.attributes {
                    let directory = n.attributes.bits() & FileAttributes::DIRECTORY.bits();
                    n.attributes = FileAttributes::from_bits(
                        attributes.without(FileAttributes::DIRECTORY).bits() | directory,
                    );
                }
                let event = Event {
                    parent: n.parent,
                    name: n.name.clone(),
                    action: Action::Modified,
                    what: NotifyFilter::ATTRIBUTES.with(NotifyFilter::LAST_WRITE),
                };
                self.notify(&[event])?;
            }
            Class::EndOfFile => {
                require(access, Access::WRITE_DATA)?;
                let size = EndOfFileInfo::decode(&request.buffer)?.end_of_file;
                if size > MAX_FILE {
                    return Err(NtStatus::DISK_FULL.into());
                }
                let now = self.tick();
                let n = self.node_mut(node)?;
                let data = n.data.as_mut().ok_or(NtStatus::INVALID_PARAMETER)?;
                data.resize(usize::try_from(size).map_err(|_| SmbError::Protocol)?, 0);
                n.times.written = now;
                n.times.changed = now;
                let event = Event {
                    parent: n.parent,
                    name: n.name.clone(),
                    action: Action::Modified,
                    what: NotifyFilter::SIZE,
                };
                self.notify(&[event])?;
            }
            Class::Disposition => {
                require(access, Access::DELETE)?;
                let delete = DispositionInfo::decode(&request.buffer)?.delete;
                if node == ROOT {
                    return Err(NtStatus::CANNOT_DELETE.into());
                }
                if delete && self.children(node).next().is_some() {
                    return Err(NtStatus::DIRECTORY_NOT_EMPTY.into());
                }
                self.node_mut(node)?.delete_pending = delete;
            }
            Class::Rename => {
                require(access, Access::DELETE)?;
                let rename = RenameInfo::decode(&request.buffer)?;
                self.rename(node, &rename)?;
            }
            _ => return Err(NtStatus::INVALID_INFO_CLASS.into()),
        }
        done(|enc| SetInfoResponse.encode(enc))
    }

    fn rename(&mut self, node: usize, rename: &RenameInfo) -> Result<(), Fail> {
        if node == ROOT {
            return Err(NtStatus::INVALID_PARAMETER.into());
        }
        let (parent, name) = match self.locate(&rename.target)? {
            Located::Absent { parent, name } => (parent, name),
            Located::Existing(existing) if existing == node => return Ok(()),
            Located::Existing(existing) => {
                let occupant = self.node(existing)?;
                if !rename.replace
                    || occupant.data.is_none()
                    || self.opens.iter().any(|open| open.node == existing)
                {
                    return Err(NtStatus::OBJECT_NAME_COLLISION.into());
                }
                let (parent, name) = (occupant.parent, occupant.name.clone());
                self.remove(existing)?;
                (parent, name)
            }
        };
        let mut at = parent;
        while at != ROOT {
            if at == node {
                return Err(NtStatus::INVALID_PARAMETER.into());
            }
            at = self.node(at)?.parent;
        }
        let now = self.tick();
        let n = self.node_mut(node)?;
        let what = if n.data.is_some() {
            NotifyFilter::FILE_NAME
        } else {
            NotifyFilter::DIR_NAME
        };
        let old = Event {
            parent: n.parent,
            name: core::mem::replace(&mut n.name, name.clone()),
            action: Action::RenamedOld,
            what,
        };
        n.parent = parent;
        n.times.changed = now;
        let path = self.path_of(node)?;
        for open in self.opens.iter_mut().filter(|open| open.node == node) {
            open.path = path.clone();
        }
        self.notify(&[
            old,
            Event {
                parent,
                name,
                action: Action::RenamedNew,
                what,
            },
        ])?;
        Ok(())
    }

    fn change_notify(
        &mut self,
        header: &Header,
        message: &[u8],
        chain: &Chain,
    ) -> Result<Answer, Fail> {
        let request = ChangeNotifyRequest::decode(message)?;
        let at = self.resolve(header, request.file_id, chain)?;
        let open = &self.opens[at];
        if self.node(open.node)?.data.is_some() {
            return Err(NtStatus::INVALID_PARAMETER.into());
        }
        let waiting = Waiting::Watch {
            file_id: open.file_id,
            node: open.node,
            tree: request.watch_tree,
            filter: request.filter,
            output_length: request.output_length,
        };
        let async_id = self.fresh_id();
        self.deferred
            .try_reserve(1)
            .map_err(|_| SmbError::OutOfMemory)?;
        self.deferred.push(Deferred {
            request: *header,
            async_id,
            waiting,
        });
        Ok(Answer::Interim(async_id))
    }

    /// Complete every watch `events` concern with the records it may see.
    fn notify(&mut self, events: &[Event]) -> Result<(), SmbError> {
        let mut at = 0;
        while at < self.deferred.len() {
            let Waiting::Watch {
                node,
                tree,
                filter,
                output_length,
                ..
            } = self.deferred[at].waiting
            else {
                at += 1;
                continue;
            };
            let mut changes = Vec::new();
            for event in events {
                if event.what.bits() & filter.bits() == 0 {
                    continue;
                }
                if let Some(below) = self.path_below(node, event.parent, tree) {
                    changes.push(Change {
                        action: Some(event.action),
                        path: below.join(&event.name)?,
                    });
                }
            }
            if changes.is_empty() {
                at += 1;
                continue;
            }
            let mut enc = Encoder::new();
            write_changes(&mut enc, &changes);
            let records = enc.finish()?;
            let fits =
                records.len() <= usize::try_from(output_length).map_err(|_| SmbError::Protocol)?;
            let (status, output) = if fits {
                (NtStatus::SUCCESS, &records[..])
            } else {
                (NtStatus::NOTIFY_ENUM_DIR, &[][..])
            };
            let body = behind_header(|enc| ChangeNotifyResponse { output }.encode(enc))?;
            let watch = self.deferred.remove(at);
            self.push(Sent::Response {
                request: watch.request,
                address: Address::Async {
                    async_id: watch.async_id,
                },
                status,
                body,
            })?;
        }
        Ok(())
    }

    /// The path from `watched` to `directory`, when a watch on `watched` sees
    /// into it: its own entries, or with `tree` everything beneath.
    fn path_below(&self, watched: usize, directory: usize, tree: bool) -> Option<SharePath> {
        let mut names = Vec::new();
        let mut at = directory;
        while at != watched {
            if !tree || at == ROOT {
                return None;
            }
            let node = self.nodes.get(at)?.as_ref()?;
            names.push(node.name.as_str());
            at = node.parent;
        }
        names
            .iter()
            .rev()
            .try_fold(SharePath::root(), |path, name| path.join(name))
            .ok()
    }

    fn lease_ack(&mut self, message: &[u8]) -> Result<Answer, Fail> {
        let ack = LeaseAck::decode(message)?;
        let entry = self
            .leases
            .iter_mut()
            .find(|lease| lease.key == ack.key)
            .ok_or(NtStatus::INVALID_PARAMETER)?;
        let Some(to) = entry.breaking_to else {
            return Err(NtStatus::INVALID_PARAMETER.into());
        };
        if ack.state.bits() & !to.bits() != 0 {
            return Err(NtStatus::INVALID_PARAMETER.into());
        }
        entry.state = ack.state;
        entry.breaking_to = None;
        Ok(Answer::Acked {
            key: ack.key,
            body: behind_header(|enc| ack.encode(enc))?,
        })
    }

    /// Carry on with the creates the break of `key` held.
    fn resume(&mut self, caller: Caller<'_>, key: LeaseKey) -> Result<(), SmbError> {
        let mut at = 0;
        while at < self.deferred.len() {
            let held = matches!(
                &self.deferred[at].waiting,
                Waiting::Create { breaking, .. } if *breaking == key
            );
            if !held {
                at += 1;
                continue;
            }
            let deferred = self.deferred.remove(at);
            let Waiting::Create { message, .. } = deferred.waiting else {
                continue;
            };
            let outcome = self.create(caller, &deferred.request, &message, Some(deferred.async_id));
            self.finish(
                &deferred.request,
                Address::Async {
                    async_id: deferred.async_id,
                },
                outcome,
            )?;
        }
        Ok(())
    }

    /// Cancel what `header` names; the cancelled request answers, the
    /// cancel never does.
    fn cancel(&mut self, header: &Header) -> Result<(), SmbError> {
        let found = self
            .deferred
            .iter()
            .position(|deferred| match header.address {
                Address::Async { async_id } => deferred.async_id == async_id,
                Address::Sync { .. } => deferred.request.message_id == header.message_id,
            });
        let Some(at) = found else {
            return Ok(());
        };
        let cancelled = self.deferred.remove(at);
        let body = behind_header(empty_error)?;
        self.push(Sent::Response {
            request: cancelled.request,
            address: Address::Async {
                async_id: cancelled.async_id,
            },
            status: NtStatus::CANCELLED,
            body,
        })
    }
}
