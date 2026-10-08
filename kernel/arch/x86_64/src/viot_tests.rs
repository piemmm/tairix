extern crate std;

use std::vec;
use std::vec::Vec;

use super::*;

const BASE: u32 = 0x800A_0000;
const PCI_KEY: &[u8] = b"virtio,pci-iommu";
const MMIO_KEY: &[u8] = b"virtio,mmio";
const WINDOW: u64 = 0x128;

/// Where the first node sits: straight after the header, as QEMU lays it.
const FIRST: u16 = 48;

fn table(nodes: &[Vec<u8>]) -> Vec<u8> {
    table_at(FIRST, u16::try_from(nodes.len()).unwrap(), &nodes.concat())
}

fn table_at(offset: u16, count: u16, body: &[u8]) -> Vec<u8> {
    let mut buf = vec![0u8; HEADER_LEN];
    buf[..4].copy_from_slice(&VIOT_SIGNATURE);
    buf[36..38].copy_from_slice(&count.to_le_bytes());
    buf[38..40].copy_from_slice(&offset.to_le_bytes());
    buf.extend_from_slice(body);
    let total = u32::try_from(buf.len()).unwrap();
    buf[4..8].copy_from_slice(&total.to_le_bytes());
    reseal(&mut buf);
    buf
}

fn reseal(buf: &mut [u8]) {
    buf[9] = 0;
    let sum = buf.iter().fold(0u8, |acc, b| acc.wrapping_add(*b));
    buf[9] = 0u8.wrapping_sub(sum);
}

fn header(kind: u8, len: usize) -> Vec<u8> {
    let mut out = vec![0u8; len];
    out[0] = kind;
    out[2..4].copy_from_slice(&u16::try_from(len).unwrap().to_le_bytes());
    out
}

fn pci_range(endpoint: u32, segments: (u16, u16), requesters: (u16, u16), unit: u16) -> Vec<u8> {
    let mut out = header(NODE_PCI_RANGE, ENDPOINT_NODE_LEN);
    out[4..8].copy_from_slice(&endpoint.to_le_bytes());
    out[8..10].copy_from_slice(&segments.0.to_le_bytes());
    out[10..12].copy_from_slice(&segments.1.to_le_bytes());
    out[12..14].copy_from_slice(&requesters.0.to_le_bytes());
    out[14..16].copy_from_slice(&requesters.1.to_le_bytes());
    out[16..18].copy_from_slice(&unit.to_le_bytes());
    out
}

fn mmio_endpoint(endpoint: u32, base: u64, unit: u16) -> Vec<u8> {
    let mut out = header(NODE_MMIO, ENDPOINT_NODE_LEN);
    out[4..8].copy_from_slice(&endpoint.to_le_bytes());
    out[8..16].copy_from_slice(&base.to_le_bytes());
    out[16..18].copy_from_slice(&unit.to_le_bytes());
    out
}

fn virtio_pci(segment: u16, requester: u16) -> Vec<u8> {
    let mut out = header(NODE_VIRTIO_PCI, UNIT_NODE_LEN);
    out[4..6].copy_from_slice(&segment.to_le_bytes());
    out[6..8].copy_from_slice(&requester.to_le_bytes());
    out
}

fn virtio_mmio(base: u64) -> Vec<u8> {
    let mut out = header(NODE_VIRTIO_MMIO, UNIT_NODE_LEN);
    out[8..16].copy_from_slice(&base.to_le_bytes());
    out
}

/// What QEMU's q35 builds for `virtio-iommu-pci` at `00:02.0`: the unit, then
/// one range over every bus of the host bridge, numbered as its requester
/// ids.
fn qemu_like() -> Vec<u8> {
    table(&[
        virtio_pci(0, 0x0010),
        pci_range(0, (0, 0), (0, 0xFFFF), FIRST),
    ])
}

struct Collect(Vec<HwNode>);

impl HwNodeSink for Collect {
    fn emit(&mut self, node: HwNode) -> Result<(), DiscoveryError> {
        self.0.push(node);
        Ok(())
    }
}

/// Holds `room` nodes, then refuses.
struct Room(usize);

impl HwNodeSink for Room {
    fn emit(&mut self, _node: HwNode) -> Result<(), DiscoveryError> {
        self.0 = self
            .0
            .checked_sub(1)
            .ok_or(DiscoveryError::MalformedSource)?;
        Ok(())
    }
}

fn emit(viot: &Viot<'_>, sink: &mut dyn HwNodeSink) -> UnitNodes {
    emit_unit_nodes(viot, BASE, PCI_KEY, MMIO_KEY, WINDOW, sink).unwrap()
}

#[test]
fn qemus_table_names_one_function_unit_translating_its_segment() {
    let bytes = qemu_like();
    let viot = Viot::parse(&bytes).unwrap();
    assert_eq!(
        viot.units().collect::<Vec<_>>(),
        [ViotUnit::Pci {
            segment: 0,
            requester: 0x0010
        }]
    );
    assert!(viot.covers(0));
    assert!(!viot.covers(1));
    let mut sink = Collect(Vec::new());
    let nodes = emit(&viot, &mut sink);
    assert_eq!(nodes.emitted, 1);
    let node = &sink.0[0];
    assert_eq!(node.id(), BASE);
    assert_eq!(node.class(), Some(HwDeviceClass::Iommu));
    assert_eq!(node.address(), 0x0010);
    assert_eq!(
        node.match_keys(),
        [HwMatchKey::compatible(PCI_KEY).unwrap()]
    );
    assert_eq!(
        node.resources(),
        [crate::acpi::unit_dma()],
        "a function's windows are its configuration space's to name"
    );
    assert_eq!(
        endpoint(&viot, BASE, nodes, 0, 0x0018),
        Some((BASE, 0x0018))
    );
    assert_eq!(
        endpoint(&viot, BASE, nodes, 0, 0xFFFF),
        Some((BASE, 0xFFFF))
    );
    assert_eq!(
        endpoint(&viot, BASE, nodes, 0, 0x0010),
        None,
        "a unit does not translate its own function"
    );
    assert_eq!(endpoint(&viot, BASE, nodes, 1, 0x0018), None);
    assert!(!viot.strands(nodes, 0));
    assert!(!contested(&viot, BASE, 0, BASE, 0x0018));
}

/// A range's endpoints run across its segments, each segment's functions
/// sixteen bits apart, from the range's first endpoint.
#[test]
fn endpoints_are_numbered_across_segments_from_the_ranges_start() {
    let bytes = table(&[
        virtio_pci(0, 0x0008),
        pci_range(0x1000, (2, 3), (0x0100, 0x01FF), FIRST),
    ]);
    let viot = Viot::parse(&bytes).unwrap();
    let nodes = emit(&viot, &mut Collect(Vec::new()));
    assert_eq!(
        endpoint(&viot, BASE, nodes, 2, 0x0100),
        Some((BASE, 0x1000))
    );
    assert_eq!(
        endpoint(&viot, BASE, nodes, 3, 0x0105),
        Some((BASE, 0x1000 + (1 << 16) + 5))
    );
    assert_eq!(endpoint(&viot, BASE, nodes, 3, 0x0200), None);
    assert_eq!(endpoint(&viot, BASE, nodes, 1, 0x0100), None);
    assert!(viot.covers(2) && viot.covers(3) && !viot.covers(4));
    let segment_3 = 0x1000 + (1 << 16) + 5;
    assert!(
        contested(&viot, BASE, 2, BASE, segment_3),
        "segment 3's function masters as that endpoint"
    );
    assert!(!contested(&viot, BASE, 3, BASE, segment_3));
    assert!(!contested(&viot, BASE, 2, BASE, 0x1000));
    assert!(!contested(&viot, BASE, 2, BASE, 0x1000 + 0x100));
}

/// A table naming more nodes than any topology needs is refused rather than
/// compared pairwise.
#[test]
fn a_table_of_too_many_nodes_is_refused() {
    let unknown = || header(0xFF, NODE_HEADER_LEN);
    let most: Vec<Vec<u8>> = (0..MAX_NODES).map(|_| unknown()).collect();
    assert!(Viot::parse(&table(&most)).is_ok());
    let more: Vec<Vec<u8>> = (0..=MAX_NODES).map(|_| unknown()).collect();
    assert_eq!(Viot::parse(&table(&more)).err(), Some(AcpiError::BadLength));
}

/// Two ranges numbering one endpoint id on one segment give it two masters,
/// read from the table whichever function the boot walk finds first.
#[test]
fn an_endpoint_two_ranges_number_is_contested() {
    let bytes = table(&[
        virtio_pci(1, 0x0008),
        pci_range(0x100, (0, 0), (0x0000, 0x00FF), FIRST),
        pci_range(0x180, (0, 0), (0x0100, 0x01FF), FIRST),
    ]);
    let viot = Viot::parse(&bytes).unwrap();
    let nodes = emit(&viot, &mut Collect(Vec::new()));
    assert_eq!(endpoint(&viot, BASE, nodes, 0, 0x0085), Some((BASE, 0x185)));
    assert_eq!(endpoint(&viot, BASE, nodes, 0, 0x0105), Some((BASE, 0x185)));
    assert!(contested(&viot, BASE, 0, BASE, 0x185));
    assert!(
        !contested(&viot, BASE, 0, BASE, 0x105),
        "one range numbers it"
    );
    assert!(!contested(&viot, BASE, 0, BASE, 0x205), "the second alone");
}

/// A virtio-mmio unit's node carries its window; a platform device it
/// translates contests its endpoint with every function.
#[test]
fn an_mmio_unit_is_keyed_and_windowed_and_its_devices_contest_their_endpoints() {
    let unit = FIRST;
    let bytes = table(&[
        virtio_mmio(0xFEB0_0000),
        mmio_endpoint(7, 0xFEB0_1000, unit),
        pci_range(0, (0, 0), (0, 0x00FF), unit),
    ]);
    let viot = Viot::parse(&bytes).unwrap();
    assert_eq!(
        viot.units().collect::<Vec<_>>(),
        [ViotUnit::Mmio { base: 0xFEB0_0000 }]
    );
    let mut sink = Collect(Vec::new());
    let nodes = emit(&viot, &mut sink);
    let node = &sink.0[0];
    assert_eq!(
        node.match_keys(),
        [HwMatchKey::compatible(MMIO_KEY).unwrap()]
    );
    assert_eq!(
        node.resources(),
        [
            crate::acpi::unit_dma(),
            HwResource::mmio(0xFEB0_0000, WINDOW)
        ]
    );
    assert_eq!(endpoint(&viot, BASE, nodes, 0, 0x0007), Some((BASE, 7)));
    assert!(contested(&viot, BASE, 0, BASE, 7));
    assert!(!contested(&viot, BASE, 0, BASE, 8));
    assert!(!contested(&viot, BASE, 0, BASE + 1, 7), "no such unit");
}

/// A unit the sink had no room for strands the segments its ranges cover,
/// and translates nothing; the units before it are unaffected.
#[test]
fn a_unit_without_a_node_strands_its_segments() {
    let second = FIRST + 16;
    let bytes = table(&[
        virtio_pci(0, 0x0008),
        virtio_pci(1, 0x0008),
        pci_range(0, (0, 0), (0, 0xFFFF), FIRST),
        pci_range(0x10000, (1, 1), (0, 0xFFFF), second),
    ]);
    let viot = Viot::parse(&bytes).unwrap();
    let nodes = emit(&viot, &mut Room(1));
    assert_eq!(nodes.emitted, 1);
    assert!(!viot.strands(nodes, 0));
    assert!(viot.strands(nodes, 1));
    assert!(!viot.strands(nodes, 2));
    assert_eq!(
        endpoint(&viot, BASE, nodes, 0, 0x0010),
        Some((BASE, 0x0010))
    );
    assert_eq!(endpoint(&viot, BASE, nodes, 1, 0x0010), None);
    let all = emit(&viot, &mut Collect(Vec::new()));
    assert_eq!(
        endpoint(&viot, BASE, all, 1, 0x0010),
        Some((BASE + 1, 0x10010))
    );
    assert!(contested(&viot, BASE, 0, BASE + 1, 0x10010));
    assert!(
        !contested(&viot, BASE, 0, BASE, 0x10010),
        "another unit's endpoints are its own"
    );
}

/// A node a later revision defines is stepped over by its length; naming it
/// as a unit is refused.
#[test]
fn a_node_of_a_later_revision_is_skipped_but_is_no_unit() {
    let unknown = header(0x80, 12);
    let unit = FIRST + 12;
    let bytes = table(&[
        unknown.clone(),
        virtio_pci(0, 0x0008),
        pci_range(0, (0, 0), (0, 0xFFFF), unit),
    ]);
    let viot = Viot::parse(&bytes).unwrap();
    assert_eq!(viot.units().count(), 1);
    let nodes = emit(&viot, &mut Collect(Vec::new()));
    assert_eq!(
        endpoint(&viot, BASE, nodes, 0, 0x0020),
        Some((BASE, 0x0020))
    );
    let bytes = table(&[unknown, pci_range(0, (0, 0), (0, 0xFFFF), FIRST)]);
    assert_eq!(Viot::parse(&bytes).err(), Some(AcpiError::BadLength));
}

#[test]
fn an_empty_table_names_nothing() {
    let bytes = table(&[]);
    let viot = Viot::parse(&bytes).unwrap();
    assert_eq!(viot.units().count(), 0);
    assert!(!viot.covers(0));
    assert_eq!(emit(&viot, &mut Collect(Vec::new())).emitted, 0);
}

fn refused(bytes: &[u8]) -> AcpiError {
    match Viot::parse(bytes) {
        Ok(_) => panic!("a malformed table parsed"),
        Err(err) => err,
    }
}

#[test]
fn a_malformed_header_or_node_list_is_refused_whole() {
    let unit = virtio_pci(0, 0x0008);
    assert_eq!(
        refused(&table_at(40, 1, &unit)),
        AcpiError::BadLength,
        "inside the header"
    );
    assert_eq!(
        refused(&table_at(200, 1, &unit)),
        AcpiError::BadLength,
        "past the table"
    );
    assert_eq!(
        refused(&table_at(FIRST, 2, &unit)),
        AcpiError::Truncated,
        "counted past the end"
    );
    let mut short = unit.clone();
    short[2] = 3;
    assert_eq!(refused(&table(&[short])), AcpiError::BadLength);
    let mut long = unit.clone();
    long[2] = 17;
    assert_eq!(refused(&table(&[long])), AcpiError::BadLength);
    let truncated = header(NODE_PCI_RANGE, 16);
    assert_eq!(
        refused(&table(&[unit.clone(), truncated])),
        AcpiError::BadLength
    );
    let truncated = header(NODE_VIRTIO_MMIO, 12);
    assert_eq!(refused(&table(&[truncated])), AcpiError::BadLength);
    let mut bytes = qemu_like();
    bytes[HEADER_LEN + 4] ^= 1;
    assert_eq!(refused(&bytes), AcpiError::BadChecksum);
    let mut bytes = qemu_like();
    bytes[0] = b'X';
    assert_eq!(refused(&bytes), AcpiError::BadSignature);
}

#[test]
fn an_endpoint_naming_no_unit_or_running_backwards_or_overflowing_is_refused() {
    let unit = virtio_pci(0, 0x0008);
    let range = FIRST + 16;
    for (case, node) in [
        ("its own node", pci_range(0, (0, 0), (0, 0xFF), range)),
        (
            "inside the unit",
            pci_range(0, (0, 0), (0, 0xFF), FIRST + 4),
        ),
        ("past the table", pci_range(0, (0, 0), (0, 0xFF), 0x400)),
        ("segments backwards", pci_range(0, (1, 0), (0, 0xFF), FIRST)),
        (
            "requesters backwards",
            pci_range(0, (0, 0), (0x10, 0x0F), FIRST),
        ),
        (
            "endpoints overflowing",
            pci_range(0xFFFF_FF00, (0, 0), (0, 0x100), FIRST),
        ),
        ("a device with no unit", mmio_endpoint(1, 0x1000, range)),
    ] {
        assert_eq!(
            refused(&table(&[unit.clone(), node])),
            AcpiError::BadLength,
            "{case}"
        );
    }
    let fits = pci_range(0xFFFF_FF00, (0, 0), (0, 0xFF), FIRST);
    assert!(Viot::parse(&table(&[unit, fits])).is_ok());
}

/// A function two ranges name, a device two endpoints name, or a unit named
/// twice would each be read as whichever came first.
#[test]
fn a_doubly_named_function_device_or_unit_is_refused() {
    let unit = virtio_pci(0, 0x0008);
    let overlapping = [
        unit.clone(),
        pci_range(0, (0, 1), (0, 0x00FF), FIRST),
        pci_range(0x20000, (1, 2), (0x0080, 0x01FF), FIRST),
    ];
    assert_eq!(refused(&table(&overlapping)), AcpiError::BadLength);
    let apart = [
        unit.clone(),
        pci_range(0, (0, 1), (0, 0x00FF), FIRST),
        pci_range(0x20000, (1, 2), (0x0100, 0x01FF), FIRST),
    ];
    assert!(Viot::parse(&table(&apart)).is_ok());
    let devices = [
        unit.clone(),
        mmio_endpoint(1, 0x1000, FIRST),
        mmio_endpoint(2, 0x1000, FIRST),
    ];
    assert_eq!(refused(&table(&devices)), AcpiError::BadLength);
    assert_eq!(refused(&table(&[unit.clone(), unit])), AcpiError::BadLength);
    let windows = [virtio_mmio(0xFEB0_0000), virtio_mmio(0xFEB0_0000)];
    assert_eq!(refused(&table(&windows)), AcpiError::BadLength);
}

#[test]
fn an_mmio_unit_whose_window_wraps_is_not_emitted() {
    let bytes = table(&[virtio_mmio(u64::MAX - 0x10)]);
    let viot = Viot::parse(&bytes).unwrap();
    assert_eq!(
        emit_unit_nodes(
            &viot,
            BASE,
            PCI_KEY,
            MMIO_KEY,
            WINDOW,
            &mut Collect(Vec::new())
        )
        .err(),
        Some(DiscoveryError::MalformedSource)
    );
}

/// A later unit that cannot be placed leaves no earlier one emitted.
#[test]
fn a_table_one_of_whose_units_wraps_emits_none_of_them() {
    let bytes = table(&[virtio_mmio(0xFEB0_0000), virtio_mmio(u64::MAX - 0x10)]);
    let viot = Viot::parse(&bytes).unwrap();
    let mut sink = Collect(Vec::new());
    assert_eq!(
        emit_unit_nodes(&viot, BASE, PCI_KEY, MMIO_KEY, WINDOW, &mut sink).err(),
        Some(DiscoveryError::MalformedSource)
    );
    assert!(sink.0.is_empty());
}

/// An endpoint names its unit by offset; a unit node too short for its own
/// fields is refused before anything is read through it.
#[test]
fn an_endpoint_naming_a_truncated_unit_is_refused_unread() {
    let unit_at = FIRST + u16::try_from(ENDPOINT_NODE_LEN).unwrap();
    for short in [header(NODE_VIRTIO_PCI, 6), header(NODE_VIRTIO_MMIO, 12)] {
        let range = pci_range(0, (0, 0), (0, 0xFF), unit_at);
        assert_eq!(
            refused(&table(&[range, short.clone()])),
            AcpiError::BadLength
        );
        let device = mmio_endpoint(1, 0x1000, unit_at);
        assert_eq!(refused(&table(&[device, short])), AcpiError::BadLength);
    }
}
