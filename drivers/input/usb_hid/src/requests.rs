//! The HID class requests a driver sends its own interface (USB HID 1.11
//! §7), each an 8-byte SETUP.

/// The protocol a device reports in (HID 1.11 §7.2.5): the fixed boot layout
/// a boot-subclass device offers, or the one its report descriptor states.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Protocol {
    /// The boot keyboard or boot mouse layout.
    Boot = 0,
    /// The report descriptor's layout, every device's default.
    Report = 1,
}

/// `wValue`'s high byte naming a feature report (HID 1.11 §7.2.1).
const REPORT_TYPE_FEATURE: u8 = 0x03;

/// `bDescriptorType` of a report descriptor (HID 1.11 §7.1).
pub const DESC_TYPE_REPORT: u8 = 0x22;

/// `GET_DESCRIPTOR(Report)`: the interface's report descriptor, `len` bytes
/// (HID 1.11 §7.1.1).
#[must_use]
pub const fn get_report_descriptor(interface: u8, len: u16) -> [u8; 8] {
    let [low, high] = len.to_le_bytes();
    [
        0x81,
        0x06,
        0x00,
        DESC_TYPE_REPORT,
        interface,
        0x00,
        low,
        high,
    ]
}

/// `SET_PROTOCOL` (HID 1.11 §7.2.6).
#[must_use]
pub const fn set_protocol(interface: u8, protocol: Protocol) -> [u8; 8] {
    [
        0x21,
        0x0B,
        protocol as u8,
        0x00,
        interface,
        0x00,
        0x00,
        0x00,
    ]
}

/// `GET_PROTOCOL`, answered with one byte (HID 1.11 §7.2.5).
#[must_use]
pub const fn get_protocol(interface: u8) -> [u8; 8] {
    [0xA1, 0x03, 0x00, 0x00, interface, 0x00, 0x01, 0x00]
}

/// `SET_IDLE` with a duration of zero for every report: the device reports
/// only when a report changes (HID 1.11 §7.2.4).
#[must_use]
pub const fn set_idle(interface: u8) -> [u8; 8] {
    [0x21, 0x0A, 0x00, 0x00, interface, 0x00, 0x00, 0x00]
}

/// `GET_REPORT` of feature report `id` (`0` with no report IDs), `len` bytes
/// (HID 1.11 §7.2.1).
#[must_use]
pub const fn get_feature(interface: u8, id: u8, len: u16) -> [u8; 8] {
    let [low, high] = len.to_le_bytes();
    [
        0xA1,
        0x01,
        id,
        REPORT_TYPE_FEATURE,
        interface,
        0x00,
        low,
        high,
    ]
}

/// `SET_REPORT` of feature report `id`, whose data stage carries its `len`
/// bytes (HID 1.11 §7.2.2).
#[must_use]
pub const fn set_feature(interface: u8, id: u8, len: u16) -> [u8; 8] {
    let [low, high] = len.to_le_bytes();
    [
        0x21,
        0x09,
        id,
        REPORT_TYPE_FEATURE,
        interface,
        0x00,
        low,
        high,
    ]
}

#[cfg(test)]
mod tests {
    use super::{
        get_feature, get_protocol, get_report_descriptor, set_feature, set_idle, set_protocol,
        Protocol,
    };

    #[test]
    fn each_request_is_the_setup_hid_1_11_defines() {
        assert_eq!(
            get_report_descriptor(2, 0x01C4),
            [0x81, 0x06, 0x00, 0x22, 2, 0, 0xC4, 0x01]
        );
        assert_eq!(
            set_protocol(1, Protocol::Report),
            [0x21, 0x0B, 1, 0, 1, 0, 0, 0]
        );
        assert_eq!(
            set_protocol(1, Protocol::Boot),
            [0x21, 0x0B, 0, 0, 1, 0, 0, 0]
        );
        assert_eq!(get_protocol(3), [0xA1, 0x03, 0, 0, 3, 0, 1, 0]);
        assert_eq!(set_idle(0), [0x21, 0x0A, 0, 0, 0, 0, 0, 0]);
        assert_eq!(get_feature(1, 5, 3), [0xA1, 0x01, 5, 3, 1, 0, 3, 0]);
        assert_eq!(set_feature(1, 7, 2), [0x21, 0x09, 7, 3, 1, 0, 2, 0]);
    }
}
