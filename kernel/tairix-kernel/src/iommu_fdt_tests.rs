use alloc::format;
use alloc::vec::Vec;
use core::cell::RefCell;

use tairix_abi::{DmaCoherence, HwNode, IommuGroup, IommuReservedWindow};
use tairix_arch_api::fdtwalk::{FdtDiscovery, FdtPlatform};
use tairix_arch_api::PlatformDiscovery;
use tairix_fdt::fixture::DtbBuilder;
use tairix_fdt::Fdt;
use tairix_pci::topology::Topology;

use super::*;
use crate::test_support::NullSink;

/// The smallest port: one interrupt cell straight through.
struct Bare;

impl FdtPlatform for Bare {
    const DEFAULT_DMA_COHERENCE: DmaCoherence = DmaCoherence::Snooped;

    fn interrupt_cells(&self) -> usize {
        1
    }

    fn from_tree(_fdt: &Fdt<'_>) -> Self {
        Self
    }

    fn interrupt_line(&self, specifier: &[u8]) -> Option<u32> {
        Some(u32::from_be_bytes(specifier.try_into().ok()?))
    }

    fn root_interrupt_controller(&self) -> Option<u32> {
        None
    }
}

/// Records each refusal's node and reason.
#[derive(Default)]
struct Refusals(RefCell<Vec<(u64, &'static str)>>);

impl Sink for Refusals {
    fn write_event(&self, event: &Event<'_>) {
        if event.id != tairix_kernel_core::AuditEvent::DmaTranslationBypass.id() {
            return;
        }
        let node = event.fields.iter().find_map(|field| match field.value {
            FieldValue::UnsignedInt(node) if field.key == "node" => Some(node),
            _ => None,
        });
        let reason = event.fields.iter().find_map(|field| match field.value {
            FieldValue::Str(reason) if field.key == "reason" => {
                ["unconfinable", "unkept_window", "undescribed"]
                    .into_iter()
                    .find(|known| *known == reason)
            }
            _ => None,
        });
        if let (Some(node), Some(reason)) = (node, reason) {
            self.0.borrow_mut().push((node, reason));
        }
    }
}

const SMMU: u32 = 0x8004;
const OTHER_SMMU: u32 = 0x8005;

fn cells(values: &[u32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_be_bytes()).collect()
}

/// Walk `blob` as a port does, then read its topology.
fn read(blob: &[u8], log: &dyn Sink) -> (FdtUnits, Vec<HwNode>) {
    let fdt = Fdt::new(blob).unwrap();
    let mut sink = CollectingHwNodeSink::new();
    FdtDiscovery::<Bare>::new(Fdt::new(blob).unwrap())
        .discover(&mut sink)
        .unwrap();
    let units = FdtUnits::read(&fdt, &mut sink, log);
    (units, sink.into_vec())
}

fn by_compatible<'a>(nodes: &'a [HwNode], compatible: &[u8]) -> &'a HwNode {
    nodes
        .iter()
        .find(|node| {
            node.match_keys()
                .iter()
                .any(|k| k.compatible_bytes() == compatible)
        })
        .unwrap()
}

fn group_of(node: &HwNode) -> Option<IommuGroup> {
    node.resources().iter().find_map(|r| r.iommu_group().ok())
}

fn windows_of(node: &HwNode) -> Vec<IommuReservedWindow> {
    node.resources()
        .iter()
        .filter_map(|r| r.iommu_reserved().ok())
        .collect()
}

fn smmu(b: &mut DtbBuilder, name: &str, phandle: u32, status: Option<&str>) {
    b.begin_node(name);
    b.prop_str("compatible", "arm,smmu-v3");
    b.prop("reg", &cells(&[0, 0x905_0000, 0, 0x2_0000]));
    b.prop_u32("#iommu-cells", 1);
    b.prop_u32("phandle", phandle);
    if let Some(status) = status {
        b.prop_str("status", status);
    }
    b.end_node();
}

fn host(b: &mut DtbBuilder, segment: u32, map: Option<&[u32]>) {
    host_at(b, 0x1000_0000 + segment * 0x1000_0000, segment, map);
}

/// A host whose configuration space sits at `base`.
fn host_at(b: &mut DtbBuilder, base: u32, segment: u32, map: Option<&[u32]>) {
    b.begin_node(&format!("pcie@{base:x}"));
    b.prop_str("compatible", "pci-host-ecam-generic");
    b.prop_u32("#address-cells", 3);
    b.prop_u32("#size-cells", 2);
    b.prop("reg", &cells(&[0x40, base, 0, 0x1000_0000]));
    b.prop_u32("linux,pci-domain", segment);
    b.prop(
        "ranges",
        &cells(&[0x0200_0000, 0, 0x1000_0000, 0, 0x1000_0000, 0, 0x2EFF_0000]),
    );
    if let Some(map) = map {
        b.prop("iommu-map", &cells(map));
    }
}

fn tree(build: impl FnOnce(&mut DtbBuilder)) -> Vec<u8> {
    let mut b = DtbBuilder::new();
    b.begin_node("");
    b.prop_u32("#address-cells", 2);
    b.prop_u32("#size-cells", 2);
    build(&mut b);
    b.end_node();
    b.build()
}

fn walk() -> Topology {
    Topology::new(Vec::new()).unwrap()
}

/// QEMU `virt,iommu=smmuv3`: one unit for the whole bus, at the requester id.
#[test]
fn a_host_mapped_whole_reaches_its_unit_at_each_requester_id() {
    let blob = tree(|b| {
        smmu(b, "smmuv3@9050000", SMMU, None);
        host(b, 0, Some(&[0, SMMU, 0, 0x1_0000]));
        b.end_node();
    });
    let (units, nodes) = read(&blob, &NullSink);
    let unit = by_compatible(&nodes, b"arm,smmu-v3").id();
    assert!(units.covers(0) && !units.strands(0));
    assert!(!units.covers(1));
    assert_eq!(units.stream(0, &walk(), 0x0010), Some((unit, 0x10)));
    assert_eq!(units.stream(1, &walk(), 0x0010), None);
    assert!(
        !units.contested(0, unit, 0x10),
        "a host's own image does not contest it"
    );
}

/// Two hosts naming one segment: which the PCI bring-up reached cannot be
/// told, so the map one of them names is no one's, and the segment strands.
#[test]
fn a_segment_two_hosts_name_takes_neither_host_s_map() {
    for (first, second) in [
        (None, Some(&[0, SMMU, 0, 0x1_0000][..])),
        (Some(&[0, SMMU, 0, 0x1_0000][..]), None),
    ] {
        let blob = tree(|b| {
            smmu(b, "smmuv3@9050000", SMMU, None);
            host_at(b, 0x1000_0000, 0, first);
            b.end_node();
            host_at(b, 0x5000_0000, 0, second);
            b.end_node();
        });
        let (units, _) = read(&blob, &NullSink);
        assert!(units.covers(0) && units.strands(0));
        assert_eq!(units.stream(0, &walk(), 0x10), None);
    }
}

/// QEMU's virtio-iommu (and riscv-iommu-pci) leaves its own function out of
/// the map: that function is untranslated, the rest are not.
#[test]
fn a_function_the_map_leaves_out_is_untranslated() {
    let blob = tree(|b| {
        host(b, 0, Some(&[0, SMMU, 0, 0x10, 0x11, SMMU, 0x11, 0xFFEF]));
        b.begin_node("virtio_iommu@2,0");
        b.prop_str("compatible", "virtio,pci-iommu");
        b.prop("reg", &cells(&[0x1000, 0, 0, 0, 0]));
        b.prop_u32("#iommu-cells", 1);
        b.prop_u32("phandle", SMMU);
        b.end_node();
        b.end_node();
    });
    let (units, nodes) = read(&blob, &NullSink);
    let unit = by_compatible(&nodes, b"virtio,pci-iommu").id();
    assert_eq!(units.stream(0, &walk(), 0x0F), Some((unit, 0x0F)));
    assert_eq!(units.stream(0, &walk(), 0x10), None);
    assert_eq!(units.stream(0, &walk(), 0x11), Some((unit, 0x11)));
}

/// QEMU riscv64 `virt,iommu-sys=on` opens its map with an entry mapping
/// nothing, and masks nothing.
#[test]
fn an_entry_mapping_nothing_is_passed_over() {
    let blob = tree(|b| {
        smmu(b, "iommu@3010000", SMMU, None);
        host(b, 0, Some(&[0, SMMU, 0, 0, 0, SMMU, 0, 0xFFFF]));
        b.end_node();
    });
    let (units, nodes) = read(&blob, &NullSink);
    let unit = by_compatible(&nodes, b"arm,smmu-v3").id();
    assert!(!units.strands(0));
    assert_eq!(units.stream(0, &walk(), 0), Some((unit, 0)));
    assert_eq!(units.stream(0, &walk(), 0xFFFF), None);
}

#[test]
fn a_mask_folds_requester_ids_before_they_are_mapped() {
    let blob = tree(|b| {
        smmu(b, "smmuv3@9050000", SMMU, None);
        host(b, 0, Some(&[0, SMMU, 0x4000, 0x1_0000]));
        b.prop_u32("iommu-map-mask", 0xFFF8);
        b.end_node();
    });
    let (units, nodes) = read(&blob, &NullSink);
    let unit = by_compatible(&nodes, b"arm,smmu-v3").id();
    assert_eq!(units.stream(0, &walk(), 0x0B), Some((unit, 0x4008)));
}

/// A map's specifier is one cell: it cannot name a unit taking two, and the
/// segment it would describe strands rather than pass its functions by.
#[test]
fn a_map_naming_a_unit_of_another_specifier_width_strands_its_segment() {
    let blob = tree(|b| {
        b.begin_node("smmuv3@9050000");
        b.prop_str("compatible", "arm,smmu-v3");
        b.prop("reg", &cells(&[0, 0x905_0000, 0, 0x2_0000]));
        b.prop_u32("#iommu-cells", 2);
        b.prop_u32("phandle", SMMU);
        b.end_node();
        host(b, 0, Some(&[0, SMMU, 0, 0x1_0000]));
        b.end_node();
    });
    let (units, _) = read(&blob, &NullSink);
    assert!(units.covers(0) && units.strands(0));
    assert_eq!(units.stream(0, &walk(), 0x10), None);
}

/// A map naming a disabled unit leaves its ids untranslated; one naming a
/// node that is no unit, or that does not decode, leaves the segment
/// undescribed: nothing on it can be confined.
#[test]
fn a_map_is_untranslated_through_a_disabled_unit_and_undescribed_through_no_unit() {
    let disabled = tree(|b| {
        smmu(b, "smmuv3@9050000", SMMU, Some("disabled"));
        host(b, 0, Some(&[0, SMMU, 0, 0x1_0000]));
        b.end_node();
    });
    let (units, _) = read(&disabled, &NullSink);
    assert!(units.covers(0) && !units.strands(0));
    assert_eq!(units.stream(0, &walk(), 0x10), None);

    let not_a_unit = tree(|b| {
        b.begin_node("gpio@0");
        b.prop_str("compatible", "test,gpio");
        b.prop_u32("phandle", SMMU);
        b.end_node();
        host(b, 0, Some(&[0, SMMU, 0, 0x1_0000]));
        b.end_node();
    });
    let (units, _) = read(&not_a_unit, &NullSink);
    assert!(units.covers(0) && units.strands(0));

    let malformed = tree(|b| {
        smmu(b, "smmuv3@9050000", SMMU, None);
        host(b, 0, Some(&[0, SMMU, 0]));
        b.end_node();
    });
    let (units, _) = read(&malformed, &NullSink);
    assert!(units.covers(0) && units.strands(0));
}

/// Masters sharing a stream form one group, transitively, named by their
/// least stream; a master alone forms its own.
#[test]
fn platform_masters_sharing_a_stream_form_one_group_named_by_its_least_stream() {
    let blob = tree(|b| {
        for (name, compatible, iommus) in [
            ("a@1000", "test,a", &[SMMU, 0x21, SMMU, 0x22][..]),
            ("b@2000", "test,b", &[SMMU, 0x22, SMMU, 0x30][..]),
            ("c@3000", "test,c", &[SMMU, 0x30][..]),
            ("d@4000", "test,d", &[SMMU, 0x40][..]),
        ] {
            b.begin_node(name);
            b.prop_str("compatible", compatible);
            b.prop("iommus", &cells(iommus));
            b.end_node();
        }
        smmu(b, "smmuv3@9050000", SMMU, None);
    });
    let (_, nodes) = read(&blob, &NullSink);
    let unit = by_compatible(&nodes, b"arm,smmu-v3").id();
    let joined = Some(IommuGroup::new(unit, 0x21));
    for member in [&b"test,a"[..], b"test,b", b"test,c"] {
        assert_eq!(
            group_of(by_compatible(&nodes, member)),
            joined,
            "{member:?}"
        );
    }
    assert_eq!(
        group_of(by_compatible(&nodes, b"test,d")),
        Some(IommuGroup::new(unit, 0x40))
    );
}

/// A master sharing a stream with a host's devices, or mastering through two
/// units, is confined by no group; the host's devices on that stream are
/// contested.
#[test]
fn a_master_no_group_can_confine_is_refused_and_audited() {
    let blob = tree(|b| {
        b.begin_node("stray@1000");
        b.prop_str("compatible", "test,stray");
        b.prop("iommus", &cells(&[SMMU, 0x18]));
        b.end_node();
        b.begin_node("split@2000");
        b.prop_str("compatible", "test,split");
        b.prop("iommus", &cells(&[SMMU, 0x2_0000, OTHER_SMMU, 1]));
        b.end_node();
        smmu(b, "smmuv3@9050000", SMMU, None);
        smmu(b, "smmuv3@9060000", OTHER_SMMU, None);
        host(b, 0, Some(&[0, SMMU, 0, 0x1_0000]));
        b.end_node();
    });
    let log = Refusals::default();
    let (units, nodes) = read(&blob, &log);
    let unit = nodes
        .iter()
        .find(|node| node.resources().iter().any(|r| r.base() == 0x905_0000))
        .unwrap()
        .id();
    let stray = by_compatible(&nodes, b"test,stray");
    let split = by_compatible(&nodes, b"test,split");
    assert_eq!(group_of(stray), None);
    assert_eq!(group_of(split), None);
    assert!(units.contested(0, unit, 0x18));
    assert!(!units.contested(0, unit, 0x19));
    let refused = log.0.borrow();
    assert!(refused.contains(&(u64::from(stray.id()), "unconfinable")));
    assert!(refused.contains(&(u64::from(split.id()), "unconfinable")));
}

fn scanout_tree(iova: u32, unit_slots: usize) -> Vec<u8> {
    tree(|b| {
        b.begin_node("reserved-memory");
        b.prop_u32("#address-cells", 2);
        b.prop_u32("#size-cells", 2);
        b.prop("ranges", &[]);
        b.begin_node("framebuffer@80000000");
        b.prop("reg", &cells(&[0, 0x8000_0000, 0, 0x80_0000]));
        b.prop("iommu-addresses", &cells(&[9, 0, iova, 0, 0x80_0000]));
        b.prop_u32("phandle", 8);
        b.end_node();
        b.end_node();
        b.begin_node("display@1000");
        b.prop_str("compatible", "test,display");
        b.prop("iommus", &cells(&[SMMU, 0x50, SMMU, 0x51]));
        b.prop_u32("memory-region", 8);
        b.prop_u32("phandle", 9);
        b.end_node();
        b.begin_node("smmuv3@9050000");
        b.prop_str("compatible", "arm,smmu-v3");
        let regs: Vec<u32> = (0..u32::try_from(unit_slots).unwrap())
            .flat_map(|at| [0, 0x905_0000 + at * 0x1000, 0, 0x1000])
            .collect();
        b.prop("reg", &cells(&regs));
        b.prop_u32("#iommu-cells", 1);
        b.prop_u32("phandle", SMMU);
        b.end_node();
    })
}

/// Firmware still scanning out through a master's streams keeps that window
/// on each of them, at its own address, on the unit's node.
#[test]
fn a_firmware_window_the_master_lists_is_kept_on_each_of_its_streams() {
    let (_, nodes) = read(&scanout_tree(0x8000_0000, 1), &NullSink);
    let unit = by_compatible(&nodes, b"arm,smmu-v3");
    assert_eq!(
        windows_of(unit),
        [
            IommuReservedWindow::new(0x50, 0x8000_0000, 0x80_0000, ReservedAccess::ReadWrite)
                .unwrap(),
            IommuReservedWindow::new(0x51, 0x8000_0000, 0x80_0000, ReservedAccess::ReadWrite)
                .unwrap(),
        ]
    );
    assert!(group_of(by_compatible(&nodes, b"test,display")).is_some());
}

/// A window mapping memory elsewhere than at its own address, or one the
/// unit's node has no room left to keep, refuses the master's group: its
/// domain could not keep what firmware asks of it.
#[test]
fn a_window_the_domain_cannot_keep_refuses_the_masters_group() {
    for blob in [
        scanout_tree(0x4000_0000, 1),
        scanout_tree(0x8000_0000, tairix_abi::HW_NODE_MAX_RESOURCES - 1),
    ] {
        let log = Refusals::default();
        let (_, nodes) = read(&blob, &log);
        let display = by_compatible(&nodes, b"test,display");
        assert_eq!(group_of(display), None);
        assert!(windows_of(by_compatible(&nodes, b"arm,smmu-v3")).is_empty());
        assert_eq!(
            log.0.borrow().as_slice(),
            [(u64::from(display.id()), "unkept_window")]
        );
    }
}

/// A master the walk gave no DMA authority, as its `iommus` named no unit it
/// could describe, is audited.
#[test]
fn a_master_left_without_dma_authority_is_audited() {
    let blob = tree(|b| {
        b.begin_node("lost@1000");
        b.prop_str("compatible", "test,lost");
        b.prop("iommus", &cells(&[0x99, 1]));
        b.end_node();
        b.begin_node("quiet@2000");
        b.prop_str("compatible", "test,quiet");
        b.prop("iommus", &[]);
        b.end_node();
    });
    let log = Refusals::default();
    let (_, nodes) = read(&blob, &log);
    assert_eq!(
        log.0.borrow().as_slice(),
        [(
            u64::from(by_compatible(&nodes, b"test,lost").id()),
            "undescribed"
        )]
    );
}

/// [`Bare`] with a root interrupt controller, phandle 1, of three-cell
/// specifiers: the GIC's shape, its second cell the line, its third's low
/// bits an edge.
struct Gic;

impl FdtPlatform for Gic {
    const DEFAULT_DMA_COHERENCE: DmaCoherence = DmaCoherence::Snooped;

    fn interrupt_cells(&self) -> usize {
        3
    }

    fn from_tree(_fdt: &Fdt<'_>) -> Self {
        Self
    }

    fn interrupt_line(&self, specifier: &[u8]) -> Option<u32> {
        Some(u32::from_be_bytes(specifier.get(4..8)?.try_into().ok()?))
    }

    fn edge_triggered(&self, specifier: &[u8]) -> bool {
        specifier.get(11).is_some_and(|flags| flags & 0b11 != 0)
    }

    fn root_interrupt_controller(&self) -> Option<u32> {
        Some(1)
    }
}

/// QEMU `virt`'s `SMMUv3` names its four edge-triggered lines; the unit's node
/// says which carries its faults, and the facility finds that line.
#[test]
fn an_smmu_s_node_states_the_line_it_raises_its_faults_on() {
    let blob = tree(|b| {
        b.prop_u32("interrupt-parent", 1);
        b.begin_node("intc@8000000");
        b.prop_str("compatible", "arm,cortex-a15-gic");
        b.prop("interrupt-controller", &[]);
        b.prop_u32("#interrupt-cells", 3);
        b.prop_u32("phandle", 1);
        b.end_node();
        b.begin_node("smmuv3@9050000");
        b.prop_str("compatible", "arm,smmu-v3");
        b.prop("reg", &cells(&[0, 0x905_0000, 0, 0x2_0000]));
        b.prop(
            "interrupts",
            &cells(&[0, 0x4a, 1, 0, 0x4b, 1, 0, 0x4c, 1, 0, 0x4d, 1]),
        );
        b.prop("interrupt-names", b"gerror\0eventq\0priq\0cmdq-sync\0");
        b.prop_u32("#iommu-cells", 1);
        b.prop_u32("phandle", SMMU);
        b.end_node();
    });
    let fdt = Fdt::new(&blob).unwrap();
    let mut sink = CollectingHwNodeSink::new();
    FdtDiscovery::<Gic>::new(Fdt::new(&blob).unwrap())
        .discover(&mut sink)
        .unwrap();
    let _ = FdtUnits::read(&fdt, &mut sink, &NullSink);
    let nodes = sink.into_vec();
    let unit = by_compatible(&nodes, b"arm,smmu-v3");
    assert_eq!(
        tairix_kernel_core::iommu::WiredFaults::of(unit),
        Some(tairix_kernel_core::iommu::WiredFaults {
            line: 0x4b,
            trigger: tairix_kernel_irq::Trigger::Edge,
            place: 1,
        })
    );
}

/// [`Bare`] with a root interrupt controller, phandle 1, of one-cell
/// specifiers: the PLIC's shape.
struct Plic;

impl FdtPlatform for Plic {
    const DEFAULT_DMA_COHERENCE: DmaCoherence = DmaCoherence::Snooped;

    fn interrupt_cells(&self) -> usize {
        1
    }

    fn from_tree(_fdt: &Fdt<'_>) -> Self {
        Self
    }

    fn interrupt_line(&self, specifier: &[u8]) -> Option<u32> {
        Some(u32::from_be_bytes(specifier.get(..4)?.try_into().ok()?))
    }

    fn root_interrupt_controller(&self) -> Option<u32> {
        Some(1)
    }
}

/// A RISC-V IOMMU raises its faults on the first line its node names, which
/// QEMU's `virt` gives as two-cell specifiers under its one-cell PLIC: read
/// one cell at a time, the first line is still the unit's own.
#[test]
fn a_riscv_iommu_raises_its_faults_on_the_first_line_its_node_names() {
    let blob = tree(|b| {
        b.prop_u32("interrupt-parent", 1);
        b.begin_node("plic@c000000");
        b.prop_str("compatible", "riscv,plic0");
        b.prop("interrupt-controller", &[]);
        b.prop_u32("#interrupt-cells", 1);
        b.prop_u32("phandle", 1);
        b.end_node();
        b.begin_node("iommu@3010000");
        b.prop_str("compatible", "riscv,iommu");
        b.prop("reg", &cells(&[0, 0x301_0000, 0, 0x1000]));
        b.prop("interrupts", &cells(&[0x24, 1, 0x25, 1, 0x26, 1, 0x27, 1]));
        b.prop_u32("#iommu-cells", 1);
        b.prop_u32("phandle", SMMU);
        b.end_node();
    });
    let fdt = Fdt::new(&blob).unwrap();
    let mut sink = CollectingHwNodeSink::new();
    FdtDiscovery::<Plic>::new(Fdt::new(&blob).unwrap())
        .discover(&mut sink)
        .unwrap();
    let _ = FdtUnits::read(&fdt, &mut sink, &NullSink);
    let nodes = sink.into_vec();
    let unit = by_compatible(&nodes, tairix_kernel_iommu_riscv::COMPATIBLE);
    assert_eq!(
        tairix_kernel_core::iommu::WiredFaults::of(unit),
        Some(tairix_kernel_core::iommu::WiredFaults {
            line: 0x24,
            trigger: tairix_kernel_irq::Trigger::Level,
            place: 0,
        })
    );
}

/// A `virtio,mmio` slot at `base`, naming `iommus` where given.
fn slot(b: &mut DtbBuilder, base: u32, iommus: Option<&[u32]>) {
    b.begin_node(&format!("virtio_mmio@{base:x}"));
    b.prop_str("compatible", "virtio,mmio");
    b.prop("reg", &cells(&[0, base, 0, 0x200]));
    if let Some(iommus) = iommus {
        b.prop("iommus", &cells(iommus));
    }
    b.end_node();
}

/// The device a probe finds in a slot masters DMA as the walk described the
/// slot: through its streams, around every unit, or — the walk refusing to
/// describe it — not at all; and it joins its slot's group.
#[test]
fn a_slots_device_masters_dma_as_the_slot_is_described() {
    let blob = tree(|b| {
        smmu(b, "smmuv3@9050000", SMMU, None);
        slot(b, 0x0A00_0000, Some(&[SMMU, 0x20, SMMU, 0x21]));
        slot(b, 0x0A00_0200, Some(&[0x99, 1]));
        slot(b, 0x0A00_0400, None);
    });
    let fdt = Fdt::new(&blob).unwrap();
    let mut sink = CollectingHwNodeSink::new();
    FdtDiscovery::<Bare>::new(Fdt::new(&blob).unwrap())
        .discover(&mut sink)
        .unwrap();
    let unit = by_compatible(sink.nodes(), b"arm,smmu-v3").id();
    let dma = SlotDma::read(&fdt, sink.nodes(), DmaCoherence::Snooped);
    let translated: Vec<IommuStreams> = dma.streams(0x0A00_0000).unwrap().collect();
    assert_eq!(translated, [IommuStreams::new(unit, 0x20, 2).unwrap()]);
    assert_eq!(dma.streams(0x0A00_0200).err(), Some(Undescribed));
    assert_eq!(dma.streams(0x0A00_0400).unwrap().count(), 0);
    assert_eq!(dma.streams(0x0B00_0000).unwrap().count(), 0, "no slot");

    let mut child = HwNode::new(
        0x8000_1000,
        tairix_abi::HW_NODE_ROOT_ID,
        tairix_abi::HwDeviceClass::Input,
    );
    for range in translated {
        child
            .push_resource(HwResource::iommu_stream(range))
            .unwrap();
    }
    sink.emit(child).unwrap();
    let _ = FdtUnits::read(&fdt, &mut sink, &NullSink);
    let nodes = sink.into_vec();
    let child = nodes.iter().find(|node| node.id() == 0x8000_1000).unwrap();
    let group = group_of(child);
    assert!(group.is_some());
    assert_eq!(group, group_of(by_compatible(&nodes, b"virtio,mmio")));
}

/// Slots that cannot be recorded leave every slot's device undescribed.
#[test]
fn unrecorded_slots_describe_no_device() {
    let dma = SlotDma {
        streams: Vec::new(),
        undescribed: None,
        coherence: SlotCoherence::Listed(Vec::new()),
    };
    assert_eq!(dma.streams(0x0A00_0400).err(), Some(Undescribed));
    assert_eq!(dma.coherence(0x0A00_0400), None, "nor states its coherence");
}

/// A virtio-iommu in a slot raises its faults on the slot's one line.
#[test]
fn a_slot_unit_raises_its_faults_on_its_line() {
    let blob = tree(|b| {
        b.prop_u32("interrupt-parent", 1);
        b.begin_node("plic@c000000");
        b.prop_str("compatible", "riscv,plic0");
        b.prop("interrupt-controller", &[]);
        b.prop_u32("#interrupt-cells", 1);
        b.prop_u32("phandle", 1);
        b.end_node();
        b.begin_node("virtio_mmio@10008000");
        b.prop_str("compatible", "virtio,mmio");
        b.prop("reg", &cells(&[0, 0x1000_8000, 0, 0x1000]));
        b.prop("interrupts", &cells(&[8]));
        b.prop_u32("#iommu-cells", 1);
        b.prop_u32("phandle", SMMU);
        b.end_node();
    });
    let fdt = Fdt::new(&blob).unwrap();
    let mut sink = CollectingHwNodeSink::new();
    FdtDiscovery::<Plic>::new(Fdt::new(&blob).unwrap())
        .discover(&mut sink)
        .unwrap();
    let _ = FdtUnits::read(&fdt, &mut sink, &NullSink);
    let nodes = sink.into_vec();
    let unit = by_compatible(&nodes, b"virtio,mmio");
    assert_eq!(
        tairix_kernel_core::iommu::WiredFaults::of(unit),
        Some(tairix_kernel_core::iommu::WiredFaults {
            line: 8,
            trigger: tairix_kernel_irq::Trigger::Level,
            place: tairix_kernel_iommu_virtio::FAULT_INTERRUPT,
        })
    );
}

/// Records the message of every event.
#[derive(Default)]
struct Messages(RefCell<Vec<alloc::string::String>>);

impl Sink for Messages {
    fn write_event(&self, event: &Event<'_>) {
        self.0.borrow_mut().push(event.message.into());
    }
}

/// A unit's node with no room left for its fault line says so, rather than
/// leave its faults raising nothing unremarked.
#[test]
fn a_fault_line_a_unit_s_node_cannot_hold_is_reported() {
    let log = Messages::default();
    let mut room = HwNode::new(3, 0, tairix_abi::HwDeviceClass::Iommu);
    record_fault_place(&mut room, 1, &log);
    assert_eq!(room.resources().len(), 1);
    assert!(log.0.borrow().is_empty());
    let mut full = HwNode::new(4, 0, tairix_abi::HwDeviceClass::Iommu);
    while full
        .push_resource(HwResource::property(HwProperty::FaultInterrupt, 0))
        .is_ok()
    {}
    record_fault_place(&mut full, 1, &log);
    assert_eq!(
        *log.0.borrow(),
        ["a translation unit's fault line unrecorded; its faults raise nothing"]
    );
}
