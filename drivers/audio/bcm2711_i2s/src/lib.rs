//! The BCM2711's PCM/I²S block (`brcm,bcm2835-i2s`): a digital audio
//! interface whose transmit FIFO a cyclic DMA channel keeps fed, composed with
//! the codec its sound card links it to.
//!
//! [`pcm`] is the block's registers and the framing a link's format makes of
//! them; [`interface`] is the audio class over the block, its DMA channel, the
//! bit clock it drives or follows, and its codec. The `Run` binary wires them
//! to the suppliers the node's links name. The interface plays; a DAC codec
//! is the only kind `codec-v1` describes.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod interface;
pub mod pcm;

use tairix_abi::{CapabilityId, DriverBindKey, DriverError, DriverHandle, DriverHost, HwMatchKey};

/// The capabilities the driver runs with, which its signed manifest requests:
/// the block's registers, the DMA, clock and codec endpoints its links name,
/// the device-channel endpoint and the node publishing it to the mixer, the
/// shared regions, and the log.
pub const REQUIRED_CAPABILITIES: &[CapabilityId] = &[
    CapabilityId::MMIO_MAP,
    CapabilityId::IPC_ENDPOINT,
    CapabilityId::IPC_BIND_PRIVILEGED,
    CapabilityId::HW_EMIT,
    CapabilityId::SHM,
    CapabilityId::LOG_EMIT,
];

/// Device-tree `compatible` string of the PCM/I²S block.
pub const I2S_COMPATIBLE: &[u8] = b"brcm,bcm2835-i2s";

/// The bind priority [`BIND_KEYS`] carries: an exact `compatible` match.
const BIND_PRIORITY: u16 = 10;

/// The driver's canonical bind table — the single source both the installed
/// bundle's signed manifest and the autoload match are built from.
pub const BIND_KEYS: &[DriverBindKey] = &[DriverBindKey::new(
    BIND_PRIORITY,
    match HwMatchKey::compatible(I2S_COMPATIBLE) {
        Ok(key) => key,
        // A literal too long for the key would fail const evaluation here,
        // never at run time.
        Err(_) => panic!("compatible string fits HW_COMPATIBLE_MAX"),
    },
)];

/// Handle marker [`register`] returns; the host re-issues its own. `"I2SA"`.
const REGISTER_HANDLE_MARKER: u64 = 0x4932_5341_0000_0001;

/// Driver entry point.
///
/// # Errors
///
/// [`DriverError::PermissionDenied`] if the host did not grant
/// [`CapabilityId::DRV_LOAD`].
///
/// # Capabilities
///
/// Requires [`CapabilityId::DRV_LOAD`]. Serving the interface additionally
/// needs the grants its matched node requested: the block's window, its
/// transmit DMA request line, its codec link and, where it drives the bit
/// clock, its clock link.
pub fn register(host: &dyn DriverHost) -> Result<DriverHandle, DriverError> {
    if !host.has_capability(CapabilityId::DRV_LOAD) {
        return Err(DriverError::PermissionDenied);
    }
    DriverHandle::from_raw(REGISTER_HANDLE_MARKER)
}
