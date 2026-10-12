//! The BCM2711's HDMI transmitters (`brcm,bcm2711-hdmi0`, `brcm,bcm2711-hdmi1`)
//! as `encoder-v1` encoders and `audiochan-v1` audio devices, driven as
//! Linux's `vc4_hdmi` drives them (`plans/SOUND.md` `SND19n`, `SND19o`).
//!
//! A transmitter finds its sink from its hotplug lines and the EDID its DDC
//! bus reads, chooses the mode the sink, the transmitter and the display
//! engine's pipe all carry, and takes the engine through a mode set's steps:
//! its clocks and PHY for the TMDS rate, its timing, colour conversion, the
//! `InfoFrames` an HDMI sink is sent, and the SCDC scrambling HDMI 2.0 rates
//! need. Its audio endpoint plays to the sink what the sink declares it plays.
//!
//! [`Hdmi`] is the encoder and the audio device over its register windows and
//! its links' clients, and [`Shared`] gives each protocol its share of it; the
//! `Run` binary maps the windows, opens the links and serves both.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

mod audio;
mod hdmi;
mod iec958;
mod mode;
mod phy;
mod regs;
mod shared;
mod sink;

#[cfg(test)]
mod model;
#[cfg(test)]
mod tests;

pub use hdmi::{AudioLinks, Hdmi, Links, Record, Variant, Windows};
pub use phy::LaneMap;
pub use shared::Shared;
pub use sink::Ddc;

use tairix_abi::{CapabilityId, DriverBindKey, DriverError, DriverHandle, DriverHost, HwMatchKey};
use tairix_vcmailbox::ENGINE_OUTPUTS;

/// The capabilities a transmitter's driver runs with, which its signed
/// manifest requests: its register windows, its DDC channel's and its audio
/// DMA buffers, the calls to its clocks, its DDC bus, its interrupt lines and
/// its DMA channel, its encoder and audio channel endpoints, the audio
/// channel's node, and the log.
pub const REQUIRED_CAPABILITIES: &[CapabilityId] = &[
    CapabilityId::MMIO_MAP,
    CapabilityId::SHM,
    CapabilityId::IPC_ENDPOINT,
    CapabilityId::IPC_BIND_PRIVILEGED,
    CapabilityId::HW_EMIT,
    CapabilityId::LOG_EMIT,
];

/// The bind priority [`BIND_KEYS`] carries: an exact `compatible` match.
const BIND_PRIORITY: u16 = 10;

const fn compatible_key(compatible: &[u8]) -> HwMatchKey {
    match HwMatchKey::compatible(compatible) {
        Ok(key) => key,
        // A literal too long for the key fails const evaluation, never at run
        // time.
        Err(_) => panic!("compatible string fits HW_COMPATIBLE_MAX"),
    }
}

/// The driver's canonical bind table, the one source its signed manifest and
/// the autoload match are built from: HDMI0, then HDMI1, the encoders the
/// display engine's outputs feed.
pub const BIND_KEYS: &[DriverBindKey] = &[
    DriverBindKey::new(BIND_PRIORITY, compatible_key(ENGINE_OUTPUTS[0].encoder)),
    DriverBindKey::new(BIND_PRIORITY, compatible_key(ENGINE_OUTPUTS[1].encoder)),
];

/// Each [`BIND_KEYS`] entry's transmitter, as Linux's
/// `bcm2711_hdmi0_variant` and `bcm2711_hdmi1_variant` describe them: only
/// HDMI0 reaches HDMI 2.0 rates, and HDMI1's lanes are wired swapped.
pub const VARIANTS: [Variant; 2] = [
    Variant {
        name: "HDMI 0",
        max_tmds_khz: 600_000,
        lanes: LaneMap {
            lanes: [0, 1, 2, 3],
        },
        vid_ctl: 0x44,
        mai: 0x10,
    },
    Variant {
        name: "HDMI 1",
        max_tmds_khz: 340_000,
        lanes: LaneMap {
            lanes: [1, 0, 3, 2],
        },
        vid_ctl: 0x48,
        mai: 0x30,
    },
];

/// The windows a transmitter is programmed through, by their `reg-names`:
/// the core, the video port, the PHY, the rate manager, the packet RAM, the
/// colour converter and the block both transmitters share.
pub const WINDOWS: [&[u8]; 7] = [b"hdmi", b"dvp", b"phy", b"rm", b"packet", b"csc", b"hd"];

/// The state machine clock's `clock-names` entry.
pub const HSM_CLOCK: &[u8] = b"hdmi";

/// The pixel path's clock's `clock-names` entry.
pub const BVB_CLOCK: &[u8] = b"bvb";

/// The MAI's sample clock's `clock-names` entry.
pub const AUDIO_CLOCK: &[u8] = b"audio";

/// The `dma-names` entry of the DMA line feeding the MAI, which the binding
/// names for the opposite direction.
pub const AUDIO_DMA: &[u8] = b"audio-rx";

/// The DDC bus's link, the node's `ddc` phandle.
pub const DDC_LINK: &[u8] = b"ddc";

/// The positions of `hpd-connected` and `hpd-removed` among the binding's
/// interrupts, whose names are longer than a link carries.
pub const HOTPLUG_LINES: [u8; 2] = [4, 5];

/// Handle marker [`register`] returns; the host re-issues its own. `"HDMI"`.
const REGISTER_HANDLE_MARKER: u64 = 0x4844_4D49_0000_0001;

/// Driver entry point.
///
/// # Errors
///
/// [`DriverError::PermissionDenied`] if the host did not grant
/// [`CapabilityId::DRV_LOAD`].
///
/// # Capabilities
///
/// Requires [`CapabilityId::DRV_LOAD`]; serving the encoder additionally
/// needs [`REQUIRED_CAPABILITIES`].
pub fn register(host: &dyn DriverHost) -> Result<DriverHandle, DriverError> {
    if !host.has_capability(CapabilityId::DRV_LOAD) {
        return Err(DriverError::PermissionDenied);
    }
    DriverHandle::from_raw(REGISTER_HANDLE_MARKER)
}
