//! Unit tests for a dropped connection made again: what its mounts hold
//! waits, goes again, or is lost, as the drop and its grace window decide.

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::Cell;

use tairix_abi::driver::filesystem::{NodeKind, NodeSecurity};
use tairix_abi::fsserve::{decode_reply, Answer, Request, Span};
use tairix_abi::netshare_ipc::{MountState, NetshareReply, NetshareRequest};
use tairix_abi::DriverError;

use super::{DEFAULT_GRACE_NS, ECHO_AFTER_NS, RETRY_FIRST_NS, RETRY_MAX_NS};
use crate::events::{
    AUTHENTICATION_REJECTED, CONNECTION_ENDED, CONNECTION_LOST, MOUNTS_LOST, RECONNECTED,
    WRITE_BACK_LOST,
};
use crate::front::held::tests::{held, pin};
use crate::front::served::tests::{
    answered, create, lookup, mounted, mounted_in, read, start, took, waiting, window, write,
};
use crate::nodes::ROOT;
use crate::testing::{alice, nas, Bench, Hand, Perturb, NAS};
use crate::wire::Report;

/// The one connection `bench` holds, cut as a network cuts one, and what
/// follows carried.
fn cut(bench: &mut Bench) {
    let [socket] = bench.world().live_sockets()[..] else {
        panic!("one connection");
    };
    bench.world().cut(socket);
    bench.run();
}

/// How `alice`'s one mount is listed.
fn listed(bench: &mut Bench) -> MountState {
    let ticket = bench.call(alice(), &NetshareRequest::Mounts);
    let reply = bench.reply(ticket);
    let Ok(NetshareReply::Mounts(mounts)) = NetshareReply::decode(&reply) else {
        panic!("not mounts");
    };
    let states: Vec<MountState> = mounts.iter().map(|entry| entry.state).collect();
    let [state] = states[..] else {
        panic!("one mount");
    };
    state
}

#[test]
fn a_dropped_connection_is_made_again_and_its_held_file_s_bytes_still_go() {
    let (mut bench, offer, file) = held(b"");
    write(&mut bench, offer, file, 0, b"held across the drop");
    cut(&mut bench);
    assert!(bench.logged(CONNECTION_LOST));
    assert!(bench.logged(RECONNECTED));
    assert_eq!(listed(&mut bench), MountState::Serving);
    assert_eq!(bench.world().server_opens(), 1);
    assert_eq!(
        bench.world().server_detached(),
        0,
        "its open taken back, not made again beside it"
    );
    assert_eq!(bench.world().server_reclaimed(), 1, "by a reconnect create");
    assert_eq!(answered(&mut bench, offer, &Request::Flush), Ok(()));
    assert_eq!(
        bench.world().server_file(&["held.txt"]).as_deref(),
        Some(&b"held across the drop"[..])
    );
    assert!(!bench.logged(WRITE_BACK_LOST));
}

#[test]
fn a_read_asked_as_the_connection_drops_is_answered_once_it_is_back() {
    let (mut bench, offer) = mounted(false);
    let file = create(&mut bench, offer, b"f", NodeKind::RegularFile);
    write(&mut bench, offer, file, 0, b"there all along");
    bench.world().hold(true);
    let request = Request::ReadAt {
        file,
        offset: 0,
        span: Span::new(4096, 15).expect("in the window"),
    };
    let ticket = start(&mut bench, offer, &request);
    cut(&mut bench);
    took(&mut bench, offer, ticket, &request, |result| {
        assert_eq!(result, Ok(Answer::Count(15)));
    });
    assert_eq!(
        &window(&mut bench, offer)[4096..4096 + 15],
        b"there all along"
    );
}

#[test]
fn a_change_asked_as_the_connection_drops_is_answered_offline() {
    let (mut bench, offer) = mounted(false);
    bench.world().hold(true);
    let made = Request::Create {
        dir: ROOT,
        name: b"maybe",
        kind: NodeKind::RegularFile,
        record: NodeSecurity::unrecorded(NodeKind::RegularFile),
    };
    let ticket = start(&mut bench, offer, &made);
    cut(&mut bench);
    took(&mut bench, offer, ticket, &made, |result| {
        assert_eq!(
            result,
            Err(DriverError::DeviceOffline),
            "whether it was made is unknown"
        );
    });
    assert!(
        lookup(&mut bench, offer, ROOT, b"maybe").is_ok(),
        "made, though its caller cannot know"
    );
}

#[test]
fn an_open_whose_answer_the_drop_took_is_asked_again_and_found() {
    let (mut bench, offer) = mounted(false);
    let file = create(&mut bench, offer, b"held.txt", NodeKind::RegularFile);
    write(&mut bench, offer, file, 0, b"abc");
    pin(&mut bench, offer, file);
    bench.world().hold(true);
    let request = Request::ReadAt {
        file,
        offset: 0,
        span: Span::new(4096, 3).expect("in the window"),
    };
    let ticket = start(&mut bench, offer, &request);
    cut(&mut bench);
    took(&mut bench, offer, ticket, &request, |result| {
        assert_eq!(result, Ok(Answer::Count(3)));
    });
    assert_eq!(
        bench.world().server_opens(),
        1,
        "the open the first ask made"
    );
    assert_eq!(bench.world().server_detached(), 0, "and no other");
    assert_eq!(bench.world().server_reclaimed(), 0, "found by its replay");
}

#[test]
fn a_file_first_held_while_its_connection_is_down_is_served_once_it_is_back() {
    let (mut bench, offer) = mounted(false);
    let file = create(&mut bench, offer, b"f", NodeKind::RegularFile);
    write(&mut bench, offer, file, 0, b"late");
    pin(&mut bench, offer, file);
    let (_, server) = bench.world().servers.remove(0);
    cut(&mut bench);
    let request = Request::ReadAt {
        file,
        offset: 0,
        span: Span::new(4096, 4).expect("in the window"),
    };
    let ticket = start(&mut bench, offer, &request);
    bench.world().servers.push((NAS, server));
    bench.pass(RETRY_FIRST_NS);
    assert!(bench.logged(RECONNECTED));
    took(&mut bench, offer, ticket, &request, |result| {
        assert_eq!(
            result,
            Ok(Answer::Count(4)),
            "its work run as the connection came back"
        );
    });
}

#[test]
fn a_lost_worker_is_replaced_as_a_dropped_connection_is() {
    let dead = Rc::new(Cell::new(false));
    let mut world = nas();
    world
        .hands
        .push_back(Hand::Mortal(Box::default(), Rc::clone(&dead)));
    let (mut bench, offer) = mounted_in(world, false);
    let file = create(&mut bench, offer, b"f", NodeKind::RegularFile);
    write(&mut bench, offer, file, 0, b"before");
    dead.set(true);
    assert_eq!(
        read(&mut bench, offer, file, 0, 6),
        b"before",
        "asked again of its successor"
    );
    assert!(bench.logged(CONNECTION_LOST));
    assert!(bench.logged(RECONNECTED));
}

#[test]
fn a_connection_gone_quiet_is_asked_after_and_then_made_again() {
    let (mut bench, offer) = mounted(false);
    let file = create(&mut bench, offer, b"f", NodeKind::RegularFile);
    write(&mut bench, offer, file, 0, b"quiet");
    let [socket] = bench.world().live_sockets()[..] else {
        panic!("one connection");
    };
    bench.world().silence(socket);
    let request = Request::ReadAt {
        file,
        offset: 0,
        span: Span::new(4096, 5).expect("in the window"),
    };
    let ticket = start(&mut bench, offer, &request);
    bench.pass(ECHO_AFTER_NS);
    assert!(waiting(&mut bench, offer, ticket), "the server asked after");
    assert!(!bench.logged(CONNECTION_LOST));
    bench.pass(ECHO_AFTER_NS);
    assert!(
        bench.logged(CONNECTION_LOST),
        "and its silence taken for a drop"
    );
    took(&mut bench, offer, ticket, &request, |result| {
        assert_eq!(result, Ok(Answer::Count(5)));
    });
}

#[test]
fn a_quiet_connection_whose_server_answers_each_ask_is_kept() {
    let (mut bench, offer, file) = held(b"kept");
    // Read once, so the server holds an open and a lease for it.
    assert_eq!(read(&mut bench, offer, file, 0, 4), b"kept");
    let asked = bench.world().echoes();
    for _ in 0..6 {
        bench.pass(ECHO_AFTER_NS);
    }
    assert_eq!(
        bench.world().echoes() - asked,
        6,
        "asked after once a quiet spell"
    );
    assert!(!bench.logged(CONNECTION_LOST), "and each ask answered");
    assert_eq!(read(&mut bench, offer, file, 0, 4), b"kept");
}

#[test]
fn an_idle_mount_takes_no_wakeups_and_is_asked_after_once_a_call_rests_on_it() {
    let (mut bench, offer) = mounted(false);
    let [socket] = bench.world().live_sockets()[..] else {
        panic!("one connection");
    };
    bench.world().silence(socket);
    bench.pass(10 * ECHO_AFTER_NS);
    assert_eq!(bench.front.next_deadline(), None, "nothing rests on it");
    assert!(
        !bench.logged(CONNECTION_LOST),
        "so its silence is not asked after"
    );
    let request = Request::NodeInfo { node: ROOT };
    let ticket = start(&mut bench, offer, &request);
    bench.pass(ECHO_AFTER_NS);
    assert!(
        !bench.logged(CONNECTION_LOST),
        "its silence counted from the call, not from when it was last heard"
    );
    bench.pass(ECHO_AFTER_NS);
    assert!(
        bench.logged(CONNECTION_LOST),
        "the ask it then took left unanswered"
    );
    assert!(bench.logged(RECONNECTED));
    took(&mut bench, offer, ticket, &request, |result| {
        assert!(matches!(result, Ok(Answer::Info(_))), "{result:?}");
    });
}

#[test]
fn a_server_that_restarted_has_the_held_file_opened_afresh_and_its_bytes_sent() {
    let (mut bench, offer, file) = held(b"");
    write(&mut bench, offer, file, 0, b"after a restart");
    let [socket] = bench.world().live_sockets()[..] else {
        panic!("one connection");
    };
    bench.world().cut(socket);
    bench.world().restart();
    bench.run();
    assert!(bench.logged(RECONNECTED));
    assert_eq!(answered(&mut bench, offer, &Request::Flush), Ok(()));
    assert_eq!(
        bench.world().server_file(&["held.txt"]).as_deref(),
        Some(&b"after a restart"[..])
    );
    assert!(!bench.logged(WRITE_BACK_LOST));
}

#[test]
fn past_the_grace_window_every_call_fails_at_once_until_its_server_is_back() {
    let (mut bench, offer, file) = held(b"");
    write(&mut bench, offer, file, 0, b"lost to the outage");
    let (_, server) = bench.world().servers.remove(0);
    cut(&mut bench);
    assert_eq!(listed(&mut bench), MountState::Reconnecting);
    bench.pass(DEFAULT_GRACE_NS);
    assert!(bench.logged(MOUNTS_LOST));
    assert!(bench.logged(WRITE_BACK_LOST), "what was held never went");
    assert_eq!(listed(&mut bench), MountState::Lost);
    let ticket = bench.serve(offer, &Request::NodeInfo { node: file });
    assert!(!waiting(&mut bench, offer, ticket), "answered at once");
    assert_eq!(
        answered(&mut bench, offer, &Request::Flush),
        Err(DriverError::DeviceOffline),
        "the loss owed to the flush"
    );
    bench.world().servers.push((NAS, server));
    bench.pass(RETRY_MAX_NS);
    assert!(bench.logged(RECONNECTED), "still tried");
    assert_eq!(listed(&mut bench), MountState::Serving);
    assert_eq!(
        read(&mut bench, offer, file, 0, 32),
        b"",
        "nothing ever reached it"
    );
}

#[test]
fn a_server_answering_as_another_on_the_reconnect_ends_its_mounts() {
    let (mut bench, offer) = mounted(false);
    bench.world().servers[0].1.server_guid = *b"another server!!";
    cut(&mut bench);
    assert!(bench.logged(CONNECTION_ENDED));
    assert!(!bench.logged(RECONNECTED));
    assert_eq!(bench.world().withdrawn, [offer]);
}

#[test]
fn an_account_the_server_now_refuses_ends_the_mounts_rather_than_being_tried_again() {
    let (mut bench, offer) = mounted(false);
    bench.world().servers[0].1.password = "changed meanwhile";
    cut(&mut bench);
    assert!(bench.logged(AUTHENTICATION_REJECTED));
    assert_eq!(bench.world().withdrawn, [offer]);
    let opened = bench.world().opened();
    bench.pass(RETRY_MAX_NS);
    assert_eq!(bench.world().opened(), opened, "never tried again");
}

#[test]
fn attempts_on_a_server_that_is_gone_wait_longer_each_time_and_never_stop() {
    let (mut bench, _offer) = mounted(false);
    bench.world().servers.remove(0);
    cut(&mut bench);
    let after_drop = bench.world().opened();
    bench.pass(RETRY_FIRST_NS - 1);
    assert_eq!(bench.world().opened(), after_drop, "the first wait");
    bench.pass(1);
    assert_eq!(bench.world().opened(), after_drop + 1);
    bench.pass(2 * RETRY_FIRST_NS - 1);
    assert_eq!(bench.world().opened(), after_drop + 1, "twice as long");
    bench.pass(1);
    assert_eq!(bench.world().opened(), after_drop + 2);
    for _ in 0..8 {
        bench.pass(RETRY_MAX_NS);
    }
    let late = bench.world().opened();
    bench.pass(RETRY_MAX_NS - 1);
    assert_eq!(
        bench.world().opened(),
        late,
        "no more often than the longest wait"
    );
    bench.pass(1);
    assert_eq!(bench.world().opened(), late + 1, "and never stopping");
}

/// A worker reporting every read's data twice, which no read holds.
fn doubling() -> Hand {
    let perturb: Perturb = Box::new(
        |frame: &[u8], out: &mut dyn tairix_sandbox::session::FrameOut| {
            out.frame(frame)?;
            if matches!(Report::decode(frame), Ok(Report::Data { .. })) {
                out.frame(frame)?;
            }
            Ok(())
        },
    );
    Hand::Chaos(Box::default(), perturb)
}

#[test]
fn a_connection_that_drops_as_soon_as_it_is_back_waits_longer_each_time() {
    let (mut bench, offer) = mounted(false);
    let file = create(&mut bench, offer, b"f", NodeKind::RegularFile);
    write(&mut bench, offer, file, 0, b"flap");
    for _ in 0..3 {
        bench.world().hands.push_back(doubling());
    }
    cut(&mut bench);
    let request = Request::ReadAt {
        file,
        offset: 0,
        span: Span::new(4096, 4).expect("in the window"),
    };
    let ticket = start(&mut bench, offer, &request);
    let opened = bench.world().opened();
    for (wait, attempts) in [(1, 1), (2, 2), (4, 3)] {
        bench.pass(wait * RETRY_FIRST_NS - 1);
        assert_eq!(
            bench.world().opened(),
            opened + attempts - 1,
            "back only to drop: it waits {wait} s"
        );
        bench.pass(1);
        assert_eq!(bench.world().opened(), opened + attempts);
    }
    took(&mut bench, offer, ticket, &request, |result| {
        assert_eq!(
            result,
            Ok(Answer::Count(4)),
            "until an honest worker answers"
        );
    });
}

#[test]
fn an_unmount_with_its_server_gone_loses_what_was_held_and_says_so() {
    let (mut bench, offer, file) = held(b"");
    write(&mut bench, offer, file, 0, b"never sent");
    bench.world().servers.remove(0);
    cut(&mut bench);
    let unmounting = bench.call(alice(), &NetshareRequest::Unmount { offer });
    bench.pass(super::CALL_DEADLINE_NS - 1);
    // Asked as the unmount runs out of time, so the volume's going answers it.
    let info = Request::NodeInfo { node: file };
    let ticket = bench.serve(offer, &info);
    bench.pass(1);
    assert!(!bench.unanswered(unmounting));
    assert_eq!(bench.world().withdrawn, [offer]);
    assert!(bench.logged(WRITE_BACK_LOST));
    let reply = bench.answer(offer, ticket);
    let reply = decode_reply(&reply, &info).expect("a reply the kernel reads");
    assert!(
        reply.state.unpublished,
        "the kernel told bytes were held that never went"
    );
}
