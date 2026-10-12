//! Raspberry Pi 4 display engine (`brcm,bcm2711-hvs`), driven natively as
//! Linux's full-KMS `vc4` drives it (`plans/SOUND.md` `SND19m`).
//!
//! The firmware lights the screen at boot; the engine tells it the display is
//! the ARM's now, brings down whatever pipe it left running in the order the
//! hardware requires, and sets every mode itself. The HVS composites the
//! seat's surface into each HDMI output's pixel valve, the encoder behind the
//! valve is driven over `encoder-v1`, and the seat's display is served from
//! frames no channel reads while they are written, so a present neither
//! tears nor waits.
//!
//! [`Engine`] is the display over its register windows and its links'
//! clients; the `Run` binary maps the windows, opens the links and serves it.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod dlist;
mod engine;
mod frames;
mod hvs;
mod regs;
mod valve;

#[cfg(test)]
mod engine_tests;
#[cfg(test)]
mod model;

pub use dlist::Surface;
pub use engine::{pipe_limits, Engine, Parts, Record, OUTPUTS_MAX};
pub use frames::{FrameAllocator, FrameMemory};
pub use hvs::Output;
pub use valve::{Load, ValveKind};

use tairix_abi::driver::dma::{DmaHost, DmaSlab};
use tairix_abi::{CapabilityId, DriverBindKey, DriverError, DriverHandle, DriverHost};
use tairix_vcmailbox::{DISPLAY_ENGINE_KEY, ENGINE_OUTPUTS};

/// The capabilities the engine runs with, which its signed manifest requests:
/// the register windows, the scan-out frames, the client frames it presents,
/// the calls to its encoders and its core clock, the seat's display endpoint,
/// the firmware notice, and the log.
pub const REQUIRED_CAPABILITIES: &[CapabilityId] = &[
    CapabilityId::MMIO_MAP,
    CapabilityId::MEM_DMA,
    CapabilityId::SHM,
    CapabilityId::IPC_ENDPOINT,
    CapabilityId::IPC_BIND_PRIVILEGED,
    CapabilityId::MAILBOX,
    CapabilityId::LOG_EMIT,
];

/// The bind priority [`BIND_KEYS`] carries: an exact `compatible` match.
const BIND_PRIORITY: u16 = 10;

/// The driver's canonical bind table, the one source its signed manifest and
/// the autoload match are built from: the HVS node, the device the mailbox
/// service admits the takeover notice for.
pub const BIND_KEYS: &[DriverBindKey] = &[DriverBindKey::new(BIND_PRIORITY, DISPLAY_ENGINE_KEY)];

/// The HDMI-feeding pixel valves, as the BCM2711 wires them: each a named
/// window on the engine's node, reading its HVS output, feeding the encoder
/// its node links to. Their FIFO depths and loads are Linux's
/// `bcm2711_pv2_data` and `bcm2711_pv4_data`.
pub const VALVES: [ValveKind; 2] = [
    ValveKind {
        window: ENGINE_OUTPUTS[0].window,
        encoder: ENGINE_OUTPUTS[0].link,
        output: Output::Four,
        channel: 0,
        fifo_depth: 256,
        fixed_level: None,
        load: Load::Active,
    },
    ValveKind {
        window: ENGINE_OUTPUTS[1].window,
        encoder: ENGINE_OUTPUTS[1].link,
        output: Output::Five,
        channel: 1,
        fifo_depth: 64,
        // This valve runs only with its FIFO level at 32, whatever its depth.
        fixed_level: Some(32),
        load: Load::Clock,
    },
];

/// The HVS's register window's name on the engine's node.
pub const HVS_WINDOW: &[u8] = b"hvs";

/// Scan-out frames in DMA memory the HVS reaches.
pub struct DmaFrames<'h>(pub &'h dyn DmaHost);

/// One scan-out frame in DMA memory.
pub struct DmaFrame {
    slab: DmaSlab,
    bus: u32,
}

impl FrameMemory for DmaFrame {
    fn bus(&self) -> u32 {
        self.bus
    }

    fn bytes_mut(&mut self) -> &mut [u8] {
        self.slab.as_bytes_mut()
    }

    fn flush(&self, offset: usize, len: usize) {
        self.slab.sync_range(offset, len);
    }
}

impl FrameAllocator for DmaFrames<'_> {
    type Memory = DmaFrame;

    /// # Errors
    ///
    /// The host's refusal, or [`DriverError::OutOfRange`] for memory past
    /// the HVS's 32-bit reach.
    fn alloc(&self, len: usize) -> Result<DmaFrame, DriverError> {
        let slab = self.0.alloc_dma_zeroed(len)?;
        let end = slab
            .device_addr()
            .checked_add(len as u64)
            .ok_or(DriverError::OutOfRange)?;
        if end > 1 << 32 {
            return Err(DriverError::OutOfRange);
        }
        let bus = u32::try_from(slab.device_addr()).map_err(|_| DriverError::OutOfRange)?;
        Ok(DmaFrame { slab, bus })
    }
}

/// Handle marker [`register`] returns; the host re-issues its own. `"HVS5"`.
const REGISTER_HANDLE_MARKER: u64 = 0x4856_5335_0000_0001;

/// Driver entry point.
///
/// # Errors
///
/// [`DriverError::PermissionDenied`] if the host did not grant
/// [`CapabilityId::DRV_LOAD`].
///
/// # Capabilities
///
/// Requires [`CapabilityId::DRV_LOAD`]; serving the display additionally needs
/// [`REQUIRED_CAPABILITIES`].
pub fn register(host: &dyn DriverHost) -> Result<DriverHandle, DriverError> {
    if !host.has_capability(CapabilityId::DRV_LOAD) {
        return Err(DriverError::PermissionDenied);
    }
    DriverHandle::from_raw(REGISTER_HANDLE_MARKER)
}
