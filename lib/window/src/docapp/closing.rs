//! Closing document windows that hold changes: the question each asks, how
//! its answer reads, and the order a quit asks them in.

use alloc::format;

use tairix_abi::window_ipc::{
    AlertButton, AlertButtonRole, AlertOutcome, AlertSeverity, AlertSpec,
};

/// The index of *Don't Save* in [`save_changes_alert`]'s question.
const DISCARD: u8 = 1;

/// The index of *Save* in [`save_changes_alert`]'s question.
const SAVE: u8 = 2;

/// The question a document window asks before it closes over changes made
/// to `name`: *Cancel* keeps it open, *Don't Save* gives the changes up, and
/// *Save*, the default, saves and then closes. A stray Enter or Escape
/// therefore loses nothing. `None` only when an alert cannot carry its fixed
/// labels.
#[must_use]
pub(super) fn save_changes_alert(name: &str) -> Option<AlertSpec> {
    let buttons = [
        AlertButton::new("Cancel", AlertButtonRole::Cancel).ok()?,
        AlertButton::new("Don\u{2019}t Save", AlertButtonRole::Destructive).ok()?,
        AlertButton::new("Save", AlertButtonRole::Default).ok()?,
    ];
    AlertSpec::fitted(
        AlertSeverity::Warning,
        &format!("Save the changes to \u{201c}{name}\u{201d}?"),
        "Your changes will be lost if you do not save them.",
        &buttons,
    )
    .ok()
}

/// How the user answered [`save_changes_alert`]'s question.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum CloseAnswer {
    /// Keep the window open, changes and all.
    Keep,
    /// Close it, losing the changes.
    Discard,
    /// Save the changes, then close it.
    Save,
}

impl CloseAnswer {
    /// What `outcome` answers: a dismissal, or a button the question does not
    /// have, keeps the window open.
    #[must_use]
    pub(super) const fn of(outcome: AlertOutcome) -> Self {
        match outcome {
            AlertOutcome::Chosen { index: SAVE } => Self::Save,
            AlertOutcome::Chosen { index: DISCARD } => Self::Discard,
            AlertOutcome::Chosen { .. } | AlertOutcome::Dismissed => Self::Keep,
        }
    }
}

/// Where a document window stands, as far as a quit is concerned.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum QuitStanding {
    /// It holds nothing the quit would lose, or is about to close.
    Clear,
    /// It holds changes this quit has not asked about.
    Unasked,
    /// A question that is part of closing it is up — the save question, its
    /// own of how to save, or the file chooser of that save.
    Asking,
    /// A save that closes it is under way.
    Saving,
    /// This quit asked about it, and the user kept it open.
    Kept,
}

/// What a quit does next.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum QuitStep {
    /// The user kept a window they were asked about: the quit is given up.
    GiveUp,
    /// Something is being asked; the next question waits for its answer.
    Wait,
    /// Ask the window at this index.
    Ask(usize),
    /// Nothing is left to ask.
    Done,
}

/// The next step of a quit over windows standing as `windows` says, in
/// their order: one question at a time, each window asked once.
#[must_use]
pub(super) fn next_quit_step(windows: impl IntoIterator<Item = QuitStanding>) -> QuitStep {
    let mut waiting = false;
    let mut next = None;
    for (index, standing) in windows.into_iter().enumerate() {
        match standing {
            QuitStanding::Kept => return QuitStep::GiveUp,
            QuitStanding::Asking => waiting = true,
            QuitStanding::Unasked => {
                next.get_or_insert(index);
            }
            QuitStanding::Clear | QuitStanding::Saving => {}
        }
    }
    match next {
        _ if waiting => QuitStep::Wait,
        Some(index) => QuitStep::Ask(index),
        None => QuitStep::Done,
    }
}

#[cfg(test)]
#[path = "closing_tests.rs"]
mod tests;
