//! A user's own sound controls, as their desktop session remembers them:
//! each endpoint's level and mute, and the sink and source they prefer as the
//! defaults, keyed by location (`plans/SOUND.md` §Desktop integration).
//!
//! The audio service holds what a session sets only while the session holds
//! the room or owns a stream; this is what outlives that. The session reads it
//! back from what the service shows its own tenancy, never from the machine's
//! baseline, and puts it back when it next claims the room.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

use tairix_abi::audio::{
    AudioDeviceDescriptor, AudioGain, AudioLocation, ControlAccess, DefaultChoice,
};
use tairix_abi::driver::audio::StreamDirection;
use tairix_appconf::MAX_VALUE_LEN;
use tairix_audio::stream::DeviceControl;

/// The most endpoint levels remembered: as many as one settings value can
/// spell at their longest. The least recently changed goes first.
pub const REMEMBERED_LEVELS: usize =
    MAX_VALUE_LEN / (AudioLocation::TEXT_MAX + 1 + AudioGain::TEXT_MAX + 1);

/// The most endpoints remembered muted, on the same terms.
pub const REMEMBERED_MUTES: usize = MAX_VALUE_LEN / (AudioLocation::TEXT_MAX + 1);

/// What a user set on the sound devices.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SoundControls {
    output: Option<AudioLocation>,
    input: Option<AudioLocation>,
    /// Most recently changed first.
    levels: Vec<(AudioLocation, AudioGain)>,
    /// Most recently muted first.
    muted: Vec<AudioLocation>,
}

impl SoundControls {
    /// The sink or source the user prefers as `direction`'s default.
    #[must_use]
    pub const fn preferred(&self, direction: StreamDirection) -> Option<AudioLocation> {
        match direction {
            StreamDirection::Playback => self.output,
            StreamDirection::Capture => self.input,
        }
    }

    /// The level the user set on the endpoint `at`.
    #[must_use]
    pub fn level(&self, at: AudioLocation) -> Option<AudioGain> {
        self.levels
            .iter()
            .find(|(place, _)| *place == at)
            .map(|(_, level)| *level)
    }

    /// Whether the user muted the endpoint `at`.
    #[must_use]
    pub fn muted(&self, at: AudioLocation) -> bool {
        self.muted.contains(&at)
    }

    /// Keep what `devices` show of the user's own room: each level the user
    /// set, each mute, and each preference. A device the room is not the
    /// user's for, or a level that is the machine's baseline, teaches nothing.
    /// Answers whether anything changed.
    pub fn observe(&mut self, devices: &[AudioDeviceDescriptor]) -> bool {
        let mut changed = false;
        for device in devices
            .iter()
            .filter(|device| device.access == ControlAccess::Own)
        {
            let at = device.location;
            if device.own_level && self.level(at) != Some(device.level) {
                self.levels.retain(|(place, _)| *place != at);
                self.levels.insert(0, (at, device.level));
                self.levels.truncate(REMEMBERED_LEVELS);
                changed = true;
            }
            if device.muted != self.muted(at) {
                self.muted.retain(|place| *place != at);
                if device.muted {
                    self.muted.insert(0, at);
                    self.muted.truncate(REMEMBERED_MUTES);
                }
                changed = true;
            }
            if device.default == DefaultChoice::Preferred {
                let preference = match device.direction {
                    StreamDirection::Playback => &mut self.output,
                    StreamDirection::Capture => &mut self.input,
                };
                if *preference != Some(at) {
                    *preference = Some(at);
                    changed = true;
                }
            }
        }
        changed
    }

    /// The controls that put `devices` back to what the user set, for a room
    /// that has just become theirs: levels and mutes first, then preferences.
    #[must_use]
    pub fn restore(&self, devices: &[AudioDeviceDescriptor]) -> Vec<(u32, DeviceControl)> {
        let mut controls = Vec::new();
        let ours = || devices.iter().filter(|device| device.access.may_change());
        for device in ours() {
            if let Some(level) = self.level(device.location) {
                if !device.own_level || device.level != level {
                    controls.push((device.device_id, DeviceControl::Level(level)));
                }
            }
            let muted = self.muted(device.location);
            if muted != device.muted {
                controls.push((device.device_id, DeviceControl::Mute(muted)));
            }
        }
        for device in ours() {
            if self.preferred(device.direction) == Some(device.location)
                && device.default != DefaultChoice::Preferred
            {
                controls.push((device.device_id, DeviceControl::Default));
            }
        }
        controls
    }

    /// The preferred sink as its settings value: its location, or empty.
    #[must_use]
    pub fn render_output(&self) -> String {
        render_place(self.output)
    }

    /// The preferred source as its settings value.
    #[must_use]
    pub fn render_input(&self) -> String {
        render_place(self.input)
    }

    /// Adopt the preferred sink `value` spells, answering whether it was one.
    #[must_use]
    pub fn set_output(&mut self, value: &str) -> bool {
        adopt_place(&mut self.output, value)
    }

    /// Adopt the preferred source `value` spells.
    #[must_use]
    pub fn set_input(&mut self, value: &str) -> bool {
        adopt_place(&mut self.input, value)
    }

    /// The levels as their one settings value: `<location>:<level>` pairs,
    /// most recently changed first, one space between.
    #[must_use]
    pub fn render_levels(&self) -> String {
        let mut text = String::new();
        for (at, level) in &self.levels {
            if !text.is_empty() {
                text.push(' ');
            }
            let _ = write!(text, "{at}:{level}");
        }
        text
    }

    /// Adopt the levels `value` spells, answering whether it was a list of
    /// them: each a location and a level in their one spelling, no location
    /// twice, and no more than are remembered. Unchanged on a refusal.
    #[must_use]
    pub fn set_levels(&mut self, value: &str) -> bool {
        let mut parsed: Vec<(AudioLocation, AudioGain)> = Vec::new();
        for entry in value.split_ascii_whitespace() {
            let Some((place, level)) = entry.split_once(':') else {
                return false;
            };
            let (Ok(place), Ok(level)) = (AudioLocation::parse(place), AudioGain::parse(level))
            else {
                return false;
            };
            if parsed.iter().any(|(seen, _)| *seen == place) {
                return false;
            }
            parsed.push((place, level));
        }
        if parsed.len() > REMEMBERED_LEVELS {
            return false;
        }
        self.levels = parsed;
        true
    }

    /// The muted endpoints as their one settings value: locations, most
    /// recently muted first, one space between.
    #[must_use]
    pub fn render_muted(&self) -> String {
        let mut text = String::new();
        for at in &self.muted {
            if !text.is_empty() {
                text.push(' ');
            }
            let _ = write!(text, "{at}");
        }
        text
    }

    /// Adopt the muted endpoints `value` spells, on the terms
    /// [`set_levels`](Self::set_levels) holds a list of levels to.
    #[must_use]
    pub fn set_muted(&mut self, value: &str) -> bool {
        let mut parsed: Vec<AudioLocation> = Vec::new();
        for entry in value.split_ascii_whitespace() {
            let Ok(place) = AudioLocation::parse(entry) else {
                return false;
            };
            if parsed.contains(&place) {
                return false;
            }
            parsed.push(place);
        }
        if parsed.len() > REMEMBERED_MUTES {
            return false;
        }
        self.muted = parsed;
        true
    }
}

fn render_place(place: Option<AudioLocation>) -> String {
    let mut text = String::new();
    if let Some(place) = place {
        let _ = write!(text, "{place}");
    }
    text
}

/// Adopt a preference's value: empty for none, else one location.
fn adopt_place(preference: &mut Option<AudioLocation>, value: &str) -> bool {
    if value.is_empty() {
        *preference = None;
        return true;
    }
    match AudioLocation::parse(value) {
        Ok(place) => {
            *preference = Some(place);
            true
        }
        Err(_) => false,
    }
}

#[cfg(test)]
#[path = "sound_tests.rs"]
mod tests;
