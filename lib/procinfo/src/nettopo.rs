//! The network-topology paging walks: the neighbour cache and the routing
//! table.
//!
//! There is no `/proc/net/arp` or `/proc/net/route`: both are read through the
//! typed, capability-checked `sysinfo-v1` queries `sysinfod` forwards to the
//! stack. The paging loop is the shared [`walk_records`]; only the query and
//! the per-record decode live here.

use tairix_abi::net_ipc::{NetNeighborRecord, NetRouteRecord, NETSTACK_LIST_LIMIT_MAX};
use tairix_abi::sysinfo::SysinfoQueryId;
use tairix_abi::Errno;

use crate::list::{walk_records, ListError, WalkStep};
use crate::transport::Transport;

/// Records requested per neighbour or route page: the stack's own list
/// limit, so a page is one round trip to it.
pub const NET_TOPOLOGY_PAGE: u16 = NETSTACK_LIST_LIMIT_MAX;

/// Page through every interface's neighbour cache
/// ([`SysinfoQueryId::NET_NEIGHBORS`]) and hand each decoded
/// [`NetNeighborRecord`] to `sink`, in the stack's interface-table order.
///
/// `sysinfod` serves the query only to a `CAP_SYSINFO_HW` holder, because the
/// entries name other machines' link-layer addresses; a caller without it sees
/// [`CallError::PermissionDenied`](crate::CallError::PermissionDenied), never an
/// empty table. `sink` bounds how much of a long table it accepts by answering
/// [`WalkStep::Stop`].
///
/// # Errors
///
/// As [`for_each_net_socket`](crate::for_each_net_socket).
pub fn for_each_net_neighbor(
    transport: &dyn Transport,
    sink: impl FnMut(&NetNeighborRecord) -> Result<WalkStep, Errno>,
) -> Result<(), ListError> {
    walk_records(
        transport,
        SysinfoQueryId::NET_NEIGHBORS,
        NetNeighborRecord::WIRE_LEN,
        NET_TOPOLOGY_PAGE,
        NetNeighborRecord::from_bytes,
        sink,
    )
}

/// Page through every interface's routes, the IPv6 default routers included
/// ([`SysinfoQueryId::NET_ROUTES`]), and hand each decoded [`NetRouteRecord`]
/// to `sink`, in the stack's interface-table order.
///
/// `sysinfod` serves the query only to a `CAP_SYSINFO_GLOBAL` holder.
///
/// # Errors
///
/// As [`for_each_net_socket`](crate::for_each_net_socket).
pub fn for_each_net_route(
    transport: &dyn Transport,
    sink: impl FnMut(&NetRouteRecord) -> Result<WalkStep, Errno>,
) -> Result<(), ListError> {
    walk_records(
        transport,
        SysinfoQueryId::NET_ROUTES,
        NetRouteRecord::WIRE_LEN,
        NET_TOPOLOGY_PAGE,
        NetRouteRecord::from_bytes,
        sink,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request::CallError;
    use crate::testing::PagedFixture;
    use alloc::vec::Vec;
    use tairix_abi::net_ipc::{NetAddrFamily, NetNeighborState, NetRouteOrigin, IF_NAME_LEN};

    fn eth0() -> [u8; IF_NAME_LEN] {
        let mut name = [0u8; IF_NAME_LEN];
        name[..4].copy_from_slice(b"eth0");
        name
    }

    fn neighbor(last: u8) -> NetNeighborRecord {
        let mut addr = [0u8; 16];
        addr[..4].copy_from_slice(&[10, 0, 0, last]);
        NetNeighborRecord {
            interface: eth0(),
            family: NetAddrFamily::V4,
            state: NetNeighborState::Stale,
            addr,
            mac: Some([2, 0, 0, 0, 0, last]),
        }
    }

    fn route() -> NetRouteRecord {
        let mut gateway = [0u8; 16];
        gateway[..4].copy_from_slice(&[10, 0, 0, 1]);
        NetRouteRecord {
            interface: eth0(),
            family: NetAddrFamily::V4,
            prefix: 0,
            origin: NetRouteOrigin::Static,
            dest: [0u8; 16],
            next_hop: Some(gateway),
        }
    }

    #[test]
    fn the_neighbor_walk_routes_its_query_and_pages_the_whole_cache() {
        let count = usize::from(NET_TOPOLOGY_PAGE) + 3;
        let expected: Vec<_> = (0..count)
            .map(|i| neighbor(u8::try_from(i).expect("small")))
            .collect();
        let fixture = PagedFixture::new(expected.iter().map(|r| r.to_le_bytes().to_vec()));
        let mut got = Vec::new();
        for_each_net_neighbor(&fixture, |record| {
            got.push(*record);
            Ok(WalkStep::Continue)
        })
        .expect("walk");
        assert_eq!(got, expected);
        assert_eq!(
            fixture.seen.borrow().as_slice(),
            &[SysinfoQueryId::NET_NEIGHBORS; 2]
        );
    }

    #[test]
    fn the_route_walk_routes_its_query_and_surfaces_a_denial() {
        let mut fixture = PagedFixture::new([route().to_le_bytes().to_vec()]);
        let mut got = Vec::new();
        for_each_net_route(&fixture, |record| {
            got.push(*record);
            Ok(WalkStep::Continue)
        })
        .expect("walk");
        assert_eq!(got, [route()]);
        assert_eq!(
            fixture.seen.borrow().as_slice(),
            &[SysinfoQueryId::NET_ROUTES]
        );

        fixture.deny = true;
        assert_eq!(
            for_each_net_route(&fixture, |_| Ok(WalkStep::Continue)),
            Err(ListError::Call(CallError::PermissionDenied))
        );
    }
}
