//! [`DisclosureSet`]: which sections of a list open in place are showing their
//! pages, and [`tree_step`]: what the tree keys do in such a list.
//!
//! Sections disclose independently. Opening one never closes another, in any
//! list the desktop draws — a settings sidebar, a program catalog's folders —
//! because a list that shuts the section a reader was in as they open the
//! next throws away where they had got to. The set is the one model every such
//! list keeps, and the step the one keyboard rule, so none carries a policy of
//! its own.

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

/// A tree key: which way it moves through a two-level list.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TreeKey {
    /// Right: into a section.
    Inward,
    /// Left: out of a section.
    Outward,
}

/// One row of a two-level list, as the tree keys read it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct TreeRow {
    /// Whether the row's own pages are shown, or `None` when it discloses none.
    pub disclosure: Option<bool>,
    /// Whether the row is a page of the section above it.
    pub nested: bool,
}

/// What a tree key asks of a two-level list.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TreeStep {
    /// Show the pages of the section at `index`, or hide them.
    Disclose {
        /// The section's row.
        index: usize,
        /// Whether its pages should be shown.
        open: bool,
    },
    /// Move the cursor onto the row at this index.
    Move(usize),
}

/// What `key` asks on row `current` of `rows`, each row read through `row`.
///
/// Inward shows a closed section's pages, or steps onto the first of them
/// once they are shown; outward hides an open section's pages, or climbs from
/// a page back to the section that disclosed it. Anything else — a plain row,
/// a page inward, a closed section outward, a row that does not exist — asks
/// nothing.
///
/// The list applies the answer: a disclosure is its model's to change, and
/// whether a row may be disclosed at all is its to refuse.
#[must_use]
pub fn tree_step<T>(
    rows: &[T],
    current: usize,
    key: TreeKey,
    row: impl Fn(&T) -> TreeRow,
) -> Option<TreeStep> {
    let here = row(rows.get(current)?);
    match (key, here.disclosure) {
        (TreeKey::Inward, Some(false)) | (TreeKey::Outward, Some(true)) => {
            Some(TreeStep::Disclose {
                index: current,
                open: key == TreeKey::Inward,
            })
        }
        (TreeKey::Inward, Some(true)) => {
            let first = current.checked_add(1)?;
            rows.get(first)
                .is_some_and(|page| row(page).nested)
                .then_some(TreeStep::Move(first))
        }
        (TreeKey::Outward, None) if here.nested => rows
            .get(..current)?
            .iter()
            .rposition(|above| !row(above).nested)
            .map(TreeStep::Move),
        _ => None,
    }
}
