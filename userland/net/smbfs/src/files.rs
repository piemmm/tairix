//! A worker's file operations: each one compound — open, act, close — sent on
//! a tree in one frame, whose answers are read here and reported as facts.
//!
//! An open lasts only its compound, so every operation sees the share as
//! other writers left it; a listing keeps its directory open between pages,
//! and a node the front holds keeps its open, with the lease on it, until the
//! front lets it go. An operation with no credit to go out on waits, in the
//! order it was asked, behind the ones before it.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use tairix_inline::ArrayVec;
use tairix_smb::client::{Client, Request};
use tairix_smb::connection::{Message, Ticket};
use tairix_smb::file::create::{
    Access, CreateOptions, CreateRequest, CreateResponse, Disposition, Durable, ShareAccess,
};
use tairix_smb::file::dir::{ListingFlags, QueryDirectoryRequest, QueryDirectoryResponse};
use tairix_smb::file::info::{
    Class, DispositionInfo, EndOfFileInfo, FsFullSize, QueryInfoRequest, QueryInfoResponse,
    RenameInfo, SetInfoRequest,
};
use tairix_smb::file::io::{
    CloseRequest, FlushRequest, ReadRequest, ReadResponse, WriteRequest, WriteResponse,
};
use tairix_smb::file::lease::{BreakNotice, Lease, LeaseAck, LeaseKey, LeaseState};
use tairix_smb::file::name::SharePath;
use tairix_smb::file::{credit_charge, FileAttributes, FileId, FileTimes, NO_FILE};
use tairix_smb::header::{Command, Flags, Header};
use tairix_smb::status::NtStatus;
use tairix_smb::wire::Encoder;
use tairix_smb::SmbError;

use crate::wire::{Durability, FileOp, Report, Space, Stat, CHUNK_MAX, IO_MAX};

/// The most output one listing page asks for.
const PAGE_LEN: u32 = 64 << 10;

/// The most entries one page can hold: each takes more than 64 bytes of it.
pub const PAGE_ENTRIES_MAX: usize = PAGE_LEN as usize / 64;

/// The most requests one operation's compound holds.
const COMPOUND_MAX: usize = 3;

const _: () = assert!(IO_MAX <= tairix_smb::file::MAX_IO);

/// Why a file operation cannot go on: a front that broke the protocol, or a
/// pipe that failed while reporting.
pub struct Halt;

/// How a worker reports one fact.
pub trait Tell {
    /// Report `report`.
    ///
    /// # Errors
    ///
    /// [`Halt`] when the pipe failed.
    fn tell(&mut self, report: &Report<'_>) -> Result<(), Halt>;
}

/// An operation, held with what it needs until a credit lets it go.
enum Planned {
    Stat(SharePath),
    Read(SharePath, u64, u32),
    Write(SharePath, u64, Vec<u8>),
    Open(SharePath, u32),
    Page(u32),
    Close(u32),
    Create(SharePath, bool),
    Remove(SharePath, Option<LeaseKey>),
    Rename(SharePath, SharePath, bool, Option<LeaseKey>),
    Truncate(SharePath, u64),
    Space,
    /// Open a node and keep it open: its path, handle and lease key,
    /// whether it is a directory and is written, and whether the open is to
    /// outlive the connection.
    Hold(SharePath, u32, LeaseKey, bool, bool, Durability),
    ReadHeld(u32, u64, u32),
    /// A write on a held open, and whether it may be a replay.
    WriteHeld(u32, u64, Vec<u8>, bool),
    Sync(u32),
    Resize(u32, u64),
    Ack(LeaseKey, LeaseState),
    /// Close an open a compound's own close left open.
    Shut(FileId),
}

/// What an operation's answers mean.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Kind {
    Stat,
    /// A read of at most this many bytes.
    Read(u32),
    Write,
    Open(u32),
    Page(u32),
    Close(u32),
    Done,
    Space,
    Shut,
    /// A held node's open.
    Held(u32),
    /// A read of at most this many bytes on a held open.
    ReadHeld(u32),
    WriteHeld,
    /// A sync, a resize or an acknowledgment: done, or refused.
    Acted,
}

impl Kind {
    /// Whether the operation's compound opens a node by path and ends by
    /// closing it.
    const fn closes(self) -> bool {
        matches!(
            self,
            Self::Stat | Self::Read(_) | Self::Write | Self::Done | Self::Space
        )
    }
}

/// An operation out on the wire: its tickets, and the answers in so far.
struct Op {
    request: u32,
    tree: u32,
    kind: Kind,
    tickets: ArrayVec<Ticket, COMPOUND_MAX>,
    answers: ArrayVec<Option<Message>, COMPOUND_MAX>,
}

/// An open the front keeps: a listing's directory, or a held node.
struct Handle {
    /// The front's name for it.
    name: u32,
    tree: u32,
    file_id: Option<FileId>,
    /// Whether a listing's first page has been asked for.
    started: bool,
    /// Whether the front let it go while its open was in flight: the open
    /// is closed as soon as it is answered.
    closing: bool,
}

/// Every file operation a worker holds.
pub struct Files {
    queue: VecDeque<(u32, u32, Planned)>,
    ops: Vec<Op>,
    /// The bytes staged for the one write the front is ordering: it stages a
    /// write's bytes and orders it before anything else.
    staged: Option<(u32, Vec<u8>)>,
    handles: Vec<Handle>,
}

impl Default for Files {
    fn default() -> Self {
        Self::new()
    }
}

fn opening(
    path: SharePath,
    access: Access,
    disposition: Disposition,
    options: CreateOptions,
) -> CreateRequest {
    CreateRequest {
        path,
        access,
        attributes: FileAttributes::NONE,
        share: ShareAccess::READ
            .with(ShareAccess::WRITE)
            .with(ShareAccess::DELETE),
        disposition,
        options,
        lease: None,
        durable: None,
        maximal_access: false,
        disk_id: true,
    }
}

/// The caching a held node asks: a directory's listing and handle, a file's
/// writes too.
const fn asked_lease(directory: bool) -> LeaseState {
    if directory {
        LeaseState::READ.with(LeaseState::HANDLE)
    } else {
        LeaseState::READ
            .with(LeaseState::HANDLE)
            .with(LeaseState::WRITE)
    }
}

/// A lease under `key` asking `state`.
const fn lease(key: LeaseKey, state: LeaseState) -> Lease {
    Lease {
        key,
        state,
        parent: None,
        epoch: 0,
        breaking: false,
    }
}

/// A lease state's bits as a report carries them.
fn lease_bits(state: LeaseState) -> u8 {
    u8::try_from(state.bits() & 0b111).unwrap_or(0)
}

/// What a node is, from its times, sizes and attributes.
fn stat_of(
    times: &FileTimes,
    size: u64,
    allocated: u64,
    attributes: FileAttributes,
    file_id: u64,
) -> Stat {
    let directory = attributes.contains(FileAttributes::DIRECTORY);
    Stat {
        directory,
        read_only: attributes.contains(FileAttributes::READONLY),
        size: if directory { 0 } else { size },
        allocated,
        file_id,
        created: times.created,
        modified: times.written,
        accessed: times.accessed,
        changed: times.changed,
    }
}

fn created_stat(response: &CreateResponse) -> Stat {
    stat_of(
        &response.times,
        response.end_of_file,
        response.allocation_size,
        response.attributes,
        opened_id(response),
    )
}

/// The server's id for what a create opened, or zero where it keeps none.
fn opened_id(created: &CreateResponse) -> u64 {
    created.disk_id.map_or(0, |id| id.file)
}

/// The report an operation that moved `count` bytes ends with.
const fn done(request: u32, count: u32, file_id: u64) -> Report<'static> {
    Report::Done {
        request,
        count,
        file_id,
    }
}

fn owned_path(path: &str) -> Result<SharePath, SmbError> {
    SharePath::parse(path)
}

/// One request on its own, not related to any other.
fn alone(command: Command, body: &dyn Fn(&mut Encoder), credit_charge: u16) -> Request<'_> {
    Request {
        command,
        body,
        credit_charge,
        flags: Flags::NONE,
    }
}

/// The header flags of a request that may repeat one a dropped connection
/// took.
const fn replaying(replay: bool) -> Flags {
    if replay {
        Flags::REPLAY_OPERATION
    } else {
        Flags::NONE
    }
}

/// The durable-handle context a held node's create carries, and whether it
/// repeats one a dropped connection took.
const fn durable_context(durability: Durability) -> (Option<Durable>, bool) {
    match durability {
        Durability::None => (None, false),
        // The server's own timeout, MS-SMB2's default where it keeps none.
        Durability::Ask {
            create_guid,
            replay,
        } => (
            Some(Durable::Request {
                timeout_ms: 0,
                create_guid,
            }),
            replay,
        ),
        Durability::Reclaim {
            create_guid,
            file_id,
        } => (
            Some(Durable::Reconnect {
                file_id,
                create_guid,
            }),
            false,
        ),
    }
}

/// Report the data of a read that asked at most `asked` bytes, then its end.
fn read_out(
    request: u32,
    asked: u32,
    read: &ReadResponse<'_>,
    file_id: u64,
    tell: &mut dyn Tell,
) -> Result<Result<(), SmbError>, Halt> {
    // More than was asked for is a server's lie, not data.
    if read.data.len() > asked as usize {
        return Ok(Err(SmbError::Protocol));
    }
    for chunk in read.data.chunks(CHUNK_MAX) {
        tell.tell(&Report::Data { request, chunk })?;
    }
    let count = u32::try_from(read.data.len()).unwrap_or(u32::MAX);
    tell.tell(&done(request, count, file_id)).map(Ok)
}

impl Files {
    /// No operations.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            queue: VecDeque::new(),
            ops: Vec::new(),
            staged: None,
            handles: Vec::new(),
        }
    }

    /// Hold `chunk` of what `request` will write. A chunk there is no room
    /// for is let go: the write it belongs to is refused for the bytes it
    /// lacks.
    ///
    /// # Errors
    ///
    /// [`Halt`] past [`IO_MAX`] bytes, which no write the front makes
    /// carries.
    pub fn stage(&mut self, request: u32, chunk: &[u8]) -> Result<(), Halt> {
        let data = match &mut self.staged {
            Some((held, data)) if *held == request => data,
            // Bytes a write the front never ordered left.
            staged => &mut staged.insert((request, Vec::new())).1,
        };
        if data.len() + chunk.len() > IO_MAX {
            return Err(Halt);
        }
        if data.try_reserve_exact(chunk.len()).is_ok() {
            data.extend_from_slice(chunk);
        }
        Ok(())
    }

    /// The bytes staged for `request`, when they are all of the `len` its
    /// write carries.
    fn staged_for(&mut self, request: u32, len: u32) -> Result<Vec<u8>, SmbError> {
        match self.staged.take_if(|(held, _)| *held == request) {
            Some((_, data)) if data.len() == len as usize => Ok(data),
            _ => Err(SmbError::OutOfMemory),
        }
    }

    /// Keep a new open as `handle` on `tree`.
    fn keep(&mut self, handle: u32, tree: u32) -> Result<(), SmbError> {
        if self.handles.iter().any(|held| held.name == handle) {
            return Err(SmbError::Protocol);
        }
        self.handles
            .try_reserve(1)
            .map_err(|_| SmbError::OutOfMemory)?;
        self.handles.push(Handle {
            name: handle,
            tree,
            file_id: None,
            started: false,
            closing: false,
        });
        Ok(())
    }

    /// Queue `op` on `tree` for `request`, or the error refusing it.
    pub fn order(&mut self, request: u32, tree: u32, op: &FileOp<'_>) -> Result<(), SmbError> {
        let planned = match *op {
            FileOp::Stat { path } => Planned::Stat(owned_path(path)?),
            FileOp::Read { path, offset, len } => Planned::Read(owned_path(path)?, offset, len),
            FileOp::Write { path, offset, len } => {
                let data = self.staged_for(request, len)?;
                Planned::Write(owned_path(path)?, offset, data)
            }
            FileOp::Open { path, handle } => {
                let path = owned_path(path)?;
                self.keep(handle, tree)?;
                Planned::Open(path, handle)
            }
            FileOp::Page { handle } => Planned::Page(handle),
            FileOp::Close { handle } => Planned::Close(handle),
            FileOp::Create { path, directory } => Planned::Create(owned_path(path)?, directory),
            FileOp::Remove { path, lease } => Planned::Remove(owned_path(path)?, lease),
            FileOp::Rename {
                from,
                to,
                replace,
                lease,
            } => {
                let to = owned_path(to)?;
                if to.is_root() {
                    return Err(SmbError::Name);
                }
                Planned::Rename(owned_path(from)?, to, replace, lease)
            }
            FileOp::Truncate { path, size } => Planned::Truncate(owned_path(path)?, size),
            FileOp::Space => Planned::Space,
            FileOp::Hold {
                path,
                handle,
                key,
                directory,
                write,
                durability,
            } => {
                let path = owned_path(path)?;
                self.keep(handle, tree)?;
                Planned::Hold(path, handle, key, directory, write, durability)
            }
            FileOp::ReadHeld {
                handle,
                offset,
                len,
            } => Planned::ReadHeld(handle, offset, len),
            FileOp::WriteHeld {
                handle,
                offset,
                len,
                replay,
            } => Planned::WriteHeld(handle, offset, self.staged_for(request, len)?, replay),
            FileOp::Sync { handle } => Planned::Sync(handle),
            FileOp::Resize { handle, size } => Planned::Resize(handle, size),
            FileOp::Ack { key, state } => {
                Planned::Ack(key, LeaseState::from_bits(u32::from(state)))
            }
        };
        self.queue
            .try_reserve(1)
            .map_err(|_| SmbError::OutOfMemory)?;
        self.queue.push_back((request, tree, planned));
        Ok(())
    }

    /// Let go of everything held on `tree`, which the server closes with
    /// the tree.
    pub fn forget_tree(&mut self, tree: u32) {
        self.handles.retain(|held| held.tree != tree);
        self.queue.retain(|(_, held, _)| *held != tree);
    }

    /// Whether `ticket` answers one of these operations.
    #[must_use]
    pub fn owns(&self, ticket: Ticket) -> bool {
        self.ops.iter().any(|op| op.tickets.contains(&ticket))
    }

    /// The break `message` carries, as the front is told it: a lease break
    /// only, since no oplock is ever asked for.
    ///
    /// # Errors
    ///
    /// [`Halt`] when the report could not be made.
    pub fn broke(message: &[u8], tell: &mut dyn Tell) -> Result<Result<(), SmbError>, Halt> {
        match BreakNotice::decode(message) {
            Ok(BreakNotice::Lease(notice)) => tell
                .tell(&Report::Break {
                    key: notice.key,
                    new: lease_bits(notice.new),
                    ack: notice.ack_required,
                })
                .map(Ok),
            Ok(BreakNotice::Oplock(_)) => Ok(Ok(())),
            Err(error) => Ok(Err(error)),
        }
    }

    /// Send what waits, in order, until a credit is wanting; whether anything
    /// went, or the error that ends the connection.
    ///
    /// # Errors
    ///
    /// [`Halt`] when a refusal could not be reported.
    pub fn send(
        &mut self,
        client: &mut Client,
        tell: &mut dyn Tell,
    ) -> Result<Result<bool, SmbError>, Halt> {
        let mut sent = false;
        while let Some((request, tree, planned)) = self.queue.front() {
            let (request, tree) = (*request, *tree);
            if let Planned::Close(handle) = *planned {
                if let Some(held) = self
                    .handles
                    .iter_mut()
                    .find(|held| held.name == handle && held.file_id.is_none())
                {
                    held.closing = true;
                    self.queue.pop_front();
                    tell.tell(&done(request, 0, 0))?;
                    continue;
                }
            }
            match self.launch(client, tree, planned) {
                Ok((kind, tickets)) => {
                    self.queue.pop_front();
                    let mut held = ArrayVec::new();
                    let mut answers = ArrayVec::new();
                    for ticket in tickets {
                        if held.try_push(ticket).is_err() || answers.try_push(None).is_err() {
                            return Ok(Err(SmbError::Protocol));
                        }
                    }
                    if self.ops.try_reserve(1).is_err() {
                        return Ok(Err(SmbError::OutOfMemory));
                    }
                    self.ops.push(Op {
                        request,
                        tree,
                        kind,
                        tickets: held,
                        answers,
                    });
                    sent = true;
                }
                Err(SmbError::NoCredit) => return Ok(Ok(sent)),
                Err(error) => {
                    if let Some((_, _, planned)) = self.queue.pop_front() {
                        match planned {
                            Planned::Open(_, handle)
                            | Planned::Close(handle)
                            | Planned::Hold(_, handle, ..) => {
                                self.handles.retain(|held| held.name != handle);
                            }
                            Planned::Shut(_) => continue,
                            _ => {}
                        }
                    }
                    tell.tell(&Report::Refused { request, error })?;
                }
            }
        }
        Ok(Ok(sent))
    }

    /// Send `planned` on `tree` as one compound, each body encoded straight
    /// into the frame: what its answers mean, and its tickets.
    fn launch(
        &self,
        client: &mut Client,
        tree: u32,
        planned: &Planned,
    ) -> Result<(Kind, Vec<Ticket>), SmbError> {
        let file = CreateOptions::NON_DIRECTORY_FILE;
        match planned {
            Planned::Stat(path) => {
                let open = opening(
                    path.clone(),
                    Access::READ_ATTRIBUTES,
                    Disposition::Open,
                    CreateOptions::NONE,
                );
                Ok((Kind::Stat, opened(client, tree, &open, None, true)?))
            }
            Planned::Read(path, offset, len) => {
                let access = Access::READ_DATA.with(Access::READ_ATTRIBUTES);
                let open = opening(path.clone(), access, Disposition::Open, file);
                let read = ReadRequest::new(NO_FILE, *offset, *len);
                let body = |enc: &mut Encoder| read.encode(enc);
                let middle: Middle<'_> = (Command::Read, &body, credit_charge(*len as usize));
                Ok((
                    Kind::Read(*len),
                    opened(client, tree, &open, Some(middle), true)?,
                ))
            }
            Planned::Write(path, offset, data) => {
                let access = Access::WRITE_DATA.with(Access::READ_ATTRIBUTES);
                let open = opening(path.clone(), access, Disposition::Open, file);
                let write = WriteRequest {
                    offset: *offset,
                    file_id: NO_FILE,
                    write_through: false,
                    data,
                };
                let body = |enc: &mut Encoder| write.encode(enc);
                let middle: Middle<'_> = (Command::Write, &body, credit_charge(data.len()));
                Ok((
                    Kind::Write,
                    opened(client, tree, &open, Some(middle), true)?,
                ))
            }
            Planned::Open(path, handle) => {
                let access = Access::READ_DATA.with(Access::READ_ATTRIBUTES);
                let open = opening(
                    path.clone(),
                    access,
                    Disposition::Open,
                    CreateOptions::DIRECTORY_FILE,
                );
                Ok((
                    Kind::Open(*handle),
                    opened(client, tree, &open, None, false)?,
                ))
            }
            Planned::Page(handle) | Planned::Close(handle) => {
                self.on_listing(client, planned, *handle)
            }
            Planned::Create(..)
            | Planned::Remove(..)
            | Planned::Rename(..)
            | Planned::Truncate(..) => launch_change(client, tree, planned),
            Planned::Space => Ok((Kind::Space, space(client, tree)?)),
            Planned::Hold(..)
            | Planned::ReadHeld(..)
            | Planned::WriteHeld(..)
            | Planned::Sync(_)
            | Planned::Resize(..)
            | Planned::Ack(..) => self.launch_held(client, tree, planned),
            Planned::Shut(file_id) => Ok((Kind::Shut, close(client, tree, *file_id)?)),
        }
    }

    /// Send `planned`, an operation on a node the front holds open, or its
    /// open itself.
    fn launch_held(
        &self,
        client: &mut Client,
        tree: u32,
        planned: &Planned,
    ) -> Result<(Kind, Vec<Ticket>), SmbError> {
        match planned {
            Planned::Hold(path, handle, key, directory, write, durability) => {
                let file = CreateOptions::NON_DIRECTORY_FILE;
                let mut access = Access::READ_DATA.with(Access::READ_ATTRIBUTES);
                if *write {
                    access = access.with(Access::WRITE_DATA);
                }
                let options = if *directory {
                    CreateOptions::DIRECTORY_FILE
                } else {
                    file
                };
                let (durable, replay) = durable_context(*durability);
                let open = CreateRequest {
                    lease: Some(lease(*key, asked_lease(*directory))),
                    durable,
                    ..opening(path.clone(), access, Disposition::Open, options)
                };
                let body = |enc: &mut Encoder| open.encode(enc);
                let request = Request {
                    flags: replaying(replay),
                    ..alone(Command::Create, &body, 1)
                };
                Ok((Kind::Held(*handle), client.send(tree, &[request])?))
            }
            Planned::ReadHeld(handle, offset, len) => {
                let (tree, file_id) = self.held(*handle)?;
                let read = ReadRequest::new(file_id, *offset, *len);
                let body = |enc: &mut Encoder| read.encode(enc);
                let request = alone(Command::Read, &body, credit_charge(*len as usize));
                Ok((Kind::ReadHeld(*len), client.send(tree, &[request])?))
            }
            Planned::WriteHeld(handle, offset, data, replay) => {
                let (tree, file_id) = self.held(*handle)?;
                let write = WriteRequest {
                    offset: *offset,
                    file_id,
                    write_through: false,
                    data,
                };
                let body = |enc: &mut Encoder| write.encode(enc);
                let request = Request {
                    flags: replaying(*replay),
                    ..alone(Command::Write, &body, credit_charge(data.len()))
                };
                Ok((Kind::WriteHeld, client.send(tree, &[request])?))
            }
            Planned::Sync(handle) => {
                let (tree, file_id) = self.held(*handle)?;
                let flush = FlushRequest { file_id };
                let body = |enc: &mut Encoder| flush.encode(enc);
                Ok((
                    Kind::Acted,
                    client.send(tree, &[alone(Command::Flush, &body, 1)])?,
                ))
            }
            Planned::Resize(handle, size) => {
                let (tree, file_id) = self.held(*handle)?;
                let length = SetInfoRequest::new(Class::EndOfFile, file_id, |enc| {
                    EndOfFileInfo { end_of_file: *size }.encode(enc);
                })?;
                let body = |enc: &mut Encoder| length.encode(enc);
                Ok((
                    Kind::Acted,
                    client.send(tree, &[alone(Command::SetInfo, &body, 1)])?,
                ))
            }
            Planned::Ack(key, state) => {
                let ack = LeaseAck {
                    key: *key,
                    state: *state,
                };
                let body = |enc: &mut Encoder| ack.encode(enc);
                Ok((
                    Kind::Acted,
                    client.send(tree, &[alone(Command::OplockBreak, &body, 1)])?,
                ))
            }
            _ => Err(SmbError::Protocol),
        }
    }

    /// Send a page or the close of the open `handle` names.
    fn on_listing(
        &self,
        client: &mut Client,
        planned: &Planned,
        handle: u32,
    ) -> Result<(Kind, Vec<Ticket>), SmbError> {
        let held = self.handle(handle)?;
        let file_id = held.file_id.ok_or(SmbError::Protocol)?;
        if let Planned::Page(_) = planned {
            let query = QueryDirectoryRequest {
                flags: if held.started {
                    ListingFlags::NONE
                } else {
                    ListingFlags::RESTART_SCANS
                },
                file_index: 0,
                file_id,
                output_length: PAGE_LEN,
            };
            let body = |enc: &mut Encoder| query.encode(enc);
            let request = alone(
                Command::QueryDirectory,
                &body,
                credit_charge(PAGE_LEN as usize),
            );
            return Ok((Kind::Page(handle), client.send(held.tree, &[request])?));
        }
        Ok((Kind::Close(handle), close(client, held.tree, file_id)?))
    }

    fn handle(&self, handle: u32) -> Result<&Handle, SmbError> {
        self.handles
            .iter()
            .find(|held| held.name == handle)
            .ok_or(SmbError::Protocol)
    }

    /// The tree and open of the node `handle` holds open.
    fn held(&self, handle: u32) -> Result<(u32, FileId), SmbError> {
        let held = self.handle(handle)?;
        Ok((held.tree, held.file_id.ok_or(SmbError::Protocol)?))
    }

    /// Take the answer `message` to `ticket`, reporting the operation once all
    /// its answers are in.
    ///
    /// # Errors
    ///
    /// [`Halt`] when a report could not be made.
    pub fn answer(
        &mut self,
        ticket: Ticket,
        message: Message,
        tell: &mut dyn Tell,
    ) -> Result<(), Halt> {
        let Some(at) = self.ops.iter().position(|op| op.tickets.contains(&ticket)) else {
            return Ok(());
        };
        let op = &mut self.ops[at];
        if let Some(place) = op.tickets.iter().position(|held| *held == ticket) {
            op.answers[place] = Some(message);
        }
        if op.answers.iter().any(Option::is_none) {
            return Ok(());
        }
        let op = self.ops.swap_remove(at);
        let request = op.request;
        let mut answers: ArrayVec<Message, COMPOUND_MAX> = ArrayVec::new();
        for answer in op.answers.into_iter().flatten() {
            // As many answers as the compound had tickets, so all fit.
            let _ = answers.try_push(answer);
        }
        // The close of an open left behind reports to no one.
        if op.kind == Kind::Shut {
            return Ok(());
        }
        if op.kind.closes() {
            self.shut_left_open(op.tree, &answers);
        }
        match self.finish(op.kind, request, &answers, tell)? {
            Ok(()) => Ok(()),
            Err(error) => tell.tell(&Report::Refused { request, error }),
        }
    }

    /// Close what a compound's create opened when the compound's own close,
    /// failed by an error before it, left it open: a server may fail a
    /// related close for the request it follows.
    fn shut_left_open(&mut self, tree: u32, answers: &[Message]) {
        let status = |answer: &Message| Header::decode(answer).map(|header| header.status);
        let (Some(first), Some(last)) = (answers.first(), answers.last()) else {
            return;
        };
        if status(first) != Ok(NtStatus::SUCCESS) || status(last) == Ok(NtStatus::SUCCESS) {
            return;
        }
        let Ok(created) = CreateResponse::decode(first) else {
            return;
        };
        self.shut(tree, created.file_id);
    }

    /// Queue the close of `file_id` on `tree`; one there is no room to
    /// queue leaves the open to the session's end, which closes it.
    fn shut(&mut self, tree: u32, file_id: FileId) {
        if self.queue.try_reserve(1).is_ok() {
            self.queue.push_back((0, tree, Planned::Shut(file_id)));
        }
    }

    /// Report what an operation's answers say, or the error they carry.
    fn finish(
        &mut self,
        kind: Kind,
        request: u32,
        answers: &[Message],
        tell: &mut dyn Tell,
    ) -> Result<Result<(), SmbError>, Halt> {
        let mut statuses: ArrayVec<NtStatus, COMPOUND_MAX> = ArrayVec::new();
        for answer in answers {
            match Header::decode(answer) {
                // As many statuses as answers, so all fit.
                Ok(header) => {
                    let _ = statuses.try_push(header.status);
                }
                Err(error) => return Ok(Err(error)),
            }
        }
        let first = statuses
            .iter()
            .copied()
            .find(|status| *status != NtStatus::SUCCESS);
        match kind {
            Kind::Page(handle) => {
                return self.paged(handle, request, &answers[0], statuses[0], tell)
            }
            Kind::Close(handle) => {
                self.handles.retain(|held| held.name != handle);
                return Ok(match first {
                    None => tell.tell(&done(request, 0, 0)).map(Ok)?,
                    Some(status) => Err(SmbError::Status(status)),
                });
            }
            // A read from the end on reads nothing.
            Kind::Read(_)
                if statuses[0] == NtStatus::SUCCESS && statuses[1] == NtStatus::END_OF_FILE =>
            {
                return Ok(match CreateResponse::decode(&answers[0]) {
                    Ok(created) => tell.tell(&done(request, 0, opened_id(&created))).map(Ok)?,
                    Err(error) => Err(error),
                });
            }
            Kind::ReadHeld(_) if statuses[0] == NtStatus::END_OF_FILE => {
                return tell.tell(&done(request, 0, 0)).map(Ok);
            }
            _ => {}
        }
        if let Some(status) = first {
            if let Kind::Open(handle) | Kind::Held(handle) = kind {
                self.handles.retain(|held| held.name != handle);
            }
            return Ok(Err(SmbError::Status(status)));
        }
        self.succeeded(kind, request, answers, tell)
    }

    /// Report what an operation's answers, every one a success, say.
    fn succeeded(
        &mut self,
        kind: Kind,
        request: u32,
        answers: &[Message],
        tell: &mut dyn Tell,
    ) -> Result<Result<(), SmbError>, Halt> {
        match kind {
            Kind::Open(handle) | Kind::Held(handle) => {
                self.opened_held(kind, handle, request, &answers[0], tell)
            }
            Kind::Read(asked) => {
                let opened = CreateResponse::decode(&answers[0]).map(|created| opened_id(&created));
                match (opened, ReadResponse::decode(&answers[1])) {
                    (Ok(file_id), Ok(read)) => read_out(request, asked, &read, file_id, tell),
                    (Err(error), _) | (_, Err(error)) => Ok(Err(error)),
                }
            }
            Kind::ReadHeld(asked) => match ReadResponse::decode(&answers[0]) {
                Ok(read) => read_out(request, asked, &read, 0, tell),
                Err(error) => Ok(Err(error)),
            },
            Kind::Write => {
                let opened = CreateResponse::decode(&answers[0]).map(|created| opened_id(&created));
                Ok(match (opened, WriteResponse::decode(&answers[1])) {
                    (Ok(file_id), Ok(written)) => {
                        tell.tell(&done(request, written.count, file_id)).map(Ok)?
                    }
                    (Err(error), _) | (_, Err(error)) => Err(error),
                })
            }
            Kind::WriteHeld => Ok(match WriteResponse::decode(&answers[0]) {
                Ok(written) => tell.tell(&done(request, written.count, 0)).map(Ok)?,
                Err(error) => Err(error),
            }),
            Kind::Done => Ok(match CreateResponse::decode(&answers[0]) {
                Ok(created) => tell.tell(&done(request, 0, opened_id(&created))).map(Ok)?,
                Err(error) => Err(error),
            }),
            Kind::Acted => tell.tell(&done(request, 0, 0)).map(Ok),
            Kind::Space => {
                let space = QueryInfoResponse::decode(&answers[1])
                    .and_then(|answer| FsFullSize::decode(answer.output));
                Ok(match space {
                    Ok(full) => tell
                        .tell(&Report::Space {
                            request,
                            space: Space {
                                unit: full.sectors_per_unit.saturating_mul(full.bytes_per_sector),
                                total: full.total_units,
                                free: full.available_units,
                                available: full.caller_available_units,
                            },
                        })
                        .map(Ok)?,
                    Err(error) => Err(error),
                })
            }
            Kind::Stat => Ok(match CreateResponse::decode(&answers[0]) {
                Ok(created) => tell
                    .tell(&Report::Stat {
                        request,
                        stat: created_stat(&created),
                    })
                    .map(Ok)?,
                Err(error) => Err(error),
            }),
            Kind::Page(_) | Kind::Close(_) | Kind::Shut => Ok(Err(SmbError::Protocol)),
        }
    }

    /// Keep the open a listing's or a held node's create answered, or close
    /// it when the front let it go meanwhile.
    fn opened_held(
        &mut self,
        kind: Kind,
        handle: u32,
        request: u32,
        answer: &[u8],
        tell: &mut dyn Tell,
    ) -> Result<Result<(), SmbError>, Halt> {
        let created = match CreateResponse::decode(answer) {
            Ok(created) => created,
            Err(error) => {
                self.handles.retain(|held| held.name != handle);
                return Ok(Err(error));
            }
        };
        let Some(at) = self.handles.iter().position(|held| held.name == handle) else {
            return Ok(Ok(()));
        };
        let held = &mut self.handles[at];
        if held.closing {
            let tree = held.tree;
            self.handles.swap_remove(at);
            self.shut(tree, created.file_id);
        } else {
            held.file_id = Some(created.file_id);
        }
        let report = match kind {
            Kind::Held(_) => Report::Held {
                request,
                stat: created_stat(&created),
                // A lease mid-break is cached by nothing until its break
                // arrives.
                lease: created
                    .lease
                    .filter(|granted| !granted.breaking)
                    .map_or(0, |granted| lease_bits(granted.state)),
                file_id: created.file_id,
                durable_ms: created.durable_timeout_ms,
            },
            _ => done(request, 0, 0),
        };
        tell.tell(&report).map(Ok)
    }

    /// Report a listing page: each entry both sides can name, then whether
    /// the directory has more.
    fn paged(
        &mut self,
        handle: u32,
        request: u32,
        answer: &[u8],
        status: NtStatus,
        tell: &mut dyn Tell,
    ) -> Result<Result<(), SmbError>, Halt> {
        if let Some(held) = self.handles.iter_mut().find(|held| held.name == handle) {
            held.started = true;
        }
        match status {
            NtStatus::NO_MORE_FILES => tell
                .tell(&Report::Page {
                    request,
                    more: false,
                })
                .map(Ok),
            NtStatus::SUCCESS => {
                let listing = match QueryDirectoryResponse::decode(answer) {
                    Ok(listing) => listing,
                    Err(error) => return Ok(Err(error)),
                };
                for entry in listing.entries() {
                    let entry = match entry {
                        Ok(entry) => entry,
                        Err(error) => return Ok(Err(error)),
                    };
                    if entry.is_dot() {
                        continue;
                    }
                    // A name one side cannot carry names nothing reachable.
                    let Ok(name) = entry.name() else {
                        continue;
                    };
                    let stat = stat_of(
                        &entry.times,
                        entry.end_of_file,
                        entry.allocation_size,
                        entry.attributes,
                        entry.file_id,
                    );
                    tell.tell(&Report::Entry {
                        request,
                        name: &name,
                        stat,
                    })?;
                }
                tell.tell(&Report::Page {
                    request,
                    more: true,
                })
                .map(Ok)
            }
            status => Ok(Err(SmbError::Status(status))),
        }
    }
}

/// A request acting on what its compound's create opened: its command, its
/// body and its credit charge.
type Middle<'a> = (Command, &'a dyn Fn(&mut Encoder), u16);

/// Send the query of the share's space on `tree`, as one compound on its
/// root.
fn space(client: &mut Client, tree: u32) -> Result<Vec<Ticket>, SmbError> {
    let open = opening(
        SharePath::root(),
        Access::READ_ATTRIBUTES,
        Disposition::Open,
        CreateOptions::DIRECTORY_FILE,
    );
    let query = QueryInfoRequest {
        class: Class::FsFullSize,
        output_length: u32::try_from(FsFullSize::LEN).unwrap_or(u32::MAX),
        file_id: NO_FILE,
    };
    let body = |enc: &mut Encoder| query.encode(enc);
    let middle: Middle<'_> = (Command::QueryInfo, &body, 1);
    opened(client, tree, &open, Some(middle), true)
}

/// Send the close of `file_id` on `tree`.
fn close(client: &mut Client, tree: u32, file_id: FileId) -> Result<Vec<Ticket>, SmbError> {
    let close = CloseRequest {
        post_query: false,
        file_id,
    };
    let body = |enc: &mut Encoder| close.encode(enc);
    client.send(tree, &[alone(Command::Close, &body, 1)])
}

/// Send `open`, then `middle` acting on what it opened when there is one,
/// then — when `close` is set — the open's close, as one compound.
fn opened(
    client: &mut Client,
    tree: u32,
    open: &CreateRequest,
    middle: Option<Middle<'_>>,
    close: bool,
) -> Result<Vec<Ticket>, SmbError> {
    let open_body = |enc: &mut Encoder| open.encode(enc);
    let closing = CloseRequest {
        post_query: false,
        file_id: NO_FILE,
    };
    let close_body = |enc: &mut Encoder| closing.encode(enc);
    let related = |command, body, credit_charge| Request {
        command,
        body,
        credit_charge,
        flags: Flags::RELATED_OPERATIONS,
    };
    let mut requests: ArrayVec<Request<'_>, COMPOUND_MAX> = ArrayVec::new();
    let mut push = |request| requests.try_push(request).map_err(|_| SmbError::TooLarge);
    push(alone(Command::Create, &open_body, 1))?;
    if let Some((command, body, charge)) = middle {
        push(related(command, body, charge))?;
    }
    if close {
        push(related(Command::Close, &close_body, 1))?;
    }
    client.send(tree, &requests)
}

/// Send `planned`, an operation that changes the share by path.
fn launch_change(
    client: &mut Client,
    tree: u32,
    planned: &Planned,
) -> Result<(Kind, Vec<Ticket>), SmbError> {
    let file = CreateOptions::NON_DIRECTORY_FILE;
    match planned {
        Planned::Create(path, directory) => {
            let options = if *directory {
                CreateOptions::DIRECTORY_FILE
            } else {
                file
            };
            let open = opening(
                path.clone(),
                Access::READ_ATTRIBUTES,
                Disposition::Create,
                options,
            );
            Ok((Kind::Stat, opened(client, tree, &open, None, true)?))
        }
        // Marked for deletion by a set, not opened delete-on-close: only the
        // set answers that a directory is not empty.
        Planned::Remove(path, key) => {
            let open = CreateRequest {
                lease: key.map(|key| lease(key, asked_lease(false))),
                ..opening(
                    path.clone(),
                    Access::DELETE,
                    Disposition::Open,
                    CreateOptions::NONE,
                )
            };
            let delete = SetInfoRequest::new(Class::Disposition, NO_FILE, |enc| {
                DispositionInfo { delete: true }.encode(enc);
            })?;
            let body = |enc: &mut Encoder| delete.encode(enc);
            let middle: Middle<'_> = (Command::SetInfo, &body, 1);
            Ok((Kind::Done, opened(client, tree, &open, Some(middle), true)?))
        }
        Planned::Rename(from, to, replace, key) => {
            let open = CreateRequest {
                lease: key.map(|key| lease(key, asked_lease(false))),
                ..opening(
                    from.clone(),
                    Access::DELETE,
                    Disposition::Open,
                    CreateOptions::NONE,
                )
            };
            let rename = SetInfoRequest::new(Class::Rename, NO_FILE, |enc| {
                RenameInfo {
                    replace: *replace,
                    target: to.clone(),
                }
                .encode(enc);
            })?;
            let body = |enc: &mut Encoder| rename.encode(enc);
            let middle: Middle<'_> = (Command::SetInfo, &body, 1);
            Ok((Kind::Done, opened(client, tree, &open, Some(middle), true)?))
        }
        Planned::Truncate(path, size) => {
            let open = opening(path.clone(), Access::WRITE_DATA, Disposition::Open, file);
            let length = SetInfoRequest::new(Class::EndOfFile, NO_FILE, |enc| {
                EndOfFileInfo { end_of_file: *size }.encode(enc);
            })?;
            let body = |enc: &mut Encoder| length.encode(enc);
            let middle: Middle<'_> = (Command::SetInfo, &body, 1);
            Ok((Kind::Done, opened(client, tree, &open, Some(middle), true)?))
        }
        _ => Err(SmbError::Protocol),
    }
}
