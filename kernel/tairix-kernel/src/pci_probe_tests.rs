use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::driver::bus::{Bus, BusDevice};
use tairix_abi::driver::pci::{function_address, requester_id};
use tairix_abi::{HwDeviceClass, HW_NODE_ROOT_ID};
use tairix_pci::topology::{Acs, Function as PciFunction, Header, PortType};
use tairix_sync::SpinLock;

use super::*;

const UNIT: u32 = 0x800A_0000;
const OTHER_UNIT: u32 = 0x800A_0001;

fn at(bus: u8, device: u8, function: u8) -> u64 {
    function_address(bus, device, function).unwrap()
}

fn rid(bus: u8, device: u8, function: u8) -> u16 {
    requester_id(at(bus, device, function))
}

fn endpoint(bus: u8, device: u8, function: u8, vendor: u16, class: u32) -> PciFunction {
    PciFunction {
        address: at(bus, device, function),
        vendor,
        device: 0x1042,
        class,
        header: Header::Endpoint,
        multifunction: false,
        express: None,
        acs: None,
    }
}

fn bridge(bus: u8, device: u8, secondary: u8, subordinate: u8) -> PciFunction {
    PciFunction {
        header: Header::Bridge {
            secondary,
            subordinate,
        },
        ..endpoint(bus, device, 0, 0x8086, 0x06_04_00)
    }
}

/// A root port with ACS, a bridge to conventional PCI below it, and two
/// virtio functions behind that.
fn behind_a_bridge() -> Topology {
    Topology::new(behind_a_bridge_functions()).unwrap()
}

fn behind_a_bridge_functions() -> Vec<PciFunction> {
    vec![
        PciFunction {
            express: Some(PortType::RootPort),
            acs: Some(Acs {
                capable: Acs::ISOLATING,
                enabled: Acs::ISOLATING,
            }),
            ..bridge(0, 0x1c, 1, 2)
        },
        PciFunction {
            express: Some(PortType::PcieToPci),
            ..bridge(1, 0, 2, 2)
        },
        endpoint(2, 1, 0, VIRTIO_PCI_VENDOR_ID, 0x01_00_00),
        endpoint(2, 2, 0, VIRTIO_PCI_VENDOR_ID, 0x09_80_00),
    ]
}

fn streams(unit: u32, first: u16) -> IommuStreams {
    IommuStreams::new(unit, u32::from(first), 1).unwrap()
}

#[test]
fn a_function_is_known_by_its_own_stream_its_aliases_and_its_group() {
    let topology = behind_a_bridge();
    let dma = SegmentDma::new(&topology, &|_| Some(UNIT)).unwrap();
    let identity = dma.of(at(2, 1, 0)).expect("behind the unit");
    assert_eq!(identity.stream, streams(UNIT, rid(2, 1, 0)));
    assert_eq!(identity.aliases.as_slice(), [streams(UNIT, rid(2, 0, 0))]);
    assert_eq!(
        identity.group,
        Some(IommuGroup::new(UNIT, u32::from(rid(1, 0, 0))))
    );
    assert_eq!(
        dma.of(at(2, 2, 0)).and_then(|identity| identity.group),
        identity.group,
        "both behind one alias"
    );
    assert_eq!(dma.of(at(7, 0, 0)), None, "no such function");
}

#[test]
fn a_function_behind_no_unit_has_no_identity() {
    let topology = behind_a_bridge();
    let dma = SegmentDma::new(&topology, &|_| None).unwrap();
    assert_eq!(dma.of(at(2, 1, 0)), None);
}

#[test]
fn a_group_whose_members_sit_behind_two_units_is_unconfinable() {
    let shared = |function, vendor| PciFunction {
        multifunction: true,
        ..endpoint(0, 0x1f, function, vendor, 0x01_06_01)
    };
    let topology = Topology::new(vec![
        shared(0, 0x8086),
        shared(2, 0x8086),
        endpoint(0, 3, 0, VIRTIO_PCI_VENDOR_ID, 0x01_00_00),
    ])
    .unwrap();
    let dma = SegmentDma::new(&topology, &|requester| {
        Some(if requester == rid(0, 0x1f, 0) {
            UNIT
        } else {
            OTHER_UNIT
        })
    })
    .unwrap();
    for function in [0, 2] {
        let identity = dma.of(at(0, 0x1f, function)).expect("behind a unit");
        assert_eq!(identity.group, None, "function {function}");
    }
    assert!(
        dma.of(at(0, 3, 0))
            .and_then(|identity| identity.group)
            .is_some(),
        "a group of its own on one unit"
    );
}

#[test]
fn a_function_tagged_with_more_aliases_than_a_node_names_is_unconfinable() {
    let depth = u8::try_from(HW_NODE_MAX_RESOURCES + 1).unwrap();
    let mut functions: Vec<PciFunction> = (0..depth)
        .map(|bus| bridge(bus, 1, bus + 1, depth))
        .collect();
    functions.push(endpoint(depth, 0, 0, VIRTIO_PCI_VENDOR_ID, 0x01_00_00));
    let topology = Topology::new(functions).unwrap();
    let dma = SegmentDma::new(&topology, &|_| Some(UNIT)).unwrap();
    let identity = dma.of(at(depth, 0, 0)).expect("behind the unit");
    assert_eq!(identity.group, None);
    assert_eq!(identity.aliases.len(), HW_NODE_MAX_RESOURCES);
}

/// Configuration space as each function's command dword, recording every
/// bus-master change; a function at an address it does not hold reads as
/// absent.
struct CommandBus {
    commands: Vec<(u64, u32)>,
    changes: SpinLock<Vec<(u64, bool)>>,
}

impl Bus for CommandBus {
    fn enumerate(&self, _out: &mut [BusDevice]) -> Result<usize, DriverError> {
        Err(DriverError::Unsupported)
    }
}

impl PciBus for CommandBus {
    fn map_bar_window(
        &self,
        _bdf: u64,
        _bar_index: u8,
        _mapper: &dyn tairix_abi::MmioMapper,
    ) -> Result<tairix_abi::RegisterWindow, DriverError> {
        Err(DriverError::Unsupported)
    }

    fn enable_memory_space(&self, _bdf: u64) -> Result<(), DriverError> {
        Err(DriverError::Unsupported)
    }

    fn set_bus_master(&self, bdf: u64, master: bool) -> Result<(), DriverError> {
        self.changes.lock().push((bdf, master));
        Ok(())
    }

    fn assign_bar(
        &self,
        _bdf: u64,
        _bar_index: u8,
        _window_base: u64,
        _window_size: u64,
    ) -> Result<u64, DriverError> {
        Err(DriverError::Unsupported)
    }

    fn read_config(&self, bdf: u64, offset: u16) -> Result<u32, DriverError> {
        assert_eq!(offset, COMMAND_OFFSET);
        Ok(self
            .commands
            .iter()
            .find(|&&(at, _)| at == bdf)
            .map_or(u32::MAX, |&(_, command)| command))
    }

    fn describe_function(&self, _bdf: u64) -> Result<HwNode, DriverError> {
        Err(DriverError::Unsupported)
    }
}

#[test]
fn the_probe_stops_every_function_it_takes_from_firmware_and_no_other() {
    let host_bridge = at(0, 0, 0);
    let lpc = at(0, 0x1f, 0);
    let virtio_alone = at(0, 3, 0);
    let ahci_alone = at(0, 4, 0);
    let virtio_behind = at(0, 5, 0);
    let kept = at(0, 6, 0);
    let quiet = at(0, 7, 0);
    let gone = at(0, 8, 0);
    let topology = Topology::new(vec![
        endpoint(0, 0, 0, 0x8086, 0x06_00_00),
        bridge(0, 1, 1, 1),
        endpoint(0, 3, 0, VIRTIO_PCI_VENDOR_ID, 0x01_00_00),
        endpoint(0, 4, 0, 0x8086, 0x01_06_01),
        endpoint(0, 5, 0, VIRTIO_PCI_VENDOR_ID, 0x01_00_00),
        endpoint(0, 6, 0, 0x8086, 0x0C_03_30),
        endpoint(0, 7, 0, 0x8086, 0x01_06_01),
        endpoint(0, 8, 0, 0x8086, 0x01_06_01),
        endpoint(0, 0x1f, 0, 0x8086, 0x06_01_00),
    ])
    .unwrap();
    let behind = [
        host_bridge,
        at(0, 1, 0),
        virtio_behind,
        kept,
        quiet,
        gone,
        lpc,
    ];
    let dma = SegmentDma::new(&topology, &|requester| {
        behind
            .contains(&tairix_abi::driver::pci::config_address(requester))
            .then_some(UNIT)
    })
    .unwrap();
    let mastering = BUS_MASTER_ENABLE | 0x2;
    let bus = CommandBus {
        commands: [
            host_bridge,
            at(0, 1, 0),
            virtio_alone,
            ahci_alone,
            virtio_behind,
            kept,
            lpc,
        ]
        .into_iter()
        .map(|address| (address, mastering))
        .chain([(quiet, 0x2)])
        .collect(),
        changes: SpinLock::new(Vec::new()),
    };
    let keeps = |stream: IommuStreams| stream.contains(u32::from(rid(0, 6, 0)));
    stop_mastering(&bus, &topology, &dma, &keeps).unwrap();
    assert_eq!(
        *bus.changes.lock(),
        [(virtio_alone, false), (virtio_behind, false), (lpc, false),],
        "every virtio function and every function behind a unit, the LPC bridge \
         included — but no host bridge, no bridge to a bus, nothing firmware \
         keeps, nothing already quiet and nothing gone"
    );
}

#[test]
fn the_record_holds_each_function_behind_a_unit_and_each_published_node() {
    let mut functions = behind_a_bridge_functions();
    functions.push(endpoint(0, 0, 0, 0x8086, 0x06_00_00));
    let topology = Topology::new(functions).unwrap();
    let dma = SegmentDma::new(&topology, &|requester| {
        (requester != rid(2, 2, 0)).then_some(UNIT)
    })
    .unwrap();
    let published = |id, bus, device| {
        let mut node = HwNode::new(id, HW_NODE_ROOT_ID, HwDeviceClass::Storage);
        node.set_address(u32::from(rid(bus, device, 0)));
        node
    };
    let functions =
        record_functions(&topology, &dma, &[published(40, 2, 1), published(41, 2, 2)]).unwrap();
    assert_eq!(
        functions,
        [
            Function {
                address: at(2, 1, 0),
                node: Some(40),
                stream: Some(streams(UNIT, rid(2, 1, 0))),
            },
            Function {
                address: at(2, 2, 0),
                node: Some(41),
                stream: None,
            },
        ],
        "no bridge or host bridge, and an untranslated node with no stream"
    );
}

/// A walk that formed no hierarchy resolves no unit's scope: where a unit
/// covers the segment every function mastering DMA of its own is stopped,
/// and every bridge, so none forwards what lies below it; elsewhere only a
/// virtio one; and a host bridge never.
#[test]
fn a_walk_that_formed_no_hierarchy_stops_every_master_a_unit_could_cover() {
    let virtio = endpoint(1, 0, 0, VIRTIO_PCI_VENDOR_ID, 0x02_00_00);
    let other = endpoint(1, 1, 0, 0x8086, 0x02_00_00);
    let host = endpoint(0, 0, 0, 0x8086, 0x06_00_00);
    let forwarding = bridge(0, 0x1c, 1, 2);
    let stopped = |covered: bool| {
        [&virtio, &other, &host, &forwarding].map(|function| stopped_unresolved(function, covered))
    };
    assert_eq!(stopped(true), [true, true, false, true]);
    assert_eq!(stopped(false), [true, false, false, false]);
}
