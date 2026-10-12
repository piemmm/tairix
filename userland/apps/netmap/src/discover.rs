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

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::net::IpAddr;

use tairix_abi::discovery_ipc::{Entry, Families, Query, ServiceTypeField, Transport};
use tairix_discovery::{request_of, DiscoveryError, Held, OwnedAnswer, Session};
use tairix_net::dns::{DisplayLabel, Presentation, WireName};
use tairix_net::mdns::{is_link_local_address, TxtStrings};

use crate::model::InterfaceName;

/// The most service types browsed at once.
pub const MAX_TYPES: usize = 64;
/// The most instances resolved at once.
pub const MAX_SERVICES: usize = 256;
/// The most hosts looked up at once.
pub const MAX_HOSTS: usize = 128;
/// The most link-local addresses named at once.
pub const MAX_REVERSES: usize = 128;

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

/// The discovery-v1 requests the map keeps open and what each holds.
#[derive(Debug, Default)]
pub struct Discovery {
    /// Each open request, by its id.
    held: BTreeMap<u32, (Ask, Held)>,
    /// Each open request's id, by what it asks.
    open: BTreeMap<Ask, u32>,
    /// A bound turned an ask away.
    truncated: bool,
    /// The service refused a request: nothing further is started.
    refused: Option<DiscoveryError>,
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

    /// Bring the open requests in line with what is held: the segment's
    /// types, a browse per type, a resolve per instance, a lookup per host,
    /// and a reverse lookup for each link-local address in `named`. A request
    /// whose updates were dropped is started again, which answers with what
    /// is held now.
    pub fn reconcile(&mut self, directory: &mut dyn Directory, named: &BTreeSet<IpAddr>) {
        let wanted = self.wanted(named);
        let lost: Vec<Ask> = self
            .held
            .values()
            .filter(|(ask, held)| held.is_lost() && wanted.contains(ask))
            .map(|(ask, _)| ask.clone())
            .collect();
        let stale: Vec<(Ask, u32)> = self
            .open
            .iter()
            .filter(|(ask, _)| !wanted.contains(*ask) || lost.contains(ask))
            .map(|(ask, &request)| (ask.clone(), request))
            .collect();
        for (ask, request) in stale {
            self.open.remove(&ask);
            self.held.remove(&request);
            // The service ends a request its session no longer names when it
            // closes; a refused stop leaves nothing this map acts on.
            let _ = directory.stop(request);
        }
        let mut counts = [0usize; 5];
        for ask in self.open.keys() {
            counts[usize::from(ask.rank())] += 1;
        }
        for ask in wanted {
            if self.refused.is_some() || self.open.contains_key(&ask) {
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
                Err(refusal) => self.refused = Some(refusal),
            }
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

    /// Whether a bound turned an ask away, so the view is incomplete.
    #[must_use]
    pub const fn truncated(&self) -> bool {
        self.truncated
    }

    /// The service's refusal, once one stopped further requests.
    #[must_use]
    pub const fn refused(&self) -> Option<&DiscoveryError> {
        self.refused.as_ref()
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
        (Ask::Resolve { .. }, OwnedAnswer::Service { target, .. }) => {
            Some(Ask::Host(target.clone()))
        }
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
