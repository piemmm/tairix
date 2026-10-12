use super::*;
use alloc::vec::Vec;
use core::net::{IpAddr, Ipv4Addr};

use tairix_abi::net_ipc::{
    NetAddrFamily, NetAddrState, NetIfAddr, NetInterfaceStateRecord, IF_NAME_LEN,
};
use tairix_geometry::Point;

use crate::ingest::Inventory;
use crate::model::{Map, NodeKey, NodeKind};

fn name(text: &str) -> [u8; IF_NAME_LEN] {
    let mut out = [0u8; IF_NAME_LEN];
    out[..text.len()].copy_from_slice(text.as_bytes());
    out
}

fn v4_addr(a: u8, b: u8, c: u8, d: u8) -> NetIfAddr {
    let mut addr = [0u8; 16];
    addr[..4].copy_from_slice(&[a, b, c, d]);
    NetIfAddr {
        family: NetAddrFamily::V4,
        prefix: 24,
        state: NetAddrState::Preferred,
        addr,
    }
}

fn state(name_bytes: [u8; IF_NAME_LEN], addr: NetIfAddr) -> NetInterfaceStateRecord {
    let mut slots = [NetInterfaceStateRecord::EMPTY_ADDR; 8];
    slots[0] = addr;
    NetInterfaceStateRecord {
        name: name_bytes,
        link_up: true,
        addr_count: 1,
        addrs: slots,
    }
}

fn sample() -> Inventory {
    let mut inv = Inventory::new();
    inv.observe_interface_state(state(name("eth0"), v4_addr(192, 168, 1, 10)));
    inv.observe_interface_state(state(name("wlan0"), v4_addr(10, 0, 0, 10)));
    inv
}

#[test]
fn local_host_sits_at_the_origin() {
    let mut map = sample().build(0);
    layout(&mut map);
    let local = map.id_for(&NodeKey::LocalHost).unwrap();
    assert_eq!(map.node(local).unwrap().pos, Point::ORIGIN);
}

#[test]
fn interfaces_land_on_the_first_ring() {
    let mut map = sample().build(0);
    layout(&mut map);
    for node in map.nodes() {
        if node.kind == NodeKind::Interface {
            let d2 = i64::from(node.pos.x) * i64::from(node.pos.x)
                + i64::from(node.pos.y) * i64::from(node.pos.y);
            let expected = i64::from(RING_STEP) * i64::from(RING_STEP);
            assert!((d2 - expected).abs() <= 4 * i64::from(RING_STEP));
        }
    }
}

#[test]
fn layout_is_deterministic() {
    let fingerprint = |map: &Map| -> Vec<(NodeKind, Point)> {
        let mut rows: Vec<_> = map.nodes().iter().map(|n| (n.kind, n.pos)).collect();
        rows.sort_by(|a, b| {
            a.1.x
                .cmp(&b.1.x)
                .then(a.1.y.cmp(&b.1.y))
                .then(a.0.cmp(&b.0))
        });
        rows
    };
    let mut first = sample().build(0);
    layout(&mut first);
    let mut second = sample().build(99);
    layout(&mut second);
    assert_eq!(fingerprint(&first), fingerprint(&second));
}

#[test]
fn an_orphan_node_is_still_placed() {
    let mut map = Map::new();
    let local = map
        .intern(NodeKey::LocalHost, NodeKind::LocalHost, 0)
        .unwrap();
    let orphan = map
        .intern(
            NodeKey::Neighbour(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 9))),
            NodeKind::Neighbour,
            0,
        )
        .unwrap();
    layout(&mut map);
    assert_eq!(map.node(local).unwrap().pos, Point::ORIGIN);
    assert_ne!(map.node(orphan).unwrap().pos, Point::ORIGIN);
}

/// This machine with two hosts on a ring, the first of them offering
/// `services` services.
fn hosts_offering(services: usize) -> Map {
    let mut map = Map::new();
    let local = map
        .intern(NodeKey::LocalHost, NodeKind::LocalHost, 0)
        .unwrap();
    let mut hosts = Vec::new();
    for n in [2, 3] {
        let key = NodeKey::Neighbour(IpAddr::V4(Ipv4Addr::new(10, 0, 0, n)));
        let host = map.intern(key, NodeKind::Neighbour, 0).unwrap();
        map.link(local, host, crate::model::EdgeKind::OnLink);
        hosts.push(host);
    }
    for i in 0..services {
        let key = NodeKey::Service(alloc::format!("svc{i}._http._tcp"));
        let service = map.intern(key, NodeKind::Service, 0).unwrap();
        map.link(hosts[0], service, crate::model::EdgeKind::Serves);
    }
    map
}

#[test]
fn a_service_is_folded_into_its_host_and_takes_no_wedge() {
    let mut bare = hosts_offering(0);
    layout(&mut bare);
    let mut busy = hosts_offering(12);
    layout(&mut busy);
    let host = |map: &Map, n: u8| {
        let key = NodeKey::Neighbour(IpAddr::V4(Ipv4Addr::new(10, 0, 0, n)));
        map.node(map.id_for(&key).unwrap()).unwrap().pos
    };
    for n in [2, 3] {
        assert_eq!(
            host(&bare, n),
            host(&busy, n),
            "twelve services move no host"
        );
    }
    for node in busy.nodes().iter().filter(|n| n.kind == NodeKind::Service) {
        assert_eq!(node.pos, host(&busy, 2), "{:?} sits on its host", node.key);
    }
}
