//! What only the desktop session answers — lock the screen now, put a
//! screensaver preview up, say which programs have notified and which
//! screensavers it offers, perform a system task behind its own prompt and
//! hand back what it printed — and which of those requests are outstanding.
//!
//! Each is a round trip to the session's serve loop, which the window's own
//! loop must not wait on, so the caller carries them on a worker and adopts
//! each answer as it lands. At most one of each kind is outstanding: a second
//! Lock pressed while the first is in flight asks nothing the first will not
//! already do, and a Preview of another document is held and asked once the
//! first is answered, the newest replacing any held before it. So is a system
//! task: what one came to arrives as an event, which can be read before the
//! worker has handed back the session's acceptance of it, so the next task
//! the reader asks for may meet that acceptance still outstanding. It
//! performs no I/O.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::pinboard_ipc::PinboardDocument;
use tairix_abi::window_ipc::{NameList, SystemTask};
use tairix_abi::{validate_bundle_id, BundleId, Errno};

use crate::saver::SaverEntry;
use crate::shell::{Authorised, Shell};

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
    /// Say which screensavers the desktop offers.
    Savers,
    /// Perform `task` for window `window_id`, behind the system's prompt.
    Authorise {
        /// The window the outcome is told to.
        window_id: u64,
        /// What to perform.
        task: SystemTask,
    },
    /// Hand back what window `window_id`'s task printed, which exited with
    /// `exit_code`.
    TakeOutput {
        /// The window whose task was captured.
        window_id: u64,
        /// The task's exit status, carried to the answer.
        exit_code: i32,
    },
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
    /// To [`DesktopAsk::Savers`]: the catalogue's entries this build can
    /// name, in its order.
    Savers(Result<Vec<SaverEntry>, Errno>),
    /// To [`DesktopAsk::Authorise`]: whether the prompt came up. What the
    /// task came to arrives later, as an event.
    Authorise(Result<(), Errno>),
    /// To [`DesktopAsk::TakeOutput`]: what the task printed.
    Output {
        /// The task's exit status.
        exit_code: i32,
        /// What it printed.
        output: Result<Vec<u8>, Errno>,
    },
}

impl DesktopAnswer {
    /// What the session would not do, and why, where it refused.
    #[must_use]
    pub fn refusal(&self) -> Option<(&'static str, Errno)> {
        match self {
            Self::Lock(Err(err)) => Some(("lock the screen", *err)),
            Self::Preview(Err(err)) => Some(("show the screensaver", *err)),
            Self::NotifySources(Err(err)) => Some(("say which programs have notified", *err)),
            Self::Savers(Err(err)) => Some(("say which screensavers it offers", *err)),
            Self::Authorise(Err(err)) => Some(("ask for authorisation", *err)),
            Self::Output {
                output: Err(err), ..
            } => Some(("hand back what the command printed", *err)),
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
            Self::Savers(savers) => shell.adopt_savers(savers.ok()),
            Self::Authorise(Ok(())) => {}
            Self::Authorise(Err(err)) => {
                shell.adopt_authorisation(Authorised::Refused(String::from(not_asked(err))));
            }
            Self::Output {
                exit_code,
                output: Ok(output),
            } => shell.adopt_authorisation(Authorised::Printed(exit_code, output)),
            Self::Output { output: Err(_), .. } => shell.adopt_authorisation(Authorised::Refused(
                String::from("The command ran, but what it printed could not be fetched."),
            )),
        }
    }
}

/// Why the desktop would not put its prompt up, as the band says it.
fn not_asked(err: Errno) -> &'static str {
    match err {
        Errno::SeatBusy => "Something else holds the screen. Try again once it has gone.",
        Errno::AlreadyExists => "The desktop is already asking about another change.",
        Errno::OutOfRange => "This change cannot be expressed as a command.",
        Errno::NotSupported => "This desktop cannot ask for authorisation.",
        Errno::PermissionDenied => "This desktop does not take that request from Settings.",
        _ => "The desktop could not be asked.",
    }
}

/// The most requests outstanding at once: one of each kind, which is what a
/// desk carrying them must hold.
pub const MOST_OUTSTANDING: usize = 6;

/// A kind of [`DesktopAsk`] held at most once, by the bit it is outstanding
/// under. A preview and a system task are not: the newest asked behind each
/// is held as well.
#[derive(Copy, Clone)]
enum Single {
    Lock = 1,
    Sources = 1 << 1,
    Output = 1 << 2,
    Savers = 1 << 3,
}

impl Single {
    /// The kind `ask` is, or `None` for one that holds its follower.
    const fn of_ask(ask: &DesktopAsk) -> Option<Self> {
        match ask {
            DesktopAsk::Lock => Some(Self::Lock),
            DesktopAsk::NotifySources => Some(Self::Sources),
            DesktopAsk::Savers => Some(Self::Savers),
            DesktopAsk::TakeOutput { .. } => Some(Self::Output),
            DesktopAsk::Preview(_) | DesktopAsk::Authorise { .. } => None,
        }
    }

    /// The kind `answer` answers, or `None` for one that holds its follower.
    const fn of_answer(answer: &DesktopAnswer) -> Option<Self> {
        match answer {
            DesktopAnswer::Lock(_) => Some(Self::Lock),
            DesktopAnswer::NotifySources(_) => Some(Self::Sources),
            DesktopAnswer::Savers(_) => Some(Self::Savers),
            DesktopAnswer::Output { .. } => Some(Self::Output),
            DesktopAnswer::Preview(_) | DesktopAnswer::Authorise(_) => None,
        }
    }

    const fn bit(self) -> u8 {
        self as u8
    }
}

/// Which kinds of [`DesktopAsk`] are outstanding, and the preview to ask
/// once the one outstanding is answered.
#[derive(Default)]
pub struct DesktopAsks {
    /// Every [`Single`] kind outstanding, by its bit.
    singles: u8,
    /// The document of the preview outstanding.
    preview: Option<PinboardDocument>,
    /// The newest preview asked for while one was outstanding.
    next_preview: Option<PinboardDocument>,
    /// Whether a system task's acceptance is outstanding.
    authorise: bool,
    /// The task asked for while one's acceptance was outstanding.
    next_task: Option<(u64, SystemTask)>,
}

impl DesktopAsks {
    /// None outstanding.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            singles: 0,
            preview: None,
            next_preview: None,
            authorise: false,
            next_task: None,
        }
    }

    /// Mark `ask` outstanding, answering whether it is to be submitted:
    /// `false` where one of its kind already is. A preview of a document
    /// other than the one outstanding is held for [`answered`](Self::answered)
    /// to hand back.
    pub fn ask(&mut self, ask: &DesktopAsk) -> bool {
        if let Some(kind) = Single::of_ask(ask) {
            let free = self.singles & kind.bit() == 0;
            self.singles |= kind.bit();
            return free;
        }
        if let DesktopAsk::Authorise { window_id, task } = *ask {
            if core::mem::replace(&mut self.authorise, true) {
                self.next_task = Some((window_id, task));
                return false;
            }
            return true;
        }
        let DesktopAsk::Preview(document) = ask else {
            return false;
        };
        match self.preview {
            None => {
                self.preview = Some(*document);
                true
            }
            Some(outstanding) => {
                self.next_preview = (outstanding != *document).then_some(*document);
                false
            }
        }
    }

    /// `ask` could not be submitted after all: its kind is free again, and
    /// nothing of it is held to follow.
    pub fn withdraw(&mut self, ask: &DesktopAsk) {
        match (Single::of_ask(ask), ask) {
            (Some(kind), _) => self.singles &= !kind.bit(),
            (None, DesktopAsk::Authorise { .. }) => {
                self.authorise = false;
                self.next_task = None;
            }
            (None, _) => {
                self.preview = None;
                self.next_preview = None;
            }
        }
    }

    /// `answer` landed: its kind is free again, unless a preview or a task
    /// was held behind it, which is now outstanding and handed back to be
    /// submitted.
    pub fn answered(&mut self, answer: &DesktopAnswer) -> Option<DesktopAsk> {
        if let Some(kind) = Single::of_answer(answer) {
            self.singles &= !kind.bit();
            return None;
        }
        if let DesktopAnswer::Authorise(_) = answer {
            let held = self.next_task.take();
            self.authorise = held.is_some();
            return held.map(|(window_id, task)| DesktopAsk::Authorise { window_id, task });
        }
        self.preview = self.next_preview.take();
        self.preview.map(DesktopAsk::Preview)
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
