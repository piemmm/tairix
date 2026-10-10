use alloc::vec;

use super::*;
use crate::fixtures::{configuration, headset_v2, interface, qemu_usb_audio};

#[test]
fn qemus_function_is_a_streaming_terminal_feeding_a_speaker_through_one_feature_unit() {
    let topology = Topology::parse(&qemu_usb_audio(), 0).expect("parses");
    assert_eq!(topology.version, Version::One);
    assert_eq!(topology.streaming, [1]);
    assert_eq!(
        topology.entity(1).map(|e| &e.kind),
        Some(&EntityKind::InputTerminal {
            terminal_type: TERMINAL_USB_STREAMING,
            cluster: Cluster {
                channels: 2,
                config: 0x3
            },
            clock: 0,
        })
    );
    assert_eq!(
        topology.entity(2).map(|e| &e.kind),
        Some(&EntityKind::Feature {
            source: 1,
            master: FeatureControls {
                mute: true,
                volume: false
            },
            channels: vec![
                FeatureControls {
                    mute: false,
                    volume: true
                };
                2
            ],
        })
    );
    assert_eq!(topology.path_from(1), Some(vec![1, 2, 3]));
}

#[test]
fn a_version_two_function_has_both_paths_and_a_clock_behind_its_selector() {
    let topology = Topology::parse(&headset_v2(), 0).expect("parses");
    assert_eq!(topology.version, Version::Two);
    assert!(topology.streaming.is_empty(), "the association groups them");
    assert_eq!(topology.path_from(1), Some(vec![1, 2, 3]), "playback");
    assert_eq!(topology.path_from(6), Some(vec![6, 5, 4]), "capture");
    assert_eq!(
        topology.clock_path(0x28, &|selector| (selector == 0x28).then_some(1)),
        Some(vec![0x28, 0x29])
    );
    assert_eq!(
        topology.clock_path(0x28, &|_| Some(2)),
        None,
        "a pin the selector lacks reaches no clock"
    );
    assert_eq!(
        topology.entity(0x29).map(|e| &e.kind),
        Some(&EntityKind::ClockSource {
            programmable: true,
            validity: true
        })
    );
    // Version 2.0 controls count only where the host may program them: the
    // capture unit's master mute is, and its channel offers nothing.
    assert_eq!(
        topology.entity(5).map(|e| &e.kind),
        Some(&EntityKind::Feature {
            source: 4,
            master: FeatureControls {
                mute: true,
                volume: false
            },
            channels: vec![FeatureControls::default()],
        })
    );
}

#[test]
fn a_read_only_version_two_control_is_none_the_host_can_set() {
    assert_eq!(FeatureControls::from_v2(0b0101), FeatureControls::default());
    assert_eq!(
        FeatureControls::from_v2(0b1111),
        FeatureControls {
            mute: true,
            volume: true
        }
    );
}

/// A 1.0 function's control interface 0 carrying `entities` after its header.
fn v1_function(entities: &[&[u8]]) -> alloc::vec::Vec<u8> {
    let interface = interface(0, 0, 0, 0x01_01_00);
    let header: [u8; 9] = [9, 0x24, 0x01, 0x00, 0x01, 0, 0, 0x01, 0x01];
    let mut body: alloc::vec::Vec<&[u8]> = vec![&interface, &header];
    body.extend_from_slice(entities);
    configuration(&body)
}

#[test]
fn a_cycle_among_units_ends_the_walk_rather_than_spinning() {
    // Units 2 and 3 name each other; neither reaches the streaming terminal
    // the speaker's path would need.
    let config = v1_function(&[
        &[12, 0x24, 0x02, 0x01, 0x01, 0x01, 0, 2, 3, 0, 0, 0],
        &[7, 0x24, 0x05, 0x02, 0x01, 0x03, 0],
        &[7, 0x24, 0x05, 0x03, 0x01, 0x02, 0],
        &[9, 0x24, 0x03, 0x04, 0x01, 0x03, 0, 0x02, 0],
    ]);
    let topology = Topology::parse(&config, 0).expect("parses");
    assert_eq!(topology.path_from(1), None);
}

#[test]
fn a_path_through_a_mixer_is_found_whichever_pin_carries_it() {
    let config = v1_function(&[
        &[12, 0x24, 0x02, 0x01, 0x01, 0x01, 0, 2, 3, 0, 0, 0],
        &[12, 0x24, 0x02, 0x05, 0x01, 0x02, 0, 1, 0, 0, 0, 0],
        // Mixer 2, pins from the microphone 5 and the stream 1.
        &[13, 0x24, 0x04, 0x02, 0x02, 0x05, 0x01, 2, 3, 0, 0, 0, 0],
        &[9, 0x24, 0x03, 0x03, 0x01, 0x03, 0, 0x02, 0],
    ]);
    let topology = Topology::parse(&config, 0).expect("parses");
    assert_eq!(topology.path_from(1), Some(vec![1, 2, 3]));
}

#[test]
fn a_malformed_or_ambiguous_control_interface_is_refused() {
    let it: &[u8] = &[12, 0x24, 0x02, 0x01, 0x01, 0x01, 0, 2, 3, 0, 0, 0];
    // Two entities sharing an id.
    let config = v1_function(&[it, it]);
    assert_eq!(Topology::parse(&config, 0), Err(DriverError::BadMagic));
    // An entity id of zero.
    let config = v1_function(&[&[12, 0x24, 0x02, 0x00, 0x01, 0x01, 0, 2, 3, 0, 0, 0]]);
    assert_eq!(Topology::parse(&config, 0), Err(DriverError::BadMagic));
    // A terminal cut short of its fields.
    let config = v1_function(&[&[6, 0x24, 0x02, 0x01, 0x01, 0x01]]);
    assert_eq!(Topology::parse(&config, 0), Err(DriverError::BadMagic));
    // A feature unit whose bitmaps do not tile its length.
    let config = v1_function(&[&[8, 0x24, 0x06, 0x02, 0x01, 0x02, 0x01, 0x00]]);
    assert_eq!(Topology::parse(&config, 0), Err(DriverError::BadMagic));
    // A revision the class does not define, and no header at all.
    let interface = interface(0, 0, 0, 0x01_01_00);
    let header: [u8; 9] = [9, 0x24, 0x01, 0x00, 0x03, 0, 0, 0, 0];
    let config = configuration(&[&interface, &header]);
    assert_eq!(Topology::parse(&config, 0), Err(DriverError::BadMagic));
    let config = configuration(&[&interface, it]);
    assert_eq!(Topology::parse(&config, 0), Err(DriverError::BadMagic));
    // A header listing more streaming interfaces than it carries.
    let header: [u8; 9] = [9, 0x24, 0x01, 0x00, 0x01, 0, 0, 0x03, 0x01];
    let config = configuration(&[&interface, &header]);
    assert_eq!(Topology::parse(&config, 0), Err(DriverError::BadMagic));
}

#[test]
fn only_the_named_control_interface_is_read() {
    let topology = Topology::parse(&headset_v2(), 0).expect("parses");
    assert_eq!(topology.entities().count(), 8);
    assert_eq!(
        Topology::parse(&headset_v2(), 4),
        Err(DriverError::BadMagic),
        "an interface with no header describes no function"
    );
}

#[test]
fn terminals_are_named_by_what_they_are() {
    assert_eq!(terminal_name(0x0301), "Speaker");
    assert_eq!(terminal_name(0x0302), "Headphones");
    assert_eq!(terminal_name(0x0402), "Headset");
    assert_eq!(terminal_name(0x0201), "Microphone");
    assert_eq!(terminal_name(0x0603), "Line");
    assert_eq!(terminal_name(0x03FF), "Output");
    assert_eq!(terminal_name(0x07FF), "Input");
}
