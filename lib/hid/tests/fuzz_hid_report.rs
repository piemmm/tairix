//! Deterministic fuzz harness for `lib/hid`: the report-descriptor model,
//! the decoders every report passes through, and the configuration exchange.
//!
//! Each is held to what it must do:
//!
//! * no input panics, and a descriptor declaring `2^32 - 1` fields is
//!   refused in the time its bytes take to walk;
//! * an accepted model places every field inside its own report, after its
//!   report ID when it has one, under a collection that exists;
//! * a long item, or global items a Push and Pop enclose, change nothing;
//! * a report an application finds malformed delivers nothing;
//! * a pointer button is never pressed twice or released unheld, and a
//!   device let go of leaves no button and no contact held;
//! * configuring a device fails only when the device has gone.
//!
//! A per-run-seeded `Prng` mutates real descriptors (boot layouts, report-ID
//! keyboards and mice, a wireless receiver's two interfaces, a high-resolution
//! mouse, a Precision Touchpad, a touchscreen, and the forged shapes the
//! parser must refuse), assembles descriptors from random items, and feeds
//! noise. A plain `cargo test` runs the [`SMOKE_ITERATIONS`] sweep once from a
//! fresh, logged seed; `cargo xtask fuzz` extends it to a wall-clock budget.

use tairix_abi::driver::input::{InputEvent, InputEventKind};
use tairix_abi::input::{KeyInput, PointerButtonCode, PointerInput};
use tairix_abi::touch::{TouchFrame, TOUCH_CONTACTS_MAX};
use tairix_abi::DriverError;
use tairix_fuzzseed::Prng;
use tairix_hid::descriptor::{DescriptorError, ReportKind};
use tairix_hid::{
    boot, Decoded, HidDevice, HidTransport, KeyboardConsole, ReportDescriptor, ReportId, SeatSink,
    MAX_DESCRIPTOR,
};

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_ITERATIONS: u64 = 20_000;

/// A boot mouse (USB HID 1.11 Appendix E.10).
const BOOT_MOUSE: &[u8] = &[
    0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0x09, 0x01, 0xA1, 0x00, 0x05, 0x09, 0x19, 0x01, 0x29, 0x03,
    0x15, 0x00, 0x25, 0x01, 0x95, 0x03, 0x75, 0x01, 0x81, 0x02, 0x95, 0x01, 0x75, 0x05, 0x81, 0x01,
    0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x15, 0x81, 0x25, 0x7F, 0x75, 0x08, 0x95, 0x02, 0x81, 0x06,
    0xC0, 0xC0,
];

/// A boot mouse with a wheel.
const WHEEL_MOUSE: &[u8] = &[
    0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0x09, 0x01, 0xA1, 0x00, 0x05, 0x09, 0x19, 0x01, 0x29, 0x03,
    0x15, 0x00, 0x25, 0x01, 0x95, 0x03, 0x75, 0x01, 0x81, 0x02, 0x95, 0x01, 0x75, 0x05, 0x81, 0x01,
    0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x09, 0x38, 0x15, 0x81, 0x25, 0x7F, 0x75, 0x08, 0x95, 0x03,
    0x81, 0x06, 0xC0, 0xC0,
];

/// A boot keyboard (USB HID 1.11 Appendix E.6), with its LED output report.
const BOOT_KEYBOARD: &[u8] = &[
    0x05, 0x01, 0x09, 0x06, 0xA1, 0x01, 0x05, 0x07, 0x19, 0xE0, 0x29, 0xE7, 0x15, 0x00, 0x25, 0x01,
    0x75, 0x01, 0x95, 0x08, 0x81, 0x02, 0x95, 0x01, 0x75, 0x08, 0x81, 0x01, 0x95, 0x05, 0x75, 0x01,
    0x05, 0x08, 0x19, 0x01, 0x29, 0x05, 0x91, 0x02, 0x95, 0x01, 0x75, 0x03, 0x91, 0x01, 0x95, 0x06,
    0x75, 0x08, 0x15, 0x00, 0x25, 0x65, 0x05, 0x07, 0x19, 0x00, 0x29, 0x65, 0x81, 0x00, 0xC0,
];

/// A keyboard whose reports carry Report ID 1.
const REPORT_ID_KEYBOARD: &[u8] = &[
    0x05, 0x01, 0x09, 0x06, 0xA1, 0x01, 0x85, 0x01, 0x05, 0x07, 0x19, 0xE0, 0x29, 0xE7, 0x15, 0x00,
    0x25, 0x01, 0x75, 0x01, 0x95, 0x08, 0x81, 0x02, 0x95, 0x01, 0x75, 0x08, 0x81, 0x01, 0x95, 0x06,
    0x75, 0x08, 0x15, 0x00, 0x25, 0x65, 0x05, 0x07, 0x19, 0x00, 0x29, 0x65, 0x81, 0x00, 0xC0,
];

/// A mouse whose reports carry Report ID 2.
const REPORT_ID_MOUSE: &[u8] = &[
    0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0x85, 0x02, 0x09, 0x01, 0xA1, 0x00, 0x05, 0x09, 0x19, 0x01,
    0x29, 0x03, 0x15, 0x00, 0x25, 0x01, 0x95, 0x03, 0x75, 0x01, 0x81, 0x02, 0x95, 0x01, 0x75, 0x05,
    0x81, 0x01, 0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x15, 0x81, 0x25, 0x7F, 0x75, 0x08, 0x95, 0x02,
    0x81, 0x06, 0xC0, 0xC0,
];

/// A mouse with 12-bit axes packed across byte boundaries.
const TWELVE_BIT_MOUSE: &[u8] = &[
    0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0x09, 0x01, 0xA1, 0x00, 0x05, 0x09, 0x19, 0x01, 0x29, 0x03,
    0x15, 0x00, 0x25, 0x01, 0x95, 0x03, 0x75, 0x01, 0x81, 0x02, 0x95, 0x01, 0x75, 0x05, 0x81, 0x01,
    0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x16, 0x01, 0xF8, 0x26, 0xFF, 0x07, 0x75, 0x0C, 0x95, 0x02,
    0x81, 0x06, 0xC0, 0xC0,
];

/// A wireless receiver's keyboard interface: the keyboard (Report ID 1), a
/// consumer-control collection (3), and system control (4).
const RECEIVER_KEYBOARD: &[u8] = &[
    0x05, 0x01, 0x09, 0x06, 0xA1, 0x01, 0x85, 0x01, 0x95, 0x08, 0x75, 0x01, 0x15, 0x00, 0x25, 0x01,
    0x05, 0x07, 0x19, 0xE0, 0x29, 0xE7, 0x81, 0x02, 0x95, 0x01, 0x75, 0x08, 0x81, 0x03, 0x95, 0x05,
    0x75, 0x01, 0x05, 0x08, 0x19, 0x01, 0x29, 0x05, 0x91, 0x02, 0x95, 0x01, 0x75, 0x03, 0x91, 0x03,
    0x95, 0x06, 0x75, 0x08, 0x15, 0x00, 0x26, 0xFF, 0x00, 0x05, 0x07, 0x19, 0x00, 0x2A, 0xFF, 0x00,
    0x81, 0x00, 0xC0, 0x05, 0x0C, 0x09, 0x01, 0xA1, 0x01, 0x85, 0x03, 0x75, 0x10, 0x95, 0x02, 0x15,
    0x01, 0x26, 0xFF, 0x02, 0x19, 0x01, 0x2A, 0xFF, 0x02, 0x81, 0x00, 0xC0, 0x05, 0x01, 0x09, 0x80,
    0xA1, 0x01, 0x85, 0x04, 0x75, 0x02, 0x95, 0x01, 0x15, 0x01, 0x25, 0x03, 0x09, 0x82, 0x09, 0x81,
    0x09, 0x83, 0x81, 0x60, 0x75, 0x06, 0x81, 0x03, 0xC0,
];

/// The receiver's mouse interface: sixteen buttons, 12-bit axes, a wheel and
/// AC Pan (Report ID 2), then two vendor reports (0x10, 0x11).
const RECEIVER_MOUSE: &[u8] = &[
    0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0x85, 0x02, 0x09, 0x01, 0xA1, 0x00, 0x05, 0x09, 0x19, 0x01,
    0x29, 0x10, 0x15, 0x00, 0x25, 0x01, 0x95, 0x10, 0x75, 0x01, 0x81, 0x02, 0x05, 0x01, 0x16, 0x01,
    0xF8, 0x26, 0xFF, 0x07, 0x75, 0x0C, 0x95, 0x02, 0x09, 0x30, 0x09, 0x31, 0x81, 0x06, 0x15, 0x81,
    0x25, 0x7F, 0x75, 0x08, 0x95, 0x01, 0x09, 0x38, 0x81, 0x06, 0x05, 0x0C, 0x0A, 0x38, 0x02, 0x95,
    0x01, 0x81, 0x06, 0xC0, 0xC0, 0x06, 0x00, 0xFF, 0x09, 0x01, 0xA1, 0x01, 0x85, 0x10, 0x75, 0x08,
    0x95, 0x06, 0x15, 0x00, 0x26, 0xFF, 0x00, 0x09, 0x01, 0x81, 0x00, 0x09, 0x01, 0x91, 0x00, 0xC0,
    0x06, 0x00, 0xFF, 0x09, 0x02, 0xA1, 0x01, 0x85, 0x11, 0x75, 0x08, 0x95, 0x13, 0x15, 0x00, 0x26,
    0xFF, 0x00, 0x09, 0x02, 0x81, 0x00, 0x09, 0x02, 0x91, 0x00, 0xC0,
];

/// A high-resolution pointer: 16-bit axes, the wheel and AC Pan each in a
/// logical collection with its own Resolution Multiplier (physical 1..16),
/// both multipliers in feature report 2.
const HI_RES_MOUSE: &[u8] = &[
    0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0x85, 0x01, 0x09, 0x01, 0xA1, 0x00, 0x05, 0x09, 0x19, 0x01,
    0x29, 0x03, 0x15, 0x00, 0x25, 0x01, 0x95, 0x03, 0x75, 0x01, 0x81, 0x02, 0x95, 0x01, 0x75, 0x05,
    0x81, 0x01, 0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x16, 0x01, 0x80, 0x26, 0xFF, 0x7F, 0x75, 0x10,
    0x95, 0x02, 0x81, 0x06, 0xA1, 0x02, 0x85, 0x02, 0x09, 0x48, 0x15, 0x00, 0x25, 0x01, 0x35, 0x01,
    0x45, 0x10, 0x75, 0x02, 0x95, 0x01, 0xB1, 0x02, 0x85, 0x01, 0x09, 0x38, 0x35, 0x00, 0x45, 0x00,
    0x15, 0x81, 0x25, 0x7F, 0x75, 0x08, 0x95, 0x01, 0x81, 0x06, 0xC0, 0xA1, 0x02, 0x85, 0x02, 0x09,
    0x48, 0x15, 0x00, 0x25, 0x01, 0x35, 0x01, 0x45, 0x10, 0x75, 0x02, 0x95, 0x01, 0xB1, 0x02, 0x35,
    0x00, 0x45, 0x00, 0x75, 0x04, 0xB1, 0x01, 0x85, 0x01, 0x05, 0x0C, 0x0A, 0x38, 0x02, 0x15, 0x81,
    0x25, 0x7F, 0x75, 0x08, 0x95, 0x01, 0x81, 0x06, 0xC0, 0xC0, 0xC0,
];

/// Forged shapes the parser once got wrong: pointer fields placed before
/// the first Report ID; buttons and X in report 1 with report 2's Y; report
/// 1 re-entered after report 2; a Report ID saved by Push; axes of
/// `2^32 - 1` fields; four modifier flags before padding.
const FORGED: [&[u8]; 6] = [
    &[
        0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0x05, 0x09, 0x19, 0x01, 0x29, 0x03, 0x15, 0x00, 0x25,
        0x01, 0x95, 0x03, 0x75, 0x01, 0x81, 0x02, 0x95, 0x01, 0x75, 0x05, 0x81, 0x03, 0x05, 0x01,
        0x09, 0x30, 0x09, 0x31, 0x15, 0x81, 0x25, 0x7F, 0x95, 0x02, 0x75, 0x08, 0x81, 0x06, 0xC0,
        0x05, 0x0C, 0x09, 0x01, 0xA1, 0x01, 0x85, 0x02, 0x19, 0x00, 0x2A, 0xFF, 0x00, 0x15, 0x00,
        0x26, 0xFF, 0x00, 0x95, 0x01, 0x75, 0x08, 0x81, 0x00, 0xC0,
    ],
    &[
        0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0x85, 0x01, 0x05, 0x09, 0x19, 0x01, 0x29, 0x08, 0x95,
        0x08, 0x75, 0x01, 0x81, 0x02, 0x05, 0x01, 0x09, 0x30, 0x95, 0x01, 0x75, 0x08, 0x81, 0x06,
        0xC0, 0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0x85, 0x02, 0x05, 0x01, 0x09, 0x31, 0x95, 0x01,
        0x75, 0x08, 0x81, 0x06, 0xC0,
    ],
    &[
        0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0x85, 0x01, 0x05, 0x09, 0x19, 0x01, 0x29, 0x03, 0x95,
        0x03, 0x75, 0x01, 0x81, 0x02, 0x95, 0x05, 0x81, 0x01, 0x85, 0x02, 0x95, 0x01, 0x75, 0x08,
        0x81, 0x01, 0x85, 0x01, 0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x95, 0x02, 0x75, 0x08, 0x81,
        0x06, 0xC0,
    ],
    &[
        0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0x85, 0x01, 0x05, 0x09, 0x19, 0x01, 0x29, 0x08, 0x95,
        0x08, 0x75, 0x01, 0x81, 0x02, 0xA4, 0x85, 0x02, 0x95, 0x01, 0x75, 0x08, 0x81, 0x01, 0xB4,
        0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x95, 0x02, 0x75, 0x08, 0x81, 0x06, 0xC0,
    ],
    &[
        0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0x05, 0x01, 0x19, 0x30, 0x29, 0x38, 0x75, 0x08, 0x97,
        0xFF, 0xFF, 0xFF, 0xFF, 0x81, 0x06, 0xC0,
    ],
    &[
        0x05, 0x01, 0x09, 0x06, 0xA1, 0x01, 0x05, 0x07, 0x19, 0xE0, 0x29, 0xE7, 0x75, 0x01, 0x95,
        0x04, 0x81, 0x02, 0x95, 0x04, 0x81, 0x03, 0x95, 0x06, 0x75, 0x08, 0x19, 0x00, 0x29, 0x65,
        0x81, 0x00, 0xC0,
    ],
];

/// A Precision Touchpad: two Finger collections (confidence, tip, a contact
/// id, 12-bit X and Y over physical centimetres), the contact count and a
/// button under report 1; Contact Count Maximum and Pad Type in feature 2; a
/// Device Configuration with Input Mode in feature 3.
const PRECISION_TOUCHPAD: &[u8] = &[
    0x05, 0x0D, 0x09, 0x05, 0xA1, 0x01, 0x85, 0x01, 0x09, 0x22, 0xA1, 0x02, 0x15, 0x00, 0x25, 0x01,
    0x75, 0x01, 0x95, 0x01, 0x09, 0x47, 0x81, 0x02, 0x09, 0x42, 0x81, 0x02, 0x95, 0x06, 0x81, 0x03,
    0x75, 0x08, 0x95, 0x01, 0x25, 0x05, 0x09, 0x51, 0x81, 0x02, 0x05, 0x01, 0x26, 0xFF, 0x0F, 0x46,
    0xE8, 0x03, 0x65, 0x11, 0x55, 0x0E, 0x75, 0x10, 0x09, 0x30, 0x81, 0x02, 0x46, 0xF4, 0x01, 0x09,
    0x31, 0x81, 0x02, 0xC0, 0x05, 0x0D, 0x09, 0x22, 0xA1, 0x02, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01,
    0x95, 0x01, 0x09, 0x47, 0x81, 0x02, 0x09, 0x42, 0x81, 0x02, 0x95, 0x06, 0x81, 0x03, 0x75, 0x08,
    0x95, 0x01, 0x25, 0x05, 0x09, 0x51, 0x81, 0x02, 0x05, 0x01, 0x26, 0xFF, 0x0F, 0x46, 0xE8, 0x03,
    0x75, 0x10, 0x09, 0x30, 0x81, 0x02, 0x46, 0xF4, 0x01, 0x09, 0x31, 0x81, 0x02, 0xC0, 0x05, 0x0D,
    0x15, 0x00, 0x25, 0x05, 0x75, 0x08, 0x95, 0x01, 0x09, 0x54, 0x81, 0x02, 0x05, 0x09, 0x09, 0x01,
    0x25, 0x01, 0x75, 0x01, 0x81, 0x02, 0x95, 0x07, 0x81, 0x03, 0x05, 0x0D, 0x85, 0x02, 0x75, 0x08,
    0x95, 0x01, 0x25, 0x05, 0x09, 0x55, 0xB1, 0x02, 0x25, 0x02, 0x09, 0x59, 0xB1, 0x02, 0xC0, 0x09,
    0x0E, 0xA1, 0x01, 0x85, 0x03, 0x25, 0x0A, 0x09, 0x52, 0xB1, 0x02, 0xC0,
];

/// A one-finger touchscreen with In Range beside the tip switch.
const TOUCHSCREEN: &[u8] = &[
    0x05, 0x0D, 0x09, 0x04, 0xA1, 0x01, 0x85, 0x04, 0x09, 0x22, 0xA1, 0x02, 0x15, 0x00, 0x25, 0x01,
    0x75, 0x01, 0x95, 0x01, 0x09, 0x42, 0x81, 0x02, 0x09, 0x32, 0x81, 0x02, 0x95, 0x06, 0x81, 0x03,
    0x75, 0x08, 0x95, 0x01, 0x25, 0x0A, 0x09, 0x51, 0x81, 0x02, 0x05, 0x01, 0x26, 0xFF, 0x7F, 0x75,
    0x10, 0x09, 0x30, 0x81, 0x02, 0x09, 0x31, 0x81, 0x02, 0xC0, 0x05, 0x0D, 0x25, 0x0A, 0x75, 0x08,
    0x09, 0x54, 0x81, 0x02, 0xC0,
];

/// The descriptors the harness mutates.
fn descriptor_seeds() -> Vec<&'static [u8]> {
    let mut seeds = vec![
        BOOT_MOUSE,
        WHEEL_MOUSE,
        BOOT_KEYBOARD,
        REPORT_ID_KEYBOARD,
        REPORT_ID_MOUSE,
        TWELVE_BIT_MOUSE,
        RECEIVER_KEYBOARD,
        RECEIVER_MOUSE,
        HI_RES_MOUSE,
        PRECISION_TOUCHPAD,
        TOUCHSCREEN,
    ];
    seeds.extend(FORGED);
    seeds
}

/// How many of [`descriptor_seeds`] are real devices' descriptors, ahead of
/// the forged ones.
const REAL_SEEDS: usize = 11;

/// Item types (`bType`).
const MAIN: u8 = 0;
const GLOBAL: u8 = 1;
const LOCAL: u8 = 2;

/// Data bytes a short item's two-bit size field selects.
const DATA_LEN: [usize; 4] = [0, 1, 2, 4];

/// A short item of `tag` and `kind` carrying `data` in the fewest bytes
/// that hold it, or in all four when `wide`.
fn item(tag: u8, kind: u8, data: u32, wide: bool) -> Vec<u8> {
    let size: u8 = if wide || data > 0xFFFF {
        3
    } else if data > 0xFF {
        2
    } else {
        u8::from(data != 0)
    };
    let mut item = vec![(tag << 4) | (kind << 2) | size];
    item.extend_from_slice(&data.to_le_bytes()[..DATA_LEN[usize::from(size)]]);
    item
}

/// A long item (prefix `0xFE`): its data size, its tag, then the data.
fn long_item(rng: &mut Prng) -> Vec<u8> {
    let mut data = vec![0u8; rng.at_most(6)];
    rng.fill(&mut data);
    let mut item = vec![0xFE, u8::try_from(data.len()).unwrap_or(0), rng.next_u8()];
    item.extend_from_slice(&data);
    item
}

/// One of `values`, or now and then any value at all.
fn value(values: &[u32], rng: &mut Prng) -> u32 {
    if rng.below(6) == 0 {
        rng.next_u32()
    } else {
        *rng.pick(values)
    }
}

/// One whole item of a descriptor, drawn from what the parser interprets
/// and what it must pass over.
fn random_item(rng: &mut Prng) -> Vec<u8> {
    let wide = rng.below(8) == 0;
    match rng.below(16) {
        0 => item(
            0x0,
            GLOBAL,
            value(&[0x01, 0x07, 0x09, 0x0C, 0x0D, 0xFF00], rng),
            wide,
        ),
        1 => item(
            0x7,
            GLOBAL,
            value(&[0, 1, 3, 5, 8, 12, 16, 32, 33, 64], rng),
            wide,
        ),
        2 => item(
            0x9,
            GLOBAL,
            value(&[0, 1, 2, 3, 6, 8, 16, 40, 255, 256, 0xFFFF_FFFF], rng),
            wide,
        ),
        3 => item(0x8, GLOBAL, value(&[0, 1, 2, 3, 0x10], rng), wide),
        4 => item(
            0x0,
            LOCAL,
            value(
                &[
                    0x01, 0x02, 0x04, 0x05, 0x06, 0x22, 0x30, 0x31, 0x38, 0x42, 0x47, 0x48, 0x51,
                    0x54, 0xE0, 0xE7,
                ],
                rng,
            ),
            wide,
        ),
        5 => item(
            0x1,
            LOCAL,
            value(&[0x00, 0x01, 0x2E, 0x30, 0xE0], rng),
            wide,
        ),
        6 => item(
            0x2,
            LOCAL,
            value(&[0x03, 0x08, 0x10, 0x31, 0x38, 0x65, 0xE7, 0xFF], rng),
            wide,
        ),
        7..=9 => item(0x8, MAIN, value(&[0x00, 0x01, 0x02, 0x03, 0x06], rng), wide),
        10 => item(0xA, MAIN, value(&[0x00, 0x01, 0x02], rng), wide),
        11 => item(0xC, MAIN, 0, false),
        12 => item(*rng.pick(&[0x9, 0xB]), MAIN, 0x02, wide),
        13 => item(*rng.pick(&[0xA, 0xB]), GLOBAL, 0, false),
        14 => long_item(rng),
        _ => {
            // Any short item at all, reserved tags and types included.
            let prefix = match rng.next_u8() {
                0xFE => 0xFF,
                prefix => prefix,
            };
            let mut item = vec![prefix; 1 + DATA_LEN[usize::from(prefix & 0x03)]];
            rng.fill(&mut item[1..]);
            item
        }
    }
}

/// A descriptor of random items, kept as its items so an item boundary is
/// known.
fn random_items(rng: &mut Prng) -> Vec<Vec<u8>> {
    (0..rng.at_most(48)).map(|_| random_item(rng)).collect()
}

/// Every item of `desc` as its type and tag, a long item as the reserved
/// type, up to any truncated item.
fn item_kinds(desc: &[u8]) -> Vec<(u8, u8)> {
    let mut kinds = Vec::new();
    let mut at = 0;
    while let Some(&prefix) = desc.get(at) {
        if prefix == 0xFE {
            let Some(&len) = desc.get(at + 1) else {
                break;
            };
            kinds.push((3, 0xF));
            at += 3 + usize::from(len);
            continue;
        }
        kinds.push(((prefix >> 2) & 0x03, prefix >> 4));
        at += 1 + DATA_LEN[usize::from(prefix & 0x03)];
    }
    kinds
}

/// A report ID the parser takes: one byte, never zero.
fn report_id(rng: &mut Prng) -> u32 {
    u32::from(rng.next_u8().max(1))
}

/// A `Push`, valid global items — a Report ID among them — and the `Pop`
/// that restores what they changed.
fn shielded_globals(rng: &mut Prng) -> Vec<Vec<u8>> {
    let mut items = vec![
        item(0xA, GLOBAL, 0, false),
        item(0x8, GLOBAL, report_id(rng), false),
    ];
    for _ in 0..rng.at_most(3) {
        let global = match rng.below(4) {
            0 => item(0x0, GLOBAL, u32::from(rng.next_u16()), false),
            1 => item(0x7, GLOBAL, u32::from(rng.next_u16()), false),
            2 => item(0x9, GLOBAL, u32::from(rng.next_u16()), false),
            _ => item(0x8, GLOBAL, report_id(rng), false),
        };
        items.push(global);
    }
    items.push(item(0xB, GLOBAL, 0, false));
    items
}

/// `template` with a handful of bytes flipped, then cut short or extended.
fn mutate(template: &[u8], rng: &mut Prng) -> Vec<u8> {
    let mut bytes = template.to_vec();
    for _ in 0..rng.at_most(6) {
        if bytes.is_empty() {
            break;
        }
        let at = rng.below(bytes.len());
        bytes[at] ^= rng.next_u8();
    }
    match rng.below(3) {
        0 => bytes.truncate(rng.at_most(bytes.len())),
        1 => bytes.extend((0..rng.at_most(12)).map(|_| rng.next_u8())),
        _ => {}
    }
    bytes
}

/// Check what an accepted model must hold.
fn check_model(model: &ReportDescriptor) {
    for (index, collection) in model.collections().iter().enumerate() {
        if let Some(parent) = collection.parent {
            assert!(
                parent.get() < index,
                "collection {index} names a later parent"
            );
        }
    }
    for field in model.fields() {
        assert!(
            field.size >= 1 && field.count >= 1,
            "an empty field: {field:?}"
        );
        if let Some(id) = field.report.id() {
            assert_ne!(id, 0, "report id zero");
            assert!(field.offset >= 8, "a field over its report id: {field:?}");
        }
        let len = model
            .report_len(field.kind, field.report)
            .expect("its report has a length");
        let end = u64::from(field.offset) + u64::from(field.size) * u64::from(field.count);
        assert!(
            end <= 8 * len as u64,
            "a field past its report: {field:?}, {len} bytes"
        );
        if let Some(collection) = field.collection {
            assert!(collection.get() < model.collections().len());
        }
        assert!(
            !model.usages(field).is_empty(),
            "a field naming no usage: {field:?}"
        );
        if model.uses_report_ids()
            && !field
                .flags
                .contains(tairix_hid::descriptor::FieldFlags::CONSTANT)
        {
            assert!(
                field.report.id().is_some(),
                "an undemuxable field: {field:?}"
            );
        }
        for element in 0..field.count.min(8) {
            let _ = model.element_usage(field, element);
        }
    }
}

/// A descriptor drawn from the seeds, from random items, or from noise.
fn random_descriptor(rng: &mut Prng, seeds: &[&[u8]]) -> Vec<u8> {
    match rng.below(4) {
        0 | 1 => mutate(rng.pick(seeds), rng),
        2 => random_items(rng).concat(),
        _ => {
            let mut noise = vec![0u8; rng.at_most(96)];
            rng.fill(&mut noise);
            noise
        }
    }
}

#[test]
fn an_accepted_model_keeps_every_field_inside_its_report() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "an_accepted_model_keeps_every_field_inside_its_report",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let seeds = descriptor_seeds();
    for seed in &seeds[..REAL_SEEDS] {
        check_model(&ReportDescriptor::parse(seed).expect("a real descriptor parses"));
    }
    let mut iteration: u64 = 0;
    loop {
        let desc = random_descriptor(&mut rng, &seeds);
        let parsed = ReportDescriptor::parse(&desc);
        if desc.is_empty() || desc.len() > MAX_DESCRIPTOR {
            assert_eq!(parsed, Err(DescriptorError::Length));
        }
        if let Ok(model) = parsed {
            check_model(&model);
        }
        iteration += 1;
        if !tairix_fuzzseed::within_budget(deadline) && iteration >= SMOKE_ITERATIONS {
            break;
        }
    }
}

#[test]
fn a_long_item_or_globals_a_push_and_pop_enclose_change_nothing() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "a_long_item_or_globals_a_push_and_pop_enclose_change_nothing",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut iteration: u64 = 0;
    loop {
        let items = random_items(&mut rng);
        let original = items.concat();
        // Where the global stack is shallow enough to take one more push.
        let mut depth = 0usize;
        let mut room = vec![0];
        for (at, kind) in item_kinds(&original).into_iter().enumerate() {
            match kind {
                (GLOBAL, 0xA) => depth += 1,
                (GLOBAL, 0xB) => depth = depth.saturating_sub(1),
                _ => {}
            }
            if depth < 7 {
                room.push(at + 1);
            }
        }
        let at = *rng.pick(&room);
        // An empty descriptor is refused for its length, which an insertion changes.
        if !items.is_empty() {
            let insert = if rng.below(2) == 0 {
                vec![long_item(&mut rng)]
            } else {
                shielded_globals(&mut rng)
            };
            let mut altered = items[..at].to_vec();
            altered.extend(insert);
            altered.extend_from_slice(&items[at..]);
            let altered = altered.concat();
            assert_eq!(
                ReportDescriptor::parse(&original).ok(),
                ReportDescriptor::parse(&altered).ok(),
                "{original:02x?} became {altered:02x?}"
            );
        }
        iteration += 1;
        if !tairix_fuzzseed::within_budget(deadline) && iteration >= SMOKE_ITERATIONS {
            break;
        }
    }
}

/// Every record a device delivered, with the button state it implies.
#[derive(Default)]
struct Seat {
    held: [bool; 3],
    last_frame: Option<TouchFrame>,
    records: usize,
}

impl Seat {
    fn button(code: PointerButtonCode) -> usize {
        match code {
            PointerButtonCode::Primary => 0,
            PointerButtonCode::Secondary => 1,
            PointerButtonCode::Middle => 2,
        }
    }
}

impl SeatSink for Seat {
    fn key(&mut self, record: &KeyInput) -> Result<(), DriverError> {
        assert_eq!(KeyInput::from_bytes(&record.to_le_bytes()), Ok(*record));
        self.records += 1;
        Ok(())
    }

    fn pointer(&mut self, record: &PointerInput) -> Result<(), DriverError> {
        match record {
            PointerInput::Pressed(code) => {
                let held = &mut self.held[Self::button(*code)];
                assert!(!*held, "{code:?} pressed twice");
                *held = true;
            }
            PointerInput::Released(code) => {
                let held = &mut self.held[Self::button(*code)];
                assert!(*held, "{code:?} released unheld");
                *held = false;
            }
            _ => {}
        }
        self.records += 1;
        Ok(())
    }

    fn touch(&mut self, frame: &TouchFrame) -> Result<(), DriverError> {
        assert!(frame.contacts().len() <= TOUCH_CONTACTS_MAX);
        assert_eq!(TouchFrame::from_bytes(&frame.to_le_bytes()), Ok(*frame));
        self.last_frame = Some(*frame);
        self.records += 1;
        Ok(())
    }
}

/// A device's feature exchange answered at random: noise, short answers,
/// refusals, and now and then the device gone.
struct RandomTransport<'a> {
    rng: &'a mut Prng,
    gone: bool,
}

impl RandomTransport<'_> {
    fn refusal(&mut self) -> Option<DriverError> {
        match self.rng.below(8) {
            0 => {
                self.gone = true;
                Some(DriverError::NotFound)
            }
            1 | 2 => Some(DriverError::Unsupported),
            _ => None,
        }
    }
}

impl HidTransport for RandomTransport<'_> {
    fn get_feature(&mut self, _id: ReportId, report: &mut [u8]) -> Result<usize, DriverError> {
        if let Some(error) = self.refusal() {
            return Err(error);
        }
        let len = self.rng.at_most(report.len());
        self.rng.fill(&mut report[..len]);
        Ok(len)
    }

    fn set_feature(&mut self, _id: ReportId, _report: &[u8]) -> Result<(), DriverError> {
        self.refusal().map_or(Ok(()), Err)
    }
}

/// A report for `model`'s device: its own report ID most of the time.
fn random_report(model: &ReportDescriptor, rng: &mut Prng) -> Vec<u8> {
    let len = model.longest_report(ReportKind::Input) + 2;
    let mut report = vec![0u8; rng.at_most(len)];
    rng.fill(&mut report);
    if let (Some(first), Some(field)) = (
        report.first_mut(),
        model.fields().get(rng.below(model.fields().len().max(1))),
    ) {
        if let (Some(id), true) = (field.report.id(), rng.below(4) != 0) {
            *first = id;
        }
    }
    report
}

#[test]
fn a_device_delivers_nothing_from_a_malformed_report_and_holds_nothing_once_let_go() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "a_device_delivers_nothing_from_a_malformed_report_and_holds_nothing_once_let_go",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let seeds = descriptor_seeds();
    let mut iteration: u64 = 0;
    loop {
        let desc = if rng.below(2) == 0 {
            rng.pick(&seeds[..REAL_SEEDS]).to_vec()
        } else {
            random_descriptor(&mut rng, &seeds)
        };
        if let Some(mut device) = ReportDescriptor::parse(&desc).ok().and_then(HidDevice::new) {
            let mut transport = RandomTransport {
                rng: &mut rng,
                gone: false,
            };
            let configured = device.configure(&mut transport);
            if configured.is_err() {
                assert!(
                    transport.gone,
                    "configuration failed with the device there: {configured:?}"
                );
                assert_eq!(configured, Err(DriverError::NotFound));
            }
            let mut seat = Seat::default();
            for _ in 0..rng.at_most(12) {
                let report = random_report(device.model(), &mut rng);
                let before = seat.records;
                let decoded = device
                    .input(&report, &mut seat)
                    .expect("the seat takes every record");
                if decoded == Decoded::Malformed {
                    assert_eq!(seat.records, before, "a malformed report delivered");
                }
            }
            device.release(&mut seat).expect("released");
            assert_eq!(seat.held, [false; 3], "a button left held");
            if let Some(frame) = seat.last_frame {
                assert!(
                    frame.contacts().is_empty() && frame.buttons().bits() == 0,
                    "a contact left down"
                );
            }
        }
        iteration += 1;
        if !tairix_fuzzseed::within_budget(deadline) && iteration >= SMOKE_ITERATIONS {
            break;
        }
    }
}

#[test]
fn a_boot_report_is_read_whatever_its_length() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "a_boot_report_is_read_whatever_its_length",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut mouse = HidDevice::new(boot::mouse().expect("boot mouse")).expect("a mouse");
    let mut keyboard =
        HidDevice::new(boot::keyboard().expect("boot keyboard")).expect("a keyboard");
    let mut iteration: u64 = 0;
    loop {
        let mut report = vec![0u8; rng.at_most(12)];
        rng.fill(&mut report);
        let mut seat = Seat::default();
        let decoded = mouse.input(&report, &mut seat).expect("delivered");
        assert_eq!(
            decoded == Decoded::Malformed,
            report.len() < 3,
            "{report:?}"
        );
        mouse.release(&mut seat).expect("released");
        let decoded = keyboard.input(&report, &mut seat).expect("delivered");
        assert_eq!(
            decoded == Decoded::Malformed,
            report.is_empty(),
            "{report:?}"
        );
        iteration += 1;
        if !tairix_fuzzseed::within_budget(deadline) && iteration >= SMOKE_ITERATIONS {
            break;
        }
    }
}

#[test]
fn the_console_producer_resolves_only_key_edges() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "the_console_producer_resolves_only_key_edges",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut console = KeyboardConsole::new();
    let mut iteration: u64 = 0;
    loop {
        let kind = *rng.pick(&[
            InputEventKind::Key,
            InputEventKind::Pointer,
            InputEventKind::Scroll,
        ]);
        let value = if rng.below(4) == 0 {
            i32::from_le_bytes(rng.next_u32().to_le_bytes())
        } else {
            i32::from(rng.below(2) == 0)
        };
        let code = if rng.below(2) == 0 {
            u16::from(rng.next_u8())
        } else {
            rng.next_u16()
        };
        let record = console.feed(InputEvent {
            kind,
            reserved0: 0,
            code,
            value,
        });
        if kind != InputEventKind::Key || !(0..=1).contains(&value) {
            assert_eq!(record, None, "{kind:?} {code:#06x} = {value}");
        }
        if let Some(record) = record {
            assert_eq!(KeyInput::from_bytes(&record.to_le_bytes()), Ok(record));
        }

        iteration += 1;
        if !tairix_fuzzseed::within_budget(deadline) && iteration >= SMOKE_ITERATIONS {
            break;
        }
    }
}
