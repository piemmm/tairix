//! The sources a window's notices come from, each keeping at most one
//! waiting in the window's shared [`tairix_window::notices::Notices`] queue.

use alloc::format;
use alloc::string::String;

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
}

/// A window's refusal notices, by where each came from.
pub type Notices = tairix_window::notices::Notices<NoticeSource>;

/// The headline a refusal to open the item `name` is stated under. The name
/// is untrusted, so a character that would draw as nothing shows as one that
/// does.
#[must_use]
pub fn not_opened(name: &str) -> String {
    format!(
        "\u{201c}{}\u{201d} could not be opened",
        tairix_util::utf8::shown(name)
    )
}

/// Which of a window's alerts goes up next, one alert a window at a time.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum NextAlert {
    /// Its delete question.
    Question,
    /// Its next notice, if one waits.
    Notice,
    /// None: the window is answering one already.
    Nothing,
}

/// What goes up next over a window whose delete question is `question` —
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

#[cfg(test)]
mod tests {
    use super::{next_alert, not_opened, NextAlert};

    #[test]
    fn an_unopened_item_is_named_with_what_hides_in_its_name_shown() {
        assert_eq!(
            not_opened("notes.txt"),
            "\u{201c}notes.txt\u{201d} could not be opened"
        );
        assert_eq!(
            not_opened("evil\u{202e}txt.exe"),
            "\u{201c}evil\u{fffd}txt.exe\u{201d} could not be opened",
            "a direction override could otherwise make the name read as another"
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
