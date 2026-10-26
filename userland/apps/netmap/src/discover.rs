//! Advertised services and link-local names as the map draws them, from the
//! walk over everything the segment offers that `lib/discovery` keeps
//! (`tairix_discovery::walk`).
//!
//! Nothing a peer authored is display-safe, so every label and `TXT` string
//! reaches the view as a [`DisplayLabel`] and every name as a [`WireName`].

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::net::IpAddr;

use tairix_abi::discovery_ipc::{Transport, REQUESTS_PER_ACCOUNT};
use tairix_discovery::walk::{Ask, Bounds, Discovery, Service};
use tairix_net::dns::{DisplayLabel, Presentation, WireName};
use tairix_net::mdns::TxtStrings;

use crate::model::InterfaceName;

pub use tairix_discovery::walk::{segment_roots, Collect, Directory, OpenSession};

/// How many of each request the map keeps open at once.
pub const BOUNDS: Bounds = Bounds {
    browses: 64,
    resolves: 256,
    hosts: 128,
    reverses: 128,
};

// The map alone never meets the bound its account shares with its other
// programs, even while a lost request is replaced before the old one stops.
const _: () = assert!(BOUNDS.requests() < REQUESTS_PER_ACCOUNT);

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

/// The resolved `services`, as the map draws them.
#[must_use]
pub fn advertised(services: Vec<Service>) -> Vec<AdvertisedService> {
    services
        .into_iter()
        .map(|service| AdvertisedService {
            instance: format!("{}", DisplayLabel(&service.instance)),
            service: service_type(&service.name, service.transport),
            interface: service.interface,
            port: service.port,
            host: format!("{}", WireName(&service.host)),
            addresses: service.addresses,
            text: service
                .text
                .iter()
                .flat_map(|octets| TxtStrings::over(octets))
                .filter(|string| !string.is_empty())
                .map(|string| format!("{}", DisplayLabel(string)))
                .collect(),
        })
        .collect()
}

/// The `names` the segment gave addresses, in wire form, as the map draws
/// them.
#[must_use]
pub fn names(names: BTreeMap<IpAddr, &[u8]>) -> BTreeMap<IpAddr, String> {
    names
        .into_iter()
        .map(|(address, name)| (address, format!("{}", WireName(name))))
        .collect()
}

/// Whether the service refused the map the type list itself, so nothing can
/// be shown at all.
#[must_use]
pub fn locked_out(discovery: &Discovery) -> bool {
    discovery.refused().is_some() && discovery.is_unanswerable(&Ask::Types)
}

/// A service type as DNS-SD spells it.
fn service_type(name: &[u8], transport: Transport) -> String {
    format!("_{}.{}", Presentation(name), transport.label())
}

#[cfg(test)]
#[path = "discover_tests.rs"]
mod tests;
