//! Host tests for the remembered sound controls.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::audio::{
    AudioDeviceDescriptor, AudioGain, AudioLocation, ControlAccess, DefaultChoice,
};
use tairix_abi::driver::audio::{
    AudioName, ChannelMap, JackState, Rate, RateSupport, SampleFormats, StreamDirection,
};
use tairix_appconf::MAX_VALUE_LEN;
use tairix_audio::stream::DeviceControl;

use super::{SoundControls, REMEMBERED_LEVELS};

fn level(millibel: i32) -> AudioGain {
    AudioGain::new(millibel).expect("attenuation")
}

fn place(device: u64) -> AudioLocation {
    AudioLocation::new(device, 0).expect("a place")
}

fn device(device_id: u32, direction: StreamDirection, at: u64) -> AudioDeviceDescriptor {
    AudioDeviceDescriptor {
        device_id,
        direction,
        jack: JackState::Present,
        default: DefaultChoice::No,
        formats: SampleFormats::EMPTY,
        channel_map: ChannelMap::STEREO,
        rates: RateSupport::Continuous {
            min: Rate::HZ_48000,
            max: Rate::HZ_48000,
        },
        gain: None,
        name: AudioName::new("Device").expect("a short name"),
        location: place(at),
        level: AudioGain::UNITY,
        own_level: false,
        muted: false,
        access: ControlAccess::Own,
        clock_millihertz: 0,
        lost_frames: 0,
    }
}

#[test]
fn only_what_the_users_own_room_shows_is_kept() {
    let mut controls = SoundControls::default();
    let mut speakers = device(1, StreamDirection::Playback, 0x51);
    speakers.level = level(-900);
    assert!(
        !controls.observe(&[speakers]),
        "a level the machine's baseline gives is nobody's choice"
    );
    speakers.own_level = true;
    speakers.level = level(-600);
    speakers.muted = true;
    speakers.default = DefaultChoice::Preferred;
    let mut headset = device(2, StreamDirection::Capture, 0x52);
    headset.access = ControlAccess::Shared;
    headset.own_level = true;
    headset.muted = true;
    assert!(controls.observe(&[speakers, headset]));
    assert_eq!(controls.level(place(0x51)), Some(level(-600)));
    assert!(controls.muted(place(0x51)));
    assert_eq!(
        controls.preferred(StreamDirection::Playback),
        Some(place(0x51))
    );
    assert_eq!(
        controls.level(place(0x52)),
        None,
        "an unclaimed room is no user's"
    );
    assert!(!controls.muted(place(0x52)));
    assert!(!controls.observe(&[speakers, headset]), "nothing moved");

    let mut inherited = speakers;
    inherited.default = DefaultChoice::Inherited;
    inherited.muted = false;
    assert!(controls.observe(&[inherited]));
    assert!(!controls.muted(place(0x51)), "an unmute is kept too");
    assert_eq!(
        controls.preferred(StreamDirection::Playback),
        Some(place(0x51)),
        "a preference is never taken back by a default inherited"
    );
}

#[test]
fn a_claimed_room_is_put_back_to_what_the_user_set() {
    let mut controls = SoundControls::default();
    let mut remembered = device(1, StreamDirection::Playback, 0x51);
    remembered.own_level = true;
    remembered.level = level(-1_200);
    remembered.muted = true;
    remembered.default = DefaultChoice::Preferred;
    let mut microphone = device(4, StreamDirection::Capture, 0x53);
    microphone.default = DefaultChoice::Preferred;
    assert!(controls.observe(&[remembered, microphone]));

    let mut fresh = device(7, StreamDirection::Playback, 0x51);
    fresh.level = level(-900);
    let other = device(8, StreamDirection::Playback, 0x55);
    let microphone = device(9, StreamDirection::Capture, 0x53);
    assert_eq!(
        controls.restore(&[fresh, other, microphone]),
        vec![
            (7, DeviceControl::Level(level(-1_200))),
            (7, DeviceControl::Mute(true)),
            (7, DeviceControl::Default),
            (9, DeviceControl::Default),
        ]
    );
    assert!(
        controls.restore(&[remembered]).is_empty(),
        "a room already as the user left it needs nothing"
    );
    let mut shown = fresh;
    shown.access = ControlAccess::Shown;
    assert!(
        controls.restore(&[shown]).is_empty(),
        "never into another's room"
    );
}

#[test]
fn the_settings_values_round_trip_and_fail_closed() {
    let mut controls = SoundControls::default();
    let mut speakers = device(1, StreamDirection::Playback, 0x51);
    speakers.own_level = true;
    speakers.level = level(-650);
    speakers.muted = true;
    let mut monitor = device(2, StreamDirection::Playback, 0x5a);
    monitor.own_level = true;
    monitor.level = level(-300);
    monitor.default = DefaultChoice::Preferred;
    assert!(controls.observe(&[speakers, monitor]));
    let levels = controls.render_levels();
    assert_eq!(
        levels, "000000000000005a.0:-3dB 0000000000000051.0:-6.5dB",
        "the most recent first"
    );
    let mut read = SoundControls::default();
    assert!(read.set_levels(&levels));
    assert!(read.set_muted(&controls.render_muted()));
    assert!(read.set_output(&controls.render_output()));
    assert!(read.set_input(&controls.render_input()));
    assert_eq!(read, controls);

    for bad in [
        "51.0:-6dB",
        "0000000000000051.0",
        "0000000000000051.0:6dB",
        "0000000000000051.0:-6",
        "0000000000000051.0:-6dB 0000000000000051.0:-3dB",
    ] {
        assert!(!read.set_levels(bad), "{bad}");
    }
    assert!(!read.set_muted("0000000000000051.0 0000000000000051.0"));
    assert!(!read.set_output("speakers"));
    assert_eq!(read, controls, "a refusal changes nothing");
    assert!(read.set_output(""));
    assert_eq!(read.preferred(StreamDirection::Playback), None);
}

#[test]
fn the_least_recently_changed_level_goes_first_and_every_value_fits() {
    let mut controls = SoundControls::default();
    let widest = |index: usize| -> AudioDeviceDescriptor {
        let mut endpoint = device(1, StreamDirection::Playback, 1);
        endpoint.location = AudioLocation::new(u64::MAX - index as u64, 31).expect("a place");
        endpoint.own_level = true;
        endpoint.level = AudioGain::new(i32::MIN).expect("attenuation");
        endpoint.muted = true;
        endpoint
    };
    let devices: Vec<AudioDeviceDescriptor> = (0..=REMEMBERED_LEVELS).map(widest).collect();
    for endpoint in &devices {
        assert!(controls.observe(core::slice::from_ref(endpoint)));
    }
    assert_eq!(
        controls.level(devices[0].location),
        None,
        "the oldest gave way"
    );
    assert!(controls
        .level(devices[REMEMBERED_LEVELS].location)
        .is_some());
    let levels: String = controls.render_levels();
    assert!(levels.len() <= MAX_VALUE_LEN, "{}", levels.len());
    assert!(controls.render_muted().len() <= MAX_VALUE_LEN);
    let mut read = SoundControls::default();
    assert!(read.set_levels(&levels), "a full list reads back");
    assert!(!read.set_levels(&format!("{levels} 0000000000000001.0:-1dB")));
}
