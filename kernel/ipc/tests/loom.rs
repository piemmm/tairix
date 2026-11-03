//! Loom models of [`tairix_kernel_ipc::port::Port`]'s lock-free send fast
//! path against `destroy`, and of the room wake a port and a call endpoint
//! give a sender parked for room in its own lane, run by `cargo xtask loom`.
//!
//! The invariants under test are:
//!
//! > A send racing with `destroy()` must observe one of two outcomes:
//! > either it observed `OPEN` and the message was enqueued before
//! > `destroy` saw it, *or* it observed `CLOSED` and returned
//! > [`Errno::NotFound`] without enqueueing. The kernel must never
//! > end up in a state where the message was enqueued *after*
//! > `destroy` drained the mailbox (which would constitute a leaked
//! > delivery on a dead port).
//!
//! > A sender that records itself as waiting for room and then finds none
//! > is always named by the wake of the receive, or the collection, that
//! > frees its lane: recording before re-checking is what keeps a slot freed
//! > between the check and the park from being lost.
//!
//! > A call awaited from a thread other than its poster is either answered
//! > by the time the awaiter takes it, or answered with a wake naming that
//! > thread: naming before taking is what keeps a reply landing between the
//! > two from waking only the poster.
//!
//! Each model keeps to two contending threads: loom cannot exhaust a schedule
//! in which two threads spin on a lock whose holder it has preempted, since
//! every spin adds a branch the search must then explore.

#![cfg(loom)]

use loom::sync::Arc;
use loom::thread;

use tairix_abi::{CapabilityId, Errno, ProcId, PROC_ID_LEN, RLIMIT_INFINITY};
use tairix_caps::CapabilitySet;
use tairix_kernel_ipc::port::{EndpointId, Port};
use tairix_kernel_ipc::{CallEndpoint, CallEndpointLimits, RecvCall, ReplyOutcome, RoomWake};
use tairix_kernel_sec::captable::{ProcessId, TaskCapabilities};
use tairix_kernel_sec::identity::UserId;
use tairix_log::{Event, Sink};

/// A do-nothing sink: loom tests are not interested in audit
/// observability, only the lock-free state machine, and `RefCell` is
/// not loom-safe.
struct DiscardSink;

impl Sink for DiscardSink {
    fn write_event(&self, _event: &Event<'_>) {}
}

fn caps_of(items: &[CapabilityId]) -> CapabilitySet {
    let mut s = CapabilitySet::empty();
    for c in items {
        s.insert(*c);
    }
    s
}

fn make_creator() -> TaskCapabilities {
    TaskCapabilities::derive(
        ProcessId(1),
        UserId(1),
        caps_of(&[CapabilityId::IPC_BIND_PRIVILEGED, CapabilityId::NET_RAW]),
        caps_of(&[CapabilityId::IPC_BIND_PRIVILEGED, CapabilityId::NET_RAW]),
        &DiscardSink,
    )
}

fn make_sender() -> TaskCapabilities {
    TaskCapabilities::derive(
        ProcessId(7),
        UserId(1),
        caps_of(&[CapabilityId::NET_RAW]),
        caps_of(&[CapabilityId::NET_RAW]),
        &DiscardSink,
    )
}

#[test]
fn loom_send_versus_destroy_never_leaks() {
    loom::model(|| {
        let port = Arc::new(
            Port::create(
                EndpointId(1),
                &make_creator(),
                caps_of(&[CapabilityId::NET_RAW]),
                CapabilitySet::empty(),
                8,
                4,
                tairix_abi::RLIMIT_INFINITY,
                &DiscardSink,
            )
            .expect("open"),
        );
        let sender_caps = make_sender();

        let sender = {
            let port = port.clone();
            thread::spawn(move || port.send(&sender_caps, b"x", RLIMIT_INFINITY, &DiscardSink))
        };
        let destroyer = {
            let port = port.clone();
            thread::spawn(move || port.destroy(&DiscardSink))
        };

        let send_result = sender.join().expect("sender joined");
        destroyer.join().expect("destroyer joined");

        // Invariant: the port is closed at the end.
        assert!(port.is_closed());

        // Invariant: if the send returned Ok, the mailbox has been
        // drained by destroy() and contains nothing afterwards. If
        // the send was refused with NotFound, the mailbox is also
        // empty. There is no third valid outcome.
        match send_result {
            Ok(()) => {
                assert!(
                    port.recv().is_none(),
                    "destroy must drain the message the send delivered"
                );
            }
            Err(Errno::NotFound) => {
                assert!(port.recv().is_none(), "no message ever enqueued");
            }
            Err(other) => panic!("unexpected send result: {other:?}"),
        }
    });
}

/// Two sends racing a destroy: neither may leave a message the destroy does
/// not drain, and a send that found the port closed is never followed by one
/// that finds it open.
#[test]
fn loom_send_fast_path_observes_closed_state() {
    loom::model(|| {
        let port = Arc::new(
            Port::create(
                EndpointId(2),
                &make_creator(),
                caps_of(&[CapabilityId::NET_RAW]),
                CapabilitySet::empty(),
                8,
                4,
                tairix_abi::RLIMIT_INFINITY,
                &DiscardSink,
            )
            .expect("open"),
        );
        let destroyer = {
            let port = port.clone();
            thread::spawn(move || port.destroy(&DiscardSink))
        };

        let task = make_sender();
        let first = port.send(&task, b"a", RLIMIT_INFINITY, &DiscardSink);
        let second = port.send(&task, b"b", RLIMIT_INFINITY, &DiscardSink);
        destroyer.join().expect("destroyer joined");

        for result in [first, second] {
            assert!(
                matches!(result, Ok(()) | Err(Errno::NotFound)),
                "unexpected send result: {result:?}"
            );
        }
        assert!(
            !(first.is_err() && second.is_ok()),
            "a send found the port open after an earlier one found it closed"
        );
        assert!(
            port.recv().is_none(),
            "destroy drained every delivered message"
        );
        assert!(port.is_closed());
    });
}

/// The scheduler id the parked sender's thread waits as.
const PARKED: u64 = 0x77;

fn names<K>(wake: &RoomWake<K>, task: u64) -> bool {
    wake.tasks().any(|named| named == task)
}

/// A sender parked for room in its full lane, against the receive that frees
/// it: a sender that found no room after recording its wait is always woken.
#[test]
fn loom_a_drain_never_misses_a_sender_parked_for_room() {
    loom::model(|| {
        let port = Arc::new(
            Port::create(
                EndpointId(3),
                &make_creator(),
                caps_of(&[CapabilityId::NET_RAW]),
                CapabilitySet::empty(),
                8,
                1,
                RLIMIT_INFINITY,
                &DiscardSink,
            )
            .expect("open"),
        );
        let instance = ProcId::from_raw([7; PROC_ID_LEN]);
        let sender = make_sender().with_proc_id(instance);
        port.send(&sender, b"full", RLIMIT_INFINITY, &DiscardSink)
            .expect("fills the lane");

        let receiver = {
            let port = port.clone();
            thread::spawn(move || {
                let taken = port.recv().expect("the queued message");
                port.room_wake_for(taken.origin.proc_id())
            })
        };
        port.watch_room(PARKED, instance).expect("recorded");
        let room = port.has_room_for(instance);
        let wake = receiver.join().expect("receiver joined");

        assert!(
            room || names(&wake, PARKED),
            "a sender that found no room was not woken by the drain that made it"
        );
    });
}

/// A thread parked in a blocking call for room at its process's full depth,
/// against the answer that frees it.
#[test]
fn loom_an_answer_never_misses_a_caller_parked_for_room() {
    loom::model(|| {
        let ep = Arc::new(
            CallEndpoint::create(
                EndpointId(4),
                &make_creator(),
                CapabilitySet::empty(),
                CapabilitySet::empty(),
                CallEndpointLimits {
                    max_request: 8,
                    max_reply: 8,
                    per_sender: 1,
                },
                RLIMIT_INFINITY,
                &DiscardSink,
            )
            .expect("open"),
        );
        let caller = make_sender();
        ep.post(&caller, 1, b"q", u64::MAX, RLIMIT_INFINITY, &DiscardSink)
            .expect("fills the depth");
        let RecvCall::Received(call) = ep.recv_call(usize::MAX) else {
            panic!("the posted call is received");
        };

        let server = {
            let ep = ep.clone();
            thread::spawn(move || ep.reply(call.ticket, b"r", &DiscardSink).expect("replied"))
        };
        ep.watch_room(PARKED, 7).expect("recorded");
        let posted = ep.post(
            &caller,
            PARKED,
            b"q",
            u64::MAX,
            RLIMIT_INFINITY,
            &DiscardSink,
        );
        let woken = server.join().expect("server joined");

        assert!(
            posted.is_ok() || woken.tasks().any(|task| task == PARKED),
            "a caller refused room was not woken by the answer that made it"
        );
    });
}

/// The thread awaiting a call another thread of its process posted.
const AWAITER: u64 = 0x99;

#[test]
fn loom_an_awaited_reply_never_misses_its_awaiter() {
    loom::model(|| {
        let ep = Arc::new(
            CallEndpoint::create(
                EndpointId(5),
                &make_creator(),
                CapabilitySet::empty(),
                CapabilitySet::empty(),
                CallEndpointLimits {
                    max_request: 8,
                    max_reply: 8,
                    per_sender: 1,
                },
                RLIMIT_INFINITY,
                &DiscardSink,
            )
            .expect("open"),
        );
        let ticket = ep
            .post(
                &make_sender(),
                1,
                b"q",
                u64::MAX,
                RLIMIT_INFINITY,
                &DiscardSink,
            )
            .expect("posted");
        let RecvCall::Received(call) = ep.recv_call(usize::MAX) else {
            panic!("the posted call is received");
        };
        let server = {
            let ep = ep.clone();
            thread::spawn(move || ep.reply(call.ticket, b"r", &DiscardSink).expect("replied"))
        };
        assert_eq!(ep.await_by(7, ticket, AWAITER), Ok(u64::MAX));
        let taken = matches!(
            ep.take_reply(7, ticket, 0, &DiscardSink),
            ReplyOutcome::Ready(_)
        );
        let woken = server.join().expect("server joined");
        assert!(
            taken || woken.tasks().any(|task| task == AWAITER),
            "an awaited reply was neither there to take nor woke its awaiter"
        );
    });
}

/// A call withdrawn by one thread of its process while another awaits it:
/// the awaiter either finds it gone or is woken to find it gone.
#[test]
fn loom_a_withdrawn_call_never_strands_its_awaiter() {
    loom::model(|| {
        let ep = Arc::new(
            CallEndpoint::create(
                EndpointId(6),
                &make_creator(),
                CapabilitySet::empty(),
                CapabilitySet::empty(),
                CallEndpointLimits {
                    max_request: 8,
                    max_reply: 8,
                    per_sender: 1,
                },
                RLIMIT_INFINITY,
                &DiscardSink,
            )
            .expect("open"),
        );
        let ticket = ep
            .post(
                &make_sender(),
                1,
                b"q",
                u64::MAX,
                RLIMIT_INFINITY,
                &DiscardSink,
            )
            .expect("posted");
        let canceller = {
            let ep = ep.clone();
            thread::spawn(move || ep.cancel_one(7, ticket))
        };
        assert_eq!(ep.await_by(7, ticket, AWAITER), Ok(u64::MAX));
        let gone = matches!(
            ep.take_reply(7, ticket, 0, &DiscardSink),
            ReplyOutcome::Unknown
        );
        let woken = canceller
            .join()
            .expect("canceller joined")
            .expect("withdrawn");
        assert!(
            gone || woken.tasks().any(|task| task == AWAITER),
            "an awaited call was withdrawn without its awaiter learning of it"
        );
    });
}
