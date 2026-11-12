//! What a refused allocation leaves where a registry or an endpoint notes the
//! processes it serves: the operation refuses whole, keeps nothing half made,
//! and the next attempt, given room, succeeds.

#![cfg(not(loom))]

use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::Arc;

use tairix_abi::ipc::PortName;
use tairix_abi::{Errno, LimitKind, ProcId, PROC_ID_LEN, RLIMIT_INFINITY};
use tairix_caps::CapabilitySet;
use tairix_fuzzseed::meter::{metered, refusing_after, Metered};
use tairix_kernel_ipc::audit::AuditEvent;
use tairix_kernel_ipc::port::{EndpointId, Port};
use tairix_kernel_ipc::senders::SenderIndex;
use tairix_kernel_ipc::{
    CallEndpoint, CallEndpointLimits, Collector, PortRegistry, RecvCall, ReplyOutcome,
};
use tairix_kernel_sec::captable::{ProcessId, TaskCapabilities};
use tairix_kernel_sec::identity::UserId;
use tairix_log::{set_max_level, Event, Level, Sink};

#[global_allocator]
static ALLOCATOR: Metered = Metered;

/// The ids of the events recorded, in atomics, so a record made while the
/// allocator refuses neither allocates nor takes a lock, which some hosts box
/// on first use.
struct Recorded {
    ids: [AtomicU32; RECORDED],
    count: AtomicUsize,
}

/// The most events one sink records.
const RECORDED: usize = 64;

impl Recorded {
    fn new() -> Self {
        set_max_level(Level::Trace);
        Self {
            ids: [const { AtomicU32::new(0) }; RECORDED],
            count: AtomicUsize::new(0),
        }
    }

    fn holds(&self, event: AuditEvent) -> bool {
        let count = self.count.load(Ordering::Acquire);
        assert!(
            count <= RECORDED,
            "{count} events, more than a sink records"
        );
        self.ids[..count]
            .iter()
            .any(|id| id.load(Ordering::Acquire) == event.id().0)
    }
}

impl Sink for Recorded {
    fn write_event(&self, event: &Event<'_>) {
        let at = self.count.fetch_add(1, Ordering::AcqRel);
        if let Some(slot) = self.ids.get(at) {
            slot.store(event.id.0, Ordering::Release);
        }
    }
}

fn task(process: u64) -> TaskCapabilities {
    let none = CapabilitySet::empty();
    TaskCapabilities::derive(ProcessId(process), UserId(1), none, none, &Recorded::new())
        .expect("derived")
}

/// A sending process instance of its own.
fn sender(byte: u8) -> TaskCapabilities {
    task(u64::from(byte)).with_proc_id(ProcId::from_raw([byte; PROC_ID_LEN]))
}

fn port(id: u64, owner: u64) -> Port {
    Port::create(
        EndpointId(id),
        &task(owner),
        CapabilitySet::empty(),
        CapabilitySet::empty(),
        64,
        4,
        RLIMIT_INFINITY,
        &Recorded::new(),
    )
    .expect("unrestricted port")
}

fn endpoint(id: u64) -> CallEndpoint {
    let limits = CallEndpointLimits {
        max_request: 64,
        max_reply: 64,
        per_sender: 4,
    };
    CallEndpoint::create(
        EndpointId(id),
        &task(1),
        CapabilitySet::empty(),
        CapabilitySet::empty(),
        limits,
        RLIMIT_INFINITY,
        &Recorded::new(),
    )
    .expect("unrestricted endpoint")
}

/// A first registration that cannot make the index of the ports each sender
/// uses binds nothing and hands its port back; given room, it binds.
#[test]
fn a_registration_refused_its_sender_index_binds_nothing() {
    let sink = Recorded::new();
    let mut reg = PortRegistry::new();
    let unbound = port(0x10, 1);
    let mut handed_back = None;
    let refused = refusing_after(0, || {
        reg.register(unbound, &sink).map_err(|(port, err)| {
            handed_back = Some(port);
            err
        })
    });
    assert_eq!(
        refused,
        Err(Errno::OutOfMemory),
        "no room for the index binds nothing"
    );
    let returned = handed_back.expect("the port is handed back");
    assert!(reg.is_empty());
    assert!(sink.holds(AuditEvent::PortRegisterDenied));
    reg.register(returned, &sink)
        .map_err(|(_, err)| err)
        .expect("given room, it binds");
    assert!(reg.contains(EndpointId(0x10)));
}

/// A wait for room whose sender cannot be noted is recorded nowhere, since
/// the sender's end would never find it to forget, and is left to the
/// broadcast the port's next wake carries; given room, the next is noted,
/// and the sender's end forgets it.
#[test]
fn a_room_wait_refused_its_note_records_nothing() {
    let sink = Recorded::new();
    let mut reg = PortRegistry::new();
    reg.register(port(0x11, 1), &sink)
        .map_err(|(_, err)| err)
        .expect("bound");
    let watched = reg.lookup(EndpointId(0x11)).expect("bound");
    let first = ProcId::from_raw([7; PROC_ID_LEN]);
    let second = ProcId::from_raw([8; PROC_ID_LEN]);
    watched.watch_room(0xA, first);
    refusing_after(0, || watched.watch_room(0xB, second));
    assert!(watched.leave_room(0xB).is_empty(), "nothing was recorded");
    watched.watch_room(0xC, second);
    reg.sender_ended(second, &sink);
    let waiting: Vec<u64> = watched.room_wake_everyone().tasks().collect();
    assert_eq!(
        waiting,
        [0xA, 0],
        "its end found the port it waited at, and the unrecorded wait is broadcast to"
    );
}

/// A send whose sender cannot be noted is refused for memory, queueing and
/// charging nothing; given room, the next is noted and delivered.
#[test]
fn a_send_refused_its_note_queues_nothing() {
    let sink = Recorded::new();
    let mut reg = PortRegistry::new();
    reg.register(port(0x12, 1), &sink)
        .map_err(|(_, err)| err)
        .expect("bound");
    let watched = reg.lookup(EndpointId(0x12)).expect("bound");
    let send = |from: &TaskCapabilities| watched.send(from, 0, b"m", RLIMIT_INFINITY, &sink);
    let (early, late) = (sender(7), sender(8));
    send(&early).expect("sent");
    drop(watched.recv().expect("taken"));
    // The payload's copy takes the one allocation granted; noting the sender
    // would take the next.
    assert_eq!(refusing_after(1, || send(&late)), Err(Errno::OutOfMemory));
    assert!(sink.holds(AuditEvent::PayloadAllocFailed));
    assert!(watched.is_empty(), "nothing was queued");
    assert_eq!(late.charges().held(LimitKind::IpcQueueBytes), 0);
    send(&late).expect("given room, sent");
    assert_eq!(watched.len(), 1);
}

/// A registration whose owner cannot be noted as the port's binds nothing
/// and hands its port back; given room, it binds.
#[test]
fn a_registration_refused_its_owners_note_binds_nothing() {
    let sink = Recorded::new();
    let mut reg = PortRegistry::new();
    reg.register(port(0x13, 1), &sink)
        .map_err(|(_, err)| err)
        .expect("bound");
    let unbound = port(0x14, 2);
    let mut handed_back = None;
    let refused = refusing_after(0, || {
        reg.register(unbound, &sink).map_err(|(port, err)| {
            handed_back = Some(port);
            err
        })
    });
    assert_eq!(
        refused,
        Err(Errno::OutOfMemory),
        "no room to note its owner binds nothing"
    );
    assert_eq!(reg.len(), 1);
    assert!(sink.holds(AuditEvent::PortRegisterDenied));
    let returned = handed_back.expect("the port is handed back");
    reg.register(returned, &sink)
        .map_err(|(_, err)| err)
        .expect("given room, it binds");
    assert!(reg.contains(EndpointId(0x14)));
}

/// A registration refused any one of its allocations binds nothing and hands
/// its port back; given them all, it binds.
#[test]
fn a_registration_refused_any_allocation_binds_nothing() {
    let sink = Recorded::new();
    let mut reg = PortRegistry::new();
    let mut offered = port(0x18, 1);
    let mut granted = 0;
    while let Some((returned, err)) = refusing_after(granted, || reg.register(offered, &sink).err())
    {
        assert_eq!(err, Errno::OutOfMemory, "after {granted} grants");
        assert!(reg.is_empty(), "nothing bound after {granted} grants");
        offered = returned;
        granted += 1;
    }
    assert!(granted > 0, "a registration allocates");
    assert!(reg.contains(EndpointId(0x18)));
    assert!(sink.holds(AuditEvent::PortRegisterDenied));
}

/// A name the registry has no room to hold is refused for memory and
/// publishes nothing; given room, it publishes.
#[test]
fn a_name_refused_its_room_publishes_nothing() {
    let sink = Recorded::new();
    let mut reg = PortRegistry::new();
    reg.register(port(0x19, 1), &sink)
        .map_err(|(_, err)| err)
        .expect("bound");
    let name = PortName::from_ascii(b"svc").expect("a valid name");
    assert_eq!(
        refusing_after(0, || reg.publish_name(name, EndpointId(0x19), &sink)),
        Err(Errno::OutOfMemory)
    );
    assert!(sink.holds(AuditEvent::PortNamePublishDenied));
    assert_eq!(reg.resolve(&name), None);
    reg.publish_name(name, EndpointId(0x19), &sink)
        .expect("given room, it publishes");
    assert_eq!(reg.resolve(&name), Some(EndpointId(0x19)));
}

/// A call is given room for what its caller can take, not for the largest
/// answer its endpoint allows.
#[test]
fn a_call_allocates_for_what_its_caller_can_take() {
    let sink = Recorded::new();
    let limits = CallEndpointLimits {
        max_request: 64,
        max_reply: 64 * 1024,
        per_sender: 4,
    };
    let ep = CallEndpoint::create(
        EndpointId(0x1A),
        &task(1),
        CapabilitySet::empty(),
        CapabilitySet::empty(),
        limits,
        RLIMIT_INFINITY,
        &sink,
    )
    .expect("unrestricted endpoint");
    let caller = task(7);
    let (posted, metering) = metered(|| {
        ep.post_within(
            &caller,
            (0, Collector::Poster),
            b"q",
            16,
            u64::MAX,
            RLIMIT_INFINITY,
            &sink,
        )
    });
    posted.expect("posted");
    assert!(metering.largest < 4096, "{metering:?}");
}

/// A post whose caller cannot be noted is refused for memory, queueing,
/// charging and noting nothing; given room, the next is noted and queued.
#[test]
fn a_post_refused_its_note_queues_nothing() {
    let sink = Recorded::new();
    let ep = endpoint(0x15);
    let index = Arc::new(SenderIndex::new());
    ep.note_senders_in(Arc::clone(&index));
    let post = |from: &TaskCapabilities| ep.post(from, 0, b"q", u64::MAX, RLIMIT_INFINITY, &sink);
    let (early, late) = (task(7), task(8));
    let first = post(&early).expect("posted");
    let RecvCall::Received(call) = ep.recv_call(usize::MAX) else {
        panic!("the call is received");
    };
    drop(ep.reply(call.ticket, b"a", &sink).expect("answered"));
    let ReplyOutcome::Ready(_) = ep.take_reply(7, first, 0, &sink) else {
        panic!("the answer is collected");
    };
    // The call's room takes the one allocation granted; noting its caller
    // would take the next.
    assert_eq!(refusing_after(1, || post(&late)), Err(Errno::OutOfMemory));
    assert!(sink.holds(AuditEvent::PayloadAllocFailed));
    assert_eq!(ep.outstanding(), 0, "nothing was queued");
    assert_eq!(late.charges().held(LimitKind::IpcQueueBytes), 0);
    assert!(index.take(8).is_none(), "and no one was noted");
    post(&late).expect("given room, posted");
    assert_eq!(ep.outstanding(), 1);
    let noted = index.take(8).expect("noted");
    assert!(noted.contains(&EndpointId(0x15)));
}

/// A post the kernel has no memory to give a room refuses its own caller at
/// once: nothing is queued or charged, and no server is handed a call
/// without room for its answer.
#[test]
fn a_post_refused_its_room_queues_and_charges_nothing() {
    let sink = Recorded::new();
    let ep = endpoint(0x16);
    let caller = task(7);
    let post = || ep.post(&caller, 0, b"q", u64::MAX, RLIMIT_INFINITY, &sink);
    assert_eq!(refusing_after(0, post), Err(Errno::OutOfMemory));
    assert!(sink.holds(AuditEvent::PayloadAllocFailed));
    assert_eq!(ep.outstanding(), 0, "nothing was queued");
    assert_eq!(caller.charges().held(LimitKind::IpcQueueBytes), 0);
    assert!(matches!(ep.recv_call(usize::MAX), RecvCall::Empty));
    post().expect("given room, posted");
}

/// A reply lands in the room its call was posted with, so an allocator that
/// refuses everything refuses none: through a room handed back, through the
/// received call itself, or through a fill.
#[test]
fn a_reply_under_an_allocator_refusing_every_allocation_still_lands() {
    let sink = Recorded::new();
    let ep = endpoint(0x17);
    let caller = task(7);
    let post = || {
        ep.post(&caller, 0, b"q", u64::MAX, RLIMIT_INFINITY, &sink)
            .expect("posted")
    };
    let receive = || {
        let RecvCall::Received(call) = ep.recv_call(usize::MAX) else {
            panic!("the call is received");
        };
        call
    };
    let kept = post();
    ep.keep_room(receive());
    let woken = refusing_after(0, || ep.reply(kept, b"answer", &sink));
    drop(woken.expect("answered with nothing allocated"));
    let carried = post();
    let call = receive();
    let woken = refusing_after(0, || ep.reply_with(call, b"answer", &sink));
    drop(woken.expect("answered with nothing allocated"));
    let filled = post();
    ep.keep_room(receive());
    let fill = |room: &mut [u8]| {
        room.copy_from_slice(b"answer");
        Ok::<(), Errno>(())
    };
    let woken = refusing_after(0, || ep.reply_from(filled, 6, &sink, fill));
    let answered = woken.expect("filled");
    drop(answered.expect("answered with nothing allocated"));
    for ticket in [kept, carried, filled] {
        let ReplyOutcome::Ready(answer) = ep.take_reply(7, ticket, 0, &sink) else {
            panic!("the answer is collected");
        };
        assert_eq!(answer.as_bytes(), b"answer");
    }
}

/// A watch for a caller's answers the heap has no room to record is left to
/// the broadcast the next answer carries, once, rather than refused: a wait
/// set's wait is never failed for want of the record.
#[test]
fn a_reply_watch_refused_its_record_is_broadcast_to() {
    let sink = Recorded::new();
    let ep = endpoint(0x18);
    let ticket = ep
        .post(&task(7), 0x77, b"q", u64::MAX, RLIMIT_INFINITY, &sink)
        .expect("posted");
    refusing_after(0, || ep.watch_replies(0x5A, 7));
    let RecvCall::Received(call) = ep.recv_call(usize::MAX) else {
        panic!("the call is received");
    };
    let woken = ep.reply(call.ticket, b"a", &sink).expect("answered");
    assert!(
        woken.tasks().any(|task| task == 0),
        "the answer is broadcast"
    );
    let ReplyOutcome::Ready(_) = ep.take_reply(7, ticket, 0, &sink) else {
        panic!("the answer is collected");
    };
    let again = ep
        .post(&task(7), 0x77, b"q", u64::MAX, RLIMIT_INFINITY, &sink)
        .expect("posted");
    let RecvCall::Received(call) = ep.recv_call(usize::MAX) else {
        panic!("the call is received");
    };
    let woken = ep.reply(call.ticket, b"a", &sink).expect("answered");
    assert!(!woken.tasks().any(|task| task == 0), "once");
    drop(ep.take_reply(7, again, 0, &sink));
}
