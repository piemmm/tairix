//! The Sound pane (`plans/SOUND.md` §Desktop integration): one plate per
//! sink and source with its default choice, level and mute, and a plate of
//! what is recording.
//!
//! The controls act on the audio service directly. It admits them for the
//! session holding the room the devices serve, so this application holds no
//! authority over them; a device another session's room holds is shown and
//! cannot be changed here. What the user sets is remembered by their desktop
//! session, which follows every change the service announces.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::audio::{AudioDeviceDescriptor, StreamDescriptor, StreamState};
use tairix_abi::driver::audio::StreamDirection;
use tairix_audio::volume::permille_of_level;
use tairix_controls::{
    AuthorityState, ControlState, FieldControl, FieldGroup, FieldRow, Slider, Toggle,
};

/// What a reader may search for to reach the pane: its plates are
/// discovered, so the index names the subject rather than each device.
pub(crate) const SOUND_FACTS: [&str; 7] = [
    "Volume",
    "Output",
    "Input",
    "Default",
    "Mute",
    "Microphone",
    "Recording",
];

/// What the pane is built from.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SoundReading {
    /// The sinks, then the sources, as the audio service shows them to this
    /// session.
    pub devices: Vec<AudioDeviceDescriptor>,
    /// This user's own capture streams.
    pub captures: Vec<StreamDescriptor>,
    /// Capture streams moving frames on the machine, everyone's.
    pub recording: u32,
}

/// Which control of which device a row changes.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum SoundRow {
    /// Make the device its direction's default.
    Default(u32),
    /// The device's level.
    Level(u32),
    /// The device's mute.
    Mute(u32),
}

/// The pane's plates and the row each control changes; a reading row
/// changes nothing.
pub(crate) fn groups(reading: Option<&SoundReading>) -> (Vec<FieldGroup>, Vec<Vec<SoundRow>>) {
    let Some(reading) = reading else {
        return (
            alloc::vec![FieldGroup::new(
                "SOUND",
                alloc::vec![FieldRow::new(
                    "Devices",
                    FieldControl::Unmeasured(String::from("not read yet")),
                )],
            )],
            alloc::vec![Vec::new()],
        );
    };
    let mut groups = Vec::new();
    let mut owners = Vec::new();
    for direction in [StreamDirection::Playback, StreamDirection::Capture] {
        let mut any = false;
        for device in reading
            .devices
            .iter()
            .filter(|device| device.direction == direction)
        {
            any = true;
            let (group, rows) = device_group(device);
            groups.push(group);
            owners.push(rows);
        }
        if !any {
            let (caption, none) = match direction {
                StreamDirection::Playback => ("OUTPUT", "No output is connected."),
                StreamDirection::Capture => ("INPUT", "No input is connected."),
            };
            groups.push(FieldGroup::new(
                caption,
                alloc::vec![FieldRow::new(
                    "Devices",
                    FieldControl::Reading(String::from(none))
                )],
            ));
            owners.push(Vec::new());
        }
    }
    groups.push(recording_group(reading));
    owners.push(Vec::new());
    (groups, owners)
}

/// One device's plate: its default choice, its level and its mute.
fn device_group(device: &AudioDeviceDescriptor) -> (FieldGroup, Vec<SoundRow>) {
    let kind = match device.direction {
        StreamDirection::Playback => "OUTPUT",
        StreamDirection::Capture => "INPUT",
    };
    let state = if device.access.may_change() {
        ControlState::idle()
    } else {
        ControlState::idle()
            .with_enabled(false)
            .with_authority(AuthorityState::Denied)
    };
    let rows = alloc::vec![
        FieldRow::new(
            "Default",
            FieldControl::Toggle(Toggle::new("", device.default.is_default())),
        )
        .with_state(state),
        FieldRow::new(
            "Level",
            FieldControl::Slider(Slider::new(permille_of_level(device.level))),
        )
        .with_description(format!("{}", device.level))
        .with_state(state),
        FieldRow::new("Mute", FieldControl::Toggle(Toggle::new("", device.muted)))
            .with_state(state),
    ];
    let id = device.device_id;
    (
        FieldGroup::new(format!("{kind} · {}", device.name.as_str()), rows),
        alloc::vec![
            SoundRow::Default(id),
            SoundRow::Level(id),
            SoundRow::Mute(id)
        ],
    )
}

/// What is recording: this user's own captures by name, and how many of
/// anyone else's.
fn recording_group(reading: &SoundReading) -> FieldGroup {
    let mut rows: Vec<FieldRow> = reading
        .captures
        .iter()
        .filter(|stream| stream.state == StreamState::Running)
        .map(|stream| {
            let owner = stream.owner_app.as_ref().map_or_else(
                || format!("Program {}", stream.owner_pid),
                |app| String::from(app.as_str()),
            );
            let from = reading
                .devices
                .iter()
                .find(|device| device.device_id == stream.device_id)
                .map_or_else(
                    || format!("device {}", stream.device_id),
                    |device| String::from(device.name.as_str()),
                );
            FieldRow::new(
                owner,
                FieldControl::Reading(format!("recording from {from}")),
            )
        })
        .collect();
    let yours = rows.len();
    if yours == 0 {
        rows.push(FieldRow::new(
            "Yours",
            FieldControl::Reading(String::from("nothing recording")),
        ));
    }
    let others = usize::try_from(reading.recording)
        .unwrap_or(usize::MAX)
        .saturating_sub(yours);
    rows.push(FieldRow::new(
        "Other users",
        FieldControl::Reading(match others {
            0 => String::from("nothing recording"),
            1 => String::from("1 recording"),
            many => format!("{many} recordings"),
        }),
    ));
    FieldGroup::new("RECORDING", rows)
}

#[cfg(test)]
#[path = "sound_tests.rs"]
mod tests;
