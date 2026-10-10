//! TAIRiX `xHCI` USB host-controller driver (HCD) — shared library.
//!
//! The Pi 4 reaches its USB-A ports through a `VL805` `PCIe` `xHCI`
//! controller (`plans/PI.md` P10). This crate is the loadable
//! **host-controller driver**: a user-space `Run` binary (`src/main.rs`) that
//! binds the discovered `usb,xhci` controller node, owns the one controller,
//! enumerates the attached device, and serves that device's transfers to an
//! autoloaded **class** driver (`drivers/input/usb_kbd`, …) over the
//! bus-agnostic URB transport seam — never touching a class driver, a board,
//! or a bus by name (`plans/USB.md`).
//!
//! This `lib` target holds the HCD's host-testable logic — the controller
//! [`bringup`] orchestration, the controller fault [`domain`], the
//! per-interface URB-[`serve`] state machine, and the table of published
//! [`interfaces`] with its reconcile, serve, and recovery sequencing — so they
//! are proven host-side over mocks; the `Run` binary composes them with the
//! live kernel seams (`shm_create`, `call_create`, `hw_emit_node`, the
//! wait-set event loop). The bus-agnostic `xHCI`
//! *protocol* engine (the [`Xhci`](tairix_usb::Xhci) controller, the TRB/ring
//! vocabulary, and the [`UsbDevice`](tairix_usb::device::UsbDevice)
//! enumeration engine) lives in `lib/usb` so this driver and the class drivers
//! both consume it without depending on each other (`drivers/* → lib/*` only).
//!
//! # Capabilities
//!
//! The `Run` binary requests exactly the resources its matched node carries
//! plus the privilege to publish the interface node and its transport seam:
//! map the register BAR (`CAP_MMIO_MAP`), carve the controller's DMA working
//! set (`CAP_MEM_DMA`), bind the completion interrupt (`CAP_IRQ_BIND`), create
//! the shared URB buffer (`CAP_SHM`), bind the restricted-sender URB endpoint
//! (`CAP_IPC_BIND_PRIVILEGED`), publish the interface node (`CAP_HW_EMIT`), and
//! emit a one-shot bring-up diagnostic (`CAP_LOG_EMIT`). It runs in user space
//! and does not request `CAP_DRV_KERNEL`.

#![no_std]
#![forbid(unsafe_op_in_unsafe_fn)]
#![deny(missing_docs)]

extern crate alloc;

use tairix_abi::driver::pci::CLASS_USB_XHCI;
use tairix_abi::{DriverBindKey, HwMatchKey};
use tairix_usb::XHCI_COMPATIBLE;

pub mod bringup;
pub mod domain;
pub mod interfaces;
pub mod serve;

/// The priority the `compatible` key binds at, mirroring the other
/// `compatible`-keyed drivers (`drivers/bus/pcie_brcm`,
/// `drivers/storage/emmc2`, priority 10).
const BIND_PRIORITY: u16 = 10;

/// The priority the class key binds at: below any driver naming a controller
/// exactly (`drivers/bus/usb/vl805`, priority 20), which brings its function
/// up first and publishes the `usb,xhci` node this driver then binds.
const CLASS_BIND_PRIORITY: u16 = 5;

/// This driver's hardware bind table: the xHCI USB host controller, matched by
/// the [`XHCI_COMPATIBLE`] `compatible` string a bus driver publishes it under
/// (`drivers/bus/usb/vl805`'s emitted node), or by the xHCI class code of a
/// PCI function the kernel discovered on a host it owns.
///
/// The HCD owns the whole controller, so it binds the controller node
/// directly — the `Xhci` controller object cannot cross a process boundary.
/// The class drivers instead bind the per-interface nodes this HCD emits,
/// never the controller node. This `const` is the single source of truth the
/// `drivers/bus/usb/xhci` signed manifest's bind table is authored from and
/// `devmgr` resolves the controller node against.
pub const BIND_KEYS: &[DriverBindKey] = &[
    DriverBindKey::new(
        BIND_PRIORITY,
        match HwMatchKey::compatible(XHCI_COMPATIBLE) {
            Ok(key) => key,
            // Unreachable: `XHCI_COMPATIBLE` is well within
            // `HW_COMPATIBLE_MAX`. A too-long literal would be a compile-time
            // const-eval error here, never a runtime panic.
            Err(_) => panic!("XHCI_COMPATIBLE fits HW_COMPATIBLE_MAX"),
        },
    ),
    DriverBindKey::new(CLASS_BIND_PRIORITY, HwMatchKey::pci(0, 0, CLASS_USB_XHCI)),
];

#[cfg(test)]
mod tests {
    use super::BIND_KEYS;
    use tairix_abi::driver::pci::CLASS_USB_XHCI;
    use tairix_abi::HwMatchKey;

    #[test]
    fn any_vendors_xhci_function_binds_and_no_older_usb_host_does() {
        let binds = |node: HwMatchKey| BIND_KEYS.iter().any(|bind| bind.key.matches(&node));
        assert!(binds(HwMatchKey::pci(0x1B36, 0x000D, CLASS_USB_XHCI)));
        assert!(binds(HwMatchKey::pci(0x8086, 0x31A8, CLASS_USB_XHCI)));
        for older in [0x0C_03_00, 0x0C_03_10, 0x0C_03_20, 0x0C_03_FE] {
            assert!(
                !binds(HwMatchKey::pci(0x1B36, 0x000D, older)),
                "{older:06x}"
            );
        }
        assert!(binds(
            HwMatchKey::compatible(tairix_usb::XHCI_COMPATIBLE).expect("fits")
        ));
    }
}
