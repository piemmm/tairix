//! The front: the process holding the multicast DNS sockets and every
//! authority, which never parses a byte a peer sent.
//!
//! It owns one supervised decoder ([`crate::decoder`]) and drives it:
//!
//! * **Relay.** A datagram the stack delivered is relayed only when the stack
//!   found its sender on-link, the sender is within its relay budget, and the
//!   decoder runs an engine for the interface; the payload crosses unread. A
//!   sender past its budget is refused before the budget every sender shares
//!   is charged, so one flooding peer cannot starve the rest.
//! * **Links.** The stack tells each socket every interface its group
//!   membership rides and every edge on one. The front tells the decoder, and
//!   believes nothing the decoder says about an interface until the decoder
//!   has acknowledged the edge, so an answer an engine emitted before its link
//!   went down is never taken for one after.
//! * **Questions.** A client's typed request becomes one or two questions,
//!   each shared with every request asking it, admitted against the caller's
//!   kernel-attested identity and grants. The front holds the decoder to its
//!   word ([`crate::questions`]): an answer is taken only for a question it
//!   asked, on an interface the decoder runs, within what an honest engine
//!   could hold.
//! * **Transmit.** The decoder asks to send; the front sends only to the
//!   group, within a per-interface budget, or back to a peer it relayed from
//!   on that interface within the last second, once for each datagram it
//!   relayed.
//! * **Back-pressure and time**, as before: a datagram the decoder's queue has
//!   no room for is held, and everything after it waits, so the decoder sees
//!   frames in the order they were decided; ticks come no closer together
//!   than [`MIN_TICK_INTERVAL_NS`].
//! * **Containment.** A decoder that dies, breaks the framing, or sends a
//!   frame the front cannot believe is reaped and logged, everything derived
//!   from it is dropped and flushed from every client, and its replacement —
//!   keyed afresh — is told every link and asked every question again.
//!
//! Every instant here is monotonic nanoseconds, the unit the owner's clock and
//! wait-set speak.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use tairix_abi::discovery_ipc::{
    encode_doorbell, encode_id_reply, Change, DiscoveryRequest, Entry, Query, NAME_MAX,
    REQUESTS_PER_ACCOUNT,
};
use tairix_abi::net::{SocketAddr, SocketDelivery, SocketId, SocketLinkEvent};
use tairix_abi::net_ipc::{address_parts, ip_from_parts, IF_NAME_LEN};
use tairix_abi::reply::encode_status_reply;
use tairix_abi::time::Duration64;
use tairix_abi::{CapabilityId, Errno, FieldValue, Origin, ProcId};
use tairix_hash::HashSeed;
use tairix_inline::{ArrayString, ArrayVec};
use tairix_log::{Event, Field, Level, Sink};
use tairix_net::dnssd::ServiceType;
use tairix_net::mdns::{Destination, GROUP_V4, GROUP_V6, MAX_QUESTIONS, PORT};
use tairix_net::rate::{PeerBudgets, TokenBucket};
use tairix_net::IpAddr;
use tairix_sandbox::proto::FRAME_HEADER_LEN;
use tairix_sandbox::session::{SessionBounds, SessionDescriptors, SessionError};
use tairix_sandbox::supervise::{SessionLauncher, SupervisedSession};
use tairix_util::fallible;
use tairix_util::secret::Wiped;

use crate::events::{DECODER_STARTED, REQUEST_DENIED};
use crate::grants::Grants;
use crate::query::{plan, Needs, QUESTIONS_PER_REQUEST};
use crate::questions::{Questions, Spend, Verdict};
use crate::sessions::{Session, Sessions, Subscribed};
use crate::wire::{
    Edge, FromDecoder, ToDecoder, ASK_HEADER_LEN, CACHE_KEY_LEN, CONFIGURE_LEN, GROUP_BURST,
    GROUP_RATE, MAX_FROM_DECODER, MAX_TO_DECODER, RNG_KEY_LEN, TICK_LEN,
};

/// Bytes of frames the front may hold queued for its decoder.
pub const OUTBOUND_QUEUE: usize = 64 * 1024;

/// Bytes of the decoder's frames the front accumulates before taking them:
/// room for a burst of answers from one datagram.
pub const INBOUND_QUEUE: usize = 16 * 1024;

/// The least time between two ticks, however soon the decoder asks.
pub const MIN_TICK_INTERVAL_NS: u64 = 10_000_000;

/// Senders whose relay budget is tracked at once.
const RELAY_TRACKED_SOURCES: usize = 32;

/// Datagrams one sender may have relayed in a burst, and per second after.
const RELAY_SOURCE_BURST: u32 = 64;
const RELAY_SOURCE_RATE: u32 = 32;

/// The same for every sender together.
const RELAY_SHARED_BURST: u32 = 2048;
const RELAY_SHARED_RATE: u32 = 1024;

/// Accounts whose budgets are tracked; a quieter one forgotten can only be
/// handed a fresh budget.
const BUDGETED_ACCOUNTS: usize = 64;

/// Fresh questions one account's requests may raise in a burst, and per
/// second after. Each is queried on every link out of the group budget every
/// client shares — 32 to a datagram, 8 datagrams a second — so one account
/// takes at most half a datagram a second of it, and a client churning names
/// cannot starve another's first query.
const FRESH_ACCOUNT_BURST: u32 = 64;
const FRESH_ACCOUNT_RATE: u32 = 16;

/// The same for every account together: a quarter of the group budget.
const FRESH_SHARED_BURST: u32 = 256;
const FRESH_SHARED_RATE: u32 = 64;

/// Replays one account's requests may have the decoder make in a burst, and
/// per second after: each walks everything its question holds, so a client
/// starting and stopping one request in a loop costs no more than this.
const REPLAY_ACCOUNT_BURST: u32 = 32;
const REPLAY_ACCOUNT_RATE: u32 = 8;

/// The same for every account together.
const REPLAY_SHARED_BURST: u32 = 256;
const REPLAY_SHARED_RATE: u32 = 64;

/// Peers a reply may be owed to at once.
const ASKERS: usize = 32;

/// How long after a relayed datagram its sender may be answered directly:
/// past the half-second a responder may hold an answer back (RFC 6762 §6).
const ASKER_WINDOW_NS: u64 = 1_000_000_000;

/// Direct replies one sender can be owed at once.
const ASKER_CREDITS: u8 = 4;

/// How long the decoder may owe an acknowledgement before it is condemned.
/// An honest one answers as it reads the frame, so only a withheld answer
/// runs this long — holding question slots and account charges hostage —
/// and the bound leaves a decoder starved of the CPU ample time.
const ACK_DEADLINE_NS: u64 = 30_000_000_000;

// Every frame either side can send fits the queue it crosses, so a refusal
// is back-pressure and never a frame that can never be sent.
const _: () = assert!(OUTBOUND_QUEUE - FRAME_HEADER_LEN >= MAX_TO_DECODER);
const _: () = assert!(INBOUND_QUEUE - FRAME_HEADER_LEN >= MAX_FROM_DECODER);

/// The question slots one account may keep: its requests' subscriptions and
/// the questions it left that the decoder has not yet said it stopped.
const ACCOUNT_QUESTIONS: usize = QUESTIONS_PER_REQUEST * REQUESTS_PER_ACCOUNT;

// No one account can keep more than half the questions every client shares.
const _: () = assert!(2 * ACCOUNT_QUESTIONS <= MAX_QUESTIONS);

/// Everything the front does to the world outside it, so the front itself is
/// host-testable.
pub trait Host {
    /// Fill `out` with unpredictable bytes.
    ///
    /// # Errors
    ///
    /// The source's typed refusal.
    fn fill_random(&mut self, out: &mut [u8]) -> Result<(), Errno>;

    /// Send `payload` to `to` out of `interface`, from the multicast DNS
    /// socket of `to`'s family.
    ///
    /// # Errors
    ///
    /// The stack's refusal.
    fn transmit(
        &mut self,
        interface: [u8; IF_NAME_LEN],
        to: SocketAddr,
        payload: &[u8],
    ) -> Result<(), Errno>;

    /// Post `doorbell` to a client's delivery port.
    ///
    /// # Errors
    ///
    /// [`Errno::WouldBlock`] when the port has no room, or the kernel's
    /// refusal.
    fn ring(&mut self, port: u64, doorbell: &[u8]) -> Result<(), Errno>;

    /// Be told when `peer` exits.
    ///
    /// # Errors
    ///
    /// [`Errno::NotFound`] when it already has, or the kernel's refusal.
    fn watch(&mut self, peer: ProcId) -> Result<(), Errno>;
}

/// A call in service on the discovery endpoint, as the kernel attests it.
pub trait Caller {
    /// Who made the call.
    fn origin(&self) -> &Origin;

    /// Whether the caller owns `port`, so its doorbell can only ring a
    /// mailbox it drains itself.
    fn owns_port(&self, port: u64) -> bool;
}

/// Why the front cannot go on at all.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Fatal {
    /// The random source refused the keys a decoder needs: without them no
    /// decoder can be started, so the service stops rather than run one
    /// under predictable keys.
    Entropy(Errno),
}

/// The two sockets the front listens on, by family.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Sockets {
    /// The IPv4 socket, joined to `224.0.0.251`.
    pub v4: Option<SocketId>,
    /// The IPv6 socket, joined to `ff02::fb`.
    pub v6: Option<SocketId>,
}

/// Where the decoder's time stands.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Clock {
    /// The decoder has reported since the last tick, or has had none.
    Idle,
    /// A tick fell due while the decoder's queue had no room for it.
    Owed,
    /// A tick was queued and the decoder has not reported since.
    Sent,
}

/// What the current decoder was last told of a link.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Told {
    /// Nothing, or that it went down.
    Down,
    /// That it came up. `flapped` records that it has since gone down, so the
    /// decoder is told so even if it has come back.
    Up {
        /// It went down after the decoder was told it up.
        flapped: bool,
    },
}

/// One interface a socket's membership rides.
struct Link {
    name: [u8; IF_NAME_LEN],
    /// Whether each family's socket says it is up.
    v4: bool,
    v6: bool,
    told: Told,
    /// Edges told the decoder it has not yet acknowledged.
    pending: u8,
    group: TokenBucket,
}

impl Link {
    fn up(&self) -> bool {
        self.v4 || self.v6
    }

    fn told_up(&self) -> bool {
        matches!(self.told, Told::Up { .. })
    }

    /// The decoder runs an engine for it and has said everything it had to
    /// say about the one before.
    fn settled(&self) -> bool {
        self.told_up() && self.pending == 0
    }
}

/// A peer the front relayed from recently, which may be answered directly.
#[derive(Copy, Clone, Debug)]
struct Asker {
    interface: [u8; IF_NAME_LEN],
    addr: IpAddr,
    port: u16,
    credits: u8,
    until: u64,
}

/// An acknowledgement the decoder owes. It carries out frames in the order
/// they were sent and answers each as it does, so it owes them in order too.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Owed {
    Linked {
        interface: [u8; IF_NAME_LEN],
        up: bool,
    },
    Stopped(u32),
    Replayed {
        question: u32,
        token: u32,
    },
}

/// The front's state.
pub struct Front<L: SessionLauncher, S: Sink + Clone, H: Host> {
    decoder: SupervisedSession<L, S>,
    sink: S,
    host: H,
    sockets: Sockets,
    grants: Grants,
    admission: PeerBudgets<IpAddr, RELAY_TRACKED_SOURCES>,
    /// What each account's requests may cost the decoder in fresh questions.
    fresh: PeerBudgets<u32, BUDGETED_ACCOUNTS>,
    /// What each account's requests may cost the decoder in replays.
    replays: PeerBudgets<u32, BUDGETED_ACCOUNTS>,
    /// The relay's encoding buffer, holding at most one datagram frame.
    frame: Vec<u8>,
    /// Where one frame from the decoder is read into.
    inbound: Vec<u8>,
    /// The length of a frame in `frame` the decoder's queue had no room for.
    held: Option<usize>,
    /// The instant the current decoder last reported needing time.
    deadline: Option<u64>,
    clock: Clock,
    last_tick: u64,
    links: Vec<Link>,
    questions: Questions,
    sessions: Sessions,
    askers: ArrayVec<Asker, ASKERS>,
    /// What the decoder owes, oldest first, with when each was sent.
    owed: VecDeque<(Owed, u64)>,
    /// Flush passes made, so each request is told once per pass.
    flush_pass: u64,
}

impl<L: SessionLauncher, S: Sink + Clone, H: Host> Front<L, S, H> {
    /// A front whose decoders `launcher` starts, logging to `sink`, keying
    /// each decoder through `host`, learning its links through `sockets`, and
    /// admitting browses against `grants`. The first decoder is due at once.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfMemory`] when a buffer cannot be committed, and the
    /// random source's refusal of the key answers are fingerprinted under.
    pub fn new(
        launcher: L,
        sink: S,
        mut host: H,
        sockets: Sockets,
        grants: Grants,
    ) -> Result<Self, Errno> {
        let bounds =
            SessionBounds::new(OUTBOUND_QUEUE, INBOUND_QUEUE).map_err(|_| Errno::OutOfRange)?;
        let frame = fallible::filled(MAX_TO_DECODER, 0u8).ok_or(Errno::OutOfMemory)?;
        let inbound = fallible::filled(MAX_FROM_DECODER, 0u8).ok_or(Errno::OutOfMemory)?;
        let mut key = Wiped::<{ HashSeed::LEN }>::new();
        host.fill_random(&mut key[..])?;
        Ok(Self {
            decoder: SupervisedSession::new(launcher, bounds, sink.clone()),
            sink,
            host,
            sockets,
            grants,
            admission: PeerBudgets::new(
                RELAY_SOURCE_BURST,
                RELAY_SOURCE_RATE,
                RELAY_SHARED_BURST,
                RELAY_SHARED_RATE,
            ),
            fresh: PeerBudgets::new(
                FRESH_ACCOUNT_BURST,
                FRESH_ACCOUNT_RATE,
                FRESH_SHARED_BURST,
                FRESH_SHARED_RATE,
            ),
            replays: PeerBudgets::new(
                REPLAY_ACCOUNT_BURST,
                REPLAY_ACCOUNT_RATE,
                REPLAY_SHARED_BURST,
                REPLAY_SHARED_RATE,
            ),
            frame,
            inbound,
            held: None,
            deadline: None,
            clock: Clock::Idle,
            last_tick: 0,
            links: Vec::new(),
            questions: Questions::new(HashSeed::from_bytes(*key)),
            sessions: Sessions::default(),
            askers: ArrayVec::new(),
            owed: VecDeque::new(),
            flush_pass: 0,
        })
    }

    /// The instant the owner must wake by, or `None` when only a delivery, a
    /// call, or the decoder can wake it. After [`Self::on_wake`] it is always
    /// later than the instant that call was given, so the owner never spins.
    #[must_use]
    pub fn wake_at(&self) -> Option<u64> {
        let tick = self
            .deadline
            .filter(|_| self.may_tick())
            .map(|at| at.max(self.last_tick.saturating_add(MIN_TICK_INTERVAL_NS)));
        let overdue = self
            .owed
            .front()
            .filter(|_| self.decoder.is_live())
            .map(|&(_, sent)| sent.saturating_add(ACK_DEADLINE_NS));
        [self.decoder.restart_deadline(), tick, overdue]
            .into_iter()
            .flatten()
            .min()
    }

    /// Do whatever `now` has reached: start and key a decoder that is due,
    /// and give a live one the tick its deadline asks for.
    ///
    /// # Errors
    ///
    /// [`Fatal`] when the keys a due decoder needs cannot be drawn.
    pub fn on_wake(&mut self, now: u64) -> Result<(), Fatal> {
        if self
            .owed
            .front()
            .is_some_and(|&(_, sent)| now >= sent.saturating_add(ACK_DEADLINE_NS))
        {
            self.decoder
                .condemn(now, "the decoder withheld an acknowledgement");
            self.forget_decoder();
        }
        if self.decoder.restart_deadline().is_some_and(|at| now >= at) {
            self.start(now)?;
        }
        let due = self.deadline.is_some_and(|at| {
            now >= at && now >= self.last_tick.saturating_add(MIN_TICK_INTERVAL_NS)
        });
        if due && self.may_tick() {
            self.tick(now);
        }
        Ok(())
    }

    /// Whether the owner should drain the delivery port: a decoder is live
    /// and nothing is waiting for room in its queue. A link event waits in
    /// the stack, which redelivers it when the port drains, never drops it.
    #[must_use]
    pub fn wants_deliveries(&self) -> bool {
        self.decoder.is_live() && self.held.is_none()
    }

    /// Take one delivery from the stack at `now`.
    pub fn on_delivery(&mut self, now: u64, delivery: &SocketDelivery<'_>) {
        match delivery {
            SocketDelivery::Datagram(datagram) => {
                if !datagram.source_on_link || !self.wants_deliveries() {
                    return;
                }
                let Some(link) = self
                    .links
                    .iter()
                    .find(|link| link.name == datagram.interface)
                else {
                    return;
                };
                if !link.told_up() {
                    return;
                }
                let source = ip_from_parts(datagram.source.family, datagram.source.addr);
                if !self.admission.allow(Duration64::from_nanos(now), source) {
                    return;
                }
                self.note_asker(now, datagram.interface, source, datagram.source.port);
                let relayed = ToDecoder::Datagram {
                    now,
                    interface: datagram.interface,
                    source,
                    port: datagram.source.port,
                    to_group: datagram.to_group,
                    payload: datagram.payload,
                };
                if let Some(len) = relayed.encode(&mut self.frame) {
                    self.queue(len);
                }
            }
            SocketDelivery::Link(event) => {
                self.on_link(event);
                self.sync(now);
            }
        }
    }

    /// Panic unless the question and session tables are each consistent,
    /// and nothing is owed by a decoder that is not live: the oracle the
    /// fuzz harness holds the front to.
    #[cfg(any(test, feature = "test-support"))]
    pub fn assert_consistent(&self) {
        self.questions.assert_consistent();
        self.sessions.assert_consistent();
        assert!(self.owed.is_empty() || self.decoder.is_live());
    }

    /// Panic unless the front holds nothing a client asked for, and its
    /// decoder owes no acknowledgement: the leak oracle once every client has
    /// gone, the decoder has said everything, and the owner synced its watches.
    #[cfg(any(test, feature = "test-support"))]
    pub fn assert_idle(&self) {
        self.assert_consistent();
        self.questions.assert_idle();
        self.sessions.assert_idle();
        assert!(self.owed.is_empty(), "an acknowledgement owed for nothing");
    }

    /// Answer one call on the discovery endpoint from `caller`, writing the
    /// reply into `reply` and returning its length.
    pub fn serve(
        &mut self,
        now: u64,
        caller: &dyn Caller,
        request: &[u8],
        reply: &mut [u8],
    ) -> usize {
        let result = match DiscoveryRequest::decode(request) {
            Ok(request) => self.dispatch(now, caller, &request, reply),
            Err(err) => Err(err),
        };
        self.sync(now);
        match result {
            Ok(len) => len,
            Err(err) => {
                let status = encode_status_reply(Err(err));
                let len = status.len().min(reply.len());
                reply[..len].copy_from_slice(&status[..len]);
                len
            }
        }
    }

    /// `peer` has exited: its sessions end, and the questions only it asked
    /// stop being asked.
    pub fn on_peer_exit(&mut self, now: u64, peer: ProcId) {
        while let Some(session) = self.sessions.close_any(peer) {
            self.unsubscribe_all(&session);
        }
        self.sync(now);
    }

    /// Bring the owner's watches on client ports in line with the doorbells
    /// owed there, which it retries through [`Self::retry_rings`] once a
    /// watched port has room; as [`Sessions::sync_rooms`].
    pub fn sync_room_watches(
        &mut self,
        watch: &mut dyn FnMut(u64) -> bool,
        unwatch: &mut dyn FnMut(u64),
    ) {
        self.sessions.sync_rooms(watch, unwatch);
    }

    /// Post every doorbell owed.
    pub fn retry_rings(&mut self) {
        let Self { sessions, host, .. } = self;
        sessions.retry_owed(&mut |port, session| host.ring(port, &encode_doorbell(session)));
    }

    /// The decoder's write end has room. The held datagram goes first — it
    /// was decided before anything waiting behind it.
    pub fn on_decoder_writable(&mut self, now: u64) {
        if self.decoder.on_writable(now).is_err() {
            self.forget_decoder();
            return;
        }
        if let Some(len) = self.held.take() {
            self.queue(len);
        }
        if self.clock == Clock::Owed {
            self.tick(now);
        }
        self.sync(now);
    }

    /// The decoder's read end has bytes, or has closed.
    pub fn on_decoder_readable(&mut self, now: u64) {
        if self.decoder.on_readable(now).is_err() {
            self.forget_decoder();
            return;
        }
        let mut inbound = core::mem::take(&mut self.inbound);
        loop {
            let taken = self.decoder.recv(now, |frame| {
                let slot = inbound.get_mut(..frame.len())?;
                slot.copy_from_slice(frame);
                Some(frame.len())
            });
            let len = match taken {
                Ok(Some(Some(len))) => len,
                Ok(Some(None)) => {
                    self.decoder
                        .condemn(now, "the decoder sent a frame longer than any it sends");
                    self.forget_decoder();
                    break;
                }
                Ok(None) => break,
                Err(_) => {
                    self.forget_decoder();
                    break;
                }
            };
            let believed = match FromDecoder::decode(&inbound[..len]) {
                Ok(frame) => self.on_frame(now, frame),
                Err(_) => false,
            };
            if !believed {
                self.decoder
                    .condemn(now, "the decoder sent a frame no decoder sends");
                self.forget_decoder();
                break;
            }
        }
        self.inbound = inbound;
        self.sync(now);
    }

    /// The live decoder's descriptors, for the owner's wait-set.
    #[must_use]
    pub fn descriptors(&self) -> Option<SessionDescriptors> {
        self.decoder.descriptors()
    }

    /// Whether the owner should arm read readiness on the decoder.
    #[must_use]
    pub fn wants_read(&self) -> bool {
        self.decoder.wants_read()
    }

    /// Whether the owner should arm write-room readiness on the decoder.
    #[must_use]
    pub fn wants_write(&self) -> bool {
        self.decoder.wants_write()
    }

    /// Whether a tick may fall due: a decoder is live to take it, and no tick
    /// is owed or awaiting the decoder's report.
    fn may_tick(&self) -> bool {
        self.decoder.is_live() && self.clock == Clock::Idle
    }

    /// Draw a fresh decoder's keys, start it, and hand them to it; it is told
    /// its links and asked its questions as its queue takes them.
    ///
    /// The keys are drawn before the start, so a decoder is never left
    /// running without them.
    fn start(&mut self, now: u64) -> Result<(), Fatal> {
        let mut cache_key = Wiped::<CACHE_KEY_LEN>::new();
        let mut rng_key = Wiped::<RNG_KEY_LEN>::new();
        self.host
            .fill_random(&mut cache_key[..])
            .map_err(Fatal::Entropy)?;
        self.host
            .fill_random(&mut rng_key[..])
            .map_err(Fatal::Entropy)?;
        let Some(generation) = self.decoder.start(now) else {
            return Ok(());
        };
        self.forget_decoder();
        let mut frame = Wiped::<CONFIGURE_LEN>::new();
        let configure = ToDecoder::Configure {
            cache_key: &cache_key,
            rng_key: &rng_key,
        };
        // A fresh worker's queue is empty, and a configuration always fits
        // one; a worker that failed already is paced like any other.
        if let Some(len) = configure.encode(&mut frame[..]) {
            let _ = self.decoder.send(&frame[..len]);
        }
        tairix_log::log(
            &self.sink,
            &Event {
                level: Level::Info,
                id: DECODER_STARTED,
                message: "discoveryd: decoder started",
                fields: &[Field {
                    key: "generation",
                    value: FieldValue::UnsignedInt(generation),
                }],
            },
        );
        self.sync(now);
        Ok(())
    }

    /// Pass the decoder time.
    fn tick(&mut self, now: u64) {
        let mut frame = [0u8; TICK_LEN];
        let Some(len) = (ToDecoder::Tick { now }).encode(&mut frame) else {
            return;
        };
        match self.decoder.send(&frame[..len]) {
            Ok(()) => {
                self.clock = Clock::Sent;
                self.last_tick = now;
            }
            Err(SessionError::WorkerFailed) => self.forget_decoder(),
            // Every refusal but a failed worker is the queue lacking room: a
            // tick always fits an empty one.
            Err(_) => self.clock = Clock::Owed,
        }
    }

    /// Queue the relay frame of `len` bytes, holding it if the queue has no
    /// room.
    fn queue(&mut self, len: usize) {
        match self.decoder.send(&self.frame[..len]) {
            Err(SessionError::OutboundFull) => self.held = Some(len),
            Err(SessionError::WorkerFailed) => self.forget_decoder(),
            // Every relay frame fits the queue, so nothing else can refuse
            // one; if it did, the datagram is dropped.
            Ok(()) | Err(_) => {}
        }
    }

    /// Record a sender relayed from on `interface` as one that may be answered
    /// directly for a while. When the table is full the entry nearest its
    /// end makes way.
    fn note_asker(&mut self, now: u64, interface: [u8; IF_NAME_LEN], addr: IpAddr, port: u16) {
        let until = now.saturating_add(ASKER_WINDOW_NS);
        if let Some(asker) = self
            .askers
            .iter_mut()
            .find(|asker| asker.interface == interface && asker.addr == addr && asker.port == port)
        {
            asker.credits = asker.credits.saturating_add(1).min(ASKER_CREDITS);
            asker.until = until;
            return;
        }
        let asker = Asker {
            interface,
            addr,
            port,
            credits: 1,
            until,
        };
        if let Err(full) = self.askers.try_push(asker) {
            if let Some(oldest) = self.askers.iter_mut().min_by_key(|asker| asker.until) {
                *oldest = full.into_value();
            }
        }
    }

    /// Apply one link event from a family socket.
    fn on_link(&mut self, event: &SocketLinkEvent) {
        let v4 = Some(event.socket) == self.sockets.v4;
        if !v4 && Some(event.socket) != self.sockets.v6 {
            return;
        }
        let at = if let Some(at) = self
            .links
            .iter()
            .position(|link| link.name == event.interface)
        {
            at
        } else {
            if !event.up || self.links.try_reserve(1).is_err() {
                return;
            }
            self.links.push(Link {
                name: event.interface,
                v4: false,
                v6: false,
                told: Told::Down,
                pending: 0,
                group: TokenBucket::new(GROUP_BURST, GROUP_RATE),
            });
            self.links.len() - 1
        };
        let link = &mut self.links[at];
        let was_up = link.up();
        if v4 {
            link.v4 = event.up;
        } else {
            link.v6 = event.up;
        }
        if was_up && !link.up() && link.told_up() {
            link.told = Told::Up { flapped: true };
        }
    }

    /// Tell the decoder whatever it has not been told — each link's edge,
    /// each question stopped, asked, or owed a replay — in that order, until
    /// its queue is full. Nothing is sent while a datagram waits for room,
    /// so the decoder never meets a frame about a link out of order.
    fn sync(&mut self, now: u64) {
        if !self.decoder.is_live() || self.held.is_some() {
            return;
        }
        for at in 0..self.links.len() {
            let link = &self.links[at];
            let down = match link.told {
                Told::Up { flapped } => flapped || !link.up(),
                Told::Down => false,
            };
            let up = link.told == Told::Down && link.up();
            if !(down || up) {
                continue;
            }
            let interface = link.name;
            let edge = if up {
                Edge::Up(link.group.level())
            } else {
                Edge::Down
            };
            if !self.send_owed(
                now,
                &ToDecoder::Link {
                    now,
                    interface,
                    edge,
                },
                Owed::Linked { interface, up },
            ) {
                return;
            }
            let link = &mut self.links[at];
            link.pending = link.pending.saturating_add(1);
            if down {
                link.told = Told::Down;
                self.flush_pass += 1;
                let Self {
                    questions,
                    sessions,
                    host,
                    flush_pass,
                    ..
                } = self;
                questions.flush_link(interface, &mut |session, request| {
                    flush_request(sessions, host, session, request, interface, *flush_pass);
                });
            } else {
                link.told = Told::Up { flapped: false };
            }
        }
        // A link whose socket no longer reports it, and which the decoder is
        // done with, is forgotten.
        self.links
            .retain(|link| link.up() || link.told_up() || link.pending > 0);
        while let Some(question) = self.questions.next_stop() {
            if !self.send_owed(now, &ToDecoder::Stop { question }, Owed::Stopped(question)) {
                return;
            }
            self.questions.stopped(question);
        }
        while let Some((question, form, name)) = self.questions.next_ask() {
            if !self.send_control(&ToDecoder::Ask {
                now,
                question,
                form,
                name: name.as_wire(),
            }) {
                return;
            }
            self.questions.asked(question);
        }
        while let Some((question, token)) = self.questions.next_replay() {
            if !self.send_owed(
                now,
                &ToDecoder::Replay { question, token },
                Owed::Replayed { question, token },
            ) {
                return;
            }
            self.questions.replay_sent(question, token);
        }
    }

    /// Send a control frame the decoder answers with `owed`, recording the
    /// debt only once the queue took the frame, and reporting whether it did.
    fn send_owed(&mut self, now: u64, frame: &ToDecoder<'_>, owed: Owed) -> bool {
        if self.owed.try_reserve(1).is_err() || !self.send_control(frame) {
            return false;
        }
        self.owed.push_back((owed, now));
        true
    }

    /// The question replay `token` replays, when that replay is the oldest
    /// thing the decoder owes.
    fn replaying(&self, token: u32) -> Option<u32> {
        match self.owed.front() {
            Some(&(
                Owed::Replayed {
                    question,
                    token: owed,
                },
                _,
            )) if owed == token => Some(question),
            _ => None,
        }
    }

    /// The decoder acknowledged `owed`, which must be the oldest it owes.
    fn acknowledged(&mut self, owed: Owed) -> bool {
        let next = self.owed.front().map(|&(next, _)| next);
        if next == Some(owed) {
            self.owed.pop_front();
        }
        next == Some(owed)
    }

    /// Send one control frame, reporting whether the decoder's queue took it.
    fn send_control(&mut self, frame: &ToDecoder<'_>) -> bool {
        let mut bytes = [0u8; ASK_HEADER_LEN + NAME_MAX];
        let Some(len) = frame.encode(&mut bytes) else {
            return false;
        };
        match self.decoder.send(&bytes[..len]) {
            Ok(()) => true,
            Err(SessionError::WorkerFailed) => {
                self.forget_decoder();
                false
            }
            Err(_) => false,
        }
    }

    /// Carry out one frame from the decoder, reporting whether it is one an
    /// honest decoder sends.
    fn on_frame(&mut self, now: u64, frame: FromDecoder<'_>) -> bool {
        match frame {
            FromDecoder::Deadline(at) => {
                self.deadline = at;
                self.clock = Clock::Idle;
                true
            }
            FromDecoder::Answer(entry) => {
                let linked = entry_interface(&entry)
                    .and_then(|name| self.links.iter().find(|link| link.name == name))
                    .is_some_and(Link::settled);
                let verdict = self.questions.on_answer(&entry, linked, now);
                self.tell(verdict, &entry)
            }
            FromDecoder::Held { token, entry } => {
                // A replay's answers come before its end, while it is the
                // oldest thing owed, and are about the question it replays.
                let replaying = self.replaying(token);
                let Entry::Answer {
                    request: question,
                    change: Change::Added,
                    ..
                } = entry
                else {
                    return false;
                };
                if replaying != Some(question) {
                    return false;
                }
                let verdict = self.questions.on_held(token, &entry);
                self.tell(verdict, &entry)
            }
            FromDecoder::Replayed { token } => {
                let Some(question) = self.replaying(token) else {
                    return false;
                };
                self.owed.pop_front();
                self.questions.replayed(question, token);
                true
            }
            FromDecoder::Transmit {
                interface,
                at,
                to,
                payload,
            } => self.transmit(now, interface, at, to, payload),
            FromDecoder::Linked { interface, up } => {
                if !self.acknowledged(Owed::Linked { interface, up }) {
                    return false;
                }
                let Some(link) = self.links.iter_mut().find(|link| link.name == interface) else {
                    return false;
                };
                let Some(pending) = link.pending.checked_sub(1) else {
                    return false;
                };
                link.pending = pending;
                true
            }
            FromDecoder::Stopped { question } => {
                self.acknowledged(Owed::Stopped(question)) && self.questions.on_stopped(question)
            }
        }
    }

    /// Queue `entry` for the requests `verdict` names, ringing each quiet
    /// session; `false` when the verdict is that the decoder lied.
    fn tell(&mut self, verdict: Verdict, entry: &Entry<'_>) -> bool {
        if verdict == Verdict::Lie {
            return false;
        }
        let Self {
            questions,
            sessions,
            host,
            ..
        } = self;
        questions.tell(verdict, &mut |session, request| {
            let queued = sessions.queue(session, request, entry);
            ring(sessions, host, session, queued.ring);
            queued.taking
        });
        true
    }

    /// Stop every question `session`'s requests asked.
    fn unsubscribe_all(&mut self, session: &Session) {
        for request in session.requests() {
            for &question in &request.questions {
                self.questions
                    .unsubscribe(question, session.id(), request.id);
            }
        }
    }

    /// Send a datagram the decoder built at `at`, if its destination is one
    /// the front may reach: the group, charged to the interface's budget at
    /// that instant, or a peer it relayed from there recently, once per
    /// datagram relayed. `false` when no honest decoder sends it: one from
    /// the future, or past a budget it spends exactly as the front charges.
    fn transmit(
        &mut self,
        now: u64,
        interface: [u8; IF_NAME_LEN],
        at: u64,
        to: Destination,
        payload: &[u8],
    ) -> bool {
        if at > now {
            return false;
        }
        let Some(link) = self.links.iter_mut().find(|link| link.name == interface) else {
            return true;
        };
        // What an engine built before its link's last edge is not sent.
        if !link.settled() {
            return true;
        }
        let (v4, v6) = (link.v4, link.v6);
        match to {
            Destination::Group => {
                if !link.group.allow(Duration64::from_nanos(at)) {
                    return false;
                }
                if v4 {
                    let _ =
                        self.host
                            .transmit(interface, group_addr(IpAddr::V4(GROUP_V4)), payload);
                }
                if v6 {
                    let _ =
                        self.host
                            .transmit(interface, group_addr(IpAddr::V6(GROUP_V6)), payload);
                }
            }
            Destination::Peer { addr, port } => {
                let family_up = if addr.is_ipv4() { v4 } else { v6 };
                let Some(asker) = self.askers.iter_mut().find(|asker| {
                    asker.interface == interface
                        && asker.addr == addr
                        && asker.port == port
                        && asker.until >= now
                        && asker.credits > 0
                }) else {
                    // The engine budgets its own replies, so one the front
                    // will not send is a refusal, not a lie.
                    return true;
                };
                if !family_up {
                    return true;
                }
                asker.credits -= 1;
                let (family, octets) = address_parts(addr);
                let _ = self.host.transmit(
                    interface,
                    SocketAddr {
                        family,
                        addr: octets,
                        port,
                    },
                    payload,
                );
            }
        }
        true
    }

    /// Carry out one client call, returning the reply's length.
    fn dispatch(
        &mut self,
        now: u64,
        caller: &dyn Caller,
        request: &DiscoveryRequest<'_>,
        reply: &mut [u8],
    ) -> Result<usize, Errno> {
        let origin = caller.origin();
        if !origin.capabilities().holds_cap(CapabilityId::NET) {
            self.deny(origin, "discovery needs CAP_NET", None);
            return Err(Errno::PermissionDenied);
        }
        let owner = origin.proc_id();
        match *request {
            DiscoveryRequest::Open { deliver_port } => {
                // Rung with this service's identity, a doorbell aimed at
                // another process's port would spam a mailbox its caller
                // cannot reach.
                if !caller.owns_port(deliver_port) {
                    self.deny(origin, "doorbell port not the caller's", None);
                    return Err(Errno::PermissionDenied);
                }
                // A session is only held for a principal whose exit will be
                // heard.
                if !self.sessions.holds_any(owner) {
                    self.host.watch(owner)?;
                }
                let session = self.sessions.open(owner, origin.uid(), deliver_port)?;
                encode_id_reply(Ok(session), reply)
            }
            DiscoveryRequest::Close { session } => {
                let closed = self.sessions.close(session, owner)?;
                self.unsubscribe_all(&closed);
                status_ok(reply)
            }
            DiscoveryRequest::Start { session, query } => {
                let id = self.start_request(now, origin, session, &query)?;
                encode_id_reply(Ok(id), reply)
            }
            DiscoveryRequest::Stop { session, request } => {
                let stopped = self.sessions.remove_request(session, owner, request)?;
                for &question in &stopped.questions {
                    self.questions.unsubscribe(question, session, request);
                }
                status_ok(reply)
            }
            DiscoveryRequest::Collect { session, capacity } => self.sessions.collect(
                session,
                owner,
                usize::try_from(capacity).map_err(|_| Errno::OutOfRange)?,
                reply,
            ),
        }
    }

    /// Admit and start one request, returning its id.
    fn start_request(
        &mut self,
        now: u64,
        origin: &Origin,
        session: u32,
        query: &Query<'_>,
    ) -> Result<u32, Errno> {
        let owner = origin.proc_id();
        let planned = plan(query)?;
        let everything = origin
            .capabilities()
            .holds_cap(CapabilityId::NET_DISCOVER_ALL);
        match planned.needs {
            Needs::Nothing => {}
            Needs::Type(service) => {
                let granted = origin
                    .app()
                    .is_some_and(|app| self.grants.allows(app, &service));
                if !(everything || granted) {
                    self.deny(
                        origin,
                        "browsing a type needs a grant for it or CAP_NET_DISCOVER_ALL",
                        Some(service),
                    );
                    return Err(Errno::PermissionDenied);
                }
            }
            Needs::Everything => {
                if !everything {
                    self.deny(
                        origin,
                        "enumerating every type needs CAP_NET_DISCOVER_ALL",
                        None,
                    );
                    return Err(Errno::PermissionDenied);
                }
            }
        }
        let account = self.sessions.owned(session, owner)?.uid();
        let charged = self.questions.charged(account);
        if !self.sessions.has_room(session, owner)?
            || charged + QUESTIONS_PER_REQUEST > ACCOUNT_QUESTIONS
        {
            return Err(Errno::LimitExceeded);
        }
        let request = self
            .sessions
            .add_request(session, owner, Subscribed::new())?;
        let mut subscribed = Subscribed::new();
        let Self {
            questions,
            fresh,
            replays,
            ..
        } = self;
        let at = Duration64::from_nanos(now);
        let mut may = |spend| match spend {
            Spend::Ask => fresh.allow(at, account),
            Spend::Replay => replays.allow(at, account),
        };
        for asking in &planned.asks {
            match questions.subscribe(
                asking.form,
                asking.name,
                session,
                request,
                account,
                &mut may,
            ) {
                Ok(question) => {
                    // Never more than the plan's own bound, which this shares.
                    let _ = subscribed.try_push(question);
                }
                Err(err) => {
                    for &question in &subscribed {
                        self.questions.unsubscribe(question, session, request);
                    }
                    let _ = self.sessions.remove_request(session, owner, request);
                    return Err(err);
                }
            }
        }
        self.sessions
            .owned(session, owner)?
            .set_questions(request, subscribed);
        Ok(request)
    }

    /// Audit a refused request, naming what the caller lacked and the type it
    /// asked for — never whether anyone else holds it.
    fn deny(&self, origin: &Origin, reason: &'static str, service: Option<ServiceType>) {
        let mut rendered = ArrayString::<24>::new();
        if let Some(service) = service {
            let _ = core::fmt::Write::write_fmt(&mut rendered, format_args!("{service}"));
        }
        tairix_log::log(
            &self.sink,
            &Event {
                level: Level::Warn,
                id: REQUEST_DENIED,
                message: "discoveryd: request refused",
                fields: &[
                    Field {
                        key: "reason",
                        value: FieldValue::Str(reason),
                    },
                    Field {
                        key: "uid",
                        value: FieldValue::UnsignedInt(u64::from(origin.uid())),
                    },
                    Field {
                        key: "service",
                        value: FieldValue::Str(rendered.as_str()),
                    },
                ],
            },
        );
    }

    /// Drop everything learned from the current decoder — a replacement knows
    /// none of it, and a decoder that failed may have lied about all of it —
    /// and tell every client that held an answer so.
    fn forget_decoder(&mut self) {
        self.held = None;
        self.deadline = None;
        self.owed.clear();
        self.clock = Clock::Idle;
        for link in &mut self.links {
            link.told = Told::Down;
            link.pending = 0;
        }
        let Self {
            questions,
            sessions,
            host,
            flush_pass,
            ..
        } = self;
        // The table names requests link by link, so a new link starts a pass.
        let mut link = None;
        questions.forget_decoder(&mut |session, request, interface| {
            if link != Some(interface) {
                *flush_pass += 1;
                link = Some(interface);
            }
            flush_request(sessions, host, session, request, interface, *flush_pass);
        });
    }
}

/// Tell request `request` of `session` that what it held on `interface` is
/// gone, once in flush pass `pass`, ringing the session as queueing asks.
fn flush_request<H: Host>(
    sessions: &mut Sessions,
    host: &mut H,
    session: u32,
    request: u32,
    interface: [u8; IF_NAME_LEN],
    pass: u64,
) {
    let rung = sessions.flush(session, request, interface, pass);
    ring(sessions, host, session, rung);
}

/// Ring `session` on the port `rung` names, when queueing for it asked for a
/// doorbell, and record how the post went.
fn ring<H: Host>(sessions: &mut Sessions, host: &mut H, session: u32, rung: Option<u64>) {
    if let Some(port) = rung {
        let result = host.ring(port, &encode_doorbell(session));
        sessions.rang(session, result);
    }
}

/// The interface an answer entry names.
fn entry_interface(entry: &Entry<'_>) -> Option<[u8; IF_NAME_LEN]> {
    match entry {
        Entry::Answer { interface, .. } | Entry::Flush { interface, .. } => Some(*interface),
        Entry::Lost { .. } => None,
    }
}

/// The multicast DNS group of `group`'s family, on the mDNS port.
fn group_addr(group: IpAddr) -> SocketAddr {
    let (family, addr) = address_parts(group);
    SocketAddr {
        family,
        addr,
        port: PORT,
    }
}

fn status_ok(reply: &mut [u8]) -> Result<usize, Errno> {
    let status = encode_status_reply(Ok(()));
    reply
        .get_mut(..status.len())
        .ok_or(Errno::BufferTooSmall)?
        .copy_from_slice(&status);
    Ok(status.len())
}

#[cfg(test)]
#[path = "front_tests.rs"]
mod tests;
