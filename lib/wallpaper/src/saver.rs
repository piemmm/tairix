//! The screensaver as the desktop's settings hold it: which one is chosen,
//! what the desktop feeds a saver that asks, the values of every saver's own
//! options, and the pictures that preview the desktop's own two
//! (`plans/SCREENSAVERS.md`).
//!
//! A saver bundle declares its options in its signed manifest. This document
//! holds only the values the user changed, under `saver.<bundle-id>.<key>`,
//! and they are read against the declaration of the saver they belong to: a
//! value a saver no longer accepts reads as its default, so an update to a
//! saver never resets the desktop.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};

use tairix_abi::saver::{ChoiceValue, OptionKey, SaverOption, SaverValue};
use tairix_abi::switchboard_ipc::TaskOwner;
use tairix_abi::window_ipc::SaverStanding;
use tairix_abi::{validate_bundle_id, BundleId};
use tairix_appconf::{as_bool, as_i64, bool_text, validate_key, Document};

use crate::catalog::is_wallpaper_category_name;

/// Where the OS ships the preview pictures of the desktop's own two
/// screensavers, Blank and Dim. A saver bundle ships its own
/// ([`SAVER_PREVIEW_PATH`]).
pub const SCREENSAVER_PREVIEW_STORE: &str = "/System/Graphics/Screensavers";

/// The file-name suffix of a screensaver's preview picture.
const PREVIEW_SUFFIX: &str = ".png";

/// Where, inside a saver bundle, its preview picture lives.
pub const SAVER_PREVIEW_PATH: &str = "Resources/Preview.png";

/// Largest screensaver preview any consumer reads, in bytes.
///
/// A fixed validation bound on untrusted input: a preview is a small picture
/// drawn in a chooser, so this bounds the work one can demand before a byte is
/// decoded, and the image build refuses a shipped preview over it.
pub const MAX_SCREENSAVER_PREVIEW_BYTES: usize = 1024 * 1024;

/// The saver chosen until the user chooses another.
pub const DEFAULT_SAVER: &str = "os.tairix.saver.ribbon";

/// What the screensaver is: one of the desktop's own display states, or a
/// saver bundle by its identifier.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum ScreensaverChoice {
    /// A black screen.
    Blank,
    /// The desktop's own backdrop, dimmed, with no window on it.
    Dim,
    /// The saver bundle with this identifier.
    Saver(BundleId),
}

const BLANK: &str = "blank";
const DIM: &str = "dim";

impl ScreensaverChoice {
    /// The desktop's own two, in the order a chooser offers them.
    pub const DESKTOP: [Self; 2] = [Self::Blank, Self::Dim];

    /// The saver chosen until the user chooses another, [`DEFAULT_SAVER`].
    #[must_use]
    pub fn default_saver() -> Self {
        Self::from_value(DEFAULT_SAVER).unwrap_or(Self::Blank)
    }

    /// The spelling a settings document holds: `blank`, `dim`, or the bundle's
    /// identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Blank => BLANK,
            Self::Dim => DIM,
            Self::Saver(id) => id.as_str(),
        }
    }

    /// Read a spelling; `None` for anything that is neither of the desktop's
    /// own nor a bundle identifier. `blank` and `dim` always name the
    /// desktop's own, so no bundle may be chosen under either.
    #[must_use]
    pub fn from_value(value: &str) -> Option<Self> {
        match value {
            BLANK => Some(Self::Blank),
            DIM => Some(Self::Dim),
            _ => {
                validate_bundle_id(value).ok()?;
                BundleId::new(value).ok().map(Self::Saver)
            }
        }
    }

    /// The shipped preview picture of the desktop's own two; `None` for a
    /// saver, which ships its own.
    #[must_use]
    pub fn preview_path(&self) -> Option<String> {
        match self {
            Self::Blank | Self::Dim => Some(format!(
                "{SCREENSAVER_PREVIEW_STORE}/{}{PREVIEW_SUFFIX}",
                self.as_str()
            )),
            Self::Saver(_) => None,
        }
    }

    /// The desktop screensaver a shipped preview file name previews, or `None`
    /// for a name neither asks for.
    ///
    /// The image build applies this to every file it plants, so a preview
    /// nothing would ever show fails the build rather than shipping.
    #[must_use]
    pub fn of_preview(file: &str) -> Option<Self> {
        let choice = Self::from_value(file.strip_suffix(PREVIEW_SUFFIX)?)?;
        Self::DESKTOP.contains(&choice).then_some(choice)
    }
}

/// A wallpaper category's name as a setting holds it: a legal category
/// directory name, within the ABI's file-name bound.
///
/// A name, not a promise: a store an update changed may no longer hold the
/// category a setting names, and its consumer decides what that means.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WallpaperCategory(String);

impl WallpaperCategory {
    /// The category named `name`, or `None` for a name no category directory
    /// could carry.
    #[must_use]
    pub fn new(name: &str) -> Option<Self> {
        (name.len() <= tairix_abi::FS_NAME_MAX && is_wallpaper_category_name(name))
            .then(|| Self(name.to_string()))
    }

    /// The category's name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Which of the shipped pictures the slides feed offers a saver.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum SlideSource {
    /// Every category's.
    #[default]
    Every,
    /// One category's.
    Category(WallpaperCategory),
}

impl SlideSource {
    /// The one category named, or `None` for every category.
    #[must_use]
    pub const fn category(&self) -> Option<&WallpaperCategory> {
        match self {
            Self::Every => None,
            Self::Category(category) => Some(category),
        }
    }

    /// Decode a value spelling: empty for every category, else a category's
    /// name; `None` for a name no category could carry.
    #[must_use]
    pub fn from_value(value: &str) -> Option<Self> {
        if value.is_empty() {
            return Some(Self::Every);
        }
        WallpaperCategory::new(value).map(Self::Category)
    }

    /// The canonical value spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.category().map_or("", WallpaperCategory::as_str)
    }
}

/// Whose busiest tasks the machine feed names to a saver: a task's name is
/// text its owner chose, and a saver draws over a locked screen.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum TaskNames {
    /// None: every task is shown by its cost alone.
    None,
    /// The user's own; another account's are shown by their cost alone.
    #[default]
    Own,
    /// Every account's.
    All,
}

impl TaskNames {
    /// Every setting, in the order a chooser offers them.
    pub const ALL: [Self; 3] = [Self::None, Self::Own, Self::All];

    /// The canonical value spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Own => "own",
            Self::All => "all",
        }
    }

    /// Decode a value spelling; `None` for anything outside the closed set.
    #[must_use]
    pub fn from_value(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|names| names.as_str() == value)
    }

    /// Whether a task `owner` holds is named.
    #[must_use]
    pub const fn names(self, owner: TaskOwner) -> bool {
        match self {
            Self::None => false,
            Self::Own => matches!(owner, TaskOwner::Own),
            Self::All => true,
        }
    }
}

/// Why a catalogued saver standing `standing` cannot be chosen, as a reader
/// is told: what follows "it cannot be chosen:".
#[must_use]
pub const fn standing_reason(standing: SaverStanding) -> &'static str {
    match standing {
        SaverStanding::Offered => "it can be",
        SaverStanding::Damaged => {
            "its manifest cannot be read, or declares what no screensaver may"
        }
        SaverStanding::AsksForAFeed => {
            "it asks for what only the system's own screensavers are given"
        }
        SaverStanding::Duplicate => "another screensaver already has its identifier",
        SaverStanding::Unkeyable => "its identifier is too long for its settings to be kept",
    }
}

/// The key prefix every saver option value is held under.
pub const SAVER_VALUE_PREFIX: &str = "saver.";

/// Most saver option values a document holds.
///
/// A containment bound, not a capacity: a value is held only where it differs
/// from its saver's default, and a full document makes room by forgetting the
/// values of savers no longer installed, so this is never reached by a desktop
/// someone uses; it bounds what a hostile store can make a reader keep.
pub const SAVER_VALUES_MAX: usize = 256;

/// The option values the user set for their savers: `saver.<bundle-id>.<key>`
/// to the value's spelling.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SaverValues {
    values: BTreeMap<(BundleId, OptionKey), StoredValue>,
}

/// A value as the document spells it: a word or a number, judged against an
/// option only when it is read for one.
#[derive(Clone, Debug, Eq, PartialEq)]
struct StoredValue(String);

impl SaverValues {
    /// The value `saver` holds for `option`: the one set, where the option
    /// still accepts it, else its default.
    #[must_use]
    pub fn value(&self, saver: &BundleId, option: &SaverOption) -> SaverValue {
        self.values
            .get(&(*saver, *option.key()))
            .and_then(|stored| read_value(option, &stored.0))
            .unwrap_or_else(|| option.default_value())
    }

    /// Set `saver`'s `option` to `value`, holding nothing where it is the
    /// default; `false`, changing nothing, for a value the option does not
    /// accept or past [`SAVER_VALUES_MAX`].
    pub fn set(&mut self, saver: &BundleId, option: &SaverOption, value: SaverValue) -> bool {
        if !option.accepts(&value) {
            return false;
        }
        let key = (*saver, *option.key());
        if value == option.default_value() {
            self.values.remove(&key);
            return true;
        }
        if !self.values.contains_key(&key) && self.values.len() >= SAVER_VALUES_MAX {
            return false;
        }
        self.values.insert(key, StoredValue(spell_value(&value)));
        true
    }

    /// Keep only the values of the savers `holds` keeps.
    pub fn retain_savers(&mut self, holds: impl Fn(&BundleId) -> bool) {
        self.values.retain(|(saver, _), _| holds(saver));
    }

    /// Take the setting `key` = `text`, answering whether it is one a saver's
    /// value could be: a key `saver.<bundle-id>.<option>` and a word or a
    /// number.
    pub(crate) fn read(&mut self, key: &str, text: &str) -> bool {
        let Some((saver, option)) = split_key(key) else {
            return false;
        };
        let held = self.values.contains_key(&(saver, option));
        if !is_stored_value(text) || (!held && self.values.len() >= SAVER_VALUES_MAX) {
            return false;
        }
        self.values
            .insert((saver, option), StoredValue(text.to_string()));
        true
    }

    /// Spell every value into `document`.
    pub(crate) fn spell(&self, document: &mut Document) {
        for ((saver, option), stored) in &self.values {
            let _ = document.set(&document_key(saver, option), &stored.0);
        }
    }
}

/// The document key `saver`'s `option` is held under.
#[must_use]
pub fn document_key(saver: &BundleId, option: &OptionKey) -> String {
    format!("{SAVER_VALUE_PREFIX}{}.{}", saver.as_str(), option.as_str())
}

/// Whether `saver`'s values can be held at all: its identifier makes a key the
/// document admits for every option key a saver may declare.
#[must_use]
pub fn keeps_values_of(saver: &BundleId) -> bool {
    let longest = "a".repeat(tairix_abi::saver::SAVER_OPTION_KEY_MAX);
    OptionKey::new(&longest).is_ok_and(|option| validate_key(&document_key(saver, &option)).is_ok())
}

fn split_key(key: &str) -> Option<(BundleId, OptionKey)> {
    let (saver, option) = key.strip_prefix(SAVER_VALUE_PREFIX)?.rsplit_once('.')?;
    validate_bundle_id(saver).ok()?;
    Some((BundleId::new(saver).ok()?, OptionKey::new(option).ok()?))
}

fn is_stored_value(text: &str) -> bool {
    ChoiceValue::new(text).is_ok() || text.parse::<i32>().is_ok()
}

/// The value `text` spells for `option`, as this document spells values:
/// a boolean for a switch, a choice's own word, a decimal number for a range.
#[must_use]
pub fn read_value(option: &SaverOption, text: &str) -> Option<SaverValue> {
    let value = match option.kind() {
        tairix_abi::saver::SaverOptionKind::Switch { .. } => {
            SaverValue::Switch(as_bool(text).ok()?)
        }
        tairix_abi::saver::SaverOptionKind::Choice { .. } => {
            SaverValue::Choice(ChoiceValue::new(text).ok()?)
        }
        tairix_abi::saver::SaverOptionKind::Range { .. } => {
            SaverValue::Range(i32::try_from(as_i64(text).ok()?).ok()?)
        }
    };
    option.accepts(&value).then_some(value)
}

/// `value` as this document spells it.
#[must_use]
pub fn spell_value(value: &SaverValue) -> String {
    match value {
        SaverValue::Switch(on) => bool_text(*on).to_string(),
        SaverValue::Choice(word) => word.as_str().to_string(),
        SaverValue::Range(number) => format!("{number}"),
    }
}

#[cfg(test)]
#[path = "saver_tests.rs"]
mod tests;
