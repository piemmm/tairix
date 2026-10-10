//! Host tests for the Sound pane: the plates a reading builds, and what each
//! control asks of the audio service.

use alloc::vec;

use tairix_abi::audio::{
    AudioDeviceDescriptor, AudioGain, AudioLocation, ControlAccess, DefaultChoice,
    StreamDescriptor, StreamRole, StreamState,
};
use tairix_abi::driver::audio::{
    AudioName, ChannelMap, Frames, JackState, Rate, RateSupport, SampleFormats, StreamDirection,
};
use tairix_audio::stream::DeviceControl;
use tairix_audio::volume::{level_at_permille, permille_of_level};
use tairix_controls::{FieldAction, FieldControl, FieldGroup, FieldRow};

use super::{groups, SoundReading, SoundRow};
use crate::form::{sound_outcome, FormOutcome};

fn device(
    device_id: u32,
    direction: StreamDirection,
    name: &str,
    access: ControlAccess,
) -> AudioDeviceDescriptor {
    AudioDeviceDescriptor {
        device_id,
        direction,
        jack: JackState::Present,
        default: DefaultChoice::Inherited,
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
        muted: true,
        access,
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

#[test]
fn a_plate_per_device_with_its_controls_and_what_is_recording() {
    let reading = SoundReading {
        devices: vec![
            device(1, StreamDirection::Playback, "Speakers", ControlAccess::Own),
            device(
                2,
                StreamDirection::Capture,
                "Microphone",
                ControlAccess::Shown,
            ),
        ],
        captures: vec![capture(40, 2)],
        recording: 3,
    };
    let (groups, owners) = groups(Some(&reading));
    let captions: alloc::vec::Vec<&str> = groups.iter().map(FieldGroup::caption).collect();
    assert_eq!(
        captions,
        ["OUTPUT · Speakers", "INPUT · Microphone", "RECORDING"]
    );
    assert_eq!(
        owners[0],
        [SoundRow::Default(1), SoundRow::Level(1), SoundRow::Mute(1)]
    );
    assert_eq!(owners[2], [], "the recording plate changes nothing");
    let speakers = groups[0].rows();
    assert!(matches!(speakers[0].control(), FieldControl::Toggle(toggle) if toggle.is_on()));
    let FieldControl::Slider(slider) = speakers[1].control() else {
        panic!("the level is a slider");
    };
    assert_eq!(
        slider.value(),
        permille_of_level(AudioGain::new(-1_200).expect("attenuation"))
    );
    assert!(matches!(speakers[2].control(), FieldControl::Toggle(toggle) if toggle.is_on()));
    assert!(
        speakers.iter().all(|row| row.state().enabled),
        "the user's own room"
    );
    assert!(
        groups[1].rows().iter().all(|row| !row.state().enabled),
        "another session's room is shown, not changed"
    );
    let recording: alloc::vec::Vec<&str> = groups[2].rows().iter().map(FieldRow::label).collect();
    assert_eq!(recording, ["Program 40", "Other users"]);
    assert!(matches!(
        groups[2].rows()[1].control(),
        FieldControl::Reading(text) if text == "2 recordings"
    ));
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
    let (groups, _) = groups(Some(&SoundReading::default()));
    let captions: alloc::vec::Vec<&str> = groups.iter().map(FieldGroup::caption).collect();
    assert_eq!(captions, ["OUTPUT", "INPUT", "RECORDING"]);
    assert!(matches!(
        groups[0].rows()[0].control(),
        FieldControl::Reading(text) if text == "No output is connected."
    ));
}

#[test]
fn a_level_moves_live_and_settles_and_a_default_is_never_unchosen() {
    let live = sound_outcome(SoundRow::Level(3), FieldAction::SetValue { permille: 500 });
    assert_eq!(
        live,
        FormOutcome::Sound {
            device_id: 3,
            control: DeviceControl::Level(level_at_permille(500)),
            settled: false,
        }
    );
    assert!(matches!(
        sound_outcome(SoundRow::Level(3), FieldAction::Settled { permille: 500 }),
        FormOutcome::Sound { settled: true, .. }
    ));
    assert_eq!(
        sound_outcome(SoundRow::Mute(3), FieldAction::Set { on: false }),
        FormOutcome::Sound {
            device_id: 3,
            control: DeviceControl::Mute(false),
            settled: true,
        }
    );
    assert_eq!(
        sound_outcome(SoundRow::Default(3), FieldAction::Set { on: true }),
        FormOutcome::Sound {
            device_id: 3,
            control: DeviceControl::Default,
            settled: true,
        }
    );
    assert_eq!(
        sound_outcome(SoundRow::Default(3), FieldAction::Set { on: false }),
        FormOutcome::Changed,
        "a direction always has a default"
    );
}
