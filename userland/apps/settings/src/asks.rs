//! What only the desktop session answers — lock the screen now, put a
//! screensaver preview up, say which programs have notified — and which of
//! those requests are outstanding.
//!
//! Each is a round trip to the session's serve loop, which the window's own
//! loop must not wait on, so the caller carries them on a worker and adopts
//! each answer as it lands. At most one of each kind is outstanding: a second
//! Lock pressed while the first is in flight asks nothing the first will not
//! already do, and a Preview of another document is held and asked once the
//! first is answered, the newest replacing any held before it. It performs no
//! I/O.

use alloc::vec::Vec;

use tairix_abi::pinboard_ipc::PinboardDocument;
use tairix_abi::window_ipc::NameList;
use tairix_abi::{validate_bundle_id, BundleId, Errno};

use crate::shell::Shell;

/// A request only the desktop session answers.
#[allow(
    clippy::large_enum_variant,
    reason = "a desk holds at most one of each, so the document is moved, never heap-allocated"
)]
#[derive(Clone, Copy)]
pub enum DesktopAsk {
    /// Lock the screen now.
    Lock,
    /// Put the screensaver the document describes up, as a preview.
    Preview(PinboardDocument),
    /// Say which sources have posted a notification.
    NotifySources,
}

/// What the session answered a [`DesktopAsk`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DesktopAnswer {
    /// To [`DesktopAsk::Lock`].
    Lock(Result<(), Errno>),
    /// To [`DesktopAsk::Preview`].
    Preview(Result<(), Errno>),
    /// To [`DesktopAsk::NotifySources`]: the sources named that this build
    /// accepts as bundle identities.
    NotifySources(Result<Vec<BundleId>, Errno>),
}

impl DesktopAnswer {
    /// What the session would not do, and why, where it refused.
    #[must_use]
    pub fn refusal(&self) -> Option<(&'static str, Errno)> {
        match self {
            Self::Lock(Err(err)) => Some(("lock the screen", *err)),
            Self::Preview(Err(err)) => Some(("show the screensaver", *err)),
            Self::NotifySources(Err(err)) => Some(("say which programs have notified", *err)),
            _ => None,
        }
    }

    /// Adopt the answer into `shell`: a refusal stated on the row that
    /// asked, the sources listed.
    pub fn adopt(self, shell: &mut Shell) {
        match self {
            Self::Lock(answer) => shell.adopt_lock_answer(answer),
            Self::Preview(answer) => shell.adopt_preview_answer(answer),
            Self::NotifySources(sources) => shell.adopt_notify_sources(sources.ok()),
        }
    }
}

/// The most requests outstanding at once: one of each kind, which is what a
/// desk carrying them must hold.
pub const MOST_OUTSTANDING: usize = 3;

/// Which kinds of [`DesktopAsk`] are outstanding, and the preview to ask
/// once the one outstanding is answered.
#[derive(Default)]
pub struct DesktopAsks {
    lock: bool,
    /// The document of the preview outstanding.
    preview: Option<PinboardDocument>,
    /// The newest preview asked for while one was outstanding.
    next_preview: Option<PinboardDocument>,
    sources: bool,
}

impl DesktopAsks {
    /// None outstanding.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            lock: false,
            preview: None,
            next_preview: None,
            sources: false,
        }
    }

    /// Mark `ask` outstanding, answering whether it is to be submitted:
    /// `false` where one of its kind already is. A preview of a document
    /// other than the one outstanding is held for [`answered`](Self::answered)
    /// to hand back.
    pub fn ask(&mut self, ask: &DesktopAsk) -> bool {
        match ask {
            DesktopAsk::Lock => !core::mem::replace(&mut self.lock, true),
            DesktopAsk::NotifySources => !core::mem::replace(&mut self.sources, true),
            DesktopAsk::Preview(document) => match self.preview {
                None => {
                    self.preview = Some(*document);
                    true
                }
                Some(outstanding) => {
                    self.next_preview = (outstanding != *document).then_some(*document);
                    false
                }
            },
        }
    }

    /// `ask` could not be submitted after all: its kind is free again, and
    /// nothing of it is held to follow.
    pub fn withdraw(&mut self, ask: &DesktopAsk) {
        match ask {
            DesktopAsk::Lock => self.lock = false,
            DesktopAsk::NotifySources => self.sources = false,
            DesktopAsk::Preview(_) => {
                self.preview = None;
                self.next_preview = None;
            }
        }
    }

    /// `answer` landed: its kind is free again, unless a preview was held
    /// behind it, which is now outstanding and handed back to be submitted.
    pub fn answered(&mut self, answer: &DesktopAnswer) -> Option<DesktopAsk> {
        match answer {
            DesktopAnswer::Lock(_) => self.lock = false,
            DesktopAnswer::NotifySources(_) => self.sources = false,
            DesktopAnswer::Preview(_) => {
                self.preview = self.next_preview.take();
                return self.preview.map(DesktopAsk::Preview);
            }
        }
        None
    }
}

/// The sources `answered` names that this build accepts as bundle identities,
/// in the order named: a policy could never be kept for any other.
#[must_use]
pub fn notified(answered: &NameList<'_>) -> Vec<BundleId> {
    answered
        .names()
        .filter_map(|name| core::str::from_utf8(name).ok())
        .filter(|name| validate_bundle_id(name).is_ok())
        .filter_map(|name| BundleId::new(name).ok())
        .collect()
}

#[cfg(test)]
#[path = "asks_tests.rs"]
mod tests;
