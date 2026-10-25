//! BCM2711 AVS temperature sensor driver (`brcm,bcm2711-thermal`).
//!
//! The sensor is a register of the system-on-chip's AVS monitor: a ten-bit
//! code, valid
//! once a conversion has completed, which the thermal zone's linear
//! calibration turns into millidegrees Celsius (`plans/COOLING.md` CL4). The
//! architecture port grants the node its monitor's window and states the
//! calibration; the `Run` binary maps the window and serves [`AvsSensor`] on
//! the node's `thermal-sensor-v1` channel.
//!
//! Reference: OpenBSD `sys/dev/fdt/bcm2711_tmon.c` (ISC), for the status
//! register's offset, valid bit and code field.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use tairix_abi::driver::thermal::{Millicelsius, ThermalCalibration, ThermalSensor};
use tairix_abi::{
    compatible_key, CapabilityId, DriverBindKey, DriverError, DriverHandle, DriverHost, Errno,
    RegisterBlock,
};

/// The capabilities the driver runs with, which its signed manifest requests:
/// the monitor's window, the sensor's channel and its node, and the log.
pub const REQUIRED_CAPABILITIES: &[CapabilityId] = &[
    CapabilityId::MMIO_MAP,
    CapabilityId::IPC_BIND_PRIVILEGED,
    CapabilityId::HW_EMIT,
    CapabilityId::LOG_EMIT,
];

/// The driver's canonical bind table: an exact `compatible` match.
pub const BIND_KEYS: &[DriverBindKey] = &[DriverBindKey::new(
    10,
    compatible_key!(b"brcm,bcm2711-thermal"),
)];

/// The temperature status register within the monitor's window.
const TEMP_STATUS: usize = 0x200;

/// Bytes of window the sensor reads.
pub const WINDOW_LEN: usize = TEMP_STATUS + 4;

/// The status bits either of which marks a completed conversion.
const TEMP_VALID: u32 = (1 << 16) | (1 << 10);

/// The conversion's code.
const TEMP_CODE: u32 = 0x3FF;

/// The sensor over the monitor's registers.
pub struct AvsSensor<R: RegisterBlock> {
    regs: R,
    calibration: ThermalCalibration,
}

impl<R: RegisterBlock> AvsSensor<R> {
    /// The sensor behind `regs`, its codes converted by `calibration`.
    ///
    /// # Errors
    ///
    /// [`DriverError::OutOfRange`] for a window too short to reach the
    /// status register.
    pub fn new(regs: R, calibration: ThermalCalibration) -> Result<Self, DriverError> {
        if regs.block_len() < WINDOW_LEN {
            return Err(DriverError::OutOfRange);
        }
        Ok(Self { regs, calibration })
    }
}

impl<R: RegisterBlock> ThermalSensor for AvsSensor<R> {
    fn read(&mut self) -> Result<Millicelsius, Errno> {
        let status = self
            .regs
            .read32(TEMP_STATUS)
            .map_err(|_| Errno::DeviceFault)?;
        if status & TEMP_VALID == 0 {
            return Err(Errno::NoData);
        }
        self.calibration
            .convert(status & TEMP_CODE)
            .ok_or(Errno::DeviceFault)
    }
}

/// Handle marker [`register`] returns; the host re-issues its own. `"AVST"`.
const REGISTER_HANDLE_MARKER: u64 = 0x4156_5354_0000_0001;

/// Driver entry point.
///
/// # Errors
///
/// [`DriverError::PermissionDenied`] if the host did not grant
/// [`CapabilityId::DRV_LOAD`].
///
/// # Capabilities
///
/// Requires [`CapabilityId::DRV_LOAD`]. Serving the sensor additionally needs
/// the window its matched node requested.
pub fn register(host: &dyn DriverHost) -> Result<DriverHandle, DriverError> {
    if !host.has_capability(CapabilityId::DRV_LOAD) {
        return Err(DriverError::PermissionDenied);
    }
    DriverHandle::from_raw(REGISTER_HANDLE_MARKER)
}

#[cfg(test)]
mod tests;
