//! What a refused allocation leaves where a registry or a port notes the
//! senders it serves: the operation refuses whole, keeps nothing half made,
//! and the next attempt, given room, succeeds.

#![cfg(not(loom))]

use std::sync::Mutex;

use tairix_abi::{Errno, LimitKind, ProcId, PROC_ID_LEN, RLIMIT_INFINITY};
use tairix_caps::CapabilitySet;
use tairix_fuzzseed::meter::{refusing_after, Metered};
use tairix_kernel_ipc::audit::AuditEvent;
use tairix_kernel_ipc::port::{EndpointId, Port};
use tairix_kernel_ipc::PortRegistry;
use tairix_kernel_sec::captable::{ProcessId, TaskCapabilities};
use tairix_kernel_sec::identity::UserId;
use tairix_log::{set_max_level, Event, Level, Sink};

#[global_allocator]
static ALLOCATOR: Metered = Metered;

/// The ids of the events recorded, kept in room made beforehand so a record
/// made while the allocator refuses allocates nothing.
struct Recorded(Mutex<Vec<u32>>);

impl Recorded {
    fn new() -> Self {
        set_max_level(Level::Trace);
        Self(Mutex::new(Vec::with_capacity(64)))
    }

    fn holds(&self, event: AuditEvent) -> bool {
        self.0.lock().expect("unpoisoned").contains(&event.id().0)
    }
}

impl Sink for Recorded {
    fn write_event(&self, event: &Event<'_>) {
        self.0.lock().expect("unpoisoned").push(event.id.0);
    }
}

fn task(process: u64) -> TaskCapabilities {
    let none = CapabilitySet::empty();
    TaskCapabilities::derive(ProcessId(process), UserId(1), none, none, &Recorded::new())
}

/// A sending process instance of its own.
fn sender(byte: u8) -> TaskCapabilities {
    task(u64::from(byte)).with_proc_id(ProcId::from_raw([byte; PROC_ID_LEN]))
}

fn port(id: u64) -> Port {
    Port::create(
        EndpointId(id),
        &task(1),
        CapabilitySet::empty(),
        CapabilitySet::empty(),
        64,
        4,
        RLIMIT_INFINITY,
        &Recorded::new(),
    )
    .expect("unrestricted port")
}

/// A first registration that cannot make the index of the ports each sender
/// uses binds nothing and hands its port back; given room, it binds.
#[test]
fn a_registration_refused_its_sender_index_binds_nothing() {
    let sink = Recorded::new();
    let mut reg = PortRegistry::new();
    let unbound = port(0x10);
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

/// A wait for room whose sender cannot be noted is refused and recorded
/// nowhere, since the sender's end would never find it to forget; given
/// room, the next is noted, and the sender's end forgets it.
#[test]
fn a_room_wait_refused_its_note_records_nothing() {
    let sink = Recorded::new();
    let mut reg = PortRegistry::new();
    reg.register(port(0x11), &sink)
        .map_err(|(_, err)| err)
        .expect("bound");
    let watched = reg.lookup(EndpointId(0x11)).expect("bound");
    let first = ProcId::from_raw([7; PROC_ID_LEN]);
    let second = ProcId::from_raw([8; PROC_ID_LEN]);
    watched.watch_room(0xA, first).expect("noted");
    assert_eq!(
        refusing_after(0, || watched.watch_room(0xB, second)),
        Err(Errno::OutOfMemory)
    );
    assert!(
        watched.room_wake_for(second).is_empty(),
        "nothing was recorded"
    );
    watched.watch_room(0xB, second).expect("noted");
    reg.sender_ended(second, &sink);
    assert!(
        watched.room_wake_for(second).is_empty(),
        "its end found the port it waited at"
    );
}

/// A send whose sender cannot be noted is refused for memory, queueing and
/// charging nothing; given room, the next is noted and delivered.
#[test]
fn a_send_refused_its_note_queues_nothing() {
    let sink = Recorded::new();
    let mut reg = PortRegistry::new();
    reg.register(port(0x12), &sink)
        .map_err(|(_, err)| err)
        .expect("bound");
    let watched = reg.lookup(EndpointId(0x12)).expect("bound");
    let send = |from: &TaskCapabilities| watched.send(from, b"m", RLIMIT_INFINITY, &sink);
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
