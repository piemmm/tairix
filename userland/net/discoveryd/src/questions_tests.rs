//! Unit tests for the front's question table.

use super::{Questions, Spend, Verdict, ACCOUNT_FLOOR, FLOOR_RESERVE};
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

/// An answer of `question`, a resolve's text, on `eth0`.
fn text_of(question: u32, change: Change, octets: &[u8]) -> Entry<'_> {
    Entry::Answer {
        request: question,
        interface: iface(b"eth0"),
        change,
        ttl: 120,
        answer: Answer::Text { octets },
    }
}

/// An added answer of the kind question `id` asks for: text when it is
/// `text`, else an instance.
fn answer(text: u32, id: u32, label: &[u8]) -> Entry<'_> {
    if id == text {
        text_of(id, Change::Added, label)
    } else {
        instance(id, Change::Added, label)
    }
}

/// The requests `verdict` names, each still taking answers.
fn told(questions: &mut Questions, verdict: Verdict) -> Vec<(u32, u32)> {
    let mut named = Vec::new();
    questions.tell(verdict, &mut |session, request| {
        named.push((session, request));
        true
    });
    named.sort_unstable();
    named
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
        .subscribe(Form::Instance, ipp(), 1, 1, ACCOUNT, &mut |_| true)
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
        .subscribe(Form::Instance, ipp(), 1, 1, ACCOUNT, &mut |_| true)
        .unwrap();
    let second = questions
        .subscribe(Form::Instance, ipp(), 2, 5, ACCOUNT, &mut |_| true)
        .unwrap();
    assert_eq!(first, second);
    let other = questions
        .subscribe(
            Form::Instance,
            Name::encode("_ssh._tcp.local").unwrap(),
            1,
            2,
            ACCOUNT,
            &mut |_| true,
        )
        .unwrap();
    assert_ne!(first, other);
    let text = questions
        .subscribe(Form::Text, ipp(), 1, 3, ACCOUNT, &mut |_| true)
        .unwrap();
    assert_ne!(first, text, "one name, another form, another question");
    // Nothing was asked of the decoder yet, so nobody is owed a replay.
    assert_eq!(questions.next_replay(), None);
}

#[test]
fn every_answer_moves_as_an_honest_engine_moves_it() {
    let (mut questions, id) = asked();
    let second = tairix_net::mdns::PER_RECORD_INTERVAL_NS;
    for (change, label, now) in [
        (Change::Added, b"A", 0),
        (Change::Refreshed, b"a", second),
        (Change::Retired, b"A", second),
    ] {
        let verdict = questions.on_answer(&instance(id, change, label), true, now);
        assert_eq!(told(&mut questions, verdict), [(1, 1)], "names fold case");
    }
    // What is not held cannot be renewed or retired, and what is held is
    // added once: each of those crossed a flush.
    for (change, label) in [(Change::Refreshed, b"A"), (Change::Retired, b"A")] {
        assert_eq!(
            questions.on_answer(&instance(id, change, label), true, 0),
            Verdict::Stale
        );
    }
    let verdict = questions.on_answer(&instance(id, Change::Added, b"B"), true, 0);
    assert_eq!(told(&mut questions, verdict), [(1, 1)]);
    assert_eq!(
        questions.on_answer(&instance(id, Change::Added, b"B"), true, 0),
        Verdict::Stale
    );
}

#[test]
fn an_answer_for_a_question_never_asked_is_a_lie_and_one_stopping_is_stale() {
    let (mut questions, id) = asked();
    assert_eq!(
        questions.on_answer(&instance(id + 1, Change::Added, b"A"), true, 0),
        Verdict::Lie
    );
    questions.unsubscribe(id, 1, 1);
    assert_eq!(questions.next_stop(), Some(id));
    questions.stopped(id);
    assert_eq!(questions.next_stop(), None);
    assert_eq!(
        questions.on_answer(&instance(id, Change::Added, b"A"), true, 0),
        Verdict::Stale,
        "it crossed the stop"
    );
    assert!(questions.on_stopped(id));
    assert_eq!(
        questions.on_answer(&instance(id, Change::Added, b"A"), true, 0),
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
            &mut |_| true,
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
            &mut |_| true,
        )
        .unwrap();
    assert_eq!(third, first);
}

#[test]
fn churning_questions_never_runs_the_ids_out() {
    let mut questions = table();
    for n in 0..4 * MAX_QUESTIONS {
        let id = questions
            .subscribe(Form::AddressV4, host(n), 1, 1, ACCOUNT, &mut |_| true)
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
        questions.on_answer(&instance(id, Change::Added, b"A"), false, 0),
        Verdict::Stale
    );
}

#[test]
fn a_link_holds_no_more_than_an_honest_engines_cache_across_every_question() {
    let (mut questions, browse) = asked();
    let text = questions
        .subscribe(Form::Text, ipp(), 1, 2, ACCOUNT, &mut |_| true)
        .unwrap();
    ask(&mut questions, text);
    for n in 0..MAX_RECORDS {
        let label = alloc::format!("i{n}");
        let id = if n % 2 == 0 { browse } else { text };
        assert!(matches!(
            questions.on_answer(&answer(text, id, label.as_bytes()), true, 0),
            Verdict::Tell(_)
        ));
    }
    for id in [browse, text] {
        assert_eq!(
            questions.on_answer(&answer(text, id, b"one more"), true, 0),
            Verdict::Lie
        );
    }
    assert!(
        matches!(
            questions.on_answer(&on(browse, b"wlan0", Change::Added, b"elsewhere"), true, 0),
            Verdict::Tell(_)
        ),
        "another link counts apart"
    );
    // Retiring one makes room for one, and retiring a question gives back
    // everything it held.
    let _ = questions.on_answer(&instance(browse, Change::Retired, b"i0"), true, 0);
    assert!(matches!(
        questions.on_answer(&text_of(text, Change::Added, b"one more"), true, 0),
        Verdict::Tell(_)
    ));
    questions.unsubscribe(text, 1, 2);
    assert!(matches!(
        questions.on_answer(&instance(browse, Change::Added, b"room again"), true, 0),
        Verdict::Tell(_)
    ));
}

#[test]
fn a_late_request_is_replayed_what_is_held_and_nothing_else_until_it_ends() {
    let (mut questions, id) = asked();
    let _ = questions.on_answer(&instance(id, Change::Added, b"A"), true, 0);
    assert_eq!(
        questions.subscribe(Form::Instance, ipp(), 2, 9, ACCOUNT, &mut |_| true),
        Ok(id)
    );
    let (question, token) = questions.next_replay().expect("owed a replay");
    assert_eq!(question, id);
    questions.replay_sent(id, token);
    assert_eq!(questions.next_replay(), None);
    // A live edge meanwhile is told only to the request already live.
    let verdict = questions.on_answer(&instance(id, Change::Added, b"B"), true, 0);
    assert_eq!(told(&mut questions, verdict), [(1, 1)]);
    let verdict = questions.on_held(token, &instance(id, Change::Added, b"A"));
    assert_eq!(told(&mut questions, verdict), [(2, 9)]);
    assert_eq!(
        questions.on_held(token, &instance(id, Change::Added, b"not held")),
        Verdict::Stale
    );
    assert_eq!(
        questions.on_held(token + 1, &instance(id, Change::Added, b"A")),
        Verdict::Stale
    );
    questions.replayed(id, token);
    let verdict = questions.on_answer(&instance(id, Change::Retired, b"A"), true, 0);
    assert_eq!(told(&mut questions, verdict), [(1, 1), (2, 9)]);
}

/// Requests joining while a replay is wanted share it, and those joining
/// while one is in flight share the next: one replay per question at a time,
/// however many join.
#[test]
fn requests_joining_together_share_one_replay() {
    let (mut questions, id) = asked();
    let _ = questions.on_answer(&instance(id, Change::Added, b"A"), true, 0);
    let mut charged = 0;
    let mut may = |spend| {
        assert_eq!(spend, Spend::Replay);
        charged += 1;
        true
    };
    for session in 2..6 {
        questions
            .subscribe(Form::Instance, ipp(), session, 1, ACCOUNT, &mut may)
            .unwrap();
    }
    assert_eq!(charged, 1, "one replay for all four");
    let (_, first) = questions.next_replay().expect("owed a replay");
    questions.replay_sent(id, first);
    for session in 6..8 {
        questions
            .subscribe(Form::Instance, ipp(), session, 1, ACCOUNT, &mut |_| true)
            .unwrap();
    }
    assert_eq!(questions.next_replay(), None, "not while one is in flight");
    let verdict = questions.on_held(first, &instance(id, Change::Added, b"A"));
    assert_eq!(
        told(&mut questions, verdict),
        [(2, 1), (3, 1), (4, 1), (5, 1)]
    );
    questions.replayed(id, first);
    let (_, second) = questions.next_replay().expect("the later joiners' turn");
    assert_ne!(second, first);
    questions.replay_sent(id, second);
    let verdict = questions.on_held(second, &instance(id, Change::Added, b"A"));
    assert_eq!(told(&mut questions, verdict), [(6, 1), (7, 1)]);
    assert_eq!(
        questions.on_held(first, &instance(id, Change::Added, b"A")),
        Verdict::Stale,
        "the first replay is over"
    );
    questions.assert_consistent();
}

/// A replay the budget refuses refuses the join, and a join sharing a replay
/// already wanted needs no budget.
#[test]
fn a_join_needing_a_fresh_replay_is_refused_when_the_budget_is_spent() {
    let (mut questions, id) = asked();
    let mut asks_only = |spend| spend == Spend::Ask;
    assert_eq!(
        questions.subscribe(Form::Instance, ipp(), 2, 1, ACCOUNT, &mut asks_only),
        Err(Errno::Busy)
    );
    assert_eq!(questions.charged(ACCOUNT), 1, "nothing was taken");
    assert_eq!(questions.next_replay(), None);
    questions
        .subscribe(Form::Instance, ipp(), 2, 1, ACCOUNT, &mut |_| true)
        .unwrap();
    assert_eq!(
        questions.subscribe(Form::Instance, ipp(), 3, 1, ACCOUNT, &mut asks_only),
        Ok(id),
        "it shares the replay wanted"
    );
    let unasked = questions
        .subscribe(Form::Text, ipp(), 2, 2, ACCOUNT, &mut asks_only)
        .expect("a question not yet asked needs no replay");
    assert_ne!(unasked, id);
    assert_eq!(
        questions.subscribe(Form::Text, ipp(), 3, 2, ACCOUNT, &mut |_| false),
        Ok(unasked),
        "nor does joining it"
    );
    questions.assert_consistent();
}

/// A fresh question the budget refuses is refused whole: no slot, charge or
/// ask is left behind.
#[test]
fn a_fresh_question_the_budget_refuses_takes_nothing() {
    let mut questions = table();
    let mut replays_only = |spend| spend == Spend::Replay;
    assert_eq!(
        questions.subscribe(Form::Instance, ipp(), 1, 1, ACCOUNT, &mut replays_only),
        Err(Errno::Busy)
    );
    assert!(questions.is_empty());
    assert_eq!(questions.charged(ACCOUNT), 0);
    assert_eq!(questions.next_ask(), None);
    questions.assert_idle();
    assert_eq!(
        questions.subscribe(Form::Instance, ipp(), 1, 1, ACCOUNT, &mut |_| true),
        Ok(0),
        "and the slot is still the first"
    );
}

/// A session may ask one question twice, so a replacement can start before
/// the request it replaces stops, and no more.
#[test]
fn a_session_asks_one_question_at_most_twice() {
    let (mut questions, id) = asked();
    assert_eq!(
        questions.subscribe(Form::Instance, ipp(), 1, 2, ACCOUNT, &mut |_| true),
        Ok(id)
    );
    assert_eq!(
        questions.subscribe(Form::Instance, ipp(), 1, 3, ACCOUNT, &mut |_| true),
        Err(Errno::LimitExceeded)
    );
    assert_eq!(
        questions.subscribe(Form::Instance, ipp(), 2, 3, ACCOUNT, &mut |_| true),
        Ok(id),
        "another session's count is its own"
    );
    questions.unsubscribe(id, 1, 1);
    assert_eq!(
        questions.subscribe(Form::Instance, ipp(), 1, 3, ACCOUNT, &mut |_| true),
        Ok(id),
        "room again once one stops"
    );
    questions.assert_consistent();
}

/// A request that takes nothing more is offered nothing more, while the rest
/// are told as before.
#[test]
fn a_request_that_takes_nothing_more_is_offered_nothing_more() {
    let (mut questions, id) = asked();
    for session in 2..5 {
        questions
            .subscribe(Form::Instance, ipp(), session, 1, ACCOUNT, &mut |_| true)
            .unwrap();
    }
    let (_, token) = questions.next_replay().expect("owed a replay");
    questions.replay_sent(id, token);
    questions.replayed(id, token);
    let mut offered = Vec::new();
    let verdict = questions.on_answer(&instance(id, Change::Added, b"A"), true, 0);
    questions.tell(verdict, &mut |session, request| {
        offered.push(session);
        session != 3 || request != 1
    });
    offered.sort_unstable();
    assert_eq!(offered, [1, 2, 3, 4]);
    let verdict = questions.on_answer(&instance(id, Change::Added, b"B"), true, 0);
    assert_eq!(told(&mut questions, verdict), [(1, 1), (2, 1), (4, 1)]);
    questions.unsubscribe(id, 3, 1);
    questions.assert_consistent();
}

/// A renewal within a second of the last one told is not told again: a copy
/// of a record cannot fan out once per copy.
#[test]
fn a_renewal_is_told_at_most_once_a_second() {
    let (mut questions, id) = asked();
    let second = tairix_net::mdns::PER_RECORD_INTERVAL_NS;
    let _ = questions.on_answer(&instance(id, Change::Added, b"A"), true, 0);
    for now in [1, second / 2, second - 1] {
        assert_eq!(
            questions.on_answer(&instance(id, Change::Refreshed, b"A"), true, now),
            Verdict::Absorbed
        );
    }
    assert!(matches!(
        questions.on_answer(&instance(id, Change::Refreshed, b"A"), true, second),
        Verdict::Tell(_)
    ));
    assert_eq!(
        questions.on_answer(&instance(id, Change::Refreshed, b"A"), true, second + 1),
        Verdict::Absorbed,
        "counted from the renewal told"
    );
    assert!(
        matches!(
            questions.on_answer(&instance(id, Change::Retired, b"A"), true, second + 2),
            Verdict::Tell(_)
        ),
        "a retirement is always told"
    );
}

/// Only a replay that was sent can end: one still wanted stays owed, so its
/// request is never left without what the question already held.
#[test]
fn a_replay_still_wanted_is_not_ended_by_an_early_end() {
    let (mut questions, id) = asked();
    questions
        .subscribe(Form::Instance, ipp(), 2, 9, ACCOUNT, &mut |_| true)
        .unwrap();
    let (_, token) = questions.next_replay().expect("owed a replay");
    questions.replayed(id, token);
    assert_eq!(questions.next_replay(), Some((id, token)), "still owed");
    let verdict = questions.on_answer(&instance(id, Change::Added, b"B"), true, 0);
    assert_eq!(told(&mut questions, verdict), [(1, 1)], "and not yet live");
}

#[test]
fn a_replay_owed_to_a_request_that_left_is_never_asked_for() {
    let (mut questions, id) = asked();
    questions
        .subscribe(Form::Instance, ipp(), 2, 9, ACCOUNT, &mut |_| true)
        .unwrap();
    questions.unsubscribe(id, 2, 9);
    assert_eq!(questions.next_replay(), None);
}

#[test]
fn requests_joining_and_leaving_unserved_leave_no_replay_owed() {
    let (mut questions, id) = asked();
    for request in 0..10_000 {
        questions
            .subscribe(Form::Instance, ipp(), 2, request, ACCOUNT, &mut |_| true)
            .unwrap();
        questions.unsubscribe(id, 2, request);
    }
    assert!(questions.replays.is_empty());
    assert_eq!(questions.next_replay(), None);
    questions.assert_consistent();
}

/// Every request `flush_link` names, sorted.
fn flushed(questions: &mut Questions, interface: [u8; IF_NAME_LEN]) -> Vec<(u32, u32)> {
    let mut named = Vec::new();
    questions.flush_link(interface, &mut |session, request| {
        named.push((session, request));
    });
    named.sort_unstable();
    named
}

/// Every request and link `forget_decoder` names, sorted.
fn forgotten(questions: &mut Questions) -> Vec<(u32, u32, [u8; IF_NAME_LEN])> {
    let mut named = Vec::new();
    questions.forget_decoder(&mut |session, request, interface| {
        named.push((session, request, interface));
    });
    named.sort_unstable();
    named
}

#[test]
fn a_flushed_link_names_each_request_that_held_something_there() {
    let (mut questions, id) = asked();
    let _ = questions.subscribe(Form::Instance, ipp(), 3, 3, ACCOUNT, &mut |_| true);
    let _ = questions.on_answer(&instance(id, Change::Added, b"A"), true, 0);
    assert_eq!(flushed(&mut questions, iface(b"eth0")), [(1, 1), (3, 3)]);
    assert!(flushed(&mut questions, iface(b"eth0")).is_empty());
    assert_eq!(
        questions.on_answer(&instance(id, Change::Retired, b"A"), true, 0),
        Verdict::Stale,
        "retiring what the flush already voided"
    );
}

/// A request asking two questions is named once for each; its session tells
/// it once a pass.
#[test]
fn a_flush_names_a_request_once_for_each_question_it_held_there() {
    let mut questions = table();
    let service = questions
        .subscribe(Form::Service, ipp(), 1, 1, ACCOUNT, &mut |_| true)
        .unwrap();
    let text = questions
        .subscribe(Form::Text, ipp(), 1, 1, ACCOUNT, &mut |_| true)
        .unwrap();
    let target = b"\x04host\x05local\x00";
    let answers = [
        Entry::Answer {
            request: service,
            interface: iface(b"eth0"),
            change: Change::Added,
            ttl: 120,
            answer: Answer::Service {
                priority: 0,
                weight: 0,
                port: 631,
                target,
            },
        },
        text_of(text, Change::Added, b"A"),
    ];
    for (id, answer) in [service, text].into_iter().zip(answers) {
        ask(&mut questions, id);
        assert!(matches!(
            questions.on_answer(&answer, true, 0),
            Verdict::Tell(_)
        ));
    }
    assert_eq!(flushed(&mut questions, iface(b"eth0")), [(1, 1), (1, 1)]);
}

#[test]
fn a_replaced_decoder_is_asked_everything_again_and_every_hold_is_flushed() {
    let (mut questions, id) = asked();
    let _ = questions.subscribe(Form::Instance, ipp(), 2, 2, ACCOUNT, &mut |_| true);
    let _ = questions.on_answer(&instance(id, Change::Added, b"A"), true, 0);
    assert_eq!(
        forgotten(&mut questions),
        [(1, 1, iface(b"eth0")), (2, 2, iface(b"eth0"))]
    );
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
    assert!(forgotten(&mut questions).is_empty());
    assert_eq!(questions.next_stop(), None);
    assert!(questions.is_empty());
    assert_eq!(
        questions.subscribe(Form::Instance, ipp(), 1, 1, ACCOUNT, &mut |_| true),
        Ok(id)
    );
}

#[test]
fn a_question_nobody_asks_any_more_is_stopped_only_if_it_was_asked() {
    let mut questions = table();
    let id = questions
        .subscribe(Form::Instance, ipp(), 1, 1, ACCOUNT, &mut |_| true)
        .unwrap();
    questions.unsubscribe(id, 1, 1);
    assert_eq!(questions.next_stop(), None, "never asked, nothing to stop");
    assert_eq!(questions.next_ask(), None, "nor anything to ask");
    assert!(questions.is_empty());
}

/// The `n`th of many accounts, none near its floor.
fn one_of_many(n: usize) -> u32 {
    10_000 + u32::try_from(n).unwrap()
}

/// Accounts at their floor can together fill no more than three quarters of
/// the table: the rest is kept for accounts below it, which share it.
#[test]
fn accounts_past_their_floor_leave_the_reserve_to_the_rest() {
    let mut questions = table();
    let heavy = [1, 2];
    let mut n = 0;
    while questions
        .subscribe(Form::AddressV4, host(n), 1, 1, heavy[n % 2], &mut |_| true)
        .is_ok()
    {
        n += 1;
    }
    assert_eq!(n, MAX_QUESTIONS - FLOOR_RESERVE);
    for account in heavy {
        assert_eq!(
            questions.subscribe(Form::AddressV4, host(n), 1, 1, account, &mut |_| true),
            Err(Errno::LimitExceeded)
        );
    }
    assert!(
        questions
            .subscribe(Form::AddressV4, host(0), 2, 1, heavy[0], &mut |_| true)
            .is_ok(),
        "joining a question asked takes no slot"
    );
    for light in 0..4 {
        for _ in 0..ACCOUNT_FLOOR {
            questions
                .subscribe(Form::AddressV4, host(n), 3, 1, 100 + light, &mut |_| true)
                .unwrap();
            n += 1;
        }
    }
    assert_eq!(n, MAX_QUESTIONS);
    assert_eq!(
        questions.subscribe(Form::AddressV4, host(n), 3, 1, 200, &mut |_| true),
        Err(Errno::LimitExceeded),
        "and then the table is full"
    );
    questions.assert_consistent();
}

#[test]
fn no_more_questions_are_asked_than_an_engine_holds() {
    let mut questions = table();
    for n in 0..MAX_QUESTIONS {
        questions
            .subscribe(Form::AddressV4, host(n), 1, 1, one_of_many(n), &mut |_| {
                true
            })
            .unwrap();
    }
    assert_eq!(
        questions.subscribe(
            Form::AddressV4,
            Name::encode("one.local").unwrap(),
            1,
            1,
            ACCOUNT,
            &mut |_| true
        ),
        Err(Errno::LimitExceeded)
    );
    // Joining a question already asked still fits.
    assert!(questions
        .subscribe(Form::AddressV4, host(0), 2, 2, ACCOUNT, &mut |_| true)
        .is_ok());
}

#[test]
fn a_question_being_stopped_still_counts_against_the_bound() {
    let mut questions = table();
    for n in 0..MAX_QUESTIONS {
        let id = questions
            .subscribe(Form::AddressV4, host(n), 1, 1, one_of_many(n), &mut |_| {
                true
            })
            .unwrap();
        ask(&mut questions, id);
    }
    questions.unsubscribe(0, 1, 1);
    let fresh = Name::encode("fresh.local").unwrap();
    assert_eq!(
        questions.subscribe(Form::AddressV4, fresh, 1, 2, ACCOUNT, &mut |_| true),
        Err(Errno::LimitExceeded),
        "the decoder may still be asking it"
    );
    stop(&mut questions, 0);
    assert_eq!(
        questions.subscribe(Form::AddressV4, fresh, 1, 2, ACCOUNT, &mut |_| true),
        Ok(0)
    );
}

#[test]
fn a_question_left_stopping_stays_charged_until_the_decoder_says_it_stopped() {
    let mut questions = table();
    let id = questions
        .subscribe(Form::Instance, ipp(), 1, 1, 7, &mut |_| true)
        .unwrap();
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
    let id = questions
        .subscribe(Form::Instance, ipp(), 1, 1, 7, &mut |_| true)
        .unwrap();
    assert_eq!(
        questions.subscribe(Form::Instance, ipp(), 2, 1, 8, &mut |_| true),
        Ok(id)
    );
    assert_eq!((questions.charged(7), questions.charged(8)), (1, 1));
    questions.unsubscribe(id, 1, 1);
    assert_eq!((questions.charged(7), questions.charged(8)), (0, 1));
    questions.unsubscribe(id, 2, 1);
    assert_eq!(questions.charged(8), 0, "never asked, so released at once");
}

#[test]
fn a_replaced_decoder_releases_what_was_charged_for_stopping() {
    let mut questions = table();
    let id = questions
        .subscribe(Form::Instance, ipp(), 1, 1, 7, &mut |_| true)
        .unwrap();
    questions.asked(id);
    questions.unsubscribe(id, 1, 1);
    assert!(forgotten(&mut questions).is_empty());
    assert_eq!(questions.charged(7), 0);
}

#[test]
fn leaving_a_crowded_question_keeps_every_other_subscription_in_its_place() {
    let mut questions = table();
    let id = questions
        .subscribe(Form::Instance, ipp(), 0, 0, ACCOUNT, &mut |_| true)
        .unwrap();
    for session in 1..64 {
        assert_eq!(
            questions.subscribe(Form::Instance, ipp(), session, 0, ACCOUNT, &mut |_| true),
            Ok(id)
        );
    }
    for session in (0..64).step_by(3) {
        questions.unsubscribe(id, session, 0);
    }
    let kept: Vec<(u32, u32)> = (0..64)
        .filter(|session| session % 3 != 0)
        .map(|session| (session, 0))
        .collect();
    assert_eq!(told(&mut questions, Verdict::Tell(id)), kept);
    questions.assert_consistent();
    for &(session, request) in &kept {
        questions.unsubscribe(id, session, request);
    }
    assert!(questions.is_empty(), "the last to leave retires it");
    assert_eq!(questions.charged(ACCOUNT), 0);
}

/// Random joins, leaves, replays, mutings and decoder losses keep every run
/// and index in step.
#[test]
fn runs_stay_in_step_through_random_churn() {
    let mut questions = table();
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut draw = |n: usize| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        usize::try_from(state % n as u64).unwrap()
    };
    let mut held: Vec<(u32, u32, u32)> = Vec::new();
    for _ in 0..4000 {
        let session = u32::try_from(draw(12)).unwrap();
        let request = u32::try_from(draw(4)).unwrap();
        let live = questions
            .slots
            .iter()
            .position(|slot| slot.live().is_some());
        let id = live.and_then(|at| u32::try_from(at).ok());
        match draw(8) {
            0..=2 => {
                let asked = questions.subscribe(
                    Form::Instance,
                    ipp(),
                    session,
                    request,
                    ACCOUNT,
                    &mut |_| true,
                );
                let duplicate = held.iter().any(|&(_, s, r)| (s, r) == (session, request));
                match asked {
                    Ok(question) => {
                        assert!(!duplicate);
                        held.push((question, session, request));
                    }
                    Err(Errno::AlreadyExists) => assert!(duplicate),
                    Err(err) => assert_eq!(err, Errno::LimitExceeded),
                }
            }
            3 if !held.is_empty() => {
                let (question, session, request) = held.swap_remove(draw(held.len()));
                questions.unsubscribe(question, session, request);
            }
            4 => {
                if let Some((question, token)) = questions.next_replay() {
                    questions.replay_sent(question, token);
                }
            }
            5 => {
                if let Some(id) = id {
                    let sent = questions.slots[id as usize].live().and_then(|q| q.sent);
                    if let Some(token) = sent {
                        questions.replayed(id, token);
                    }
                    if questions
                        .next_ask()
                        .is_some_and(|(asking, ..)| asking == id)
                    {
                        questions.asked(id);
                    }
                }
            }
            6 => {
                if let Some(id) = id {
                    let mute = u32::try_from(draw(4)).unwrap();
                    questions.tell(Verdict::Tell(id), &mut |_, request| request != mute);
                }
            }
            7 => questions.forget_decoder(&mut |_, _, _| {}),
            _ => {}
        }
        questions.assert_consistent();
    }
}

/// An honest engine answers a question only with the kind its form asks for,
/// so an answer of another kind, live or held, is a lie.
#[test]
fn an_answer_of_another_kind_than_its_question_asks_is_a_lie() {
    let (mut questions, browse) = asked();
    let text = questions
        .subscribe(Form::Text, ipp(), 1, 2, ACCOUNT, &mut |_| true)
        .unwrap();
    ask(&mut questions, text);
    let v4 = questions
        .subscribe(Form::AddressV4, host(1), 1, 3, ACCOUNT, &mut |_| true)
        .unwrap();
    ask(&mut questions, v4);
    let v6 = Entry::Answer {
        request: v4,
        interface: iface(b"eth0"),
        change: Change::Added,
        ttl: 120,
        answer: Answer::Address {
            address: tairix_net::IpAddr::V6(tairix_net::Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1)),
        },
    };
    for (lie, why) in [
        (text_of(browse, Change::Added, b"x"), "a browse told text"),
        (instance(text, Change::Added, b"x"), "text told an instance"),
        (v6, "an IPv4 lookup told an IPv6 address"),
    ] {
        assert_eq!(questions.on_answer(&lie, true, 0), Verdict::Lie, "{why}");
    }
    let _ = questions.on_answer(&instance(browse, Change::Added, b"A"), true, 0);
    assert_eq!(
        questions.subscribe(Form::Instance, ipp(), 2, 9, ACCOUNT, &mut |_| true),
        Ok(browse)
    );
    let (_, token) = questions.next_replay().expect("owed a replay");
    questions.replay_sent(browse, token);
    assert_eq!(
        questions.on_held(token, &text_of(browse, Change::Added, b"A")),
        Verdict::Lie,
        "a replay is held to its question's form too"
    );
}

/// A computer is one answer for as long as its key, whatever it is called or
/// wherever it answers, so its retirement matches it however it was renamed.
#[test]
fn a_computer_is_known_by_its_key() {
    let mut questions = table();
    let id = questions
        .subscribe(
            Form::Computer,
            Name::from_labels(&[]).unwrap(),
            1,
            1,
            ACCOUNT,
            &mut |_| true,
        )
        .unwrap();
    ask(&mut questions, id);
    let computer = |change, key, name| Entry::Answer {
        request: id,
        interface: iface(b"eth0"),
        change,
        ttl: 0,
        answer: Answer::Computer {
            key,
            address: tairix_net::IpAddr::V4(tairix_net::Ipv4Addr::new(192, 168, 1, 9)),
            name,
            membership: tairix_abi::discovery_ipc::Membership::Unstated,
        },
    };
    assert!(matches!(
        questions.on_answer(&computer(Change::Added, 7, "PC"), true, 0),
        Verdict::Tell(_)
    ));
    assert_eq!(
        questions.on_answer(&computer(Change::Added, 7, "STUDY"), true, 0),
        Verdict::Stale,
        "one key is one answer, however it is called"
    );
    assert!(matches!(
        questions.on_answer(&computer(Change::Retired, 7, "STUDY"), true, 0),
        Verdict::Tell(_)
    ));
    assert!(matches!(
        questions.on_answer(&computer(Change::Added, 7, "STUDY"), true, 0),
        Verdict::Tell(_)
    ));
}
