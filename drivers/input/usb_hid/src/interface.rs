//! The driver's own interface as the device's configuration descriptor
//! describes it: its class code, the report descriptor its HID descriptor
//! states (USB HID 1.11 §6.2.1), and its interrupt-IN endpoint.

use tairix_usb::descriptor::{descriptors, Malformed};

/// Bytes of a configuration descriptor's own header.
pub const CONFIGURATION_HEADER_LEN: usize = 9;

const DESC_TYPE_INTERFACE: u8 = 0x04;
const DESC_TYPE_ENDPOINT: u8 = 0x05;
const DESC_TYPE_HID: u8 = 0x21;
const INTERFACE_DESC_LEN: usize = 9;
const ENDPOINT_DESC_LEN: usize = 7;

/// Bytes of a HID descriptor before its list of class descriptors.
const HID_DESC_LIST: usize = 6;

/// `bEndpointAddress`'s direction bit and number (USB 2.0 §9.6.6).
const ENDPOINT_IN: u8 = 0x80;
const ENDPOINT_NUMBER: u8 = 0x0F;

/// `bmAttributes` naming an interrupt endpoint.
const ATTRIBUTES_INTERRUPT: u8 = 0x03;

/// The boot sub-class (HID 1.11 §4.2) and its keyboard and mouse protocols.
const SUBCLASS_BOOT: u8 = 0x01;
const PROTOCOL_KEYBOARD: u8 = 0x01;
const PROTOCOL_MOUSE: u8 = 0x02;

/// The boot layout a boot-subclass interface offers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BootLayout {
    /// The eight-byte boot keyboard report.
    Keyboard,
    /// The boot mouse report.
    Mouse,
}

/// One HID interface's description.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HidInterface {
    /// `bInterfaceClass`, `bInterfaceSubClass`, `bInterfaceProtocol`.
    pub class: [u8; 3],
    /// The report descriptor length its HID descriptor states, `None` when it
    /// carries none.
    pub report_descriptor_len: Option<u16>,
    /// The number of its first interrupt-IN endpoint.
    pub interrupt_endpoint: u8,
}

/// Why an interface could not be found.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InterfaceError {
    /// The configuration descriptor is malformed.
    Malformed,
    /// It describes no such interface with an interrupt-IN endpoint.
    NotFound,
}

impl HidInterface {
    /// Interface `number`'s default setting in configuration descriptor
    /// `config`. Its HID descriptor is taken wherever its descriptors carry
    /// it, before its endpoints or, as some devices place it, after.
    ///
    /// # Errors
    ///
    /// [`InterfaceError::Malformed`] for a malformed descriptor, or
    /// [`InterfaceError::NotFound`] when no such interface carries an
    /// interrupt-IN endpoint.
    pub fn find(config: &[u8], number: u8) -> Result<Self, InterfaceError> {
        let body = config
            .get(CONFIGURATION_HEADER_LEN..)
            .ok_or(InterfaceError::Malformed)?;
        let mut found: Option<[u8; 3]> = None;
        let mut report_descriptor_len = None;
        let mut interrupt_endpoint = None;
        for descriptor in descriptors(body) {
            let descriptor = descriptor.map_err(|Malformed| InterfaceError::Malformed)?;
            match descriptor[1] {
                DESC_TYPE_INTERFACE => {
                    if found.is_some() {
                        break;
                    }
                    if descriptor.len() < INTERFACE_DESC_LEN {
                        return Err(InterfaceError::Malformed);
                    }
                    if descriptor[2] == number && descriptor[3] == 0 {
                        found = Some([descriptor[5], descriptor[6], descriptor[7]]);
                    }
                }
                DESC_TYPE_HID if found.is_some() && report_descriptor_len.is_none() => {
                    report_descriptor_len = stated_report_length(descriptor);
                }
                DESC_TYPE_ENDPOINT if found.is_some() && interrupt_endpoint.is_none() => {
                    if descriptor.len() < ENDPOINT_DESC_LEN {
                        return Err(InterfaceError::Malformed);
                    }
                    let (address, attributes) = (descriptor[2], descriptor[3]);
                    if address & ENDPOINT_IN != 0
                        && address & ENDPOINT_NUMBER != 0
                        && attributes & 0x03 == ATTRIBUTES_INTERRUPT
                    {
                        interrupt_endpoint = Some(address & ENDPOINT_NUMBER);
                    }
                }
                _ => {}
            }
        }
        match (found, interrupt_endpoint) {
            (Some(class), Some(interrupt_endpoint)) => Ok(Self {
                class,
                report_descriptor_len,
                interrupt_endpoint,
            }),
            _ => Err(InterfaceError::NotFound),
        }
    }

    /// The boot layout the interface offers, when it is a boot keyboard or
    /// boot mouse.
    #[must_use]
    pub const fn boot_layout(&self) -> Option<BootLayout> {
        match self.class {
            [_, SUBCLASS_BOOT, PROTOCOL_KEYBOARD] => Some(BootLayout::Keyboard),
            [_, SUBCLASS_BOOT, PROTOCOL_MOUSE] => Some(BootLayout::Mouse),
            _ => None,
        }
    }
}

/// The length a HID descriptor states for its report descriptor: the first
/// entry of its class-descriptor list naming one.
fn stated_report_length(hid: &[u8]) -> Option<u16> {
    let count = usize::from(*hid.get(5)?);
    hid.get(HID_DESC_LIST..)?
        .as_chunks::<3>()
        .0
        .iter()
        .take(count)
        .find(|[kind, ..]| *kind == crate::requests::DESC_TYPE_REPORT)
        .map(|&[_, low, high]| u16::from_le_bytes([low, high]))
}

#[cfg(test)]
#[path = "interface_tests.rs"]
pub(crate) mod tests;
