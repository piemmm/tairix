//! What a screensaver declares in its signed manifest: the options Settings
//! offers for it, and the values those options take (`plans/SCREENSAVERS.md`).
//!
//! An option is one fixed [`SAVER_OPTION_ENTRY_LEN`] entry of the `AppInfo`
//! body after the browsed services ([`crate::appinfo::saver_option_at`]). Its
//! key and every choice value are [`SaverWord`]s, because the session spells
//! them into its settings document; its labels are prose Settings draws.

use crate::appinfo::{
    APPINFO_FLAGS_SAVER_FEEDS, APPINFO_FLAG_FEED_KEEP, APPINFO_FLAG_FEED_MACHINE,
    APPINFO_FLAG_FEED_SLIDES,
};
use crate::bounded_text::BoundedText;
use crate::Errno;

/// Most options one saver declares.
pub const SAVER_MAX_OPTIONS: u8 = 8;

/// Most values one choice offers.
pub const SAVER_MAX_CHOICES: usize = 12;

/// Longest option key, in bytes.
pub const SAVER_OPTION_KEY_MAX: usize = 24;

/// Longest option label, in bytes.
pub const SAVER_OPTION_LABEL_MAX: usize = 48;

/// Longest spelled choice value, in bytes.
pub const SAVER_CHOICE_VALUE_MAX: usize = 16;

/// Longest choice label, in bytes.
pub const SAVER_CHOICE_LABEL_MAX: usize = 32;

/// Longest range unit, in bytes.
pub const SAVER_UNIT_MAX: usize = 16;

/// Longest note beneath an option's label, in bytes: what choosing it does.
pub const SAVER_OPTION_NOTE_MAX: usize = 240;

/// The most values one range may hold: as many as a slider stops at, one for
/// each thousandth of its travel and both its ends, so every one can be set. A
/// bound on what a manifest may ask Settings to draw.
pub const SAVER_RANGE_VALUES_MAX: u16 = 1001;

const KIND_SWITCH: u8 = 1;
const KIND_CHOICE: u8 = 2;
const KIND_RANGE: u8 = 3;

const OFF_KIND: usize = 0;
const OFF_KEY_LEN: usize = 1;
const OFF_LABEL_LEN: usize = 2;
const OFF_COUNT: usize = 3;
const OFF_DEFAULT: usize = 4;
const OFF_MIN: usize = 8;
const OFF_MAX: usize = 12;
const OFF_STEP: usize = 16;
const OFF_NOTE_LEN: usize = 20;
const OFF_KEY: usize = 24;
const OFF_LABEL: usize = OFF_KEY + SAVER_OPTION_KEY_MAX;
const OFF_UNIT: usize = OFF_LABEL + SAVER_OPTION_LABEL_MAX;
const OFF_NOTE: usize = OFF_UNIT + SAVER_UNIT_MAX;
const OFF_CHOICES: usize = OFF_NOTE + SAVER_OPTION_NOTE_MAX;
const CHOICE_LEN: usize = 2 + SAVER_CHOICE_VALUE_MAX + SAVER_CHOICE_LABEL_MAX;

/// Encoded length of one option entry: its kind, lengths and numbers, the key,
/// label, unit and note buffers, then [`SAVER_MAX_CHOICES`] choice slots, each
/// a value length, a label length and their two buffers.
pub const SAVER_OPTION_ENTRY_LEN: usize = OFF_CHOICES + SAVER_MAX_CHOICES * CHOICE_LEN;

/// A lower-case ASCII word: a letter, then letters, digits and `_`.
///
/// Option keys and choice values are spelled into a settings document, so
/// they share that document's own key grammar rather than admitting prose.
#[derive(Copy, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct SaverWord<const MAX: usize> {
    text: BoundedText<0, MAX>,
}

impl<const MAX: usize> SaverWord<MAX> {
    /// Padding for a fixed array decode fills; never handed out.
    pub(crate) const BLANK: Self = Self {
        text: BoundedText::EMPTY,
    };

    /// `word`, when it is one.
    ///
    /// # Errors
    ///
    /// [`Errno::LengthOutOfRange`] for an empty or over-long word, and
    /// [`Errno::OutOfRange`] for anything outside the word grammar.
    pub fn new(word: &str) -> Result<Self, Errno> {
        check_word(word, MAX)?;
        Ok(Self {
            text: BoundedText::new(word)?,
        })
    }

    pub(crate) fn from_wire(len: u8, bytes: &[u8; MAX]) -> Result<Self, Errno> {
        let text = BoundedText::from_wire(len, bytes)?;
        check_word(text.as_str(), MAX)?;
        Ok(Self { text })
    }

    /// The word's fixed-width wire field.
    pub(crate) const fn text(&self) -> &BoundedText<0, MAX> {
        &self.text
    }

    /// The word.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.text.as_str()
    }
}

impl<const MAX: usize> core::fmt::Debug for SaverWord<MAX> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

fn check_word(word: &str, max: usize) -> Result<(), Errno> {
    if word.is_empty() || word.len() > max {
        return Err(Errno::LengthOutOfRange);
    }
    let mut bytes = word.bytes();
    let lead = bytes.next().is_some_and(|b| b.is_ascii_lowercase());
    if !lead || !bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_') {
        return Err(Errno::OutOfRange);
    }
    Ok(())
}

/// An option's key.
pub type OptionKey = SaverWord<SAVER_OPTION_KEY_MAX>;

/// One value a choice offers, as it is spelled.
pub type ChoiceValue = SaverWord<SAVER_CHOICE_VALUE_MAX>;

/// What Settings shows for an option.
pub type OptionLabel = BoundedText<1, SAVER_OPTION_LABEL_MAX>;

/// What Settings shows beneath an option's label; may be empty.
pub type OptionNote = BoundedText<0, SAVER_OPTION_NOTE_MAX>;

/// One value a choice offers: its spelling and the label Settings shows.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Choice {
    /// How the value is spelled in the settings document and to the saver.
    pub value: ChoiceValue,
    /// What Settings shows for it.
    pub label: BoundedText<0, SAVER_CHOICE_LABEL_MAX>,
}

impl Choice {
    const BLANK: Self = Self {
        value: ChoiceValue::BLANK,
        label: BoundedText::EMPTY,
    };
}

/// A choice's values, in the order Settings offers them.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Choices {
    items: [Choice; SAVER_MAX_CHOICES],
    len: u8,
}

impl Choices {
    /// `choices`, when there are two to [`SAVER_MAX_CHOICES`] of them, each
    /// labelled, no value spelled twice.
    ///
    /// # Errors
    ///
    /// [`Errno::LengthOutOfRange`] for too few or too many, and
    /// [`Errno::OutOfRange`] for a repeated value or an unlabelled one.
    pub fn new(choices: &[Choice]) -> Result<Self, Errno> {
        if choices.len() < 2 || choices.len() > SAVER_MAX_CHOICES {
            return Err(Errno::LengthOutOfRange);
        }
        for (at, choice) in choices.iter().enumerate() {
            if choice.label.is_empty() || choices[..at].iter().any(|c| c.value == choice.value) {
                return Err(Errno::OutOfRange);
            }
        }
        let mut items = [Choice::BLANK; SAVER_MAX_CHOICES];
        items[..choices.len()].copy_from_slice(choices);
        Ok(Self {
            items,
            len: u8::try_from(choices.len()).map_err(|_| Errno::LengthOutOfRange)?,
        })
    }

    /// The values.
    #[must_use]
    pub fn as_slice(&self) -> &[Choice] {
        &self.items[..usize::from(self.len)]
    }

    /// The position of the value spelled `value`.
    #[must_use]
    pub fn position(&self, value: &str) -> Option<usize> {
        self.as_slice()
            .iter()
            .position(|c| c.value.as_str() == value)
    }
}

/// A whole-number range: `min` to `max` in steps of `step`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Range {
    min: i32,
    max: i32,
    step: u32,
}

impl Range {
    /// The range, when `min < max`, `step` divides the span, and it holds
    /// no more than [`SAVER_RANGE_VALUES_MAX`] values.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] otherwise: a range a slider cannot reach both
    /// ends of, or every value of, is not one.
    pub fn new(min: i32, max: i32, step: u32) -> Result<Self, Errno> {
        let span = i64::from(max) - i64::from(min);
        if span <= 0
            || step == 0
            || span % i64::from(step) != 0
            || span / i64::from(step) >= i64::from(SAVER_RANGE_VALUES_MAX)
        {
            return Err(Errno::OutOfRange);
        }
        Ok(Self { min, max, step })
    }

    /// How many values it holds, both ends among them.
    #[must_use]
    pub fn values(&self) -> u16 {
        let steps = (i64::from(self.max) - i64::from(self.min)) / i64::from(self.step);
        u16::try_from(steps + 1).unwrap_or(SAVER_RANGE_VALUES_MAX)
    }

    /// Its value `index` steps above the least, or `None` past the greatest.
    #[must_use]
    pub fn value_at(&self, index: u16) -> Option<i32> {
        if index >= self.values() {
            return None;
        }
        i32::try_from(i64::from(self.min) + i64::from(index) * i64::from(self.step)).ok()
    }

    /// How many steps above the least `value` is, where it is one of the
    /// range's values.
    #[must_use]
    pub fn index_of(&self, value: i32) -> Option<u16> {
        if !self.holds(value) {
            return None;
        }
        u16::try_from((i64::from(value) - i64::from(self.min)) / i64::from(self.step)).ok()
    }

    /// The least value.
    #[must_use]
    pub const fn min(&self) -> i32 {
        self.min
    }

    /// The greatest value.
    #[must_use]
    pub const fn max(&self) -> i32 {
        self.max
    }

    /// The distance between neighbouring values.
    #[must_use]
    pub const fn step(&self) -> u32 {
        self.step
    }

    /// Whether `value` is one of the range's values.
    #[must_use]
    pub fn holds(&self, value: i32) -> bool {
        (self.min..=self.max).contains(&value)
            && (i64::from(value) - i64::from(self.min)) % i64::from(self.step) == 0
    }
}

/// What kind of option it is, with its default.
// A choice's values are fixed-width, and lib/abi has no allocator to box them.
#[allow(clippy::large_enum_variant)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SaverOptionKind {
    /// On or off.
    Switch {
        /// The value until the user changes it.
        default: bool,
    },
    /// One of a few named values.
    Choice {
        /// The values.
        choices: Choices,
        /// The position of the value until the user changes it.
        default: u8,
    },
    /// A whole number.
    Range {
        /// The values.
        range: Range,
        /// The value until the user changes it.
        default: i32,
        /// What the number counts, shown beside it; may be empty.
        unit: BoundedText<0, SAVER_UNIT_MAX>,
    },
}

/// One option a saver declares.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct SaverOption {
    key: OptionKey,
    label: OptionLabel,
    note: OptionNote,
    kind: SaverOptionKind,
}

impl SaverOption {
    /// The option, when its default is one of its values.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] for a default outside the option's values.
    pub fn new(
        key: OptionKey,
        (label, note): (OptionLabel, OptionNote),
        kind: SaverOptionKind,
    ) -> Result<Self, Errno> {
        let default_held = match kind {
            SaverOptionKind::Switch { .. } => true,
            SaverOptionKind::Choice { choices, default } => {
                usize::from(default) < choices.as_slice().len()
            }
            SaverOptionKind::Range { range, default, .. } => range.holds(default),
        };
        if !default_held {
            return Err(Errno::OutOfRange);
        }
        Ok(Self {
            key,
            label,
            note,
            kind,
        })
    }

    /// The key it is stored and sent under.
    #[must_use]
    pub const fn key(&self) -> &OptionKey {
        &self.key
    }

    /// What Settings shows for it.
    #[must_use]
    pub fn label(&self) -> &str {
        self.label.as_str()
    }

    /// What Settings shows beneath its label; empty when it says nothing more.
    #[must_use]
    pub fn note(&self) -> &str {
        self.note.as_str()
    }

    /// Its kind and default.
    #[must_use]
    pub const fn kind(&self) -> &SaverOptionKind {
        &self.kind
    }

    /// The value until the user changes it.
    #[must_use]
    pub fn default_value(&self) -> SaverValue {
        match self.kind {
            SaverOptionKind::Switch { default } => SaverValue::Switch(default),
            SaverOptionKind::Choice { choices, default } => {
                SaverValue::Choice(choices.as_slice()[usize::from(default)].value)
            }
            SaverOptionKind::Range { default, .. } => SaverValue::Range(default),
        }
    }

    /// Whether `value` is one of this option's values.
    #[must_use]
    pub fn accepts(&self, value: &SaverValue) -> bool {
        match (&self.kind, value) {
            (SaverOptionKind::Switch { .. }, SaverValue::Switch(_)) => true,
            (SaverOptionKind::Choice { choices, .. }, SaverValue::Choice(word)) => {
                choices.position(word.as_str()).is_some()
            }
            (SaverOptionKind::Range { range, .. }, SaverValue::Range(number)) => {
                range.holds(*number)
            }
            _ => false,
        }
    }

    /// Encode this option as its body entry.
    #[must_use]
    pub fn to_entry(&self) -> [u8; SAVER_OPTION_ENTRY_LEN] {
        let mut entry = [0u8; SAVER_OPTION_ENTRY_LEN];
        entry[OFF_KEY_LEN] = self.key.text.len_byte();
        entry[OFF_KEY..OFF_KEY + SAVER_OPTION_KEY_MAX].copy_from_slice(self.key.text.raw_bytes());
        entry[OFF_LABEL_LEN] = self.label.len_byte();
        entry[OFF_LABEL..OFF_LABEL + SAVER_OPTION_LABEL_MAX]
            .copy_from_slice(self.label.raw_bytes());
        entry[OFF_NOTE_LEN] = self.note.len_byte();
        entry[OFF_NOTE..OFF_CHOICES].copy_from_slice(self.note.raw_bytes());
        match self.kind {
            SaverOptionKind::Switch { default } => {
                entry[OFF_KIND] = KIND_SWITCH;
                put_i32(&mut entry, OFF_DEFAULT, i32::from(default));
            }
            SaverOptionKind::Choice { choices, default } => {
                entry[OFF_KIND] = KIND_CHOICE;
                entry[OFF_COUNT] = choices.len;
                put_i32(&mut entry, OFF_DEFAULT, i32::from(default));
                for (slot, choice) in choices.as_slice().iter().enumerate() {
                    let at = OFF_CHOICES + slot * CHOICE_LEN;
                    entry[at] = choice.value.text.len_byte();
                    entry[at + 1] = choice.label.len_byte();
                    entry[at + 2..at + 2 + SAVER_CHOICE_VALUE_MAX]
                        .copy_from_slice(choice.value.text.raw_bytes());
                    entry[at + 2 + SAVER_CHOICE_VALUE_MAX..at + CHOICE_LEN]
                        .copy_from_slice(choice.label.raw_bytes());
                }
            }
            SaverOptionKind::Range {
                range,
                default,
                unit,
            } => {
                entry[OFF_KIND] = KIND_RANGE;
                entry[OFF_COUNT] = unit.len_byte();
                put_i32(&mut entry, OFF_DEFAULT, default);
                put_i32(&mut entry, OFF_MIN, range.min);
                put_i32(&mut entry, OFF_MAX, range.max);
                entry[OFF_STEP..OFF_STEP + 4].copy_from_slice(&range.step.to_le_bytes());
                entry[OFF_UNIT..OFF_UNIT + SAVER_UNIT_MAX].copy_from_slice(unit.raw_bytes());
            }
        }
        entry
    }

    /// Decode one body entry, refusing every byte a well-formed entry of its
    /// kind leaves zero.
    ///
    /// # Errors
    ///
    /// [`Errno::BufferTooSmall`] for a short entry, [`Errno::BadMagic`] for an
    /// unknown kind or a stray byte, and the text and range errors of the
    /// constructors.
    pub fn from_entry(entry: &[u8]) -> Result<Self, Errno> {
        let entry: &[u8; SAVER_OPTION_ENTRY_LEN] = entry
            .get(..SAVER_OPTION_ENTRY_LEN)
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or(Errno::BufferTooSmall)?;
        let key = OptionKey::from_wire(entry[OFF_KEY_LEN], &inline(entry, OFF_KEY))?;
        let label = BoundedText::from_wire(entry[OFF_LABEL_LEN], &inline(entry, OFF_LABEL))?;
        let note = BoundedText::from_wire(entry[OFF_NOTE_LEN], &inline(entry, OFF_NOTE))?;
        if !zero(&entry[OFF_NOTE_LEN + 1..OFF_KEY]) {
            return Err(Errno::BadMagic);
        }
        let default = read_i32(entry, OFF_DEFAULT);
        let kind = match entry[OFF_KIND] {
            KIND_SWITCH => {
                let default = match default {
                    0 => false,
                    1 => true,
                    _ => return Err(Errno::OutOfRange),
                };
                if entry[OFF_COUNT] != 0
                    || !zero(&entry[OFF_MIN..OFF_NOTE_LEN])
                    || !zero(&entry[OFF_UNIT..OFF_NOTE])
                    || !zero(&entry[OFF_CHOICES..])
                {
                    return Err(Errno::BadMagic);
                }
                SaverOptionKind::Switch { default }
            }
            KIND_CHOICE => {
                let count = usize::from(entry[OFF_COUNT]);
                if count > SAVER_MAX_CHOICES {
                    return Err(Errno::LengthOutOfRange);
                }
                if !zero(&entry[OFF_MIN..OFF_NOTE_LEN])
                    || !zero(&entry[OFF_UNIT..OFF_NOTE])
                    || !zero(&entry[OFF_CHOICES + count * CHOICE_LEN..])
                {
                    return Err(Errno::BadMagic);
                }
                let mut items = [Choice::BLANK; SAVER_MAX_CHOICES];
                for (slot, item) in items.iter_mut().enumerate().take(count) {
                    let at = OFF_CHOICES + slot * CHOICE_LEN;
                    *item = Choice {
                        value: ChoiceValue::from_wire(entry[at], &inline(entry, at + 2))?,
                        label: BoundedText::from_wire(
                            entry[at + 1],
                            &inline(entry, at + 2 + SAVER_CHOICE_VALUE_MAX),
                        )?,
                    };
                }
                SaverOptionKind::Choice {
                    choices: Choices::new(&items[..count])?,
                    default: u8::try_from(default).map_err(|_| Errno::OutOfRange)?,
                }
            }
            KIND_RANGE => {
                if !zero(&entry[OFF_CHOICES..]) {
                    return Err(Errno::BadMagic);
                }
                SaverOptionKind::Range {
                    range: Range::new(
                        read_i32(entry, OFF_MIN),
                        read_i32(entry, OFF_MAX),
                        u32::from_le_bytes(inline(entry, OFF_STEP)),
                    )?,
                    default,
                    unit: BoundedText::from_wire(entry[OFF_COUNT], &inline(entry, OFF_UNIT))?,
                }
            }
            _ => return Err(Errno::BadMagic),
        };
        Self::new(key, (label, note), kind)
    }
}

/// A set of the privileged feeds a saver declares or is granted, held as the
/// manifest's own [`APPINFO_FLAGS_SAVER_FEEDS`] bits.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct SaverFeeds(u32);

impl SaverFeeds {
    /// No privileged feed.
    pub const NONE: Self = Self(0);

    /// The feeds the manifest `flags` declare.
    #[must_use]
    pub const fn of_flags(flags: u32) -> Self {
        Self(flags & APPINFO_FLAGS_SAVER_FEEDS)
    }

    /// The set `bits` spell.
    ///
    /// # Errors
    ///
    /// [`Errno::BadMagic`] for a bit naming no feed.
    pub const fn from_bits(bits: u32) -> Result<Self, Errno> {
        if bits & !APPINFO_FLAGS_SAVER_FEEDS != 0 {
            return Err(Errno::BadMagic);
        }
        Ok(Self(bits))
    }

    /// The set as its flag bits.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Whether the set holds no feed.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The machine's readings ([`APPINFO_FLAG_FEED_MACHINE`]).
    #[must_use]
    pub const fn machine(self) -> bool {
        self.0 & APPINFO_FLAG_FEED_MACHINE != 0
    }

    /// Picture keeping ([`APPINFO_FLAG_FEED_KEEP`]).
    #[must_use]
    pub const fn keep(self) -> bool {
        self.0 & APPINFO_FLAG_FEED_KEEP != 0
    }

    /// Slides ([`APPINFO_FLAG_FEED_SLIDES`]).
    #[must_use]
    pub const fn slides(self) -> bool {
        self.0 & APPINFO_FLAG_FEED_SLIDES != 0
    }

    /// Whether the feed area — the second picture of the region — is used.
    #[must_use]
    pub const fn uses_feed_area(self) -> bool {
        self.keep() || self.slides()
    }
}

/// The value of one option.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SaverValue {
    /// A switch's.
    Switch(bool),
    /// A choice's, by its spelling.
    Choice(ChoiceValue),
    /// A range's.
    Range(i32),
}

fn inline<const N: usize>(entry: &[u8; SAVER_OPTION_ENTRY_LEN], at: usize) -> [u8; N] {
    let mut out = [0u8; N];
    out.copy_from_slice(&entry[at..at + N]);
    out
}

fn read_i32(entry: &[u8; SAVER_OPTION_ENTRY_LEN], at: usize) -> i32 {
    i32::from_le_bytes(inline(entry, at))
}

fn put_i32(entry: &mut [u8; SAVER_OPTION_ENTRY_LEN], at: usize, value: i32) {
    entry[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

fn zero(bytes: &[u8]) -> bool {
    bytes.iter().all(|&b| b == 0)
}

#[cfg(test)]
#[path = "saver_tests.rs"]
mod tests;
