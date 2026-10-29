//! Unit tests for the front, over the in-memory world of `crate::testing`.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::net::{IpAddr, Ipv4Addr};

use tairix_abi::netshare_ipc::{
    Connect, Credentials, Endpoint, MountState, NetshareError, NetshareReply, NetshareRequest, Pin,
    Rejection,
};
use tairix_smb::peer::{DISK_SHARE, SERVER_GUID};

use super::{Caller, CALLS_MAX, CALL_DEADLINE_NS, CONNECTIONS_MAX, LINGER_NS};
use crate::events::{
    AUTHENTICATION_REJECTED, CONNECTION_ENDED, MOUNTED, PIN_MISMATCH, REQUEST_UNREADABLE, UNMOUNTED,
};
use crate::host::{Resolution, Unresolved};
use crate::testing::{
    alice, connect, mount, nas, session, shares, Bench, Hand, Perturb, ALICE, BOB, ELSEWHERE, NAS,
    PASSWORD, SILENT,
};
use crate::wire::Report;
use tairix_smb::tree::ShareType;

#[test]
fn a_listing_names_the_disk_shares_and_the_pin_the_connection_reported() {
    let mut bench = Bench::new(nas());
    let ticket = bench.call(alice(), &shares(connect(PASSWORD)));
    let reply = bench.reply(ticket);
    let NetshareReply::Shares(listed) = NetshareReply::decode(&reply).expect("a reply") else {
        panic!("not a listing");
    };
    assert_eq!(
        listed.pin,
        Pin {
            server_guid: SERVER_GUID,
            address: NAS
        }
    );
    let names: Vec<&str> = listed.shares.iter().map(|entry| entry.name).collect();
    assert_eq!(names, [DISK_SHARE]);
    assert_eq!((listed.hidden, listed.unnamed, listed.unlisted), (1, 0, 0));
}

#[test]
fn a_host_named_without_an_address_is_resolved_off_the_loop() {
    let mut bench = Bench::new(nas());
    let unnamed = Connect {
        endpoint: Endpoint {
            address: None,
            ..connect(PASSWORD).endpoint
        },
        ..connect(PASSWORD)
    };
    let ticket = bench.call(alice(), &shares(unnamed));
    assert!(bench.unanswered(ticket), "waiting on the lookup");
    let (conn, host) = bench.world().asked.pop().expect("asked");
    assert_eq!(host, "nas.example");
    bench.front.resolved(bench.now, conn, Ok(NAS));
    bench.run();
    let reply = bench.reply(ticket);
    assert!(matches!(
        NetshareReply::decode(&reply),
        Ok(NetshareReply::Shares(_))
    ));

    for (resolution, refusal) in [
        (Err(Unresolved::NoSuchHost), NetshareError::UnknownHost),
        (Err(Unresolved::Unanswered), NetshareError::Unreachable),
        (
            Ok(IpAddr::V4(Ipv4Addr::BROADCAST)),
            NetshareError::UnknownHost,
        ),
    ] {
        let other = Connect {
            credentials: Credentials {
                user: "bob",
                ..unnamed.credentials
            },
            ..unnamed
        };
        let ticket = bench.call(alice(), &shares(other));
        let (conn, _) = bench.world().asked.pop().expect("asked");
        bench
            .front
            .resolved(bench.now, conn, resolution as Resolution);
        bench.run();
        assert_eq!(bench.refusal(ticket), refusal, "{resolution:?}");
    }
}

#[test]
fn a_lookup_answered_after_its_connection_ended_reaches_nothing() {
    let mut bench = Bench::new(nas());
    let unnamed = Connect {
        endpoint: Endpoint {
            address: None,
            ..connect(PASSWORD).endpoint
        },
        ..connect(PASSWORD)
    };
    let first = bench.call(alice(), &shares(unnamed));
    let (ended, _) = bench.world().asked.pop().expect("asked");
    bench
        .front
        .resolved(bench.now, ended, Err(Unresolved::NoSuchHost));
    bench.run();
    assert_eq!(bench.refusal(first), NetshareError::UnknownHost);

    let second = bench.call(alice(), &shares(unnamed));
    let (live, _) = bench.world().asked.pop().expect("asked");
    assert_eq!(live.index(), ended.index(), "the slot is taken again");
    assert_ne!(live, ended);
    bench.front.resolved(bench.now, ended, Ok(NAS));
    bench.run();
    assert!(
        bench.unanswered(second),
        "the ended connection's answer reached its successor"
    );
    bench.front.resolved(bench.now, live, Ok(NAS));
    bench.run();
    assert!(matches!(
        NetshareReply::decode(&bench.reply(second)),
        Ok(NetshareReply::Shares(_))
    ));
}

#[test]
fn a_server_nothing_answers_at_is_unreachable() {
    let mut bench = Bench::new(nas());
    let away = Connect {
        endpoint: Endpoint {
            address: Some(ELSEWHERE),
            ..connect(PASSWORD).endpoint
        },
        ..connect(PASSWORD)
    };
    let ticket = bench.call(alice(), &shares(away));
    assert_eq!(bench.refusal(ticket), NetshareError::Unreachable);
    assert_eq!(bench.world().live(), 0, "the socket is closed");
}

#[test]
fn a_wrong_password_is_the_server_s_rejection_and_is_recorded() {
    let mut bench = Bench::new(nas());
    let ticket = bench.call(alice(), &shares(connect("not the password")));
    assert_eq!(
        bench.refusal(ticket),
        NetshareError::Rejected(Rejection::Credentials)
    );
    assert!(bench.logged(AUTHENTICATION_REJECTED));
    assert_eq!(bench.world().live(), 0);
}

#[test]
fn a_listing_and_a_mount_with_the_same_account_share_one_connection() {
    let mut bench = Bench::new(nas());
    let listing = bench.call(alice(), &shares(connect(PASSWORD)));
    let _ = bench.reply(listing);
    let ticket = bench.call(alice(), &mount(connect(PASSWORD), DISK_SHARE));
    let (pin, offer) = bench.mounted(ticket);
    assert_eq!(pin.server_guid, SERVER_GUID);
    assert_eq!(bench.world().opened(), 1, "one connection served both");
    assert_eq!(
        bench.world().offers,
        [(offer, ALICE, String::from("//nas.example/share"))]
    );
    assert_eq!(bench.world().watched, [session(1)]);
    assert!(bench.logged(MOUNTED));

    let listed = bench.call(alice(), &NetshareRequest::Mounts);
    let reply = bench.reply(listed);
    let NetshareReply::Mounts(mounts) = NetshareReply::decode(&reply).expect("a reply") else {
        panic!("not mounts");
    };
    let entries: Vec<_> = mounts
        .iter()
        .map(|entry| (entry.offer, entry.host, entry.share, entry.state))
        .collect();
    assert_eq!(
        entries,
        [(offer, "nas.example", DISK_SHARE, MountState::Serving)]
    );
}

#[test]
fn a_call_naming_another_password_gets_a_connection_of_its_own() {
    let mut bench = Bench::new(nas());
    let first = bench.call(alice(), &shares(connect(PASSWORD)));
    let _ = bench.reply(first);
    let second = bench.call(alice(), &shares(connect("not the password")));
    assert_eq!(
        bench.refusal(second),
        NetshareError::Rejected(Rejection::Credentials)
    );
    assert_eq!(bench.world().opened(), 2, "no ride on another's session");
}

#[test]
fn a_remembered_pin_that_differs_is_refused_before_any_credential_leaves() {
    let mut bench = Bench::new(nas());
    let pinned = Connect {
        pin: Some(Pin {
            server_guid: *b"another server!!",
            address: NAS,
        }),
        ..connect(PASSWORD)
    };
    let ticket = bench.call(alice(), &shares(pinned));
    assert_eq!(
        bench.refusal(ticket),
        NetshareError::PinMismatch(Pin {
            server_guid: SERVER_GUID,
            address: NAS
        })
    );
    assert!(bench.logged(PIN_MISMATCH));
    assert_eq!(
        bench.world().authenticated,
        [false],
        "closed unauthenticated"
    );
}

#[test]
fn a_mount_needs_a_login_session_to_end_with() {
    let mut bench = Bench::new(nas());
    let sessionless = Caller {
        uid: ALICE,
        session: None,
    };
    let ticket = bench.call(sessionless, &mount(connect(PASSWORD), DISK_SHARE));
    assert_eq!(bench.refusal(ticket), NetshareError::NoSession);
    assert_eq!(bench.world().opened(), 0);
}

#[test]
fn a_share_mounted_twice_is_refused_as_mounted_and_its_tree_let_go() {
    let mut bench = Bench::new(nas());
    let first = bench.call(alice(), &mount(connect(PASSWORD), DISK_SHARE));
    let _ = bench.mounted(first);
    let again = bench.call(alice(), &mount(connect(PASSWORD), "SHARE"));
    assert_eq!(bench.refusal(again), NetshareError::AlreadyMounted);
    assert_eq!(bench.world().offers.len(), 1);
}

#[test]
fn a_share_the_server_does_not_offer_is_no_such_share() {
    let mut bench = Bench::new(nas());
    let ticket = bench.call(alice(), &mount(connect(PASSWORD), "nosuch"));
    assert_eq!(bench.refusal(ticket), NetshareError::NoSuchShare);
    assert!(bench.world().offers.is_empty());
}

#[test]
fn only_its_owner_unmounts_a_mount() {
    let mut bench = Bench::new(nas());
    let ticket = bench.call(alice(), &mount(connect(PASSWORD), DISK_SHARE));
    let (_, offer) = bench.mounted(ticket);
    let bob = Caller {
        uid: BOB,
        session: Some(session(2)),
    };
    let theirs = bench.call(bob, &NetshareRequest::Unmount { offer });
    assert_eq!(bench.refusal(theirs), NetshareError::NoSuchMount);
    assert!(bench.world().withdrawn.is_empty());
    let own = bench.call(alice(), &NetshareRequest::Unmount { offer });
    let reply = bench.reply(own);
    assert_eq!(NetshareReply::decode(&reply), Ok(NetshareReply::Unmounted));
    assert_eq!(bench.world().withdrawn, [offer]);
    assert_eq!(bench.world().unwatched, [session(1)]);
    assert!(bench.logged(UNMOUNTED));
}

#[test]
fn the_end_of_a_login_session_withdraws_its_mounts_and_no_others() {
    let mut bench = Bench::new(nas());
    let first = bench.call(alice(), &mount(connect(PASSWORD), DISK_SHARE));
    let (_, kept) = bench.mounted(first);
    let bob = Caller {
        uid: BOB,
        session: Some(session(7)),
    };
    let second = bench.call(bob, &mount(connect(PASSWORD), DISK_SHARE));
    let (_, gone) = bench.mounted(second);
    bench.front.peer_exited(bench.now, session(7));
    bench.run();
    assert_eq!(bench.world().withdrawn, [gone]);
    assert_eq!(bench.world().unwatched, [session(7)]);
    let listed = bench.call(alice(), &NetshareRequest::Mounts);
    let reply = bench.reply(listed);
    let NetshareReply::Mounts(mounts) = NetshareReply::decode(&reply).expect("a reply") else {
        panic!("not mounts");
    };
    assert_eq!(
        mounts.iter().map(|entry| entry.offer).collect::<Vec<_>>(),
        [kept]
    );
    let theirs = bench.call(bob, &NetshareRequest::Mounts);
    let reply = bench.reply(theirs);
    let NetshareReply::Mounts(mounts) = NetshareReply::decode(&reply).expect("a reply") else {
        panic!("not mounts");
    };
    assert!(mounts.is_empty());
}

#[test]
fn a_mount_whose_session_ended_first_is_withdrawn_at_once() {
    let mut world = nas();
    world.ended.push(session(1));
    let mut bench = Bench::new(world);
    let ticket = bench.call(alice(), &mount(connect(PASSWORD), DISK_SHARE));
    assert_eq!(bench.refusal(ticket), NetshareError::NoSession);
    let offered = bench.world().offers[0].0;
    assert_eq!(bench.world().withdrawn, [offered]);
}

#[test]
fn a_call_that_outlives_its_deadline_is_answered_unreachable() {
    let mut bench = Bench::new(nas());
    let silent = Connect {
        endpoint: Endpoint {
            address: Some(SILENT),
            ..connect(PASSWORD).endpoint
        },
        ..connect(PASSWORD)
    };
    let ticket = bench.call(alice(), &shares(silent));
    assert!(bench.unanswered(ticket));
    assert_eq!(
        bench.front.next_deadline(),
        Some(bench.now + CALL_DEADLINE_NS)
    );
    bench.pass(CALL_DEADLINE_NS - 1);
    assert!(bench.unanswered(ticket));
    bench.pass(1);
    assert_eq!(bench.refusal(ticket), NetshareError::Unreachable);
    assert_eq!(bench.world().live(), 0, "the connection was let go");
    assert_eq!(bench.front.next_deadline(), None);
}

#[test]
fn a_connection_nothing_needs_lingers_then_closes() {
    let mut bench = Bench::new(nas());
    let ticket = bench.call(alice(), &shares(connect(PASSWORD)));
    let _ = bench.reply(ticket);
    assert_eq!(bench.world().live(), 1);
    bench.pass(LINGER_NS - 1);
    assert_eq!(bench.world().live(), 1);
    bench.pass(1);
    assert_eq!(bench.world().live(), 0);
    assert_eq!(bench.world().authenticated, [true]);
}

fn silent(user: &str) -> Connect<'_> {
    Connect {
        endpoint: Endpoint {
            address: Some(SILENT),
            ..connect(PASSWORD).endpoint
        },
        credentials: Credentials {
            user,
            ..connect(PASSWORD).credentials
        },
        ..connect(PASSWORD)
    }
}

#[test]
fn a_principal_opens_no_more_connections_than_its_bound() {
    let mut bench = Bench::new(nas());
    let users: Vec<String> = (0..=CONNECTIONS_MAX)
        .map(|at| alloc::format!("user{at}"))
        .collect();
    for user in &users[..CONNECTIONS_MAX] {
        let ticket = bench.call(alice(), &shares(silent(user)));
        assert!(bench.unanswered(ticket));
    }
    assert_eq!(bench.world().opened(), CONNECTIONS_MAX);
    let over = bench.call(alice(), &shares(silent(&users[CONNECTIONS_MAX])));
    assert_eq!(bench.refusal(over), NetshareError::Limit);
    assert_eq!(bench.world().opened(), CONNECTIONS_MAX);
    let bob = Caller {
        uid: BOB,
        session: Some(session(2)),
    };
    let theirs = bench.call(bob, &shares(silent("bob")));
    assert!(
        bench.unanswered(theirs),
        "another principal's bound is its own"
    );
}

#[test]
fn a_principal_has_no_more_calls_waiting_than_its_bound() {
    let mut bench = Bench::new(nas());
    for _ in 0..CALLS_MAX {
        let ticket = bench.call(alice(), &shares(silent("alice")));
        assert!(bench.unanswered(ticket));
    }
    assert_eq!(
        bench.world().opened(),
        1,
        "every call shares one connection"
    );
    let over = bench.call(alice(), &shares(silent("alice")));
    assert_eq!(bench.refusal(over), NetshareError::Limit);
}

#[test]
fn an_unreadable_request_is_answered_unreadable_and_recorded() {
    let mut bench = Bench::new(nas());
    let mut request = alloc::vec![0x55; 12];
    bench.front.call(bench.now, 9, alice(), &mut request);
    assert!(request.iter().all(|&byte| byte == 0), "erased");
    let reply = bench.reply(9);
    assert_eq!(NetshareReply::decode(&reply), Ok(NetshareReply::Unreadable));
    assert!(bench.logged(REQUEST_UNREADABLE));
}

#[test]
fn a_worker_that_writes_what_no_worker_writes_costs_only_its_connection() {
    let mut world = nas();
    world.hands.push_back(Hand::Liar);
    let mut bench = Bench::new(world);
    let lied = bench.call(alice(), &shares(connect(PASSWORD)));
    assert_eq!(bench.refusal(lied), NetshareError::Protocol);
    assert_eq!(bench.world().live(), 0);
    let honest = bench.call(alice(), &shares(connect(PASSWORD)));
    let reply = bench.reply(honest);
    assert!(matches!(
        NetshareReply::decode(&reply),
        Ok(NetshareReply::Shares(_))
    ));
}

/// A worker reporting, for each report `swap` names, the one it returns.
fn swapping(swap: fn(&Report<'_>) -> Option<Report<'static>>) -> Hand {
    let perturb: Perturb = Box::new(move |frame, out| {
        let swapped = Report::decode(frame).ok().as_ref().and_then(swap);
        match swapped {
            Some(report) => {
                let mut bytes = Vec::new();
                report.encode(&mut bytes).expect("encodes");
                out.frame(&bytes)
            }
            None => out.frame(frame),
        }
    });
    Hand::Chaos(Box::default(), perturb)
}

#[test]
fn a_call_answered_with_a_report_for_another_kind_of_call_is_refused_and_costs_its_connection() {
    let mut world = nas();
    world.hands.push_back(swapping(|report| match *report {
        Report::Listed { request, .. } => Some(Report::Connected {
            request,
            tree: 1,
            kind: ShareType::Disk,
        }),
        _ => None,
    }));
    world.hands.push_back(swapping(|report| match *report {
        Report::Connected { request, .. } => Some(Report::Listed {
            request,
            hidden: 0,
            unnamed: 0,
            unlisted: 0,
        }),
        _ => None,
    }));
    world.hands.push_back(swapping(|report| match *report {
        Report::Connected { request, .. } => Some(Report::Share {
            request,
            name: "share",
            special: false,
            remark: "",
        }),
        _ => None,
    }));
    let mut bench = Bench::new(world);
    let listing = bench.call(alice(), &shares(connect(PASSWORD)));
    assert_eq!(
        bench.refusal(listing),
        NetshareError::Protocol,
        "a tree for a listing"
    );
    let mounting = bench.call(alice(), &mount(connect(PASSWORD), DISK_SHARE));
    assert_eq!(
        bench.refusal(mounting),
        NetshareError::Protocol,
        "a listing for a mount"
    );
    let sharing = bench.call(alice(), &mount(connect(PASSWORD), DISK_SHARE));
    assert_eq!(
        bench.refusal(sharing),
        NetshareError::Protocol,
        "a share for a mount"
    );
    assert_eq!(bench.world().live(), 0);
    assert!(bench.logged(CONNECTION_ENDED));
}
