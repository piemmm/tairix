extern crate std;

use alloc::vec::Vec;

use super::{BootLayout, HidInterface, InterfaceError};

/// A configuration descriptor of `body`, its total length filled in.
pub(crate) fn configuration(body: &[&[u8]]) -> Vec<u8> {
    let mut config = alloc::vec![9u8, 0x02, 0, 0, 2, 1, 0, 0xA0, 50];
    for descriptor in body {
        config.extend_from_slice(descriptor);
    }
    let total = u16::try_from(config.len()).expect("fits");
    config[2..4].copy_from_slice(&total.to_le_bytes());
    config
}

pub(crate) const fn interface(number: u8, class: [u8; 3]) -> [u8; 9] {
    [9, 0x04, number, 0, 1, class[0], class[1], class[2], 0]
}

pub(crate) const fn hid(report_len: u16) -> [u8; 9] {
    let [low, high] = report_len.to_le_bytes();
    [9, 0x21, 0x11, 0x01, 0, 1, 0x22, low, high]
}

pub(crate) const fn interrupt_in(endpoint: u8) -> [u8; 7] {
    [7, 0x05, 0x80 | endpoint, 0x03, 8, 0, 10]
}

#[test]
fn an_interface_is_found_by_its_number_among_its_siblings() {
    let config = configuration(&[
        &interface(0, [3, 1, 1]),
        &hid(63),
        &interrupt_in(1),
        &interface(1, [3, 0, 0]),
        &hid(700),
        &interrupt_in(2),
    ]);
    assert_eq!(
        HidInterface::find(&config, 1),
        Ok(HidInterface {
            class: [3, 0, 0],
            report_descriptor_len: Some(700),
            interrupt_endpoint: 2,
        })
    );
    assert_eq!(
        HidInterface::find(&config, 0).map(|found| found.boot_layout()),
        Ok(Some(BootLayout::Keyboard))
    );
    assert_eq!(
        HidInterface::find(&config, 2),
        Err(InterfaceError::NotFound)
    );
}

#[test]
fn a_hid_descriptor_after_the_endpoint_is_taken_too() {
    let config = configuration(&[&interface(0, [3, 1, 2]), &interrupt_in(1), &hid(52)]);
    let found = HidInterface::find(&config, 0).expect("found");
    assert_eq!(found.report_descriptor_len, Some(52));
    assert_eq!(found.boot_layout(), Some(BootLayout::Mouse));
}

#[test]
fn the_report_entry_is_found_in_a_list_of_class_descriptors() {
    let physical_then_report = [12, 0x21, 0x11, 0x01, 0, 2, 0x23, 9, 0, 0x22, 0x40, 0x01];
    let config = configuration(&[
        &interface(0, [3, 0, 0]),
        &physical_then_report,
        &interrupt_in(1),
    ]);
    assert_eq!(
        HidInterface::find(&config, 0).map(|found| found.report_descriptor_len),
        Ok(Some(0x140))
    );
}

#[test]
fn an_interface_without_an_interrupt_in_endpoint_or_an_alternate_setting_is_not_found() {
    let interrupt_out = [7, 0x05, 0x01, 0x03, 8, 0, 10];
    let config = configuration(&[&interface(0, [3, 0, 0]), &hid(9), &interrupt_out]);
    assert_eq!(
        HidInterface::find(&config, 0),
        Err(InterfaceError::NotFound)
    );
    let mut alternate = interface(0, [3, 0, 0]);
    alternate[3] = 1;
    let config = configuration(&[&alternate, &hid(9), &interrupt_in(1)]);
    assert_eq!(
        HidInterface::find(&config, 0),
        Err(InterfaceError::NotFound)
    );
}

#[test]
fn a_malformed_configuration_is_refused() {
    let config = configuration(&[&interface(0, [3, 0, 0]), &[0, 0x21]]);
    assert_eq!(
        HidInterface::find(&config, 0),
        Err(InterfaceError::Malformed)
    );
    assert_eq!(
        HidInterface::find(&[9, 2], 0),
        Err(InterfaceError::Malformed)
    );
    let config = configuration(&[&[5, 0x04, 0, 0, 1]]);
    assert_eq!(
        HidInterface::find(&config, 0),
        Err(InterfaceError::Malformed)
    );
}
