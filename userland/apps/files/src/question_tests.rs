use tairix_abi::window_ipc::{
    AlertButton, AlertButtonRole, AlertOutcome, AlertSeverity, AlertSpec,
};

use super::{Answer, Question};

const YES: usize = 1;

fn spec(headline: &str) -> AlertSpec {
    let cancel = AlertButton::new("Cancel", AlertButtonRole::Cancel).expect("a label");
    let yes = AlertButton::new("Eject", AlertButtonRole::Destructive).expect("a label");
    AlertSpec::new(AlertSeverity::Warning, headline, "", &[cancel, yes]).expect("a spec")
}

fn chosen(index: u8) -> AlertOutcome {
    AlertOutcome::Chosen { index }
}

/// A question asked about `confirms`, up as alert `alert`.
fn up(confirms: &'static str, alert: u64) -> Question<&'static str> {
    let mut question = Question::default();
    question
        .ask(spec("Eject “game”?"), YES, confirms)
        .expect("asked");
    question.raised(alert);
    question
}

#[test]
fn a_yes_carries_out_what_the_question_confirms_once() {
    let mut question = up("eject game", 7);
    assert_eq!(
        question.answer(7, chosen(1)),
        Answer::Confirmed("eject game")
    );
    assert!(!question.is_pending());
    assert_eq!(
        question.answer(7, chosen(1)),
        Answer::Other,
        "answered already"
    );
}

#[test]
fn a_cancel_or_a_dismissal_carries_out_nothing() {
    for outcome in [chosen(0), AlertOutcome::Dismissed] {
        let mut question = up("eject game", 7);
        assert_eq!(question.answer(7, outcome), Answer::Declined);
        assert!(!question.is_pending(), "the question is over");
    }
}

#[test]
fn a_question_asked_while_one_waits_is_refused_and_the_first_kept() {
    let mut question = Question::default();
    question
        .ask(spec("Delete “a”?"), YES, "delete a")
        .expect("asked");
    assert_eq!(
        question.ask(spec("Eject “game”?"), YES, "eject game"),
        Err("eject game"),
        "handed back to be stated"
    );
    assert_eq!(
        question.waiting().map(AlertSpec::headline),
        Some("Delete “a”?")
    );
    question.raised(3);
    assert_eq!(
        question.ask(spec("Eject “game”?"), YES, "eject game"),
        Err("eject game"),
        "nor once it is up"
    );
    assert_eq!(question.answer(3, chosen(1)), Answer::Confirmed("delete a"));
}

#[test]
fn an_answer_to_another_alert_changes_nothing() {
    let mut question = up("eject game", 7);
    assert_eq!(question.answer(8, chosen(1)), Answer::Other);
    assert_eq!(question.is_up(), Some(true));
    assert_eq!(
        question.answer(7, chosen(1)),
        Answer::Confirmed("eject game"),
        "the question still holds what it confirms"
    );
}

#[test]
fn a_question_waits_until_it_goes_up_and_keeps_the_alert_it_went_up_as() {
    let mut question = Question::default();
    assert_eq!(question.is_up(), None);
    question
        .ask(spec("Eject “game”?"), YES, "eject game")
        .expect("asked");
    assert_eq!(question.is_up(), Some(false));
    assert_eq!(question.answer(7, chosen(1)), Answer::Other, "not up yet");
    question.raised(7);
    question.raised(9);
    assert_eq!(question.is_up(), Some(true));
    assert!(question.waiting().is_none());
    assert_eq!(question.answer(9, chosen(1)), Answer::Other);
    assert_eq!(
        question.answer(7, chosen(1)),
        Answer::Confirmed("eject game")
    );
}

#[test]
fn a_withdrawn_question_hands_back_what_it_would_have_confirmed() {
    let mut question = up("eject game", 7);
    assert_eq!(question.withdraw(), Some("eject game"));
    assert!(!question.is_pending());
    assert_eq!(question.withdraw(), None);
    question
        .ask(spec("Delete “a”?"), YES, "delete a")
        .expect("a new question may be asked");
}
