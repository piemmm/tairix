use super::*;
use core::net::{IpAddr, Ipv4Addr};

fn neighbour(n: u32) -> NodeKey {
    NodeKey::Neighbour(IpAddr::V4(Ipv4Addr::from(n)))
}

#[test]
fn intern_deduplicates_by_key() {
    let mut map = Map::new();
    let a = map
        .intern(NodeKey::LocalHost, NodeKind::LocalHost, 1)
        .unwrap();
    let b = map
        .intern(NodeKey::LocalHost, NodeKind::LocalHost, 2)
        .unwrap();
    assert_eq!(a, b);
    assert_eq!(map.nodes().len(), 1);
    assert_eq!(map.node(a).unwrap().last_seen_ns, 2);
}

#[test]
fn intern_fails_closed_at_the_node_bound() {
    let mut map = Map::new();
    for n in 0..MAX_NODES {
        let key = neighbour(u32::try_from(n).expect("index fits u32"));
        assert!(map.intern(key, NodeKind::Neighbour, 0).is_some());
    }
    assert!(map
        .intern(neighbour(u32::MAX), NodeKind::Neighbour, 0)
        .is_none());
    assert!(map.truncated());
    assert_eq!(map.nodes().len(), MAX_NODES);
}

#[test]
fn set_label_truncates_on_a_char_boundary() {
    let mut map = Map::new();
    let id = map
        .intern(NodeKey::LocalHost, NodeKind::LocalHost, 0)
        .unwrap();
    let label = "é".repeat(MAX_LABEL_LEN);
    map.set_label(id, &label);
    let stored = &map.node(id).unwrap().label;
    assert!(stored.len() <= MAX_LABEL_LEN);
    assert!(stored.chars().all(|c| c == 'é'));
}

#[test]
fn link_deduplicates_and_rejects_self_loops() {
    let mut map = Map::new();
    let a = map
        .intern(NodeKey::LocalHost, NodeKind::LocalHost, 0)
        .unwrap();
    let b = map.intern(neighbour(1), NodeKind::Neighbour, 0).unwrap();
    assert!(map.link(a, b, EdgeKind::OnLink));
    assert!(!map.link(a, b, EdgeKind::OnLink));
    assert!(map.link(a, b, EdgeKind::Serves));
    assert!(!map.link(a, a, EdgeKind::OnLink));
    assert_eq!(map.edges().len(), 2);
}

#[test]
fn addresses_and_facts_dedup_and_replace() {
    let mut map = Map::new();
    let id = map.intern(neighbour(1), NodeKind::Neighbour, 0).unwrap();
    let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
    map.add_addr(id, ip);
    map.add_addr(id, ip);
    assert_eq!(map.node(id).unwrap().addrs.len(), 1);
    map.set_fact(id, "mtu", "1500");
    map.set_fact(id, "mtu", "9000");
    let facts = &map.node(id).unwrap().facts;
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].1, "9000");
}
