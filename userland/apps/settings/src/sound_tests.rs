//! Host tests for the Sound pane: the plates a reading builds, what each
//! control asks of the audio service, and what the pane shows at once.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::audio::{
    AudioDeviceDescriptor, AudioGain, AudioLocation, ControlAccess, DefaultChoice,
    StreamDescriptor, StreamRole, StreamState,
};
use tairix_abi::driver::audio::{
    AudioName, ChannelMap, Frames, JackState, Rate, RateSupport, SampleFormats, StreamDirection,
};
use tairix_audio::stream::DeviceControl;
use tairix_audio::volume::{level_at_permille, permille_of_level};
use tairix_controls::testkit::covers;
use tairix_controls::{
    AuthorityState, FieldAction, FieldControl, FieldGroup, FieldRow, SelectionState,
};
use tairix_geometry::Scale;

use super::{apply, ask, groups, touches, SoundReading, SoundRow};
use crate::form::{FormOutcome, FormPlace};
use crate::test_support::{showing, theme, WIDE};

const SPEAKERS: u32 = 1;
const HDMI: u32 = 2;
const MICROPHONE: u32 = 3;

fn device(
    device_id: u32,
    direction: StreamDirection,
    name: &str,
    default: DefaultChoice,
) -> AudioDeviceDescriptor {
    AudioDeviceDescriptor {
        device_id,
        direction,
        jack: JackState::Present,
        default,
        formats: SampleFormats::EMPTY,
        channel_map: ChannelMap::STEREO,
        rates: RateSupport::Continuous {
            min: Rate::HZ_48000,
            max: Rate::HZ_48000,
        },
        gain: None,
        name: AudioName::new(name).expect("a short name"),
        location: AudioLocation::new(0x51, 0).expect("a place"),
        level: AudioGain::new(-1_200).expect("attenuation"),
        own_level: false,
        muted: false,
        access: ControlAccess::Own,
        clock_millihertz: 0,
        lost_frames: 0,
    }
}

fn capture(owner_pid: u64, device_id: u32) -> StreamDescriptor {
    StreamDescriptor {
        stream_id: 9,
        device_id,
        direction: StreamDirection::Capture,
        role: StreamRole::Communication,
        state: StreamState::Running,
        position: Frames::new(480),
        xruns: 0,
        xrun_frames: 0,
        owner_uid: 1_000,
        owner_pid,
        owner_app: None,
    }
}

/// Two outputs, the speakers the default, and a microphone.
fn machine() -> SoundReading {
    SoundReading {
        devices: vec![
            device(
                SPEAKERS,
                StreamDirection::Playback,
                "Speakers",
                DefaultChoice::Inherited,
            ),
            device(HDMI, StreamDirection::Playback, "HDMI", DefaultChoice::No),
            device(
                MICROPHONE,
                StreamDirection::Capture,
                "Microphone",
                DefaultChoice::Preferred,
            ),
        ],
        captures: Vec::new(),
        recording: 0,
    }
}

fn captions(groups: &[FieldGroup]) -> Vec<&str> {
    groups.iter().map(FieldGroup::caption).collect()
}

fn labels(group: &FieldGroup) -> Vec<&str> {
    group.rows().iter().map(FieldRow::label).collect()
}

fn reading_of(row: &FieldRow) -> &str {
    match row.control() {
        FieldControl::Reading(text) | FieldControl::Unmeasured(text) => text,
        _ => panic!("{} is not a reading", row.label()),
    }
}

/// Each direction is one plate however many devices it has: which device it
/// uses, then that device's own volume and mute.
#[test]
fn a_direction_is_one_plate_choosing_its_device_then_setting_it() {
    let (groups, owners) = groups(Some(&machine()));
    assert_eq!(captions(&groups), ["OUTPUT", "INPUT", "RECORDING"]);
    assert_eq!(labels(&groups[0]), ["Device", "Volume", "Mute"]);
    assert_eq!(
        owners[0],
        [
            SoundRow::Device(StreamDirection::Playback),
            SoundRow::Level(SPEAKERS),
            SoundRow::Mute(SPEAKERS)
        ]
    );
    let FieldControl::Combo(chooser) = groups[0].rows()[0].control() else {
        panic!("the device is a choice list");
    };
    assert_eq!(chooser.choices(), ["Speakers", "HDMI"]);
    assert_eq!(chooser.selected(), Some(0), "the default is the one shown");
    let FieldControl::Slider(volume) = groups[0].rows()[1].control() else {
        panic!("the volume is a slider");
    };
    assert_eq!(
        volume.value(),
        permille_of_level(AudioGain::new(-1_200).expect("attenuation"))
    );
    assert_eq!(groups[0].rows()[1].description(), Some("-12dB"));
    assert!(matches!(
        groups[0].rows()[2].control(),
        FieldControl::Checkbox(mute) if mute.selection() == SelectionState::Unselected
    ));
    assert!(groups[0].rows().iter().all(|row| row.state().enabled));
    assert_eq!(
        owners[1],
        [
            SoundRow::Device(StreamDirection::Capture),
            SoundRow::Level(MICROPHONE),
            SoundRow::Mute(MICROPHONE)
        ]
    );
}

/// A muted device's volume stands at the bottom and does not apply, and
/// unmuting restores the level it was left at.
#[test]
fn a_muted_device_reads_silent_and_restores_its_level_unmuted() {
    let mut reading = machine();
    reading.devices[0].muted = true;
    let (muted, _) = groups(Some(&reading));
    let volume = &muted[0].rows()[1];
    assert!(matches!(volume.control(), FieldControl::Slider(slider) if slider.value() == 0));
    assert_eq!(volume.description(), Some("Muted"));
    assert!(
        !volume.state().enabled,
        "nothing a muted volume sets is heard"
    );
    assert_eq!(
        volume.state().authority,
        AuthorityState::Allowed,
        "muted is not refused"
    );
    assert!(matches!(
        muted[0].rows()[2].control(),
        FieldControl::Checkbox(mute) if mute.selection() == SelectionState::Selected
    ));
    reading.devices[0].muted = false;
    let (unmuted, _) = groups(Some(&reading));
    assert!(matches!(
        unmuted[0].rows()[1].control(),
        FieldControl::Slider(slider) if slider.value() == permille_of_level(reading.devices[0].level)
    ));
}

/// Another session's room is shown and cannot be changed: every control is
/// refused under the Authority Mark.
#[test]
fn another_sessions_room_is_shown_not_changed() {
    let mut reading = machine();
    for device in &mut reading.devices {
        device.access = ControlAccess::Shown;
    }
    let (groups, _) = groups(Some(&reading));
    for group in &groups[..2] {
        for row in group.rows() {
            assert_eq!(
                row.state().authority,
                AuthorityState::Denied,
                "{}",
                row.label()
            );
            assert!(!row.state().enabled, "{}", row.label());
        }
    }
}

/// Devices that share a name are told apart in the chooser by a count.
#[test]
fn devices_alike_are_told_apart() {
    let mut reading = machine();
    reading.devices[1].name = AudioName::new("Speakers").expect("a short name");
    let (groups, _) = groups(Some(&reading));
    let FieldControl::Combo(chooser) = groups[0].rows()[0].control() else {
        panic!("the device is a choice list");
    };
    assert_eq!(chooser.choices(), ["Speakers", "Speakers (2)"]);
}

/// With no device the default, nothing names one and no controls are shown
/// for a device the direction does not use.
#[test]
fn a_direction_with_no_default_offers_only_its_choice() {
    let mut reading = machine();
    reading.devices[0].default = DefaultChoice::No;
    let (groups, owners) = groups(Some(&reading));
    assert_eq!(labels(&groups[0]), ["Device"]);
    assert_eq!(owners[0], [SoundRow::Device(StreamDirection::Playback)]);
    let FieldControl::Combo(chooser) = groups[0].rows()[0].control() else {
        panic!("the device is a choice list");
    };
    assert_eq!(chooser.selected(), None);
}

#[test]
fn an_unread_or_empty_machine_says_so() {
    let (unread, owners) = groups(None);
    assert_eq!(unread.len(), 1);
    assert!(matches!(
        unread[0].rows()[0].control(),
        FieldControl::Unmeasured(_)
    ));
    assert_eq!(owners, [vec![]]);
    let (groups, owners) = groups(Some(&SoundReading::default()));
    assert_eq!(captions(&groups), ["OUTPUT", "INPUT", "RECORDING"]);
    assert_eq!(reading_of(&groups[0].rows()[0]), "No output is connected.");
    assert_eq!(reading_of(&groups[1].rows()[0]), "No input is connected.");
    assert!(
        owners.iter().all(Vec::is_empty),
        "a statement changes nothing"
    );
}

/// The recording plate names what is recording, once, and says so when
/// nothing is.
#[test]
fn the_recording_plate_names_what_records_or_that_nothing_does() {
    let rows = |reading: &SoundReading| -> Vec<(String, String)> {
        let (groups, _) = groups(Some(reading));
        groups[2]
            .rows()
            .iter()
            .map(|row| (String::from(row.label()), String::from(reading_of(row))))
            .collect()
    };
    let mut reading = machine();
    assert_eq!(
        rows(&reading),
        [(String::from("Now"), String::from("Nothing is recording"))]
    );
    reading.captures.push(capture(40, MICROPHONE));
    reading.recording = 1;
    assert_eq!(
        rows(&reading),
        [(String::from("Program 40"), String::from("From Microphone"))],
        "nobody else's to count"
    );
    reading.recording = 3;
    assert_eq!(
        rows(&reading),
        [
            (String::from("Program 40"), String::from("From Microphone")),
            (String::from("Other users"), String::from("2 recordings")),
        ]
    );
    reading.captures.clear();
    reading.recording = 1;
    assert_eq!(
        rows(&reading),
        [(String::from("Other users"), String::from("1 recording"))]
    );
}

/// Choosing a device makes that device the default; a level moves live and
/// settles where it rests; a mute asks for exactly the mark it was given.
#[test]
fn each_control_asks_for_its_own_change() {
    let reading = machine();
    let output = SoundRow::Device(StreamDirection::Playback);
    assert_eq!(
        ask(output, &FieldAction::Selected { index: 1 }, &reading),
        Some(FormOutcome::Sound {
            device_id: HDMI,
            control: DeviceControl::Default,
            settled: true,
        })
    );
    assert_eq!(
        ask(output, &FieldAction::Selected { index: 2 }, &reading),
        None,
        "a choice the reading does not list asks nothing"
    );
    assert_eq!(
        ask(output, &FieldAction::Choices { expanded: true }, &reading),
        None
    );
    assert_eq!(
        ask(
            SoundRow::Level(SPEAKERS),
            &FieldAction::SetValue { permille: 500 },
            &reading
        ),
        Some(FormOutcome::Sound {
            device_id: SPEAKERS,
            control: DeviceControl::Level(level_at_permille(500)),
            settled: false,
        })
    );
    assert!(matches!(
        ask(
            SoundRow::Level(SPEAKERS),
            &FieldAction::Settled { permille: 500 },
            &reading
        ),
        Some(FormOutcome::Sound { settled: true, .. })
    ));
    assert_eq!(
        ask(
            SoundRow::Mute(SPEAKERS),
            &FieldAction::Set { on: true },
            &reading
        ),
        Some(FormOutcome::Sound {
            device_id: SPEAKERS,
            control: DeviceControl::Mute(true),
            settled: true,
        })
    );
}

/// What the pane shows before the service answers is what the service then
/// holds: one default per direction, the other direction left alone.
#[test]
fn a_change_is_shown_as_the_service_will_hold_it() {
    let mut reading = machine();
    assert!(apply(&mut reading, HDMI, DeviceControl::Default));
    let defaults: Vec<DefaultChoice> = reading
        .devices
        .iter()
        .map(|device| device.default)
        .collect();
    assert_eq!(
        defaults,
        [
            DefaultChoice::No,
            DefaultChoice::Preferred,
            DefaultChoice::Preferred
        ]
    );
    let level = AudioGain::new(-300).expect("attenuation");
    assert!(apply(&mut reading, SPEAKERS, DeviceControl::Level(level)));
    assert_eq!(
        (reading.devices[0].level, reading.devices[0].own_level),
        (level, true)
    );
    assert_eq!(
        reading.devices[1].level,
        AudioGain::new(-1_200).expect("attenuation"),
        "another device's level is its own"
    );
    assert!(apply(&mut reading, SPEAKERS, DeviceControl::Mute(true)));
    assert!(reading.devices[0].muted && !reading.devices[1].muted);
    assert!(!apply(&mut reading, 99, DeviceControl::Mute(true)));
}

#[test]
fn a_mute_touches_its_devices_volume_and_a_level_only_itself() {
    let (level, mute) = (SoundRow::Level(SPEAKERS), SoundRow::Mute(SPEAKERS));
    assert!(touches(mute, level) && touches(mute, mute));
    assert!(touches(level, level) && !touches(level, mute));
    assert!(!touches(mute, SoundRow::Level(HDMI)));
}

/// A mute press shows at once in the rows it touches, restated in place and
/// reported row by row; nothing else on the pane moves.
#[test]
fn a_mute_press_restates_its_rows_in_place() {
    let theme = theme();
    let mut shell = showing("sound");
    shell.adopt_sound(Some(machine()));
    let form = shell.form_mut_for_test().expect("a composed form");
    form.take_reshaped();
    let place = FormPlace {
        bounds: WIDE,
        viewport: WIDE,
        scale: Scale::ONE,
        theme: &theme,
    };
    let volume_rect = form.row_rect_for_test(0, 1, place).expect("the volume row");
    let (outcome, damage) =
        form.act_in_place_for_test((0, 2), FieldAction::Set { on: true }, place);
    assert_eq!(
        outcome,
        FormOutcome::Sound {
            device_id: SPEAKERS,
            control: DeviceControl::Mute(true),
            settled: true,
        }
    );
    assert!(!form.take_reshaped(), "nothing was built again");
    let rows = form.groups()[0].rows();
    assert!(matches!(rows[1].control(), FieldControl::Slider(slider) if slider.value() == 0));
    assert_eq!(rows[1].description(), Some("Muted"));
    assert!(!rows[1].state().enabled);
    assert!(matches!(
        rows[2].control(),
        FieldControl::Checkbox(mute) if mute.selection() == SelectionState::Selected
    ));
    assert!(covers(&damage, volume_rect), "the volume row is repainted");
    let input = form
        .row_rect_for_test(1, 1, place)
        .expect("the input's volume row");
    assert!(
        !damage.intersects(input),
        "the input plate is not repainted"
    );
}

/// Choosing another device brings that device's own controls.
#[test]
fn choosing_a_device_shows_its_controls_at_once() {
    let mut shell = showing("sound");
    let mut reading = machine();
    reading.devices[1].muted = true;
    shell.adopt_sound(Some(reading));
    let form = shell.form_mut_for_test().expect("a composed form");
    form.take_reshaped();
    let outcome = form.act_for_test(0, 0, FieldAction::Selected { index: 1 });
    assert_eq!(
        outcome,
        FormOutcome::Sound {
            device_id: HDMI,
            control: DeviceControl::Default,
            settled: true,
        }
    );
    assert!(form.take_reshaped(), "another device's rows");
    let rows = form.groups()[0].rows();
    assert!(
        matches!(rows[0].control(), FieldControl::Combo(chooser) if chooser.selected() == Some(1))
    );
    assert!(matches!(
        rows[2].control(),
        FieldControl::Checkbox(mute) if mute.selection() == SelectionState::Selected
    ));
}
