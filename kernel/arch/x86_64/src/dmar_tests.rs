extern crate std;

use std::vec;
use std::vec::Vec;

use super::*;
use crate::acpi::UnitNodes;

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

/// One segment's walk as a test sets it out: each function's aliases by its
/// source id, and the functions below an external-facing port.
#[derive(Default)]
struct Walk {
    aliases: Vec<(SourceId, SourceId)>,
    untrusted: Vec<SourceId>,
}

impl BridgeBuses for Walk {
    fn bus_range(&self, _bridge: SourceId) -> Option<(u8, u8)> {
        None
    }
}

impl DmaAliases for Walk {
    fn aliases(&self, source: SourceId, visit: &mut dyn FnMut(SourceId)) {
        for &(of, alias) in &self.aliases {
            if of == source {
                visit(alias);
            }
        }
    }
}

impl Fabric for Walk {
    fn untrusted(&self, source: SourceId) -> bool {
        self.untrusted.contains(&source)
    }
}

/// `walk` as the walk of segment 0, and no other segment walked.
fn segment_zero<'w>(walk: &'w Walk) -> impl Fn(u16) -> Option<&'w dyn Fabric> {
    move |segment| (segment == 0).then_some(walk as &dyn Fabric)
}

/// No segment walked.
fn unwalked(_segment: u16) -> Option<&'static dyn Fabric> {
    None
}

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
    let placed = emit_unit_nodes(
        &dmar,
        0x800A_0000,
        b"intel,vtd",
        &segment_zero(&Walk::default()),
        &mut sink,
    )
    .unwrap();
    assert_eq!(
        placed,
        UnitNodes {
            emitted: 2,
            dropped: 0,
            untrusted: 0,
        }
    );
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
        [IommuReservedWindow::new(
            u32::from(sid(0, 2, 0).raw()),
            0x8000_0000,
            0x0400_0000,
            ReservedAccess::ReadWrite,
        )
        .unwrap()],
        "the graphics unit keeps the graphics function's window"
    );
    assert_eq!(
        window(second),
        [IommuReservedWindow::new(
            u32::from(sid(0, 0x14, 0).raw()),
            0x7B80_0000,
            0x10_0000,
            ReservedAccess::ReadWrite,
        )
        .unwrap()],
        "the catch-all unit keeps the USB function's window"
    );
}

/// With no hierarchy to resolve a scope through, every unit still gets its
/// node, so it comes up, but keeps no firmware window: it blocks every stream,
/// and every window it would have kept is counted dropped.
#[test]
fn without_a_hierarchy_every_unit_comes_up_keeping_no_window() {
    let graphics = drhd(0, 0, 0, 0xFED9_0000, &scope(1, 0, 0, &[(2, 0)]));
    let rest = drhd(DRHD_INCLUDE_PCI_ALL, 1, 0, 0xFED9_1000, &[]);
    let usb = rmrr(0, 0x7B80_0000, 0x7B8F_FFFF, &scope(1, 0, 0, &[(0x14, 0)]));
    let stolen = rmrr(0, 0x8000_0000, 0x83FF_FFFF, &scope(1, 0, 0, &[(2, 0)]));
    let bytes = table(46, 0, &[graphics, rest, usb, stolen].concat());
    let dmar = Dmar::parse(&bytes).unwrap();
    let mut sink = Sink(Vec::new());
    let placed = emit_unit_nodes(&dmar, 1, b"intel,vtd", &unwalked, &mut sink).unwrap();
    assert_eq!(
        placed,
        UnitNodes {
            emitted: 2,
            dropped: 2,
            untrusted: 0,
        }
    );
    let registers = [
        HwResource::mmio(0xFED9_0000, 0x1000),
        HwResource::mmio(0xFED9_1000, 0x2000),
    ];
    for (node, registers) in sink.0.iter().zip(registers) {
        assert_eq!(node.class(), Some(HwDeviceClass::Iommu));
        assert_eq!(
            node.resources(),
            [registers, crate::acpi::unit_dma()],
            "its registers and its own DMA alone"
        );
    }
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
    let placed = emit_unit_nodes(
        &dmar,
        1,
        b"intel,vtd",
        &segment_zero(&Walk::default()),
        &mut sink,
    )
    .unwrap();
    assert_eq!(
        sink.0[0].resources().len(),
        tairix_abi::HW_NODE_MAX_RESOURCES
    );
    // The registers and the unit's own DMA take two of the node's places.
    assert_eq!(placed.dropped, 20 - (tairix_abi::HW_NODE_MAX_RESOURCES - 2));
}

#[test]
fn a_function_is_translated_by_the_unit_node_its_scopes_name() {
    let graphics = drhd(0, 0, 0, 0xFED9_0000, &scope(1, 0, 0, &[(2, 0)]));
    let rest = drhd(DRHD_INCLUDE_PCI_ALL, 0, 0, 0xFED9_1000, &[]);
    let bytes = table(46, 0, &[graphics, rest].concat());
    let dmar = Dmar::parse(&bytes).unwrap();
    let both = UnitNodes {
        emitted: 2,
        ..UnitNodes::default()
    };
    let unit = |source| unit_node(&dmar, 0x800A_0000, both, 0, source, NO_BRIDGES);
    assert_eq!(unit(sid(0, 2, 0)), Some(0x800A_0000));
    assert_eq!(unit(sid(0, 3, 0)), Some(0x800A_0001));
    assert_eq!(
        unit_node(&dmar, 0x800A_0000, both, 1, sid(0, 3, 0), NO_BRIDGES),
        None
    );
    let first_only = UnitNodes {
        emitted: 1,
        ..UnitNodes::default()
    };
    assert_eq!(
        unit_node(&dmar, 0x800A_0000, first_only, 0, sid(0, 3, 0), NO_BRIDGES),
        None,
        "a unit with no node brings nothing up, so its functions name none"
    );
}

/// Firmware's DMA for a function behind a bridge to conventional PCI arrives
/// under the bridge's alias, so the window is kept there too, once however
/// many functions share it.
#[test]
fn a_reserved_window_is_kept_for_every_alias_of_its_function() {
    let rest = drhd(DRHD_INCLUDE_PCI_ALL, 0, 0, 0xFED9_0000, &[]);
    let first = rmrr(0, 0x7B80_0000, 0x7B8F_FFFF, &scope(1, 0, 2, &[(1, 0)]));
    let second = rmrr(0, 0x7B80_0000, 0x7B8F_FFFF, &scope(1, 0, 2, &[(2, 0)]));
    let bytes = table(46, 0, &[rest, first, second].concat());
    let dmar = Dmar::parse(&bytes).unwrap();
    let alias = sid(2, 0, 0);
    let aliases = Walk {
        aliases: vec![(sid(2, 1, 0), alias), (sid(2, 2, 0), alias)],
        ..Walk::default()
    };
    let mut sink = Sink(Vec::new());
    let placed =
        emit_unit_nodes(&dmar, 1, b"intel,vtd", &segment_zero(&aliases), &mut sink).unwrap();
    assert_eq!(placed.dropped, 0);
    let streams: Vec<u32> = sink.0[0]
        .resources()
        .iter()
        .filter_map(|r| r.iommu_reserved().ok())
        .map(IommuReservedWindow::stream)
        .collect();
    assert_eq!(
        streams,
        [sid(2, 1, 0), alias, sid(2, 2, 0)].map(|stream| u32::from(stream.raw()))
    );
}

#[test]
fn units_sharing_registers_or_a_segment_s_every_function_are_refused_whole() {
    let one = drhd(0, 1, 0, 0xFED9_0000, &scope(1, 0, 0, &[(2, 0)]));
    let inside = drhd(DRHD_INCLUDE_PCI_ALL, 0, 0, 0xFED9_1000, &[]);
    assert_eq!(
        Dmar::parse(&table(46, 0, &[one, inside].concat())).err(),
        Some(AcpiError::BadLength),
        "a window inside another unit's"
    );
    let first = drhd(DRHD_INCLUDE_PCI_ALL, 0, 0, 0xFED9_0000, &[]);
    let second = drhd(DRHD_INCLUDE_PCI_ALL, 0, 0, 0xFED9_1000, &[]);
    assert_eq!(
        Dmar::parse(&table(46, 0, &[first.clone(), second].concat())).err(),
        Some(AcpiError::BadLength),
        "two catch-all units on one segment"
    );
    let elsewhere = drhd(DRHD_INCLUDE_PCI_ALL, 0, 1, 0xFED9_1000, &[]);
    assert!(Dmar::parse(&table(46, 0, &[first, elsewhere].concat())).is_ok());
    let wrapping = drhd(DRHD_INCLUDE_PCI_ALL, 0, 0, 0xFFFF_FFFF_FFFF_F000, &[]);
    assert_eq!(
        Dmar::parse(&table(46, 0, &wrapping)).err(),
        Some(AcpiError::BadLength),
        "a window past the address space"
    );
}

#[test]
fn a_window_firmware_names_twice_takes_one_slot() {
    let unit = drhd(DRHD_INCLUDE_PCI_ALL, 0, 0, 0xFED9_0000, &[]);
    let usb = scope(1, 0, 0, &[(0x14, 0)]);
    let twice = [
        unit,
        rmrr(
            0,
            0x7B80_0000,
            0x7B8F_FFFF,
            &[usb.clone(), usb.clone()].concat(),
        ),
        rmrr(0, 0x7B80_0000, 0x7B8F_FFFF, &usb),
    ]
    .concat();
    let bytes = table(46, 0, &twice);
    let dmar = Dmar::parse(&bytes).unwrap();
    let mut sink = Sink(Vec::new());
    let placed = emit_unit_nodes(
        &dmar,
        1,
        b"intel,vtd",
        &segment_zero(&Walk::default()),
        &mut sink,
    )
    .unwrap();
    assert_eq!(placed.dropped, 0);
    let windows = sink.0[0]
        .resources()
        .iter()
        .filter(|r| r.iommu_reserved().is_ok())
        .count();
    assert_eq!(windows, 1);
}

/// A window on a segment no walk reaches would be resolved through another
/// segment's configuration space; it is counted, never placed.
#[test]
fn a_window_on_a_segment_discovery_does_not_walk_is_never_resolved() {
    let near = drhd(DRHD_INCLUDE_PCI_ALL, 0, 0, 0xFED9_0000, &[]);
    let far = drhd(DRHD_INCLUDE_PCI_ALL, 0, 1, 0xFED9_1000, &[]);
    let window = rmrr(1, 0x7B80_0000, 0x7B8F_FFFF, &scope(1, 0, 0, &[(0x14, 0)]));
    let bytes = table(46, 0, &[near, far, window].concat());
    let dmar = Dmar::parse(&bytes).unwrap();
    let mut sink = Sink(Vec::new());
    let placed = emit_unit_nodes(
        &dmar,
        1,
        b"intel,vtd",
        &segment_zero(&Walk::default()),
        &mut sink,
    )
    .unwrap();
    assert_eq!(placed.dropped, 1, "counted once, not once per unit");
    assert!(sink
        .0
        .iter()
        .all(|node| node.resources().iter().all(|r| r.iommu_reserved().is_err())));
}

/// A tree with no room for a unit's node ends the emission there: the units
/// before it keep theirs, and the rest bring nothing up.
#[test]
fn a_full_tree_keeps_the_units_it_could_hold() {
    struct Room(usize, Vec<HwNode>);
    impl HwNodeSink for Room {
        fn emit(&mut self, node: HwNode) -> Result<(), DiscoveryError> {
            if self.1.len() == self.0 {
                return Err(DiscoveryError::SinkFull);
            }
            self.1.push(node);
            Ok(())
        }
    }
    let graphics = drhd(0, 0, 0, 0xFED9_0000, &scope(1, 0, 0, &[(2, 0)]));
    let rest = drhd(DRHD_INCLUDE_PCI_ALL, 0, 0, 0xFED9_1000, &[]);
    let bytes = table(46, 0, &[graphics, rest].concat());
    let dmar = Dmar::parse(&bytes).unwrap();
    let mut room = Room(1, Vec::new());
    let placed = emit_unit_nodes(
        &dmar,
        1,
        b"intel,vtd",
        &segment_zero(&Walk::default()),
        &mut room,
    )
    .unwrap();
    assert_eq!(placed.emitted, 1);
    assert_eq!(room.1.len(), 1);
    assert!(
        placed.strands(dmar.units().map(|unit| unit.segment()), 0),
        "the unit left out strands its segment"
    );
    assert!(
        !placed.strands(dmar.units().map(|unit| unit.segment()), 1),
        "and no other"
    );
}

/// A function below an external-facing port is untrusted: firmware keeps no
/// window for it, however its table names one.
#[test]
fn no_window_is_kept_for_a_function_below_an_external_facing_port() {
    let unit = drhd(DRHD_INCLUDE_PCI_ALL, 0, 0, 0xFED9_0000, &[]);
    let usb = rmrr(0, 0x7B80_0000, 0x7B8F_FFFF, &scope(1, 0, 0, &[(0x14, 0)]));
    let dock = rmrr(0, 0x7C00_0000, 0x7C0F_FFFF, &scope(1, 0, 0, &[(0x1C, 0)]));
    let bytes = table(46, 0, &[unit, usb, dock].concat());
    let dmar = Dmar::parse(&bytes).unwrap();
    let walk = Walk {
        untrusted: vec![sid(0, 0x1C, 0)],
        ..Walk::default()
    };
    let mut sink = Sink(Vec::new());
    let placed = emit_unit_nodes(&dmar, 1, b"intel,vtd", &segment_zero(&walk), &mut sink).unwrap();
    assert_eq!((placed.dropped, placed.untrusted), (0, 1));
    let streams: Vec<u32> = sink.0[0]
        .resources()
        .iter()
        .filter_map(|r| r.iommu_reserved().ok())
        .map(IommuReservedWindow::stream)
        .collect();
    assert_eq!(streams, [u32::from(sid(0, 0x14, 0).raw())]);
}

/// Each unit is emitted once whatever segment it covers, its windows
/// resolved through its own segment's walk.
#[test]
fn units_of_two_segments_each_keep_their_own_segment_s_windows() {
    let near = drhd(DRHD_INCLUDE_PCI_ALL, 0, 0, 0xFED9_0000, &[]);
    let far = drhd(DRHD_INCLUDE_PCI_ALL, 0, 1, 0xFED9_1000, &[]);
    let near_window = rmrr(0, 0x7B80_0000, 0x7B8F_FFFF, &scope(1, 0, 0, &[(0x14, 0)]));
    let far_window = rmrr(1, 0x7C00_0000, 0x7C0F_FFFF, &scope(1, 0, 0, &[(0x02, 0)]));
    let bytes = table(46, 0, &[near, far, near_window, far_window].concat());
    let dmar = Dmar::parse(&bytes).unwrap();
    let walk = Walk::default();
    let both = |_segment: u16| Some(&walk as &dyn Fabric);
    let mut sink = Sink(Vec::new());
    let placed = emit_unit_nodes(&dmar, 1, b"intel,vtd", &both, &mut sink).unwrap();
    assert_eq!((placed.emitted, placed.dropped), (2, 0));
    let windows = |node: &HwNode| {
        node.resources()
            .iter()
            .filter_map(|r| r.iommu_reserved().ok())
            .map(|w| (w.stream(), w.base()))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        windows(&sink.0[0]),
        [(u32::from(sid(0, 0x14, 0).raw()), 0x7B80_0000)]
    );
    assert_eq!(
        windows(&sink.0[1]),
        [(u32::from(sid(0, 0x02, 0).raw()), 0x7C00_0000)]
    );
}

/// Each I/O APIC a unit with a node names is reported with that unit's node
/// and the requester id its path resolves to; one named by a unit without a
/// node, or by no unit, is not.
#[test]
fn an_io_apic_is_remapped_by_the_unit_whose_scope_names_it() {
    let first = drhd(0, 0, 0, 0xFED9_0000, &scope(3, 2, 0xF0, &[(0x1F, 0)]));
    let second = drhd(
        DRHD_INCLUDE_PCI_ALL,
        0,
        0,
        0xFED9_1000,
        &[
            scope(3, 8, 0x00, &[(0x1E, 7)]),
            scope(4, 0, 0x00, &[(0x1E, 6)]),
        ]
        .concat(),
    );
    let bytes = table(46, 0, &[first, second].concat());
    let dmar = Dmar::parse(&bytes).unwrap();
    let mut seen = Vec::new();
    let both = UnitNodes {
        emitted: 2,
        ..UnitNodes::default()
    };
    ioapic_sources(
        &dmar,
        0x800A_0000,
        both,
        &unwalked,
        &mut |id, node, source| {
            seen.push((id, node, source));
        },
    );
    assert_eq!(
        seen,
        [
            (2, 0x800A_0000, sid(0xF0, 0x1F, 0)),
            (8, 0x800A_0001, sid(0, 0x1E, 7)),
        ],
        "an HPET scope is no I/O APIC"
    );
    seen.clear();
    let first_only = UnitNodes {
        emitted: 1,
        ..UnitNodes::default()
    };
    ioapic_sources(
        &dmar,
        0x800A_0000,
        first_only,
        &unwalked,
        &mut |id, node, source| {
            seen.push((id, node, source));
        },
    );
    assert_eq!(seen, [(2, 0x800A_0000, sid(0xF0, 0x1F, 0))]);
}
