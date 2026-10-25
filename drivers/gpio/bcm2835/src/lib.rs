//! BCM2835-family GPIO controller driver (`brcm,bcm2711-gpio`).
//!
//! The controller's lines reach two kinds of holder (`plans/GPIO.md` G2,
//! `plans/COOLING.md` CL3): a device the hardware tree links to a line waits
//! on it as an interrupt over the node's `irqline-v1` endpoint, and a system
//! service holding `CAP_GPIO` drives one of the lines the board leads to its
//! header over `gpio-header-v1`.
//!
//! [`block`] is the register model and [`controller`] the server over it; the
//! `Run` binary maps the window, binds the bank interrupts and both endpoints,
//! publishes the header and serves.
//!
//! Reference: BCM2711 ARM Peripherals §5 and §6.2.4, BCM2835 ARM Peripherals
//! §6, and the `brcm,bcm2835-gpio` binding (taken under BSD-2-Clause) for the
//! order of the node's interrupts.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod block;
pub mod controller;

#[cfg(test)]
mod controller_tests;

use tairix_abi::{
    compatible_key, CapabilityId, DriverBindKey, DriverError, DriverHandle, DriverHost,
};

/// The capabilities the driver runs with, which its signed manifest requests:
/// the register window, the bank interrupts, the node's two endpoints, the
/// header's channel node, and the log its decisions go to.
pub const REQUIRED_CAPABILITIES: &[CapabilityId] = &[
    CapabilityId::MMIO_MAP,
    CapabilityId::IRQ_BIND,
    CapabilityId::IPC_BIND_PRIVILEGED,
    CapabilityId::HW_EMIT,
    CapabilityId::LOG_EMIT,
];

/// The bind priority [`BIND_KEYS`] carries: an exact `compatible` match.
const BIND_PRIORITY: u16 = 10;

/// The driver's canonical bind table — the single source both the installed
/// bundle's signed manifest and the autoload match are built from.
pub const BIND_KEYS: &[DriverBindKey] = &[DriverBindKey::new(
    BIND_PRIORITY,
    compatible_key!(b"brcm,bcm2711-gpio"),
)];

/// Handle marker [`register`] returns; the host re-issues its own. `"GPIO"`.
const REGISTER_HANDLE_MARKER: u64 = 0x4750_494F_0000_0001;

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
/// needs the grants its matched node requested and the node's interrupt-line
/// `LinkDuty`.
pub fn register(host: &dyn DriverHost) -> Result<DriverHandle, DriverError> {
    if !host.has_capability(CapabilityId::DRV_LOAD) {
        return Err(DriverError::PermissionDenied);
    }
    DriverHandle::from_raw(REGISTER_HANDLE_MARKER)
}
