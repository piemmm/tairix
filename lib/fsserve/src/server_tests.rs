use alloc::collections::BTreeSet;
use alloc::format;
use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::driver::filesystem::{
    AllocationClass, FilesystemWrite, NodeId, NodeKind, NodeSecurity, Unpinned, VolumeHost,
};
use tairix_abi::driver::DriverHandle;
use tairix_abi::fs::{SeekRegion, FS_NAME_MAX};
use tairix_abi::fsserve::{
    decode_reply, encode_request, Answer, DirEnd, Reply, Request, ServerState, Span,
    FSSERVE_REPLY_MAX, FSSERVE_REQUEST_MAX, FSSERVE_WINDOW_LEN,
};
use tairix_abi::time::Time64;
use tairix_abi::DriverError;

use super::Server;
use crate::host::{Clock, ServedHost};
use crate::testfs::{TestFs, ROOT};

struct Fixed;

/// The record the kernel hands every node these tests create.
const RECORD: NodeSecurity = NodeSecurity::new(0o640, 1000, 100);

impl Clock for Fixed {
    fn now_ns(&self) -> Option<u64> {
        Some(0)
    }

    fn wall_time(&self) -> Option<Time64> {
        None
    }
}

/// One call's reply, kept with the bytes it borrows from.
struct Answered {
    bytes: Vec<u8>,
}

impl Answered {
    fn reply<'a>(&'a self, request: &Request<'_>) -> Reply<'a> {
        decode_reply(&self.bytes, request).expect("the server's reply decodes")
    }
}

fn ask(
    server: &mut Server<'_, TestFs, Fixed>,
    request: &Request<'_>,
    class: AllocationClass,
    window: &mut [u8],
) -> Answered {
    let mut frame = vec![0u8; FSSERVE_REQUEST_MAX];
    let len = encode_request(request, class, &mut frame).expect("a request encodes");
    let mut bytes = vec![0u8; FSSERVE_REPLY_MAX];
    let len = server.answer(&frame[..len], window, &mut bytes);
    bytes.truncate(len);
    Answered { bytes }
}

/// What `request` answered, asked under the ordinary class.
fn answer_of(server: &mut Server<'_, TestFs, Fixed>, request: &Request<'_>) -> Answered {
    ask(
        server,
        request,
        AllocationClass::Ordinary,
        &mut window()[..],
    )
}

fn result<'a>(answered: &'a Answered, request: &Request<'_>) -> Result<Answer<'a>, DriverError> {
    answered.reply(request).result
}

fn window() -> Vec<u8> {
    vec![0u8; FSSERVE_WINDOW_LEN]
}

fn create(server: &mut Server<'_, TestFs, Fixed>, name: &[u8], kind: NodeKind) -> NodeId {
    create_in(server, ROOT, name, kind)
}

fn create_in(
    server: &mut Server<'_, TestFs, Fixed>,
    dir: NodeId,
    name: &[u8],
    kind: NodeKind,
) -> NodeId {
    let request = Request::Create {
        dir,
        name,
        kind,
        record: RECORD,
    };
    match result(&answer_of(server, &request), &request) {
        Ok(Answer::Node(node)) => node,
        other => panic!("create answered {other:?}"),
    }
}

#[test]
fn a_created_node_is_found_and_described() {
    let host = ServedHost::new(Fixed);
    let mut server = Server::new(TestFs::new(), &host).unwrap();
    let node = create(&mut server, b"notes", NodeKind::RegularFile);
    let lookup = Request::Lookup {
        dir: ROOT,
        name: b"notes",
    };
    let answered = answer_of(&mut server, &lookup);
    assert_eq!(result(&answered, &lookup), Ok(Answer::Node(node)));
    let info = Request::NodeInfo { node };
    let answered = answer_of(&mut server, &info);
    let Ok(Answer::Info(info)) = result(&answered, &info) else {
        panic!("an info answer");
    };
    assert_eq!(info.kind, NodeKind::RegularFile);
    assert_eq!(info.ino, node.raw());
    let security = Request::Security { node };
    let answered = answer_of(&mut server, &security);
    assert_eq!(
        result(&answered, &security),
        Ok(Answer::Security(RECORD)),
        "a node is born with the record the kernel sent"
    );
    let missing = Request::Lookup {
        dir: ROOT,
        name: b"absent",
    };
    let answered = answer_of(&mut server, &missing);
    assert_eq!(result(&answered, &missing), Err(DriverError::NotFound));
}

#[test]
fn file_data_moves_through_the_window_at_its_span() {
    let host = ServedHost::new(Fixed);
    let mut server = Server::new(TestFs::new(), &host).unwrap();
    let file = create(&mut server, b"data", NodeKind::RegularFile);
    let mut shared = window();
    shared[100..105].copy_from_slice(b"hello");
    let write = Request::WriteAt {
        file,
        offset: 3,
        span: Span::new(100, 5).unwrap(),
    };
    let answered = ask(&mut server, &write, AllocationClass::Ordinary, &mut shared);
    assert_eq!(result(&answered, &write), Ok(Answer::Count(5)));
    let read = Request::ReadAt {
        file,
        offset: 0,
        span: Span::new(2000, 64).unwrap(),
    };
    let answered = ask(&mut server, &read, AllocationClass::Ordinary, &mut shared);
    assert_eq!(result(&answered, &read), Ok(Answer::Count(8)));
    assert_eq!(&shared[2000..2008], b"\0\0\0hello");
}

/// A truncate and a punch reach the driver with the file they name, as a
/// byte write does.
#[test]
fn a_truncate_and_a_punch_change_the_file_they_name() {
    let host = ServedHost::new(Fixed);
    let mut server = Server::new(TestFs::new(), &host).unwrap();
    let file = create(&mut server, b"data", NodeKind::RegularFile);
    let other = create(&mut server, b"other", NodeKind::RegularFile);
    let mut shared = window();
    shared[..8].copy_from_slice(b"abcdefgh");
    for target in [file, other] {
        let write = Request::WriteAt {
            file: target,
            offset: 0,
            span: Span::new(0, 8).unwrap(),
        };
        let answered = ask(&mut server, &write, AllocationClass::Ordinary, &mut shared);
        assert_eq!(result(&answered, &write), Ok(Answer::Count(8)));
    }
    let punch = Request::Punch {
        file,
        offset: 2,
        len: 3,
    };
    assert_eq!(
        result(&answer_of(&mut server, &punch), &punch),
        Ok(Answer::Done)
    );
    let truncate = Request::Truncate { file, size: 6 };
    assert_eq!(
        result(&answer_of(&mut server, &truncate), &truncate),
        Ok(Answer::Done)
    );
    for (target, held) in [(file, &b"ab\0\0\0f"[..]), (other, &b"abcdefgh"[..])] {
        let read = Request::ReadAt {
            file: target,
            offset: 0,
            span: Span::new(100, 8).unwrap(),
        };
        let answered = ask(&mut server, &read, AllocationClass::Ordinary, &mut shared);
        let count = u32::try_from(held.len()).unwrap();
        assert_eq!(result(&answered, &read), Ok(Answer::Count(count)));
        assert_eq!(&shared[100..100 + held.len()], held);
    }
}

#[test]
fn a_listing_is_answered_in_batches_that_resume_where_they_stopped() {
    let host = ServedHost::new(Fixed);
    let mut server = Server::new(TestFs::new(), &host).unwrap();
    let mut expected = BTreeSet::new();
    for n in 0..700 {
        let name = format!("{n:0>200}");
        server
            .filesystem()
            .create(ROOT, name.as_bytes(), NodeKind::RegularFile, &RECORD)
            .unwrap();
        expected.insert(name.into_bytes());
    }
    let mut seen = Vec::new();
    let mut cursor = 0;
    let mut after: Vec<u8> = Vec::new();
    let mut batches = 0;
    loop {
        let list = Request::ReadDir {
            dir: ROOT,
            cursor,
            after: &after,
        };
        let answered = answer_of(&mut server, &list);
        let Ok(Answer::Entries(batch)) = result(&answered, &list) else {
            panic!("a batch");
        };
        batches += 1;
        let mut resume = None;
        for (entry, name) in &batch {
            seen.push(name.to_vec());
            resume = Some((entry.next_cursor, name.to_vec()));
        }
        match batch.end() {
            DirEnd::Ended => break,
            DirEnd::More => {
                let (next, name) = resume.expect("a continuing batch has an entry");
                cursor = next;
                after = name;
            }
            DirEnd::Failed(err) => panic!("listing failed: {err:?}"),
        }
    }
    assert!(batches > 1, "the listing outgrew one reply");
    assert_eq!(seen.len(), expected.len(), "each entry exactly once");
    assert_eq!(seen.into_iter().collect::<BTreeSet<_>>(), expected);
}

#[test]
fn a_name_no_path_can_spell_fails_the_listing_closed_after_the_good_ones() {
    let host = ServedHost::new(Fixed);
    let mut server = Server::new(TestFs::new(), &host).unwrap();
    let fs = server.filesystem();
    fs.create(ROOT, b"a-good", NodeKind::RegularFile, &RECORD)
        .unwrap();
    fs.create(ROOT, b"b/bad", NodeKind::RegularFile, &RECORD)
        .unwrap();
    fs.create(ROOT, b"c-good", NodeKind::RegularFile, &RECORD)
        .unwrap();
    let list = Request::ReadDir {
        dir: ROOT,
        cursor: 0,
        after: b"",
    };
    let answered = answer_of(&mut server, &list);
    let Ok(Answer::Entries(batch)) = result(&answered, &list) else {
        panic!("a batch");
    };
    assert_eq!(batch.end(), DirEnd::Failed(DriverError::DeviceFault));
    let names: Vec<_> = batch.iter().map(|(_, name)| name.to_vec()).collect();
    assert_eq!(names, vec![b"a-good".to_vec()]);
}

#[test]
fn listing_a_file_fails_with_the_drivers_error() {
    let host = ServedHost::new(Fixed);
    let mut server = Server::new(TestFs::new(), &host).unwrap();
    let file = create(&mut server, b"f", NodeKind::RegularFile);
    let list = Request::ReadDir {
        dir: file,
        cursor: 0,
        after: b"",
    };
    let answered = answer_of(&mut server, &list);
    let Ok(Answer::Entries(batch)) = result(&answered, &list) else {
        panic!("a batch");
    };
    assert!(batch.is_empty());
    assert_eq!(batch.end(), DirEnd::Failed(DriverError::Unsupported));
}

#[test]
fn a_request_the_kernel_could_not_send_never_reaches_the_driver() {
    let host = ServedHost::new(Fixed);
    let mut server = Server::new(TestFs::new(), &host).unwrap();
    let request = Request::Create {
        dir: ROOT,
        name: b"ab",
        kind: NodeKind::RegularFile,
        record: RECORD,
    };
    let mut frame = vec![0u8; FSSERVE_REQUEST_MAX];
    let len = encode_request(&request, AllocationClass::Ordinary, &mut frame).unwrap();
    // The name's second byte becomes a separator.
    frame[4 + 8 + 2 + 1] = b'/';
    let mut out = vec![0u8; FSSERVE_REPLY_MAX];
    let replied = server.answer(&frame[..len], &mut window()[..], &mut out);
    assert_eq!(
        decode_reply(&out[..replied], &request).unwrap().result,
        Err(DriverError::OutOfRange)
    );
    assert!(
        server.filesystem().classes.is_empty(),
        "nothing was created"
    );
    // A frame naming no operation is answered with nothing a kernel accepts.
    assert_eq!(server.answer(&[0xFF; 9], &mut window()[..], &mut out), 0);
}

#[test]
fn each_call_runs_under_the_class_it_names_and_no_other() {
    let host = ServedHost::new(Fixed);
    let mut server = Server::new(TestFs::new(), &host).unwrap();
    let request = Request::Create {
        dir: ROOT,
        name: b"sys",
        kind: NodeKind::RegularFile,
        record: RECORD,
    };
    ask(
        &mut server,
        &request,
        AllocationClass::System,
        &mut window()[..],
    );
    server
        .filesystem()
        .create(ROOT, b"after", NodeKind::RegularFile, &RECORD)
        .unwrap();
    assert_eq!(
        server.filesystem().classes,
        vec![AllocationClass::System, AllocationClass::Ordinary]
    );
}

#[test]
fn a_driver_claiming_more_than_its_span_is_answered_as_a_device_fault() {
    let host = ServedHost::new(Fixed);
    let mut server = Server::new(TestFs::new(), &host).unwrap();
    let file = create(&mut server, b"f", NodeKind::RegularFile);
    // An empty file read claiming more than the span held.
    server.filesystem().overcount = 17;
    let read = Request::ReadAt {
        file,
        offset: 0,
        span: Span::new(0, 16).unwrap(),
    };
    let answered = answer_of(&mut server, &read);
    assert_eq!(result(&answered, &read), Err(DriverError::DeviceFault));
}

#[test]
fn a_link_target_is_answered_within_the_callers_buffer() {
    let host = ServedHost::new(Fixed);
    let mut server = Server::new(TestFs::new(), &host).unwrap();
    let make = Request::CreateLink {
        dir: ROOT,
        name: b"l",
        target: b"../target",
        record: RECORD,
    };
    let answered = answer_of(&mut server, &make);
    let Ok(Answer::Node(link)) = result(&answered, &make) else {
        panic!("a link");
    };
    let small = Request::ReadLink { link, cap: 4 };
    let answered = answer_of(&mut server, &small);
    assert_eq!(result(&answered, &small), Err(DriverError::BufferTooSmall));
    let enough = Request::ReadLink { link, cap: 64 };
    let answered = answer_of(&mut server, &enough);
    assert_eq!(result(&answered, &enough), Ok(Answer::Target(b"../target")));
}

#[test]
fn attributes_cross_the_protocol_and_a_volume_without_them_refuses() {
    let host = ServedHost::new(Fixed);
    let mut server = Server::new(TestFs::new(), &host).unwrap();
    let node = create(&mut server, b"f", NodeKind::RegularFile);
    let key = &b"user.colour"[..];
    let set = Request::SetAttr {
        node,
        key,
        value: b"",
    };
    let answered = answer_of(&mut server, &set);
    assert_eq!(result(&answered, &set), Ok(Answer::Done));
    let get = Request::GetAttr { node, key, cap: 0 };
    let answered = answer_of(&mut server, &get);
    assert_eq!(result(&answered, &get), Ok(Answer::Attr(Some(b""))));
    let list = Request::ListAttr {
        node,
        index: 0,
        cap: 64,
    };
    let answered = answer_of(&mut server, &list);
    assert_eq!(result(&answered, &list), Ok(Answer::Attr(Some(key))));
    let past = Request::ListAttr {
        node,
        index: 1,
        cap: 64,
    };
    let answered = answer_of(&mut server, &past);
    assert_eq!(result(&answered, &past), Ok(Answer::Attr(None)));
    let remove = Request::RemoveAttr { node, key };
    let answered = answer_of(&mut server, &remove);
    assert_eq!(result(&answered, &remove), Ok(Answer::Done));
    server.filesystem().keeps_attrs = false;
    let answered = answer_of(&mut server, &get);
    assert_eq!(result(&answered, &get), Err(DriverError::Unsupported));
}

#[test]
fn security_space_and_regions_cross_the_protocol() {
    let host = ServedHost::new(Fixed);
    let mut server = Server::new(TestFs::new(), &host).unwrap();
    let node = create(&mut server, b"f", NodeKind::RegularFile);
    let record = NodeSecurity::new(0o600, 1000, 100);
    let set = Request::SetSecurity {
        node,
        security: record,
    };
    let answered = answer_of(&mut server, &set);
    assert_eq!(result(&answered, &set), Ok(Answer::Done));
    let get = Request::Security { node };
    let answered = answer_of(&mut server, &get);
    assert_eq!(result(&answered, &get), Ok(Answer::Security(record)));
    let stats = Request::Stats;
    let answered = answer_of(&mut server, &stats);
    assert!(matches!(result(&answered, &stats), Ok(Answer::Stats(s)) if s.total_blocks == 100));
    // The defaulted seek: an empty file holds no region at all.
    let seek = Request::SeekRegion {
        file: node,
        offset: 0,
        region: SeekRegion::Data,
    };
    let answered = answer_of(&mut server, &seek);
    assert_eq!(result(&answered, &seek), Ok(Answer::Region(None)));
    let owned = Request::RecordsOwnership { node };
    let answered = answer_of(&mut server, &owned);
    assert_eq!(result(&answered, &owned), Ok(Answer::Owned(true)));
}

#[test]
fn every_reply_says_what_the_host_holds_and_a_publish_flushes() {
    let host = ServedHost::new(Fixed);
    let mut server = Server::new(TestFs::new(), &host).unwrap();
    let volume = DriverHandle::from_raw(9).unwrap();
    host.writeback_due(volume, Some(77));
    host.reserve_crossed(volume, true);
    let release = Request::Release {
        span: Span::new(0, FSSERVE_WINDOW_LEN).unwrap(),
    };
    let answered = answer_of(&mut server, &release);
    assert_eq!(
        answered.reply(&release),
        Reply {
            state: ServerState {
                unpublished: true,
                in_reserve: true,
            },
            result: Ok(Answer::Done),
        }
    );
    server.publish().unwrap();
    assert_eq!(server.filesystem().flushes, 1);
    let flush = Request::Flush;
    answer_of(&mut server, &flush);
    assert_eq!(server.filesystem().flushes, 2);
}

#[test]
fn a_removed_or_renamed_node_answers_as_the_driver_does() {
    let host = ServedHost::new(Fixed);
    let mut server = Server::new(TestFs::new(), &host).unwrap();
    create(&mut server, b"old", NodeKind::RegularFile);
    let name = [b'n'; FS_NAME_MAX];
    let rename = Request::Rename {
        src_dir: ROOT,
        src_name: b"old",
        dst_dir: ROOT,
        dst_name: &name,
    };
    let answered = answer_of(&mut server, &rename);
    assert_eq!(result(&answered, &rename), Ok(Answer::Done));
    let remove = Request::Remove {
        dir: ROOT,
        name: &name,
    };
    let answered = answer_of(&mut server, &remove);
    assert_eq!(result(&answered, &remove), Ok(Answer::Done));
    let again = answer_of(&mut server, &remove);
    assert_eq!(result(&again, &remove), Err(DriverError::NotFound));
}

#[test]
fn a_climb_answers_the_directory_holding_the_name() {
    let host = ServedHost::new(Fixed);
    let mut server = Server::new(TestFs::new(), &host).unwrap();
    let sub = create(&mut server, b"sub", NodeKind::Directory);
    let inner = create_in(&mut server, sub, b"inner", NodeKind::Directory);
    for (dir, parent) in [(inner, sub), (sub, ROOT), (ROOT, ROOT)] {
        let climb = Request::Parent { dir };
        let answered = answer_of(&mut server, &climb);
        assert_eq!(result(&answered, &climb), Ok(Answer::Node(parent)));
    }
    let file = create(&mut server, b"f", NodeKind::RegularFile);
    let climb = Request::Parent { dir: file };
    let answered = answer_of(&mut server, &climb);
    assert_eq!(result(&answered, &climb), Err(DriverError::Unsupported));
}

#[test]
fn a_hold_keeps_a_removed_node_until_its_last_release_frees_it() {
    let host = ServedHost::new(Fixed);
    let mut server = Server::new(TestFs::new(), &host).unwrap();
    let file = create(&mut server, b"held", NodeKind::RegularFile);
    let pin = Request::Pin { node: file };
    for _ in 0..2 {
        let answered = answer_of(&mut server, &pin);
        assert_eq!(result(&answered, &pin), Ok(Answer::Node(file)));
    }
    let remove = Request::Remove {
        dir: ROOT,
        name: b"held",
    };
    let answered = answer_of(&mut server, &remove);
    assert_eq!(result(&answered, &remove), Ok(Answer::Done));
    let info = Request::NodeInfo { node: file };
    let answered = answer_of(&mut server, &info);
    let Ok(Answer::Info(held)) = result(&answered, &info) else {
        panic!("a held node is still described");
    };
    assert_eq!(held.nlink, 0, "and no name reaches it");
    let unpin = Request::Unpin { pinned: file };
    let answered = answer_of(&mut server, &unpin);
    assert_eq!(
        result(&answered, &unpin),
        Ok(Answer::Unpinned(Unpinned::Held))
    );
    let answered = answer_of(&mut server, &unpin);
    assert_eq!(
        result(&answered, &unpin),
        Ok(Answer::Unpinned(Unpinned::Freed))
    );
    let answered = answer_of(&mut server, &info);
    assert_eq!(result(&answered, &info), Err(DriverError::NotFound));
    let answered = answer_of(&mut server, &unpin);
    assert_eq!(
        result(&answered, &unpin),
        Err(DriverError::NotFound),
        "no hold is left to release"
    );
}

#[test]
fn a_named_node_is_released_and_every_hold_goes_at_once() {
    let host = ServedHost::new(Fixed);
    let mut server = Server::new(TestFs::new(), &host).unwrap();
    let kept = create(&mut server, b"kept", NodeKind::RegularFile);
    let doomed = create(&mut server, b"doomed", NodeKind::RegularFile);
    for node in [kept, doomed] {
        let pin = Request::Pin { node };
        let answered = answer_of(&mut server, &pin);
        assert_eq!(result(&answered, &pin), Ok(Answer::Node(node)));
    }
    let unpin = Request::Unpin { pinned: kept };
    let answered = answer_of(&mut server, &unpin);
    assert_eq!(
        result(&answered, &unpin),
        Ok(Answer::Unpinned(Unpinned::Released))
    );
    let pin = Request::Pin { node: kept };
    answer_of(&mut server, &pin);
    let remove = Request::Remove {
        dir: ROOT,
        name: b"doomed",
    };
    answer_of(&mut server, &remove);
    let release = Request::ReleasePins;
    let answered = answer_of(&mut server, &release);
    assert_eq!(result(&answered, &release), Ok(Answer::Done));
    let info = Request::NodeInfo { node: doomed };
    let answered = answer_of(&mut server, &info);
    assert_eq!(
        result(&answered, &info),
        Err(DriverError::NotFound),
        "the nameless node went with its holds"
    );
    let info = Request::NodeInfo { node: kept };
    let answered = answer_of(&mut server, &info);
    assert!(
        matches!(result(&answered, &info), Ok(Answer::Info(_))),
        "a named one stays"
    );
    let unpin = Request::Unpin { pinned: kept };
    let answered = answer_of(&mut server, &unpin);
    assert_eq!(result(&answered, &unpin), Err(DriverError::NotFound));
}
