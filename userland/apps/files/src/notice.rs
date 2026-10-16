//! The refusals a window states, one at a time, in the order they arose.
//!
//! A refusal that arrives while another is showing waits behind it rather than
//! replacing it or being lost. Each source keeps at most one waiting — a newer
//! refusal replaces its own source's older one — so the queue is bounded by the
//! sources a window has, never by how often one of them refuses.

use alloc::collections::VecDeque;

use tairix_controls::decision::Dialog;

/// What a refusal came from.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum NoticeSource {
    /// Reading the folder the window shows.
    Listing,
    /// A long operation that stopped.
    Operation,
    /// A paste or a drop that could not begin.
    Paste,
    /// A one-shot change: a new entry, the Trash.
    Write,
    /// A rename.
    Rename,
}

/// A window's refusal notices: the one showing, and those waiting behind it.
#[derive(Debug, Default)]
pub struct Notices {
    showing: Option<Dialog>,
    waiting: VecDeque<(NoticeSource, Dialog)>,
}

impl Notices {
    /// The notice the window shows, which owns its input until dismissed.
    #[must_use]
    pub const fn showing(&self) -> Option<&Dialog> {
        self.showing.as_ref()
    }

    /// Whether a notice shows.
    #[must_use]
    pub const fn is_showing(&self) -> bool {
        self.showing.is_some()
    }

    /// State `notice` from `source`: at once if nothing shows, else after
    /// those already waiting.
    pub fn state(&mut self, source: NoticeSource, notice: Dialog) {
        if self.showing.is_none() {
            self.showing = Some(notice);
        } else if let Some(waiting) = self.waiting.iter_mut().find(|(from, _)| *from == source) {
            waiting.1 = notice;
        } else {
            self.waiting.push_back((source, notice));
        }
    }

    /// Dismiss the notice showing; the oldest waiting one shows next.
    pub fn dismiss(&mut self) {
        self.showing = self.waiting.pop_front().map(|(_, notice)| notice);
    }
}

#[cfg(test)]
#[path = "notice_tests.rs"]
mod tests;
