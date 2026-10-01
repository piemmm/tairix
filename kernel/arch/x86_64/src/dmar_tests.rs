extern crate std;

use std::vec;
use std::vec::Vec;

use super::*;

fn table(host_address_bits: u8, flags: u8, structures: &[u8]) -> Vec<u8> {
    let total = REMAPPING_OFFSET + structures.len();
    let mut buf = vec![0u8; total];
    buf[..4].copy_from_slice(&DMAR_SIGNATURE);
    buf[4..8].copy_from_slice(&u32::try_from(total).unwrap().to_le_bytes());
    buf[8] = 1;
    buf[36] = host_address_bits - 1;
    buf[37] = flags;
    buf[REMAPPING_OFFSET..].copy_from_slice(structures);
    reseal(&mut buf);
    buf
}

fn reseal(buf: &mut [u8]) {
    buf[9] = 0;
    let sum = buf.iter().fold(0u8, |acc, b| acc.wrapping_add(*b));
    buf[9] = 0u8.wrapping_sub(sum);
}

fn structure(kind: u16, fixed: &[u8], scopes: &[u8]) -> Vec<u8> {
    let len = STRUCTURE_HEADER_LEN + fixed.len() + scopes.len();
    let mut out = Vec::with_capacity(len);
    out.extend_from_slice(&kind.to_le_bytes());
    out.extend_from_slice(&u16::try_from(len).unwrap().to_le_bytes());
    out.extend_from_slice(fixed);
    out.extend_from_slice(scopes);
    out
}

fn drhd(flags: u8, size: u8, segment: u16, base: u64, scopes: &[u8]) -> Vec<u8> {
    let mut fixed = vec![flags, size];
    fixed.extend_from_slice(&segment.to_le_bytes());
    fixed.extend_from_slice(&base.to_le_bytes());
    structure(TYPE_DRHD, &fixed, scopes)
}

fn rmrr(segment: u16, base: u64, limit: u64, scopes: &[u8]) -> Vec<u8> {
    let mut fixed = vec![0, 0];
    fixed.extend_from_slice(&segment.to_le_bytes());
    fixed.extend_from_slice(&base.to_le_bytes());
    fixed.extend_from_slice(&limit.to_le_bytes());
    structure(TYPE_RMRR, &fixed, scopes)
}

fn scope(kind: u8, enumeration_id: u8, start_bus: u8, path: &[(u8, u8)]) -> Vec<u8> {
    let mut out = vec![
        kind,
        u8::try_from(SCOPE_HEADER_LEN + 2 * path.len()).unwrap(),
        0,
        0,
        enumeration_id,
        start_bus,
    ];
    for &(device, function) in path {
        out.push(device);
        out.push(function);
    }
    out
}

fn sid(bus: u8, device: u8, function: u8) -> SourceId {
    SourceId::new(bus, device, function).unwrap()
}

struct Bridges(Vec<(SourceId, u8, u8)>);

impl BridgeBuses for Bridges {
    fn bus_range(&self, bridge: SourceId) -> Option<(u8, u8)> {
        self.0
            .iter()
            .find(|(at, _, _)| *at == bridge)
            .map(|&(_, secondary, subordinate)| (secondary, subordinate))
    }
}

const NO_BRIDGES: &Bridges = &Bridges(Vec::new());

/// The shape QEMU's `intel-iommu` reports: one catch-all unit whose only
/// scope names the I/O APIC.
fn qemu_like() -> Vec<u8> {
    let ioapic = scope(3, 0, 0xFF, &[(0, 0)]);
    table(
        39,
        0x1,
        &drhd(DRHD_INCLUDE_PCI_ALL, 0, 0, 0xFED9_0000, &ioapic),
    )
}

#[test]
fn a_catch_all_unit_covers_every_function_of_its_segment() {
    let bytes = qemu_like();
    let dmar = Dmar::parse(&bytes).unwrap();
    assert_eq!(dmar.host_address_bits(), 39);
    assert!(dmar.flags().interrupt_remapping());
    assert!(!dmar.flags().x2apic_opt_out());
    let units: Vec<_> = dmar.units().collect();
    assert_eq!(units.len(), 1);
    assert!(units[0].include_pci_all());
    assert_eq!(units[0].register_base(), 0xFED9_0000);
    assert_eq!(units[0].register_len(), 0x1000);
    let scopes: Vec<_> = units[0].scopes().collect();
    assert_eq!(scopes.len(), 1);
    assert_eq!(scopes[0].kind(), ScopeKind::IoApic);
    assert_eq!(dmar.unit_for(0, sid(0, 3, 0), NO_BRIDGES), Some(0));
    assert_eq!(dmar.unit_for(0, sid(0xFF, 31, 7), NO_BRIDGES), Some(0));
    assert_eq!(dmar.unit_for(1, sid(0, 3, 0), NO_BRIDGES), None);
    assert_eq!(dmar.reserved_regions().count(), 0);
}

#[test]
fn a_unit_naming_a_function_claims_it_ahead_of_the_catch_all() {
    let graphics = drhd(0, 0, 0, 0xFED9_0000, &scope(1, 0, 0, &[(2, 0)]));
    let rest = drhd(DRHD_INCLUDE_PCI_ALL, 0, 0, 0xFED9_1000, &[]);
    let bytes = table(46, 0, &[graphics, rest].concat());
    let dmar = Dmar::parse(&bytes).unwrap();
    assert_eq!(dmar.unit_for(0, sid(0, 2, 0), NO_BRIDGES), Some(0));
    assert_eq!(dmar.unit_for(0, sid(0, 2, 1), NO_BRIDGES), Some(1));
    assert_eq!(dmar.unit_for(0, sid(0, 0x1F, 0), NO_BRIDGES), Some(1));
}

#[test]
fn a_bridge_scope_claims_the_buses_behind_the_bridge() {
    let port = drhd(0, 0, 0, 0xFED9_0000, &scope(2, 0, 0, &[(0x1C, 0)]));
    let rest = drhd(DRHD_INCLUDE_PCI_ALL, 0, 0, 0xFED9_1000, &[]);
    let bytes = table(46, 0, &[port, rest].concat());
    let dmar = Dmar::parse(&bytes).unwrap();
    let bridges = Bridges(vec![(sid(0, 0x1C, 0), 2, 4)]);
    assert_eq!(dmar.unit_for(0, sid(0, 0x1C, 0), &bridges), Some(0));
    assert_eq!(dmar.unit_for(0, sid(2, 0, 0), &bridges), Some(0));
    assert_eq!(dmar.unit_for(0, sid(4, 0, 0), &bridges), Some(0));
    assert_eq!(dmar.unit_for(0, sid(5, 0, 0), &bridges), Some(1));
    assert_eq!(dmar.unit_for(0, sid(1, 0, 0), &bridges), Some(1));
}

#[test]
fn a_scope_path_is_walked_through_each_bridge_secondary_bus() {
    let endpoint = scope(1, 0, 0, &[(0x1C, 0), (0, 0), (3, 1)]);
    let (scope, rest) = split_scope(&endpoint).unwrap();
    assert!(rest.is_empty());
    let bridges = Bridges(vec![(sid(0, 0x1C, 0), 2, 6), (sid(2, 0, 0), 3, 3)]);
    assert_eq!(scope.resolve(&bridges), Some(sid(3, 3, 1)));
    assert_eq!(
        scope.resolve(NO_BRIDGES),
        None,
        "a silent bridge ends the walk"
    );
}

#[test]
fn a_scope_hop_past_the_configuration_space_resolves_nothing() {
    let past_devices = scope(1, 0, 0, &[(32, 0)]);
    let (hop, _) = split_scope(&past_devices).unwrap();
    assert_eq!(hop.resolve(NO_BRIDGES), None);
    let no_path = scope(1, 0, 0, &[]);
    let (empty, _) = split_scope(&no_path).unwrap();
    assert_eq!(empty.resolve(NO_BRIDGES), None);
}

#[test]
fn a_reserved_region_reports_whole_pages_and_its_functions() {
    let usb = scope(1, 0, 0, &[(0x14, 0)]);
    let region = rmrr(0, 0x7B80_0000, 0x7FFF_FFFF, &usb);
    let unit = drhd(DRHD_INCLUDE_PCI_ALL, 0, 0, 0xFED9_0000, &[]);
    let bytes = table(39, 0, &[unit, region].concat());
    let dmar = Dmar::parse(&bytes).unwrap();
    let regions: Vec<_> = dmar.reserved_regions().collect();
    assert_eq!(regions.len(), 1);
    assert_eq!(regions[0].base(), 0x7B80_0000);
    assert_eq!(regions[0].len(), 0x0480_0000);
    assert_eq!(regions[0].segment(), 0);
    let scopes: Vec<_> = regions[0].scopes().collect();
    assert_eq!(scopes[0].resolve(NO_BRIDGES), Some(sid(0, 0x14, 0)));
}

#[test]
fn the_register_window_grows_with_the_size_field() {
    let bytes = table(39, 0, &drhd(DRHD_INCLUDE_PCI_ALL, 2, 0, 0xFED9_0000, &[]));
    let unit = Dmar::parse(&bytes).unwrap().units().next().unwrap();
    assert_eq!(unit.register_len(), 0x4000);
}

#[test]
fn a_structure_a_later_revision_defines_is_skipped() {
    let future = structure(0x7F, &[1, 2, 3, 4], &[]);
    let unit = drhd(DRHD_INCLUDE_PCI_ALL, 0, 0, 0xFED9_0000, &[]);
    let bytes = table(39, 0, &[future, unit].concat());
    assert_eq!(Dmar::parse(&bytes).unwrap().units().count(), 1);
}

fn refused(structures: &[u8]) -> AcpiError {
    Dmar::parse(&table(39, 0, structures)).unwrap_err()
}

#[test]
fn every_malformed_structure_refuses_the_whole_table() {
    let good = drhd(DRHD_INCLUDE_PCI_ALL, 0, 0, 0xFED9_0000, &[]);

    let mut zero_len = good.clone();
    zero_len[2..4].copy_from_slice(&0u16.to_le_bytes());
    assert_eq!(refused(&zero_len), AcpiError::BadLength);

    let mut overlong = good.clone();
    overlong[2..4].copy_from_slice(&0x100u16.to_le_bytes());
    assert_eq!(refused(&overlong), AcpiError::BadLength);

    assert_eq!(refused(&good[..3]), AcpiError::Truncated);

    let short = structure(TYPE_DRHD, &[0; 8], &[]);
    assert_eq!(refused(&short), AcpiError::BadLength);

    let misaligned = drhd(0, 0, 0, 0xFED9_0010, &[]);
    assert_eq!(refused(&misaligned), AcpiError::BadLength);

    let null = drhd(0, 0, 0, 0, &[]);
    assert_eq!(refused(&null), AcpiError::BadLength);

    let mut odd_path = scope(1, 0, 0, &[(1, 0)]);
    odd_path[1] = 7;
    odd_path.push(0);
    assert_eq!(
        refused(&drhd(0, 0, 0, 0xFED9_0000, &odd_path)),
        AcpiError::BadLength
    );

    let mut spilling = scope(1, 0, 0, &[(1, 0)]);
    spilling[1] = 10;
    assert_eq!(
        refused(&drhd(0, 0, 0, 0xFED9_0000, &spilling)),
        AcpiError::BadLength
    );

    assert_eq!(
        refused(&drhd(0, 0, 0, 0xFED9_0000, &[1, 6, 0])),
        AcpiError::Truncated
    );

    assert_eq!(
        refused(&[good.clone(), rmrr(0, 0x1000, 0x1FFE, &[])].concat()),
        AcpiError::BadLength,
        "a limit one short of a page"
    );
    assert_eq!(
        refused(&rmrr(0, 0x1800, 0x1FFF, &[])),
        AcpiError::BadLength,
        "a base inside a page"
    );
    assert_eq!(
        refused(&rmrr(0, 0x2000, 0x0FFF, &[])),
        AcpiError::BadLength,
        "an end before the start"
    );
    assert_eq!(
        refused(&rmrr(0, 0, u64::MAX, &[])),
        AcpiError::BadLength,
        "a limit whose end overflows"
    );

    let rhsa = structure(TYPE_RHSA, &[0; 8], &[]);
    assert_eq!(refused(&rhsa), AcpiError::BadLength);
    let andd = structure(TYPE_ANDD, &[0; 2], &[]);
    assert_eq!(refused(&andd), AcpiError::BadLength);
    let atsr = structure(TYPE_ATSR, &[0; 2], &[]);
    assert_eq!(refused(&atsr), AcpiError::BadLength);
}

#[test]
fn a_damaged_header_is_refused() {
    let mut bytes = qemu_like();
    bytes[40] ^= 0xFF;
    assert_eq!(Dmar::parse(&bytes).unwrap_err(), AcpiError::BadChecksum);

    let mut bytes = qemu_like();
    bytes[36] = 64;
    reseal(&mut bytes);
    assert_eq!(Dmar::parse(&bytes).unwrap_err(), AcpiError::BadLength);

    let mut bytes = qemu_like();
    bytes[36] = 0xFF;
    reseal(&mut bytes);
    assert_eq!(Dmar::parse(&bytes).unwrap_err(), AcpiError::BadLength);

    let mut bytes = qemu_like();
    bytes.truncate(REMAPPING_OFFSET - 4);
    let short = u32::try_from(bytes.len()).unwrap();
    bytes[4..8].copy_from_slice(&short.to_le_bytes());
    reseal(&mut bytes);
    assert_eq!(Dmar::parse(&bytes).unwrap_err(), AcpiError::BadLength);

    let mut bytes = qemu_like();
    bytes[0] = b'X';
    assert_eq!(Dmar::parse(&bytes).unwrap_err(), AcpiError::BadSignature);
}

#[test]
fn a_source_id_packs_bus_device_and_function() {
    assert_eq!(sid(0, 0, 0).raw(), 0);
    assert_eq!(sid(0x12, 0x1F, 7).raw(), 0x12FF);
    assert_eq!(sid(0x12, 0x1F, 7).bus(), 0x12);
    assert_eq!(SourceId::new(0, 32, 0), None);
    assert_eq!(SourceId::new(0, 0, 8), None);
}

struct Sink(Vec<HwNode>);

impl HwNodeSink for Sink {
    fn emit(&mut self, node: HwNode) -> Result<(), DiscoveryError> {
        self.0.push(node);
        Ok(())
    }
}

#[test]
fn every_unit_becomes_a_node_carrying_its_registers_and_reserved_windows() {
    let graphics = drhd(0, 0, 0, 0xFED9_0000, &scope(1, 0, 0, &[(2, 0)]));
    let rest = drhd(DRHD_INCLUDE_PCI_ALL, 1, 0, 0xFED9_1000, &[]);
    let usb = rmrr(0, 0x7B80_0000, 0x7B8F_FFFF, &scope(1, 0, 0, &[(0x14, 0)]));
    let stolen = rmrr(0, 0x8000_0000, 0x83FF_FFFF, &scope(1, 0, 0, &[(2, 0)]));
    let bytes = table(46, 0, &[graphics, rest, usb, stolen].concat());
    let dmar = Dmar::parse(&bytes).unwrap();
    let mut sink = Sink(Vec::new());
    let dropped = emit_unit_nodes(&dmar, 0x800A_0000, b"intel,vtd", NO_BRIDGES, &mut sink).unwrap();
    assert_eq!(dropped, 0);
    assert_eq!(sink.0.len(), 2);
    let [first, second] = [&sink.0[0], &sink.0[1]];
    assert_eq!(first.id(), 0x800A_0000);
    assert_eq!(second.id(), 0x800A_0001);
    for node in [first, second] {
        assert_eq!(node.class(), Some(HwDeviceClass::Iommu));
        assert_eq!(node.parent(), HW_NODE_ROOT_ID);
        assert_eq!(
            node.match_keys(),
            &[HwMatchKey::compatible(b"intel,vtd").unwrap()]
        );
    }
    assert_eq!(first.resources()[0], HwResource::mmio(0xFED9_0000, 0x1000));
    assert_eq!(second.resources()[0], HwResource::mmio(0xFED9_1000, 0x2000));
    let window = |node: &HwNode| {
        node.resources()
            .iter()
            .filter_map(|r| r.iommu_reserved().ok())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        window(first),
        [
            IommuReservedWindow::new(u32::from(sid(0, 2, 0).raw()), 0x8000_0000, 0x0400_0000)
                .unwrap()
        ],
        "the graphics unit keeps the graphics function's window"
    );
    assert_eq!(
        window(second),
        [
            IommuReservedWindow::new(u32::from(sid(0, 0x14, 0).raw()), 0x7B80_0000, 0x10_0000)
                .unwrap()
        ],
        "the catch-all unit keeps the USB function's window"
    );
}

#[test]
fn reserved_windows_past_a_node_s_room_are_counted_not_forced() {
    let unit = drhd(DRHD_INCLUDE_PCI_ALL, 0, 0, 0xFED9_0000, &[]);
    let mut structures = unit;
    for device in 0..20u8 {
        let base = 0x1000_0000 + u64::from(device) * 0x10_0000;
        structures.extend(rmrr(
            0,
            base,
            base + 0xF_FFFF,
            &scope(1, 0, 0, &[(device, 0)]),
        ));
    }
    let bytes = table(46, 0, &structures);
    let dmar = Dmar::parse(&bytes).unwrap();
    let mut sink = Sink(Vec::new());
    let dropped = emit_unit_nodes(&dmar, 1, b"intel,vtd", NO_BRIDGES, &mut sink).unwrap();
    assert_eq!(
        sink.0[0].resources().len(),
        tairix_abi::HW_NODE_MAX_RESOURCES
    );
    assert_eq!(dropped, 20 - (tairix_abi::HW_NODE_MAX_RESOURCES - 1));
}

#[test]
fn a_function_s_stream_names_the_unit_node_that_translates_it() {
    let graphics = drhd(0, 0, 0, 0xFED9_0000, &scope(1, 0, 0, &[(2, 0)]));
    let rest = drhd(DRHD_INCLUDE_PCI_ALL, 0, 0, 0xFED9_1000, &[]);
    let bytes = table(46, 0, &[graphics, rest].concat());
    let dmar = Dmar::parse(&bytes).unwrap();
    let stream = |source| stream_resource(&dmar, 0x800A_0000, 0, source, NO_BRIDGES);
    assert_eq!(
        stream(sid(0, 2, 0)).unwrap().iommu_streams().unwrap(),
        IommuStreams::new(0x800A_0000, 0x0010, 1).unwrap()
    );
    assert_eq!(
        stream(sid(0, 3, 0)).unwrap().iommu_streams().unwrap(),
        IommuStreams::new(0x800A_0001, 0x0018, 1).unwrap()
    );
    assert_eq!(
        stream_resource(&dmar, 0x800A_0000, 1, sid(0, 3, 0), NO_BRIDGES),
        None
    );
}

/// Configuration space as a map from a function's packed address and a dword
/// offset to its value; everything else reads as no device.
struct ConfigBus(Vec<(u64, u16, u32)>);

impl tairix_abi::driver::bus::Bus for ConfigBus {
    fn enumerate(
        &self,
        _out: &mut [tairix_abi::driver::bus::BusDevice],
    ) -> Result<usize, tairix_abi::DriverError> {
        Ok(0)
    }
}

impl PciBus for ConfigBus {
    fn map_bar_window(
        &self,
        _bdf: u64,
        _bar_index: u8,
        _mapper: &dyn tairix_abi::MmioMapper,
    ) -> Result<tairix_abi::RegisterWindow, tairix_abi::DriverError> {
        Err(tairix_abi::DriverError::Unsupported)
    }

    fn enable_bus_master(&self, _bdf: u64) -> Result<(), tairix_abi::DriverError> {
        Err(tairix_abi::DriverError::Unsupported)
    }

    fn assign_bar(
        &self,
        _bdf: u64,
        _bar_index: u8,
        _window_base: u64,
        _window_size: u64,
    ) -> Result<u64, tairix_abi::DriverError> {
        Err(tairix_abi::DriverError::Unsupported)
    }

    fn read_config(&self, bdf: u64, offset: u16) -> Result<u32, tairix_abi::DriverError> {
        Ok(self
            .0
            .iter()
            .find(|&&(at, register, _)| at == bdf && register == offset)
            .map_or(0xFFFF_FFFF, |&(_, _, value)| value))
    }

    fn describe_function(&self, _bdf: u64) -> Result<HwNode, tairix_abi::DriverError> {
        Err(tairix_abi::DriverError::Unsupported)
    }
}

#[test]
fn a_bridge_s_buses_are_read_from_its_type_1_header() {
    let port = u64::from(sid(0, 0x1C, 0).raw()) << 8;
    let endpoint = u64::from(sid(0, 0x1F, 0).raw()) << 8;
    let bus = ConfigBus(vec![
        (port, CONFIG_ID, 0x9D10_8086),
        (port, CONFIG_HEADER, 0x0081_0000),
        (port, CONFIG_BUSES, 0x0004_0200),
        (endpoint, CONFIG_ID, 0x1234_8086),
        (endpoint, CONFIG_HEADER, 0),
    ]);
    let bridges = PciBridges(&bus);
    assert_eq!(bridges.bus_range(sid(0, 0x1C, 0)), Some((2, 4)));
    assert_eq!(
        bridges.bus_range(sid(0, 0x1F, 0)),
        None,
        "a type-0 header is no bridge"
    );
    assert_eq!(bridges.bus_range(sid(0, 5, 0)), None, "no device answers");
}
