//! Host tests: the firmware power switch against the protocol-faithful mock
//! firmware, and the surface delegation against a recording surface.

use core::cell::{Cell, RefCell};

use tairix_abi::driver::display::{DamageRect, Display, DisplayFormat, DisplayMode, DisplayPower};
use tairix_abi::driver::mailbox::{MailboxChannel, MAILBOX_PROPERTY_WORDS};
use tairix_abi::{DriverError, HwMatchKey};
use tairix_vcmailbox::mock::MockFirmware;
use tairix_vcmailbox::FIRMWARE_FRAMEBUFFER_COMPATIBLE;

use crate::{FirmwareDisplay, BIND_KEYS};

const MODE: DisplayMode = DisplayMode {
    width_px: 4,
    height_px: 2,
    stride_bytes: 16,
    format: DisplayFormat::Bgra8888,
};

/// The mock firmware behind the service channel.
struct Firmware(RefCell<MockFirmware>);

impl Firmware {
    fn healthy() -> Self {
        Self(RefCell::new(MockFirmware::healthy()))
    }

    fn blanked(&self) -> bool {
        self.0.borrow().blanked
    }
}

impl MailboxChannel for Firmware {
    fn exchange(&self, message: &mut [u32; MAILBOX_PROPERTY_WORDS]) -> Result<(), DriverError> {
        self.0.borrow_mut().respond(message);
        Ok(())
    }
}

/// A mailbox service that is not there.
struct Unreachable;

impl MailboxChannel for Unreachable {
    fn exchange(&self, _: &mut [u32; MAILBOX_PROPERTY_WORDS]) -> Result<(), DriverError> {
        Err(DriverError::NotFound)
    }
}

/// A firmware that honours the tag but leaves the output where it was.
struct Stuck(RefCell<MockFirmware>);

impl MailboxChannel for Stuck {
    fn exchange(&self, message: &mut [u32; MAILBOX_PROPERTY_WORDS]) -> Result<(), DriverError> {
        let asked = message[5];
        self.0.borrow_mut().respond(message);
        message[5] = asked ^ 1;
        Ok(())
    }
}

/// A surface that counts what reached it.
#[derive(Default)]
struct Surface {
    presents: Cell<u32>,
    region_presents: Cell<u32>,
}

impl Display for Surface {
    fn mode_info(&self) -> Result<DisplayMode, DriverError> {
        Ok(MODE)
    }

    fn present(&mut self, _frame: &[u8]) -> Result<(), DriverError> {
        self.presents.set(self.presents.get() + 1);
        Ok(())
    }

    fn present_rects(&mut self, _frame: &[u8], _damage: &[DamageRect]) -> Result<(), DriverError> {
        self.region_presents.set(self.region_presents.get() + 1);
        Ok(())
    }
}

#[test]
fn switching_the_display_off_blanks_the_firmware_output() {
    let firmware = Firmware::healthy();
    let mut display = FirmwareDisplay::new(Surface::default(), &firmware);
    assert_eq!(display.set_power(DisplayPower::Off), Ok(()));
    assert!(firmware.blanked());
    assert_eq!(display.set_power(DisplayPower::On), Ok(()));
    assert!(!firmware.blanked());
}

#[test]
fn the_picture_is_the_firmware_surfaces_own() {
    let firmware = Firmware::healthy();
    let mut display = FirmwareDisplay::new(Surface::default(), &firmware);
    assert_eq!(display.mode_info(), Ok(MODE));
    assert_eq!(display.present(&[0u8; 32]), Ok(()));
    assert_eq!(
        display.present_rects(&[0u8; 32], &[DamageRect::full(&MODE)]),
        Ok(())
    );
    assert_eq!(display.surface.presents.get(), 1);
    assert_eq!(display.surface.region_presents.get(), 1);
}

#[test]
fn an_unreachable_mailbox_service_is_a_refused_switch() {
    let mut display = FirmwareDisplay::new(Surface::default(), &Unreachable);
    assert_eq!(
        display.set_power(DisplayPower::Off),
        Err(DriverError::NotFound)
    );
}

/// A switch the firmware did not make must not read as made: the session
/// would believe the display off and leave a lit, frozen screen behind it.
#[test]
fn a_firmware_that_did_not_switch_is_a_refused_switch() {
    let stuck = Stuck(RefCell::new(MockFirmware::healthy()));
    let mut display = FirmwareDisplay::new(Surface::default(), &stuck);
    assert_eq!(
        display.set_power(DisplayPower::Off),
        Err(DriverError::DeviceFault)
    );
}

#[test]
fn the_bind_table_matches_the_surface_the_port_publishes() {
    let published = HwMatchKey::compatible(FIRMWARE_FRAMEBUFFER_COMPATIBLE).expect("fits");
    assert_eq!(BIND_KEYS.len(), 1);
    assert!(BIND_KEYS[0].key.matches(&published));
    let generic = HwMatchKey::compatible(b"simple-framebuffer").expect("fits");
    assert!(
        !BIND_KEYS[0].key.matches(&generic),
        "a surface without the firmware binding is not this driver's"
    );
}
