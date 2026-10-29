//! What the front needs of the world: workers, sockets, the resolver, the
//! kernel's offers, and the endpoint's replies.
//!
//! The production [`Host`] is the kernel, reached from the `Run` binary; host
//! tests answer it from memory, with in-process workers and the scripted SMB
//! server standing in for a socket's far end.

use core::net::IpAddr;

use tairix_abi::fsserve::Span;
use tairix_abi::net::{SocketAddr, SocketId};
use tairix_abi::net_ipc::{NetAddrFamily, IF_NAME_LEN};
use tairix_abi::netattach::{NetworkOffer, OfferId};
use tairix_abi::time::Time64;
use tairix_abi::{Errno, ProcId};
use tairix_collections::{Key, SlotKey};
use tairix_sandbox::session::{SessionDescriptors, SessionTransport};

/// One connection's name, as the front's connection table issued it: a slot
/// reused for another connection is never mistaken for the one before.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct ConnId(SlotKey);

impl ConnId {
    /// Its slot, dense from zero, for a table kept beside the front's.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0.index()
    }
}

impl Key for ConnId {
    fn from_slot(slot: SlotKey) -> Self {
        Self(slot)
    }

    fn slot(self) -> SlotKey {
        self.0
    }
}

/// What a connection waits on: events on its socket, the worker's reports,
/// room toward the worker.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Arming {
    /// Whether the connection takes its socket's next event.
    pub socket: bool,
    /// Its worker's descriptors, while it has a worker.
    pub worker: Option<SessionDescriptors>,
    /// Whether it reads the worker's reports.
    pub read: bool,
    /// Whether it waits for room toward the worker.
    pub write: bool,
}

/// What the front needs of the world.
pub trait Host {
    /// The transport to one launched worker.
    type Worker: SessionTransport;

    /// Start a connection's worker.
    ///
    /// # Errors
    ///
    /// Why it could not be started.
    fn launch(&mut self) -> Result<Self::Worker, Errno>;

    /// Open `conn`'s stream socket for `family`, its events delivered to
    /// `conn` alone, so one connection's are held back without another's.
    ///
    /// # Errors
    ///
    /// The stack's refusal.
    fn open(&mut self, conn: ConnId, family: NetAddrFamily) -> Result<SocketId, Errno>;

    /// Begin connecting `socket` to `to`, on `interface` when one is named.
    ///
    /// # Errors
    ///
    /// The stack's refusal.
    fn connect(
        &mut self,
        socket: SocketId,
        to: SocketAddr,
        interface: Option<[u8; IF_NAME_LEN]>,
    ) -> Result<(), Errno>;

    /// Send `bytes` on `socket`, answering how many the stack took.
    ///
    /// # Errors
    ///
    /// The stack's refusal.
    fn send(&mut self, socket: SocketId, bytes: &[u8]) -> Result<usize, Errno>;

    /// Close `conn`'s socket, and the delivery its events came by.
    fn close(&mut self, conn: ConnId, socket: SocketId);

    /// Wait on what `arming` names for `conn`, and on nothing else of it.
    ///
    /// # Errors
    ///
    /// The wait-set's refusal, after which `conn` would never be woken.
    fn arm(&mut self, conn: ConnId, arming: Arming) -> Result<(), Errno>;

    /// Resolve `host` for `conn`, on `interface` when one is named, off the
    /// loop: answered to the front's `resolved` for `conn`.
    ///
    /// # Errors
    ///
    /// Why the lookup could not be asked.
    fn resolve(
        &mut self,
        conn: ConnId,
        host: &str,
        interface: Option<[u8; IF_NAME_LEN]>,
    ) -> Result<(), Errno>;

    /// Fill `out` from the CSPRNG.
    ///
    /// # Errors
    ///
    /// The kernel's refusal.
    fn fill_random(&mut self, out: &mut [u8]) -> Result<(), Errno>;

    /// The wall clock, for a server that sends no time of its own.
    fn wall_time(&mut self) -> Time64;

    /// Answer the call `ticket` names with `reply`.
    fn reply(&mut self, ticket: u64, reply: &[u8]);

    /// Offer a volume for its principal to attach.
    ///
    /// # Errors
    ///
    /// The kernel's refusal: [`Errno::AlreadyExists`] for a volume of the
    /// same identity offered already.
    fn offer(&mut self, offer: &NetworkOffer<'_>) -> Result<OfferId, Errno>;

    /// Retire an offer, and its attachment if it has one.
    fn withdraw(&mut self, offer: OfferId);

    /// Learn when the process `anchor` ends.
    ///
    /// # Errors
    ///
    /// [`Errno::NotFound`] when it has already ended.
    fn watch(&mut self, anchor: ProcId) -> Result<(), Errno>;

    /// Stop watching `anchor`.
    fn unwatch(&mut self, anchor: ProcId);

    /// Take the next call waiting on `offer`'s volume into `into`: its ticket
    /// and length, or `None` when none waits.
    ///
    /// # Errors
    ///
    /// The kernel's refusal: the volume's endpoint is gone.
    fn served_take(
        &mut self,
        offer: OfferId,
        into: &mut [u8],
    ) -> Result<Option<(u64, usize)>, Errno>;

    /// Answer the volume's call `ticket` with `reply`.
    fn served_reply(&mut self, offer: OfferId, ticket: u64, reply: &[u8]);

    /// The bytes of `span` in the window `offer`'s volume moves file data
    /// through: the call in service's own span alone, since the kernel stages
    /// and collects the other lanes' spans meanwhile.
    fn window(&mut self, offer: OfferId, span: Span) -> Option<&mut [u8]>;

    /// Tell the kernel `offer`'s server is working on the calls it holds.
    fn alive(&mut self, offer: OfferId);
}

/// Why a host's name resolved to no address the service may connect to.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Unresolved {
    /// The name names no host.
    NoSuchHost,
    /// No answer could be had.
    Unanswered,
}

/// A resolver's answer: the address, or why there is none.
pub type Resolution = Result<IpAddr, Unresolved>;
