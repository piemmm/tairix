//! The kernel call-endpoint registry: a map from [`EndpointId`] to the
//! live [`CallEndpoint`] bound to it (Design D D2b — `plans/PI.md`).
//!
//! A [`CallEndpoint`] (`kernel/ipc`) is the synchronous request/reply
//! state machine the reactive driver-store file service is built on. The
//! server half lives on a never-returning kernel kthread (the disk-owning
//! driver-store service) and the caller half is the `ipc_call` syscall
//! handler (`crate::syscalls`); both must reach the *same* endpoint
//! instance. This registry is that rendezvous: the server
//! [`register`]s the endpoint it created under its well-known id,
//! and the handler [`lookup`]s the id the caller named. A server receives
//! through [`receive_call`], the one blocking receive.
//!
//! # Why a registry and not a borrowed seam
//!
//! Like [`crate::waitq::CALL_WAITQ`], the registry is global pure data
//! behind a [`SpinLock`] (never a `static mut`, so it is not the global
//! mutable static the charter forbids): the kthread server and the
//! syscall handler live in different crates and neither owns the other, so
//! a global rendezvous keyed by the well-known
//! [`tairix_abi::driver_store::DRIVER_STORE_ENDPOINT`] avoids threading an
//! [`Arc`] through `KernelState`'s cross-crate wiring. Each endpoint is held
//! behind an [`Arc`] so the server and every in-flight caller share one
//! instance for its life.
//!
//! # What an exit visits
//!
//! The registry indexes the endpoints each process owns, and every endpoint
//! bound notes its callers in the registry's [`SenderIndex`], so an exit tears
//! down and scrubs the endpoints its process owned and called, and no other.
//! The locks order registry, endpoint, index, heap: an endpoint notes a caller
//! under its own lock, nothing but the heap is taken under the index's, and
//! no endpoint is destroyed or scrubbed under the registry's. An owner's
//! teardown takes the address-space registry's write lock before this
//! registry's, so the grants it revokes name exactly the endpoints the owner
//! holds.
//!
//! # Fail closed
//!
//! [`register`] refuses to overwrite a live binding ([`Errno::AlreadyExists`])
//! and a binding it cannot keep ([`Errno::OutOfMemory`]), auditing either as
//! [`AuditEvent::CallEndpointRegisterDenied`], mirroring the port registry's
//! register-denied event, so the kernel never silently re-points a live
//! endpoint; [`lookup`] of an unbound id yields `None`, which a posting
//! handler maps to [`Errno::NotFound`] and audits at that boundary (mirroring
//! [`tairix_kernel_ipc::registry::PortRegistry`]), and a reaping one to
//! [`Errno::BrokenPipe`]: nothing will answer.

use alloc::sync::Arc;

use tairix_abi::hwlink::LinkRole;
use tairix_abi::Errno;
use tairix_collections::HashMap;
use tairix_hash::BuildSipHash13;
use tairix_kernel_ipc::audit::record;
use tairix_kernel_ipc::senders::{EndpointSets, Endpoints, SenderIndex};
use tairix_kernel_ipc::{AuditEvent, CallEndpoint, Destroyed, EndpointId, ReceivedCall, RecvCall};
use tairix_kernel_sec::ProcessId;
use tairix_log::{Field, Sink};
use tairix_sync::{RwLock, SpinLock};
use tairix_util::fmt::{format_hex_u64, format_u64};

use crate::aspace::AddressSpaceRegistry;

/// The global call-endpoint registry (set-up by the boot path's kthread
/// server, read by the `ipc_call` syscall handler), built by the first
/// registration. Pure data behind a [`SpinLock`]; see the module docs for why
/// it is global.
static CALL_ENDPOINTS: SpinLock<Option<CallRegistry>> = SpinLock::new(None);

/// The endpoints bound, those each process owns, and the index of those each
/// caller holds a lane on, which every endpoint bound shares.
struct CallRegistry {
    /// Keyed: endpoint ids are numbers their binders choose.
    endpoints: HashMap<EndpointId, Arc<CallEndpoint>, BuildSipHash13>,
    owners: EndpointSets<u64>,
    senders: Arc<SenderIndex<u64>>,
}

/// An unbound endpoint, and the set of its owner's endpoints its unbinding
/// emptied, which the caller drops once it has released the registry.
type Unbound = (Arc<CallEndpoint>, Option<Endpoints>);

impl CallRegistry {
    /// An empty registry, or [`None`] when its index cannot be made. A boot
    /// that never got the per-boot key hashes unkeyed, as the futex table does.
    fn new() -> Option<Self> {
        let keyed = BuildSipHash13::keyed().unwrap_or(BuildSipHash13::UNKEYED);
        Some(Self {
            endpoints: HashMap::with_hasher(keyed),
            owners: EndpointSets::new(),
            senders: Arc::try_new(SenderIndex::new()).ok()?,
        })
    }

    /// Bind `endpoint`, handing it the index first so its first caller is
    /// noted, or hand it back with why it was refused: its id is bound, or the
    /// binding cannot be kept.
    fn bind(&mut self, endpoint: Arc<CallEndpoint>) -> Result<(), (Arc<CallEndpoint>, Errno)> {
        let id = endpoint.id();
        if self.endpoints.contains_key(&id) {
            return Err((endpoint, Errno::AlreadyExists));
        }
        if self.endpoints.try_reserve(1).is_err() {
            return Err((endpoint, Errno::OutOfMemory));
        }
        if let Err(err) = self.owners.insert(endpoint.owner(), id) {
            return Err((endpoint, err));
        }
        endpoint.note_senders_in(Arc::clone(&self.senders));
        // Reserved above, so this cannot fail.
        let _ = self.endpoints.try_insert(id, endpoint);
        Ok(())
    }

    /// Unbind `id`, forgetting it as its owner's.
    fn unbind(&mut self, id: EndpointId) -> Option<Unbound> {
        let endpoint = self.endpoints.remove(&id)?;
        let emptied = self.owners.remove(endpoint.owner(), id);
        Some((endpoint, emptied))
    }

    /// Unbind `id` if `owner` still holds it: an id another party unbound and
    /// rebound since is no longer `owner`'s to tear down.
    fn unbind_owned(&mut self, id: EndpointId, owner: u64) -> Option<Unbound> {
        if self.endpoints.get(&id)?.owner() != owner {
            return None;
        }
        self.unbind(id)
    }
}

/// Serialise a test that binds into or reads the process-global call-endpoint
/// registry, so no two tests hold the same endpoint id at once.
///
/// It does **not** keep a test's in-flight calls safe. Endpoints are torn
/// down by owner and calls cancelled by poster, and both reach this registry
/// from the `exit` path — which sibling tests drive holding no guard, so
/// serialising the registry's residents cannot cover it. What does is a task
/// id no other test can name: draw owner and poster ids from
/// [`crate::test_boot::claim_task`] and
/// [`crate::test_boot::claim_peer_task`].
#[cfg(test)]
pub(crate) fn registry_guard() -> std::sync::MutexGuard<'static, ()> {
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Bind `endpoint` into the registry under its own [`CallEndpoint::id`],
/// noting it as its owner's.
///
/// # Errors
///
/// [`Errno::AlreadyExists`] if the id is already bound; the existing
/// binding is left untouched and the kernel never silently overwrites a
/// live endpoint. [`Errno::OutOfMemory`] if the registry cannot keep the
/// binding; nothing is bound. A refusal is the security decision this
/// rendezvous makes, so it is recorded on `audit` as
/// [`AuditEvent::CallEndpointRegisterDenied`] (a bare "endpoint created"
/// with no subsequent denial would misread as a live endpoint); the
/// emission happens after the registry lock is released.
pub fn register(endpoint: Arc<CallEndpoint>, audit: &dyn Sink) -> Result<(), Errno> {
    let id = endpoint.id();
    let mut guard = CALL_ENDPOINTS.lock();
    if guard.is_none() {
        *guard = CallRegistry::new();
    }
    let bound = match guard.as_mut() {
        Some(registry) => registry.bind(endpoint),
        None => Err((endpoint, Errno::OutOfMemory)),
    };
    drop(guard);
    if let Err((_, err)) = bound {
        let mut id_buf = [0u8; 16];
        let id_field = Field {
            key: "endpoint",
            value: tairix_log::FieldValue::Str(format_hex_u64(id.0, &mut id_buf)),
        };
        record(audit, AuditEvent::CallEndpointRegisterDenied, &[id_field]);
        return Err(err);
    }
    report_link_service(id);
    Ok(())
}

/// Resolve `id` to the live [`CallEndpoint`] bound to it, if any.
///
/// Returns an [`Arc`] clone so the caller may post/take outside the
/// registry lock (the endpoint owns its own interior lock). A miss is not
/// a security decision and is not audited here.
#[must_use]
pub fn lookup(id: EndpointId) -> Option<Arc<CallEndpoint>> {
    #[cfg(test)]
    LOOKUPS.with(|made| made.set(made.get() + 1));
    CALL_ENDPOINTS.lock().as_ref()?.endpoints.get(&id).cloned()
}

#[cfg(test)]
std::thread_local! {
    /// Lookups made on this thread, for the tests that bound them.
    static LOOKUPS: core::cell::Cell<usize> = const { core::cell::Cell::new(0) };
}

/// The lookups made on this thread so far.
#[cfg(test)]
pub(crate) fn lookups_made() -> usize {
    LOOKUPS.with(core::cell::Cell::get)
}

/// Unbind and destroy `id` under a server that lives on, waking every thread
/// its destruction concerns: the callers whose calls it cancelled or who were
/// parked for room, those waiting for the budget those calls held, and its
/// server, whose next receive answers that it went. Answers whether `id` was
/// bound.
pub fn take_down(id: EndpointId, audit: &dyn Sink) -> bool {
    let Some(endpoint) = unregister(id) else {
        return false;
    };
    let destroyed = endpoint.destroy(audit);
    crate::waitq::call_wake_named(destroyed.tasks());
    crate::waitq::serve_wake(id.0);
    release_callers(destroyed);
    true
}

/// Drop what a destruction cancelled, waking those waiting for each caller's
/// budget once what its calls held is back.
fn release_callers(destroyed: Destroyed) {
    destroyed.release_each(|caller| crate::waitq::budget_released_all(ProcessId(caller)));
}

/// What one look at an endpoint's queue for a call found.
pub enum Receive<T> {
    /// A call, taken into service as `T`.
    Taken(T),
    /// The next call is larger than the server's buffer, left queued.
    TooLarge,
    /// No call is queued.
    Empty,
    /// The look waited for a page of the server's buffer, and looks again.
    Again,
}

impl From<RecvCall> for Receive<ReceivedCall> {
    fn from(received: RecvCall) -> Self {
        match received {
            RecvCall::Received(call) => Self::Taken(call),
            RecvCall::TooLarge { .. } => Self::TooLarge,
            RecvCall::Empty => Self::Empty,
        }
    }
}

/// Receive, as server task `server`, the next call posted to `endpoint`,
/// each look at its queue made by `receive`. A non-`blocking` receive answers
/// an empty queue at once; a blocking one parks through `park` on
/// [`SERVE_WAITQ`](crate::waitq::SERVE_WAITQ) under the endpoint's key,
/// registered before its first look so a post in between still wakes it,
/// until a call is posted.
///
/// # Errors
///
/// What `receive` answers; [`Errno::NotFound`] once the endpoint is closed;
/// [`Errno::BufferTooSmall`] for a call larger than the server's buffer, left
/// queued; [`Errno::WouldBlock`] for an empty queue a non-blocking receive
/// meets; [`Errno::OutOfMemory`] when a blocking one has no room to register
/// its wait; [`Errno::NotImplemented`] when `park` cannot park the caller;
/// [`Errno::Interrupted`] once `abandoned` reports the server ending.
pub fn receive_call<T>(
    endpoint: &CallEndpoint,
    server: u64,
    blocking: bool,
    park: &mut dyn FnMut() -> bool,
    abandoned: &dyn Fn() -> bool,
    mut receive: impl FnMut() -> Result<Receive<T>, Errno>,
) -> Result<T, Errno> {
    let key = crate::waitq::endpoint_key(endpoint.id().0);
    if blocking {
        crate::waitq::SERVE_WAITQ.register_keyed(key, server, crate::waitq::NO_DEADLINE)?;
    }
    let received = loop {
        if endpoint.is_closed() {
            break Err(Errno::NotFound);
        }
        match receive() {
            Err(refused) => break Err(refused),
            Ok(Receive::Taken(call)) => break Ok(call),
            Ok(Receive::TooLarge) => break Err(Errno::BufferTooSmall),
            Ok(Receive::Again) => {}
            Ok(Receive::Empty) if !blocking => break Err(Errno::WouldBlock),
            Ok(Receive::Empty) => {
                if !park() {
                    break Err(Errno::NotImplemented);
                }
                if abandoned() {
                    break Err(Errno::Interrupted);
                }
            }
        }
    };
    if blocking {
        crate::waitq::SERVE_WAITQ.deregister_keyed(key, server);
    }
    received
}

/// Remove the binding for `id`, returning the endpoint that was bound (if
/// any) so the caller can [`CallEndpoint::destroy`] it. Idempotent.
pub fn unregister(id: EndpointId) -> Option<Arc<CallEndpoint>> {
    let (removed, emptied) = CALL_ENDPOINTS.lock().as_mut()?.unbind(id)?;
    // What the unbinding emptied goes once the registry is released.
    drop(emptied);
    report_link_service(id);
    Some(removed)
}

/// An observer of the link suppliers' endpoints: told whether one is live
/// each time it is bound or unbound, so the hardware tree can state which
/// suppliers are serving.
pub trait LinkServiceObserver: Sync {
    /// The link endpoint `id` is now live, or is not.
    fn link_service_changed(&self, id: EndpointId, live: bool);
}

/// The set-once link-service observer, installed by the boot path.
static LINK_OBSERVER: tairix_sync::OnceCell<&'static dyn LinkServiceObserver> =
    tairix_sync::OnceCell::new();

/// Orders each report's reading of the registry with its delivery, so the
/// last report delivered states the registry as it stands.
static LINK_REPORT: SpinLock<()> = SpinLock::new(());

/// Install the link-service observer. First-wins and idempotent, like the
/// vanish observer.
pub fn install_link_observer(observer: &'static dyn LinkServiceObserver) {
    let _ = LINK_OBSERVER.set(observer);
}

/// Tell the observer whether `id` is live, when it is a link endpoint.
///
/// The registry is re-read rather than the change being passed along: a
/// teardown and a rebind of one id can race, and a report of what the caller
/// did could land after the other's and leave the tree stating the past.
fn report_link_service(id: EndpointId) {
    if LinkRole::of_endpoint(id.0).is_none() {
        return;
    }
    if let Ok(Some(observer)) = LINK_OBSERVER.get() {
        let _ordered = LINK_REPORT.lock();
        observer.link_service_changed(id, contains(id));
    }
}

/// An observer of a device's endpoints going: told, after the fact, that an
/// owner's endpoints were destroyed, or that the node declaring one was
/// retracted while its driver lives on. The runtime volume service listens
/// here so a surprise-removed disk's volume reacts the moment its serving
/// driver dies, or its medium leaves (`plans/DEVICES.md` D4).
pub trait EndpointVanishObserver: Sync {
    /// `id` was unbound and destroyed because its owning task ended. Runs
    /// in the tearing-down task's context after parked callers were woken:
    /// the observer may take sleeping locks, but every in-flight call on
    /// the endpoint has already been cancelled, so nothing it waits on can
    /// depend on the dead endpoint.
    fn endpoint_vanished(&self, id: EndpointId);

    /// The hardware node declaring `id` was surprise-removed by the driver
    /// serving it, which lives on: the medium the node published left its
    /// unit. The endpoint stays bound for the next medium's publication, and
    /// refuses every request made for this one as stale. Runs in the
    /// removing driver's syscall, after the node's grants were revoked.
    fn publication_retracted(&self, id: EndpointId);
}

/// Tell the vanish observer the node declaring each of `endpoints` was
/// surprise-removed while its driver lives on.
pub fn notify_publication_retracted(endpoints: &[u64]) {
    if let Ok(Some(observer)) = VANISH_OBSERVER.get() {
        for &id in endpoints {
            observer.publication_retracted(EndpointId(id));
        }
    }
}

/// The set-once vanish observer, installed by the boot path. Fail-closed
/// `None` before install: teardown simply has no listener.
static VANISH_OBSERVER: tairix_sync::OnceCell<&'static dyn EndpointVanishObserver> =
    tairix_sync::OnceCell::new();

/// Install the endpoint-vanish observer. First-wins and idempotent, like
/// the other late-installed boot seams.
pub fn install_vanish_observer(observer: &'static dyn EndpointVanishObserver) {
    let _ = VANISH_OBSERVER.set(observer);
}

/// Tear down every endpoint owned by the exiting task `owner`, wake the
/// callers its destruction cancelled, revoke the per-endpoint grants those
/// endpoints' ids named, and notify the vanish observer, answering how many
/// it tore down. Only the endpoints `owner` owns are visited.
///
/// A user-space service may exit (cleanly, by fault, or killed) while
/// callers are blocked in `ipc_call` awaiting its replies. Without this,
/// those callers would park forever on a dead endpoint; destroying the
/// endpoint flips every outstanding call to
/// [`tairix_kernel_ipc::ReplyOutcome::Cancelled`] and wakes exactly the
/// threads it was holding, so each abandons fail-closed and no other parked
/// caller stirs. The observer is notified strictly **after** every wake: a
/// caller parked mid-call on a dead endpoint may hold a lock the observer
/// needs, and the wake is what lets that caller finish and release it. The
/// per-endpoint grants naming the doomed ids are revoked as the ids are taken
/// from `owner`, under both registries' locks and before any is unbound: no
/// woken caller can re-post to an id another task may bind next, and an id
/// rebound by another owner meanwhile keeps that owner's grants.
pub fn teardown_owned_by(
    owner: u64,
    aspaces: &RwLock<AddressSpaceRegistry>,
    audit: &dyn Sink,
) -> usize {
    // Most processes own no endpoint, and never take the address-space
    // registry's write lock here.
    let owns = CALL_ENDPOINTS
        .lock()
        .as_ref()
        .is_some_and(|registry| registry.owners.holds(owner));
    if !owns {
        return 0;
    }
    let taken = {
        let mut grants = aspaces.write();
        let mut guard = CALL_ENDPOINTS.lock();
        let endpoints = guard
            .as_mut()
            .and_then(|registry| registry.owners.take(owner));
        endpoints.map(|endpoints| {
            let revoked = grants.revoke_endpoint_grants(&endpoints);
            (endpoints, revoked)
        })
    };
    let Some((mut endpoints, revoked)) = taken else {
        return 0;
    };
    record_grants_revoked(owner, revoked, audit);
    endpoints.retain(|&id| {
        let unbound = CALL_ENDPOINTS
            .lock()
            .as_mut()
            .and_then(|registry| registry.unbind_owned(id, owner));
        let Some((endpoint, emptied)) = unbound else {
            return false;
        };
        // What the unbinding emptied goes once the registry is released.
        drop(emptied);
        report_link_service(id);
        let destroyed = endpoint.destroy(audit);
        crate::waitq::call_wake_named(destroyed.tasks());
        release_callers(destroyed);
        true
    });
    if let Ok(Some(observer)) = VANISH_OBSERVER.get() {
        for &id in &endpoints {
            observer.endpoint_vanished(id);
        }
    }
    endpoints.len()
}

/// Record that a teardown of `owner`'s endpoints withdrew `revoked` grants
/// naming them.
///
/// An endpoint id is a number, and a number is re-creatable: once these
/// endpoints are gone another task may bind the same ids. A holder's grant
/// that outlived its endpoint would then silently retarget onto the new
/// instance, handing it the authority to call a service it was never
/// granted, which is why a teardown revokes before it unbinds.
fn record_grants_revoked(owner: u64, revoked: usize, audit: &dyn Sink) {
    if revoked == 0 {
        return;
    }
    let mut owner_buf = [0u8; 16];
    let mut count_buf = [0u8; 20];
    record(
        audit,
        AuditEvent::CallEndpointGrantsRevoked,
        &[
            // The task whose teardown withdrew the authority; the ids it lost
            // are the `CallEndpointDestroyed` records that follow this one.
            Field {
                key: "owner",
                value: tairix_log::FieldValue::Str(format_hex_u64(owner, &mut owner_buf)),
            },
            Field {
                key: "grants",
                value: tairix_log::FieldValue::Str(format_u64(revoked as u64, &mut count_buf)),
            },
        ],
    );
}

/// Cancel every in-flight call the exiting task `sender` posted — the
/// converse of [`teardown_owned_by`]: that releases the endpoints a dead task
/// *served*; this scrubs the calls a dead task *sent* — answering how many
/// endpoints it visited: those `sender` called, which the index every
/// endpoint notes its callers in names, however many are bound.
///
/// Without it a dead caller's queued request outlives it and is later
/// handed to the server as if live — the USB defect where an unloaded
/// class driver's final URB submit survived its death and wedged the
/// single-slot URB transport against the replacement driver. Each endpoint
/// with cancellations records one `CallPosterVanished` audit event. No wake
/// is needed: the poster is dead and the server parks only on *arrival* of
/// requests, never their removal.
pub fn cancel_posted_by(sender: u64, audit: &dyn Sink) -> usize {
    let index = CALL_ENDPOINTS
        .lock()
        .as_ref()
        .map(|registry| Arc::clone(&registry.senders));
    let Some(used) = index.and_then(|index| index.take(sender)) else {
        return 0;
    };
    for &id in &used {
        if let Some(endpoint) = lookup(id) {
            let _ = endpoint.cancel_posted_by(sender, audit);
        }
    }
    used.len()
}

/// `true` if `id` is currently bound. Diagnostic / test observer.
#[must_use]
pub fn contains(id: EndpointId) -> bool {
    CALL_ENDPOINTS
        .lock()
        .as_ref()
        .is_some_and(|registry| registry.endpoints.contains_key(&id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tairix_caps::CapabilitySet;
    use tairix_kernel_ipc::CallEndpointLimits;
    use tairix_kernel_sec::captable::{ProcessId, TaskCapabilities};
    use tairix_kernel_sec::identity::UserId;
    use tairix_log::Sink;

    /// A throwaway audit sink for endpoint construction in tests.
    struct NullSink;
    impl Sink for NullSink {
        fn write_event(&self, _event: &tairix_log::Event<'_>) {}
    }

    /// A recording sink capturing emitted event ids, for asserting the
    /// registration-denied audit.
    struct RecordingSink {
        ids: std::cell::RefCell<std::vec::Vec<u32>>,
    }
    impl RecordingSink {
        fn new() -> Self {
            tairix_log::set_max_level(tairix_log::Level::Trace);
            Self {
                ids: std::cell::RefCell::new(std::vec::Vec::new()),
            }
        }
    }
    impl Sink for RecordingSink {
        fn write_event(&self, event: &tairix_log::Event<'_>) {
            self.ids.borrow_mut().push(event.id.0);
        }
    }

    /// An endpoint `owner` binds at `id`.
    fn endpoint(id: u64, owner: u64) -> Arc<CallEndpoint> {
        let sink = NullSink;
        let creator = TaskCapabilities::derive(
            ProcessId(owner),
            UserId(1),
            CapabilitySet::empty(),
            CapabilitySet::empty(),
            &sink,
        )
        .expect("derived");
        Arc::new(
            CallEndpoint::create(
                EndpointId(id),
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
            .expect("unrestricted endpoint"),
        )
    }

    #[test]
    fn register_then_lookup_round_trips() {
        let id = EndpointId(0xCA11_0001);
        assert!(!contains(id));
        register(endpoint(id.0, 1), &NullSink).expect("first bind succeeds");
        assert!(contains(id));
        assert_eq!(lookup(id).map(|e| e.id()), Some(id));
        // Clean up so the global registry does not leak across tests.
        assert!(unregister(id).is_some());
        assert!(!contains(id));
    }

    #[test]
    fn duplicate_register_is_already_exists_and_audited() {
        let id = EndpointId(0xCA11_0002);
        let sink = RecordingSink::new();
        register(endpoint(id.0, 1), &sink).expect("first bind succeeds");
        assert!(
            sink.ids.borrow().is_empty(),
            "a successful bind emits no registry audit"
        );
        let err = register(endpoint(id.0, 1), &sink).expect_err("duplicate refused");
        assert_eq!(err, Errno::AlreadyExists);
        assert_eq!(
            *sink.ids.borrow(),
            [AuditEvent::CallEndpointRegisterDenied.id().0],
            "the refused bind is the audited decision"
        );
        unregister(id);
    }

    #[test]
    fn cancel_posted_by_scrubs_the_dead_posters_calls_on_registered_endpoints() {
        let id = EndpointId(0xCA11_0004);
        let ep = endpoint(id.0, 1);
        register(Arc::clone(&ep), &NullSink).expect("bound");
        // The poster task id must be unique to this test, not a small
        // shared constant: `cancel_posted_by` scrubs the poster's calls
        // across the whole (process-global) registry, so a sibling test
        // reusing the same id and scrubbing it in parallel would cancel
        // this call between the post and the assert. The endpoint id is
        // already made unique for the same reason.
        let poster = 0x0004_1234;
        let caller = TaskCapabilities::derive(
            ProcessId(poster),
            UserId(1),
            CapabilitySet::empty(),
            CapabilitySet::empty(),
            &NullSink,
        )
        .expect("derived");
        ep.post(
            &caller,
            0,
            b"q",
            u64::MAX,
            tairix_abi::RLIMIT_INFINITY,
            &NullSink,
        )
        .expect("posted");
        assert!(ep.has_pending());

        cancel_posted_by(poster, &NullSink);

        // The dead poster's queued call is gone before any server sees it.
        assert!(!ep.has_pending());
        unregister(id);
    }

    /// An exit scrubs the calls its process posted at the endpoints it called,
    /// visiting those alone however many are bound.
    #[test]
    fn an_exit_visits_only_the_endpoints_its_process_used() {
        let owner = crate::test_boot::claim_task();
        let poster = crate::test_boot::claim_peer_task();
        let ids = 0xCA11_8000..0xCA11_8040;
        let bound: std::vec::Vec<_> = ids.map(|id| endpoint(id, owner)).collect();
        for ep in &bound {
            register(Arc::clone(ep), &NullSink).expect("bound");
        }
        let caller = TaskCapabilities::derive(
            ProcessId(poster),
            UserId(1),
            CapabilitySet::empty(),
            CapabilitySet::empty(),
            &NullSink,
        )
        .expect("derived");
        for ep in [&bound[3], &bound[40]] {
            ep.post(
                &caller,
                0,
                b"q",
                u64::MAX,
                tairix_abi::RLIMIT_INFINITY,
                &NullSink,
            )
            .expect("posted");
        }
        assert_eq!(cancel_posted_by(poster, &NullSink), 2, "two of 64");
        assert!(bound.iter().all(|ep| ep.outstanding() == 0));
        assert_eq!(cancel_posted_by(poster, &NullSink), 0, "its notes went");
        let aspaces = RwLock::new(AddressSpaceRegistry::new());
        assert_eq!(teardown_owned_by(owner, &aspaces, &NullSink), 64);
    }

    /// An exit tears down the endpoints its process owns, visiting those alone
    /// however many are bound.
    #[test]
    fn a_teardown_visits_only_the_endpoints_its_process_owned() {
        let owner = crate::test_boot::claim_task();
        let neighbour = crate::test_boot::claim_peer_task();
        let ids = 0xCA11_8100..0xCA11_8140_u64;
        for id in ids.clone() {
            let by = if id % 32 == 5 { owner } else { neighbour };
            register(endpoint(id, by), &NullSink).expect("bound");
        }
        let aspaces = RwLock::new(AddressSpaceRegistry::new());
        assert_eq!(teardown_owned_by(owner, &aspaces, &NullSink), 2);
        let still_bound = ids.filter(|&id| contains(EndpointId(id))).count();
        assert_eq!(still_bound, 62, "every other stays bound");
        assert_eq!(teardown_owned_by(owner, &aspaces, &NullSink), 0);
        assert_eq!(teardown_owned_by(neighbour, &aspaces, &NullSink), 62);
    }

    /// A destruction gives back what the calls it cancelled held of their
    /// callers' budgets and wakes the threads waiting for it, however the
    /// endpoint goes.
    #[test]
    fn taking_an_endpoint_down_wakes_its_callers_budget_waiters() {
        let (_, waiter) = crate::test_boot::claim_scheduler();
        for (at, owned_teardown) in [(0xCA11_8410, false), (0xCA11_8411, true)] {
            let owner = crate::test_boot::claim_peer_task();
            let caller = crate::test_boot::claim_peer_task();
            let ep = endpoint(at, owner);
            register(Arc::clone(&ep), &NullSink).expect("bound");
            let caps = TaskCapabilities::derive(
                ProcessId(caller),
                UserId(1),
                CapabilitySet::empty(),
                CapabilitySet::empty(),
                &NullSink,
            )
            .expect("derived");
            ep.post(
                &caps,
                0,
                b"q",
                u64::MAX,
                tairix_abi::RLIMIT_INFINITY,
                &NullSink,
            )
            .expect("posted");
            crate::waitq::budget_watch(ProcessId(caller), waiter, false).expect("filed");
            let _ = crate::test_boot::take_unparked();
            if owned_teardown {
                let aspaces = RwLock::new(AddressSpaceRegistry::new());
                assert_eq!(teardown_owned_by(owner, &aspaces, &NullSink), 1);
            } else {
                assert!(take_down(ep.id(), &NullSink));
            }
            let woken = crate::test_boot::take_unparked();
            crate::waitq::budget_unwatch(ProcessId(caller), waiter);
            assert!(
                woken.contains(&waiter),
                "a budget waiter of its caller wakes"
            );
            assert_eq!(caps.charges().held(tairix_abi::LimitKind::IpcQueueBytes), 0);
        }
    }

    /// A teardown revokes the grants naming the endpoints its owner holds, and
    /// leaves those naming another's.
    #[test]
    fn a_teardown_revokes_only_the_grants_of_what_its_owner_holds() {
        let owner = crate::test_boot::claim_task();
        let other = crate::test_boot::claim_peer_task();
        let holder = crate::test_boot::claim_peer_task();
        let (mine, theirs) = (0xCA11_8412, 0xCA11_8413);
        register(endpoint(mine, owner), &NullSink).expect("bound");
        register(endpoint(theirs, other), &NullSink).expect("bound");
        let aspaces = RwLock::new(AddressSpaceRegistry::new());
        for id in [mine, theirs] {
            aspaces
                .write()
                .mint_grant(ProcessId(holder), tairix_abi::HwResource::endpoint(id))
                .expect("minted");
        }
        assert_eq!(teardown_owned_by(owner, &aspaces, &NullSink), 1);
        let covers = |id| {
            aspaces
                .read()
                .grant_covers(ProcessId(holder), &tairix_abi::HwResource::endpoint(id))
        };
        assert!(!covers(mine), "its endpoint's grant is revoked");
        assert!(covers(theirs), "another's stays");
        assert_eq!(teardown_owned_by(other, &aspaces, &NullSink), 1);
    }

    /// Taking an endpoint down under its live server wakes the callers whose
    /// calls it cancelled and the server, which would otherwise park on an
    /// endpoint that is gone.
    #[test]
    fn taking_an_endpoint_down_wakes_its_server_and_its_callers() {
        let (_, poster) = crate::test_boot::claim_scheduler();
        let owner = crate::test_boot::claim_peer_task();
        let caller = crate::test_boot::claim_peer_task();
        let server = crate::test_boot::claim_peer_task();
        let ep = endpoint(0xCA11_8300, owner);
        register(Arc::clone(&ep), &NullSink).expect("bound");
        let caps = TaskCapabilities::derive(
            ProcessId(caller),
            UserId(1),
            CapabilitySet::empty(),
            CapabilitySet::empty(),
            &NullSink,
        )
        .expect("derived");
        ep.post(
            &caps,
            poster,
            b"q",
            u64::MAX,
            tairix_abi::RLIMIT_INFINITY,
            &NullSink,
        )
        .expect("posted");
        crate::waitq::CALL_WAITQ
            .register(poster, crate::waitq::NO_DEADLINE)
            .expect("filed");
        let key = crate::waitq::endpoint_key(ep.id().0);
        crate::waitq::SERVE_WAITQ
            .register_keyed(key, server, crate::waitq::NO_DEADLINE)
            .expect("filed");
        let _ = crate::test_boot::take_unparked();
        assert!(take_down(ep.id(), &NullSink));
        let woken = crate::test_boot::take_unparked();
        crate::waitq::CALL_WAITQ.deregister(poster);
        crate::waitq::SERVE_WAITQ.deregister_keyed(key, server);
        assert!(woken.contains(&poster), "its caller wakes");
        assert!(woken.contains(&server), "its server wakes");
        assert!(!contains(ep.id()));
        assert!(!take_down(ep.id(), &NullSink), "taken down once");
    }

    /// A receive meeting an empty queue answers at once unless it blocks; a
    /// blocking one parks until a post wakes it, and one that cannot park fails
    /// closed rather than spin.
    #[test]
    fn a_blocking_receive_parks_until_a_post_and_one_that_cannot_park_fails_closed() {
        let boot = crate::test_boot::HostBoot::claim();
        let ep = endpoint(0xCA11_8400, crate::test_boot::claim_peer_task());
        register(Arc::clone(&ep), &NullSink).expect("bound");
        let never = || false;
        assert_eq!(
            receive_call(&ep, boot.task(), false, &mut || true, &never, || Ok(ep
                .recv_call(usize::MAX)
                .into()))
            .err(),
            Some(Errno::WouldBlock)
        );
        assert_eq!(
            receive_call(&ep, boot.task(), true, &mut || false, &never, || Ok(ep
                .recv_call(usize::MAX)
                .into()))
            .err(),
            Some(Errno::NotImplemented)
        );
        let server = {
            let ep = Arc::clone(&ep);
            boot.spawn(move || {
                let task = crate::test_boot::HostBoot::current().task();
                let park = &mut crate::waitq::park_calling_task;
                receive_call(&ep, task, true, park, &|| false, || {
                    Ok(ep.recv_call(usize::MAX).into())
                })
                .map(|call: ReceivedCall| call.request.as_bytes().to_vec())
            })
        };
        boot.until_blocked(server.id());
        let caps = TaskCapabilities::derive(
            ProcessId(boot.task()),
            UserId(1),
            CapabilitySet::empty(),
            CapabilitySet::empty(),
            &NullSink,
        )
        .expect("derived");
        ep.post(
            &caps,
            boot.task(),
            b"q",
            u64::MAX,
            tairix_abi::RLIMIT_INFINITY,
            &NullSink,
        )
        .expect("posted");
        crate::waitq::serve_wake(ep.id().0);
        let id = server.id();
        assert_eq!(server.join(), Ok(std::vec![b'q']));
        assert_eq!(boot.waits(id), 1, "the receive parked once, until the post");
        assert!(take_down(ep.id(), &NullSink));
    }

    /// A teardown wakes the callers of the calls it cancelled, and no other
    /// parked caller: it is not a broadcast.
    #[test]
    fn a_teardown_wakes_only_the_callers_of_what_it_destroyed() {
        let (_, thread) = crate::test_boot::claim_scheduler();
        let owner = crate::test_boot::claim_peer_task();
        let caller = crate::test_boot::claim_peer_task();
        let bystander = crate::test_boot::claim_peer_task();
        let ep = endpoint(0xCA11_8200, owner);
        register(Arc::clone(&ep), &NullSink).expect("bound");
        let caps = TaskCapabilities::derive(
            ProcessId(caller),
            UserId(1),
            CapabilitySet::empty(),
            CapabilitySet::empty(),
            &NullSink,
        )
        .expect("derived");
        ep.post(
            &caps,
            thread,
            b"q",
            u64::MAX,
            tairix_abi::RLIMIT_INFINITY,
            &NullSink,
        )
        .expect("posted");
        for task in [thread, bystander] {
            crate::waitq::CALL_WAITQ
                .register(task, crate::waitq::NO_DEADLINE)
                .expect("filed");
        }
        let _ = crate::test_boot::take_unparked();
        let aspaces = RwLock::new(AddressSpaceRegistry::new());
        assert_eq!(teardown_owned_by(owner, &aspaces, &NullSink), 1);
        let woken = crate::test_boot::take_unparked();
        for task in [thread, bystander] {
            crate::waitq::CALL_WAITQ.deregister(task);
        }
        assert!(woken.contains(&thread), "its caller wakes");
        assert!(!woken.contains(&bystander), "no other caller does");
    }

    #[test]
    fn lookup_unbound_is_none_and_unregister_is_idempotent() {
        let id = EndpointId(0xCA11_0003);
        assert!(lookup(id).is_none());
        assert!(unregister(id).is_none());
    }

    struct Recorder;

    static REPORTS: std::sync::Mutex<std::vec::Vec<(u64, bool)>> =
        std::sync::Mutex::new(std::vec::Vec::new());

    impl LinkServiceObserver for Recorder {
        fn link_service_changed(&self, id: EndpointId, live: bool) {
            REPORTS
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((id.0, live));
        }
    }

    static RECORDER: Recorder = Recorder;

    fn reports_for(id: u64) -> std::vec::Vec<bool> {
        REPORTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter(|&&(of, _)| of == id)
            .map(|&(_, live)| live)
            .collect()
    }

    /// An endpoint `owner` binds at `id`, which may be reserved.
    fn privileged_endpoint(id: u64, owner: u64) -> Arc<CallEndpoint> {
        let sink = NullSink;
        let mut caps = CapabilitySet::empty();
        caps.insert(tairix_abi::CapabilityId::IPC_BIND_PRIVILEGED);
        let creator = TaskCapabilities::derive(ProcessId(owner), UserId(1), caps, caps, &sink)
            .expect("derived");
        let limits = CallEndpointLimits {
            max_request: 64,
            max_reply: 64,
            per_sender: 4,
        };
        Arc::new(
            CallEndpoint::create(
                EndpointId(id),
                &creator,
                CapabilitySet::empty(),
                CapabilitySet::empty(),
                limits,
                tairix_abi::RLIMIT_INFINITY,
                &sink,
            )
            .expect("a privileged bind"),
        )
    }

    #[test]
    fn a_link_endpoint_is_reported_live_and_gone_however_it_leaves() {
        install_link_observer(&RECORDER);
        let sink = NullSink;
        let owner = crate::test_boot::claim_task();
        let clock = tairix_abi::driver::clock::CLOCK_CONTROLLER_ENDPOINTS.endpoint(0xCA11_0006);
        register(privileged_endpoint(clock, owner), &sink).expect("bound");
        assert_eq!(reports_for(clock), [true]);
        assert!(unregister(EndpointId(clock)).is_some());
        assert_eq!(reports_for(clock), [true, false]);

        register(privileged_endpoint(clock, owner), &sink).expect("rebound");
        teardown_owned_by(owner, &RwLock::new(AddressSpaceRegistry::new()), &sink);
        assert_eq!(reports_for(clock), [true, false, true, false]);

        let plain = 0xCA11_0007;
        register(endpoint(plain, 1), &sink).expect("bound");
        unregister(EndpointId(plain));
        assert!(
            reports_for(plain).is_empty(),
            "only a link endpoint is reported"
        );
    }
}
