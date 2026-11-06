//! Host tests for the session's sound: coalescing, the claim edge, and what
//! is remembered when.

use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::audio::{
    AudioDeviceDescriptor, AudioGain, AudioLocation, ControlAccess, DefaultChoice,
};
use tairix_abi::driver::audio::{
    AudioName, ChannelMap, JackState, Rate, RateSupport, SampleFormats, StreamDirection,
};
use tairix_abi::Errno;
use tairix_audio::stream::DeviceControl;
use tairix_wallpaper::SoundControls;

use super::{SoundAnswer, SoundJob, SoundSession};

fn level(millibel: i32) -> AudioGain {
    AudioGain::new(millibel).expect("attenuation")
}

fn speakers(access: ControlAccess) -> AudioDeviceDescriptor {
    AudioDeviceDescriptor {
        device_id: 3,
        direction: StreamDirection::Playback,
        jack: JackState::Present,
        default: DefaultChoice::Inherited,
        formats: SampleFormats::EMPTY,
        channel_map: ChannelMap::STEREO,
        rates: RateSupport::Continuous {
            min: Rate::HZ_48000,
            max: Rate::HZ_48000,
        },
        gain: None,
        name: AudioName::new("Speakers").expect("a short name"),
        location: AudioLocation::new(0x51, 0).expect("a place"),
        level: level(-900),
        own_level: false,
        muted: false,
        access,
        clock_millihertz: 0,
        lost_frames: 0,
    }
}

fn listed(devices: Vec<AudioDeviceDescriptor>) -> SoundAnswer {
    SoundAnswer {
        refused: None,
        devices: Ok(devices),
    }
}

#[test]
fn one_round_trip_at_a_time_and_the_latest_of_each_kind_wins() {
    let mut sound = SoundSession::new();
    sound.refresh();
    assert_eq!(sound.next_job(), Some(SoundJob::default()));
    assert_eq!(sound.next_job(), None, "one in flight");
    for millibel in [-3_000, -2_000, -1_000] {
        sound.ask(3, DeviceControl::Level(level(millibel)), false);
    }
    sound.ask(3, DeviceControl::Mute(true), true);
    sound.ask(4, DeviceControl::Level(level(-500)), true);
    assert_eq!(sound.next_job(), None, "still in flight");
    let _ = sound.landed(
        listed(vec![speakers(ControlAccess::Shown)]),
        &SoundControls::default(),
    );
    assert_eq!(
        sound.next_job(),
        Some(SoundJob {
            controls: vec![
                (3, DeviceControl::Level(level(-1_000))),
                (3, DeviceControl::Mute(true)),
                (4, DeviceControl::Level(level(-500))),
            ],
        })
    );
}

#[test]
fn a_claimed_room_is_put_back_before_it_is_learnt_from() {
    let mut remembered = SoundControls::default();
    let mut set = speakers(ControlAccess::Own);
    set.own_level = true;
    set.level = level(-1_500);
    assert!(remembered.observe(&[set]));

    let mut sound = SoundSession::new();
    sound.refresh();
    let _ = sound.next_job();
    // Another session's room: nothing is restored and nothing learnt.
    let landed = sound.landed(listed(vec![speakers(ControlAccess::Shown)]), &remembered);
    assert_eq!(landed.remember, None);
    sound.refresh();
    assert_eq!(sound.next_job(), Some(SoundJob::default()));

    // The room becomes ours, showing the baseline: it is put back, not learnt.
    let landed = sound.landed(listed(vec![speakers(ControlAccess::Own)]), &remembered);
    assert_eq!(landed.remember, None, "a baseline is nobody's choice");
    assert_eq!(
        sound.next_job(),
        Some(SoundJob {
            controls: vec![(3, DeviceControl::Level(level(-1_500)))],
        })
    );
    let landed = sound.landed(listed(vec![set]), &remembered);
    assert_eq!(landed.remember, None, "already as remembered");
    let output = landed.shown.output.expect("the default sink");
    assert_eq!(output.level, level(-1_500));
    assert!(output.may_change);
}

#[test]
fn a_drag_is_remembered_where_it_settles() {
    let remembered = SoundControls::default();
    let mut sound = SoundSession::new();
    sound.refresh();
    let _ = sound.next_job();
    let _ = sound.landed(listed(vec![speakers(ControlAccess::Own)]), &remembered);
    let _ = sound.next_job();
    let _ = sound.landed(listed(vec![speakers(ControlAccess::Own)]), &remembered);

    let mut moving = speakers(ControlAccess::Own);
    moving.own_level = true;
    moving.level = level(-2_000);
    sound.ask(3, DeviceControl::Level(moving.level), false);
    let _ = sound.next_job();
    let landed = sound.landed(listed(vec![moving]), &remembered);
    assert_eq!(landed.remember, None, "mid-drag");

    moving.level = level(-1_200);
    sound.ask(3, DeviceControl::Level(moving.level), true);
    let _ = sound.next_job();
    let landed = sound.landed(listed(vec![moving]), &remembered);
    let kept = landed.remember.expect("remembered where it settled");
    assert_eq!(kept.level(moving.location), Some(level(-1_200)));
}

#[test]
fn the_bar_follows_the_default_sink_and_the_captures() {
    let mut sound = SoundSession::new();
    assert!(sound.captures(2).recording);
    let lost = sound.landed(
        SoundAnswer {
            refused: Some(Errno::SeatNotOwner),
            devices: Err(Errno::NotFound),
        },
        &SoundControls::default(),
    );
    assert_eq!(lost.shown.output, None, "no service, no output");
    assert!(lost.shown.recording, "the captures are their own notice's");
    assert_eq!(lost.refused, Some(Errno::SeatNotOwner));
    let mut other = speakers(ControlAccess::Shared);
    other.device_id = 5;
    other.default = DefaultChoice::No;
    let shown = sound
        .landed(
            listed(vec![other, speakers(ControlAccess::Shared)]),
            &SoundControls::default(),
        )
        .shown;
    assert_eq!(shown.output.map(|output| output.device_id), Some(3));
    assert!(!sound.captures(0).recording);
}

/// A drag is heard once: where it settles, once the round trip carrying that
/// level lists it applied; nothing on its way is.
#[test]
fn a_drag_is_heard_once_after_the_round_trip_that_applies_it() {
    let remembered = SoundControls::default();
    let mut sound = SoundSession::new();
    sound.refresh();
    let _ = sound.next_job();
    let _ = sound.landed(listed(vec![speakers(ControlAccess::Own)]), &remembered);

    let mut moving = speakers(ControlAccess::Own);
    moving.own_level = true;
    for millibel in [-2_000, -1_600] {
        moving.level = level(millibel);
        sound.ask(3, DeviceControl::Level(moving.level), false);
        let _ = sound.next_job();
        let landed = sound.landed(listed(vec![moving]), &remembered);
        assert!(!landed.confirmed, "mid-drag");
    }
    moving.level = level(-1_200);
    sound.ask(3, DeviceControl::Level(moving.level), true);
    assert!(sound.next_job().is_some(), "the settled level goes out");
    assert!(sound.landed(listed(vec![moving]), &remembered).confirmed);
    sound.refresh();
    let _ = sound.next_job();
    let again = sound.landed(listed(vec![moving]), &remembered);
    assert!(!again.confirmed, "heard once");
}

/// Only the user's own hand on the output is heard: not the controls put back
/// as the room is claimed, a refused change, a mute or an input's level; an
/// unmute is.
#[test]
fn a_restore_a_refusal_a_mute_and_an_input_are_not_heard() {
    let mut set = speakers(ControlAccess::Own);
    set.own_level = true;
    set.level = level(-1_500);
    let mut remembered = SoundControls::default();
    assert!(remembered.observe(&[set]));

    let mut sound = SoundSession::new();
    sound.refresh();
    let _ = sound.next_job();
    let _ = sound.landed(listed(vec![speakers(ControlAccess::Own)]), &remembered);
    assert!(sound.next_job().is_some(), "the remembered level put back");
    let restored = sound.landed(listed(vec![set]), &remembered);
    assert!(!restored.confirmed, "a restore is not the user's hand");

    sound.ask(3, DeviceControl::Level(level(-1_500)), true);
    let _ = sound.next_job();
    let refused = SoundAnswer {
        refused: Some(Errno::SeatNotOwner),
        devices: Ok(vec![set]),
    };
    assert!(!sound.landed(refused, &remembered).confirmed, "refused");

    let mut muted = set;
    muted.muted = true;
    sound.ask(3, DeviceControl::Mute(true), true);
    let _ = sound.next_job();
    assert!(!sound.landed(listed(vec![muted]), &remembered).confirmed);
    sound.ask(3, DeviceControl::Mute(false), true);
    let _ = sound.next_job();
    let unmuted = sound.landed(listed(vec![set]), &remembered);
    assert!(unmuted.confirmed, "an unmute is heard");

    let mut microphone = set;
    microphone.device_id = 7;
    microphone.direction = StreamDirection::Capture;
    sound.ask(7, DeviceControl::Level(level(-1_500)), true);
    let _ = sound.next_job();
    let input = sound.landed(listed(vec![set, microphone]), &remembered);
    assert!(!input.confirmed, "an input");
}
