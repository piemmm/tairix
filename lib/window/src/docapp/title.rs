//! What a document host's window says of its document: the title, kept in
//! step with what it reads, and the status it shows while the document opens
//! — once it has been opening long enough to be worth showing.

use alloc::string::String;

/// How long a window opened for a document is kept off the screen while the
/// document is read in: about where a wait stops reading as the computer
/// simply answering and wants saying. A read that lands sooner shows the
/// document's own window and nothing before it; a slower one shows that it
/// is opening.
pub(super) const OPENING_SHOWN_AFTER_NS: u64 = 1_000_000_000;

/// The status a window shows while `name` opens, the name as shown.
pub(super) fn opening(name: &str) -> String {
    alloc::format!("Opening {}\u{2026}", tairix_util::utf8::shown(name))
}

/// Whether a window is kept off the screen while the document it was opened
/// for is read in, and until when.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct Withheld(Option<u64>);

impl Withheld {
    /// Kept off the screen from `now_ns` until its document lands or
    /// [`OPENING_SHOWN_AFTER_NS`] has passed.
    pub(super) const fn since(now_ns: u64) -> Self {
        Self(Some(now_ns.saturating_add(OPENING_SHOWN_AFTER_NS)))
    }

    /// Its document landed, or will not: it is shown from now on.
    pub(super) fn end(&mut self) {
        self.0 = None;
    }

    /// Whether it is still kept off the screen at `now_ns`. Once the wait is
    /// over it is shown from then on, so it is never asked to wake for a wait
    /// already past.
    pub(super) fn holds(&mut self, now_ns: u64) -> bool {
        let holds = self.0.is_some_and(|until| now_ns < until);
        if !holds {
            self.end();
        }
        holds
    }

    /// When it is to be shown for want of its document, while it is kept off
    /// the screen.
    pub(super) const fn until(self) -> Option<u64> {
        self.0
    }
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
    use super::{opening, Title, Withheld, OPENING_SHOWN_AFTER_NS};
    use alloc::string::String;

    /// A window opened for a document stays off the screen until the wait is
    /// over, then shows from then on and asks for no further wake.
    #[test]
    fn a_withheld_window_shows_once_its_wait_is_over() {
        let mut withheld = Withheld::since(1_000);
        let until = 1_000 + OPENING_SHOWN_AFTER_NS;
        assert_eq!(withheld.until(), Some(until));
        assert!(withheld.holds(1_000));
        assert!(withheld.holds(until - 1));
        assert!(!withheld.holds(until), "shown when the wait ends");
        assert_eq!(withheld.until(), None, "no wake asked for a wait past");
        assert!(!withheld.holds(1_000), "and never held back again");
    }

    /// A document that lands, or will not, shows its window at once.
    #[test]
    fn a_withheld_window_shows_once_its_document_lands() {
        let mut withheld = Withheld::since(0);
        withheld.end();
        assert!(!withheld.holds(0));
        assert_eq!(withheld.until(), None);
    }

    /// A window not opened for a document is never held back, and a clock at
    /// its end still gives a wait that ends.
    #[test]
    fn only_an_opening_window_is_held_back() {
        assert!(!Withheld::default().holds(0));
        assert_eq!(Withheld::default().until(), None);
        assert_eq!(Withheld::since(u64::MAX).until(), Some(u64::MAX));
    }

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
