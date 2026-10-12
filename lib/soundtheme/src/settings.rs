//! The sound document: whether the desktop makes sounds, the theme, and each
//! event's switch, gain and replacement file.
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
    /// `sound.<event>.enabled` — whether the event makes its sound.
    EventEnabled(SoundEvent),
    /// `sound.<event>.gain` — how far the event is turned down.
    EventGain(SoundEvent),
    /// `sound.<event>.file` — a file the user chose to play instead; an
    /// empty value clears it.
    EventFile(SoundEvent),
}

const EVENTS: usize = SoundEvent::ALL.len();

/// Every key: the two machine-wide ones, then each event's three.
const KEYS: [SoundKey; 2 + 3 * EVENTS] = {
    let mut keys = [SoundKey::Enabled; 2 + 3 * EVENTS];
    keys[1] = SoundKey::Theme;
    let mut at = 0;
    while at < EVENTS {
        let event = SoundEvent::ALL[at];
        keys[2 + 3 * at] = SoundKey::EventEnabled(event);
        keys[3 + 3 * at] = SoundKey::EventGain(event);
        keys[4 + 3 * at] = SoundKey::EventFile(event);
        at += 1;
    }
    keys
};

/// Each event's keys — switch, gain, file — in [`SoundEvent::ALL`] order.
const EVENT_KEY_NAMES: [[&str; 3]; EVENTS] = [
    [
        "sound.startup.enabled",
        "sound.startup.gain",
        "sound.startup.file",
    ],
    [
        "sound.shutdown.enabled",
        "sound.shutdown.gain",
        "sound.shutdown.file",
    ],
    [
        "sound.login.enabled",
        "sound.login.gain",
        "sound.login.file",
    ],
    [
        "sound.logout.enabled",
        "sound.logout.gain",
        "sound.logout.file",
    ],
    [
        "sound.device_attached.enabled",
        "sound.device_attached.gain",
        "sound.device_attached.file",
    ],
    [
        "sound.device_detached.enabled",
        "sound.device_detached.gain",
        "sound.device_detached.file",
    ],
    [
        "sound.notification.enabled",
        "sound.notification.gain",
        "sound.notification.file",
    ],
    [
        "sound.notification_message.enabled",
        "sound.notification_message.gain",
        "sound.notification_message.file",
    ],
    [
        "sound.notification_reminder.enabled",
        "sound.notification_reminder.gain",
        "sound.notification_reminder.file",
    ],
    [
        "sound.error.enabled",
        "sound.error.gain",
        "sound.error.file",
    ],
    [
        "sound.warning.enabled",
        "sound.warning.gain",
        "sound.warning.file",
    ],
    ["sound.info.enabled", "sound.info.gain", "sound.info.file"],
    ["sound.bell.enabled", "sound.bell.gain", "sound.bell.file"],
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
    /// How far it is turned down; unity plays the asset bit-exact.
    pub gain: AudioGain,
    /// A file the user chose to play instead of the theme's.
    pub file: Option<SettingPath>,
}

impl Default for EventSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            gain: AudioGain::UNITY,
            file: None,
        }
    }
}

/// The sound settings one document implies.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SoundSettings {
    enabled: bool,
    theme: String,
    events: [EventSettings; EVENTS],
}

impl Default for SoundSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            theme: String::from(DEFAULT_THEME),
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
            SoundKey::EventEnabled(event) => EVENT_KEY_NAMES[event.index()][0],
            SoundKey::EventGain(event) => EVENT_KEY_NAMES[event.index()][1],
            SoundKey::EventFile(event) => EVENT_KEY_NAMES[event.index()][2],
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
            SoundKey::EventEnabled(event) => as_bool(text)
                .map(|on| self.events[event.index()].enabled = on)
                .is_ok(),
            SoundKey::EventGain(event) => AudioGain::parse(text)
                .map(|gain| self.events[event.index()].gain = gain)
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
            SoundKey::EventEnabled(event) => out.push_str(bool_text(self.event(event).enabled)),
            SoundKey::EventGain(event) => return write!(out, "{}", self.event(event).gain).is_ok(),
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
