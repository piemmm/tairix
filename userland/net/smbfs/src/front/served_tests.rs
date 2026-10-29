//! Unit tests for a mounted share's served volume, against the scripted
//! server's disk share.

use alloc::string::String;
use alloc::vec::Vec;

use alloc::boxed::Box;

use tairix_abi::driver::filesystem::{NodeId, NodeKind, NodeSecurity, Unpinned};
use tairix_abi::fs::SeekRegion;
use tairix_abi::fsserve::{decode_reply, Answer, DirEnd, Request, Span};
use tairix_abi::netattach::OfferId;
use tairix_abi::netshare_ipc::NetshareRequest;
use tairix_abi::DriverError;
use tairix_sandbox::session::FrameOut;
use tairix_smb::negotiate::Dialect;
use tairix_smb::peer::{Fault, PeerConfig, DISK_SHARE};

use super::ALIVE_EVERY_NS;
use crate::events::{CONNECTION_LOST, RECONNECTED};
use crate::front::CALL_DEADLINE_NS;
use crate::nodes::ROOT;
use crate::testing::{alice, connect, nas, Bench, Hand, World, PASSWORD};
use crate::wire::{Report, CHUNK_MAX};

/// A bench with the scripted server's disk share mounted, and its offer.
pub(in crate::front) fn mounted(read_only: bool) -> (Bench, OfferId) {
    mounted_in(nas(), read_only)
}

/// A bench over `world` with its server's disk share mounted, and its offer.
pub(in crate::front) fn mounted_in(world: World, read_only: bool) -> (Bench, OfferId) {
    let mut bench = Bench::new(world);
    let ticket = bench.call(
        alice(),
        &NetshareRequest::Mount {
            connect: connect(PASSWORD),
            share: DISK_SHARE,
            read_only,
        },
    );
    let (_, offer) = bench.mounted(ticket);
    (bench, offer)
}

/// The answer the volume gives `request`, as `f` reads it.
pub(in crate::front) fn ask<T>(
    bench: &mut Bench,
    offer: OfferId,
    request: &Request<'_>,
    f: impl FnOnce(Result<Answer<'_>, DriverError>) -> T,
) -> T {
    let ticket = bench.serve(offer, request);
    let reply = bench.answer(offer, ticket);
    f(decode_reply(&reply, request)
        .expect("a reply the kernel reads")
        .result)
}

/// The answer `request` takes: `Ok` with what it answered, or its error.
pub(in crate::front) fn answered(
    bench: &mut Bench,
    offer: OfferId,
    request: &Request<'_>,
) -> Result<(), DriverError> {
    ask(bench, offer, request, |result| result.map(|_| ()))
}

pub(in crate::front) fn node(bench: &mut Bench, offer: OfferId, request: &Request<'_>) -> NodeId {
    ask(bench, offer, request, |result| match result {
        Ok(Answer::Node(id)) => id,
        other => panic!("not a node: {other:?}"),
    })
}

pub(in crate::front) fn lookup(
    bench: &mut Bench,
    offer: OfferId,
    dir: NodeId,
    name: &[u8],
) -> Result<NodeId, DriverError> {
    ask(
        bench,
        offer,
        &Request::Lookup { dir, name },
        |result| match result {
            Ok(Answer::Node(id)) => Ok(id),
            Ok(other) => panic!("not a node: {other:?}"),
            Err(error) => Err(error),
        },
    )
}

pub(in crate::front) fn create_in(
    bench: &mut Bench,
    offer: OfferId,
    dir: NodeId,
    name: &[u8],
    kind: NodeKind,
) -> NodeId {
    node(
        bench,
        offer,
        &Request::Create {
            dir,
            name,
            kind,
            record: NodeSecurity::unrecorded(kind),
        },
    )
}

pub(in crate::front) fn create(
    bench: &mut Bench,
    offer: OfferId,
    name: &[u8],
    kind: NodeKind,
) -> NodeId {
    create_in(bench, offer, ROOT, name, kind)
}

pub(in crate::front) fn rename(
    bench: &mut Bench,
    offer: OfferId,
    from: (NodeId, &[u8]),
    to: (NodeId, &[u8]),
) -> Result<(), DriverError> {
    let request = Request::Rename {
        src_dir: from.0,
        src_name: from.1,
        dst_dir: to.0,
        dst_name: to.1,
    };
    answered(bench, offer, &request)
}

/// Hand `request` to the volume, its answer still to come.
pub(in crate::front) fn start(bench: &mut Bench, offer: OfferId, request: &Request<'_>) -> u64 {
    let ticket = bench.serve(offer, request);
    assert!(
        waiting(bench, offer, ticket),
        "{request:?} answered at once"
    );
    ticket
}

pub(in crate::front) fn waiting(bench: &mut Bench, offer: OfferId, ticket: u64) -> bool {
    let volume = bench.world().volume(offer).expect("offered");
    !volume
        .answers
        .iter()
        .any(|(answered, _)| *answered == ticket)
}

/// The answer the call `ticket` took, as `f` reads it.
pub(in crate::front) fn took<T>(
    bench: &mut Bench,
    offer: OfferId,
    ticket: u64,
    request: &Request<'_>,
    f: impl FnOnce(Result<Answer<'_>, DriverError>) -> T,
) -> T {
    let reply = bench.answer(offer, ticket);
    f(decode_reply(&reply, request)
        .expect("a reply the kernel reads")
        .result)
}

pub(in crate::front) fn window(bench: &mut Bench, offer: OfferId) -> &mut [u8] {
    &mut bench.world().volume(offer).expect("offered").window
}

pub(in crate::front) fn write(
    bench: &mut Bench,
    offer: OfferId,
    file: NodeId,
    offset: u64,
    data: &[u8],
) -> u32 {
    let at = 8192;
    bench.world().volume(offer).expect("offered").window[at..at + data.len()].copy_from_slice(data);
    let span = Span::new(at, data.len()).expect("in the window");
    ask(
        bench,
        offer,
        &Request::WriteAt { file, offset, span },
        |result| match result {
            Ok(Answer::Count(count)) => count,
            other => panic!("not a count: {other:?}"),
        },
    )
}

pub(in crate::front) fn read(
    bench: &mut Bench,
    offer: OfferId,
    file: NodeId,
    offset: u64,
    len: usize,
) -> Vec<u8> {
    let at = 4096;
    let span = Span::new(at, len).expect("in the window");
    let count = ask(
        bench,
        offer,
        &Request::ReadAt { file, offset, span },
        |result| match result {
            Ok(Answer::Count(count)) => count as usize,
            other => panic!("not a count: {other:?}"),
        },
    );
    bench.world().volume(offer).expect("offered").window[at..at + count].to_vec()
}

pub(in crate::front) fn listed(
    bench: &mut Bench,
    offer: OfferId,
    dir: NodeId,
) -> Vec<(String, NodeId)> {
    let mut names = Vec::new();
    let mut cursor = 0;
    loop {
        let request = Request::ReadDir {
            dir,
            cursor,
            after: b"",
        };
        let end = ask(bench, offer, &request, |result| {
            let Ok(Answer::Entries(batch)) = result else {
                panic!("not a batch: {result:?}");
            };
            for (entry, name) in &batch {
                names.push((String::from_utf8(name.to_vec()).expect("UTF-8"), entry.node));
                cursor = entry.next_cursor;
            }
            batch.end()
        });
        match end {
            DirEnd::More => {}
            DirEnd::Ended => return names,
            DirEnd::Failed(error) => panic!("the listing failed: {error:?}"),
        }
    }
}

#[test]
fn a_file_is_made_written_read_found_renamed_and_removed_over_the_share() {
    let (mut bench, offer) = mounted(false);
    let file = create(&mut bench, offer, b"hello.txt", NodeKind::RegularFile);
    assert_eq!(write(&mut bench, offer, file, 0, b"hello, world"), 12);
    assert_eq!(read(&mut bench, offer, file, 7, 64), b"world");
    let found = node(
        &mut bench,
        offer,
        &Request::Lookup {
            dir: ROOT,
            name: b"hello.txt",
        },
    );
    assert_eq!(found, file, "the id a node keeps while it keeps its name");
    let size = ask(
        &mut bench,
        offer,
        &Request::NodeInfo { node: file },
        |result| match result {
            Ok(Answer::Info(info)) => (info.kind, info.size),
            other => panic!("not info: {other:?}"),
        },
    );
    assert_eq!(size, (NodeKind::RegularFile, 12));
    let dir = create(&mut bench, offer, b"docs", NodeKind::Directory);
    assert!(listed(&mut bench, offer, ROOT)
        .iter()
        .any(|(name, id)| name == "hello.txt" && *id == file));
    assert!(listed(&mut bench, offer, dir).is_empty());

    let renamed = Request::Rename {
        src_dir: ROOT,
        src_name: b"hello.txt",
        dst_dir: dir,
        dst_name: b"moved.txt",
    };
    ask(&mut bench, offer, &renamed, |result| {
        assert_eq!(result, Ok(Answer::Done));
    });
    assert_eq!(
        read(&mut bench, offer, file, 0, 5),
        b"hello",
        "a rename keeps the node"
    );
    let moved = node(
        &mut bench,
        offer,
        &Request::Lookup {
            dir,
            name: b"moved.txt",
        },
    );
    assert_eq!(moved, file);
    let gone = Request::Lookup {
        dir: ROOT,
        name: b"hello.txt",
    };
    ask(&mut bench, offer, &gone, |result| {
        assert_eq!(result, Err(DriverError::NotFound));
    });

    let removed = Request::Remove {
        dir,
        name: b"moved.txt",
    };
    ask(&mut bench, offer, &removed, |result| {
        assert_eq!(result, Ok(Answer::Done));
    });
    ask(
        &mut bench,
        offer,
        &Request::NodeInfo { node: file },
        |result| {
            assert_eq!(result, Err(DriverError::Stale), "a removed node is stale");
        },
    );
    let truncated = Request::Truncate { file, size: 0 };
    ask(&mut bench, offer, &truncated, |result| {
        assert_eq!(result, Err(DriverError::Stale));
    });
}

#[test]
fn a_volume_answers_what_needs_no_server_itself() {
    let (mut bench, offer) = mounted(false);
    ask(
        &mut bench,
        offer,
        &Request::Parent { dir: ROOT },
        |result| {
            assert_eq!(result, Ok(Answer::Node(ROOT)));
        },
    );
    ask(
        &mut bench,
        offer,
        &Request::RecordsOwnership { node: ROOT },
        |result| {
            assert_eq!(result, Ok(Answer::Owned(false)));
        },
    );
    ask(
        &mut bench,
        offer,
        &Request::Security { node: ROOT },
        |result| {
            assert_eq!(
                result,
                Ok(Answer::Security(NodeSecurity::unrecorded(
                    NodeKind::Directory
                )))
            );
        },
    );
    ask(&mut bench, offer, &Request::Flush, |result| {
        assert_eq!(result, Ok(Answer::Done));
    });
    let unknown = NodeId::from_raw(999);
    ask(
        &mut bench,
        offer,
        &Request::NodeInfo { node: unknown },
        |result| {
            assert_eq!(result, Err(DriverError::Stale));
        },
    );
    ask(
        &mut bench,
        offer,
        &Request::ReadLink {
            link: ROOT,
            cap: 64,
        },
        |result| {
            assert_eq!(result, Err(DriverError::Unsupported));
        },
    );
    let stats = ask(&mut bench, offer, &Request::Stats, |result| match result {
        Ok(Answer::Stats(stats)) => stats.block_size,
        other => panic!("not stats: {other:?}"),
    });
    assert!(stats > 0);
    assert_eq!(
        bench.world().volume(offer).expect("offered").alive,
        0,
        "nothing waited long"
    );
}

#[test]
fn a_read_only_mount_refuses_every_change_before_the_server_sees_it() {
    let (mut bench, offer) = mounted(true);
    let span = Span::new(0, 4).expect("in the window");
    for request in [
        Request::Create {
            dir: ROOT,
            name: b"new",
            kind: NodeKind::RegularFile,
            record: NodeSecurity::unrecorded(NodeKind::RegularFile),
        },
        Request::WriteAt {
            file: ROOT,
            offset: 0,
            span,
        },
        Request::Remove {
            dir: ROOT,
            name: b"x",
        },
        Request::Truncate {
            file: ROOT,
            size: 0,
        },
    ] {
        ask(&mut bench, offer, &request, |result| {
            assert_eq!(result, Err(DriverError::ReadOnly), "{request:?}");
        });
    }
}

#[test]
fn a_volume_its_owner_detached_is_withdrawn() {
    let (mut bench, offer) = mounted(false);
    bench.world().volume(offer).expect("offered").gone = true;
    let now = bench.now;
    bench.front.served(now, offer);
    bench.run();
    assert_eq!(bench.world().withdrawn, [offer]);
    let mounts = bench.call(alice(), &NetshareRequest::Mounts);
    let reply = bench.reply(mounts);
    let Ok(tairix_abi::netshare_ipc::NetshareReply::Mounts(left)) =
        tairix_abi::netshare_ipc::NetshareReply::decode(&reply)
    else {
        panic!("not mounts");
    };
    assert!(left.is_empty());
}

#[test]
fn a_read_from_the_end_on_reads_nothing() {
    for fault in [Fault::None, Fault::ChainedFailure] {
        let world = World::serving(PeerConfig {
            fault,
            ..PeerConfig::new(Dialect::Smb311)
        });
        let (mut bench, offer) = mounted_in(world, false);
        let file = create(&mut bench, offer, b"f", NodeKind::RegularFile);
        write(&mut bench, offer, file, 0, b"0123456789");
        for offset in [10, 11, u64::MAX] {
            assert_eq!(
                read(&mut bench, offer, file, offset, 64),
                b"",
                "{offset} {fault:?}"
            );
        }
        assert_eq!(read(&mut bench, offer, file, 9, 64), b"9");
        assert_eq!(bench.world().server_opens(), 0, "{fault:?}");
    }
}

#[test]
fn a_name_in_either_case_is_one_node() {
    let (mut bench, offer) = mounted(false);
    let file = create(&mut bench, offer, b"Notes.txt", NodeKind::RegularFile);
    assert_eq!(lookup(&mut bench, offer, ROOT, b"notes.TXT"), Ok(file));
    assert_eq!(lookup(&mut bench, offer, ROOT, b"NOTES.TXT"), Ok(file));
    let made = Request::Create {
        dir: ROOT,
        name: b"NOTES.txt",
        kind: NodeKind::RegularFile,
        record: NodeSecurity::unrecorded(NodeKind::RegularFile),
    };
    assert_eq!(
        answered(&mut bench, offer, &made),
        Err(DriverError::AlreadyExists)
    );
}

#[test]
fn a_rename_replaces_what_it_lands_on_as_the_driver_contract_asks() {
    let (mut bench, offer) = mounted(false);
    let a = create(&mut bench, offer, b"a", NodeKind::RegularFile);
    write(&mut bench, offer, a, 0, b"new");
    let b = create(&mut bench, offer, b"b", NodeKind::RegularFile);
    write(&mut bench, offer, b, 0, b"old");
    assert_eq!(
        rename(&mut bench, offer, (ROOT, b"a"), (ROOT, b"b")),
        Ok(()),
        "a file over a file"
    );
    assert_eq!(
        lookup(&mut bench, offer, ROOT, b"b"),
        Ok(a),
        "the moved node keeps its id"
    );
    assert_eq!(read(&mut bench, offer, a, 0, 8), b"new");
    assert_eq!(
        answered(&mut bench, offer, &Request::NodeInfo { node: b }),
        Err(DriverError::Stale),
        "the replaced node is gone"
    );
    assert_eq!(
        rename(&mut bench, offer, (ROOT, b"b"), (ROOT, b"b")),
        Ok(()),
        "onto itself"
    );
    assert_eq!(
        rename(&mut bench, offer, (ROOT, b"b"), (ROOT, b"B")),
        Ok(()),
        "a new spelling"
    );
    let names = listed(&mut bench, offer, ROOT);
    assert_eq!(names, [(String::from("B"), a)]);

    let full = create(&mut bench, offer, b"full", NodeKind::Directory);
    create_in(&mut bench, offer, full, b"inner", NodeKind::RegularFile);
    let empty = create(&mut bench, offer, b"empty", NodeKind::Directory);
    assert_eq!(
        rename(&mut bench, offer, (ROOT, b"empty"), (ROOT, b"full")),
        Err(DriverError::DirectoryNotEmpty)
    );
    assert_eq!(
        rename(&mut bench, offer, (ROOT, b"full"), (ROOT, b"empty")),
        Ok(()),
        "a directory over an empty one"
    );
    assert_eq!(lookup(&mut bench, offer, ROOT, b"empty"), Ok(full));
    assert!(
        lookup(&mut bench, offer, full, b"inner").is_ok(),
        "what it held moved with it"
    );
    assert_eq!(
        lookup(&mut bench, offer, ROOT, b"full"),
        Err(DriverError::NotFound)
    );
    assert_eq!(
        answered(&mut bench, offer, &Request::NodeInfo { node: empty }),
        Err(DriverError::Stale)
    );
    assert_eq!(
        rename(&mut bench, offer, (ROOT, b"empty"), (full, b"sub")),
        Err(DriverError::DirectoryCycle)
    );
    assert_eq!(bench.world().server_opens(), 0);
}

#[test]
fn an_operation_on_the_wrong_kind_of_node_is_unsupported() {
    let (mut bench, offer) = mounted(false);
    let file = create(&mut bench, offer, b"f", NodeKind::RegularFile);
    let dir = create(&mut bench, offer, b"d", NodeKind::Directory);
    let span = Span::new(0, 4).expect("in the window");
    for request in [
        Request::Lookup {
            dir: file,
            name: b"x",
        },
        Request::Parent { dir: file },
        Request::ReadDir {
            dir: file,
            cursor: 0,
            after: b"",
        },
        Request::Create {
            dir: file,
            name: b"x",
            kind: NodeKind::RegularFile,
            record: NodeSecurity::unrecorded(NodeKind::RegularFile),
        },
        Request::ReadAt {
            file: dir,
            offset: 0,
            span,
        },
        Request::WriteAt {
            file: dir,
            offset: 0,
            span,
        },
        Request::Truncate { file: dir, size: 0 },
        Request::SeekRegion {
            file: dir,
            offset: 0,
            region: SeekRegion::Data,
        },
    ] {
        assert_eq!(
            answered(&mut bench, offer, &request),
            Err(DriverError::Unsupported),
            "{request:?}"
        );
    }
    assert_eq!(node(&mut bench, offer, &Request::Parent { dir }), ROOT);
}

#[test]
fn a_long_listing_comes_in_batches_and_resumes_from_any_cursor() {
    let (mut bench, offer) = mounted(false);
    let mut made = Vec::new();
    for at in 0..700 {
        let name = alloc::format!("{at:04}-{}", "n".repeat(96));
        create(&mut bench, offer, name.as_bytes(), NodeKind::RegularFile);
        made.push(name);
    }
    let mut batches = Vec::new();
    let mut cursor = 0;
    loop {
        let request = Request::ReadDir {
            dir: ROOT,
            cursor,
            after: b"",
        };
        let (names, end) = ask(&mut bench, offer, &request, |result| {
            let Ok(Answer::Entries(batch)) = result else {
                panic!("not a batch: {result:?}");
            };
            let mut names = Vec::new();
            for (entry, name) in &batch {
                names.push((
                    String::from_utf8(name.to_vec()).expect("UTF-8"),
                    entry.next_cursor,
                ));
            }
            (names, batch.end())
        });
        cursor = names.last().map_or(cursor, |(_, next)| *next);
        batches.push(names);
        if end == DirEnd::Ended {
            break;
        }
        assert_eq!(end, DirEnd::More);
    }
    assert!(batches.len() > 2, "{} batches", batches.len());
    let mut all: Vec<String> = batches
        .iter()
        .flatten()
        .map(|(name, _)| name.clone())
        .collect();
    all.sort();
    assert_eq!(all, made);

    // A cursor no open listing stands at opens the directory again and passes
    // over what was handed out before it.
    let (resumed_at, tail): (u64, Vec<String>) = (
        batches[0].last().expect("a batch").1,
        batches[1..]
            .iter()
            .flatten()
            .map(|(name, _)| name.clone())
            .collect(),
    );
    let mut cursor = resumed_at;
    let mut again = Vec::new();
    loop {
        let request = Request::ReadDir {
            dir: ROOT,
            cursor,
            after: b"",
        };
        let end = ask(&mut bench, offer, &request, |result| {
            let Ok(Answer::Entries(batch)) = result else {
                panic!("not a batch: {result:?}");
            };
            for (entry, name) in &batch {
                again.push(String::from_utf8(name.to_vec()).expect("UTF-8"));
                cursor = entry.next_cursor;
            }
            batch.end()
        });
        if end == DirEnd::Ended {
            break;
        }
    }
    assert_eq!(again, tail);
    assert_eq!(bench.world().server_opens(), 0, "every listing closed");
}

#[test]
fn a_call_past_its_deadline_times_out_and_its_late_data_lands_nowhere() {
    let (mut bench, offer) = mounted(false);
    let file = create(&mut bench, offer, b"f", NodeKind::RegularFile);
    write(&mut bench, offer, file, 0, b"late bytes");
    window(&mut bench, offer)[4096..4160].fill(0xa5);
    bench.world().hold(true);
    let span = Span::new(4096, 64).expect("in the window");
    let request = Request::ReadAt {
        file,
        offset: 0,
        span,
    };
    let ticket = start(&mut bench, offer, &request);
    bench.pass(CALL_DEADLINE_NS);
    took(&mut bench, offer, ticket, &request, |result| {
        assert_eq!(result, Err(DriverError::TimedOut));
    });
    bench.world().hold(false);
    bench.run();
    assert!(window(&mut bench, offer)[4096..4160]
        .iter()
        .all(|&byte| byte == 0xa5));
    assert_eq!(
        read(&mut bench, offer, file, 0, 4),
        b"late",
        "the volume serves on"
    );
}

#[test]
fn a_volume_holding_calls_tells_the_kernel_it_is_working_and_only_then() {
    let (mut bench, offer) = mounted(false);
    let file = create(&mut bench, offer, b"f", NodeKind::RegularFile);
    write(&mut bench, offer, file, 0, b"bytes");
    bench.world().hold(true);
    let request = Request::ReadAt {
        file,
        offset: 0,
        span: Span::new(4096, 64).expect("in the window"),
    };
    let ticket = start(&mut bench, offer, &request);
    bench.pass(ALIVE_EVERY_NS);
    bench.pass(ALIVE_EVERY_NS);
    assert_eq!(bench.world().volume(offer).expect("offered").alive, 2);
    bench.world().hold(false);
    bench.run();
    took(&mut bench, offer, ticket, &request, |result| {
        assert_eq!(result, Ok(Answer::Count(5)));
    });
    bench.pass(ALIVE_EVERY_NS);
    assert_eq!(
        bench.world().volume(offer).expect("offered").alive,
        2,
        "nothing in hand"
    );
}

#[test]
fn a_release_waits_for_the_read_still_writing_into_its_span() {
    let (mut bench, offer) = mounted(false);
    let file = create(&mut bench, offer, b"f", NodeKind::RegularFile);
    write(&mut bench, offer, file, 0, b"bytes");
    bench.world().hold(true);
    let span = Span::new(4096, 64).expect("in the window");
    let read_request = Request::ReadAt {
        file,
        offset: 0,
        span,
    };
    let reading = start(&mut bench, offer, &read_request);
    let release = Request::Release { span };
    let releasing = start(&mut bench, offer, &release);
    let elsewhere = Request::Release {
        span: Span::new(65_536, 64).expect("in the window"),
    };
    assert_eq!(
        answered(&mut bench, offer, &elsewhere),
        Ok(()),
        "no read writes there"
    );
    bench.world().hold(false);
    bench.run();
    took(&mut bench, offer, reading, &read_request, |result| {
        assert_eq!(result, Ok(Answer::Count(5)));
    });
    took(&mut bench, offer, releasing, &release, |result| {
        assert_eq!(result, Ok(Answer::Done));
    });
}

#[test]
fn a_held_node_outlives_its_name_until_its_last_hold_goes() {
    let (mut bench, offer) = mounted(false);
    let file = create(&mut bench, offer, b"held", NodeKind::RegularFile);
    for _ in 0..2 {
        assert_eq!(node(&mut bench, offer, &Request::Pin { node: file }), file);
    }
    let removed = Request::Remove {
        dir: ROOT,
        name: b"held",
    };
    assert_eq!(answered(&mut bench, offer, &removed), Ok(()));
    assert_eq!(
        answered(&mut bench, offer, &Request::NodeInfo { node: file }),
        Err(DriverError::Stale)
    );
    let unpin = Request::Unpin { pinned: file };
    for left in [Unpinned::Held, Unpinned::Freed] {
        ask(&mut bench, offer, &unpin, |result| {
            assert_eq!(result, Ok(Answer::Unpinned(left)));
        });
    }
    assert_eq!(
        answered(&mut bench, offer, &unpin),
        Err(DriverError::OutOfRange)
    );
    assert_eq!(
        answered(
            &mut bench,
            offer,
            &Request::Pin {
                node: NodeId::from_raw(999)
            }
        ),
        Err(DriverError::Stale)
    );
    let kept = create(&mut bench, offer, b"kept", NodeKind::RegularFile);
    assert_eq!(node(&mut bench, offer, &Request::Pin { node: kept }), kept);
    assert_eq!(answered(&mut bench, offer, &Request::ReleasePins), Ok(()));
    assert_eq!(
        answered(&mut bench, offer, &Request::NodeInfo { node: kept }),
        Ok(())
    );
    assert_eq!(
        answered(&mut bench, offer, &Request::Unpin { pinned: kept }),
        Err(DriverError::OutOfRange)
    );
}

#[test]
fn a_write_of_a_whole_span_is_staged_in_chunks_and_reads_back() {
    let (mut bench, offer) = mounted(false);
    let file = create(&mut bench, offer, b"big", NodeKind::RegularFile);
    let data: Vec<u8> = (0..65_536_u32)
        .map(|at| (at * 7 + at / 251).to_le_bytes()[0])
        .collect();
    window(&mut bench, offer)[65_536..131_072].copy_from_slice(&data);
    let span = Span::new(65_536, 65_536).expect("in the window");
    ask(
        &mut bench,
        offer,
        &Request::WriteAt {
            file,
            offset: 0,
            span,
        },
        |result| {
            assert_eq!(result, Ok(Answer::Count(65_536)));
        },
    );
    assert_eq!(
        bench.world().server_file(&["big"]).as_deref(),
        Some(&data[..])
    );
    let span = Span::new(131_072, 65_536).expect("in the window");
    ask(
        &mut bench,
        offer,
        &Request::ReadAt {
            file,
            offset: 0,
            span,
        },
        |result| {
            assert_eq!(result, Ok(Answer::Count(65_536)));
        },
    );
    assert_eq!(&window(&mut bench, offer)[131_072..196_608], &data[..]);
}

#[test]
fn reads_in_flight_on_two_lanes_land_each_in_its_own_span() {
    let (mut bench, offer) = mounted(false);
    let a = create(&mut bench, offer, b"a", NodeKind::RegularFile);
    write(&mut bench, offer, a, 0, b"first file");
    let b = create(&mut bench, offer, b"b", NodeKind::RegularFile);
    write(&mut bench, offer, b, 0, b"second file");
    bench.world().hold(true);
    let first = Request::ReadAt {
        file: a,
        offset: 0,
        span: Span::new(0, 64).expect("in the window"),
    };
    let second = Request::ReadAt {
        file: b,
        offset: 0,
        span: Span::new(65_536, 64).expect("in the window"),
    };
    let tickets = [
        start(&mut bench, offer, &first),
        start(&mut bench, offer, &second),
    ];
    bench.world().hold(false);
    bench.run();
    took(&mut bench, offer, tickets[0], &first, |result| {
        assert_eq!(result, Ok(Answer::Count(10)));
    });
    took(&mut bench, offer, tickets[1], &second, |result| {
        assert_eq!(result, Ok(Answer::Count(11)));
    });
    assert_eq!(&window(&mut bench, offer)[..10], b"first file");
    assert_eq!(&window(&mut bench, offer)[65_536..65_547], b"second file");
}

#[test]
fn a_worker_reporting_more_than_a_read_holds_is_replaced_and_the_read_asked_again() {
    let mut world = nas();
    let doubling: crate::testing::Perturb = Box::new(|frame: &[u8], out: &mut dyn FrameOut| {
        out.frame(frame)?;
        if matches!(Report::decode(frame), Ok(Report::Data { .. })) {
            out.frame(frame)?;
        }
        Ok(())
    });
    world.hands.push_back(Hand::Chaos(Box::default(), doubling));
    let (mut bench, offer) = mounted_in(world, false);
    let file = create(&mut bench, offer, b"f", NodeKind::RegularFile);
    write(&mut bench, offer, file, 0, b"ten bytes!");
    window(&mut bench, offer)[..CHUNK_MAX].fill(0x5a);
    let request = Request::ReadAt {
        file,
        offset: 0,
        span: Span::new(0, 10).expect("in the window"),
    };
    ask(&mut bench, offer, &request, |result| {
        assert_eq!(
            result,
            Ok(Answer::Count(10)),
            "asked again of the next worker"
        );
    });
    assert_eq!(&window(&mut bench, offer)[..10], b"ten bytes!");
    assert!(window(&mut bench, offer)[10..CHUNK_MAX]
        .iter()
        .all(|&byte| byte == 0x5a));
    assert!(bench.world().withdrawn.is_empty(), "the mount serves on");
    assert!(bench.logged(CONNECTION_LOST));
    assert!(bench.logged(RECONNECTED));
}

#[test]
fn a_directory_holding_anything_is_not_removed() {
    let (mut bench, offer) = mounted(false);
    let dir = create(&mut bench, offer, b"d", NodeKind::Directory);
    create_in(&mut bench, offer, dir, b"inside", NodeKind::RegularFile);
    let remove = |name: &'static [u8]| Request::Remove { dir: ROOT, name };
    assert_eq!(
        answered(&mut bench, offer, &remove(b"d")),
        Err(DriverError::DirectoryNotEmpty)
    );
    assert_eq!(lookup(&mut bench, offer, ROOT, b"d"), Ok(dir));
    let inside = Request::Remove {
        dir,
        name: b"inside",
    };
    assert_eq!(answered(&mut bench, offer, &inside), Ok(()));
    assert_eq!(answered(&mut bench, offer, &remove(b"d")), Ok(()));
    assert_eq!(
        lookup(&mut bench, offer, ROOT, b"d"),
        Err(DriverError::NotFound)
    );
}

#[test]
fn a_listing_resumed_past_the_end_ends_empty() {
    let (mut bench, offer) = mounted(false);
    for name in [b"a", b"b"] {
        create(&mut bench, offer, name, NodeKind::RegularFile);
    }
    let request = Request::ReadDir {
        dir: ROOT,
        cursor: 5,
        after: b"",
    };
    ask(&mut bench, offer, &request, |result| {
        let Ok(Answer::Entries(batch)) = result else {
            panic!("not a batch: {result:?}");
        };
        assert_eq!((&batch).into_iter().count(), 0);
        assert_eq!(batch.end(), DirEnd::Ended);
    });
    assert_eq!(bench.world().server_opens(), 0);
}
