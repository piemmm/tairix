//! TAIRiX USB Audio Class driver (`plans/SOUND.md` SND7): one per audio
//! function, every sink and source on it served through the [`Audio`] class
//! trait.
//!
//! The driver binds the function's control interface, which the host
//! controller publishes, and claims the streaming interfaces the function
//! groups with it, which nothing else serves. It reads the function's own
//! descriptors — the entity graph of [`topology`], the formats of
//! [`streaming`] — and drives each streaming interface as one endpoint of the
//! device: selecting the alternate setting a configuration needs, setting its
//! clock, and moving its samples over an isochronous stream the host
//! controller schedules ([`engine`]). Its control requests reach no other
//! interface, and it holds no controller register, DMA or interrupt.
//!
//! The `lib` target is the driver's identity — [`BIND_KEYS`] and
//! [`REQUIRED_CAPS`], which the image builder authors its signed manifest
//! from — and its host-testable model and engine; `src/main.rs` is the `Run`
//! program.
//!
//! [`Audio`]: tairix_abi::driver::audio::Audio

#![no_std]
#![forbid(unsafe_op_in_unsafe_fn)]
#![deny(missing_docs)]

extern crate alloc;

mod controls;
pub mod engine;
pub mod requests;
pub mod stream;
pub mod streaming;
pub mod topology;

use tairix_abi::{CapabilityId, DriverBindKey, HwMatchKey};

/// The control interface of a version 1.0 function: class audio, sub-class
/// control, no protocol.
pub const AUDIO_CONTROL_V1: u32 = 0x01_01_00;

/// The control interface of a version 2.0 function: protocol
/// `IP_VERSION_02_00`.
pub const AUDIO_CONTROL_V2: u32 = 0x01_01_20;

/// A class match, so a driver naming an exact device outranks it.
const BIND_PRIORITY: u16 = 5;

/// Every audio function's control interface, any vendor and product.
pub const BIND_KEYS: &[DriverBindKey] = &[
    DriverBindKey::new(BIND_PRIORITY, HwMatchKey::usb(0, 0, AUDIO_CONTROL_V1)),
    DriverBindKey::new(BIND_PRIORITY, HwMatchKey::usb(0, 0, AUDIO_CONTROL_V2)),
];

/// The capability set the driver runs under, and the one its manifest
/// requests: map its interface's shared buffer and its streams' regions, call
/// its interface's URB endpoint and bind its stream ports, bind the reserved
/// device-channel endpoint, publish the device channel's node, and log.
pub const REQUIRED_CAPS: &[CapabilityId] = &[
    CapabilityId::SHM,
    CapabilityId::IPC_ENDPOINT,
    CapabilityId::IPC_BIND_PRIVILEGED,
    CapabilityId::HW_EMIT,
    CapabilityId::LOG_EMIT,
];

#[cfg(test)]
mod fixtures;

#[cfg(test)]
mod tests {
    use super::{AUDIO_CONTROL_V1, AUDIO_CONTROL_V2, BIND_KEYS};
    use tairix_abi::HwMatchKey;

    #[test]
    fn both_revisions_control_interfaces_bind_and_nothing_else_does() {
        let binds = |class| {
            BIND_KEYS
                .iter()
                .any(|bind| bind.key.matches(&HwMatchKey::usb(0x46F4, 0x0002, class)))
        };
        assert!(binds(AUDIO_CONTROL_V1));
        assert!(binds(AUDIO_CONTROL_V2));
        // Streaming interfaces are claimed, never bound; MIDI and the 3.0
        // protocol are other drivers' work.
        for class in [0x01_02_00, 0x01_02_20, 0x01_03_00, 0x01_01_30, 0x03_00_00] {
            assert!(!binds(class), "{class:06x}");
        }
    }
}
