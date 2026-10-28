//! The catalogue of storage the segment offers (`plans/ZEROCONF.md` Z8): what
//! a file manager's Network view and the share commands browse.
//!
//! It lists the instances DNS-SD advertises for each storage service type,
//! the computers WS-Discovery names, and the hosts the user names, as one list
//! of endpoints: an instance and a computer at one address on one link are one
//! entry. It is a query, never a path — nothing in it is reachable by a path
//! until it is mounted — and nothing in it raises a credential prompt.
//!
//! Every name in it but a host the user named was authored by an
//! unauthenticated peer: structurally valid, never display-safe. It is
//! bounded: at most [`MAX_ENDPOINTS`] endpoints, and at most [`PER_SOURCE`]
//! from one source on one link, the rest counted against that source rather
//! than dropped silently, so one peer cannot crowd out the others.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec::Vec;
use core::net::IpAddr;

use tairix_abi::discovery_ipc::{Entry, ServiceTypeField, Transport, REQUESTS_PER_ACCOUNT};
use tairix_abi::net_ipc::IF_NAME_LEN;
use tairix_abi::netshare_ipc::SMB_PORT;

use crate::walk::{Ask, Bounds, Computer, Directory, Discovery, Service};
use crate::OwnedMembership;

/// The storage service types the catalogue browses: the share protocols this
/// system mounts.
pub const STORAGE_TYPES: [ServiceTypeField<'static>; 1] = [ServiceTypeField {
    name: b"smb",
    transport: Transport::Tcp,
}];

/// The most endpoints listed.
pub const MAX_ENDPOINTS: usize = 256;

/// The most endpoints listed from one source on one link.
pub const PER_SOURCE: usize = 4;

/// The most hosts the user may name at once.
pub const MAX_NAMED: usize = 32;

const BOUNDS: Bounds = Bounds {
    browses: STORAGE_TYPES.len(),
    resolves: MAX_ENDPOINTS,
    hosts: MAX_ENDPOINTS,
    reverses: 0,
};

const _: () = assert!(BOUNDS.requests() < REQUESTS_PER_ACCOUNT);

/// What found an endpoint.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum Found {
    /// DNS-SD advertised it.
    Advertised,
    /// WS-Discovery named it.
    Announced,
    /// Both, at one address on one link.
    Both,
    /// The user named it.
    Named,
}

/// One storage endpoint, as a choice of it carries it to the mount.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Endpoint {
    /// The link it was learned on, or `None` for a host the user named.
    pub interface: Option<[u8; IF_NAME_LEN]>,
    /// Its name: an instance's label, a computer's name, or the user's own
    /// spelling of a host.
    pub name: Vec<u8>,
    /// What found it.
    pub found: Found,
    /// The host an advertised instance is reached at, in wire form.
    pub host: Option<Vec<u8>>,
    /// The addresses it is reached at, once known.
    pub addresses: Vec<IpAddr>,
    /// The port it is reached on.
    pub port: u16,
    /// The workgroup or domain a computer said it belongs to.
    pub membership: OwnedMembership,
}

/// The endpoints the catalogue lists now, and what it holds back.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Listing {
    /// The endpoints, ordered by name.
    pub endpoints: Vec<Endpoint>,
    /// Endpoints past their source's quota, by link and source.
    pub omitted: Vec<([u8; IF_NAME_LEN], Source, usize)>,
    /// Whether the catalogue is short of what the segment offers: a bound
    /// turned an ask or an endpoint away.
    pub truncated: bool,
}

/// What an endpoint came from, for the per-source quota: the first address it
/// is reached at, or the host it was advertised at before that is known.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Source {
    /// An address.
    Address(IpAddr),
    /// A host, in wire form.
    Host(Vec<u8>),
}

/// The catalogue: the walk over the storage types and the computers, and the
/// hosts the user named.
#[derive(Debug)]
pub struct Catalogue {
    walk: Discovery,
    named: BTreeSet<String>,
}

impl Catalogue {
    /// An empty catalogue, a lost request replaced again no sooner than
    /// `pace` nanoseconds after the last.
    #[must_use]
    pub fn new(pace: u64) -> Self {
        Self {
            walk: Discovery::new(BOUNDS, pace),
            named: BTreeSet::new(),
        }
    }

    /// Fold one entry from the service into what the catalogue holds.
    pub fn apply(&mut self, entry: &Entry<'_>) {
        self.walk.apply(entry);
    }

    /// Bring the requests the catalogue keeps open in line with what it
    /// holds at `now`.
    pub fn reconcile(&mut self, directory: &mut dyn Directory, now: u64) {
        let mut roots: BTreeSet<Ask> = STORAGE_TYPES
            .iter()
            .map(|service| Ask::Browse {
                name: service.name.to_vec(),
                transport: service.transport,
            })
            .collect();
        roots.insert(Ask::Computers);
        self.walk.reconcile(directory, &roots, now);
    }

    /// List `host`, as the user spelled it, beside what the segment offers;
    /// `false` when [`MAX_NAMED`] hosts are named already.
    pub fn name(&mut self, host: &str) -> bool {
        if self.named.contains(host) {
            return true;
        }
        self.named.len() < MAX_NAMED && self.named.insert(String::from(host))
    }

    /// Stop listing `host`.
    pub fn forget(&mut self, host: &str) {
        self.named.remove(host);
    }

    /// The walk the catalogue keeps, for what it says about its own state:
    /// what the service refused, and whether a pass ended early.
    #[must_use]
    pub const fn walk(&self) -> &Discovery {
        &self.walk
    }

    /// What the catalogue lists now.
    #[must_use]
    pub fn listing(&self) -> Listing {
        let mut found = merged(self.walk.services(), self.walk.computers());
        found.sort_by(|a, b| (&a.name, &a.interface).cmp(&(&b.name, &b.interface)));
        let mut listing = Listing {
            truncated: self.walk.truncated(),
            ..Listing::default()
        };
        let mut from_source: BTreeMap<([u8; IF_NAME_LEN], Source), usize> = BTreeMap::new();
        for endpoint in found {
            let (Some(interface), Some(source)) = (endpoint.interface, source_of(&endpoint)) else {
                continue;
            };
            let count = from_source.entry((interface, source)).or_insert(0);
            *count += 1;
            if *count > PER_SOURCE {
                continue;
            }
            if listing.endpoints.len() == MAX_ENDPOINTS {
                listing.truncated = true;
                continue;
            }
            listing.endpoints.push(endpoint);
        }
        listing.omitted = from_source
            .into_iter()
            .filter(|(_, count)| *count > PER_SOURCE)
            .map(|((interface, source), count)| (interface, source, count - PER_SOURCE))
            .collect();
        listing
            .endpoints
            .extend(self.named.iter().map(|host| Endpoint {
                interface: None,
                name: host.as_bytes().to_vec(),
                found: Found::Named,
                host: None,
                addresses: Vec::new(),
                port: SMB_PORT,
                membership: OwnedMembership::Unstated,
            }));
        listing
    }
}

/// Every endpoint `services` and `computers` describe, a computer merged into
/// the first instance on its link reached at its address.
fn merged(services: Vec<Service>, computers: Vec<Computer>) -> Vec<Endpoint> {
    let mut endpoints: Vec<Endpoint> = services
        .into_iter()
        .map(|service| Endpoint {
            interface: Some(service.interface),
            name: service.instance,
            found: Found::Advertised,
            host: Some(service.host),
            addresses: service.addresses,
            port: service.port,
            membership: OwnedMembership::Unstated,
        })
        .collect();
    let mut by_address: BTreeMap<([u8; IF_NAME_LEN], IpAddr), usize> = BTreeMap::new();
    for (at, endpoint) in endpoints.iter().enumerate() {
        if let Some(interface) = endpoint.interface {
            for address in &endpoint.addresses {
                by_address.entry((interface, *address)).or_insert(at);
            }
        }
    }
    for computer in computers {
        match by_address.get(&(computer.interface, computer.address)) {
            Some(&at) if endpoints[at].found == Found::Advertised => {
                let endpoint = &mut endpoints[at];
                endpoint.found = Found::Both;
                endpoint.membership = computer.membership;
            }
            _ => endpoints.push(Endpoint {
                interface: Some(computer.interface),
                name: computer.name.into_bytes(),
                found: Found::Announced,
                host: None,
                addresses: Vec::from([computer.address]),
                port: SMB_PORT,
                membership: computer.membership,
            }),
        }
    }
    endpoints
}

/// What `endpoint` is counted against: the first address it is reached at,
/// or the host it was advertised at until that is known.
fn source_of(endpoint: &Endpoint) -> Option<Source> {
    match (endpoint.addresses.first(), &endpoint.host) {
        (Some(address), _) => Some(Source::Address(*address)),
        (None, Some(host)) => Some(Source::Host(host.clone())),
        (None, None) => None,
    }
}

#[cfg(test)]
#[path = "catalogue_tests.rs"]
mod tests;
