//! Broadcom DMA engine driver: the legacy engines (`brcm,bcm2835-dma`) and
//! the BCM2711's DMA4 engines (`brcm,bcm2711-dma`).
//!
//! A control block holds bus addresses and nothing between the engines and
//! RAM checks them, so this driver is the only process that maps the
//! controller's registers or writes a control block. It serves the node's
//! `dmaengine-v1` endpoint: a consumer driver quotes the request line and the
//! FIFO the kernel attests it holds, and receives a buffer this driver carved,
//! never an address of its own choosing.
//!
//! Both engines read one request-cell binding ([`request`]) and lay out one
//! cyclic chain of 32-byte blocks ([`chain`]); [`legacy`] and [`dma4`] are
//! the register sequences and block encodings behind the [`DmaEngine`] and
//! [`DmaChannel`] class traits. The endpoint written over those traits, which
//! holds no knowledge of this part, is `tairix_dmaengine`; the `Run` binary
//! builds the engine its node's matched bind key names and hands it over.
//!
//! [`DmaEngine`]: tairix_abi::driver::dmaengine::DmaEngine
//! [`DmaChannel`]: tairix_abi::driver::dmaengine::DmaChannel

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod chain;
pub mod dma4;
pub mod legacy;
pub mod request;

#[cfg(test)]
mod controller_tests;
#[cfg(test)]
mod dma4_model;
#[cfg(test)]
mod dma4_tests;
#[cfg(test)]
mod legacy_model;
#[cfg(test)]
mod legacy_tests;
#[cfg(test)]
mod model_memory;

use tairix_abi::driver::dmaengine::DMA_MAX_CHANNELS;
use tairix_abi::{
    compatible_key, CapabilityId, DriverBindKey, DriverError, DriverHandle, DriverHost,
};

/// The channels every per-channel table in the driver holds.
const CHANNEL_SLOTS: usize = DMA_MAX_CHANNELS as usize;

/// Bytes between two channels' register blocks, in either engine's window.
pub const CHANNEL_STRIDE: usize = 0x100;

/// The channels a register window of `len` bytes holds.
///
/// # Errors
///
/// [`DriverError::LengthOutOfRange`] for a window that is empty or not whole
/// channel blocks.
fn channel_count(len: usize) -> Result<u8, DriverError> {
    if len == 0 || !len.is_multiple_of(CHANNEL_STRIDE) {
        return Err(DriverError::LengthOutOfRange);
    }
    u8::try_from((len / CHANNEL_STRIDE).min(CHANNEL_SLOTS))
        .map_err(|_| DriverError::LengthOutOfRange)
}

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

/// Device-tree `compatible` string of the BCM2711's DMA4 engines.
pub const DMA4_COMPATIBLE: &[u8] = b"brcm,bcm2711-dma";

/// The bind priority [`BIND_KEYS`] carries: an exact `compatible` match.
const BIND_PRIORITY: u16 = 10;

/// The driver's canonical bind table — the single source both the installed
/// bundle's signed manifest and the autoload match are built from — in
/// [`Engine`] order.
pub const BIND_KEYS: &[DriverBindKey] = &[
    DriverBindKey::new(BIND_PRIORITY, compatible_key!(DMA_COMPATIBLE)),
    DriverBindKey::new(BIND_PRIORITY, compatible_key!(DMA4_COMPATIBLE)),
];

/// Which engines a node holds, as its matched bind key names them.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Engine {
    /// The legacy engines.
    Legacy,
    /// The BCM2711's DMA4 engines.
    Dma4,
}

impl Engine {
    /// The engines bound by [`BIND_KEYS`]'s entry `matched`.
    #[must_use]
    pub const fn of(matched: usize) -> Option<Self> {
        match matched {
            0 => Some(Self::Legacy),
            1 => Some(Self::Dma4),
            _ => None,
        }
    }
}

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
