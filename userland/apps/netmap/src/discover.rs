//! Advertised services and link-local names: the discovery-v1 requests the
//! map keeps open, and the view of them it draws.
//!
//! A [`Discovery`] enumerates the service types the segment offers, browses
//! each, resolves each instance it finds, looks up the host each resolves to,
//! and asks for the names of the link-local addresses the map shows — every
//! step bounded, and every answer kept current from the service's incremental
//! entries, so a map left open costs what changes rather than what is held.
//!
//! The requests are reconciled against what is held as a whole, once per
//! collect pass: a type that appears is browsed, one that goes takes its
//! browse with it, and so on down, so the open set is always exactly what the
//! answers call for. Nothing a peer authored is display-safe, so every label
//! and `TXT` string reaches the view as a [`DisplayLabel`] and every name as a
//! [`WireName`].
//!
//! A refusal bears on what it refused. An ask the service will not answer,
//! or this program may not make, is not made again while it is wanted; one
//! refused for the service's bounds, or failing for want of the service, ends
//! the pass, and the next asks again. A request whose replacement is lost
//! again is held back by time, from the read's pace doubling, so one that
//! cannot be replayed whole is retried ever more rarely, however fast its
//! losses arrive.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::net::IpAddr;

use core::sync::atomic::{AtomicU64, Ordering};
use tairix_abi::discovery_ipc::{
    Entry, Families, Query, ServiceTypeField, Transport, REQUESTS_PER_ACCOUNT,
};

use tairix_abi::discovery_ipc::genuine_doorbell;
use tairix_abi::Errno;
use tairix_discovery::{request_of, DiscoveryError, Held, OwnedAnswer, Session};
use tairix_net::dns::{DisplayLabel, Name, Presentation, WireName};
use tairix_net::mdns::{is_link_local_address, is_link_local_name, TxtStrings};

use crate::model::InterfaceName;

/// The most service types browsed at once.
pub const MAX_TYPES: usize = 64;
/// The most instances resolved at once.
pub const MAX_SERVICES: usize = 256;
/// The most hosts looked up at once.
pub const MAX_HOSTS: usize = 128;
/// The most link-local addresses named at once.
pub const MAX_REVERSES: usize = 128;

// The map alone never meets the bound its account shares with its other
// programs, even while a lost request is replaced before the old one stops.
const _: () =
    assert!(1 + MAX_TYPES + MAX_SERVICES + MAX_HOSTS + MAX_REVERSES < REQUESTS_PER_ACCOUNT);

/// The requests a [`Discovery`] keeps open with the discovery service.
pub trait Directory {
    /// Start `query`, answering its request id.
    ///
    /// # Errors
    ///
    /// The service's refusal, or the call's failure.
    fn start(&mut self, query: &Query<'_>) -> Result<u32, DiscoveryError>;

    /// Stop request `request`.
    ///
    /// # Errors
    ///
    /// The service's refusal, or the call's failure.
    fn stop(&mut self, request: u32) -> Result<(), DiscoveryError>;
}

impl<T: tairix_discovery::Transport> Directory for Session<T> {
    fn start(&mut self, query: &Query<'_>) -> Result<u32, DiscoveryError> {
        Session::start(self, query)
    }

    fn stop(&mut self, request: u32) -> Result<(), DiscoveryError> {
        Session::stop(self, request)
    }
}

/// One advertised service instance, as the map draws it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdvertisedService {
    /// The instance's own label.
    pub instance: String,
    /// Its service type, as `_ipp._tcp`.
    pub service: String,
    /// The interface it was learned on.
    pub interface: InterfaceName,
    /// The port it is reached on.
    pub port: u16,
    /// The host it is reached at.
    pub host: String,
    /// The host's addresses, once looked up.
    pub addresses: Vec<IpAddr>,
    /// Each `TXT` string it publishes.
    pub text: Vec<String>,
}

/// What one open request asks.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Ask {
    Types,
    Browse {
        name: Vec<u8>,
        transport: Transport,
    },
    Resolve {
        instance: Vec<u8>,
        name: Vec<u8>,
        transport: Transport,
    },
    /// A host, by its name in wire form.
    Host(Vec<u8>),
    Reverse(IpAddr),
}

impl Ask {
    fn query(&self) -> Query<'_> {
        match self {
            Self::Types => Query::Types,
            Self::Browse { name, transport } => Query::Browse {
                service: ServiceTypeField {
                    name,
                    transport: *transport,
                },
            },
            Self::Resolve {
                instance,
                name,
                transport,
            } => Query::Resolve {
                instance,
                service: ServiceTypeField {
                    name,
                    transport: *transport,
                },
            },
            Self::Host(name) => Query::Host {
                name,
                families: Families::BOTH,
            },
            Self::Reverse(address) => Query::Reverse { address: *address },
        }
    }

    /// The bound on how many of this kind are open at once.
    const fn bound(&self) -> usize {
        match self {
            Self::Types => 1,
            Self::Browse { .. } => MAX_TYPES,
            Self::Resolve { .. } => MAX_SERVICES,
            Self::Host(_) => MAX_HOSTS,
            Self::Reverse(_) => MAX_REVERSES,
        }
    }

    const fn rank(&self) -> u8 {
        match self {
            Self::Types => 0,
            Self::Browse { .. } => 1,
            Self::Resolve { .. } => 2,
            Self::Host(_) => 3,
            Self::Reverse(_) => 4,
        }
    }
}

/// The discovery session the map holds, published by the thread that opens
/// it to the loop that drains its doorbell.
#[derive(Debug)]
pub struct OpenSession(AtomicU64);

/// Outside the id space, so every session id, 0 included, is a session.
const NO_SESSION: u64 = u64::MAX;

impl Default for OpenSession {
    fn default() -> Self {
        Self(AtomicU64::new(NO_SESSION))
    }
}

impl OpenSession {
    /// The map now holds session `id`.
    pub fn open(&self, id: u32) {
        self.0.store(u64::from(id), Ordering::Relaxed);
    }

    /// The map holds no session.
    pub fn close(&self) {
        self.0.store(NO_SESSION, Ordering::Relaxed);
    }

    /// Whether `frame`, posted to the doorbell by kernel-attested `sender`,
    /// is the service ringing the open session.
    #[must_use]
    pub fn rung_by(&self, sender: &[u8], frame: &[u8]) -> bool {
        genuine_doorbell(sender, frame)
            .is_some_and(|rung| u64::from(rung) == self.0.load(Ordering::Relaxed))
    }
}

/// Where the map's discovery collects stand.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Collect {
    /// The map asks the service nothing: it has no doorbell, no thread to
    /// collect on, or the service refused it a session.
    #[default]
    Off,
    /// Nothing has moved since the last collect.
    Idle,
    /// The service rang, or the addresses the map shows moved.
    Due,
    /// A collect is out; `again` once something moved since it was asked.
    Asked {
        /// Something moved since the collect was asked.
        again: bool,
    },
    /// The last collect failed — the service had not started, or lost the
    /// session — so the next read asks again.
    Retry,
}

impl Collect {
    /// Where collects start: due once the map has a doorbell and its reader a
    /// thread of its own, since a collect makes many blocking calls the event
    /// loop must never make.
    #[must_use]
    pub const fn start(doorbell: bool, reader_thread: bool) -> Self {
        if doorbell && reader_thread {
            Self::Due
        } else {
            Self::Off
        }
    }

    /// Something moved that a collect should see.
    pub fn moved(&mut self) {
        *self = match *self {
            Self::Idle | Self::Due | Self::Retry => Self::Due,
            Self::Asked { .. } => Self::Asked { again: true },
            Self::Off => Self::Off,
        };
    }

    /// The outstanding collect answered: whether its pass settled, or why it
    /// failed.
    pub fn answered(&mut self, result: Result<bool, &DiscoveryError>) {
        // A session refused outright is not asked for again, as an ask
        // refused outright is not.
        if matches!(
            result,
            Err(DiscoveryError::Refused(Errno::PermissionDenied))
        ) {
            *self = Self::Off;
            return;
        }
        // A pass that left work is asked again at the next read, not only
        // when the service next rings.
        let settled = result.unwrap_or(false);
        if let Self::Asked { again } = *self {
            *self = match (again, settled) {
                (true, _) => Self::Due,
                (false, true) => Self::Idle,
                (false, false) => Self::Retry,
            };
        }
    }

    /// A read landed: a failed collect is asked again.
    pub fn read_landed(&mut self) {
        if *self == Self::Retry {
            *self = Self::Due;
        }
    }
}

/// The discovery-v1 requests the map keeps open and what each holds.
#[derive(Debug, Default)]
pub struct Discovery {
    /// Each open request, by its id.
    held: BTreeMap<u32, (Ask, Held)>,
    /// Each open request's id, by what it asks.
    open: BTreeMap<Ask, u32>,
    /// A bound turned an ask away in the last pass.
    truncated: bool,
    /// The service refused an ask this program may not make.
    refused: Option<DiscoveryError>,
    /// What ended the last pass early: the next pass asks again.
    stalled: Option<DiscoveryError>,
    /// Asks the service refused for themselves, not made again while wanted.
    unanswerable: BTreeSet<Ask>,
    /// When each lost request was last replaced, and how often in a row.
    replaced: BTreeMap<Ask, Backoff>,
    /// A lost request was held back in the last pass.
    deferred: bool,
}

/// A replaced request's wait before it may be replaced again.
#[derive(Copy, Clone, Debug)]
struct Backoff {
    /// When it was last replaced.
    at: u64,
    /// Replacements in a row that were lost again within their wait.
    streak: u32,
}

/// The longest wait doubles the read's pace this many times.
const MAX_BACKOFF_DOUBLINGS: u32 = 6;

impl Backoff {
    /// When the request may next be replaced.
    fn until(self) -> u64 {
        self.at
            .saturating_add(crate::REFRESH_NS << self.streak.min(MAX_BACKOFF_DOUBLINGS))
    }
}

impl Discovery {
    /// Nothing open yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold one entry into the request it is for; an entry for a request no
    /// longer open is dropped.
    pub fn apply(&mut self, entry: &Entry<'_>) {
        if let Some((_, held)) = self.held.get_mut(&request_of(entry)) {
            held.apply(entry);
        }
    }

    /// Bring the open requests in line with what is held at `now`: the
    /// segment's types, a browse per type, a resolve per instance, a lookup
    /// per host, and a reverse lookup for each link-local address in `named`.
    /// A request whose updates were dropped is started again, which answers
    /// with what is held now, once its backoff allows.
    pub fn reconcile(&mut self, directory: &mut dyn Directory, named: &BTreeSet<IpAddr>, now: u64) {
        let wanted = self.wanted(named);
        self.truncated = false;
        self.stalled = None;
        self.deferred = false;
        self.unanswerable.retain(|ask| wanted.contains(ask));
        let stale: Vec<(Ask, u32)> = self
            .open
            .iter()
            .filter(|(ask, _)| !wanted.contains(*ask))
            .map(|(ask, &request)| (ask.clone(), request))
            .collect();
        for (ask, request) in stale {
            self.open.remove(&ask);
            self.held.remove(&request);
            // The service ends a request its session no longer names when it
            // closes; a refused stop leaves nothing this map acts on.
            let _ = directory.stop(request);
        }
        let lost: Vec<(Ask, u32)> = self
            .open
            .iter()
            .filter(|(_, request)| {
                self.held
                    .get(request)
                    .is_some_and(|(_, held)| held.is_lost())
            })
            .map(|(ask, &request)| (ask.clone(), request))
            .collect();
        let (open, held) = (&self.open, &self.held);
        // A replacement that outlived its wait has proved it fits: the next
        // loss starts the backoff afresh.
        self.replaced.retain(|ask, backoff| {
            open.get(ask).is_some_and(|request| {
                now < backoff.until() || held.get(request).is_some_and(|(_, held)| held.is_lost())
            })
        });
        for (ask, old) in lost {
            if self.stalled.is_some() {
                break;
            }
            let backoff = self.replaced.get(&ask).copied();
            if let Some(backoff) = backoff.filter(|backoff| now < backoff.until()) {
                self.deferred = true;
                // A request still lost at the longest wait is not being kept
                // whole, so the map is short of it.
                self.truncated |= backoff.streak >= MAX_BACKOFF_DOUBLINGS;
                continue;
            }
            // Started before the old one stops, so the question they share
            // stays asked and the service replays what it holds rather than
            // asking the segment again.
            let started = match directory.start(&ask.query()) {
                // At the account's bound the replacement has no room until
                // the old request goes, so it goes first.
                Err(DiscoveryError::Refused(Errno::LimitExceeded)) => {
                    let _ = directory.stop(old);
                    self.held.remove(&old);
                    self.open.remove(&ask);
                    directory.start(&ask.query())
                }
                started => started,
            };
            match started {
                Ok(request) => {
                    if self.open.remove(&ask).is_some() {
                        let _ = directory.stop(old);
                        self.held.remove(&old);
                    }
                    self.held.insert(request, (ask.clone(), Held::default()));
                    let streak = backoff.map_or(0, |backoff| backoff.streak.saturating_add(1));
                    self.replaced
                        .insert(ask.clone(), Backoff { at: now, streak });
                    self.open.insert(ask, request);
                }
                Err(refusal) => self.refuse(ask, refusal),
            }
        }
        let mut counts = [0usize; 5];
        for ask in self.open.keys() {
            counts[usize::from(ask.rank())] += 1;
        }
        for ask in wanted {
            if self.stalled.is_some() {
                break;
            }
            if self.open.contains_key(&ask) || self.unanswerable.contains(&ask) {
                continue;
            }
            let count = &mut counts[usize::from(ask.rank())];
            if *count >= ask.bound() {
                self.truncated = true;
                continue;
            }
            match directory.start(&ask.query()) {
                Ok(request) => {
                    *count += 1;
                    self.held.insert(request, (ask.clone(), Held::default()));
                    self.open.insert(ask, request);
                }
                Err(refusal) => self.refuse(ask, refusal),
            }
        }
    }

    /// Record why `ask` was refused, as it bears on what is asked next.
    fn refuse(&mut self, ask: Ask, refusal: DiscoveryError) {
        match refusal {
            // Authority does not change while the program runs, so asking
            // again would only be refused, and audited, again.
            DiscoveryError::Refused(Errno::PermissionDenied) => {
                self.unanswerable.insert(ask);
                self.refused = Some(refusal);
            }
            DiscoveryError::Refused(Errno::OutOfRange) => {
                self.unanswerable.insert(ask);
            }
            _ => self.stalled = Some(refusal),
        }
    }

    /// Every request the held answers call for: a walk down from the type
    /// list through what each wanted request holds, so a request whose parent
    /// is no longer wanted is not wanted either, however deep it sits.
    fn wanted(&self, named: &BTreeSet<IpAddr>) -> BTreeSet<Ask> {
        let mut wanted = BTreeSet::new();
        let mut frontier = Vec::from([Ask::Types]);
        while let Some(ask) = frontier.pop() {
            if wanted.contains(&ask) {
                continue;
            }
            let held = self
                .open
                .get(&ask)
                .and_then(|request| self.held.get(request))
                .map(|(_, held)| held);
            wanted.insert(ask.clone());
            for answer in held.into_iter().flat_map(Held::answers) {
                frontier.extend(child_of(&ask, &answer.answer));
            }
        }
        wanted.extend(
            named
                .iter()
                .filter(|address| is_link_local_address(**address))
                .map(|address| Ask::Reverse(*address)),
        );
        wanted
    }

    /// The services resolved so far, each with its host's addresses once they
    /// are known.
    #[must_use]
    pub fn services(&self) -> Vec<AdvertisedService> {
        let addresses = self.host_addresses();
        let mut services = Vec::new();
        for (ask, held) in self.held.values() {
            let Ask::Resolve {
                instance,
                name,
                transport,
            } = ask
            else {
                continue;
            };
            let Some((interface, port, target)) = preferred_target(held) else {
                continue;
            };
            services.push(AdvertisedService {
                instance: format!("{}", DisplayLabel(instance)),
                service: service_type(name, *transport),
                interface,
                port,
                host: format!("{}", WireName(target)),
                addresses: addresses
                    .get(target.as_slice())
                    .cloned()
                    .unwrap_or_default(),
                text: held
                    .answers()
                    .iter()
                    .filter_map(|answer| match &answer.answer {
                        OwnedAnswer::Text(octets) => Some(octets),
                        _ => None,
                    })
                    .flat_map(|octets| TxtStrings::over(octets))
                    .filter(|string| !string.is_empty())
                    .map(|string| format!("{}", DisplayLabel(string)))
                    .collect(),
            });
        }
        services.sort_by(|a, b| (&a.service, &a.instance).cmp(&(&b.service, &b.instance)));
        services
    }

    /// The names the segment gave addresses: each host looked up names its
    /// addresses, and each reverse lookup the address it was asked about.
    #[must_use]
    pub fn names(&self) -> BTreeMap<IpAddr, String> {
        let mut names = BTreeMap::new();
        for (ask, held) in self.held.values() {
            match ask {
                Ask::Host(target) => {
                    for answer in held.answers() {
                        if let OwnedAnswer::Address(address) = answer.answer {
                            names
                                .entry(address)
                                .or_insert_with(|| format!("{}", WireName(target)));
                        }
                    }
                }
                Ask::Reverse(address) => {
                    if let Some(OwnedAnswer::Pointer(name)) =
                        held.answers().first().map(|answer| &answer.answer)
                    {
                        names.insert(*address, format!("{}", WireName(name)));
                    }
                }
                _ => {}
            }
        }
        names
    }

    /// Whether a bound turned an ask away in the last pass, so the view is
    /// incomplete.
    #[must_use]
    pub const fn truncated(&self) -> bool {
        self.truncated
    }

    /// The service's refusal of an ask this program may not make; what it
    /// may make goes on being asked.
    #[must_use]
    pub const fn refused(&self) -> Option<&DiscoveryError> {
        self.refused.as_ref()
    }

    /// What ended the last pass before every ask was made, which the next
    /// pass asks again: the service at its bounds, or not reached.
    #[must_use]
    pub const fn stalled(&self) -> Option<&DiscoveryError> {
        self.stalled.as_ref()
    }

    /// Whether the last pass held a lost request back for the next.
    #[must_use]
    pub const fn deferred(&self) -> bool {
        self.deferred
    }

    /// Whether the service refused the type list itself, so nothing can be
    /// shown at all.
    #[must_use]
    pub fn locked_out(&self) -> bool {
        self.refused.is_some() && self.unanswerable.contains(&Ask::Types)
    }

    /// Each looked-up host's addresses, by its wire-form name.
    fn host_addresses(&self) -> BTreeMap<&[u8], Vec<IpAddr>> {
        let mut addresses = BTreeMap::new();
        for (ask, held) in self.held.values() {
            if let Ask::Host(name) = ask {
                let found: Vec<IpAddr> = held
                    .answers()
                    .iter()
                    .filter_map(|answer| match answer.answer {
                        OwnedAnswer::Address(address) => Some(address),
                        _ => None,
                    })
                    .collect();
                addresses.insert(name.as_slice(), found);
            }
        }
        addresses
    }
}

/// The request `answer` to `parent` calls for, if any.
fn child_of(parent: &Ask, answer: &OwnedAnswer) -> Option<Ask> {
    match (parent, answer) {
        (Ask::Types, OwnedAnswer::Type(name, transport)) => Some(Ask::Browse {
            name: name.clone(),
            transport: *transport,
        }),
        (Ask::Browse { name, transport }, OwnedAnswer::Instance(instance)) => Some(Ask::Resolve {
            instance: instance.clone(),
            name: name.clone(),
            transport: *transport,
        }),
        // A target outside `.local` is a unicast DNS name the link does not
        // answer for.
        (Ask::Resolve { .. }, OwnedAnswer::Service { target, .. }) => Name::from_wire(target)
            .filter(is_link_local_name)
            .map(|_| Ask::Host(target.clone())),
        _ => None,
    }
}

/// The target a resolve holds that a client should reach first: the lowest
/// priority, then the heaviest weight (RFC 2782).
fn preferred_target(held: &Held) -> Option<(InterfaceName, u16, &Vec<u8>)> {
    held.answers()
        .iter()
        .filter_map(|answer| match &answer.answer {
            OwnedAnswer::Service {
                priority,
                weight,
                port,
                target,
            } => Some((
                *priority,
                core::cmp::Reverse(*weight),
                answer.interface,
                *port,
                target,
            )),
            _ => None,
        })
        .min_by_key(|(priority, weight, ..)| (*priority, *weight))
        .map(|(_, _, interface, port, target)| (interface, port, target))
}

/// A service type as DNS-SD spells it.
fn service_type(name: &[u8], transport: Transport) -> String {
    format!("_{}.{}", Presentation(name), transport.label())
}

#[cfg(test)]
#[path = "discover_tests.rs"]
mod tests;
