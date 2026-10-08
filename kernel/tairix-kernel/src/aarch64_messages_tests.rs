extern crate std;

use tairix_fdt::fixture::DtbBuilder;
use tairix_fdt::pci::each_pci_host;

use super::*;

const ITS: u32 = 0x8003;
const ITS_BASE: u64 = 0x0808_0000;

/// What QEMU's ITS reports: 16 `DeviceID` and 16 `EventID` bits.
const FEATURES: ItsFeatures = ItsFeatures {
    device_bits: 16,
    event_bits: 16,
    itt_entry_bytes: 12,
    physical_targets: false,
    hardware_collections: 0,
    collection_bits: 16,
};

fn service(features: ItsFeatures) -> Service {
    Service {
        phandle: ITS,
        base: ITS_BASE,
        features,
    }
}

/// A host whose requester ids reach `ITS` through `msi_map`, where given.
fn host_tree(msi_map: Option<&[u32]>) -> std::vec::Vec<u8> {
    let cells = |values: &[u32]| -> std::vec::Vec<u8> {
        values
            .iter()
            .flat_map(|value| value.to_be_bytes())
            .collect()
    };
    let mut b = DtbBuilder::new();
    b.begin_node("");
    b.prop_u32("#address-cells", 2);
    b.prop_u32("#size-cells", 2);
    b.begin_node("pcie@10000000");
    b.prop_str("compatible", "pci-host-ecam-generic");
    b.prop_u32("#address-cells", 3);
    b.prop_u32("#size-cells", 2);
    b.prop("reg", &cells(&[0x40, 0x1000_0000, 0, 0x1000_0000]));
    b.prop(
        "ranges",
        &cells(&[0x0200_0000, 0, 0x1000_0000, 0, 0x1000_0000, 0, 0x2EFF_0000]),
    );
    if let Some(map) = msi_map {
        b.prop("msi-map", &cells(map));
    }
    b.end_node();
    b.end_node();
    b.build()
}

/// Offer `requester` a route and take it.
fn take(planner: &mut LpiPlanner, fdt: &Fdt<'_>, requester: u16) -> Option<MessageRoute> {
    let mut route = None;
    each_pci_host(fdt, |host| {
        route = planner.offer(fdt, &host, 0, requester);
    });
    if route.is_some() {
        planner.accept();
    }
    route
}

#[test]
fn each_function_raises_its_own_device_s_first_event_on_a_line_of_its_own() {
    let blob = host_tree(Some(&[0, ITS, 0, 0x1_0000]));
    let fdt = Fdt::new(&blob).unwrap();
    let mut planner = LpiPlanner::new(std::vec![service(FEATURES)], 64);
    let first = take(&mut planner, &fdt, 0x0008).expect("routed");
    let second = take(&mut planner, &fdt, 0x0010).expect("routed");
    assert_eq!(
        first.message,
        MsiMessage {
            address: ITS_BASE + TRANSLATER,
            data: 0,
        }
    );
    assert_eq!(
        (first.line, second.line),
        (LPI_LINE_BASE, LPI_LINE_BASE + 1)
    );
    assert_eq!(
        second.message.data, 0,
        "a device of its own starts at event zero"
    );
    assert_eq!(
        first.doorbell.doorbell_window(),
        Ok(ITS_BASE + TRANSLATION_PAGE..ITS_BASE + TRANSLATION_PAGE + 0x1000)
    );
    let routes = planner.into_routes();
    assert_eq!(
        routes.lpis,
        [
            Lpi {
                service: 0,
                route: ItsRoute {
                    device: 0x0008,
                    event: 0,
                    lpi: FIRST_LPI,
                },
            },
            Lpi {
                service: 0,
                route: ItsRoute {
                    device: 0x0010,
                    event: 0,
                    lpi: FIRST_LPI + 1,
                },
            },
        ]
    );
    assert_eq!(routes.line_of(FIRST_LPI + 1), Some(LPI_LINE_BASE + 1));
    assert_eq!(routes.line_of(FIRST_LPI + 2), None, "an LPI no route took");
    assert_eq!(routes.last_line(), Some(LPI_LINE_BASE + 1));
}

/// Functions whose messages reach the service as one `DeviceID` — behind a
/// bridge that tags them with its own — take that device's events in turn.
#[test]
fn functions_sharing_a_device_id_take_its_events_in_turn() {
    let blob = host_tree(Some(&[0, ITS, 0, 0x1_0000]));
    let fdt = Fdt::new(&blob).unwrap();
    let mut planner = LpiPlanner::new(std::vec![service(FEATURES)], 64);
    let events: std::vec::Vec<u32> = (0..3)
        .map(|_| {
            take(&mut planner, &fdt, 0x0200)
                .expect("routed")
                .message
                .data
        })
        .collect();
    assert_eq!(events, [0, 1, 2]);
}

#[test]
fn a_route_its_function_did_not_take_is_offered_again() {
    let blob = host_tree(Some(&[0, ITS, 0, 0x1_0000]));
    let fdt = Fdt::new(&blob).unwrap();
    let mut planner = LpiPlanner::new(std::vec![service(FEATURES)], 64);
    each_pci_host(&fdt, |host| {
        let refused = planner.offer(&fdt, &host, 0, 0x0008).expect("offered");
        let again = planner.offer(&fdt, &host, 0, 0x0008).expect("offered");
        assert_eq!(refused, again, "nothing was recorded for the refusal");
    });
    assert!(planner.into_routes().lpis.is_empty());
}

#[test]
fn a_function_the_service_cannot_name_is_offered_nothing() {
    let none = host_tree(None);
    let fdt = Fdt::new(&none).unwrap();
    let mut planner = LpiPlanner::new(std::vec![service(FEATURES)], 64);
    assert!(take(&mut planner, &fdt, 0x0008).is_none(), "no msi-map");

    let elsewhere = host_tree(Some(&[0, 0x8009, 0, 0x1_0000]));
    let fdt = Fdt::new(&elsewhere).unwrap();
    assert!(
        take(&mut planner, &fdt, 0x0008).is_none(),
        "a controller no service is"
    );

    let unmapped = host_tree(Some(&[0, ITS, 0, 0x10]));
    let fdt = Fdt::new(&unmapped).unwrap();
    assert!(
        take(&mut planner, &fdt, 0x0010).is_none(),
        "an id the map leaves out"
    );

    let narrow = ItsFeatures {
        device_bits: 4,
        event_bits: 1,
        ..FEATURES
    };
    let mapped = host_tree(Some(&[0, ITS, 0, 0x1_0000]));
    let fdt = Fdt::new(&mapped).unwrap();
    let mut planner = LpiPlanner::new(std::vec![service(narrow)], 64);
    assert!(
        take(&mut planner, &fdt, 0x0010).is_none(),
        "a DeviceID past its bits"
    );
    assert!(take(&mut planner, &fdt, 0x0002).is_some());
    assert!(take(&mut planner, &fdt, 0x0002).is_some());
    assert!(
        take(&mut planner, &fdt, 0x0002).is_none(),
        "an EventID past its bits"
    );
}

#[test]
fn no_more_lpis_are_routed_than_the_distributor_names() {
    let blob = host_tree(Some(&[0, ITS, 0, 0x1_0000]));
    let fdt = Fdt::new(&blob).unwrap();
    let mut planner = LpiPlanner::for_distributor(std::vec![service(FEATURES)], 13);
    assert!(
        take(&mut planner, &fdt, 0x0008).is_none(),
        "thirteen bits name no LPI"
    );
    let mut planner = LpiPlanner::new(std::vec![service(FEATURES)], 1);
    assert!(take(&mut planner, &fdt, 0x0008).is_some());
    assert!(take(&mut planner, &fdt, 0x0010).is_none());
}

#[test]
fn lpi_tables_cover_every_route_and_never_fewer_than_fourteen_bits() {
    let lpi = |index| Lpi {
        service: 0,
        route: ItsRoute {
            device: 0,
            event: index,
            lpi: FIRST_LPI + index,
        },
    };
    let mut routes = LpiRoutes::default();
    assert_eq!(routes.id_bits(), 14);
    assert_eq!(routes.last_line(), None);
    routes.lpis = (0..8192).map(lpi).collect();
    assert_eq!(routes.id_bits(), 14, "the last of fourteen bits' LPIs");
    routes.lpis.push(lpi(8192));
    assert_eq!(routes.id_bits(), 15);
}
