//! The production transport and the doorbell park.
//!
//! [`RtDiscovery`] binds a process-private port for its session's doorbell
//! and parks on it through a wait-set, so a program waiting for answers
//! sleeps until the service rings — never a poll — and releases both with the
//! session. A doorbell is believed only
//! from the discovery service's kernel-attested account and only for this
//! session: the port is an inbox anyone holding its id may post to.
//!
//! This module is compiled only for a freestanding userland program that
//! opts into the `program` feature.

use alloc::vec::Vec;

use tairix_abi::discovery_ipc::{
    genuine_doorbell, Entry, Families, Query, DISCOVERY_ENDPOINT, DOORBELL_LEN,
};
use tairix_abi::waitset::WaitSourceKind;
use tairix_abi::{Errno, ORIGIN_WIRE_LEN};

use tairix_rt::{PrivatePort, WaitSet};

use crate::{DiscoveryError, Held, OwnedAnswer, Session, Transport, Waited};

/// Doorbells the private port queues: one per session is ever outstanding,
/// so this is room for that one and a forgery racing it.
const DOORBELL_CAPACITY: usize = 4;

/// The wait-set token of the doorbell port.
const DOORBELL_TOKEN: u64 = 1;

/// The discovery endpoint, called directly.
pub struct RtTransport;

impl Transport for RtTransport {
    fn call(&mut self, request: &[u8], reply: &mut [u8]) -> Result<usize, Errno> {
        tairix_rt::ipc_call(DISCOVERY_ENDPOINT, request, reply).map_err(Errno::from_syscall)
    }
}

/// A session with the doorbell it is woken by.
pub struct RtDiscovery {
    session: Session<RtTransport>,
    set: WaitSet,
    port: PrivatePort,
}

impl RtDiscovery {
    /// Bind the doorbell port, register it, and open a session ringing on
    /// it.
    ///
    /// # Errors
    ///
    /// [`DiscoveryError::Unavailable`] when no service is running, the
    /// service's refusal, or [`DiscoveryError::Transport`] when the port or
    /// wait-set cannot be made.
    pub fn open() -> Result<Self, DiscoveryError> {
        let port = PrivatePort::bind(DOORBELL_LEN, DOORBELL_CAPACITY)
            .map_err(DiscoveryError::Transport)?;
        let set = WaitSet::create().map_err(DiscoveryError::Transport)?;
        set.add(WaitSourceKind::Port, port.id(), DOORBELL_TOKEN)
            .map_err(DiscoveryError::Transport)?;
        Ok(Self {
            session: Session::open(RtTransport, port.id())?,
            set,
            port,
        })
    }

    /// The session, to start and stop requests in.
    pub fn session(&mut self) -> &mut Session<RtTransport> {
        &mut self.session
    }

    /// Park until the service rings this session or the monotonic instant
    /// `until` passes, then take everything waiting, handing each entry to
    /// `each`.
    ///
    /// # Errors
    ///
    /// The service's refusal of the collect, or a wait-set that failed.
    pub fn wait(
        &mut self,
        until: Option<u64>,
        each: &mut dyn FnMut(&Entry<'_>),
    ) -> Result<Waited, DiscoveryError> {
        loop {
            let rang = self
                .set
                .wait_until(until.unwrap_or(u64::MAX))
                .map_err(DiscoveryError::Transport)?;
            if rang.is_none() {
                return Ok(Waited::TimedOut);
            }
            if self.drain_doorbells() {
                while self.session.collect(each)? {}
                return Ok(Waited::Rung);
            }
        }
    }

    /// Drain the port, reporting whether the service rang this session.
    fn drain_doorbells(&mut self) -> bool {
        let mut rung = false;
        let mut frame = [0u8; DOORBELL_LEN];
        let mut sender = [0u8; ORIGIN_WIRE_LEN];
        while let Ok(len) = tairix_rt::ipc_recv(self.port.id(), &mut frame, &mut sender) {
            rung |= genuine_doorbell(&sender, &frame[..len]) == Some(self.session.id());
        }
        rung
    }
}

/// Everything one request holds by the time the first answer arrives or
/// `until` passes, whichever is sooner.
fn first_answers(query: &Query<'_>, until: u64) -> Result<Held, DiscoveryError> {
    let mut discovery = RtDiscovery::open()?;
    let request = discovery.session().start(query)?;
    let mut held = Held::default();
    while held.answers().is_empty() {
        let waited = discovery.wait(Some(until), &mut |entry| {
            if crate::request_of(entry) == request {
                held.apply(entry);
            }
        })?;
        if waited == Waited::TimedOut {
            break;
        }
    }
    Ok(held)
}

/// The addresses `name` — a host under `local`, in uncompressed wire form —
/// has on the link, as held once the first arrives or `until` (monotonic
/// nanoseconds) passes.
///
/// # Errors
///
/// As [`RtDiscovery::open`] and [`Session::start`].
pub fn host_addresses(
    name: &[u8],
    families: Families,
    until: u64,
) -> Result<Vec<core::net::IpAddr>, DiscoveryError> {
    let held = first_answers(&Query::Host { name, families }, until)?;
    let mut addresses: Vec<core::net::IpAddr> = Vec::new();
    for answer in held.answers() {
        if let OwnedAnswer::Address(address) = answer.answer {
            if !addresses.contains(&address) && addresses.try_reserve(1).is_ok() {
                addresses.push(address);
            }
        }
    }
    Ok(addresses)
}

/// The name link-local `address` has on the link, in uncompressed wire form,
/// as held once it arrives or `until` passes.
///
/// # Errors
///
/// As [`host_addresses`].
pub fn reverse_name(
    address: core::net::IpAddr,
    until: u64,
) -> Result<Option<Vec<u8>>, DiscoveryError> {
    let held = first_answers(&Query::Reverse { address }, until)?;
    Ok(held
        .answers()
        .iter()
        .find_map(|answer| match &answer.answer {
            OwnedAnswer::Pointer(target) => Some(target.clone()),
            _ => None,
        }))
}
