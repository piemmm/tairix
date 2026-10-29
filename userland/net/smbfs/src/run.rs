//! The `Run` entry-point binary of `smbfs`, installed at
//! `/System/Services/smbfs.app/Run` (`plans/SMB.md`).
//!
//! One binary, two roles. Spawned in the sandbox's session-worker role it is
//! one connection's worker, holding two pipe ends and nothing else; started
//! normally it is the front, holding the `netshare-v1` endpoint, the sockets
//! and every authority the service has.
//!
//! # The reactor
//!
//! One wait-set: the endpoint, the exits of the login sessions mounts end
//! with, the resolver's wake, and per connection its delivery port —
//! registered only while the connection takes socket events, so a worker
//! that has not caught up holds its server back — and its worker's two pipe
//! ends, registered as its session wants them. A timeout is set for the one
//! instant the front must next act by. The loop parks; it never polls.
//!
//! On the host it is an inert stub so `cargo build --workspace`, clippy, and
//! fmt still cover the file.

#![cfg_attr(all(freestanding, feature = "program"), no_std)]
#![cfg_attr(all(freestanding, feature = "program"), no_main)]
#![deny(missing_docs)]

#[cfg(all(freestanding, feature = "program"))]
mod program {
    extern crate alloc;

    use alloc::sync::Arc;
    use alloc::vec::Vec;

    use tairix_abi::fsserve::Span;
    use tairix_abi::net::{SocketAddr, SocketId, SocketStreamEvent};
    use tairix_abi::net_ipc::{NetAddrFamily, IF_NAME_LEN};
    use tairix_abi::netattach::{FsserveCtl, NetworkOffer, OfferId};
    use tairix_abi::netshare_ipc::{
        NetshareReply, HOST_MAX, NETSHARE_ENDPOINT, NETSHARE_MAX_REPLY, NETSHARE_MAX_REQUEST,
    };
    use tairix_abi::time::Time64;
    use tairix_abi::waitset::{WaitSetOp, WaitSourceKind};
    use tairix_abi::{CapabilityId, Errno, FieldValue, ProcId};
    use tairix_caps::CapabilitySet;
    use tairix_hash::BuildSipHash13;
    use tairix_inline::ArrayString;
    use tairix_log::{Event, EventId, Field, Level};
    use tairix_procinfo::IpcTransport;
    use tairix_reclaim::PinnedShare;
    use tairix_resolver::RtDnsTransport;
    use tairix_rt::shm::ServeWindow;
    use tairix_rt::sync::WorkerWake;
    use tairix_rt::work::Worker;
    use tairix_rt::{LogSink, PrivatePort};
    use tairix_sandbox::rt::{
        serve_session_stdio, session_worker_role, RtSessionChannel, SessionMembers,
    };
    use tairix_sandbox::WorkerMemory;
    use tairix_smbfs::events::{
        CACHE_UNWATCHED, ORIGIN_UNREADABLE, RESOLVER_UNTHREADED, SERVICE_STARTED,
        SERVICE_UNAVAILABLE,
    };
    use tairix_smbfs::front::{Caching, Caller, Front};
    use tairix_smbfs::host::{Arming, ConnId, Host, Resolution, Unresolved};
    use tairix_smbfs::worker::Worker as ConnectionWorker;
    use tairix_util::defer::JobQueue;
    use tairix_util::fallible;
    use tairix_util::secret::Wiped;

    /// Exit code when the service cannot serve; the reason is logged first.
    const EXIT_UNAVAILABLE: i32 = 70;

    /// Calls the endpoint holds queued before they are taken: a fixed memory
    /// bound on what waits in the kernel, beside the front's own per-principal
    /// bound on what it holds.
    const CALL_CAPACITY: usize = 32;

    /// One connection's delivery port depth, and the most of its events one
    /// wake takes before the loop serves the others.
    const STREAM_CAPACITY: usize = 32;

    /// Lookups waiting for the resolver.
    const RESOLVER_QUEUE: usize = 64;

    /// Wait-set tokens. A connection's carry its slot and the end it names
    /// beneath the top bit; the rest are the service's own.
    const TOKEN_CALL: u64 = 1;
    const TOKEN_EXIT: u64 = 2;
    const TOKEN_RESOLVED: u64 = 3;
    const TOKEN_PRESSURE: u64 = 4;
    const CONN: u64 = 1 << 63;
    /// A served volume's token: its slot beneath this bit.
    const SERVED: u64 = 1 << 62;
    const END_PORT: u64 = 0;
    const END_READ: u64 = 1;
    const END_WRITE: u64 = 2;

    /// The audit sink every record is written through.
    static LOG_SINK: LogSink = LogSink;

    /// The service's one share of the machine's unwritten bytes, which every
    /// mounted volume draws on, as the process's pressure gauge is its one.
    static PINNED: PinnedShare = PinnedShare::new();

    const fn conn_token(slot: usize, end: u64) -> u64 {
        CONN | ((slot as u64) << 2) | end
    }

    /// A lookup the resolver is asked for a connection.
    struct Lookup {
        conn: ConnId,
        host: ArrayString<HOST_MAX>,
    }

    /// Its answer.
    struct Looked {
        conn: ConnId,
        resolution: Resolution,
    }

    type Resolver = Worker<Option<RtDnsTransport>, Lookup, Looked, JobQueue<Lookup, Looked>>;

    /// Resolve one host, keeping the transport for the next.
    fn look_up(transport: &mut Option<RtDnsTransport>, lookup: &mut Lookup) -> Looked {
        if transport.is_none() {
            *transport = RtDnsTransport::open().ok();
        }
        let resolution = match transport {
            Some(transport) => transport
                .host_address(&lookup.host, None)
                .ok_or(Unresolved::NoSuchHost),
            None => Err(Unresolved::Unanswered),
        };
        Looked {
            conn: lookup.conn,
            resolution,
        }
    }

    /// One connection's delivery and what of it is registered.
    struct Delivery {
        id: ConnId,
        port: Option<PrivatePort>,
        port_armed: bool,
        members: SessionMembers,
    }

    /// One offered volume: its offer, the endpoint its calls arrive on, and
    /// its window.
    struct Offered {
        offer: OfferId,
        endpoint: u64,
        window: ServeWindow,
    }

    /// The kernel, as the front sees it.
    struct RtHost {
        set: u64,
        deliveries: Vec<Option<Delivery>>,
        resolver: Arc<Resolver>,
        /// Every offered volume, by the slot its token names.
        served: Vec<Option<Offered>>,
    }

    impl RtHost {
        /// `id`'s delivery, made afresh for a slot taken by a new connection.
        fn delivery(&mut self, id: ConnId) -> Result<&mut Delivery, Errno> {
            let slot = id.index();
            if self.deliveries.len() <= slot {
                let more = slot + 1 - self.deliveries.len();
                self.deliveries
                    .try_reserve(more)
                    .map_err(|_| Errno::OutOfMemory)?;
                self.deliveries.resize_with(slot + 1, || None);
            }
            let set = self.set;
            let entry = &mut self.deliveries[slot];
            if entry.as_ref().is_none_or(|held| held.id != id) {
                *entry = Some(Delivery {
                    id,
                    port: None,
                    port_armed: false,
                    members: SessionMembers::new(
                        set,
                        conn_token(id.index(), END_READ),
                        conn_token(id.index(), END_WRITE),
                    ),
                });
            }
            entry.as_mut().ok_or(Errno::OutOfMemory)
        }

        fn offered(&mut self, offer: OfferId) -> Option<&mut Offered> {
            self.served
                .iter_mut()
                .flatten()
                .find(|held| held.offer == offer)
        }

        /// The offer a served volume's token names.
        fn served_token(&self, token: u64) -> Option<OfferId> {
            if token & (CONN | SERVED) != SERVED {
                return None;
            }
            let slot = usize::try_from(token & !SERVED).ok()?;
            self.served.get(slot)?.as_ref().map(|held| held.offer)
        }

        /// The connection a token names, and which of its ends.
        fn token(&self, token: u64) -> Option<(ConnId, u64, Option<u64>)> {
            if token & CONN == 0 {
                return None;
            }
            let slot = usize::try_from((token & !CONN) >> 2).ok()?;
            let delivery = self.deliveries.get(slot)?.as_ref()?;
            let port = delivery
                .port
                .as_ref()
                .filter(|_| delivery.port_armed)
                .map(PrivatePort::id);
            Some((delivery.id, token & 0b11, port))
        }
    }

    impl Host for RtHost {
        type Worker = RtSessionChannel;

        fn launch(&mut self) -> Result<RtSessionChannel, Errno> {
            RtSessionChannel::launch(tairix_abi::SPAWN_SELF, WorkerMemory::Messages)
        }

        fn open(&mut self, conn: ConnId, family: NetAddrFamily) -> Result<SocketId, Errno> {
            let port = PrivatePort::bind(SocketStreamEvent::MAX_WIRE_LEN, STREAM_CAPACITY)?;
            let socket = tairix_rt::net::stream_socket(family, port.id())?;
            self.delivery(conn)?.port = Some(port);
            Ok(socket)
        }

        fn connect(
            &mut self,
            socket: SocketId,
            to: SocketAddr,
            interface: Option<[u8; IF_NAME_LEN]>,
        ) -> Result<(), Errno> {
            tairix_rt::net::connect(socket, to, interface)
        }

        fn send(&mut self, socket: SocketId, bytes: &[u8]) -> Result<usize, Errno> {
            tairix_rt::net::stream_send(socket, bytes)
                .map(|taken| usize::try_from(taken).unwrap_or(bytes.len()))
        }

        fn close(&mut self, conn: ConnId, socket: SocketId) {
            let _ = tairix_rt::net::close(socket);
            let slot = conn.index();
            if self
                .deliveries
                .get(slot)
                .and_then(Option::as_ref)
                .is_some_and(|held| held.id == conn)
            {
                // Its members were withdrawn first; the port goes with it.
                self.deliveries[slot] = None;
            }
        }

        fn arm(&mut self, conn: ConnId, arming: Arming) -> Result<(), Errno> {
            let set = self.set;
            let delivery = self.delivery(conn)?;
            let port = delivery.port.as_ref().map(PrivatePort::id);
            let want = arming.socket && port.is_some();
            if let (Some(port), true) = (port, want != delivery.port_armed) {
                let op = if want { WaitSetOp::Add } else { WaitSetOp::Del };
                let token = conn_token(conn.index(), END_PORT);
                if tairix_rt::waitset_ctl(set, op, WaitSourceKind::Port, port, token) != 0 {
                    return Err(Errno::OutOfMemory);
                }
                delivery.port_armed = want;
            }
            delivery
                .members
                .sync(arming.worker, arming.read, arming.write)
        }

        fn resolve(
            &mut self,
            conn: ConnId,
            host: &str,
            _interface: Option<[u8; IF_NAME_LEN]>,
        ) -> Result<(), Errno> {
            let host = ArrayString::try_from(host).map_err(|_| Errno::OutOfRange)?;
            match self.resolver.submit(Lookup { conn, host }) {
                Ok(_) => Ok(()),
                Err(_) => Err(Errno::Busy),
            }
        }

        fn fill_random(&mut self, out: &mut [u8]) -> Result<(), Errno> {
            tairix_rt::random_fill(out)
        }

        fn wall_time(&mut self) -> Time64 {
            tairix_rt::wall_time().map_or(Time64::UNIX_EPOCH, |reading| reading.time())
        }

        fn reply(&mut self, ticket: u64, reply: &[u8]) {
            // A caller that has gone away is not answered.
            let _ = tairix_rt::call_reply(NETSHARE_ENDPOINT, ticket, reply);
        }

        fn offer(&mut self, offer: &NetworkOffer<'_>) -> Result<OfferId, Errno> {
            let slot = if let Some(slot) = self.served.iter().position(Option::is_none) {
                slot
            } else {
                self.served.try_reserve(1).map_err(|_| Errno::OutOfMemory)?;
                self.served.push(None);
                self.served.len() - 1
            };
            let (id, endpoint, window) = tairix_rt::fsserve_offer(offer)?;
            let token = SERVED | slot as u64;
            if tairix_rt::waitset_ctl(
                self.set,
                WaitSetOp::Add,
                WaitSourceKind::Endpoint,
                endpoint,
                token,
            ) != 0
            {
                let _ = tairix_rt::fsserve_ctl(id, FsserveCtl::Withdraw);
                return Err(Errno::OutOfMemory);
            }
            self.served[slot] = Some(Offered {
                offer: id,
                endpoint,
                window,
            });
            Ok(id)
        }

        fn withdraw(&mut self, offer: OfferId) {
            let _ = tairix_rt::fsserve_ctl(offer, FsserveCtl::Withdraw);
            let set = self.set;
            for (slot, entry) in self.served.iter_mut().enumerate() {
                if entry.as_ref().is_some_and(|held| held.offer == offer) {
                    if let Some(held) = entry.take() {
                        let _ = tairix_rt::waitset_ctl(
                            set,
                            WaitSetOp::Del,
                            WaitSourceKind::Endpoint,
                            held.endpoint,
                            SERVED | slot as u64,
                        );
                    }
                }
            }
        }

        fn watch(&mut self, anchor: ProcId) -> Result<(), Errno> {
            tairix_rt::peer_watch(anchor)
        }

        fn unwatch(&mut self, anchor: ProcId) {
            let _ = tairix_rt::peer_unwatch(anchor);
        }

        fn served_take(
            &mut self,
            offer: OfferId,
            into: &mut [u8],
        ) -> Result<Option<(u64, usize)>, Errno> {
            let endpoint = self.offered(offer).ok_or(Errno::NotFound)?.endpoint;
            Ok(tairix_rt::call_recv_ready(endpoint, into)?.map(|call| (call.ticket, call.len)))
        }

        fn served_reply(&mut self, offer: OfferId, ticket: u64, reply: &[u8]) {
            if let Some(held) = self.offered(offer) {
                // A call the kernel abandoned is answered to no one.
                let _ = tairix_rt::call_reply(held.endpoint, ticket, reply);
            }
        }

        fn window(&mut self, offer: OfferId, span: Span) -> Option<&mut [u8]> {
            self.offered(offer)
                .and_then(|held| held.window.span_mut(span.range()))
        }

        fn alive(&mut self, offer: OfferId) {
            let _ = tairix_rt::fsserve_ctl(offer, FsserveCtl::Alive);
        }
    }

    fn record(id: EventId, level: Level, message: &'static str, reason: &'static str) {
        tairix_log::log(
            &LOG_SINK,
            &Event {
                level,
                id,
                message,
                fields: &[Field {
                    key: "reason",
                    value: FieldValue::Str(reason),
                }],
            },
        );
    }

    /// Record why the service cannot serve, and the exit code that says so.
    fn unavailable(reason: &'static str) -> i32 {
        record(
            SERVICE_UNAVAILABLE,
            Level::Error,
            "smbfs: cannot serve",
            reason,
        );
        EXIT_UNAVAILABLE
    }

    /// Bind the endpoint, whose callers the kernel holds to `CAP_NET`, and
    /// watch it, the session exits, and the resolver.
    fn bind(set: u64, resolver: &Resolver) -> bool {
        let mut senders = CapabilitySet::empty();
        senders.insert(CapabilityId::NET);
        let watched =
            |kind, id, token| tairix_rt::waitset_ctl(set, WaitSetOp::Add, kind, id, token) == 0;
        tairix_rt::call_create(
            NETSHARE_ENDPOINT,
            &senders,
            &CapabilitySet::empty(),
            NETSHARE_MAX_REQUEST,
            NETSHARE_MAX_REPLY,
            CALL_CAPACITY,
        ) == 0
            && watched(WaitSourceKind::Endpoint, NETSHARE_ENDPOINT, TOKEN_CALL)
            && watched(WaitSourceKind::PeerExit, 0, TOKEN_EXIT)
            && resolver
                .wake()
                .read_end()
                .is_some_and(|fd| watched(WaitSourceKind::Stream, u64::from(fd), TOKEN_RESOLVED))
    }

    /// Serve every call waiting on the endpoint. A call whose origin cannot
    /// be attested is answered as unreadable, never served.
    fn serve_calls(
        front: &mut Front<RtHost, LogSink>,
        now: u64,
        request: &mut Wiped<NETSHARE_MAX_REQUEST>,
    ) {
        for _ in 0..CALL_CAPACITY {
            let Ok(Some(tairix_rt::ServedCall { ticket, len })) =
                tairix_rt::call_recv_ready(NETSHARE_ENDPOINT, &mut **request)
            else {
                return;
            };
            let Ok(origin) = tairix_rt::peer_origin(NETSHARE_ENDPOINT, ticket) else {
                record(
                    ORIGIN_UNREADABLE,
                    Level::Warn,
                    "smbfs: a caller could not be attested",
                    "the kernel would not attest the caller",
                );
                let mut unreadable = [0u8; 16];
                if let Ok(written) = NetshareReply::Unreadable.encode(&mut unreadable) {
                    let _ =
                        tairix_rt::call_reply(NETSHARE_ENDPOINT, ticket, &unreadable[..written]);
                }
                request.wipe();
                continue;
            };
            let caller = Caller {
                uid: origin.uid(),
                session: origin.login_session(),
            };
            let len = len.min(NETSHARE_MAX_REQUEST);
            front.call(now, ticket, caller, &mut request[..len]);
        }
    }

    /// Program entry point. `tairix-rt`'s `_start` calls it once the runtime
    /// is set up and routes its return value through the `exit` syscall.
    fn main() -> i32 {
        // The worker role first, before anything else: a worker never
        // behaves as the service.
        if session_worker_role() {
            let _ = serve_session_stdio(&mut ConnectionWorker::new());
            return 0;
        }
        let Ok(set) = u64::try_from(tairix_rt::waitset_create()) else {
            return unavailable("the reactor wait-set could not be created");
        };
        let Ok(resolver) = Worker::queued(look_up, None, WorkerWake::create(), RESOLVER_QUEUE)
        else {
            return unavailable("the resolver's queue could not be committed");
        };
        let resolver = Arc::new(resolver);
        if Worker::start(&resolver).is_err() {
            record(
                RESOLVER_UNTHREADED,
                Level::Warn,
                "smbfs: lookups run on the service's own thread",
                "the kernel refused the resolver its thread",
            );
        }
        if !bind(set, &resolver) {
            return unavailable("the endpoint could not be bound or watched");
        }
        let Some(seed) = tairix_rt::hash_seed() else {
            return unavailable("no key to index the shares' names under");
        };
        if !tairix_procinfo::pressure::watch(set, TOKEN_PRESSURE) {
            // The gauge stays at its unknown band, which caches nothing: the
            // shares are served, each byte sent before it is answered.
            record(
                CACHE_UNWATCHED,
                Level::Warn,
                "smbfs: nothing is cached",
                "the kernel refused the memory-pressure wake",
            );
        }
        let host = RtHost {
            set,
            deliveries: Vec::new(),
            resolver: Arc::clone(&resolver),
            served: Vec::new(),
        };
        let caching = Caching {
            memory: tairix_procinfo::memory_total_for_budget(&IpcTransport),
            pressure: tairix_rt::pressure::gauge(),
            sink: &LOG_SINK,
            pinned: &PINNED,
        };
        let Ok(mut front) = Front::new(host, LOG_SINK, BuildSipHash13::with_seed(seed), caching)
        else {
            return unavailable("the front's buffers could not be committed");
        };
        record(
            SERVICE_STARTED,
            Level::Info,
            "smbfs: serving",
            "bound the network-share endpoint",
        );
        serve(&mut front, set, &resolver)
    }

    /// Serve for the life of the service, returning only the exit code of a
    /// failure it cannot serve past.
    fn serve(front: &mut Front<RtHost, LogSink>, set: u64, resolver: &Resolver) -> i32 {
        let Some(mut stream) = fallible::filled(SocketStreamEvent::MAX_WIRE_LEN, 0u8) else {
            return unavailable("the receive buffer could not be committed");
        };
        let mut request = Wiped::<NETSHARE_MAX_REQUEST>::new();
        loop {
            let now = tairix_rt::clock_get();
            front.expire(now);
            let timeout = front
                .next_deadline()
                .map_or(u64::MAX, |at| at.saturating_sub(now));
            let mut token = 0u64;
            let waited = tairix_rt::waitset_wait(set, timeout, &mut token);
            let now = tairix_rt::clock_get();
            if waited != 0 {
                if Errno::from_syscall(waited) == Errno::TimedOut {
                    continue;
                }
                // A dead wait-set would degrade the loop into a busy poll.
                return unavailable("the reactor wait-set failed");
            }
            match token {
                TOKEN_CALL => serve_calls(front, now, &mut request),
                TOKEN_EXIT => {
                    while let Ok(anchor) = tairix_rt::peer_exit_take() {
                        front.peer_exited(now, anchor);
                    }
                }
                TOKEN_RESOLVED => resolver.wake().drain(),
                TOKEN_PRESSURE => {
                    if tairix_procinfo::pressure::refresh() {
                        front.pressure_changed();
                    }
                }
                token if front.host_mut().served_token(token).is_some() => {
                    if let Some(offer) = front.host_mut().served_token(token) {
                        front.served(now, offer);
                    }
                }
                token => match front.host_mut().token(token) {
                    Some((id, END_PORT, Some(port))) => {
                        // Bounded, so one busy connection cannot keep the
                        // loop from the others.
                        for _ in 0..STREAM_CAPACITY {
                            let Ok(event) = tairix_rt::net::stream_recv(port, &mut stream) else {
                                break;
                            };
                            front.socket(now, id, &event);
                            if front
                                .host_mut()
                                .token(token)
                                .and_then(|(_, _, port)| port)
                                .is_none()
                            {
                                break;
                            }
                        }
                    }
                    Some((id, END_READ, _)) => front.worker_readable(now, id),
                    Some((id, END_WRITE, _)) => front.worker_writable(now, id),
                    _ => {}
                },
            }
            // An answer lands on this thread when the resolver has none of
            // its own, with no wake to say so.
            resolver.collect_landed(|looked| {
                front.resolved(now, looked.conn, looked.resolution);
            });
        }
    }

    tairix_rt::entry!(main);
}

// --- Host stub ----------------------------------------------------------
//
// On the host the program's real entry — the freestanding `tairix-rt`
// `_start` path — is not compiled, so this inert `main` keeps the crate
// building under the host tooling. The front and the worker are host-tested
// in their own modules.
#[cfg(not(all(freestanding, feature = "program")))]
fn main() {}
