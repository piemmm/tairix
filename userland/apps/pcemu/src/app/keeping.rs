//! The machines suspended whose state is not yet kept.
//!
//! Each holds the window it ran in until its state is written: a session
//! logging out waits on its applications' open windows and then ends them, so
//! a window closed while the state was still on its way to the store would let
//! the session end the program part-way through writing it.

use alloc::string::String;
use alloc::vec::Vec;

use super::state::{LastScreen, Suspended};

/// One machine suspended, whose state is not yet kept.
#[derive(Debug)]
struct Entry<W> {
    id: u32,
    name: String,
    /// Its last screen and snapshot, until its images are durable.
    waiting: Option<(LastScreen, Suspended)>,
    /// What went to be written holds the snapshot.
    with_snapshot: bool,
    window: W,
}

/// A suspend ended, its state kept or not to be, and the window it held.
#[derive(Debug, PartialEq, Eq)]
pub struct Ended<W> {
    /// What the machine is called.
    pub name: String,
    /// Whether what went to be written held the snapshot.
    pub with_snapshot: bool,
    /// The window it ran in, to close now.
    pub window: W,
}

/// The machines being kept as they suspend, each holding its window open.
#[derive(Debug)]
pub struct Suspends<W> {
    entries: Vec<Entry<W>>,
}

impl<W> Default for Suspends<W> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
        }
    }
}

impl<W> Suspends<W> {
    /// None being kept.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether none is being kept.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// How many are being kept.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether machine `id` is being kept.
    #[must_use]
    pub fn holds(&self, id: u32) -> bool {
        self.entries.iter().any(|entry| entry.id == id)
    }

    /// Make room for one more, answering whether there is: the
    /// [`suspend`](Self::suspend) that follows takes it without allocating.
    pub fn reserve(&mut self) -> bool {
        self.entries.try_reserve(1).is_ok()
    }

    /// Keep machine `id`, called `name`, whose last screen and snapshot
    /// wait on its images being durable, holding `window` open meanwhile.
    pub fn suspend(
        &mut self,
        (id, name): (u32, String),
        waiting: (LastScreen, Suspended),
        window: W,
    ) {
        self.entries.push(Entry {
            id,
            name,
            waiting: Some(waiting),
            with_snapshot: false,
            window,
        });
    }

    /// Machine `id`'s images are durable, or not: once, its name, its last
    /// screen, and its snapshot where they are, to go to be written. Its
    /// window stays held until the write [`end`](Self::end)s.
    pub fn synced(
        &mut self,
        id: u32,
        durable: bool,
    ) -> Option<(&str, LastScreen, Option<Suspended>)> {
        let entry = self.entries.iter_mut().find(|entry| entry.id == id)?;
        let (screen, suspended) = entry.waiting.take()?;
        entry.with_snapshot = durable;
        Some((&entry.name, screen, durable.then_some(suspended)))
    }

    /// Machine `id`'s state is kept, or will not be: its suspend, ended.
    pub fn end(&mut self, id: u32) -> Option<Ended<W>> {
        let at = self.entries.iter().position(|entry| entry.id == id)?;
        let Entry {
            name,
            with_snapshot,
            window,
            ..
        } = self.entries.remove(at);
        Some(Ended {
            name,
            with_snapshot,
            window,
        })
    }
}

#[cfg(test)]
#[path = "keeping_tests.rs"]
mod tests;
