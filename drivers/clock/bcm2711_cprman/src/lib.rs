//! BCM2711 clock manager driver (`brcm,bcm2711-cprman`).
//!
//! The clock manager's page sets every clock on the chip, the cores' and the
//! memory's among them, so this driver is the only process that maps it, and
//! it serves only clocks no firmware or kernel component depends on: the
//! audio blocks' PCM and PWM clocks. It serves the node's `clock-v1` endpoint:
//! a consumer quotes the clock link the kernel attests it holds and asks for a
//! rate, which the driver makes from the oscillator or PLLD's peripheral
//! channel, the two sources the firmware leaves fixed. It never reprograms a
//! PLL.
//!
//! [`cprman`] is the hardware: the generators, the PLL channels they divide,
//! and the divisor a rate takes. [`controller`] is the endpoint: who holds each
//! clock, and the rule that a clock two consumers share runs at the rate the
//! first set. The `Run` binary wires both to the kernel.
//!
//! References: BCM2835 ARM Peripherals, section 6.3, for the generators the
//! PCM and PWM clocks share with the general-purpose ones; Linux
//! `drivers/clk/bcm/clk-bcm2835.c` for the PLL registers.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

pub mod controller;
pub mod cprman;

#[cfg(test)]
mod controller_tests;
#[cfg(test)]
mod cprman_tests;
#[cfg(test)]
mod model;

use tairix_abi::{CapabilityId, DriverBindKey, DriverError, DriverHandle, DriverHost, HwMatchKey};

/// The capabilities the driver runs with, which its signed manifest requests:
/// the register window, the controller's endpoint, and the log its decisions
/// go to.
pub const REQUIRED_CAPABILITIES: &[CapabilityId] = &[
    CapabilityId::MMIO_MAP,
    CapabilityId::IPC_BIND_PRIVILEGED,
    CapabilityId::LOG_EMIT,
];

/// Device-tree `compatible` string of the clock manager.
pub const CPRMAN_COMPATIBLE: &[u8] = b"brcm,bcm2711-cprman";

/// The bind priority [`BIND_KEYS`] carries: an exact `compatible` match.
const BIND_PRIORITY: u16 = 10;

/// The driver's canonical bind table — the single source both the installed
/// bundle's signed manifest and the autoload match are built from.
pub const BIND_KEYS: &[DriverBindKey] = &[DriverBindKey::new(
    BIND_PRIORITY,
    match HwMatchKey::compatible(CPRMAN_COMPATIBLE) {
        Ok(key) => key,
        // A literal too long for the key would fail const evaluation here,
        // never at run time.
        Err(_) => panic!("compatible string fits HW_COMPATIBLE_MAX"),
    },
)];

/// Handle marker [`register`] returns; the host re-issues its own. `"CLK1"`.
const REGISTER_HANDLE_MARKER: u64 = 0x434C_4B31_0000_0001;

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
/// needs the register window its matched node requested and the node's clock
/// `LinkDuty`.
pub fn register(host: &dyn DriverHost) -> Result<DriverHandle, DriverError> {
    if !host.has_capability(CapabilityId::DRV_LOAD) {
        return Err(DriverError::PermissionDenied);
    }
    DriverHandle::from_raw(REGISTER_HANDLE_MARKER)
}
