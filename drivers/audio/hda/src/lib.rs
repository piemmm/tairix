//! TAIRiX Intel High Definition Audio driver (`plans/SOUND.md` SND11): the
//! controller every PC motherboard carries, and the one QEMU emulates as
//! `intel-hda`, served through the [`Audio`] class trait.
//!
//! The controller's command and response rings carry codec verbs, and its
//! stream descriptors move samples over buffers this driver owns. Each codec
//! on its link is read from what the codec itself states and its graph
//! planned into sinks and sources: no quirk table names a board.
//! [`engine::Hda`] composes them.
//!
//! The `lib` target is the driver's identity — [`BIND_KEYS`] and
//! [`REQUIRED_CAPS`], which the image builder authors its signed manifest
//! from — and its host-testable engine; `src/main.rs` is the `Run` program.
//!
//! [`Audio`]: tairix_abi::driver::audio::Audio

#![no_std]
#![forbid(unsafe_op_in_unsafe_fn)]
#![deny(missing_docs)]

extern crate alloc;

mod codec;
mod controller;
pub mod engine;
mod format;
mod plan;
mod regs;
mod verb;

pub use controller::Wait;
pub use regs::Registers;

#[cfg(test)]
mod model;

use tairix_abi::driver::pci::CLASS_HD_AUDIO;
use tairix_abi::{CapabilityId, DriverBindKey, HwMatchKey};

/// A class match, so a driver naming an exact controller outranks it.
const BIND_PRIORITY: u16 = 5;

/// Every HD Audio controller, any vendor and product.
pub const BIND_KEYS: &[DriverBindKey] = &[DriverBindKey::new(
    BIND_PRIORITY,
    HwMatchKey::pci(0, 0, CLASS_HD_AUDIO),
)];

/// The capability set the driver runs under, and the one its manifest
/// requests: map its register window, carve its rings and buffers, park on
/// its interrupt, map the mixer's regions, claim and bind the reserved
/// device-channel endpoint, publish its node, and log.
pub const REQUIRED_CAPS: &[CapabilityId] = &[
    CapabilityId::MMIO_MAP,
    CapabilityId::MEM_DMA,
    CapabilityId::IRQ_BIND,
    CapabilityId::SHM,
    CapabilityId::IPC_ENDPOINT,
    CapabilityId::IPC_BIND_PRIVILEGED,
    CapabilityId::HW_EMIT,
    CapabilityId::LOG_EMIT,
];

#[cfg(test)]
mod tests {
    use super::BIND_KEYS;
    use tairix_abi::driver::pci::CLASS_HD_AUDIO;
    use tairix_abi::HwMatchKey;

    #[test]
    fn every_hd_audio_controller_binds_and_nothing_else_does() {
        let binds = |node: HwMatchKey| BIND_KEYS.iter().any(|bind| bind.key.matches(&node));
        assert!(binds(HwMatchKey::pci(0x8086, 0x2668, CLASS_HD_AUDIO)));
        assert!(binds(HwMatchKey::pci(0x1002, 0xAB38, CLASS_HD_AUDIO)));
        // AC'97 and the legacy multimedia classes are no business of this
        // driver.
        for class in [0x04_01_00, 0x04_80_00, 0x0C_03_30] {
            assert!(
                !binds(HwMatchKey::pci(0x8086, 0x2415, class)),
                "{class:06x}"
            );
        }
    }
}
