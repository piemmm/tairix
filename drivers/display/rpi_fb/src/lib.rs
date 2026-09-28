//! Raspberry Pi firmware-framebuffer display service
//! (`brcm,bcm2708-fb`).
//!
//! On a Pi the scan-out surface is one the `VideoCore` firmware allocated,
//! and its output belongs to the firmware too. The surface is plain linear
//! memory, so it is presented through the one linear-surface engine
//! (`tairix_display::Framebuffer`, wrapped by [`FirmwareDisplay`]); what this
//! driver adds is the one control a generic framebuffer cannot reach —
//! switching the display off, through the firmware's blank request over the
//! mailbox service.
//!
//! Whether a blanked HDMI output also drops its signal, so the monitor
//! sleeps, is the firmware's own `hdmi_blanking` setting; either way the
//! panel shows nothing and the desktop does no work for it.
//!
//! Reference: the Raspberry Pi firmware property interface
//! (`RPI_FIRMWARE_FRAMEBUFFER_BLANK`), as the `bcm2708_fb` driver in the
//! Raspberry Pi Linux tree spells it.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use tairix_abi::driver::display::{
    DamageRect, Display, DisplayDeviceReport, DisplayMode, DisplayPower,
};
use tairix_abi::driver::mailbox::MailboxChannel;
use tairix_abi::{CapabilityId, DriverBindKey, DriverError, HwMatchKey};
use tairix_vcmailbox::{
    decode_blank_screen_response, encode_blank_screen, MailboxError,
    FIRMWARE_FRAMEBUFFER_COMPATIBLE,
};

#[cfg(test)]
mod tests;

/// The bind priority [`BIND_KEYS`] carries: above the generic linear-surface
/// driver's exact-match tier, which also matches this surface through the
/// `simple-framebuffer` key published beside this one.
const BIND_PRIORITY: u16 = 20;

/// The driver's canonical bind table: a scan-out surface the `VideoCore`
/// firmware allocated, as the aarch64 port publishes it.
pub const BIND_KEYS: &[DriverBindKey] = &[DriverBindKey::new(
    BIND_PRIORITY,
    match HwMatchKey::compatible(FIRMWARE_FRAMEBUFFER_COMPATIBLE) {
        Ok(key) => key,
        // Unreachable: the literal is well within `HW_COMPATIBLE_MAX`. A
        // too-long literal would be a compile-time const-eval error here,
        // never a runtime panic.
        Err(_) => panic!("compatible string fits HW_COMPATIBLE_MAX"),
    },
)];

/// The capabilities the service needs, and the single definition of them:
/// the surface window, the client frame regions, the reserved rendezvous,
/// the one-shot first-present record, and the firmware channel the display
/// is switched through. The signed manifest is built from this and the
/// program re-checks itself against it.
pub const REQUIRED_CAPABILITIES: &[CapabilityId] = &[
    CapabilityId::MMIO_MAP,
    CapabilityId::SHM,
    CapabilityId::IPC_BIND_PRIVILEGED,
    CapabilityId::LOG_EMIT,
    CapabilityId::MAILBOX,
];

/// A display whose picture is `surface` and whose power is the `VideoCore`
/// firmware's, reached over `firmware`.
pub struct FirmwareDisplay<'c, D> {
    surface: D,
    firmware: &'c dyn MailboxChannel,
}

impl<'c, D: Display> FirmwareDisplay<'c, D> {
    /// The firmware-allocated `surface`, switched through `firmware`.
    pub const fn new(surface: D, firmware: &'c dyn MailboxChannel) -> Self {
        Self { surface, firmware }
    }
}

impl<D: Display> Display for FirmwareDisplay<'_, D> {
    fn mode_info(&self) -> Result<DisplayMode, DriverError> {
        self.surface.mode_info()
    }

    fn device_report(&self) -> DisplayDeviceReport {
        self.surface.device_report()
    }

    fn present(&mut self, frame: &[u8]) -> Result<(), DriverError> {
        self.surface.present(frame)
    }

    fn present_rects(&mut self, frame: &[u8], damage: &[DamageRect]) -> Result<(), DriverError> {
        self.surface.present_rects(frame, damage)
    }

    fn set_power(&mut self, power: DisplayPower) -> Result<(), DriverError> {
        let blank = power == DisplayPower::Off;
        let mut message = encode_blank_screen(blank);
        self.firmware.exchange(&mut message)?;
        decode_blank_screen_response(&message, blank).map_err(MailboxError::as_driver_error)
    }
}
