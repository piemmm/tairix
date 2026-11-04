//! Capability-checked synchronous call/reply endpoints.
//!
//! A [`Port`](crate::port::Port) is fire-and-forget: a sender enqueues a
//! message and never hears back. A request/reply service instead needs
//! **synchronous** semantics: a caller posts a request, blocks until exactly
//! one matching reply arrives, and reads it. [`CallEndpoint`] is that
//! primitive.
//!
//! Like [`Port`](crate::port::Port) it is a kernel-owned endpoint identified
//! by a stable [`EndpointId`] and gated by capabilities (checked at create
//! and on every post; the single bound server does not re-check). Unlike a
//! port it correlates each request with one reply through an opaque,
//! unforgeable [`CallTicket`]:
//!
//! * a caller [`post`](CallEndpoint::post)s a request and receives a ticket;
//! * the server [`recv_call`](CallEndpoint::recv_call)s the next request in
//!   turn (moving it to an in-service table keyed by its ticket);
//! * the server [`reply`](CallEndpoint::reply)s with that ticket;
//! * the caller [`take_reply`](CallEndpoint::take_reply)s its ticket to claim
//!   the reply.
//!
//! # One lane per caller
//!
//! Every caller process's calls sit in a lane of their own
//! (`kernel/ipc/src/lanes.rs`), charged to that caller's own `ipc-queue-bytes`
//! limit as it grows and served in turn with every other caller's, each
//! caller's own requests oldest first. The endpoint's per-caller depth bounds
//! the calls a caller has waiting on the server, so no caller can make the
//! endpoint refuse another, and room comes back as the server answers. An
//! answer the caller has not collected is its own memory, paid for until it
//! collects it, and never holds that room, so a blocking call waits on the
//! server and never on its own caller. A caller holding
//! [`IPC_SENDER_DEPTH_MAX`] answers it has not collected is refused every post
//! until it holds fewer — the calls it posted below that may all be answered,
//! taking it up to its depth past it — so no search of a lane grows with the
//! caller's budget.
//!
//! # Not a scheduler primitive
//!
//! This type is the request/reply *state machine* only; it never blocks. The
//! blocking — the caller parking until its ticket is replied, the server
//! parking until a request arrives — is layered above through the same
//! park seam the IRQ wait and `wait` syscalls use (`kernel/core`), so the
//! primitive stays synchronous-test-friendly and free of any scheduler
//! dependency. A caller that cannot park spins in
//! [`CallEndpoint::await_settle`], for its answer or for room, never taking
//! the lock either must take.
//!
//! # Fail closed
//!
//! Every refused operation emits exactly one [`crate::audit`] event before
//! returning a stable [`Errno`]. A destroyed endpoint cancels every in-flight
//! ticket: an outstanding [`CallEndpoint::take_reply`] reports
//! [`ReplyOutcome::Cancelled`] so a parked caller wakes and abandons rather
//! than waiting forever.

extern crate alloc;

use alloc::collections::{TryReserveError, VecDeque};
use alloc::vec::Vec;

use tairix_abi::ipc::{IPC_MESSAGE_MAX_PAYLOAD_LEN, IPC_SENDER_DEPTH_MAX};
use tairix_abi::{Errno, LimitKind, Origin, ProcId};
use tairix_caps::CapabilitySet;
use tairix_collections::HashMap;
use tairix_hash::BuildFastHash;
use tairix_kernel_mem::SensitiveBuffer;
use tairix_kernel_sec::captable::TaskCapabilities;
use tairix_kernel_sec::Charge;
use tairix_log::{Field, Sink};
use tairix_util::fmt::{format_hex_u64, format_usize};

use crate::audit::{record, AuditEvent};
use crate::lanes::{
    lane_cost, Lane, Lanes, Opened, RoomWaiters, RoomWake, SPARE_LANES, SPARE_LANE_ITEMS,
};

/// Why a reply was refused: the server's payload exceeded `max_reply`.
const REPLY_DENIED_OVERSIZE: Field<'static> = Field {
    key: "reason",
    value: tairix_log::FieldValue::Str("oversize_reply"),
};

/// Why a reply was refused: no such in-flight call. The ticket timed out,
/// was cancelled, or is forged — a late reply after a missed deadline is the
/// common case and is preceded by its own `CallTimedOut` record.
const REPLY_DENIED_UNKNOWN_TICKET: Field<'static> = Field {
    key: "reason",
    value: tairix_log::FieldValue::Str("unknown_ticket"),
};
use crate::loom_compat::{AtomicU32, AtomicU64, Ordering};
use crate::port::EndpointId;

/// Fixed atomic states a [`CallEndpoint`] can be in, encoded into one
/// `AtomicU32` so the post fast path observes liveness without the lock.
mod state {
    /// Open and accepting requests.
    pub(super) const OPEN: u32 = 0;
    /// `destroy()` has begun; posters fail closed and in-flight callers
    /// observe [`super::ReplyOutcome::Cancelled`].
    pub(super) const CLOSED: u32 = 1;
}

/// The in-service table capacity an endpoint keeps once it falls idle; more
/// than that is handed back, so a burst of callers leaves no lasting cost.
const IDLE_IN_SERVICE: usize = 16;

/// What entitles a creator to bind an endpoint.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum BindAuthority {
    /// The creator's own capabilities.
    Capability,
    /// The live seat lease the handler attested, for a seat-scoped reserved
    /// id.
    SeatLease,
    /// A serve the kernel accepted, for an id in the served range.
    ServedVolume,
}

impl BindAuthority {
    /// How an attested bind's record names its authority.
    const fn as_str(self) -> &'static str {
        match self {
            Self::Capability => "capability",
            Self::SeatLease => "seat-lease",
            Self::ServedVolume => "served-volume",
        }
    }
}

/// An opaque, unforgeable handle correlating a posted request with its
/// reply.
///
/// Minted by [`CallEndpoint::post`] from a per-endpoint monotonic counter
/// and surrendered to [`CallEndpoint::take_reply`]. The newtype keeps call
/// tickets distinct from endpoint, task, and capability identifiers.
#[repr(transparent)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct CallTicket(pub u64);

/// How many calls a [`CallEndpoint`] had settled — answered, withdrawn, timed
/// out, or cancelled by its teardown — when [`CallEndpoint::settle_mark`] was
/// read.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct SettleMark(u64);

/// The outcome of a server-side [`CallEndpoint::recv_call`].
///
/// `recv_call` is *size-bounded*: it dequeues the next request only when it
/// fits the server's buffer, so a too-small buffer never silently drops a
/// queued request. The in-kernel server passes [`usize::MAX`] and so only ever
/// observes [`RecvCall::Empty`] or [`RecvCall::Received`].
#[derive(Debug)]
pub enum RecvCall {
    /// No request is pending; the server should park and retry (this never
    /// blocks).
    Empty,
    /// The next request is larger than the server's buffer; it is left
    /// queued and `request_len` is reported so the server can resize and
    /// retry (or fail the call closed). The kernel surfaces this as
    /// [`Errno::BufferTooSmall`].
    TooLarge {
        /// Byte length of the request the buffer could not hold.
        request_len: usize,
    },
    /// A request was dequeued for service.
    Received(ReceivedCall),
}

/// A request handed to the server by [`CallEndpoint::recv_call`].
#[derive(Debug)]
pub struct ReceivedCall {
    /// The ticket the server must [`reply`](CallEndpoint::reply) with.
    pub ticket: CallTicket,
    /// **Process** identifier of the caller that posted the request. The
    /// scheduler id of the *thread* its reply wakes is carried separately,
    /// because only that names the entity to wake.
    pub sender: u64,
    /// The request payload (kernel-owned copy), wiped when this value is
    /// dropped.
    pub request: SensitiveBuffer,
}

/// The result of [`CallEndpoint::take_reply`] for a given ticket.
#[derive(Debug)]
pub enum ReplyOutcome {
    /// The request has been posted (and perhaps received) but the server
    /// has not replied yet. The caller should park and retry.
    Pending,
    /// The reply is ready; its bytes are returned (wiped when dropped) and
    /// the ticket retired.
    Ready(SensitiveBuffer),
    /// The request's per-ticket deadline elapsed before a reply arrived; the
    /// ticket is retired so the caller fails closed with a timeout rather
    /// than parking forever on a wedged callee.
    TimedOut,
    /// The endpoint was destroyed before a reply arrived; the caller must
    /// abandon the call.
    Cancelled,
    /// The ticket is unknown to this (open) endpoint for this caller — never
    /// posted here by it, or already claimed. Fail closed.
    Unknown,
}

/// The size and depth bounds a [`CallEndpoint`] is created with.
///
/// Grouped into one value so [`CallEndpoint::create`] stays a small,
/// reviewable constructor rather than a long positional argument list, and
/// so a caller cannot transpose the two payload caps.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct CallEndpointLimits {
    /// Maximum request payload (bytes) [`CallEndpoint::post`] accepts.
    pub max_request: u32,
    /// Maximum reply payload (bytes) [`CallEndpoint::reply`] accepts.
    pub max_reply: u32,
    /// The most calls one caller process may have waiting on the server at
    /// once, posted or in service. A bound on each caller, never a total they
    /// share, so no caller can refuse another.
    pub per_sender: usize,
}

impl CallEndpointLimits {
    /// What the owner is charged against its `ipc-queue-bytes` limit: the
    /// endpoint's own bookkeeping and the spare lanes it keeps. Callers pay
    /// for their own lanes and calls ([`Self::call_charge`]).
    #[must_use]
    pub fn reservation(self) -> u64 {
        crate::queue_reservation(
            size_of::<CallEndpoint>() + SPARE_LANES * size_of::<CallLane>(),
            call_item_bytes(),
            0,
            SPARE_LANES * SPARE_LANE_ITEMS,
        )
    }

    /// What one outstanding call costs its caller against its own
    /// `ipc-queue-bytes` limit beyond its lane's storage: the larger of the
    /// two payloads, since a call holds its request until the server takes it
    /// and its reply until the caller collects it, and its place among the
    /// calls in service. Taken whole when the call is posted, so a reply never
    /// fails for its caller's budget.
    #[must_use]
    pub fn call_charge(self) -> u64 {
        crate::queue_reservation(
            0,
            SERVING_SLOT_BYTES,
            u64::from(self.max_request.max(self.max_reply)),
            1,
        )
    }
}

/// The storage one call takes in each of its lane's lists.
const fn call_item_bytes() -> usize {
    size_of::<PendingCall>() + size_of::<InServiceCall>() + size_of::<CompletedCall>()
}

/// A call's share of the in-service index: its slot and control byte, at
/// the lowest load the index runs at.
const SERVING_SLOT_BYTES: usize = 2 * (size_of::<(u64, u64)>() + 1);

/// The threads a call's answer wakes: the one awaiting it, and its poster,
/// which may be watching for it some other way.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Wakes {
    poster: u64,
    awaiter: Option<u64>,
}

impl Wakes {
    const fn posted_by(poster: u64) -> Self {
        Self {
            poster,
            awaiter: None,
        }
    }

    /// Make `waiter` the one thread awaiting the call.
    fn await_by(&mut self, waiter: u64) -> Result<(), Errno> {
        if self.awaiter.is_some_and(|awaiter| awaiter != waiter) {
            return Err(Errno::Busy);
        }
        self.awaiter = Some(waiter);
        Ok(())
    }
}

/// The threads a change to an endpoint leaves to be woken once its locks are
/// released: those a call's end concerns, and those waiting for the room it
/// freed.
#[must_use = "a thread left unwoken waits forever"]
#[derive(Debug)]
pub struct Woken {
    answer: Option<Wakes>,
    room: RoomWake<u64>,
}

impl Woken {
    /// Nobody to wake.
    pub const fn none() -> Self {
        Self {
            answer: None,
            room: RoomWake::none(),
        }
    }

    /// Every scheduler task to wake. A `0` is a poster with no scheduler
    /// identity, which only a broadcast reaches.
    pub fn tasks(&self) -> impl Iterator<Item = u64> + '_ {
        let answer = self.answer.map_or([None, None], |wakes| {
            [
                Some(wakes.poster),
                wakes.awaiter.filter(|&awaiter| awaiter != wakes.poster),
            ]
        });
        answer.into_iter().flatten().chain(self.room.tasks())
    }
}

/// One posted request awaiting receipt.
struct PendingCall {
    ticket: u64,
    /// Who the call's answer wakes, so the reply path wakes exactly those
    /// threads instead of broadcasting.
    wakes: Wakes,
    /// The caller's kernel-attested identity, snapshotted at post so the
    /// server reads the principal that made *this* call, immune to later
    /// changes to that task or to PID reuse.
    origin: Origin,
    /// Absolute monotonic deadline (ns) after which an unanswered request is
    /// reaped as [`ReplyOutcome::TimedOut`]; [`u64::MAX`] means none.
    deadline: u64,
    request: SensitiveBuffer,
    charge: Charge,
}

/// One call received by the server and awaiting [`CallEndpoint::reply`].
struct InServiceCall {
    ticket: u64,
    origin: Origin,
    wakes: Wakes,
    deadline: u64,
    charge: Charge,
}

/// One replied call awaiting its poster's [`CallEndpoint::take_reply`].
struct CompletedCall {
    ticket: u64,
    reply: SensitiveBuffer,
    _charge: Charge,
}

/// One caller's calls at an endpoint.
struct CallLane {
    /// Posted and not yet received, oldest first.
    pending: VecDeque<PendingCall>,
    /// Received and not yet answered.
    in_service: Vec<InServiceCall>,
    /// Answered and not yet collected: fewer than [`IPC_SENDER_DEPTH_MAX`]
    /// plus the depth, since a post is refused once [`IPC_SENDER_DEPTH_MAX`]
    /// wait.
    completed: Vec<CompletedCall>,
    /// Unanswered calls carrying a deadline, so a lane with none is never
    /// searched for one.
    timed: usize,
    /// What the caller pays for the lane's storage; `None` while it is a
    /// spare the owner holds.
    paid: Option<Charge>,
}

impl CallLane {
    /// A lane with room for the first few calls, made outside any lock.
    fn fresh(per_sender: usize) -> Result<Self, TryReserveError> {
        let room = per_sender.min(SPARE_LANE_ITEMS);
        let mut pending = VecDeque::new();
        pending.try_reserve_exact(room)?;
        let mut in_service = Vec::new();
        in_service.try_reserve_exact(room)?;
        let mut completed = Vec::new();
        completed.try_reserve_exact(room)?;
        Ok(Self {
            pending,
            in_service,
            completed,
            timed: 0,
            paid: None,
        })
    }

    /// Calls waiting on the server: what the depth bounds.
    fn unanswered(&self) -> usize {
        self.pending.len() + self.in_service.len()
    }

    fn outstanding(&self) -> usize {
        self.unanswered() + self.completed.len()
    }

    /// The bytes the lane's storage holds, whatever is in it.
    fn storage(&self) -> u64 {
        Self::storage_for(
            self.pending.capacity(),
            self.in_service.capacity(),
            self.completed.capacity(),
        )
    }

    /// The bytes a lane with room for these many calls in each list holds.
    fn storage_for(pending: usize, in_service: usize, completed: usize) -> u64 {
        let bytes = |n: usize| u64::try_from(n).unwrap_or(u64::MAX);
        bytes(lane_cost::<u64, Self>())
            .saturating_add(bytes(pending).saturating_mul(bytes(size_of::<PendingCall>())))
            .saturating_add(bytes(in_service).saturating_mul(bytes(size_of::<InServiceCall>())))
            .saturating_add(bytes(completed).saturating_mul(bytes(size_of::<CompletedCall>())))
    }

    /// Room for one more call in each list it will pass through, so its
    /// service and its answer need none later. The caller pays for the growth
    /// under `limit` before it is taken, so its limit bounds the lane too.
    fn reserve_post(&mut self, per_sender: usize, limit: u64) -> Result<(), Errno> {
        let pending =
            crate::grown_capacity(self.pending.capacity(), self.pending.len() + 1, per_sender);
        let in_service = crate::grown_capacity(
            self.in_service.capacity(),
            self.unanswered() + 1,
            per_sender,
        );
        // Calls posted below the uncollected bound may be answered past it.
        let completed = crate::grown_capacity(
            self.completed.capacity(),
            self.outstanding() + 1,
            IPC_SENDER_DEPTH_MAX - 1 + per_sender,
        );
        let wanted = Self::storage_for(pending, in_service, completed);
        let grown = wanted.saturating_sub(self.storage());
        if grown == 0 {
            return Ok(());
        }
        let Some(paid) = self.paid.as_mut() else {
            return Err(Errno::OutOfMemory);
        };
        paid.grow(grown, limit)?;
        let reserved = self
            .pending
            .try_reserve_exact(pending - self.pending.len())
            .is_ok()
            && self
                .in_service
                .try_reserve_exact(in_service - self.in_service.len())
                .is_ok()
            && self
                .completed
                .try_reserve_exact(completed - self.completed.len())
                .is_ok();
        // Paid for only as far as the heap granted it.
        let unspent = wanted.saturating_sub(self.storage());
        if let Some(paid) = self.paid.as_mut() {
            paid.shrink(unspent);
        }
        if reserved {
            Ok(())
        } else {
            Err(Errno::OutOfMemory)
        }
    }

    /// The deadlines of the unanswered calls.
    fn deadlines(&self) -> impl Iterator<Item = u64> + '_ {
        self.pending
            .iter()
            .map(|call| call.deadline)
            .chain(self.in_service.iter().map(|call| call.deadline))
    }
}

impl Lane for CallLane {
    fn is_idle(&self) -> bool {
        self.outstanding() == 0
    }

    fn is_reusable(&self) -> bool {
        self.pending.capacity() <= SPARE_LANE_ITEMS
            && self.in_service.capacity() <= SPARE_LANE_ITEMS
            && self.completed.capacity() <= SPARE_LANE_ITEMS
    }

    fn release(&mut self) -> Option<Charge> {
        self.paid.take()
    }
}

/// The caller of each call in service, by ticket. Tickets are numbers the
/// endpoint mints, never chosen by a caller, so the fast unkeyed hash is safe.
type Serving = HashMap<u64, u64, BuildFastHash>;

const fn serving() -> Serving {
    HashMap::with_hasher(BuildFastHash::new())
}

/// The lock-guarded request/reply state of a [`CallEndpoint`].
///
/// Only [`CallEndpoint::post`] grows anything here, fallibly, and it reserves
/// what the call will need later — its place among the calls in service, and
/// room in its lane for its service and its reply — so receive, reply and
/// collection never allocate and cannot fail for memory.
struct Inner {
    /// Next ticket value; monotonic for the life of the endpoint.
    next_ticket: u64,
    lanes: Lanes<u64, CallLane>,
    /// The caller of each call in service, so an answer finds its caller's
    /// lane at once.
    serving: Serving,
    /// Calls posted and not yet received, across every lane: each will need
    /// a place in `serving`.
    pending: usize,
}

impl Inner {
    /// Reserve everything `sender`'s next call will need once it is posted,
    /// its caller paying for the lane storage that takes under `limit`.
    fn reserve_post(&mut self, sender: u64, per_sender: usize, limit: u64) -> Result<(), Errno> {
        let pending = self.pending;
        let Some(lane) = self.lanes.get_mut(sender) else {
            return Err(Errno::OutOfMemory);
        };
        lane.reserve_post(per_sender, limit)?;
        if self.serving.try_reserve(pending + 1).is_err() {
            return Err(Errno::OutOfMemory);
        }
        self.lanes.join_turn(sender);
        Ok(())
    }

    /// The in-service index, handed back once the endpoint falls idle with
    /// more room than an idle endpoint keeps.
    fn shed_idle_room(&mut self) -> Option<Serving> {
        (self.lanes.len() == 0
            && self.serving.is_empty()
            && self.serving.capacity() > IDLE_IN_SERVICE)
            .then(|| core::mem::replace(&mut self.serving, serving()))
    }
}

/// A kernel-owned synchronous call/reply endpoint.
///
/// Construct with [`CallEndpoint::create`] (which performs the bind-time
/// capability check) and tear down with [`CallEndpoint::destroy`]. Callers
/// [`post`](Self::post) requests; the single bound server drains them with
/// [`recv_call`](Self::recv_call) and answers with [`reply`](Self::reply);
/// callers claim answers with [`take_reply`](Self::take_reply).
///
/// `CallEndpoint` is [`Sync`]: the [`SpinLock`](tairix_sync::SpinLock) makes
/// interior access exclusive and the state word is atomic, so it may be
/// shared by `&` across CPUs exactly like a [`Port`](crate::port::Port).
pub struct CallEndpoint {
    id: EndpointId,
    required_send_caps: CapabilitySet,
    required_recv_caps: CapabilitySet,
    max_request: u32,
    max_reply: u32,
    per_sender: usize,
    /// What each outstanding call costs its caller ([`CallEndpointLimits::call_charge`]).
    call_charge: u64,
    /// **Process** that created and serves this endpoint. The kernel tears
    /// the endpoint down when that process exits so in-flight callers are
    /// released fail-closed rather than blocked forever; a single thread of a
    /// multi-threaded server exiting does not take the endpoint with it.
    owner: u64,
    /// The instance of [`Self::owner`] that bound the endpoint. The endpoint
    /// can be reached after its owner has ended, and the number with it.
    owner_instance: ProcId,
    /// The serving task's *scheduler* id, recorded the first time the server
    /// receives on this endpoint (`0` until then). The post path wakes
    /// exactly this task instead of broadcasting to every parked server; a
    /// plain store/load suffices because the value is a wake *hint* —
    /// correctness is carried by the server's register-before-poll park loop.
    server_task: AtomicU64,
    /// Liveness read on the post fast path before taking the lock.
    state: AtomicU32,
    /// Calls settled so far, which a caller no scheduler can park watches
    /// ([`Self::await_settle`]) rather than polling `inner`.
    settled: AtomicU64,
    inner: tairix_sync::SpinLock<Inner>,
    /// Callers' threads parked in a blocking call until their own lane has
    /// room.
    room: tairix_sync::SpinLock<RoomWaiters<u64>>,
    /// What the owner is charged for the endpoint's bookkeeping, returned
    /// when the endpoint is dropped.
    _queue_charge: Charge,
}

impl CallEndpoint {
    /// Create a new capability-checked synchronous call endpoint.
    ///
    /// The authority model is identical to [`Port::create`](crate::port::Port::create):
    /// `creator` must already hold every capability in `required_recv_caps`
    /// (no ambient authority), and must additionally hold
    /// [`tairix_abi::CapabilityId::IPC_BIND_PRIVILEGED`] when
    /// `required_send_caps` is non-empty (a restricted-sender endpoint is by
    /// definition privileged) **or** when `id` is a reserved well-known
    /// service rendezvous ([`tairix_abi::ipc::is_reserved_endpoint`]): even
    /// an open bind on a reserved id would let an unprivileged squatter
    /// claim the rendezvous and receive traffic meant for the service (an
    /// elevation request carries an offered password), so it fails closed.
    ///
    /// # Errors
    ///
    /// * [`Errno::LengthOutOfRange`] if `max_request` or `max_reply` exceeds
    ///   [`IPC_MESSAGE_MAX_PAYLOAD_LEN`], or `per_sender` is `0` or above
    ///   [`IPC_SENDER_DEPTH_MAX`].
    /// * [`Errno::PermissionDenied`] if `creator` lacks the bind authority
    ///   above.
    /// * [`Errno::LimitExceeded`] if the endpoint's
    ///   [`reservation`](CallEndpointLimits::reservation) would carry the
    ///   creator past `queue_limit`, its `ipc-queue-bytes` limit.
    /// * [`Errno::OutOfMemory`] if the endpoint's bookkeeping cannot be
    ///   allocated.
    ///
    /// On any failure exactly one [`AuditEvent::CallEndpointCreateDenied`] is
    /// emitted; on success exactly one [`AuditEvent::CallEndpointCreated`],
    /// naming the process that now serves the endpoint.
    pub fn create<S: Sink + ?Sized>(
        id: EndpointId,
        creator: &TaskCapabilities,
        required_send_caps: CapabilitySet,
        required_recv_caps: CapabilitySet,
        limits: CallEndpointLimits,
        queue_limit: u64,
        audit: &S,
    ) -> Result<Self, Errno> {
        Self::create_gated(
            id,
            creator,
            required_send_caps,
            required_recv_caps,
            limits,
            queue_limit,
            BindAuthority::Capability,
            audit,
        )
    }

    /// Create a call endpoint on a **reserved** id whose bind authority is
    /// a fact the *syscall handler* kernel-attested instead of
    /// `CAP_IPC_BIND_PRIVILEGED` — today exactly one such fact exists: the
    /// creator holds a live seat lease, which entitles it to the
    /// seat-scoped desktop rendezvous — the window, notification, and
    /// Switchboard tray-summary channels
    /// (`tairix_abi::ipc::is_seat_scoped_endpoint`, `plans/APPWIN.md` AW3).
    /// The attestation must come from kernel state the handler resolved
    /// itself (the seat registry), never from anything the caller supplied.
    ///
    /// Everything else is [`Self::create`]: the limits are re-bounded, the
    /// creator must hold every required receive capability, and a
    /// restricted-**sender** endpoint still demands
    /// `CAP_IPC_BIND_PRIVILEGED` — the attestation substitutes only for
    /// the reserved-id half of the gate.
    ///
    /// # Errors
    ///
    /// As [`Self::create`].
    pub fn create_seat_attested<S: Sink + ?Sized>(
        id: EndpointId,
        creator: &TaskCapabilities,
        required_send_caps: CapabilitySet,
        required_recv_caps: CapabilitySet,
        limits: CallEndpointLimits,
        queue_limit: u64,
        audit: &S,
    ) -> Result<Self, Errno> {
        Self::create_gated(
            id,
            creator,
            required_send_caps,
            required_recv_caps,
            limits,
            queue_limit,
            BindAuthority::SeatLease,
            audit,
        )
    }

    /// Create the endpoint a filesystem driver serves its volume on, owned
    /// by `driver`, on an id in the served range
    /// (`tairix_abi::fsserve::is_served_endpoint`).
    ///
    /// The kernel creates it as part of a serve it accepted, so the bind
    /// authority is that acceptance rather than anything the driver holds.
    /// Its senders must hold `CAP_IPC_ENDPOINT` and, from user space, a grant
    /// for it that is never minted: only the kernel posts to it.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] for an id outside the served range, and
    /// otherwise as [`Self::create`].
    pub fn create_served<S: Sink + ?Sized>(
        id: EndpointId,
        driver: &TaskCapabilities,
        limits: CallEndpointLimits,
        queue_limit: u64,
        audit: &S,
    ) -> Result<Self, Errno> {
        if !tairix_abi::fsserve::is_served_endpoint(id.0) {
            Self::record_create_denied(id, audit);
            return Err(Errno::OutOfRange);
        }
        let mut senders = CapabilitySet::empty();
        senders.insert(tairix_abi::CapabilityId::IPC_ENDPOINT);
        Self::create_gated(
            id,
            driver,
            senders,
            CapabilitySet::empty(),
            limits,
            queue_limit,
            BindAuthority::ServedVolume,
            audit,
        )
    }

    /// Record a bind refusal a caller resolved for itself, so every denied
    /// bind reaches the log through one event id.
    ///
    /// The kernel's `call_create` handler owns one authority this module
    /// cannot see: a link supplier's endpoint is served only by the driver
    /// holding its node's duty, which lives in the address-space registry. It
    /// refuses such a bind before the endpoint exists and calls this so the
    /// decision is auditable exactly like a capability refusal.
    pub fn record_create_denied<S: Sink + ?Sized>(id: EndpointId, audit: &S) {
        let mut id_buf = [0u8; 16];
        record(
            audit,
            AuditEvent::CallEndpointCreateDenied,
            &[Field {
                key: "endpoint",
                value: tairix_log::FieldValue::Str(format_hex_u64(id.0, &mut id_buf)),
            }],
        );
    }

    /// The one create path the public constructors share, `authority` saying
    /// what entitles the creator to bind `id`.
    #[allow(
        clippy::too_many_arguments,
        reason = "the two public constructors' inputs plus the one that tells them apart"
    )]
    fn create_gated<S: Sink + ?Sized>(
        id: EndpointId,
        creator: &TaskCapabilities,
        required_send_caps: CapabilitySet,
        required_recv_caps: CapabilitySet,
        limits: CallEndpointLimits,
        queue_limit: u64,
        authority: BindAuthority,
        audit: &S,
    ) -> Result<Self, Errno> {
        let CallEndpointLimits {
            max_request,
            max_reply,
            per_sender,
        } = limits;
        let mut id_buf = [0u8; 16];
        let id_field = Field {
            key: "endpoint",
            value: tairix_log::FieldValue::Str(format_hex_u64(id.0, &mut id_buf)),
        };

        if max_request > IPC_MESSAGE_MAX_PAYLOAD_LEN
            || max_reply > IPC_MESSAGE_MAX_PAYLOAD_LEN
            || per_sender == 0
            || per_sender > IPC_SENDER_DEPTH_MAX
        {
            record(audit, AuditEvent::CallEndpointCreateDenied, &[id_field]);
            return Err(Errno::LengthOutOfRange);
        }

        // The kernel-driven block-device identities name devices the kernel
        // drives itself, not rendezvous, and a consumer that groups volumes by
        // the device serving them relies on the two spaces being disjoint: an
        // endpoint sharing an identity with the boot disk would fold its
        // counters into that disk's and let its volumes answer for the disk's
        // removal check. Refused outright rather than merely privileged, so no
        // holder of any capability can create the collision.
        if tairix_abi::blkio::is_kernel_block_device(id.0) {
            record(audit, AuditEvent::CallEndpointCreateDenied, &[id_field]);
            return Err(Errno::OutOfRange);
        }
        // A served volume's endpoint is the kernel's to make, so a squatter
        // can never stand in for the driver of a volume it was not given.
        if tairix_abi::fsserve::is_served_endpoint(id.0)
            != matches!(authority, BindAuthority::ServedVolume)
        {
            record(audit, AuditEvent::CallEndpointCreateDenied, &[id_field]);
            return Err(Errno::OutOfRange);
        }

        if !required_recv_caps.is_subset_of(creator.effective()) {
            record(audit, AuditEvent::CallEndpointCreateDenied, &[id_field]);
            return Err(Errno::PermissionDenied);
        }

        // A restricted-sender endpoint is privileged unless the kernel made
        // it for a serve it accepted; a reserved id is privileged unless the
        // handler kernel-attested the live seat lease.
        let sender_unauthorized =
            !required_send_caps.is_empty() && !matches!(authority, BindAuthority::ServedVolume);
        let reserved_unauthorized = tairix_abi::ipc::is_reserved_endpoint(id.0)
            && !matches!(authority, BindAuthority::SeatLease);
        if (sender_unauthorized || reserved_unauthorized)
            && !creator.has(tairix_abi::CapabilityId::IPC_BIND_PRIVILEGED)
        {
            record(audit, AuditEvent::CallEndpointCreateDenied, &[id_field]);
            return Err(Errno::PermissionDenied);
        }

        let queue_charge = match creator.charges().charge(
            LimitKind::IpcQueueBytes,
            limits.reservation(),
            queue_limit,
        ) {
            Ok(charge) => charge,
            Err(err) => {
                record(audit, AuditEvent::CallEndpointCreateDenied, &[id_field]);
                return Err(err);
            }
        };
        let mut lanes = Lanes::new();
        if lanes.try_reserve_spares().is_err() {
            record(audit, AuditEvent::CallEndpointCreateDenied, &[id_field]);
            return Err(Errno::OutOfMemory);
        }
        Self::record_created(id_field, creator, authority, audit);

        Ok(Self {
            id,
            required_send_caps,
            required_recv_caps,
            max_request,
            max_reply,
            per_sender,
            call_charge: limits.call_charge(),
            owner: creator.process().0,
            owner_instance: creator.proc_id(),
            server_task: AtomicU64::new(0),
            state: AtomicU32::new(state::OPEN),
            settled: AtomicU64::new(0),
            inner: tairix_sync::SpinLock::new(Inner {
                next_ticket: 0,
                lanes,
                serving: serving(),
                pending: 0,
            }),
            room: tairix_sync::SpinLock::new(RoomWaiters::new()),
            _queue_charge: queue_charge,
        })
    }

    /// Record a bind that succeeded. A bind the kernel attested is a distinct
    /// security decision, recorded distinguishably from a
    /// capability-authorised one.
    fn record_created<S: Sink + ?Sized>(
        id_field: Field<'_>,
        creator: &TaskCapabilities,
        authority: BindAuthority,
        audit: &S,
    ) {
        let server_field = Field {
            key: "process",
            value: tairix_log::FieldValue::UnsignedInt(creator.process().0),
        };
        let _ = match authority {
            BindAuthority::Capability => record(
                audit,
                AuditEvent::CallEndpointCreated,
                &[id_field, server_field],
            ),
            BindAuthority::SeatLease | BindAuthority::ServedVolume => record(
                audit,
                AuditEvent::CallEndpointCreated,
                &[
                    id_field,
                    server_field,
                    Field {
                        key: "bind",
                        value: tairix_log::FieldValue::Str(authority.as_str()),
                    },
                ],
            ),
        };
    }

    /// Endpoint identifier this endpoint was created with.
    #[must_use]
    pub fn id(&self) -> EndpointId {
        self.id
    }

    /// Maximum request payload (bytes) [`post`](Self::post) will accept.
    #[must_use]
    pub fn max_request(&self) -> u32 {
        self.max_request
    }

    /// Maximum reply payload (bytes) [`reply`](Self::reply) will accept.
    #[must_use]
    pub fn max_reply(&self) -> u32 {
        self.max_reply
    }

    /// The most calls one caller may have waiting on the server here at once.
    #[must_use]
    pub fn per_sender(&self) -> usize {
        self.per_sender
    }

    /// Capability set required of every caller.
    #[must_use]
    pub fn required_send_caps(&self) -> &CapabilitySet {
        &self.required_send_caps
    }

    /// Capability set required of the binder/server at create time.
    #[must_use]
    pub fn required_recv_caps(&self) -> &CapabilitySet {
        &self.required_recv_caps
    }

    /// Task id that created and serves this endpoint.
    ///
    /// The call-endpoint registry indexes by this so a task's endpoints can
    /// be torn down when it exits.
    #[must_use]
    pub fn owner(&self) -> u64 {
        self.owner
    }

    /// The process instance that bound this endpoint: what a mint aimed at
    /// its server resolves, where [`Self::owner`]'s number may since have
    /// been issued to a successor.
    #[must_use]
    pub fn owner_instance(&self) -> ProcId {
        self.owner_instance
    }

    /// Record the serving task's *scheduler* id so a post can wake exactly
    /// this server (see the `server_task` field). Called by the receive
    /// paths (the `call_recv` syscall handler and the in-kernel store serve
    /// loop) with the kernel-attested current task — never a caller-supplied
    /// value — after the endpoint-ownership gate has already passed, so it
    /// can only ever name the endpoint's own server. Idempotent; `0` is
    /// never a valid scheduler id and is rejected so the "unrecorded"
    /// sentinel cannot be forged back in.
    pub fn record_server_task(&self, sched_task: u64) {
        if sched_task != 0 {
            self.server_task.store(sched_task, Ordering::Release);
        }
    }

    /// The serving task's recorded *scheduler* id, or [`None`] before the
    /// server's first receive. The post path uses this to wake exactly the
    /// bound server; `None` falls back to the broadcast wake (fail-safe —
    /// a server that never received yet drains its queue on its first
    /// poll regardless).
    #[must_use]
    pub fn server_task(&self) -> Option<u64> {
        match self.server_task.load(Ordering::Acquire) {
            0 => None,
            id => Some(id),
        }
    }

    /// `true` once [`Self::destroy`] has run.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.state.load(Ordering::Acquire) == state::CLOSED
    }

    /// The calls settled so far, read without the endpoint's lock.
    ///
    /// Read it before the look an [`await_settle`](Self::await_settle)
    /// follows — a [`take_reply`](Self::take_reply), or a
    /// [`has_room_for`](Self::has_room_for) — so a call settling between the
    /// two still ends the wait.
    #[must_use]
    pub fn settle_mark(&self) -> SettleMark {
        SettleMark(self.settled.load(Ordering::Acquire))
    }

    /// Spin until a call is answered, withdrawn or timed out, or the endpoint
    /// torn down, after `mark` was read, or until `over` reports the wait
    /// over: the wait of a caller no scheduler can park, for its answer or for
    /// room. It never takes the endpoint's lock, which polling would keep
    /// retaking from the answer that needs it; once it returns for a settle, a
    /// look under the lock sees the change.
    pub fn await_settle(&self, mark: SettleMark, mut over: impl FnMut() -> bool) {
        tairix_sync::spinwait::spin_until(|| {
            self.settled.load(Ordering::Acquire) != mark.0 || over()
        });
    }

    /// Count a call settled. Called under `inner` once the change is in
    /// place, so a caller that reads the new count and then takes the lock
    /// finds it; the lock orders every count, so no locked read-modify-write
    /// is needed.
    fn settle(&self) {
        let settled = self.settled.load(Ordering::Relaxed);
        self.settled
            .store(settled.wrapping_add(1), Ordering::Release);
    }

    /// Number of outstanding calls (posted, in service, or unclaimed) across
    /// every caller.
    ///
    /// Snapshot only; production paths must not branch on it.
    #[must_use]
    pub fn outstanding(&self) -> usize {
        self.inner
            .lock()
            .lanes
            .iter()
            .map(|(_, lane)| lane.outstanding())
            .sum()
    }

    /// `true` if a posted request is waiting to be received (the readiness a
    /// wait-set member of kind `Endpoint` observes).
    ///
    /// Non-consuming: the subsequent [`recv_call`](Self::recv_call) is what
    /// takes the request. A closed endpoint is never ready (its lanes were
    /// cleared by [`destroy`](Self::destroy) and stay empty).
    #[must_use]
    pub fn has_pending(&self) -> bool {
        self.inner.lock().lanes.has_turn()
    }

    /// Mark the endpoint closed and cancel every in-flight call.
    ///
    /// Idempotent and fail-closed: subsequent [`post`](Self::post)s return
    /// [`Errno::NotFound`], outstanding [`take_reply`](Self::take_reply)s
    /// observe [`ReplyOutcome::Cancelled`], and any buffered request/reply
    /// bytes are wiped as they are dropped. Records one
    /// [`AuditEvent::CallEndpointDestroyed`]. Threads parked for room are
    /// woken by the teardown's broadcast on the call wait-queue, and find the
    /// endpoint gone.
    pub fn destroy<S: Sink + ?Sized>(&self, audit: &S) {
        self.state.store(state::CLOSED, Ordering::Release);
        // Taking the lanes and the in-service table wholesale is a pointer
        // swap, so the buffers wipe and free themselves as these locals drop
        // *after* the lock is released — the heap's own lock never nests
        // inside this endpoint's critical section.
        let (retired, in_service) = {
            let mut g = self.inner.lock();
            g.pending = 0;
            let taken = (
                g.lanes.take_all(),
                core::mem::replace(&mut g.serving, serving()),
            );
            self.settle();
            taken
        };
        let cancelled: usize = retired.iter().map(|(_, lane)| lane.outstanding()).sum();
        drop(in_service);
        drop(retired);
        let mut id_buf = [0u8; 16];
        let mut n_buf = [0u8; 12];
        record(
            audit,
            AuditEvent::CallEndpointDestroyed,
            &[
                Field {
                    key: "endpoint",
                    value: tairix_log::FieldValue::Str(format_hex_u64(self.id.0, &mut id_buf)),
                },
                Field {
                    key: "cancelled",
                    value: tairix_log::FieldValue::Str(format_usize(cancelled, &mut n_buf)),
                },
            ],
        );
    }

    /// Cancel every in-flight call posted by the process `sender`, which has
    /// exited: queued requests are dropped before service, in-service
    /// tickets are retired (a later [`reply`](Self::reply) for one is
    /// refused fail-closed, its bytes going nowhere), and unclaimed
    /// replies are discarded, every buffer wiped as it drops. Returns the
    /// number of calls cancelled.
    ///
    /// Without this, a dead caller's queued request would later be served
    /// on its ticket — the observed metal defect: an unloaded USB class
    /// driver's final request survived its death, was held by the
    /// host-controller driver after a fault recovery, and wedged the
    /// single-URB transport against the replacement driver. Records one
    /// [`AuditEvent::CallPosterVanished`] when anything was cancelled.
    pub fn cancel_posted_by<S: Sink + ?Sized>(&self, sender: u64, audit: &S) -> usize {
        // The lane is moved out whole, so nothing is freed inside the critical
        // section.
        let lane = {
            let mut g = self.inner.lock();
            let Inner {
                lanes,
                serving,
                pending,
                ..
            } = &mut *g;
            let Some(lane) = lanes.remove(sender) else {
                return 0;
            };
            *pending -= lane.pending.len();
            for call in &lane.in_service {
                serving.remove(&call.ticket);
            }
            self.settle();
            lane
        };
        // Its threads have ended, so nothing it waited for room with is woken.
        let forgotten = self.room.lock().take_waiting_as(sender);
        drop(forgotten);
        let cancelled = lane.outstanding();
        drop(lane);
        if cancelled > 0 {
            let mut id_buf = [0u8; 16];
            let mut sender_buf = [0u8; 16];
            let mut n_buf = [0u8; 12];
            record(
                audit,
                AuditEvent::CallPosterVanished,
                &[
                    Field {
                        key: "endpoint",
                        value: tairix_log::FieldValue::Str(format_hex_u64(self.id.0, &mut id_buf)),
                    },
                    Field {
                        key: "sender",
                        value: tairix_log::FieldValue::Str(format_hex_u64(sender, &mut sender_buf)),
                    },
                    Field {
                        key: "cancelled",
                        value: tairix_log::FieldValue::Str(format_usize(cancelled, &mut n_buf)),
                    },
                ],
            );
        }
        cancelled
    }

    /// Post `request` and obtain the [`CallTicket`] correlating its reply.
    ///
    /// The kernel enforces every check, mirroring [`Port::send`](crate::port::Port::send):
    ///
    /// 1. **Lock-free fast path.** A destroyed endpoint returns
    ///    [`Errno::NotFound`] without taking the lock and records one
    ///    [`AuditEvent::CallPostToClosedEndpoint`].
    /// 2. **Capability check.** Every capability in `required_send_caps` must
    ///    be in `caller.effective()`, else [`Errno::PermissionDenied`] +
    ///    [`AuditEvent::CallPostDenied`].
    /// 3. **Size check.** The request must be `<= max_request` (bounded again
    ///    by [`IPC_MESSAGE_MAX_PAYLOAD_LEN`]), else [`Errno::MessageTooLarge`]
    ///    + [`AuditEvent::CallRequestTooLarge`].
    /// 4. **Charge.** The call's [`call_charge`](CallEndpointLimits::call_charge)
    ///    is taken from the caller's own `ipc-queue-bytes` budget under
    ///    `sender_limit`, else [`Errno::LimitExceeded`] +
    ///    [`AuditEvent::IpcSenderLimitReached`].
    /// 5. **Uncollected check.** If the caller holds [`IPC_SENDER_DEPTH_MAX`]
    ///    or more answers it has not collected, [`Errno::LimitExceeded`] +
    ///    [`AuditEvent::IpcSenderLimitReached`], whatever its depth: only its
    ///    collecting enough to hold fewer lets it post again, so this refusal
    ///    is never waited out. The calls it posted below that may all be
    ///    answered, so it can come to hold up to its depth more.
    /// 6. **Depth check.** If the caller already has the endpoint's per-caller
    ///    depth of calls waiting on the server, [`Errno::WouldBlock`] +
    ///    [`AuditEvent::CallQueueFull`] — the server answering one of them is
    ///    what makes room. Its uncollected answers never count toward it.
    ///
    /// The caller's lane grows with its calls, the growth charged under
    /// `sender_limit` like the call itself and refused past it the same way.
    ///
    /// On success the request is copied into a kernel-owned
    /// [`SensitiveBuffer`] — so a request that carried a passphrase, key, or
    /// capability token is wiped when the endpoint releases it, however the
    /// call ends — a fresh ticket is minted, and one
    /// [`AuditEvent::CallPosted`] is emitted. The returned ticket is later
    /// surrendered to [`take_reply`](Self::take_reply) by the *same* caller
    /// process.
    ///
    /// `poster_sched` is the posting task's kernel-attested *scheduler* id
    /// (never a caller-supplied value): [`reply`](Self::reply) names it among
    /// the threads to wake so the syscall layer wakes exactly this caller
    /// rather than broadcasting to every parked one. An in-kernel poster with
    /// no scheduler identity passes `0`, which only the broadcast wake reaches.
    ///
    /// `deadline` is the absolute monotonic time (ns) after which an
    /// unanswered request is reaped as [`ReplyOutcome::TimedOut`] by
    /// [`take_reply`](Self::take_reply); [`u64::MAX`] means no deadline (the
    /// deadline-less `ipc_call` park).
    ///
    /// # Errors
    ///
    /// As enumerated above, plus [`Errno::OutOfMemory`] if the kernel-owned
    /// copy of the request, or the room the call will need, cannot be
    /// allocated.
    #[allow(
        clippy::too_many_arguments,
        reason = "who posts, what, until when, and the poster's own budget are independent inputs"
    )]
    pub fn post<S: Sink + ?Sized>(
        &self,
        caller: &TaskCapabilities,
        poster_sched: u64,
        request: &[u8],
        deadline: u64,
        sender_limit: u64,
        audit: &S,
    ) -> Result<CallTicket, Errno> {
        let mut id_buf = [0u8; 16];
        let mut sender_buf = [0u8; 16];
        let mut len_buf = [0u8; 12];
        let id_field = Field {
            key: "endpoint",
            value: tairix_log::FieldValue::Str(format_hex_u64(self.id.0, &mut id_buf)),
        };
        let sender_field = Field {
            key: "sender",
            value: tairix_log::FieldValue::Str(format_hex_u64(caller.process().0, &mut sender_buf)),
        };
        let len_field = Field {
            key: "len",
            value: tairix_log::FieldValue::Str(format_usize(request.len(), &mut len_buf)),
        };

        // 1. Fast path: reject posts to a closed endpoint without locking.
        if self.state.load(Ordering::Acquire) == state::CLOSED {
            record(
                audit,
                AuditEvent::CallPostToClosedEndpoint,
                &[id_field, sender_field],
            );
            return Err(Errno::NotFound);
        }

        // 2. Capability check.
        if !self.required_send_caps.is_subset_of(caller.effective()) {
            record(audit, AuditEvent::CallPostDenied, &[id_field, sender_field]);
            return Err(Errno::PermissionDenied);
        }

        // 3. Size check (endpoint-local plus the global ABI cap), computed in
        //    `u64` so a 32-bit target (wasm32) rejects oversize correctly.
        let effective_max = u64::from(self.max_request).min(u64::from(IPC_MESSAGE_MAX_PAYLOAD_LEN));
        if request.len() as u64 > effective_max {
            record(
                audit,
                AuditEvent::CallRequestTooLarge,
                &[id_field, sender_field, len_field],
            );
            return Err(Errno::MessageTooLarge);
        }

        // 4. The caller pays for its own call, whatever the server does.
        let charge =
            match caller
                .charges()
                .charge(LimitKind::IpcQueueBytes, self.call_charge, sender_limit)
            {
                Ok(charge) => charge,
                Err(err) => {
                    record(
                        audit,
                        AuditEvent::IpcSenderLimitReached,
                        &[id_field, sender_field],
                    );
                    return Err(err);
                }
            };

        // The kernel-owned copy is taken *before* the lock: the buffer wipes
        // itself on drop, so a request refused below leaves no plaintext
        // behind, and the allocation stays out of the critical section.
        let request = match SensitiveBuffer::copy_from_slice(request) {
            Ok(buf) => buf,
            Err(err) => {
                record(
                    audit,
                    AuditEvent::PayloadAllocFailed,
                    &[id_field, sender_field, len_field],
                );
                return Err(err.as_errno());
            }
        };

        let posted = self.enqueue(
            caller,
            sender_limit,
            PendingCall {
                ticket: 0,
                wakes: Wakes::posted_by(poster_sched),
                origin: caller.attest_origin(),
                deadline,
                request,
                charge,
            },
        );
        Self::record_post(posted, [id_field, sender_field, len_field], audit)
    }

    /// Record how a post that reached the lock ended, and answer it.
    fn record_post<S: Sink + ?Sized>(
        posted: Result<u64, Errno>,
        [id_field, sender_field, len_field]: [Field<'_>; 3],
        audit: &S,
    ) -> Result<CallTicket, Errno> {
        let (event, fields, answer) = match posted {
            Ok(ticket) => (
                AuditEvent::CallPosted,
                &[id_field, sender_field, len_field][..],
                Ok(CallTicket(ticket)),
            ),
            Err(Errno::NotFound) => (
                AuditEvent::CallPostToClosedEndpoint,
                &[id_field, sender_field][..],
                Err(Errno::NotFound),
            ),
            // The caller's own calls fill its depth; the server answering one is
            // what makes room, so this is back-pressure on the caller alone.
            Err(Errno::WouldBlock) => (
                AuditEvent::CallQueueFull,
                &[id_field, sender_field][..],
                Err(Errno::WouldBlock),
            ),
            // The caller's own limits refuse it, never the heap: its budget,
            // paying for its lane included, or the answers it left uncollected.
            Err(Errno::LimitExceeded) => (
                AuditEvent::IpcSenderLimitReached,
                &[id_field, sender_field][..],
                Err(Errno::LimitExceeded),
            ),
            Err(err) => (
                AuditEvent::PayloadAllocFailed,
                &[id_field, sender_field, len_field][..],
                Err(err),
            ),
        };
        record(audit, event, fields);
        answer
    }

    /// Put `call` at the back of its caller's lane, minting its ticket, once
    /// the lane has room for it and for everything it will need later.
    ///
    /// The lane is made outside the lock when the caller has none and no spare
    /// is free, and is paid for by the caller, as a spare it takes is; whatever
    /// is not kept — that lane, or the refused call with its payload and charge
    /// — drops after the lock is released.
    fn enqueue(
        &self,
        caller: &TaskCapabilities,
        sender_limit: u64,
        call: PendingCall,
    ) -> Result<u64, Errno> {
        let sender = caller.process().0;
        let mut fresh: Option<CallLane> = None;
        let mut call = Some(call);
        let posted = loop {
            let mut g = self.inner.lock();
            // `destroy()` may have raced the post's fast path.
            if self.state.load(Ordering::Acquire) == state::CLOSED {
                break Err(Errno::NotFound);
            }
            match g.lanes.open(sender, &mut fresh) {
                Ok(Opened::Held | Opened::Made) => {}
                Ok(Opened::NeedsFresh) => {
                    drop(g);
                    match self.fresh_lane(caller, sender_limit) {
                        Ok(lane) => fresh = Some(lane),
                        Err(err) => break Err(err),
                    }
                    continue;
                }
                Err(err) => break Err(err),
            }
            // A spare is paid for by the caller that takes it.
            let paid = match g.lanes.get_mut(sender) {
                Some(lane) if lane.paid.is_none() => caller
                    .charges()
                    .charge(LimitKind::IpcQueueBytes, lane.storage(), sender_limit)
                    .map(|paid| lane.paid = Some(paid)),
                Some(_) => Ok(()),
                None => Err(Errno::OutOfMemory),
            };
            // Past the ceiling no room waited for helps, so it refuses first.
            let room = match g.lanes.get(sender) {
                Some(lane) if lane.completed.len() >= IPC_SENDER_DEPTH_MAX => {
                    Err(Errno::LimitExceeded)
                }
                Some(lane) if lane.unanswered() >= self.per_sender => Err(Errno::WouldBlock),
                Some(_) => Ok(()),
                None => Err(Errno::OutOfMemory),
            };
            let admitted = paid
                .and(room)
                .and_then(|()| g.reserve_post(sender, self.per_sender, sender_limit));
            if let Err(err) = admitted {
                let idle = g.lanes.retire_if_idle(sender);
                drop(g);
                drop(idle);
                break Err(err);
            }
            let inner = &mut *g;
            let (Some(lane), Some(mut queued)) = (inner.lanes.get_mut(sender), call.take()) else {
                break Err(Errno::OutOfMemory);
            };
            let ticket = inner.next_ticket;
            inner.next_ticket += 1;
            inner.pending += 1;
            queued.ticket = ticket;
            lane.timed += usize::from(queued.deadline != u64::MAX);
            lane.pending.push_back(queued);
            break Ok(ticket);
        };
        drop(fresh);
        drop(call);
        posted
    }

    /// A lane for `caller`, made outside any lock and paid for by it.
    fn fresh_lane(&self, caller: &TaskCapabilities, sender_limit: u64) -> Result<CallLane, Errno> {
        let mut lane = CallLane::fresh(self.per_sender).map_err(|_| Errno::OutOfMemory)?;
        lane.paid = Some(caller.charges().charge(
            LimitKind::IpcQueueBytes,
            lane.storage(),
            sender_limit,
        )?);
        Ok(lane)
    }

    /// Take the next request in turn for the server to service, if it fits a
    /// buffer of `max_copy` bytes.
    ///
    /// Callers are served in turn — each caller's own requests oldest first —
    /// so a caller with many requests queued waits behind one from every other
    /// caller rather than ahead of them all. Returns [`RecvCall::Empty`] when
    /// nothing is pending (the server should park and retry — this never
    /// blocks), [`RecvCall::TooLarge`] when the next request exceeds
    /// `max_copy` (left queued, its turn kept, so no request is lost), or
    /// [`RecvCall::Received`] with the call moved into service so a later
    /// [`reply`](Self::reply) can match it. Performs no capability check: the
    /// server's authority is fixed at [`create`](Self::create) time, exactly
    /// like [`Port::recv`](crate::port::Port::recv); the syscall layer gates
    /// the caller against [`required_recv_caps`](Self::required_recv_caps).
    /// Never allocates.
    #[must_use]
    pub fn recv_call(&self, max_copy: usize) -> RecvCall {
        let mut g = self.inner.lock();
        let Inner {
            lanes,
            serving,
            pending,
            ..
        } = &mut *g;
        let Some(sender) = lanes.next_turn() else {
            return RecvCall::Empty;
        };
        let Some(lane) = lanes.get_mut(sender) else {
            return RecvCall::Empty;
        };
        let Some(request_len) = lane.pending.front().map(|c| c.request.len()) else {
            return RecvCall::Empty;
        };
        // Refuse to dequeue a request the server's buffer cannot hold: leave
        // it queued and report its size so the server can resize (the kernel
        // maps this to `BufferTooSmall`).
        if request_len > max_copy {
            return RecvCall::TooLarge { request_len };
        }
        // The room was reserved when the call was posted, so neither record
        // below allocates; without it the call is left queued.
        if lane.in_service.len() == lane.in_service.capacity()
            || serving.len() == serving.capacity()
        {
            return RecvCall::Empty;
        }
        let Some(call) = lane.pending.pop_front() else {
            return RecvCall::Empty;
        };
        let still_waiting = !lane.pending.is_empty();
        let PendingCall {
            ticket,
            wakes,
            origin,
            deadline,
            request,
            charge,
        } = call;
        lane.in_service.push(InServiceCall {
            ticket,
            origin,
            wakes,
            deadline,
            charge,
        });
        let _ = serving.try_insert(ticket, sender);
        *pending -= 1;
        lanes.end_turn(still_waiting);
        RecvCall::Received(ReceivedCall {
            ticket: CallTicket(ticket),
            sender,
            request,
        })
    }

    /// The kernel-attested [`Origin`] of the caller that posted the
    /// in-service call identified by `ticket`.
    ///
    /// Returns [`None`] unless `ticket` names a call this endpoint has handed
    /// to the server via [`recv_call`](Self::recv_call) and not yet replied —
    /// so a server reads a caller's identity only while actively servicing
    /// that caller's request, never for a pending, completed, or unknown
    /// ticket (fail closed). The origin was snapshotted from the caller's own
    /// kernel state at [`post`](Self::post) time and is unforgeable by the
    /// caller; the syscall layer additionally confirms the reader owns this
    /// endpoint before exposing it.
    #[must_use]
    pub fn peer_origin(&self, ticket: CallTicket) -> Option<Origin> {
        let g = self.inner.lock();
        let sender = *g.serving.get(&ticket.0)?;
        g.lanes
            .get(sender)?
            .in_service
            .iter()
            .find(|call| call.ticket == ticket.0)
            .map(|call| call.origin)
    }

    /// Deliver `reply` for the in-service call identified by `ticket`.
    ///
    /// The reply must be `<= max_reply` (bounded again by
    /// [`IPC_MESSAGE_MAX_PAYLOAD_LEN`]) and `ticket` must name a call the
    /// server is currently servicing (received but not yet replied);
    /// otherwise the reply is refused fail-closed and one
    /// [`AuditEvent::CallReplyDenied`] is emitted. On success the reply is
    /// buffered in its caller's lane, one [`AuditEvent::CallReplied`] is
    /// emitted, and the threads to wake are returned: those the call's answer
    /// concerns, and the caller's threads waiting for the room it freed.
    ///
    /// No capability check: the single bound server's authority is fixed at
    /// create time.
    ///
    /// # Errors
    ///
    /// * [`Errno::MessageTooLarge`] if the reply exceeds `max_reply`.
    /// * [`Errno::NotFound`] if `ticket` is not currently in service (unknown,
    ///   already replied, or cancelled by [`destroy`](Self::destroy)).
    /// * [`Errno::OutOfMemory`] if the kernel-owned copy of the reply cannot
    ///   be allocated.
    pub fn reply<S: Sink + ?Sized>(
        &self,
        ticket: CallTicket,
        reply: &[u8],
        audit: &S,
    ) -> Result<Woken, Errno> {
        let mut id_buf = [0u8; 16];
        let mut ticket_buf = [0u8; 16];
        let mut len_buf = [0u8; 12];
        let id_field = Field {
            key: "endpoint",
            value: tairix_log::FieldValue::Str(format_hex_u64(self.id.0, &mut id_buf)),
        };
        let ticket_field = Field {
            key: "ticket",
            value: tairix_log::FieldValue::Str(format_hex_u64(ticket.0, &mut ticket_buf)),
        };
        let len_field = Field {
            key: "len",
            value: tairix_log::FieldValue::Str(format_usize(reply.len(), &mut len_buf)),
        };

        let effective_max = u64::from(self.max_reply).min(u64::from(IPC_MESSAGE_MAX_PAYLOAD_LEN));
        if reply.len() as u64 > effective_max {
            record(
                audit,
                AuditEvent::CallReplyDenied,
                &[id_field, ticket_field, len_field, REPLY_DENIED_OVERSIZE],
            );
            return Err(Errno::MessageTooLarge);
        }

        // Copy before the lock, as `post` does: the buffer wipes itself on
        // drop so a refused reply leaks nothing, and the allocation stays out
        // of the critical section.
        let reply = match SensitiveBuffer::copy_from_slice(reply) {
            Ok(buf) => buf,
            Err(err) => {
                record(
                    audit,
                    AuditEvent::PayloadAllocFailed,
                    &[id_field, ticket_field, len_field],
                );
                return Err(err.as_errno());
            }
        };

        let mut g = self.inner.lock();
        let Inner { lanes, serving, .. } = &mut *g;
        // The caller's lane outlives every call it has in service, and its
        // room for this reply was reserved at post, so the push below never
        // allocates.
        let found = match serving.get(&ticket.0).copied() {
            Some(sender) => lanes.get_mut(sender).map(|lane| (sender, lane)),
            None => None,
        };
        let answered = found.and_then(|(sender, lane)| {
            let pos = lane
                .in_service
                .iter()
                .position(|call| call.ticket == ticket.0)?;
            (lane.completed.len() < lane.completed.capacity()).then_some((sender, lane, pos))
        });
        let Some((sender, lane, pos)) = answered else {
            drop(g);
            record(
                audit,
                AuditEvent::CallReplyDenied,
                &[id_field, ticket_field, REPLY_DENIED_UNKNOWN_TICKET],
            );
            return Err(Errno::NotFound);
        };
        let InServiceCall {
            wakes,
            deadline,
            charge,
            ..
        } = lane.in_service.swap_remove(pos);
        lane.timed -= usize::from(deadline != u64::MAX);
        lane.completed.push(CompletedCall {
            ticket: ticket.0,
            reply,
            _charge: charge,
        });
        serving.remove(&ticket.0);
        self.settle();
        drop(g);
        let room = self.room.lock().take_waiting_as(sender);
        record(
            audit,
            AuditEvent::CallReplied,
            &[id_field, ticket_field, len_field],
        );
        Ok(Woken {
            answer: Some(wakes),
            room,
        })
    }

    /// Claim the reply for `ticket` on behalf of `claimant` (the process that
    /// posted it), as of monotonic time `now` (ns).
    ///
    /// This is the caller's poll step; it never blocks and never allocates.
    /// The ticket is the unforgeable authority, and only `claimant`'s own lane
    /// is searched, so a ticket another process holds is indistinguishable
    /// from one that never existed.
    ///
    /// * [`ReplyOutcome::Ready`] — the reply is available; its bytes are
    ///   returned and the ticket retired.
    /// * [`ReplyOutcome::TimedOut`] — the ticket's per-post `deadline` has
    ///   passed with no reply; the ticket is retired so a wedged callee fails
    ///   the caller closed rather than parking it forever, and the room it
    ///   held is free ([`room_wake_for`](Self::room_wake_for)).
    /// * [`ReplyOutcome::Pending`] — posted or in service, not yet replied,
    ///   and still inside its deadline.
    /// * [`ReplyOutcome::Cancelled`] — the endpoint was destroyed; abandon.
    /// * [`ReplyOutcome::Unknown`] — no such ticket for `claimant`.
    #[must_use]
    pub fn take_reply<S: Sink + ?Sized>(
        &self,
        claimant: u64,
        ticket: CallTicket,
        now: u64,
        audit: &S,
    ) -> ReplyOutcome {
        let mut g = self.inner.lock();
        let closed = self.is_closed();
        let Inner {
            lanes,
            serving,
            pending,
            ..
        } = &mut *g;
        let Some(lane) = lanes.get_mut(claimant) else {
            return if closed {
                ReplyOutcome::Cancelled
            } else {
                ReplyOutcome::Unknown
            };
        };
        if let Some(pos) = lane.completed.iter().position(|c| c.ticket == ticket.0) {
            let done = lane.completed.swap_remove(pos);
            let idle = lanes.retire_if_idle(claimant);
            let shed = g.shed_idle_room();
            drop(g);
            drop(idle);
            drop(shed);
            return ReplyOutcome::Ready(done.reply);
        }
        // A ticket still waiting, wherever it sits; its deadline decides
        // between a timeout and a live wait.
        let queued = lane.pending.iter().position(|c| c.ticket == ticket.0);
        let served = queued
            .is_none()
            .then(|| lane.in_service.iter().position(|c| c.ticket == ticket.0))
            .flatten();
        let deadline = match (queued, served) {
            (Some(pos), _) => lane.pending.get(pos).map(|c| c.deadline),
            (None, Some(pos)) => lane.in_service.get(pos).map(|c| c.deadline),
            (None, None) => None,
        };
        match deadline {
            None => ReplyOutcome::Unknown,
            Some(deadline) if deadline > now => ReplyOutcome::Pending,
            Some(deadline) => {
                // Retired from wherever it sits so a late reply for it is
                // refused fail-closed and its place in the lane is freed. The
                // call is moved out and dropped after the lock.
                lane.timed -= usize::from(deadline != u64::MAX);
                let mut dead_request = None;
                let mut dead_service = None;
                if let Some(pos) = queued {
                    dead_request = lane.pending.remove(pos);
                    *pending -= 1;
                    if lane.pending.is_empty() {
                        lanes.leave_turn(claimant);
                    }
                } else if let Some(pos) = served {
                    dead_service = Some(lane.in_service.swap_remove(pos));
                    serving.remove(&ticket.0);
                }
                self.settle();
                let idle = lanes.retire_if_idle(claimant);
                let shed = g.shed_idle_room();
                drop(g);
                drop(dead_request);
                drop(dead_service);
                drop(idle);
                drop(shed);
                // Recorded here rather than at each caller: an in-kernel
                // caller never reaches the syscall dispatcher's audit, so
                // this is the only trace that the server missed its budget.
                let mut id_buf = [0u8; 16];
                let mut ticket_buf = [0u8; 16];
                record(
                    audit,
                    AuditEvent::CallTimedOut,
                    &[
                        Field {
                            key: "endpoint",
                            value: tairix_log::FieldValue::Str(format_hex_u64(
                                self.id.0,
                                &mut id_buf,
                            )),
                        },
                        Field {
                            key: "ticket",
                            value: tairix_log::FieldValue::Str(format_hex_u64(
                                ticket.0,
                                &mut ticket_buf,
                            )),
                        },
                    ],
                );
                ReplyOutcome::TimedOut
            }
        }
    }

    /// Whether a [`take_reply`](Self::take_reply) by `claimant` as of `now`
    /// would make progress — a reply is ready, or one of the claimant's
    /// tickets has timed out, or the endpoint is closed. The non-consuming
    /// peek a [`tairix_abi::WaitSourceKind::CallReply`] wait-set member scans:
    /// the woken owner drains with `take_reply`, never the wait.
    #[must_use]
    pub fn has_ready_reply_for(&self, claimant: u64, now: u64) -> bool {
        // A torn-down endpoint is "ready" so the parked reaper wakes and
        // observes `Cancelled`/`NotFound` rather than parking forever.
        if self.is_closed() {
            return true;
        }
        let g = self.inner.lock();
        g.lanes.get(claimant).is_some_and(|lane| {
            !lane.completed.is_empty()
                || (lane.timed > 0 && lane.deadlines().any(|deadline| deadline <= now))
        })
    }

    /// The nearest (soonest) per-ticket deadline across every outstanding
    /// call `claimant` posted, or [`u64::MAX`] if it has none (or all are
    /// deadline-less). The wait-set layer folds this into its one-shot timer
    /// arming so a wedged callee's deadline wakes the parked reaper.
    #[must_use]
    pub fn next_deadline_for(&self, claimant: u64) -> u64 {
        let g = self.inner.lock();
        g.lanes
            .get(claimant)
            .filter(|lane| lane.timed > 0)
            .and_then(|lane| lane.deadlines().min())
            .unwrap_or(u64::MAX)
    }

    /// Make `waiter` the thread awaiting `claimant`'s call `ticket`, returning
    /// the deadline that wait is bounded by: [`u64::MAX`] for none, and for a
    /// call already answered or unknown, which the reap that follows settles
    /// at once.
    ///
    /// A reply wakes the call's awaiter and its poster rather than every
    /// reaper, so a call awaited from a thread other than its poster must name
    /// that thread instead.
    ///
    /// # Errors
    ///
    /// [`Errno::Busy`] when another of the process's threads awaits the call
    /// already: one answer wakes one awaiter.
    pub fn await_by(&self, claimant: u64, ticket: CallTicket, waiter: u64) -> Result<u64, Errno> {
        let mut g = self.inner.lock();
        let Some(lane) = g.lanes.get_mut(claimant) else {
            return Ok(u64::MAX);
        };
        let waiting = lane
            .pending
            .iter_mut()
            .find(|call| call.ticket == ticket.0)
            .map(|call| (&mut call.wakes, call.deadline))
            .or_else(|| {
                lane.in_service
                    .iter_mut()
                    .find(|call| call.ticket == ticket.0)
                    .map(|call| (&mut call.wakes, call.deadline))
            });
        let Some((wakes, deadline)) = waiting else {
            return Ok(u64::MAX);
        };
        wakes.await_by(waiter)?;
        Ok(deadline)
    }

    /// Withdraw the single call `ticket` that `claimant` posted, wherever it
    /// sits (pending, in service, or replied-but-unclaimed), returning the
    /// threads to wake, or [`None`] when nothing was removed. A per-ticket form
    /// of [`cancel_posted_by`](Self::cancel_posted_by): a caller abandoning a
    /// wedged transfer frees its place deterministically, and a thread awaiting
    /// the call wakes to find it gone. Only `claimant`'s own lane is searched,
    /// so a foreign or unknown ticket removes nothing (no existence oracle).
    pub fn cancel_one(&self, claimant: u64, ticket: CallTicket) -> Option<Woken> {
        // The withdrawn payloads are moved out and dropped after the guard,
        // keeping the heap's lock out of this endpoint's critical section.
        let mut g = self.inner.lock();
        let Inner {
            lanes,
            serving,
            pending,
            ..
        } = &mut *g;
        let lane = lanes.get_mut(claimant)?;
        let mut dead_request = None;
        let mut dead_service = None;
        let mut dead_reply = None;
        let mut emptied = false;
        let answer = if let Some(pos) = lane.pending.iter().position(|c| c.ticket == ticket.0) {
            let call = lane.pending.remove(pos)?;
            *pending -= 1;
            emptied = lane.pending.is_empty();
            lane.timed -= usize::from(call.deadline != u64::MAX);
            let wakes = call.wakes;
            dead_request = Some(call);
            Some(wakes)
        } else if let Some(pos) = lane.in_service.iter().position(|c| c.ticket == ticket.0) {
            let call = lane.in_service.swap_remove(pos);
            serving.remove(&ticket.0);
            lane.timed -= usize::from(call.deadline != u64::MAX);
            let wakes = call.wakes;
            dead_service = Some(call);
            Some(wakes)
        } else {
            let pos = lane.completed.iter().position(|c| c.ticket == ticket.0)?;
            dead_reply = Some(lane.completed.swap_remove(pos));
            None
        };
        if emptied {
            lanes.leave_turn(claimant);
        }
        self.settle();
        let idle = lanes.retire_if_idle(claimant);
        let shed = g.shed_idle_room();
        drop(g);
        drop(dead_request);
        drop(dead_service);
        drop(dead_reply);
        drop(idle);
        drop(shed);
        // Only an unanswered call held room.
        let room = if answer.is_some() {
            self.room.lock().take_waiting_as(claimant)
        } else {
            RoomWake::none()
        };
        Some(Woken { answer, room })
    }

    /// Record `task`, a thread of process `sender`, as parked in a blocking
    /// call until `sender` has room.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfMemory`] when the record cannot be kept: the thread must
    /// then not park, since nothing would wake it.
    pub fn watch_room(&self, task: u64, sender: u64) -> Result<(), Errno> {
        self.room.lock().watch(task, sender)
    }

    /// Forget `task`'s wait for room as `sender`.
    pub fn unwatch_room(&self, task: u64, sender: u64) {
        let emptied = self.room.lock().unwatch(task, sender);
        drop(emptied);
    }

    /// The threads to wake now that one of `sender`'s calls retired unanswered
    /// and left it room.
    #[must_use]
    pub fn room_wake_for(&self, sender: u64) -> RoomWake<u64> {
        self.room.lock().take_waiting_as(sender)
    }

    /// `true` when a post from `sender` would not be refused for want of
    /// room: the endpoint is closed, or `sender` has fewer than the per-caller
    /// depth of calls waiting on the server. The peek a blocking call woken
    /// for room takes before it posts again; every other refusal of a post is
    /// an answer, never something to wait out.
    #[must_use]
    pub fn has_room_for(&self, sender: u64) -> bool {
        self.is_closed()
            || self
                .inner
                .lock()
                .lanes
                .get(sender)
                .is_none_or(|lane| lane.unanswered() < self.per_sender)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::RecordingSink;
    use tairix_abi::CapabilityId;
    use tairix_kernel_sec::captable::ProcessId;
    use tairix_kernel_sec::identity::UserId;

    /// The scheduler id the tests post under — arbitrary but non-zero, so
    /// `reply` hands it back verbatim.
    const POSTER_SCHED: u64 = 0x51;

    /// No budget, for the tests that are not about one.
    const NO_LIMIT: u64 = tairix_abi::RLIMIT_INFINITY;

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
        TaskCapabilities::derive(ProcessId(task_id), UserId(1), set, set, &sink)
    }

    /// An unrestricted endpoint any task may call, with generous bounds.
    fn open_endpoint(sink: &RecordingSink) -> CallEndpoint {
        let creator = task_with(1, &[]);
        CallEndpoint::create(
            EndpointId(0xC),
            &creator,
            CapabilitySet::empty(),
            CapabilitySet::empty(),
            CallEndpointLimits {
                max_request: 128,
                max_reply: 128,
                per_sender: 8,
            },
            tairix_abi::RLIMIT_INFINITY,
            sink,
        )
        .expect("unrestricted endpoint")
    }

    /// The bytes of a `Ready` reply. Payload-carrying outcomes are not
    /// comparable with `==` (a secret is never compared, only read), so the
    /// tests assert the variant and the bytes separately.
    fn ready_bytes(outcome: ReplyOutcome) -> alloc::vec::Vec<u8> {
        match outcome {
            ReplyOutcome::Ready(bytes) => bytes.as_bytes().to_vec(),
            other => panic!("expected a ready reply, got {other:?}"),
        }
    }

    /// Every thread `woken` names, in order.
    fn tasks(woken: &Woken) -> alloc::vec::Vec<u64> {
        let mut tasks: alloc::vec::Vec<u64> = woken.tasks().collect();
        tasks.sort_unstable();
        tasks
    }

    /// The threads answering `ticket` wakes.
    fn replied(
        ep: &CallEndpoint,
        ticket: CallTicket,
        sink: &RecordingSink,
    ) -> alloc::vec::Vec<u64> {
        tasks(&ep.reply(ticket, b"r", sink).expect("replied"))
    }

    /// Unbounded receive for the tests: the test buffers are never the
    /// constraint, so map the size-bounded [`CallEndpoint::recv_call`] onto
    /// the simple `Option` the round-trip assertions read.
    fn recv_one(ep: &CallEndpoint) -> Option<ReceivedCall> {
        match ep.recv_call(usize::MAX) {
            RecvCall::Received(call) => Some(call),
            RecvCall::Empty => None,
            RecvCall::TooLarge { .. } => unreachable!("usize::MAX bounds nothing"),
        }
    }

    #[test]
    fn create_charges_the_owner_only_its_bookkeeping() {
        let sink = RecordingSink::new();
        let creator = task_with(1, &[]);
        let limits = CallEndpointLimits {
            max_request: 512,
            max_reply: 64,
            per_sender: 4,
        };
        let reservation = limits.reservation();
        assert_eq!(
            reservation,
            CallEndpointLimits {
                max_request: IPC_MESSAGE_MAX_PAYLOAD_LEN,
                max_reply: IPC_MESSAGE_MAX_PAYLOAD_LEN,
                per_sender: IPC_SENDER_DEPTH_MAX,
            }
            .reservation(),
            "the owner pays for no caller's calls, however large or deep"
        );
        let create = |limit| {
            CallEndpoint::create(
                EndpointId(0xC1),
                &creator,
                CapabilitySet::empty(),
                CapabilitySet::empty(),
                limits,
                limit,
                &sink,
            )
        };
        assert_eq!(
            create(reservation - 1).map(|ep| ep.id()),
            Err(Errno::LimitExceeded)
        );
        assert!(sink
            .ids()
            .contains(&AuditEvent::CallEndpointCreateDenied.id().0));
        assert_eq!(creator.charges().held(LimitKind::IpcQueueBytes), 0);
        let endpoint = create(reservation).expect("exactly fits");
        assert_eq!(
            creator.charges().held(LimitKind::IpcQueueBytes),
            reservation
        );
        drop(endpoint);
        assert_eq!(creator.charges().held(LimitKind::IpcQueueBytes), 0);
    }

    #[test]
    fn a_call_charges_its_caller_the_larger_payload_whatever_it_sends() {
        let limits = CallEndpointLimits {
            max_request: 512,
            max_reply: 64,
            per_sender: 4,
        };
        assert!(limits.call_charge() >= 512, "room for the larger payload");
        assert_eq!(
            limits.call_charge(),
            CallEndpointLimits {
                max_reply: 0,
                ..limits
            }
            .call_charge(),
            "but never both: a call holds its request or its reply"
        );
    }

    #[test]
    fn create_rejects_oversize_request_cap() {
        let sink = RecordingSink::new();
        let creator = task_with(1, &[]);
        let err = CallEndpoint::create(
            EndpointId(1),
            &creator,
            CapabilitySet::empty(),
            CapabilitySet::empty(),
            CallEndpointLimits {
                max_request: IPC_MESSAGE_MAX_PAYLOAD_LEN + 1,
                max_reply: 8,
                per_sender: 8,
            },
            tairix_abi::RLIMIT_INFINITY,
            &sink,
        )
        .err()
        .expect("oversize request cap is refused");
        assert_eq!(err, Errno::LengthOutOfRange);
        assert!(sink
            .ids()
            .contains(&AuditEvent::CallEndpointCreateDenied.id().0));
    }

    /// The per-caller depth reaches `create` straight from the syscall's
    /// argument, so it needs a fixed ceiling of its own: a lane is searched by
    /// scanning, and an unbounded depth leaves that scan unbounded.
    #[test]
    fn create_rejects_a_depth_above_the_ceiling() {
        let sink = RecordingSink::new();
        let creator = task_with(1, &[]);
        let err = CallEndpoint::create(
            EndpointId(1),
            &creator,
            CapabilitySet::empty(),
            CapabilitySet::empty(),
            CallEndpointLimits {
                max_request: 8,
                max_reply: 8,
                per_sender: IPC_SENDER_DEPTH_MAX + 1,
            },
            tairix_abi::RLIMIT_INFINITY,
            &sink,
        )
        .err()
        .expect("a depth above the ceiling is refused");
        assert_eq!(err, Errno::LengthOutOfRange);
        assert!(sink
            .ids()
            .contains(&AuditEvent::CallEndpointCreateDenied.id().0));
        // The ceiling itself is accepted, so the bound is inclusive.
        CallEndpoint::create(
            EndpointId(1),
            &creator,
            CapabilitySet::empty(),
            CapabilitySet::empty(),
            CallEndpointLimits {
                max_request: 8,
                max_reply: 8,
                per_sender: IPC_SENDER_DEPTH_MAX,
            },
            tairix_abi::RLIMIT_INFINITY,
            &sink,
        )
        .expect("the ceiling is a legal depth");
    }

    #[test]
    fn create_rejects_oversize_reply_cap() {
        let sink = RecordingSink::new();
        let creator = task_with(1, &[]);
        let err = CallEndpoint::create(
            EndpointId(1),
            &creator,
            CapabilitySet::empty(),
            CapabilitySet::empty(),
            CallEndpointLimits {
                max_request: 8,
                max_reply: IPC_MESSAGE_MAX_PAYLOAD_LEN + 1,
                per_sender: 8,
            },
            tairix_abi::RLIMIT_INFINITY,
            &sink,
        )
        .err()
        .expect("oversize reply cap is refused");
        assert_eq!(err, Errno::LengthOutOfRange);
    }

    #[test]
    fn create_rejects_a_zero_depth() {
        let sink = RecordingSink::new();
        let creator = task_with(1, &[]);
        let err = CallEndpoint::create(
            EndpointId(2),
            &creator,
            CapabilitySet::empty(),
            CapabilitySet::empty(),
            CallEndpointLimits {
                max_request: 64,
                max_reply: 64,
                per_sender: 0,
            },
            tairix_abi::RLIMIT_INFINITY,
            &sink,
        )
        .err()
        .expect("zero capacity is refused");
        assert_eq!(err, Errno::LengthOutOfRange);
    }

    #[test]
    fn create_rejects_reserved_id_squat_without_bind_privilege() {
        let sink = RecordingSink::new();
        let squatter = task_with(1, &[]);
        for id in [
            tairix_abi::sysinfo::SYSINFO_ENDPOINT,
            tairix_abi::log_ingress::LOG_INGRESS_ENDPOINT,
            tairix_abi::elevate::ELEVATE_ENDPOINT_BASE,
            tairix_abi::elevate::ELEVATE_ENDPOINT_BASE
                + u64::from(tairix_abi::process::CONSOLE_INDEX_MAX),
        ] {
            let err = CallEndpoint::create(
                EndpointId(id),
                &squatter,
                CapabilitySet::empty(),
                CapabilitySet::empty(),
                CallEndpointLimits {
                    max_request: 64,
                    max_reply: 64,
                    per_sender: 8,
                },
                tairix_abi::RLIMIT_INFINITY,
                &sink,
            )
            .err()
            .expect("reserved-id squat is refused");
            assert_eq!(err, Errno::PermissionDenied);
        }
        assert!(sink
            .ids()
            .contains(&AuditEvent::CallEndpointCreateDenied.id().0));
    }

    #[test]
    fn create_allows_reserved_id_with_bind_privilege() {
        let sink = RecordingSink::new();
        let service = task_with(1, &[CapabilityId::IPC_BIND_PRIVILEGED]);
        let ep = CallEndpoint::create(
            EndpointId(tairix_abi::sysinfo::SYSINFO_ENDPOINT),
            &service,
            CapabilitySet::empty(),
            CapabilitySet::empty(),
            CallEndpointLimits {
                max_request: 64,
                max_reply: 64,
                per_sender: 8,
            },
            tairix_abi::RLIMIT_INFINITY,
            &sink,
        )
        .expect("privileged service binds its reserved rendezvous");
        assert_eq!(ep.id(), EndpointId(tairix_abi::sysinfo::SYSINFO_ENDPOINT));
        assert_eq!(
            sink.unsigned(AuditEvent::CallEndpointCreated.id().0, "process"),
            Some(service.process().0),
            "the record names the process that serves it"
        );
    }

    #[test]
    fn create_rejects_recv_caps_not_held_by_creator() {
        let sink = RecordingSink::new();
        let creator = task_with(1, &[]); // holds nothing
        let required_recv = caps_of(&[CapabilityId::AUDIT_READ]);
        let err = CallEndpoint::create(
            EndpointId(3),
            &creator,
            CapabilitySet::empty(),
            required_recv,
            CallEndpointLimits {
                max_request: 64,
                max_reply: 64,
                per_sender: 8,
            },
            tairix_abi::RLIMIT_INFINITY,
            &sink,
        )
        .err()
        .expect("must not grant unheld authority");
        assert_eq!(err, Errno::PermissionDenied);
        assert!(sink
            .ids()
            .contains(&AuditEvent::CallEndpointCreateDenied.id().0));
    }

    #[test]
    fn create_requires_ipc_bind_privileged_for_restricted_sender() {
        let sink = RecordingSink::new();
        // Holds the send cap but lacks IPC_BIND_PRIVILEGED, so may not bind
        // an endpoint that restricts who may call it.
        let creator = task_with(1, &[CapabilityId::NET_RAW]);
        let err = CallEndpoint::create(
            EndpointId(4),
            &creator,
            caps_of(&[CapabilityId::NET_RAW]),
            CapabilitySet::empty(),
            CallEndpointLimits {
                max_request: 64,
                max_reply: 64,
                per_sender: 8,
            },
            tairix_abi::RLIMIT_INFINITY,
            &sink,
        )
        .err()
        .expect("privileged bind requires IPC_BIND_PRIVILEGED");
        assert_eq!(err, Errno::PermissionDenied);
    }

    const SERVED_LIMITS: CallEndpointLimits = CallEndpointLimits {
        max_request: 64,
        max_reply: 64,
        per_sender: 2,
    };

    #[test]
    fn a_served_volume_endpoint_is_the_kernels_to_make_and_takes_only_its_posts() {
        let sink = RecordingSink::new();
        let served = EndpointId(tairix_abi::fsserve::served_endpoint(7));
        // The driver holds no bind privilege: the kernel's acceptance of its
        // serve is the authority.
        let driver = task_with(9, &[]);
        let ep = CallEndpoint::create_served(
            served,
            &driver,
            SERVED_LIMITS,
            tairix_abi::RLIMIT_INFINITY,
            &sink,
        )
        .expect("the kernel makes it");
        assert_eq!(ep.owner(), 9);
        assert_eq!(
            ep.required_send_caps(),
            &caps_of(&[CapabilityId::IPC_ENDPOINT])
        );
        // A process naming the range itself is refused, privileged or not.
        let privileged = task_with(2, &[CapabilityId::IPC_BIND_PRIVILEGED]);
        assert_eq!(
            CallEndpoint::create(
                served,
                &privileged,
                CapabilitySet::empty(),
                CapabilitySet::empty(),
                SERVED_LIMITS,
                tairix_abi::RLIMIT_INFINITY,
                &sink,
            )
            .err(),
            Some(Errno::OutOfRange)
        );
        // …and the kernel makes no served endpoint outside it.
        assert_eq!(
            CallEndpoint::create_served(
                EndpointId(0x77),
                &driver,
                SERVED_LIMITS,
                tairix_abi::RLIMIT_INFINITY,
                &sink,
            )
            .err(),
            Some(Errno::OutOfRange)
        );
        // Only a sender holding the endpoint class capability may post.
        let stranger = task_with(3, &[]);
        assert_eq!(
            ep.post(&stranger, POSTER_SCHED, b"x", u64::MAX, NO_LIMIT, &sink)
                .err(),
            Some(Errno::PermissionDenied)
        );
        let kernel = task_with(4, &[CapabilityId::IPC_ENDPOINT]);
        assert!(ep
            .post(&kernel, POSTER_SCHED, b"x", u64::MAX, NO_LIMIT, &sink)
            .is_ok());
    }

    #[test]
    fn create_succeeds_and_exposes_its_parameters() {
        let sink = RecordingSink::new();
        let creator = task_with(
            1,
            &[CapabilityId::IPC_BIND_PRIVILEGED, CapabilityId::NET_RAW],
        );
        let ep = CallEndpoint::create(
            EndpointId(5),
            &creator,
            caps_of(&[CapabilityId::NET_RAW]),
            CapabilitySet::empty(),
            CallEndpointLimits {
                max_request: 100,
                max_reply: 200,
                per_sender: 4,
            },
            tairix_abi::RLIMIT_INFINITY,
            &sink,
        )
        .expect("authorised");
        assert_eq!(ep.id(), EndpointId(5));
        assert_eq!(ep.max_request(), 100);
        assert_eq!(ep.max_reply(), 200);
        assert!(ep.required_send_caps().contains(CapabilityId::NET_RAW));
        assert!(ep.required_recv_caps().is_empty());
        assert!(!ep.is_closed());
        assert_eq!(ep.outstanding(), 0);
        assert!(sink.ids().contains(&AuditEvent::CallEndpointCreated.id().0));
    }

    #[test]
    fn post_without_required_cap_is_denied_and_audited() {
        let sink = RecordingSink::new();
        let creator = task_with(
            1,
            &[CapabilityId::IPC_BIND_PRIVILEGED, CapabilityId::NET_RAW],
        );
        let ep = CallEndpoint::create(
            EndpointId(0xA),
            &creator,
            caps_of(&[CapabilityId::NET_RAW]),
            CapabilitySet::empty(),
            CallEndpointLimits {
                max_request: 64,
                max_reply: 64,
                per_sender: 4,
            },
            tairix_abi::RLIMIT_INFINITY,
            &sink,
        )
        .expect("authorised");
        let caller = task_with(7, &[]); // lacks NET_RAW
        let err = ep
            .post(&caller, POSTER_SCHED, b"hi", u64::MAX, NO_LIMIT, &sink)
            .expect_err("denied");
        assert_eq!(err, Errno::PermissionDenied);
        assert!(sink.ids().contains(&AuditEvent::CallPostDenied.id().0));
        assert_eq!(ep.outstanding(), 0);
    }

    #[test]
    fn post_oversize_request_is_too_large() {
        let sink = RecordingSink::new();
        let creator = task_with(1, &[]);
        let ep = CallEndpoint::create(
            EndpointId(0xB),
            &creator,
            CapabilitySet::empty(),
            CapabilitySet::empty(),
            CallEndpointLimits {
                max_request: 4,
                max_reply: 64,
                per_sender: 4,
            },
            tairix_abi::RLIMIT_INFINITY,
            &sink,
        )
        .expect("ok");
        let caller = task_with(7, &[]);
        let err = ep
            .post(
                &caller,
                POSTER_SCHED,
                b"too many bytes",
                u64::MAX,
                NO_LIMIT,
                &sink,
            )
            .expect_err("oversize");
        assert_eq!(err, Errno::MessageTooLarge);
        assert!(sink.ids().contains(&AuditEvent::CallRequestTooLarge.id().0));
    }

    #[test]
    fn post_to_closed_endpoint_is_not_found() {
        let sink = RecordingSink::new();
        let ep = open_endpoint(&sink);
        ep.destroy(&sink);
        let caller = task_with(7, &[]);
        let err = ep
            .post(&caller, POSTER_SCHED, b"x", u64::MAX, NO_LIMIT, &sink)
            .expect_err("closed");
        assert_eq!(err, Errno::NotFound);
        assert!(sink
            .ids()
            .contains(&AuditEvent::CallPostToClosedEndpoint.id().0));
        assert!(ep.is_closed());
    }

    /// Cancelling one sender's calls must leave the other sender's requests
    /// in arrival order: the server drains strictly FIFO, so a scrub that
    /// reordered survivors would silently serve requests out of sequence.
    #[test]
    fn cancelling_one_senders_calls_keeps_the_others_in_fifo_order() {
        let sink = RecordingSink::new();
        let creator = task_with(1, &[]);
        let ep = CallEndpoint::create(
            EndpointId(0x5F),
            &creator,
            CapabilitySet::empty(),
            CapabilitySet::empty(),
            CallEndpointLimits {
                max_request: 64,
                max_reply: 64,
                per_sender: 8,
            },
            tairix_abi::RLIMIT_INFINITY,
            &sink,
        )
        .expect("ok");
        let keep = task_with(7, &[]);
        let doomed = task_with(9, &[]);
        // Interleaved so a reorder cannot pass by luck.
        ep.post(&keep, POSTER_SCHED, b"k1", u64::MAX, NO_LIMIT, &sink)
            .expect("k1");
        ep.post(&doomed, POSTER_SCHED, b"d1", u64::MAX, NO_LIMIT, &sink)
            .expect("d1");
        ep.post(&keep, POSTER_SCHED, b"k2", u64::MAX, NO_LIMIT, &sink)
            .expect("k2");
        ep.post(&doomed, POSTER_SCHED, b"d2", u64::MAX, NO_LIMIT, &sink)
            .expect("d2");
        ep.post(&keep, POSTER_SCHED, b"k3", u64::MAX, NO_LIMIT, &sink)
            .expect("k3");

        assert_eq!(ep.cancel_posted_by(9, &sink), 2);
        assert_eq!(ep.outstanding(), 3);
        for expected in [b"k1".as_slice(), b"k2".as_slice(), b"k3".as_slice()] {
            let call = recv_one(&ep).expect("survivor still queued");
            assert_eq!(call.sender, 7);
            assert_eq!(call.request.as_bytes(), expected);
        }
        assert!(recv_one(&ep).is_none());
    }

    /// A sender absent from an endpoint that is *not* empty cancels nothing
    /// and disturbs nothing — the case the empty-endpoint no-op above cannot
    /// reach, and the one a task exit hits on almost every live endpoint.
    #[test]
    fn cancelling_a_sender_absent_from_a_non_empty_endpoint_is_a_no_op() {
        let sink = RecordingSink::new();
        let creator = task_with(1, &[]);
        let ep = CallEndpoint::create(
            EndpointId(0x60),
            &creator,
            CapabilitySet::empty(),
            CapabilitySet::empty(),
            CallEndpointLimits {
                max_request: 64,
                max_reply: 64,
                per_sender: 4,
            },
            tairix_abi::RLIMIT_INFINITY,
            &sink,
        )
        .expect("ok");
        let keep = task_with(7, &[]);
        ep.post(&keep, POSTER_SCHED, b"k", u64::MAX, NO_LIMIT, &sink)
            .expect("k");
        assert_eq!(ep.cancel_posted_by(9, &sink), 0);
        assert_eq!(ep.outstanding(), 1);
        assert!(!sink.ids().contains(&AuditEvent::CallPosterVanished.id().0));
    }

    #[test]
    fn a_post_beyond_the_callers_depth_is_queue_full() {
        let sink = RecordingSink::new();
        let creator = task_with(1, &[]);
        let ep = CallEndpoint::create(
            EndpointId(0xD),
            &creator,
            CapabilitySet::empty(),
            CapabilitySet::empty(),
            CallEndpointLimits {
                max_request: 64,
                max_reply: 64,
                per_sender: 2,
            },
            tairix_abi::RLIMIT_INFINITY,
            &sink,
        )
        .expect("ok");
        let caller = task_with(7, &[]);
        ep.post(&caller, POSTER_SCHED, b"a", u64::MAX, NO_LIMIT, &sink)
            .expect("1");
        ep.post(&caller, POSTER_SCHED, b"b", u64::MAX, NO_LIMIT, &sink)
            .expect("2");
        let err = ep
            .post(&caller, POSTER_SCHED, b"c", u64::MAX, NO_LIMIT, &sink)
            .expect_err("full");
        // A full lane is retryable back-pressure on this caller alone, not a
        // malformed request, and therefore distinct from the
        // `LengthOutOfRange` a bad `CallEndpoint::create` configuration
        // returns.
        assert_eq!(err, Errno::WouldBlock);
        assert!(sink.ids().contains(&AuditEvent::CallQueueFull.id().0));
        assert_eq!(ep.outstanding(), 2);
    }

    #[test]
    fn full_round_trip_post_recv_reply_take() {
        let sink = RecordingSink::new();
        let ep = open_endpoint(&sink);
        let caller = task_with(7, &[]);

        let ticket = ep
            .post(&caller, POSTER_SCHED, b"ping", u64::MAX, NO_LIMIT, &sink)
            .expect("posted");
        // Before the server receives it, the caller sees Pending.
        assert!(matches!(
            ep.take_reply(7, ticket, 0, &sink),
            ReplyOutcome::Pending
        ));

        let received = recv_one(&ep).expect("a pending call");
        assert_eq!(received.ticket, ticket);
        assert_eq!(received.sender, 7);
        assert_eq!(received.request.as_bytes(), b"ping");
        // Received but unreplied is still Pending for the caller.
        assert!(matches!(
            ep.take_reply(7, ticket, 0, &sink),
            ReplyOutcome::Pending
        ));

        let _ = ep.reply(ticket, b"pong", &sink).expect("replied");
        assert!(sink.ids().contains(&AuditEvent::CallReplied.id().0));

        // The caller claims the reply exactly once.
        assert_eq!(ready_bytes(ep.take_reply(7, ticket, 0, &sink)), b"pong");
        // A second claim finds nothing.
        assert!(matches!(
            ep.take_reply(7, ticket, 0, &sink),
            ReplyOutcome::Unknown
        ));
        assert_eq!(ep.outstanding(), 0);
    }

    #[test]
    fn peer_origin_reflects_the_in_service_caller_and_fails_closed() {
        use tairix_abi::{ProcId, TrustDomain};
        let sink = RecordingSink::new();
        let ep = open_endpoint(&sink);
        // A caller with a minted process instance and a real capability.
        let mut caller = task_with(7, &[CapabilityId::SYSINFO_GLOBAL]);
        let minted = ProcId::from_raw([0x9E; 16]);
        caller = caller.with_proc_id(minted);

        let ticket = ep
            .post(&caller, POSTER_SCHED, b"ping", u64::MAX, NO_LIMIT, &sink)
            .expect("posted");
        // A pending (not-yet-received) call exposes no origin: the server
        // only learns a caller's identity while actively servicing it.
        assert_eq!(ep.peer_origin(ticket), None);
        // An unknown ticket never resolves.
        assert_eq!(ep.peer_origin(CallTicket(0xDEAD)), None);

        recv_one(&ep).expect("received");
        let origin = ep.peer_origin(ticket).expect("in-service origin");
        assert_eq!(origin.trust_domain(), TrustDomain::User);
        assert_eq!(origin.pid(), 7);
        assert_eq!(origin.proc_id(), minted);
        assert!(origin
            .capabilities()
            .holds_cap(CapabilityId::SYSINFO_GLOBAL));
        // It equals exactly what the caller's own record attests — proving
        // the server reads kernel-attested state, not anything on the wire.
        assert_eq!(origin, caller.attest_origin());

        // Once replied, the call leaves the in-service table and the origin
        // is no longer readable.
        let _ = ep.reply(ticket, b"pong", &sink).expect("replied");
        assert_eq!(ep.peer_origin(ticket), None);
    }

    #[test]
    fn recv_call_is_fifo_and_empty_yields_none() {
        let sink = RecordingSink::new();
        let ep = open_endpoint(&sink);
        let caller = task_with(7, &[]);
        assert!(recv_one(&ep).is_none());

        let t1 = ep
            .post(&caller, POSTER_SCHED, b"1", u64::MAX, NO_LIMIT, &sink)
            .expect("1");
        let t2 = ep
            .post(&caller, POSTER_SCHED, b"2", u64::MAX, NO_LIMIT, &sink)
            .expect("2");
        assert_ne!(t1, t2);
        assert_eq!(recv_one(&ep).expect("first").ticket, t1);
        assert_eq!(recv_one(&ep).expect("second").ticket, t2);
        assert!(recv_one(&ep).is_none());
    }

    #[test]
    fn recv_call_too_large_leaves_the_request_queued() {
        let sink = RecordingSink::new();
        let ep = open_endpoint(&sink);
        let caller = task_with(7, &[]);
        let ticket = ep
            .post(&caller, POSTER_SCHED, b"four", u64::MAX, NO_LIMIT, &sink)
            .expect("posted");

        // A buffer too small for the front request reports its size and does
        // not dequeue it (no lost request).
        assert!(matches!(
            ep.recv_call(3),
            RecvCall::TooLarge { request_len: 4 }
        ));
        assert_eq!(ep.outstanding(), 1);

        // A buffer that fits then receives the very same call.
        match ep.recv_call(4) {
            RecvCall::Received(call) => assert_eq!(call.ticket, ticket),
            other => panic!("expected the queued call, got {other:?}"),
        }
    }

    #[test]
    fn owner_reports_the_creating_task() {
        let sink = RecordingSink::new();
        let creator = task_with(0x4242, &[]).with_proc_id(tairix_abi::ProcId::from_raw([0x42; 16]));
        let ep = CallEndpoint::create(
            EndpointId(0xF),
            &creator,
            CapabilitySet::empty(),
            CapabilitySet::empty(),
            CallEndpointLimits {
                max_request: 16,
                max_reply: 16,
                per_sender: 2,
            },
            tairix_abi::RLIMIT_INFINITY,
            &sink,
        )
        .expect("created");
        assert_eq!(ep.owner(), 0x4242);
        assert_eq!(
            ep.owner_instance(),
            tairix_abi::ProcId::from_raw([0x42; 16])
        );
    }

    #[test]
    fn take_reply_for_unknown_ticket_is_unknown() {
        let sink = RecordingSink::new();
        let ep = open_endpoint(&sink);
        assert!(matches!(
            ep.take_reply(7, CallTicket(999), 0, &sink),
            ReplyOutcome::Unknown
        ));
    }

    #[test]
    fn a_reply_is_claimable_only_by_its_poster() {
        let sink = RecordingSink::new();
        let ep = open_endpoint(&sink);
        let caller = task_with(7, &[]);
        let ticket = ep
            .post(&caller, POSTER_SCHED, b"q", u64::MAX, NO_LIMIT, &sink)
            .expect("posted");
        recv_one(&ep).expect("received");
        let _ = ep.reply(ticket, b"r", &sink).expect("replied");

        // A different task may not claim it, and learns nothing.
        assert!(matches!(
            ep.take_reply(8, ticket, 0, &sink),
            ReplyOutcome::Unknown
        ));
        // The reply is preserved for its rightful owner.
        assert_eq!(ready_bytes(ep.take_reply(7, ticket, 0, &sink)), b"r");
    }

    #[test]
    fn pending_and_in_service_are_invisible_to_non_posters() {
        let sink = RecordingSink::new();
        let ep = open_endpoint(&sink);
        let caller = task_with(7, &[]);
        let ticket = ep
            .post(&caller, POSTER_SCHED, b"q", u64::MAX, NO_LIMIT, &sink)
            .expect("posted");
        // A non-poster polling the ticket while it is pending learns nothing.
        assert!(matches!(
            ep.take_reply(8, ticket, 0, &sink),
            ReplyOutcome::Unknown
        ));
        recv_one(&ep).expect("received");
        assert!(matches!(
            ep.take_reply(8, ticket, 0, &sink),
            ReplyOutcome::Unknown
        ));
    }

    #[test]
    fn reply_to_unknown_ticket_is_denied() {
        let sink = RecordingSink::new();
        let ep = open_endpoint(&sink);
        let err = ep
            .reply(CallTicket(42), b"r", &sink)
            .expect_err("unknown ticket");
        assert_eq!(err, Errno::NotFound);
        assert!(sink.ids().contains(&AuditEvent::CallReplyDenied.id().0));
    }

    /// A stalled server that misses its deadline leaves a record of its own.
    ///
    /// Retiring the ticket is what makes the server's eventual reply bounce,
    /// and that bounce used to be the *only* trace of the whole episode — an
    /// ERROR reading "reply denied" that named neither a timeout nor a
    /// device. An in-kernel caller (the filesystem's block path) takes no
    /// route through the syscall dispatcher's audit, so without this record
    /// a disk that stopped answering was invisible.
    #[test]
    fn an_elapsed_deadline_is_recorded_before_the_late_reply_bounces() {
        let sink = RecordingSink::new();
        let ep = open_endpoint(&sink);
        let caller = task_with(7, &[]);
        let ticket = ep
            .post(&caller, POSTER_SCHED, b"q", 100, NO_LIMIT, &sink)
            .expect("posted");
        recv_one(&ep).expect("server took the request");

        // The deadline elapses before the server answers.
        assert!(matches!(
            ep.take_reply(7, ticket, 100, &sink),
            ReplyOutcome::TimedOut
        ));
        assert!(
            sink.ids().contains(&AuditEvent::CallTimedOut.id().0),
            "the missed deadline must be recorded, not just its aftermath"
        );

        // The stalled server finally replies; the ticket is gone.
        let err = ep.reply(ticket, b"r", &sink).expect_err("ticket retired");
        assert_eq!(err, Errno::NotFound);
        assert!(sink.ids().contains(&AuditEvent::CallReplyDenied.id().0));
    }

    /// Retiring the ticket makes the timeout edge-triggered: a wait loop that
    /// polls again after the deadline must not re-record it.
    #[test]
    fn a_timed_out_call_is_recorded_once_however_often_it_is_polled() {
        let sink = RecordingSink::new();
        let ep = open_endpoint(&sink);
        let caller = task_with(7, &[]);
        let ticket = ep
            .post(&caller, POSTER_SCHED, b"q", 100, NO_LIMIT, &sink)
            .expect("posted");
        recv_one(&ep).expect("server took the request");
        for now in [100_u64, 101, 200] {
            let _ = ep.take_reply(7, ticket, now, &sink);
        }
        assert_eq!(
            sink.ids()
                .iter()
                .filter(|id| **id == AuditEvent::CallTimedOut.id().0)
                .count(),
            1
        );
    }

    #[test]
    fn reply_twice_is_denied_the_second_time() {
        let sink = RecordingSink::new();
        let ep = open_endpoint(&sink);
        let caller = task_with(7, &[]);
        let ticket = ep
            .post(&caller, POSTER_SCHED, b"q", u64::MAX, NO_LIMIT, &sink)
            .expect("posted");
        recv_one(&ep).expect("received");
        let _ = ep.reply(ticket, b"r", &sink).expect("first reply");
        // The ticket left the in-service table on the first reply.
        let err = ep.reply(ticket, b"r2", &sink).expect_err("second reply");
        assert_eq!(err, Errno::NotFound);
    }

    #[test]
    fn oversize_reply_is_denied_and_leaves_the_call_in_service() {
        let sink = RecordingSink::new();
        let creator = task_with(1, &[]);
        let ep = CallEndpoint::create(
            EndpointId(0xE),
            &creator,
            CapabilitySet::empty(),
            CapabilitySet::empty(),
            CallEndpointLimits {
                max_request: 64,
                max_reply: 4,
                per_sender: 4,
            },
            tairix_abi::RLIMIT_INFINITY,
            &sink,
        )
        .expect("ok");
        let caller = task_with(7, &[]);
        let ticket = ep
            .post(&caller, POSTER_SCHED, b"q", u64::MAX, NO_LIMIT, &sink)
            .expect("posted");
        recv_one(&ep).expect("received");
        let err = ep
            .reply(ticket, b"too long", &sink)
            .expect_err("oversize reply");
        assert_eq!(err, Errno::MessageTooLarge);
        assert!(sink.ids().contains(&AuditEvent::CallReplyDenied.id().0));
        // The call is still in service: a correctly-sized reply still works.
        let _ = ep.reply(ticket, b"ok", &sink).expect("retry");
        assert_eq!(ready_bytes(ep.take_reply(7, ticket, 0, &sink)), b"ok");
    }

    #[test]
    fn reply_returns_the_posters_scheduler_id() {
        let sink = RecordingSink::new();
        let ep = open_endpoint(&sink);
        let caller = task_with(7, &[]);
        let ticket = ep
            .post(&caller, 0xBEEF, b"q", u64::MAX, NO_LIMIT, &sink)
            .expect("posted");
        recv_one(&ep).expect("received");
        // The reply names exactly the scheduler id the post captured, so the
        // syscall layer wakes that one caller and no other.
        assert_eq!(replied(&ep, ticket, &sink), [0xBEEF]);
    }

    #[test]
    fn server_task_is_recorded_once_and_zero_is_rejected() {
        let sink = RecordingSink::new();
        let ep = open_endpoint(&sink);
        // Unrecorded until the server's first receive: posts fall back to
        // the broadcast wake.
        assert_eq!(ep.server_task(), None);
        // A zero id is the "unrecorded" sentinel and must not overwrite a
        // real recording (or be recordable at all).
        ep.record_server_task(0);
        assert_eq!(ep.server_task(), None);
        ep.record_server_task(42);
        assert_eq!(ep.server_task(), Some(42));
        // Recording is idempotent for the same server and a later record
        // (a restarted server task) supersedes the old id.
        ep.record_server_task(42);
        ep.record_server_task(43);
        assert_eq!(ep.server_task(), Some(43));
        // Zero still cannot forge the sentinel back in.
        ep.record_server_task(0);
        assert_eq!(ep.server_task(), Some(43));
    }

    #[test]
    fn destroy_cancels_in_flight_calls_and_audits_the_count() {
        let sink = RecordingSink::new();
        let ep = open_endpoint(&sink);
        let caller = task_with(7, &[]);
        // Post three, then drive them into three distinct states: the first
        // received-and-replied (completed), the second received (in service),
        // the third never received (pending).
        let t_done = ep
            .post(&caller, POSTER_SCHED, b"d", u64::MAX, NO_LIMIT, &sink)
            .expect("done");
        let t_in_service = ep
            .post(&caller, POSTER_SCHED, b"s", u64::MAX, NO_LIMIT, &sink)
            .expect("in service");
        let t_pending = ep
            .post(&caller, POSTER_SCHED, b"p", u64::MAX, NO_LIMIT, &sink)
            .expect("pending");
        assert_eq!(recv_one(&ep).expect("first").ticket, t_done);
        assert_eq!(recv_one(&ep).expect("second").ticket, t_in_service);
        let _ = ep.reply(t_done, b"r", &sink).expect("reply the first");
        assert_eq!(ep.outstanding(), 3);

        ep.destroy(&sink);

        for t in [t_pending, t_in_service, t_done] {
            assert!(matches!(
                ep.take_reply(7, t, 0, &sink),
                ReplyOutcome::Cancelled
            ));
        }
        assert_eq!(ep.outstanding(), 0);
        assert!(sink
            .ids()
            .contains(&AuditEvent::CallEndpointDestroyed.id().0));
    }

    #[test]
    fn destroy_is_idempotent() {
        let sink = RecordingSink::new();
        let ep = open_endpoint(&sink);
        ep.destroy(&sink);
        ep.destroy(&sink);
        assert!(ep.is_closed());
    }

    #[test]
    fn cancel_posted_by_scrubs_every_state_of_the_dead_poster() {
        // The keyboard-wedge regression: a killed class driver's final
        // queued URB submit survived its death, was later received and held
        // by the HCD, and the replacement driver's every submit was refused
        // as AlreadyExists. Cancellation on the poster's exit must scrub the
        // dead task's calls in all three states.
        let sink = RecordingSink::new();
        let ep = open_endpoint(&sink);
        let caller = task_with(7, &[]);
        let t_done = ep
            .post(&caller, POSTER_SCHED, b"d", u64::MAX, NO_LIMIT, &sink)
            .expect("done");
        let t_in_service = ep
            .post(&caller, POSTER_SCHED, b"s", u64::MAX, NO_LIMIT, &sink)
            .expect("in service");
        let t_pending = ep
            .post(&caller, POSTER_SCHED, b"p", u64::MAX, NO_LIMIT, &sink)
            .expect("pending");
        assert_eq!(recv_one(&ep).expect("first").ticket, t_done);
        assert_eq!(recv_one(&ep).expect("second").ticket, t_in_service);
        let _ = ep.reply(t_done, b"r", &sink).expect("reply the first");
        assert_eq!(ep.outstanding(), 3);

        assert_eq!(ep.cancel_posted_by(7, &sink), 3);

        // The pending call is never handed to the server...
        assert!(!ep.has_pending());
        assert!(recv_one(&ep).is_none());
        // ...a reply to the retired in-service ticket is refused fail-closed...
        assert_eq!(
            ep.reply(t_in_service, b"r", &sink).expect_err("cancelled"),
            Errno::NotFound
        );
        // ...and the unclaimed completed reply is discarded.
        assert!(matches!(
            ep.take_reply(7, t_pending, 0, &sink),
            ReplyOutcome::Unknown
        ));
        assert!(matches!(
            ep.take_reply(7, t_done, 0, &sink),
            ReplyOutcome::Unknown
        ));
        assert_eq!(ep.outstanding(), 0);
        assert!(sink.ids().contains(&AuditEvent::CallPosterVanished.id().0));
    }

    #[test]
    fn cancel_posted_by_leaves_other_posters_calls_untouched() {
        let sink = RecordingSink::new();
        let ep = open_endpoint(&sink);
        let dead = task_with(7, &[]);
        let live = task_with(8, &[]);
        ep.post(&dead, POSTER_SCHED, b"dead", u64::MAX, NO_LIMIT, &sink)
            .expect("dead");
        let t_live = ep
            .post(&live, POSTER_SCHED, b"live", u64::MAX, NO_LIMIT, &sink)
            .expect("live");

        assert_eq!(ep.cancel_posted_by(7, &sink), 1);

        // The live poster's call is still delivered and answerable.
        let got = recv_one(&ep).expect("live call survives");
        assert_eq!(got.ticket, t_live);
        assert_eq!(got.sender, 8);
        let _ = ep.reply(t_live, b"r", &sink).expect("replied");
        assert_eq!(ready_bytes(ep.take_reply(8, t_live, 0, &sink)), b"r");
    }

    #[test]
    fn cancel_posted_by_with_nothing_in_flight_is_silent() {
        let sink = RecordingSink::new();
        let ep = open_endpoint(&sink);
        let before = sink.len();
        assert_eq!(ep.cancel_posted_by(7, &sink), 0);
        // No calls cancelled: no audit record is emitted.
        assert_eq!(sink.len(), before);
    }

    #[test]
    fn a_pending_call_times_out_only_once_its_deadline_passes() {
        let sink = RecordingSink::new();
        let ep = open_endpoint(&sink);
        let caller = task_with(7, &[]);
        // A request with an absolute deadline of 100ns.
        let ticket = ep
            .post(&caller, POSTER_SCHED, b"slow", 100, NO_LIMIT, &sink)
            .expect("posted");
        // Before the deadline the caller is still pending, even received.
        assert!(matches!(
            ep.take_reply(7, ticket, 50, &sink),
            ReplyOutcome::Pending
        ));
        recv_one(&ep).expect("received");
        assert!(matches!(
            ep.take_reply(7, ticket, 99, &sink),
            ReplyOutcome::Pending
        ));
        // At/after the deadline with no reply, it fails closed and the ticket
        // is retired so the endpoint slot is freed.
        assert!(matches!(
            ep.take_reply(7, ticket, 100, &sink),
            ReplyOutcome::TimedOut
        ));
        assert_eq!(ep.outstanding(), 0);
        // A late reply for the retired ticket is refused fail-closed.
        assert_eq!(
            ep.reply(ticket, b"late", &sink).expect_err("retired"),
            Errno::NotFound
        );
    }

    #[test]
    fn a_ready_reply_beats_an_elapsed_deadline() {
        let sink = RecordingSink::new();
        let ep = open_endpoint(&sink);
        let caller = task_with(7, &[]);
        let ticket = ep
            .post(&caller, POSTER_SCHED, b"q", 100, NO_LIMIT, &sink)
            .expect("posted");
        recv_one(&ep).expect("received");
        let _ = ep.reply(ticket, b"r", &sink).expect("replied");
        // Even well past the deadline, a delivered reply is returned, never a
        // spurious timeout that would discard a completed answer.
        assert_eq!(
            ready_bytes(ep.take_reply(7, ticket, 1_000_000, &sink)),
            b"r"
        );
    }

    #[test]
    fn has_ready_reply_for_is_a_faithful_peek() {
        let sink = RecordingSink::new();
        let ep = open_endpoint(&sink);
        let caller = task_with(7, &[]);
        let ticket = ep
            .post(&caller, POSTER_SCHED, b"q", 100, NO_LIMIT, &sink)
            .expect("posted");
        // Nothing to reap while pending and inside the deadline.
        assert!(!ep.has_ready_reply_for(7, 50));
        // A non-poster never sees the reply as ready.
        assert!(!ep.has_ready_reply_for(8, 50));
        // The elapsed deadline is itself a reap-worthy event (a timeout).
        assert!(ep.has_ready_reply_for(7, 100));
        // A delivered reply is ready regardless of time.
        recv_one(&ep).expect("received");
        let _ = ep.reply(ticket, b"r", &sink).expect("replied");
        assert!(ep.has_ready_reply_for(7, 0));
        assert!(!ep.has_ready_reply_for(8, 0));
    }

    #[test]
    fn next_deadline_for_reports_the_soonest_outstanding_deadline() {
        let sink = RecordingSink::new();
        let ep = open_endpoint(&sink);
        let caller = task_with(7, &[]);
        assert_eq!(ep.next_deadline_for(7), u64::MAX);
        ep.post(&caller, POSTER_SCHED, b"a", 300, NO_LIMIT, &sink)
            .expect("a");
        ep.post(&caller, POSTER_SCHED, b"b", 150, NO_LIMIT, &sink)
            .expect("b");
        // Both are still pending; the nearest deadline is reported, and only
        // for the querying poster.
        assert_eq!(ep.next_deadline_for(7), 150);
        assert_eq!(ep.next_deadline_for(8), u64::MAX);
    }

    #[test]
    fn an_awaited_call_wakes_its_awaiter_wherever_it_sits() {
        let sink = RecordingSink::new();
        let ep = open_endpoint(&sink);
        let caller = task_with(7, &[]);
        let queued = ep
            .post(&caller, POSTER_SCHED, b"a", 300, NO_LIMIT, &sink)
            .expect("a");
        let served = ep
            .post(&caller, POSTER_SCHED, b"b", u64::MAX, NO_LIMIT, &sink)
            .expect("b");
        assert_eq!(ep.await_by(7, queued, 0xA1), Ok(300), "its own deadline");
        let first = recv_one(&ep).expect("a received");
        let second = recv_one(&ep).expect("b received");
        assert_eq!(
            ep.await_by(7, served, 0xB2),
            Ok(u64::MAX),
            "in service it is still named, and has no deadline"
        );
        // The awaiter is woken, and so is the poster, which may be watching
        // for the answer some other way.
        assert_eq!(replied(&ep, first.ticket, &sink), [POSTER_SCHED, 0xA1]);
        assert_eq!(replied(&ep, second.ticket, &sink), [POSTER_SCHED, 0xB2]);
    }

    /// One answer wakes one awaiter, so a second thread awaiting the same call
    /// is refused rather than left asleep when the first takes the answer.
    #[test]
    fn a_call_is_awaited_by_one_thread_at_a_time() {
        let sink = RecordingSink::new();
        let ep = open_endpoint(&sink);
        let caller = task_with(7, &[]);
        let ticket = ep
            .post(&caller, POSTER_SCHED, b"a", u64::MAX, NO_LIMIT, &sink)
            .expect("a");
        assert_eq!(ep.await_by(7, ticket, 0xA1), Ok(u64::MAX));
        assert_eq!(
            ep.await_by(7, ticket, 0xA1),
            Ok(u64::MAX),
            "again, the same thread"
        );
        assert_eq!(ep.await_by(7, ticket, 0xB2), Err(Errno::Busy));
        let call = recv_one(&ep).expect("received");
        assert_eq!(
            ep.await_by(7, ticket, 0xB2),
            Err(Errno::Busy),
            "in service too"
        );
        assert_eq!(replied(&ep, call.ticket, &sink), [POSTER_SCHED, 0xA1]);
    }

    /// A call withdrawn while a thread awaits it wakes that thread, which
    /// then finds it gone rather than waiting for an answer that never comes.
    #[test]
    fn withdrawing_an_awaited_call_wakes_its_awaiter() {
        let sink = RecordingSink::new();
        let ep = open_endpoint(&sink);
        let caller = task_with(7, &[]);
        let queued = ep
            .post(&caller, POSTER_SCHED, b"a", u64::MAX, NO_LIMIT, &sink)
            .expect("a");
        let served = ep
            .post(&caller, POSTER_SCHED, b"b", u64::MAX, NO_LIMIT, &sink)
            .expect("b");
        let _ = recv_one(&ep).expect("a received");
        assert_eq!(ep.await_by(7, queued, 0xA1), Ok(u64::MAX));
        assert_eq!(ep.await_by(7, served, 0xB2), Ok(u64::MAX));
        for (ticket, awaiter) in [(queued, 0xA1), (served, 0xB2)] {
            let woken = ep.cancel_one(7, ticket).expect("withdrawn");
            assert_eq!(tasks(&woken), [POSTER_SCHED, awaiter]);
            assert!(matches!(
                ep.take_reply(7, ticket, 0, &sink),
                ReplyOutcome::Unknown
            ));
        }
    }

    #[test]
    fn awaiting_a_call_settled_or_not_the_callers_changes_nothing() {
        let sink = RecordingSink::new();
        let ep = open_endpoint(&sink);
        let caller = task_with(7, &[]);
        let ticket = ep
            .post(&caller, POSTER_SCHED, b"a", 300, NO_LIMIT, &sink)
            .expect("a");
        assert_eq!(ep.await_by(8, ticket, 0xA1), Ok(u64::MAX), "another's call");
        let call = recv_one(&ep).expect("received");
        assert_eq!(replied(&ep, call.ticket, &sink), [POSTER_SCHED]);
        assert_eq!(ep.await_by(7, ticket, 0xA1), Ok(u64::MAX), "answered");
        assert_eq!(ready_bytes(ep.take_reply(7, ticket, 0, &sink)), b"r");
    }

    #[test]
    fn cancel_one_withdraws_only_the_posters_ticket() {
        let sink = RecordingSink::new();
        let ep = open_endpoint(&sink);
        let caller = task_with(7, &[]);
        let ticket = ep
            .post(&caller, POSTER_SCHED, b"q", u64::MAX, NO_LIMIT, &sink)
            .expect("posted");
        // A foreign claimant cancels nothing (no existence oracle).
        assert!(ep.cancel_one(8, ticket).is_none());
        assert_eq!(ep.outstanding(), 1);
        // The rightful poster withdraws it, freeing the slot.
        assert!(ep.cancel_one(7, ticket).is_some());
        assert_eq!(ep.outstanding(), 0);
        // A second cancel finds nothing.
        assert!(ep.cancel_one(7, ticket).is_none());
    }

    /// Every room the endpoint promises later operations is reserved, and the
    /// turn order names exactly the callers with a request waiting.
    fn assert_room_reserved(ep: &CallEndpoint) {
        let g = ep.inner.lock();
        let pending: usize = g.lanes.iter().map(|(_, lane)| lane.pending.len()).sum();
        assert_eq!(pending, g.pending, "the pending count is the lanes' sum");
        assert!(
            g.serving.capacity() >= g.serving.len() + g.pending,
            "every pending call has its place among the calls in service"
        );
        let served: usize = g.lanes.iter().map(|(_, lane)| lane.in_service.len()).sum();
        assert_eq!(served, g.serving.len(), "every call in service is indexed");
        for (sender, lane) in g.lanes.iter() {
            assert!(
                lane.in_service.capacity() >= lane.in_service.len() + lane.pending.len(),
                "every pending call has its place in service"
            );
            assert!(
                lane.completed.capacity() >= lane.completed.len() + lane.unanswered(),
                "every call a caller is owed has its reply slot"
            );
            let timed = lane.deadlines().filter(|&d| d != u64::MAX).count();
            assert_eq!(timed, lane.timed, "every timed call is counted");
            for call in &lane.in_service {
                assert_eq!(g.serving.get(&call.ticket), Some(&sender));
            }
            assert_eq!(
                lane.paid.as_ref().map(Charge::units),
                Some(lane.storage()),
                "a lane in use is its caller's, storage and all"
            );
        }
        assert_eq!(
            g.lanes.has_turn(),
            g.pending > 0,
            "a caller with a request waiting is in the turn order"
        );
    }

    fn endpoint_of_depth(per_sender: usize, sink: &RecordingSink) -> CallEndpoint {
        let creator = task_with(1, &[]);
        CallEndpoint::create(
            EndpointId(0xE0),
            &creator,
            CapabilitySet::empty(),
            CapabilitySet::empty(),
            CallEndpointLimits {
                max_request: 64,
                max_reply: 64,
                per_sender,
            },
            NO_LIMIT,
            sink,
        )
        .expect("endpoint")
    }

    /// The lock-out a shared total allowed: a caller that posted and never
    /// collected its replies held the endpoint against every other caller.
    #[test]
    fn a_full_caller_holds_only_its_own_depth() {
        let sink = RecordingSink::new();
        let ep = endpoint_of_depth(2, &sink);
        let hog = task_with(9, &[]);
        for request in [b"a", b"b"] {
            ep.post(&hog, POSTER_SCHED, request, u64::MAX, NO_LIMIT, &sink)
                .expect("posted");
        }
        assert_eq!(
            ep.post(&hog, POSTER_SCHED, b"c", u64::MAX, NO_LIMIT, &sink),
            Err(Errno::WouldBlock),
            "its own unanswered calls fill its own depth"
        );
        let other = task_with(7, &[]);
        for request in [b"x", b"y"] {
            ep.post(&other, POSTER_SCHED, request, u64::MAX, NO_LIMIT, &sink)
                .expect("another caller is never refused for the hog's calls");
        }
        assert_room_reserved(&ep);
    }

    /// Room comes back as the server answers, never as the caller collects:
    /// a blocking call waiting for room waits on the server, so a caller whose
    /// answers wait for it to collect them can never wait on itself.
    #[test]
    fn the_depth_bounds_unanswered_calls_and_an_answer_frees_room() {
        let sink = RecordingSink::new();
        let ep = endpoint_of_depth(2, &sink);
        let caller = task_with(9, &[]);
        let post = || ep.post(&caller, POSTER_SCHED, b"q", u64::MAX, NO_LIMIT, &sink);
        for _ in 0..2 {
            post().expect("posted");
        }
        ep.watch_room(0x10, 9).expect("recorded");
        assert_eq!(post(), Err(Errno::WouldBlock));
        let call = recv_one(&ep).expect("received");
        let woken = ep.reply(call.ticket, b"r", &sink).expect("replied");
        assert_eq!(
            tasks(&woken),
            [0x10, POSTER_SCHED],
            "the answer wakes what waits for room"
        );
        post().expect("the answer made room");
        assert_eq!(post(), Err(Errno::WouldBlock));
        while let Some(call) = recv_one(&ep) {
            let _ = ep.reply(call.ticket, b"r", &sink).expect("replied");
        }
        for _ in 0..2 {
            post().expect("uncollected answers never hold room");
        }
        assert_room_reserved(&ep);
    }

    #[test]
    fn recv_call_serves_callers_in_turn_each_oldest_first() {
        let sink = RecordingSink::new();
        let ep = endpoint_of_depth(4, &sink);
        let (a, b, c) = (task_with(1, &[]), task_with(2, &[]), task_with(3, &[]));
        for request in [b"a1", b"a2", b"a3"] {
            ep.post(&a, POSTER_SCHED, request, u64::MAX, NO_LIMIT, &sink)
                .expect("a");
        }
        ep.post(&b, POSTER_SCHED, b"b1", u64::MAX, NO_LIMIT, &sink)
            .expect("b");
        ep.post(&c, POSTER_SCHED, b"c1", u64::MAX, NO_LIMIT, &sink)
            .expect("c");
        ep.post(&b, POSTER_SCHED, b"b2", u64::MAX, NO_LIMIT, &sink)
            .expect("b");
        let order: alloc::vec::Vec<alloc::vec::Vec<u8>> = core::iter::from_fn(|| recv_one(&ep))
            .map(|call| call.request.as_bytes().to_vec())
            .collect();
        let order: alloc::vec::Vec<&[u8]> = order.iter().map(alloc::vec::Vec::as_slice).collect();
        assert_eq!(order, [&b"a1"[..], b"b1", b"c1", b"a2", b"b2", b"a3"]);
        assert_room_reserved(&ep);
    }

    #[test]
    fn a_too_large_request_keeps_its_callers_turn() {
        let sink = RecordingSink::new();
        let ep = endpoint_of_depth(4, &sink);
        let (a, b) = (task_with(1, &[]), task_with(2, &[]));
        ep.post(&a, POSTER_SCHED, b"large", u64::MAX, NO_LIMIT, &sink)
            .expect("a");
        ep.post(&b, POSTER_SCHED, b"s", u64::MAX, NO_LIMIT, &sink)
            .expect("b");
        assert!(matches!(
            ep.recv_call(2),
            RecvCall::TooLarge { request_len: 5 }
        ));
        assert_eq!(recv_one(&ep).expect("a's turn").sender, 1);
        assert_eq!(recv_one(&ep).expect("then b's").sender, 2);
    }

    #[test]
    fn a_call_is_charged_to_its_caller_until_it_ends_however_it_ends() {
        let sink = RecordingSink::new();
        let ep = endpoint_of_depth(8, &sink);
        let caller = task_with(7, &[]);
        let charge = CallEndpointLimits {
            max_request: 64,
            max_reply: 64,
            per_sender: 8,
        }
        .call_charge();
        let held = || caller.charges().held(LimitKind::IpcQueueBytes);
        let lane = || ep.inner.lock().lanes.get(7).map_or(0, CallLane::storage);
        let post = |deadline| {
            ep.post(&caller, POSTER_SCHED, b"q", deadline, NO_LIMIT, &sink)
                .expect("posted")
        };
        // Collected.
        let collected = post(u64::MAX);
        assert_eq!(held(), lane() + charge, "the call and the lane it opened");
        let call = recv_one(&ep).expect("received");
        assert_eq!(
            held(),
            lane() + charge,
            "a call in service is still its caller's"
        );
        let _ = ep.reply(call.ticket, b"r", &sink).expect("replied");
        assert_eq!(
            held(),
            lane() + charge,
            "an uncollected reply is still its caller's"
        );
        assert_eq!(ready_bytes(ep.take_reply(7, collected, 0, &sink)), b"r");
        assert_eq!(held(), 0, "the idle lane went back to the owner's spares");
        // Timed out, pending and in service.
        let queued = post(10);
        assert!(matches!(
            ep.take_reply(7, queued, 10, &sink),
            ReplyOutcome::TimedOut
        ));
        let served = post(10);
        let _ = recv_one(&ep).expect("received");
        assert!(matches!(
            ep.take_reply(7, served, 10, &sink),
            ReplyOutcome::TimedOut
        ));
        assert_eq!(held(), 0);
        // Withdrawn.
        let withdrawn = post(u64::MAX);
        assert!(ep.cancel_one(7, withdrawn).is_some());
        assert_eq!(held(), 0);
        // Its caller gone, or the endpoint torn down.
        let _ = post(u64::MAX);
        let _ = post(u64::MAX);
        assert_eq!(ep.cancel_posted_by(7, &sink), 2);
        assert_eq!(held(), 0);
        let _ = post(u64::MAX);
        ep.destroy(&sink);
        assert_eq!(held(), 0);
    }

    #[test]
    fn a_caller_past_its_own_budget_is_refused_and_charged_nothing() {
        let sink = RecordingSink::new();
        let ep = endpoint_of_depth(8, &sink);
        let caller = task_with(7, &[]);
        let lane = CallLane::fresh(8).expect("a lane").storage();
        let limit = lane
            + CallEndpointLimits {
                max_request: 64,
                max_reply: 64,
                per_sender: 8,
            }
            .call_charge();
        ep.post(&caller, POSTER_SCHED, b"one", u64::MAX, limit, &sink)
            .expect("fits");
        assert_eq!(
            ep.post(&caller, POSTER_SCHED, b"two", u64::MAX, limit, &sink),
            Err(Errno::LimitExceeded)
        );
        assert!(sink
            .ids()
            .contains(&AuditEvent::IpcSenderLimitReached.id().0));
        assert_eq!(caller.charges().held(LimitKind::IpcQueueBytes), limit);
        assert_eq!(ep.outstanding(), 1);
        let other = task_with(9, &[]);
        ep.post(&other, POSTER_SCHED, b"mine", u64::MAX, limit, &sink)
            .expect("one caller's budget is never another's");
    }

    /// Paying for its lane is the caller's own budget refusing it, whether the
    /// lane is fresh or a spare another caller left, and is recorded as that,
    /// never as the heap failing.
    #[test]
    fn a_caller_that_cannot_pay_for_its_lane_is_refused_as_over_its_limit() {
        let sink = RecordingSink::new();
        let ep = endpoint_of_depth(8, &sink);
        let charge = CallEndpointLimits {
            max_request: 64,
            max_reply: 64,
            per_sender: 8,
        }
        .call_charge();
        let fresh = task_with(7, &[]);
        assert_eq!(
            ep.post(&fresh, POSTER_SCHED, b"q", u64::MAX, charge, &sink),
            Err(Errno::LimitExceeded),
            "the call fits its budget but a fresh lane does not"
        );
        let early = task_with(8, &[]);
        let ticket = ep
            .post(&early, POSTER_SCHED, b"q", u64::MAX, NO_LIMIT, &sink)
            .expect("posted");
        let call = recv_one(&ep).expect("received");
        let _ = ep.reply(call.ticket, b"r", &sink).expect("replied");
        let _ = ready_bytes(ep.take_reply(8, ticket, 0, &sink));
        assert_eq!(ep.inner.lock().lanes.spares(), 1, "its lane is kept");
        let late = task_with(9, &[]);
        assert_eq!(
            ep.post(&late, POSTER_SCHED, b"q", u64::MAX, charge, &sink),
            Err(Errno::LimitExceeded),
            "nor does the spare it would take"
        );
        let refused = AuditEvent::IpcSenderLimitReached.id().0;
        assert_eq!(sink.ids().iter().filter(|&&id| id == refused).count(), 2);
        assert!(!sink.ids().contains(&AuditEvent::PayloadAllocFailed.id().0));
        for caller in [&fresh, &late] {
            assert_eq!(caller.charges().held(LimitKind::IpcQueueBytes), 0);
        }
    }

    /// A lane grows as its caller's calls need, and only as far as the caller
    /// can pay: its budget bounds what it holds, the lane's storage included.
    #[test]
    fn a_lane_grows_only_as_far_as_its_caller_can_pay() {
        let sink = RecordingSink::new();
        let ep = endpoint_of_depth(8, &sink);
        let caller = task_with(7, &[]);
        let charge = CallEndpointLimits {
            max_request: 64,
            max_reply: 64,
            per_sender: 8,
        }
        .call_charge();
        let limit = CallLane::fresh(8).expect("a lane").storage() + 5 * charge;
        let post = || ep.post(&caller, POSTER_SCHED, b"q", u64::MAX, limit, &sink);
        for _ in 0..SPARE_LANE_ITEMS {
            post().expect("the lane as made has room");
        }
        assert_eq!(
            post(),
            Err(Errno::LimitExceeded),
            "growing the lane would carry its caller past its limit"
        );
        assert!(caller.charges().held(LimitKind::IpcQueueBytes) <= limit);
        assert_eq!(ep.outstanding(), SPARE_LANE_ITEMS);
        assert_room_reserved(&ep);
    }

    /// A caller that never collects its answers is refused once it holds the
    /// ceiling of them, however large its budget, so its lane stays a bounded
    /// search; collecting one lets it post again, and no other caller is
    /// refused for it.
    #[test]
    fn a_caller_leaving_its_answers_uncollected_is_refused_at_the_ceiling() {
        let sink = RecordingSink::new();
        let ep = endpoint_of_depth(1, &sink);
        let hoarder = task_with(9, &[]);
        let post = || ep.post(&hoarder, POSTER_SCHED, b"q", u64::MAX, NO_LIMIT, &sink);
        let mut answered = alloc::vec::Vec::new();
        for _ in 0..IPC_SENDER_DEPTH_MAX {
            answered.push(post().expect("below the ceiling"));
            let call = recv_one(&ep).expect("received");
            let _ = ep.reply(call.ticket, b"r", &sink).expect("replied");
        }
        assert_eq!(post(), Err(Errno::LimitExceeded));
        assert!(sink
            .ids()
            .contains(&AuditEvent::IpcSenderLimitReached.id().0));
        assert_eq!(ep.outstanding(), IPC_SENDER_DEPTH_MAX);
        let other = task_with(7, &[]);
        ep.post(&other, POSTER_SCHED, b"q", u64::MAX, NO_LIMIT, &sink)
            .expect("another caller is never refused for it");
        let _ = ready_bytes(ep.take_reply(9, answered[0], 0, &sink));
        post().expect("a collected answer lets it post again");
        assert_room_reserved(&ep);
    }

    /// The calls posted below the ceiling may all be answered, taking a caller
    /// past it, and it is then refused, never told to wait, until it has
    /// collected enough to hold fewer: collecting one is not always enough.
    #[test]
    fn a_caller_answered_past_the_ceiling_collects_below_it_to_post() {
        let sink = RecordingSink::new();
        let ep = endpoint_of_depth(2, &sink);
        let hoarder = task_with(9, &[]);
        let post = || ep.post(&hoarder, POSTER_SCHED, b"q", u64::MAX, NO_LIMIT, &sink);
        let answer = || {
            let call = recv_one(&ep).expect("received");
            let _ = ep.reply(call.ticket, b"r", &sink).expect("replied");
        };
        let mut answered = alloc::vec::Vec::new();
        for _ in 1..IPC_SENDER_DEPTH_MAX {
            answered.push(post().expect("below the ceiling"));
            answer();
        }
        for _ in 0..2 {
            answered.push(post().expect("below the ceiling, to its depth"));
        }
        answer();
        answer();
        assert_eq!(
            post(),
            Err(Errno::LimitExceeded),
            "past it, never WouldBlock"
        );
        assert!(ep.has_room_for(9), "no room is waited for");
        let _ = ready_bytes(ep.take_reply(9, answered[0], 0, &sink));
        assert_eq!(
            post(),
            Err(Errno::LimitExceeded),
            "one collected is not enough"
        );
        let _ = ready_bytes(ep.take_reply(9, answered[1], 0, &sink));
        post().expect("below the ceiling again");
        assert_room_reserved(&ep);
    }

    /// Room is the depth of calls waiting on the server, exactly what refuses
    /// a post `WouldBlock`: an answer leaves room whether or not it is
    /// collected, and a closed endpoint has room, so a woken caller posts and
    /// finds it gone.
    #[test]
    fn room_is_the_depth_of_calls_waiting_on_the_server() {
        let sink = RecordingSink::new();
        let ep = endpoint_of_depth(2, &sink);
        let caller = task_with(9, &[]);
        let post = || ep.post(&caller, POSTER_SCHED, b"q", u64::MAX, NO_LIMIT, &sink);
        assert!(ep.has_room_for(9), "a caller with no lane has room");
        for _ in 0..2 {
            post().expect("posted");
        }
        assert!(!ep.has_room_for(9), "its depth is waiting on the server");
        assert_eq!(post(), Err(Errno::WouldBlock));
        assert!(ep.has_room_for(7), "another caller's room is untouched");
        let call = recv_one(&ep).expect("received");
        let _ = ep.reply(call.ticket, b"r", &sink).expect("replied");
        assert!(ep.has_room_for(9), "an answer makes room, collected or not");
        ep.destroy(&sink);
        assert!(
            ep.has_room_for(9),
            "a closed endpoint has room to find it gone"
        );
    }

    fn named(wake: &RoomWake<u64>) -> alloc::vec::Vec<u64> {
        let mut tasks: alloc::vec::Vec<u64> = wake.tasks().collect();
        tasks.sort_unstable();
        tasks
    }

    #[test]
    fn room_is_woken_only_for_the_caller_whose_lane_freed() {
        let sink = RecordingSink::new();
        let ep = endpoint_of_depth(2, &sink);
        ep.watch_room(0x10, 7).expect("recorded");
        ep.watch_room(0x11, 7).expect("recorded");
        ep.watch_room(0x20, 9).expect("recorded");
        ep.watch_room(0x10, 7).expect("recorded once");
        ep.unwatch_room(0x11, 7);
        assert_eq!(named(&ep.room_wake_for(7)), [0x10]);
        assert!(ep.room_wake_for(7).is_empty(), "each wait is woken once");
        assert_eq!(named(&ep.room_wake_for(9)), [0x20]);
        assert!(ep.room_wake_for(5).is_empty());
    }

    /// However many threads wait for one caller's room, each is woken, and
    /// never a thread waiting at any other caller's.
    #[test]
    fn every_thread_waiting_as_a_caller_is_woken_and_no_other() {
        let sink = RecordingSink::new();
        let ep = endpoint_of_depth(1, &sink);
        for task in 0..64 {
            ep.watch_room(task, 7).expect("recorded");
        }
        ep.watch_room(0x99, 8).expect("recorded");
        assert_eq!(
            named(&ep.room_wake_for(7)),
            (0..64).collect::<alloc::vec::Vec<u64>>()
        );
        assert_eq!(named(&ep.room_wake_for(8)), [0x99]);
    }

    /// A caller that has ended is never woken for room, and leaves no record
    /// behind to be woken in its place later.
    #[test]
    fn an_ended_caller_leaves_no_wait_for_room_behind() {
        let sink = RecordingSink::new();
        let ep = endpoint_of_depth(1, &sink);
        let caller = task_with(7, &[]);
        ep.post(&caller, POSTER_SCHED, b"q", u64::MAX, NO_LIMIT, &sink)
            .expect("posted");
        ep.watch_room(0x10, 7).expect("recorded");
        assert_eq!(ep.cancel_posted_by(7, &sink), 1);
        assert!(ep.room_wake_for(7).is_empty());
    }

    /// A reply finds its call by ticket, whichever of many callers posted it.
    #[test]
    fn a_reply_finds_its_caller_among_many() {
        let sink = RecordingSink::new();
        let ep = endpoint_of_depth(4, &sink);
        let callers: alloc::vec::Vec<TaskCapabilities> =
            (1..=40).map(|id| task_with(id, &[])).collect();
        for caller in &callers {
            for _ in 0..2 {
                ep.post(caller, caller.process().0, b"q", u64::MAX, NO_LIMIT, &sink)
                    .expect("posted");
            }
        }
        let mut received = alloc::vec::Vec::new();
        while let Some(call) = recv_one(&ep) {
            assert_eq!(
                ep.peer_origin(call.ticket).map(|origin| origin.pid()),
                Some(call.sender)
            );
            received.push(call);
        }
        assert_room_reserved(&ep);
        for call in received.into_iter().rev() {
            assert_eq!(replied(&ep, call.ticket, &sink), [call.sender]);
        }
        assert_room_reserved(&ep);
    }

    #[test]
    fn receive_reply_and_collection_find_their_room_reserved() {
        let sink = RecordingSink::new();
        let ep = endpoint_of_depth(4, &sink);
        let callers = [task_with(1, &[]), task_with(2, &[]), task_with(3, &[])];
        let mut tickets = alloc::vec::Vec::new();
        for caller in &callers {
            for _ in 0..3 {
                let ticket = ep
                    .post(caller, POSTER_SCHED, b"q", u64::MAX, NO_LIMIT, &sink)
                    .expect("posted");
                tickets.push((caller.process().0, ticket));
                assert_room_reserved(&ep);
            }
        }
        while let Some(call) = recv_one(&ep) {
            assert_room_reserved(&ep);
            let _ = ep.reply(call.ticket, b"r", &sink).expect("replied");
            assert_room_reserved(&ep);
        }
        for (caller, ticket) in tickets {
            assert_eq!(ready_bytes(ep.take_reply(caller, ticket, 0, &sink)), b"r");
            assert_room_reserved(&ep);
        }
        assert_eq!(ep.outstanding(), 0);
    }

    #[test]
    fn a_callers_lane_is_kept_for_reuse_once_it_holds_nothing() {
        let sink = RecordingSink::new();
        let ep = endpoint_of_depth(4, &sink);
        let caller = task_with(7, &[]);
        for _ in 0..3 {
            let ticket = ep
                .post(&caller, POSTER_SCHED, b"q", u64::MAX, NO_LIMIT, &sink)
                .expect("posted");
            let call = recv_one(&ep).expect("received");
            let _ = ep.reply(call.ticket, b"r", &sink).expect("replied");
            let _ = ready_bytes(ep.take_reply(7, ticket, 0, &sink));
            assert_eq!(ep.inner.lock().lanes.len(), 0, "an idle lane is retired");
        }
        assert_eq!(
            ep.inner.lock().lanes.spares(),
            1,
            "and its storage kept, so the next post allocates no lane"
        );
    }

    /// The settle count moves exactly when a caller's look could change: an
    /// answer, a withdrawal, a timeout, a dead poster's calls cancelled, the
    /// teardown. Posting, receiving and collecting move it not at all.
    #[test]
    fn the_settle_mark_moves_only_when_a_call_settles() {
        /// What `act` returned, and whether it moved `ep`'s settle mark.
        fn settles<T>(ep: &CallEndpoint, act: impl FnOnce() -> T) -> (T, bool) {
            let mark = ep.settle_mark();
            let out = act();
            (out, ep.settle_mark() != mark)
        }
        let sink = RecordingSink::new();
        let ep = open_endpoint(&sink);
        let caller = task_with(7, &[]);
        let post = || {
            ep.post(&caller, POSTER_SCHED, b"q", u64::MAX, NO_LIMIT, &sink)
                .expect("posted")
        };

        let (ticket, moved) = settles(&ep, post);
        assert!(!moved, "a post settles nothing");
        let (call, moved) = settles(&ep, || recv_one(&ep).expect("received"));
        assert!(!moved, "nor does a receive");
        let (_, moved) = settles(&ep, || ep.reply(call.ticket, b"r", &sink).expect("replied"));
        assert!(moved, "an answer settles its call");
        let (_, moved) = settles(&ep, || ready_bytes(ep.take_reply(7, ticket, 0, &sink)));
        assert!(!moved, "collecting it settles nothing more");

        let withdrawn = post();
        let (_, moved) = settles(&ep, || ep.cancel_one(7, withdrawn).expect("withdrawn"));
        assert!(moved, "a withdrawal settles its call");
        let timed = ep
            .post(&caller, POSTER_SCHED, b"q", 5, NO_LIMIT, &sink)
            .expect("posted");
        let (outcome, moved) = settles(&ep, || ep.take_reply(7, timed, 5, &sink));
        assert!(matches!(outcome, ReplyOutcome::TimedOut));
        assert!(moved, "so does a timeout, which frees its room");
        let _ = post();
        let (cancelled, moved) = settles(&ep, || ep.cancel_posted_by(7, &sink));
        assert_eq!(cancelled, 1);
        assert!(moved, "so does a dead poster's call cancelled");
        let _ = post();
        let ((), moved) = settles(&ep, || ep.destroy(&sink));
        assert!(moved, "and the teardown");
    }

    /// An unparked caller's wait never takes the endpoint's lock: with the
    /// lock held elsewhere it still runs, and ends as soon as a call
    /// settles. A wait that polled `take_reply` would sit on that lock, the
    /// only way an answer gets in.
    #[test]
    fn an_unparked_wait_ends_on_a_settle_while_the_lock_is_held_elsewhere() {
        extern crate std;
        use std::sync::{mpsc, Arc};

        let sink = RecordingSink::new();
        let ep = Arc::new(open_endpoint(&sink));
        let mark = ep.settle_mark();
        let held = ep.inner.lock();
        let (spinning, spun) = mpsc::channel();
        let waiter = {
            let ep = Arc::clone(&ep);
            std::thread::spawn(move || {
                ep.await_settle(mark, || {
                    let _ = spinning.send(());
                    false
                });
            })
        };
        spun.recv().expect("the wait runs with the lock held");
        ep.settle();
        waiter.join().expect("the wait ends on the settle");
        drop(held);
    }

    #[test]
    fn an_idle_endpoint_hands_back_an_in_service_table_a_burst_grew() {
        let sink = RecordingSink::new();
        let ep = endpoint_of_depth(IPC_SENDER_DEPTH_MAX, &sink);
        let caller = task_with(7, &[]);
        let tickets: alloc::vec::Vec<CallTicket> = (0..2 * IDLE_IN_SERVICE)
            .map(|_| {
                ep.post(&caller, POSTER_SCHED, b"q", u64::MAX, NO_LIMIT, &sink)
                    .expect("posted")
            })
            .collect();
        while let Some(call) = recv_one(&ep) {
            let _ = ep.reply(call.ticket, b"r", &sink).expect("replied");
        }
        assert!(ep.inner.lock().serving.capacity() > IDLE_IN_SERVICE);
        for ticket in tickets {
            let _ = ready_bytes(ep.take_reply(7, ticket, 0, &sink));
        }
        assert_eq!(
            ep.inner.lock().serving.capacity(),
            0,
            "nothing a burst grew outlives it"
        );
    }
}
