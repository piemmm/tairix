//! What plays, in what order, and how much of each: the programme the engine
//! asks as it goes.
//!
//! The engine names an entry only by its [`EntryId`], which survives the
//! programme being reordered or edited beneath it, and asks the programme for
//! what follows each entry as it reaches it. A pass is one run through the
//! programme; [`Programme::first_of_pass`] decides whether another begins.

use alloc::string::String;
use alloc::vec::Vec;
use core::num::NonZeroU32;

use crate::Span;

/// A stable name for one entry of a programme.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct EntryId(u64);

impl EntryId {
    /// The entry named `raw`.
    #[must_use]
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    /// The name's raw value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// The part of an entry a pass plays.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Extent {
    /// Where it begins.
    pub start: Span,
    /// How much of it plays from there, or all of it.
    pub duration: Option<Span>,
}

/// Why the engine is moving past an entry.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Advance {
    /// It played to its end.
    Played,
    /// The listener asked for the next one.
    Skipped,
}

/// The order a playback follows.
///
/// An entry an edit removes still answers `next` with what followed it until
/// [`Programme::settle`], so the engine can go on from an entry that was being
/// heard when it went; after that it has no neighbours. An entry the programme
/// does not hold has no item.
pub trait Programme {
    /// What an entry names for the engine's files to open: a path, or a
    /// descriptor the program holds.
    type Item: ?Sized;
    /// The entry pass `pass` begins with, or [`None`] when there is no such
    /// pass and playback ends.
    fn first_of_pass(&self, pass: u32) -> Option<EntryId>;
    /// The entry after `entry` in the same pass, or [`None`] at its end.
    fn next(&self, entry: EntryId, why: Advance) -> Option<EntryId>;
    /// The entry before `entry` in the same pass, or [`None`] at its start.
    fn previous(&self, entry: EntryId) -> Option<EntryId>;
    /// What `entry` names, while the programme holds it.
    fn item(&self, entry: EntryId) -> Option<&Self::Item>;
    /// The part of `entry` a pass plays.
    fn extent(&self, entry: EntryId) -> Extent;
    /// The engine has re-planned against the last edit: what was kept for the
    /// entries it removed may be let go.
    fn settle(&mut self) {}
}

/// How many times a list is played.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Passes {
    /// This many times in all.
    Times(NonZeroU32),
    /// Until stopped.
    Forever,
}

impl Passes {
    /// Once through.
    pub const ONCE: Self = Self::Times(NonZeroU32::MIN);

    /// Whether pass `pass`, counted from zero, is one of these.
    #[must_use]
    pub const fn includes(self, pass: u32) -> bool {
        match self {
            Self::Times(times) => pass < times.get(),
            Self::Forever => true,
        }
    }
}

/// A fixed list of files, each played over one extent, for a number of
/// passes: what a command line asks for.
///
/// An entry's name is its place in the list.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct List {
    paths: Vec<String>,
    extent: Extent,
    passes: Passes,
}

impl List {
    /// `paths` in order, each played over `extent`, `passes` times.
    #[must_use]
    pub const fn new(paths: Vec<String>, extent: Extent, passes: Passes) -> Self {
        Self {
            paths,
            extent,
            passes,
        }
    }

    /// The entry at `index`, when the list is that long.
    #[must_use]
    pub fn entry(&self, index: usize) -> Option<EntryId> {
        (index < self.paths.len())
            .then(|| u64::try_from(index).ok().map(EntryId::new))
            .flatten()
    }

    /// The place `entry` holds in the list.
    #[must_use]
    pub fn index(&self, entry: EntryId) -> Option<usize> {
        usize::try_from(entry.get())
            .ok()
            .filter(|&index| index < self.paths.len())
    }

    /// The files, in order.
    #[must_use]
    pub fn paths(&self) -> &[String] {
        &self.paths
    }
}

impl Programme for List {
    type Item = str;

    fn first_of_pass(&self, pass: u32) -> Option<EntryId> {
        self.passes.includes(pass).then(|| self.entry(0)).flatten()
    }

    fn next(&self, entry: EntryId, _why: Advance) -> Option<EntryId> {
        self.entry(self.index(entry)?.checked_add(1)?)
    }

    fn previous(&self, entry: EntryId) -> Option<EntryId> {
        self.entry(self.index(entry)?.checked_sub(1)?)
    }

    fn item(&self, entry: EntryId) -> Option<&str> {
        self.paths.get(self.index(entry)?).map(String::as_str)
    }

    fn extent(&self, entry: EntryId) -> Extent {
        if self.index(entry).is_some() {
            self.extent
        } else {
            Extent::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::ToString;
    use alloc::vec;

    use super::{Advance, EntryId, Extent, List, Passes, Programme};
    use crate::Span;

    fn list(passes: Passes) -> List {
        let extent = Extent {
            start: Span::from_nanos(5),
            duration: None,
        };
        List::new(vec!["a".to_string(), "b".to_string()], extent, passes)
    }

    #[test]
    fn a_list_is_walked_in_order_for_its_passes() {
        let twice = list(Passes::Times(core::num::NonZeroU32::new(2).expect("two")));
        let (a, b) = (EntryId::new(0), EntryId::new(1));
        assert_eq!(twice.first_of_pass(0), Some(a));
        assert_eq!(twice.first_of_pass(1), Some(a));
        assert_eq!(twice.first_of_pass(2), None);
        assert_eq!(twice.next(a, Advance::Played), Some(b));
        assert_eq!(twice.next(b, Advance::Skipped), None);
        assert_eq!(twice.previous(b), Some(a));
        assert_eq!(twice.previous(a), None);
        assert_eq!(twice.item(b), Some("b"));
        assert_eq!(twice.extent(a).start, Span::from_nanos(5));
        assert_eq!(list(Passes::Forever).first_of_pass(u32::MAX), Some(a));
    }

    #[test]
    fn an_entry_the_list_does_not_hold_has_no_neighbours() {
        let once = list(Passes::ONCE);
        let stray = EntryId::new(2);
        assert_eq!(once.next(stray, Advance::Played), None);
        assert_eq!(once.previous(stray), None);
        assert_eq!(once.item(stray), None);
        assert_eq!(once.extent(stray), Extent::default());
        assert_eq!(once.index(EntryId::new(u64::MAX)), None);
        let empty = List::new(vec![], Extent::default(), Passes::Forever);
        assert_eq!(empty.first_of_pass(0), None);
    }
}
