//! The map's data model: a bounded, idempotent graph of discovered nodes and
//! the relationships between them.
//!
//! The map is a projection recomputed from the [`Inventory`](crate::Inventory)
//! whenever a source changes, so a [`Map`] only ever grows during one build and
//! needs no incremental retire. Every count and length is a fixed containment
//! bound (an attacker on the segment authors these), not a capacity a machine
//! scales: a source that would exceed one is truncated and [`Map::truncated`]
//! is set, never grown without bound.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec::Vec;
use core::net::IpAddr;

use tairix_abi::net_ipc::IF_NAME_LEN;
use tairix_geometry::Point;

use crate::infer::Evidence;

/// Most nodes a map holds before further ones are dropped.
pub const MAX_NODES: usize = 4096;
/// Most edges a map holds before further ones are dropped.
pub const MAX_EDGES: usize = 16_384;
/// Longest display label a node keeps; a longer one is truncated on a `char`
/// boundary.
pub const MAX_LABEL_LEN: usize = 128;
/// Most addresses one node accrues.
pub const MAX_NODE_ADDRS: usize = 16;
/// Most facts one node accrues.
pub const MAX_NODE_FACTS: usize = 16;

/// An interface's NUL-padded admin-chosen name, the stack's own identifier.
pub type InterfaceName = [u8; IF_NAME_LEN];

/// What a node *is* on the map.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum NodeKind {
    /// This machine.
    LocalHost,
    /// One of this machine's network interfaces.
    Interface,
    /// An IP subnet an interface is on.
    Subnet,
    /// A host seen on a subnet (an ARP/NDP neighbour or a discovered peer).
    Neighbour,
    /// A service instance a host advertises.
    Service,
    /// A router learned from the routing table, an RA, or a traceroute hop.
    Router,
    /// The default-route gateway.
    Gateway,
    /// A layer-2 switch inferred from captured control frames.
    Switch,
    /// A wireless access point inferred from captured control frames.
    AccessPoint,
    /// A host beyond the local link, learned from a traceroute path.
    RemoteHost,
    /// The sink a default route points at — the rest of the network / WAN.
    Beyond,
}

/// The passively-inferred device class that selects a node's icon.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub enum DeviceClass {
    /// No class could be inferred yet.
    #[default]
    Unknown,
    /// This machine.
    LocalHost,
    /// A general-purpose desktop computer.
    Desktop,
    /// A portable computer.
    Laptop,
    /// A server.
    Server,
    /// A router.
    Router,
    /// A layer-2 switch.
    Switch,
    /// A wireless access point.
    AccessPoint,
    /// A printer.
    Printer,
    /// A phone or other handheld.
    Phone,
    /// Network-attached storage.
    Nas,
    /// An embedded / Internet-of-things device.
    Iot,
}

/// The passively-inferred operating-system family of a host.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub enum OsFamily {
    /// No family could be inferred yet.
    #[default]
    Unknown,
    /// TAIRiX.
    Tairix,
    /// A Linux distribution.
    Linux,
    /// Android.
    Android,
    /// Microsoft Windows.
    Windows,
    /// macOS.
    MacOs,
    /// iOS and the systems derived from it: iPadOS, tvOS, audioOS.
    Ios,
    /// A dedicated network-equipment OS.
    NetworkOs,
}

/// A node's passively-inferred classification, with the confidence behind
/// each half of it.
///
/// A guess is always a guess: each confidence is `0..=100`, and `Unknown` with
/// zero confidence is the honest default a node carries before any inference
/// runs, so a node always has a meaningful icon and nothing reads as fact.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Classification {
    /// The inferred device class.
    pub device: DeviceClass,
    /// Confidence in `device`, `0..=100`.
    pub device_confidence: u8,
    /// The inferred OS family.
    pub os: OsFamily,
    /// Confidence in `os`, `0..=100`.
    pub os_confidence: u8,
}

/// A node's content-derived identity, so re-ingesting a source updates a node
/// in place rather than duplicating it.
///
/// Text identities (a service instance, a switch's chassis id) hold the
/// peer-authored, already-escaped display string, which is injective enough to
/// key on.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum NodeKey {
    /// This machine.
    LocalHost,
    /// An interface, by its name.
    Interface(InterfaceName),
    /// A subnet, by the interface it is on and its network/prefix.
    Subnet {
        /// The interface the subnet is reached through.
        iface: InterfaceName,
        /// The network address (host bits cleared).
        network: IpAddr,
        /// The prefix length.
        prefix: u8,
    },
    /// A neighbour, by its address.
    Neighbour(IpAddr),
    /// A service, by its escaped `instance._type` identity.
    Service(String),
    /// A router, by its address.
    Router(IpAddr),
    /// The gateway, by its address.
    Gateway(IpAddr),
    /// A switch, by its escaped chassis/bridge identity.
    Switch(String),
    /// An access point, by its escaped identity.
    AccessPoint(String),
    /// A remote host, by its address.
    RemoteHost(IpAddr),
    /// The WAN sink.
    Beyond,
}

/// A stable handle to a node within one [`Map`], assigned on insertion.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct NodeId(pub u32);

/// How alive a node is believed to be.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Liveness {
    /// No reachability evidence either way.
    #[default]
    Unknown,
    /// Advertised itself (mDNS/DNS-SD) but not actively probed.
    Advertised,
    /// Answered an active reachability probe.
    Reachable,
    /// Failed an active reachability probe.
    Unreachable,
}

/// Where a service is reached, as its host advertised it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Endpoint {
    /// Its DNS-SD service type, as `_ipp._tcp`, display-safe.
    pub service_type: String,
    /// The port it is reached on.
    pub port: u16,
    /// The host name it advertised, display-safe.
    pub host: String,
    /// Each `TXT` string it publishes, display-safe.
    pub text: Vec<String>,
}

impl Endpoint {
    /// The type's own name, `ipp` for `_ipp._tcp`: what a list of services
    /// is short enough to name a host's by.
    #[must_use]
    pub fn short_type(&self) -> &str {
        let name = self
            .service_type
            .strip_prefix('_')
            .unwrap_or(&self.service_type);
        name.split_once('.').map_or(name, |(head, _)| head)
    }

    /// The value of the `TXT` key `key`, matched without regard to case as
    /// DNS-SD keys are; `None` when it is absent or a bare key.
    #[must_use]
    pub fn text_value(&self, key: &str) -> Option<&str> {
        self.text.iter().find_map(|entry| {
            let (name, value) = entry.split_once('=')?;
            name.eq_ignore_ascii_case(key).then_some(value)
        })
    }
}

/// One node on the map.
#[derive(Clone, Debug)]
pub struct Node {
    /// This node's stable handle.
    pub id: NodeId,
    /// Its content identity.
    pub key: NodeKey,
    /// What it is.
    pub kind: NodeKind,
    /// Its display label — already escaped display-safe text.
    pub label: String,
    /// The addresses accrued for it.
    pub addrs: Vec<IpAddr>,
    /// Its hardware address, when one is known.
    pub mac: Option<[u8; 6]>,
    /// Where it is reached, for a service.
    pub endpoint: Option<Endpoint>,
    /// Key/value facts accrued for it (both sides already display-safe).
    pub facts: Vec<(String, String)>,
    /// The passively-inferred class driving its icon.
    pub classification: Classification,
    /// What the classification rests on.
    pub evidence: Vec<Evidence>,
    /// How alive it is believed to be.
    pub liveness: Liveness,
    /// Monotonic nanoseconds of the most recent observation.
    pub last_seen_ns: u64,
    /// Its laid-out position in graph space (set by [`crate::layout()`]).
    pub pos: Point,
}

/// What one edge means.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum EdgeKind {
    /// Host → interface.
    Hosts,
    /// Interface → subnet, or subnet → neighbour.
    OnLink,
    /// Host/neighbour → service.
    Serves,
    /// Interface → router, or router → subnet.
    Routes,
    /// Interface → gateway → beyond.
    Default,
    /// Switch ↔ switch or switch ↔ host port adjacency (layer-2 capture).
    Bridged,
    /// A traceroute path segment.
    Hop,
}

/// One relationship between two nodes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Edge {
    /// The source node.
    pub from: NodeId,
    /// The destination node.
    pub to: NodeId,
    /// What the edge means.
    pub kind: EdgeKind,
}

/// The discovered network as a bounded graph.
#[derive(Debug, Default)]
pub struct Map {
    nodes: Vec<Node>,
    by_key: BTreeMap<NodeKey, NodeId>,
    by_id: BTreeMap<NodeId, usize>,
    edges: Vec<Edge>,
    edge_set: BTreeSet<(NodeId, NodeId, EdgeKind)>,
    next_id: u32,
    truncated: bool,
}

impl Map {
    /// An empty map.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The nodes, in insertion order.
    #[must_use]
    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    /// The edges, in insertion order.
    #[must_use]
    pub fn edges(&self) -> &[Edge] {
        &self.edges
    }

    /// `true` when a node or edge was dropped because a containment bound was
    /// reached, so the view can say the map is incomplete.
    #[must_use]
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    /// `true` when the map holds no nodes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// The node for `id`, if present.
    #[must_use]
    pub fn node(&self, id: NodeId) -> Option<&Node> {
        self.by_id.get(&id).map(|&slot| &self.nodes[slot])
    }

    /// The id interned for `key`, if present.
    #[must_use]
    pub fn id_for(&self, key: &NodeKey) -> Option<NodeId> {
        self.by_key.get(key).copied()
    }

    fn node_mut(&mut self, id: NodeId) -> Option<&mut Node> {
        self.by_id.get(&id).map(|&slot| &mut self.nodes[slot])
    }

    /// Insert the node for `key` (or fetch it if already present), refreshing
    /// its last-seen stamp. Returns `None` only when the node bound is reached
    /// and the key is new, setting [`Map::truncated`].
    pub fn intern(&mut self, key: NodeKey, kind: NodeKind, now_ns: u64) -> Option<NodeId> {
        if let Some(&id) = self.by_key.get(&key) {
            if let Some(node) = self.node_mut(id) {
                node.last_seen_ns = now_ns;
            }
            return Some(id);
        }
        if self.nodes.len() >= MAX_NODES {
            self.truncated = true;
            return None;
        }
        let id = NodeId(self.next_id);
        self.next_id += 1;
        let slot = self.nodes.len();
        self.nodes.push(Node {
            id,
            key: key.clone(),
            kind,
            label: String::new(),
            addrs: Vec::new(),
            mac: None,
            endpoint: None,
            facts: Vec::new(),
            classification: Classification::default(),
            evidence: Vec::new(),
            liveness: Liveness::default(),
            last_seen_ns: now_ns,
            pos: Point::ORIGIN,
        });
        self.by_key.insert(key, id);
        self.by_id.insert(id, slot);
        Some(id)
    }

    /// Set a node's display label, truncated to [`MAX_LABEL_LEN`] on a `char`
    /// boundary. The caller supplies already-escaped text.
    pub fn set_label(&mut self, id: NodeId, label: &str) {
        let capped = &label[..label.floor_char_boundary(MAX_LABEL_LEN)];
        if let Some(node) = self.node_mut(id) {
            node.label.clear();
            node.label.push_str(capped);
        }
    }

    /// Record an address for a node (deduplicated, bounded).
    pub fn add_addr(&mut self, id: NodeId, addr: IpAddr) {
        if let Some(node) = self.node_mut(id) {
            if node.addrs.len() < MAX_NODE_ADDRS && !node.addrs.contains(&addr) {
                node.addrs.push(addr);
            }
        }
    }

    /// Record a node's hardware address.
    pub fn set_mac(&mut self, id: NodeId, mac: [u8; 6]) {
        if let Some(node) = self.node_mut(id) {
            node.mac = Some(mac);
        }
    }

    /// Record where a service node is reached.
    pub fn set_endpoint(&mut self, id: NodeId, endpoint: Endpoint) {
        if let Some(node) = self.node_mut(id) {
            node.endpoint = Some(endpoint);
        }
    }

    /// Record a fact for a node, replacing any existing value for `key`
    /// (bounded). Both sides are already display-safe.
    pub fn set_fact(&mut self, id: NodeId, key: &str, value: &str) {
        if let Some(node) = self.node_mut(id) {
            if let Some(entry) = node.facts.iter_mut().find(|(k, _)| k == key) {
                entry.1.clear();
                entry.1.push_str(value);
            } else if node.facts.len() < MAX_NODE_FACTS {
                node.facts.push((String::from(key), String::from(value)));
            }
        }
    }

    /// Set a node's inferred classification and the evidence it rests on.
    pub fn set_classification(
        &mut self,
        id: NodeId,
        classification: Classification,
        evidence: Vec<Evidence>,
    ) {
        if let Some(node) = self.node_mut(id) {
            node.classification = classification;
            node.evidence = evidence;
        }
    }

    /// Set a node's liveness.
    pub fn set_liveness(&mut self, id: NodeId, liveness: Liveness) {
        if let Some(node) = self.node_mut(id) {
            node.liveness = liveness;
        }
    }

    /// Set a node's laid-out position.
    pub fn set_pos(&mut self, id: NodeId, pos: Point) {
        if let Some(node) = self.node_mut(id) {
            node.pos = pos;
        }
    }

    /// Add an edge, deduplicated by `(from, to, kind)` and bounded by
    /// [`MAX_EDGES`]. Returns `true` if a new edge was added.
    pub fn link(&mut self, from: NodeId, to: NodeId, kind: EdgeKind) -> bool {
        if from == to {
            return false;
        }
        let triple = (from, to, kind);
        if self.edge_set.contains(&triple) {
            return false;
        }
        if self.edges.len() >= MAX_EDGES {
            self.truncated = true;
            return false;
        }
        self.edges.push(Edge { from, to, kind });
        self.edge_set.insert(triple);
        true
    }
}

#[cfg(test)]
#[path = "model_tests.rs"]
mod tests;
