//! [`DisclosureSet`]: which sections of a list open in place are showing their
//! pages.
//!
//! Sections disclose independently. Opening one never closes another, in any
//! list the desktop draws — a settings sidebar, a program catalog's folders —
//! because a list that shuts the section a reader was in as they open the
//! next throws away where they had got to. The set is the one model every such
//! list keeps, so none carries a policy of its own.

use alloc::collections::BTreeSet;

/// Which sections of a list are showing their pages.
///
/// Every section starts in one posture, open or closed, and the set records
/// only the sections the reader has moved from it — so a list of any length
/// costs nothing until its sections are touched, and a section that comes
/// into being later starts where every other one did.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisclosureSet<K> {
    /// Whether a section nobody has touched is showing its pages.
    open_by_default: bool,
    /// The sections in the other posture.
    moved: BTreeSet<K>,
}

impl<K: Ord> DisclosureSet<K> {
    /// A set in which every section starts closed.
    #[must_use]
    pub const fn closed() -> Self {
        Self {
            open_by_default: false,
            moved: BTreeSet::new(),
        }
    }

    /// A set in which every section starts open.
    #[must_use]
    pub const fn open() -> Self {
        Self {
            open_by_default: true,
            moved: BTreeSet::new(),
        }
    }

    /// Whether `section` is showing its pages.
    #[must_use]
    pub fn is_open(&self, section: &K) -> bool {
        self.open_by_default != self.moved.contains(section)
    }

    /// Show `section`'s pages, or hide them, answering whether that moved it.
    ///
    /// Every other section keeps its posture.
    pub fn set(&mut self, section: K, open: bool) -> bool {
        if open == self.open_by_default {
            self.moved.remove(&section)
        } else {
            self.moved.insert(section)
        }
    }

    /// Show `section`'s pages if they are hidden and hide them if they are
    /// shown, answering whether they are now shown.
    pub fn toggle(&mut self, section: K) -> bool {
        let moved = !self.moved.remove(&section);
        if moved {
            self.moved.insert(section);
        }
        self.open_by_default != moved
    }

    /// Put every section back in the posture it started in.
    pub fn reset(&mut self) {
        self.moved.clear();
    }
}
