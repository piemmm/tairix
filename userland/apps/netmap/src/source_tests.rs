use super::*;
use alloc::collections::BTreeMap;

use tairix_abi::net_ipc::{NetAddrFamily, NetNeighborState, NetRouteOrigin, IF_NAME_LEN};
use tairix_abi::sysinfo::{PageRequest, SysinfoRequestHeader};

use crate::model::{NodeKey, NodeKind};

/// A `sysinfod` stand-in: each query answered from its own encoded records,
/// paged as the service pages, or refused.
struct Fixture {
    records: BTreeMap<u16, Vec<Vec<u8>>>,
    refused: Vec<SysinfoQueryId>,
}

impl Transport for Fixture {
    fn query(&self, request: &[u8]) -> Result<Vec<u8>, Errno> {
        let header = SysinfoRequestHeader::from_bytes(request)?;
        if self.refused.contains(&header.query) {
            return Err(Errno::PermissionDenied);
        }
        let payload = &request[SysinfoRequestHeader::WIRE_LEN
            ..SysinfoRequestHeader::WIRE_LEN + header.payload_len as usize];
        let page = PageRequest::from_bytes(payload)?;
        Ok(self
            .records
            .get(&header.query.as_u16())
            .into_iter()
            .flatten()
            .skip(page.offset as usize)
            .take(usize::from(page.limit))
            .flatten()
            .copied()
            .collect())
    }
}

fn eth0() -> [u8; IF_NAME_LEN] {
    let mut name = [0u8; IF_NAME_LEN];
    name[..4].copy_from_slice(b"eth0");
    name
}

fn v4(octets: [u8; 4]) -> [u8; 16] {
    let mut out = [0u8; 16];
    out[..4].copy_from_slice(&octets);
    out
}

fn fixture(refused: Vec<SysinfoQueryId>) -> Fixture {
    let neighbor = NetNeighborRecord {
        interface: eth0(),
        family: NetAddrFamily::V4,
        state: NetNeighborState::Reachable,
        addr: v4([10, 0, 0, 7]),
        mac: Some([2, 0, 0, 0, 0, 7]),
    };
    let route = NetRouteRecord {
        interface: eth0(),
        family: NetAddrFamily::V4,
        prefix: 0,
        origin: NetRouteOrigin::Static,
        dest: [0u8; 16],
        next_hop: Some(v4([10, 0, 0, 1])),
    };
    let mut records = BTreeMap::new();
    records.insert(
        SysinfoQueryId::NET_NEIGHBORS.as_u16(),
        alloc::vec![neighbor.to_le_bytes().to_vec()],
    );
    records.insert(
        SysinfoQueryId::NET_ROUTES.as_u16(),
        alloc::vec![route.to_le_bytes().to_vec()],
    );
    Fixture { records, refused }
}

#[test]
fn a_refresh_maps_every_source_it_may_read() {
    let mut inventory = Inventory::new();
    let notices = Snapshot::read(&fixture(Vec::new())).apply(&mut inventory);
    assert!(notices.is_empty(), "{notices:?}");
    let map = inventory.build(0);
    assert!(map
        .nodes()
        .iter()
        .any(|node| node.kind == NodeKind::Gateway));
    assert!(map
        .id_for(&NodeKey::Neighbour(core::net::IpAddr::V4(
            core::net::Ipv4Addr::new(10, 0, 0, 7)
        )))
        .is_some());
}

#[test]
fn a_refused_source_is_stated_and_the_rest_is_still_mapped() {
    let mut inventory = Inventory::new();
    let notices =
        Snapshot::read(&fixture(alloc::vec![SysinfoQueryId::NET_NEIGHBORS])).apply(&mut inventory);
    assert_eq!(
        notices,
        ["Neighbouring hosts are not shown: reading them needs CAP_SYSINFO_HW."]
    );
    let map = inventory.build(0);
    assert!(map
        .nodes()
        .iter()
        .all(|node| node.kind != NodeKind::Neighbour));
    assert!(map
        .nodes()
        .iter()
        .any(|node| node.kind == NodeKind::Gateway));
}

#[test]
fn a_refresh_replaces_what_the_last_one_held() {
    let mut inventory = Inventory::new();
    Snapshot::read(&fixture(Vec::new())).apply(&mut inventory);
    Snapshot::read(&fixture(alloc::vec![
        SysinfoQueryId::NET_NEIGHBORS,
        SysinfoQueryId::NET_ROUTES
    ]))
    .apply(&mut inventory);
    let map = inventory.build(0);
    assert!(map
        .nodes()
        .iter()
        .all(|node| !matches!(node.kind, NodeKind::Neighbour | NodeKind::Gateway)));
}

/// A fixture with one neighbour at each of `macs`.
fn neighbours(macs: &[[u8; 6]]) -> Fixture {
    let mut fixture = fixture(Vec::new());
    let records = macs
        .iter()
        .zip(10u8..)
        .map(|(mac, last)| {
            NetNeighborRecord {
                interface: eth0(),
                family: NetAddrFamily::V4,
                state: NetNeighborState::Reachable,
                addr: v4([10, 0, 0, last]),
                mac: Some(*mac),
            }
            .to_le_bytes()
            .to_vec()
        })
        .collect();
    fixture
        .records
        .insert(SysinfoQueryId::NET_NEIGHBORS.as_u16(), records);
    fixture
}

fn oui_table(text: &str) -> Vec<u8> {
    tairix_devids::textdb::parse(tairix_devids::DbKind::Oui, text.as_bytes())
        .expect("fixture vets")
        .encode()
}

#[test]
fn each_neighbours_maker_is_named_once_and_escaped() {
    let table = oui_table("008077  Brother industries, LTD.\nb827eb  Pi\\Shop\n");
    let table = DevIds::parse(tairix_devids::DbKind::Oui, table.as_slice()).expect("decodes");
    let fixture = neighbours(&[
        [0x00, 0x80, 0x77, 1, 1, 1],
        [0x00, 0x80, 0x77, 2, 2, 2],
        [0xb8, 0x27, 0xeb, 3, 3, 3],
        [0x02, 0x80, 0x77, 4, 4, 4],
        [0x00, 0x11, 0x22, 5, 5, 5],
    ]);
    let mut snapshot = Snapshot::read(&fixture);
    snapshot.name_vendors(&table);
    assert_eq!(
        snapshot.vendors,
        Ok(BTreeMap::from([
            (0x00_80_77, String::from("Brother industries, LTD.")),
            (0xb8_27_eb, String::from("Pi\\\\Shop")),
        ])),
        "an unlisted or self-chosen address names no one"
    );
}

#[test]
fn makers_the_table_cannot_name_are_stated_once_per_refresh() {
    let mut inventory = Inventory::new();
    let mut snapshot = Snapshot::read(&fixture(Vec::new()));
    snapshot.vendors = Err(String::from("the bundle carries no table"));
    assert_eq!(
        snapshot.apply(&mut inventory),
        ["Hardware makers are not shown: the bundle carries no table."]
    );
}
