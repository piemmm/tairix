//! The DMA-translation paging walks: the units, the isolation groups an owner
//! holds, and each node a unit translates for an owner.
//!
//! The kernel's translation state reaches a tool through the System
//! Information API, gated on the hardware-read authority, as the IRQ table
//! does. Each walk is the generic [`walk_records`](crate::list) loop.

use tairix_abi::sysinfo::{DmaGroupRecord, DmaNodeRecord, DmaUnitRecord, SysinfoQueryId};
use tairix_abi::Errno;

use crate::list::{walk_records, ListError, WalkStep};
use crate::transport::Transport;

/// Records requested per DMA-translation page: as many of the widest of the
/// three records as one reply holds, so a walk costs as few calls as it can.
pub const DMA_PAGE: u16 = tairix_abi::reply_page(DmaNodeRecord::WIRE_LEN);

/// Page through every discovered unit ([`SysinfoQueryId::DMA_UNITS`]) and
/// hand each decoded [`DmaUnitRecord`] to `sink`: those translating, in
/// discovery order, then those stranded.
///
/// The walk fails closed on a reply that is not whole records or a record
/// that does not decode.
///
/// # Errors
///
/// * [`ListError::Call`] — the transport failed, the service denied the
///   query (without `CAP_SYSINFO_HW`), or the reply was structurally invalid.
/// * [`ListError::Sink`] — `sink` refused a record; the walk stops there.
pub fn for_each_dma_unit(
    transport: &dyn Transport,
    sink: impl FnMut(&DmaUnitRecord) -> Result<WalkStep, Errno>,
) -> Result<(), ListError> {
    walk_records(
        transport,
        SysinfoQueryId::DMA_UNITS,
        DmaUnitRecord::WIRE_LEN,
        DMA_PAGE,
        DmaUnitRecord::from_bytes,
        sink,
    )
}

/// Page through the isolation groups an owner has taken
/// ([`SysinfoQueryId::DMA_GROUPS`]), ascending by unit and group.
///
/// # Errors
///
/// As [`for_each_dma_unit`].
pub fn for_each_dma_group(
    transport: &dyn Transport,
    sink: impl FnMut(&DmaGroupRecord) -> Result<WalkStep, Errno>,
) -> Result<(), ListError> {
    walk_records(
        transport,
        SysinfoQueryId::DMA_GROUPS,
        DmaGroupRecord::WIRE_LEN,
        DMA_PAGE,
        DmaGroupRecord::from_bytes,
        sink,
    )
}

/// Page through the nodes units translate for an owner
/// ([`SysinfoQueryId::DMA_NODES`]), ascending by node.
///
/// # Errors
///
/// As [`for_each_dma_unit`].
pub fn for_each_dma_node(
    transport: &dyn Transport,
    sink: impl FnMut(&DmaNodeRecord) -> Result<WalkStep, Errno>,
) -> Result<(), ListError> {
    walk_records(
        transport,
        SysinfoQueryId::DMA_NODES,
        DmaNodeRecord::WIRE_LEN,
        DMA_PAGE,
        DmaNodeRecord::from_bytes,
        sink,
    )
}

#[cfg(test)]
mod tests {
    use super::{for_each_dma_group, for_each_dma_node, for_each_dma_unit, DMA_PAGE};
    use crate::list::{ListError, WalkStep};
    use crate::request::CallError;
    use crate::transport::Transport;
    use alloc::vec::Vec;
    use tairix_abi::sysinfo::{
        DmaFaultSignal, DmaGroupRecord, DmaNodeRecord, DmaOwnerState, DmaTables, DmaUnitFamily,
        DmaUnitRecord, DmaUnitState, PageRequest, SysinfoQueryId, SysinfoRequestHeader,
    };
    use tairix_abi::Errno;

    /// A `sysinfod` stand-in serving `units` many unit records, one group and
    /// one node, or refusing every query.
    struct Fixture {
        units: u32,
        denied: bool,
        malformed: bool,
    }

    fn unit(node: u32) -> DmaUnitRecord {
        DmaUnitRecord {
            node,
            family: DmaUnitFamily::Smmuv3,
            state: DmaUnitState::Translating,
            faults: DmaFaultSignal::Wired,
            tables: DmaTables::FirstStage,
            owners: 2,
            firmware_streams: 0,
            faults_recorded: 0,
            faults_dropped: 0,
            streams_silenced: 0,
        }
    }

    const GROUP: DmaGroupRecord = DmaGroupRecord {
        unit: 1,
        group: 3,
        holder: 8,
        state: DmaOwnerState::Live,
        generation: 2,
    };

    const NODE: DmaNodeRecord = DmaNodeRecord {
        node: 8,
        unit: 1,
        group: 3,
        state: DmaOwnerState::Unconfirmed,
        streams: 1,
        generation: 2,
        mappings: 0,
        mapped_bytes: 0,
    };

    impl Transport for Fixture {
        fn query(&self, request: &[u8]) -> Result<Vec<u8>, Errno> {
            let header = SysinfoRequestHeader::from_bytes(request)?;
            if self.denied {
                return Err(Errno::PermissionDenied);
            }
            let payload = &request[SysinfoRequestHeader::WIRE_LEN..];
            let page = PageRequest::from_bytes(payload)?;
            let records: Vec<Vec<u8>> = if header.query == SysinfoQueryId::DMA_UNITS {
                (0..self.units)
                    .map(|node| unit(node).to_le_bytes().to_vec())
                    .collect()
            } else if header.query == SysinfoQueryId::DMA_GROUPS {
                alloc::vec![GROUP.to_le_bytes().to_vec()]
            } else if header.query == SysinfoQueryId::DMA_NODES {
                alloc::vec![NODE.to_le_bytes().to_vec()]
            } else {
                return Err(Errno::NotImplemented);
            };
            let mut out: Vec<u8> = records
                .iter()
                .skip(page.offset as usize)
                .take(usize::from(page.limit))
                .flatten()
                .copied()
                .collect();
            if self.malformed {
                out.push(0);
            }
            Ok(out)
        }
    }

    #[test]
    fn every_unit_is_walked_across_pages_in_order() {
        let fixture = Fixture {
            units: u32::from(DMA_PAGE) + 3,
            denied: false,
            malformed: false,
        };
        let mut seen = Vec::new();
        for_each_dma_unit(&fixture, |record| {
            seen.push(record.node);
            Ok(WalkStep::Continue)
        })
        .unwrap();
        assert_eq!(seen, (0..u32::from(DMA_PAGE) + 3).collect::<Vec<_>>());
    }

    #[test]
    fn groups_and_nodes_decode_whole() {
        let fixture = Fixture {
            units: 0,
            denied: false,
            malformed: false,
        };
        let mut groups = Vec::new();
        for_each_dma_group(&fixture, |record| {
            groups.push(*record);
            Ok(WalkStep::Continue)
        })
        .unwrap();
        assert_eq!(groups, [GROUP]);
        let mut nodes = Vec::new();
        for_each_dma_node(&fixture, |record| {
            nodes.push(*record);
            Ok(WalkStep::Continue)
        })
        .unwrap();
        assert_eq!(nodes, [NODE]);
    }

    #[test]
    fn a_denial_or_a_torn_reply_fails_the_walk_closed() {
        let denied = Fixture {
            units: 2,
            denied: true,
            malformed: false,
        };
        assert_eq!(
            for_each_dma_unit(&denied, |_| Ok(WalkStep::Continue)),
            Err(ListError::Call(CallError::PermissionDenied))
        );
        let torn = Fixture {
            units: 2,
            denied: false,
            malformed: true,
        };
        let mut delivered = 0;
        assert!(for_each_dma_unit(&torn, |_| {
            delivered += 1;
            Ok(WalkStep::Continue)
        })
        .is_err());
        assert_eq!(delivered, 0, "nothing of a torn reply is delivered");
    }
}
