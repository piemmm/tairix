//! A user's own sound choices: what they set and nothing they left, so the
//! machine's document still decides every setting they did not, and the one
//! reading of them the session that keeps them and the cue authority they are
//! handed to share.
//!
//! A chosen file is kept as its path in the session's own store and never
//! travels as one: the session hands the cue authority the open file instead,
//! so a document arriving over a channel that names a file is refused.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_appconf::{bool_text, merge_admitting, Document, Refusal, Registry, SettingPath};

use crate::settings::in_namespace;
use crate::{SoundEvent, SoundKey, SoundSettings};

/// Whether a user's own choices reach `event`'s sound.
///
/// Every event's do but those marking a moment no session holds the seat — the
/// machine starting, a session starting or ending — whose sounds are the
/// machine document's alone: no user's choices are in force then.
#[must_use]
pub const fn layered(event: SoundEvent) -> bool {
    !matches!(
        event,
        SoundEvent::Startup | SoundEvent::Login | SoundEvent::Logout
    )
}

/// Whether a document a session hands over may set `key`: never a file, which
/// travels as a descriptor, nor a setting of a sound no user's choices reach.
#[must_use]
pub const fn pushable(key: SoundKey) -> bool {
    match key {
        SoundKey::Enabled | SoundKey::Theme | SoundKey::Gain => true,
        SoundKey::EventEnabled(event) => layered(event),
        SoundKey::EventFile(_) => false,
    }
}

const PUSHABLE_COUNT: usize = {
    let mut count = 0;
    let mut at = 0;
    while at < SoundSettings::KEYS.len() {
        if pushable(SoundSettings::KEYS[at]) {
            count += 1;
        }
        at += 1;
    }
    count
};

/// Every key [`pushable`] admits, in registry order: what a session hands over
/// and what a settings pane shows.
pub const PUSHABLE: [SoundKey; PUSHABLE_COUNT] = {
    let mut keys = [SoundKey::Enabled; PUSHABLE_COUNT];
    let mut taken = 0;
    let mut at = 0;
    while at < SoundSettings::KEYS.len() {
        if pushable(SoundSettings::KEYS[at]) {
            keys[taken] = SoundSettings::KEYS[at];
            taken += 1;
        }
        at += 1;
    }
    keys
};

/// `machine` with a session's `text` laid over it, or why `text` is refused
/// whole: it names a file, a sound no user's choices reach, or a value its key
/// does not accept.
///
/// # Errors
///
/// The [`Refusal`] naming the first thing wrong with `text`.
pub fn merge_pushed(
    machine: &SoundSettings,
    text: &str,
) -> Result<SoundSettings, Refusal<SoundKey>> {
    merge_admitting(machine, text, pushable)
}

const KEY_COUNT: usize = SoundSettings::KEYS.len();

/// What a user set of their sounds, each setting in its canonical spelling.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UserChoices {
    /// Each key's value where the user set it, in registry order.
    values: [Option<String>; KEY_COUNT],
}

impl Default for UserChoices {
    fn default() -> Self {
        Self {
            values: core::array::from_fn(|_| None),
        }
    }
}

impl UserChoices {
    /// The choices a stored `document` holds, and the name of every sound
    /// setting left out of them: one outside the registry, one no user's
    /// choices reach, or one whose value its key does not accept. A setting
    /// outside the sound namespace belongs to another reader of the same store
    /// and is passed over.
    #[must_use]
    pub fn load(document: &Document) -> (Self, Vec<String>) {
        let mut choices = Self::default();
        let mut refused = Vec::new();
        for setting in document
            .settings()
            .filter(|setting| in_namespace(setting.key))
        {
            let kept = key_named(setting.key).is_some_and(|key| {
                let settable = match key {
                    SoundKey::EventFile(event) => layered(event),
                    key => pushable(key),
                };
                settable && choices.set(key, setting.value)
            });
            if !kept {
                refused.push(String::from(setting.key));
            }
        }
        (choices, refused)
    }

    /// Lay a pane's `text` over these choices, refusing it whole for anything
    /// [`merge_pushed`] would refuse.
    ///
    /// # Errors
    ///
    /// The [`Refusal`] naming the first thing wrong with `text`; the choices
    /// are left as they were.
    pub fn apply(&mut self, text: &str) -> Result<(), Refusal<SoundKey>> {
        merge_pushed(&SoundSettings::default(), text)?;
        let document = Document::parse(text).map_err(Refusal::Malformed)?;
        let mut next = self.clone();
        for setting in document.settings() {
            let key = key_named(setting.key)
                .ok_or_else(|| Refusal::UnknownKey(String::from(setting.key)))?;
            if !next.set(key, setting.value) {
                return Err(Refusal::InvalidValue(key));
            }
        }
        *self = next;
        Ok(())
    }

    /// The file chosen for `event`, if one is.
    #[must_use]
    pub fn file(&self, event: SoundEvent) -> Option<&str> {
        self.value(SoundKey::EventFile(event))
    }

    /// Have `event` play the file at `path`, turning it on if it was off,
    /// answering whether it may: a sound no user's choices reach plays no
    /// file of theirs.
    pub fn choose(&mut self, event: SoundEvent, path: &SettingPath) -> bool {
        layered(event)
            && self.set(SoundKey::EventFile(event), path.as_str())
            && self.set(SoundKey::EventEnabled(event), bool_text(true))
    }

    /// Have `event` play its theme's sound again, turning it on if it was off.
    pub fn use_theme(&mut self, event: SoundEvent) {
        if let Some(slot) = slot_of(SoundKey::EventFile(event)) {
            self.values[slot] = None;
        }
        if layered(event) {
            self.set(SoundKey::EventEnabled(event), bool_text(true));
        }
    }

    /// The document a session hands the cue authority: every choice but the
    /// files.
    #[must_use]
    pub fn pushed(&self) -> String {
        self.document_of(&PUSHABLE).render()
    }

    /// The settings in effect with these choices over `machine`.
    #[must_use]
    pub fn over(&self, machine: &SoundSettings) -> SoundSettings {
        let mut settings = machine.clone();
        for key in PUSHABLE {
            if let Some(value) = self.value(key) {
                settings.read(key, value);
            }
        }
        settings.normalise();
        settings
    }

    /// The whole document, files and all, as the session stores it.
    #[must_use]
    pub fn document(&self) -> Document {
        self.document_of(SoundSettings::KEYS)
    }

    /// `keys`' values, those the user set, as a document.
    fn document_of(&self, keys: &[SoundKey]) -> Document {
        let mut document = Document::new();
        for &key in keys {
            if let Some(value) = self.value(key) {
                // Each value is a canonical spelling this engine accepted.
                let _ = document.set(SoundSettings::name(key), value);
            }
        }
        document
    }

    fn value(&self, key: SoundKey) -> Option<&str> {
        slot_of(key).and_then(|slot| self.values[slot].as_deref())
    }

    /// Set `key` to `text` in its canonical spelling, answering whether the
    /// key accepts it.
    fn set(&mut self, key: SoundKey, text: &str) -> bool {
        let mut scratch = SoundSettings::default();
        let Some(slot) = slot_of(key) else {
            return false;
        };
        if !scratch.read(key, text) {
            return false;
        }
        let mut canonical = String::new();
        self.values[slot] = scratch.spell(key, &mut canonical).then_some(canonical);
        true
    }
}

/// Where `key` sits in the registry's order.
fn slot_of(key: SoundKey) -> Option<usize> {
    SoundSettings::KEYS.iter().position(|&held| held == key)
}

/// The registry key spelled `name`.
fn key_named(name: &str) -> Option<SoundKey> {
    SoundSettings::KEYS
        .iter()
        .copied()
        .find(|&key| SoundSettings::name(key) == name)
}

#[cfg(test)]
#[path = "user_tests.rs"]
mod tests;
