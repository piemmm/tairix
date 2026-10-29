//! An in-memory world for the front: each worker runs in-process, and each
//! socket's far end is `tairix_smb`'s scripted server. Shared by the unit
//! tests and the fuzz harness.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::net::{IpAddr, Ipv4Addr};

use tairix_abi::driver::filesystem::AllocationClass;
use tairix_abi::fsserve::{encode_request, Request, Span, FSSERVE_REQUEST_MAX, FSSERVE_WINDOW_LEN};
use tairix_abi::net::{SocketAddr, SocketId, SocketStreamEvent, StreamCloseReason};
use tairix_abi::net_ipc::{ip_from_parts, NetAddrFamily, IF_NAME_LEN};
use tairix_abi::netattach::{NetworkOffer, OfferId};
use tairix_abi::netshare_ipc::{
    Connect, Credentials, DialectFloor, Endpoint, NetshareError, NetshareReply, NetshareRequest,
    Pin, NETSHARE_MAX_REQUEST, SMB_PORT,
};
use tairix_abi::secret::Secret;
use tairix_abi::time::Time64;
use tairix_abi::{Errno, ProcId};
use tairix_hash::{BuildSipHash13, FastHash, HashSeed};
use tairix_log::{DiscardSink, Event, EventId, Sink};
use tairix_reclaim::{PinnedShare, PressureBand, PressureGauge, ReportedPressure};
use tairix_sandbox::loopback::LoopbackSession;
use tairix_sandbox::proto::ProtoError;
use tairix_sandbox::session::{FrameOut, SessionService, SessionStep};
use tairix_smb::negotiate::Dialect;
use tairix_smb::peer::{Peer, PeerConfig, Share};

use crate::front::{Caching, Caller, Front};
use crate::host::{Arming, ConnId, Host};
use crate::worker::Worker;

/// Where the scripted server answers.
pub const NAS: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10));

/// Where nothing answers, and a connection is refused.
pub const ELSEWHERE: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 99));

/// Where nothing answers, and a connection never completes.
pub const SILENT: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 50));

/// A principal.
pub const ALICE: u32 = 1000;

/// Another.
pub const BOB: u32 = 1001;

/// The login session anchor `byte` names.
#[must_use]
pub fn session(byte: u8) -> ProcId {
    ProcId::from_raw([byte; 16])
}

/// [`ALICE`] in session 1.
#[must_use]
pub fn alice() -> Caller {
    Caller {
        uid: ALICE,
        session: Some(session(1)),
    }
}

/// The audit records the front wrote.
#[derive(Clone, Default)]
pub struct Journal(Rc<RefCell<Vec<EventId>>>);

impl Sink for Journal {
    fn write_event(&self, event: &Event<'_>) {
        self.0.borrow_mut().push(event.id);
    }
}

impl Journal {
    /// Whether a record of `id` was written.
    #[must_use]
    pub fn logged(&self, id: EventId) -> bool {
        self.0.borrow().contains(&id)
    }
}

/// What a chaos worker does with each frame its honest worker wrote: write
/// it, or anything else, to the front.
pub type Perturb = Box<dyn FnMut(&[u8], &mut dyn FrameOut) -> Result<(), ProtoError>>;

/// A connection's worker as the world starts it.
pub enum Hand {
    /// The worker.
    Honest(Box<Worker>),
    /// One answering every order with a frame no worker writes.
    Liar,
    /// The worker, every frame it writes handed to a perturbation first.
    Chaos(Box<Worker>, Perturb),
    /// The worker, ending at the first order after the flag is set, as a
    /// worker that crashes.
    Mortal(Box<Worker>, Rc<Cell<bool>>),
}

impl SessionService for Hand {
    fn handle(&mut self, request: &[u8], out: &mut dyn FrameOut) -> SessionStep {
        match self {
            Self::Mortal(_, dead) if dead.get() => SessionStep::Finished,
            Self::Honest(worker) | Self::Mortal(worker, _) => worker.handle(request, out),
            Self::Liar => match out.frame(&[0xee]) {
                Ok(()) => SessionStep::Continue,
                Err(_) => SessionStep::Finished,
            },
            Self::Chaos(worker, perturb) => {
                let mut written = Written::default();
                let step = worker.handle(request, &mut written);
                for frame in &written.0 {
                    if perturb(frame, out).is_err() {
                        return SessionStep::Finished;
                    }
                }
                step
            }
        }
    }
}

/// The frames a worker wrote, held.
#[derive(Default)]
struct Written(Vec<Vec<u8>>);

impl FrameOut for Written {
    fn frame(&mut self, payload: &[u8]) -> Result<(), ProtoError> {
        self.0.push(payload.to_vec());
        Ok(())
    }
}

/// An event a socket has for the front.
pub enum Pending {
    /// It connected.
    Connected,
    /// The server sent these bytes.
    Data(Vec<u8>),
    /// It closed.
    Closed,
}

/// One socket and the server at its far end.
pub struct Socket {
    /// The connection it is for.
    pub conn: ConnId,
    /// The server, once connected.
    pub peer: Option<Peer>,
    /// What it has for the front, in order.
    pub pending: VecDeque<Pending>,
    /// Whether what it has is held back from the front.
    pub held: bool,
    /// Whether the network between it and its server went quiet: what the
    /// front sends is lost, and nothing comes back.
    pub silent: bool,
}

/// One offered volume as the kernel holds it: the calls waiting on it, its
/// window, and what the front answered.
pub struct Offered {
    /// The offer.
    pub offer: OfferId,
    /// Calls waiting to be taken: their tickets and requests.
    pub waiting: VecDeque<(u64, Vec<u8>)>,
    /// The window.
    pub window: Vec<u8>,
    /// Every answer, with its ticket.
    pub answers: Vec<(u64, Vec<u8>)>,
    /// How often the server said it was working.
    pub alive: usize,
    /// Whether it was withdrawn, its endpoint taken down.
    pub gone: bool,
}

/// What the front did to the world, and how the world answers it.
#[derive(Default)]
pub struct World {
    /// The servers answering at each address.
    pub servers: Vec<(IpAddr, PeerConfig)>,
    /// Each server's share, which every connection to it serves.
    shares: Vec<(IpAddr, Rc<RefCell<Share>>)>,
    /// Every socket opened, by id; a closed one is `None`.
    pub sockets: Vec<Option<Socket>>,
    /// What each connection last waited on.
    pub armed: Vec<(ConnId, Arming)>,
    /// Every reply, with its ticket.
    pub replies: Vec<(u64, Vec<u8>)>,
    /// Every lookup asked, with the connection it is for.
    pub asked: Vec<(ConnId, String)>,
    /// Every offer made: its id, its principal and its source.
    pub offers: Vec<(OfferId, u32, String)>,
    /// Every offer withdrawn.
    pub withdrawn: Vec<OfferId>,
    /// Every anchor watched.
    pub watched: Vec<ProcId>,
    /// Every anchor no longer watched.
    pub unwatched: Vec<ProcId>,
    /// Anchors already gone, which a watch is refused for.
    pub ended: Vec<ProcId>,
    /// Per closed socket, whether its server had authenticated a session.
    pub authenticated: Vec<bool>,
    /// The workers the next launches start; an honest one when none is left.
    pub hands: VecDeque<Hand>,
    /// Every volume offered.
    pub volumes: Vec<Offered>,
    /// Words of randomness drawn, each hashed into the next eight bytes, so
    /// no two keys drawn are alike as a byte counter's would be every 256.
    draws: u64,
    changes: u64,
}

impl World {
    /// A world with `config`'s server answering at [`NAS`].
    #[must_use]
    pub fn serving(config: PeerConfig) -> Self {
        Self {
            servers: vec![(NAS, config)],
            ..Self::default()
        }
    }

    /// How many sockets the front opened.
    #[must_use]
    pub fn opened(&self) -> usize {
        self.sockets.len()
    }

    /// How many are still open.
    #[must_use]
    pub fn live(&self) -> usize {
        self.sockets.iter().flatten().count()
    }

    /// Hold back, or let go, what every open socket has for the front: a
    /// server that has answered, its answers not yet arrived.
    pub fn hold(&mut self, held: bool) {
        for socket in self.sockets.iter_mut().flatten() {
            socket.held = held;
        }
    }

    /// The data of the file `names` reach on [`NAS`]'s share.
    #[must_use]
    pub fn server_file(&self, names: &[&str]) -> Option<Vec<u8>> {
        let (_, share) = self.shares.iter().find(|(at, _)| *at == NAS)?;
        share.borrow().contents(names).map(<[u8]>::to_vec)
    }

    /// Cut the connection socket `at` carries, as a network does: its server
    /// lets the connection go, and the front is told the socket closed.
    pub fn cut(&mut self, at: usize) {
        self.changes += 1;
        let Some(entry) = self.sockets.get_mut(at).and_then(Option::as_mut) else {
            return;
        };
        if let Some(peer) = entry.peer.take() {
            peer.disconnect().expect("the share lets the connection go");
        }
        entry.held = false;
        entry.pending.clear();
        entry.pending.push_back(Pending::Closed);
    }

    /// Quieten the network under socket `at`: nothing the front sends
    /// reaches its server, and nothing comes back, until the front gives
    /// the socket up.
    pub fn silence(&mut self, at: usize) {
        if let Some(entry) = self.sockets.get_mut(at).and_then(Option::as_mut) {
            entry.silent = true;
            entry.pending.clear();
        }
    }

    /// The live sockets, by index.
    #[must_use]
    pub fn live_sockets(&self) -> Vec<usize> {
        self.sockets
            .iter()
            .enumerate()
            .filter_map(|(at, entry)| entry.as_ref().map(|_| at))
            .collect()
    }

    /// Every server restarted: what each share held open is gone.
    pub fn restart(&mut self) {
        for (_, share) in &self.shares {
            share.borrow_mut().restart();
        }
    }

    /// How many `ECHO`s the servers of the open connections have answered.
    #[must_use]
    pub fn echoes(&self) -> usize {
        self.sockets
            .iter()
            .flatten()
            .filter_map(|entry| entry.peer.as_ref())
            .map(Peer::echoes)
            .sum()
    }

    /// How many durable opens the servers' reconnect creates took back.
    #[must_use]
    pub fn server_reclaimed(&self) -> usize {
        self.shares
            .iter()
            .map(|(_, share)| share.borrow().reclaimed())
            .sum()
    }

    /// How many durable opens the servers keep for connections that
    /// dropped.
    #[must_use]
    pub fn server_detached(&self) -> usize {
        self.shares
            .iter()
            .map(|(_, share)| share.borrow().detached_opens())
            .sum()
    }

    /// Time reaches `now` for every server's share: the durable opens they
    /// let go of go.
    pub fn tick(&mut self, now: u64) {
        for (_, share) in &self.shares {
            share.borrow_mut().advance(now).expect("the share moves on");
        }
        self.deliver_all();
    }

    /// How many opens the servers hold for their connections.
    #[must_use]
    pub fn server_opens(&self) -> usize {
        self.shares
            .iter()
            .map(|(_, share)| share.borrow().attached_opens())
            .sum()
    }
}

impl Host for World {
    type Worker = LoopbackSession<Hand>;

    fn launch(&mut self) -> Result<Self::Worker, Errno> {
        self.changes += 1;
        let hand = self
            .hands
            .pop_front()
            .unwrap_or_else(|| Hand::Honest(Box::default()));
        Ok(LoopbackSession::new(hand))
    }

    fn open(&mut self, conn: ConnId, _family: NetAddrFamily) -> Result<SocketId, Errno> {
        self.changes += 1;
        self.sockets.push(Some(Socket {
            conn,
            peer: None,
            pending: VecDeque::new(),
            held: false,
            silent: false,
        }));
        Ok(SocketId::try_from(self.sockets.len() - 1).expect("few"))
    }

    fn connect(
        &mut self,
        socket: SocketId,
        to: SocketAddr,
        _interface: Option<[u8; IF_NAME_LEN]>,
    ) -> Result<(), Errno> {
        self.changes += 1;
        let address = ip_from_parts(to.family, to.addr);
        let server = self
            .servers
            .iter()
            .find(|(at, _)| *at == address && to.port == SMB_PORT)
            .map(|(_, config)| config.clone());
        let share = server.is_some().then(|| self.share_at(address));
        let entry = self.sockets[socket as usize].as_mut().expect("open");
        match server.zip(share) {
            Some((config, share)) => {
                entry.peer = Some(Peer::with_share(config, share));
                entry.pending.push_back(Pending::Connected);
            }
            None if address == SILENT => {}
            None => entry.pending.push_back(Pending::Closed),
        }
        Ok(())
    }

    fn send(&mut self, socket: SocketId, bytes: &[u8]) -> Result<usize, Errno> {
        self.changes += 1;
        let entry = self.sockets[socket as usize].as_mut().expect("open");
        let Some(peer) = entry.peer.as_mut().filter(|_| !entry.silent) else {
            return Ok(bytes.len());
        };
        if peer.receive(bytes).is_err() {
            entry.pending.push_back(Pending::Closed);
            return Ok(bytes.len());
        }
        let output = peer.take_output();
        for chunk in output.chunks(crate::wire::CHUNK_MAX) {
            entry.pending.push_back(Pending::Data(chunk.to_vec()));
        }
        self.deliver_all();
        Ok(bytes.len())
    }

    fn close(&mut self, _conn: ConnId, socket: SocketId) {
        self.changes += 1;
        if let Some(entry) = self.sockets[socket as usize].take() {
            let keyed = entry
                .peer
                .as_ref()
                .is_some_and(|peer| peer.session_id().is_some());
            self.authenticated.push(keyed);
            if let Some(peer) = entry.peer {
                peer.disconnect().expect("the share lets the connection go");
            }
        }
        self.deliver_all();
    }

    fn arm(&mut self, conn: ConnId, arming: Arming) -> Result<(), Errno> {
        match self.armed.iter_mut().find(|(id, _)| *id == conn) {
            Some((_, held)) if *held == arming => {}
            Some((_, held)) => {
                *held = arming;
                self.changes += 1;
            }
            None => {
                self.armed.push((conn, arming));
                self.changes += 1;
            }
        }
        Ok(())
    }

    fn resolve(
        &mut self,
        conn: ConnId,
        host: &str,
        _interface: Option<[u8; IF_NAME_LEN]>,
    ) -> Result<(), Errno> {
        self.changes += 1;
        self.asked.push((conn, String::from(host)));
        Ok(())
    }

    fn fill_random(&mut self, out: &mut [u8]) -> Result<(), Errno> {
        for chunk in out.chunks_mut(8) {
            self.draws += 1;
            let word = FastHash::hash_bytes(0, &self.draws.to_le_bytes()).to_le_bytes();
            chunk.copy_from_slice(&word[..chunk.len()]);
        }
        Ok(())
    }

    fn wall_time(&mut self) -> Time64 {
        Time64::from_secs(1_790_000_000)
    }

    fn reply(&mut self, ticket: u64, reply: &[u8]) {
        self.changes += 1;
        self.replies.push((ticket, reply.to_vec()));
    }

    fn offer(&mut self, offer: &NetworkOffer<'_>) -> Result<OfferId, Errno> {
        self.changes += 1;
        let id = OfferId::new(100 + self.offers.len() as u64).expect("not zero");
        self.offers
            .push((id, offer.for_uid, String::from(offer.source)));
        self.volumes.push(Offered {
            offer: id,
            waiting: VecDeque::new(),
            window: vec![0; FSSERVE_WINDOW_LEN],
            answers: Vec::new(),
            alive: 0,
            gone: false,
        });
        Ok(id)
    }

    fn withdraw(&mut self, offer: OfferId) {
        self.changes += 1;
        self.withdrawn.push(offer);
        if let Some(volume) = self.volume(offer) {
            volume.gone = true;
        }
    }

    fn watch(&mut self, anchor: ProcId) -> Result<(), Errno> {
        self.changes += 1;
        if self.ended.contains(&anchor) {
            return Err(Errno::NotFound);
        }
        self.watched.push(anchor);
        Ok(())
    }

    fn unwatch(&mut self, anchor: ProcId) {
        self.changes += 1;
        self.unwatched.push(anchor);
    }

    fn served_take(
        &mut self,
        offer: OfferId,
        into: &mut [u8],
    ) -> Result<Option<(u64, usize)>, Errno> {
        let volume = self.volume(offer).ok_or(Errno::NotFound)?;
        if volume.gone {
            return Err(Errno::NotFound);
        }
        let Some((ticket, request)) = volume.waiting.pop_front() else {
            return Ok(None);
        };
        into[..request.len()].copy_from_slice(&request);
        self.changes += 1;
        Ok(Some((ticket, request.len())))
    }

    fn served_reply(&mut self, offer: OfferId, ticket: u64, reply: &[u8]) {
        self.changes += 1;
        if let Some(volume) = self.volume(offer) {
            volume.answers.push((ticket, reply.to_vec()));
        }
    }

    fn window(&mut self, offer: OfferId, span: Span) -> Option<&mut [u8]> {
        self.volume(offer)
            .and_then(|volume| volume.window.get_mut(span.range()))
    }

    fn alive(&mut self, offer: OfferId) {
        if let Some(volume) = self.volume(offer) {
            volume.alive += 1;
        }
    }
}

impl World {
    /// The share the server at `address` serves, made on its first
    /// connection.
    fn share_at(&mut self, address: IpAddr) -> Rc<RefCell<Share>> {
        if let Some((_, share)) = self.shares.iter().find(|(at, _)| *at == address) {
            return Rc::clone(share);
        }
        let share = Rc::new(RefCell::new(Share::new()));
        self.shares.push((address, Rc::clone(&share)));
        share
    }

    /// Hand every connection what other connections' requests left it: a
    /// break, a create resumed, a watch completed.
    fn deliver_all(&mut self) {
        for entry in self.sockets.iter_mut().flatten() {
            let Some(peer) = entry.peer.as_mut().filter(|_| !entry.silent) else {
                continue;
            };
            if peer.deliver().is_err() {
                entry.pending.push_back(Pending::Closed);
                continue;
            }
            let output = peer.take_output();
            for chunk in output.chunks(crate::wire::CHUNK_MAX) {
                entry.pending.push_back(Pending::Data(chunk.to_vec()));
            }
        }
    }

    /// The volume `offer` names.
    pub fn volume(&mut self, offer: OfferId) -> Option<&mut Offered> {
        self.volumes.iter_mut().find(|volume| volume.offer == offer)
    }
}

/// The pressure a bench's front caches under unless it names its own.
static CALM: ReportedPressure = ReportedPressure::unknown();

/// A front over a world, and the bench driving it as the loop would.
pub struct Bench {
    /// The front.
    pub front: Front<World, Journal>,
    /// Its audit records.
    pub journal: Journal,
    /// The monotonic clock.
    pub now: u64,
    next_ticket: u64,
}

impl Bench {
    /// A front with nothing open over `world`, on a machine of a gibibyte
    /// under no memory pressure.
    ///
    /// # Panics
    ///
    /// When the front's buffers cannot be had.
    #[must_use]
    pub fn new(world: World) -> Self {
        CALM.report(PressureBand::Normal);
        Self::caching(world, 1 << 30, &CALM)
    }

    /// A front with nothing open over `world`, caching within `memory`
    /// bytes as `pressure` says.
    ///
    /// # Panics
    ///
    /// When the front's buffers cannot be had.
    #[must_use]
    pub fn caching(world: World, memory: usize, pressure: &'static dyn PressureGauge) -> Self {
        let journal = Journal::default();
        let hasher = BuildSipHash13::with_seed(HashSeed::from_bytes([0x5e; HashSeed::LEN]));
        let caching = Caching {
            memory,
            pressure,
            sink: &DiscardSink,
            // A share of its own, so no bench sees another's unwritten bytes.
            pinned: Box::leak(Box::new(PinnedShare::new())),
        };
        Self {
            front: Front::new(world, journal.clone(), hasher, caching).expect("room"),
            journal,
            now: 1_000,
            next_ticket: 1,
        }
    }

    /// The world.
    pub fn world(&mut self) -> &mut World {
        self.front.host_mut()
    }

    /// Make the call `bytes` hold as `caller`, carry everything it causes
    /// until nothing moves, and answer its ticket.
    ///
    /// # Panics
    ///
    /// When the request is not erased once the front has it.
    pub fn call_bytes(&mut self, caller: Caller, bytes: &mut [u8]) -> u64 {
        let ticket = self.next_ticket;
        self.next_ticket += 1;
        self.front.call(self.now, ticket, caller, bytes);
        assert!(bytes.iter().all(|&byte| byte == 0), "the request is erased");
        self.run();
        ticket
    }

    /// Make the call `request` as `caller`, as [`Self::call_bytes`].
    ///
    /// # Panics
    ///
    /// When `request` does not encode.
    pub fn call(&mut self, caller: Caller, request: &NetshareRequest<'_>) -> u64 {
        let mut bytes = vec![0; NETSHARE_MAX_REQUEST];
        let len = request.encode(&mut bytes).expect("encodes");
        self.call_bytes(caller, &mut bytes[..len])
    }

    /// Carry socket events and worker traffic until nothing changes.
    ///
    /// # Panics
    ///
    /// When it never settles.
    pub fn run(&mut self) {
        let mut still = 0;
        for _ in 0..10_000 {
            // As the service's loop does on every turn.
            let now = self.now;
            self.front.expire(now);
            let before = self.world().changes;
            let sockets = self.world().sockets.len();
            for at in 0..sockets {
                let world = self.world();
                let Some(entry) = world.sockets[at].as_ref().filter(|entry| !entry.held) else {
                    continue;
                };
                let conn = entry.conn;
                let armed = world
                    .armed
                    .iter()
                    .find(|(id, _)| *id == conn)
                    .is_some_and(|(_, arming)| arming.socket);
                if !armed {
                    continue;
                }
                let Some(event) = world.sockets[at]
                    .as_mut()
                    .and_then(|entry| entry.pending.pop_front())
                else {
                    continue;
                };
                world.changes += 1;
                self.deliver(at, conn, &event);
            }
            let armed: Vec<(ConnId, Arming)> = self.world().armed.clone();
            for (conn, arming) in armed {
                if arming.write {
                    self.front.worker_writable(self.now, conn);
                }
                if arming.read {
                    self.front.worker_readable(self.now, conn);
                }
            }
            if self.world().changes == before {
                still += 1;
                if still == 4 {
                    return;
                }
            } else {
                still = 0;
            }
        }
        panic!("the world never settled");
    }

    /// Hand the front `event`, from socket `at` of `conn`.
    fn deliver(&mut self, at: usize, conn: ConnId, event: &Pending) {
        let socket = SocketId::try_from(at).expect("few");
        let event = match event {
            Pending::Connected => SocketStreamEvent::Connected { socket },
            Pending::Data(payload) => SocketStreamEvent::Data { socket, payload },
            Pending::Closed => SocketStreamEvent::Closed {
                socket,
                reason: StreamCloseReason::Refused,
            },
        };
        self.front.socket(self.now, conn, &event);
    }

    /// Let through the first of what a held socket holds back, and carry
    /// what it causes: a server's answers arriving one at a time. Whether
    /// anything was held.
    pub fn let_one_through(&mut self) -> bool {
        let world = self.world();
        let Some((at, conn, event)) =
            world
                .sockets
                .iter_mut()
                .enumerate()
                .find_map(|(at, entry)| {
                    let entry = entry.as_mut().filter(|entry| entry.held)?;
                    Some((at, entry.conn, entry.pending.pop_front()?))
                })
        else {
            return false;
        };
        world.changes += 1;
        self.deliver(at, conn, &event);
        self.run();
        true
    }

    /// The reply to `ticket`, taken.
    ///
    /// # Panics
    ///
    /// When `ticket` has not been answered.
    pub fn reply(&mut self, ticket: u64) -> Vec<u8> {
        let world = self.world();
        let at = world
            .replies
            .iter()
            .position(|(answered, _)| *answered == ticket)
            .unwrap_or_else(|| panic!("ticket {ticket} unanswered"));
        world.replies.remove(at).1
    }

    /// Whether `ticket` has not been answered.
    pub fn unanswered(&mut self, ticket: u64) -> bool {
        !self
            .world()
            .replies
            .iter()
            .any(|(answered, _)| *answered == ticket)
    }

    /// The refusal `ticket` was answered with.
    ///
    /// # Panics
    ///
    /// When it was answered otherwise, or not at all.
    pub fn refusal(&mut self, ticket: u64) -> NetshareError {
        let reply = self.reply(ticket);
        match NetshareReply::decode(&reply).expect("a reply") {
            NetshareReply::Refused { error, .. } => error,
            other => panic!("not refused: {other:?}"),
        }
    }

    /// The mount `ticket` was answered with.
    ///
    /// # Panics
    ///
    /// When it was answered otherwise, or not at all.
    pub fn mounted(&mut self, ticket: u64) -> (Pin, OfferId) {
        let reply = self.reply(ticket);
        match NetshareReply::decode(&reply).expect("a reply") {
            NetshareReply::Mounted { pin, offer } => (pin, offer),
            other => panic!("not mounted: {other:?}"),
        }
    }

    /// Whether a record of `id` was written.
    #[must_use]
    pub fn logged(&self, id: EventId) -> bool {
        self.journal.logged(id)
    }

    /// Hand `request` to `offer`'s volume as a call, carry everything it
    /// causes until nothing moves, and answer its ticket.
    ///
    /// # Panics
    ///
    /// When `request` does not encode.
    pub fn serve(&mut self, offer: OfferId, request: &Request<'_>) -> u64 {
        let ticket = self.next_ticket;
        self.next_ticket += 1;
        let mut bytes = vec![0; FSSERVE_REQUEST_MAX];
        let len = encode_request(request, AllocationClass::Ordinary, &mut bytes).expect("encodes");
        bytes.truncate(len);
        let volume = self.world().volume(offer).expect("offered");
        volume.waiting.push_back((ticket, bytes));
        let now = self.now;
        self.front.served(now, offer);
        self.run();
        ticket
    }

    /// The answer `offer`'s volume gave the call `ticket`, taken.
    ///
    /// # Panics
    ///
    /// When it gave none.
    pub fn answer(&mut self, offer: OfferId, ticket: u64) -> Vec<u8> {
        let volume = self.world().volume(offer).expect("offered");
        let at = volume
            .answers
            .iter()
            .position(|(answered, _)| *answered == ticket)
            .unwrap_or_else(|| panic!("call {ticket} unanswered"));
        volume.answers.remove(at).1
    }

    /// Let `by` pass and act on what fell due.
    pub fn pass(&mut self, by: u64) {
        self.now += by;
        let now = self.now;
        self.world().tick(now);
        self.front.expire(now);
        self.run();
    }
}

/// A call's way to [`NAS`] as `alice`, with `password`.
#[must_use]
pub fn connect(password: &str) -> Connect<'_> {
    Connect {
        endpoint: Endpoint {
            host: "nas.example",
            address: Some(NAS),
            interface: None,
            port: SMB_PORT,
        },
        pin: None,
        floor: DialectFloor::Smb300,
        credentials: Credentials {
            user: "alice",
            domain: "WORKGROUP",
            password: Secret::new(password),
        },
    }
}

/// The password the scripted server's account has.
pub const PASSWORD: &str = "correct horse battery staple";

/// A listing over `connect`.
#[must_use]
pub fn shares(connect: Connect<'_>) -> NetshareRequest<'_> {
    NetshareRequest::Shares {
        connect,
        special: false,
    }
}

/// A mount of `share` over `connect`.
#[must_use]
pub fn mount<'a>(connect: Connect<'a>, share: &'a str) -> NetshareRequest<'a> {
    NetshareRequest::Mount {
        connect,
        share,
        read_only: false,
    }
}

/// A world with a scripted 3.1.1 server at [`NAS`].
#[must_use]
pub fn nas() -> World {
    World::serving(PeerConfig::new(Dialect::Smb311))
}
