//! The usages the decoders read (HID Usage Tables 1.4).

use crate::descriptor::Usage;

/// Generic Desktop page.
pub const PAGE_GENERIC_DESKTOP: u16 = 0x01;
/// Keyboard/Keypad page.
pub const PAGE_KEYBOARD: u16 = 0x07;
/// Button page: usage `n` is button `n`, 1 the primary.
pub const PAGE_BUTTON: u16 = 0x09;
/// Consumer page.
pub const PAGE_CONSUMER: u16 = 0x0C;
/// Digitizers page.
pub const PAGE_DIGITIZER: u16 = 0x0D;

/// A pointing device's application.
pub const POINTER: Usage = Usage::new(PAGE_GENERIC_DESKTOP, 0x01);
/// A mouse's application.
pub const MOUSE: Usage = Usage::new(PAGE_GENERIC_DESKTOP, 0x02);
/// A keyboard's application.
pub const KEYBOARD: Usage = Usage::new(PAGE_GENERIC_DESKTOP, 0x06);
/// A keypad's application.
pub const KEYPAD: Usage = Usage::new(PAGE_GENERIC_DESKTOP, 0x07);
/// The X axis.
pub const X: Usage = Usage::new(PAGE_GENERIC_DESKTOP, 0x30);
/// The Y axis.
pub const Y: Usage = Usage::new(PAGE_GENERIC_DESKTOP, 0x31);
/// The vertical wheel, counting away from the user.
pub const WHEEL: Usage = Usage::new(PAGE_GENERIC_DESKTOP, 0x38);
/// The feature setting how many counts a wheel reports a detent.
pub const RESOLUTION_MULTIPLIER: Usage = Usage::new(PAGE_GENERIC_DESKTOP, 0x48);
/// The horizontal (tilt) wheel, counting rightward.
pub const AC_PAN: Usage = Usage::new(PAGE_CONSUMER, 0x238);

/// A touch screen's application.
pub const TOUCH_SCREEN: Usage = Usage::new(PAGE_DIGITIZER, 0x04);
/// A touch pad's application.
pub const TOUCH_PAD: Usage = Usage::new(PAGE_DIGITIZER, 0x05);
/// The application holding a digitizer's mode features.
pub const DEVICE_CONFIGURATION: Usage = Usage::new(PAGE_DIGITIZER, 0x0E);
/// One contact's logical collection.
pub const FINGER: Usage = Usage::new(PAGE_DIGITIZER, 0x22);
/// A contact touching the surface.
pub const TIP_SWITCH: Usage = Usage::new(PAGE_DIGITIZER, 0x42);
/// The device's belief a contact is intentional; zero for a palm.
pub const CONFIDENCE: Usage = Usage::new(PAGE_DIGITIZER, 0x47);
/// A contact's identity while it touches.
pub const CONTACT_IDENTIFIER: Usage = Usage::new(PAGE_DIGITIZER, 0x51);
/// The feature choosing what a digitizer reports.
pub const INPUT_MODE: Usage = Usage::new(PAGE_DIGITIZER, 0x52);
/// How many contacts a frame carries.
pub const CONTACT_COUNT: Usage = Usage::new(PAGE_DIGITIZER, 0x54);
/// The most contacts a frame may carry.
pub const CONTACT_COUNT_MAXIMUM: Usage = Usage::new(PAGE_DIGITIZER, 0x55);
/// The feature switching contact reports on.
pub const SURFACE_SWITCH: Usage = Usage::new(PAGE_DIGITIZER, 0x57);
/// The feature switching button reports on.
pub const BUTTON_SWITCH: Usage = Usage::new(PAGE_DIGITIZER, 0x58);
/// The feature stating whether the pad itself is a button.
pub const PAD_TYPE: Usage = Usage::new(PAGE_DIGITIZER, 0x59);

/// The keyboard usage an array reports when more keys are down than it holds.
pub const KEY_ERROR_ROLL_OVER: u16 = 0x01;
/// The first keyboard modifier usage, Left Control; the eight end at Right GUI.
pub const MODIFIER_FIRST: u16 = 0xE0;
/// The last keyboard modifier usage.
pub const MODIFIER_LAST: u16 = 0xE7;

/// [`INPUT_MODE`] for a touchscreen reporting contacts.
pub const INPUT_MODE_TOUCHSCREEN: u32 = 2;
/// [`INPUT_MODE`] for a touchpad reporting contacts.
pub const INPUT_MODE_TOUCHPAD: u32 = 3;
/// [`PAD_TYPE`] of a pad that is itself pressed: a clickpad.
pub const PAD_TYPE_DEPRESSIBLE: i64 = 0;
