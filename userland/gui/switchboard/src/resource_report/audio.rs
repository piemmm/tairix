//! The Audio pane: each sink and source with its level, mute and measured
//! rate, and the live streams with their owners, positions and underruns
//! (`plans/SOUND.md` §Desktop integration).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

use tairix_abi::audio::{AudioDeviceDescriptor, StreamDescriptor, StreamState};
use tairix_abi::driver::audio::StreamDirection;
use tairix_controls::PressureKind;

use crate::sample::{DegradedField, Sample};
use crate::view::reading::{absence_statement, Reading, ReadingFact, Unmeasured};
use crate::view::resources::{
    BlockBody, DeviceId, PaneBlock, PaneHero, RailGroup, ResourceDevice, Trace,
};

/// The sound devices' rail entry and pane.
pub(super) fn device(sample: &Sample) -> ResourceDevice {
    let live = sample.audio_streams.as_ref().map(|streams| {
        streams
            .iter()
            .filter(|stream| stream.state == StreamState::Running)
            .count()
    });
    let reading = live.map_or_else(
        || Reading::Absent(absence(sample, DegradedField::AudioStreams)),
        |live| Reading::measured(count(live, "stream", "streams")),
    );
    ResourceDevice {
        id: DeviceId::Audio,
        group: RailGroup::Sound,
        name: String::from("Audio"),
        // Sound has no pressure colour of its own, and the pane draws no
        // instrument, so the tint only rims its blocks.
        kind: PressureKind::Accelerator,
        reading: reading.clone(),
        trend: Trace::Absent,
        hero: PaneHero::facts(reading, "playing or recording").with_context(context(sample)),
        blocks: alloc::vec![
            PaneBlock::half("OUTPUTS", endpoints(sample, StreamDirection::Playback)),
            PaneBlock::half("INPUTS", endpoints(sample, StreamDirection::Capture)),
            PaneBlock::full("STREAMS", streams(sample)),
        ],
        banner: None,
        actions: Vec::new(),
    }
}

fn absence(sample: &Sample, field: DegradedField) -> Unmeasured {
    Unmeasured::from_absence(sample.absence(field))
}

/// How many of each direction there are.
fn context(sample: &Sample) -> Vec<String> {
    let Some(devices) = sample.audio_devices.as_ref() else {
        return Vec::new();
    };
    let sinks = devices
        .iter()
        .filter(|device| device.direction == StreamDirection::Playback)
        .count();
    alloc::vec![format!(
        "{} · {}",
        count(sinks, "output", "outputs"),
        count(devices.len() - sinks, "input", "inputs")
    )]
}

/// One fact per sink or source of `direction`.
fn endpoints(sample: &Sample, direction: StreamDirection) -> BlockBody {
    let Some(devices) = sample.audio_devices.as_ref() else {
        return BlockBody::Absence(absence_statement(
            "the sound devices",
            absence(sample, DegradedField::AudioDevices),
        ));
    };
    let facts: Vec<ReadingFact> = devices
        .iter()
        .filter(|device| device.direction == direction)
        .map(|device| ReadingFact::text(device.name.as_str(), endpoint_line(device)))
        .collect();
    if facts.is_empty() {
        return BlockBody::Absence(String::from(match direction {
            StreamDirection::Playback => "No output is bound.",
            StreamDirection::Capture => "No input is bound.",
        }));
    }
    BlockBody::Facts(facts)
}

/// A device's level, mute, measured rate and losses, on one line.
fn endpoint_line(device: &AudioDeviceDescriptor) -> String {
    let mut line = format!("{}", device.level);
    if device.muted {
        line.push_str(" · muted");
    }
    if device.default.is_default() {
        line.push_str(" · default");
    }
    if device.clock_millihertz == 0 {
        line.push_str(" · stopped");
    } else {
        let _ = write!(
            line,
            " · {}.{:03} Hz",
            device.clock_millihertz / 1_000,
            device.clock_millihertz % 1_000
        );
    }
    if device.lost_frames > 0 {
        let _ = write!(line, " · {} frames lost", device.lost_frames);
    }
    line
}

/// One fact per stream: who owns it, where it plays, how far it is, and how
/// often it ran short.
fn streams(sample: &Sample) -> BlockBody {
    let Some(streams) = sample.audio_streams.as_ref() else {
        return BlockBody::Absence(absence_statement(
            "the sound streams",
            absence(sample, DegradedField::AudioStreams),
        ));
    };
    if streams.is_empty() {
        return BlockBody::Absence(String::from("Nothing is playing or recording."));
    }
    BlockBody::Facts(
        streams
            .iter()
            .map(|stream| ReadingFact::text(owner(stream), stream_line(sample, stream)))
            .collect(),
    )
}

fn owner(stream: &StreamDescriptor) -> String {
    stream.owner_app.as_ref().map_or_else(
        || format!("pid {}", stream.owner_pid),
        |app| String::from(app.as_str()),
    )
}

fn stream_line(sample: &Sample, stream: &StreamDescriptor) -> String {
    let device = sample
        .audio_devices
        .iter()
        .flatten()
        .find(|device| device.device_id == stream.device_id)
        .map_or_else(
            || format!("device {}", stream.device_id),
            |device| String::from(device.name.as_str()),
        );
    let verb = match stream.direction {
        StreamDirection::Playback => "plays on",
        StreamDirection::Capture => "records from",
    };
    format!(
        "{verb} {device} · {} · frame {} · {} underruns",
        stream.state.name(),
        stream.position.get(),
        stream.xruns
    )
}

fn count(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}
