//! The Sound pane (`plans/SOUND.md` §Desktop integration): which output and
//! which input the desktop uses, the chosen device's volume and mute, and
//! what is recording.
//!
//! The controls act on the audio service directly. It admits them for the
//! session holding the room the devices serve, so this application holds no
//! authority over them; a device another session's room holds is shown and
//! cannot be changed here. What the user sets is remembered by their desktop
//! session, which follows every change the service announces.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::audio::{AudioDeviceDescriptor, DefaultChoice, StreamDescriptor, StreamState};
use tairix_abi::driver::audio::StreamDirection;
use tairix_audio::stream::DeviceControl;
use tairix_audio::volume::{control_permille, level_at_permille};
use tairix_controls::{
    AuthorityState, Checkbox, ComboBox, ControlState, FieldAction, FieldControl, FieldGroup,
    FieldRow, SelectionState, Slider,
};

use crate::form::FormOutcome;

/// What a reader may search for to reach the pane: its devices are
/// discovered, so the index names the subject rather than each device.
pub(crate) const SOUND_FACTS: [&str; 8] = [
    "Output device",
    "Input device",
    "Default",
    "Volume",
    "Mute",
    "Speakers",
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

/// Which control of the sound devices a row changes.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum SoundRow {
    /// Which device is the direction's default.
    Device(StreamDirection),
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
    let mut groups = Vec::with_capacity(3);
    let mut owners = Vec::with_capacity(3);
    for direction in [StreamDirection::Playback, StreamDirection::Capture] {
        let (group, rows) = direction_group(reading, direction);
        groups.push(group);
        owners.push(rows);
    }
    groups.push(recording_group(reading));
    owners.push(Vec::new());
    (groups, owners)
}

/// One direction's plate: which device it uses, then that device's volume
/// and mute.
fn direction_group(
    reading: &SoundReading,
    direction: StreamDirection,
) -> (FieldGroup, Vec<SoundRow>) {
    let (caption, none) = match direction {
        StreamDirection::Playback => ("OUTPUT", "No output is connected."),
        StreamDirection::Capture => ("INPUT", "No input is connected."),
    };
    let mut wanted = alloc::vec![SoundRow::Device(direction)];
    if let Some(chosen) = of(reading, direction).find(|device| device.default.is_default()) {
        wanted.extend([
            SoundRow::Level(chosen.device_id),
            SoundRow::Mute(chosen.device_id),
        ]);
    }
    let (rows, owners): (Vec<FieldRow>, Vec<SoundRow>) = wanted
        .into_iter()
        .filter_map(|owner| Some((row(owner, reading)?, owner)))
        .unzip();
    if rows.is_empty() {
        return (
            FieldGroup::new(
                caption,
                alloc::vec![FieldRow::new(
                    "Device",
                    FieldControl::Reading(String::from(none))
                )],
            ),
            Vec::new(),
        );
    }
    (FieldGroup::new(caption, rows), owners)
}

/// The row `owner` changes, as `reading` has it, or `None` where the device
/// it names is not listed.
///
/// The one definition of each row, so a plate built afresh and a row brought
/// in line in place cannot disagree.
pub(crate) fn row(owner: SoundRow, reading: &SoundReading) -> Option<FieldRow> {
    match owner {
        SoundRow::Device(direction) => {
            let devices: Vec<&AudioDeviceDescriptor> = of(reading, direction).collect();
            if devices.is_empty() {
                return None;
            }
            let chooser = ComboBox::new(names(&devices));
            let chooser = match devices
                .iter()
                .position(|device| device.default.is_default())
            {
                Some(at) => chooser.with_selected(at),
                None => chooser.with_placeholder("None chosen"),
            };
            Some(
                FieldRow::new("Device", FieldControl::Combo(chooser)).with_state(access(
                    devices.iter().all(|device| device.access.may_change()),
                )),
            )
        }
        SoundRow::Level(id) => {
            let device = device(reading, id)?;
            let mut state = access(device.access.may_change());
            if device.muted {
                state = state.with_enabled(false);
            }
            Some(
                FieldRow::new(
                    "Volume",
                    FieldControl::Slider(Slider::new(control_permille(device.level, device.muted))),
                )
                .with_description(if device.muted {
                    String::from("Muted")
                } else {
                    format!("{}", device.level)
                })
                .with_state(state),
            )
        }
        SoundRow::Mute(id) => {
            let device = device(reading, id)?;
            Some(
                FieldRow::new(
                    "Mute",
                    FieldControl::Checkbox(Checkbox::new("", SelectionState::of(device.muted))),
                )
                .with_state(access(device.access.may_change())),
            )
        }
    }
}

/// Whether `row` shows what a change to `changed`'s control moves: a mute
/// silences its device's volume as well as marking itself.
pub(crate) const fn touches(changed: SoundRow, row: SoundRow) -> bool {
    match (changed, row) {
        (SoundRow::Level(id), SoundRow::Level(other))
        | (SoundRow::Mute(id), SoundRow::Level(other) | SoundRow::Mute(other)) => id == other,
        _ => false,
    }
}

/// What `row`'s `action` asks of the audio service over `reading`, or `None`
/// for nothing: a list opening, a choice the reading no longer lists. A level
/// moves the device live as it is dragged and settles where it rests.
pub(crate) fn ask(
    row: SoundRow,
    action: &FieldAction,
    reading: &SoundReading,
) -> Option<FormOutcome> {
    let (device_id, control, settled) = match (row, action) {
        (SoundRow::Device(direction), FieldAction::Selected { index }) => (
            of(reading, direction).nth(*index)?.device_id,
            DeviceControl::Default,
            true,
        ),
        (SoundRow::Level(id), FieldAction::SetValue { permille }) => (
            id,
            DeviceControl::Level(level_at_permille(*permille)),
            false,
        ),
        (SoundRow::Level(id), FieldAction::Settled { permille }) => {
            (id, DeviceControl::Level(level_at_permille(*permille)), true)
        }
        (SoundRow::Mute(id), FieldAction::Set { on }) => (id, DeviceControl::Mute(*on), true),
        _ => return None,
    };
    Some(FormOutcome::Sound {
        device_id,
        control,
        settled,
    })
}

/// Show `control` on `device_id` in `reading` as the audio service will hold
/// it once made, answering whether the device is listed.
pub(crate) fn apply(reading: &mut SoundReading, device_id: u32, control: DeviceControl) -> bool {
    let Some(direction) = device(reading, device_id).map(|device| device.direction) else {
        return false;
    };
    for device in reading
        .devices
        .iter_mut()
        .filter(|device| device.direction == direction)
    {
        let own = device.device_id == device_id;
        match control {
            DeviceControl::Default => {
                device.default = if own {
                    DefaultChoice::Preferred
                } else {
                    DefaultChoice::No
                };
            }
            DeviceControl::Level(level) if own => {
                device.level = level;
                device.own_level = true;
            }
            DeviceControl::Mute(muted) if own => device.muted = muted,
            DeviceControl::Level(_) | DeviceControl::Mute(_) => {}
        }
    }
    true
}

/// The devices of `direction`, in the order the service lists them.
fn of(
    reading: &SoundReading,
    direction: StreamDirection,
) -> impl Iterator<Item = &AudioDeviceDescriptor> {
    reading
        .devices
        .iter()
        .filter(move |device| device.direction == direction)
}

fn device(reading: &SoundReading, id: u32) -> Option<&AudioDeviceDescriptor> {
    reading.devices.iter().find(|device| device.device_id == id)
}

/// What each of `devices` is called in its chooser: its own name, with a
/// count after a name an earlier device already has, so two alike can be
/// told apart.
fn names(devices: &[&AudioDeviceDescriptor]) -> Vec<String> {
    let mut seen: BTreeMap<&str, u32> = BTreeMap::new();
    devices
        .iter()
        .map(|device| {
            let name = device.name.as_str();
            let count = seen.entry(name).or_insert(0);
            *count += 1;
            match *count {
                1 => String::from(name),
                count => format!("{name} ({count})"),
            }
        })
        .collect()
}

/// The state a row changing a device takes: actionable where the caller may
/// change it, else refused under the Authority Mark.
fn access(may_change: bool) -> ControlState {
    if may_change {
        ControlState::idle()
    } else {
        ControlState::idle()
            .with_enabled(false)
            .with_authority(AuthorityState::Denied)
    }
}

/// What is recording: this user's own captures by name, then how many of
/// anyone else's — or that nothing is.
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
            let from = device(reading, stream.device_id).map_or_else(
                || format!("device {}", stream.device_id),
                |device| String::from(device.name.as_str()),
            );
            FieldRow::new(owner, FieldControl::Reading(format!("From {from}")))
        })
        .collect();
    let others = usize::try_from(reading.recording)
        .unwrap_or(usize::MAX)
        .saturating_sub(rows.len());
    match others {
        0 if rows.is_empty() => rows.push(FieldRow::new(
            "Now",
            FieldControl::Reading(String::from("Nothing is recording")),
        )),
        0 => {}
        1 => rows.push(FieldRow::new(
            "Other users",
            FieldControl::Reading(String::from("1 recording")),
        )),
        many => rows.push(FieldRow::new(
            "Other users",
            FieldControl::Reading(format!("{many} recordings")),
        )),
    }
    FieldGroup::new("RECORDING", rows)
}

#[cfg(test)]
#[path = "sound_tests.rs"]
mod tests;
