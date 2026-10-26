//! The per-link WS-Discovery state machine: probing while something browses,
//! the computers that answer, the resolves and metadata fetches that name
//! them, and their expiry once they stop answering.
//!
//! A computer is reported only once its metadata has been read, and only
//! from an address it sent from: a target is fetched at the address its
//! message came from, by the `XAddrs` URL naming that address or else one
//! naming a host — a name is never resolved — so a peer can steer a fetch at
//! nobody but itself. A computer heard at several addresses — over IPv4 and
//! IPv6 at once — keeps the one it was first heard at until that one falls
//! silent.
//!
//! Only an unasked announcement is held to its `AppSequence`. An answer to
//! this host's own recent probe or resolve is current however its target
//! numbers it, so a target that numbers every answer afresh is still heard.

use alloc::vec::Vec;

use tairix_abi::time::Duration64;
use tairix_hash::siphash::SipHash13;
use tairix_hash::HashSeed;
use tairix_inline::{ArrayVec, RingBuf};

use super::codec::{decode, decode_metadata, write_probe, write_resolve, Message, Target};
use super::{AppSequence, Computer, Endpoint, MetadataPath, Uuid, XAddr};
use crate::addr::IpAddr;
use crate::http::{Host, HostName};
use crate::timeutil::{from_nanos, nanos, ONE_SEC_NANOS};

/// The most targets tracked on one link.
///
/// A fixed bound against a segment of hostile peers, not a capacity: a
/// target past it is not tracked.
pub const MAX_TARGETS: usize = 128;

/// The most targets one sender may hold on one link, so a flooding peer
/// crowds out only itself.
pub const MAX_TARGETS_PER_SOURCE: usize = 4;

/// The most metadata fetches in flight on one link.
pub const MAX_FETCHES: usize = 2;

/// How many recent message ids are remembered, so a repeat is told from a
/// fresh message: SOAP-over-UDP sends each multicast message four times.
const SEEN: usize = 64;

/// How many recent probes and resolves this host sent are remembered.
const ASKED: usize = 16;

/// How long after a probe or resolve an answer to it is believed.
const ASKED_WINDOW: u128 = 10 * ONE_SEC_NANOS;

/// How long between probe rounds while something browses.
const PROBE_INTERVAL: u128 = 60 * ONE_SEC_NANOS;

/// How long a computer that has stopped answering is kept: two rounds and
/// the time an answer may take.
const EXPIRY: u128 = 150 * ONE_SEC_NANOS;

/// The repeats of each probe after its first sending (SOAP-over-UDP's
/// `MULTICAST_UDP_REPEAT` less one).
const REPEATS: u8 = 3;

/// The first repeat's delay is drawn from this range, then doubles up to
/// [`REPEAT_UPPER`] (SOAP-over-UDP's `UDP_MIN_DELAY`, `UDP_MAX_DELAY` and
/// `UDP_UPPER_DELAY`).
const REPEAT_MIN: u128 = 50 * MILLI;
const REPEAT_MAX: u128 = 250 * MILLI;
const REPEAT_UPPER: u128 = 500 * MILLI;

const MILLI: u128 = ONE_SEC_NANOS / 1000;

/// How long between resolves of one target, and how many it is given.
const RESOLVE_RETRY: u128 = 5 * ONE_SEC_NANOS;
const RESOLVE_ATTEMPTS: u8 = 3;

/// The delay before a failed fetch is tried again, doubling up to the most.
const FETCH_BACKOFF_FIRST: u128 = 30 * ONE_SEC_NANOS;
const FETCH_BACKOFF_MOST: u128 = 300 * ONE_SEC_NANOS;

/// What the engine asks its caller to do next.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Output<'a> {
    /// Send the [`Emit`] written into the caller's buffer.
    Transmit(Emit),
    /// Ask a target for its metadata, answering with
    /// [`WsdEngine::on_fetched`] or [`WsdEngine::on_fetch_failed`].
    Fetch(Fetch<'a>),
}

/// A datagram the engine wrote into the caller's buffer.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Emit {
    /// Octets written at the front of the buffer.
    pub len: usize,
    /// Where to send them.
    pub to: Destination,
}

/// Where a datagram the engine built must be sent.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Destination {
    /// The link's WS-Discovery group, on whichever families it has joined.
    Group,
}

/// One metadata fetch to make.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Fetch<'a> {
    /// The fetch's id, which its answer names.
    pub id: u32,
    /// The target's address: the one its message came from.
    pub addr: IpAddr,
    /// The port its URL names.
    pub port: u16,
    /// The path its URL names.
    pub path: &'a MetadataPath,
    /// The endpoint the request is addressed to.
    pub endpoint: &'a Endpoint,
    /// The name its URL gives its host, which the request's `Host` field
    /// names; `None` when the URL names `addr` itself.
    pub name: Option<&'a HostName>,
}

/// A change to what the link's computers are. A computer renamed, or now
/// answering elsewhere, is what it was reported as gone and what it is now
/// found, under one key.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Change {
    /// A computer was learned.
    Found {
        /// A keyed hash of its endpoint, the same for as long as the engine.
        key: u64,
        /// The address it answers at.
        addr: IpAddr,
        /// What its metadata says it is.
        computer: Computer,
    },
    /// A computer reported earlier is gone: it said goodbye, stopped
    /// answering, or is reported again as something else.
    Gone {
        /// The key it was reported under.
        key: u64,
        /// The address it was last reported at.
        addr: IpAddr,
        /// What it was last reported as.
        computer: Computer,
    },
}

/// Where one target is in being named, its instants in nanoseconds.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Step {
    /// It named no address it sent from: resolve it at `at`.
    Resolve { at: u128, attempts: u8 },
    /// Fetch its metadata at `at`.
    Fetch { at: u128, backoff: u128 },
    /// A fetch is in flight to `addr`, for metadata `version`.
    Fetching {
        id: u32,
        backoff: u128,
        addr: IpAddr,
        version: u32,
    },
    /// Its metadata has been read.
    Settled,
}

/// How a message about a target came.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Heard {
    /// Announced unasked, so its `AppSequence` decides whether it is stale.
    Announced,
    /// Answering this host's own recent probe or resolve.
    Answered,
}

/// One target the link has heard from.
#[derive(Clone, Debug)]
struct Tracked {
    key: u64,
    endpoint: Endpoint,
    /// The address it is reached at, and when that address last sent.
    source: IpAddr,
    source_heard: u128,
    /// The URL it is fetched by, at `source`.
    xaddr: Option<XAddr>,
    version: u32,
    sequence: Option<AppSequence>,
    /// When any of its addresses last sent.
    heard: u128,
    step: Step,
    reported: Option<Reported>,
}

/// What a target was last reported as, and where.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Reported {
    addr: IpAddr,
    computer: Computer,
}

impl Tracked {
    /// Whether it still says what it said when a fetch to `addr` for
    /// `version` was made, so what the fetch read describes it.
    fn unchanged_since(&self, addr: IpAddr, version: u32) -> bool {
        self.version == version && self.source == addr && self.xaddr.is_some()
    }
}

/// Where the probe rounds stand, its instants in nanoseconds.
#[derive(Copy, Clone, Debug)]
struct Probing {
    id: Uuid,
    sent: u8,
    next: u128,
    delay: u128,
}

/// The WS-Discovery engine of one link.
#[derive(Debug)]
pub struct WsdEngine {
    seed: HashSeed,
    probing: Option<Probing>,
    targets: Vec<Tracked>,
    seen: RingBuf<u64, SEEN>,
    asked: RingBuf<(Uuid, u128), ASKED>,
    in_flight: ArrayVec<u32, MAX_FETCHES>,
    next_fetch: u32,
}

impl WsdEngine {
    /// An idle engine, keying what it remembers with `seed`.
    #[must_use]
    pub fn new(seed: HashSeed) -> Self {
        Self {
            seed,
            probing: None,
            targets: Vec::new(),
            seen: RingBuf::new(),
            asked: RingBuf::new(),
            in_flight: ArrayVec::new(),
            next_fetch: 0,
        }
    }

    /// Whether something is browsing the link.
    #[must_use]
    pub const fn is_browsing(&self) -> bool {
        self.probing.is_some()
    }

    /// Start browsing: probe at once, and every round after.
    pub fn browse(&mut self, now: Duration64, rng: &mut dyn FnMut() -> u32) {
        if self.probing.is_none() {
            self.probing = Some(Probing::round(nanos(now), rng));
        }
    }

    /// Stop browsing and forget everything learned: nothing is heard, sent
    /// or fetched until browsing starts again, and an answer to a fetch made
    /// before is ignored.
    pub fn stop(&mut self) {
        self.probing = None;
        self.targets.clear();
        self.seen.clear();
        self.asked.clear();
        self.in_flight.clear();
    }

    /// Read a datagram `from` a sender the stack found on-link.
    pub fn on_datagram(
        &mut self,
        now: Duration64,
        datagram: &[u8],
        from: IpAddr,
        changes: &mut dyn FnMut(Change),
    ) {
        if self.probing.is_none() {
            return;
        }
        let Ok(envelope) = decode(datagram, self.seed) else {
            return;
        };
        let now = nanos(now);
        if self.seen.iter().any(|&id| id == envelope.message_id) {
            return;
        }
        self.seen.push_back_overwrite(envelope.message_id);
        let sequence = envelope.app_sequence;
        match envelope.message {
            Message::Hello(target) => self.learn(now, target, from, sequence, Heard::Announced),
            Message::ProbeMatches(targets) if self.asked_for(now, envelope.relates_to) => {
                for target in targets {
                    self.learn(now, target, from, sequence, Heard::Answered);
                }
            }
            Message::ResolveMatch(target) if self.asked_for(now, envelope.relates_to) => {
                self.learn(now, target, from, sequence, Heard::Answered);
            }
            Message::Bye { endpoint } => self.forget(&endpoint, sequence, changes),
            Message::ProbeMatches(_) | Message::ResolveMatch(_) | Message::Query => {}
        }
    }

    /// Whether `relates_to` names a probe or resolve this engine sent
    /// recently enough to believe an answer to it.
    fn asked_for(&self, now: u128, relates_to: Option<Uuid>) -> bool {
        relates_to.is_some_and(|id| {
            self.asked
                .iter()
                .any(|&(asked, at)| asked == id && now.saturating_sub(at) <= ASKED_WINDOW)
        })
    }

    fn position(&self, key: u64, endpoint: &Endpoint) -> Option<usize> {
        self.targets
            .iter()
            .position(|target| target.key == key && target.endpoint == *endpoint)
    }

    fn learn(
        &mut self,
        now: u128,
        target: Target,
        from: IpAddr,
        sequence: Option<AppSequence>,
        heard: Heard,
    ) {
        if !target.computer {
            return;
        }
        let key = SipHash13::hash_bytes(self.seed, target.endpoint.as_str().as_bytes());
        let xaddr = reachable_at(&target.xaddrs, from);
        let Some(index) = self.position(key, &target.endpoint) else {
            self.track(now, key, target, from, xaddr, sequence);
            return;
        };
        let tracked = &mut self.targets[index];
        if let Some(sequence) = sequence {
            let stale = heard == Heard::Announced
                && tracked.sequence.is_some_and(|last| !sequence.follows(last));
            if stale {
                return;
            }
            tracked.sequence = Some(sequence);
        }
        tracked.heard = now;
        // Another address takes over only once the one it is reached at
        // falls silent, so a computer on both families stays put.
        let from_source = tracked.source == from;
        let moved = if from_source {
            xaddr.is_some() && tracked.xaddr != xaddr
        } else {
            now.saturating_sub(tracked.source_heard) > EXPIRY
        };
        let changed = tracked.version != target.metadata_version;
        if from_source || moved {
            tracked.source_heard = now;
        }
        if moved {
            tracked.source = from;
            tracked.xaddr = xaddr;
        }
        tracked.version = target.metadata_version;
        if (moved || changed) && !matches!(tracked.step, Step::Fetching { .. }) {
            tracked.step = first_step(now, tracked.xaddr.as_ref());
        }
    }

    fn track(
        &mut self,
        now: u128,
        key: u64,
        target: Target,
        from: IpAddr,
        xaddr: Option<XAddr>,
        sequence: Option<AppSequence>,
    ) {
        let from_source = self.targets.iter().filter(|t| t.source == from).count();
        if self.targets.len() >= MAX_TARGETS
            || from_source >= MAX_TARGETS_PER_SOURCE
            || self.targets.try_reserve(1).is_err()
        {
            return;
        }
        let step = first_step(now, xaddr.as_ref());
        self.targets.push(Tracked {
            key,
            endpoint: target.endpoint,
            source: from,
            xaddr,
            version: target.metadata_version,
            sequence,
            heard: now,
            source_heard: now,
            step,
            reported: None,
        });
    }

    fn forget(
        &mut self,
        endpoint: &Endpoint,
        sequence: Option<AppSequence>,
        changes: &mut dyn FnMut(Change),
    ) {
        let key = SipHash13::hash_bytes(self.seed, endpoint.as_str().as_bytes());
        let Some(index) = self.position(key, endpoint) else {
            return;
        };
        let stale = sequence
            .zip(self.targets[index].sequence)
            .is_some_and(|(seq, last)| !seq.follows(last));
        if stale {
            return;
        }
        report_gone(self.targets.swap_remove(index), changes);
    }

    /// Advance to `now`: expire computers that stopped answering, and return
    /// the next thing to send or fetch, a datagram written into `out`.
    ///
    /// A datagram is built only once `affordable` agrees to pay for it, so
    /// work the caller's send budget cannot cover stays due rather than being
    /// counted sent and dropped.
    pub fn poll(
        &mut self,
        now: Duration64,
        rng: &mut dyn FnMut() -> u32,
        out: &mut [u8],
        changes: &mut dyn FnMut(Change),
        affordable: &mut dyn FnMut() -> bool,
    ) -> Option<Output<'_>> {
        let probing = self.probing?;
        let now = nanos(now);
        let mut index = 0;
        while index < self.targets.len() {
            let target = &self.targets[index];
            let expired = now.saturating_sub(target.heard) > EXPIRY;
            let unnamed = matches!(target.step,
                Step::Resolve { at, attempts } if at <= now && attempts >= RESOLVE_ATTEMPTS);
            if expired || unnamed {
                report_gone(self.targets.swap_remove(index), changes);
            } else {
                index += 1;
            }
        }
        let resolve_due = self
            .targets
            .iter()
            .any(|t| matches!(t.step, Step::Resolve { at, .. } if at <= now));
        if (probing.next <= now || resolve_due) && affordable() {
            if probing.next <= now {
                self.probing = Some(probing.after(now, rng));
                self.asked.push_back_overwrite((probing.id, now));
                let len = write_probe(out, probing.id).ok()?;
                return Some(Output::Transmit(Emit {
                    len,
                    to: Destination::Group,
                }));
            }
            if let Some(emit) = self.resolve_due(now, rng, out) {
                return Some(Output::Transmit(emit));
            }
        }
        self.fetch_due(now).map(Output::Fetch)
    }

    /// The computers named so far, as `(key, address, computer)`.
    pub fn computers(&self) -> impl Iterator<Item = (u64, IpAddr, &Computer)> + '_ {
        self.targets.iter().filter_map(|target| {
            let reported = target.reported.as_ref()?;
            Some((target.key, reported.addr, &reported.computer))
        })
    }

    fn resolve_due(
        &mut self,
        now: u128,
        rng: &mut dyn FnMut() -> u32,
        out: &mut [u8],
    ) -> Option<Emit> {
        let target = self
            .targets
            .iter_mut()
            .find(|t| matches!(t.step, Step::Resolve { at, .. } if at <= now))?;
        let Step::Resolve { attempts, .. } = target.step else {
            return None;
        };
        let id = Uuid::draw(rng);
        target.step = Step::Resolve {
            at: now.saturating_add(RESOLVE_RETRY),
            attempts: attempts + 1,
        };
        let len = write_resolve(out, id, &target.endpoint).ok()?;
        self.asked.push_back_overwrite((id, now));
        Some(Emit {
            len,
            to: Destination::Group,
        })
    }

    fn fetch_due(&mut self, now: u128) -> Option<Fetch<'_>> {
        let index = self.targets.iter().position(|t| {
            t.xaddr.is_some() && matches!(t.step, Step::Fetch { at, .. } if at <= now)
        })?;
        let target = &self.targets[index];
        let Step::Fetch { backoff, .. } = target.step else {
            return None;
        };
        let (addr, version) = (target.source, target.version);
        let id = self.next_fetch;
        self.in_flight.try_push(id).ok()?;
        self.next_fetch = id.wrapping_add(1);
        let target = &mut self.targets[index];
        target.step = Step::Fetching {
            id,
            backoff,
            addr,
            version,
        };
        let xaddr = target.xaddr.as_ref()?;
        Some(Fetch {
            id,
            addr,
            port: xaddr.port,
            path: &xaddr.path,
            endpoint: &target.endpoint,
            name: match &xaddr.host {
                Host::Name(name) => Some(name),
                Host::Addr(_) => None,
            },
        })
    }

    /// Take the whole response a fetch's target sent before closing.
    pub fn on_fetched(
        &mut self,
        now: Duration64,
        id: u32,
        response: &[u8],
        changes: &mut dyn FnMut(Change),
    ) {
        let now = nanos(now);
        let Some(target) = self.landed(id) else {
            return;
        };
        let Step::Fetching {
            backoff,
            addr,
            version,
            ..
        } = target.step
        else {
            return;
        };
        if !target.unchanged_since(addr, version) {
            // It moved or changed while the fetch flew: what it read is
            // stale, so ask again.
            target.step = first_step(now, target.xaddr.as_ref());
            return;
        }
        let Ok(computer) = decode_metadata(response) else {
            target.step = retry(now, backoff);
            return;
        };
        target.step = Step::Settled;
        let key = target.key;
        let now_reported = Reported { addr, computer };
        match target.reported.replace(now_reported.clone()) {
            None => changes(Change::Found {
                key,
                addr,
                computer: now_reported.computer,
            }),
            Some(was) if was != now_reported => {
                changes(Change::Gone {
                    key,
                    addr: was.addr,
                    computer: was.computer,
                });
                changes(Change::Found {
                    key,
                    addr,
                    computer: now_reported.computer,
                });
            }
            Some(_) => {}
        }
    }

    /// Take a fetch that could not complete.
    pub fn on_fetch_failed(&mut self, now: Duration64, id: u32) {
        let now = nanos(now);
        if let Some(target) = self.landed(id) {
            if let Step::Fetching {
                backoff,
                addr,
                version,
                ..
            } = target.step
            {
                target.step = if target.unchanged_since(addr, version) {
                    retry(now, backoff)
                } else {
                    first_step(now, target.xaddr.as_ref())
                };
            }
        }
    }

    /// The target a fetch landed for, once the fetch is no longer in flight;
    /// `None` for a fetch this engine is not waiting on.
    fn landed(&mut self, id: u32) -> Option<&mut Tracked> {
        let slot = self.in_flight.iter().position(|&flying| flying == id)?;
        self.in_flight.swap_remove(slot);
        self.targets
            .iter_mut()
            .find(|t| matches!(t.step, Step::Fetching { id: flying, .. } if flying == id))
    }

    /// When the engine next needs [`poll`](Self::poll): the next probe or
    /// resolve once the send budget allows at `sendable`, the next fetch due,
    /// or the next computer to expire. A `sendable` of `None` says the budget
    /// never allows, so sending is never due.
    #[must_use]
    pub fn next_deadline(&self, sendable: Option<Duration64>) -> Option<Duration64> {
        let probing = self.probing?;
        let sendable = sendable.map(nanos);
        let send_at = |at: u128| sendable.map(|ready| at.max(ready));
        let fetch_room = !self.in_flight.is_full();
        let earliest = self
            .targets
            .iter()
            .map(|target| {
                // Expiry is strictly past `EXPIRY`, so the poll at this
                // instant finds the computer gone.
                let expires = target.heard.saturating_add(EXPIRY + 1);
                match target.step {
                    Step::Resolve { at, .. } => send_at(at).map_or(expires, |at| at.min(expires)),
                    Step::Fetch { at, .. } if fetch_room => at.min(expires),
                    Step::Fetch { .. } | Step::Fetching { .. } | Step::Settled => expires,
                }
            })
            .chain(send_at(probing.next))
            .min();
        earliest.map(from_nanos)
    }
}

impl Probing {
    /// A fresh round starting at `now`.
    fn round(now: u128, rng: &mut dyn FnMut() -> u32) -> Self {
        let jitter = u128::from(rng()) % (REPEAT_MAX - REPEAT_MIN + 1);
        Self {
            id: Uuid::draw(rng),
            sent: 0,
            next: now,
            delay: REPEAT_MIN + jitter,
        }
    }

    /// Where the rounds stand once this sending has gone at `now`.
    fn after(self, now: u128, rng: &mut dyn FnMut() -> u32) -> Self {
        if self.sent < REPEATS {
            Self {
                sent: self.sent + 1,
                next: now.saturating_add(self.delay),
                delay: self.delay.saturating_add(self.delay).min(REPEAT_UPPER),
                ..self
            }
        } else {
            Self::round(now.saturating_add(PROBE_INTERVAL), rng)
        }
    }
}

/// Tell what a target leaving takes with it: nothing, unless it was reported.
fn report_gone(gone: Tracked, changes: &mut dyn FnMut(Change)) {
    if let Some(reported) = gone.reported {
        changes(Change::Gone {
            key: gone.key,
            addr: reported.addr,
            computer: reported.computer,
        });
    }
}

/// The URL a target sending from `from` is fetched by: the first naming that
/// address, else the first naming a host by name, which is fetched at that
/// address. A URL naming another address is never used.
fn reachable_at(xaddrs: &[XAddr], from: IpAddr) -> Option<XAddr> {
    let sender = Host::Addr(from);
    xaddrs
        .iter()
        .find(|url| url.host == sender)
        .or_else(|| xaddrs.iter().find(|url| matches!(url.host, Host::Name(_))))
        .cloned()
}

/// How a newly heard or changed target is named: fetched at the address it
/// sent from, or resolved when it named none.
fn first_step(now: u128, xaddr: Option<&XAddr>) -> Step {
    match xaddr {
        Some(_) => Step::Fetch {
            at: now,
            backoff: FETCH_BACKOFF_FIRST,
        },
        None => Step::Resolve {
            at: now,
            attempts: 0,
        },
    }
}

/// The step after a failed fetch: the same fetch after `backoff`, which then
/// doubles.
fn retry(now: u128, backoff: u128) -> Step {
    Step::Fetch {
        at: now.saturating_add(backoff),
        backoff: backoff.saturating_add(backoff).min(FETCH_BACKOFF_MOST),
    }
}
