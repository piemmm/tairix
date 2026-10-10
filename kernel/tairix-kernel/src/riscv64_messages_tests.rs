extern crate std;

use tairix_fdt::pci::each_pci_host;
use tairix_fdt::write::FdtWriter;

use super::*;

const IMSIC: u32 = 4;
const OTHER_CONTROLLER: u32 = 5;
const UNIT: u32 = 0x8000;
const OTHER_UNIT: u32 = 0x8001;
const PAGE: u64 = 0x2800_0000;

fn file() -> ImsicFile {
    ImsicFile {
        phandle: IMSIC,
        page: PAGE,
        ids: 255,
        hart_index: 0,
    }
}

/// A host whose messages go to `controller` and whose requester ids reach
/// `unit` as streams of the same number.
fn host_tree(controller: u32, unit: u32) -> std::vec::Vec<u8> {
    let cells = |values: &[u32]| -> std::vec::Vec<u8> {
        values
            .iter()
            .flat_map(|value| value.to_be_bytes())
            .collect()
    };
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
    b.prop_u32("msi-parent", controller);
    b.prop("iommu-map", &cells(&[0, unit, 0, 0x1_0000]));
    b.end_node();
    b.end_node();
    b.build()
}

/// Offer `requester` of `node` a route and take it.
fn take(
    planner: &mut MrifPlanner,
    fdt: &Fdt<'_>,
    node: u32,
    requester: u16,
) -> Option<MessageRoute> {
    let mut route = None;
    each_pci_host(fdt, |host| {
        route = planner.offer(fdt, &host, node, requester);
    });
    if route.is_some() {
        planner.accept();
    }
    route
}

#[test]
fn each_confined_function_writes_its_vector_to_the_harts_page_on_a_line_of_its_own() {
    let blob = host_tree(IMSIC, UNIT);
    let fdt = Fdt::new(&blob).unwrap();
    let mut planner = MrifPlanner::new(file(), std::vec![UNIT], 8);
    let first = take(&mut planner, &fdt, 21, 0x0008).expect("routed");
    let second = take(&mut planner, &fdt, 22, 0x0010).expect("routed");
    assert_eq!(
        first.message,
        MsiMessage {
            address: PAGE,
            data: VECTOR
        }
    );
    assert_eq!(
        (first.line, second.line),
        (MESSAGE_LINE_BASE, MESSAGE_LINE_BASE + 1)
    );
    assert_eq!(first.doorbell.doorbell_window(), Ok(PAGE..PAGE + 0x1000));
    assert_eq!(planner.into_nodes(), [21, 22]);
}

#[test]
fn a_function_no_confining_unit_takes_keeps_its_wire() {
    for (controller, unit) in [(OTHER_CONTROLLER, UNIT), (IMSIC, OTHER_UNIT)] {
        let blob = host_tree(controller, unit);
        let fdt = Fdt::new(&blob).unwrap();
        let mut planner = MrifPlanner::new(file(), std::vec![UNIT], 8);
        assert!(
            take(&mut planner, &fdt, 21, 0x0008).is_none(),
            "{controller:#x} {unit:#x}"
        );
        assert!(planner.into_nodes().is_empty());
    }
}

#[test]
fn routes_stop_at_the_identities_left_and_a_refused_one_is_offered_again() {
    let blob = host_tree(IMSIC, UNIT);
    let fdt = Fdt::new(&blob).unwrap();
    let mut planner = MrifPlanner::new(file(), std::vec![UNIT], 1);
    each_pci_host(&fdt, |host| {
        let refused = planner.offer(&fdt, &host, 21, 0x0008).expect("offered");
        let again = planner.offer(&fdt, &host, 21, 0x0008).expect("offered");
        assert_eq!(refused, again, "nothing was recorded for the refusal");
    });
    assert!(take(&mut planner, &fdt, 21, 0x0008).is_some());
    assert!(
        take(&mut planner, &fdt, 22, 0x0010).is_none(),
        "no identity left"
    );
}
