//! The client's file requests carried end to end against the scripted
//! server: creates, data, listings, information, watches, leases and their
//! breaks, related compounds, a durable open reclaimed over a new
//! connection, and the server's shares listed through `srvsvc` on `IPC$`.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::time::Time64;

use super::tests::{connected, connected_with, events, pump};
use super::{Client, Event, Request};
use crate::connection::{Body, Message, Ticket};
use crate::file::create::{
    Access, CreateAction, CreateOptions, CreateRequest, CreateResponse, Disposition, Durable,
    ShareAccess,
};
use crate::file::dir::{ListingFlags, QueryDirectoryRequest, QueryDirectoryResponse};
use crate::file::info::{
    BasicInfo, BasicUpdate, Class, DispositionInfo, EndOfFileInfo, FsAttribute, FsVolume,
    QueryInfoRequest, QueryInfoResponse, RenameInfo, SetInfoRequest, SetInfoResponse, StandardInfo,
    MAX_INFO,
};
use crate::file::io::{
    CloseRequest, CloseResponse, ClosedAttributes, FlushRequest, ReadRequest, ReadResponse,
    WriteRequest, WriteResponse,
};
use crate::file::lease::{BreakNotice, Lease, LeaseAck, LeaseBreak, LeaseState};
use crate::file::name::SharePath;
use crate::file::notify::{
    Action, Change, ChangeNotifyRequest, ChangeNotifyResponse, NotifyFilter,
};
use crate::file::{credit_charge, FileAttributes, FileId, MAX_IO, NO_FILE};
use crate::header::{Command, Flags, Header};
use crate::negotiate::Dialect;
use crate::peer::{Fault, Peer, PeerConfig};
use crate::rpc::{ShareKind, ShareList, ShareListing};
use crate::simple::decode_empty;
use crate::status::NtStatus;
use crate::SmbError;

const CREATE_GUID: [u8; 16] = *b"create guid 0001";
const KEY: [u8; 16] = *b"lease key 0001ab";
const OTHER_KEY: [u8; 16] = *b"lease key 0002ab";

fn path(names: &[&str]) -> SharePath {
    names
        .iter()
        .try_fold(SharePath::root(), |path, name| path.join(name))
        .expect("a path")
}

fn status(message: &[u8]) -> NtStatus {
    Header::decode(message).expect("a header").status
}

fn one(command: Command, body: Body<'_>) -> Request<'_> {
    Request {
        command,
        body,
        credit_charge: 1,
        flags: Flags::NONE,
    }
}

fn related(command: Command, body: Body<'_>) -> Request<'_> {
    Request {
        flags: Flags::RELATED_OPERATIONS,
        ..one(command, body)
    }
}

/// Every response the client reports, by ticket; anything else is a failure.
fn responses(client: &mut Client) -> Vec<(Ticket, Message)> {
    events(client)
        .into_iter()
        .map(|event| match event {
            Event::Response { ticket, message } => (ticket, message),
            other => panic!("{other:?}"),
        })
        .collect()
}

fn take(answered: &mut Vec<(Ticket, Message)>, ticket: Ticket) -> Message {
    let at = answered
        .iter()
        .position(|(answering, _)| *answering == ticket)
        .expect("answered");
    answered.remove(at).1
}

fn opening(names: &[&str], disposition: Disposition) -> CreateRequest {
    CreateRequest {
        path: path(names),
        access: Access::READ_DATA
            .with(Access::WRITE_DATA)
            .with(Access::READ_ATTRIBUTES)
            .with(Access::WRITE_ATTRIBUTES)
            .with(Access::DELETE),
        attributes: FileAttributes::NONE,
        share: ShareAccess::READ
            .with(ShareAccess::WRITE)
            .with(ShareAccess::DELETE),
        disposition,
        options: CreateOptions::NON_DIRECTORY_FILE,
        lease: None,
        durable: None,
        maximal_access: false,
        disk_id: false,
    }
}

fn directory(names: &[&str], disposition: Disposition) -> CreateRequest {
    CreateRequest {
        options: CreateOptions::DIRECTORY_FILE,
        ..opening(names, disposition)
    }
}

fn leased(names: &[&str], key: [u8; 16], state: LeaseState) -> CreateRequest {
    CreateRequest {
        lease: Some(Lease {
            key,
            state,
            parent: None,
            epoch: 0,
            breaking: false,
        }),
        ..opening(names, Disposition::OpenIf)
    }
}

/// A connected client, its server, and the tree its requests go on.
struct Files {
    client: Client,
    peer: Peer,
    tree: u32,
}

impl Files {
    fn new(dialect: Dialect) -> Self {
        let (client, peer, tree) = connected(&PeerConfig::new(dialect));
        Self {
            client,
            peer,
            tree: tree.id,
        }
    }

    /// `requests` sent as one frame, and each one's response.
    fn calls(&mut self, requests: &[Request<'_>]) -> Vec<Message> {
        let tickets = self.client.send(self.tree, requests).expect("sends");
        pump(&mut self.client, &mut self.peer).expect("carries");
        let mut answered = responses(&mut self.client);
        let messages = tickets
            .iter()
            .map(|ticket| take(&mut answered, *ticket))
            .collect();
        assert!(answered.is_empty(), "{answered:?}");
        messages
    }

    fn call(&mut self, command: Command, body: Body<'_>) -> Message {
        self.calls(&[one(command, body)]).remove(0)
    }

    /// Send `request` and carry it, its response still to come.
    fn start(&mut self, command: Command, body: Body<'_>) -> Ticket {
        let [ticket] = self
            .client
            .send(self.tree, &[one(command, body)])
            .expect("sends")[..]
        else {
            panic!("one ticket");
        };
        pump(&mut self.client, &mut self.peer).expect("carries");
        ticket
    }

    fn create(&mut self, request: &CreateRequest) -> Result<CreateResponse, NtStatus> {
        let message = self.call(Command::Create, &|enc| request.encode(enc));
        match status(&message) {
            NtStatus::SUCCESS => Ok(CreateResponse::decode(&message).expect("a create response")),
            refused => Err(refused),
        }
    }

    fn write(&mut self, file_id: FileId, offset: u64, data: &[u8]) -> u32 {
        let write = WriteRequest {
            offset,
            file_id,
            write_through: false,
            data,
        };
        let message = self.call(Command::Write, &|enc| write.encode(enc));
        WriteResponse::decode(&message).expect("written").count
    }

    fn read(&mut self, file_id: FileId, offset: u64, length: u32) -> Result<Vec<u8>, NtStatus> {
        let read = ReadRequest::new(file_id, offset, length);
        let message = self.call(Command::Read, &|enc| read.encode(enc));
        match status(&message) {
            NtStatus::SUCCESS => Ok(ReadResponse::decode(&message).expect("read").data.to_vec()),
            refused => Err(refused),
        }
    }

    fn close(&mut self, file_id: FileId) -> Option<ClosedAttributes> {
        let close = CloseRequest {
            post_query: true,
            file_id,
        };
        let message = self.call(Command::Close, &|enc| close.encode(enc));
        assert_eq!(status(&message), NtStatus::SUCCESS);
        CloseResponse::decode(&message).expect("closed").attributes
    }

    fn query(&mut self, file_id: FileId, class: Class) -> Vec<u8> {
        let query = QueryInfoRequest {
            class,
            output_length: u32::try_from(MAX_INFO).expect("a length"),
            file_id,
        };
        let message = self.call(Command::QueryInfo, &|enc| query.encode(enc));
        assert_eq!(status(&message), NtStatus::SUCCESS, "{class:?}");
        QueryInfoResponse::decode(&message)
            .expect("queried")
            .output
            .to_vec()
    }

    fn set(&mut self, set: &SetInfoRequest) -> NtStatus {
        let message = self.call(Command::SetInfo, &|enc| set.encode(enc));
        let answered = status(&message);
        if answered == NtStatus::SUCCESS {
            assert_eq!(SetInfoResponse::decode(&message), Ok(SetInfoResponse));
        }
        answered
    }

    fn list(&mut self, file_id: FileId, flags: ListingFlags, output_length: u32) -> Message {
        let query = QueryDirectoryRequest {
            flags,
            file_index: 0,
            file_id,
            output_length,
        };
        self.call(Command::QueryDirectory, &|enc| query.encode(enc))
    }
}

/// A listing's names, a dot entry as `dot`, since it is no name a path takes.
fn names(message: &[u8]) -> Vec<String> {
    QueryDirectoryResponse::decode(message)
        .expect("a listing")
        .entries()
        .map(|entry| {
            let entry = entry.expect("well-formed");
            if entry.is_dot() {
                String::from("dot")
            } else {
                entry.name().expect("a name both sides carry")
            }
        })
        .collect()
}

#[test]
fn a_file_is_created_written_flushed_read_and_closed() {
    for dialect in [Dialect::Smb300, Dialect::Smb302, Dialect::Smb311] {
        let mut files = Files::new(dialect);
        let created = files
            .create(&opening(&["notes.txt"], Disposition::Create))
            .expect("creates");
        assert_eq!(created.action, CreateAction::Created);
        let id = created.file_id;
        assert_eq!(files.write(id, 0, b"hello"), 5);
        assert_eq!(files.write(id, 5, b" world"), 6);
        let flushed = files.call(Command::Flush, &|enc| {
            FlushRequest { file_id: id }.encode(enc);
        });
        assert_eq!(
            (status(&flushed), decode_empty(&flushed)),
            (NtStatus::SUCCESS, Ok(()))
        );
        assert_eq!(files.read(id, 0, 64), Ok(b"hello world".to_vec()));
        assert_eq!(files.read(id, 6, 5), Ok(b"world".to_vec()));
        assert_eq!(files.read(id, 11, 1), Err(NtStatus::END_OF_FILE));
        let closed = files.close(id).expect("the attributes asked for");
        assert_eq!(closed.end_of_file, 11);
        assert_eq!(
            files.peer.share().contents(&["notes.txt"]),
            Some(&b"hello world"[..])
        );
        assert_eq!(
            files.read(id, 0, 1),
            Err(NtStatus::FILE_CLOSED),
            "{dialect:?}"
        );
    }
}

#[test]
fn a_create_keeps_its_disposition() {
    let mut files = Files::new(Dialect::Smb311);
    let id = files
        .create(&opening(&["a.txt"], Disposition::Create))
        .expect("creates")
        .file_id;
    files.write(id, 0, b"old data");
    files.close(id);
    assert_eq!(
        files.create(&opening(&["a.txt"], Disposition::Create)),
        Err(NtStatus::OBJECT_NAME_COLLISION)
    );
    assert_eq!(
        files.create(&opening(&["missing.txt"], Disposition::Open)),
        Err(NtStatus::OBJECT_NAME_NOT_FOUND)
    );
    assert_eq!(
        files.create(&opening(&["nowhere", "a.txt"], Disposition::Create)),
        Err(NtStatus::OBJECT_PATH_NOT_FOUND)
    );
    let opened = files
        .create(&opening(&["a.txt"], Disposition::Open))
        .expect("opens");
    assert_eq!(
        (opened.action, opened.end_of_file),
        (CreateAction::Opened, 8)
    );
    let overwritten = files
        .create(&opening(&["a.txt"], Disposition::OverwriteIf))
        .expect("overwrites");
    assert_eq!(
        (overwritten.action, overwritten.end_of_file),
        (CreateAction::Overwritten, 0)
    );
    assert_eq!(
        files.create(&directory(&["a.txt"], Disposition::Open)),
        Err(NtStatus::NOT_A_DIRECTORY)
    );
}

#[test]
fn a_write_and_a_read_at_the_bound_are_charged_their_credits() {
    let mut files = Files::new(Dialect::Smb311);
    let id = files
        .create(&opening(&["big.bin"], Disposition::Create))
        .expect("creates")
        .file_id;
    let data: Vec<u8> = (0..MAX_IO)
        .map(|at| u8::try_from(at % 251).expect("a byte"))
        .collect();
    let length = u32::try_from(MAX_IO).expect("a length");
    let charge = credit_charge(MAX_IO);
    assert_eq!(charge, 16);
    let write = WriteRequest {
        offset: 0,
        file_id: id,
        write_through: true,
        data: &data,
    };
    let written = files.calls(&[Request {
        credit_charge: charge,
        ..one(Command::Write, &|enc| write.encode(enc))
    }]);
    assert_eq!(
        WriteResponse::decode(&written[0]).map(|w| w.count),
        Ok(length)
    );
    let read = ReadRequest::new(id, 0, length);
    let message = files.calls(&[Request {
        credit_charge: charge,
        ..one(Command::Read, &|enc| read.encode(enc))
    }]);
    assert_eq!(
        ReadResponse::decode(&message[0]).map(|r| r.data),
        Ok(&data[..])
    );
}

#[test]
fn a_directory_lists_its_entries_a_page_at_a_time() {
    let mut files = Files::new(Dialect::Smb311);
    let docs = files
        .create(&directory(&["docs"], Disposition::Create))
        .expect("makes the directory")
        .file_id;
    for name in ["a.txt", "b.txt", "c.txt"] {
        let id = files
            .create(&opening(&["docs", name], Disposition::Create))
            .expect("creates")
            .file_id;
        files.close(id);
    }
    // Room for ".", ".." and "a.txt", and not for "b.txt" after them.
    let page = 340;
    let first = files.list(docs, ListingFlags::RESTART_SCANS, page);
    assert_eq!(names(&first), ["dot", "dot", "a.txt"]);
    let second = files.list(docs, ListingFlags::NONE, page);
    assert_eq!(names(&second), ["b.txt", "c.txt"]);
    let done = files.list(docs, ListingFlags::NONE, page);
    assert_eq!(status(&done), NtStatus::NO_MORE_FILES);
    let again = files.list(docs, ListingFlags::RESTART_SCANS, 65_536);
    assert_eq!(names(&again), ["dot", "dot", "a.txt", "b.txt", "c.txt"]);
}

#[test]
fn information_is_queried_and_set_exactly() {
    let mut files = Files::new(Dialect::Smb311);
    let id = files
        .create(&opening(&["info.txt"], Disposition::Create))
        .expect("creates")
        .file_id;
    let size = EndOfFileInfo { end_of_file: 4096 };
    let grow = SetInfoRequest::new(Class::EndOfFile, id, |enc| size.encode(enc)).expect("a set");
    assert_eq!(files.set(&grow), NtStatus::SUCCESS);
    let standard = StandardInfo::decode(&files.query(id, Class::Standard)).expect("standard");
    assert_eq!((standard.end_of_file, standard.directory), (4096, false));

    let before_1970 = Time64::new(-86_400, 100).expect("a time");
    let after_2038 = Time64::new(4_102_444_800, 0).expect("a time");
    let update = BasicUpdate {
        created: Some(before_1970),
        written: Some(after_2038),
        attributes: Some(FileAttributes::HIDDEN),
        ..BasicUpdate::default()
    };
    let touch = SetInfoRequest::new(Class::Basic, id, |enc| update.encode(enc)).expect("a set");
    assert_eq!(files.set(&touch), NtStatus::SUCCESS);
    let basic = BasicInfo::decode(&files.query(id, Class::Basic)).expect("basic");
    assert_eq!(
        (basic.times.created, basic.times.written),
        (before_1970, after_2038)
    );
    assert!(basic.attributes.contains(FileAttributes::HIDDEN));

    let volume = FsVolume::decode(&files.query(id, Class::FsVolume)).expect("a volume");
    assert_eq!(volume.label, "share");
    let filesystem = FsAttribute::decode(&files.query(id, Class::FsAttribute)).expect("abilities");
    assert_eq!(filesystem.name, "NTFS");
}

#[test]
fn a_deleted_file_goes_with_its_last_open_and_a_full_directory_stays() {
    let mut files = Files::new(Dialect::Smb311);
    let docs = files
        .create(&directory(&["docs"], Disposition::Create))
        .expect("makes the directory")
        .file_id;
    let id = files
        .create(&opening(&["docs", "gone.txt"], Disposition::Create))
        .expect("creates")
        .file_id;
    let delete = DispositionInfo { delete: true };
    let doomed =
        SetInfoRequest::new(Class::Disposition, docs, |enc| delete.encode(enc)).expect("a set");
    assert_eq!(files.set(&doomed), NtStatus::DIRECTORY_NOT_EMPTY);
    let doomed =
        SetInfoRequest::new(Class::Disposition, id, |enc| delete.encode(enc)).expect("a set");
    assert_eq!(files.set(&doomed), NtStatus::SUCCESS);
    assert!(files.peer.share().contents(&["docs", "gone.txt"]).is_some());
    files.close(id);
    assert_eq!(files.peer.share().contents(&["docs", "gone.txt"]), None);
}

#[test]
fn a_watch_waits_and_reports_a_rename_beneath_it() {
    let mut files = Files::new(Dialect::Smb311);
    let inbox = files
        .create(&directory(&["inbox"], Disposition::Create))
        .expect("makes the directory")
        .file_id;
    let letter = files
        .create(&opening(&["inbox", "a.txt"], Disposition::Create))
        .expect("creates")
        .file_id;
    let watch = ChangeNotifyRequest {
        watch_tree: false,
        output_length: 4096,
        file_id: inbox,
        filter: NotifyFilter::FILE_NAME,
    };
    let watching = files.start(Command::ChangeNotify, &|enc| watch.encode(enc));
    assert_eq!(
        events(&mut files.client),
        [],
        "the interim response is no answer"
    );

    let rename = RenameInfo {
        replace: false,
        target: path(&["inbox", "b.txt"]),
    };
    let set = SetInfoRequest::new(Class::Rename, letter, |enc| rename.encode(enc)).expect("a set");
    let renaming = files.start(Command::SetInfo, &|enc| set.encode(enc));
    let mut answered = responses(&mut files.client);
    assert_eq!(status(&take(&mut answered, renaming)), NtStatus::SUCCESS);
    let reported = take(&mut answered, watching);
    assert_eq!(status(&reported), NtStatus::SUCCESS);
    let changes: Vec<Change> = ChangeNotifyResponse::decode(&reported)
        .expect("changes")
        .changes()
        .collect::<Result<_, _>>()
        .expect("well-formed");
    assert_eq!(
        changes,
        [
            Change {
                action: Some(Action::RenamedOld),
                path: path(&["a.txt"]),
            },
            Change {
                action: Some(Action::RenamedNew),
                path: path(&["b.txt"]),
            },
        ]
    );
}

#[test]
fn a_watch_ends_cancelled_or_with_its_open() {
    let mut files = Files::new(Dialect::Smb311);
    let inbox = files
        .create(&directory(&["inbox"], Disposition::Create))
        .expect("makes the directory")
        .file_id;
    let watch = ChangeNotifyRequest {
        watch_tree: true,
        output_length: 4096,
        file_id: inbox,
        filter: NotifyFilter::FILE_NAME,
    };
    let watching = files.start(Command::ChangeNotify, &|enc| watch.encode(enc));
    files.client.cancel(watching).expect("cancels");
    pump(&mut files.client, &mut files.peer).expect("carries");
    let mut answered = responses(&mut files.client);
    assert_eq!(status(&take(&mut answered, watching)), NtStatus::CANCELLED);

    let watching = files.start(Command::ChangeNotify, &|enc| watch.encode(enc));
    let close = CloseRequest {
        post_query: false,
        file_id: inbox,
    };
    let closing = files.start(Command::Close, &|enc| close.encode(enc));
    let mut answered = responses(&mut files.client);
    assert_eq!(
        status(&take(&mut answered, watching)),
        NtStatus::NOTIFY_CLEANUP
    );
    assert_eq!(status(&take(&mut answered, closing)), NtStatus::SUCCESS);
}

#[test]
fn a_conflicting_open_waits_for_the_lease_it_breaks() {
    let mut files = Files::new(Dialect::Smb311);
    let all = LeaseState::READ
        .with(LeaseState::HANDLE)
        .with(LeaseState::WRITE);
    let first = files
        .create(&leased(&["shared.txt"], KEY, all))
        .expect("creates");
    assert_eq!(first.lease.map(|lease| lease.state), Some(all));

    let second = leased(&["shared.txt"], OTHER_KEY, LeaseState::READ);
    let opening = files.start(Command::Create, &|enc| second.encode(enc));
    let [Event::Break { message }] = &events(&mut files.client)[..] else {
        panic!("a break and nothing else");
    };
    let BreakNotice::Lease(LeaseBreak {
        key,
        current,
        new,
        ack_required,
        ..
    }) = BreakNotice::decode(message).expect("a break")
    else {
        panic!("a lease break");
    };
    assert_eq!(
        (key, current, new, ack_required),
        (KEY, all, LeaseState::READ, true)
    );

    let ack = LeaseAck {
        key: KEY,
        state: LeaseState::READ,
    };
    let acking = files.start(Command::OplockBreak, &|enc| ack.encode(enc));
    let mut answered = responses(&mut files.client);
    assert_eq!(LeaseAck::decode(&take(&mut answered, acking)), Ok(ack));
    let opened = take(&mut answered, opening);
    assert_eq!(status(&opened), NtStatus::SUCCESS);
    let opened = CreateResponse::decode(&opened).expect("opened");
    assert_eq!(
        opened.lease.map(|lease| lease.state),
        Some(LeaseState::READ)
    );
}

#[test]
fn a_related_compound_opens_reads_and_closes_in_one_frame() {
    let mut files = Files::new(Dialect::Smb311);
    let id = files
        .create(&opening(&["compound.txt"], Disposition::Create))
        .expect("creates")
        .file_id;
    files.write(id, 0, b"in one frame");
    files.close(id);

    let open = opening(&["compound.txt"], Disposition::Open);
    let read = ReadRequest::new(NO_FILE, 0, 64);
    let close = CloseRequest {
        post_query: false,
        file_id: NO_FILE,
    };
    let answers = files.calls(&[
        one(Command::Create, &|enc| open.encode(enc)),
        related(Command::Read, &|enc| read.encode(enc)),
        related(Command::Close, &|enc| close.encode(enc)),
    ]);
    assert_eq!(
        answers
            .iter()
            .map(|answer| status(answer))
            .collect::<Vec<_>>(),
        [NtStatus::SUCCESS; 3]
    );
    assert_eq!(
        ReadResponse::decode(&answers[1]).map(|r| r.data),
        Ok(&b"in one frame"[..])
    );

    let missing = opening(&["missing.txt"], Disposition::Open);
    let answers = files.calls(&[
        one(Command::Create, &|enc| missing.encode(enc)),
        related(Command::Read, &|enc| read.encode(enc)),
    ]);
    assert_eq!(
        answers
            .iter()
            .map(|answer| status(answer))
            .collect::<Vec<_>>(),
        [NtStatus::OBJECT_NAME_NOT_FOUND; 2]
    );
}

#[test]
fn a_durable_open_is_reclaimed_over_a_new_connection() {
    let config = PeerConfig::new(Dialect::Smb311);
    let (client, peer, tree) = connected(&config);
    let mut files = Files {
        client,
        peer,
        tree: tree.id,
    };
    let held = LeaseState::READ.with(LeaseState::HANDLE);
    let durable = CreateRequest {
        durable: Some(Durable::Request {
            timeout_ms: 0,
            create_guid: CREATE_GUID,
        }),
        ..leased(&["durable.txt"], KEY, held)
    };
    let granted = files.create(&durable).expect("creates");
    assert_eq!(granted.durable_timeout_ms, Some(60_000));
    files.write(granted.file_id, 0, b"kept");
    let scratch = files
        .create(&opening(&["scratch.txt"], Disposition::Create))
        .expect("creates")
        .file_id;
    files.write(scratch, 0, b"lost with its connection");
    let old_session = files.peer.session_id().expect("a session");

    let share = files.peer.disconnect().expect("drops");
    assert_eq!(share.detached_opens(), 1);
    let (client, peer, tree) = connected_with(
        Peer::with_share(config.clone(), share),
        &config,
        Some(old_session),
    );
    assert_eq!(
        peer.previous_session_ids(),
        [old_session; 2],
        "on every leg"
    );
    let mut files = Files {
        client,
        peer,
        tree: tree.id,
    };

    let reconnect = |create_guid, lease| CreateRequest {
        durable: Some(Durable::Reconnect {
            file_id: granted.file_id,
            create_guid,
        }),
        lease,
        ..opening(&["durable.txt"], Disposition::Open)
    };
    let lease = durable.lease;
    assert_eq!(
        files.create(&reconnect(*b"another create!!", lease)),
        Err(NtStatus::OBJECT_NAME_NOT_FOUND)
    );
    assert_eq!(
        files.create(&reconnect(CREATE_GUID, None)),
        Err(NtStatus::OBJECT_NAME_NOT_FOUND)
    );
    let reclaimed = files
        .create(&reconnect(CREATE_GUID, lease))
        .expect("reclaims");
    assert_eq!(
        reclaimed.file_id[..8],
        granted.file_id[..8],
        "the persistent half"
    );
    assert_ne!(reclaimed.file_id, granted.file_id, "a new volatile half");
    assert_eq!(reclaimed.lease.map(|lease| lease.state), Some(held));
    assert_eq!(files.read(reclaimed.file_id, 0, 16), Ok(b"kept".to_vec()));
    assert_eq!(
        files.read(granted.file_id, 0, 16),
        Err(NtStatus::FILE_CLOSED)
    );
    assert_eq!(files.read(scratch, 0, 16), Err(NtStatus::FILE_CLOSED));
}

/// The shares `config`'s server lists through `srvsvc` on `IPC$`, and the
/// server after the listing.
fn list_shares(config: &PeerConfig) -> (Result<ShareList, SmbError>, Peer) {
    let (mut client, mut peer, _) = connected(config);
    let asked = client.connect_tree("IPC$").expect("asks");
    pump(&mut client, &mut peer).expect("connects");
    let ipc = match &events(&mut client)[..] {
        [Event::TreeConnected { ticket, tree }] if *ticket == asked => tree.id,
        other => panic!("{other:?}"),
    };
    let mut listing = ShareListing::new(config.host).expect("a listing");
    while let Some(command) = listing.command() {
        let tickets = client
            .send(ipc, &[one(command, &|enc| listing.encode(enc))])
            .expect("sends");
        pump(&mut client, &mut peer).expect("carries");
        let message = take(&mut responses(&mut client), tickets[0]);
        listing.answer(&message);
    }
    (listing.finish(), peer)
}

#[test]
fn the_shares_are_listed_through_the_srvsvc_pipe() {
    let (listed, peer) = list_shares(&PeerConfig::new(Dialect::Smb311));
    let list = listed.expect("listed");
    assert_eq!(list.omitted, 0);
    assert_eq!(
        list.shares
            .iter()
            .map(|share| (share.name.as_str(), share.kind, share.special))
            .collect::<Vec<_>>(),
        [
            ("share", ShareKind::Disk, false),
            ("IPC$", ShareKind::Ipc, true)
        ]
    );
    assert_eq!(list.shares[0].remark, "The test share");
    assert_eq!(peer.open_pipes(), 0, "the pipe is closed");
}

#[test]
fn a_listing_spanning_fragments_is_put_back_together() {
    let config = PeerConfig {
        extra_shares: 100,
        ..PeerConfig::new(Dialect::Smb300)
    };
    let (listed, _) = list_shares(&config);
    let list = listed.expect("listed");
    assert_eq!(list.shares.len(), 102);
    assert_eq!(list.shares[101].name, "share-099");
}

#[test]
fn a_listing_that_fails_says_why_and_closes_its_pipe() {
    for (config, failure) in [
        (
            PeerConfig {
                fault: Fault::RefuseBind,
                ..PeerConfig::new(Dialect::Smb311)
            },
            SmbError::Rpc(4),
        ),
        (
            PeerConfig {
                fault: Fault::DenyShareEnum,
                ..PeerConfig::new(Dialect::Smb311)
            },
            SmbError::Rpc(5),
        ),
        (
            PeerConfig {
                extra_shares: 300,
                ..PeerConfig::new(Dialect::Smb311)
            },
            SmbError::TooLarge,
        ),
    ] {
        let (listed, peer) = list_shares(&config);
        assert_eq!(listed, Err(failure), "{:?}", config.fault);
        assert_eq!(peer.open_pipes(), 0, "{failure:?}");
    }
}

#[test]
fn a_share_no_tree_connect_could_name_is_left_out_and_counted() {
    let config = PeerConfig {
        fault: Fault::HostileShareName,
        ..PeerConfig::new(Dialect::Smb311)
    };
    let (listed, _) = list_shares(&config);
    let list = listed.expect("listed");
    assert_eq!((list.shares.len(), list.omitted), (2, 1));
}
