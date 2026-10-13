//! The sound document: whether the desktop makes sounds, the theme, how far
//! its sounds are turned down, and each event's switch and replacement file.
//!
//! The same registry is read at two layers — the machine's document beneath,
//! the user's own choices over it while their session holds the seat — read
//! tolerantly from a store ([`Registry::load`]) and strictly from a channel
//! ([`merge_pushed`](crate::user::merge_pushed)).

use alloc::string::String;
use core::fmt::{self, Write as _};

use tairix_abi::audio::AudioGain;
use tairix_appconf::{as_bool, bool_text, Registry, SettingPath};

use crate::catalog::{is_theme_name, DEFAULT_THEME};
use crate::SoundEvent;

/// One setting of the sound document.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SoundKey {
    /// `sound.enabled` — whether the desktop makes any sound.
    Enabled,
    /// `sound.theme` — the theme sounds are drawn from.
    Theme,
    /// `sound.gain` — how far every sound is turned down.
    Gain,
    /// `sound.<event>.enabled` — whether the event makes its sound.
    EventEnabled(SoundEvent),
    /// `sound.<event>.file` — a file the user chose to play instead; an
    /// empty value clears it.
    EventFile(SoundEvent),
}

const EVENTS: usize = SoundEvent::ALL.len();

/// The keys every event has.
const PER_EVENT: usize = 2;

/// Every key: the three that hold for every sound, then each event's own.
const KEYS: [SoundKey; 3 + PER_EVENT * EVENTS] = {
    let mut keys = [SoundKey::Enabled; 3 + PER_EVENT * EVENTS];
    keys[1] = SoundKey::Theme;
    keys[2] = SoundKey::Gain;
    let mut at = 0;
    while at < EVENTS {
        let event = SoundEvent::ALL[at];
        keys[3 + PER_EVENT * at] = SoundKey::EventEnabled(event);
        keys[4 + PER_EVENT * at] = SoundKey::EventFile(event);
        at += 1;
    }
    keys
};

/// Each event's keys — switch, file — in [`SoundEvent::ALL`] order.
const EVENT_KEY_NAMES: [[&str; PER_EVENT]; EVENTS] = [
    ["sound.startup.enabled", "sound.startup.file"],
    ["sound.shutdown.enabled", "sound.shutdown.file"],
    ["sound.login.enabled", "sound.login.file"],
    ["sound.logout.enabled", "sound.logout.file"],
    [
        "sound.device_attached.enabled",
        "sound.device_attached.file",
    ],
    [
        "sound.device_detached.enabled",
        "sound.device_detached.file",
    ],
    ["sound.notification.enabled", "sound.notification.file"],
    [
        "sound.notification_message.enabled",
        "sound.notification_message.file",
    ],
    [
        "sound.notification_reminder.enabled",
        "sound.notification_reminder.file",
    ],
    ["sound.error.enabled", "sound.error.file"],
    ["sound.warning.enabled", "sound.warning.file"],
    ["sound.info.enabled", "sound.info.file"],
    ["sound.bell.enabled", "sound.bell.file"],
];

impl fmt::Display for SoundKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(SoundSettings::name(*self))
    }
}

/// One event's settings.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EventSettings {
    /// Whether the event makes its sound.
    pub enabled: bool,
    /// A file the user chose to play instead of the theme's.
    pub file: Option<SettingPath>,
}

impl Default for EventSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            file: None,
        }
    }
}

/// The sound settings one document implies.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SoundSettings {
    enabled: bool,
    theme: String,
    gain: AudioGain,
    events: [EventSettings; EVENTS],
}

impl Default for SoundSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            theme: String::from(DEFAULT_THEME),
            gain: AudioGain::UNITY,
            events: core::array::from_fn(|_| EventSettings::default()),
        }
    }
}

impl SoundSettings {
    /// Whether the desktop makes any sound.
    #[must_use]
    pub const fn enabled(&self) -> bool {
        self.enabled
    }

    /// The theme sounds are drawn from.
    #[must_use]
    pub fn theme(&self) -> &str {
        &self.theme
    }

    /// How far every sound is turned down; unity plays each asset bit-exact.
    #[must_use]
    pub const fn gain(&self) -> AudioGain {
        self.gain
    }

    /// `event`'s own settings.
    #[must_use]
    pub const fn event(&self, event: SoundEvent) -> &EventSettings {
        &self.events[event.index()]
    }
}

impl Registry for SoundSettings {
    type Key = SoundKey;
    const KEYS: &'static [SoundKey] = &KEYS;

    fn name(key: SoundKey) -> &'static str {
        match key {
            SoundKey::Enabled => "sound.enabled",
            SoundKey::Theme => "sound.theme",
            SoundKey::Gain => "sound.gain",
            SoundKey::EventEnabled(event) => EVENT_KEY_NAMES[event.index()][0],
            SoundKey::EventFile(event) => EVENT_KEY_NAMES[event.index()][1],
        }
    }

    fn read(&mut self, key: SoundKey, text: &str) -> bool {
        match key {
            SoundKey::Enabled => as_bool(text).map(|on| self.enabled = on).is_ok(),
            SoundKey::Theme => {
                let legal = is_theme_name(text);
                if legal {
                    self.theme = String::from(text);
                }
                legal
            }
            SoundKey::Gain => AudioGain::parse(text).map(|gain| self.gain = gain).is_ok(),
            SoundKey::EventEnabled(event) => as_bool(text)
                .map(|on| self.events[event.index()].enabled = on)
                .is_ok(),
            SoundKey::EventFile(event) if text.is_empty() => {
                self.events[event.index()].file = None;
                true
            }
            SoundKey::EventFile(event) => SettingPath::new(text)
                .map(|file| self.events[event.index()].file = Some(file))
                .is_ok(),
        }
    }

    fn spell(&self, key: SoundKey, out: &mut String) -> bool {
        match key {
            SoundKey::Enabled => out.push_str(bool_text(self.enabled)),
            SoundKey::Theme => out.push_str(&self.theme),
            SoundKey::Gain => return write!(out, "{}", self.gain).is_ok(),
            SoundKey::EventEnabled(event) => out.push_str(bool_text(self.event(event).enabled)),
            SoundKey::EventFile(event) => match &self.event(event).file {
                Some(file) => out.push_str(file.as_str()),
                None => return false,
            },
        }
        true
    }
}

#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;
