//! Broadcom second-level interrupt controller driver (`brcm,l2-intc` and its
//! family).
//!
//! The controller aggregates up to thirty-two device interrupts onto one line
//! of the root controller, so the devices behind it — the BCM2711's HDMI
//! hotplug and CEC lines among them — reach their drivers only through it. It
//! serves the node's `irqline-v1` endpoint: a consumer waits on the line its
//! link names, and is answered when the line fires.
//!
//! [`controller`] is the line server over the controller's registers; the
//! `Run` binary maps the window, binds the controller's own line and serves.
//!
//! Reference: the Linux `irq-brcmstb-l2` driver's register layouts.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod controller;

#[cfg(test)]
mod controller_tests;

use controller::Layout;
use tairix_abi::{CapabilityId, DriverBindKey, DriverError, DriverHandle, DriverHost, HwMatchKey};

/// The capabilities the driver runs with, which its signed manifest requests:
/// the register window, its own line on the root controller, the node's
/// endpoint, and the log its decisions go to.
pub const REQUIRED_CAPABILITIES: &[CapabilityId] = &[
    CapabilityId::MMIO_MAP,
    CapabilityId::IRQ_BIND,
    CapabilityId::IPC_BIND_PRIVILEGED,
    CapabilityId::LOG_EMIT,
];

/// The bind priority [`BIND_KEYS`] carries: an exact `compatible` match.
const BIND_PRIORITY: u16 = 10;

/// The driver's canonical bind table — the single source both the installed
/// bundle's signed manifest and the autoload match are built from — each key
/// with the register layout its controller keeps, in [`LAYOUTS`] order.
pub const BIND_KEYS: &[DriverBindKey] = &[
    DriverBindKey::new(BIND_PRIORITY, compatible_key(b"brcm,bcm2711-l2-intc")),
    DriverBindKey::new(BIND_PRIORITY, compatible_key(b"brcm,l2-intc")),
    DriverBindKey::new(BIND_PRIORITY, compatible_key(b"brcm,hif-spi-l2-intc")),
    DriverBindKey::new(BIND_PRIORITY, compatible_key(b"brcm,upg-aux-aon-l2-intc")),
    DriverBindKey::new(BIND_PRIORITY, compatible_key(b"brcm,bcm7271-l2-intc")),
];

/// The layout each entry of [`BIND_KEYS`] names.
pub const LAYOUTS: [Layout; 5] = [
    Layout::EDGE,
    Layout::EDGE,
    Layout::EDGE,
    Layout::EDGE,
    Layout::LEVEL,
];

const _: () = assert!(LAYOUTS.len() == BIND_KEYS.len());

const fn compatible_key(compatible: &[u8]) -> HwMatchKey {
    match HwMatchKey::compatible(compatible) {
        Ok(key) => key,
        // A literal too long for the key would fail const evaluation here,
        // never at run time.
        Err(_) => panic!("compatible string fits HW_COMPATIBLE_MAX"),
    }
}

/// Handle marker [`register`] returns; the host re-issues its own. `"L2IC"`.
const REGISTER_HANDLE_MARKER: u64 = 0x4C32_4943_0000_0001;

/// Driver entry point.
///
/// # Errors
///
/// [`DriverError::PermissionDenied`] if the host did not grant
/// [`CapabilityId::DRV_LOAD`].
///
/// # Capabilities
///
/// Requires [`CapabilityId::DRV_LOAD`]. Serving the controller additionally
/// needs the grants its matched node requested and the node's
/// interrupt-line `LinkDuty`.
pub fn register(host: &dyn DriverHost) -> Result<DriverHandle, DriverError> {
    if !host.has_capability(CapabilityId::DRV_LOAD) {
        return Err(DriverError::PermissionDenied);
    }
    DriverHandle::from_raw(REGISTER_HANDLE_MARKER)
}
