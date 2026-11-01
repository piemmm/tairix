//! What a document host's window says of its document: the title, kept in
//! step with what it reads, and the status it shows while the document opens.

use alloc::string::String;

/// The status a window shows while `name` opens, the name as shown.
pub(super) fn opening(name: &str) -> String {
    alloc::format!("Opening {}\u{2026}", tairix_util::utf8::shown(name))
}

/// The title a window shows and the one it now reads, so a present tells the
/// session only of a title that changed.
pub(super) struct Title {
    shown: String,
    /// What the title now reads: written over before it is compared with
    /// `shown`, so a paint builds no title of its own, and left as it was by
    /// a writer that only writes a change.
    pub(super) draft: String,
}

impl Title {
    pub(super) fn new(shown: String) -> Self {
        Self {
            draft: shown.clone(),
            shown,
        }
    }

    /// The title to send before the next present, if it is not the one shown.
    pub(super) fn changed(&self) -> Option<&str> {
        (self.draft != self.shown).then_some(self.draft.as_str())
    }

    /// Record that the session now shows the draft.
    pub(super) fn sent(&mut self) {
        self.shown.clone_from(&self.draft);
    }

    /// Set the draft to `title`, reusing its room.
    pub(super) fn read(&mut self, title: &str) {
        self.draft.clear();
        self.draft.push_str(title);
    }
}

#[cfg(test)]
mod tests {
    use super::{opening, Title};
    use alloc::string::String;

    /// The opening status names the document as its title does, so what
    /// hides in a name cannot make it read as another's.
    #[test]
    fn the_opening_status_names_the_document_as_shown() {
        assert_eq!(opening("notes.txt"), "Opening notes.txt\u{2026}");
        assert_eq!(opening("a\u{202e}b.txt"), "Opening a\u{fffd}b.txt\u{2026}");
    }

    /// A tool window's writer writes only a change, and a title it never wrote
    /// is never sent: a new window's title is already shown, so repeated
    /// presents send nothing, never a blank, and a change is sent once.
    #[test]
    fn a_title_written_only_on_change_is_sent_once_and_never_blanked() {
        let mut title = Title::new(String::from("Tools"));
        for _ in 0..3 {
            assert_eq!(title.changed(), None, "a shown title is not sent again");
        }
        title.read("Layers");
        assert_eq!(title.changed(), Some("Layers"));
        title.sent();
        for _ in 0..3 {
            assert_eq!(title.changed(), None, "nor a sent one");
        }
    }

    /// A title the session refused stays owed until it is sent.
    #[test]
    fn a_refused_title_is_offered_again() {
        let mut title = Title::new(String::from("Tools"));
        title.read("Layers");
        assert_eq!(title.changed(), Some("Layers"));
        assert_eq!(title.changed(), Some("Layers"), "unsent, still owed");
    }
}
