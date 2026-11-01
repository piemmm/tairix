//! Loom models of [`tairix_kernel_ipc::port::Port`]'s lock-free send fast
//! path against `destroy`, run by `cargo xtask loom`.
//!
//! The invariant under test is:
//!
//! > A send racing with `destroy()` must observe one of two outcomes:
//! > either it observed `OPEN` and the message was enqueued before
//! > `destroy` saw it, *or* it observed `CLOSED` and returned
//! > [`Errno::NotFound`] without enqueueing. The kernel must never
//! > end up in a state where the message was enqueued *after*
//! > `destroy` drained the mailbox (which would constitute a leaked
//! > delivery on a dead port).
//!
//! Each model keeps to two contending threads: loom cannot exhaust a schedule
//! in which two threads spin on a lock whose holder it has preempted, since
//! every spin adds a branch the search must then explore.

#![cfg(loom)]

use loom::sync::Arc;
use loom::thread;

use tairix_abi::{CapabilityId, Errno};
use tairix_caps::CapabilitySet;
use tairix_kernel_ipc::port::{EndpointId, Port};
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
            thread::spawn(move || port.send(&sender_caps, b"x", &DiscardSink))
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
        let first = port.send(&task, b"a", &DiscardSink);
        let second = port.send(&task, b"b", &DiscardSink);
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
