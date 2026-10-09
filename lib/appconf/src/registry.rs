//! Closed registries: the fixed set of settings an application keeps under
//! keys of a store's open namespace, read and spelled once here so every
//! registry treats a missing key, a refused value and a key read in the light
//! of another alike.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;
use core::marker::PhantomData;

use crate::{Document, Lookup};

/// The most keys a registry holds: one bit of a [`Keys`] set each.
pub const MOST_KEYS: usize = 64;

/// A record of settings kept under a closed set of keys.
///
/// Its [`Default`] is what a store holding none of them implies, and a key
/// outside the set is one the registry neither reads nor removes.
pub trait Registry: Clone + PartialEq + Default {
    /// One of its settings.
    type Key: Copy + Eq + fmt::Debug + 'static;

    /// Every key, in the order a store is read: a key whose meaning depends on
    /// another's value follows it. At most [`MOST_KEYS`].
    const KEYS: &'static [Self::Key];

    /// `key`'s spelling in the store.
    fn name(key: Self::Key) -> &'static str;

    /// Take `key`'s setting from its stored `text`, answering whether the key
    /// accepts it: a refused value leaves the record as it was.
    fn read(&mut self, key: Self::Key, text: &str) -> bool;

    /// Spell `key`'s setting into `out` as it is stored, answering `false`
    /// where a store says it by not holding the key.
    fn spell(&self, key: Self::Key, out: &mut String) -> bool;

    /// Bring the record within its bounds once read: by default, it is.
    fn normalise(&mut self) {}

    /// The record `source` implies, and every key whose stored value was
    /// refused, its setting left at what the keys read before it imply.
    fn load<L: Lookup + ?Sized>(source: &L) -> (Self, Vec<Self::Key>) {
        let mut record = Self::default();
        let mut refused = Vec::new();
        for &key in Self::KEYS {
            if let Some(text) = source.get(Self::name(key)) {
                if !record.read(key, text) {
                    refused.push(key);
                }
            }
        }
        record.normalise();
        (record, refused)
    }

    /// `keys` of the record as a document, each in its stored spelling and
    /// one a store says by its absence left out.
    ///
    /// A registry's own names and spellings lie inside the format, so a key
    /// the document refuses is a defect of that registry, and leaving it out
    /// is the one answer that publishes nothing wrong.
    fn document_of(&self, keys: &[Self::Key]) -> Document {
        let mut document = Document::new();
        let mut text = String::new();
        for &key in keys {
            text.clear();
            if self.spell(key, &mut text) {
                let _ = document.set(Self::name(key), &text);
            }
        }
        document
    }
}

/// A registry edited live, setting by setting: what an editor changed is told
/// apart from what it left.
pub trait Live: Registry {
    /// Take `key`'s setting from `other`, answering whether that changed it.
    fn take(&mut self, other: &Self, key: Self::Key) -> bool;

    /// The settings `self` and `other` disagree on.
    fn differing(&self, other: &Self) -> Keys<Self> {
        self.clone().set_from(other, Keys::ALL)
    }

    /// Take the settings `keys` names from `other`, answering which of them
    /// changed.
    fn set_from(&mut self, other: &Self, keys: Keys<Self>) -> Keys<Self> {
        let mut changed = Keys::EMPTY;
        for (index, &key) in Self::KEYS.iter().enumerate() {
            let bit = Keys::<Self>::at(index);
            if keys.bits & bit != 0 && self.take(other, key) {
                changed.bits |= bit;
            }
        }
        changed
    }
}

/// Store `value` in `slot`, answering whether that changed it: the body of
/// most [`Live::take`] arms.
pub fn overwrite<T: PartialEq>(slot: &mut T, value: T) -> bool {
    let changed = *slot != value;
    *slot = value;
    changed
}

/// A set of a registry's settings.
pub struct Keys<R> {
    bits: u64,
    registry: PhantomData<fn() -> R>,
}

impl<R: Registry> Keys<R> {
    /// No setting.
    pub const EMPTY: Self = Self::from_bits(0);

    /// Every setting.
    pub const ALL: Self = {
        let () = Self::BOUND;
        let count = R::KEYS.len();
        Self::from_bits(if count == MOST_KEYS {
            u64::MAX
        } else {
            (1 << count) - 1
        })
    };

    /// Refuses at build time a registry past [`MOST_KEYS`].
    const BOUND: () = assert!(
        R::KEYS.len() <= MOST_KEYS,
        "a registry holds at most MOST_KEYS keys"
    );

    /// The bit of the key at `index` of [`Registry::KEYS`].
    const fn at(index: usize) -> u64 {
        let () = Self::BOUND;
        1 << index
    }

    const fn from_bits(bits: u64) -> Self {
        Self {
            bits,
            registry: PhantomData,
        }
    }

    /// The set holding `key` alone.
    #[must_use]
    pub fn of(key: R::Key) -> Self {
        Self::from_bits(Self::bit(key))
    }

    /// Whether the set names no setting.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.bits == 0
    }

    /// Whether the set names `key`.
    #[must_use]
    pub fn contains(self, key: R::Key) -> bool {
        self.bits & Self::bit(key) != 0
    }

    /// Every setting either set names.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self::from_bits(self.bits | other.bits)
    }

    /// `key`'s bit: none for a key outside the registry.
    fn bit(key: R::Key) -> u64 {
        R::KEYS.iter().position(|&at| at == key).map_or(0, Self::at)
    }
}

impl<R> Clone for Keys<R> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<R> Copy for Keys<R> {}

impl<R> PartialEq for Keys<R> {
    fn eq(&self, other: &Self) -> bool {
        self.bits == other.bits
    }
}

impl<R> Eq for Keys<R> {}

impl<R> fmt::Debug for Keys<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Keys({:#x})", self.bits)
    }
}

#[cfg(test)]
#[path = "registry_tests.rs"]
mod tests;
