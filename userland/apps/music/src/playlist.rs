//! The playlist: the entries as the listener arranged them, the order they
//! play in, and what plays after what — the programme the playback engine
//! asks as it goes.
//!
//! Two threads hold one each: the window's, whose entries carry what a row
//! shows, and the playback thread's, whose entries carry the file each names.
//! Every change is [`Playlist::add`] or one of the closed [`Edit`]s, applied
//! to both, and none draws on anything the change does not carry — a shuffle
//! carries its seed — so the two stay alike without either reading the other.
//!
//! # A stable shuffle
//!
//! A shuffled playlist plays in the order of a key each entry draws from the
//! shuffle's seed and its own name, so adding or removing one entry moves no
//! other: what was coming next is still coming next.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec::Vec;

use tairix_player::{Advance, EntryId, Extent, Programme};
use tairix_rng::{NonCryptoRng, RandU64};

/// What plays again when the playlist runs out.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum Repeat {
    /// Nothing: playback ends with the last entry.
    #[default]
    Off,
    /// The whole playlist, from the top.
    All,
    /// The entry playing, until the listener moves on.
    One,
}

impl Repeat {
    /// The mode after this one, in the order the repeat control steps.
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::Off => Self::All,
            Self::All => Self::One,
            Self::One => Self::Off,
        }
    }

    /// The word a setting stores it as.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::All => "all",
            Self::One => "one",
        }
    }

    /// The mode a stored word names.
    #[must_use]
    pub fn from_word(word: &str) -> Option<Self> {
        [Self::Off, Self::All, Self::One]
            .into_iter()
            .find(|mode| mode.word() == word)
    }
}

/// A change to a playlist that carries no entry's payload.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Edit {
    /// Take these entries out.
    Remove(Vec<EntryId>),
    /// Move this entry to this place in the arrangement.
    Move {
        /// The entry.
        entry: EntryId,
        /// Its new place, clamped to the end.
        to: usize,
    },
    /// Play shuffled by this seed, or in the arrangement's order.
    Shuffle(Option<u64>),
    /// Repeat as this says.
    Repeat(Repeat),
    /// Take every entry out.
    Clear,
}

/// The playlist over entries carrying `T`.
#[derive(Clone, Debug)]
pub struct Playlist<T> {
    arranged: Vec<EntryId>,
    playing: Vec<EntryId>,
    entries: BTreeMap<EntryId, T>,
    shuffle: Option<u64>,
    repeat: Repeat,
    /// What followed each entry the last edit removed, in play order, until
    /// the engine settles.
    removed: BTreeMap<EntryId, Option<EntryId>>,
}

impl<T> Default for Playlist<T> {
    fn default() -> Self {
        Self {
            arranged: Vec::new(),
            playing: Vec::new(),
            entries: BTreeMap::new(),
            shuffle: None,
            repeat: Repeat::Off,
            removed: BTreeMap::new(),
        }
    }
}

impl<T> Playlist<T> {
    /// Append `added` to the arrangement, each under the name it carries.
    ///
    /// An entry under a name the playlist already holds keeps its first
    /// payload: a name is never given twice, so a second is a caller's slip,
    /// not a new entry.
    pub fn add(&mut self, added: Vec<(EntryId, T)>) {
        for (entry, payload) in added {
            if let alloc::collections::btree_map::Entry::Vacant(slot) = self.entries.entry(entry) {
                slot.insert(payload);
                self.arranged.push(entry);
            }
        }
        self.reorder();
    }

    /// Make `edit`.
    pub fn apply(&mut self, edit: Edit) {
        match edit {
            Edit::Remove(gone) => self.remove(&gone),
            Edit::Move { entry, to } => {
                if let Some(at) = self.position(entry) {
                    self.arranged.remove(at);
                    let to = to.min(self.arranged.len());
                    self.arranged.insert(to, entry);
                }
            }
            Edit::Shuffle(seed) => self.shuffle = seed,
            Edit::Repeat(repeat) => self.repeat = repeat,
            Edit::Clear => {
                let all = self.arranged.clone();
                self.remove(&all);
            }
        }
        self.reorder();
    }

    /// The entries in the order the listener arranged them.
    #[must_use]
    pub fn arranged(&self) -> &[EntryId] {
        &self.arranged
    }

    /// The entries in the order they play.
    #[must_use]
    pub fn playing(&self) -> &[EntryId] {
        &self.playing
    }

    /// What `entry` carries.
    #[must_use]
    pub fn get(&self, entry: EntryId) -> Option<&T> {
        self.entries.get(&entry)
    }

    /// What `entry` carries, to change it.
    pub fn get_mut(&mut self, entry: EntryId) -> Option<&mut T> {
        self.entries.get_mut(&entry)
    }

    /// Where `entry` stands in the arrangement.
    #[must_use]
    pub fn position(&self, entry: EntryId) -> Option<usize> {
        self.arranged.iter().position(|&held| held == entry)
    }

    /// How many entries the playlist holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.arranged.len()
    }

    /// Whether it holds none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.arranged.is_empty()
    }

    /// The shuffle's seed, while it is shuffled.
    #[must_use]
    pub const fn shuffle(&self) -> Option<u64> {
        self.shuffle
    }

    /// What plays again when it runs out.
    #[must_use]
    pub const fn repeat(&self) -> Repeat {
        self.repeat
    }

    /// Take `gone` out, remembering what followed each in play order.
    fn remove(&mut self, gone: &[EntryId]) {
        let gone: BTreeSet<EntryId> = gone
            .iter()
            .copied()
            .filter(|entry| self.entries.contains_key(entry))
            .collect();
        // Walked backwards, so each removed entry's survivor is the one most
        // recently passed rather than a scan forward per entry.
        let mut after = None;
        for &entry in self.playing.iter().rev() {
            if gone.contains(&entry) {
                self.removed.insert(entry, after);
            } else {
                after = Some(entry);
            }
        }
        self.arranged.retain(|entry| !gone.contains(entry));
        for entry in &gone {
            self.entries.remove(entry);
        }
    }

    /// Put the play order back in step with the arrangement and the shuffle.
    fn reorder(&mut self) {
        self.playing.clone_from(&self.arranged);
        if let Some(seed) = self.shuffle {
            self.playing
                .sort_by_cached_key(|&entry| (shuffle_key(seed, entry), entry));
        }
    }
}

/// Where `entry` falls in a shuffle by `seed`: its own draw, so no other
/// entry's coming or going moves it.
fn shuffle_key(seed: u64, entry: EntryId) -> u64 {
    NonCryptoRng::seed_from_u64(seed ^ entry.get()).next_u64()
}

impl<T> Programme for Playlist<T> {
    type Item = T;

    fn first_of_pass(&self, pass: u32) -> Option<EntryId> {
        (pass == 0 || self.repeat != Repeat::Off)
            .then(|| self.playing.first().copied())
            .flatten()
    }

    fn next(&self, entry: EntryId, why: Advance) -> Option<EntryId> {
        if !self.entries.contains_key(&entry) {
            return self.removed.get(&entry).copied().flatten();
        }
        if self.repeat == Repeat::One && why == Advance::Played {
            return Some(entry);
        }
        let at = self.playing.iter().position(|&held| held == entry)?;
        self.playing.get(at + 1).copied()
    }

    fn previous(&self, entry: EntryId) -> Option<EntryId> {
        let at = self.playing.iter().position(|&held| held == entry)?;
        self.playing.get(at.checked_sub(1)?).copied()
    }

    fn item(&self, entry: EntryId) -> Option<&T> {
        self.entries.get(&entry)
    }

    fn extent(&self, _entry: EntryId) -> Extent {
        Extent::default()
    }

    fn settle(&mut self) {
        self.removed.clear();
    }
}

#[cfg(test)]
#[path = "playlist_tests.rs"]
mod tests;
