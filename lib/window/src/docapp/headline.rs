//! The headlines a document window's refusals are stated under.
//!
//! Each names the document by its file's own name, which is untrusted, so a
//! character that would draw as nothing — or reorder the line around it —
//! shows as one that does.

use alloc::format;
use alloc::string::String;

use tairix_util::utf8::shown;

/// The headline of a refusal to open the document `name`.
#[must_use]
pub(super) fn not_opened(name: &str) -> String {
    format!("\u{201c}{}\u{201d} could not be opened", shown(name))
}

/// The headline of a refusal to save the document `name`.
#[must_use]
pub(super) fn not_saved(name: &str) -> String {
    format!("\u{201c}{}\u{201d} could not be saved", shown(name))
}

/// The headline of a refusal to show where the document `name` lives.
#[must_use]
pub(super) fn not_shown(name: &str) -> String {
    format!(
        "\u{201c}{}\u{201d} could not be shown in its folder",
        shown(name)
    )
}

#[cfg(test)]
mod tests {
    use super::{not_opened, not_saved, not_shown};

    #[test]
    fn a_documents_name_is_quoted_with_what_hides_in_it_made_visible() {
        assert_eq!(
            not_opened("notes.txt"),
            "\u{201c}notes.txt\u{201d} could not be opened"
        );
        assert_eq!(
            not_saved("evil\u{202e}txt.exe"),
            "\u{201c}evil\u{fffd}txt.exe\u{201d} could not be saved",
            "a direction override could otherwise make the name read as another"
        );
        assert_eq!(
            not_shown("a\u{1b}[2Jb"),
            "\u{201c}a\u{fffd}[2Jb\u{201d} could not be shown in its folder",
            "a control character never reaches the text it is stated in"
        );
    }
}
