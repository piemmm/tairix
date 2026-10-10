use alloc::format;
use core::num::NonZeroU32;

use tairix_abi::audio::{
    AudioDeviceDescriptor, AudioGain, AudioLocation, ControlAccess, DefaultChoice,
};
use tairix_abi::driver::audio::{
    AudioName, ChannelMap, JackState, Rate, RateSupport, SampleFormats, StreamDirection,
};

use super::{AudioDevice, AudioTarget, TargetError};

fn id(value: u32) -> AudioDevice {
    AudioDevice::Id(NonZeroU32::new(value).expect("nonzero"))
}

#[test]
fn the_four_shapes_parse_and_spell_back_the_same() {
    for (text, direction, device) in [
        (
            "audio:sink/default",
            StreamDirection::Playback,
            AudioDevice::Default,
        ),
        (
            "audio:source/default",
            StreamDirection::Capture,
            AudioDevice::Default,
        ),
        ("audio:sink/7", StreamDirection::Playback, id(7)),
        (
            "audio:source/4294967295",
            StreamDirection::Capture,
            id(u32::MAX),
        ),
        (
            "audio:sink/9f3a1c0042de7701.3",
            StreamDirection::Playback,
            AudioDevice::At(AudioLocation::new(0x9f3a_1c00_42de_7701, 3).expect("a place")),
        ),
    ] {
        let target = AudioTarget::parse(text).expect(text);
        assert_eq!(target, AudioTarget { direction, device });
        assert_eq!(format!("{target}"), text);
    }
}

#[test]
fn a_target_opens_on_the_device_the_stream_abi_names() {
    assert_eq!(AudioTarget::DEFAULT_SINK.device_id(), Some(0));
    assert_eq!(
        AudioTarget::parse("audio:sink/9").map(AudioTarget::device_id),
        Ok(Some(9))
    );
}

/// Two spellings of one device would let a pinned identity be dodged, and
/// zero is the default's own number, never a device's.
#[test]
fn an_identity_has_exactly_one_spelling() {
    for text in [
        "audio:sink/0",
        "audio:sink/07",
        "audio:sink/+7",
        "audio:sink/-7",
        "audio:sink/4294967296",
        "audio:sink/7x",
        "audio:sink/",
    ] {
        assert!(AudioTarget::parse(text).is_err(), "{text}");
    }
}

#[test]
fn nothing_but_a_sink_or_a_source_is_a_target() {
    for text in [
        "audio:sink",
        "audio:sink/default/left",
        "audio:speaker/default",
        "audio:sink/default@a1b2",
        "audio:sink/default::volume",
        "audio:sink/default?rate=48000",
    ] {
        assert_eq!(
            AudioTarget::parse(text),
            Err(TargetError::NoSuchTarget),
            "{text}"
        );
    }
    assert_eq!(
        AudioTarget::parse("disk:sink/default"),
        Err(TargetError::NotAudio)
    );
    assert!(matches!(
        AudioTarget::parse("speakers"),
        Err(TargetError::Malformed(_))
    ));
}

#[test]
fn an_enumerated_device_names_itself() {
    let descriptor = AudioDeviceDescriptor {
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
        level: AudioGain::UNITY,
        muted: false,
        own_level: false,
        access: ControlAccess::Shared,
        clock_millihertz: 0,
        lost_frames: 0,
    };
    assert_eq!(format!("{}", AudioTarget::of(&descriptor)), "audio:sink/3");
}

fn sink(device_id: u32, location: u64, is_default: bool) -> AudioDeviceDescriptor {
    AudioDeviceDescriptor {
        device_id,
        direction: StreamDirection::Playback,
        jack: JackState::Present,
        default: if is_default {
            DefaultChoice::Inherited
        } else {
            DefaultChoice::No
        },
        formats: SampleFormats::EMPTY,
        channel_map: ChannelMap::STEREO,
        rates: RateSupport::Continuous {
            min: Rate::HZ_48000,
            max: Rate::HZ_48000,
        },
        gain: None,
        name: AudioName::new("Speakers").expect("a short name"),
        location: AudioLocation::new(location, 0).expect("a place"),
        level: AudioGain::UNITY,
        muted: false,
        own_level: false,
        access: ControlAccess::Shared,
        clock_millihertz: 0,
        lost_frames: 0,
    }
}

#[test]
fn a_target_resolves_to_the_device_it_names_now() {
    let devices = [sink(1, 0x51, true), sink(2, 0x52, false)];
    let default = AudioTarget::DEFAULT_SINK
        .resolve(&devices)
        .expect("a default");
    assert_eq!(default.device_id, 1);
    let by_id = AudioTarget::parse("audio:sink/2").expect("a target");
    assert_eq!(by_id.resolve(&devices).map(|d| d.device_id), Some(2));
    assert_eq!(by_id.device_id(), Some(2));
    // A source target never resolves to a sink.
    let source = AudioTarget::parse("audio:source/2").expect("a target");
    assert!(source.resolve(&devices).is_none());
}

/// An identity is a boot's; a kept target names the place, so the device is
/// found again when it comes back under another identity.
#[test]
fn a_kept_target_finds_its_device_under_a_new_identity() {
    let before = [sink(1, 0x51, true), sink(2, 0x52, false)];
    let kept = AudioTarget::at(&before[1]);
    assert_eq!(kept.device_id(), None, "a place is resolved, not assumed");
    let spelled = format!("{kept}");
    let after = [sink(5, 0x52, false), sink(6, 0x51, true)];
    let back = AudioTarget::parse(&spelled).expect("its own spelling");
    assert_eq!(back.resolve(&after).map(|d| d.device_id), Some(5));
    // The identity it had would now name the other device.
    let stale = AudioTarget::of(&before[1]);
    assert_ne!(
        stale.resolve(&after).map(|d| d.location),
        Some(before[1].location)
    );
}
