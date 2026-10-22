use alloc::vec::Vec;

use super::*;

const SECOND: u64 = 1_000_000_000;

fn sense(key: u8, asc: u8, ascq: u8) -> Sense {
    Sense { key, asc, ascq }
}

#[test]
fn each_answer_is_classified_by_key_asc_and_ascq() {
    let cases = [
        (sense(0x02, 0x04, 0x01), Readiness::BecomingReady),
        (sense(0x02, 0x04, 0x00), Readiness::BecomingReady),
        (sense(0x02, 0x04, 0x02), Readiness::NeedsStart),
        (sense(0x02, 0x04, 0x03), Readiness::NeedsIntervention),
        (sense(0x02, 0x3A, 0x00), Readiness::NoMedium),
        (sense(0x02, 0x3A, 0x02), Readiness::NoMedium),
        (sense(0x06, 0x28, 0x00), Readiness::MediumChanged),
        (sense(0x06, 0x29, 0x00), Readiness::Attention),
        (sense(0x06, 0x2A, 0x01), Readiness::Attention),
    ];
    for (answer, expected) in cases {
        assert_eq!(Readiness::of_sense(answer), expected, "{answer:?}");
    }
    let medium_error = sense(0x03, 0x11, 0x00);
    assert_eq!(
        Readiness::of_sense(medium_error),
        Readiness::Refused(medium_error)
    );
}

/// What a run of a wait did: the instants the unit was tested at, which of
/// them were starts, and how the wait ended.
struct Run {
    tests: Vec<u64>,
    starts: Vec<u64>,
    end: WaitEnd,
}

/// Drive `wait` against a unit that answers `answer(now, started)`, testing
/// exactly when the wait is due, until it ends.
fn run(mut wait: ReadyWait, mut answer: impl FnMut(u64, bool) -> Readiness) -> Run {
    let mut tests = Vec::new();
    let mut starts = Vec::new();
    let mut started = false;
    loop {
        let now = wait.due_ns();
        tests.push(now);
        if wait.owes_start() {
            starts.push(now);
            started = true;
        }
        if let Some(end) = wait.answer(answer(now, started), now) {
            return Run { tests, starts, end };
        }
        assert!(wait.due_ns() >= now, "a wait never tests in the past");
        assert!(tests.len() < 1000, "the wait must end");
    }
}

#[test]
fn a_unit_becoming_ready_after_twenty_seconds_is_served_with_paced_retests() {
    let ready_at = 20 * SECOND;
    let run = run(ReadyWait::new(0, 60 * SECOND), |now, _| {
        if now >= ready_at {
            Readiness::Ready
        } else {
            Readiness::BecomingReady
        }
    });
    assert_eq!(run.end, WaitEnd::Ready);
    assert!(
        run.starts.is_empty(),
        "a unit coming ready by itself is never started"
    );
    for pair in run.tests.windows(2) {
        assert!(
            pair[1] - pair[0] >= READY_POLL_NS,
            "no two tests back to back"
        );
    }
    assert_eq!(run.tests.first(), Some(&0), "the first test is at once");
    assert_eq!(run.tests.last(), Some(&ready_at));
    assert_eq!(run.tests.len(), 21);
}

#[test]
fn an_initializing_command_required_issues_one_start_then_waits() {
    let run = run(ReadyWait::new(0, 60 * SECOND), |now, started| {
        if started && now >= 5 * SECOND {
            Readiness::Ready
        } else {
            Readiness::NeedsStart
        }
    });
    assert_eq!(run.end, WaitEnd::Ready);
    assert_eq!(
        run.starts,
        [0],
        "started at once, once, and only waited on after"
    );
}

#[test]
fn a_wait_for_a_refused_command_is_paced_or_starts_at_once() {
    let coming = ReadyWait::refused(SECOND, 10 * SECOND, false);
    assert_eq!(coming.due_ns(), SECOND + READY_POLL_NS);
    assert!(!coming.owes_start());

    let mut starting = ReadyWait::refused(SECOND, 10 * SECOND, true);
    assert_eq!(starting.due_ns(), SECOND);
    assert!(starting.owes_start());
    // The owed start was the wait's one start: asking again is waited on.
    assert_eq!(starting.answer(Readiness::NeedsStart, SECOND), None);
    assert!(!starting.owes_start());
    assert_eq!(starting.due_ns(), 2 * SECOND);
}

#[test]
fn manual_intervention_fails_closed_with_its_reason() {
    let mut wait = ReadyWait::new(0, 60 * SECOND);
    assert_eq!(
        wait.answer(Readiness::NeedsIntervention, 0),
        Some(WaitEnd::GiveUp(NotReady::Intervention))
    );
}

#[test]
fn a_refusal_that_is_not_about_readiness_fails_closed_with_its_sense() {
    let medium_error = sense(0x03, 0x11, 0x00);
    let mut wait = ReadyWait::new(0, 60 * SECOND);
    assert_eq!(
        wait.answer(Readiness::Refused(medium_error), 0),
        Some(WaitEnd::GiveUp(NotReady::Refused(medium_error)))
    );
}

#[test]
fn a_unit_with_no_medium_is_present_not_failed() {
    let mut wait = ReadyWait::new(0, 60 * SECOND);
    assert_eq!(wait.answer(Readiness::NoMedium, 0), Some(WaitEnd::NoMedium));
}

#[test]
fn a_unit_that_never_comes_ready_is_given_up_at_its_budget() {
    let wait = ReadyWait::new(3 * SECOND, 10 * SECOND);
    let run = run(wait, |_, _| Readiness::BecomingReady);
    assert_eq!(run.end, WaitEnd::GiveUp(NotReady::Budget));
    assert_eq!(run.tests.last(), Some(&wait.deadline_ns()));
}

#[test]
fn an_attention_is_retested_at_once_but_a_storm_of_them_is_paced() {
    let mut wait = ReadyWait::new(0, 60 * SECOND);
    for _ in 0..IMMEDIATE_RETESTS {
        assert_eq!(wait.answer(Readiness::Attention, 0), None);
        assert_eq!(wait.due_ns(), 0);
    }
    assert_eq!(wait.answer(Readiness::Attention, 0), None);
    assert_eq!(wait.due_ns(), READY_POLL_NS);
    // Any other answer ends the run of attentions.
    assert_eq!(wait.answer(Readiness::BecomingReady, SECOND), None);
    assert_eq!(wait.due_ns(), 2 * SECOND);
    assert_eq!(wait.answer(Readiness::MediumChanged, 2 * SECOND), None);
    assert_eq!(wait.due_ns(), 2 * SECOND);
}

#[test]
fn a_unit_ready_to_a_test_but_not_servable_is_given_up_at_its_budget() {
    let failed = Errno::OutOfRange;
    let mut wait = ReadyWait::new(0, 3 * SECOND);
    let mut tests = 0;
    let end = loop {
        let now = wait.due_ns();
        tests += 1;
        assert_eq!(wait.answer(Readiness::Ready, now), Some(WaitEnd::Ready));
        if let Some(why) = wait.retry(NotReady::Unusable(failed), now) {
            break why;
        }
        assert!(wait.due_ns() - now >= READY_POLL_NS || wait.due_ns() == wait.deadline_ns());
        assert!(tests < 100, "the retries must end");
    };
    assert_eq!(end, NotReady::Unusable(failed));
    assert_eq!(tests, 4, "tested at 0, 1, 2 and 3 seconds");
}

#[test]
fn a_held_request_ends_its_wait_when_the_medium_changes_under_it() {
    // A published unit's request, held while the unit comes ready.
    let mut held = ReadyWait::refused(0, 60 * SECOND, false);
    assert_eq!(held.answer(Readiness::Attention, SECOND), None);
    assert_eq!(
        held.answer(Readiness::MediumChanged, SECOND),
        Some(WaitEnd::Changed)
    );
    // A unit coming up has published nothing: the attention is retested.
    let mut arriving = ReadyWait::new(0, 60 * SECOND);
    assert_eq!(arriving.answer(Readiness::MediumChanged, 0), None);
    assert_eq!(arriving.answer(Readiness::Ready, 0), Some(WaitEnd::Ready));
}
