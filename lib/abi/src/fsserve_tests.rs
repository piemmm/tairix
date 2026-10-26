extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;

use super::{
    decode_reply, decode_request, encode_reply, encode_request, is_served_endpoint, request_op,
    served_endpoint, Answer, DirEnd, DirReply, Op, Reply, Request, ServeFrame, ServeGrant,
    ServeOffer, ServerState, Span, BATCH_TRAILER_LEN, ENTRY_FIXED_LEN, FSSERVE_REPLY_MAX,
    FSSERVE_REQUEST_MAX, FSSERVE_VERSION, FSSERVE_WINDOW_LEN, NODE_INFO_LEN, REPLY_HEAD_LEN,
    SERVE_FRAME_LEN, SERVE_GRANT_LEN,
};
use crate::driver::filesystem::{
    AclModel, AllocationClass, DirEntry, NameMatching, NodeId, NodeIdentity, NodeInfo, NodeKind,
    NodeSecurity, NodeTimes, SecurityAcl, SecuritySubject, Unpinned, VolumeStats, WriteRefusal,
    MAX_ACL_ENTRIES,
};
use crate::fs::{SeekRegion, FS_ATTR_KEY_MAX, FS_ATTR_VALUE_MAX, FS_NAME_MAX, FS_SYMLINK_MAX};
use crate::time::Time64;
use crate::volume::FormatName;
use crate::{CapabilityId, DriverError, Errno};

const DIR: NodeId = NodeId::from_raw(2);
const FILE: NodeId = NodeId::from_raw(17);

/// `len` as the `u32` a request carries it in.
fn cap(len: usize) -> u32 {
    u32::try_from(len).expect("a bound fits a u32")
}

fn span(offset: usize, len: usize) -> Span {
    Span::new(offset, len).expect("a span inside the window")
}

fn security() -> NodeSecurity {
    let mut record = NodeSecurity::new(0o750, 1000, 100);
    record.required_cap = Some(CapabilityId::AUDIT_READ);
    record.acl_model = AclModel::Posix {
        group_obj: 0o5,
        mask: 0o7,
    };
    record
        .push_acl(SecurityAcl {
            subject: SecuritySubject::User(1001),
            perms: 0o6,
        })
        .unwrap();
    record
        .push_acl(SecurityAcl {
            subject: SecuritySubject::Group(42),
            perms: 0o4,
        })
        .unwrap();
    record
}

/// A record holding every ACL entry it can.
fn full_security() -> NodeSecurity {
    let mut record = security();
    for gid in 0..cap(MAX_ACL_ENTRIES - record.acl().len()) {
        record
            .push_acl(SecurityAcl {
                subject: SecuritySubject::Group(u32::MAX - gid),
                perms: 0o7,
            })
            .unwrap();
    }
    record
}

fn info(kind: NodeKind, size: u64) -> NodeInfo {
    NodeInfo {
        kind,
        nlink: 1,
        ino: u64::MAX - size,
        size,
        allocated: size.next_multiple_of(4096),
        times: NodeTimes {
            created: Time64::new(-86_400, 1).unwrap(),
            modified: Time64::new(2_200_000_000, 999_999_999).unwrap(),
            accessed: Time64::UNIX_EPOCH,
            changed: Time64::new(1_700_000_000, 5).unwrap(),
        },
        content_gen: 9,
    }
}

fn stats() -> VolumeStats {
    VolumeStats {
        block_size: 4096,
        in_reserve: false,
        total_blocks: 1000,
        free_blocks: 600,
        avail_blocks: 550,
        files: 64,
        files_free: 60,
    }
}

/// One request of every read operation.
fn read_requests() -> Vec<Request<'static>> {
    vec![
        Request::NodeInfo { node: FILE },
        Request::Lookup {
            dir: DIR,
            name: b"notes.txt",
        },
        Request::ReadAt {
            file: FILE,
            offset: u64::MAX - 7,
            span: span(4096, FSSERVE_WINDOW_LEN - 4096),
        },
        Request::ReadLink {
            link: FILE,
            cap: cap(FS_SYMLINK_MAX),
        },
        Request::SeekRegion {
            file: FILE,
            offset: 12,
            region: SeekRegion::Hole,
        },
        Request::ReadDir {
            dir: DIR,
            cursor: 0,
            after: b"",
        },
        Request::ReadDir {
            dir: DIR,
            cursor: 77,
            after: b"previous",
        },
    ]
}

/// One request of every operation on a hold, and of the climb.
fn hold_requests() -> Vec<Request<'static>> {
    vec![
        Request::Parent { dir: DIR },
        Request::Pin { node: FILE },
        Request::Unpin { pinned: FILE },
        Request::ReleasePins,
    ]
}

/// One request of every write operation.
fn write_requests() -> Vec<Request<'static>> {
    vec![
        Request::Create {
            dir: DIR,
            name: b"sub",
            kind: NodeKind::Directory,
            record: security(),
        },
        Request::CreateLink {
            dir: DIR,
            name: b"link",
            target: b"../a/./b",
            record: NodeSecurity::new(0o777, 1000, 100),
        },
        Request::Link {
            dir: DIR,
            name: b"second",
            node: FILE,
        },
        Request::WriteAt {
            file: FILE,
            offset: 3,
            span: span(0, 1),
        },
        Request::Truncate {
            file: FILE,
            size: 1 << 40,
        },
        Request::Punch {
            file: FILE,
            offset: 4096,
            len: 8192,
        },
        Request::Remove {
            dir: DIR,
            name: b"old",
        },
        Request::Rename {
            src_dir: DIR,
            src_name: b"a",
            dst_dir: FILE,
            dst_name: b"b",
        },
    ]
}

/// One request of every other operation.
fn other_requests() -> Vec<Request<'static>> {
    vec![
        Request::Flush,
        Request::Security { node: FILE },
        Request::SetSecurity {
            node: FILE,
            security: security(),
        },
        Request::RecordsOwnership { node: FILE },
        Request::Stats,
        Request::GetAttr {
            node: FILE,
            key: b"user.colour",
            cap: 0,
        },
        Request::SetAttr {
            node: FILE,
            key: b"user.colour",
            value: b"",
        },
        Request::ListAttr {
            node: FILE,
            index: 3,
            cap: cap(FS_ATTR_KEY_MAX),
        },
        Request::RemoveAttr {
            node: FILE,
            key: b"user.colour",
        },
        Request::Release,
    ]
}

/// One request of every operation, each argument away from its default.
fn every_request() -> Vec<Request<'static>> {
    let mut all = read_requests();
    all.extend(hold_requests());
    all.extend(write_requests());
    all.extend(other_requests());
    all
}

fn encoded(request: &Request<'_>, class: AllocationClass) -> Vec<u8> {
    let mut out = vec![0u8; FSSERVE_REQUEST_MAX];
    let len = encode_request(request, class, &mut out).expect("encodes");
    out.truncate(len);
    out
}

#[test]
fn every_request_round_trips_under_either_class() {
    for request in every_request() {
        for class in [AllocationClass::Ordinary, AllocationClass::System] {
            let bytes = encoded(&request, class);
            assert_eq!(decode_request(&bytes), Ok((class, request)), "{request:?}");
        }
    }
}

#[test]
fn the_largest_request_fits_the_bound_exactly() {
    let name = [b'n'; FS_NAME_MAX];
    let target = [b't'; FS_SYMLINK_MAX];
    let request = Request::CreateLink {
        dir: DIR,
        name: &name,
        target: &target,
        record: full_security(),
    };
    assert_eq!(
        encoded(&request, AllocationClass::Ordinary).len(),
        FSSERVE_REQUEST_MAX
    );
}

#[test]
fn served_endpoints_lie_in_their_own_range() {
    assert!(is_served_endpoint(served_endpoint(0)));
    assert!(is_served_endpoint(served_endpoint(u32::MAX)));
    assert!(!is_served_endpoint(served_endpoint(0) - 1));
    assert!(!is_served_endpoint(served_endpoint(u32::MAX) + 1));
    assert!(!crate::blkio::is_kernel_block_device(served_endpoint(0)));
}

#[test]
fn the_operation_of_a_frame_that_fails_to_decode_is_still_known() {
    let mut frame = encoded(&Request::Stats, AllocationClass::Ordinary);
    frame.push(0);
    assert_eq!(decode_request(&frame), Err(Errno::LengthOutOfRange));
    assert_eq!(request_op(&frame), Some(Op::Stats));
    assert_eq!(request_op(&frame[..2]), None);
    frame[0] = 9;
    assert_eq!(request_op(&frame), None);
}

#[test]
fn a_request_frame_out_of_shape_is_refused() {
    let good = encoded(&Request::NodeInfo { node: FILE }, AllocationClass::Ordinary);
    let mut version = good.clone();
    version[0] = 2;
    assert_eq!(decode_request(&version), Err(Errno::AbiVersionUnsupported));
    let mut op = good.clone();
    op[2] = 0;
    assert_eq!(decode_request(&op), Err(Errno::OutOfRange));
    op[2] = Op::Release as u8 + 1;
    assert_eq!(decode_request(&op), Err(Errno::OutOfRange));
    let mut flags = good.clone();
    flags[3] = 2;
    assert_eq!(decode_request(&flags), Err(Errno::OutOfRange));
    let mut trailing = good.clone();
    trailing.push(0);
    assert_eq!(decode_request(&trailing), Err(Errno::LengthOutOfRange));
    assert_eq!(
        decode_request(&good[..good.len() - 1]),
        Err(Errno::LengthOutOfRange)
    );
    assert_eq!(
        decode_request(&vec![0u8; FSSERVE_REQUEST_MAX + 1]),
        Err(Errno::LengthOutOfRange)
    );
    let mut none = good;
    none[4..12].fill(0);
    assert_eq!(decode_request(&none), Err(Errno::OutOfRange));
}

#[test]
fn a_name_that_is_not_a_component_is_never_sent_nor_accepted() {
    let long = [b'n'; FS_NAME_MAX + 1];
    for name in [&b""[..], b".", b"..", b"a/b", b"a\0b", &long] {
        let request = Request::Lookup { dir: DIR, name };
        let mut out = vec![0u8; FSSERVE_REQUEST_MAX];
        assert!(
            encode_request(&request, AllocationClass::Ordinary, &mut out).is_err(),
            "{name:?}"
        );
        // Hand-built, so the decoder is tested on its own.
        let mut frame = vec![0u8; 0];
        frame.extend_from_slice(&FSSERVE_VERSION.to_le_bytes());
        frame.extend_from_slice(&[Op::Lookup as u8, 0]);
        frame.extend_from_slice(&DIR.raw().to_le_bytes());
        frame.extend_from_slice(&u16::try_from(name.len()).unwrap().to_le_bytes());
        frame.extend_from_slice(name);
        assert!(decode_request(&frame).is_err(), "{name:?}");
    }
}

#[test]
fn every_argument_bound_holds_on_both_sides() {
    let too_long_target = [b't'; FS_SYMLINK_MAX + 1];
    let too_long_key = [b'k'; FS_ATTR_KEY_MAX + 1];
    let too_long_value = [b'v'; FS_ATTR_VALUE_MAX + 1];
    let out_of_range = NodeSecurity::new(0o10000, 0, 0);
    let refused = [
        Request::Create {
            dir: DIR,
            name: b"x",
            kind: NodeKind::Symlink,
            record: security(),
        },
        Request::Create {
            dir: DIR,
            name: b"x",
            kind: NodeKind::RegularFile,
            record: out_of_range,
        },
        Request::CreateLink {
            dir: DIR,
            name: b"x",
            target: b"",
            record: security(),
        },
        Request::CreateLink {
            dir: DIR,
            name: b"x",
            target: b"a\0b",
            record: security(),
        },
        Request::CreateLink {
            dir: DIR,
            name: b"x",
            target: &too_long_target,
            record: security(),
        },
        Request::CreateLink {
            dir: DIR,
            name: b"x",
            target: b"t",
            record: out_of_range,
        },
        Request::ReadLink {
            link: FILE,
            cap: cap(FS_SYMLINK_MAX) + 1,
        },
        Request::GetAttr {
            node: FILE,
            key: b"",
            cap: 4,
        },
        Request::GetAttr {
            node: FILE,
            key: &too_long_key,
            cap: 4,
        },
        Request::GetAttr {
            node: FILE,
            key: b"k",
            cap: cap(FS_ATTR_VALUE_MAX) + 1,
        },
        Request::SetAttr {
            node: FILE,
            key: b"k",
            value: &too_long_value,
        },
        Request::ListAttr {
            node: FILE,
            index: 0,
            cap: cap(FS_ATTR_KEY_MAX) + 1,
        },
        Request::ReadDir {
            dir: DIR,
            cursor: 1,
            after: b"..",
        },
        Request::Link {
            dir: DIR,
            name: b"x",
            node: NodeId::NONE,
        },
        Request::Pin { node: NodeId::NONE },
        Request::Unpin {
            pinned: NodeId::NONE,
        },
        Request::Parent { dir: NodeId::NONE },
    ];
    for request in refused {
        let mut out = vec![0u8; FSSERVE_REQUEST_MAX + 64];
        assert!(
            encode_request(&request, AllocationClass::Ordinary, &mut out).is_err(),
            "{request:?}"
        );
    }
}

#[test]
fn a_span_lies_inside_the_window_and_is_never_empty() {
    assert!(Span::new(0, FSSERVE_WINDOW_LEN).is_ok());
    assert_eq!(Span::new(0, 0), Err(Errno::LengthOutOfRange));
    assert_eq!(
        Span::new(1, FSSERVE_WINDOW_LEN),
        Err(Errno::LengthOutOfRange)
    );
    assert_eq!(Span::new(usize::MAX, 2), Err(Errno::LengthOutOfRange));
    let at = span(10, 20);
    assert_eq!(at.range(), 10..30);
    // A span outside the window, spelt by hand, is refused at decode.
    let mut frame = encoded(
        &Request::ReadAt {
            file: FILE,
            offset: 0,
            span: span(0, 8),
        },
        AllocationClass::Ordinary,
    );
    let at = frame.len() - 8;
    frame[at..at + 4].copy_from_slice(&cap(FSSERVE_WINDOW_LEN).to_le_bytes());
    assert_eq!(decode_request(&frame), Err(Errno::OutOfRange));
}

fn reply_bytes(op: Op, state: ServerState, result: Result<Answer<'_>, DriverError>) -> Vec<u8> {
    let mut out = vec![0u8; FSSERVE_REPLY_MAX];
    let len = encode_reply(op, state, result, &mut out).expect("encodes");
    out.truncate(len);
    out
}

fn answered<'a>(bytes: &'a [u8], request: &Request<'_>) -> Result<Answer<'a>, DriverError> {
    decode_reply(bytes, request).expect("decodes").result
}

/// A request and its answer, the cases every answer is tested through.
type Case = (Request<'static>, Answer<'static>);

/// The read operations' answers.
fn read_answers() -> Vec<Case> {
    vec![
        (
            Request::NodeInfo { node: FILE },
            Answer::Info(info(NodeKind::RegularFile, 5000)),
        ),
        (
            Request::Lookup {
                dir: DIR,
                name: b"x",
            },
            Answer::Node(FILE),
        ),
        (
            Request::ReadAt {
                file: FILE,
                offset: 0,
                span: span(0, 512),
            },
            Answer::Count(512),
        ),
        (
            Request::ReadLink {
                link: FILE,
                cap: 64,
            },
            Answer::Target(b"../somewhere"),
        ),
        (
            Request::SeekRegion {
                file: FILE,
                offset: 100,
                region: SeekRegion::Data,
            },
            Answer::Region(Some(4096)),
        ),
        (
            Request::SeekRegion {
                file: FILE,
                offset: 100,
                region: SeekRegion::Data,
            },
            Answer::Region(None),
        ),
        (
            Request::Create {
                dir: DIR,
                name: b"n",
                kind: NodeKind::RegularFile,
                record: security(),
            },
            Answer::Node(FILE),
        ),
        (
            Request::WriteAt {
                file: FILE,
                offset: 0,
                span: span(0, 512),
            },
            Answer::Count(0),
        ),
    ]
}

/// The answers of the climb and of the operations on a hold.
fn hold_answers() -> Vec<Case> {
    let unpin = Request::Unpin { pinned: FILE };
    vec![
        (Request::Parent { dir: FILE }, Answer::Node(DIR)),
        (Request::Pin { node: FILE }, Answer::Node(FILE)),
        (unpin, Answer::Unpinned(Unpinned::Held)),
        (unpin, Answer::Unpinned(Unpinned::Released)),
        (unpin, Answer::Unpinned(Unpinned::Freed)),
        (Request::ReleasePins, Answer::Done),
    ]
}

/// Every other operation's answers.
fn other_answers() -> Vec<Case> {
    vec![
        (Request::Flush, Answer::Done),
        (
            Request::Security { node: FILE },
            Answer::Security(security()),
        ),
        (
            Request::RecordsOwnership { node: FILE },
            Answer::Owned(true),
        ),
        (Request::Stats, Answer::Stats(stats())),
        (
            Request::GetAttr {
                node: FILE,
                key: b"user.k",
                cap: 8,
            },
            Answer::Attr(Some(b"value")),
        ),
        (
            Request::GetAttr {
                node: FILE,
                key: b"user.k",
                cap: 0,
            },
            Answer::Attr(Some(b"")),
        ),
        (
            Request::ListAttr {
                node: FILE,
                index: 9,
                cap: 16,
            },
            Answer::Attr(None),
        ),
        (Request::Release, Answer::Done),
    ]
}

#[test]
fn every_answer_round_trips_against_its_request() {
    let mut cases = read_answers();
    cases.extend(hold_answers());
    cases.extend(other_answers());
    for (request, answer) in cases {
        let state = ServerState {
            unpublished: true,
            in_reserve: false,
        };
        let bytes = reply_bytes(request.op(), state, Ok(answer));
        assert_eq!(
            decode_reply(&bytes, &request),
            Ok(Reply {
                state,
                result: Ok(answer)
            }),
            "{request:?}"
        );
    }
}

#[test]
fn a_failure_carries_its_error_and_nothing_else() {
    let request = Request::Lookup {
        dir: DIR,
        name: b"missing",
    };
    let bytes = reply_bytes(
        Op::Lookup,
        ServerState::default(),
        Err(DriverError::NotFound),
    );
    assert_eq!(answered(&bytes, &request), Err(DriverError::NotFound));
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert_eq!(
        decode_reply(&trailing, &request),
        Err(Errno::LengthOutOfRange)
    );
    let mut unknown = bytes;
    unknown[4..8].copy_from_slice(&999i32.to_le_bytes());
    assert_eq!(decode_reply(&unknown, &request), Err(Errno::OutOfRange));
}

#[test]
fn an_answer_of_the_wrong_kind_is_never_encoded() {
    let mut out = vec![0u8; 64];
    assert_eq!(
        encode_reply(
            Op::Lookup,
            ServerState::default(),
            Ok(Answer::Done),
            &mut out
        ),
        Err(Errno::OutOfRange)
    );
    assert_eq!(
        encode_reply(
            Op::Flush,
            ServerState::default(),
            Ok(Answer::Count(1)),
            &mut out
        ),
        Err(Errno::OutOfRange)
    );
}

#[test]
fn a_reply_is_held_to_the_request_it_answers() {
    let read = Request::ReadAt {
        file: FILE,
        offset: 0,
        span: span(0, 16),
    };
    // More bytes than the span held.
    let bytes = reply_bytes(Op::ReadAt, ServerState::default(), Ok(Answer::Count(17)));
    assert_eq!(decode_reply(&bytes, &read), Err(Errno::OutOfRange));
    // An answer to another operation.
    let bytes = reply_bytes(Op::WriteAt, ServerState::default(), Ok(Answer::Count(1)));
    assert_eq!(decode_reply(&bytes, &read), Err(Errno::OutOfRange));
    // A target longer than the caller's buffer.
    let link = Request::ReadLink { link: FILE, cap: 3 };
    let bytes = reply_bytes(
        Op::ReadLink,
        ServerState::default(),
        Ok(Answer::Target(b"abcd")),
    );
    assert_eq!(decode_reply(&bytes, &link), Err(Errno::OutOfRange));
    // A region found before the search began.
    let seek = Request::SeekRegion {
        file: FILE,
        offset: 100,
        region: SeekRegion::Hole,
    };
    let bytes = reply_bytes(
        Op::SeekRegion,
        ServerState::default(),
        Ok(Answer::Region(Some(99))),
    );
    assert_eq!(decode_reply(&bytes, &seek), Err(Errno::OutOfRange));
    // A value longer than the caller's buffer.
    let get = Request::GetAttr {
        node: FILE,
        key: b"user.k",
        cap: 2,
    };
    let bytes = reply_bytes(
        Op::GetAttr,
        ServerState::default(),
        Ok(Answer::Attr(Some(b"abc"))),
    );
    assert_eq!(decode_reply(&bytes, &get), Err(Errno::OutOfRange));
}

/// Encode `request`'s successful reply carrying `answer`, let `corrupt` edit
/// it, and say why the decoder refuses the result, if it does.
fn corrupted(
    request: &Request<'_>,
    answer: Answer<'_>,
    corrupt: impl FnOnce(&mut Vec<u8>),
) -> Result<(), Errno> {
    let mut bytes = reply_bytes(request.op(), ServerState::default(), Ok(answer));
    corrupt(&mut bytes);
    decode_reply(&bytes, request).map(|_| ())
}

#[test]
fn a_malformed_value_in_an_answer_is_refused() {
    let lookup = Request::Lookup {
        dir: DIR,
        name: b"x",
    };
    assert_eq!(
        corrupted(&lookup, Answer::Node(FILE), |b| b[8..16].fill(0)),
        Err(Errno::OutOfRange),
        "no live node is NONE"
    );
    let node = Request::NodeInfo { node: FILE };
    let file = Answer::Info(info(NodeKind::RegularFile, 1));
    assert_eq!(
        corrupted(&node, file, |b| b[8] = 3),
        Err(Errno::OutOfRange),
        "a kind the trait does not name"
    );
    assert_eq!(
        corrupted(&node, file, |b| {
            // The first timestamp's nanoseconds, past kind, links, number,
            // size, allocation and seconds.
            let at = 8 + 1 + 4 + 8 + 8 + 8 + 8;
            b[at..at + 4].copy_from_slice(&1_000_000_000u32.to_le_bytes());
        }),
        Err(Errno::OutOfRange),
        "a time past its second"
    );
    let stat = Request::Stats;
    for (at, value) in [(8, 3u64), (13, 1), (21, 1001), (29, 601)] {
        let refused = corrupted(&stat, Answer::Stats(stats()), |b| {
            if at == 8 {
                b[8..12].copy_from_slice(&u32::try_from(value).unwrap().to_le_bytes());
            } else {
                b[at..at + 8].copy_from_slice(&value.to_le_bytes());
            }
        });
        assert_eq!(refused, Err(Errno::OutOfRange), "stats field at {at}");
    }
    let owned = Request::RecordsOwnership { node: FILE };
    assert_eq!(
        corrupted(&owned, Answer::Owned(false), |b| b[8] = 2),
        Err(Errno::OutOfRange)
    );
    let unpin = Request::Unpin { pinned: FILE };
    assert_eq!(
        corrupted(&unpin, Answer::Unpinned(Unpinned::Freed), |b| b[8] = 3),
        Err(Errno::OutOfRange),
        "a hold's end the trait does not name"
    );
    let pin = Request::Pin { node: FILE };
    assert_eq!(
        corrupted(&pin, Answer::Node(FILE), |b| b[8..16].fill(0)),
        Err(Errno::OutOfRange),
        "no hold is named NONE"
    );
    let seek = Request::SeekRegion {
        file: FILE,
        offset: 0,
        region: SeekRegion::Data,
    };
    assert_eq!(
        corrupted(&seek, Answer::Region(None), |b| b[9] = 1),
        Err(Errno::OutOfRange),
        "an absent region carrying an offset"
    );
    let state = Request::Flush;
    assert_eq!(
        corrupted(&state, Answer::Done, |b| b[3] = 4),
        Err(Errno::OutOfRange),
        "a reserved state bit"
    );
}

/// What a corruption is called, and how it edits a frame.
type Corruption = (&'static str, fn(&mut Vec<u8>));

#[test]
fn a_security_record_out_of_bounds_is_refused() {
    let request = Request::Security { node: FILE };
    let good = Answer::Security(security());
    // Body offsets: mode 8, uid 12, gid 16, cap flag 20, cap 21, model 23,
    // group_obj 24, mask 25, entries 26, then (tag, id, perms) per entry.
    let cases: [Corruption; 7] = [
        ("mode", |b| {
            b[8..12].copy_from_slice(&0o10000u32.to_le_bytes());
        }),
        ("cap flag", |b| b[20] = 2),
        ("absent cap with a value", |b| b[20] = 0),
        ("model", |b| b[23] = 2),
        ("triad", |b| b[25] = 8),
        ("subject", |b| b[27] = 2),
        ("perms", |b| b[32] = 8),
    ];
    for (what, corrupt) in cases {
        assert_eq!(
            corrupted(&request, good, corrupt),
            Err(Errno::OutOfRange),
            "{what}"
        );
    }
    let mut full = NodeSecurity::new(0o644, 0, 0);
    for uid in 0..cap(MAX_ACL_ENTRIES) {
        full.push_acl(SecurityAcl {
            subject: SecuritySubject::User(uid),
            perms: 7,
        })
        .unwrap();
    }
    assert_eq!(
        corrupted(&request, Answer::Security(full), |b| b[26] = 9),
        Err(Errno::OutOfRange),
        "more entries than a record holds"
    );
}

fn entry(node: u64, cursor: u64) -> DirEntry {
    DirEntry {
        node: NodeId::from_raw(node),
        info: info(NodeKind::RegularFile, node),
        next_cursor: cursor,
    }
}

fn listing(entries: &[(DirEntry, &[u8])], end: DirEnd) -> Vec<u8> {
    let mut out = vec![0u8; FSSERVE_REPLY_MAX];
    let len = {
        let mut reply = DirReply::new(&mut out).unwrap();
        for (e, name) in entries {
            reply.push(e, name).unwrap();
        }
        reply
            .finish(
                end,
                ServerState {
                    unpublished: false,
                    in_reserve: true,
                },
            )
            .unwrap()
    };
    out.truncate(len);
    out
}

const LIST: Request<'static> = Request::ReadDir {
    dir: DIR,
    cursor: 0,
    after: b"",
};

#[test]
fn a_listing_batch_replays_its_entries_and_how_it_ended() {
    let entries = [(entry(10, 1), &b"alpha"[..]), (entry(11, 2), b"beta")];
    for end in [
        DirEnd::More,
        DirEnd::Ended,
        DirEnd::Failed(DriverError::DeviceFault),
    ] {
        let bytes = listing(&entries, end);
        let reply = decode_reply(&bytes, &LIST).unwrap();
        assert!(reply.state.in_reserve);
        let Ok(Answer::Entries(batch)) = reply.result else {
            panic!("a batch");
        };
        assert_eq!(batch.end(), end);
        assert_eq!(batch.len(), 2);
        let seen: Vec<_> = batch.iter().collect();
        assert_eq!(seen, entries.to_vec());
    }
}

#[test]
fn an_empty_batch_that_claims_more_is_refused() {
    let mut out = vec![0u8; FSSERVE_REPLY_MAX];
    let reply = DirReply::new(&mut out).unwrap();
    assert_eq!(
        reply.finish(DirEnd::More, ServerState::default()),
        Err(Errno::OutOfRange)
    );
    // Spelt by hand: the decoder refuses it on its own.
    let mut bytes = listing(&[], DirEnd::Ended);
    let tag = bytes.len() - 5;
    bytes[tag] = 0;
    assert_eq!(decode_reply(&bytes, &LIST), Err(Errno::OutOfRange));
}

/// The lengths the codec reserves are the ones it writes, measured off the
/// encoder, so no reservation can drift from the record it sizes in either
/// direction.
#[test]
fn the_reserved_lengths_are_what_the_encoder_writes() {
    let node = reply_bytes(
        Op::NodeInfo,
        ServerState::default(),
        Ok(Answer::Info(info(NodeKind::RegularFile, 1))),
    );
    assert_eq!(node.len(), REPLY_HEAD_LEN + NODE_INFO_LEN);
    let listed = listing(&[(entry(1, 1), b"abcd")], DirEnd::Ended);
    assert_eq!(
        listed.len(),
        REPLY_HEAD_LEN + 2 + ENTRY_FIXED_LEN + 4 + BATCH_TRAILER_LEN
    );
}

#[test]
fn a_batch_stops_before_an_entry_that_would_crowd_out_its_trailer() {
    // The head, the count, the trailer, and two entries named in four bytes.
    let mut out = vec![0u8; REPLY_HEAD_LEN + 2 + BATCH_TRAILER_LEN + 2 * (ENTRY_FIXED_LEN + 4)];
    let mut reply = DirReply::new(&mut out).unwrap();
    reply.push(&entry(1, 1), b"aaaa").unwrap();
    reply.push(&entry(2, 2), b"bbbb").unwrap();
    assert_eq!(reply.push(&entry(3, 3), b"c"), Err(Errno::BufferTooSmall));
    assert_eq!(reply.len(), 2);
    let len = reply.finish(DirEnd::More, ServerState::default()).unwrap();
    let Ok(Answer::Entries(batch)) = decode_reply(&out[..len], &LIST).unwrap().result else {
        panic!("a batch");
    };
    assert_eq!(batch.len(), 2);
}

#[test]
fn a_batch_entry_with_a_bad_name_or_node_is_refused() {
    let mut out = vec![0u8; FSSERVE_REPLY_MAX];
    let mut reply = DirReply::new(&mut out).unwrap();
    assert_eq!(reply.push(&entry(4, 4), b".."), Err(Errno::OutOfRange));
    assert_eq!(reply.push(&entry(0, 4), b"x"), Err(Errno::OutOfRange));
    assert!(reply.is_empty());
    // A name corrupted to contain a separator after encoding.
    let mut bytes = listing(&[(entry(5, 5), b"abc")], DirEnd::Ended);
    let name_at = bytes.len() - 5 - 3;
    bytes[name_at + 1] = b'/';
    assert_eq!(decode_reply(&bytes, &LIST), Err(Errno::OutOfRange));
    // A count claiming an entry the frame does not hold.
    let mut bytes = listing(&[(entry(5, 5), b"abc")], DirEnd::Ended);
    bytes[8..10].copy_from_slice(&2u16.to_le_bytes());
    assert!(decode_reply(&bytes, &LIST).is_err());
}

#[test]
fn a_listing_fills_its_whole_reply_and_still_decodes() {
    let name = [b'x'; FS_NAME_MAX];
    let mut out = vec![0u8; FSSERVE_REPLY_MAX];
    let mut reply = DirReply::new(&mut out).unwrap();
    let mut pushed = 0u64;
    while reply.push(&entry(pushed + 1, pushed + 1), &name).is_ok() {
        pushed += 1;
    }
    assert!(pushed > 100, "a few hundred entries fit one batch");
    let len = reply.finish(DirEnd::More, ServerState::default()).unwrap();
    assert!(len <= FSSERVE_REPLY_MAX);
    let Ok(Answer::Entries(batch)) = decode_reply(&out[..len], &LIST).unwrap().result else {
        panic!("a batch");
    };
    assert_eq!(u64::try_from(batch.len()).unwrap(), pushed);
}

#[test]
fn a_serve_frame_round_trips_and_refuses_what_it_cannot_name() {
    let offers = [
        ServeFrame::Serve(ServeOffer {
            root: NodeId::from_raw(2),
            name_matching: NameMatching::AsciiCaseInsensitive,
            node_identity: NodeIdentity::ContentDerived,
            attrs: false,
            write_refusal: None,
            format: FormatName::Fat32,
        }),
        ServeFrame::Serve(ServeOffer {
            root: NodeId::from_raw(u64::MAX),
            name_matching: NameMatching::Exact,
            node_identity: NodeIdentity::Stable,
            attrs: true,
            write_refusal: Some(WriteRefusal::FormatReadOnly),
            format: FormatName::Iso9660,
        }),
        ServeFrame::Refuse(DriverError::BadMagic),
    ];
    for frame in offers {
        assert_eq!(ServeFrame::decode(&frame.encode()), Ok(frame));
    }
    let good = offers[0].encode();
    assert_eq!(
        ServeFrame::decode(&good[..SERVE_FRAME_LEN - 1]),
        Err(Errno::LengthOutOfRange)
    );
    for (at, value) in [
        (0, 9),
        (2, 3),
        (3, 2),
        (4, 2),
        (5, 2),
        (6, 19),
        (7, 0),
        (7, 9),
    ] {
        let mut bad = good;
        bad[at] = value;
        assert!(ServeFrame::decode(&bad).is_err(), "byte {at}");
    }
    let mut none = good;
    none[8..].fill(0);
    assert_eq!(ServeFrame::decode(&none), Err(Errno::OutOfRange));
    let mut refusal = offers[2].encode();
    refusal[4..8].copy_from_slice(&0i32.to_le_bytes());
    assert_eq!(
        ServeFrame::decode(&refusal),
        Err(Errno::OutOfRange),
        "a refusal names an error"
    );
}

#[test]
fn a_serve_grant_round_trips_and_names_the_one_window_length() {
    let grant = ServeGrant {
        endpoint: 0xF5_0000_0001,
        window: 0x7000_0000,
        window_len: cap(FSSERVE_WINDOW_LEN),
    };
    let bytes = grant.encode();
    assert_eq!(bytes.len(), SERVE_GRANT_LEN);
    assert_eq!(ServeGrant::decode(&bytes), Ok(grant));
    let mut short = bytes;
    short[16..20].copy_from_slice(&4096u32.to_le_bytes());
    assert_eq!(ServeGrant::decode(&short), Err(Errno::LengthOutOfRange));
    let mut pad = bytes;
    pad[23] = 1;
    assert_eq!(ServeGrant::decode(&pad), Err(Errno::OutOfRange));
}
