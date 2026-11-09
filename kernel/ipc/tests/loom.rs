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
//! > A freed slot is handed to one thread waiting as its sender, which holds
//! > it: no sibling's send takes it, the next freed slot names the thread
//! > left waiting, and a holder that leaves its slot unused — a woken caller
//! > killed or refused, an owing sender whose next call is neither a send nor
//! > a wait watching the port — hands it on, so a sibling looking for room as
//! > it leaves either finds the slot or is named.
//!
//! > An answer written with the endpoint's lock released lands only on a
//! > call still in service: one withdrawn while its answer is written is
//! > refused `NotFound`, and its caller never finds an answer to it.
//!
//! > A call awaited from a thread other than its poster is either answered
//! > by the time the awaiter takes it, or answered with a wake naming that
//! > thread: naming before taking is what keeps a reply landing between the
//! > two from waking only the poster.
//!
//! > An await abandoned while its call is answered hands the answer to the
//! > reapers whichever lands first: the reply wakes them, or the abandon does.
//!
//! Each model keeps to two contending threads: loom cannot exhaust a schedule
//! in which two threads spin on a lock whose holder it has preempted, since
//! every spin adds a branch the search must then explore.

#![cfg(loom)]

use loom::sync::Arc;
use loom::thread;

use tairix_abi::{CapabilityId, Errno, ProcId, PROC_ID_LEN, RLIMIT_INFINITY};
use tairix_caps::CapabilitySet;
use tairix_kernel_ipc::port::{EndpointId, Port, Room};
use tairix_kernel_ipc::{
    CallEndpoint, CallEndpointLimits, ReceivedCall, RecvCall, ReplyOutcome, RoomWake,
};
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
    .expect("derived")
}

fn make_sender() -> TaskCapabilities {
    TaskCapabilities::derive(
        ProcessId(7),
        UserId(1),
        caps_of(&[CapabilityId::NET_RAW]),
        caps_of(&[CapabilityId::NET_RAW]),
        &DiscardSink,
    )
    .expect("derived")
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
            thread::spawn(move || port.send(&sender_caps, 0, b"x", RLIMIT_INFINITY, &DiscardSink))
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
        let first = port.send(&task, 0, b"a", RLIMIT_INFINITY, &DiscardSink);
        let second = port.send(&task, 0, b"b", RLIMIT_INFINITY, &DiscardSink);
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
        port.send(&sender, 0, b"full", RLIMIT_INFINITY, &DiscardSink)
            .expect("fills the lane");

        let receiver = {
            let port = port.clone();
            thread::spawn(move || {
                let (_, freed) = port.recv().expect("the queued message");
                names(&freed, PARKED)
            })
        };
        port.watch_room(PARKED, instance);
        let room = port.room_for(&sender, PARKED, u64::MAX) == Room::Free;
        let named = receiver.join().expect("receiver joined");

        assert!(
            room || named,
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
        let posted = look_by_posting(&ep, PARKED, &caller);
        let woken = server.join().expect("server joined");

        assert!(
            posted || woken.tasks().any(|task| task == PARKED),
            "a caller refused room was not woken by the answer that made it"
        );
    });
}

/// The second thread waiting as the parked one's process.
const SIBLING: u64 = 0x78;

/// One look of a thread waiting for room in a port: record the wait, then
/// send as itself; a send that lands has used its slot.
fn look_by_sending(port: &Port, waiter: u64, sender: &TaskCapabilities) -> bool {
    port.watch_room(waiter, sender.proc_id());
    port.send(sender, waiter, b"x", RLIMIT_INFINITY, &DiscardSink)
        .is_ok()
}

/// A port whose sender's lane of depth one is full, with `waiters` of the
/// sender's threads waiting for room in it, in order.
fn port_with_waiting(id: u64, waiters: &[u64]) -> (Arc<Port>, ProcId) {
    let port = Arc::new(
        Port::create(
            EndpointId(id),
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
    port.send(&sender, 0, b"full", RLIMIT_INFINITY, &DiscardSink)
        .expect("fills the lane");
    for &waiter in waiters {
        port.watch_room(waiter, instance);
    }
    (port, instance)
}

/// Two threads waiting as one sender for one slot: the receive that frees it
/// wakes one, which holds it whatever its sibling does, and the sibling left
/// waiting is named by the next receive.
#[test]
fn loom_of_two_senders_waiting_for_one_slot_one_is_woken_and_the_other_stays_named() {
    loom::model(|| {
        let (port, instance) = port_with_waiting(7, &[PARKED, SIBLING]);
        let (_, freed) = port.recv().expect("the queued message");
        assert!(names(&freed, PARKED), "one slot wakes one");
        assert!(!names(&freed, SIBLING));

        let sibling = {
            let port = port.clone();
            let caps = make_sender().with_proc_id(instance);
            thread::spawn(move || look_by_sending(&port, SIBLING, &caps))
        };
        let parked = look_by_sending(&port, PARKED, &make_sender().with_proc_id(instance));
        let sibling_sent = sibling.join().expect("sibling joined");

        assert!(parked && !sibling_sent, "the slot is the woken thread's");
        let (_, freed) = port.recv().expect("the woken thread's message");
        assert!(
            names(&freed, SIBLING),
            "the thread left waiting was not named by the next receive"
        );
    });
}

/// One post by `poster`, a thread of `caller`.
fn posted(ep: &CallEndpoint, poster: u64, caller: &TaskCapabilities) -> bool {
    ep.post(
        caller,
        poster,
        b"q",
        u64::MAX,
        RLIMIT_INFINITY,
        &DiscardSink,
    )
    .is_ok()
}

/// One look of a thread blocked in a call for room: it posts once its look
/// finds a slot it holds.
fn look_by_posting(ep: &CallEndpoint, waiter: u64, caller: &TaskCapabilities) -> bool {
    ep.room_or_wait(waiter, caller.process().0) && posted(ep, waiter, caller)
}

/// A call endpoint of depth one whose caller has its depth in service, with
/// `waiters` of the caller's threads waiting for room, in order.
fn endpoint_with_waiting(id: u64, waiters: &[u64]) -> (Arc<CallEndpoint>, ReceivedCall) {
    let ep = Arc::new(
        CallEndpoint::create(
            EndpointId(id),
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
    assert!(posted(&ep, 1, &make_sender()), "fills the depth");
    let RecvCall::Received(call) = ep.recv_call(usize::MAX) else {
        panic!("the posted call is received");
    };
    for &waiter in waiters {
        assert!(!ep.room_or_wait(waiter, 7));
    }
    (ep, call)
}

/// Two threads of one process blocked in a call for one slot: the answer that
/// frees it wakes one, which holds it whatever its sibling does, and the
/// sibling left waiting is named by the next answer.
#[test]
fn loom_of_two_callers_waiting_for_one_slot_one_is_woken_and_the_other_stays_named() {
    loom::model(|| {
        let (ep, call) = endpoint_with_waiting(8, &[PARKED, SIBLING]);
        let woken = ep.reply(call.ticket, b"r", &DiscardSink).expect("replied");
        assert!(woken.tasks().any(|task| task == PARKED), "one wake");
        assert!(!woken.tasks().any(|task| task == SIBLING));

        let sibling = {
            let ep = ep.clone();
            thread::spawn(move || look_by_posting(&ep, SIBLING, &make_sender()))
        };
        let parked = look_by_posting(&ep, PARKED, &make_sender());
        let sibling_posted = sibling.join().expect("sibling joined");

        assert!(parked && !sibling_posted, "the slot is the woken thread's");
        let RecvCall::Received(next) = ep.recv_call(usize::MAX) else {
            panic!("the woken thread's call is received");
        };
        let answered = ep.reply(next.ticket, b"r", &DiscardSink).expect("replied");
        assert!(
            answered.tasks().any(|task| task == SIBLING),
            "the thread left waiting was not named by the next answer"
        );
    });
}

/// A caller handed a slot that leaves its wait without posting — killed, or
/// its post refused — hands the slot on: a sibling looking for room as it
/// leaves either finds the slot or is named by the hand-on.
#[test]
fn loom_a_designated_caller_leaving_unused_hands_the_slot_on() {
    loom::model(|| {
        let (ep, call) = endpoint_with_waiting(9, &[PARKED]);
        drop(ep.reply(call.ticket, b"r", &DiscardSink).expect("replied"));
        let sibling = {
            let ep = ep.clone();
            thread::spawn(move || ep.room_or_wait(SIBLING, 7))
        };
        let handed = ep.leave_room_wait(PARKED);
        let found = sibling.join().expect("sibling joined");
        assert!(
            found || names(&handed, SIBLING),
            "a slot left unused stranded the sibling waiting for it"
        );
    });
}

/// An owing sender whose next call is neither the send it owes nor a wait
/// watching the port lets its slot go, and it passes on: a sibling looking for
/// room as it lets go either finds the slot or is named by the hand-on.
#[test]
fn loom_an_owing_sender_released_by_its_next_wait_hands_the_slot_on() {
    loom::model(|| {
        let (port, instance) = port_with_waiting(10, &[PARKED]);
        drop(port.recv().expect("the queued message"));
        assert!(port.own_room(PARKED), "its wait reported the room ready");
        let sibling = {
            let port = port.clone();
            let caps = make_sender().with_proc_id(instance);
            thread::spawn(move || {
                port.watch_room(SIBLING, instance);
                port.room_for(&caps, SIBLING, u64::MAX) == Room::Free
            })
        };
        let handed = port.leave_room(PARKED);
        let found = sibling.join().expect("sibling joined");
        assert!(
            found || names(&handed, SIBLING),
            "a slot let go of stranded the sibling waiting for it"
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

/// The thread recorded watching for the caller's answers.
const WATCHER: u64 = 0x77;

/// An await abandoned while the call is answered: whichever lands first, the
/// answer reaches the reapers — the reply wakes the watcher if the abandon
/// came first, the abandon wakes it if the reply did — and the answer is no
/// longer held for the awaiter that left.
#[test]
fn loom_an_abandoned_await_never_strands_the_answer() {
    loom::model(|| {
        let ep = Arc::new(
            CallEndpoint::create(
                EndpointId(8),
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
        assert_eq!(ep.await_by(7, ticket, AWAITER), Ok(u64::MAX));
        ep.watch_replies(WATCHER, 7);
        let RecvCall::Received(call) = ep.recv_call(usize::MAX) else {
            panic!("the posted call is received");
        };
        let server = {
            let ep = ep.clone();
            thread::spawn(move || ep.reply(call.ticket, b"r", &DiscardSink).expect("replied"))
        };
        let abandoned = ep.abandon_await(7, ticket, AWAITER);
        let replied = server.join().expect("server joined");
        assert!(
            abandoned
                .tasks()
                .chain(replied.tasks())
                .any(|task| task == WATCHER),
            "an abandoned await kept its answer from the reapers"
        );
        assert!(ep.has_ready_reply_for(7, 0), "the answer is a reaper's");
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

/// An answer written with the lock released, racing its call's withdrawal:
/// it lands before the withdrawal, which then takes the answered call, or it
/// is refused, and either way the caller finds nothing left.
#[test]
fn loom_an_answer_written_unlocked_never_lands_on_a_withdrawn_call() {
    loom::model(|| {
        let ep = Arc::new(
            CallEndpoint::create(
                EndpointId(7),
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
        ep.keep_room(call);
        let server = {
            let ep = ep.clone();
            thread::spawn(move || ep.reply(ticket, b"r", &DiscardSink).map(drop))
        };
        let withdrawn = ep.cancel_one(7, ticket).is_some();
        let answered = server.join().expect("server joined");
        assert!(
            withdrawn,
            "answered or not, the call is its caller's to withdraw"
        );
        assert!(matches!(answered, Ok(()) | Err(Errno::NotFound)));
        assert!(matches!(
            ep.take_reply(7, ticket, 0, &DiscardSink),
            ReplyOutcome::Unknown
        ));
        assert_eq!(ep.outstanding(), 0);
    });
}

/// The budget wake's handshake at its own orderings: a waiter publishes its
/// row under the queue's lock, fences, and reads what its process holds; a
/// release gives a charge back, fences, and reads the row count without the
/// lock. The waiter sees the release or the release sees the waiter, so a
/// wait for budget never sleeps through the release it waits for. Modelled
/// here because neither `kernel/core`'s queue nor `kernel/sec`'s charges reach
/// loom's atomics.
#[test]
fn loom_a_budget_wait_never_sleeps_through_its_release() {
    use loom::sync::atomic::{fence, AtomicU64, AtomicUsize, Ordering};
    use loom::sync::Mutex;

    loom::model(|| {
        let rows = Arc::new((Mutex::new(0usize), AtomicUsize::new(0)));
        let held = Arc::new(AtomicU64::new(1));
        let waiter = {
            let (rows, held) = (rows.clone(), held.clone());
            thread::spawn(move || {
                {
                    let mut set = rows.0.lock().expect("unpoisoned");
                    *set += 1;
                    rows.1.store(*set, Ordering::Relaxed);
                }
                fence(Ordering::SeqCst);
                held.load(Ordering::Relaxed) == 0
            })
        };
        held.fetch_sub(1, Ordering::Relaxed);
        fence(Ordering::SeqCst);
        let woken = rows.1.load(Ordering::Relaxed) != 0;
        let saw_release = waiter.join().expect("waiter joined");
        assert!(
            saw_release || woken,
            "a wait for budget and its release missed each other"
        );
    });
}
