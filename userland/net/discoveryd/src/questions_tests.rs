//! Unit tests for the front's question table.

use super::{Questions, Verdict};
use crate::wire::Form;
use alloc::vec::Vec;
use tairix_abi::discovery_ipc::{Answer, Change, Entry};
use tairix_abi::net_ipc::IF_NAME_LEN;
use tairix_abi::Errno;
use tairix_hash::HashSeed;
use tairix_net::dns::Name;
use tairix_net::mdns::{MAX_QUESTIONS, MAX_RECORDS};

/// The account every request here is charged to, unless a test says otherwise.
const ACCOUNT: u32 = 1000;

fn iface(name: &[u8]) -> [u8; IF_NAME_LEN] {
    let mut out = [0u8; IF_NAME_LEN];
    out[..name.len()].copy_from_slice(name);
    out
}

fn ipp() -> Name {
    Name::encode("_ipp._tcp.local").unwrap()
}

fn host(n: usize) -> Name {
    Name::encode(&alloc::format!("host{n}.local")).unwrap()
}

fn table() -> Questions {
    Questions::new(HashSeed::from_words(1, 2))
}

fn instance(question: u32, change: Change, label: &[u8]) -> Entry<'_> {
    on(question, b"eth0", change, label)
}

fn on<'a>(question: u32, link: &[u8], change: Change, label: &'a [u8]) -> Entry<'a> {
    Entry::Answer {
        request: question,
        interface: iface(link),
        change,
        ttl: 120,
        answer: Answer::Instance { label },
    }
}

/// The requests `verdict` names.
fn told(questions: &Questions, verdict: &Verdict) -> Vec<(u32, u32)> {
    match *verdict {
        Verdict::Tell(question) => questions.told(question).collect(),
        Verdict::TellOne(session, request) => alloc::vec![(session, request)],
        Verdict::Stale | Verdict::Lie => Vec::new(),
    }
}

/// Ask `id` of the decoder.
fn ask(questions: &mut Questions, id: u32) {
    assert_eq!(questions.next_ask().map(|(asked, ..)| asked), Some(id));
    questions.asked(id);
}

/// A table asking one browse for session 1 request 1, the decoder asked.
fn asked() -> (Questions, u32) {
    let mut questions = table();
    let id = questions
        .subscribe(Form::Instance, ipp(), 1, 1, ACCOUNT)
        .unwrap();
    assert_eq!(questions.next_ask(), Some((id, Form::Instance, ipp())));
    questions.asked(id);
    assert_eq!(questions.next_ask(), None);
    (questions, id)
}

/// Stop `id`, as the front and then the decoder carry it out.
fn stop(questions: &mut Questions, id: u32) {
    assert_eq!(questions.next_stop(), Some(id));
    questions.stopped(id);
    assert!(questions.on_stopped(id));
}

#[test]
fn requests_asking_the_same_question_share_it() {
    let mut questions = table();
    let first = questions
        .subscribe(Form::Instance, ipp(), 1, 1, ACCOUNT)
        .unwrap();
    let second = questions
        .subscribe(Form::Instance, ipp(), 2, 5, ACCOUNT)
        .unwrap();
    assert_eq!(first, second);
    let other = questions
        .subscribe(
            Form::Instance,
            Name::encode("_ssh._tcp.local").unwrap(),
            1,
            2,
            ACCOUNT,
        )
        .unwrap();
    assert_ne!(first, other);
    let text = questions
        .subscribe(Form::Text, ipp(), 1, 3, ACCOUNT)
        .unwrap();
    assert_ne!(first, text, "one name, another form, another question");
    // Nothing was asked of the decoder yet, so nobody is owed a replay.
    assert_eq!(questions.next_replay(), None);
}

#[test]
fn every_answer_moves_as_an_honest_engine_moves_it() {
    let (mut questions, id) = asked();
    for (change, label) in [
        (Change::Added, b"A"),
        (Change::Refreshed, b"a"),
        (Change::Retired, b"A"),
    ] {
        let verdict = questions.on_answer(&instance(id, change, label), true);
        assert_eq!(told(&questions, &verdict), [(1, 1)], "names fold case");
    }
    // What is not held cannot be renewed or retired, and what is held is
    // added once: each of those crossed a flush.
    for (change, label) in [(Change::Refreshed, b"A"), (Change::Retired, b"A")] {
        assert_eq!(
            questions.on_answer(&instance(id, change, label), true),
            Verdict::Stale
        );
    }
    let verdict = questions.on_answer(&instance(id, Change::Added, b"B"), true);
    assert_eq!(told(&questions, &verdict), [(1, 1)]);
    assert_eq!(
        questions.on_answer(&instance(id, Change::Added, b"B"), true),
        Verdict::Stale
    );
}

#[test]
fn an_answer_for_a_question_never_asked_is_a_lie_and_one_stopping_is_stale() {
    let (mut questions, id) = asked();
    assert_eq!(
        questions.on_answer(&instance(id + 1, Change::Added, b"A"), true),
        Verdict::Lie
    );
    questions.unsubscribe(id, 1, 1);
    assert_eq!(questions.next_stop(), Some(id));
    questions.stopped(id);
    assert_eq!(questions.next_stop(), None);
    assert_eq!(
        questions.on_answer(&instance(id, Change::Added, b"A"), true),
        Verdict::Stale,
        "it crossed the stop"
    );
    assert!(questions.on_stopped(id));
    assert_eq!(
        questions.on_answer(&instance(id, Change::Added, b"A"), true),
        Verdict::Lie,
        "nothing about it follows the decoder's word"
    );
}

#[test]
fn a_stopped_word_for_a_question_the_decoder_was_not_told_to_stop_is_a_lie() {
    let (mut questions, id) = asked();
    assert!(!questions.on_stopped(id), "still asked");
    questions.unsubscribe(id, 1, 1);
    assert!(!questions.on_stopped(id), "its stop was never sent");
    questions.stopped(id);
    assert!(questions.on_stopped(id));
    assert!(!questions.on_stopped(id), "said once");
    assert!(!questions.on_stopped(id + 7), "never issued");
}

#[test]
fn an_id_is_reused_only_once_the_decoder_has_said_it_stopped() {
    let (mut questions, first) = asked();
    questions.unsubscribe(first, 1, 1);
    questions.stopped(first);
    let second = questions
        .subscribe(
            Form::Instance,
            Name::encode("_ssh._tcp.local").unwrap(),
            1,
            2,
            ACCOUNT,
        )
        .unwrap();
    assert_ne!(second, first, "the decoder may still answer the first");
    assert!(questions.on_stopped(first));
    let third = questions
        .subscribe(
            Form::Instance,
            Name::encode("_smb._tcp.local").unwrap(),
            1,
            3,
            ACCOUNT,
        )
        .unwrap();
    assert_eq!(third, first);
}

#[test]
fn churning_questions_never_runs_the_ids_out() {
    let mut questions = table();
    for n in 0..4 * MAX_QUESTIONS {
        let id = questions
            .subscribe(Form::AddressV4, host(n), 1, 1, ACCOUNT)
            .unwrap();
        assert!((id as usize) < MAX_QUESTIONS);
        ask(&mut questions, id);
        questions.unsubscribe(id, 1, 1);
        stop(&mut questions, id);
    }
    assert!(questions.is_empty());
}

#[test]
fn an_answer_on_a_link_the_decoder_has_not_settled_is_stale() {
    let (mut questions, id) = asked();
    assert_eq!(
        questions.on_answer(&instance(id, Change::Added, b"A"), false),
        Verdict::Stale
    );
}

#[test]
fn a_link_holds_no_more_than_an_honest_engines_cache_across_every_question() {
    let (mut questions, browse) = asked();
    let text = questions
        .subscribe(Form::Text, ipp(), 1, 2, ACCOUNT)
        .unwrap();
    ask(&mut questions, text);
    for n in 0..MAX_RECORDS {
        let label = alloc::format!("i{n}");
        let id = if n % 2 == 0 { browse } else { text };
        assert!(matches!(
            questions.on_answer(&instance(id, Change::Added, label.as_bytes()), true),
            Verdict::Tell(_)
        ));
    }
    for id in [browse, text] {
        assert_eq!(
            questions.on_answer(&instance(id, Change::Added, b"one more"), true),
            Verdict::Lie
        );
    }
    assert!(
        matches!(
            questions.on_answer(&on(browse, b"wlan0", Change::Added, b"elsewhere"), true),
            Verdict::Tell(_)
        ),
        "another link counts apart"
    );
    // Retiring one makes room for one, and retiring a question gives back
    // everything it held.
    let _ = questions.on_answer(&instance(browse, Change::Retired, b"i0"), true);
    assert!(matches!(
        questions.on_answer(&instance(text, Change::Added, b"one more"), true),
        Verdict::Tell(_)
    ));
    questions.unsubscribe(text, 1, 2);
    assert!(matches!(
        questions.on_answer(&instance(browse, Change::Added, b"room again"), true),
        Verdict::Tell(_)
    ));
}

#[test]
fn a_late_request_is_replayed_what_is_held_and_nothing_else_until_it_ends() {
    let (mut questions, id) = asked();
    let _ = questions.on_answer(&instance(id, Change::Added, b"A"), true);
    assert_eq!(
        questions.subscribe(Form::Instance, ipp(), 2, 9, ACCOUNT),
        Ok(id)
    );
    let (question, token) = questions.next_replay().expect("owed a replay");
    assert_eq!(question, id);
    questions.replay_sent(token);
    assert_eq!(questions.next_replay(), None);
    // A live edge meanwhile is told only to the request already live.
    let verdict = questions.on_answer(&instance(id, Change::Added, b"B"), true);
    assert_eq!(told(&questions, &verdict), [(1, 1)]);
    assert_eq!(
        questions.on_held(token, &instance(id, Change::Added, b"A")),
        Verdict::TellOne(2, 9)
    );
    assert_eq!(
        questions.on_held(token, &instance(id, Change::Added, b"not held")),
        Verdict::Stale
    );
    assert_eq!(
        questions.on_held(token + 1, &instance(id, Change::Added, b"A")),
        Verdict::Stale
    );
    questions.replayed(token);
    let verdict = questions.on_answer(&instance(id, Change::Retired, b"A"), true);
    assert_eq!(told(&questions, &verdict), [(1, 1), (2, 9)]);
}

#[test]
fn a_replay_owed_to_a_request_that_left_is_never_asked_for() {
    let (mut questions, id) = asked();
    questions
        .subscribe(Form::Instance, ipp(), 2, 9, ACCOUNT)
        .unwrap();
    questions.unsubscribe(id, 2, 9);
    assert_eq!(questions.next_replay(), None);
}

#[test]
fn requests_joining_and_leaving_unserved_hold_the_replay_queue_to_its_bound() {
    let (mut questions, id) = asked();
    for request in 0..10_000 {
        questions
            .subscribe(Form::Instance, ipp(), 2, request, ACCOUNT)
            .unwrap();
        questions.unsubscribe(id, 2, request);
    }
    assert!(questions.replays.len() <= 2 * questions.tokens.len() + 64 + 1);
    assert_eq!(questions.next_replay(), None);
}

#[test]
fn a_flushed_link_names_each_request_that_held_something_there() {
    let (mut questions, id) = asked();
    let _ = questions.subscribe(Form::Instance, ipp(), 3, 3, ACCOUNT);
    let _ = questions.on_answer(&instance(id, Change::Added, b"A"), true);
    let flushed: Vec<(u32, u32)> = questions.flush_link(iface(b"eth0"));
    assert_eq!(flushed, [(1, 1), (3, 3)]);
    assert!(questions.flush_link(iface(b"eth0")).is_empty());
    assert_eq!(
        questions.on_answer(&instance(id, Change::Retired, b"A"), true),
        Verdict::Stale,
        "retiring what the flush already voided"
    );
}

#[test]
fn a_flush_names_a_request_holding_two_questions_there_once() {
    let mut questions = table();
    let service = questions
        .subscribe(Form::Service, ipp(), 1, 1, ACCOUNT)
        .unwrap();
    let text = questions
        .subscribe(Form::Text, ipp(), 1, 1, ACCOUNT)
        .unwrap();
    for id in [service, text] {
        ask(&mut questions, id);
        let _ = questions.on_answer(&instance(id, Change::Added, b"A"), true);
    }
    assert_eq!(questions.flush_link(iface(b"eth0")), [(1, 1)]);
}

#[test]
fn a_replaced_decoder_is_asked_everything_again_and_every_hold_is_flushed() {
    let (mut questions, id) = asked();
    let _ = questions.subscribe(Form::Instance, ipp(), 2, 2, ACCOUNT);
    let _ = questions.on_answer(&instance(id, Change::Added, b"A"), true);
    let flushed = questions.forget_decoder();
    assert_eq!(flushed, [(1, 1, iface(b"eth0")), (2, 2, iface(b"eth0"))]);
    assert_eq!(questions.next_ask(), Some((id, Form::Instance, ipp())));
    assert_eq!(
        questions.next_replay(),
        None,
        "a new decoder tells everyone"
    );
}

#[test]
fn a_replaced_decoder_frees_every_question_it_was_stopping() {
    let (mut questions, id) = asked();
    questions.unsubscribe(id, 1, 1);
    let flushed = questions.forget_decoder();
    assert!(flushed.is_empty());
    assert_eq!(questions.next_stop(), None);
    assert!(questions.is_empty());
    assert_eq!(
        questions.subscribe(Form::Instance, ipp(), 1, 1, ACCOUNT),
        Ok(id)
    );
}

#[test]
fn a_question_nobody_asks_any_more_is_stopped_only_if_it_was_asked() {
    let mut questions = table();
    let id = questions
        .subscribe(Form::Instance, ipp(), 1, 1, ACCOUNT)
        .unwrap();
    questions.unsubscribe(id, 1, 1);
    assert_eq!(questions.next_stop(), None, "never asked, nothing to stop");
    assert_eq!(questions.next_ask(), None, "nor anything to ask");
    assert!(questions.is_empty());
}

#[test]
fn no_more_questions_are_asked_than_an_engine_holds() {
    let mut questions = table();
    for n in 0..MAX_QUESTIONS {
        questions
            .subscribe(Form::AddressV4, host(n), 1, 1, ACCOUNT)
            .unwrap();
    }
    assert_eq!(
        questions.subscribe(
            Form::AddressV4,
            Name::encode("one.local").unwrap(),
            1,
            1,
            ACCOUNT
        ),
        Err(Errno::LimitExceeded)
    );
    // Joining a question already asked still fits.
    assert!(questions
        .subscribe(Form::AddressV4, host(0), 2, 2, ACCOUNT)
        .is_ok());
}

#[test]
fn a_question_being_stopped_still_counts_against_the_bound() {
    let mut questions = table();
    for n in 0..MAX_QUESTIONS {
        let id = questions
            .subscribe(Form::AddressV4, host(n), 1, 1, ACCOUNT)
            .unwrap();
        ask(&mut questions, id);
    }
    questions.unsubscribe(0, 1, 1);
    let fresh = Name::encode("fresh.local").unwrap();
    assert_eq!(
        questions.subscribe(Form::AddressV4, fresh, 1, 2, ACCOUNT),
        Err(Errno::LimitExceeded),
        "the decoder may still be asking it"
    );
    stop(&mut questions, 0);
    assert_eq!(
        questions.subscribe(Form::AddressV4, fresh, 1, 2, ACCOUNT),
        Ok(0)
    );
}

#[test]
fn a_question_left_stopping_stays_charged_until_the_decoder_says_it_stopped() {
    let mut questions = table();
    let id = questions.subscribe(Form::Instance, ipp(), 1, 1, 7).unwrap();
    questions.asked(id);
    assert_eq!(questions.charged(7), 1);
    questions.unsubscribe(id, 1, 1);
    assert_eq!(questions.charged(7), 1, "its slot is still taken");
    questions.stopped(id);
    assert!(questions.on_stopped(id));
    assert_eq!(questions.charged(7), 0);
}

#[test]
fn every_subscription_is_charged_to_its_own_account() {
    let mut questions = table();
    let id = questions.subscribe(Form::Instance, ipp(), 1, 1, 7).unwrap();
    assert_eq!(questions.subscribe(Form::Instance, ipp(), 2, 1, 8), Ok(id));
    assert_eq!((questions.charged(7), questions.charged(8)), (1, 1));
    questions.unsubscribe(id, 1, 1);
    assert_eq!((questions.charged(7), questions.charged(8)), (0, 1));
    questions.unsubscribe(id, 2, 1);
    assert_eq!(questions.charged(8), 0, "never asked, so released at once");
}

#[test]
fn a_replaced_decoder_releases_what_was_charged_for_stopping() {
    let mut questions = table();
    let id = questions.subscribe(Form::Instance, ipp(), 1, 1, 7).unwrap();
    questions.asked(id);
    questions.unsubscribe(id, 1, 1);
    let _ = questions.forget_decoder();
    assert_eq!(questions.charged(7), 0);
}

#[test]
fn leaving_a_crowded_question_keeps_every_other_subscription_in_its_place() {
    let mut questions = table();
    let id = questions
        .subscribe(Form::Instance, ipp(), 0, 0, ACCOUNT)
        .unwrap();
    for request in 1..64 {
        assert_eq!(
            questions.subscribe(Form::Instance, ipp(), 0, request, ACCOUNT),
            Ok(id)
        );
    }
    for request in (0..64).step_by(3) {
        questions.unsubscribe(id, 0, request);
    }
    let mut told: Vec<(u32, u32)> = questions.told(id).collect();
    told.sort_unstable();
    let kept: Vec<(u32, u32)> = (0..64)
        .filter(|request| request % 3 != 0)
        .map(|request| (0, request))
        .collect();
    assert_eq!(told, kept);
    for &(session, request) in &kept {
        questions.unsubscribe(id, session, request);
    }
    assert!(questions.is_empty(), "the last to leave retires it");
    assert_eq!(questions.charged(ACCOUNT), 0);
}
