use tairix_abi::window_ipc::{AlertButton, AlertButtonRole, AlertOutcome, AlertSeverity};

use super::{next_quit_step, save_changes_alert, CloseAnswer, QuitStanding, QuitStep};

use QuitStanding::{Asking, Clear, Kept, Saving, Unasked};

#[test]
fn the_save_question_names_the_document_and_cannot_be_answered_by_accident() {
    let question = save_changes_alert("notes.txt").expect("it fits an alert");
    assert_eq!(question.severity(), AlertSeverity::Warning);
    assert_eq!(
        question.headline(),
        "Save the changes to \u{201c}notes.txt\u{201d}?"
    );
    let roles: alloc::vec::Vec<AlertButtonRole> =
        question.buttons().map(AlertButton::role).collect();
    assert_eq!(
        roles,
        [
            AlertButtonRole::Cancel,
            AlertButtonRole::Destructive,
            AlertButtonRole::Default
        ]
    );
}

#[test]
fn a_name_too_long_for_the_question_is_shortened_not_refused() {
    let name = "n".repeat(400);
    let question = save_changes_alert(&name).expect("shortened");
    assert!(question.headline().ends_with('\u{2026}'));
}

#[test]
fn each_button_answers_as_it_reads_and_a_dismissal_keeps_the_window() {
    let chosen = |index| CloseAnswer::of(AlertOutcome::Chosen { index });
    assert_eq!(chosen(0), CloseAnswer::Keep);
    assert_eq!(chosen(1), CloseAnswer::Discard);
    assert_eq!(chosen(2), CloseAnswer::Save);
    assert_eq!(chosen(3), CloseAnswer::Keep, "no such button");
    assert_eq!(CloseAnswer::of(AlertOutcome::Dismissed), CloseAnswer::Keep);
}

#[test]
fn a_quit_asks_the_first_window_holding_changes() {
    assert_eq!(next_quit_step([Clear, Unasked, Unasked]), QuitStep::Ask(1));
    assert_eq!(next_quit_step([Clear]), QuitStep::Done);
    assert_eq!(next_quit_step([]), QuitStep::Done);
}

#[test]
fn a_quit_asks_one_window_at_a_time() {
    assert_eq!(next_quit_step([Asking, Unasked]), QuitStep::Wait);
    assert_eq!(next_quit_step([Unasked, Asking]), QuitStep::Wait);
}

#[test]
fn a_window_saving_on_its_way_out_lets_the_next_be_asked() {
    assert_eq!(next_quit_step([Saving, Unasked]), QuitStep::Ask(1));
    assert_eq!(next_quit_step([Saving]), QuitStep::Done);
}

#[test]
fn a_window_kept_open_gives_the_quit_up() {
    assert_eq!(next_quit_step([Unasked, Kept]), QuitStep::GiveUp);
    assert_eq!(next_quit_step([Asking, Kept]), QuitStep::GiveUp);
}
