//! BCM2711 DVP clock gates (`brcm,brcm2711-dvp`).
//!
//! The DVP block gates the 108 MHz clock each HDMI block's audio logic runs
//! from. It serves the node's `clock-v1` endpoint: an HDMI driver holds its
//! gate open while its block is up.
//!
//! The block's HDMI resets are not served: an HDMI block is brought up by
//! programming it, and the HDMI driver asks for no reset.
//!
//! The gate register and its bits are documented nowhere public; the metal
//! run confirms them (`plans/PI.md` P14).

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

#[cfg(test)]
mod tests;

use tairix_abi::{
    compatible_key, CapabilityId, DriverBindKey, DriverError, DriverHandle, DriverHost,
    RegisterBlock,
};
use tairix_clock::{Applied, ClockHardware, Halt, Policy};

/// The register holding the gates.
const MISC_CONFIG: usize = 0x08;

/// Each gate by the binding's clock index, with its bit, set to close it.
const GATES: [(u32, u32); 2] = [(0, 1 << 3), (1, 1 << 4)];

/// Bytes of register window the gates need.
pub const WINDOW_LEN: usize = MISC_CONFIG + 4;

/// The capabilities the driver runs with, which its signed manifest requests:
/// the register window, the controller's endpoint, and the log its decisions
/// go to.
pub const REQUIRED_CAPABILITIES: &[CapabilityId] = &[
    CapabilityId::MMIO_MAP,
    CapabilityId::IPC_BIND_PRIVILEGED,
    CapabilityId::LOG_EMIT,
];

/// Device-tree `compatible` string of the DVP block, as the binding spells it.
pub const DVP_COMPATIBLE: &[u8] = b"brcm,brcm2711-dvp";

/// The bind priority [`BIND_KEYS`] carries: an exact `compatible` match.
const BIND_PRIORITY: u16 = 10;

/// The driver's canonical bind table — the single source both the installed
/// bundle's signed manifest and the autoload match are built from.
pub const BIND_KEYS: &[DriverBindKey] = &[DriverBindKey::new(
    BIND_PRIORITY,
    compatible_key!(DVP_COMPATIBLE),
)];

/// The two gates. A gate runs only at the rate of the clock it gates, so each
/// is floored: a holder needing no more than that rate is served by it, one
/// needing more is refused rather than given less.
pub struct Gates<'r, R: RegisterBlock + ?Sized> {
    regs: &'r R,
    parent_hz: u64,
}

impl<'r, R: RegisterBlock + ?Sized> Gates<'r, R> {
    /// The gates behind `regs`, gating a clock of `parent_hz`. Touches no
    /// register.
    ///
    /// # Errors
    ///
    /// [`DriverError::LengthOutOfRange`] for a window too short for the
    /// gates, or [`DriverError::OutOfRange`] for a parent stated at no rate.
    pub fn new(regs: &'r R, parent_hz: u64) -> Result<Self, DriverError> {
        if regs.block_len() < WINDOW_LEN {
            return Err(DriverError::LengthOutOfRange);
        }
        if parent_hz == 0 {
            return Err(DriverError::OutOfRange);
        }
        Ok(Self { regs, parent_hz })
    }

    fn is_open(&self, at: usize) -> Result<bool, DriverError> {
        Ok(self.regs.read32(MISC_CONFIG)? & GATES[at].1 == 0)
    }

    fn set(&self, at: usize, open: bool) -> Result<Halt, DriverError> {
        let bit = GATES[at].1;
        let config = self.regs.read32(MISC_CONFIG)?;
        self.regs
            .write32(MISC_CONFIG, if open { config & !bit } else { config | bit })?;
        if self.is_open(at)? != open {
            return Err(DriverError::DeviceFault);
        }
        Ok(Halt::Clean)
    }
}

impl<R: RegisterBlock + ?Sized> ClockHardware for Gates<'_, R> {
    type Setting = ();

    fn count(&self) -> usize {
        GATES.len()
    }

    fn served(&self, selector: &[u32]) -> Option<usize> {
        let &[index] = selector else {
            return None;
        };
        GATES.iter().position(|&(gate, _)| gate == index)
    }

    fn id(&self, at: usize) -> u32 {
        GATES[at].0
    }

    fn policy(&self, _at: usize) -> Policy {
        Policy::Floor
    }

    fn rate(&self, at: usize) -> Result<u64, DriverError> {
        Ok(if self.is_open(at)? { self.parent_hz } else { 0 })
    }

    fn max_rate(&self, _at: usize) -> Result<u64, DriverError> {
        Ok(self.parent_hz)
    }

    fn nearest(&self, _at: usize, hz: u64) -> Result<(), DriverError> {
        if hz > self.parent_hz {
            return Err(DriverError::OutOfRange);
        }
        Ok(())
    }

    fn run(&mut self, at: usize, (): ()) -> Result<Applied, DriverError> {
        let halt = self.set(at, true)?;
        Ok(Applied {
            halt,
            hz: self.parent_hz,
        })
    }

    fn stop(&mut self, at: usize) -> Result<Halt, DriverError> {
        self.set(at, false)
    }
}

/// Handle marker [`register`] returns; the host re-issues its own. `"DVP1"`.
const REGISTER_HANDLE_MARKER: u64 = 0x4456_5031_0000_0001;

/// Driver entry point.
///
/// # Errors
///
/// [`DriverError::PermissionDenied`] if the host did not grant
/// [`CapabilityId::DRV_LOAD`].
///
/// # Capabilities
///
/// Requires [`CapabilityId::DRV_LOAD`]. Serving the gates additionally needs
/// the register window its matched node requested and the node's clock
/// `LinkDuty`.
pub fn register(host: &dyn DriverHost) -> Result<DriverHandle, DriverError> {
    if !host.has_capability(CapabilityId::DRV_LOAD) {
        return Err(DriverError::PermissionDenied);
    }
    DriverHandle::from_raw(REGISTER_HANDLE_MARKER)
}
