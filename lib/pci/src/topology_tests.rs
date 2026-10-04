use alloc::collections::BTreeMap;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::RefCell;

use tairix_abi::driver::pci::{function_address, Quiesced, BUS_MASTER_ENABLE, COMMAND_OFFSET};

use super::*;
use crate::config::{ConfigAddress, ConfigSpace, EXTENDED_REGISTER};
use crate::enumerate::Pci;

fn at(bus: u8, device: u8, function: u8) -> u64 {
    function_address(bus, device, function).unwrap()
}

fn rid(bus: u8, device: u8, function: u8) -> u16 {
    requester_id(at(bus, device, function))
}

fn endpoint(bus: u8, device: u8, function: u8) -> Function {
    Function {
        address: at(bus, device, function),
        vendor: 0x1af4,
        device: 0x1041,
        class: 0x02_00_00,
        header: Header::Endpoint,
        multifunction: false,
        express: None,
        acs: None,
    }
}

fn bridge(bus: u8, device: u8, secondary: u8, subordinate: u8) -> Function {
    Function {
        header: Header::Bridge {
            secondary,
            subordinate,
        },
        class: 0x06_04_00,
        ..endpoint(bus, device, 0)
    }
}

const ENFORCED: Acs = Acs {
    capable: Acs::ISOLATING,
    enabled: Acs::ISOLATING,
};

fn express(function: Function, port: PortType) -> Function {
    Function {
        express: Some(port),
        ..function
    }
}

fn acs(function: Function, acs: Acs) -> Function {
    Function {
        acs: Some(acs),
        ..function
    }
}

fn root_port(device: u8, secondary: u8, subordinate: u8, isolating: bool) -> Function {
    let port = express(
        bridge(0, device, secondary, subordinate),
        PortType::RootPort,
    );
    if isolating {
        acs(port, ENFORCED)
    } else {
        port
    }
}

fn index(topology: &Topology, bus: u8, device: u8, function: u8) -> usize {
    topology.index_of(at(bus, device, function)).unwrap()
}

fn group(topology: &Topology, bus: u8, device: u8, function: u8) -> u16 {
    topology.group(index(topology, bus, device, function))
}

#[test]
fn a_bridge_to_conventional_pci_tags_everything_below_it_with_its_secondary_bus() {
    let topology = Topology::new(vec![
        root_port(0x1c, 1, 2, true),
        express(bridge(1, 0, 2, 2), PortType::PcieToPci),
        endpoint(2, 1, 0),
        endpoint(2, 2, 0),
    ])
    .unwrap();
    let below = index(&topology, 2, 1, 0);
    let aliases: Vec<Alias> = topology.aliases(below).collect();
    assert_eq!(
        aliases,
        [Alias {
            requester: rid(2, 0, 0),
            bridge: index(&topology, 1, 0, 0),
        }],
        "the root port passes it on"
    );
    assert_eq!(group(&topology, 2, 1, 0), rid(1, 0, 0));
    assert_eq!(
        group(&topology, 2, 2, 0),
        rid(1, 0, 0),
        "one alias, one group"
    );
    assert_eq!(group(&topology, 1, 0, 0), rid(1, 0, 0));
    assert_eq!(group(&topology, 0, 0x1c, 0), rid(0, 0x1c, 0));
}

#[test]
fn a_conventional_bridge_tags_requests_with_its_own_id() {
    let topology = Topology::new(vec![bridge(0, 0x1e, 1, 1), endpoint(1, 3, 0)]).unwrap();
    let below = index(&topology, 1, 3, 0);
    let aliases: Vec<u16> = topology
        .aliases(below)
        .map(|alias| alias.requester)
        .collect();
    assert_eq!(aliases, [rid(0, 0x1e, 0)]);
    assert_eq!(topology.group(below), rid(0, 0x1e, 0));
}

#[test]
fn nested_bridges_tag_requests_nearest_first() {
    let topology = Topology::new(vec![
        root_port(1, 1, 3, true),
        express(bridge(1, 0, 2, 3), PortType::PcieToPci),
        bridge(2, 1, 3, 3),
        endpoint(3, 0, 0),
    ])
    .unwrap();
    let aliases: Vec<u16> = topology
        .aliases(index(&topology, 3, 0, 0))
        .map(|alias| alias.requester)
        .collect();
    assert_eq!(aliases, [rid(2, 1, 0), rid(2, 0, 0)]);
    assert_eq!(
        group(&topology, 3, 0, 0),
        rid(1, 0, 0),
        "up to the outermost"
    );
}

#[test]
fn a_root_port_without_acs_shares_a_group_with_everything_below_it() {
    let topology = Topology::new(vec![
        root_port(1, 1, 1, false),
        express(endpoint(1, 0, 0), PortType::Endpoint),
        root_port(2, 2, 2, true),
        express(endpoint(2, 0, 0), PortType::Endpoint),
    ])
    .unwrap();
    assert_eq!(group(&topology, 1, 0, 0), rid(0, 1, 0));
    assert_eq!(group(&topology, 0, 1, 0), rid(0, 1, 0));
    assert_eq!(group(&topology, 2, 0, 0), rid(2, 0, 0), "behind ACS");
    assert_eq!(group(&topology, 0, 2, 0), rid(0, 2, 0));
}

/// A root port, a switch's upstream port, and two downstream ports, each
/// with one endpoint below; `second_isolates` gives the second port ACS.
fn switch(second_isolates: bool) -> Topology {
    let second = express(bridge(2, 1, 4, 4), PortType::DownstreamPort);
    Topology::new(vec![
        root_port(1, 1, 4, true),
        express(bridge(1, 0, 2, 4), PortType::UpstreamPort),
        acs(
            express(bridge(2, 0, 3, 3), PortType::DownstreamPort),
            ENFORCED,
        ),
        if second_isolates {
            acs(second, ENFORCED)
        } else {
            second
        },
        express(endpoint(3, 0, 0), PortType::Endpoint),
        express(endpoint(4, 0, 0), PortType::Endpoint),
    ])
    .unwrap()
}

#[test]
fn a_switch_whose_downstream_ports_enforce_acs_isolates_each() {
    let topology = switch(true);
    let groups = [
        group(&topology, 3, 0, 0),
        group(&topology, 4, 0, 0),
        group(&topology, 2, 0, 0),
        group(&topology, 2, 1, 0),
    ];
    assert_eq!(
        groups,
        [rid(3, 0, 0), rid(4, 0, 0), rid(2, 0, 0), rid(2, 1, 0)]
    );
}

/// The port without ACS lets its device's requests across to its sibling's
/// window, so grouping by the path alone — which isolates the first device —
/// would let two owners share the switch.
#[test]
fn one_downstream_port_without_acs_puts_the_whole_switch_in_one_group() {
    let topology = switch(false);
    for (bus, device) in [(3, 0), (4, 0), (2, 0), (2, 1), (1, 0)] {
        assert_eq!(
            group(&topology, bus, device, 0),
            rid(1, 0, 0),
            "{bus}:{device}"
        );
    }
    assert_eq!(group(&topology, 0, 1, 0), rid(0, 1, 0), "above the switch");
}

/// The bridge tags its conventional devices' DMA with its secondary bus, so
/// they group at it; every port above it isolates, so the group stops there,
/// clear of its switch's other port.
#[test]
fn an_alias_below_an_isolating_switch_port_groups_at_its_bridge() {
    let topology = Topology::new(vec![
        root_port(1, 1, 5, true),
        express(bridge(1, 0, 2, 5), PortType::UpstreamPort),
        acs(
            express(bridge(2, 0, 3, 4), PortType::DownstreamPort),
            ENFORCED,
        ),
        acs(
            express(bridge(2, 1, 5, 5), PortType::DownstreamPort),
            ENFORCED,
        ),
        express(bridge(3, 0, 4, 4), PortType::PcieToPci),
        endpoint(4, 1, 0),
        endpoint(4, 2, 0),
        express(endpoint(5, 0, 0), PortType::Endpoint),
    ])
    .unwrap();
    let aliases: Vec<u16> = topology
        .aliases(index(&topology, 4, 1, 0))
        .map(|alias| alias.requester)
        .collect();
    assert_eq!(aliases, [rid(4, 0, 0)]);
    for (bus, device) in [(4, 1), (4, 2), (3, 0)] {
        assert_eq!(
            group(&topology, bus, device, 0),
            rid(3, 0, 0),
            "{bus}:{device}"
        );
    }
    assert_eq!(group(&topology, 2, 0, 0), rid(2, 0, 0), "the port above");
    assert_eq!(group(&topology, 5, 0, 0), rid(5, 0, 0), "the other port's");
}

#[test]
fn an_endpoint_beside_a_switch_s_ports_reaches_them_without_a_port() {
    let topology = Topology::new(vec![
        root_port(1, 1, 3, true),
        express(bridge(1, 0, 2, 3), PortType::UpstreamPort),
        acs(
            express(bridge(2, 0, 3, 3), PortType::DownstreamPort),
            ENFORCED,
        ),
        express(endpoint(2, 5, 0), PortType::Endpoint),
        express(endpoint(3, 0, 0), PortType::Endpoint),
    ])
    .unwrap();
    assert_eq!(group(&topology, 3, 0, 0), group(&topology, 2, 5, 0));
}

#[test]
fn functions_sharing_a_slot_without_acs_share_a_group() {
    let function = |number, isolating| {
        let mut function = express(endpoint(1, 0, number), PortType::Endpoint);
        function.multifunction = true;
        if isolating {
            acs(function, ENFORCED)
        } else {
            function
        }
    };
    let shared = Topology::new(vec![
        root_port(1, 1, 1, true),
        function(0, false),
        function(1, false),
    ])
    .unwrap();
    assert_eq!(group(&shared, 1, 0, 1), rid(1, 0, 0));
    let isolated = Topology::new(vec![
        root_port(1, 1, 1, true),
        function(0, true),
        function(1, true),
    ])
    .unwrap();
    assert_eq!(group(&isolated, 1, 0, 0), rid(1, 0, 0));
    assert_eq!(group(&isolated, 1, 0, 1), rid(1, 0, 1), "ACS between them");
}

#[test]
fn conventional_functions_on_the_root_bus_group_by_slot() {
    let shared = |number| Function {
        multifunction: true,
        ..endpoint(0, 0x1f, number)
    };
    let topology = Topology::new(vec![endpoint(0, 3, 0), shared(0), shared(2), shared(3)]).unwrap();
    assert_eq!(group(&topology, 0, 3, 0), rid(0, 3, 0), "a slot of its own");
    for number in [0, 2, 3] {
        assert_eq!(group(&topology, 0, 0x1f, number), rid(0, 0x1f, 0));
    }
}

#[test]
fn a_port_type_the_specification_reserves_isolates_nothing() {
    let topology = Topology::new(vec![
        express(bridge(0, 1, 1, 1), PortType::Reserved(0xB)),
        express(endpoint(1, 0, 0), PortType::Endpoint),
    ])
    .unwrap();
    assert_eq!(group(&topology, 1, 0, 0), rid(0, 1, 0));
}

#[test]
fn an_unassigned_bridge_forwards_to_nothing_and_a_held_back_bus_is_no_error() {
    let topology = Topology::new(vec![
        bridge(0, 1, 0, 0),
        root_port(2, 1, 8, true),
        express(endpoint(1, 0, 0), PortType::Endpoint),
    ])
    .unwrap();
    assert_eq!(group(&topology, 1, 0, 0), rid(1, 0, 0));
}

#[test]
fn bus_numbers_that_form_no_tree_are_refused() {
    let refused = |functions: Vec<Function>| Topology::new(functions).unwrap_err();
    assert_eq!(
        refused(vec![endpoint(0, 3, 0), endpoint(0, 3, 0)]),
        TopologyError::Duplicate
    );
    assert_eq!(
        refused(vec![bridge(2, 0, 1, 1)]),
        TopologyError::Inconsistent,
        "upward"
    );
    assert_eq!(
        refused(vec![bridge(0, 1, 3, 2)]),
        TopologyError::Inconsistent,
        "empty range"
    );
    assert_eq!(
        refused(vec![bridge(0, 1, 1, 1), bridge(0, 2, 1, 1)]),
        TopologyError::Inconsistent,
        "two bridges to one bus"
    );
    assert_eq!(
        refused(vec![bridge(0, 1, 1, 3), endpoint(2, 0, 0)]),
        TopologyError::Inconsistent,
        "a bus in range that no bridge forwards to answers"
    );
    assert_eq!(
        refused(vec![bridge(0, 1, 1, 5), bridge(0, 2, 3, 4)]),
        TopologyError::Inconsistent,
        "sibling ranges overlap"
    );
    assert_eq!(
        refused(vec![
            bridge(0, 1, 1, 2),
            bridge(1, 0, 2, 4),
            bridge(2, 0, 4, 4)
        ]),
        TopologyError::Inconsistent,
        "a range escaping the one above it"
    );
}

#[test]
fn a_function_reports_its_identity_and_enumerated_record() {
    let function = bridge(3, 4, 5, 5);
    assert_eq!(function.bus(), 3);
    assert_eq!(function.requester_id(), rid(3, 4, 0));
    assert!(function.is_bridge());
    assert!(!endpoint(0, 1, 0).is_bridge());
    let classed = |class| Function {
        class,
        ..endpoint(0, 0, 0)
    };
    assert!(endpoint(0, 1, 0).masters_dma());
    assert!(classed(0x06_01_00).masters_dma(), "an LPC bridge masters");
    assert!(!classed(0x06_00_00).masters_dma(), "a host bridge does not");
    assert!(!function.masters_dma(), "nor a bridge to a bus");
    let record = function.bus_device();
    assert_eq!(
        (record.vendor, record.device, record.class, record.address),
        (0x1af4, 0x1041, 0x0604, at(3, 4, 0))
    );
    let topology = Topology::new(vec![endpoint(0, 2, 0), endpoint(0, 1, 0)]).unwrap();
    assert_eq!(
        topology.functions()[0].address,
        at(0, 1, 0),
        "ascending by address"
    );
    assert_eq!(topology.index_of(at(0, 9, 0)), None);
}

#[test]
fn a_port_type_decodes_from_its_field() {
    for (field, port) in [
        (0x0, PortType::Endpoint),
        (0x1, PortType::LegacyEndpoint),
        (0x4, PortType::RootPort),
        (0x5, PortType::UpstreamPort),
        (0x6, PortType::DownstreamPort),
        (0x7, PortType::PcieToPci),
        (0x8, PortType::PciToPcie),
        (0x9, PortType::IntegratedEndpoint),
        (0xA, PortType::EventCollector),
        (0x2, PortType::Reserved(0x2)),
    ] {
        assert_eq!(PortType::from_field(field), port);
    }
}

#[test]
fn acs_isolates_once_every_control_the_function_implements_is_on() {
    assert!(ENFORCED.isolates());
    assert!(Acs {
        capable: Acs::SOURCE_VALIDATION,
        enabled: Acs::SOURCE_VALIDATION,
    }
    .isolates());
    assert!(!Acs {
        capable: Acs::ISOLATING,
        enabled: Acs::ISOLATING & !Acs::REQUEST_REDIRECT,
    }
    .isolates());
}

/// A configuration space holding each function's registers, applying every
/// write: the ACS dword's capability half is read-only, and a control the
/// function does not implement stays off.
/// A function's bus, device and function numbers.
type Slot = (u8, u8, u8);

struct Space {
    functions: RefCell<BTreeMap<Slot, BTreeMap<u16, u32>>>,
    /// Reaches only the legacy 256 bytes, as mechanism #1 does.
    legacy_only: bool,
    /// Functions whose ACS controls ignore writes.
    stuck: Vec<Slot>,
    writes: RefCell<Vec<(ConfigAddress, u32)>>,
}

/// Where the fixtures put each function's PCI Express capability and its ACS
/// registers.
const EXPRESS_OFFSET: u16 = 0x40;
const ACS_REGISTERS: u16 = EXTENDED_REGISTER + 1;

impl Space {
    fn new() -> Self {
        Self {
            functions: RefCell::new(BTreeMap::new()),
            legacy_only: false,
            stuck: Vec::new(),
            writes: RefCell::new(Vec::new()),
        }
    }

    fn put(&self, slot: Slot, registers: &[(u16, u32)]) {
        let mut functions = self.functions.borrow_mut();
        let function = functions.entry(slot).or_default();
        for &(register, value) in registers {
            *function.entry(register).or_insert(0) |= value;
        }
    }

    /// A function with header layout `header` (bit 7 set for a slot holding
    /// several).
    fn function(&self, slot: Slot, header: u8) {
        self.put(
            slot,
            &[
                (0, 0x1041_1af4),
                (2, 0x0200_0000),
                (3, u32::from(header) << 16),
            ],
        );
    }

    fn bridge(&self, slot: Slot, secondary: u8, subordinate: u8) {
        self.function(slot, 0x01);
        self.put(
            slot,
            &[(
                6,
                u32::from(slot.0) | u32::from(secondary) << 8 | u32::from(subordinate) << 16,
            )],
        );
    }

    fn express(&self, slot: Slot, port_type: u8) {
        self.put(
            slot,
            &[
                (1, 0x0010_0000),
                (13, u32::from(EXPRESS_OFFSET)),
                (
                    EXPRESS_OFFSET >> 2,
                    0x10 | (u32::from(port_type) << 4 | 2) << 16,
                ),
            ],
        );
    }

    fn acs(&self, slot: Slot, capable: u16, enabled: u16) {
        self.put(
            slot,
            &[
                (EXTENDED_REGISTER, 0x0001_000D),
                (ACS_REGISTERS, u32::from(capable) | u32::from(enabled) << 16),
            ],
        );
    }
}

impl ConfigSpace for Space {
    fn read32(&self, addr: ConfigAddress) -> u32 {
        if self.legacy_only && addr.register >= EXTENDED_REGISTER {
            return 0xFFFF_FFFF;
        }
        self.functions
            .borrow()
            .get(&(addr.bus, addr.device, addr.function))
            .map_or(0xFFFF_FFFF, |function| {
                function.get(&addr.register).copied().unwrap_or(0)
            })
    }

    fn write32(&self, addr: ConfigAddress, value: u32) {
        self.writes.borrow_mut().push((addr, value));
        let slot = (addr.bus, addr.device, addr.function);
        if self.stuck.contains(&slot) {
            return;
        }
        let mut functions = self.functions.borrow_mut();
        let Some(function) = functions.get_mut(&slot) else {
            return;
        };
        let held = function.entry(addr.register).or_insert(0);
        *held = if addr.register == ACS_REGISTERS {
            let capable = *held & 0xFFFF;
            capable | (value & capable << 16)
        } else {
            value
        };
    }
}

/// A root port at `00:01.0` offering every isolating control with none on,
/// and a PCI Express endpoint below it.
fn port_and_endpoint() -> Space {
    let space = Space::new();
    space.bridge((0, 1, 0), 1, 1);
    space.express((0, 1, 0), 0x4);
    space.acs((0, 1, 0), Acs::ISOLATING, 0);
    space.function((1, 0, 0), 0x00);
    space.express((1, 0, 0), 0x0);
    space
}

#[test]
fn a_walk_turns_acs_on_where_offered_and_isolates_what_it_proves() {
    let pci = Pci::new(port_and_endpoint());
    let topology = pci.topology(AcsPolicy::Enable).unwrap();
    let port = &topology.functions()[index(&topology, 0, 1, 0)];
    assert_eq!(port.express, Some(PortType::RootPort));
    assert_eq!(
        port.header,
        Header::Bridge {
            secondary: 1,
            subordinate: 1,
        }
    );
    assert_eq!(port.acs, Some(ENFORCED), "read back after the write");
    assert_eq!(group(&topology, 1, 0, 0), rid(1, 0, 0));
}

#[test]
fn a_walk_that_leaves_acs_writes_nothing_and_groups_by_what_firmware_left() {
    let pci = Pci::new(port_and_endpoint());
    let topology = pci.topology(AcsPolicy::Leave).unwrap();
    assert_eq!(group(&topology, 1, 0, 0), rid(0, 1, 0));
}

#[test]
fn acs_is_written_once_through_its_control_half() {
    let space = port_and_endpoint();
    let writes = Pci::new(space);
    writes.topology(AcsPolicy::Enable).unwrap();
    writes.topology(AcsPolicy::Enable).unwrap();
    let pci_writes = writes_of(&writes);
    assert_eq!(
        pci_writes.len(),
        1,
        "an enabled control is not written again"
    );
    let (addr, value) = pci_writes[0];
    assert_eq!(
        (addr.bus, addr.device, addr.register),
        (0, 1, ACS_REGISTERS)
    );
    assert_eq!(
        value,
        u32::from(Acs::ISOLATING) << 16 | u32::from(Acs::ISOLATING)
    );
}

fn writes_of(pci: &Pci<Space>) -> Vec<(ConfigAddress, u32)> {
    pci.config_space().writes.borrow().clone()
}

#[test]
fn a_port_that_ignores_the_write_stays_grouped_with_its_device() {
    let mut space = port_and_endpoint();
    space.stuck.push((0, 1, 0));
    let topology = Pci::new(space).topology(AcsPolicy::Enable).unwrap();
    assert_eq!(
        topology.functions()[index(&topology, 0, 1, 0)].acs,
        Some(Acs {
            capable: Acs::ISOLATING,
            enabled: 0,
        })
    );
    assert_eq!(group(&topology, 1, 0, 0), rid(0, 1, 0));
}

#[test]
fn a_mechanism_that_reaches_no_extended_space_finds_no_acs() {
    let mut space = port_and_endpoint();
    space.legacy_only = true;
    let topology = Pci::new(space).topology(AcsPolicy::Enable).unwrap();
    assert_eq!(topology.functions()[index(&topology, 0, 1, 0)].acs, None);
    assert_eq!(group(&topology, 1, 0, 0), rid(0, 1, 0));
}

#[test]
fn a_conventional_function_is_never_asked_for_acs() {
    let space = Space::new();
    space.function((0, 3, 0), 0x00);
    space.acs((0, 3, 0), Acs::ISOLATING, 0);
    let pci = Pci::new(space);
    let topology = pci.topology(AcsPolicy::Enable).unwrap();
    assert_eq!(topology.functions()[0].express, None);
    assert_eq!(topology.functions()[0].acs, None);
    assert!(writes_of(&pci).is_empty());
}

#[test]
fn an_extended_list_pointing_back_into_the_legacy_space_ends_the_walk() {
    let space = port_and_endpoint();
    space
        .functions
        .borrow_mut()
        .get_mut(&(0, 1, 0))
        .unwrap()
        .insert(EXTENDED_REGISTER, 0x0400_0001);
    let topology = Pci::new(space).topology(AcsPolicy::Enable).unwrap();
    assert_eq!(topology.functions()[index(&topology, 0, 1, 0)].acs, None);
}

#[test]
fn a_slot_holding_several_functions_marks_each_one() {
    let space = Space::new();
    space.function((0, 0x1f, 0), 0x80);
    space.function((0, 0x1f, 3), 0x00);
    let topology = Pci::new(space).topology(AcsPolicy::Leave).unwrap();
    assert!(topology
        .functions()
        .iter()
        .all(|function| function.multifunction));
    assert_eq!(group(&topology, 0, 0x1f, 3), rid(0, 0x1f, 0));
}

#[test]
fn a_walk_over_bridges_that_form_no_tree_is_a_device_fault() {
    let space = Space::new();
    space.bridge((0, 1, 0), 1, 1);
    space.bridge((0, 2, 0), 1, 1);
    assert_eq!(
        Pci::new(space).topology(AcsPolicy::Leave).unwrap_err(),
        DriverError::DeviceFault
    );
}

/// Bus numbers that form no tree refuse the walk, and a flat quiesce still
/// reaches every function on the bus: it stops exactly those it is told to,
/// writing only a command whose bit is set, and says how many it stopped and
/// how many read back mastering still.
#[test]
fn a_walk_refused_its_hierarchy_still_quiesces_every_function_it_is_told_to() {
    let mut space = Space::new();
    let mastering = (COMMAND_OFFSET >> 2, BUS_MASTER_ENABLE);
    // A host bridge, whose bit chipsets hardwire on.
    space.put((0, 0, 0), &[(0, 0x1237_8086), (2, 0x0600_0000), mastering]);
    space.function((0, 2, 0), 0x00);
    space.put((0, 2, 0), &[mastering]);
    space.put((0, 3, 0), &[(0, 0x10d3_8086), (2, 0x0200_0000), mastering]);
    space.put((0, 4, 0), &[(0, 0x10d3_8086), (2, 0x0200_0000)]);
    // One that ignores the write.
    space.put((0, 5, 0), &[(0, 0x10d3_8086), (2, 0x0200_0000), mastering]);
    space.stuck.push((0, 5, 0));
    // A bridge forwarding to a bus above its own.
    space.bridge((2, 0, 0), 1, 1);
    let pci = Pci::new(space);
    assert_eq!(
        pci.topology(AcsPolicy::Leave).unwrap_err(),
        DriverError::DeviceFault
    );
    assert_eq!(
        pci.quiesce(&Function::masters_dma),
        Quiesced {
            stopped: 2,
            refused: 1
        }
    );
    let command = |device: u8| pci.read_config(at(0, device, 0), COMMAND_OFFSET) & 0xFFFF;
    assert_eq!(command(0), BUS_MASTER_ENABLE, "a host bridge is left alone");
    assert_eq!((command(2), command(3), command(4)), (0, 0, 0));
    assert_eq!(command(5), BUS_MASTER_ENABLE);
    let stopped: Vec<_> = writes_of(&pci)
        .iter()
        .map(|(addr, _)| (addr.bus, addr.device))
        .collect();
    assert_eq!(
        stopped,
        [(0, 2), (0, 3), (0, 5)],
        "only a set bit is written"
    );
}
