//! The Raspberry Pi 4's 3.5 mm jack (`tairix,bcm2711-pwm-audio`): the PWM
//! block whose two channels the board wires to the jack, fed by a cyclic DMA
//! channel.
//!
//! The jack runs at [`TARGET_RATE_HZ`], each PWM period [`TARGET_LEVELS`]
//! clock cycles long, so a duty carries about eight bits; [`shaper`] shapes
//! each sample onto those levels with third-order error feedback, which
//! leaves the audible band near 15 bits quiet while the noise rises far above
//! it. The mixer resamples to the jack's rate, so no second resampler exists
//! here. [`pwm`] is the block's registers, [`jack`] the audio class over the
//! DMA channel, and the `Run` binary wires them to the PWM clock and the DMA
//! controller the node's links name.
//!
//! The binding is first-party: the image's overlay names the jack's PWM block
//! `tairix,bcm2711-pwm-audio` ahead of `brcm,bcm2835-pwm`, so the block a
//! board wires to its jack is told apart from one driving a fan or a
//! backlight.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod jack;
pub mod pwm;
pub mod shaper;

use tairix_abi::driver::audio::Rate;
use tairix_abi::{CapabilityId, DriverBindKey, DriverError, DriverHandle, DriverHost, HwMatchKey};

/// The capabilities the driver runs with, which its signed manifest requests:
/// the PWM block's registers, the DMA and clock endpoints its links name, the
/// device-channel endpoint and the node publishing it to the mixer, the
/// shared regions, and the log.
pub const REQUIRED_CAPABILITIES: &[CapabilityId] = &[
    CapabilityId::MMIO_MAP,
    CapabilityId::IPC_ENDPOINT,
    CapabilityId::IPC_BIND_PRIVILEGED,
    CapabilityId::HW_EMIT,
    CapabilityId::SHM,
    CapabilityId::LOG_EMIT,
];

/// Device-tree `compatible` string the image's overlay gives the jack's PWM
/// block.
pub const PWM_AUDIO_COMPATIBLE: &[u8] = b"tairix,bcm2711-pwm-audio";

/// The bind priority [`BIND_KEYS`] carries: an exact `compatible` match.
const BIND_PRIORITY: u16 = 10;

/// The driver's canonical bind table — the single source both the installed
/// bundle's signed manifest and the autoload match are built from.
pub const BIND_KEYS: &[DriverBindKey] = &[DriverBindKey::new(
    BIND_PRIORITY,
    match HwMatchKey::compatible(PWM_AUDIO_COMPATIBLE) {
        Ok(key) => key,
        // A literal too long for the key would fail const evaluation here,
        // never at run time.
        Err(_) => panic!("compatible string fits HW_COMPATIBLE_MAX"),
    },
)];

/// The rate the jack runs at, for which the shaper's measured figures stand.
pub const TARGET_RATE_HZ: u32 = 375_000;

/// Clock cycles a PWM period lasts at that rate.
pub const TARGET_LEVELS: u32 = 250;

/// The PWM clock the two ask for, which the clock manager makes from PLLD by
/// a whole divisor and so without MASH jitter.
pub const CLOCK_HZ: u64 = TARGET_RATE_HZ as u64 * TARGET_LEVELS as u64;

/// The levels a period lasts and the rate the jack runs at on a PWM clock of
/// `clock_hz`: the levels nearest the target rate's, and the rate they make.
/// [`None`] for a clock too slow to shape onto.
#[must_use]
pub fn timing(clock_hz: u64) -> Option<(u32, Rate)> {
    let target = u64::from(TARGET_RATE_HZ);
    let levels = u32::try_from((clock_hz + target / 2) / target).ok()?;
    if levels < shaper::MIN_LEVELS {
        return None;
    }
    let levels_hz = u64::from(levels);
    let hz = u32::try_from((clock_hz + levels_hz / 2) / levels_hz).ok()?;
    Some((levels, Rate::new(hz).ok()?))
}

/// Handle marker [`register`] returns; the host re-issues its own. `"PWMA"`.
const REGISTER_HANDLE_MARKER: u64 = 0x5057_4D41_0000_0001;

/// Driver entry point.
///
/// # Errors
///
/// [`DriverError::PermissionDenied`] if the host did not grant
/// [`CapabilityId::DRV_LOAD`].
///
/// # Capabilities
///
/// Requires [`CapabilityId::DRV_LOAD`]. Serving the jack additionally needs
/// the grants its matched node requested: the PWM block's window, its DMA
/// request line and its clock link.
pub fn register(host: &dyn DriverHost) -> Result<DriverHandle, DriverError> {
    if !host.has_capability(CapabilityId::DRV_LOAD) {
        return Err(DriverError::PermissionDenied);
    }
    DriverHandle::from_raw(REGISTER_HANDLE_MARKER)
}

#[cfg(test)]
mod tests {
    use super::{timing, CLOCK_HZ, TARGET_LEVELS, TARGET_RATE_HZ};

    #[test]
    fn the_target_clock_gives_the_target_rate_and_another_its_nearest() {
        let (levels, rate) = timing(CLOCK_HZ).expect("a timing");
        assert_eq!((levels, rate.hz()), (TARGET_LEVELS, TARGET_RATE_HZ));
        let (levels, rate) = timing(100_000_000).expect("a timing");
        assert_eq!((levels, rate.hz()), (267, 374_532));
        assert!(timing(1_000_000).is_none(), "too slow to shape onto");
    }
}
