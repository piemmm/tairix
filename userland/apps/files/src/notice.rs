//! The sources a window's notices come from, each keeping at most one
//! waiting in the window's shared [`tairix_window::notices::Notices`] queue.

use alloc::string::String;

use tairix_abi::Errno;
use tairix_util::utf8::shown;

/// What a notice came from.
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
    /// Opening an item in an application, or starting one.
    Open,
    /// What a finished operation's copies could not keep.
    Omission,
    /// Browsing the network.
    Network,
    /// Mounting a disk image, or ejecting a volume.
    Mount,
    /// Volumes disconnected without being ejected.
    Disconnected,
    /// Volumes still attached whose devices failed a re-read.
    Unreadable,
}

/// A window's refusal notices, by where each came from.
pub type Notices = tairix_window::notices::Notices<NoticeSource>;

/// Which of a window's alerts goes up next, one alert a window at a time.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum NextAlert {
    /// Its question.
    Question,
    /// Its next notice, if one waits.
    Notice,
    /// None: the window is answering one already.
    Nothing,
}

/// What goes up next over a window whose question is `question` —
/// `Some(true)` up, `Some(false)` waiting, `None` not asked — and with a
/// notice up when `notice_up`: the question first, since the user has just
/// asked for it, but never over another alert.
#[must_use]
pub const fn next_alert(question: Option<bool>, notice_up: bool) -> NextAlert {
    match question {
        Some(true) => NextAlert::Nothing,
        Some(false) if notice_up => NextAlert::Nothing,
        Some(false) => NextAlert::Question,
        None => NextAlert::Notice,
    }
}

/// What an open's notice says when `application`, which was to show the
/// document, would not start: its name shown as a name from elsewhere is.
#[must_use]
pub fn not_started(application: &str, why: Errno) -> String {
    alloc::format!("{} could not be started: {why}", shown(application))
}

#[cfg(test)]
mod tests {
    use super::{next_alert, not_started, NextAlert};
    use tairix_abi::Errno;

    #[test]
    fn an_application_that_would_not_start_is_named_as_shown() {
        assert_eq!(
            not_started("Vi\u{202e}ew.app", Errno::NotFound),
            alloc::format!("Vi\u{fffd}ew.app could not be started: {}", Errno::NotFound)
        );
    }

    #[test]
    fn a_window_answers_one_alert_at_a_time_its_question_first() {
        assert_eq!(next_alert(Some(false), false), NextAlert::Question);
        assert_eq!(
            next_alert(Some(false), true),
            NextAlert::Nothing,
            "a question waits behind a notice already up"
        );
        assert_eq!(
            next_alert(Some(true), false),
            NextAlert::Nothing,
            "no notice goes up over the question"
        );
        assert_eq!(next_alert(None, false), NextAlert::Notice);
    }
}
