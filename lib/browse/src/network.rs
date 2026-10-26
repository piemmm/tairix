//! The Network location (`plans/ZEROCONF.md` Z9): the storage endpoints the
//! network catalogue offers, as the items a [`Browser`](crate::Browser) shows
//! there.
//!
//! Items are built once per catalogue answer, never per paint. Every name in
//! one but a host the user named was authored by an unauthenticated peer, so
//! each is shown through [`tairix_util::utf8::shown`], and an endpoint whose
//! name mixes scripts the way an impersonation needs is marked by a caution
//! picture the peer cannot draw. Nothing here is a path: activating an
//! endpoint hands back the catalogue's own record of it, as it was displayed.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cmp::Ordering;
use core::net::IpAddr;

use tairix_abi::net_ipc::{if_name_str, IF_NAME_LEN};
use tairix_discovery::catalogue::{Endpoint, Found, Listing, Source};
use tairix_discovery::OwnedMembership;
use tairix_icon::IconKind;
use tairix_net::addr::needs_zone;
use tairix_net::dns::WireName;
use tairix_util::utf8::{mixes_scripts, shown};

use crate::sort::{name_cmp, SortDirection, SortKey, SortMode};

/// What the network location is called: its window's title, and the menu row
/// that opens it.
pub const NETWORK_TITLE: &str = "Network";

/// What the type column says of an endpoint whose name mixes scripts.
pub const MIXED_SCRIPTS_LABEL: &str = "Name mixes scripts";

/// What the type column says of a row standing for what is not listed.
pub const NOT_LISTED_LABEL: &str = "Not listed";

/// The row standing for what the catalogue could not hold.
pub const TRUNCATED_LABEL: &str = "More is offered than can be listed";

/// One item the Network location lists.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NetworkItem {
    /// A storage endpoint.
    Endpoint(NetworkEndpoint),
    /// Endpoints one source offered past its quota, counted rather than
    /// listed, so one peer cannot crowd out the rest unseen.
    Omitted(Omission),
    /// A bound turned an endpoint or a question away.
    Truncated,
}

/// A storage endpoint as the Network location shows it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetworkEndpoint {
    endpoint: Endpoint,
    name: String,
    address: String,
    membership: String,
    mixed: bool,
}

/// What one source offered past its quota.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Omission {
    interface: [u8; IF_NAME_LEN],
    source: String,
    label: String,
}

/// Every item `listing` holds: its endpoints, then a row per source it held
/// back and one when it is short of what is offered.
#[must_use]
pub fn items(listing: Listing) -> Vec<NetworkItem> {
    let mut items: Vec<NetworkItem> = listing
        .endpoints
        .into_iter()
        .map(|endpoint| NetworkItem::Endpoint(NetworkEndpoint::new(endpoint)))
        .collect();
    items.extend(listing.omitted.iter().map(|(interface, source, count)| {
        NetworkItem::Omitted(Omission::new(*interface, source, *count))
    }));
    if listing.truncated {
        items.push(NetworkItem::Truncated);
    }
    items
}

impl NetworkEndpoint {
    /// `endpoint` as it is shown.
    #[must_use]
    pub fn new(endpoint: Endpoint) -> Self {
        let decoded = String::from_utf8_lossy(&endpoint.name);
        let mixed = mixes_scripts(&decoded);
        let name = shown(&decoded).into_owned();
        Self {
            address: reached_at(&endpoint),
            membership: membership(&endpoint.membership),
            name,
            mixed,
            endpoint,
        }
    }

    /// The catalogue's record of the endpoint: what a choice of it carries.
    #[must_use]
    pub const fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// Its name, as shown.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Where it is reached, as shown: its first address, the host it was
    /// advertised at while no address is known, or nothing for a host the
    /// user named.
    #[must_use]
    pub fn address(&self) -> &str {
        &self.address
    }

    /// The workgroup or domain it said it belongs to, as shown.
    #[must_use]
    pub fn membership(&self) -> &str {
        &self.membership
    }

    /// Whether its name mixes scripts the way an impersonation needs.
    #[must_use]
    pub const fn mixes_scripts(&self) -> bool {
        self.mixed
    }

    /// Whether it can be reached now: an address is known for it, or the user
    /// named it and the mount resolves it.
    #[must_use]
    pub fn is_available(&self) -> bool {
        !self.endpoint.addresses.is_empty() || self.endpoint.found == Found::Named
    }

    /// What the type column says of it.
    #[must_use]
    pub const fn kind_label(&self) -> &'static str {
        if self.mixed {
            return MIXED_SCRIPTS_LABEL;
        }
        match self.endpoint.found {
            Found::Advertised | Found::Both => "File server",
            Found::Announced => "Computer",
            Found::Named => "Named by you",
        }
    }

    /// Its picture: what found it says it is, or the caution a name that mixes
    /// scripts wears instead. Nothing the peer supplied chooses it.
    #[must_use]
    pub const fn icon(&self) -> IconKind {
        if self.mixed {
            return IconKind::AlertWarning;
        }
        match self.endpoint.found {
            Found::Announced => IconKind::Computer,
            Found::Advertised | Found::Both | Found::Named => IconKind::Server,
        }
    }

    const fn kind_rank(&self) -> u8 {
        if self.mixed {
            return 3;
        }
        match self.endpoint.found {
            Found::Advertised | Found::Both => 0,
            Found::Announced => 1,
            Found::Named => 2,
        }
    }
}

impl Omission {
    /// `count` endpoints from `source`, learned on `interface`, held back.
    #[must_use]
    pub fn new(interface: [u8; IF_NAME_LEN], source: &Source, count: usize) -> Self {
        let source = match source {
            Source::Address(address) => spelled_address(*address, Some(interface)),
            Source::Host(host) => WireName(host).to_string(),
        };
        Self {
            interface,
            label: format!("{count} more from {source}"),
            source,
        }
    }

    /// The row's text: how many were held back, and from where.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Where they came from, as shown.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }
}

impl NetworkItem {
    /// The item's name column: an endpoint's name, or what a row standing for
    /// the unlisted says.
    #[must_use]
    pub fn label(&self) -> &str {
        match self {
            Self::Endpoint(endpoint) => endpoint.name(),
            Self::Omitted(omission) => omission.label(),
            Self::Truncated => TRUNCATED_LABEL,
        }
    }

    /// The item's address column.
    #[must_use]
    pub fn address(&self) -> &str {
        match self {
            Self::Endpoint(endpoint) => endpoint.address(),
            Self::Omitted(omission) => omission.source(),
            Self::Truncated => "",
        }
    }

    /// The item's workgroup column.
    #[must_use]
    pub fn membership(&self) -> &str {
        match self {
            Self::Endpoint(endpoint) => endpoint.membership(),
            Self::Omitted(_) | Self::Truncated => "",
        }
    }

    /// The item's type column.
    #[must_use]
    pub const fn kind_label(&self) -> &'static str {
        match self {
            Self::Endpoint(endpoint) => endpoint.kind_label(),
            Self::Omitted(_) | Self::Truncated => NOT_LISTED_LABEL,
        }
    }

    /// The item's picture.
    #[must_use]
    pub const fn icon(&self) -> IconKind {
        match self {
            Self::Endpoint(endpoint) => endpoint.icon(),
            Self::Omitted(_) | Self::Truncated => IconKind::Info,
        }
    }

    /// Whether activating the item can do anything now.
    #[must_use]
    pub fn is_available(&self) -> bool {
        match self {
            Self::Endpoint(endpoint) => endpoint.is_available(),
            Self::Omitted(_) | Self::Truncated => false,
        }
    }

    /// The endpoint the item is, if it is one.
    #[must_use]
    pub const fn endpoint(&self) -> Option<&NetworkEndpoint> {
        match self {
            Self::Endpoint(endpoint) => Some(endpoint),
            Self::Omitted(_) | Self::Truncated => None,
        }
    }

    /// What names this item across catalogue answers, so a selection follows
    /// an endpoint whose addresses or membership moved.
    pub(crate) fn identity(&self) -> Identity<'_> {
        match self {
            Self::Endpoint(shown) => {
                let endpoint = &shown.endpoint;
                // An advertised endpoint is its instance at its host; one
                // nothing advertised is the computer at its address.
                let address = match endpoint.host {
                    Some(_) => None,
                    None => endpoint.addresses.first().copied(),
                };
                Identity::Endpoint {
                    interface: endpoint.interface,
                    name: &endpoint.name,
                    host: endpoint.host.as_deref(),
                    address,
                }
            }
            Self::Omitted(omission) => Identity::Omitted(omission.interface, &omission.source),
            Self::Truncated => Identity::Truncated,
        }
    }

    const fn rank(&self) -> u8 {
        match self {
            Self::Endpoint(_) => 0,
            Self::Omitted(_) => 1,
            Self::Truncated => 2,
        }
    }
}

/// What names a [`NetworkItem`] across catalogue answers.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) enum Identity<'a> {
    Endpoint {
        interface: Option<[u8; IF_NAME_LEN]>,
        name: &'a [u8],
        host: Option<&'a [u8]>,
        address: Option<IpAddr>,
    },
    Omitted([u8; IF_NAME_LEN], &'a str),
    Truncated,
}

/// Sort `items` into the order `mode` gives the Network location: endpoints
/// by its key, then a row per source held back, then the truncation row.
///
/// A key no endpoint carries — a file's size or a volume's health — orders
/// them by name, as a key no entry carries orders a folder.
pub(crate) fn order(items: &mut [NetworkItem], mode: SortMode) {
    items.sort_by(|a, b| item_cmp(a, b, mode));
}

/// The total order two items take under `mode`.
pub(crate) fn item_cmp(a: &NetworkItem, b: &NetworkItem, mode: SortMode) -> Ordering {
    a.rank().cmp(&b.rank()).then_with(|| match (a, b) {
        (NetworkItem::Endpoint(a), NetworkItem::Endpoint(b)) => endpoint_cmp(a, b, mode),
        // The catalogue lists what it held back in its own stable order.
        _ => Ordering::Equal,
    })
}

fn endpoint_cmp(a: &NetworkEndpoint, b: &NetworkEndpoint, mode: SortMode) -> Ordering {
    let primary = match mode.key {
        SortKey::Name => name_cmp(&a.name, &b.name),
        SortKey::Address => reach_order(a).cmp(&reach_order(b)),
        SortKey::Workgroup => name_cmp(&a.membership, &b.membership),
        SortKey::Kind => a.kind_rank().cmp(&b.kind_rank()),
        SortKey::Size
        | SortKey::Modified
        | SortKey::Free
        | SortKey::Filesystem
        | SortKey::Health => Ordering::Equal,
    };
    let primary = match mode.direction {
        SortDirection::Ascending => primary,
        SortDirection::Descending => primary.reverse(),
    };
    primary
        .then_with(|| name_cmp(&a.name, &b.name))
        .then_with(|| a.identity_cmp(b))
}

impl NetworkEndpoint {
    fn identity_cmp(&self, other: &Self) -> Ordering {
        (
            &self.endpoint.interface,
            &self.endpoint.addresses,
            &self.endpoint.host,
        )
            .cmp(&(
                &other.endpoint.interface,
                &other.endpoint.addresses,
                &other.endpoint.host,
            ))
    }
}

/// Where each of `old`'s items sits in `fresh`, a later answer, by what names
/// it; or [`None`] without the memory to match them.
pub(crate) fn carried(old: &[NetworkItem], fresh: &[NetworkItem]) -> Option<Vec<Option<usize>>> {
    let mut by_identity: Vec<(Identity<'_>, usize)> = Vec::new();
    by_identity.try_reserve_exact(fresh.len()).ok()?;
    by_identity.extend(fresh.iter().map(NetworkItem::identity).zip(0..));
    by_identity.sort_unstable();
    let mut placed = tairix_util::fallible::filled(old.len(), None)?;
    for (slot, item) in placed.iter_mut().zip(old) {
        let identity = item.identity();
        *slot = by_identity
            .binary_search_by(|probe| probe.0.cmp(&identity))
            .ok()
            .map(|found| by_identity[found].1);
    }
    Some(placed)
}

/// Where an endpoint sorts by address: by its first address, one with none
/// first, then by the host it was advertised at.
fn reach_order(endpoint: &NetworkEndpoint) -> (Option<IpAddr>, Option<&[u8]>) {
    (
        endpoint.endpoint.addresses.first().copied(),
        endpoint.endpoint.host.as_deref(),
    )
}

/// Where `endpoint` is reached, as shown.
fn reached_at(endpoint: &Endpoint) -> String {
    match (endpoint.addresses.first(), &endpoint.host) {
        (Some(&address), _) => spelled_address(address, endpoint.interface),
        (None, Some(host)) => WireName(host).to_string(),
        (None, None) => String::new(),
    }
}

/// `address`, qualified by the link it was met on when it names a host only
/// within one (RFC 4007 §11).
fn spelled_address(address: IpAddr, interface: Option<[u8; IF_NAME_LEN]>) -> String {
    match interface.filter(|_| needs_zone(address)) {
        Some(zone) => format!("{address}%{}", if_name_str(&zone)),
        None => format!("{address}"),
    }
}

/// What a computer said it belongs to, as shown.
fn membership(membership: &OwnedMembership) -> String {
    match membership {
        OwnedMembership::Workgroup(name) => shown(name).into_owned(),
        OwnedMembership::Domain(name) => format!("{} (domain)", shown(name)),
        OwnedMembership::Unstated => String::new(),
    }
}

#[cfg(test)]
#[path = "network_tests.rs"]
mod tests;
