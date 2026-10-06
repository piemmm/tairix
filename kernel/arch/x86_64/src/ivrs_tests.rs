extern crate std;

use std::vec;
use std::vec::Vec;

use super::*;
use crate::dmar::{BridgeBuses, DmaAliases};

/// A padding entry, which names nothing.
const ENTRY_PAD: u8 = 0x00;

fn table(blocks: &[u8]) -> Vec<u8> {
    let total = BLOCKS_OFFSET + blocks.len();
    let mut buf = vec![0u8; total];
    buf[..4].copy_from_slice(&IVRS_SIGNATURE);
    buf[4..8].copy_from_slice(&u32::try_from(total).unwrap().to_le_bytes());
    buf[8] = 2;
    buf[BLOCKS_OFFSET..].copy_from_slice(blocks);
    reseal(&mut buf);
    buf
}

fn reseal(buf: &mut [u8]) {
    buf[9] = 0;
    let sum = buf.iter().fold(0u8, |acc, b| acc.wrapping_add(*b));
    buf[9] = 0u8.wrapping_sub(sum);
}

fn ivhd(kind: u8, device: u16, base: u64, segment: u16, entries: &[u8]) -> Vec<u8> {
    let fixed = entries_offset(kind);
    let len = fixed + entries.len();
    let mut out = vec![0u8; fixed];
    out[0] = kind;
    out[2..4].copy_from_slice(&u16::try_from(len).unwrap().to_le_bytes());
    out[4..6].copy_from_slice(&device.to_le_bytes());
    out[6..8].copy_from_slice(&0x40u16.to_le_bytes());
    out[8..16].copy_from_slice(&base.to_le_bytes());
    out[16..18].copy_from_slice(&segment.to_le_bytes());
    out.extend_from_slice(entries);
    out
}

/// A unity window its devices read and write.
const UNITY_RW: u8 = IVMD_UNITY | IVMD_READ | IVMD_WRITE;

fn ivmd(kind: u8, flags: u8, first: u16, last: u16, base: u64, len: u64) -> Vec<u8> {
    let mut out = vec![0u8; IVMD_LEN];
    out[0] = kind;
    out[1] = flags;
    out[2..4].copy_from_slice(&u16::try_from(IVMD_LEN).unwrap().to_le_bytes());
    out[4..6].copy_from_slice(&first.to_le_bytes());
    out[6..8].copy_from_slice(&last.to_le_bytes());
    out[16..24].copy_from_slice(&base.to_le_bytes());
    out[24..32].copy_from_slice(&len.to_le_bytes());
    out
}

fn entry4(kind: u8, device: u16) -> Vec<u8> {
    let [low, high] = device.to_le_bytes();
    vec![kind, low, high, 0]
}

fn entry8(kind: u8, device: u16, aux: u8, other: u16, last: u8) -> Vec<u8> {
    let [low, high] = device.to_le_bytes();
    let [other_low, other_high] = other.to_le_bytes();
    vec![kind, low, high, 0, aux, other_low, other_high, last]
}

fn special_ioapic(id: u8, device: u16) -> Vec<u8> {
    entry8(ENTRY_SPECIAL, 0, id, device, SPECIAL_IOAPIC)
}

/// What QEMU's `amd-iommu` reports: the unit in a legacy and an EFR block,
/// every device on the segment, and the I/O APIC at `00:14.0`'s id.
fn qemu_like() -> Vec<u8> {
    let entries = [entry4(ENTRY_ALL, 0), special_ioapic(0, 0x00A0)].concat();
    table(
        &[
            ivhd(IVHD_LEGACY, 0x0010, 0xFED8_0000, 0, &entries),
            ivhd(IVHD_EFR, 0x0010, 0xFED8_0000, 0, &entries),
        ]
        .concat(),
    )
}

#[test]
fn a_unit_described_twice_is_read_once_from_its_richer_block() {
    let bytes = qemu_like();
    let ivrs = Ivrs::parse(&bytes).unwrap();
    let units: Vec<Ivhd<'_>> = ivrs.units().collect();
    assert_eq!(units.len(), 1);
    let unit = units[0];
    assert_eq!(unit.device(), 0x0010);
    assert_eq!(unit.register_base(), 0xFED8_0000);
    assert_eq!(unit.kind, IVHD_EFR, "the EFR block was chosen");
    assert!(unit.covers(0x0018) && unit.covers(0xFFFF));
    assert_eq!(unit.ioapics().collect::<Vec<_>>(), [(0, 0x00A0)]);
    assert_eq!(ivrs.unit_for(0, 0x0018), Some(0));
    assert_eq!(ivrs.unit_for(1, 0x0018), None, "another segment");
}

#[test]
fn ranges_are_joined_and_aliases_named() {
    let entries = [
        entry4(ENTRY_SELECT, 0x0008),
        entry4(ENTRY_RANGE_START, 0x0100),
        entry4(ENTRY_RANGE_END, 0x01FF),
        entry8(ENTRY_ALIAS_RANGE, 0x0300, 0, 0x0200, 0),
        entry4(ENTRY_RANGE_END, 0x03FF),
        entry8(ENTRY_ALIAS_SELECT, 0x0410, 0, 0x0400, 0),
        entry4(ENTRY_PAD, 0),
    ]
    .concat();
    let bytes = table(&ivhd(IVHD_LEGACY, 0x0002, 0xFEB8_0000, 0, &entries));
    let ivrs = Ivrs::parse(&bytes).unwrap();
    let unit = ivrs.units().next().unwrap();
    assert_eq!(
        unit.entries().collect::<Vec<_>>(),
        [
            DeviceEntry::Devices(0x0008..=0x0008),
            DeviceEntry::Devices(0x0100..=0x01FF),
            DeviceEntry::Alias {
                devices: 0x0300..=0x03FF,
                alias: 0x0200
            },
            DeviceEntry::Alias {
                devices: 0x0410..=0x0410,
                alias: 0x0400
            },
        ]
    );
    assert!(unit.covers(0x0150));
    assert!(unit.covers(0x0200), "an alias is a stream the unit sees");
    assert!(!unit.covers(0x0009));
    assert_eq!(unit.alias_of(0x0350), Some(0x0200));
    assert_eq!(unit.alias_of(0x0410), Some(0x0400));
    assert_eq!(unit.alias_of(0x0150), None);
}

fn refused(blocks: &[u8]) -> AcpiError {
    Ivrs::parse(&table(blocks)).unwrap_err()
}

#[test]
fn every_malformed_block_refuses_the_whole_table() {
    let open = [entry4(ENTRY_RANGE_START, 1)].concat();
    assert_eq!(
        refused(&ivhd(IVHD_LEGACY, 2, 0xFEB8_0000, 0, &open)),
        AcpiError::BadLength
    );
    let dangling = entry4(ENTRY_RANGE_END, 1);
    assert_eq!(
        refused(&ivhd(IVHD_LEGACY, 2, 0xFEB8_0000, 0, &dangling)),
        AcpiError::BadLength
    );
    let twice = [entry4(ENTRY_RANGE_START, 1), entry4(ENTRY_RANGE_START, 2)].concat();
    assert_eq!(
        refused(&ivhd(IVHD_LEGACY, 2, 0xFEB8_0000, 0, &twice)),
        AcpiError::BadLength
    );
    let odd_special = entry8(ENTRY_SPECIAL, 0, 0, 0xA0, 9);
    assert_eq!(
        refused(&ivhd(IVHD_LEGACY, 2, 0xFEB8_0000, 0, &odd_special)),
        AcpiError::BadLength
    );
    let cut = &entry8(ENTRY_ALIAS_SELECT, 1, 0, 2, 0)[..6];
    assert_eq!(
        refused(&ivhd(IVHD_LEGACY, 2, 0xFEB8_0000, 0, cut)),
        AcpiError::Truncated
    );
    assert_eq!(
        refused(&ivhd(IVHD_LEGACY, 2, 0xFEB8_0800, 0, &[])),
        AcpiError::BadLength
    );
    assert_eq!(
        refused(&ivhd(IVHD_LEGACY, 2, 0, 0, &[])),
        AcpiError::BadLength
    );
    let shared = [
        ivhd(IVHD_LEGACY, 2, 0xFEB8_0000, 0, &[]),
        ivhd(IVHD_LEGACY, 3, 0xFEB8_2000, 0, &[]),
    ]
    .concat();
    assert_eq!(
        refused(&shared),
        AcpiError::BadLength,
        "overlapping register windows"
    );
    assert_eq!(
        refused(&ivmd(IVMD_SELECT, IVMD_UNITY, 1, 1, 0x1000, 0x800)),
        AcpiError::BadLength
    );
    assert_eq!(
        refused(&ivmd(IVMD_RANGE, IVMD_UNITY, 9, 1, 0x1000, 0x1000)),
        AcpiError::BadLength,
        "a range ending before it starts"
    );
    let inverted = [
        entry4(ENTRY_RANGE_START, 0x0109),
        entry4(ENTRY_RANGE_END, 0x0100),
    ]
    .concat();
    assert_eq!(
        refused(&ivhd(IVHD_LEGACY, 2, 0xFEB8_0000, 0, &inverted)),
        AcpiError::BadLength,
        "a device range ending before it starts"
    );
    let mut overrun = ivhd(IVHD_LEGACY, 2, 0xFEB8_0000, 0, &[]);
    overrun[2] = 0xFF;
    assert_eq!(refused(&overrun), AcpiError::BadLength);
    let mut hid = vec![ENTRY_ACPI_HID, 1, 0, 0];
    hid.extend_from_slice(&[0; 18]);
    hid[21] = 40;
    assert_eq!(
        refused(&ivhd(IVHD_LEGACY, 2, 0xFEB8_0000, 0, &hid)),
        AcpiError::Truncated,
        "a UID past the block"
    );
}

#[test]
fn a_memory_definition_names_its_devices_and_whole_pages() {
    let bytes = table(
        &[
            ivhd(IVHD_LEGACY, 2, 0xFEB8_0000, 0, &entry4(ENTRY_ALL, 0)),
            ivmd(IVMD_SELECT, UNITY_RW, 0x00A0, 0, 0x7B80_0000, 0x10_0000),
            ivmd(
                IVMD_RANGE,
                IVMD_EXCLUSION,
                0x0100,
                0x0107,
                0x9000_0000,
                0x1000,
            ),
            ivmd(IVMD_ALL, 0, 0, 0, 0xA000_0000, 0x1000),
        ]
        .concat(),
    );
    let ivrs = Ivrs::parse(&bytes).unwrap();
    let windows: Vec<Ivmd> = ivrs.memory_definitions().collect();
    assert_eq!(
        windows.len(),
        2,
        "a definition that is neither unity nor exclusion keeps nothing"
    );
    assert!(windows[0].names(0, 0x00A0) && !windows[0].names(0, 0x00A1));
    assert!(windows[1].names(0, 0x0104) && !windows[1].names(0, 0x0108));
    assert_eq!(
        windows[1].access(),
        ReservedAccess::ReadWrite,
        "an exclusion"
    );
    assert_eq!((windows[1].base(), windows[1].len()), (0x9000_0000, 0x1000));
}

/// A memory definition names devices on its own segment alone, and keeps
/// for them only the access it allows: a unity window allowing nothing
/// keeps nothing.
#[test]
fn a_memory_definition_names_its_segment_s_devices_for_the_access_it_allows() {
    let mut other_segment = ivmd(IVMD_SELECT, UNITY_RW, 0x00A0, 0, 0x7B80_0000, 0x1000);
    other_segment[8..10].copy_from_slice(&1u16.to_le_bytes());
    let bytes = table(
        &[
            ivhd(IVHD_LEGACY, 2, 0xFEB8_0000, 0, &entry4(ENTRY_ALL, 0)),
            other_segment,
            ivmd(
                IVMD_SELECT,
                IVMD_UNITY | IVMD_READ,
                0x00B0,
                0,
                0x7C00_0000,
                0x1000,
            ),
            ivmd(IVMD_SELECT, IVMD_UNITY, 0x00C0, 0, 0x7D00_0000, 0x1000),
        ]
        .concat(),
    );
    let ivrs = Ivrs::parse(&bytes).unwrap();
    let windows: Vec<Ivmd> = ivrs.memory_definitions().collect();
    assert_eq!(windows.len(), 2, "unity allowing nothing keeps nothing");
    assert!(windows[0].names(1, 0x00A0) && !windows[0].names(0, 0x00A0));
    assert_eq!(windows[1].access(), ReservedAccess::Read);
}

/// One segment's walk as a test sets it out.
#[derive(Default)]
struct TestWalk {
    functions: Vec<u16>,
    aliases: Vec<(u16, u16)>,
    untrusted: Vec<u16>,
}

impl BridgeBuses for TestWalk {
    fn bus_range(&self, _bridge: SourceId) -> Option<(u8, u8)> {
        None
    }
}

impl DmaAliases for TestWalk {
    fn aliases(&self, source: SourceId, visit: &mut dyn FnMut(SourceId)) {
        for &(of, alias) in &self.aliases {
            if of == source.raw() {
                visit(SourceId::from_raw(alias));
            }
        }
    }
}

impl Fabric for TestWalk {
    fn untrusted(&self, source: SourceId) -> bool {
        self.untrusted.contains(&source.raw())
    }
}

impl Walk for TestWalk {
    fn functions(&self, visit: &mut dyn FnMut(SourceId)) {
        for &function in &self.functions {
            visit(SourceId::from_raw(function));
        }
    }
}

struct Collect(Vec<HwNode>);

impl HwNodeSink for Collect {
    fn emit(&mut self, node: HwNode) -> Result<(), DiscoveryError> {
        self.0.push(node);
        Ok(())
    }
}

fn windows(node: &HwNode) -> Vec<(u32, u64)> {
    node.resources()
        .iter()
        .filter_map(|r| r.iommu_reserved().ok())
        .map(|window| (window.stream(), window.base()))
        .collect()
}

/// A unit's node carries its registers and its own function, and keeps each
/// memory definition's window for every present function it names — under
/// its own id, its firmware alias and its walk's alias, once each — and for
/// none below an external-facing port.
#[test]
fn a_unit_keeps_firmware_windows_for_the_functions_present() {
    let entries = [
        entry4(ENTRY_ALL, 0),
        entry8(ENTRY_ALIAS_SELECT, 0x0310, 0, 0x0300, 0),
    ]
    .concat();
    let bytes = table(
        &[
            ivhd(IVHD_LEGACY, 0x0002, 0xFEB8_0000, 0, &entries),
            ivmd(IVMD_ALL, UNITY_RW, 0, 0, 0x7B80_0000, 0x1000),
        ]
        .concat(),
    );
    let ivrs = Ivrs::parse(&bytes).unwrap();
    let walk = TestWalk {
        functions: vec![0x00A0, 0x0310, 0x0400],
        aliases: vec![(0x00A0, 0x0098)],
        untrusted: vec![0x0400],
    };
    let mut sink = Collect(Vec::new());
    let placed = emit_unit_nodes(
        &ivrs,
        0x800A_0000,
        b"amd,iommu",
        &|segment| (segment == 0).then_some(&walk as &dyn Walk),
        &mut sink,
    )
    .unwrap();
    assert_eq!(
        placed,
        UnitNodes {
            emitted: 1,
            dropped: 0,
            untrusted: 1,
        }
    );
    let node = &sink.0[0];
    assert_eq!(node.class(), Some(HwDeviceClass::Iommu));
    assert_eq!(node.address(), 0x0002);
    assert_eq!(
        node.resources()[0],
        HwResource::mmio(0xFEB8_0000, UNIT_REGISTER_LEN)
    );
    assert_eq!(
        windows(node),
        [
            (0x00A0, 0x7B80_0000),
            (0x0098, 0x7B80_0000),
            (0x0310, 0x7B80_0000),
            (0x0300, 0x7B80_0000),
        ]
    );
    assert_eq!(
        unit_node(&ivrs, 0x800A_0000, placed, 0, 0x0310),
        Some(0x800A_0000)
    );
    assert_eq!(
        unit_node(&ivrs, 0x800A_0000, UnitNodes::default(), 0, 0x0310),
        None,
        "a unit with no node translates nothing"
    );
}

/// With no walk of its segment a unit still comes up, keeping no window.
#[test]
fn a_unit_on_an_unwalked_segment_keeps_no_window() {
    let bytes = table(
        &[
            ivhd(IVHD_LEGACY, 0x0002, 0xFEB8_0000, 0, &entry4(ENTRY_ALL, 0)),
            ivmd(IVMD_ALL, UNITY_RW, 0, 0, 0x7B80_0000, 0x1000),
        ]
        .concat(),
    );
    let ivrs = Ivrs::parse(&bytes).unwrap();
    let mut sink = Collect(Vec::new());
    let placed = emit_unit_nodes(&ivrs, 0x800A_0000, b"amd,iommu", &|_| None, &mut sink).unwrap();
    assert_eq!(placed.emitted, 1);
    assert_eq!(placed.dropped, 1);
    assert!(windows(&sink.0[0]).is_empty());
}

/// An unwalked unit counts as dropped only the windows naming a device it
/// covers, on its own segment.
#[test]
fn an_unwalked_unit_counts_only_the_windows_it_would_have_kept() {
    let mut elsewhere = ivmd(IVMD_ALL, UNITY_RW, 0, 0, 0x7C00_0000, 0x1000);
    elsewhere[8..10].copy_from_slice(&1u16.to_le_bytes());
    let bytes = table(
        &[
            ivhd(
                IVHD_LEGACY,
                0x0002,
                0xFEB8_0000,
                0,
                &[
                    entry4(ENTRY_RANGE_START, 0x0100),
                    entry4(ENTRY_RANGE_END, 0x01FF),
                ]
                .concat(),
            ),
            ivmd(IVMD_SELECT, UNITY_RW, 0x0180, 0, 0x7B80_0000, 0x1000),
            ivmd(IVMD_RANGE, UNITY_RW, 0x0300, 0x0400, 0x7B90_0000, 0x1000),
            elsewhere,
        ]
        .concat(),
    );
    let ivrs = Ivrs::parse(&bytes).unwrap();
    let mut sink = Collect(Vec::new());
    let placed = emit_unit_nodes(&ivrs, 0x800A_0000, b"amd,iommu", &|_| None, &mut sink).unwrap();
    assert_eq!(
        placed.dropped, 1,
        "devices it does not cover, a segment it is not on"
    );
}

#[test]
fn an_io_apic_is_remapped_by_the_unit_naming_it() {
    let bytes = qemu_like();
    let ivrs = Ivrs::parse(&bytes).unwrap();
    let mut seen = Vec::new();
    let placed = UnitNodes {
        emitted: 1,
        ..UnitNodes::default()
    };
    ioapic_sources(&ivrs, 0x800A_0000, placed, &mut |id, node, device| {
        seen.push((id, node, device));
    });
    assert_eq!(seen, [(0, 0x800A_0000, 0x00A0)]);
    seen.clear();
    ioapic_sources(
        &ivrs,
        0x800A_0000,
        UnitNodes::default(),
        &mut |id, node, device| {
            seen.push((id, node, device));
        },
    );
    assert!(seen.is_empty(), "a unit with no node remaps nothing");
}
