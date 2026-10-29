//! Unit tests for the files the kernel holds open on a mounted share: the
//! cache their leases allow, the bytes held written, and what sends them.

use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::driver::filesystem::{NodeId, NodeKind, Unpinned};
use tairix_abi::fs::SeekRegion;
use tairix_abi::fsserve::{decode_reply, Answer, Request, Span};
use tairix_abi::netattach::OfferId;
use tairix_abi::netshare_ipc::NetshareRequest;
use tairix_abi::DriverError;
use tairix_reclaim::{PressureBand, ReportedPressure};
use tairix_smb::negotiate::Dialect;
use tairix_smb::peer::{Fault, PeerConfig, DISK_SHARE};

use super::{PAGE, WRITE_BACK_NS};
use crate::events::WRITE_BACK_LOST;
use crate::front::served::tests::{
    answered, ask, create, lookup, mounted, mounted_in, node, read, rename, window, write,
};
use crate::front::{Caller, CALL_DEADLINE_NS};
use crate::nodes::ROOT;
use crate::testing::{connect, nas, session, Bench, World, BOB, PASSWORD};

/// Hold `file` as an open descriptor does.
pub(in crate::front) fn pin(bench: &mut Bench, offer: OfferId, file: NodeId) {
    assert_eq!(node(bench, offer, &Request::Pin { node: file }), file);
}

fn unpin(bench: &mut Bench, offer: OfferId, file: NodeId) -> Unpinned {
    ask(
        bench,
        offer,
        &Request::Unpin { pinned: file },
        |result| match result {
            Ok(Answer::Unpinned(left)) => left,
            other => panic!("not an unpin: {other:?}"),
        },
    )
}

/// The answer `request` takes and whether its reply says the volume holds
/// bytes the server does not have.
pub(in crate::front) fn told(
    bench: &mut Bench,
    offer: OfferId,
    request: &Request<'_>,
) -> (Result<(), DriverError>, bool) {
    let ticket = bench.serve(offer, request);
    let reply = bench.answer(offer, ticket);
    let reply = decode_reply(&reply, request).expect("a reply the kernel reads");
    (reply.result.map(|_| ()), reply.state.unpublished)
}

/// A mounted share holding `name`, made with `data`, and pinned.
pub(in crate::front) fn held(data: &[u8]) -> (Bench, OfferId, NodeId) {
    let (mut bench, offer) = mounted(false);
    let file = create(&mut bench, offer, b"held.txt", NodeKind::RegularFile);
    if !data.is_empty() {
        write(&mut bench, offer, file, 0, data);
    }
    pin(&mut bench, offer, file);
    (bench, offer, file)
}

#[test]
fn a_held_file_s_pages_are_read_once_while_its_lease_lets_it() {
    let (mut bench, offer, file) = held(b"hello, world");
    assert_eq!(read(&mut bench, offer, file, 7, 64), b"world");
    // With the server's answers held back, only a cached page answers.
    bench.world().hold(true);
    assert_eq!(
        read(&mut bench, offer, file, 0, 5),
        b"hello",
        "from the cache"
    );
    let info = ask(
        &mut bench,
        offer,
        &Request::NodeInfo { node: file },
        |result| match result {
            Ok(Answer::Info(info)) => info.size,
            other => panic!("not info: {other:?}"),
        },
    );
    assert_eq!(info, 12, "the attributes the lease keeps current");
    let seek = Request::SeekRegion {
        file,
        offset: 3,
        region: SeekRegion::Hole,
    };
    ask(&mut bench, offer, &seek, |result| {
        assert_eq!(result, Ok(Answer::Region(Some(12))));
    });
    bench.world().hold(false);
    bench.run();
    assert_eq!(read(&mut bench, offer, file, 12, 8), b"", "the file's end");
}

#[test]
fn a_held_write_is_answered_at_once_sent_by_its_window_and_said_unpublished_meanwhile() {
    let (mut bench, offer, file) = held(b"0123456789");
    let at = 8192;
    window(&mut bench, offer)[at..at + 3].copy_from_slice(b"abc");
    let span = Span::new(at, 3).expect("in the window");
    let written = Request::WriteAt {
        file,
        offset: 2,
        span,
    };
    let (result, unpublished) = told(&mut bench, offer, &written);
    assert_eq!((result, unpublished), (Ok(()), true));
    assert_eq!(
        bench.world().server_file(&["held.txt"]).as_deref(),
        Some(&b"0123456789"[..]),
        "held, not yet sent"
    );
    assert_eq!(
        read(&mut bench, offer, file, 0, 10),
        b"01abc56789",
        "what is held is read"
    );
    bench.pass(WRITE_BACK_NS);
    assert_eq!(
        bench.world().server_file(&["held.txt"]).as_deref(),
        Some(&b"01abc56789"[..]),
        "sent by its window"
    );
    let (_, unpublished) = told(&mut bench, offer, &Request::NodeInfo { node: file });
    assert!(!unpublished, "nothing held once the server has it");
}

#[test]
fn a_flush_sends_and_commits_every_held_byte() {
    let (mut bench, offer, file) = held(b"");
    write(&mut bench, offer, file, 0, b"durable");
    assert_eq!(
        bench.world().server_file(&["held.txt"]).as_deref(),
        Some(&b""[..])
    );
    assert_eq!(answered(&mut bench, offer, &Request::Flush), Ok(()));
    assert_eq!(
        bench.world().server_file(&["held.txt"]).as_deref(),
        Some(&b"durable"[..])
    );
    assert_eq!(
        answered(&mut bench, offer, &Request::Flush),
        Ok(()),
        "nothing left"
    );
}

#[test]
fn the_last_hold_let_go_sends_what_is_held_and_closes_the_open() {
    let (mut bench, offer, file) = held(b"");
    pin(&mut bench, offer, file);
    write(&mut bench, offer, file, 0, b"kept");
    assert_eq!(unpin(&mut bench, offer, file), Unpinned::Held);
    assert_eq!(bench.world().server_opens(), 1, "still held once");
    assert_eq!(unpin(&mut bench, offer, file), Unpinned::Released);
    assert_eq!(
        bench.world().server_file(&["held.txt"]).as_deref(),
        Some(&b"kept"[..])
    );
    assert_eq!(bench.world().server_opens(), 0, "the open let go");
}

#[test]
fn a_truncate_lands_after_the_bytes_written_before_it() {
    let (mut bench, offer, file) = held(b"");
    write(&mut bench, offer, file, 0, b"0123456789");
    let truncated = Request::Truncate { file, size: 4 };
    assert_eq!(answered(&mut bench, offer, &truncated), Ok(()));
    assert_eq!(
        bench.world().server_file(&["held.txt"]).as_deref(),
        Some(&b"0123"[..])
    );
    assert_eq!(read(&mut bench, offer, file, 0, 16), b"0123");
    write(&mut bench, offer, file, 6, b"x");
    assert_eq!(
        read(&mut bench, offer, file, 0, 16),
        b"0123\0\0x",
        "a hole reads as zeros"
    );
}

#[test]
fn another_client_s_open_breaks_the_lease_and_finds_the_bytes_held() {
    let (mut bench, offer, file) = held(b"");
    write(&mut bench, offer, file, 0, b"alice's bytes");
    let bob = Caller {
        uid: BOB,
        session: Some(session(2)),
    };
    let ticket = bench.call(
        bob,
        &NetshareRequest::Mount {
            connect: connect(PASSWORD),
            share: DISK_SHARE,
            read_only: false,
        },
    );
    let (_, theirs) = bench.mounted(ticket);
    let seen = lookup(&mut bench, theirs, ROOT, b"held.txt").expect("on the share");
    assert_eq!(
        read(&mut bench, theirs, seen, 0, 64),
        b"alice's bytes",
        "the break sent what was held before the other open went on"
    );
    write(&mut bench, offer, file, 0, b"A");
    assert_eq!(
        bench.world().server_file(&["held.txt"]).as_deref(),
        Some(&b"Alice's bytes"[..]),
        "a lease broken to reads holds no write"
    );
}

#[test]
fn a_write_the_server_refuses_after_it_was_answered_is_owed_to_the_next_flush() {
    let (mut bench, offer, file) = held(b"");
    write(
        &mut bench,
        offer,
        file,
        (64 << 20) - 1,
        b"past the share's room",
    );
    assert_eq!(
        answered(&mut bench, offer, &Request::Flush),
        Err(DriverError::NoSpace)
    );
    assert!(bench.logged(WRITE_BACK_LOST));
    assert_eq!(
        answered(&mut bench, offer, &Request::Flush),
        Ok(()),
        "owed once"
    );
}

#[test]
fn a_loss_at_a_file_s_close_is_still_owed_to_the_next_flush() {
    let (mut bench, offer, file) = held(b"");
    write(
        &mut bench,
        offer,
        file,
        (64 << 20) - 1,
        b"past the share's room",
    );
    assert_eq!(unpin(&mut bench, offer, file), Unpinned::Released);
    assert_eq!(bench.world().server_opens(), 0, "the file let go");
    assert!(bench.logged(WRITE_BACK_LOST));
    assert_eq!(
        answered(&mut bench, offer, &Request::Flush),
        Err(DriverError::NoSpace)
    );
}

/// The size `offer` tells the kernel `file` has.
fn size_of(bench: &mut Bench, offer: OfferId, file: NodeId) -> u64 {
    ask(
        bench,
        offer,
        &Request::NodeInfo { node: file },
        |result| match result {
            Ok(Answer::Info(info)) => info.size,
            other => panic!("not info: {other:?}"),
        },
    )
}

#[test]
fn a_short_write_sends_its_rest_again_and_one_taking_nothing_is_lost() {
    let config = PeerConfig {
        fault: Fault::ShortWrite,
        ..PeerConfig::new(Dialect::Smb311)
    };
    let (mut bench, offer) = mounted_in(World::serving(config), false);
    let file = create(&mut bench, offer, b"held.txt", NodeKind::RegularFile);
    pin(&mut bench, offer, file);
    write(&mut bench, offer, file, 0, b"0123456789");
    assert_eq!(size_of(&mut bench, offer, file), 10, "what is held, told");
    assert_eq!(
        answered(&mut bench, offer, &Request::Flush),
        Err(DriverError::NoSpace)
    );
    assert_eq!(
        bench.world().server_file(&["held.txt"]).as_deref(),
        Some(&b"012345678"[..]),
        "each rest went again until a write took nothing"
    );
    assert!(bench.logged(WRITE_BACK_LOST));
    assert_eq!(
        size_of(&mut bench, offer, file),
        9,
        "the server's word, once a loss leaves nothing to trust"
    );
    assert_eq!(read(&mut bench, offer, file, 0, 16), b"012345678");
}

#[test]
fn scattered_writes_are_bounded_by_what_they_pin_not_by_their_bytes() {
    static CALM: ReportedPressure = ReportedPressure::unknown();
    CALM.report(PressureBand::Normal);
    // A sixteenth of this is the ceiling: one page, which a few blocks fill.
    let mut bench = Bench::caching(nas(), 16 * PAGE, &CALM);
    let ticket = bench.call(
        crate::testing::alice(),
        &NetshareRequest::Mount {
            connect: connect(PASSWORD),
            share: DISK_SHARE,
            read_only: false,
        },
    );
    let (_, offer) = bench.mounted(ticket);
    let file = create(&mut bench, offer, b"sparse", NodeKind::RegularFile);
    pin(&mut bench, offer, file);
    for at in 0..32u64 {
        write(&mut bench, offer, file, at * 8192, b"x");
    }
    let server = bench
        .world()
        .server_file(&["sparse"])
        .expect("on the share");
    assert!(
        !server.is_empty(),
        "thirty-two bytes, a block each, pin past one page: some were sent to make room"
    );
    assert_eq!(answered(&mut bench, offer, &Request::Flush), Ok(()));
    let server = bench
        .world()
        .server_file(&["sparse"])
        .expect("on the share");
    assert_eq!(server.len(), 31 * 8192 + 1);
    assert!(server
        .iter()
        .enumerate()
        .all(|(at, &byte)| byte == if at % 8192 == 0 { b'x' } else { 0 }));
}

#[test]
fn a_wave_answered_slowly_but_steadily_is_never_given_up() {
    let (mut bench, offer, file) = held(b"");
    let data = vec![0x6b; PAGE];
    window(&mut bench, offer)[PAGE..2 * PAGE].copy_from_slice(&data);
    let span = Span::new(PAGE, PAGE).expect("in the window");
    let writes = 48;
    for at in 0..writes {
        let request = Request::WriteAt {
            file,
            offset: (at * PAGE) as u64,
            span,
        };
        assert_eq!(answered(&mut bench, offer, &request), Ok(()));
    }
    bench.world().hold(true);
    bench.pass(WRITE_BACK_NS);
    // Each answer a breath short of what would make the server's silence
    // worth asking about: far longer than a call's deadline in all, never
    // one without progress.
    let gap = crate::front::ECHO_AFTER_NS - 1;
    let mut answers = 0u64;
    for _ in 0..4 * writes {
        bench.pass(gap);
        if !bench.let_one_through() {
            break;
        }
        answers += 1;
    }
    assert!(
        answers * gap > CALL_DEADLINE_NS,
        "the wave outlived a deadline"
    );
    bench.world().hold(false);
    bench.run();
    assert!(!bench.logged(WRITE_BACK_LOST), "nothing given up");
    assert!(
        !bench.logged(crate::events::CONNECTION_LOST),
        "nor the connection"
    );
    assert_eq!(answered(&mut bench, offer, &Request::Flush), Ok(()));
    let server = bench
        .world()
        .server_file(&["held.txt"])
        .expect("on the share");
    assert_eq!(server.len(), writes * PAGE);
    assert!(server.iter().all(|&byte| byte == 0x6b));
}

#[test]
fn a_rename_over_a_held_file_waits_for_the_front_to_let_it_go() {
    let (mut bench, offer, held_file) = held(b"old");
    assert_eq!(read(&mut bench, offer, held_file, 0, 8), b"old");
    let other = create(&mut bench, offer, b"new.txt", NodeKind::RegularFile);
    write(&mut bench, offer, other, 0, b"new");
    assert_eq!(
        rename(&mut bench, offer, (ROOT, b"new.txt"), (ROOT, b"held.txt")),
        Ok(())
    );
    assert_eq!(
        bench.world().server_file(&["held.txt"]).as_deref(),
        Some(&b"new"[..])
    );
    assert_eq!(lookup(&mut bench, offer, ROOT, b"held.txt"), Ok(other));
}

#[test]
fn an_open_file_outlives_its_name() {
    let (mut bench, offer, file) = held(b"still here");
    assert_eq!(read(&mut bench, offer, file, 0, 5), b"still");
    let removed = Request::Remove {
        dir: ROOT,
        name: b"held.txt",
    };
    assert_eq!(answered(&mut bench, offer, &removed), Ok(()));
    assert_eq!(
        lookup(&mut bench, offer, ROOT, b"held.txt"),
        Err(DriverError::NotFound)
    );
    assert_eq!(
        read(&mut bench, offer, file, 6, 4),
        b"here",
        "its open reads on"
    );
    assert_eq!(unpin(&mut bench, offer, file), Unpinned::Freed);
    assert_eq!(bench.world().server_file(&["held.txt"]), None);
    assert_eq!(bench.world().server_opens(), 0);
}

#[test]
fn past_the_bound_what_is_held_is_sent_before_more_is_held() {
    static CALM: ReportedPressure = ReportedPressure::unknown();
    CALM.report(PressureBand::Normal);
    // A sixteenth of this is the ceiling: one page.
    let mut bench = Bench::caching(nas(), 16 * PAGE, &CALM);
    let ticket = bench.call(
        crate::testing::alice(),
        &NetshareRequest::Mount {
            connect: connect(PASSWORD),
            share: DISK_SHARE,
            read_only: false,
        },
    );
    let (_, offer) = bench.mounted(ticket);
    let file = create(&mut bench, offer, b"big", NodeKind::RegularFile);
    pin(&mut bench, offer, file);
    let data = vec![0x5a; PAGE];
    window(&mut bench, offer)[PAGE..2 * PAGE].copy_from_slice(&data);
    let whole = |offset| Request::WriteAt {
        file,
        offset,
        span: Span::new(PAGE, PAGE).expect("in the window"),
    };
    assert_eq!(told(&mut bench, offer, &whole(0)), (Ok(()), true), "held");
    assert_eq!(
        bench.world().server_file(&["big"]).map(|bytes| bytes.len()),
        Some(0)
    );
    assert_eq!(told(&mut bench, offer, &whole(PAGE as u64)), (Ok(()), true));
    let server = bench.world().server_file(&["big"]).expect("on the share");
    assert_eq!(
        server.len(),
        PAGE,
        "the first sent to make room for the second"
    );
    assert_eq!(answered(&mut bench, offer, &Request::Flush), Ok(()));
    let server = bench.world().server_file(&["big"]).expect("on the share");
    assert_eq!(server.len(), 2 * PAGE);
    assert!(server.iter().all(|&byte| byte == 0x5a));
}

#[test]
fn under_critical_pressure_no_page_is_kept() {
    static TIGHT: ReportedPressure = ReportedPressure::unknown();
    TIGHT.report(PressureBand::Critical);
    let mut bench = Bench::caching(nas(), 1 << 30, &TIGHT);
    let ticket = bench.call(
        crate::testing::alice(),
        &NetshareRequest::Mount {
            connect: connect(PASSWORD),
            share: DISK_SHARE,
            read_only: false,
        },
    );
    let (_, offer) = bench.mounted(ticket);
    let file = create(&mut bench, offer, b"f", NodeKind::RegularFile);
    write(&mut bench, offer, file, 0, b"sent");
    pin(&mut bench, offer, file);
    assert_eq!(read(&mut bench, offer, file, 0, 8), b"sent");
    bench.world().hold(true);
    let span = Span::new(4096, 4).expect("in the window");
    let request = Request::ReadAt {
        file,
        offset: 0,
        span,
    };
    let ticket = bench.serve(offer, &request);
    assert!(
        bench
            .world()
            .volume(offer)
            .expect("offered")
            .answers
            .iter()
            .all(|(answered, _)| *answered != ticket),
        "no page was kept to answer from"
    );
}

#[test]
fn under_critical_pressure_every_write_is_sent_before_it_is_answered() {
    static TIGHT: ReportedPressure = ReportedPressure::unknown();
    TIGHT.report(PressureBand::Critical);
    let mut bench = Bench::caching(nas(), 1 << 30, &TIGHT);
    let ticket = bench.call(
        crate::testing::alice(),
        &NetshareRequest::Mount {
            connect: connect(PASSWORD),
            share: DISK_SHARE,
            read_only: false,
        },
    );
    let (_, offer) = bench.mounted(ticket);
    let file = create(&mut bench, offer, b"f", NodeKind::RegularFile);
    pin(&mut bench, offer, file);
    window(&mut bench, offer)[..4].copy_from_slice(b"sent");
    let written = Request::WriteAt {
        file,
        offset: 0,
        span: Span::new(0, 4).expect("in the window"),
    };
    assert_eq!(
        told(&mut bench, offer, &written),
        (Ok(()), false),
        "nothing held"
    );
    assert_eq!(
        bench.world().server_file(&["f"]).as_deref(),
        Some(&b"sent"[..])
    );
}

#[test]
fn a_read_spanning_two_pages_lays_each_and_what_is_held_over_them() {
    let (mut bench, offer, file) = held(b"");
    let data: Vec<u8> = (0..PAGE + 100)
        .map(|at| (at % 251).to_le_bytes()[0])
        .collect();
    for (at, chunk) in data.chunks(PAGE).enumerate() {
        let span = Span::new(PAGE, chunk.len()).expect("in the window");
        window(&mut bench, offer)[PAGE..PAGE + chunk.len()].copy_from_slice(chunk);
        let request = Request::WriteAt {
            file,
            offset: (at * PAGE) as u64,
            span,
        };
        assert_eq!(answered(&mut bench, offer, &request), Ok(()));
    }
    assert_eq!(answered(&mut bench, offer, &Request::Flush), Ok(()));
    write(&mut bench, offer, file, PAGE as u64 - 2, b"XYZW");
    let got = read(&mut bench, offer, file, PAGE as u64 - 50, 100);
    let mut expected = data[PAGE - 50..PAGE + 50].to_vec();
    expected[48..52].copy_from_slice(b"XYZW");
    assert_eq!(got, expected);
    let unpinned = Request::Unpin { pinned: file };
    ask(&mut bench, offer, &unpinned, |result| {
        assert_eq!(result, Ok(Answer::Unpinned(Unpinned::Released)));
    });
}

/// Unmount `offer` as alice, answering the call's ticket.
fn unmount(bench: &mut Bench, offer: OfferId) -> u64 {
    bench.call(crate::testing::alice(), &NetshareRequest::Unmount { offer })
}

fn unmounted(bench: &mut Bench, ticket: u64) -> bool {
    !bench.unanswered(ticket)
        && matches!(
            tairix_abi::netshare_ipc::NetshareReply::decode(&bench.reply(ticket)),
            Ok(tairix_abi::netshare_ipc::NetshareReply::Unmounted)
        )
}

#[test]
fn an_unmount_sends_what_is_held_before_the_volume_goes() {
    let (mut bench, offer, file) = held(b"");
    write(&mut bench, offer, file, 0, b"kept on unmount");
    let ticket = unmount(&mut bench, offer);
    assert!(unmounted(&mut bench, ticket));
    assert_eq!(
        bench.world().server_file(&["held.txt"]).as_deref(),
        Some(&b"kept on unmount"[..])
    );
    assert_eq!(bench.world().withdrawn, [offer]);
    assert_eq!(bench.world().server_opens(), 0);
}

#[test]
fn a_session_s_end_sends_what_is_held_before_its_mounts_go() {
    let (mut bench, offer, file) = held(b"");
    write(&mut bench, offer, file, 0, b"kept at logout");
    let now = bench.now;
    bench.front.peer_exited(now, session(1));
    bench.run();
    assert_eq!(
        bench.world().server_file(&["held.txt"]).as_deref(),
        Some(&b"kept at logout"[..])
    );
    assert_eq!(bench.world().withdrawn, [offer]);
}

#[test]
fn an_unmount_waits_on_a_silent_server_no_longer_than_a_call_s_deadline() {
    let (mut bench, offer, file) = held(b"");
    write(&mut bench, offer, file, 0, b"never sent");
    bench.world().hold(true);
    let ticket = unmount(&mut bench, offer);
    assert!(bench.unanswered(ticket), "waits on the held bytes");
    bench.pass(crate::front::CALL_DEADLINE_NS);
    assert!(unmounted(&mut bench, ticket));
    assert_eq!(bench.world().withdrawn, [offer]);
}
