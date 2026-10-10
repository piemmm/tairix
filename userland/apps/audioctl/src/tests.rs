//! Host tests for `audioctl`: the command line, and each command against a
//! fake audio service and System Information API.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::RefCell;

use tairix_abi::audio::{
    AudioDeviceDescriptor, AudioGain, AudioLocation, ControlAccess, DefaultChoice,
    StreamDescriptor, StreamRole, StreamState,
};
use tairix_abi::driver::audio::{
    AudioName, ChannelMap, Frames, JackState, Rate, RateSupport, SampleFormats, StreamDirection,
};
use tairix_abi::sysinfo::{PageRequest, SysinfoQueryId, SysinfoRequestHeader};
use tairix_abi::Errno;
use tairix_audio::stream::DeviceControl;
use tairix_audio::target::{AudioTarget, TargetError};
use tairix_audio::volume::LevelError;
use tairix_help::{HelpSource, SourceError};
use tairix_procinfo::{Output, Transport, AUDIO_DEVICE_HEADER, AUDIO_STREAM_HEADER};

use super::{parse, run, Command, Failure, Sound, UsageError, USAGE};

fn level(millibel: i32) -> AudioGain {
    AudioGain::new(millibel).expect("attenuation")
}

fn target(text: &str) -> AudioTarget {
    AudioTarget::parse(text).expect("a target")
}

fn device(
    device_id: u32,
    direction: StreamDirection,
    place: u64,
    is_default: bool,
) -> AudioDeviceDescriptor {
    AudioDeviceDescriptor {
        device_id,
        direction,
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
        name: AudioName::new("Device").expect("a short name"),
        location: AudioLocation::new(place, 0).expect("a place"),
        level: AudioGain::UNITY,
        muted: false,
        own_level: false,
        access: ControlAccess::Shared,
        clock_millihertz: 0,
        lost_frames: 0,
    }
}

/// Two sinks, the second the default, and one source.
struct FakeSound {
    devices: Vec<AudioDeviceDescriptor>,
    set: Vec<(u32, DeviceControl)>,
    refuse: Option<Errno>,
}

impl FakeSound {
    fn new() -> Self {
        Self {
            devices: vec![
                device(1, StreamDirection::Playback, 0x51, false),
                device(2, StreamDirection::Playback, 0x52, true),
                device(3, StreamDirection::Capture, 0x53, true),
            ],
            set: Vec::new(),
            refuse: None,
        }
    }
}

impl Sound for FakeSound {
    fn devices(&mut self, direction: StreamDirection) -> Result<Vec<AudioDeviceDescriptor>, Errno> {
        Ok(self
            .devices
            .iter()
            .filter(|device| device.direction == direction)
            .copied()
            .collect())
    }

    fn set(&mut self, device_id: u32, control: DeviceControl) -> Result<(), Errno> {
        if let Some(err) = self.refuse {
            return Err(err);
        }
        self.set.push((device_id, control));
        Ok(())
    }
}

/// The System Information API, answering the caller's own streams and
/// refusing every principal's unless it holds the global grant.
struct FakeInfo {
    global: bool,
}

fn stream(stream_id: u64, owner_uid: u32) -> StreamDescriptor {
    StreamDescriptor {
        stream_id,
        device_id: 2,
        direction: StreamDirection::Playback,
        role: StreamRole::Media,
        state: StreamState::Running,
        position: Frames::new(4_800),
        xruns: 0,
        xrun_frames: 0,
        owner_uid,
        owner_pid: 40,
        owner_app: None,
    }
}

impl Transport for FakeInfo {
    fn query(&self, request: &[u8]) -> Result<Vec<u8>, Errno> {
        let header = SysinfoRequestHeader::from_bytes(request)?;
        let page = PageRequest::from_bytes(&request[SysinfoRequestHeader::WIRE_LEN..])?;
        let streams = match header.query {
            SysinfoQueryId::SELF_AUDIO_STREAMS => vec![stream(7, 1_000)],
            SysinfoQueryId::GLOBAL_AUDIO_STREAMS if self.global => {
                vec![stream(7, 1_000), stream(9, 1_001)]
            }
            SysinfoQueryId::GLOBAL_AUDIO_STREAMS => return Err(Errno::PermissionDenied),
            _ => return Err(Errno::NotImplemented),
        };
        Ok(streams
            .iter()
            .skip(page.offset as usize)
            .take(usize::from(page.limit))
            .flat_map(StreamDescriptor::to_le_bytes)
            .collect())
    }
}

struct NoHelp;

impl HelpSource for NoHelp {
    fn locale_dirs(&self) -> Result<Vec<String>, SourceError> {
        Ok(Vec::new())
    }

    fn read(&self, _: &str, _: &str) -> Result<Option<Vec<u8>>, SourceError> {
        Ok(None)
    }
}

#[derive(Default)]
struct Lines {
    lines: RefCell<Vec<String>>,
    records: RefCell<usize>,
}

impl Output for Lines {
    fn write_line(&self, line: &str) -> Result<(), Errno> {
        self.lines.borrow_mut().push(String::from(line));
        Ok(())
    }

    fn info(&self, _: &[u8]) {
        *self.records.borrow_mut() += 1;
    }
}

fn run_with(
    command: Command,
    sound: &mut FakeSound,
    info: &FakeInfo,
) -> (Result<(), Failure>, Lines) {
    let out = Lines::default();
    let result = run(command, None, sound, info, &NoHelp, &out);
    (result, out)
}

#[test]
fn a_bare_invocation_lists_the_devices_and_the_commands_take_their_operands() {
    assert_eq!(parse(&[]), Ok(Command::Devices));
    assert_eq!(parse(&["devices"]), Ok(Command::Devices));
    for args in [
        &["streams", "-a"][..],
        &["streams", "--all"],
        &["-a", "streams"],
    ] {
        assert_eq!(parse(args), Ok(Command::Streams { all: true }), "{args:?}");
    }
    assert_eq!(parse(&["streams"]), Ok(Command::Streams { all: false }));
    assert_eq!(
        parse(&["default", "audio:sink/3"]),
        Ok(Command::Set(target("audio:sink/3"), DeviceControl::Default))
    );
    for (args, millibel) in [
        (&["level", "audio:sink/default", "-6"][..], -600),
        (&["level", "audio:sink/default", "-3.5dB"], -350),
        (&["level", "--", "audio:sink/default", "-12"], -1_200),
        (&["level", "audio:sink/default", "0"], 0),
    ] {
        assert_eq!(
            parse(args),
            Ok(Command::Set(
                AudioTarget::DEFAULT_SINK,
                DeviceControl::Level(level(millibel))
            )),
            "{args:?}"
        );
    }
    assert_eq!(
        parse(&["mute", "audio:source/default"]),
        Ok(Command::Set(
            target("audio:source/default"),
            DeviceControl::Mute(true)
        ))
    );
    assert_eq!(
        parse(&["unmute", "audio:source/default"]),
        Ok(Command::Set(
            target("audio:source/default"),
            DeviceControl::Mute(false)
        ))
    );
    for switch in ["-h", "-?", "--help"] {
        assert_eq!(parse(&["level", switch]), Ok(Command::Help));
    }
    assert_eq!(parse(&["--version"]), Ok(Command::Version));
}

#[test]
fn a_malformed_command_line_says_what_is_wrong_with_it() {
    let cases: [(&[&str], UsageError); 10] = [
        (
            &["frobnicate"],
            UsageError::UnknownCommand(String::from("frobnicate")),
        ),
        (
            &["devices", "-x"],
            UsageError::UnknownOption(String::from("-x")),
        ),
        (
            &["devices", "-a"],
            UsageError::UnknownOption(String::from("-a")),
        ),
        (&["level"], UsageError::Missing("a device")),
        (&["level", "audio:sink/1"], UsageError::Missing("a level")),
        (
            &["level", "audio:sink/1", "3"],
            UsageError::Level(String::from("3"), LevelError::Boost),
        ),
        (
            &["level", "audio:sink/1", "loud"],
            UsageError::Level(String::from("loud"), LevelError::Malformed),
        ),
        (
            &["mute", "info:cpu/0"],
            UsageError::Device(String::from("info:cpu/0"), TargetError::NotAudio),
        ),
        (
            &["mute", "audio:sink/1", "now"],
            UsageError::Extra(String::from("now")),
        ),
        (&["devices", "now"], UsageError::Extra(String::from("now"))),
    ];
    for (args, error) in cases {
        assert_eq!(parse(args), Err(error), "{args:?}");
    }
}

#[test]
fn the_devices_are_listed_sinks_first_under_their_header() {
    let (result, out) = run_with(
        Command::Devices,
        &mut FakeSound::new(),
        &FakeInfo { global: false },
    );
    assert_eq!(result, Ok(()));
    let lines = out.lines.borrow();
    assert_eq!(lines.len(), 4);
    assert_eq!(lines[0], AUDIO_DEVICE_HEADER);
    assert!(lines[1].starts_with("sink    1 "), "{}", lines[1]);
    assert!(lines[2].starts_with("sink    2 "), "{}", lines[2]);
    assert!(lines[3].starts_with("source  3 "), "{}", lines[3]);
}

#[test]
fn a_control_reaches_the_device_its_target_names_now() {
    let mut sound = FakeSound::new();
    let info = FakeInfo { global: false };
    for (text, control, device_id) in [
        ("audio:sink/default", DeviceControl::Level(level(-600)), 2),
        ("audio:sink/1", DeviceControl::Default, 1),
        (
            "audio:sink/0000000000000051.0",
            DeviceControl::Mute(true),
            1,
        ),
        ("audio:source/default", DeviceControl::Mute(false), 3),
    ] {
        let (result, _) = run_with(Command::Set(target(text), control), &mut sound, &info);
        assert_eq!(result, Ok(()), "{text}");
        assert_eq!(sound.set.last(), Some(&(device_id, control)), "{text}");
    }
    // A source's identity never names a sink.
    let (result, _) = run_with(
        Command::Set(target("audio:sink/3"), DeviceControl::Default),
        &mut sound,
        &info,
    );
    assert_eq!(result, Err(Failure::NoDevice(target("audio:sink/3"))));
    assert_eq!(
        sound.set.len(),
        4,
        "nothing was sent for a device not there"
    );
}

#[test]
fn a_refused_control_is_reported_with_the_rooms_reason() {
    let mut sound = FakeSound::new();
    sound.refuse = Some(Errno::SeatNotOwner);
    let (result, _) = run_with(
        Command::Set(AudioTarget::DEFAULT_SINK, DeviceControl::Mute(true)),
        &mut sound,
        &FakeInfo { global: false },
    );
    let failure = result.expect_err("refused");
    assert_eq!(
        failure,
        Failure::Refused(AudioTarget::DEFAULT_SINK, Errno::SeatNotOwner)
    );
    let reason = alloc::format!("{failure}");
    assert!(reason.starts_with("audio:sink/default: "), "{reason}");
    assert!(reason.contains("session holding the room"), "{reason}");
}

#[test]
fn the_callers_own_streams_are_listed_and_every_principals_need_the_grant() {
    let (result, out) = run_with(
        Command::Streams { all: false },
        &mut FakeSound::new(),
        &FakeInfo { global: false },
    );
    assert_eq!(result, Ok(()));
    {
        let lines = out.lines.borrow();
        assert_eq!(lines[0], AUDIO_STREAM_HEADER);
        assert_eq!(lines.len(), 2);
        assert!(lines[1].starts_with("7 "), "{}", lines[1]);
    }
    assert_eq!(
        *out.records.borrow(),
        1,
        "the omission is stated on stdinfo"
    );

    let (result, out) = run_with(
        Command::Streams { all: true },
        &mut FakeSound::new(),
        &FakeInfo { global: true },
    );
    assert_eq!(result, Ok(()));
    assert_eq!(out.lines.borrow().len(), 3);
    assert_eq!(
        *out.records.borrow(),
        0,
        "an exhaustive listing omits nothing"
    );

    let (result, out) = run_with(
        Command::Streams { all: true },
        &mut FakeSound::new(),
        &FakeInfo { global: false },
    );
    let reason = alloc::format!("{}", result.expect_err("refused"));
    assert!(reason.contains("CAP_SYSINFO_GLOBAL"), "{reason}");
    assert!(
        out.lines.borrow().is_empty(),
        "a refusal is not an empty listing"
    );
}

#[test]
fn help_falls_back_to_the_usage_and_the_version_names_the_tool() {
    let (result, out) = run_with(
        Command::Help,
        &mut FakeSound::new(),
        &FakeInfo { global: false },
    );
    assert_eq!(result, Ok(()));
    assert_eq!(out.lines.borrow()[0], USAGE.trim_end_matches('\n'));
    let (result, out) = run_with(
        Command::Version,
        &mut FakeSound::new(),
        &FakeInfo { global: false },
    );
    assert_eq!(result, Ok(()));
    assert!(out.lines.borrow()[0].starts_with("audioctl (TAIRiX) "));
}

/// Every locale's `OPTIONS` names exactly the switches the parser accepts,
/// read from the bundle's own `Help/` tree, the one source the image plants.
#[test]
fn help_documents_the_parser_switches() {
    extern crate std;
    use alloc::format;
    use std::fs;

    const SWITCHES: [(&str, &[&str]); 3] = [
        ("`-a, --all`", &["-a", "--all"]),
        ("`-h, -?, --help`", &["-h", "-?", "--help"]),
        ("`--version`", &["--version"]),
    ];
    for (_, spellings) in SWITCHES {
        for spelling in spellings {
            assert!(parse(&["streams", spelling]).is_ok(), "{spelling}");
        }
    }
    let help_root = format!("{}/Help", env!("CARGO_MANIFEST_DIR"));
    for locale in tairix_help::REQUIRED_LOCALES {
        let path = format!("{help_root}/{locale}/audioctl.md");
        let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
        let documented: Vec<&str> = text
            .lines()
            .filter(|line| line.starts_with("- `-"))
            .filter_map(|line| line.split(" — ").next())
            .map(|key| key.trim_start_matches("- "))
            .collect();
        let pinned: Vec<&str> = SWITCHES.iter().map(|(key, _)| *key).collect();
        assert_eq!(documented, pinned, "{locale}/audioctl.md");
    }
}
