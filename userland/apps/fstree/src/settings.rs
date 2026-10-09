//! Persisted session preferences: the configurable confirmation prompts.
//!
//! They live in this application's own app-data store, reached through
//! [`tairix_appdata`] — so they are private to `fstree`, gated on the
//! kernel-attested bundle identity, and readable or writable by no other
//! application the user launches. Nothing here spells a path, a user, or a
//! bundle identifier: the store derives all three from the identity the
//! kernel attests for this task.
//!
//! This module is the **closed registry** over the store's open key
//! namespace: the fixed set of keys the file manager reads
//! ([`SettingKey`]) and their bridges, kept by the shared engine
//! ([`Registry`], `tairix_appdata::save`). A key outside the registry is one
//! this session leaves alone rather than destroying on the next save.
//!
//! Reading fails **safe**: a store the service cannot serve, an absent key,
//! or a value that is not a boolean leaves the affected setting at its
//! default — and every default keeps its confirmation *on*, so a
//! damage-limiting question is never silently lost. A refused value is
//! *named* to the caller rather than swallowed, so one broken setting costs
//! only itself and the user can be told which.

use alloc::string::String;

use tairix_appconf::{as_bool, bool_text, Registry};

/// One key of the closed preference registry.
///
/// Adding a key means adding a variant here, its row in [`SettingKey::ALL`],
/// its field on [`Settings`], and its arms in this module's private
/// `set_field` and `field_value` bridges — the compiler then forces every
/// consumer to state what the new key means.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SettingKey {
    /// `confirm.delete` — whether a single delete (`d`) asks first.
    ConfirmDelete,
    /// `confirm.batch-delete` — whether a batch delete over the tagged set
    /// asks first.
    ConfirmBatchDelete,
}

impl SettingKey {
    /// Every registry key, in the order the settings menu lists them.
    pub const ALL: [Self; 2] = [Self::ConfirmDelete, Self::ConfirmBatchDelete];

    /// The canonical key spelling in the store.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::ConfirmDelete => "confirm.delete",
            Self::ConfirmBatchDelete => "confirm.batch-delete",
        }
    }

    /// How the settings menu labels the key.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::ConfirmDelete => "confirm delete",
            Self::ConfirmBatchDelete => "confirm batch delete",
        }
    }
}

/// The persisted preferences. Every field defaults to the safe choice.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Settings {
    /// Whether a single delete (`d`) asks before removing (default on).
    pub confirm_delete: bool,
    /// Whether a batch delete over the tagged set asks first (default on).
    pub confirm_batch_delete: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            confirm_delete: true,
            confirm_batch_delete: true,
        }
    }
}

impl Registry for Settings {
    type Key = SettingKey;
    const KEYS: &'static [SettingKey] = &SettingKey::ALL;

    fn name(key: SettingKey) -> &'static str {
        key.name()
    }

    fn read(&mut self, key: SettingKey, text: &str) -> bool {
        as_bool(text).is_ok_and(|value| {
            set_field(self, key, value);
            true
        })
    }

    fn spell(&self, key: SettingKey, out: &mut String) -> bool {
        out.push_str(bool_text(field_value(*self, key)));
        true
    }
}

impl Settings {
    /// Whether `key` is currently on.
    #[must_use]
    pub const fn is_on(self, key: SettingKey) -> bool {
        field_value(self, key)
    }

    /// Flip `key`.
    pub fn toggle(&mut self, key: SettingKey) {
        set_field(self, key, !field_value(*self, key));
    }
}

/// Set `key` on `settings`.
fn set_field(settings: &mut Settings, key: SettingKey, value: bool) {
    match key {
        SettingKey::ConfirmDelete => settings.confirm_delete = value,
        SettingKey::ConfirmBatchDelete => settings.confirm_batch_delete = value,
    }
}

/// The current value of `key` on `settings`.
const fn field_value(settings: Settings, key: SettingKey) -> bool {
    match key {
        SettingKey::ConfirmDelete => settings.confirm_delete,
        SettingKey::ConfirmBatchDelete => settings.confirm_batch_delete,
    }
}
