//! Multi-entry selection: the set of entries the user has marked in the
//! current listing (`plans/NEW-FILEMANAGER.md` `FM7`).
//!
//! The [`Browser`](crate::Browser) has always carried a single *focus* cursor —
//! the entry `Enter`, rename, and activate act on. A file manager also needs a
//! *set* of selected entries so the management verbs (cut / copy / delete) can
//! operate on many items at once. [`Selection`] is that set, modelled here as
//! the one pure, host-provable definition the file manager and the trusted
//! picker share — the picker composes the same [`Browser`](crate::Browser) and
//! simply never invokes the write verbs the selection feeds.
//!
//! The selection is **per-listing**: its members are indices into the entries
//! the browser is currently showing. Another directory's listing or a re-sort
//! collapses it to the single focused entry; a reload or a reported change
//! carries it across by entry ([`Selection::remap`]). Persisting a
//! *cross-directory* set of items for a move or copy is
//! the [`clipboard`](crate::clipboard)'s job, which captures absolute paths at
//! the moment of cut / copy and so survives navigating away.
//!
//! This module holds no authority and touches no filesystem: it is bookkeeping
//! over `usize` indices, bounds-checked by the browser against the live listing
//! before every mutation.

use core::fmt;

use tairix_collections::BitVec;

/// The set of entries selected in the current listing, plus the *anchor* a
/// range extension (`Shift`-click) grows from.
///
/// Indices are positions in the browser's current [`entries`](crate::Browser::entries),
/// held one bit per entry, so a member is tested, added and removed in constant
/// time and the walk visits only the members, low-to-high. The bits grow to the
/// listing they are fitted to; nothing a mutation names past what they could
/// grow to is selected, so a selection with no memory to grow selects less and
/// never aborts its holder.
#[derive(Default)]
pub struct Selection {
    marks: BitVec,
    /// What a remap builds the next marks in, kept so remapping a listing no
    /// larger than an earlier one allocates nothing.
    spare: BitVec,
    anchor: Option<usize>,
}

impl Selection {
    /// An empty selection with no anchor.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            marks: BitVec::new(),
            spare: BitVec::new(),
            anchor: None,
        }
    }

    /// The number of selected entries.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.marks.count_ones()
    }

    /// `true` when no entry is selected.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.marks.count_ones() == 0
    }

    /// Whether `index` is currently selected.
    #[must_use]
    pub fn contains(&self, index: usize) -> bool {
        self.marks.get(index) == Some(true)
    }

    /// The selected indices, low-to-high.
    pub fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        self.marks.iter_ones()
    }

    /// The anchor a [`range_to`](Self::range_to) extension grows from — the
    /// last index a plain-click ([`single`](Self::single)) or toggle
    /// ([`toggle`](Self::toggle)) set. `None` only while the selection is empty.
    #[must_use]
    pub const fn anchor(&self) -> Option<usize> {
        self.anchor
    }

    /// Select exactly `index`, dropping any previous selection — a plain click
    /// or an unmodified keyboard move. `index` becomes the anchor.
    pub fn single(&mut self, index: usize) {
        self.clear();
        if self.mark(index) {
            self.anchor = Some(index);
        }
    }

    /// Add `index` to the selection if absent, or remove it if present — a
    /// `Ctrl`-click. `index` becomes the new anchor either way (the next
    /// range extension grows from the entry the user just acted on).
    pub fn toggle(&mut self, index: usize) {
        let held = self.contains(index);
        if held {
            self.marks.set(index, false);
        } else if !self.mark(index) {
            return;
        }
        self.anchor = Some(index);
    }

    /// Add `index`, answering whether it was absent. Into a selection with no
    /// anchor it becomes the anchor.
    pub fn insert(&mut self, index: usize) -> bool {
        if self.contains(index) || !self.mark(index) {
            return false;
        }
        self.anchor.get_or_insert(index);
        true
    }

    /// Take `index` out, answering whether it was present. An anchor taken out
    /// passes to the first member still selected.
    pub fn remove(&mut self, index: usize) -> bool {
        let removed = self.marks.set(index, false) == Some(true);
        if self.anchor == Some(index) {
            self.anchor = self.marks.next_one(0);
        }
        removed
    }

    /// Select the contiguous range between the current anchor and `index`
    /// (inclusive), replacing any previous selection — a `Shift`-click. The
    /// anchor is left unchanged, so successive `Shift`-clicks all grow from the
    /// same origin. With no anchor yet this is a plain [`single`](Self::single).
    pub fn range_to(&mut self, index: usize) {
        let Some(anchor) = self.anchor else {
            self.single(index);
            return;
        };
        let (lo, hi) = (anchor.min(index), anchor.max(index));
        self.clear();
        if self.fits(hi) {
            self.marks.set_range(lo..hi + 1, true);
        }
        self.anchor = Some(anchor);
    }

    /// Select every entry `0..count` — Select All. The first entry becomes the
    /// anchor; an empty listing (`count == 0`) leaves the selection empty.
    pub fn select_all(&mut self, count: usize) {
        self.clear();
        let Some(last) = count.checked_sub(1).filter(|&last| self.fits(last)) else {
            return;
        };
        self.marks.set_range(0..last + 1, true);
        self.anchor = Some(0);
    }

    /// Drop the whole selection and the anchor.
    pub fn clear(&mut self) {
        let mut at = 0;
        while let Some(index) = self.marks.next_one(at) {
            self.marks.set(index, false);
            at = index + 1;
        }
        self.anchor = None;
    }

    /// Empty the selection over a listing of `count` entries.
    pub fn reset(&mut self, count: usize) {
        self.clear();
        if self.marks.try_resize(count, false).is_err() {
            // What it held goes back to a heap short of memory; a choice
            // grows it again, or is refused.
            self.marks = BitVec::new();
        }
    }

    /// Carry the selection and anchor across a listing change to `count`
    /// entries, through `moved` — an entry's index before it to its index
    /// after, [`None`] for one the change removed, which leaves the selection
    /// rather than passing to whatever now sits where it was.
    /// An anchor whose entry went passes to the first member still selected.
    /// Without the memory to span the listing the selection is emptied rather
    /// than guessed at.
    pub fn remap(&mut self, count: usize, moved: impl Fn(usize) -> Option<usize>) {
        if self.spare.try_resize(count, false).is_err() {
            self.reset(count);
            return;
        }
        self.spare.fill(false);
        for index in self.marks.iter_ones() {
            if let Some(after) = moved(index) {
                self.spare.set(after, true);
            }
        }
        core::mem::swap(&mut self.marks, &mut self.spare);
        self.anchor = self
            .anchor
            .and_then(&moved)
            .or_else(|| self.marks.next_one(0));
    }

    /// A copy of the selection, or `None` without the memory for one.
    #[must_use]
    pub fn try_clone(&self) -> Option<Self> {
        Some(Self {
            marks: self.marks.try_clone().ok()?,
            spare: BitVec::new(),
            anchor: self.anchor,
        })
    }

    /// Mark `index`, growing the bits to hold it; `false` where they cannot.
    fn mark(&mut self, index: usize) -> bool {
        self.fits(index) && self.marks.set(index, true).is_some()
    }

    /// Whether the bits reach `index`, growing them to it where they do not.
    fn fits(&mut self, index: usize) -> bool {
        index < self.marks.len()
            || index
                .checked_add(1)
                .is_some_and(|len| self.marks.try_resize(len, false).is_ok())
    }
}

impl PartialEq for Selection {
    fn eq(&self, other: &Self) -> bool {
        self.anchor == other.anchor && self.iter().eq(other.iter())
    }
}

impl Eq for Selection {}

impl fmt::Debug for Selection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Selection")
            .field(
                "members",
                &fmt::from_fn(|f| f.debug_set().entries(self.iter()).finish()),
            )
            .field("anchor", &self.anchor)
            .finish_non_exhaustive()
    }
}
