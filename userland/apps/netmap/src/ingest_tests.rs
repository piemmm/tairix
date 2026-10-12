use super::*;
use alloc::vec::Vec;

use tairix_abi::net_ipc::{
    NetAddrFamily, NetAddrState, NetIfAddr, NetIfKind, NetInterfaceFactsRecord,
    NetInterfaceStateRecord, NetNeighborRecord, NetNeighborState, NetRouteOrigin, NetRouteRecord,
    IF_NAME_LEN,
};

use crate::model::{EdgeKind, NodeKey, NodeKind};

fn name(text: &str) -> [u8; IF_NAME_LEN] {
    let mut out = [0u8; IF_NAME_LEN];
    out[..text.len()].copy_from_slice(text.as_bytes());
    out
}

fn v4(a: u8, b: u8, c: u8, d: u8) -> NetIfAddr {
    let mut addr = [0u8; 16];
    addr[..4].copy_from_slice(&[a, b, c, d]);
    NetIfAddr {
        family: NetAddrFamily::V4,
        prefix: 24,
        state: NetAddrState::Preferred,
        addr,
    }
}

fn state_with(
    name_bytes: [u8; IF_NAME_LEN],
    up: bool,
    addrs: &[NetIfAddr],
) -> NetInterfaceStateRecord {
    let mut slots = [NetInterfaceStateRecord::EMPTY_ADDR; 8];
    slots[..addrs.len()].copy_from_slice(addrs);
    NetInterfaceStateRecord {
        name: name_bytes,
        link_up: up,
        addr_count: u8::try_from(addrs.len()).expect("few addresses"),
        addrs: slots,
    }
}

fn facts(name_bytes: [u8; IF_NAME_LEN]) -> NetInterfaceFactsRecord {
    NetInterfaceFactsRecord {
        name: name_bytes,
        kind: NetIfKind::Ethernet,
        mac: [0x02, 0x11, 0x22, 0x33, 0x44, 0x55],
        mtu: 1500,
        offloads: 0,
        rx_queues: 1,
    }
}

#[test]
fn one_interface_projects_host_interface_and_subnet() {
    let eth0 = name("eth0");
    let mut inv = Inventory::new();
    inv.observe_interface_facts(facts(eth0));
    inv.observe_interface_state(state_with(eth0, true, &[v4(192, 168, 1, 50)]));

    let map = inv.build(42);
    assert_eq!(map.nodes().len(), 3);
    assert_eq!(map.edges().len(), 2);

    let iface = map.id_for(&NodeKey::Interface(eth0)).unwrap();
    assert_eq!(map.node(iface).unwrap().label, "eth0");
    assert_eq!(map.node(iface).unwrap().kind, NodeKind::Interface);

    assert_eq!(
        map.node(iface).unwrap().mac,
        Some([0x02, 0x11, 0x22, 0x33, 0x44, 0x55])
    );
    let facts = &map.node(iface).unwrap().facts;
    assert!(facts.iter().any(|(k, v)| k == "mtu" && v == "1500"));

    let subnet = map
        .nodes()
        .iter()
        .find(|n| n.kind == NodeKind::Subnet)
        .unwrap();
    assert_eq!(subnet.label, "192.168.1.0/24");

    let has_onlink = map
        .edges()
        .iter()
        .any(|e| e.from == iface && e.to == subnet.id && e.kind == EdgeKind::OnLink);
    assert!(has_onlink);
}

#[test]
fn build_is_deterministic() {
    let eth0 = name("eth0");
    let wlan0 = name("wlan0");
    let mut inv = Inventory::new();
    inv.observe_interface_facts(facts(eth0));
    inv.observe_interface_state(state_with(eth0, true, &[v4(10, 0, 0, 2)]));
    inv.observe_interface_state(state_with(wlan0, false, &[v4(10, 0, 1, 2)]));

    let fingerprint = |map: &Map| -> Vec<(NodeKind, alloc::string::String)> {
        let mut rows: Vec<_> = map
            .nodes()
            .iter()
            .map(|n| (n.kind, n.label.clone()))
            .collect();
        rows.sort();
        rows
    };
    assert_eq!(fingerprint(&inv.build(1)), fingerprint(&inv.build(2)));
}

#[test]
fn ipv6_subnet_network_is_masked() {
    let eth0 = name("eth0");
    let mut addr = [0u8; 16];
    addr[0] = 0xfe;
    addr[1] = 0x80;
    addr[15] = 0x01;
    let v6 = NetIfAddr {
        family: NetAddrFamily::V6,
        prefix: 64,
        state: NetAddrState::Preferred,
        addr,
    };
    let mut inv = Inventory::new();
    inv.observe_interface_state(state_with(eth0, true, &[v6]));

    let map = inv.build(0);
    let subnet = map
        .nodes()
        .iter()
        .find(|n| n.kind == NodeKind::Subnet)
        .unwrap();
    assert_eq!(subnet.label, "fe80::/64");
}

#[test]
fn an_interface_down_is_unreachable() {
    let eth0 = name("eth0");
    let mut inv = Inventory::new();
    inv.observe_interface_state(state_with(eth0, false, &[]));
    let map = inv.build(0);
    let iface = map.id_for(&NodeKey::Interface(eth0)).unwrap();
    assert_eq!(
        map.node(iface).unwrap().liveness,
        crate::model::Liveness::Unreachable
    );
}

fn v4_bytes(a: u8, b: u8, c: u8, d: u8) -> [u8; 16] {
    let mut out = [0u8; 16];
    out[..4].copy_from_slice(&[a, b, c, d]);
    out
}

fn neighbor(iface: [u8; IF_NAME_LEN], addr: [u8; 16], mac: Option<[u8; 6]>) -> NetNeighborRecord {
    NetNeighborRecord {
        interface: iface,
        family: NetAddrFamily::V4,
        state: if mac.is_some() {
            NetNeighborState::Reachable
        } else {
            NetNeighborState::Incomplete
        },
        addr,
        mac,
    }
}

fn route(
    iface: [u8; IF_NAME_LEN],
    dest: [u8; 16],
    prefix: u8,
    next_hop: Option<[u8; 16]>,
) -> NetRouteRecord {
    NetRouteRecord {
        interface: iface,
        family: NetAddrFamily::V4,
        prefix,
        origin: NetRouteOrigin::Static,
        dest,
        next_hop,
    }
}

fn lan() -> (Inventory, [u8; IF_NAME_LEN]) {
    let eth0 = name("eth0");
    let mut inv = Inventory::new();
    inv.observe_interface_state(state_with(eth0, true, &[v4(192, 168, 1, 50)]));
    (inv, eth0)
}

fn edge(map: &Map, from: &NodeKey, to: &NodeKey, kind: EdgeKind) -> bool {
    let (Some(from), Some(to)) = (map.id_for(from), map.id_for(to)) else {
        return false;
    };
    map.edges()
        .iter()
        .any(|e| e.from == from && e.to == to && e.kind == kind)
}

fn ip4(a: u8, b: u8, c: u8, d: u8) -> core::net::IpAddr {
    core::net::IpAddr::V4(core::net::Ipv4Addr::new(a, b, c, d))
}

#[test]
fn a_neighbour_hangs_off_the_subnet_it_is_in_with_its_mac() {
    let (mut inv, eth0) = lan();
    inv.replace_neighbors(alloc::vec![neighbor(
        eth0,
        v4_bytes(192, 168, 1, 7),
        Some([2, 0, 0, 0, 0, 7])
    )]);
    let map = inv.build(0);

    let subnet = NodeKey::Subnet {
        iface: eth0,
        network: ip4(192, 168, 1, 0),
        prefix: 24,
    };
    let host = NodeKey::Neighbour(ip4(192, 168, 1, 7));
    assert!(edge(&map, &subnet, &host, EdgeKind::OnLink));
    let node = map.node(map.id_for(&host).unwrap()).unwrap();
    assert_eq!(node.label, "192.168.1.7");
    assert_eq!(node.mac, Some([2, 0, 0, 0, 0, 7]));
    assert_eq!(node.liveness, crate::model::Liveness::Reachable);
}

#[test]
fn a_neighbours_maker_is_named_and_counts_toward_what_it_is() {
    let (mut inv, eth0) = lan();
    let brother = [0x00, 0x80, 0x77, 0x01, 0x02, 0x03];
    let private = [0x02, 0x80, 0x77, 0x01, 0x02, 0x04];
    inv.replace_neighbors(alloc::vec![
        neighbor(eth0, v4_bytes(192, 168, 1, 20), Some(brother)),
        neighbor(eth0, v4_bytes(192, 168, 1, 21), Some(private)),
    ]);
    inv.replace_vendors(BTreeMap::from([(
        0x00_80_77,
        String::from("Brother industries, LTD."),
    )]));
    let map = inv.build(0);

    let printer = map
        .node(
            map.id_for(&NodeKey::Neighbour(ip4(192, 168, 1, 20)))
                .unwrap(),
        )
        .unwrap();
    assert!(printer
        .facts
        .iter()
        .any(|(k, v)| k == "vendor" && v == "Brother industries, LTD."));
    assert_eq!(
        printer.classification.device,
        crate::model::DeviceClass::Printer
    );
    assert_eq!(
        printer.evidence[0].seen,
        crate::infer::Seen::Vendor(String::from("Brother industries, LTD."))
    );

    let other = map
        .node(
            map.id_for(&NodeKey::Neighbour(ip4(192, 168, 1, 21)))
                .unwrap(),
        )
        .unwrap();
    assert!(
        other.facts.iter().all(|(k, _)| k != "vendor"),
        "an address its owner chose names no maker"
    );
    assert_eq!(
        other.classification,
        crate::model::Classification::default()
    );
}

#[test]
fn only_an_address_its_maker_assigned_has_an_identifier() {
    assert_eq!(assigned_oui([0x00, 0x1b, 0x63, 1, 2, 3]), Some(0x00_1b_63));
    assert_eq!(
        assigned_oui([0x02, 0x1b, 0x63, 1, 2, 3]),
        None,
        "locally administered"
    );
    assert_eq!(
        assigned_oui([0x01, 0x00, 0x5e, 0, 0, 1]),
        None,
        "a group address"
    );
}

#[test]
fn a_neighbour_outside_every_subnet_hangs_off_its_interface() {
    let (mut inv, eth0) = lan();
    inv.replace_neighbors(alloc::vec![neighbor(eth0, v4_bytes(10, 9, 9, 9), None)]);
    let map = inv.build(0);
    assert!(edge(
        &map,
        &NodeKey::Interface(eth0),
        &NodeKey::Neighbour(ip4(10, 9, 9, 9)),
        EdgeKind::OnLink
    ));
}

#[test]
fn the_gateway_is_one_node_with_its_neighbour_mac_and_leads_beyond() {
    let (mut inv, eth0) = lan();
    let gateway = v4_bytes(192, 168, 1, 1);
    inv.replace_routes(alloc::vec![route(eth0, [0u8; 16], 0, Some(gateway))]);
    inv.replace_neighbors(alloc::vec![neighbor(
        eth0,
        gateway,
        Some([2, 0, 0, 0, 0, 1])
    )]);
    let map = inv.build(0);

    assert!(map
        .id_for(&NodeKey::Neighbour(ip4(192, 168, 1, 1)))
        .is_none());
    let key = NodeKey::Gateway(ip4(192, 168, 1, 1));
    let gw = map.node(map.id_for(&key).unwrap()).unwrap();
    assert_eq!(gw.kind, NodeKind::Gateway);
    assert_eq!(gw.classification.device, crate::model::DeviceClass::Router);
    assert_eq!(gw.evidence[0].seen, crate::infer::Seen::Gateway);
    assert!(
        gw.mac.is_some(),
        "its neighbour entry gives the gateway its MAC"
    );
    assert!(edge(
        &map,
        &NodeKey::Interface(eth0),
        &key,
        EdgeKind::Default
    ));
    assert!(edge(&map, &key, &NodeKey::Beyond, EdgeKind::Default));
    assert!(edge(
        &map,
        &NodeKey::Subnet {
            iface: eth0,
            network: ip4(192, 168, 1, 0),
            prefix: 24,
        },
        &key,
        EdgeKind::OnLink
    ));
}

#[test]
fn a_named_route_draws_its_router_and_the_network_behind_it() {
    let (mut inv, eth0) = lan();
    inv.replace_routes(alloc::vec![route(
        eth0,
        v4_bytes(10, 20, 0, 0),
        16,
        Some(v4_bytes(192, 168, 1, 254))
    )]);
    let map = inv.build(0);
    let router = NodeKey::Router(ip4(192, 168, 1, 254));
    let remote = NodeKey::Subnet {
        iface: eth0,
        network: ip4(10, 20, 0, 0),
        prefix: 16,
    };
    assert!(edge(
        &map,
        &NodeKey::Interface(eth0),
        &router,
        EdgeKind::Routes
    ));
    assert!(edge(&map, &router, &remote, EdgeKind::Routes));
    assert_eq!(
        map.node(map.id_for(&remote).unwrap()).unwrap().label,
        "10.20.0.0/16"
    );
}

#[test]
fn a_hop_carrying_the_default_and_a_named_route_is_drawn_as_the_gateway() {
    let (mut inv, eth0) = lan();
    let hop = v4_bytes(192, 168, 1, 1);
    // The named route arrives first; the projection still makes it the gateway.
    inv.replace_routes(alloc::vec![
        route(eth0, v4_bytes(10, 0, 0, 0), 8, Some(hop)),
        route(eth0, [0u8; 16], 0, Some(hop)),
    ]);
    let map = inv.build(0);
    assert!(map.id_for(&NodeKey::Router(ip4(192, 168, 1, 1))).is_none());
    let gateway = NodeKey::Gateway(ip4(192, 168, 1, 1));
    assert!(edge(
        &map,
        &gateway,
        &NodeKey::Subnet {
            iface: eth0,
            network: ip4(10, 0, 0, 0),
            prefix: 8,
        },
        EdgeKind::Routes
    ));
}

#[test]
fn an_on_link_route_adds_its_subnet_once() {
    let (mut inv, eth0) = lan();
    inv.replace_routes(alloc::vec![
        route(eth0, v4_bytes(192, 168, 1, 0), 24, None),
        route(eth0, v4_bytes(172, 16, 0, 0), 12, None),
    ]);
    let map = inv.build(0);
    let subnets = map
        .nodes()
        .iter()
        .filter(|n| n.kind == NodeKind::Subnet)
        .count();
    assert_eq!(subnets, 2);
}

fn printer_at(eth0: [u8; IF_NAME_LEN], addresses: Vec<core::net::IpAddr>) -> AdvertisedService {
    AdvertisedService {
        instance: String::from("Office Printer"),
        service: String::from("_ipp._tcp"),
        interface: eth0,
        port: 631,
        host: String::from("printer.local"),
        addresses,
        text: Vec::from([String::from("txtvers=1"), String::from("rp=ipp/print")]),
    }
}

fn service_key() -> NodeKey {
    NodeKey::Service(String::from("Office Printer._ipp._tcp"))
}

#[test]
fn a_service_hangs_off_the_neighbour_it_is_advertised_at() {
    let (mut inv, eth0) = lan();
    inv.replace_neighbors(Vec::from([neighbor(
        eth0,
        v4_bytes(192, 168, 1, 20),
        Some([2, 0, 0, 0, 0, 0x20]),
    )]));
    inv.replace_services(Vec::from([printer_at(
        eth0,
        Vec::from([ip4(192, 168, 1, 20)]),
    )]));
    let map = inv.build(1);
    let host = NodeKey::Neighbour(ip4(192, 168, 1, 20));
    assert!(edge(&map, &host, &service_key(), EdgeKind::Serves));
    let service = map.node(map.id_for(&service_key()).unwrap()).unwrap();
    assert_eq!(service.kind, NodeKind::Service);
    assert_eq!(service.label, "Office Printer");
    assert_eq!(service.liveness, Liveness::Advertised);
    assert_eq!(
        service.endpoint,
        Some(crate::model::Endpoint {
            service_type: String::from("_ipp._tcp"),
            port: 631,
            host: String::from("printer.local"),
            text: Vec::from([String::from("txtvers=1"), String::from("rp=ipp/print")]),
        })
    );
    assert_eq!(service.addrs, [ip4(192, 168, 1, 20)], "reached at its host");
}

#[test]
fn a_host_only_discovery_knows_is_drawn_on_its_subnet() {
    let (mut inv, eth0) = lan();
    inv.replace_services(Vec::from([printer_at(
        eth0,
        Vec::from([ip4(192, 168, 1, 30)]),
    )]));
    let mut names = BTreeMap::new();
    names.insert(ip4(192, 168, 1, 30), String::from("printer.local"));
    inv.replace_segment_names(names);
    let map = inv.build(1);
    let host = NodeKey::Neighbour(ip4(192, 168, 1, 30));
    let subnet = NodeKey::Subnet {
        iface: eth0,
        network: ip4(192, 168, 1, 0),
        prefix: 24,
    };
    assert!(edge(&map, &subnet, &host, EdgeKind::OnLink));
    assert!(edge(&map, &host, &service_key(), EdgeKind::Serves));
    let node = map.node(map.id_for(&host).unwrap()).unwrap();
    assert_eq!(node.label, "printer.local");
    assert_eq!(node.liveness, Liveness::Advertised);
}

#[test]
fn a_service_whose_host_is_not_yet_looked_up_waits_under_its_interface() {
    let (mut inv, eth0) = lan();
    inv.replace_services(Vec::from([printer_at(eth0, Vec::new())]));
    let map = inv.build(1);
    assert!(edge(
        &map,
        &NodeKey::Interface(eth0),
        &service_key(),
        EdgeKind::Serves
    ));
}

#[test]
fn this_machines_own_service_is_its_own() {
    let (mut inv, eth0) = lan();
    inv.replace_services(Vec::from([printer_at(
        eth0,
        Vec::from([ip4(192, 168, 1, 50)]),
    )]));
    let map = inv.build(1);
    assert!(edge(
        &map,
        &NodeKey::LocalHost,
        &service_key(),
        EdgeKind::Serves
    ));
}

#[test]
fn a_host_is_named_by_dns_before_by_the_segment_and_by_either_before_its_address() {
    let (mut inv, eth0) = lan();
    let named = ip4(192, 168, 1, 21);
    let local_only = ip4(192, 168, 1, 22);
    let bare = ip4(192, 168, 1, 23);
    inv.replace_neighbors(
        [21, 22, 23]
            .map(|last| neighbor(eth0, v4_bytes(192, 168, 1, last), None))
            .into(),
    );
    let mut segment = BTreeMap::new();
    segment.insert(named, String::from("nas.local"));
    segment.insert(local_only, String::from("tv.local"));
    inv.replace_segment_names(segment);
    inv.record_dns_name(named, Some(String::from("nas.example.org")));
    inv.record_dns_name(bare, None);
    let map = inv.build(1);
    let label = |addr| {
        map.node(map.id_for(&NodeKey::Neighbour(addr)).unwrap())
            .unwrap()
            .label
            .clone()
    };
    assert_eq!(label(named), "nas.example.org");
    assert_eq!(label(local_only), "tv.local");
    assert_eq!(label(bare), "192.168.1.23");
    let facts = &map
        .node(map.id_for(&NodeKey::Neighbour(named)).unwrap())
        .unwrap()
        .facts;
    assert!(facts
        .iter()
        .any(|(k, v)| k == "local name" && v == "nas.local"));
}

#[test]
fn only_routable_unnamed_hosts_await_reverse_dns() {
    let (mut inv, eth0) = lan();
    let mut link = neighbor(eth0, [0; 16], None);
    link.family = NetAddrFamily::V6;
    link.addr = [0xfe, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x22];
    inv.replace_neighbors(Vec::from([
        neighbor(eth0, v4_bytes(192, 168, 1, 21), None),
        neighbor(eth0, v4_bytes(192, 168, 1, 22), None),
        link,
    ]));
    inv.record_dns_name(ip4(192, 168, 1, 22), None);
    assert_eq!(
        inv.unnamed_hosts().collect::<Vec<_>>(),
        [ip4(192, 168, 1, 21)],
        "a host already answered is not asked again"
    );
    let link_local: Vec<_> = inv.link_local_hosts().into_iter().collect();
    assert_eq!(link_local.len(), 1);
    assert!(is_link_local_address(link_local[0]));
}
