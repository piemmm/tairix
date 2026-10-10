//! Broadcom legacy DMA engine driver (`brcm,bcm2835-dma`).
//!
//! A control block holds bus addresses and nothing between the engines and
//! RAM checks them, so this driver is the only process that maps the
//! controller's registers or writes a control block. It serves the node's
//! `dmaengine-v1` endpoint: a consumer driver quotes the request line and the
//! FIFO the kernel attests it holds, and receives a buffer this driver carved,
//! never an address of its own choosing.
//!
//! [`engine`] is the hardware: the register sequences and the cyclic
//! control-block chains, behind the [`DmaEngine`] and [`DmaChannel`] class
//! traits. [`controller`] is the endpoint written over those traits, which
//! holds no knowledge of this part. The `Run` binary wires both to the kernel.
//!
//! Reference: BCM2711 ARM Peripherals, chapter 4 (DMA Controller).
//!
//! [`DmaEngine`]: tairix_abi::driver::dmaengine::DmaEngine
//! [`DmaChannel`]: tairix_abi::driver::dmaengine::DmaChannel

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod controller;
pub mod engine;

#[cfg(test)]
mod controller_tests;
#[cfg(test)]
mod engine_tests;
#[cfg(test)]
mod model;

use tairix_abi::driver::dmaengine::DMA_MAX_CHANNELS;
use tairix_abi::{CapabilityId, DriverBindKey, DriverError, DriverHandle, DriverHost, HwMatchKey};

/// The channels every per-channel table in the driver holds.
const CHANNEL_SLOTS: usize = DMA_MAX_CHANNELS as usize;

/// The capabilities the driver runs with, which its signed manifest requests:
/// the register window, the channels' interrupt lines, the controller's
/// endpoint, the buffers and chains it carves and hands on, and the log its
/// decisions go to.
pub const REQUIRED_CAPABILITIES: &[CapabilityId] = &[
    CapabilityId::MMIO_MAP,
    CapabilityId::IRQ_BIND,
    CapabilityId::IPC_BIND_PRIVILEGED,
    CapabilityId::MEM_DMA,
    CapabilityId::SHM,
    CapabilityId::LOG_EMIT,
];

/// Device-tree `compatible` string of the legacy engines.
pub const DMA_COMPATIBLE: &[u8] = b"brcm,bcm2835-dma";

/// The bind priority [`BIND_KEYS`] carries: an exact `compatible` match.
const BIND_PRIORITY: u16 = 10;

/// The driver's canonical bind table — the single source both the installed
/// bundle's signed manifest and the autoload match are built from.
pub const BIND_KEYS: &[DriverBindKey] = &[DriverBindKey::new(
    BIND_PRIORITY,
    match HwMatchKey::compatible(DMA_COMPATIBLE) {
        Ok(key) => key,
        // A literal too long for the key would fail const evaluation here,
        // never at run time.
        Err(_) => panic!("compatible string fits HW_COMPATIBLE_MAX"),
    },
)];

/// Handle marker [`register`] returns; the host re-issues its own. `"DMA1"`.
const REGISTER_HANDLE_MARKER: u64 = 0x444D_4131_0000_0001;

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
/// DMA `LinkDuty`.
pub fn register(host: &dyn DriverHost) -> Result<DriverHandle, DriverError> {
    if !host.has_capability(CapabilityId::DRV_LOAD) {
        return Err(DriverError::PermissionDenied);
    }
    DriverHandle::from_raw(REGISTER_HANDLE_MARKER)
}
