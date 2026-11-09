//! Capability-checked typed message ports.
//!
//! A [`Port`] is a kernel-owned endpoint identified by a stable
//! [`EndpointId`] declared via `lib/abi`. Each port carries:
//!
//! * `required_send_caps` — the [`CapabilitySet`] a sender's task must
//!   hold for a send to succeed. The kernel enforces this on every
//!   call; the receiver does **not** re-check.
//! * `required_recv_caps` — the set the creator must hold *at port creation*.
//! * `max_payload` — the maximum payload length, bounded above by
//!   [`tairix_abi::ipc::IPC_MESSAGE_MAX_PAYLOAD_LEN`].
//! * a mailbox with one lane per sending process instance
//!   (`kernel/ipc/src/lanes.rs`): each lane holds at most the port's per-sender depth,
//!   each message is charged to its sender's own `ipc-queue-bytes` limit until
//!   it is taken, and the owner takes the lanes in turn, each sender's own
//!   messages oldest first. A send refused for room is transient
//!   back-pressure on that sender alone ([`Errno::WouldBlock`]): no sender's
//!   messages ever take another's room.
//!
//! Every refused operation emits exactly one audit event through
//! [`crate::audit`] before returning the [`Errno`] to the caller
//! ("fail-closed"). Ports use a lock-free atomic state word so the
//! send fast path can reject delivery to a closed port without
//! taking the mailbox lock; see [`tests/loom.rs`](../../tests/loom.rs)
//! for the model-checked interleavings.

extern crate alloc;

use alloc::collections::{TryReserveError, VecDeque};
use alloc::sync::Arc;

use tairix_abi::ipc::{IPC_MESSAGE_MAX_PAYLOAD_LEN, IPC_SENDER_DEPTH_MAX};
use tairix_abi::{Errno, LimitKind, Origin, ProcId};
use tairix_caps::CapabilitySet;
use tairix_kernel_mem::SensitiveBuffer;
use tairix_kernel_sec::captable::TaskCapabilities;
use tairix_kernel_sec::Charge;
use tairix_log::{Field, Sink};
use tairix_util::fmt::{format_hex_bytes, format_hex_u64, format_u64, format_usize};

use crate::audit::{record, AuditEvent};
use crate::lanes::{
    lane_cost, KeepRoom, Lane, Lanes, Opened, RoomWaiters, RoomWake, SPARE_LANES, SPARE_LANE_ITEMS,
};
use crate::loom_compat::{AtomicU32, Ordering};
use crate::senders::{Noted, SenderIndex};

/// Why a send was refused: the sender lacks the port's send capabilities.
const SEND_DENIED_CAPABILITY: Field<'static> = Field {
    key: "reason",
    value: tairix_log::FieldValue::Str("missing_capability"),
};

/// Why a send was refused: the port's owner admitted another sender.
const SEND_DENIED_NOT_ADMITTED: Field<'static> = Field {
    key: "reason",
    value: tairix_log::FieldValue::Str("not_admitted"),
};

/// Stable endpoint identifier carried in the IPC header.
///
/// Wraps the same `u64` declared by
/// [`tairix_abi::ipc::IpcMessageHeader::endpoint`]; the newtype keeps
/// port identifiers distinct from task identifiers, capability
/// identifiers, and other 64-bit kernel handles.
#[repr(transparent)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct EndpointId(pub u64);

/// Fixed (non-tunable) atomic states a [`Port`] can be in.
///
/// Encoded into a single `AtomicU32` so the send fast path observes
/// the port's liveness in one relaxed-acquire load and can reject
/// closed ports without acquiring the mailbox lock.
mod state {
    /// Open and accepting messages.
    pub(super) const OPEN: u32 = 0;
    /// `destroy()` has begun; senders must fail-closed.
    pub(super) const CLOSED: u32 = 1;
}

/// What a committed receive hands back: the receiver's answer, the message
/// taken, and the thread of its sender the slot its taking freed was handed to.
pub type Received<R> = (R, Message, RoomWake<ProcId>);

/// A message in flight or queued for delivery on a [`Port`].
///
/// Payload is owned by the kernel until `recv`; sender bytes are copied into
/// a kernel-side buffer at enqueue time so the sender cannot mutate the
/// buffer after the send has been accepted.
#[derive(Debug)]
pub struct Message {
    /// Sending **process** identifier, taken from the kernel-trusted
    /// capability record at enqueue time — never a caller-supplied value.
    /// Process-scoped because the principal a receiver authorises is the
    /// process, not whichever of its threads happened to call `send`.
    pub sender: u64,
    /// The sender's kernel-attested [`Origin`], snapshotted from its own
    /// task state when the send was accepted, so a receiver can
    /// authenticate each message's principal without trusting anything
    /// the sender wrote into the payload.
    pub origin: Origin,
    /// Payload bytes; length is always `<= max_payload` of the port. Wiped
    /// when the message is dropped, so a payload that carried a passphrase,
    /// key, or capability token leaves nothing in freed kernel heap.
    pub payload: SensitiveBuffer,
    /// What the sender is charged for the message, returned once it is taken.
    _charge: Charge,
}

/// Why [`Port::enqueue`] did not take a message.
#[derive(Copy, Clone)]
enum Refused {
    /// The port was destroyed.
    Closed,
    /// The owner admitted another sender; `first` under this admission.
    NotAdmitted { first: bool },
    /// The sender's own lane is at the per-sender depth.
    Full,
    /// Paying for a lane, or for its growth, would carry the sender past its
    /// own limit.
    OverLimit,
    /// The room for the message could not be allocated.
    NoMemory,
}

/// Whether a send from one sender would find room at a port, and what it
/// waits for when it would not.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Room {
    /// A send of the port's largest message would be admitted — or the
    /// sender holds nothing, so no release could make more room and its send
    /// answers.
    Free,
    /// The sender's lane holds the per-sender depth: a receive there frees a
    /// slot.
    Full,
    /// The message, with any lane the send opens or grows, would carry the
    /// sender past its `ipc-queue-bytes` limit: a release of what it holds
    /// anywhere frees it.
    OverBudget,
}

/// One sending process instance's messages at a port, oldest first.
struct PortLane {
    messages: VecDeque<Message>,
    /// What its messages are charged, beyond the lane's storage.
    charged: u64,
    /// What the sender pays for the lane's storage; `None` while it is a spare
    /// the owner holds.
    paid: Option<Charge>,
    /// Its sender has ended, and what the lane holds is held against the room
    /// the port's owner set aside for an ended sender's traffic.
    ended: bool,
}

impl PortLane {
    /// How many messages a lane is made with room for.
    fn first_room(per_sender: usize) -> usize {
        per_sender.min(SPARE_LANE_ITEMS)
    }

    /// A lane with room for the first few messages, made outside any lock.
    fn fresh(per_sender: usize) -> Result<Self, TryReserveError> {
        let mut messages = VecDeque::new();
        messages.try_reserve_exact(Self::first_room(per_sender))?;
        Ok(Self {
            messages,
            charged: 0,
            paid: None,
            ended: false,
        })
    }

    /// The bytes the lane's storage holds, whatever is in it.
    fn storage(&self) -> u64 {
        lane_bytes(self.messages.capacity())
    }

    /// What the lane holds: its storage and its messages' payloads.
    fn held(&self) -> u64 {
        self.storage().saturating_add(self.charged)
    }

    /// The capacity the lane grows to, at most `per_sender`, before it takes
    /// one more message, and what the growth charges the sender; `None` while
    /// it has room.
    fn growth(&self, per_sender: usize) -> Option<(usize, u64)> {
        (self.messages.len() >= self.messages.capacity()).then(|| {
            let target = crate::grown_capacity(
                self.messages.capacity(),
                self.messages.len() + 1,
                per_sender,
            );
            (target, lane_bytes(target).saturating_sub(self.storage()))
        })
    }

    /// Make room for one more message. The sender pays for the growth under
    /// `limit` before it is taken, so its limit bounds the lane as it bounds
    /// the messages.
    fn reserve_one(&mut self, per_sender: usize, limit: u64) -> Result<(), Refused> {
        let Some((target, grown)) = self.growth(per_sender) else {
            return Ok(());
        };
        let Some(paid) = self.paid.as_mut() else {
            return Err(Refused::NoMemory);
        };
        paid.grow(grown, limit).map_err(|_| Refused::OverLimit)?;
        if self
            .messages
            .try_reserve_exact(target - self.messages.len())
            .is_err()
        {
            paid.shrink(grown);
            return Err(Refused::NoMemory);
        }
        Ok(())
    }

    /// Queue `message` behind the lane's others.
    fn push(&mut self, message: Message) {
        let charge = Port::message_charge(message.payload.len());
        self.charged = self.charged.saturating_add(charge);
        self.messages.push_back(message);
    }

    /// Take the lane's oldest message.
    fn pop(&mut self) -> Option<Message> {
        let message = self.messages.pop_front()?;
        let charge = Port::message_charge(message.payload.len());
        self.charged = self.charged.saturating_sub(charge);
        Some(message)
    }
}

/// The bytes a lane with room for `capacity` messages holds.
fn lane_bytes(capacity: usize) -> u64 {
    crate::queue_reservation(
        lane_cost::<ProcId, PortLane>(),
        size_of::<Message>(),
        0,
        capacity,
    )
}

impl Lane for PortLane {
    fn is_idle(&self) -> bool {
        self.messages.is_empty()
    }

    fn is_reusable(&self) -> bool {
        self.messages.capacity() <= SPARE_LANE_ITEMS
    }

    fn release(&mut self) -> Option<Charge> {
        self.ended = false;
        self.paid.take()
    }
}

/// What a port's mailbox lock guards: every sender's lane, and the one sender
/// its owner admitted — checked at enqueue under the lock every send already
/// takes, so admission costs a send nothing more.
///
/// Only [`Port::send`] grows anything here, fallibly; taking a message never
/// allocates.
struct Mailbox {
    /// Lanes keyed by the sending process *instance*, because a message
    /// outlives its sender and a recycled process number must never inherit
    /// a dead sender's lane.
    lanes: Lanes<ProcId, PortLane>,
    /// Messages queued across every lane.
    queued: usize,
    /// What the lanes of ended senders hold, against [`Port::ended_room`].
    ended: u64,
    /// The only process instance whose messages the port takes, once its
    /// owner has named one.
    admitted: Option<ProcId>,
    /// Sends refused for want of that admission over the port's life.
    refused: u64,
    /// Whether a refusal under the current admission has been recorded. Only
    /// the first is, so a sender the port will never take cannot flood the
    /// audit trail through it; the rest are counted into its destruction.
    refusal_recorded: bool,
    /// Notes each sender in the registry's index once, so only its first use
    /// of the port takes the index's lock.
    noted: Noted<ProcId>,
    /// Senders' threads waiting on a wait-set member for room in their own
    /// lane, under the lock that frees it, so taking a message and handing
    /// the slot it freed on are one critical section.
    room: RoomWaiters<ProcId>,
}

/// One end of an IPC message channel.
///
/// Construct with [`Port::create`]; tear down with [`Port::destroy`].
/// Sends use [`Port::send`], which performs the capability check
/// described in the module docs; receives use [`Port::recv`].
pub struct Port {
    id: EndpointId,
    /// **Process** that bound this port. Only that process may receive from
    /// it (any of its threads may) or observe it through a wait-set, and the
    /// exit path reclaims every port by this owner so a dead process's
    /// mailbox never lingers. Never caller-supplied: recorded from the
    /// kernel-trusted capability record at create time.
    owner: u64,
    required_send_caps: CapabilitySet,
    required_recv_caps: CapabilitySet,
    max_payload: u32,
    /// The most messages one sender may have queued here at once.
    per_sender: usize,
    /// What the owner set aside for the messages of senders that have ended:
    /// one full lane.
    ended_room: u64,
    // State word read on every send before taking the lock.
    state: AtomicU32,
    // Mailbox under a spinlock: IPC sends never block on I/O, and the lock
    // is held only for bounded lane bookkeeping.
    mailbox: tairix_sync::SpinLock<Mailbox>,
    /// What the owner is charged for the port's bookkeeping, returned when the
    /// port is dropped.
    _queue_charge: Charge,
}

impl Port {
    /// What a port's owner is charged against its `ipc-queue-bytes` limit:
    /// its bookkeeping, the spare lanes it keeps, and the room it sets aside
    /// for the messages of senders that have ended ([`Self::ended_room`]).
    /// Senders pay for their own lanes and messages.
    #[must_use]
    pub fn reservation(max_payload: u32, per_sender: usize) -> u64 {
        crate::queue_reservation(
            size_of::<Self>() + SPARE_LANES * size_of::<PortLane>(),
            size_of::<Message>(),
            0,
            SPARE_LANES * SPARE_LANE_ITEMS,
        )
        .saturating_add(Self::ended_room(max_payload, per_sender))
    }

    /// The room a port keeps for the messages of senders that have ended: one
    /// full lane of the largest messages, as they are charged. A message
    /// outlives its sender, at its receiver's expense, up to this much.
    #[must_use]
    pub fn ended_room(max_payload: u32, per_sender: usize) -> u64 {
        lane_bytes(per_sender).saturating_add(
            u64::try_from(per_sender)
                .unwrap_or(u64::MAX)
                .saturating_mul(Self::largest_charge(max_payload)),
        )
    }

    /// What one queued message of `len` payload bytes costs its sender against
    /// its own `ipc-queue-bytes` limit until the owner takes it, beyond its
    /// lane's storage: the payload, and never less than the message's own
    /// record, so the limit refuses an empty message as it does any other.
    #[must_use]
    pub fn message_charge(len: usize) -> u64 {
        u64::try_from(len.max(size_of::<Message>())).unwrap_or(u64::MAX)
    }

    /// What a message of the largest payload `max_payload` allows costs its
    /// sender.
    fn largest_charge(max_payload: u32) -> u64 {
        Self::message_charge(usize::try_from(max_payload).unwrap_or(usize::MAX))
    }

    /// What a sender with no lane here pays for the one its send opens, a
    /// spare or a fresh one alike.
    fn opening_charge(&self) -> u64 {
        lane_bytes(PortLane::first_room(self.per_sender))
    }

    /// Create a new capability-checked port.
    ///
    /// `creator` must already hold every capability listed in
    /// `required_recv_caps`; this enforces the bind-time half of the
    /// check (the sender check happens on every [`Self::send`]). The creator
    /// additionally must hold
    /// [`tairix_abi::CapabilityId::IPC_BIND_PRIVILEGED`] when
    /// `required_send_caps` is non-empty — i.e. a port that restricts
    /// who may *send* into it is by definition a privileged endpoint —
    /// **or** when `id` is a reserved well-known service rendezvous
    /// ([`tairix_abi::ipc::is_reserved_endpoint`]): an open bind on a
    /// reserved id would let an unprivileged squatter claim traffic
    /// meant for the service, exactly the refusal
    /// [`crate::CallEndpoint::create`] makes. The creator becomes the
    /// port's [`owner`](Self::owner).
    ///
    /// `per_sender` bounds the messages one sending process instance may have
    /// queued at once.
    ///
    /// # Errors
    ///
    /// * [`Errno::PermissionDenied`] if `creator` does not satisfy the
    ///   bind authority described above.
    /// * [`Errno::LengthOutOfRange`] if `max_payload >
    ///   IPC_MESSAGE_MAX_PAYLOAD_LEN`, or `per_sender` is `0` or above
    ///   [`IPC_SENDER_DEPTH_MAX`].
    /// * [`Errno::LimitExceeded`] if the port's
    ///   [`reservation`](Self::reservation) would carry the creator past
    ///   `queue_limit`, its `ipc-queue-bytes` limit.
    /// * [`Errno::OutOfMemory`] if the port's bookkeeping cannot be
    ///   allocated.
    ///
    /// On any failure exactly one
    /// [`AuditEvent::PortCreateDenied`] is emitted; on success exactly
    /// one [`AuditEvent::PortCreated`].
    #[allow(
        clippy::too_many_arguments,
        reason = "identity, authority, two bounds and the owner's budget are independent"
    )]
    pub fn create<S: Sink + ?Sized>(
        id: EndpointId,
        creator: &TaskCapabilities,
        required_send_caps: CapabilitySet,
        required_recv_caps: CapabilitySet,
        max_payload: u32,
        per_sender: usize,
        queue_limit: u64,
        audit: &S,
    ) -> Result<Self, Errno> {
        let mut id_buf = [0u8; 16];
        let id_field = Field {
            key: "port",
            value: tairix_log::FieldValue::Str(format_hex_u64(id.0, &mut id_buf)),
        };

        if max_payload > IPC_MESSAGE_MAX_PAYLOAD_LEN
            || per_sender == 0
            || per_sender > IPC_SENDER_DEPTH_MAX
        {
            record(audit, AuditEvent::PortCreateDenied, &[id_field]);
            return Err(Errno::LengthOutOfRange);
        }

        // The creator must already hold every required-recv capability:
        // a binder may not grant itself authority it does not already
        // have (no ambient authority).
        if !required_recv_caps.is_subset_of(creator.effective()) {
            record(audit, AuditEvent::PortCreateDenied, &[id_field]);
            return Err(Errno::PermissionDenied);
        }

        // A port that restricts who may send is privileged, and so is a
        // reserved well-known rendezvous id (a squatter must not claim a
        // service's traffic); binding either requires IPC_BIND_PRIVILEGED.
        if (!required_send_caps.is_empty() || tairix_abi::ipc::is_reserved_endpoint(id.0))
            && !creator.has(tairix_abi::CapabilityId::IPC_BIND_PRIVILEGED)
        {
            record(audit, AuditEvent::PortCreateDenied, &[id_field]);
            return Err(Errno::PermissionDenied);
        }

        let queue_charge = match creator.charges().charge(
            LimitKind::IpcQueueBytes,
            Self::reservation(max_payload, per_sender),
            queue_limit,
        ) {
            Ok(charge) => charge,
            Err(err) => {
                record(audit, AuditEvent::PortCreateDenied, &[id_field]);
                return Err(err);
            }
        };
        let mut lanes = Lanes::new();
        if lanes.try_reserve_spares().is_err() {
            record(audit, AuditEvent::PortCreateDenied, &[id_field]);
            return Err(Errno::OutOfMemory);
        }

        record(audit, AuditEvent::PortCreated, &[id_field]);

        Ok(Self {
            id,
            owner: creator.process().0,
            required_send_caps,
            required_recv_caps,
            max_payload,
            per_sender,
            ended_room: Self::ended_room(max_payload, per_sender),
            state: AtomicU32::new(state::OPEN),
            mailbox: tairix_sync::SpinLock::new(Mailbox {
                lanes,
                queued: 0,
                ended: 0,
                admitted: None,
                refused: 0,
                refusal_recorded: false,
                noted: Noted::new(),
                room: RoomWaiters::new(),
            }),
            _queue_charge: queue_charge,
        })
    }

    /// Endpoint identifier this port was created with.
    #[must_use]
    pub fn id(&self) -> EndpointId {
        self.id
    }

    /// The task that bound this port — the only task that may receive
    /// from it or observe it through a wait-set.
    #[must_use]
    pub fn owner(&self) -> u64 {
        self.owner
    }

    /// `true` when at least one delivered message is waiting to be
    /// drained — the non-consuming readiness peek the wait-set scan
    /// uses; the woken owner's `ipc_recv` performs the actual dequeue.
    #[must_use]
    pub fn has_pending(&self) -> bool {
        self.mailbox.lock().lanes.has_turn()
    }

    /// The slots a sender with `lane` has free here.
    fn free_in(&self, lane: Option<&PortLane>) -> usize {
        lane.map_or(self.per_sender, |lane| {
            self.per_sender.saturating_sub(lane.messages.len())
        })
    }

    /// Whether a send of the port's largest message from `task`, a thread of
    /// `sender`, would be admitted under `limit`, its `ipc-queue-bytes` limit:
    /// a slot in its lane here that it holds or no sibling holds, and all it
    /// holds anywhere, with that message and any lane the send opens or grows,
    /// within the limit. A sender holding nothing has room whatever it is
    /// short of: waiting could never end, so its send answers. The
    /// non-consuming readiness peek a wait-set member observing this port's
    /// room uses; the woken sender's own [`Self::send`] takes the slot.
    #[must_use]
    pub fn room_for(&self, sender: &TaskCapabilities, task: u64, limit: u64) -> Room {
        let q = self.mailbox.lock();
        let instance = sender.proc_id();
        let lane = q.lanes.get(instance);
        if !q.room.has_room(task, instance, self.free_in(lane)) {
            return Room::Full;
        }
        let lane = match lane {
            Some(lane) => lane.growth(self.per_sender).map_or(0, |(_, cost)| cost),
            None => self.opening_charge(),
        };
        drop(q);
        let held = sender.charges().held(LimitKind::IpcQueueBytes);
        let needed = held
            .saturating_add(Self::largest_charge(self.max_payload))
            .saturating_add(lane);
        if needed <= limit || held == 0 {
            Room::Free
        } else {
            Room::OverBudget
        }
    }

    /// Look for room as `task`, a thread of `sender` waiting on a wait-set,
    /// and note `sender`'s use of the port so its end forgets the wait: a slot
    /// no sibling holds is handed to it, and otherwise it waits in its
    /// sender's line for the next [`Self::recv`] to free one, keeping its
    /// place there if it waited before. A wait the heap has no room to record
    /// is woken by the broadcast the next slot freed carries.
    pub fn watch_room(&self, task: u64, sender: ProcId) {
        let mut q = self.mailbox.lock();
        if q.noted.note(sender, self.id, self.is_closed()).is_err() {
            q.room.overflow();
            return;
        }
        let free = self.free_in(q.lanes.get(sender));
        let _ = q.room.look(task, sender, free);
    }

    /// Note each sender that uses the port in the registry's `index`.
    pub(crate) fn note_senders_in(&mut self, index: Arc<SenderIndex<ProcId>>) {
        self.mailbox.get_mut().noted.note_in(index);
    }

    /// End `task`'s wait for room here, answering whom to wake once the lock
    /// is released: a slot it holds and did not send into passes to the next
    /// thread waiting as its sender. Every exit of a wait ends it so but for
    /// the member the wait reported ready ([`Self::own_room`]).
    #[must_use]
    pub fn leave_room(&self, task: u64) -> RoomWake<ProcId> {
        let mut q = self.mailbox.lock();
        let Mailbox { lanes, room, .. } = &mut *q;
        let Some(sender) = room.leave(task) else {
            return RoomWake::none();
        };
        room.hand_out(sender, self.free_in(lanes.get(sender)))
    }

    /// Keep the slot `task` holds here now its wait reported this port's room
    /// ready, until its next send here, its next call that is neither a send
    /// nor a wait watching the port again, or its end ([`Self::leave_room`]):
    /// the `EPOLLEXCLUSIVE` contract, under which a slot is held only from the
    /// holder's own siblings, and only while it runs between its wait and its
    /// send. Answers whether it holds one; a wait that holds none ends.
    #[must_use]
    pub fn own_room(&self, task: u64) -> bool {
        let mut q = self.mailbox.lock();
        let Mailbox { lanes, room, .. } = &mut *q;
        let Some(sender) = room.sender_of(task) else {
            return false;
        };
        room.own(task, self.free_in(lanes.get(sender)))
    }

    /// Whether `task` holds a slot here: one handed to it while it waited, or
    /// kept as its wait reported the room ready, and not yet sent into.
    #[must_use]
    pub fn holds_room(&self, task: u64) -> bool {
        self.mailbox.lock().room.holds(task)
    }

    /// Every task waiting for room, for a change that touches every lane at
    /// once — an admission discarding other senders' messages, or the port
    /// going away. Each woken sender re-checks its own lane, so waking one
    /// whose lane is still full costs it one re-park and loses nothing.
    #[must_use]
    pub fn room_wake_everyone(&self) -> RoomWake<ProcId> {
        self.mailbox.lock().room.take_everyone()
    }

    /// Maximum payload (bytes) this port will accept.
    #[must_use]
    pub fn max_payload(&self) -> u32 {
        self.max_payload
    }

    /// The most messages one sender may have queued here at once.
    #[must_use]
    pub fn per_sender(&self) -> usize {
        self.per_sender
    }

    /// Capability set required of every sender.
    #[must_use]
    pub fn required_send_caps(&self) -> &CapabilitySet {
        &self.required_send_caps
    }

    /// Capability set required of any binder/receiver at create time.
    #[must_use]
    pub fn required_recv_caps(&self) -> &CapabilitySet {
        &self.required_recv_caps
    }

    /// `true` once [`Self::destroy`] has run.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.state.load(Ordering::Acquire) == state::CLOSED
    }

    /// Mark the port closed; subsequent sends fail with
    /// [`Errno::NotFound`].
    ///
    /// Idempotent: a second call is a no-op (and still records one
    /// [`AuditEvent::PortDestroyed`], since destruction *attempts* are
    /// the security event). Messages already queued are discarded, their
    /// payloads wiped as they drop outside the mailbox lock.
    pub fn destroy<S: Sink + ?Sized>(&self, audit: &S) {
        // Transition OPEN -> CLOSED. Use Release so the prior content
        // of the mailbox is visible to any thread that subsequently
        // sees `CLOSED`, and Acquire on the reverse to pair with the
        // load on the send fast path.
        self.state.store(state::CLOSED, Ordering::Release);
        let (retired, drained, refused, noted) = {
            let mut q = self.mailbox.lock();
            let drained = core::mem::take(&mut q.queued);
            let noted = q.noted.take();
            (q.lanes.take_all(), drained, q.refused, noted)
        };
        drop(retired);
        noted.forget(self.id);
        let mut id_buf = [0u8; 16];
        let mut drained_buf = [0u8; 12];
        let mut refused_buf = [0u8; 20];
        record(
            audit,
            AuditEvent::PortDestroyed,
            &[
                Field {
                    key: "port",
                    value: tairix_log::FieldValue::Str(format_hex_u64(self.id.0, &mut id_buf)),
                },
                Field {
                    key: "drained",
                    value: tairix_log::FieldValue::Str(format_usize(drained, &mut drained_buf)),
                },
                Field {
                    key: "refused",
                    value: tairix_log::FieldValue::Str(format_u64(refused, &mut refused_buf)),
                },
            ],
        );
    }

    /// Enqueue `payload` for delivery on this port.
    ///
    /// The kernel enforces every check:
    ///
    /// 1. **Lock-free fast path.** If the port has been destroyed,
    ///    [`Errno::NotFound`] is returned without taking the mailbox
    ///    lock and one [`AuditEvent::MessageSendToClosedPort`] is
    ///    emitted.
    /// 2. **Capability check.** Every capability in
    ///    `required_send_caps` must be in `sender.effective()`;
    ///    otherwise [`Errno::PermissionDenied`] +
    ///    [`AuditEvent::MessageSendDenied`].
    /// 3. **Size check.** Payload bytes must be `<= max_payload`,
    ///    bounded again by [`IPC_MESSAGE_MAX_PAYLOAD_LEN`], and the message,
    ///    with the lane a sender holding nothing opens for it, within
    ///    `sender_limit`; otherwise [`Errno::MessageTooLarge`] +
    ///    [`AuditEvent::MessageTooLarge`]. Either way no release of what the
    ///    sender holds could admit it, so it is not the retryable refusal the
    ///    charge below gives.
    /// 4. **Charge.** The message's [`message_charge`](Self::message_charge)
    ///    is taken from the sender's own `ipc-queue-bytes` budget under
    ///    `sender_limit`; otherwise [`Errno::LimitExceeded`] +
    ///    [`AuditEvent::IpcSenderLimitReached`].
    /// 5. **Admission check.** Once the owner has admitted a sender
    ///    ([`Self::admit`]), any other process instance gets
    ///    [`Errno::PermissionDenied`], taking no room. The first such
    ///    refusal under an admission is recorded as
    ///    [`AuditEvent::MessageSendDenied`]; the rest are counted into the
    ///    port's [`AuditEvent::PortDestroyed`].
    /// 6. **Depth check.** If the sender's own lane holds the per-sender
    ///    depth, or each slot it has free is held by a sibling waiting for
    ///    room, [`Errno::WouldBlock`] + [`AuditEvent::MailboxFull`] — the
    ///    receiver has not taken that sender's earlier messages yet, the
    ///    same retryable signal [`Self::recv`]'s empty-mailbox case would
    ///    report in reverse. A slot `task` holds — handed to it while it
    ///    waited, or kept as its wait reported the room ready — is its own,
    ///    and this send uses it.
    ///
    /// The sender's lane grows with its messages, the growth charged under
    /// `sender_limit` like each message and refused past it the same way.
    ///
    /// On success the payload is copied into a kernel-owned
    /// [`SensitiveBuffer`] — wiped when the message is released, however the
    /// send ends — and one [`AuditEvent::MessageDelivered`] is emitted.
    ///
    /// `task` is the sending thread's kernel-attested scheduler id, never a
    /// caller-supplied value; `0` names a sender with none, which holds no
    /// slot.
    ///
    /// # Errors
    ///
    /// As enumerated above, plus [`Errno::OutOfMemory`] if the kernel-owned
    /// copy of the payload, or room for it in the sender's lane, cannot be
    /// allocated.
    pub fn send<S: Sink + ?Sized>(
        &self,
        sender: &TaskCapabilities,
        task: u64,
        payload: &[u8],
        sender_limit: u64,
        audit: &S,
    ) -> Result<(), Errno> {
        // Stable field rendering for every audit branch.
        let mut id_buf = [0u8; 16];
        let mut sender_buf = [0u8; 16];
        let mut len_buf = [0u8; 12];
        let port_field = Field {
            key: "port",
            value: tairix_log::FieldValue::Str(format_hex_u64(self.id.0, &mut id_buf)),
        };
        let sender_field = Field {
            key: "sender",
            value: tairix_log::FieldValue::Str(format_hex_u64(sender.process().0, &mut sender_buf)),
        };
        let len_field = Field {
            key: "len",
            value: tairix_log::FieldValue::Str(format_usize(payload.len(), &mut len_buf)),
        };

        // 1. Fast path: reject sends to closed ports without locking.
        if self.state.load(Ordering::Acquire) == state::CLOSED {
            record(
                audit,
                AuditEvent::MessageSendToClosedPort,
                &[port_field, sender_field],
            );
            return Err(Errno::NotFound);
        }

        // 2. Capability check.
        if !self.required_send_caps.is_subset_of(sender.effective()) {
            record(
                audit,
                AuditEvent::MessageSendDenied,
                &[port_field, sender_field, SEND_DENIED_CAPABILITY],
            );
            return Err(Errno::PermissionDenied);
        }

        // 3. Size check (port-local plus global ABI cap). Compute the
        //    effective limit in `u64` so a port whose `max_payload`
        //    saturates `usize` on a 32-bit target (wasm32) still
        //    rejects oversize payloads correctly.
        let effective_max = u64::from(self.max_payload).min(u64::from(IPC_MESSAGE_MAX_PAYLOAD_LEN));
        let unaffordable = Self::message_charge(payload.len())
            .saturating_add(self.opening_charge())
            > sender_limit;
        if payload.len() as u64 > effective_max || unaffordable {
            record(
                audit,
                AuditEvent::MessageTooLarge,
                &[port_field, sender_field, len_field],
            );
            return Err(Errno::MessageTooLarge);
        }

        // 4. The sender pays for its own message until the owner takes it.
        let charge = match sender.charges().charge(
            LimitKind::IpcQueueBytes,
            Self::message_charge(payload.len()),
            sender_limit,
        ) {
            Ok(charge) => charge,
            Err(err) => {
                record(
                    audit,
                    AuditEvent::IpcSenderLimitReached,
                    &[port_field, sender_field],
                );
                return Err(err);
            }
        };

        // The kernel-owned copy is taken *before* the lock: the buffer wipes
        // itself on drop, so a send refused below leaves no plaintext behind,
        // and the allocation stays out of the critical section.
        let payload = match SensitiveBuffer::copy_from_slice(payload) {
            Ok(buf) => buf,
            Err(err) => {
                record(
                    audit,
                    AuditEvent::PayloadAllocFailed,
                    &[port_field, sender_field, len_field],
                );
                return Err(err.as_errno());
            }
        };

        let delivered = self.enqueue(
            sender,
            task,
            sender_limit,
            Message {
                sender: sender.process().0,
                origin: sender.attest_origin(),
                payload,
                _charge: charge,
            },
        );
        Self::record_send(delivered, [port_field, sender_field, len_field], audit)
    }

    /// Record how a send that reached the lock ended, and answer it.
    fn record_send<S: Sink + ?Sized>(
        delivered: Result<(), Refused>,
        [port_field, sender_field, len_field]: [Field<'_>; 3],
        audit: &S,
    ) -> Result<(), Errno> {
        let (event, answer) = match delivered {
            Ok(()) => (Some(AuditEvent::MessageDelivered), Ok(())),
            Err(Refused::Closed) => (
                Some(AuditEvent::MessageSendToClosedPort),
                Err(Errno::NotFound),
            ),
            // Only the first refusal under an admission is recorded, so a
            // sender the port will never take cannot flood the trail.
            Err(Refused::NotAdmitted { first }) => (
                first.then_some(AuditEvent::MessageSendDenied),
                Err(Errno::PermissionDenied),
            ),
            Err(Refused::Full) => (Some(AuditEvent::MailboxFull), Err(Errno::WouldBlock)),
            Err(Refused::OverLimit) => (
                Some(AuditEvent::IpcSenderLimitReached),
                Err(Errno::LimitExceeded),
            ),
            Err(Refused::NoMemory) => (
                Some(AuditEvent::PayloadAllocFailed),
                Err(Errno::OutOfMemory),
            ),
        };
        let fields: &[Field<'_>] = match event {
            Some(AuditEvent::MessageDelivered | AuditEvent::PayloadAllocFailed) => {
                &[port_field, sender_field, len_field]
            }
            Some(AuditEvent::MessageSendDenied) => {
                &[port_field, sender_field, SEND_DENIED_NOT_ADMITTED]
            }
            _ => &[port_field, sender_field],
        };
        if let Some(event) = event {
            record(audit, event, fields);
        }
        answer
    }

    /// Put `message` at the back of its sender's lane, once the lane has room.
    ///
    /// The lane is made outside the lock when the sender has none and no spare
    /// is free, and is paid for by the sender, as a spare it takes is; whatever
    /// is not kept — that lane, or the refused message with its payload and
    /// charge — drops after the lock is released.
    fn enqueue(
        &self,
        sender: &TaskCapabilities,
        task: u64,
        sender_limit: u64,
        message: Message,
    ) -> Result<(), Refused> {
        let instance = message.origin.proc_id();
        let mut message = Some(message);
        let mut fresh: Option<PortLane> = None;
        let delivered = loop {
            let mut q = self.mailbox.lock();
            // `destroy()` may have raced the send's fast path.
            if self.state.load(Ordering::Acquire) == state::CLOSED {
                break Err(Refused::Closed);
            }
            // A port whose owner named its one sender takes no other's
            // message, so knowing — or deriving — its id is not enough to
            // fill it and starve the sender it serves.
            if q.admitted.is_some_and(|admitted| admitted != instance) {
                q.refused = q.refused.saturating_add(1);
                let first = !core::mem::replace(&mut q.refusal_recorded, true);
                break Err(Refused::NotAdmitted { first });
            }
            // A sender is noted as its lane opens, so one that held a lane
            // already is noted.
            let made = match q.lanes.open(instance, &mut fresh) {
                Ok(Opened::Held) => false,
                Ok(Opened::Made) => true,
                Ok(Opened::NeedsFresh) => {
                    drop(q);
                    match self.fresh_lane(sender, sender_limit) {
                        Ok(lane) => fresh = Some(lane),
                        Err(refused) => break Err(refused),
                    }
                    continue;
                }
                Err(_) => break Err(Refused::NoMemory),
            };
            let per_sender = self.per_sender;
            let admitted = match q.lanes.get_mut(instance) {
                None => Err(Refused::NoMemory),
                // A spare is paid for by the sender that takes it.
                Some(lane) if lane.paid.is_none() => sender
                    .charges()
                    .charge(LimitKind::IpcQueueBytes, lane.storage(), sender_limit)
                    .map(|paid| lane.paid = Some(paid))
                    .map_err(|_| Refused::OverLimit),
                Some(_) => Ok(()),
            }
            .and_then(|()| {
                // A lane at its depth takes nothing whatever the room records
                // say, and a slot handed to a sibling waiting for room is that
                // sibling's.
                let free = self.free_in(q.lanes.get(instance));
                if free > 0 && q.room.has_room(task, instance, free) {
                    Ok(())
                } else {
                    Err(Refused::Full)
                }
            })
            .and_then(|()| match q.lanes.get_mut(instance) {
                Some(lane) => lane.reserve_one(per_sender, sender_limit),
                None => Err(Refused::NoMemory),
            });
            let admitted = if made {
                admitted.and_then(|()| {
                    q.noted
                        .note(instance, self.id, self.is_closed())
                        .map_err(|_| Refused::NoMemory)
                })
            } else {
                admitted
            };
            if let Err(refused) = admitted {
                let idle = q.lanes.retire_if_idle(instance);
                drop(q);
                drop(idle);
                break Err(refused);
            }
            q.lanes.join_turn(instance);
            let mailbox = &mut *q;
            let (Some(lane), Some(message)) = (mailbox.lanes.get_mut(instance), message.take())
            else {
                break Err(Refused::NoMemory);
            };
            lane.push(message);
            mailbox.queued += 1;
            // A slot the sending thread held is used.
            mailbox.room.consume(task);
            break Ok(());
        };
        drop(fresh);
        drop(message);
        delivered
    }

    /// A lane for `sender`, made outside any lock and paid for by it.
    fn fresh_lane(
        &self,
        sender: &TaskCapabilities,
        sender_limit: u64,
    ) -> Result<PortLane, Refused> {
        let mut lane = PortLane::fresh(self.per_sender).map_err(|_| Refused::NoMemory)?;
        lane.paid = Some(
            sender
                .charges()
                .charge(LimitKind::IpcQueueBytes, lane.storage(), sender_limit)
                .map_err(|_| Refused::OverLimit)?,
        );
        Ok(lane)
    }

    /// Take the next message in turn, if any, with the thread of its sender
    /// the slot its taking freed was handed to, for the receiver to wake once
    /// the port's lock is released: senders are taken in turn, each sender's
    /// own messages oldest first.
    ///
    /// Does *not* perform a capability check: the kernel decides whether a
    /// receiver may bind to a port at creation time, and the receiver does
    /// not re-check on every read. The message's
    /// [`origin`](Message::origin) names the sender whose lane now has room.
    /// Never allocates.
    pub fn recv(&self) -> Option<(Message, RoomWake<ProcId>)> {
        self.recv_with(|_| Ok::<(), core::convert::Infallible>(()))
            .and_then(Result::ok)
            .map(|((), message, freed)| (message, freed))
    }

    /// Deliver the next message in turn to `f`, dequeuing it only if `f`
    /// succeeds (peek-then-commit), and hand the dequeued message back with
    /// `f`'s result.
    ///
    /// The message stays at the head of its sender's lane while `f` runs and
    /// is removed only when `f` returns `Ok`. If `f` returns `Err` —
    /// for example the receiver's `copy_to_user` faulted, or the
    /// destination buffer was too small — the message is left queued, its
    /// sender keeping its turn, so a later receive re-delivers it rather than
    /// dropping it on the floor (fail closed). The mailbox lock is held for
    /// the duration of `f`, so the peek and the commit are atomic against a
    /// concurrent receive: two receivers can never observe the same message.
    /// The committed message is returned rather than dropped, so its payload
    /// is wiped and freed outside the lock, with the thread of its sender the
    /// slot its taking freed was handed to, for the receiver to wake.
    ///
    /// Returns `None` when the mailbox is empty (and `f` is not called),
    /// otherwise `Some` of whatever `f` returned. Like [`Self::recv`] it
    /// performs no capability check — the receiver's authority is fixed
    /// at bind time.
    pub fn recv_with<R, E>(
        &self,
        f: impl FnOnce(&Message) -> Result<R, E>,
    ) -> Option<Result<Received<R>, E>> {
        let mut q = self.mailbox.lock();
        let Mailbox {
            lanes,
            queued,
            ended,
            room,
            ..
        } = &mut *q;
        let sender = lanes.next_turn()?;
        let lane = lanes.get_mut(sender)?;
        let value = match f(lane.messages.front()?) {
            Ok(value) => value,
            Err(err) => return Some(Err(err)),
        };
        let message = lane.pop()?;
        let still_waiting = !lane.messages.is_empty();
        if lane.ended {
            // An ended sender's lane retires once drained; what it held is
            // given back to the room it was held against.
            let freed = Self::message_charge(message.payload.len())
                .saturating_add(if still_waiting { 0 } else { lane.storage() });
            *ended = ended.saturating_sub(freed);
        }
        let free = self.free_in(Some(&*lane));
        *queued -= 1;
        lanes.end_turn(still_waiting);
        // The slot it freed goes to the first thread of its sender waiting
        // for one, named for the receiver to wake.
        let freed = room.hand_out(sender, free);
        let idle = lanes.retire_if_idle(sender);
        drop(q);
        drop(idle);
        Some(Ok((value, message, freed)))
    }

    /// Admit `sender` as the only process instance whose messages the port
    /// takes from now on, replacing any admitted before, and discard every
    /// queued message another instance sent — it arrived before the owner
    /// named its sender. Answers whether anything was discarded, so the
    /// caller can wake the senders parked for room, who now find their sends
    /// refused. The discarded lanes are wiped and freed outside the mailbox
    /// lock.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfMemory`] when the room the admitted lane is kept in cannot
    /// be made; nothing changes.
    pub fn admit<S: Sink + ?Sized>(&self, sender: ProcId, audit: &S) -> Result<bool, Errno> {
        let room = KeepRoom::new()?;
        let (discarded, others) = {
            let mut q = self.mailbox.lock();
            let others = q.lanes.keep_only(sender, room);
            let discarded: usize = others.iter().map(|(_, lane)| lane.messages.len()).sum();
            let freed: u64 = others
                .iter()
                .filter(|(_, lane)| lane.ended)
                .map(|(_, lane)| lane.held())
                .fold(0, u64::saturating_add);
            q.queued -= discarded;
            q.ended = q.ended.saturating_sub(freed);
            q.admitted = Some(sender);
            q.refusal_recorded = false;
            (discarded, others)
        };
        drop(others);
        let mut id_buf = [0u8; 16];
        let mut sender_buf = [0u8; 2 * tairix_abi::PROC_ID_LEN];
        let mut discarded_buf = [0u8; 12];
        record(
            audit,
            AuditEvent::PortSenderAdmitted,
            &[
                Field {
                    key: "port",
                    value: tairix_log::FieldValue::Str(format_hex_u64(self.id.0, &mut id_buf)),
                },
                Field {
                    key: "admitted",
                    value: tairix_log::FieldValue::Str(format_hex_bytes(
                        &sender.to_le_bytes(),
                        &mut sender_buf,
                    )),
                },
                Field {
                    key: "discarded",
                    value: tairix_log::FieldValue::Str(format_usize(discarded, &mut discarded_buf)),
                },
            ],
        );
        Ok(discarded > 0)
    }

    /// Note that the process instance `sender` has ended. Its queued messages
    /// stay deliverable, held against the room the owner set aside for an
    /// ended sender's traffic while they fit it, and are discarded whole when
    /// they do not; nothing it waited for room with is woken.
    pub fn sender_ended<S: Sink + ?Sized>(&self, sender: ProcId, audit: &S) {
        let discarded = {
            let mut q = self.mailbox.lock();
            let Mailbox {
                lanes,
                queued,
                ended,
                noted,
                room,
                ..
            } = &mut *q;
            noted.sender_ended(sender);
            // Its threads have ended, so nothing it waited for room with is
            // woken.
            room.take_sender(sender);
            let held = lanes
                .get(sender)
                .filter(|lane| !lane.ended)
                .map(PortLane::held);
            match held {
                None => None,
                Some(held) if ended.saturating_add(held) <= self.ended_room => {
                    if let Some(lane) = lanes.get_mut(sender) {
                        lane.ended = true;
                    }
                    *ended += held;
                    None
                }
                Some(_) => {
                    let lane = lanes.remove(sender);
                    *queued -= lane.as_ref().map_or(0, |lane| lane.messages.len());
                    lane
                }
            }
        };
        let Some(lane) = discarded else {
            return;
        };
        let count = lane.messages.len();
        drop(lane);
        let mut id_buf = [0u8; 16];
        let mut sender_buf = [0u8; 2 * tairix_abi::PROC_ID_LEN];
        let mut count_buf = [0u8; 12];
        record(
            audit,
            AuditEvent::PortEndedSenderDiscarded,
            &[
                Field {
                    key: "port",
                    value: tairix_log::FieldValue::Str(format_hex_u64(self.id.0, &mut id_buf)),
                },
                Field {
                    key: "sender",
                    value: tairix_log::FieldValue::Str(format_hex_bytes(
                        &sender.to_le_bytes(),
                        &mut sender_buf,
                    )),
                },
                Field {
                    key: "discarded",
                    value: tairix_log::FieldValue::Str(format_usize(count, &mut count_buf)),
                },
            ],
        );
    }

    /// Number of messages currently buffered across every lane.
    ///
    /// Snapshot only — the value may change immediately under
    /// concurrent senders. Useful for assertions and for the test
    /// suite; production paths should not branch on this.
    #[must_use]
    pub fn len(&self) -> usize {
        self.mailbox.lock().queued
    }

    /// `true` if the mailbox currently has no buffered messages.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::RecordingSink;
    use alloc::vec::Vec;
    use tairix_abi::{CapabilityId, PROC_ID_LEN, RLIMIT_INFINITY};
    use tairix_kernel_sec::captable::ProcessId;
    use tairix_kernel_sec::identity::UserId;

    /// The per-sender depth of [`open_port`].
    const DEPTH: usize = 4;

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

    fn instance(byte: u8) -> ProcId {
        ProcId::from_raw([byte; PROC_ID_LEN])
    }

    /// A sending process instance of its own, holding `NET_RAW`.
    fn sender(id: u8) -> TaskCapabilities {
        task_with(u64::from(id), &[CapabilityId::NET_RAW]).with_proc_id(instance(id))
    }

    fn send(
        port: &Port,
        from: &TaskCapabilities,
        payload: &[u8],
        sink: &RecordingSink,
    ) -> Result<(), Errno> {
        port.send(from, 0, payload, RLIMIT_INFINITY, sink)
    }

    fn open_port() -> (RecordingSink, Port) {
        let sink = RecordingSink::new();
        let creator = task_with(
            1,
            &[CapabilityId::IPC_BIND_PRIVILEGED, CapabilityId::NET_RAW],
        );
        let p = Port::create(
            EndpointId(0xA),
            &creator,
            caps_of(&[CapabilityId::NET_RAW]),
            CapabilitySet::empty(),
            32,
            DEPTH,
            RLIMIT_INFINITY,
            &sink,
        )
        .expect("open port");
        (sink, p)
    }

    fn port_on(
        creator: &TaskCapabilities,
        id: u64,
        queue_limit: u64,
        sink: &RecordingSink,
    ) -> Result<Port, Errno> {
        Port::create(
            EndpointId(id),
            creator,
            CapabilitySet::empty(),
            CapabilitySet::empty(),
            64,
            DEPTH,
            queue_limit,
            sink,
        )
    }

    fn payloads(port: &Port) -> Vec<Vec<u8>> {
        core::iter::from_fn(|| port.recv())
            .map(|(message, _)| message.payload.as_bytes().to_vec())
            .collect()
    }

    #[test]
    fn create_rejects_oversize_max_payload() {
        let sink = RecordingSink::new();
        let creator = task_with(1, &[]);
        let err = Port::create(
            EndpointId(1),
            &creator,
            CapabilitySet::empty(),
            CapabilitySet::empty(),
            IPC_MESSAGE_MAX_PAYLOAD_LEN + 1,
            8,
            RLIMIT_INFINITY,
            &sink,
        )
        .err()
        .expect("oversize is refused");
        assert_eq!(err, Errno::LengthOutOfRange);
        assert!(sink.ids().contains(&AuditEvent::PortCreateDenied.id().0));
    }

    #[test]
    fn create_charges_the_owner_its_bookkeeping_and_one_lane_until_the_port_drops() {
        let sink = RecordingSink::new();
        let creator = task_with(1, &[]);
        let deepest = Port::reservation(IPC_MESSAGE_MAX_PAYLOAD_LEN, IPC_SENDER_DEPTH_MAX);
        let held = || creator.charges().held(LimitKind::IpcQueueBytes);
        let deep = Port::create(
            EndpointId(0x10),
            &creator,
            CapabilitySet::empty(),
            CapabilitySet::empty(),
            IPC_MESSAGE_MAX_PAYLOAD_LEN,
            IPC_SENDER_DEPTH_MAX,
            RLIMIT_INFINITY,
            &sink,
        )
        .expect("fits");
        assert_eq!(
            held(),
            deepest,
            "the owner pays its bookkeeping and one lane for ended senders"
        );
        assert_eq!(
            deepest - Port::ended_room(IPC_MESSAGE_MAX_PAYLOAD_LEN, IPC_SENDER_DEPTH_MAX),
            Port::reservation(64, DEPTH) - Port::ended_room(64, DEPTH),
            "never a live sender's messages, however large or deep"
        );
        drop(deep);
        let reservation = Port::reservation(64, DEPTH);
        let first = port_on(&creator, 0x11, 2 * reservation, &sink).expect("fits");
        let second = port_on(&creator, 0x12, 2 * reservation, &sink).expect("fits");
        assert_eq!(held(), 2 * reservation);
        assert_eq!(
            port_on(&creator, 0x13, 2 * reservation, &sink).map(|p| p.id()),
            Err(Errno::LimitExceeded)
        );
        assert_eq!(held(), 2 * reservation, "a refused port takes nothing");
        let denied = AuditEvent::PortCreateDenied.id().0;
        assert_eq!(sink.ids().iter().filter(|&&id| id == denied).count(), 1);
        drop(first);
        assert_eq!(held(), reservation);
        drop(second);
        assert_eq!(held(), 0);
    }

    #[test]
    fn a_message_is_charged_to_its_sender_until_it_is_taken() {
        let (sink, port) = open_port();
        let from = sender(7);
        let held = || from.charges().held(LimitKind::IpcQueueBytes);
        let lane = lane_bytes(DEPTH.min(SPARE_LANE_ITEMS));
        send(&port, &from, b"hello", &sink).expect("delivered");
        send(&port, &from, b"hi", &sink).expect("delivered");
        assert_eq!(
            held(),
            lane + Port::message_charge(5) + Port::message_charge(2),
            "each message is charged its own length, and the lane they opened"
        );
        let first = port.recv().expect("first");
        assert_eq!(
            held(),
            lane + Port::message_charge(5) + Port::message_charge(2)
        );
        drop(first);
        assert_eq!(
            held(),
            lane + Port::message_charge(2),
            "taken and dropped, it is returned"
        );
        port.destroy(&sink);
        assert_eq!(held(), 0, "a destroyed port returns what it held");
    }

    #[test]
    fn a_sender_past_its_own_budget_is_refused_and_queues_nothing() {
        let (sink, port) = open_port();
        let from = sender(7);
        let limit = lane_bytes(DEPTH.min(SPARE_LANE_ITEMS)) + Port::message_charge(4);
        assert_eq!(port.send(&from, 0, b"four", limit, &sink), Ok(()));
        assert_eq!(
            port.send(&from, 0, b"more", limit, &sink),
            Err(Errno::LimitExceeded)
        );
        assert!(sink
            .ids()
            .contains(&AuditEvent::IpcSenderLimitReached.id().0));
        assert_eq!(port.len(), 1);
        assert_eq!(from.charges().held(LimitKind::IpcQueueBytes), limit);
        let other = sender(9);
        assert_eq!(
            port.send(&other, 0, b"mine", limit, &sink),
            Ok(()),
            "one sender's budget is never another's"
        );
    }

    /// A message the sender's limit could not hold were it holding nothing is
    /// too large rather than over budget: no release of what it holds could
    /// admit it, so a wait for room would never end. It is charged nothing.
    #[test]
    fn a_message_no_holding_could_afford_is_too_large() {
        let (sink, port) = open_port();
        let from = sender(7);
        let affordable = lane_bytes(DEPTH.min(SPARE_LANE_ITEMS)) + Port::message_charge(4);
        assert_eq!(
            port.send(&from, 0, b"four", affordable - 1, &sink),
            Err(Errno::MessageTooLarge)
        );
        assert_eq!(from.charges().held(LimitKind::IpcQueueBytes), 0);
        assert_eq!(port.len(), 0);
        assert!(sink.ids().contains(&AuditEvent::MessageTooLarge.id().0));
        assert_eq!(port.send(&from, 0, b"four", affordable, &sink), Ok(()));
    }

    /// A sender at its limit is refused even an empty message, and its lane
    /// grows only as far as it can pay: what it holds never passes its limit.
    #[test]
    fn a_senders_limit_bounds_empty_messages_and_its_lane_alike() {
        let sink = RecordingSink::new();
        let creator = task_with(1, &[]);
        let port = Port::create(
            EndpointId(0x16),
            &creator,
            CapabilitySet::empty(),
            CapabilitySet::empty(),
            16,
            2 * SPARE_LANE_ITEMS,
            RLIMIT_INFINITY,
            &sink,
        )
        .expect("bound");
        let empty = Port::message_charge(0);
        assert!(empty > 0, "an empty message is not free");
        let lane = lane_bytes(SPARE_LANE_ITEMS);
        let made = u64::try_from(SPARE_LANE_ITEMS).expect("small");
        let exact = sender(7);
        let filled = lane + made * empty;
        for _ in 0..SPARE_LANE_ITEMS {
            assert_eq!(port.send(&exact, 0, b"", filled, &sink), Ok(()));
        }
        assert_eq!(
            port.send(&exact, 0, b"", filled, &sink),
            Err(Errno::LimitExceeded),
            "at its limit, not even an empty message"
        );
        let roomy = sender(8);
        let limit = filled + empty;
        for _ in 0..SPARE_LANE_ITEMS {
            assert_eq!(port.send(&roomy, 0, b"", limit, &sink), Ok(()));
        }
        assert_eq!(
            port.send(&roomy, 0, b"", limit, &sink),
            Err(Errno::LimitExceeded),
            "growing the lane would carry it past its limit"
        );
        assert!(roomy.charges().held(LimitKind::IpcQueueBytes) <= limit);
        assert_eq!(port.len(), 2 * SPARE_LANE_ITEMS);
        assert!(sink
            .ids()
            .contains(&AuditEvent::IpcSenderLimitReached.id().0));
    }

    /// Room counts all the sender holds, anywhere, and what its next message
    /// needs — the port's largest, and the lane the send opens or grows —
    /// against its limit, and names what a send would wait for.
    #[test]
    fn room_counts_all_the_sender_holds_and_what_its_next_message_needs() {
        let sink = RecordingSink::new();
        let creator = task_with(1, &[]);
        let port = Port::create(
            EndpointId(0x17),
            &creator,
            CapabilitySet::empty(),
            CapabilitySet::empty(),
            32,
            2 * SPARE_LANE_ITEMS,
            RLIMIT_INFINITY,
            &sink,
        )
        .expect("bound");
        let from = sender(7);
        let held = || from.charges().held(LimitKind::IpcQueueBytes);
        let largest = Port::largest_charge(port.max_payload());
        let opening = lane_bytes(SPARE_LANE_ITEMS);
        assert_eq!(
            port.room_for(&from, 0, 0),
            Room::Free,
            "holding nothing, no release could make room, so the send answers"
        );
        let other = from
            .charges()
            .charge(LimitKind::IpcQueueBytes, 1, RLIMIT_INFINITY)
            .expect("held elsewhere");
        assert_eq!(port.room_for(&from, 0, 1 + opening + largest), Room::Free);
        assert_eq!(
            port.room_for(&from, 0, opening + largest),
            Room::OverBudget,
            "with no lane here, the lane it would open is paid for too"
        );
        drop(other);
        send(&port, &from, b"x", &sink).expect("fits");
        assert_eq!(port.room_for(&from, 0, held() + largest), Room::Free);
        assert_eq!(
            port.room_for(&from, 0, held() + largest - 1),
            Room::OverBudget,
            "one byte short of the largest"
        );
        let elsewhere = from
            .charges()
            .charge(LimitKind::IpcQueueBytes, 1 << 20, RLIMIT_INFINITY)
            .expect("held elsewhere");
        assert_eq!(
            port.room_for(&from, 0, held() - elsewhere.units() + largest),
            Room::OverBudget,
            "what it holds elsewhere is counted"
        );
        drop(elsewhere);
        for _ in 1..SPARE_LANE_ITEMS {
            send(&port, &from, b"x", &sink).expect("fits");
        }
        let grown = lane_bytes(2 * SPARE_LANE_ITEMS) - lane_bytes(SPARE_LANE_ITEMS);
        assert_eq!(
            port.room_for(&from, 0, held() + largest),
            Room::OverBudget,
            "a full lane grows to take the next"
        );
        assert_eq!(
            port.room_for(&from, 0, held() + largest + grown),
            Room::Free
        );
        for _ in 0..SPARE_LANE_ITEMS {
            send(&port, &from, b"x", &sink).expect("fits");
        }
        assert_eq!(
            port.room_for(&from, 0, u64::MAX),
            Room::Full,
            "at its depth, whatever its budget"
        );
    }

    #[test]
    fn a_malformed_bound_is_refused_as_malformed_before_it_is_priced() {
        let sink = RecordingSink::new();
        let creator = task_with(1, &[]);
        for (payload, depth) in [
            (IPC_MESSAGE_MAX_PAYLOAD_LEN + 1, 4),
            (64, IPC_SENDER_DEPTH_MAX + 1),
            (64, 0),
        ] {
            let err = Port::create(
                EndpointId(0x14),
                &creator,
                CapabilitySet::empty(),
                CapabilitySet::empty(),
                payload,
                depth,
                0,
                &sink,
            )
            .err();
            assert_eq!(err, Some(Errno::LengthOutOfRange), "{payload} / {depth}");
        }
        assert_eq!(
            Port::message_charge(usize::MAX),
            u64::MAX,
            "an absurd length saturates to more than any limit allows"
        );
    }

    #[test]
    fn the_deepest_per_sender_bound_is_accepted() {
        let sink = RecordingSink::new();
        let creator = task_with(1, &[]);
        let port = Port::create(
            EndpointId(0x15),
            &creator,
            CapabilitySet::empty(),
            CapabilitySet::empty(),
            8,
            IPC_SENDER_DEPTH_MAX,
            RLIMIT_INFINITY,
            &sink,
        )
        .expect("the ceiling is a legal depth");
        assert_eq!(port.per_sender(), IPC_SENDER_DEPTH_MAX);
    }

    #[test]
    fn create_rejects_recv_caps_not_held_by_creator() {
        let sink = RecordingSink::new();
        let creator = task_with(1, &[]); // holds nothing
        let required_recv = caps_of(&[CapabilityId::AUDIT_READ]);
        let err = Port::create(
            EndpointId(3),
            &creator,
            CapabilitySet::empty(),
            required_recv,
            128,
            8,
            RLIMIT_INFINITY,
            &sink,
        )
        .err()
        .expect("must not grant unheld authority");
        assert_eq!(err, Errno::PermissionDenied);
        assert!(sink.ids().contains(&AuditEvent::PortCreateDenied.id().0));
    }

    #[test]
    fn create_requires_ipc_bind_privileged_for_restricted_sender() {
        let sink = RecordingSink::new();
        // Creator holds the send-caps but lacks IPC_BIND_PRIVILEGED;
        // therefore may not bind a port that restricts its senders.
        let creator = task_with(1, &[CapabilityId::NET_RAW]);
        let required_send = caps_of(&[CapabilityId::NET_RAW]);
        let err = Port::create(
            EndpointId(4),
            &creator,
            required_send,
            CapabilitySet::empty(),
            128,
            8,
            RLIMIT_INFINITY,
            &sink,
        )
        .err()
        .expect("privileged bind requires IPC_BIND_PRIVILEGED");
        assert_eq!(err, Errno::PermissionDenied);
    }

    #[test]
    fn create_succeeds_for_authorised_creator() {
        let sink = RecordingSink::new();
        let creator = task_with(
            1,
            &[CapabilityId::IPC_BIND_PRIVILEGED, CapabilityId::NET_RAW],
        );
        let p = Port::create(
            EndpointId(5),
            &creator,
            caps_of(&[CapabilityId::NET_RAW]),
            CapabilitySet::empty(),
            128,
            8,
            RLIMIT_INFINITY,
            &sink,
        )
        .expect("authorised");
        assert_eq!(p.id(), EndpointId(5));
        assert_eq!(p.max_payload(), 128);
        assert_eq!(p.per_sender(), 8);
        assert!(p.is_empty());
        assert!(sink.ids().contains(&AuditEvent::PortCreated.id().0));
    }

    #[test]
    fn send_with_required_cap_succeeds_and_recv_returns_payload() {
        let (sink, port) = open_port();
        let from = sender(7);
        assert_eq!(send(&port, &from, b"hello", &sink), Ok(()));
        let (msg, _) = port.recv().expect("delivered");
        assert_eq!(msg.sender, 7);
        assert_eq!(msg.origin.proc_id(), instance(7));
        assert_eq!(msg.payload.as_bytes(), b"hello");
        assert!(sink.ids().contains(&AuditEvent::MessageDelivered.id().0));
    }

    #[test]
    fn send_without_required_cap_is_eperm_and_audited() {
        let (sink, port) = open_port();
        let from = task_with(7, &[]).with_proc_id(instance(7)); // missing NET_RAW
        assert_eq!(
            send(&port, &from, b"x", &sink),
            Err(Errno::PermissionDenied)
        );
        assert!(sink.ids().contains(&AuditEvent::MessageSendDenied.id().0));
        assert!(port.is_empty());
        assert_eq!(
            from.charges().held(LimitKind::IpcQueueBytes),
            0,
            "a refused send is charged nothing"
        );
    }

    #[test]
    fn an_admitted_sender_is_the_only_one_a_port_takes_from() {
        let (sink, port) = open_port();
        let admitted = sender(0x7A);
        let flooder = sender(0x9F);
        assert_eq!(send(&port, &flooder, b"before", &sink), Ok(()));
        assert_eq!(send(&port, &admitted, b"early", &sink), Ok(()));
        assert_eq!(
            port.admit(instance(0x7A), &sink),
            Ok(true),
            "the flooder's message is discarded"
        );
        assert!(sink.ids().contains(&AuditEvent::PortSenderAdmitted.id().0));
        assert_eq!(
            flooder.charges().held(LimitKind::IpcQueueBytes),
            0,
            "a discarded message returns its charge"
        );
        assert_eq!(
            port.recv().map(|(message, _)| message.origin.proc_id()),
            Some(instance(0x7A)),
            "only the admitted sender's message stays"
        );
        let denials = |sink: &RecordingSink| {
            sink.ids()
                .iter()
                .filter(|&&id| id == AuditEvent::MessageSendDenied.id().0)
                .count()
        };
        for _ in 0..8 {
            assert_eq!(
                send(&port, &flooder, b"junk", &sink),
                Err(Errno::PermissionDenied)
            );
        }
        assert_eq!(denials(&sink), 1, "a flood is recorded once");
        assert!(port.is_empty(), "the refused sends took no room");
        assert_eq!(send(&port, &admitted, b"notice", &sink), Ok(()));
        assert_eq!(
            port.admit(instance(0x7A), &sink),
            Ok(false),
            "nothing of another's to discard"
        );
        assert_eq!(
            port.admit(instance(0x9F), &sink),
            Ok(true),
            "a later admission replaces the earlier and discards its messages"
        );
        assert_eq!(
            send(&port, &admitted, b"late", &sink),
            Err(Errno::PermissionDenied)
        );
        assert_eq!(
            denials(&sink),
            2,
            "a new admission records its own first refusal"
        );
    }

    #[test]
    fn oversize_payload_is_emsgsize_and_audited() {
        let (sink, port) = open_port();
        let from = sender(7);
        let big = alloc::vec![0u8; port.max_payload() as usize + 1];
        assert_eq!(send(&port, &from, &big, &sink), Err(Errno::MessageTooLarge));
        assert!(sink.ids().contains(&AuditEvent::MessageTooLarge.id().0));
    }

    #[test]
    fn a_full_lane_is_audited_and_does_not_drop_existing_messages() {
        let (sink, port) = open_port();
        let from = sender(7);
        for _ in 0..DEPTH {
            send(&port, &from, b"x", &sink).expect("fits");
        }
        // A full lane is retryable back-pressure, not a malformed request,
        // and therefore distinct from the `LengthOutOfRange` a bad
        // configuration returns.
        assert_eq!(send(&port, &from, b"x", &sink), Err(Errno::WouldBlock));
        assert!(sink.ids().contains(&AuditEvent::MailboxFull.id().0));
        assert_eq!(port.len(), DEPTH);
    }

    /// A lane at its depth takes no message, whatever slot its room records
    /// claim a thread holds: the depth refuses on its own, failing closed
    /// should the records and the lane ever disagree.
    #[test]
    fn a_full_lane_refuses_a_holder_whatever_the_room_records_say() {
        let (sink, port) = open_port();
        let from = sender(7);
        for _ in 0..DEPTH {
            send(&port, &from, b"x", &sink).expect("fits");
        }
        assert!(
            port.mailbox.lock().room.look(11, from.proc_id(), 1),
            "records that disagree with the lane"
        );
        assert!(port.holds_room(11));
        assert_eq!(
            port.send(&from, 11, b"x", RLIMIT_INFINITY, &sink),
            Err(Errno::WouldBlock)
        );
        assert_eq!(port.len(), DEPTH, "the depth holds");
        drop(port.leave_room(11));
    }

    /// The lock-out a shared total allowed: one sender filling the port
    /// refused every other until the owner drained it.
    #[test]
    fn a_full_sender_never_refuses_another() {
        let (sink, port) = open_port();
        let flooder = sender(9);
        for _ in 0..DEPTH {
            send(&port, &flooder, b"junk", &sink).expect("fits");
        }
        assert_eq!(
            send(&port, &flooder, b"junk", &sink),
            Err(Errno::WouldBlock)
        );
        let other = sender(7);
        for _ in 0..DEPTH {
            assert_eq!(send(&port, &other, b"mine", &sink), Ok(()));
        }
        assert_eq!(port.len(), 2 * DEPTH);
    }

    #[test]
    fn senders_are_served_in_turn_each_oldest_first() {
        let (sink, port) = open_port();
        let (a, b, c) = (sender(1), sender(2), sender(3));
        for payload in [b"a1", b"a2", b"a3"] {
            send(&port, &a, payload, &sink).expect("a");
        }
        send(&port, &b, b"b1", &sink).expect("b");
        send(&port, &c, b"c1", &sink).expect("c");
        send(&port, &b, b"b2", &sink).expect("b");
        let order: Vec<Vec<u8>> = payloads(&port);
        let order: Vec<&[u8]> = order.iter().map(Vec::as_slice).collect();
        assert_eq!(
            order,
            [&b"a1"[..], b"b1", b"c1", b"a2", b"b2", b"a3"],
            "one from each sender a round, a deep backlog behind the rest"
        );
    }

    /// Room is the send-side peek a parked sender is woken on, so it must
    /// track the very condition `send` refuses on, for that sender alone.
    #[test]
    fn room_is_per_sender_and_tracks_what_send_refuses_on() {
        let (sink, port) = open_port();
        let full = sender(7);
        let idle = sender(9);
        assert_eq!(
            port.room_for(&full, 0, u64::MAX),
            Room::Free,
            "an empty lane has room"
        );
        for _ in 0..DEPTH {
            send(&port, &full, b"x", &sink).expect("fits");
        }
        assert_eq!(
            port.room_for(&full, 0, u64::MAX),
            Room::Full,
            "a full lane has none"
        );
        assert_eq!(
            port.room_for(&idle, 0, u64::MAX),
            Room::Free,
            "another sender's room is untouched"
        );
        assert_eq!(send(&port, &full, b"x", &sink), Err(Errno::WouldBlock));
        send(&port, &idle, b"y", &sink).expect("its own room");
        let (taken, _) = port.recv().expect("drains one");
        assert_eq!(taken.origin.proc_id(), instance(7));
        assert_eq!(
            port.room_for(&full, 0, u64::MAX),
            Room::Free,
            "one drain frees one slot"
        );
        send(&port, &full, b"x", &sink).expect("the freed slot");
    }

    fn named(wake: &RoomWake<ProcId>) -> Vec<u64> {
        let mut tasks: Vec<u64> = wake.tasks().collect();
        tasks.sort_unstable();
        tasks
    }

    /// Fill `from`'s lane, then leave `waiters` waiting for room as it.
    fn full_lane_waited_on(
        port: &Port,
        from: &TaskCapabilities,
        waiters: &[u64],
        sink: &RecordingSink,
    ) {
        for _ in 0..DEPTH {
            send(port, from, b"x", sink).expect("fits");
        }
        for &waiter in waiters {
            port.watch_room(waiter, from.proc_id());
        }
    }

    /// Two threads of one sender wait for room in its full lane: a receive
    /// frees one slot and wakes one of them, the first to wait, and no sibling
    /// may take that slot from it.
    #[test]
    fn one_receive_for_two_threads_waiting_as_one_sender_wakes_one() {
        let (sink, port) = open_port();
        let from = sender(7);
        full_lane_waited_on(&port, &from, &[11, 22], &sink);
        let (_, freed) = port.recv().expect("drains one");
        assert_eq!(named(&freed), [11], "the first to wait, alone");
        assert_eq!(port.room_for(&from, 22, u64::MAX), Room::Full);
        assert_eq!(
            port.send(&from, 22, b"x", RLIMIT_INFINITY, &sink),
            Err(Errno::WouldBlock),
            "the slot is not its sibling's"
        );
        assert_eq!(port.room_for(&from, 11, u64::MAX), Room::Free);
        assert_eq!(port.send(&from, 11, b"x", RLIMIT_INFINITY, &sink), Ok(()));
        assert!(port.leave_room(11).is_empty(), "a used slot is spent");
        let (_, freed) = port.recv().expect("drains another");
        assert_eq!(named(&freed), [22], "the next receive wakes the next");
        drop(port.leave_room(22));
    }

    /// A thread a receive woke that ends its wait without sending passes the
    /// slot to the next thread waiting as its sender.
    #[test]
    fn a_woken_sender_leaving_unsent_passes_the_wake_on() {
        let (sink, port) = open_port();
        let from = sender(7);
        full_lane_waited_on(&port, &from, &[11, 22], &sink);
        let (_, freed) = port.recv().expect("drains one");
        assert_eq!(named(&freed), [11]);
        assert_eq!(named(&port.leave_room(11)), [22]);
        assert_eq!(port.send(&from, 22, b"x", RLIMIT_INFINITY, &sink), Ok(()));
    }

    /// A wait that reported the room ready keeps its slot, held from its
    /// siblings until it sends, and handed to the next waiting sibling when it
    /// leaves instead: waits elsewhere, or ends.
    #[test]
    fn an_owing_sender_holds_its_slot_until_it_sends_and_hands_it_on_when_it_leaves() {
        let (sink, port) = open_port();
        let from = sender(7);
        full_lane_waited_on(&port, &from, &[11, 22], &sink);
        drop(port.recv().expect("drains one"));
        assert!(port.own_room(11), "the slot it was handed");
        assert_eq!(
            port.send(&from, 22, b"x", RLIMIT_INFINITY, &sink),
            Err(Errno::WouldBlock),
            "held from its siblings"
        );
        assert_eq!(
            port.send(&from, 11, b"x", RLIMIT_INFINITY, &sink),
            Ok(()),
            "until it sends"
        );
        drop(port.recv().expect("drains one"));
        assert!(port.own_room(22));
        port.watch_room(33, from.proc_id());
        assert_eq!(named(&port.leave_room(22)), [33], "or until it leaves");
        assert!(
            !port.own_room(44),
            "a thread that never waited owes nothing"
        );
        drop(port.leave_room(33));
    }

    /// A thread waiting again keeps its place in its sender's line.
    #[test]
    fn a_rewatch_keeps_its_place_in_line() {
        let (sink, port) = open_port();
        let from = sender(7);
        full_lane_waited_on(&port, &from, &[11, 22, 11, 22], &sink);
        let (_, freed) = port.recv().expect("drains one");
        assert_eq!(named(&freed), [11]);
        for waiter in [11, 22] {
            drop(port.leave_room(waiter));
        }
    }

    /// A receive frees a slot in one sender's lane alone, so no other sender's
    /// waiter is woken; a change to every lane at once wakes them all.
    #[test]
    fn a_receive_wakes_only_its_own_senders_waiter_and_a_teardown_every_one() {
        let (sink, port) = open_port();
        let (seven, nine) = (sender(7), sender(9));
        full_lane_waited_on(&port, &seven, &[11], &sink);
        full_lane_waited_on(&port, &nine, &[22], &sink);
        let (taken, freed) = port.recv().expect("seven's, first in turn");
        assert_eq!(taken.origin.proc_id(), instance(7));
        assert_eq!(named(&freed), [11]);
        assert_eq!(named(&port.room_wake_everyone()), [11, 22]);
        assert!(port.room_wake_everyone().is_empty(), "taken once");
    }

    /// A message outlives its sender, held against the room the owner set
    /// aside, and gives that room back once taken.
    #[test]
    fn an_ended_senders_messages_stay_deliverable_within_the_owners_room() {
        let (sink, port) = open_port();
        let gone = sender(0x31);
        for payload in [b"last".as_slice(), b"words"] {
            send(&port, &gone, payload, &sink).expect("sent");
        }
        port.watch_room(5, instance(0x31));
        port.sender_ended(instance(0x31), &sink);
        assert!(
            port.room_wake_everyone().is_empty(),
            "its waits went with it"
        );
        assert!(port.mailbox.lock().ended > 0);
        assert_eq!(payloads(&port), [b"last".to_vec(), b"words".to_vec()]);
        assert_eq!(
            port.mailbox.lock().ended,
            0,
            "drained, the room is free again"
        );
        assert!(!sink
            .ids()
            .contains(&AuditEvent::PortEndedSenderDiscarded.id().0));
    }

    /// The room for ended senders is bounded: senders that send and end
    /// cannot make a port hold more than its owner set aside, so the
    /// messages that would pass it are discarded and recorded.
    #[test]
    fn ended_senders_cannot_make_a_port_hold_more_than_its_owner_set_aside() {
        let (sink, port) = open_port();
        let per_sender = port.per_sender();
        let payload = [0xA5u8; 8];
        for id in 0x40..0x48u8 {
            let gone = sender(id);
            for _ in 0..per_sender {
                send(&port, &gone, &payload, &sink).expect("sent");
            }
            port.sender_ended(instance(id), &sink);
            let ended = port.mailbox.lock().ended;
            assert!(
                ended <= port.ended_room,
                "{ended} held past {}",
                port.ended_room
            );
        }
        assert!(sink
            .ids()
            .contains(&AuditEvent::PortEndedSenderDiscarded.id().0));
        let live = sender(0x7F);
        send(&port, &live, b"still taken", &sink).expect("a live sender is unaffected");
    }

    /// Admission discards the other lanes outside the mailbox lock, including
    /// an ended sender's, and gives back the room that one held.
    #[test]
    fn admission_discards_an_ended_senders_lane_and_its_room() {
        let (sink, port) = open_port();
        let gone = sender(0x31);
        send(&port, &gone, b"left", &sink).expect("sent");
        port.sender_ended(instance(0x31), &sink);
        assert!(port.mailbox.lock().ended > 0);
        assert_eq!(port.admit(instance(0x7A), &sink), Ok(true));
        assert_eq!(port.mailbox.lock().ended, 0);
        assert!(port.is_empty());
    }

    #[test]
    fn send_to_destroyed_port_fast_path_returns_not_found() {
        let (sink, port) = open_port();
        port.destroy(&sink);
        assert!(port.is_closed());
        let from = sender(7);
        assert_eq!(send(&port, &from, b"x", &sink), Err(Errno::NotFound));
        assert!(sink
            .ids()
            .contains(&AuditEvent::MessageSendToClosedPort.id().0));
    }

    #[test]
    fn destroy_drains_every_lane() {
        let (sink, port) = open_port();
        let (a, b) = (sender(7), sender(9));
        send(&port, &a, b"a", &sink).expect("a");
        send(&port, &b, b"b", &sink).expect("b");
        port.destroy(&sink);
        assert!(port.recv().is_none());
        assert!(!port.has_pending());
        // Subsequent sends are refused fail-closed.
        assert_eq!(send(&port, &a, b"c", &sink), Err(Errno::NotFound));
    }

    #[test]
    fn destroy_is_idempotent_and_records_each_attempt() {
        let (sink, port) = open_port();
        port.destroy(&sink);
        let n1 = sink.len();
        port.destroy(&sink);
        let n2 = sink.len();
        assert!(n2 > n1, "each destroy attempt is audited");
    }

    #[test]
    fn unrestricted_port_accepts_any_sender_and_recv_is_uncapped() {
        // A port that declares no required send caps may be used by
        // any task — but the kernel still enforces the size check.
        let sink = RecordingSink::new();
        let creator = task_with(1, &[]);
        let port = Port::create(
            EndpointId(0xB),
            &creator,
            CapabilitySet::empty(),
            CapabilitySet::empty(),
            16,
            2,
            RLIMIT_INFINITY,
            &sink,
        )
        .expect("open");
        let anyone = task_with(99, &[]).with_proc_id(instance(99)); // no caps at all
        send(&port, &anyone, b"ok", &sink).expect("anyone can send");
        let _ = port.recv().expect("delivered");
    }

    #[test]
    fn recv_with_on_empty_mailbox_does_not_call_the_closure() {
        let (_sink, port) = open_port();
        let mut called = false;
        let outcome = port.recv_with(|_msg| -> Result<(), ()> {
            called = true;
            Ok(())
        });
        assert!(outcome.is_none(), "empty mailbox yields None");
        assert!(!called, "the closure runs only when a message is present");
    }

    #[test]
    fn recv_with_commits_and_hands_back_the_message_when_the_closure_succeeds() {
        let (sink, port) = open_port();
        let from = sender(7);
        send(&port, &from, b"payload", &sink).expect("delivered");

        let (seen, message, _) = port
            .recv_with(|msg| -> Result<Vec<u8>, ()> { Ok(msg.payload.as_bytes().to_vec()) })
            .expect("a message was present")
            .expect("the closure succeeded");
        assert_eq!(seen, b"payload");
        assert_eq!(message.payload.as_bytes(), b"payload");
        // A successful closure commits the dequeue.
        assert!(port.is_empty());
    }

    #[test]
    fn recv_with_retains_the_message_and_its_turn_when_the_closure_fails() {
        let (sink, port) = open_port();
        let (a, b) = (sender(7), sender(9));
        send(&port, &a, b"first", &sink).expect("delivered");
        send(&port, &a, b"second", &sink).expect("delivered");
        send(&port, &b, b"other", &sink).expect("delivered");

        // A failing closure (e.g. a faulting `copy_to_user`) must leave
        // the message queued so it is not dropped on the floor.
        let outcome = port.recv_with(|_msg| -> Result<(), Errno> { Err(Errno::BadAddress) });
        assert!(matches!(outcome, Some(Err(Errno::BadAddress))));
        assert_eq!(port.len(), 3, "a failed receive drops nothing");

        // The very next receive still sees the same message, its sender's
        // turn kept.
        let (head, _) = port.recv().expect("head still present");
        assert_eq!(head.payload.as_bytes(), b"first");
        let (next, _) = port.recv().expect("next");
        assert_eq!(next.payload.as_bytes(), b"other");
    }

    #[test]
    fn a_drained_lane_is_kept_for_reuse() {
        let (sink, port) = open_port();
        let from = sender(7);
        for _ in 0..3 {
            send(&port, &from, b"x", &sink).expect("delivered");
            let _ = port.recv().expect("taken");
            assert_eq!(
                port.mailbox.lock().lanes.len(),
                0,
                "an empty lane is retired"
            );
        }
        assert_eq!(
            port.mailbox.lock().lanes.spares(),
            1,
            "and its storage kept, so the next send allocates no lane"
        );
    }

    /// A queued message holds what it carries and nothing more, its own record
    /// being the least any message is charged: the wake its taking produces
    /// is handed back beside it.
    ///
    /// Regression: every queued message carried that wake, which raised every
    /// message's least charge and every lane slot's storage.
    #[test]
    fn a_queued_message_is_no_larger_than_what_it_carries() {
        let carried = size_of::<u64>()
            + size_of::<Origin>()
            + size_of::<SensitiveBuffer>()
            + size_of::<Charge>();
        assert!(size_of::<Message>() < carried + align_of::<Message>());
    }
}
