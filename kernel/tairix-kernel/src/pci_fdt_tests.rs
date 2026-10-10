extern crate std;

use core::ptr::NonNull;

use tairix_fdt::fixture::ecam_host_arm;
use tairix_fdt::pci::{each_pci_host, InterruptSpec, PciHost};
use tairix_fdt::Fdt;
use tairix_pci::topology::Topology;

use super::*;
use crate::pci_probe::tests::{at, bridge, endpoint};
use tairix_pci::topology::{Function as PciFunction, PortType};

fn host(blob: &[u8]) -> PciHost<'_> {
    let fdt = Fdt::new(blob).unwrap();
    let mut found = None;
    each_pci_host(&fdt, |host| found = Some(host));
    found.unwrap()
}

/// A port decoding the fixture's GIC: SPI `n` is line `n + 32`.
struct Gic;

impl FdtPort for Gic {
    fn dma_convention(&self) -> tairix_abi::DmaCoherence {
        tairix_abi::DmaCoherence::Snooped
    }

    fn registers(&self, _base: u64, _len: usize) -> Option<NonNull<u8>> {
        None
    }

    fn reach(&self) -> u64 {
        1 << 39
    }
}

impl IntxLines for Gic {
    fn line(&self, spec: &InterruptSpec) -> Option<u32> {
        match spec.cells() {
            &[0, spi, _] if spec.parent == 0x8002 => spi.checked_add(32),
            _ => None,
        }
    }
}

/// A port that answers no mapping, remembering whether it was asked.
struct Asked(core::sync::atomic::AtomicBool);

impl FdtPort for Asked {
    fn dma_convention(&self) -> tairix_abi::DmaCoherence {
        tairix_abi::DmaCoherence::Snooped
    }

    fn registers(&self, _base: u64, _len: usize) -> Option<NonNull<u8>> {
        self.0.store(true, core::sync::atomic::Ordering::Relaxed);
        None
    }

    fn reach(&self) -> u64 {
        1 << 39
    }
}

/// A tree placing a host's configuration region over memory would have the
/// probe's sizing writes land in RAM: the region is never mapped.
#[test]
fn a_configuration_region_over_memory_is_never_mapped() {
    let blob = ecam_host_arm(false);
    let host = host(&blob);
    let (base, len) = host.ecam;
    let port = Asked(core::sync::atomic::AtomicBool::new(false));
    let memory = base + len / 2..base + len + 0x1000;
    assert!(segment_of(
        &host,
        &port,
        core::slice::from_ref(&memory),
        &crate::test_support::NullSink
    )
    .is_none());
    assert!(!port.0.load(core::sync::atomic::Ordering::Relaxed));
    let elsewhere = base + len..base + 2 * len;
    let _ = segment_of(
        &host,
        &port,
        core::slice::from_ref(&elsewhere),
        &crate::test_support::NullSink,
    );
    assert!(
        port.0.load(core::sync::atomic::Ordering::Relaxed),
        "memory beside it is no bar"
    );
}

#[test]
fn only_the_windows_the_port_reaches_are_used() {
    let blob = ecam_host_arm(false);
    let host = host(&blob);
    let (low, apertures) = windows_of(&host, 1 << 39, &[]).unwrap();
    assert_eq!(low.io, Some(0..0x1_0000));
    assert_eq!(low.memory, Some(0x1000_0000..0x3EFF_0000));
    assert_eq!(low.wide, None, "the 64-bit window lies past the reach");
    assert_eq!(
        apertures.len(),
        1,
        "no aperture for I/O or an unreached window"
    );
    let (high, apertures) = windows_of(&host, 1 << 41, &[]).unwrap();
    assert_eq!(high.wide, Some(0x80_0000_0000..0x100_0000_0000));
    assert_eq!(apertures.len(), 2);
}

/// A window over memory would have assignment place BARs over RAM, which
/// would then decode peer requests meant for it: it is not used.
#[test]
fn a_window_over_memory_is_not_used() {
    let blob = ecam_host_arm(false);
    let host = host(&blob);
    let over = 0x2000_0000..0x2000_1000;
    let (windows, apertures) = windows_of(&host, 1 << 39, core::slice::from_ref(&over)).unwrap();
    assert_eq!(windows.memory, None);
    assert!(apertures.is_empty());
    let beside = 0x4000_0000..0x4000_1000;
    let (windows, _) = windows_of(&host, 1 << 39, core::slice::from_ref(&beside)).unwrap();
    assert_eq!(
        windows.memory,
        Some(0x1000_0000..0x3EFF_0000),
        "memory beside it"
    );
}

/// Two memory windows decoding one address would have assignment place two
/// BARs on it: the host is refused.
#[test]
fn a_host_whose_windows_overlap_is_refused() {
    let tree = |second: u32| {
        let mut b = tairix_fdt::write::FdtWriter::new();
        b.begin_node("");
        b.prop_u32("#address-cells", 2);
        b.prop_u32("#size-cells", 2);
        b.begin_node("pcie@10000000");
        b.prop_str("compatible", "pci-host-ecam-generic");
        b.prop_u32("#address-cells", 3);
        b.prop_u32("#size-cells", 2);
        let cells: std::vec::Vec<u8> = [0, 0x1000_0000, 0, 0x1000_0000]
            .iter()
            .flat_map(|v: &u32| v.to_be_bytes())
            .collect();
        b.prop("reg", &cells);
        let ranges: std::vec::Vec<u8> = [
            0x0200_0000,
            0,
            0x2000_0000,
            0,
            0x2000_0000,
            0,
            0x1000_0000, // 32-bit
            0x4300_0000,
            0,
            second,
            0,
            second,
            0,
            0x1000_0000, // 64-bit
        ]
        .iter()
        .flat_map(|v: &u32| v.to_be_bytes())
        .collect();
        b.prop("ranges", &ranges);
        b.end_node();
        b.end_node();
        b.build()
    };
    let overlapping = tree(0x2800_0000);
    assert!(windows_of(&host(&overlapping), 1 << 39, &[]).is_none());
    let apart = tree(0x3000_0000);
    assert!(windows_of(&host(&apart), 1 << 39, &[]).is_some());
}

/// A function's INTx reaches the controller through the root port's swizzle
/// and the host's map.
#[test]
fn a_function_s_intx_is_resolved_through_its_bridges_and_the_map() {
    let blob = ecam_host_arm(false);
    let fdt = Fdt::new(&blob).unwrap();
    let host = host(&blob);
    let topology = Topology::new(std::vec![
        bridge(0, 1, 1, 1),
        endpoint(1, 0, 0, 0x1AF4, 0x09_80_00),
        endpoint(0, 2, 0, 0x1AF4, 0x09_80_00),
    ])
    .unwrap();
    // Behind the port in slot 1: pin 1 swizzles to pin 1 of slot 1, SPI 4.
    assert_eq!(
        intx_line(&fdt, &Gic, &host, &topology, at(1, 0, 0), 1),
        Some(36)
    );
    // On the root bus in slot 2, pin 2: SPI 3 + (2 + 1) % 4.
    assert_eq!(
        intx_line(&fdt, &Gic, &host, &topology, at(0, 2, 0), 2),
        Some(38)
    );
    assert_eq!(
        intx_line(&fdt, &Gic, &host, &topology, at(0, 2, 0), 0),
        None,
        "no pin"
    );
    assert_eq!(
        intx_line(&fdt, &Gic, &host, &topology, at(3, 0, 0), 1),
        None,
        "not walked"
    );
}

/// A router offering each function a route of its own, recording which it
/// was told were taken.
#[derive(Default)]
struct Offering {
    offered: u32,
    accepted: std::vec::Vec<u32>,
}

impl MessageRouter for Offering {
    fn offer(
        &mut self,
        _fdt: &Fdt<'_>,
        _host: &PciHost<'_>,
        node: u32,
        requester: u16,
    ) -> Option<MessageRoute> {
        self.offered += 1;
        Some(MessageRoute {
            message: MsiMessage {
                address: 0x0809_0040,
                data: u32::from(requester),
            },
            line: 1052 + node,
            doorbell: HwResource::msi_doorbell(0x0809_0000, 0x1000).unwrap(),
        })
    }

    fn accept(&mut self) {
        self.accepted.push(self.offered);
    }
}

/// A function whose MSI-X takes the route it is offered raises that message
/// and carries its doorbell; one refusing it keeps its pin, and the route is
/// never recorded as given.
#[test]
fn a_function_raises_the_message_it_took_and_otherwise_its_pin() {
    let blob = ecam_host_arm(false);
    let fdt = Fdt::new(&blob).unwrap();
    let host = host(&blob);
    let pin = HwResource::irq(37, 1);
    let mut offering = Offering::default();
    let routed = interrupt_of(
        &mut offering,
        &fdt,
        &host,
        3,
        0x0018,
        |message| message.data == 0x18,
        || panic!("a function raising messages is given no pin"),
    );
    assert_eq!(
        routed,
        Some(DeviceInterrupt {
            line: HwResource::message_irq(1055, MSIX_ENTRY),
            doorbell: Some(HwResource::msi_doorbell(0x0809_0000, 0x1000).unwrap()),
        })
    );
    assert_eq!(offering.accepted, [1]);
    let refused = interrupt_of(
        &mut offering,
        &fdt,
        &host,
        4,
        0x0020,
        |_| false,
        || Some(pin),
    );
    assert_eq!(refused, Some(DeviceInterrupt::line(pin)));
    assert_eq!(
        offering.accepted,
        [1],
        "a route its function refused is not recorded"
    );
    let unrouted = interrupt_of(
        &mut NoMessages,
        &fdt,
        &host,
        4,
        0x0020,
        |_| panic!("no route was offered"),
        || Some(pin),
    );
    assert_eq!(unrouted, Some(DeviceInterrupt::line(pin)));
}

/// A function's messages reach the fabric as its own requester id, or, below
/// a bridge to conventional PCI, as that bridge's secondary bus at function
/// zero.
#[test]
fn messages_from_behind_a_bridge_to_conventional_pci_carry_its_secondary_bus() {
    let topology = Topology::new(std::vec![
        PciFunction {
            express: Some(PortType::RootPort),
            ..bridge(0, 0x1c, 1, 2)
        },
        PciFunction {
            express: Some(PortType::PcieToPci),
            ..bridge(1, 0, 2, 2)
        },
        endpoint(2, 1, 0, 0x1AF4, 0x02_00_00),
        endpoint(0, 3, 0, 0x1AF4, 0x02_00_00),
    ])
    .unwrap();
    assert_eq!(message_requester(&topology, at(0, 3, 0)), Some(0x0018));
    assert_eq!(message_requester(&topology, at(2, 1, 0)), Some(0x0200));
    assert_eq!(
        message_requester(&topology, at(5, 0, 0)),
        None,
        "not walked"
    );
}
