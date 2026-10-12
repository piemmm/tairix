//! Turning the landed network ABI records into the [`Map`].
//!
//! The [`Inventory`] is the source of truth the GUI updates as queries answer;
//! [`Inventory::build`] projects it into a fresh [`Map`] deterministically, so
//! the map is a derived view recomputed on change rather than mutated in place.
//!
//! One device is one node, whichever source names it first: a router the
//! routing table names and the neighbour cache resolves is the same machine,
//! so its neighbour entry adds its hardware address to the router's node rather
//! than drawing a second host beside it.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::net::IpAddr;

use tairix_abi::net_ipc::{
    address_parts, ip_from_parts, ip_network, NetInterfaceFactsRecord, NetInterfaceStateRecord,
    NetNeighborRecord, NetNeighborState, NetRouteRecord,
};
use tairix_procinfo::{render_ip, render_mac};

use tairix_net::mdns::is_link_local_address;

use crate::discover::AdvertisedService;
use crate::infer::{infer, Role, Signals};
use crate::model::{
    Classification, DeviceClass, EdgeKind, InterfaceName, Liveness, Map, NodeId, NodeKey, NodeKind,
    OsFamily,
};

/// One interface's accrued observations: its facts page, its state page, or
/// both. Either may arrive first or alone.
#[derive(Clone, Debug, Default)]
pub struct InterfaceObservation {
    /// The static facts (MAC, MTU, kind), if the facts query answered.
    pub facts: Option<NetInterfaceFactsRecord>,
    /// The live state (link, addresses), if the state query answered.
    pub state: Option<NetInterfaceStateRecord>,
}

/// The gathered inventory the map is projected from.
#[derive(Debug, Default)]
pub struct Inventory {
    interfaces: BTreeMap<InterfaceName, InterfaceObservation>,
    neighbors: Vec<NetNeighborRecord>,
    routes: Vec<NetRouteRecord>,
    services: Vec<AdvertisedService>,
    /// What the segment calls each address it was asked about or advertised.
    segment_names: BTreeMap<IpAddr, String>,
    /// Each routable address's reverse DNS answer, `None` for one with no
    /// name: the administrator's own record, preferred to what a device on
    /// the segment says of itself.
    dns_names: BTreeMap<IpAddr, Option<String>>,
    /// The organisation each assigned identifier the neighbours' hardware
    /// addresses carry was registered to, already display-safe.
    vendors: BTreeMap<u32, String>,
}

impl Inventory {
    /// An empty inventory.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record (or replace) an interface's static facts.
    pub fn observe_interface_facts(&mut self, facts: NetInterfaceFactsRecord) {
        self.interfaces.entry(facts.name).or_default().facts = Some(facts);
    }

    /// Record (or replace) an interface's live state.
    pub fn observe_interface_state(&mut self, state: NetInterfaceStateRecord) {
        self.interfaces.entry(state.name).or_default().state = Some(state);
    }

    /// Drop all interface observations, so a fresh sysinfo walk replaces rather
    /// than accumulates them.
    pub fn clear_interfaces(&mut self) {
        self.interfaces.clear();
    }

    /// Replace the neighbour-cache snapshot with a fresh walk's records.
    pub fn replace_neighbors(&mut self, neighbors: Vec<NetNeighborRecord>) {
        self.neighbors = neighbors;
    }

    /// Replace the routing-table snapshot with a fresh walk's records.
    pub fn replace_routes(&mut self, routes: Vec<NetRouteRecord>) {
        self.routes = routes;
    }

    /// Replace the advertised services.
    pub fn replace_services(&mut self, services: Vec<AdvertisedService>) {
        self.services = services;
    }

    /// Replace what the segment calls its addresses.
    pub fn replace_segment_names(&mut self, names: BTreeMap<IpAddr, String>) {
        self.segment_names = names;
    }

    /// Record a reverse DNS answer for `addr`, `None` when it has no name.
    pub fn record_dns_name(&mut self, addr: IpAddr, name: Option<String>) {
        self.dns_names.insert(addr, name);
    }

    /// Replace the organisations the neighbours' hardware addresses name.
    pub fn replace_vendors(&mut self, vendors: BTreeMap<u32, String>) {
        self.vendors = vendors;
    }

    /// Every host address the map shows — each neighbour's and each next
    /// hop's — that the segment is asked to name: the link-local ones.
    #[must_use]
    pub fn link_local_hosts(&self) -> BTreeSet<IpAddr> {
        self.host_addresses()
            .filter(|addr| is_link_local_address(*addr))
            .collect()
    }

    /// Every routable host address the map shows with no reverse DNS answer
    /// recorded yet.
    pub fn unnamed_hosts(&self) -> impl Iterator<Item = IpAddr> + '_ {
        self.host_addresses()
            .filter(|addr| !is_link_local_address(*addr) && !self.dns_names.contains_key(addr))
    }

    fn host_addresses(&self) -> impl Iterator<Item = IpAddr> + '_ {
        let neighbours = self
            .neighbors
            .iter()
            .map(|neighbor| ip_from_parts(neighbor.family, neighbor.addr));
        let hops = self
            .routes
            .iter()
            .filter_map(|route| route.next_hop.map(|hop| ip_from_parts(route.family, hop)));
        neighbours.chain(hops)
    }

    /// The interface observations, keyed by name.
    #[must_use]
    pub fn interfaces(&self) -> &BTreeMap<InterfaceName, InterfaceObservation> {
        &self.interfaces
    }

    /// Project the inventory into a fresh map, stamping every node with `now_ns`.
    #[must_use]
    pub fn build(&self, now_ns: u64) -> Map {
        let mut builder = Builder {
            map: Map::new(),
            now_ns,
            local: None,
            subnets: BTreeMap::new(),
            hosts: BTreeMap::new(),
            local_addrs: BTreeSet::new(),
            roles: BTreeMap::new(),
            macs: BTreeMap::new(),
            served: BTreeMap::new(),
            inventory: self,
        };
        let Some(local) = builder.local_host() else {
            return builder.map;
        };
        builder.local = Some(local);
        for (name, observation) in &self.interfaces {
            builder.interface(*name, observation);
        }
        // Default routes first, so a next hop that also carries named routes
        // is already the gateway when they reach it.
        let (defaults, named): (Vec<_>, Vec<_>) =
            self.routes.iter().partition(|route| route.prefix == 0);
        for route in defaults.into_iter().chain(named) {
            builder.route(route);
        }
        for neighbor in &self.neighbors {
            builder.neighbor(neighbor);
        }
        for service in &self.services {
            builder.service(service);
        }
        builder.classify();
        builder.map
    }
}

/// The first octet's bit marking an address its owner chose rather than its
/// maker (IEEE 802 "locally administered").
const LOCALLY_ADMINISTERED: u8 = 0b10;
/// The first octet's bit marking a group (multicast) address.
const GROUP: u8 = 0b01;

/// The identifier the registry assigned `mac` under, or `None` for an address
/// its owner chose itself (a privacy or virtual-machine address) or a group
/// address, neither of which names a maker.
#[must_use]
pub fn assigned_oui(mac: [u8; 6]) -> Option<u32> {
    let chosen = mac[0] & LOCALLY_ADMINISTERED != 0;
    let group = mac[0] & GROUP != 0;
    (!chosen && !group).then(|| u32::from_be_bytes([0, mac[0], mac[1], mac[2]]))
}

/// One projection in progress: the map, and the indices that let a later
/// source find the node an earlier one made.
struct Builder<'a> {
    map: Map,
    now_ns: u64,
    local: Option<NodeId>,
    /// Each interface's subnets, for placing a neighbour on the one it is in.
    subnets: BTreeMap<InterfaceName, Vec<(IpAddr, u8, NodeId)>>,
    /// Every router and gateway by address, so a neighbour entry for one
    /// joins its node.
    hosts: BTreeMap<IpAddr, NodeId>,
    /// This machine's own addresses, so a service it advertises is its.
    local_addrs: BTreeSet<IpAddr>,
    /// What each host is to classify by: the routing role it plays, its
    /// hardware address, the services it advertises.
    roles: BTreeMap<NodeId, Role>,
    macs: BTreeMap<NodeId, [u8; 6]>,
    served: BTreeMap<NodeId, Vec<&'a AdvertisedService>>,
    inventory: &'a Inventory,
}

impl<'a> Builder<'a> {
    fn local_host(&mut self) -> Option<NodeId> {
        let local = self
            .map
            .intern(NodeKey::LocalHost, NodeKind::LocalHost, self.now_ns)?;
        self.map.set_label(local, "This machine");
        self.map.set_classification(
            local,
            Classification {
                device: DeviceClass::LocalHost,
                device_confidence: 100,
                os: OsFamily::Tairix,
                os_confidence: 100,
            },
            Vec::new(),
        );
        self.map.set_liveness(local, Liveness::Reachable);
        Some(local)
    }

    /// The node for interface `name`, made and hung off the local host on
    /// first sight — a neighbour or route may name an interface whose own
    /// facts were refused.
    fn interface_node(&mut self, name: InterfaceName) -> Option<NodeId> {
        let iface = self
            .map
            .intern(NodeKey::Interface(name), NodeKind::Interface, self.now_ns)?;
        if self
            .map
            .node(iface)
            .is_some_and(|node| node.label.is_empty())
        {
            self.map.set_label(iface, &interface_label(&name));
        }
        if let Some(local) = self.local {
            self.map.link(local, iface, EdgeKind::Hosts);
        }
        Some(iface)
    }

    fn interface(&mut self, name: InterfaceName, observation: &InterfaceObservation) {
        let Some(iface) = self.interface_node(name) else {
            return;
        };
        if let Some(facts) = &observation.facts {
            self.map.set_fact(iface, "mac", &render_mac(facts.mac));
            self.map.set_fact(iface, "mtu", &format!("{}", facts.mtu));
            self.map.set_fact(iface, "kind", facts.kind.name());
        }
        let Some(state) = &observation.state else {
            return;
        };
        self.map.set_liveness(
            iface,
            if state.link_up {
                Liveness::Reachable
            } else {
                Liveness::Unreachable
            },
        );
        let count = usize::from(state.addr_count).min(state.addrs.len());
        for addr in &state.addrs[..count] {
            let ip = ip_from_parts(addr.family, addr.addr);
            self.local_addrs.insert(ip);
            self.map.add_addr(iface, ip);
            self.subnet(name, iface, ip_network(ip, addr.prefix), addr.prefix);
        }
    }

    /// The subnet `network/prefix` reached through `iface`, made once and
    /// recorded for neighbour placement.
    fn subnet(
        &mut self,
        name: InterfaceName,
        iface: NodeId,
        network: IpAddr,
        prefix: u8,
    ) -> Option<NodeId> {
        let subnet = self.map.intern(
            NodeKey::Subnet {
                iface: name,
                network,
                prefix,
            },
            NodeKind::Subnet,
            self.now_ns,
        )?;
        if self.map.link(iface, subnet, EdgeKind::OnLink) {
            let (family, bytes) = address_parts(network);
            self.map
                .set_label(subnet, &format!("{}/{prefix}", render_ip(family, &bytes)));
            self.subnets
                .entry(name)
                .or_default()
                .push((network, prefix, subnet));
        }
        Some(subnet)
    }

    fn route(&mut self, route: &NetRouteRecord) {
        let Some(iface) = self.interface_node(route.interface) else {
            return;
        };
        let dest = ip_from_parts(route.family, route.dest);
        let Some(hop) = route.next_hop else {
            // An on-link prefix: a subnet the interface reaches directly.
            self.subnet(
                route.interface,
                iface,
                ip_network(dest, route.prefix),
                route.prefix,
            );
            return;
        };
        let hop = ip_from_parts(route.family, hop);
        if route.prefix == 0 {
            self.gateway(iface, hop);
        } else if let Some(router) = self.router(hop, NodeKind::Router) {
            self.map.link(iface, router, EdgeKind::Routes);
            let network = ip_network(dest, route.prefix);
            let remote = self.map.intern(
                NodeKey::Subnet {
                    iface: route.interface,
                    network,
                    prefix: route.prefix,
                },
                NodeKind::Subnet,
                self.now_ns,
            );
            if let Some(remote) = remote {
                self.map.set_label(
                    remote,
                    &format!("{}/{}", render_ip(route.family, &route.dest), route.prefix),
                );
                self.map.link(router, remote, EdgeKind::Routes);
            }
        }
    }

    /// The default route's next hop, and the rest of the network beyond it.
    fn gateway(&mut self, iface: NodeId, hop: IpAddr) {
        let Some(gateway) = self.router(hop, NodeKind::Gateway) else {
            return;
        };
        self.map.link(iface, gateway, EdgeKind::Default);
        if let Some(beyond) = self
            .map
            .intern(NodeKey::Beyond, NodeKind::Beyond, self.now_ns)
        {
            self.map.set_label(beyond, "Beyond this network");
            self.map.link(gateway, beyond, EdgeKind::Default);
        }
    }

    /// The router at `addr`, one node however many routes name it: the first
    /// kind it was made as stands, and [`Inventory::build`] makes gateways
    /// first.
    fn router(&mut self, addr: IpAddr, kind: NodeKind) -> Option<NodeId> {
        if let Some(&existing) = self.hosts.get(&addr) {
            return Some(existing);
        }
        let (key, role) = match kind {
            NodeKind::Gateway => (NodeKey::Gateway(addr), Role::Gateway),
            _ => (NodeKey::Router(addr), Role::Router),
        };
        let router = self.map.intern(key, kind, self.now_ns)?;
        self.name_host(router, addr);
        self.roles.insert(router, role);
        self.hosts.insert(addr, router);
        Some(router)
    }

    fn neighbor(&mut self, neighbor: &NetNeighborRecord) {
        let ip = ip_from_parts(neighbor.family, neighbor.addr);
        let Some(node) = self
            .hosts
            .get(&ip)
            .copied()
            .or_else(|| self.neighbor_node(ip))
        else {
            return;
        };
        if let Some(mac) = neighbor.mac {
            let rendered = render_mac(mac);
            if assigned_oui(mac).is_some() {
                self.map.set_fact(node, "mac", &rendered);
            } else {
                self.map
                    .set_fact(node, "mac", &format!("{rendered} (locally administered)"));
            }
            self.macs.insert(node, mac);
        }
        self.map.set_fact(node, "neighbor", neighbor.state.name());
        if neighbor.state == NetNeighborState::Reachable {
            self.map.set_liveness(node, Liveness::Reachable);
        }
        let parent = self
            .containing_subnet(neighbor.interface, ip)
            .or_else(|| self.interface_node(neighbor.interface));
        if let Some(parent) = parent {
            self.map.link(parent, node, EdgeKind::OnLink);
        }
    }

    /// A fresh node for a neighbour no other source has named.
    fn neighbor_node(&mut self, ip: IpAddr) -> Option<NodeId> {
        let node = self
            .map
            .intern(NodeKey::Neighbour(ip), NodeKind::Neighbour, self.now_ns)?;
        self.name_host(node, ip);
        Some(node)
    }

    /// Give host `node` its address, and its label: the administrator's DNS
    /// name for `addr`, else what the segment calls it, else the address.
    fn name_host(&mut self, node: NodeId, addr: IpAddr) {
        self.map.add_addr(node, addr);
        let dns = self.inventory.dns_names.get(&addr).and_then(Option::as_ref);
        let local = self.inventory.segment_names.get(&addr);
        if let Some(name) = dns {
            self.map.set_fact(node, "dns name", name);
        }
        if let Some(name) = local {
            self.map.set_fact(node, "local name", name);
        }
        if let Some(name) = dns.or(local) {
            self.map.set_label(node, name);
        } else {
            let (family, bytes) = address_parts(addr);
            self.map.set_label(node, &render_ip(family, &bytes));
        }
    }

    /// A service under the node at one of its addresses — a router, a
    /// neighbour, or this machine — a neighbour made for it when only
    /// discovery knows the host, or its interface until its host's addresses
    /// are known.
    fn service(&mut self, service: &'a AdvertisedService) {
        let owner = service
            .addresses
            .iter()
            .find_map(|addr| self.host_at(*addr))
            .or_else(|| {
                let addr = *service.addresses.first()?;
                self.advertised_host(service.interface, addr)
            })
            .or_else(|| self.interface_node(service.interface));
        let Some(owner) = owner else {
            return;
        };
        let key = NodeKey::Service(format!("{}.{}", service.instance, service.service));
        let Some(node) = self.map.intern(key, NodeKind::Service, self.now_ns) else {
            return;
        };
        self.map.set_label(node, &service.instance);
        self.map.set_fact(node, "service", &service.service);
        self.map
            .set_fact(node, "port", &format!("{}", service.port));
        self.map.set_fact(node, "host", &service.host);
        if !service.text.is_empty() {
            self.map.set_fact(node, "txt", &service.text.join(" "));
        }
        self.map.set_liveness(node, Liveness::Advertised);
        self.map.link(owner, node, EdgeKind::Serves);
        if self.map.node(owner).is_some_and(|node| is_host(node.kind)) {
            self.served.entry(owner).or_default().push(service);
        }
    }

    /// Infer what each host is from everything the sources said of it, and
    /// name the organisation its hardware address was assigned to.
    fn classify(&mut self) {
        let inventory = self.inventory;
        let hosts: Vec<NodeId> = self
            .map
            .nodes()
            .iter()
            .filter(|node| is_host(node.kind))
            .map(|node| node.id)
            .collect();
        for id in hosts {
            let vendor = self
                .macs
                .get(&id)
                .and_then(|mac| assigned_oui(*mac))
                .and_then(|oui| inventory.vendors.get(&oui))
                .map(String::as_str);
            if let Some(vendor) = vendor {
                self.map.set_fact(id, "vendor", vendor);
            }
            let (classification, evidence) = infer(&Signals {
                role: self.roles.get(&id).copied(),
                vendor,
                services: self.served.get(&id).map_or(&[], Vec::as_slice),
            });
            self.map.set_classification(id, classification, evidence);
        }
    }

    /// The node already standing for the host at `addr`.
    fn host_at(&self, addr: IpAddr) -> Option<NodeId> {
        if self.local_addrs.contains(&addr) {
            return self.local;
        }
        self.hosts
            .get(&addr)
            .copied()
            .or_else(|| self.map.id_for(&NodeKey::Neighbour(addr)))
    }

    /// A neighbour no source but discovery named, on the subnet of `iface`
    /// that holds `addr`.
    fn advertised_host(&mut self, iface: InterfaceName, addr: IpAddr) -> Option<NodeId> {
        let node = self
            .map
            .intern(NodeKey::Neighbour(addr), NodeKind::Neighbour, self.now_ns)?;
        self.name_host(node, addr);
        self.map.set_liveness(node, Liveness::Advertised);
        let parent = self
            .containing_subnet(iface, addr)
            .or_else(|| self.interface_node(iface));
        if let Some(parent) = parent {
            self.map.link(parent, node, EdgeKind::OnLink);
        }
        Some(node)
    }

    /// The narrowest subnet on `iface` that contains `ip`.
    fn containing_subnet(&self, iface: InterfaceName, ip: IpAddr) -> Option<NodeId> {
        self.subnets
            .get(&iface)?
            .iter()
            .filter(|(network, prefix, _)| ip_network(ip, *prefix) == *network)
            .max_by_key(|(_, prefix, _)| *prefix)
            .map(|(_, _, subnet)| *subnet)
    }
}

/// Whether a node of `kind` is a machine on the network whose class is
/// inferred rather than fixed.
fn is_host(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Neighbour | NodeKind::Router | NodeKind::Gateway | NodeKind::RemoteHost
    )
}

/// An interface name's printable form (its bytes up to the first NUL).
fn interface_label(name: &InterfaceName) -> String {
    let end = name.iter().position(|&b| b == 0).unwrap_or(name.len());
    String::from(core::str::from_utf8(&name[..end]).unwrap_or("?"))
}

#[cfg(test)]
#[path = "ingest_tests.rs"]
mod tests;
