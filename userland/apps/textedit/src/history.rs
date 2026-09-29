//! Undo and redo: groups of document changes, each with the selection before
//! and after it, a typing run folded into one group.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use crate::detect::LineEnding;
use crate::document::{Change, OutOfMemory};
use crate::selection::Selection;

/// What made a group, which decides whether the next edit may join it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Kind {
    /// Characters typed one after another.
    Typing,
    /// Anything else: a deletion, a paste, a replace, a conversion.
    Other,
}

/// What undoing or redoing a group puts back.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Restored {
    /// The selection to restore.
    pub selection: Selection,
    /// The line-ending convention to restore, for a group that converted it.
    pub eol: Option<LineEnding>,
}

/// One undoable step.
#[derive(Clone, Debug)]
struct Group {
    changes: Vec<Change>,
    before: Selection,
    after: Selection,
    kind: Kind,
    /// The convention before and after, for a conversion of every line
    /// break, so the setting is undone with the bytes.
    eol: Option<[LineEnding; 2]>,
}

/// The history of one document.
#[derive(Debug, Default)]
pub struct History {
    undo: VecDeque<Group>,
    redo: Vec<Group>,
    /// How many groups deep the undo stack was when the document last
    /// matched its file; `None` once that state can no longer be reached.
    saved: Option<usize>,
    /// Whether the newest group may still take the next typed character.
    open: bool,
}

impl History {
    /// An empty history of a document that matches its file.
    #[must_use]
    pub fn new() -> Self {
        Self {
            saved: Some(0),
            ..Self::default()
        }
    }

    /// Record `change`, made with `before` selected and leaving `after`.
    pub fn record(&mut self, change: Change, before: Selection, after: Selection, kind: Kind) {
        if self.saved.is_some_and(|depth| depth > self.undo.len()) {
            self.saved = None;
        }
        self.redo.clear();
        // The group the file was saved at never grows: the flag that says the
        // document is unmodified must stay true of it.
        let at_saved = self.saved == Some(self.undo.len());
        let mut change = change;
        if self.open && kind == Kind::Typing && !at_saved {
            if let Some(group) = self
                .undo
                .back_mut()
                .filter(|group| group.kind == Kind::Typing)
            {
                if let Some(last) = group.changes.last_mut() {
                    match last.absorb(change) {
                        Ok(()) => {
                            group.after = after;
                            return;
                        }
                        // Overwriting in place continues the run too, a change
                        // of its own each: what hex typing is.
                        Err(apart)
                            if apart.at == last.at || apart.at == last.at + last.inserted_len() =>
                        {
                            group.changes.push(apart);
                            group.after = after;
                            return;
                        }
                        Err(apart) => change = apart,
                    }
                }
            }
        }
        self.push(alloc::vec![change], before, after, kind);
    }

    /// Record `change`, which converted every line break from `eol[0]` to
    /// `eol[1]`, as one step that undoes the convention with the bytes.
    pub fn record_conversion(
        &mut self,
        change: Change,
        before: Selection,
        after: Selection,
        eol: [LineEnding; 2],
    ) {
        self.record(change, before, after, Kind::Other);
        if let Some(group) = self.undo.back_mut() {
            group.eol = Some(eol);
        }
    }

    fn push(&mut self, changes: Vec<Change>, before: Selection, after: Selection, kind: Kind) {
        self.undo.push_back(Group {
            changes,
            before,
            after,
            kind,
            eol: None,
        });
        self.open = kind == Kind::Typing;
    }

    /// Stop the newest group taking further typing: the caret moved, or
    /// something other than typing happened.
    pub fn close_run(&mut self) {
        self.open = false;
    }

    /// The changes the next undo takes back, for the room it needs to be
    /// made first.
    #[must_use]
    pub fn next_undo(&self) -> Option<&[Change]> {
        self.undo.back().map(|group| group.changes.as_slice())
    }

    /// The changes the next redo puts back.
    #[must_use]
    pub fn next_redo(&self) -> Option<&[Change]> {
        self.redo.last().map(|group| group.changes.as_slice())
    }

    /// Undo the newest group through `revert`, which takes each change back
    /// out newest first; answers what to restore.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`] when the redo stack cannot take the group; nothing
    /// has been undone.
    pub fn undo(
        &mut self,
        mut revert: impl FnMut(&Change),
    ) -> Result<Option<Restored>, OutOfMemory> {
        self.redo.try_reserve(1).map_err(|_| OutOfMemory)?;
        let Some(group) = self.undo.pop_back() else {
            return Ok(None);
        };
        for change in group.changes.iter().rev() {
            revert(change);
        }
        let restored = Restored {
            selection: group.before,
            eol: group.eol.map(|[before, _]| before),
        };
        self.redo.push(group);
        self.open = false;
        Ok(Some(restored))
    }

    /// Redo the newest undone group through `reapply`, which puts each
    /// change back oldest first; answers what to restore.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`] when the undo stack cannot take the group; nothing
    /// has been redone.
    pub fn redo(
        &mut self,
        mut reapply: impl FnMut(&Change),
    ) -> Result<Option<Restored>, OutOfMemory> {
        self.undo.try_reserve(1).map_err(|_| OutOfMemory)?;
        let Some(group) = self.redo.pop() else {
            return Ok(None);
        };
        for change in &group.changes {
            reapply(change);
        }
        let restored = Restored {
            selection: group.after,
            eol: group.eol.map(|[_, after]| after),
        };
        self.undo.push_back(group);
        self.open = false;
        Ok(Some(restored))
    }

    /// Every change the history can still undo or redo.
    pub fn changes(&self) -> impl Iterator<Item = &Change> {
        self.undo
            .iter()
            .chain(&self.redo)
            .flat_map(|group| &group.changes)
    }

    /// Whether there is anything to undo.
    #[must_use]
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    /// Whether there is anything to redo.
    #[must_use]
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Whether the document differs from its file.
    #[must_use]
    pub fn is_modified(&self) -> bool {
        self.saved != Some(self.undo.len())
    }

    /// The document now matches its file.
    pub fn mark_saved(&mut self) {
        self.saved = Some(self.undo.len());
        self.open = false;
    }

    /// The file now holds a state of the document this history can no
    /// longer name: it differs from the file until the next save.
    pub fn forget_saved(&mut self) {
        self.saved = None;
        self.open = false;
    }

    /// Forget all but the newest `keep` groups, and every redo: what memory
    /// pressure costs the history before it costs the document.
    pub fn trim(&mut self, keep: usize) {
        let drop = self.undo.len().saturating_sub(keep);
        self.undo.drain(..drop);
        self.saved = self.saved.and_then(|depth| depth.checked_sub(drop));
        self.redo.clear();
    }

    /// How many groups can be undone.
    #[cfg(test)]
    pub(crate) fn depth(&self) -> usize {
        self.undo.len()
    }
}

#[cfg(test)]
#[path = "history_tests.rs"]
mod tests;
