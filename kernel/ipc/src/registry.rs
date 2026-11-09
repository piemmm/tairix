//! The named-port registry: a capability-checked map from
//! [`EndpointId`] to the live [`Port`] bound to it.
//!
//! A [`Port`] on its own is anonymous — [`Port::create`] performs the
//! bind-time capability check and hands back an owned
//! value, but nothing yet lets a *sender* or *receiver* reach it by its
//! [`EndpointId`]. The syscall dispatcher's `ipc_send` / `ipc_recv`
//! handlers need exactly that lookup: given the endpoint number carried
//! in an [`tairix_abi::ipc::IpcMessageHeader`], find the kernel-owned
//! [`Port`] to enqueue into or drain from. [`PortRegistry`] is that map.
//!
//! # Well-known names
//!
//! A numeric [`EndpointId`] is still an opaque handle a binder must
//! already know. So that a process can reach a *well-known* endpoint by
//! a stable name instead — the desktop's pointer/keyboard input ports, a
//! system service — the registry also keeps a name index: an endpoint may
//! be [`published`](PortRegistry::publish_name) under a validated
//! [`tairix_abi::ipc::PortName`] and later
//! [`resolved`](PortRegistry::resolve) back to its [`EndpointId`]. The
//! index only ever points at a live binding (publishing requires the
//! endpoint to be registered, and unregistering it withdraws its names),
//! so a name can never resolve to a torn-down port. A name grants no
//! authority of its own.
//!
//! # Locking
//!
//! Like [`tairix_kernel_sec::CapTable`], the registry exposes a plain
//! `&self` / `&mut self` surface; the synchronisation policy lives with the
//! kernel's `KernelState`, which composes the registry with the scheduler and
//! the capability table under one lock-ordering policy (no global mutable
//! static). Lookups borrow `&self` (so concurrent senders share a read
//! guard); registration and unregistration take `&mut self`, keeping the
//! index of the ports each process bound. The one lock it owns guards its
//! index of the ports each sender uses ([`SenderIndex`]), which a port
//! updates under the read guard the first time a sender uses it; nothing but
//! the heap is taken under it.
//!
//! # Fail closed
//!
//! Every state-changing operation emits exactly one audit record before
//! returning:
//!
//! * [`PortRegistry::register`] refuses to overwrite a live binding. A
//!   duplicate [`EndpointId`] returns the *supplied* port back to the
//!   caller alongside [`Errno::AlreadyExists`] (so the caller can tear
//!   the rejected port down) and emits [`AuditEvent::PortRegisterDenied`];
//!   the existing binding is left untouched.
//! * [`PortRegistry::unregister`] destroys the removed port (draining any
//!   in-flight messages) and emits
//!   [`AuditEvent::PortUnregistered`]; unregistering an unknown endpoint
//!   returns [`Errno::NotFound`].
//!
//! The registry performs *no* capability check of its own: the authority
//! to bind an endpoint was already proven at [`Port::create`] time, and
//! the per-send authority is re-checked on every [`Port::send`]. The
//! registry is a pure ownership map, mirroring how [`CapTable`] stores
//! the output of `TaskCapabilities::derive` without re-deriving it.
//!
//! [`CapTable`]: tairix_kernel_sec::CapTable

extern crate alloc;

use alloc::sync::Arc;

use tairix_abi::ipc::PortName;
use tairix_abi::{Errno, ProcId};
use tairix_collections::OrderedMap;
use tairix_log::{Field, Sink};
use tairix_util::fmt::format_hex_u64;

use crate::audit::{record, AuditEvent};
use crate::port::{EndpointId, Port};
use crate::senders::{EndpointSets, Endpoints, SenderIndex};

/// A map from [`EndpointId`] to the kernel-owned [`Port`] bound to it,
/// plus an optional well-known-name index over those endpoints and indexes
/// of the ports each process bound and each sender uses.
///
/// Construct with [`PortRegistry::new`]. The registry owns each
/// registered [`Port`] for the lifetime of the binding; dropping the
/// registry drops every port it still holds.
///
/// # Well-known names
///
/// A numeric [`EndpointId`] is an opaque handle a binder must already
/// know. To let a process reach a *well-known* endpoint — the desktop's
/// pointer-input port, the keyboard port, a system service — without
/// hard-coding that number, an endpoint may additionally be published
/// under a validated [`PortName`]. The name index is a
/// pure pointer into the endpoint map: a name only ever resolves to a
/// live binding, and unregistering an endpoint withdraws every name that
/// resolved to it, so a resolution can never dangle. Publishing a name
/// grants no authority of its own; the per-send capability check on
/// [`Port::send`] is unchanged.
#[derive(Default)]
pub struct PortRegistry {
    /// Fallible to grow, so a refused heap refuses the registration.
    ports: OrderedMap<EndpointId, Port>,
    names: OrderedMap<PortName, EndpointId>,
    /// The ports each process bound, so its exit visits those and no other.
    owners: EndpointSets<u64>,
    /// Shared with every port it holds; made with the first registration.
    senders: Option<Arc<SenderIndex<ProcId>>>,
}

impl PortRegistry {
    /// Create an empty registry.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            ports: OrderedMap::new(),
            names: OrderedMap::new(),
            owners: EndpointSets::new(),
            senders: None,
        }
    }

    /// Bind `port` into the registry under its own [`Port::id`], noting it as
    /// its owner's.
    ///
    /// # Errors
    ///
    /// Returns `Err((port, `[`Errno::AlreadyExists`]`))` if the port's
    /// [`EndpointId`] is already bound. The existing binding is left
    /// untouched and the *supplied* `port` is handed back whole, for the
    /// caller to drop once it has released the registry; the kernel never
    /// silently overwrites a live endpoint. [`Errno::OutOfMemory`], the port
    /// handed back the same way, when the registry cannot hold the binding or
    /// note the port in its indexes: of the ports each sender uses, made with
    /// the first registration, and of the ports each process bound.
    ///
    /// On refusal exactly one [`AuditEvent::PortRegisterDenied`] is
    /// emitted; on success exactly one [`AuditEvent::PortRegistered`].
    // Handed back whole, so a refusal for want of memory allocates nothing.
    #[allow(clippy::result_large_err)]
    pub fn register<S: Sink + ?Sized>(
        &mut self,
        mut port: Port,
        audit: &S,
    ) -> Result<EndpointId, (Port, Errno)> {
        let id = port.id();
        let mut id_buf = [0u8; 16];
        let id_field = Field {
            key: "port",
            value: tairix_log::FieldValue::Str(format_hex_u64(id.0, &mut id_buf)),
        };

        if self.ports.contains_key(&id) {
            record(audit, AuditEvent::PortRegisterDenied, &[id_field]);
            return Err((port, Errno::AlreadyExists));
        }
        let Some(senders) = self.sender_index() else {
            record(audit, AuditEvent::PortRegisterDenied, &[id_field]);
            return Err((port, Errno::OutOfMemory));
        };
        if self.ports.try_reserve(1).is_err() || self.owners.insert(port.owner(), id).is_err() {
            record(audit, AuditEvent::PortRegisterDenied, &[id_field]);
            return Err((port, Errno::OutOfMemory));
        }
        port.note_senders_in(senders);
        // Reserved above, so this cannot fail.
        let _ = self.ports.try_insert(id, port);
        record(audit, AuditEvent::PortRegistered, &[id_field]);
        Ok(id)
    }

    /// The index every registered port notes its senders in, made with the
    /// first registration.
    fn sender_index(&mut self) -> Option<Arc<SenderIndex<ProcId>>> {
        if self.senders.is_none() {
            self.senders = Arc::try_new(SenderIndex::new()).ok();
        }
        self.senders.clone()
    }

    /// Borrow the [`Port`] bound to `id`, if any.
    ///
    /// Returns `None` for an unbound endpoint — a lookup miss is not a
    /// security decision and is not audited (the eventual `ipc_send` /
    /// `ipc_recv` handler maps the `None` to [`Errno::NotFound`] and
    /// audits at that boundary). Borrowing `&self` lets concurrent
    /// senders share a read guard over the registry while each
    /// [`Port::send`] re-checks the per-send capability.
    #[must_use]
    pub fn lookup(&self, id: EndpointId) -> Option<&Port> {
        self.ports.get(&id)
    }

    /// `true` if `id` is currently bound.
    #[must_use]
    pub fn contains(&self, id: EndpointId) -> bool {
        self.ports.contains_key(&id)
    }

    /// Remove the port bound to `id` and destroy it.
    ///
    /// The removed port is [`Port::destroy`]ed (transitioning it closed
    /// so any racing sender fails closed, and draining its mailbox).
    /// Every well-known name that resolved to `id` is withdrawn first —
    /// each emitting one [`AuditEvent::PortNameWithdrawn`] — so no name
    /// can outlive the endpoint it pointed at; one
    /// [`AuditEvent::PortUnregistered`] then records the binding's
    /// removal. The set of its owner's ports the unbinding emptied is handed
    /// back, for the caller to drop once it has released the registry.
    ///
    /// # Errors
    ///
    /// Returns [`Errno::NotFound`] if `id` is not bound; nothing is
    /// changed and no event is emitted (an unregister miss is not a
    /// security decision, mirroring [`Self::lookup`]).
    pub fn unregister<S: Sink + ?Sized>(
        &mut self,
        id: EndpointId,
        audit: &S,
    ) -> Result<Option<Endpoints>, Errno> {
        let Some(port) = self.ports.remove(&id) else {
            return Err(Errno::NotFound);
        };
        let emptied = self.owners.remove(port.owner(), id);

        // Every name that resolved to this endpoint goes before the port
        // does, so a resolution can never dangle; withdrawn in place, so a
        // teardown allocates nothing.
        self.names.retain(|name, &mut bound| {
            if bound != id {
                return true;
            }
            record(
                audit,
                AuditEvent::PortNameWithdrawn,
                &[Field {
                    key: "name",
                    value: tairix_log::FieldValue::Str(name.as_str()),
                }],
            );
            false
        });

        // `destroy` emits PortDestroyed and drains in-flight messages;
        // the registry then records that the *binding* is gone so the
        // security trail distinguishes "port torn down" from "binding
        // removed".
        port.destroy(audit);
        let mut id_buf = [0u8; 16];
        record(
            audit,
            AuditEvent::PortUnregistered,
            &[Field {
                key: "port",
                value: tairix_log::FieldValue::Str(format_hex_u64(id.0, &mut id_buf)),
            }],
        );
        Ok(emptied)
    }

    /// Take out the ports `owner` bound — the task-exit reclamation path — for
    /// the caller to [`unregister`](Self::unregister) one at a time, so a task
    /// that exits, faults, or is killed never leaves a mailbox behind for a
    /// later task to squat on or drain. Each port's senders parked for room
    /// ([`Port::room_wake_everyone`]) are woken once the registry is released,
    /// since waking takes scheduler locks, which never nest inside it. A port
    /// `owner` never bound is not visited.
    #[must_use]
    pub fn take_owned_by(&mut self, owner: u64) -> Option<Endpoints> {
        self.owners.take(owner)
    }

    /// Note at every port the process instance `sender` used that it has
    /// ended ([`Port::sender_ended`]): what it left queued stays deliverable
    /// within the room each port's owner set aside, and nothing it waited for
    /// room with is woken. A port it never used is not visited.
    pub fn sender_ended<S: Sink + ?Sized>(&self, sender: ProcId, audit: &S) {
        let used = self.senders.as_ref().and_then(|index| index.take(sender));
        for id in used.iter().flatten() {
            if let Some(port) = self.ports.get(id) {
                port.sender_ended(sender, audit);
            }
        }
    }

    /// Publish the well-known `name` as resolving to the endpoint `id`.
    ///
    /// After this, [`Self::resolve`]`(name)` returns `id` and
    /// [`Self::resolve_port`]`(name)` borrows its [`Port`], until the
    /// name is [`withdrawn`](Self::withdraw_name) or its endpoint is
    /// [`unregistered`](Self::unregister). Names are an index over live
    /// endpoints, never an independent namespace.
    ///
    /// # Errors
    ///
    /// * [`Errno::AlreadyExists`] if `name` is already published. The
    ///   existing binding is left untouched; the kernel never silently
    ///   re-points a live name.
    /// * [`Errno::NotFound`] if `id` is not a currently-registered
    ///   endpoint, so a name can never resolve to a non-existent port.
    /// * [`Errno::OutOfMemory`] if the name index cannot hold the binding.
    ///
    /// On refusal exactly one [`AuditEvent::PortNamePublishDenied`] is
    /// emitted; on success exactly one [`AuditEvent::PortNamePublished`].
    pub fn publish_name<S: Sink + ?Sized>(
        &mut self,
        name: PortName,
        id: EndpointId,
        audit: &S,
    ) -> Result<(), Errno> {
        let mut id_buf = [0u8; 16];
        let fields = [
            Field {
                key: "name",
                value: tairix_log::FieldValue::Str(name.as_str()),
            },
            Field {
                key: "port",
                value: tairix_log::FieldValue::Str(format_hex_u64(id.0, &mut id_buf)),
            },
        ];

        if self.names.contains_key(&name) {
            record(audit, AuditEvent::PortNamePublishDenied, &fields);
            return Err(Errno::AlreadyExists);
        }
        if !self.ports.contains_key(&id) {
            record(audit, AuditEvent::PortNamePublishDenied, &fields);
            return Err(Errno::NotFound);
        }

        if self.names.try_insert(name, id).is_err() {
            record(audit, AuditEvent::PortNamePublishDenied, &fields);
            return Err(Errno::OutOfMemory);
        }
        record(audit, AuditEvent::PortNamePublished, &fields);
        Ok(())
    }

    /// Resolve a well-known `name` to the [`EndpointId`] it was published
    /// against, or `None` if no such name is bound.
    ///
    /// A resolution miss is not a security decision and is not audited,
    /// mirroring [`Self::lookup`].
    #[must_use]
    pub fn resolve(&self, name: &PortName) -> Option<EndpointId> {
        self.names.get(name).copied()
    }

    /// Resolve a well-known `name` directly to the live [`Port`] it
    /// points at, or `None` if the name is unbound.
    ///
    /// Equivalent to [`Self::resolve`] followed by [`Self::lookup`];
    /// because a name is withdrawn when its endpoint is unregistered, a
    /// bound name always yields a live port.
    #[must_use]
    pub fn resolve_port(&self, name: &PortName) -> Option<&Port> {
        self.resolve(name).and_then(|id| self.ports.get(&id))
    }

    /// Withdraw the well-known `name`, returning the endpoint it
    /// resolved to.
    ///
    /// The endpoint itself is left registered; only the name index entry
    /// is removed. To tear the endpoint down use [`Self::unregister`],
    /// which withdraws its names automatically.
    ///
    /// # Errors
    ///
    /// Returns [`Errno::NotFound`] if `name` is not bound; nothing is
    /// changed and no event is emitted (a withdraw miss is not a
    /// security decision, mirroring [`Self::unregister`]).
    ///
    /// On success exactly one [`AuditEvent::PortNameWithdrawn`] is
    /// emitted.
    pub fn withdraw_name<S: Sink + ?Sized>(
        &mut self,
        name: &PortName,
        audit: &S,
    ) -> Result<EndpointId, Errno> {
        let Some(id) = self.names.remove(name) else {
            return Err(Errno::NotFound);
        };
        record(
            audit,
            AuditEvent::PortNameWithdrawn,
            &[Field {
                key: "name",
                value: tairix_log::FieldValue::Str(name.as_str()),
            }],
        );
        Ok(id)
    }

    /// Number of currently-bound endpoints.
    #[must_use]
    pub fn len(&self) -> usize {
        self.ports.len()
    }

    /// `true` if no endpoint is currently bound.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ports.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::RecordingSink;
    use alloc::vec::Vec;
    use tairix_abi::ipc::IPC_MESSAGE_MAX_PAYLOAD_LEN;
    use tairix_abi::CapabilityId;
    use tairix_caps::CapabilitySet;
    use tairix_kernel_sec::captable::{ProcessId, TaskCapabilities};
    use tairix_kernel_sec::identity::UserId;

    fn caps_of(items: &[CapabilityId]) -> CapabilitySet {
        let mut s = CapabilitySet::empty();
        for c in items {
            s.insert(*c);
        }
        s
    }

    fn task_with(task_id: u64, caps: &[CapabilityId]) -> TaskCapabilities {
        let sink = RecordingSink::new();
        let set = caps_of(caps);
        TaskCapabilities::derive(ProcessId(task_id), UserId(1), set, set, &sink).expect("derived")
    }

    fn open_port(id: u64, sink: &RecordingSink) -> Port {
        let creator = task_with(1, &[]);
        Port::create(
            EndpointId(id),
            &creator,
            CapabilitySet::empty(),
            CapabilitySet::empty(),
            IPC_MESSAGE_MAX_PAYLOAD_LEN,
            8,
            tairix_abi::RLIMIT_INFINITY,
            sink,
        )
        .expect("unrestricted port creation succeeds")
    }

    /// A port `owner` binds at `id`.
    fn owned_port(id: u64, owner: u64, sink: &RecordingSink) -> Port {
        Port::create(
            EndpointId(id),
            &task_with(owner, &[]),
            CapabilitySet::empty(),
            CapabilitySet::empty(),
            64,
            4,
            tairix_abi::RLIMIT_INFINITY,
            sink,
        )
        .expect("bound")
    }

    /// An exit takes the ports its process bound and no other, however many
    /// are bound.
    #[test]
    fn an_exit_visits_only_the_ports_its_process_bound() {
        let sink = RecordingSink::new();
        let mut reg = PortRegistry::new();
        for id in 0x100..0x140 {
            let owner = if id % 32 == 5 { 5 } else { 6 };
            reg.register(owned_port(id, owner, &sink), &sink)
                .map_err(|(_, e)| e)
                .expect("registered");
        }
        let taken = reg.take_owned_by(5).expect("owner 5 bound ports");
        let mut torn: Vec<EndpointId> = taken.iter().copied().collect();
        torn.sort_unstable();
        assert_eq!(torn, [EndpointId(0x105), EndpointId(0x125)], "two of 64");
        assert!(reg.take_owned_by(5).is_none(), "taken once");
        assert_eq!(reg.len(), 64, "taking unbinds nothing");
    }

    /// Tearing an owner's ports down wakes exactly the senders waiting at
    /// those ports, never one waiting anywhere else: an exit is not a
    /// broadcast.
    #[test]
    fn teardown_wakes_only_the_senders_waiting_at_the_ports_it_destroys() {
        let sink = RecordingSink::new();
        let mut reg = PortRegistry::new();
        for (id, owner) in [(0x10, 5), (0x11, 6)] {
            reg.register(owned_port(id, owner, &sink), &sink)
                .map_err(|(_, e)| e)
                .expect("registered");
        }
        let sender = ProcId::from_raw([9; tairix_abi::PROC_ID_LEN]);
        for (id, task) in [(0x10, 0xA), (0x11, 0xB)] {
            reg.lookup(EndpointId(id))
                .expect("registered")
                .watch_room(task, sender);
        }
        let taken = reg.take_owned_by(5).expect("owner 5 bound a port");
        let torn: Vec<EndpointId> = taken.iter().copied().collect();
        assert_eq!(torn, [EndpointId(0x10)]);
        let woken = reg
            .lookup(EndpointId(0x10))
            .expect("registered")
            .room_wake_everyone();
        reg.unregister(EndpointId(0x10), &sink).expect("unbound");
        assert_eq!(woken.tasks().collect::<Vec<u64>>(), [0xA]);
        let kept: Vec<u64> = reg
            .lookup(EndpointId(0x11))
            .expect("still registered")
            .room_wake_everyone()
            .tasks()
            .collect();
        assert_eq!(kept, [0xB], "the other port's waiter is untouched");
    }

    /// A port leaves its owner's set as it is unregistered, so an exit takes
    /// only the ports still bound.
    #[test]
    fn an_unregistered_port_leaves_its_owners_set() {
        let sink = RecordingSink::new();
        let mut reg = PortRegistry::new();
        for (id, owner) in [(0x20, 5), (0x21, 6), (0x22, 5)] {
            reg.register(owned_port(id, owner, &sink), &sink)
                .map_err(|(_, e)| e)
                .expect("registered");
        }
        reg.unregister(EndpointId(0x20), &sink).expect("unbound");
        let left: Vec<EndpointId> = reg.take_owned_by(5).into_iter().flatten().collect();
        assert_eq!(left, [EndpointId(0x22)]);
        reg.unregister(EndpointId(0x21), &sink).expect("unbound");
        assert!(reg.take_owned_by(6).is_none(), "its set went");
    }

    /// A sender's end visits the ports it used and no other: a port notes a
    /// sender once, at its first use, and the note goes with the sender or
    /// with the port.
    #[test]
    fn a_senders_end_visits_only_the_ports_it_used() {
        let sink = RecordingSink::new();
        let mut reg = PortRegistry::new();
        for id in [0x30, 0x31, 0x32, 0x33] {
            reg.register(open_port(id, &sink), &sink)
                .map_err(|(_, e)| e)
                .expect("registered");
        }
        let instance = ProcId::from_raw([7; tairix_abi::PROC_ID_LEN]);
        let sender = task_with(7, &[]).with_proc_id(instance);
        for id in [0x30, 0x32, 0x32] {
            reg.lookup(EndpointId(id))
                .expect("registered")
                .send(&sender, 0, b"m", tairix_abi::RLIMIT_INFINITY, &sink)
                .expect("sent");
        }
        reg.lookup(EndpointId(0x33))
            .expect("registered")
            .watch_room(0xA, instance);
        let used = |reg: &PortRegistry| {
            reg.senders
                .as_ref()
                .map_or_else(Vec::new, |index| index.uses(instance))
        };
        assert_eq!(
            used(&reg),
            [EndpointId(0x30), EndpointId(0x32), EndpointId(0x33)],
            "each port it used, once"
        );
        reg.unregister(EndpointId(0x30), &sink).expect("unbound");
        assert_eq!(
            used(&reg),
            [EndpointId(0x32), EndpointId(0x33)],
            "a port takes its notes with it"
        );
        reg.sender_ended(instance, &sink);
        assert!(used(&reg).is_empty(), "and the sender its own");
        assert_eq!(
            reg.lookup(EndpointId(0x32)).map(Port::len),
            Some(2),
            "what it left queued stays deliverable"
        );
        assert!(reg
            .lookup(EndpointId(0x33))
            .expect("registered")
            .room_wake_everyone()
            .is_empty());
    }

    #[test]
    fn new_registry_is_empty() {
        let reg = PortRegistry::new();
        assert!(reg.is_empty());
        assert_eq!(reg.len(), 0);
        assert!(reg.lookup(EndpointId(1)).is_none());
        assert!(!reg.contains(EndpointId(1)));
    }

    #[test]
    fn register_binds_and_lookup_returns_the_port() {
        let sink = RecordingSink::new();
        let mut reg = PortRegistry::new();
        let port = open_port(7, &sink);

        let id = reg
            .register(port, &sink)
            .map_err(|(_, e)| e)
            .expect("first bind succeeds");
        assert_eq!(id, EndpointId(7));
        assert_eq!(reg.len(), 1);
        assert!(reg.contains(EndpointId(7)));
        assert_eq!(reg.lookup(EndpointId(7)).map(Port::id), Some(EndpointId(7)));
        assert!(sink.ids().contains(&AuditEvent::PortRegistered.id().0));
    }

    #[test]
    fn register_duplicate_is_already_exists_and_hands_the_port_back() {
        let sink = RecordingSink::new();
        let mut reg = PortRegistry::new();
        reg.register(open_port(3, &sink), &sink)
            .map_err(|(_, e)| e)
            .expect("first bind succeeds");

        let dup = open_port(3, &sink);
        match reg.register(dup, &sink) {
            Ok(_) => panic!("a duplicate endpoint must be refused"),
            Err((returned, err)) => {
                assert_eq!(err, Errno::AlreadyExists);
                // The rejected port is handed back so the caller can tear it down.
                assert_eq!(returned.id(), EndpointId(3));
            }
        }
        // The original binding is untouched.
        assert_eq!(reg.len(), 1);
        assert!(sink.ids().contains(&AuditEvent::PortRegisterDenied.id().0));
    }

    #[test]
    fn lookup_finds_sent_messages_through_the_registry() {
        let sink = RecordingSink::new();
        let mut reg = PortRegistry::new();
        reg.register(open_port(5, &sink), &sink)
            .map_err(|(_, e)| e)
            .expect("bind succeeds");
        let sender = task_with(2, &[]);

        let port = reg.lookup(EndpointId(5)).expect("bound port is found");
        port.send(&sender, 0, b"ping", tairix_abi::RLIMIT_INFINITY, &sink)
            .expect("send succeeds");

        let (msg, _woken) = reg
            .lookup(EndpointId(5))
            .and_then(Port::recv)
            .expect("the delivered message is reachable through the registry");
        assert_eq!(msg.payload.as_bytes(), b"ping");
    }

    #[test]
    fn unregister_removes_destroys_and_audits() {
        let sink = RecordingSink::new();
        let mut reg = PortRegistry::new();
        reg.register(open_port(9, &sink), &sink)
            .map_err(|(_, e)| e)
            .expect("bind succeeds");

        reg.unregister(EndpointId(9), &sink)
            .expect("unregister of a bound endpoint succeeds");
        assert!(reg.is_empty());
        assert!(reg.lookup(EndpointId(9)).is_none());
        let ids = sink.ids();
        assert!(ids.contains(&AuditEvent::PortDestroyed.id().0));
        assert!(ids.contains(&AuditEvent::PortUnregistered.id().0));
    }

    #[test]
    fn unregister_unknown_endpoint_is_not_found() {
        let sink = RecordingSink::new();
        let mut reg = PortRegistry::new();
        let err = reg
            .unregister(EndpointId(42), &sink)
            .expect_err("unregistering an unbound endpoint fails closed");
        assert_eq!(err, Errno::NotFound);
    }

    #[test]
    fn endpoint_can_be_rebound_after_unregister() {
        let sink = RecordingSink::new();
        let mut reg = PortRegistry::new();
        reg.register(open_port(11, &sink), &sink)
            .map_err(|(_, e)| e)
            .expect("first bind succeeds");
        reg.unregister(EndpointId(11), &sink)
            .expect("unregister succeeds");

        reg.register(open_port(11, &sink), &sink)
            .map_err(|(_, e)| e)
            .expect("the freed endpoint can be re-bound");
        assert!(reg.contains(EndpointId(11)));
    }

    #[test]
    fn distinct_endpoints_coexist() {
        let sink = RecordingSink::new();
        let mut reg = PortRegistry::new();
        reg.register(open_port(1, &sink), &sink)
            .map_err(|(_, e)| e)
            .expect("bind 1");
        reg.register(open_port(2, &sink), &sink)
            .map_err(|(_, e)| e)
            .expect("bind 2");
        assert_eq!(reg.len(), 2);
        assert!(reg.contains(EndpointId(1)));
        assert!(reg.contains(EndpointId(2)));
    }

    fn name(s: &str) -> PortName {
        PortName::from_ascii(s.as_bytes()).expect("test name is valid")
    }

    #[test]
    fn publish_then_resolve_round_trips_to_the_endpoint() {
        let sink = RecordingSink::new();
        let mut reg = PortRegistry::new();
        reg.register(open_port(7, &sink), &sink)
            .map_err(|(_, e)| e)
            .expect("bind succeeds");

        reg.publish_name(name("input.pointer"), EndpointId(7), &sink)
            .expect("publishing over a live endpoint succeeds");

        assert_eq!(reg.resolve(&name("input.pointer")), Some(EndpointId(7)));
        assert_eq!(
            reg.resolve_port(&name("input.pointer")).map(Port::id),
            Some(EndpointId(7))
        );
        assert!(sink.ids().contains(&AuditEvent::PortNamePublished.id().0));
    }

    #[test]
    fn resolve_unknown_name_is_none() {
        let reg = PortRegistry::new();
        assert_eq!(reg.resolve(&name("nope")), None);
        assert!(reg.resolve_port(&name("nope")).is_none());
    }

    #[test]
    fn publish_for_unregistered_endpoint_is_not_found() {
        let sink = RecordingSink::new();
        let mut reg = PortRegistry::new();
        let err = reg
            .publish_name(name("svc.bad"), EndpointId(99), &sink)
            .expect_err("a name may not point at a non-existent endpoint");
        assert_eq!(err, Errno::NotFound);
        assert_eq!(reg.resolve(&name("svc.bad")), None);
        assert!(sink
            .ids()
            .contains(&AuditEvent::PortNamePublishDenied.id().0));
    }

    #[test]
    fn publish_duplicate_name_is_already_exists() {
        let sink = RecordingSink::new();
        let mut reg = PortRegistry::new();
        reg.register(open_port(1, &sink), &sink)
            .map_err(|(_, e)| e)
            .expect("bind 1");
        reg.register(open_port(2, &sink), &sink)
            .map_err(|(_, e)| e)
            .expect("bind 2");
        reg.publish_name(name("the.name"), EndpointId(1), &sink)
            .expect("first publish succeeds");

        let err = reg
            .publish_name(name("the.name"), EndpointId(2), &sink)
            .expect_err("a live name may not be silently re-pointed");
        assert_eq!(err, Errno::AlreadyExists);
        // The original binding is untouched.
        assert_eq!(reg.resolve(&name("the.name")), Some(EndpointId(1)));
        assert!(sink
            .ids()
            .contains(&AuditEvent::PortNamePublishDenied.id().0));
    }

    #[test]
    fn one_endpoint_may_carry_several_names() {
        let sink = RecordingSink::new();
        let mut reg = PortRegistry::new();
        reg.register(open_port(4, &sink), &sink)
            .map_err(|(_, e)| e)
            .expect("bind succeeds");
        reg.publish_name(name("alias.one"), EndpointId(4), &sink)
            .expect("first alias");
        reg.publish_name(name("alias.two"), EndpointId(4), &sink)
            .expect("second alias");

        assert_eq!(reg.resolve(&name("alias.one")), Some(EndpointId(4)));
        assert_eq!(reg.resolve(&name("alias.two")), Some(EndpointId(4)));
    }

    #[test]
    fn withdraw_name_removes_only_the_index_entry() {
        let sink = RecordingSink::new();
        let mut reg = PortRegistry::new();
        reg.register(open_port(6, &sink), &sink)
            .map_err(|(_, e)| e)
            .expect("bind succeeds");
        reg.publish_name(name("gone.soon"), EndpointId(6), &sink)
            .expect("publish succeeds");

        let id = reg
            .withdraw_name(&name("gone.soon"), &sink)
            .expect("withdrawing a bound name returns its endpoint");
        assert_eq!(id, EndpointId(6));
        assert_eq!(reg.resolve(&name("gone.soon")), None);
        // The endpoint itself survives the name withdrawal.
        assert!(reg.contains(EndpointId(6)));
        assert!(sink.ids().contains(&AuditEvent::PortNameWithdrawn.id().0));
    }

    #[test]
    fn withdraw_unknown_name_is_not_found() {
        let sink = RecordingSink::new();
        let mut reg = PortRegistry::new();
        let err = reg
            .withdraw_name(&name("never.bound"), &sink)
            .expect_err("withdrawing an unbound name fails closed");
        assert_eq!(err, Errno::NotFound);
    }

    #[test]
    fn unregister_withdraws_every_name_pointing_at_the_endpoint() {
        let sink = RecordingSink::new();
        let mut reg = PortRegistry::new();
        reg.register(open_port(8, &sink), &sink)
            .map_err(|(_, e)| e)
            .expect("bind succeeds");
        reg.publish_name(name("a.name"), EndpointId(8), &sink)
            .expect("first alias");
        reg.publish_name(name("b.name"), EndpointId(8), &sink)
            .expect("second alias");

        reg.unregister(EndpointId(8), &sink)
            .expect("unregister succeeds");

        // Both names are gone; neither can resolve to the torn-down port.
        assert_eq!(reg.resolve(&name("a.name")), None);
        assert_eq!(reg.resolve(&name("b.name")), None);
        let withdrawn = sink
            .ids()
            .iter()
            .filter(|&&id| id == AuditEvent::PortNameWithdrawn.id().0)
            .count();
        assert_eq!(withdrawn, 2);
    }

    #[test]
    fn name_freed_by_unregister_can_be_republished_to_a_new_endpoint() {
        let sink = RecordingSink::new();
        let mut reg = PortRegistry::new();
        reg.register(open_port(1, &sink), &sink)
            .map_err(|(_, e)| e)
            .expect("bind 1");
        reg.publish_name(name("shared.name"), EndpointId(1), &sink)
            .expect("publish on 1");
        reg.unregister(EndpointId(1), &sink).expect("unregister 1");

        reg.register(open_port(2, &sink), &sink)
            .map_err(|(_, e)| e)
            .expect("bind 2");
        reg.publish_name(name("shared.name"), EndpointId(2), &sink)
            .expect("the freed name can be re-pointed at a new endpoint");
        assert_eq!(reg.resolve(&name("shared.name")), Some(EndpointId(2)));
    }
}
