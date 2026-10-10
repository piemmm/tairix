use alloc::vec::Vec;

use super::*;
use crate::fixture::ecam_host_arm as arm_virt;
use crate::write::FdtWriter;

fn cells(values: &[u32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_be_bytes()).collect()
}

fn hosts(blob: &[u8]) -> Vec<PciHost<'_>> {
    let fdt = Fdt::new(blob).unwrap();
    let mut found = Vec::new();
    each_pci_host(&fdt, |host| found.push(host));
    found
}

/// `/chosen` says whether firmware's layout is kept, for every host; a host
/// node saying so is no binding anything reads.
#[test]
fn probe_only_is_chosen_s_word_for_every_host() {
    let tree = |chosen: Option<u32>, on_host: bool| {
        let mut b = FdtWriter::new();
        b.begin_node("");
        b.prop_u32("#address-cells", 2);
        b.prop_u32("#size-cells", 2);
        if let Some(keep) = chosen {
            b.begin_node("chosen");
            b.prop_u32("linux,pci-probe-only", keep);
            b.end_node();
        }
        b.begin_node("pcie@10000000");
        b.prop_str("compatible", "pci-host-ecam-generic");
        b.prop_u32("#address-cells", 3);
        b.prop_u32("#size-cells", 2);
        b.prop("reg", &cells(&[0, 0x1000_0000, 0, 0x1000_0000]));
        b.prop(
            "ranges",
            &cells(&[0x0200_0000, 0, 0x2000_0000, 0, 0x2000_0000, 0, 0x1000_0000]),
        );
        if on_host {
            b.prop_u32("linux,pci-probe-only", 1);
        }
        b.end_node();
        b.end_node();
        b.build()
    };
    for (chosen, on_host, kept) in [
        (Some(1), false, true),
        (Some(0), false, false),
        (None, true, false),
        (None, false, false),
    ] {
        let blob = tree(chosen, on_host);
        let [host] = &hosts(&blob)[..] else {
            panic!("one host");
        };
        assert_eq!(host.probe_only, kept, "{chosen:?}, on the host: {on_host}");
    }
}

#[test]
fn a_host_names_its_region_buses_segment_and_windows() {
    let blob = arm_virt(false);
    let [host] = &hosts(&blob)[..] else {
        panic!("one host");
    };
    assert_eq!(host.ecam, (0x40_1000_0000, 0x1000_0000));
    assert_eq!(host.buses, (0, 0xFF));
    assert_eq!(host.segment, 2);
    assert!(!host.probe_only);
    let windows: Vec<PciWindow> = host.windows().collect();
    assert_eq!(
        windows,
        [
            PciWindow {
                space: PciSpace::Io,
                prefetchable: false,
                pci: 0,
                cpu: 0x3EFF_0000,
                size: 0x1_0000,
            },
            PciWindow {
                space: PciSpace::Memory32,
                prefetchable: false,
                pci: 0x1000_0000,
                cpu: 0x1000_0000,
                size: 0x2EFF_0000,
            },
            PciWindow {
                space: PciSpace::Memory64,
                prefetchable: false,
                pci: 0x80_0000_0000,
                cpu: 0x80_0000_0000,
                size: 0x80_0000_0000,
            },
        ]
    );
}

#[test]
fn an_intx_pin_is_swizzled_through_the_map_to_its_parent() {
    let blob = arm_virt(false);
    let fdt = Fdt::new(&blob).unwrap();
    let host = hosts(&blob)[0];
    // SPI 3 + (slot + pin - 1) % 4, the slot masked to its low two bits.
    for (slot, pin, spi) in [(0, 1, 3), (1, 1, 4), (3, 2, 3), (6, 4, 4)] {
        let spec = host.intx(&fdt, slot, pin).expect("mapped");
        assert_eq!(spec.parent, 0x8002);
        assert_eq!(spec.cells(), [0, spi, 4], "slot {slot} pin {pin}");
    }
    assert_eq!(host.intx(&fdt, 0, 5), None, "no fifth pin");
}

#[test]
fn a_port_marked_external_facing_is_recorded() {
    let blob = arm_virt(true);
    let fdt = Fdt::new(&blob).unwrap();
    let host = hosts(&blob)[0];
    assert!(host.external_facing(&fdt, 0x0800));
    assert!(!host.external_facing(&fdt, 0x1000));
}

/// The fixture's host with `mask`, `map` and `ports` external-facing ports
/// at devices 1 and up.
fn shaped(mask: &[u32], map: &[u32], ports: u32) -> Vec<u8> {
    let mut b = FdtWriter::new();
    b.begin_node("");
    b.prop_u32("#address-cells", 2);
    b.prop_u32("#size-cells", 2);
    b.begin_node("intc@8000000");
    b.prop_u32("phandle", 0x8002);
    b.prop_u32("#address-cells", 2);
    b.prop_u32("#interrupt-cells", 3);
    b.end_node();
    b.begin_node("pcie@10000000");
    b.prop_str("compatible", "pci-host-ecam-generic");
    b.prop_u32("#address-cells", 3);
    b.prop_u32("#size-cells", 2);
    b.prop_u32("#interrupt-cells", 1);
    b.prop("reg", &cells(&[0x40, 0x1000_0000, 0, 0x1000_0000]));
    b.prop(
        "ranges",
        &cells(&[0x0200_0000, 0, 0x1000_0000, 0, 0x1000_0000, 0, 0x2EFF_0000]),
    );
    b.prop("interrupt-map-mask", &cells(mask));
    b.prop("interrupt-map", &cells(map));
    for device in 1..=ports {
        b.begin_node(&alloc::format!("pcie@{device},0"));
        b.prop("reg", &cells(&[device << 11, 0, 0, 0, 0]));
        b.prop("external-facing", &[]);
        b.end_node();
    }
    b.end_node();
    b.end_node();
    b.build()
}

/// Slot 0's four pins, each to SPI 3 + pin.
fn slot_zero() -> Vec<u32> {
    (0..4u32)
        .flat_map(|pin| [0, 0, 0, pin + 1, 0x8002, 0, 0, 0, 3 + pin, 4])
        .collect()
}

#[test]
fn a_map_that_does_not_frame_to_its_end_answers_nothing() {
    let whole = shaped(&[0x1800, 0, 0, 7], &slot_zero(), 0);
    let fdt = Fdt::new(&whole).unwrap();
    assert!(hosts(&whole)[0].intx(&fdt, 0, 1).is_some());
    let mut cut = slot_zero();
    cut.extend_from_slice(&[0x0800, 0, 0, 1, 0x8002]);
    let cut = shaped(&[0x1800, 0, 0, 7], &cut, 0);
    let fdt = Fdt::new(&cut).unwrap();
    assert_eq!(
        hosts(&cut)[0].intx(&fdt, 0, 1),
        None,
        "nothing a half-read map says is taken"
    );
}

#[test]
fn a_mask_short_of_every_child_cell_names_no_map() {
    let blob = shaped(&[0x1800, 0, 0], &slot_zero(), 0);
    let fdt = Fdt::new(&blob).unwrap();
    assert_eq!(hosts(&blob)[0].intx(&fdt, 0, 1), None);
}

#[test]
fn every_external_facing_port_a_host_marks_is_known() {
    let blob = shaped(&[0x1800, 0, 0, 7], &slot_zero(), 9);
    let fdt = Fdt::new(&blob).unwrap();
    let [host] = &hosts(&blob)[..] else {
        panic!("one host, however many ports it marks");
    };
    for device in 1..=9u32 {
        assert!(host.external_facing(&fdt, device << 11), "device {device}");
    }
    assert!(!host.external_facing(&fdt, 10 << 11));
}

/// QEMU riscv64 `virt`'s host under `/soc`, its INTx on a PLIC taking one
/// cell and no address.
#[test]
fn a_plic_parent_takes_one_cell_and_no_address() {
    let mut b = FdtWriter::new();
    b.begin_node("");
    b.prop_u32("#address-cells", 2);
    b.prop_u32("#size-cells", 2);
    b.begin_node("soc");
    b.prop_u32("#address-cells", 2);
    b.prop_u32("#size-cells", 2);
    b.prop("ranges", &[]);
    b.begin_node("plic@c000000");
    b.prop_u32("phandle", 9);
    b.prop_u32("#address-cells", 0);
    b.prop_u32("#interrupt-cells", 1);
    b.end_node();
    b.begin_node("pci@30000000");
    b.prop_str("compatible", "pci-host-ecam-generic");
    b.prop_u32("#address-cells", 3);
    b.prop_u32("#size-cells", 2);
    b.prop_u32("#interrupt-cells", 1);
    b.prop("reg", &cells(&[0, 0x3000_0000, 0, 0x1000_0000]));
    b.prop(
        "ranges",
        &cells(&[0x0200_0000, 0, 0x4000_0000, 0, 0x4000_0000, 0, 0x4000_0000]),
    );
    b.prop("interrupt-map-mask", &cells(&[0x1800, 0, 0, 7]));
    let mut map = Vec::new();
    for slot in 0..4u32 {
        for pin in 0..4u32 {
            map.extend_from_slice(&[slot << 11, 0, 0, pin + 1, 9, 32 + (slot + pin) % 4]);
        }
    }
    b.prop("interrupt-map", &cells(&map));
    b.end_node();
    b.end_node();
    b.end_node();
    let blob = b.build();
    let fdt = Fdt::new(&blob).unwrap();
    let host = hosts(&blob)[0];
    assert_eq!(host.segment, 0, "no domain named");
    let spec = host.intx(&fdt, 2, 3).unwrap();
    assert_eq!((spec.parent, spec.cells()), (9, &[32][..]), "(2 + 2) % 4");
}

#[test]
fn a_malformed_host_is_skipped_whole() {
    let mut b = FdtWriter::new();
    b.begin_node("");
    b.prop_u32("#address-cells", 2);
    b.prop_u32("#size-cells", 2);
    b.begin_node("pcie@0");
    b.prop_str("compatible", "pci-host-ecam-generic");
    b.prop_u32("#address-cells", 3);
    b.prop_u32("#size-cells", 2);
    b.prop("reg", &cells(&[0, 0x3000_0000, 0, 0x1000_0000]));
    b.prop("bus-range", &cells(&[4, 2]));
    b.prop("ranges", &[]);
    b.end_node();
    b.end_node();
    assert!(
        hosts(&b.build()).is_empty(),
        "a bus range ending before it starts"
    );
}

/// A minimal host under `parent_status`'s bus, carrying `iommu_map` where
/// given.
fn host_on_bus(parent_status: Option<&str>, iommu_map: Option<&[u32]>) -> Vec<u8> {
    let mut b = FdtWriter::new();
    b.begin_node("");
    b.prop_u32("#address-cells", 2);
    b.prop_u32("#size-cells", 2);
    b.begin_node("soc");
    b.prop_u32("#address-cells", 2);
    b.prop_u32("#size-cells", 2);
    b.prop("ranges", &[]);
    if let Some(status) = parent_status {
        b.prop_str("status", status);
    }
    b.begin_node("pci@30000000");
    b.prop_str("compatible", "pci-host-ecam-generic");
    b.prop_u32("#address-cells", 3);
    b.prop_u32("#size-cells", 2);
    b.prop("reg", &cells(&[0, 0x3000_0000, 0, 0x1000_0000]));
    b.prop(
        "ranges",
        &cells(&[0x0200_0000, 0, 0x4000_0000, 0, 0x4000_0000, 0, 0x4000_0000]),
    );
    if let Some(map) = iommu_map {
        b.prop("iommu-map", &cells(map));
    }
    b.end_node();
    b.end_node();
    b.end_node();
    b.build()
}

#[test]
fn a_host_names_the_unit_and_stream_each_requester_id_reaches() {
    let blob = host_on_bus(None, Some(&[0, 0x8000, 0, 8, 9, 0x8000, 9, 0xFFF7]));
    let host = hosts(&blob)[0];
    let map = host.iommu_map().expect("decodes").expect("present");
    assert_eq!(map.map(0x7), Some((0x8000, 0x7)));
    assert_eq!(
        map.map(0x8),
        None,
        "the unit's own function stays untranslated"
    );
    assert_eq!(map.map(0x9), Some((0x8000, 0x9)));
    let plain = host_on_bus(None, None);
    assert!(hosts(&plain)[0].iommu_map().expect("no map").is_none());
}

#[test]
fn a_host_whose_map_does_not_decode_says_so_rather_than_naming_none() {
    let blob = host_on_bus(None, Some(&[0, 0x8000, 0]));
    let [host] = &hosts(&blob)[..] else {
        panic!("one host");
    };
    assert_eq!(host.iommu_map().map(|_| ()), Err(FdtError::BadProperty));
}

/// A minimal host carrying `msi_map` and `msi_parent` where given.
fn host_with_msi(msi_map: Option<&[u32]>, msi_parent: Option<&[u32]>) -> Vec<u8> {
    let mut b = FdtWriter::new();
    b.begin_node("");
    b.prop_u32("#address-cells", 2);
    b.prop_u32("#size-cells", 2);
    b.begin_node("pci@30000000");
    b.prop_str("compatible", "pci-host-ecam-generic");
    b.prop_u32("#address-cells", 3);
    b.prop_u32("#size-cells", 2);
    b.prop("reg", &cells(&[0, 0x3000_0000, 0, 0x1000_0000]));
    b.prop(
        "ranges",
        &cells(&[0x0200_0000, 0, 0x4000_0000, 0, 0x4000_0000, 0, 0x4000_0000]),
    );
    if let Some(map) = msi_map {
        b.prop("msi-map", &cells(map));
    }
    if let Some(parent) = msi_parent {
        b.prop("msi-parent", &cells(parent));
    }
    b.end_node();
    b.end_node();
    b.build()
}

#[test]
fn a_requester_s_messages_reach_the_controller_its_msi_map_names_as_the_id_it_maps() {
    let blob = host_with_msi(Some(&[0, 0x8003, 0x100, 0x10]), Some(&[0x8009]));
    let host = hosts(&blob)[0];
    assert_eq!(host.msi_target(0x3), Ok(Some((0x8003, 0x103))));
    assert_eq!(
        host.msi_target(0x10),
        Ok(None),
        "an id the map leaves out raises no message, msi-parent or not"
    );
}

#[test]
fn without_a_map_messages_reach_msi_parent_as_the_requester_id() {
    let blob = host_with_msi(None, Some(&[0x8003]));
    assert_eq!(
        hosts(&blob)[0].msi_target(0x0108),
        Ok(Some((0x8003, 0x0108)))
    );
    let neither = host_with_msi(None, None);
    assert_eq!(hosts(&neither)[0].msi_target(0x0108), Ok(None));
}

#[test]
fn a_map_or_parent_that_does_not_decode_names_no_controller() {
    let short = host_with_msi(Some(&[0, 0x8003, 0]), None);
    assert_eq!(hosts(&short)[0].msi_target(0), Err(FdtError::BadProperty));
    let nameless = host_with_msi(None, Some(&[0]));
    assert_eq!(
        hosts(&nameless)[0].msi_target(0),
        Err(FdtError::BadProperty)
    );
}

/// A unit that is a function on the root bus is found with its requester
/// id, from the bus the host's range starts at; one below a bridge, one that
/// is disabled, and a child that is no unit are not.
#[test]
fn a_unit_that_is_a_root_bus_function_is_found_by_its_requester_id() {
    let mut b = FdtWriter::new();
    b.begin_node("");
    b.prop_u32("#address-cells", 2);
    b.prop_u32("#size-cells", 2);
    b.begin_node("pcie@10000000");
    b.prop_str("compatible", "pci-host-ecam-generic");
    b.prop_u32("#address-cells", 3);
    b.prop_u32("#size-cells", 2);
    b.prop("reg", &cells(&[0, 0x1000_0000, 0, 0x1000_0000]));
    b.prop("bus-range", &cells(&[0x10, 0x1F]));
    b.prop(
        "ranges",
        &cells(&[0x0200_0000, 0, 0x2000_0000, 0, 0x2000_0000, 0, 0x1000_0000]),
    );
    let function = |b: &mut FdtWriter, name: &str, devfn: u32, unit: bool, status: Option<&str>| {
        b.begin_node(name);
        b.prop("reg", &cells(&[devfn << 8, 0, 0, 0, 0]));
        if unit {
            b.prop_u32("#iommu-cells", 1);
        }
        if let Some(status) = status {
            b.prop_str("status", status);
        }
    };
    function(&mut b, "virtio_iommu@2,0", 0x10, true, None);
    b.end_node();
    function(&mut b, "ethernet@3,0", 0x18, false, None);
    b.end_node();
    function(&mut b, "iommu@4,0", 0x20, true, Some("disabled"));
    b.end_node();
    function(&mut b, "pcie@5,0", 0x28, false, None);
    function(&mut b, "iommu@0,0", 0x1_0000, true, None);
    b.end_node();
    b.end_node();
    b.end_node();
    b.end_node();
    let blob = b.build();
    let fdt = Fdt::new(&blob).unwrap();
    let host = hosts(&blob)[0];
    let mut found = Vec::new();
    host.units(&fdt, &mut |requester, node| {
        found.push((requester, node.name()));
    });
    assert_eq!(found, [(0x1010, &b"virtio_iommu@2,0"[..])]);
}

#[test]
fn a_host_under_a_disabled_bus_is_not_one() {
    assert!(hosts(&host_on_bus(Some("disabled"), None)).is_empty());
    assert_eq!(hosts(&host_on_bus(Some("okay"), None)).len(), 1);
}
