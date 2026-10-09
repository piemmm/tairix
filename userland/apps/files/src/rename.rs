//! The in-place rename while it is under way: the editor, and the move it
//! committed until the volume answers.
//!
//! The move runs on the reader thread, so its answer lands turns after the
//! commit. One rename is under way per window, which is what lets the answer
//! reach only the editor that asked: while one is with the volume no editor
//! opens, and the one that committed takes no input, so nothing typed
//! meanwhile is thrown away and no refusal is stated against a name the user
//! did not commit.

use tairix_browse::PendingRename;
use tairix_controls::text::{TextAction, TextField};
use tairix_geometry::{Rect, Region};
use tairix_input::{Key, Modifiers};

/// A window's rename editor, and the rename it — or the context menu's
/// quick-entry field — handed to the volume.
#[derive(Debug, Default)]
pub struct RenameEdit {
    field: Option<TextField>,
    in_flight: Option<(u64, PendingRename)>,
}

impl RenameEdit {
    /// Open the editor on `field`, answering whether it opened: never while a
    /// rename is with the volume.
    pub fn open(&mut self, field: TextField) -> bool {
        if self.in_flight.is_some() {
            return false;
        }
        self.field = Some(field);
        true
    }

    /// The open editor, to draw.
    #[must_use]
    pub const fn field(&self) -> Option<&TextField> {
        self.field.as_ref()
    }

    /// The open editor, while it may take input: not while the name it
    /// committed is with the volume.
    pub fn editing(&mut self) -> Option<&mut TextField> {
        if self.in_flight.is_some() {
            return None;
        }
        self.field.as_mut()
    }

    /// Whether the editor is open.
    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.field.is_some()
    }

    /// Close the editor.
    pub fn close(&mut self) {
        self.field = None;
    }

    /// Feed `key` to the editor drawn at `bounds`, reporting what it redrew
    /// into `damage`. An Escape closes it; while the name it committed is with
    /// the volume it takes nothing.
    pub fn key(
        &mut self,
        key: Key,
        modifiers: Modifiers,
        bounds: Rect,
        damage: &mut Region,
    ) -> Option<TextAction> {
        let action = self.editing()?.on_key(key, modifiers, bounds, damage);
        if action == Some(TextAction::Cancelled) {
            self.close();
            damage.add(bounds);
        }
        action
    }

    /// Whether a rename is with the volume.
    #[must_use]
    pub const fn in_flight(&self) -> bool {
        self.in_flight.is_some()
    }

    /// Note that `pending` went to the volume under `ticket`. Only with none
    /// in flight: one already there would lose the window its answer.
    pub fn submitted(&mut self, ticket: u64, pending: PendingRename) {
        self.in_flight = Some((ticket, pending));
    }

    /// Whether `ticket` names the rename with the volume here.
    #[must_use]
    pub fn awaits(&self, ticket: u64) -> bool {
        self.in_flight
            .as_ref()
            .is_some_and(|(held, _)| *held == ticket)
    }

    /// Take the rename `ticket` answers, if it is the one with the volume
    /// here.
    pub fn answered(&mut self, ticket: u64) -> Option<PendingRename> {
        if !self.awaits(ticket) {
            return None;
        }
        self.in_flight.take().map(|(_, pending)| pending)
    }
}

#[cfg(test)]
mod tests {
    use super::RenameEdit;
    use crate::test_fs::filled;
    use tairix_browse::PendingRename;
    use tairix_controls::damage;
    use tairix_controls::text::{TextAction, TextField};
    use tairix_geometry::Rect;
    use tairix_input::{Key, Modifiers, NamedKey};

    const BOUNDS: Rect = Rect::new(10, 20, 160, 28);

    fn pending() -> PendingRename {
        let mut browser = filled(3);
        browser.select(0).expect("the listing holds a first file");
        browser
            .prepare_rename("renamed")
            .expect("a fresh name for the chosen file prepares")
    }

    fn editing(text: &str) -> RenameEdit {
        let mut field = TextField::new().with_text(text);
        field.set_focused(true);
        let mut edit = RenameEdit::default();
        assert!(edit.open(field));
        edit
    }

    #[test]
    fn a_committed_editor_takes_no_input_until_the_volume_answers() {
        let mut edit = editing("file-000");
        edit.submitted(7, pending());
        assert!(edit.is_open());
        assert!(edit.editing().is_none());
        let mut redrawn = damage::sink();
        let typed = edit.key(Key::Char('x'), Modifiers::default(), BOUNDS, &mut redrawn);
        assert_eq!(typed, None);
        assert!(redrawn.is_empty());
        assert_eq!(edit.field().map(TextField::text), Some("file-000"));
        assert_eq!(edit.answered(7), Some(pending()));
        assert!(edit.editing().is_some());
    }

    #[test]
    fn a_caret_move_reports_the_editor_it_redrew() {
        let mut edit = editing("abc");
        let mut redrawn = damage::sink();
        let moved = edit.key(
            Key::Named(NamedKey::Left),
            Modifiers::default(),
            BOUNDS,
            &mut redrawn,
        );
        assert_eq!(moved, None);
        assert_eq!(redrawn.rects(), [BOUNDS]);
    }

    #[test]
    fn escape_closes_the_editor_and_reports_where_it_was() {
        let mut edit = editing("abc");
        let mut redrawn = damage::sink();
        let cancelled = edit.key(
            Key::Named(NamedKey::Escape),
            Modifiers::default(),
            BOUNDS,
            &mut redrawn,
        );
        assert_eq!(cancelled, Some(TextAction::Cancelled));
        assert!(!edit.is_open());
        assert_eq!(redrawn.rects(), [BOUNDS]);
    }

    #[test]
    fn no_editor_opens_while_a_rename_is_with_the_volume() {
        let mut edit = RenameEdit::default();
        edit.submitted(3, pending());
        assert!(!edit.open(TextField::new()));
        assert!(!edit.is_open());
        assert!(edit.answered(3).is_some());
        assert!(edit.open(TextField::new()));
    }

    #[test]
    fn an_answer_for_another_rename_is_not_taken() {
        let mut edit = RenameEdit::default();
        edit.submitted(5, pending());
        assert!(!edit.awaits(6));
        assert_eq!(edit.answered(6), None);
        assert!(edit.in_flight());
        assert!(edit.awaits(5));
    }
}
