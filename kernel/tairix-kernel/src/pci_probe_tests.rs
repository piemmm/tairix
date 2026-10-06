use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::driver::bus::{Bus, BusDevice};
use tairix_abi::driver::pci::{function_address, requester_id};
use tairix_abi::{HwDeviceClass, HwResource, HW_NODE_ROOT_ID};
use tairix_pci::topology::{Acs, Function as PciFunction, Header, PortType};
use tairix_sync::SpinLock;

use super::*;
use crate::test_support::NullSink;

const UNIT: u32 = 0x800A_0000;
const OTHER_UNIT: u32 = 0x800A_0001;

pub(crate) fn at(bus: u8, device: u8, function: u8) -> u64 {
    function_address(bus, device, function).unwrap()
}

fn rid(bus: u8, device: u8, function: u8) -> u16 {
    requester_id(at(bus, device, function))
}

pub(crate) fn endpoint(bus: u8, device: u8, function: u8, vendor: u16, class: u32) -> PciFunction {
    PciFunction {
        address: at(bus, device, function),
        vendor,
        device: 0x1042,
        class,
        header: Header::Endpoint,
        multifunction: false,
        express: None,
        external_facing: false,
        acs: None,
        ats: None,
        pri: None,
        pasid: None,
        sriov: None,
    }
}

pub(crate) fn bridge(bus: u8, device: u8, secondary: u8, subordinate: u8) -> PciFunction {
    PciFunction {
        header: Header::Bridge {
            secondary,
            subordinate,
            ari: false,
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

/// A unit map as a closure, with no firmware alias and no master off the
/// segment.
impl<F: Fn(u16) -> Option<(u32, u32)>> Streams for F {
    fn stream(&self, requester: u16) -> Option<(u32, u32)> {
        self(requester)
    }

    fn firmware_alias(&self, _requester: u16) -> Option<u16> {
        None
    }

    fn contested(&self, _unit: u32, _stream: u32) -> bool {
        false
    }
}

/// `unit` knows every function by its requester id, as a VT-d unit does.
fn by_requester(unit: u32) -> impl Fn(u16) -> Option<(u32, u32)> {
    move |requester| Some((unit, u32::from(requester)))
}

/// Every function by its requester id on [`UNIT`], and the firmware aliases
/// in `aliases`.
struct FirmwareAliased(Vec<(u16, u16)>);

impl Streams for FirmwareAliased {
    fn stream(&self, requester: u16) -> Option<(u32, u32)> {
        Some((UNIT, u32::from(requester)))
    }

    fn firmware_alias(&self, requester: u16) -> Option<u16> {
        self.0
            .iter()
            .find(|&&(of, _)| of == requester)
            .map(|&(_, alias)| alias)
    }

    fn contested(&self, _unit: u32, _stream: u32) -> bool {
        false
    }
}

/// A firmware alias inside the function's own group is translated with its
/// own stream; one a function of another group arrives under, or is, leaves
/// both unconfined, as either could master into the other's domain.
#[test]
fn a_firmware_alias_joins_the_identity_only_inside_its_group() {
    let topology = behind_a_bridge();
    let inside = FirmwareAliased(vec![
        (rid(2, 1, 0), rid(2, 0, 0)),
        (rid(2, 2, 0), rid(2, 0, 7)),
    ]);
    let dma = SegmentDma::new(&topology, &inside).unwrap();
    let identity = dma.of(at(2, 2, 0)).unwrap();
    assert!(identity.group.is_some());
    assert_eq!(
        identity.aliases.as_slice(),
        [streams(UNIT, rid(2, 0, 0)), streams(UNIT, rid(2, 0, 7))],
        "the walk's alias and firmware's"
    );
    assert_eq!(
        identity.interrupts,
        InterruptSource::Requester(rid(2, 0, 7)),
        "its interrupts arrive as firmware says its requests do"
    );

    let shared = |function, vendor| PciFunction {
        multifunction: false,
        ..endpoint(0, function, 0, vendor, 0x01_06_01)
    };
    let apart = Topology::new(vec![shared(3, 0x8086), shared(4, 0x8086)]).unwrap();
    let crossing = FirmwareAliased(vec![(rid(0, 3, 0), rid(0, 4, 0))]);
    let dma = SegmentDma::new(&apart, &crossing).unwrap();
    assert_eq!(
        dma.of(at(0, 3, 0)).and_then(|identity| identity.group),
        None
    );
    assert_eq!(
        dma.of(at(0, 4, 0)).and_then(|identity| identity.group),
        None,
        "the function aliased to is reachable through the alias"
    );
}

/// Two functions apart in the fabric that the unit cannot tell apart — a
/// masked map folding their requester ids onto one stream — are both left
/// unconfined; a function of a third group keeps its own.
#[test]
fn a_stream_two_groups_master_as_leaves_every_function_using_it_unconfined() {
    let alone = |device| endpoint(0, device, 0, 0x8086, 0x01_06_01);
    let topology = Topology::new(vec![alone(2), alone(3), alone(9)]).unwrap();
    // Devices 2 and 3 differ only in requester-id bit 3.
    let folded = |requester: u16| Some((UNIT, u32::from(requester & !0x0008)));
    let dma = SegmentDma::new(&topology, &folded).unwrap();
    assert_eq!(
        dma.of(at(0, 2, 0)).and_then(|identity| identity.group),
        None
    );
    assert_eq!(
        dma.of(at(0, 3, 0)).and_then(|identity| identity.group),
        None
    );
    assert!(dma
        .of(at(0, 9, 0))
        .and_then(|identity| identity.group)
        .is_some());
}

/// Every function of one segment by its requester id on [`UNIT`], where
/// a master off the segment also uses `taken`.
struct Contested {
    taken: u32,
}

impl Streams for Contested {
    fn stream(&self, requester: u16) -> Option<(u32, u32)> {
        Some((UNIT, u32::from(requester)))
    }

    fn firmware_alias(&self, _requester: u16) -> Option<u16> {
        None
    }

    fn contested(&self, unit: u32, stream: u32) -> bool {
        unit == UNIT && stream == self.taken
    }
}

#[test]
fn a_stream_a_master_off_the_segment_uses_leaves_its_function_unconfined() {
    let alone = |device| endpoint(0, device, 0, 0x8086, 0x01_06_01);
    let topology = Topology::new(vec![alone(3), alone(4)]).unwrap();
    let dma = SegmentDma::new(
        &topology,
        &Contested {
            taken: u32::from(rid(0, 4, 0)),
        },
    )
    .unwrap();
    assert!(dma
        .of(at(0, 3, 0))
        .and_then(|identity| identity.group)
        .is_some());
    assert_eq!(
        dma.of(at(0, 4, 0)).and_then(|identity| identity.group),
        None
    );
}

/// A unit numbering the segment's streams from an offset, as an `iommu-map`
/// does for a second host on a shared unit, names each group by its members'
/// least stream rather than by a requester id another segment repeats.
#[test]
fn a_group_is_named_by_its_members_least_stream() {
    let topology = behind_a_bridge();
    let offset = |requester: u16| Some((UNIT, 0x1_0000 + u32::from(requester)));
    let dma = SegmentDma::new(&topology, &offset).unwrap();
    let expected = Some(IommuGroup::new(UNIT, 0x1_0000 + u32::from(rid(1, 0, 0))));
    for member in [at(1, 0, 0), at(2, 1, 0), at(2, 2, 0)] {
        assert_eq!(dma.of(member).and_then(|identity| identity.group), expected);
    }
}

#[test]
fn a_function_is_known_by_its_own_stream_its_aliases_and_its_group() {
    let topology = behind_a_bridge();
    let dma = SegmentDma::new(&topology, &by_requester(UNIT)).unwrap();
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
        let unit = if requester == rid(0, 0x1f, 0) {
            UNIT
        } else {
            OTHER_UNIT
        };
        Some((unit, u32::from(requester)))
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
    let dma = SegmentDma::new(&topology, &by_requester(UNIT)).unwrap();
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

    fn set_intx(&self, _bdf: u64, _raise: bool) -> Result<(), DriverError> {
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
            .then_some((UNIT, u32::from(requester)))
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
        (requester != rid(2, 2, 0)).then_some((UNIT, u32::from(requester)))
    })
    .unwrap();
    let published = |id, bus, device| {
        let mut node = HwNode::new(id, HW_NODE_ROOT_ID, HwDeviceClass::Storage);
        node.set_address(PciAddress::new(0, rid(bus, device, 0)).node_address());
        node
    };
    let mut elsewhere = published(42, 2, 1);
    elsewhere.set_address(PciAddress::new(1, rid(2, 1, 0)).node_address());
    let functions = record_functions(
        0,
        &topology,
        &dma,
        &[published(40, 2, 1), published(41, 2, 2), elsewhere],
    )
    .unwrap();
    assert_eq!(
        functions,
        [
            Function {
                address: at(2, 1, 0),
                node: Some(40),
                stream: Some(streams(UNIT, rid(2, 1, 0))),
                interrupts: Some(tairix_kernel_core::iommu::InterruptSource::Buses {
                    first: 2,
                    last: 2
                }),
            },
            Function {
                address: at(2, 2, 0),
                node: Some(41),
                stream: None,
                interrupts: None,
            },
        ],
        "no bridge or host bridge, an untranslated node with no stream, and nothing \
         another segment's probe published"
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

/// A function below an external-facing port whose port cannot check
/// requester ids could present any other device's: no domain can confine it.
#[test]
fn an_untrusted_function_below_a_port_without_source_validation_is_unconfinable() {
    let port = |acs| PciFunction {
        express: Some(PortType::RootPort),
        external_facing: true,
        acs: Some(acs),
        ..bridge(0, 0x1c, 1, 1)
    };
    let below = endpoint(1, 0, 0, VIRTIO_PCI_VENDOR_ID, 0x01_00_00);
    let group_of = |acs| {
        let topology = Topology::new(vec![port(acs), below]).unwrap();
        SegmentDma::new(&topology, &by_requester(UNIT))
            .unwrap()
            .of(at(1, 0, 0))
            .and_then(|identity| identity.group)
    };
    let isolating = Acs {
        capable: Acs::ISOLATING,
        enabled: Acs::ISOLATING,
    };
    assert!(
        group_of(isolating).is_some(),
        "a validating port confines it"
    );
    let unchecked = Acs {
        capable: Acs::ISOLATING & !Acs::SOURCE_VALIDATION,
        enabled: Acs::ISOLATING & !Acs::SOURCE_VALIDATION,
    };
    assert_eq!(group_of(unchecked), None);
}

/// A unit that knows a function by a stream of its own numbering — an
/// `iommu-map` translating requester ids — gets the function and its aliases
/// under those streams; an alias the function's unit does not translate
/// leaves it unconfinable.
#[test]
fn a_function_is_known_by_the_streams_its_unit_maps_its_requester_ids_to() {
    let topology = behind_a_bridge();
    let mapped = |requester: u16| Some((UNIT, 0x1_0000 + u32::from(requester)));
    let dma = SegmentDma::new(&topology, &mapped).unwrap();
    let identity = dma.of(at(2, 1, 0)).unwrap();
    assert_eq!(identity.stream.first(), 0x1_0000 + u32::from(rid(2, 1, 0)));
    assert_eq!(
        identity.aliases.as_slice()[0].first(),
        0x1_0000 + u32::from(rid(2, 0, 0))
    );
    let elsewhere = |requester: u16| {
        let unit = if requester == rid(2, 0, 0) {
            OTHER_UNIT
        } else {
            UNIT
        };
        Some((unit, u32::from(requester)))
    };
    let split = SegmentDma::new(&topology, &elsewhere).unwrap();
    assert_eq!(split.of(at(2, 1, 0)).unwrap().group, None);
}

/// What a fake segment's probe saw and did.
#[derive(Default)]
struct Seen {
    confinement: Option<Confinement>,
    external_asked: Vec<u64>,
    quiesced: usize,
}

/// A segment whose walk yields `functions`, or fails where `refuse` says so.
struct FakeSegment {
    functions: Vec<PciFunction>,
    refuse: bool,
    seen: alloc::sync::Arc<SpinLock<Seen>>,
}

impl Bus for FakeSegment {
    fn enumerate(&self, _out: &mut [BusDevice]) -> Result<usize, DriverError> {
        Ok(0)
    }
}

impl tairix_abi::driver::virtio_pci::VirtioPciBus for FakeSegment {
    fn virtio_window_region(&self, _bdf: u64, _cfg: u8) -> Result<(u64, usize), DriverError> {
        Err(DriverError::Unsupported)
    }

    fn notify_off_multiplier(&self, _bdf: u64) -> Result<u32, DriverError> {
        Err(DriverError::Unsupported)
    }
}

impl tairix_abi::driver::msix::MsixBus for FakeSegment {
    fn route_msix(
        &self,
        _bdf: u64,
        _entry: u16,
        _message: tairix_abi::MsiMessage,
        _mapper: &dyn tairix_abi::MmioMapper,
    ) -> Result<(), DriverError> {
        Err(DriverError::Unsupported)
    }
}

impl PciBus for FakeSegment {
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

    fn set_bus_master(&self, _bdf: u64, _master: bool) -> Result<(), DriverError> {
        Ok(())
    }

    fn set_intx(&self, _bdf: u64, _raise: bool) -> Result<(), DriverError> {
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

    fn read_config(&self, _bdf: u64, _offset: u16) -> Result<u32, DriverError> {
        Ok(0)
    }

    fn describe_function(&self, _bdf: u64) -> Result<HwNode, DriverError> {
        Err(DriverError::Unsupported)
    }
}

impl PciTopology for FakeSegment {
    fn topology(
        &self,
        confinement: Confinement,
        external: &dyn Fn(u64) -> bool,
    ) -> Result<Topology, DriverError> {
        let mut seen = self.seen.lock();
        seen.confinement = Some(confinement);
        for function in &self.functions {
            if external(function.address) {
                seen.external_asked.push(function.address);
            }
        }
        if self.refuse {
            return Err(DriverError::DeviceFault);
        }
        Ok(Topology::new(self.functions.clone()).unwrap())
    }

    fn quiesce(
        &self,
        _stopped: &dyn Fn(&PciFunction) -> bool,
    ) -> tairix_abi::driver::pci::Quiesced {
        self.seen.lock().quiesced += 1;
        tairix_abi::driver::pci::Quiesced::default()
    }
}

/// Units that cover the segments in `covered`, describe themselves unless
/// told not to, strand the segments in `stranded`, and know every function
/// by its requester id on unit [`UNIT`].
struct FakeUnits {
    covered: Vec<u16>,
    stranded: Vec<u16>,
    describable: bool,
}

impl UnitTopology for FakeUnits {
    fn covers(&self, segment: u16) -> bool {
        self.covered.contains(&segment)
    }

    fn firmware_alias(&self, _segment: u16, _requester: u16) -> Option<u16> {
        None
    }

    fn emit(
        &mut self,
        _walks: &[(u16, &Topology)],
        sink: &mut dyn HwNodeSink,
        _log: &dyn Sink,
    ) -> Result<(), Unconfined> {
        if !self.describable {
            return Err(Unconfined);
        }
        let mut unit = HwNode::new(UNIT, HW_NODE_ROOT_ID, HwDeviceClass::Iommu);
        unit.push_resource(HwResource::iommu_reserved_window(
            tairix_abi::IommuReservedWindow::new(
                u32::from(rid(0, 6, 0)),
                0x7000_0000,
                0x1000,
                tairix_abi::ReservedAccess::ReadWrite,
            )
            .unwrap(),
        ))
        .unwrap();
        sink.emit(unit).map_err(|_| Unconfined)
    }

    fn strands(&self, segment: u16) -> bool {
        self.stranded.contains(&segment)
    }

    fn stream(&self, segment: u16, _walk: &Topology, requester: u16) -> Option<(u32, u32)> {
        self.covers(segment).then_some((UNIT, u32::from(requester)))
    }

    fn contested(&self, _segment: u16, _unit: u32, _stream: u32) -> bool {
        false
    }
}

/// Probe `segments` (number, refuse the walk) with `units`, answering what
/// each segment saw, which segments published at which ordinal, and the
/// owned segments' numbers.
fn run_probe(
    segments: &[(u16, bool)],
    units: &mut FakeUnits,
) -> (
    Vec<alloc::sync::Arc<SpinLock<Seen>>>,
    Vec<PciSegment>,
    Vec<u16>,
) {
    run_probe_logged(segments, units, &[], &NullSink)
}

/// [`run_probe`] with `extra` functions on every segment, recording to `log`.
fn run_probe_logged(
    segments: &[(u16, bool)],
    units: &mut FakeUnits,
    extra: &[PciFunction],
    log: &dyn Sink,
) -> (
    Vec<alloc::sync::Arc<SpinLock<Seen>>>,
    Vec<PciSegment>,
    Vec<u16>,
) {
    let seen: Vec<_> = segments
        .iter()
        .map(|_| alloc::sync::Arc::new(SpinLock::new(Seen::default())))
        .collect();
    let probed = segments
        .iter()
        .zip(&seen)
        .map(|(&(number, refuse), seen)| ProbeSegment {
            number,
            bus: Box::new(FakeSegment {
                functions: [
                    endpoint(0, 3, 0, VIRTIO_PCI_VENDOR_ID, 0x01_00_00),
                    endpoint(0, 6, 0, 0x8086, 0x0C_03_30),
                ]
                .into_iter()
                .chain(extra.iter().copied())
                .collect(),
                refuse,
                seen: alloc::sync::Arc::clone(seen),
            }),
        })
        .collect();
    let mut published = Vec::new();
    let mut publish = |segment: PciSegment,
                       _bus: &dyn HostBus,
                       _topology: &Topology,
                       _functions: &[BusDevice],
                       _dma: DmaIdentity<'_>,
                       _sink: &mut CollectingHwNodeSink| {
        published.push(segment);
    };
    let mut sink = CollectingHwNodeSink::new();
    let owned = probe(
        probed,
        units,
        &|segment, address| segment == 1 && address == at(0, 3, 0),
        &mut publish,
        &mut sink,
        log,
    );
    let host = crate::pci_host::PciHost::new(owned).unwrap();
    let numbers = segments
        .iter()
        .map(|&(number, _)| number)
        .filter(|&number| host.with(number, |_| ()).is_some())
        .collect();
    (seen, published, numbers)
}

#[test]
fn every_segment_is_walked_and_owned_confined_where_a_unit_covers_it() {
    let mut units = FakeUnits {
        covered: vec![1],
        stranded: vec![],
        describable: true,
    };
    let (seen, published, owned) = run_probe(&[(0, false), (1, false)], &mut units);
    assert_eq!(seen[0].lock().confinement, Some(Confinement::Leave));
    assert_eq!(seen[1].lock().confinement, Some(Confinement::Confine));
    assert_eq!(
        published,
        [
            PciSegment {
                number: 0,
                ordinal: 0
            },
            PciSegment {
                number: 1,
                ordinal: 1
            },
        ],
        "each segment numbered by its position"
    );
    assert_eq!(owned, [0, 1]);
    assert_eq!(
        seen[1].lock().external_asked,
        [at(0, 3, 0)],
        "the platform's external ports reach the segment's walk"
    );
    assert!(seen[0].lock().external_asked.is_empty());
}

#[test]
fn a_segment_that_cannot_be_confined_is_stopped_and_publishes_nothing() {
    let mut refused_walk = FakeUnits {
        covered: vec![0, 1],
        stranded: vec![],
        describable: true,
    };
    let (seen, published, owned) = run_probe(&[(0, true), (1, false)], &mut refused_walk);
    assert_eq!(seen[0].lock().quiesced, 1);
    assert_eq!(published.len(), 1, "its neighbour still publishes");
    assert_eq!(owned, [0, 1], "an unconfined segment is still the kernel's");

    let mut stranded = FakeUnits {
        covered: vec![0, 1],
        stranded: vec![1],
        describable: true,
    };
    let (seen, published, _) = run_probe(&[(0, false), (1, false)], &mut stranded);
    assert_eq!(seen[1].lock().quiesced, 1);
    assert_eq!(published.iter().map(|s| s.number).collect::<Vec<_>>(), [0]);

    let mut undescribed = FakeUnits {
        covered: vec![1],
        stranded: vec![],
        describable: false,
    };
    let (seen, published, _) = run_probe(&[(0, false), (1, false)], &mut undescribed);
    assert_eq!(
        (seen[0].lock().quiesced, seen[1].lock().quiesced),
        (0, 1),
        "only the segment a unit covers"
    );
    assert_eq!(published.iter().map(|s| s.number).collect::<Vec<_>>(), [0]);
}

/// Records every event's message.
#[derive(Default)]
struct MessageLog(SpinLock<Vec<&'static str>>);

impl Sink for MessageLog {
    fn write_event(&self, event: &tairix_log::Event<'_>) {
        let known = ["pci functions left translation services or virtual functions on"];
        if let Some(message) = known.iter().find(|known| **known == event.message) {
            self.0.lock().push(message);
        }
    }
}

/// A confined segment whose function will not turn its ATS off is named in
/// the record; a segment no unit covers is walked as firmware left it, so
/// nothing there is named.
#[test]
fn a_confined_function_left_with_ats_on_is_recorded() {
    let stuck = PciFunction {
        express: Some(PortType::Endpoint),
        ats: Some(tairix_pci::topology::Ats { enabled: true }),
        ..endpoint(0, 9, 0, 0x8086, 0x02_00_00)
    };
    let covered = |segments: Vec<u16>| {
        let log = MessageLog::default();
        let mut units = FakeUnits {
            covered: segments,
            stranded: vec![],
            describable: true,
        };
        run_probe_logged(&[(0, false)], &mut units, &[stuck], &log);
        let recorded = log.0.lock().len();
        recorded
    };
    assert_eq!(covered(vec![0]), 1);
    assert_eq!(covered(vec![]), 0);
}

/// An interrupt arrives under the requester ids the fabric gives it: a
/// function's own, every bus below a bridge to conventional PCI, or a
/// conventional bridge's own id.
#[test]
fn a_function_s_interrupts_are_admitted_by_the_ids_its_unit_sees() {
    use tairix_kernel_core::iommu::InterruptSource;

    let topology = behind_a_bridge();
    let dma = SegmentDma::new(&topology, &by_requester(UNIT)).unwrap();
    assert_eq!(
        dma.of(at(2, 1, 0)).unwrap().interrupts,
        InterruptSource::Buses { first: 2, last: 2 },
        "below a bridge to conventional PCI, by bus"
    );
    let conventional = Topology::new(vec![
        bridge(0, 0x1e, 1, 1),
        endpoint(1, 3, 0, VIRTIO_PCI_VENDOR_ID, 0x01_00_00),
        endpoint(0, 4, 0, VIRTIO_PCI_VENDOR_ID, 0x01_00_00),
    ])
    .unwrap();
    let dma = SegmentDma::new(&conventional, &by_requester(UNIT)).unwrap();
    assert_eq!(
        dma.of(at(1, 3, 0)).unwrap().interrupts,
        InterruptSource::Requester(rid(0, 0x1e, 0)),
        "a conventional bridge's own id"
    );
    assert_eq!(
        dma.of(at(0, 4, 0)).unwrap().interrupts,
        InterruptSource::Requester(rid(0, 4, 0)),
        "a function no bridge tags, its own"
    );
}
