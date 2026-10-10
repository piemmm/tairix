use alloc::vec::Vec;

use super::*;
use crate::descriptor::DESC_TYPE_CONFIGURATION;
use crate::periodic::{IsoSync, IsoUsage, TransferKind};

/// A configuration descriptor wrapping `body`, its total length stated.
fn config(body: &[&[u8]]) -> Vec<u8> {
    let mut bytes = alloc::vec![9, DESC_TYPE_CONFIGURATION, 0, 0, 2, 1, 0, 0x80, 50];
    for descriptor in body {
        bytes.extend_from_slice(descriptor);
    }
    let total = u16::try_from(bytes.len()).expect("small").to_le_bytes();
    bytes[2..4].copy_from_slice(&total);
    bytes
}

fn interface(number: u8, alternate: u8, endpoints: u8, class: [u8; 3]) -> [u8; 9] {
    [
        9,
        DESC_TYPE_INTERFACE,
        number,
        alternate,
        endpoints,
        class[0],
        class[1],
        class[2],
        0,
    ]
}

const AUDIO_CONTROL: [u8; 3] = [1, 1, 0];
const AUDIO_STREAMING: [u8; 3] = [1, 2, 0];

/// A USB Audio 1.0 function shaped like the one QEMU's `usb-audio` presents:
/// a control interface carrying class descriptors, and a streaming interface
/// whose default setting moves nothing and whose setting 1 brings one
/// isochronous OUT endpoint.
fn audio_function() -> Vec<u8> {
    config(&[
        &interface(0, 0, 0, AUDIO_CONTROL),
        // A class-specific AC header the walk must step over.
        &[9, 0x24, 1, 0x00, 0x01, 30, 0, 1, 1],
        &interface(1, 0, 0, AUDIO_STREAMING),
        &interface(1, 1, 1, AUDIO_STREAMING),
        &[7, 0x24, 1, 1, 1, 1, 0],
        // Audio-class endpoint: synchronous isochronous OUT, 192 bytes.
        &[9, DESC_TYPE_ENDPOINT, 0x01, 0x0D, 192, 0, 1, 0, 0],
        &[7, 0x25, 1, 0, 0, 0, 0],
    ])
}

#[test]
fn a_streaming_setting_brings_its_endpoints_and_the_default_brings_none() {
    let bytes = audio_function();
    let streaming = alternate_setting(&bytes, 1, 1).expect("present");
    assert_eq!(streaming.class24, 0x01_02_00);
    let endpoint = streaming.endpoint(0x01).expect("its endpoint");
    assert_eq!(
        endpoint.kind,
        TransferKind::Isochronous {
            sync: IsoSync::Synchronous,
            usage: IsoUsage::Data
        }
    );
    assert_eq!(endpoint.max_packet, 192);
    assert_eq!(streaming.dci_mask(), 1 << 2);
    let idle = alternate_setting(&bytes, 1, 0).expect("present");
    assert_eq!(idle.endpoints().count(), 0);
    assert_eq!(idle.dci_mask(), 0);
    // The control interface's class descriptors are not endpoints.
    assert_eq!(
        alternate_setting(&bytes, 0, 0)
            .expect("present")
            .endpoints()
            .count(),
        0
    );
}

#[test]
fn a_setting_the_configuration_lacks_is_not_found() {
    let bytes = audio_function();
    assert_eq!(alternate_setting(&bytes, 1, 2), Err(DriverError::NotFound));
    assert_eq!(alternate_setting(&bytes, 2, 0), Err(DriverError::NotFound));
}

#[test]
fn interface_numbers_are_every_interface_declared() {
    let numbers = interface_numbers(&audio_function()).expect("well formed");
    assert!(numbers.contains(0) && numbers.contains(1));
    assert!(!numbers.contains(2));
}

#[test]
fn a_companion_completes_the_endpoint_it_follows() {
    let bytes = config(&[
        &interface(0, 1, 2, AUDIO_STREAMING),
        &[7, DESC_TYPE_ENDPOINT, 0x81, 0x05, 0, 4, 1],
        &[6, DESC_TYPE_SS_ENDPOINT_COMPANION, 3, 1, 0x70, 0x17],
        &[7, DESC_TYPE_ENDPOINT, 0x82, 0x11, 4, 0, 4],
    ]);
    let setting = alternate_setting(&bytes, 0, 1).expect("present");
    assert_eq!(
        setting
            .endpoint(0x81)
            .and_then(|endpoint| endpoint.companion),
        Some(SsCompanion {
            max_burst: 3,
            attributes: 1,
            bytes_per_interval: 6000
        })
    );
    assert_eq!(
        setting
            .endpoint(0x82)
            .and_then(|endpoint| endpoint.companion),
        None
    );
}

#[test]
fn a_forged_setting_fails_closed() {
    // Two endpoints at one address.
    let twice = config(&[
        &interface(0, 1, 2, AUDIO_STREAMING),
        &[7, DESC_TYPE_ENDPOINT, 0x81, 0x05, 64, 0, 1],
        &[7, DESC_TYPE_ENDPOINT, 0x81, 0x05, 64, 0, 1],
    ]);
    assert_eq!(alternate_setting(&twice, 0, 1), Err(DriverError::BadMagic));
    // One setting described twice.
    let duplicate = config(&[
        &interface(0, 1, 0, AUDIO_STREAMING),
        &interface(0, 1, 0, AUDIO_STREAMING),
    ]);
    assert_eq!(
        alternate_setting(&duplicate, 0, 1),
        Err(DriverError::BadMagic)
    );
    // More endpoints than the bound.
    let endpoints: Vec<[u8; 7]> = (1..=9u8)
        .map(|number| [7, DESC_TYPE_ENDPOINT, 0x80 | number, 0x05, 64, 0, 1])
        .collect();
    let mut body: Vec<&[u8]> = alloc::vec![];
    let head = interface(0, 1, 9, AUDIO_STREAMING);
    body.push(&head);
    body.extend(endpoints.iter().map(<[u8; 7]>::as_slice));
    assert_eq!(
        alternate_setting(&config(&body), 0, 1),
        Err(DriverError::BadMagic)
    );
    // Not a configuration at all.
    assert_eq!(
        alternate_setting(&[9, 0x01, 0, 0, 0, 0, 0, 0, 0], 0, 0),
        Err(DriverError::BadMagic)
    );
    // A descriptor running past the stream.
    let mut torn = audio_function();
    torn.push(9);
    torn.push(0x24);
    assert_eq!(alternate_setting(&torn, 1, 1), Err(DriverError::BadMagic));
}

#[test]
fn an_association_names_the_interfaces_of_one_function() {
    let iad = [8, DESC_TYPE_INTERFACE_ASSOCIATION, 0, 2, 1, 0, 0x20, 0];
    let mut body: Vec<&[u8]> = alloc::vec![&iad];
    let function = audio_function();
    body.push(&function[9..]);
    let bytes = config(&body);
    let association = association_of(&bytes, 1)
        .expect("well formed")
        .expect("covered");
    assert_eq!(
        association,
        InterfaceAssociation {
            first: 0,
            count: 2,
            class24: 0x01_00_20
        }
    );
    assert!(association.covers(0) && association.covers(1) && !association.covers(2));
    assert_eq!(association_of(&bytes, 2), Ok(None));
    assert_eq!(association_of(&audio_function(), 1), Ok(None));
    let empty = [8, DESC_TYPE_INTERFACE_ASSOCIATION, 0, 0, 1, 0, 0, 0];
    assert_eq!(
        association_of(&config(&[&empty]), 0),
        Err(DriverError::BadMagic)
    );
}

#[test]
fn only_an_interface_with_one_empty_setting_is_control_only() {
    let bytes = audio_function();
    assert_eq!(is_control_only(&bytes, 0), Ok(true));
    assert_eq!(is_control_only(&bytes, 1), Ok(false), "it has alternates");
    assert_eq!(is_control_only(&bytes, 2), Ok(false), "it does not exist");
    let with_endpoint = config(&[
        &interface(0, 0, 1, AUDIO_CONTROL),
        &[7, DESC_TYPE_ENDPOINT, 0x81, 0x03, 8, 0, 10],
    ]);
    assert_eq!(is_control_only(&with_endpoint, 0), Ok(false));
    let mut torn = audio_function();
    torn.extend_from_slice(&[9, DESC_TYPE_INTERFACE, 3]);
    assert_eq!(is_control_only(&torn, 0), Err(DriverError::BadMagic));
    // An endpoint descriptor too short to be one is refused, as every other
    // reader of the setting refuses it, rather than counted.
    let short = config(&[&interface(0, 0, 1, AUDIO_CONTROL), &[2, DESC_TYPE_ENDPOINT]]);
    assert_eq!(is_control_only(&short, 0), Err(DriverError::BadMagic));
}
