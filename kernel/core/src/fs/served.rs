//! The kernel's proxy for a volume a user-space filesystem driver serves
//! (`plans/FSSERVE.md` §3).
//!
//! [`ServedFs`] implements the filesystem driver traits by asking the driver
//! over `fsserve-v1`: each call is encoded, posted on the driver's endpoint
//! under a synthetic kernel identity, and parked on until the reply lands.
//! The driver is a peer, not a part of the kernel, so every reply is decoded
//! against the request it answers and a malformed one fails the call closed.
//!
//! A call is abandoned when the driver goes [`SERVED_IDLE_NS`] neither
//! answering nor touching its volume, or when the caller is being killed.
//! The driver may still be working on an abandoned call, so the shared
//! window is quarantined: nothing stages data in it or reads data out of it
//! until a [`Request::Release`] round trip proves the driver has finished
//! with every earlier call.
//!
//! A call on the driver's holds is abandoned without being withdrawn, since
//! the kernel counts every hold it takes and lets go: it is waited out before
//! any later call is posted, and a pin the driver turns out to have granted,
//! which no descriptor owns, is let go again. A release that cannot be sent
//! meanwhile answers [`DriverError::Busy`], and the registration keeps it.
//!
//! A volume with other writers is served by several *lanes* ([`ServedFs::lanes`]):
//! each is this proxy over the shared endpoint, moving its data through its
//! own span of the window, so its quarantine and its holds are its own and a
//! call on one lane never waits for another's.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use tairix_abi::driver::filesystem::{
    AllocationClass, DirEntry, DirVisit, FilesystemAttrs, FilesystemAttrsFs,
    FilesystemAttrsProvider, FilesystemRead, FilesystemSecurity, FilesystemStats, FilesystemWrite,
    NameMatching, NodeId, NodeIdentity, NodeInfo, NodeKind, NodeSecurity, Unpinned, VolumeHost,
    VolumeStats,
};
use tairix_abi::fs::{SeekRegion, FS_ATTR_KEY_MAX, FS_ATTR_VALUE_MAX, FS_SYMLINK_MAX};
use tairix_abi::fsserve::{
    decode_reply, encode_request, Answer, DirEnd, Lanes, Op, Request, ServeOffer, ServerState,
    Span, FSSERVE_REPLY_MAX, FSSERVE_REQUEST_MAX, SERVED_IDLE_NS,
};
use tairix_abi::netattach::OfferId;
use tairix_abi::sysinfo::MountAvailability;
use tairix_abi::{CapabilityId, DriverError, DriverHandle, Errno};
use tairix_caps::CapabilitySet;
use tairix_kernel_ipc::{CallEndpoint, CallTicket, ReplyOutcome};
use tairix_kernel_mem::SensitiveBuffer;
use tairix_kernel_sec::{ProcessId, TaskCapabilities, UserId};
use tairix_log::{Field, FieldValue, Level, Sink};

use crate::audit::{emit, AuditEvent};
use crate::sharedreg::KernelHold;
use crate::waitq::{
    nearest_timed_deadline, park_calling_task, serve_wake, serve_wake_task, wait_arch,
    WaitQueueArch, CALL_WAITQ, NO_DEADLINE,
};

/// The most entries one listing call hands its caller before the driver is
/// taken to be inventing them: a directory this large is no removable
/// volume's, and the bound keeps a hostile driver from holding a caller in an
/// endless listing.
const LISTING_MAX: usize = 1 << 24;

/// How many calls one lane of a served volume puts on its endpoint at once:
/// one call out at a time, and the release after a call it abandoned.
pub const LANE_ENDPOINT_CAPACITY: usize = 2;

/// Where the synthetic identities the proxies post under are numbered from:
/// readable in an audit record, and disjoint from every real process.
const POSTER_BASE: u64 = 0xF5E0_0000_0000_0000;

/// The next proxy identity to mint.
static NEXT_POSTER: AtomicU64 = AtomicU64::new(POSTER_BASE);

/// The serial of the next endpoint the kernel creates for a served volume.
static NEXT_SERVED_ENDPOINT: AtomicU32 = AtomicU32::new(0);

/// The next network offer's id.
static NEXT_OFFER: AtomicU64 = AtomicU64::new(1);

/// A served-volume endpoint id no endpoint has held this boot, or `None` once
/// the range is spent.
#[must_use]
pub fn mint_served_endpoint() -> Option<u64> {
    NEXT_SERVED_ENDPOINT
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
        .ok()
        .map(tairix_abi::fsserve::served_endpoint)
}

/// A network offer id no offer has held this boot, or `None` once the range is
/// spent.
#[must_use]
pub fn mint_offer() -> Option<OfferId> {
    NEXT_OFFER
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
        .ok()
        .and_then(OfferId::new)
}

/// What a served volume's driver is doing with its block grant, which is how
/// the proxy tells a driver waiting on its device from one that has stopped.
#[derive(Debug, Default)]
pub struct ServerActivity {
    busy: AtomicU32,
    progress: AtomicU64,
}

impl ServerActivity {
    /// The driver began a transfer on its volume; it is working until the
    /// returned guard drops.
    pub fn enter(&self) -> ActivityGuard<'_> {
        self.busy.fetch_add(1, Ordering::Relaxed);
        ActivityGuard(self)
    }

    /// The driver showed life outside any transfer: a network server's word
    /// that it is still working on the calls it holds.
    pub fn touch(&self) {
        self.progress.fetch_add(1, Ordering::Relaxed);
    }

    /// A reading that changes whenever the driver makes progress, or `None`
    /// while it is inside a transfer.
    fn mark(&self) -> Option<u64> {
        (self.busy.load(Ordering::Relaxed) == 0).then(|| self.progress.load(Ordering::Relaxed))
    }
}

/// A transfer in progress on a served volume.
pub struct ActivityGuard<'a>(&'a ServerActivity);

impl Drop for ActivityGuard<'_> {
    fn drop(&mut self) {
        self.0.progress.fetch_add(1, Ordering::Relaxed);
        self.0.busy.fetch_sub(1, Ordering::Relaxed);
    }
}

/// What a served volume reports of itself to whoever must know without
/// taking its mount's lock: the unplug classification and the mount
/// listing.
#[derive(Debug, Default)]
pub struct ServedState {
    unpublished: AtomicBool,
    degraded: AtomicBool,
}

impl ServedState {
    /// The driver last said it holds writes it has not published: lost if
    /// the medium leaves now.
    #[must_use]
    pub fn holds_unpublished(&self) -> bool {
        self.unpublished.load(Ordering::Relaxed)
    }

    /// How the driver's conduct reads: degraded after a malformed reply or a
    /// stall, until it answers cleanly again.
    #[must_use]
    pub fn availability(&self) -> MountAvailability {
        if self.degraded.load(Ordering::Relaxed) {
            MountAvailability::Degraded
        } else {
            MountAvailability::Available
        }
    }
}

/// Why a call returned no reply.
#[derive(Copy, Clone, Debug)]
enum Unanswered {
    /// The driver stopped answering.
    Stalled,
    /// The caller is being killed.
    Killed,
    /// The driver's endpoint is gone.
    Gone,
}

/// How a call's driver was last judged: what it was doing, and when it is
/// judged again. Carried across waits on one call, so a driver found stopped
/// earns no fresh budget until it shows life.
#[derive(Copy, Clone, Debug)]
struct Liveness {
    mark: Option<u64>,
    judged_at: u64,
    /// The stall found since it last showed life is recorded.
    recorded: bool,
}

impl Liveness {
    /// A budget from `now`, the driver's activity reading `mark`.
    fn from(mark: Option<u64>, now: u64) -> Self {
        Self {
            mark,
            judged_at: now.saturating_add(SERVED_IDLE_NS),
            recorded: false,
        }
    }
}

/// A call on the driver's holds.
#[derive(Copy, Clone, Debug)]
enum HoldCall {
    Pin(NodeId),
    Unpin(NodeId),
    ReleasePins,
}

impl HoldCall {
    fn of(request: &Request<'_>) -> Option<Self> {
        match *request {
            Request::Pin { node } => Some(Self::Pin(node)),
            Request::Unpin { pinned } => Some(Self::Unpin(pinned)),
            Request::ReleasePins => Some(Self::ReleasePins),
            _ => None,
        }
    }

    fn request(self) -> Request<'static> {
        match self {
            Self::Pin(node) => Request::Pin { node },
            Self::Unpin(pinned) => Request::Unpin { pinned },
            Self::ReleasePins => Request::ReleasePins,
        }
    }
}

/// A call on the driver's holds not yet accounted for.
enum Unsettled {
    /// Abandoned unanswered: the driver may yet take or let go of the hold.
    Asked {
        ticket: CallTicket,
        call: HoldCall,
        liveness: Liveness,
    },
    /// A pin it answered after it was abandoned, which no descriptor owns:
    /// still to be let go.
    Owed(NodeId),
}

/// Why a call failed, and whether its request reached the driver.
#[derive(Copy, Clone, Debug)]
enum Failed {
    /// Never posted: the driver did nothing.
    Unsent(DriverError),
    /// Posted: the driver may have acted on it.
    Sent(DriverError),
}

impl From<Failed> for DriverError {
    fn from(failed: Failed) -> Self {
        match failed {
            Failed::Unsent(err) | Failed::Sent(err) => err,
        }
    }
}

/// A volume served by a user-space driver, seen as a filesystem driver: one
/// lane of it.
pub struct ServedFs {
    endpoint: Arc<CallEndpoint>,
    window: Arc<KernelHold>,
    /// The part of the window this lane moves data through.
    span: Span,
    caps: TaskCapabilities,
    audit: &'static (dyn Sink + Sync),
    offer: ServeOffer,
    activity: Arc<ServerActivity>,
    state: Arc<ServedState>,
    /// An abandoned call may still be writing the window.
    quarantined: bool,
    /// The call on the driver's holds every later call waits for.
    unsettled: Option<Unsettled>,
    class: AllocationClass,
    host: Option<(DriverHandle, &'static dyn VolumeHost)>,
    in_reserve: bool,
    /// The request being encoded, allocated once.
    frame: Vec<u8>,
}

impl ServedFs {
    /// Serve the volume the driver `offer`ed over `endpoint`, moving file
    /// data through `window` and judging the driver's liveness by
    /// `activity`.
    ///
    /// The proxy posts under a synthetic identity of its own holding exactly
    /// `CAP_IPC_ENDPOINT`, the capability a served endpoint asks of its
    /// senders.
    ///
    /// # Errors
    ///
    /// [`Errno::LengthOutOfRange`] for a window of any length but
    /// [`FSSERVE_WINDOW_LEN`](tairix_abi::fsserve::FSSERVE_WINDOW_LEN);
    /// [`Errno::MessageTooLarge`] for an endpoint whose
    /// frame bounds cannot carry the protocol; [`Errno::OutOfMemory`] if the
    /// request buffer cannot be allocated.
    pub fn new(
        endpoint: &Arc<CallEndpoint>,
        window: KernelHold,
        offer: ServeOffer,
        activity: &Arc<ServerActivity>,
        audit: &'static (dyn Sink + Sync),
    ) -> Result<Self, Errno> {
        let window = Arc::try_new(window).map_err(|_| Errno::OutOfMemory)?;
        let mut lanes = Self::lanes(endpoint, &window, Lanes::ONE, offer, activity, audit)?;
        lanes.pop().ok_or(Errno::OutOfRange)
    }

    /// Serve the volume the driver `offer`ed over `endpoint` by `lanes`
    /// lanes, each moving file data through its own span of `window` and all
    /// sharing the driver's liveness `activity` and the state it reports.
    /// Dropped, the lanes leave the endpoint and the window as they found
    /// them, so a refused attach can build them again.
    ///
    /// # Errors
    ///
    /// [`Errno::LengthOutOfRange`] for a window of any length but the one
    /// `lanes` divide; [`Errno::MessageTooLarge`] for an endpoint whose frame
    /// bounds cannot carry the protocol; [`Errno::OutOfMemory`] if a lane
    /// cannot be allocated.
    pub fn lanes(
        endpoint: &Arc<CallEndpoint>,
        window: &Arc<KernelHold>,
        lanes: Lanes,
        offer: ServeOffer,
        activity: &Arc<ServerActivity>,
        audit: &'static (dyn Sink + Sync),
    ) -> Result<Vec<Self>, Errno> {
        if window.len() != lanes.window_len() {
            return Err(Errno::LengthOutOfRange);
        }
        if (endpoint.max_request() as usize) < FSSERVE_REQUEST_MAX
            || (endpoint.max_reply() as usize) < FSSERVE_REPLY_MAX
        {
            return Err(Errno::MessageTooLarge);
        }
        let state = Arc::try_new(ServedState::default()).map_err(|_| Errno::OutOfMemory)?;
        let mut served = Vec::new();
        served
            .try_reserve_exact(lanes.count())
            .map_err(|_| Errno::OutOfMemory)?;
        for index in 0..lanes.count() {
            let span = lanes.lane(index).ok_or(Errno::OutOfRange)?;
            served.push(Self::lane(
                Arc::clone(endpoint),
                Arc::clone(window),
                span,
                offer,
                Arc::clone(activity),
                Arc::clone(&state),
                audit,
            )?);
        }
        Ok(served)
    }

    /// One lane over `span` of `window`.
    fn lane(
        endpoint: Arc<CallEndpoint>,
        window: Arc<KernelHold>,
        span: Span,
        offer: ServeOffer,
        activity: Arc<ServerActivity>,
        state: Arc<ServedState>,
        audit: &'static (dyn Sink + Sync),
    ) -> Result<Self, Errno> {
        let mut frame = Vec::new();
        frame
            .try_reserve_exact(FSSERVE_REQUEST_MAX)
            .map_err(|_| Errno::OutOfMemory)?;
        frame.resize(FSSERVE_REQUEST_MAX, 0);
        let mut poster = CapabilitySet::empty();
        poster.insert(CapabilityId::IPC_ENDPOINT);
        let caps = TaskCapabilities::derive(
            ProcessId(NEXT_POSTER.fetch_add(1, Ordering::Relaxed)),
            UserId(0),
            poster,
            poster,
            audit,
        );
        Ok(Self {
            endpoint,
            window,
            span,
            caps,
            audit,
            offer,
            activity,
            state,
            quarantined: false,
            unsettled: None,
            class: AllocationClass::Ordinary,
            host: None,
            in_reserve: false,
            frame,
        })
    }

    /// What this volume reports of itself without its mount's lock.
    #[must_use]
    pub fn state(&self) -> Arc<ServedState> {
        Arc::clone(&self.state)
    }

    /// Ask `request` and hand its successful answer to `take`.
    fn ask<T>(
        &mut self,
        request: &Request<'_>,
        take: impl FnOnce(&Self, Answer<'_>) -> Result<T, DriverError>,
    ) -> Result<T, DriverError> {
        if request.span().is_some() && self.quarantined {
            self.release()?;
        }
        let bytes = self.call(request)?;
        self.answered(request, &bytes, take)
    }

    /// Decode `bytes`, the reply to `request`, and hand its successful answer
    /// to `take`.
    fn answered<T>(
        &mut self,
        request: &Request<'_>,
        bytes: &[u8],
        take: impl FnOnce(&Self, Answer<'_>) -> Result<T, DriverError>,
    ) -> Result<T, DriverError> {
        let Ok(reply) = decode_reply(bytes, request) else {
            self.misbehaved(AuditEvent::ServedReplyMalformed, request.op());
            return Err(DriverError::DeviceFault);
        };
        self.note(reply.state);
        let answer = reply.result?;
        self.state.degraded.store(false, Ordering::Relaxed);
        take(self, answer)
    }

    /// Prove the driver done with every call it was sent before this one
    /// through this lane's span.
    fn release(&mut self) -> Result<(), DriverError> {
        let request = Request::Release { span: self.span };
        let bytes = self.call(&request)?;
        let Ok(reply) = decode_reply(&bytes, &request) else {
            self.misbehaved(AuditEvent::ServedReplyMalformed, Op::Release);
            return Err(DriverError::DeviceFault);
        };
        self.note(reply.state);
        reply.result?;
        self.quarantined = false;
        Ok(())
    }

    /// Post `request` once the hold call abandoned before it is settled, and
    /// park until its reply.
    fn call(&mut self, request: &Request<'_>) -> Result<SensitiveBuffer, Failed> {
        self.settle().map_err(Failed::Unsent)?;
        self.post_and_wait(request)
    }

    /// Account for the hold call last abandoned: wait out its answer, then
    /// let go of a pin it granted.
    ///
    /// # Errors
    ///
    /// [`DriverError::Interrupted`] for a caller being killed meanwhile and
    /// [`DriverError::TimedOut`] for a driver still stopped, the call left
    /// unsettled; [`DriverError::DeviceOffline`] for a driver gone, and its
    /// holds with it.
    fn settle(&mut self) -> Result<(), DriverError> {
        loop {
            match self.unsettled.take() {
                None => return Ok(()),
                Some(Unsettled::Owed(pinned)) => {
                    let request = Request::Unpin { pinned };
                    return match self.post_and_wait(&request) {
                        // The hold goes whatever this answers.
                        Ok(bytes) => {
                            if let Ok(reply) = decode_reply(&bytes, &request) {
                                self.note(reply.state);
                            }
                            Ok(())
                        }
                        Err(Failed::Unsent(err)) => {
                            self.unsettled = Some(Unsettled::Owed(pinned));
                            Err(err)
                        }
                        Err(Failed::Sent(err)) => Err(err),
                    };
                }
                Some(Unsettled::Asked {
                    ticket,
                    call,
                    mut liveness,
                }) => {
                    let request = call.request();
                    let bytes = match self.wait(ticket, calling_task(), &mut liveness) {
                        Ok(bytes) => bytes,
                        Err(why) => {
                            let err = self.abandoned(why, request.op(), &mut liveness);
                            if !matches!(why, Unanswered::Gone) {
                                self.unsettled = Some(Unsettled::Asked {
                                    ticket,
                                    call,
                                    liveness,
                                });
                            }
                            return Err(err);
                        }
                    };
                    let Ok(reply) = decode_reply(&bytes, &request) else {
                        self.misbehaved(AuditEvent::ServedReplyMalformed, request.op());
                        return Ok(());
                    };
                    self.note(reply.state);
                    let (HoldCall::Pin(_), Ok(Answer::Node(pinned))) = (call, reply.result) else {
                        return Ok(());
                    };
                    self.unsettled = Some(Unsettled::Owed(pinned));
                }
            }
        }
    }

    /// Post `request`, wake the driver, and park until its reply. Abandoned,
    /// a call on the driver's holds is left unsettled; any other is withdrawn
    /// and the window quarantined.
    fn post_and_wait(&mut self, request: &Request<'_>) -> Result<SensitiveBuffer, Failed> {
        let len = encode_request(request, self.class, &mut self.frame)
            .map_err(|_| Failed::Unsent(DriverError::OutOfRange))?;
        let task = calling_task();
        let ticket = self.post(len, task).map_err(Failed::Unsent)?;
        let mut liveness = Liveness::from(self.activity.mark(), now_ns());
        let why = match self.wait(ticket, task, &mut liveness) {
            Ok(bytes) => return Ok(bytes),
            Err(why) => why,
        };
        let err = self.abandoned(why, request.op(), &mut liveness);
        if !matches!(why, Unanswered::Gone) {
            if let Some(call) = HoldCall::of(request) {
                self.unsettled = Some(Unsettled::Asked {
                    ticket,
                    call,
                    liveness,
                });
            } else {
                let _ = self.endpoint.cancel_one(self.caps.process().0, ticket);
                self.quarantined = true;
            }
        }
        Err(Failed::Sent(err))
    }

    /// Post the first `len` bytes of the frame for `task` and wake the driver.
    fn post(&self, len: usize, task: Option<u64>) -> Result<CallTicket, DriverError> {
        let ticket = self
            .endpoint
            .post(
                &self.caps,
                task.unwrap_or(0),
                &self.frame[..len],
                NO_DEADLINE,
                self.audit,
            )
            .map_err(|err| match err {
                Errno::NotFound => DriverError::DeviceOffline,
                _ => DriverError::DeviceFault,
            })?;
        match self.endpoint.server_task() {
            Some(server) => serve_wake_task(server),
            None => serve_wake(),
        }
        Ok(ticket)
    }

    /// What a call abandoned for `why` answers; a stall is recorded once each
    /// time the driver stops.
    fn abandoned(&self, why: Unanswered, op: Op, liveness: &mut Liveness) -> DriverError {
        match why {
            Unanswered::Gone => DriverError::DeviceOffline,
            Unanswered::Killed => DriverError::Interrupted,
            Unanswered::Stalled => {
                if !liveness.recorded {
                    liveness.recorded = true;
                    self.misbehaved(AuditEvent::ServedCallStalled, op);
                }
                DriverError::TimedOut
            }
        }
    }

    /// Park on `ticket` until its reply lands, the driver stalls by
    /// `liveness`, the caller is killed, or the endpoint goes.
    fn wait(
        &self,
        ticket: CallTicket,
        task: Option<u64>,
        liveness: &mut Liveness,
    ) -> Result<SensitiveBuffer, Unanswered> {
        let claimant = self.caps.process().0;
        let arm = |deadline: u64| {
            if let Some(task) = task {
                CALL_WAITQ.register(task, deadline);
                if let Some(hook) = wait_arch() {
                    hook.set_wakeup(nearest_timed_deadline());
                }
            }
        };
        arm(liveness.judged_at);
        let outcome = loop {
            match self
                .endpoint
                .take_reply(claimant, ticket, now_ns(), self.audit)
            {
                ReplyOutcome::Ready(bytes) => break Ok(bytes),
                ReplyOutcome::Cancelled | ReplyOutcome::Unknown | ReplyOutcome::TimedOut => {
                    break Err(Unanswered::Gone)
                }
                ReplyOutcome::Pending => {}
            }
            if task.is_some_and(crate::procsignal::kill_pending) {
                break Err(Unanswered::Killed);
            }
            if task.is_some() && now_ns() >= liveness.judged_at {
                let seen = self.activity.mark();
                if !shows_life(liveness.mark, seen) {
                    break Err(Unanswered::Stalled);
                }
                *liveness = Liveness::from(seen, now_ns());
                arm(liveness.judged_at);
            }
            // With no scheduler to park on (a host test, where the driver
            // runs on another thread) the wait degrades to a cooperative
            // spin, exactly as `SleepLock` does; never a steady state on a
            // running kernel.
            if task.is_none() || !park_calling_task() {
                core::hint::spin_loop();
            }
        };
        if let Some(task) = task {
            CALL_WAITQ.deregister(task);
            if let Some(hook) = wait_arch() {
                hook.set_wakeup(nearest_timed_deadline());
            }
        }
        outcome
    }

    /// Take the driver's reported state.
    fn note(&mut self, state: ServerState) {
        self.state
            .unpublished
            .store(state.unpublished, Ordering::Relaxed);
        if state.in_reserve != self.in_reserve {
            self.in_reserve = state.in_reserve;
            if let Some((volume, host)) = self.host {
                host.reserve_crossed(volume, state.in_reserve);
            }
        }
    }

    /// Record the driver misbehaving on `op`, and read the volume as
    /// degraded until it answers cleanly.
    fn misbehaved(&self, event: AuditEvent, op: Op) {
        self.state.degraded.store(true, Ordering::Relaxed);
        emit(
            self.audit,
            Level::Warn,
            event,
            &[
                Field {
                    key: "endpoint",
                    value: FieldValue::UnsignedInt(self.endpoint.id().0),
                },
                Field {
                    key: "op",
                    value: FieldValue::UnsignedInt(u64::from(op as u8)),
                },
            ],
        );
    }

    /// Copy `data` into this lane's span.
    fn stage(&self, data: &[u8]) {
        debug_assert!(data.len() <= self.span.size());
        // SAFETY: `lanes` held the window to what its lanes divide, so the
        // span lies inside it, and every caller bounds `data` by the span.
        // The copy goes through raw pointers, so no reference is formed over
        // memory the driver shares; the source is a live kernel slice.
        unsafe {
            core::ptr::copy_nonoverlapping(
                data.as_ptr(),
                self.window.as_ptr().add(self.span.offset()),
                data.len(),
            );
        }
    }

    /// Copy the first `out.len()` bytes of this lane's span out.
    fn unstage(&self, out: &mut [u8]) {
        debug_assert!(out.len() <= self.span.size());
        // SAFETY: as `stage`, inside the span, raw-pointer copy only, into a
        // live kernel slice. The bytes are file data the driver vouches for;
        // a torn write by a hostile driver yields wrong data, never
        // unsafety.
        unsafe {
            core::ptr::copy_nonoverlapping(
                self.window.as_ptr().add(self.span.offset()),
                out.as_mut_ptr(),
                out.len(),
            );
        }
    }
}

/// The monotonic clock, or zero with no scheduler to read it from.
fn now_ns() -> u64 {
    wait_arch().map_or(0, WaitQueueArch::now_ns)
}

/// The task making the call, or `None` with no scheduler to park it on.
fn calling_task() -> Option<u64> {
    wait_arch().and_then(|hook| hook.current_cpu().and_then(|cpu| hook.current_task(cpu)))
}

/// Whether a driver whose activity read `before` shows life reading `now`:
/// it is inside a transfer, or finished one in between.
fn shows_life(before: Option<u64>, now: Option<u64>) -> bool {
    now.is_none() || now != before
}

/// The node an answer names.
fn node_of(answer: Answer<'_>) -> Result<NodeId, DriverError> {
    match answer {
        Answer::Node(node) => Ok(node),
        _ => Err(DriverError::DeviceFault),
    }
}

/// The count an answer gives.
fn count_of(answer: Answer<'_>) -> Result<usize, DriverError> {
    match answer {
        Answer::Count(count) => Ok(count as usize),
        _ => Err(DriverError::DeviceFault),
    }
}

/// An answer that carries nothing.
fn done(answer: Answer<'_>) -> Result<(), DriverError> {
    match answer {
        Answer::Done => Ok(()),
        _ => Err(DriverError::DeviceFault),
    }
}

/// `out`'s length clamped to `max`, as the request carries it.
fn cap(out: &[u8], max: usize) -> u32 {
    u32::try_from(out.len().min(max)).unwrap_or(u32::MAX)
}

/// Copy an attribute answer into `out`.
fn attr_into(answer: Answer<'_>, out: &mut [u8]) -> Result<Option<usize>, DriverError> {
    match answer {
        Answer::Attr(None) => Ok(None),
        Answer::Attr(Some(bytes)) => {
            out.get_mut(..bytes.len())
                .ok_or(DriverError::DeviceFault)?
                .copy_from_slice(bytes);
            Ok(Some(bytes.len()))
        }
        _ => Err(DriverError::DeviceFault),
    }
}

impl FilesystemRead for ServedFs {
    fn root(&self) -> NodeId {
        self.offer.root
    }

    fn name_matching(&self) -> NameMatching {
        self.offer.name_matching
    }

    fn node_identity(&self) -> NodeIdentity {
        self.offer.node_identity
    }

    fn node_info(&mut self, node: NodeId) -> Result<NodeInfo, DriverError> {
        self.ask(&Request::NodeInfo { node }, |_, answer| match answer {
            Answer::Info(info) => Ok(info),
            _ => Err(DriverError::DeviceFault),
        })
    }

    fn lookup(&mut self, dir: NodeId, name: &[u8]) -> Result<NodeId, DriverError> {
        self.ask(&Request::Lookup { dir, name }, |_, answer| node_of(answer))
    }

    fn parent(&mut self, dir: NodeId) -> Result<NodeId, DriverError> {
        self.ask(&Request::Parent { dir }, |_, answer| node_of(answer))
    }

    fn read_at(&mut self, file: NodeId, offset: u64, buf: &mut [u8]) -> Result<usize, DriverError> {
        let mut done = 0;
        while done < buf.len() {
            let want = (buf.len() - done).min(self.span.size());
            let span = Span::new(self.span.offset(), want).map_err(|_| DriverError::OutOfRange)?;
            let offset = offset
                .checked_add(done as u64)
                .ok_or(DriverError::OutOfRange)?;
            let out = &mut buf[done..done + want];
            let got = self.ask(&Request::ReadAt { file, offset, span }, |fs, answer| {
                let got = count_of(answer)?;
                fs.unstage(&mut out[..got]);
                Ok(got)
            })?;
            done += got;
            if got < want {
                break;
            }
        }
        Ok(done)
    }

    fn read_link(&mut self, link: NodeId, out: &mut [u8]) -> Result<usize, DriverError> {
        let request = Request::ReadLink {
            link,
            cap: cap(out, FS_SYMLINK_MAX),
        };
        self.ask(&request, |_, answer| match answer {
            Answer::Target(target) => {
                out.get_mut(..target.len())
                    .ok_or(DriverError::DeviceFault)?
                    .copy_from_slice(target);
                Ok(target.len())
            }
            _ => Err(DriverError::DeviceFault),
        })
    }

    fn seek_region(
        &mut self,
        file: NodeId,
        offset: u64,
        region: SeekRegion,
    ) -> Result<Option<u64>, DriverError> {
        let request = Request::SeekRegion {
            file,
            offset,
            region,
        };
        self.ask(&request, |_, answer| match answer {
            Answer::Region(at) => Ok(at),
            _ => Err(DriverError::DeviceFault),
        })
    }

    fn read_dir(
        &mut self,
        dir: NodeId,
        cursor: u64,
        after: &[u8],
        visit: &mut dyn FnMut(&DirEntry, &[u8]) -> DirVisit,
    ) -> Result<(), DriverError> {
        let mut at = cursor;
        let mut resume = Vec::new();
        resume
            .try_reserve_exact(after.len())
            .map_err(|_| DriverError::OutOfMemory)?;
        resume.extend_from_slice(after);
        let mut handed = 0usize;
        loop {
            let request = Request::ReadDir {
                dir,
                cursor: at,
                after: &resume,
            };
            // `None` once the visitor stops or the directory ends; the next
            // batch's position otherwise.
            let next = self.ask(&request, |_, answer| {
                let Answer::Entries(batch) = answer else {
                    return Err(DriverError::DeviceFault);
                };
                let mut last = None;
                for (entry, name) in &batch {
                    handed += 1;
                    if handed > LISTING_MAX {
                        return Err(DriverError::DeviceFault);
                    }
                    if visit(&entry, name) == DirVisit::Stop {
                        return Ok(None);
                    }
                    last = Some((entry.next_cursor, name));
                }
                match batch.end() {
                    DirEnd::Ended => Ok(None),
                    DirEnd::Failed(err) => Err(err),
                    DirEnd::More => {
                        let (next, name) = last.ok_or(DriverError::DeviceFault)?;
                        let mut owned = Vec::new();
                        owned
                            .try_reserve_exact(name.len())
                            .map_err(|_| DriverError::OutOfMemory)?;
                        owned.extend_from_slice(name);
                        Ok(Some((next, owned)))
                    }
                }
            })?;
            let Some((next, name)) = next else {
                return Ok(());
            };
            // A batch that resumes where it began would be asked for again
            // forever.
            if next == at && name == resume {
                return Err(DriverError::DeviceFault);
            }
            at = next;
            resume = name;
        }
    }

    fn pin(&mut self, node: NodeId) -> Result<NodeId, DriverError> {
        self.ask(&Request::Pin { node }, |_, answer| node_of(answer))
    }

    fn unpin(&mut self, pinned: NodeId) -> Result<Unpinned, DriverError> {
        let request = Request::Unpin { pinned };
        let bytes = self.call(&request).map_err(|failed| match failed {
            // Still the kernel's to send: its registration keeps it.
            Failed::Unsent(_) => DriverError::Busy,
            Failed::Sent(err) => err,
        })?;
        self.answered(&request, &bytes, |_, answer| match answer {
            Answer::Unpinned(left) => Ok(left),
            _ => Err(DriverError::DeviceFault),
        })
    }

    fn release_pins(&mut self) -> Result<(), DriverError> {
        self.ask(&Request::ReleasePins, |_, answer| done(answer))
    }
}

impl FilesystemWrite for ServedFs {
    fn create(
        &mut self,
        dir: NodeId,
        name: &[u8],
        kind: NodeKind,
        record: &NodeSecurity,
    ) -> Result<NodeId, DriverError> {
        if kind == NodeKind::Symlink {
            return Err(DriverError::Unsupported);
        }
        let request = Request::Create {
            dir,
            name,
            kind,
            record: *record,
        };
        self.ask(&request, |_, answer| node_of(answer))
    }

    fn create_link(
        &mut self,
        dir: NodeId,
        name: &[u8],
        target: &[u8],
        record: &NodeSecurity,
    ) -> Result<NodeId, DriverError> {
        let request = Request::CreateLink {
            dir,
            name,
            target,
            record: *record,
        };
        self.ask(&request, |_, answer| node_of(answer))
    }

    fn link(&mut self, dir: NodeId, name: &[u8], node: NodeId) -> Result<(), DriverError> {
        self.ask(&Request::Link { dir, name, node }, |_, answer| done(answer))
    }

    fn write_at(&mut self, file: NodeId, offset: u64, data: &[u8]) -> Result<usize, DriverError> {
        let mut written = 0;
        while written < data.len() {
            let chunk = &data[written..data.len().min(written + self.span.size())];
            let span =
                Span::new(self.span.offset(), chunk.len()).map_err(|_| DriverError::OutOfRange)?;
            let offset = offset
                .checked_add(written as u64)
                .ok_or(DriverError::OutOfRange)?;
            if self.quarantined {
                self.release()?;
            }
            self.stage(chunk);
            let request = Request::WriteAt { file, offset, span };
            let stored = self.ask(&request, |_, answer| count_of(answer))?;
            written += stored;
            // A short count is the driver's back-pressure.
            if stored < chunk.len() {
                break;
            }
        }
        Ok(written)
    }

    fn truncate(&mut self, file: NodeId, size: u64) -> Result<(), DriverError> {
        self.ask(&Request::Truncate { file, size }, |_, answer| done(answer))
    }

    fn punch(&mut self, file: NodeId, offset: u64, len: u64) -> Result<(), DriverError> {
        let request = Request::Punch { file, offset, len };
        self.ask(&request, |_, answer| done(answer))
    }

    fn remove(&mut self, dir: NodeId, name: &[u8]) -> Result<(), DriverError> {
        self.ask(&Request::Remove { dir, name }, |_, answer| done(answer))
    }

    fn rename(
        &mut self,
        src_dir: NodeId,
        src_name: &[u8],
        dst_dir: NodeId,
        dst_name: &[u8],
    ) -> Result<(), DriverError> {
        let request = Request::Rename {
            src_dir,
            src_name,
            dst_dir,
            dst_name,
        };
        self.ask(&request, |_, answer| done(answer))
    }

    fn flush(&mut self) -> Result<(), DriverError> {
        self.ask(&Request::Flush, |_, answer| done(answer))
    }

    fn set_volume_host(&mut self, volume: DriverHandle, host: &'static dyn VolumeHost) {
        // Kept for the reserve alone: the driver's commit window runs in its
        // own process, and its registration is reaped inline, so the kernel's
        // flusher never waits on it.
        self.host = Some((volume, host));
    }

    fn set_allocation_class(&mut self, class: AllocationClass) {
        self.class = class;
    }
}

impl FilesystemSecurity for ServedFs {
    fn security(&mut self, node: NodeId) -> Result<NodeSecurity, DriverError> {
        self.ask(&Request::Security { node }, |_, answer| match answer {
            Answer::Security(security) => Ok(security),
            _ => Err(DriverError::DeviceFault),
        })
    }

    fn set_security(&mut self, node: NodeId, security: NodeSecurity) -> Result<(), DriverError> {
        self.ask(&Request::SetSecurity { node, security }, |_, answer| {
            done(answer)
        })
    }

    fn records_ownership(&mut self, node: NodeId) -> Result<bool, DriverError> {
        self.ask(
            &Request::RecordsOwnership { node },
            |_, answer| match answer {
                Answer::Owned(owned) => Ok(owned),
                _ => Err(DriverError::DeviceFault),
            },
        )
    }
}

impl FilesystemStats for ServedFs {
    fn stats(&mut self) -> Result<VolumeStats, DriverError> {
        self.ask(&Request::Stats, |_, answer| match answer {
            Answer::Stats(stats) => Ok(stats),
            _ => Err(DriverError::DeviceFault),
        })
    }
}

impl FilesystemAttrs for ServedFs {
    fn get_attr(
        &mut self,
        node: NodeId,
        key: &[u8],
        value_out: &mut [u8],
    ) -> Result<Option<usize>, DriverError> {
        let request = Request::GetAttr {
            node,
            key,
            cap: cap(value_out, FS_ATTR_VALUE_MAX),
        };
        self.ask(&request, |_, answer| attr_into(answer, value_out))
    }

    fn set_attr(&mut self, node: NodeId, key: &[u8], value: &[u8]) -> Result<(), DriverError> {
        self.ask(&Request::SetAttr { node, key, value }, |_, answer| {
            done(answer)
        })
    }

    fn list_attr(
        &mut self,
        node: NodeId,
        index: u64,
        key_out: &mut [u8],
    ) -> Result<Option<usize>, DriverError> {
        let request = Request::ListAttr {
            node,
            index,
            cap: cap(key_out, FS_ATTR_KEY_MAX),
        };
        self.ask(&request, |_, answer| attr_into(answer, key_out))
    }

    fn remove_attr(&mut self, node: NodeId, key: &[u8]) -> Result<(), DriverError> {
        self.ask(&Request::RemoveAttr { node, key }, |_, answer| done(answer))
    }
}

impl FilesystemAttrsProvider for ServedFs {
    fn attrs_fs(&mut self) -> Option<&mut dyn FilesystemAttrsFs> {
        if self.offer.attrs {
            Some(self)
        } else {
            None
        }
    }
}

#[cfg(test)]
#[path = "served_tests.rs"]
mod tests;
