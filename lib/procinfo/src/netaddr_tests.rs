//! Unit tests for the one address rendering every surface prints.

use super::{render_if_addr, render_ip, render_mac, render_neighbor, render_route, render_server};

use tairix_abi::net_ipc::{
    NetAddrFamily, NetAddrState, NetIfAddr, NetNeighborRecord, NetNeighborState, NetRouteOrigin,
    NetRouteRecord, NetServerAddr, IF_NAME_LEN,
};

/// A V6 slot holding `groups`.
fn v6(groups: [u16; 8]) -> [u8; 16] {
    let mut out = [0u8; 16];
    for (index, group) in groups.iter().enumerate() {
        out[index * 2..index * 2 + 2].copy_from_slice(&group.to_be_bytes());
    }
    out
}

/// A V4 slot holding `octets`, with the tail left zero as the wire has it.
fn v4(octets: [u8; 4]) -> [u8; 16] {
    let mut out = [0u8; 16];
    out[..4].copy_from_slice(&octets);
    out
}

#[test]
fn v4_renders_dotted_quad_from_the_first_four_bytes() {
    assert_eq!(
        render_ip(NetAddrFamily::V4, &v4([192, 168, 0, 1])),
        "192.168.0.1"
    );
    assert_eq!(render_ip(NetAddrFamily::V4, &v4([0, 0, 0, 0])), "0.0.0.0");
    assert_eq!(
        render_ip(NetAddrFamily::V4, &v4([255, 255, 255, 255])),
        "255.255.255.255"
    );
}

#[test]
fn v6_suppresses_leading_zeros_and_lowercases() {
    assert_eq!(
        render_ip(
            NetAddrFamily::V6,
            &v6([0x2001, 0x0db8, 0x0001, 0x0002, 0x0003, 0x0004, 0x0005, 0x0006])
        ),
        "2001:db8:1:2:3:4:5:6"
    );
    assert_eq!(
        render_ip(
            NetAddrFamily::V6,
            &v6([0xfe80, 0xabcd, 0xef01, 1, 2, 3, 4, 5])
        ),
        "fe80:abcd:ef01:1:2:3:4:5"
    );
}

#[test]
fn v6_compresses_the_leftmost_longest_run_of_two_or_more() {
    // One run of three beats a later run of two.
    assert_eq!(
        render_ip(NetAddrFamily::V6, &v6([0x2001, 0, 0, 0, 1, 0, 0, 1])),
        "2001::1:0:0:1"
    );
    // Equal-length runs: the leftmost wins.
    assert_eq!(
        render_ip(NetAddrFamily::V6, &v6([1, 0, 0, 2, 3, 0, 0, 4])),
        "1::2:3:0:0:4"
    );
    // A lone zero group is never compressed.
    assert_eq!(
        render_ip(NetAddrFamily::V6, &v6([1, 0, 2, 3, 4, 5, 6, 7])),
        "1:0:2:3:4:5:6:7"
    );
}

#[test]
fn v6_renders_the_all_zero_and_loopback_addresses() {
    assert_eq!(render_ip(NetAddrFamily::V6, &v6([0; 8])), "::");
    assert_eq!(
        render_ip(NetAddrFamily::V6, &v6([0, 0, 0, 0, 0, 0, 0, 1])),
        "::1"
    );
    assert_eq!(
        render_ip(NetAddrFamily::V6, &v6([0xfe80, 0, 0, 0, 0, 0, 0, 1])),
        "fe80::1"
    );
}

#[test]
fn v6_compresses_a_trailing_run() {
    assert_eq!(
        render_ip(NetAddrFamily::V6, &v6([0x2001, 0xdb8, 0, 0, 0, 0, 0, 0])),
        "2001:db8::"
    );
}

#[test]
fn a_server_renders_as_its_bare_address() {
    let server = NetServerAddr {
        family: NetAddrFamily::V4,
        addr: v4([10, 0, 0, 53]),
    };
    assert_eq!(render_server(&server), "10.0.0.53");
    let server = NetServerAddr {
        family: NetAddrFamily::V6,
        addr: v6([0x2001, 0x4860, 0x4860, 0, 0, 0, 0, 0x8888]),
    };
    assert_eq!(render_server(&server), "2001:4860:4860::8888");
}

#[test]
fn an_interface_address_carries_its_prefix_and_only_a_notable_state() {
    let preferred = NetIfAddr {
        family: NetAddrFamily::V4,
        prefix: 24,
        addr: v4([192, 168, 1, 10]),
        state: NetAddrState::Preferred,
    };
    assert_eq!(render_if_addr(&preferred), "192.168.1.10/24");

    let tentative = NetIfAddr {
        state: NetAddrState::Tentative,
        ..preferred
    };
    assert_eq!(render_if_addr(&tentative), "192.168.1.10/24 (tentative)");

    let deprecated = NetIfAddr {
        family: NetAddrFamily::V6,
        prefix: 64,
        addr: v6([0xfe80, 0, 0, 0, 0, 0, 0, 1]),
        state: NetAddrState::Deprecated,
    };
    assert_eq!(render_if_addr(&deprecated), "fe80::1/64 (deprecated)");
}

#[test]
fn a_mac_renders_as_lowercase_colon_separated_octets() {
    assert_eq!(
        render_mac([0x02, 0xAB, 0x00, 0x0f, 0xC0, 0xFF]),
        "02:ab:00:0f:c0:ff"
    );
}

#[test]
fn a_route_renders_its_destination_next_hop_and_origin() {
    let on_link = NetRouteRecord {
        interface: [0u8; IF_NAME_LEN],
        family: NetAddrFamily::V6,
        prefix: 64,
        origin: NetRouteOrigin::RouterAdvertisement,
        dest: v6([0x2001, 0xdb8, 0, 0, 0, 0, 0, 0]),
        next_hop: None,
    };
    assert_eq!(render_route(&on_link), "2001:db8::/64 (ra)");
    let default_router = NetRouteRecord {
        prefix: 0,
        origin: NetRouteOrigin::DefaultRouter,
        dest: [0u8; 16],
        next_hop: Some(v6([0xfe80, 0, 0, 0, 0, 0, 0, 1])),
        ..on_link
    };
    assert_eq!(
        render_route(&default_router),
        "::/0 via fe80::1 (default-router)"
    );
}

#[test]
fn a_neighbor_renders_its_mac_only_once_resolved() {
    let resolved = NetNeighborRecord {
        interface: [0u8; IF_NAME_LEN],
        family: NetAddrFamily::V4,
        state: NetNeighborState::Delay,
        addr: v4([192, 168, 7, 1]),
        mac: Some([2, 0, 0, 0, 0, 1]),
    };
    assert_eq!(
        render_neighbor(&resolved),
        "192.168.7.1 at 02:00:00:00:00:01 (delay)"
    );
    let resolving = NetNeighborRecord {
        state: NetNeighborState::Incomplete,
        mac: None,
        ..resolved
    };
    assert_eq!(render_neighbor(&resolving), "192.168.7.1 (incomplete)");
}
