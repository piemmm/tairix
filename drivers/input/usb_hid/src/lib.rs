//! TAIRiX USB HID class driver (`plans/HID.md` H5): one per HID interface,
//! every keyboard, mouse, touch pad and touch screen on it served through
//! `lib/hid`.
//!
//! The `lib` target is the driver's identity — [`BIND_KEYS`] and
//! [`REQUIRED_CAPS`], which the image builder authors its signed manifest
//! from — and its host-testable bring-up; `src/main.rs` is the `Run` program.
//! The driver holds no controller register, DMA or interrupt: it reaches its
//! interface through the URB transport the host-controller driver serves, and
//! its control requests reach no other interface.

#![no_std]
#![forbid(unsafe_op_in_unsafe_fn)]
#![deny(missing_docs)]

extern crate alloc;

pub mod bringup;
pub mod interface;
pub mod requests;

use tairix_abi::{CapabilityId, DriverBindKey, HwMatchKey};

/// The interface codes a HID interface states: class `0x03`, sub-class none
/// or boot, protocol none, keyboard or mouse. A protocol code means something
/// only on the boot sub-class, but the report descriptor, not the code, says
/// what the interface is, so every combination binds.
const HID_INTERFACES: [u32; 6] = [
    0x03_00_00, 0x03_00_01, 0x03_00_02, 0x03_01_00, 0x03_01_01, 0x03_01_02,
];

/// A class match, so a vendor-specific HID driver naming an exact device
/// outranks it.
const BIND_PRIORITY: u16 = 5;

/// Every HID interface, any vendor and product.
pub const BIND_KEYS: &[DriverBindKey] = &[
    DriverBindKey::new(BIND_PRIORITY, HwMatchKey::usb(0, 0, HID_INTERFACES[0])),
    DriverBindKey::new(BIND_PRIORITY, HwMatchKey::usb(0, 0, HID_INTERFACES[1])),
    DriverBindKey::new(BIND_PRIORITY, HwMatchKey::usb(0, 0, HID_INTERFACES[2])),
    DriverBindKey::new(BIND_PRIORITY, HwMatchKey::usb(0, 0, HID_INTERFACES[3])),
    DriverBindKey::new(BIND_PRIORITY, HwMatchKey::usb(0, 0, HID_INTERFACES[4])),
    DriverBindKey::new(BIND_PRIORITY, HwMatchKey::usb(0, 0, HID_INTERFACES[5])),
];

/// The capability set the driver runs under, and the one its manifest
/// requests: inject seat records, map its interface's shared buffer, call its
/// interface's URB endpoint, and log.
pub const REQUIRED_CAPS: &[CapabilityId] = &[
    CapabilityId::INPUT_INJECT,
    CapabilityId::SHM,
    CapabilityId::IPC_ENDPOINT,
    CapabilityId::LOG_EMIT,
];

#[cfg(test)]
mod tests {
    use super::{BIND_KEYS, HID_INTERFACES};
    use tairix_abi::HwMatchKey;

    #[test]
    fn every_hid_interface_binds_and_nothing_else_does() {
        for class in HID_INTERFACES {
            let node = HwMatchKey::usb(0x046D, 0xC52B, class);
            assert!(
                BIND_KEYS.iter().any(|bind| bind.key.matches(&node)),
                "{class:06x}"
            );
        }
        for class in [0x08_06_50, 0x03_02_00, 0x09_00_00] {
            let node = HwMatchKey::usb(0x046D, 0xC52B, class);
            assert!(
                !BIND_KEYS.iter().any(|bind| bind.key.matches(&node)),
                "{class:06x}"
            );
        }
    }
}
