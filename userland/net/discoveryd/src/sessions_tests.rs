//! Unit tests for the client session table.

use super::{
    Sessions, Subscribed, ACCOUNT_QUEUE_BYTES, MAX_SESSIONS, QUEUED_BYTES_MAX, RETAINED_QUEUE,
};
use alloc::vec;
use alloc::vec::Vec;
use tairix_abi::discovery_ipc::{
    Answer, Change, CollectReply, Entry, COLLECT_MIN_CAPACITY, DISCOVERY_MAX_REPLY,
    REQUESTS_PER_ACCOUNT, SESSIONS_PER_ACCOUNT, SESSION_QUEUE_BYTES,
};
use tairix_abi::net_ipc::IF_NAME_LEN;
use tairix_abi::{Errno, ProcId};

fn iface(name: &[u8]) -> [u8; IF_NAME_LEN] {
    let mut out = [0u8; IF_NAME_LEN];
    out[..name.len()].copy_from_slice(name);
    out
}

fn owner(n: u8) -> ProcId {
    ProcId::from_raw([n; 16])
}

fn text(octets: &[u8]) -> Entry<'_> {
    Entry::Answer {
        request: 0,
        interface: iface(b"eth0"),
        change: Change::Added,
        ttl: 4500,
        answer: Answer::Text { octets },
    }
}

/// A session of owner 1 with one request.
fn one() -> (Sessions, u32, u32) {
    let mut sessions = Sessions::default();
    let id = sessions.open(owner(1), 1000, 77).unwrap();
    let request = sessions
        .add_request(id, owner(1), Subscribed::new())
        .unwrap();
    (sessions, id, request)
}

fn collect(sessions: &mut Sessions, id: u32) -> (usize, bool) {
    let mut reply = vec![0u8; DISCOVERY_MAX_REPLY];
    let len = sessions
        .collect(id, owner(1), DISCOVERY_MAX_REPLY, &mut reply)
        .unwrap();
    let parsed = CollectReply::parse(&reply[..len]).unwrap();
    (parsed.len(), parsed.more)
}

#[test]
fn a_session_belongs_to_whoever_opened_it() {
    let (mut sessions, id, _) = one();
    assert_eq!(
        sessions.owned(id, owner(2)).map(|_| ()),
        Err(Errno::NotFound)
    );
    assert_eq!(
        sessions.close(id, owner(2)).map(|_| ()),
        Err(Errno::NotFound)
    );
    assert_eq!(
        sessions.add_request(id, owner(2), Subscribed::new()),
        Err(Errno::NotFound)
    );
    assert!(sessions.holds_any(owner(1)));
    assert!(!sessions.holds_any(owner(2)));
    assert_eq!(
        sessions
            .close(id, owner(1))
            .map(|session| session.requests().count()),
        Ok(1)
    );
    assert!(!sessions.holds_any(owner(1)));
}

#[test]
fn an_account_holds_a_bounded_number_of_sessions_and_so_does_the_service() {
    let mut sessions = Sessions::default();
    for n in 0..SESSIONS_PER_ACCOUNT {
        let who = owner(u8::try_from(n).unwrap());
        assert!(sessions.open(who, 1000, 1).is_ok());
    }
    assert_eq!(sessions.open(owner(99), 1000, 1), Err(Errno::LimitExceeded));
    assert!(sessions.open(owner(99), 1001, 1).is_ok(), "another account");
    let mut all = Sessions::default();
    for n in 0..MAX_SESSIONS {
        let uid = u32::try_from(n).unwrap();
        assert!(all.open(owner(1), uid, 1).is_ok());
    }
    assert_eq!(all.open(owner(1), 5_000_000, 1), Err(Errno::LimitExceeded));
}

#[test]
fn one_session_holds_an_inventorys_worth_of_requests() {
    // What a map of a busy segment keeps open in its one session.
    let (mut sessions, id, _) = one();
    for _ in 1..600 {
        assert!(sessions
            .add_request(id, owner(1), Subscribed::new())
            .is_ok());
    }
    assert_eq!(sessions.has_room(id, owner(1)), Ok(true));
}

#[test]
fn an_account_holds_a_bounded_number_of_requests_across_its_sessions() {
    let mut sessions = Sessions::default();
    let first = sessions.open(owner(1), 1000, 1).unwrap();
    let second = sessions.open(owner(2), 1000, 2).unwrap();
    for n in 0..REQUESTS_PER_ACCOUNT {
        let session = if n % 2 == 0 { first } else { second };
        let who = if n % 2 == 0 { owner(1) } else { owner(2) };
        assert!(sessions
            .add_request(session, who, Subscribed::new())
            .is_ok());
    }
    for (session, who) in [(first, owner(1)), (second, owner(2))] {
        assert_eq!(sessions.has_room(session, who), Ok(false));
        assert_eq!(
            sessions.add_request(session, who, Subscribed::new()),
            Err(Errno::LimitExceeded),
            "opening another session cannot multiply the bound"
        );
    }
    let other = sessions.open(owner(3), 2000, 3).unwrap();
    assert!(
        sessions
            .add_request(other, owner(3), Subscribed::new())
            .is_ok(),
        "another account is unaffected"
    );
}

#[test]
fn what_an_account_holds_is_given_back_as_it_stops_and_closes() {
    let mut sessions = Sessions::default();
    let id = sessions.open(owner(1), 1000, 1).unwrap();
    let mut requests = Vec::new();
    for _ in 0..REQUESTS_PER_ACCOUNT {
        requests.push(
            sessions
                .add_request(id, owner(1), Subscribed::new())
                .unwrap(),
        );
    }
    sessions.remove_request(id, owner(1), requests[0]).unwrap();
    assert_eq!(sessions.has_room(id, owner(1)), Ok(true));
    sessions.close(id, owner(1)).unwrap();
    let again = sessions.open(owner(1), 1000, 1).unwrap();
    for _ in 0..REQUESTS_PER_ACCOUNT {
        assert!(sessions
            .add_request(again, owner(1), Subscribed::new())
            .is_ok());
    }
}

#[test]
fn a_session_id_that_wraps_never_names_a_live_session() {
    let mut sessions = Sessions {
        next_id: u32::MAX - 1,
        ..Sessions::default()
    };
    let ids = [0, 1, 2].map(|n| sessions.open(owner(n), 1000, 1).unwrap());
    assert_eq!(ids, [u32::MAX - 1, u32::MAX, 0]);
    sessions.next_id = u32::MAX - 1;
    assert_eq!(
        sessions.open(owner(3), 1000, 1),
        Ok(1),
        "every live id skipped"
    );
    for (n, id) in ids.into_iter().enumerate() {
        let who = owner(u8::try_from(n).unwrap());
        assert!(sessions.owned(id, who).is_ok(), "nothing was overwritten");
    }
}

#[test]
fn a_request_id_that_wraps_never_names_a_live_request() {
    let (mut sessions, id, first) = one();
    sessions.owned(id, owner(1)).unwrap().next_request = first;
    let second = sessions
        .add_request(id, owner(1), Subscribed::new())
        .unwrap();
    assert_ne!(second, first);
    assert_eq!(sessions.owned(id, owner(1)).unwrap().requests().count(), 2);
}

#[test]
fn a_session_is_rung_once_until_it_has_taken_everything() {
    let (mut sessions, id, request) = one();
    assert_eq!(sessions.queue(id, request, &text(b"\x03a=1")), Some(77));
    assert_eq!(
        sessions.queue(id, request, &text(b"\x03b=2")),
        None,
        "already rung"
    );
    assert_eq!(collect(&mut sessions, id), (2, false));
    assert_eq!(
        sessions.queue(id, request, &text(b"\x03c=3")),
        Some(77),
        "quiet again"
    );
}

#[test]
fn an_entry_is_rewritten_to_name_its_request() {
    let (mut sessions, id, request) = one();
    sessions.queue(id, request, &text(b"\x03a=1"));
    let mut reply = vec![0u8; DISCOVERY_MAX_REPLY];
    let len = sessions
        .collect(id, owner(1), DISCOVERY_MAX_REPLY, &mut reply)
        .unwrap();
    let parsed = CollectReply::parse(&reply[..len]).unwrap();
    let entry = parsed.entries().next().unwrap();
    assert!(matches!(entry, Entry::Answer { request: r, .. } if r == request));
}

/// Drain session `id` of owner 1, returning the answers and losses taken.
fn drain(sessions: &mut Sessions, id: u32, request: u32) -> (usize, usize) {
    let mut taken = 0;
    let mut lost = 0;
    loop {
        let mut reply = vec![0u8; DISCOVERY_MAX_REPLY];
        let len = sessions
            .collect(id, owner(1), DISCOVERY_MAX_REPLY, &mut reply)
            .unwrap();
        let parsed = CollectReply::parse(&reply[..len]).unwrap();
        for entry in parsed.entries() {
            if let Entry::Lost { request: r } = entry {
                assert_eq!(r, request);
                lost += 1;
            } else {
                assert_eq!(lost, 0, "the loss comes after what was queued before it");
                taken += 1;
            }
        }
        if !parsed.more {
            return (taken, lost);
        }
    }
}

#[test]
fn a_full_queue_drops_the_requests_updates_and_says_so_after_the_rest() {
    let (mut sessions, id, request) = one();
    let big = [0x10u8; 400];
    let mut queued = 0;
    while sessions.owned(id, owner(1)).is_ok() {
        sessions.queue(id, request, &text(&big));
        queued += 1;
        if queued * (big.len() + 30) > SESSION_QUEUE_BYTES + 1000 {
            break;
        }
    }
    let (taken, lost) = drain(&mut sessions, id, request);
    assert!(taken > 0 && taken < queued);
    assert_eq!(lost, 1, "told once");
    assert_eq!(
        sessions.queue(id, request, &text(b"\x01x")),
        None,
        "nothing more is queued for it"
    );
    assert_eq!(collect(&mut sessions, id), (0, false));
}

#[test]
fn what_every_queue_holds_together_is_bounded() {
    let (mut sessions, id, request) = one();
    sessions.queue(id, request, &text(b"\x03a=1"));
    // Another reader's backlog fills the service to its ceiling.
    sessions.queued = QUEUED_BYTES_MAX;
    sessions.queue(id, request, &text(b"\x03b=2"));
    let (taken, lost) = drain(&mut sessions, id, request);
    assert_eq!((taken, lost), (1, 1));
}

#[test]
fn one_account_holds_its_share_of_the_queue_and_no_more() {
    let (mut sessions, id, request) = one();
    // The table charges by account, so one process may stand in for both.
    let other = sessions.open(owner(1), 2000, 9).unwrap();
    let theirs = sessions
        .add_request(other, owner(1), Subscribed::new())
        .unwrap();
    sessions.queue(id, request, &text(b"\x03a=1"));
    // The account's other sessions hold the rest of its share.
    sessions.accounts.get_mut(&1000).unwrap().queued = ACCOUNT_QUEUE_BYTES;
    sessions.queue(id, request, &text(b"\x03b=2"));
    assert_eq!(drain(&mut sessions, id, request), (1, 1));
    sessions.queue(other, theirs, &text(b"\x03c=3"));
    assert_eq!(
        drain(&mut sessions, other, theirs),
        (1, 0),
        "another account keeps its own share"
    );
}

#[test]
fn a_drained_queue_gives_back_its_storage() {
    let (mut sessions, id, request) = one();
    let big = [0x10u8; 400];
    for _ in 0..64 {
        sessions.queue(id, request, &text(&big));
    }
    assert!(sessions.owned(id, owner(1)).unwrap().queue.capacity() > RETAINED_QUEUE);
    drain(&mut sessions, id, request);
    let session = sessions.owned(id, owner(1)).unwrap();
    assert!(session.queue.capacity() <= RETAINED_QUEUE);
    assert_eq!(sessions.queued, 0, "every byte is accounted back");
    assert_eq!(
        sessions.accounts.get(&1000).map(|account| account.queued),
        Some(0)
    );
}

#[test]
fn a_collect_names_a_capacity_that_can_always_hold_one_entry() {
    let (mut sessions, id, _) = one();
    let mut reply = vec![0u8; DISCOVERY_MAX_REPLY];
    assert_eq!(
        sessions.collect(id, owner(1), COLLECT_MIN_CAPACITY - 1, &mut reply),
        Err(Errno::OutOfRange)
    );
    assert_eq!(
        sessions.collect(
            id,
            owner(1),
            COLLECT_MIN_CAPACITY,
            &mut reply[..COLLECT_MIN_CAPACITY - 1]
        ),
        Err(Errno::BufferTooSmall)
    );
    let len = sessions
        .collect(id, owner(1), COLLECT_MIN_CAPACITY, &mut reply)
        .unwrap();
    assert!(CollectReply::parse(&reply[..len]).unwrap().is_empty());
}

#[test]
fn a_doorbell_refused_for_room_is_owed_until_it_is_posted() {
    let (mut sessions, id, request) = one();
    assert_eq!(sessions.queue(id, request, &text(b"\x01x")), Some(77));
    sessions.rang(id, Err(Errno::WouldBlock));
    assert_eq!(sessions.owed_rings().collect::<Vec<_>>(), [77]);
    assert_eq!(
        sessions.queue(id, request, &text(b"\x01y")),
        None,
        "owed, not rung"
    );
    let mut rung = Vec::new();
    sessions.retry_owed(&mut |port, session| {
        rung.push((session, port));
        Err(Errno::WouldBlock)
    });
    assert_eq!(rung, [(id, 77)]);
    assert_eq!(
        sessions.owed_rings().collect::<Vec<_>>(),
        [77],
        "refused again, so still owed"
    );
    sessions.retry_owed(&mut |port, session| {
        rung.push((session, port));
        Ok(())
    });
    assert_eq!(rung, [(id, 77), (id, 77)]);
    assert_eq!(sessions.owed_rings().count(), 0);
    sessions.retry_owed(&mut |_, _| panic!("nothing is owed"));
}

#[test]
fn a_closed_sessions_owed_doorbell_is_forgotten() {
    let (mut sessions, id, request) = one();
    sessions.queue(id, request, &text(b"\x01x"));
    sessions.rang(id, Err(Errno::WouldBlock));
    sessions.close(id, owner(1)).unwrap();
    assert_eq!(sessions.owed_rings().count(), 0);
    sessions.retry_owed(&mut |_, _| panic!("a closed session is owed nothing"));
}

#[test]
fn an_exited_owners_sessions_all_close() {
    let mut sessions = Sessions::default();
    let a = sessions.open(owner(1), 1000, 1).unwrap();
    let b = sessions.open(owner(1), 1000, 2).unwrap();
    let kept = sessions.open(owner(2), 1000, 3).unwrap();
    let mut closed = Vec::new();
    while let Some(session) = sessions.close_any(owner(1)) {
        closed.push(session.id());
    }
    closed.sort_unstable();
    assert_eq!(closed, [a, b]);
    assert!(sessions.owned(kept, owner(2)).is_ok());
    assert!(!sessions.holds_any(owner(1)));
    assert!(sessions.close_any(owner(1)).is_none());
}

#[test]
fn a_queue_that_is_not_whole_entries_is_dropped_rather_than_read() {
    for bogus in [
        // A length past any entry.
        vec![0xFF, 0xFF, 1, 2, 3],
        // A length past what is queued.
        vec![40, 0, 1, 2, 3],
    ] {
        let (mut sessions, id, _) = one();
        sessions
            .owned(id, owner(1))
            .unwrap()
            .queue
            .extend(bogus.iter().copied());
        assert_eq!(collect(&mut sessions, id), (0, false));
        assert!(sessions.owned(id, owner(1)).unwrap().queue.is_empty());
    }
}
