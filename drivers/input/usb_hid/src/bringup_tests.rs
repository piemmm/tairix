extern crate std;

use alloc::vec::Vec;

use tairix_abi::Errno;
use tairix_hid::DescriptorError;

use super::{bring_up, BringupError, HidLink};
use crate::interface::tests::{configuration, hid, interface, interrupt_in};
use crate::interface::InterfaceError;
use crate::requests::{self, Protocol};

/// A keyboard reporting under ID 1: modifiers and six key slots.
const KEYBOARD: [u8; 37] = [
    0x05, 0x01, 0x09, 0x06, 0xA1, 0x01, 0x85, 0x01, 0x05, 0x07, 0x19, 0xE0, 0x29, 0xE7, 0x15, 0x00,
    0x25, 0x01, 0x75, 0x01, 0x95, 0x08, 0x81, 0x02, 0x95, 0x06, 0x75, 0x08, 0x25, 0x65, 0x19, 0x00,
    0x29, 0x65, 0x81, 0x00, 0xC0,
];

/// A one-finger touch pad under ID 1 with its Input Mode in feature 3.
const TOUCHPAD: [u8; 80] = [
    0x05, 0x0D, 0x09, 0x05, 0xA1, 0x01, 0x85, 0x01, 0x09, 0x22, 0xA1, 0x02, 0x15, 0x00, 0x25, 0x01,
    0x75, 0x01, 0x95, 0x01, 0x09, 0x42, 0x81, 0x02, 0x95, 0x07, 0x81, 0x03, 0x05, 0x01, 0x26, 0xFF,
    0x0F, 0x75, 0x10, 0x95, 0x01, 0x09, 0x30, 0x81, 0x02, 0x09, 0x31, 0x81, 0x02, 0xC0, 0x05, 0x0D,
    0x09, 0x54, 0x25, 0x05, 0x75, 0x08, 0x95, 0x01, 0x81, 0x02, 0xC0, 0x05, 0x0D, 0x09, 0x0E, 0xA1,
    0x01, 0x85, 0x03, 0x09, 0x52, 0x15, 0x00, 0x25, 0x0A, 0x75, 0x08, 0x95, 0x01, 0xB1, 0x02, 0xC0,
];

/// A device answering its interface's requests.
#[derive(Default)]
struct Device {
    config: Vec<u8>,
    /// `None` STALLs the read.
    report_descriptor: Option<Vec<u8>>,
    /// `GET_PROTOCOL`'s answer; `None` STALLs it.
    protocol: Option<u8>,
    /// `SET_PROTOCOL` and `SET_IDLE` STALL.
    declines_optional: bool,
    /// Requests answered before the device goes.
    gone_after: Option<usize>,
    sent: Vec<[u8; 8]>,
    written: Vec<([u8; 8], Vec<u8>)>,
}

impl Device {
    fn new(config: Vec<u8>, report_descriptor: &[u8]) -> Self {
        Self {
            config,
            report_descriptor: Some(report_descriptor.to_vec()),
            ..Self::default()
        }
    }

    fn accept(&mut self, setup: [u8; 8]) -> Result<(), Errno> {
        if self
            .gone_after
            .is_some_and(|after| self.sent.len() >= after)
        {
            return Err(Errno::NotFound);
        }
        self.sent.push(setup);
        Ok(())
    }

    fn answer(data: &mut [u8], bytes: &[u8]) -> usize {
        let len = data.len().min(bytes.len());
        data[..len].copy_from_slice(&bytes[..len]);
        len
    }
}

impl HidLink for Device {
    fn control_in(&mut self, setup: [u8; 8], data: &mut [u8]) -> Result<usize, Errno> {
        self.accept(setup)?;
        match (setup[0], setup[1], setup[3]) {
            (0x80, 0x06, 0x02) => Ok(Self::answer(data, &self.config)),
            (0x81, 0x06, 0x22) => match &self.report_descriptor {
                Some(descriptor) => Ok(Self::answer(data, descriptor)),
                None => Err(Errno::EndpointStalled),
            },
            (0xA1, 0x03, _) => match self.protocol {
                Some(answer) => Ok(Self::answer(data, &[answer])),
                None => Err(Errno::EndpointStalled),
            },
            _ => Err(Errno::EndpointStalled),
        }
    }

    fn control_out(&mut self, setup: [u8; 8], data: &[u8]) -> Result<(), Errno> {
        self.accept(setup)?;
        self.written.push((setup, data.to_vec()));
        Ok(())
    }

    fn control_no_data(&mut self, setup: [u8; 8]) -> Result<(), Errno> {
        self.accept(setup)?;
        if self.declines_optional {
            return Err(Errno::EndpointStalled);
        }
        Ok(())
    }
}

fn keyboard_device(class: [u8; 3]) -> Device {
    let config = configuration(&[&interface(0, class), &hid(37), &interrupt_in(1)]);
    Device::new(config, &KEYBOARD)
}

#[test]
fn a_keyboard_is_read_in_report_protocol_and_left_idle_until_it_changes() {
    let mut device = keyboard_device([3, 1, 1]);
    device.protocol = Some(1);
    let bound = bring_up(&mut device, 0).expect("brought up");
    assert_eq!(bound.protocol, Protocol::Report);
    assert_eq!(bound.longest_input, 8, "its ID, the modifiers and six keys");
    assert_eq!(bound.descriptor, KEYBOARD);
    assert_eq!(bound.device.applications().keyboards, 1);
    assert!(device
        .sent
        .contains(&requests::get_report_descriptor(0, 37)));
    assert!(device
        .sent
        .contains(&requests::set_protocol(0, Protocol::Report)));
    assert!(device.sent.contains(&requests::set_idle(0)));
}

#[test]
fn a_boot_keyboard_that_stays_in_boot_protocol_is_read_in_it() {
    let mut device = keyboard_device([3, 1, 1]);
    device.protocol = Some(0);
    let bound = bring_up(&mut device, 0).expect("brought up");
    assert_eq!(bound.protocol, Protocol::Boot);
    assert_eq!(bound.longest_input, 8, "the boot keyboard report");
}

#[test]
fn a_device_declining_the_optional_requests_is_brought_up() {
    let mut device = keyboard_device([3, 0, 0]);
    device.declines_optional = true;
    let bound = bring_up(&mut device, 0).expect("a refusal is an answer");
    assert_eq!(bound.protocol, Protocol::Report);
    assert!(
        !device.sent.contains(&requests::get_protocol(0)),
        "only a boot-subclass device is asked which protocol it kept"
    );
}

#[test]
fn a_boot_mouse_whose_descriptor_will_not_parse_is_read_in_boot_protocol() {
    let config = configuration(&[&interface(0, [3, 1, 2]), &hid(4), &interrupt_in(1)]);
    let mut device = Device::new(config, &[0x05, 0x01, 0xA1, 0x01]);
    let bound = bring_up(&mut device, 0).expect("the boot layout serves it");
    assert_eq!(bound.protocol, Protocol::Boot);
    assert_eq!(bound.device.applications().mice, 1);
    assert!(device
        .sent
        .contains(&requests::set_protocol(0, Protocol::Boot)));
}

#[test]
fn an_interface_without_a_boot_layout_or_a_readable_descriptor_is_refused() {
    let config = configuration(&[&interface(0, [3, 0, 0]), &hid(4), &interrupt_in(1)]);
    let mut unparsable = Device::new(config.clone(), &[0x05, 0x01, 0xA1, 0x01]);
    assert_eq!(
        bring_up(&mut unparsable, 0).map(|_| ()),
        Err(BringupError::Descriptor(DescriptorError::Unbalanced))
    );
    let mut unreadable = Device::new(config, &[]);
    unreadable.report_descriptor = None;
    assert_eq!(
        bring_up(&mut unreadable, 0).map(|_| ()),
        Err(BringupError::Descriptor(DescriptorError::Length))
    );
}

#[test]
fn a_touch_pad_is_switched_to_report_its_contacts() {
    let config = configuration(&[&interface(0, [3, 0, 0]), &hid(80), &interrupt_in(1)]);
    let mut device = Device::new(config, &TOUCHPAD);
    let bound = bring_up(&mut device, 0).expect("brought up");
    assert_eq!(bound.device.applications().touchpads, 1);
    assert!(device
        .written
        .contains(&(requests::set_feature(0, 3, 2), alloc::vec![3, 3])));
}

#[test]
fn a_device_gone_mid_bring_up_is_gone_and_one_describing_no_interface_is_refused() {
    let mut device = keyboard_device([3, 1, 1]);
    device.gone_after = Some(3);
    assert_eq!(
        bring_up(&mut device, 0).map(|_| ()),
        Err(BringupError::Gone)
    );
    let mut device = keyboard_device([3, 1, 1]);
    assert_eq!(
        bring_up(&mut device, 1).map(|_| ()),
        Err(BringupError::Interface(InterfaceError::NotFound))
    );
}

#[test]
fn an_interface_carrying_nothing_the_seat_serves_is_refused() {
    let vendor = [
        0x06, 0x00, 0xFF, 0x09, 0x01, 0xA1, 0x01, 0x15, 0x00, 0x26, 0xFF, 0x00, 0x75, 0x08, 0x95,
        0x08, 0x09, 0x01, 0x81, 0x02, 0xC0,
    ];
    let config = configuration(&[&interface(0, [3, 0, 0]), &hid(21), &interrupt_in(1)]);
    let mut device = Device::new(config, &vendor);
    assert_eq!(
        bring_up(&mut device, 0).map(|_| ()),
        Err(BringupError::NothingServed)
    );
}
