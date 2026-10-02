extern crate std;

use alloc::vec::Vec;
use std::vec;

use tairix_abi::input::{KeyInput, PointerInput};
use tairix_abi::touch::TouchSurface;
use tairix_abi::DriverError;

use super::{Applications, HidDevice};
use crate::config::HidTransport;
use crate::descriptor::{ReportDescriptor, ReportId};
use crate::test_support::items::*;
use crate::test_support::Recorder;
use crate::Decoded;

/// Feature reports a device answers, and every write it was sent.
#[derive(Default)]
struct Device {
    features: Vec<(Option<u8>, Vec<u8>)>,
    writes: Vec<(Option<u8>, Vec<u8>)>,
    refuse_with: Option<DriverError>,
    echo_writes: bool,
}

impl HidTransport for Device {
    fn get_feature(&mut self, id: ReportId, report: &mut [u8]) -> Result<usize, DriverError> {
        if let Some(error) = self.refuse_with {
            return Err(error);
        }
        let answer = self
            .features
            .iter()
            .find(|(feature, _)| *feature == id.id())
            .map(|(_, bytes)| bytes.clone())
            .ok_or(DriverError::Unsupported)?;
        let len = answer.len().min(report.len());
        report[..len].copy_from_slice(&answer[..len]);
        Ok(len)
    }

    fn set_feature(&mut self, id: ReportId, report: &[u8]) -> Result<(), DriverError> {
        if let Some(error) = self.refuse_with {
            return Err(error);
        }
        self.writes.push((id.id(), report.to_vec()));
        if self.echo_writes {
            self.features.retain(|(feature, _)| *feature != id.id());
            self.features.push((id.id(), report.to_vec()));
        }
        Ok(())
    }
}

/// A keyboard under report 1, a one-finger clickpad under report 2 with its
/// limits in feature 5, and a Device Configuration: input mode in feature 3,
/// the surface and button switches in feature 7.
fn keyboard_and_pad() -> Vec<u8> {
    join(&[
        usage_page(0x01),
        usage(0x06),
        collection(1),
        report_id(1),
        usage_page(0x07),
        usage_min(0xE0),
        usage_max(0xE7),
        logical_min(0),
        logical_max(1),
        report_size(1),
        report_count(8),
        input(DATA_VAR),
        report_size(8),
        report_count(2),
        logical_max(0x65),
        usage_min(0),
        usage_max(0x65),
        input(DATA_ARRAY),
        end_collection(),
        usage_page(0x0D),
        usage(0x05),
        collection(1),
        report_id(2),
        usage(0x22),
        collection(2),
        logical_min(0),
        logical_max(1),
        report_size(1),
        report_count(1),
        usage(0x42),
        input(DATA_VAR),
        report_count(7),
        input(CONSTANT),
        usage_page(0x01),
        logical_max(0xFF),
        report_size(8),
        report_count(1),
        usage(0x30),
        input(DATA_VAR),
        usage(0x31),
        input(DATA_VAR),
        end_collection(),
        usage_page(0x0D),
        logical_max(1),
        usage(0x54),
        input(DATA_VAR),
        report_id(5),
        logical_max(5),
        usage(0x55),
        feature(DATA_VAR),
        logical_max(2),
        usage(0x59),
        feature(DATA_VAR),
        end_collection(),
        usage(0x0E),
        collection(1),
        report_id(3),
        logical_max(10),
        usage(0x52),
        feature(DATA_VAR),
        usage(0x23),
        collection(2),
        report_id(7),
        logical_max(1),
        report_size(1),
        report_count(1),
        usage(0x57),
        feature(DATA_VAR),
        usage(0x58),
        feature(DATA_VAR),
        report_count(6),
        feature(CONSTANT),
        end_collection(),
        end_collection(),
    ])
}

fn device(bytes: &[u8]) -> HidDevice {
    HidDevice::new(ReportDescriptor::parse(bytes).expect("parses")).expect("served")
}

#[test]
fn a_composite_interface_is_served_whole() {
    let device = device(&keyboard_and_pad());
    assert_eq!(
        device.applications(),
        Applications {
            keyboards: 1,
            mice: 0,
            touchpads: 1,
            touchscreens: 0,
        }
    );
    assert_eq!(
        device.longest_input(),
        5,
        "the pad's report, its ID, finger, count"
    );
}

#[test]
fn a_digitizer_is_switched_to_report_contacts_with_its_switches_on() {
    let mut device = device(&keyboard_and_pad());
    let mut transport = Device::default();
    device.configure(&mut transport).expect("configured");
    assert!(
        transport.writes.contains(&(Some(3), vec![3, 3])),
        "input mode: touchpad"
    );
    assert!(
        transport.writes.contains(&(Some(7), vec![7, 0b11])),
        "surface and button switches"
    );
}

#[test]
fn a_pads_stated_type_and_limit_are_adopted() {
    let mut device = device(&keyboard_and_pad());
    let mut transport = Device {
        features: vec![(Some(5), vec![5, 1, 2])],
        ..Device::default()
    };
    device.configure(&mut transport).expect("configured");
    let mut sink = Recorder::default();
    let decoded = device
        .input(&[2, 0b1, 10, 20, 2], &mut sink)
        .expect("delivered");
    assert_eq!(
        decoded,
        Decoded::Malformed,
        "two contacts against a stated maximum of one"
    );
    let decoded = device
        .input(&[2, 0b1, 10, 20, 1], &mut sink)
        .expect("delivered");
    assert_eq!(decoded, Decoded::Applied);
    assert_eq!(
        sink.touch[0].surface(),
        TouchSurface::Touchpad,
        "pad type two is no clickpad"
    );
}

#[test]
fn a_refusal_is_an_answer_and_only_a_device_gone_fails_the_configuration() {
    let mut device = device(&keyboard_and_pad());
    let mut refusing = Device {
        refuse_with: Some(DriverError::Unsupported),
        ..Device::default()
    };
    assert_eq!(device.configure(&mut refusing), Ok(()));
    let mut gone = Device {
        refuse_with: Some(DriverError::NotFound),
        ..Device::default()
    };
    assert_eq!(device.configure(&mut gone), Err(DriverError::NotFound));
}

#[test]
fn each_report_reaches_its_own_application() {
    let mut device = device(&keyboard_and_pad());
    let mut sink = Recorder::default();
    assert_eq!(
        device.input(&[1, 0, 0x04, 0], &mut sink),
        Ok(Decoded::Applied)
    );
    assert!(matches!(sink.keys[..], [KeyInput::Pressed { .. }]));
    assert_eq!(
        device.input(&[2, 0b1, 10, 20, 1], &mut sink),
        Ok(Decoded::Applied)
    );
    assert_eq!(sink.touch.len(), 1);
    assert_eq!(device.input(&[9, 0, 0, 0], &mut sink), Ok(Decoded::NotMine));
}

#[test]
fn a_device_going_releases_everything_it_held() {
    let mut device = device(&keyboard_and_pad());
    let mut sink = Recorder::default();
    let _ = device.input(&[1, 0, 0x04, 0], &mut sink);
    let _ = device.input(&[2, 0b1, 10, 20, 1], &mut sink);
    let mut released = Recorder::default();
    device.release(&mut released).expect("released");
    assert!(matches!(released.keys[..], [KeyInput::Released { .. }]));
    assert!(released.touch[0].contacts().is_empty());
}

#[test]
fn a_device_with_nothing_the_seat_serves_is_refused() {
    let vendor = join(&[
        item(0x06, &[0x00, 0xFF]),
        usage(0x01),
        collection(1),
        usage(0x02),
        logical_min(0),
        logical_max(0xFF),
        report_size(8),
        report_count(4),
        input(DATA_VAR),
        end_collection(),
    ]);
    assert!(HidDevice::new(ReportDescriptor::parse(&vendor).expect("parses")).is_none());
}

/// A mouse whose wheel's Resolution Multiplier (feature 4, settings 0..1 for
/// 1..8 counts a detent) sits in the collection enclosing it.
fn mouse_with_multiplier() -> Vec<u8> {
    join(&[
        usage_page(0x01),
        usage(0x02),
        collection(1),
        usage(0x01),
        collection(0),
        report_id(1),
        usage(0x30),
        usage(0x31),
        usage(0x38),
        logical_min(-127),
        logical_max(127),
        report_size(8),
        report_count(3),
        input(DATA_VAR_REL),
        report_id(4),
        usage(0x48),
        logical_min(0),
        logical_max(1),
        item(0x34, &[1]),
        item(0x44, &[8]),
        report_size(8),
        report_count(1),
        feature(DATA_VAR),
        end_collection(),
        end_collection(),
    ])
}

#[test]
fn a_wheel_counts_at_the_resolution_the_device_confirmed() {
    let mut device = device(&mouse_with_multiplier());
    let mut transport = Device {
        echo_writes: true,
        ..Device::default()
    };
    device.configure(&mut transport).expect("configured");
    assert_eq!(transport.writes, [(Some(4), vec![4, 1])]);
    let mut sink = Recorder::default();
    for _ in 0..8 {
        let _ = device.input(&[1, 0, 0, 0xFF], &mut sink);
    }
    let down: i32 = sink
        .pointer
        .iter()
        .map(|record| match record {
            PointerInput::Scrolled { dy, .. } => *dy,
            _ => 0,
        })
        .sum();
    assert_eq!(down, 120, "eight counts are one detent");
}

#[test]
fn a_multiplier_the_device_will_not_set_leaves_whole_detents() {
    let mut device = device(&mouse_with_multiplier());
    let mut transport = Device {
        refuse_with: Some(DriverError::Unsupported),
        ..Device::default()
    };
    device.configure(&mut transport).expect("configured");
    let mut sink = Recorder::default();
    let _ = device.input(&[1, 0, 0, 0xFF], &mut sink);
    assert_eq!(sink.pointer, [PointerInput::Scrolled { dx: 0, dy: 120 }]);
}
