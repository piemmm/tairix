//! Unit tests for the multicast DNS responder and querier.

use super::*;
use crate::addr::{Ipv4Addr, Ipv6Addr};
use crate::mdns::{Service, TxtRecord, MAX_RECORDS_PER_KEY, TTL_HOST_SECS};
use alloc::string::String;
use alloc::vec::Vec;

/// A buffer comfortably larger than anything these tests build.
const BUF: usize = 4096;

/// The id the tests' one browse is asked under.
const BROWSE: QuestionId = QuestionId::new(1);

fn at(millis: u64) -> Duration64 {
    Duration64::from_nanos(millis * 1_000_000)
}

fn name(dotted: &str) -> Name {
    Name::encode(dotted).expect("a test name encodes")
}

fn instance() -> Name {
    Name::from_labels(&[b"Hall Printer", b"_ipp", b"_tcp", b"local"]).expect("labels encode")
}

fn peer(last: u8) -> IpAddr {
    IpAddr::V4(Ipv4Addr::new(192, 168, 1, last))
}

/// [`peer`] speaking multicast DNS from 5353, found on-link by the stack.
fn on_link(last: u8) -> Sender {
    Sender {
        addr: peer(last),
        port: PORT,
        on_link: true,
        to_group: true,
    }
}

fn engine() -> MdnsEngine {
    MdnsEngine::new(HashSeed::UNKEYED)
}

/// A CSPRNG stand-in that always draws its lowest value, so every jittered
/// delay in a test is the low end of its window and the schedule is exact.
fn lowest() -> impl FnMut() -> u32 {
    || 0
}

/// A draw that always returns the top of a `u32`, exercising the high end.
fn highest() -> impl FnMut() -> u32 {
    || u32::MAX
}

fn address(last: u8) -> RData {
    RData::A(Ipv4Addr::new(10, 0, 0, last))
}

/// Drive `poll` once and decode whatever it emitted.
fn poll(engine: &mut MdnsEngine, now: Duration64) -> Option<(Destination, Vec<u8>)> {
    let mut buf = [0u8; BUF];
    let emit = engine.poll(now, &mut buf, &mut |_| {})?;
    Some((emit.to, buf[..emit.len].to_vec()))
}

/// Drive `poll` until it has nothing more to do at `now`.
fn drain(engine: &mut MdnsEngine, now: Duration64) -> Vec<(Destination, Vec<u8>)> {
    let mut out = Vec::new();
    while let Some(emitted) = poll(engine, now) {
        out.push(emitted);
        assert!(out.len() <= 32, "poll did not settle");
    }
    out
}

fn answers(bytes: &[u8]) -> Vec<Record> {
    Message::parse(bytes)
        .expect("the engine builds valid messages")
        .records()
        .filter(|(section, _)| *section == Section::Answer)
        .map(|(_, record)| record)
        .collect()
}

fn authority(bytes: &[u8]) -> Vec<Record> {
    Message::parse(bytes)
        .expect("the engine builds valid messages")
        .records()
        .filter(|(section, _)| *section == Section::Authority)
        .map(|(_, record)| record)
        .collect()
}

fn questions(bytes: &[u8]) -> Vec<Question> {
    Message::parse(bytes)
        .expect("the engine builds valid messages")
        .questions()
        .collect()
}

/// Build a datagram to feed the engine.
fn message(id: u16, response: bool, fill: impl FnOnce(&mut MessageWriter<'_>)) -> Vec<u8> {
    let mut buf = [0u8; BUF];
    let len = {
        let mut writer = MessageWriter::new(&mut buf, id, response).expect("a header fits");
        fill(&mut writer);
        writer.finish()
    };
    buf[..len].to_vec()
}

fn query(name: &Name, qtype: QuestionType, unicast: bool) -> Vec<u8> {
    message(0, false, |writer| {
        let mut question = Question::new(*name, qtype);
        question.unicast_response = unicast;
        assert!(writer.push_question(&question));
    })
}

/// Take a publication all the way from probing to live.
fn establish(engine: &mut MdnsEngine, id: PublishId) {
    establish_after(engine, id, 0);
}

/// [`establish`], driving the engine from `millis` on, returning the instant
/// it went live.
fn establish_after(engine: &mut MdnsEngine, id: PublishId, mut millis: u64) -> u64 {
    for _ in 0..64 {
        if engine.state_of(id) == Some(ServiceState::Live) {
            return millis;
        }
        drain(engine, at(millis));
        millis += 250;
    }
    panic!("the publication never went live");
}

/// A response asserting `address(9)` at `owner`: a rival's claim.
fn claim(owner: &Name) -> Vec<u8> {
    message(0, true, |writer| {
        assert!(writer.push_record(Section::Answer, &Record::unique(*owner, address(9))));
    })
}

// -- the probe, announce, live lifecycle ---------------------------------

#[test]
fn a_unique_name_is_probed_three_times_then_announced() {
    let mut engine = engine();
    let mut rng = lowest();
    let id = engine
        .publish(
            at(0),
            name("printer.local"),
            NameKind::Host,
            true,
            &[address(5)],
            &mut rng,
        )
        .expect("publishes");
    assert_eq!(engine.state_of(id), Some(ServiceState::Probing));

    let mut probes = 0usize;
    for step in 0..3 {
        let (to, bytes) = poll(&mut engine, at(step * 250)).expect("a probe is due");
        assert_eq!(to, Destination::Group);
        let asked = questions(&bytes);
        assert_eq!(asked.len(), 1);
        assert_eq!(asked[0].qtype, QuestionType::Any);
        assert!(asked[0].unicast_response, "the first probes ask unicast");
        // The proposed records travel in the authority section, claiming
        // nothing until the probe succeeds — and only what the caller
        // published, never the NSEC the engine derived.
        let proposed = authority(&bytes);
        assert_eq!(proposed.len(), 1);
        assert_eq!(proposed[0].data, address(5));
        assert!(!proposed[0].cache_flush);
        probes += 1;
    }
    assert_eq!(probes, 3);
    assert_eq!(engine.state_of(id), Some(ServiceState::Announcing));
    assert_eq!(engine.take_event(), Some(MdnsEvent::Established { id }));

    // Two announcements, the second a second later, and the records now
    // carry the cache-flush bit that claims the name.
    let (_, first) = poll(&mut engine, at(500)).expect("the first announcement");
    let announced = answers(&first);
    assert!(announced.iter().any(|record| record.data == address(5)));
    assert!(announced.iter().all(|record| record.cache_flush));
    assert_eq!(engine.state_of(id), Some(ServiceState::Announcing));
    assert!(poll(&mut engine, at(900)).is_none(), "not yet due");
    assert!(poll(&mut engine, at(1500)).is_some());
    assert_eq!(engine.state_of(id), Some(ServiceState::Live));
}

#[test]
fn a_shared_publication_is_announced_without_probing() {
    let mut engine = engine();
    let mut rng = lowest();
    let id = engine
        .publish(
            at(0),
            name("_ipp._tcp.local"),
            NameKind::Instance,
            false,
            &[RData::Ptr(instance())],
            &mut rng,
        )
        .expect("publishes");
    assert_eq!(engine.state_of(id), Some(ServiceState::Announcing));
    let (_, bytes) = poll(&mut engine, at(0)).expect("announces at once");
    let announced = answers(&bytes);
    assert_eq!(announced.len(), 1);
    assert!(!announced[0].cache_flush, "a shared record claims nothing");
    assert!(questions(&bytes).is_empty(), "no probe question");
}

#[test]
fn a_unique_publication_gains_an_nsec_asserting_what_it_does_not_hold() {
    let mut engine = engine();
    let mut rng = lowest();
    let id = engine
        .publish(
            at(0),
            name("printer.local"),
            NameKind::Host,
            true,
            &[address(5)],
            &mut rng,
        )
        .expect("publishes");
    establish(&mut engine, id);

    let mut rng = lowest();
    let mut buf = [0u8; BUF];
    let asked = query(&name("printer.local"), QuestionType::Any, true);
    let emit = engine
        .on_message(
            at(5000),
            &asked,
            on_link(9),
            &mut rng,
            &mut buf,
            &mut |_| {},
        )
        .expect("answers");
    let given = answers(&buf[..emit.len]);
    let nsec = given
        .iter()
        .find_map(|record| match record.data {
            RData::Nsec(bitmap) => Some(bitmap),
            _ => None,
        })
        .expect("an NSEC is published alongside");
    assert!(nsec.contains(RecordType::A));
    assert!(nsec.contains(RecordType::Nsec));
    assert!(!nsec.contains(RecordType::Srv), "we hold no SRV here");
}

#[test]
fn a_publication_past_the_bound_is_refused() {
    let mut engine = engine();
    let mut rng = lowest();
    let all: Vec<RData> = (0..u8::try_from(super::MAX_PUBLISHED).expect("small"))
        .map(address)
        .collect();
    assert_eq!(
        engine.publish(
            at(0),
            name("printer.local"),
            NameKind::Host,
            true,
            &all,
            &mut rng
        ),
        Err(PublishError::TooMany)
    );
    assert_eq!(
        engine.publish(at(0), name("x.local"), NameKind::Host, true, &[], &mut rng),
        Err(PublishError::NoRecords)
    );
}

// -- conflicts -----------------------------------------------------------

/// A claim on an established name has it probed again (RFC 6762 §9): the
/// claim may be a stale record. Only a claim that holds while it is probed
/// renames it.
#[test]
fn a_peer_claiming_our_live_name_has_it_probed_again_then_renamed() {
    let mut engine = engine();
    let mut rng = lowest();
    let id = engine
        .publish(
            at(0),
            name("printer.local"),
            NameKind::Host,
            true,
            &[address(5)],
            &mut rng,
        )
        .expect("publishes");
    establish(&mut engine, id);
    while engine.take_event().is_some() {}

    // A peer asserts a different address at our name.
    let theirs = message(0, true, |writer| {
        assert!(writer.push_record(
            Section::Answer,
            &Record::unique(name("printer.local"), address(9)),
        ));
    });
    let mut buf = [0u8; BUF];
    assert!(engine
        .on_message(
            at(5000),
            &theirs,
            on_link(9),
            &mut rng,
            &mut buf,
            &mut |_| {}
        )
        .is_none());

    assert_eq!(engine.take_event(), None, "not renamed yet");
    assert_eq!(engine.name_of(id), Some(name("printer.local")));
    assert_eq!(engine.state_of(id), Some(ServiceState::Probing));
    let (_, bytes) = poll(&mut engine, at(5000)).expect("probes the name again");
    assert_eq!(questions(&bytes)[0].name, name("printer.local"));

    engine.on_message(
        at(5100),
        &claim(&name("printer.local")),
        on_link(9),
        &mut rng,
        &mut buf,
        &mut |_| {},
    );
    assert_eq!(engine.take_event(), Some(MdnsEvent::Renamed { id }));
    assert_eq!(engine.name_of(id), Some(name("printer-2.local")));
    assert_eq!(engine.state_of(id), Some(ServiceState::Probing));
    let (_, bytes) = poll(&mut engine, at(5100)).expect("probes the new name");
    assert_eq!(questions(&bytes)[0].name, name("printer-2.local"));
}

/// A name being withdrawn is not fought for.
#[test]
fn a_claim_on_a_retiring_name_is_ignored() {
    let mut engine = engine();
    let mut rng = lowest();
    let id = live_host(&mut engine, "printer.local", &[address(5)]);
    while engine.take_event().is_some() {}
    engine.withdraw(at(5000), id);
    let mut buf = [0u8; BUF];
    engine.on_message(
        at(5001),
        &claim(&name("printer.local")),
        on_link(9),
        &mut rng,
        &mut buf,
        &mut |_| {},
    );
    assert_eq!(engine.take_event(), None);
    assert_eq!(engine.state_of(id), Some(ServiceState::Retiring));
}

/// Past fifteen conflicts in ten seconds, each probe a conflict forces waits
/// five seconds (RFC 6762 §8.1).
#[test]
fn a_storm_of_conflicts_spaces_the_probes_it_forces() {
    let mut engine = engine();
    let mut rng = lowest();
    let owners: Vec<String> = (0..16).map(|n| alloc::format!("host{n}.local")).collect();
    let ids: Vec<PublishId> = owners
        .iter()
        .map(|owner| live_host(&mut engine, owner, &[address(5)]))
        .collect();
    let mut buf = [0u8; BUF];
    for (n, owner) in owners.iter().take(15).enumerate() {
        let now = at(9000 + n as u64);
        engine.on_message(
            now,
            &claim(&name(owner)),
            on_link(9),
            &mut rng,
            &mut buf,
            &mut |_| {},
        );
        assert!(
            engine.next_deadline().is_some_and(|next| next <= now),
            "conflict {n} probes at once"
        );
    }
    let mut now = 9015;
    for &id in &ids[..15] {
        now = establish_after(&mut engine, id, now);
    }
    assert_eq!(engine.next_deadline(), None, "every name is live again");
    engine.on_message(
        at(now),
        &claim(&name(&owners[15])),
        on_link(9),
        &mut rng,
        &mut buf,
        &mut |_| {},
    );
    assert_eq!(
        engine.next_deadline(),
        Some(at(now + 5000)),
        "the sixteenth waits"
    );
}

#[test]
fn the_same_data_at_our_name_is_not_a_conflict() {
    let mut engine = engine();
    let mut rng = lowest();
    let id = engine
        .publish(
            at(0),
            name("printer.local"),
            NameKind::Host,
            true,
            &[address(5)],
            &mut rng,
        )
        .expect("publishes");
    establish(&mut engine, id);
    while engine.take_event().is_some() {}

    let echo = message(0, true, |writer| {
        assert!(writer.push_record(
            Section::Answer,
            &Record::unique(name("printer.local"), address(5)),
        ));
    });
    let mut buf = [0u8; BUF];
    engine.on_message(at(5000), &echo, on_link(9), &mut rng, &mut buf, &mut |_| {});
    assert_eq!(engine.take_event(), None);
    assert_eq!(engine.name_of(id), Some(name("printer.local")));
}

#[test]
fn the_rename_budget_runs_out_and_the_name_fails_closed_to_unpublished() {
    // The attack this bounds: a peer that keeps claiming whatever name we
    // move to walks a stock implementation through the integers forever.
    let mut engine = engine();
    let mut rng = lowest();
    let id = engine
        .publish(
            at(0),
            name("printer.local"),
            NameKind::Host,
            true,
            &[address(5)],
            &mut rng,
        )
        .expect("publishes");
    establish(&mut engine, id);
    while engine.take_event().is_some() {}

    let mut buf = [0u8; BUF];
    let mut exhausted = false;
    let mut now = 5000;
    for _ in 0..=MAX_RENAMES {
        // Claimed while live it is probed again; claimed again while probed,
        // it is renamed — or, once the budget is spent, given up.
        for _ in 0..2 {
            let claimed = engine.name_of(id).expect("still held");
            now += 10;
            engine.on_message(
                at(now),
                &claim(&claimed),
                on_link(9),
                &mut rng,
                &mut buf,
                &mut |_| {},
            );
        }
        while let Some(event) = engine.take_event() {
            if event == (MdnsEvent::ConflictBudgetExhausted { id }) {
                exhausted = true;
            }
        }
        if exhausted {
            break;
        }
        now = establish_after(&mut engine, id, now);
    }
    assert!(exhausted, "the budget is bounded, not endless");
    assert_eq!(engine.state_of(id), None, "withdrawn, and forgotten");

    // Nothing is published under it any more: a query gets no answer.
    let asked = query(&name("printer-9.local"), QuestionType::Any, true);
    assert!(engine
        .on_message(
            at(9000),
            &asked,
            on_link(9),
            &mut rng,
            &mut buf,
            &mut |_| {}
        )
        .is_none());
    assert_eq!(engine.next_deadline(), None);
}

#[test]
fn losing_a_simultaneous_probe_defers_rather_than_races() {
    let mut engine = engine();
    let mut rng = lowest();
    let id = engine
        .publish(
            at(0),
            name("printer.local"),
            NameKind::Host,
            true,
            &[address(5)],
            &mut rng,
        )
        .expect("publishes");
    assert_eq!(engine.state_of(id), Some(ServiceState::Probing));

    // A peer probing for the same name with lexicographically later rdata
    // wins, so we wait rather than both claiming it.
    let theirs = message(0, false, |writer| {
        assert!(writer.push_question(&Question::new(name("printer.local"), QuestionType::Any)));
        let mut proposed = Record::unique(name("printer.local"), address(200));
        proposed.cache_flush = false;
        assert!(writer.push_record(Section::Authority, &proposed));
    });
    let mut buf = [0u8; BUF];
    engine.on_message(at(10), &theirs, on_link(9), &mut rng, &mut buf, &mut |_| {});
    assert_eq!(engine.state_of(id), Some(ServiceState::Deferred));
    assert!(poll(&mut engine, at(500)).is_none(), "still waiting");
    assert_eq!(engine.state_of(id), Some(ServiceState::Deferred));
    drain(&mut engine, at(1100));
    assert!(matches!(
        engine.state_of(id),
        Some(ServiceState::Probing | ServiceState::Announcing)
    ));
}

#[test]
fn winning_a_simultaneous_probe_carries_on() {
    let mut engine = engine();
    let mut rng = lowest();
    let id = engine
        .publish(
            at(0),
            name("printer.local"),
            NameKind::Host,
            true,
            &[address(200)],
            &mut rng,
        )
        .expect("publishes");
    let theirs = message(0, false, |writer| {
        assert!(writer.push_question(&Question::new(name("printer.local"), QuestionType::Any)));
        let mut proposed = Record::unique(name("printer.local"), address(5));
        proposed.cache_flush = false;
        assert!(writer.push_record(Section::Authority, &proposed));
    });
    let mut buf = [0u8; BUF];
    engine.on_message(at(10), &theirs, on_link(9), &mut rng, &mut buf, &mut |_| {});
    assert_eq!(engine.state_of(id), Some(ServiceState::Probing));
}

// -- answering -----------------------------------------------------------

#[test]
fn a_multicast_query_is_answered_after_the_jitter_and_not_before() {
    let mut engine = engine();
    let mut rng = highest();
    let id = engine
        .publish(
            at(0),
            name("printer.local"),
            NameKind::Host,
            true,
            &[address(5)],
            &mut rng,
        )
        .expect("publishes");
    establish(&mut engine, id);

    let asked = query(
        &name("printer.local"),
        QuestionType::Record(RecordType::A),
        false,
    );
    let mut buf = [0u8; BUF];
    let mut rng = highest();
    assert!(
        engine
            .on_message(
                at(5000),
                &asked,
                on_link(9),
                &mut rng,
                &mut buf,
                &mut |_| {}
            )
            .is_none(),
        "a multicast answer is delayed, never immediate"
    );
    let due = engine.next_deadline().expect("a response is scheduled");
    assert!(due > at(5000) && due <= at(5120), "{due:?}");
    assert!(poll(&mut engine, at(5010)).is_none(), "not yet");
    let (to, bytes) = poll(&mut engine, due).expect("the delayed answer");
    assert_eq!(to, Destination::Group);
    assert!(answers(&bytes).iter().any(|r| r.data == address(5)));
}

#[test]
fn a_unicast_question_is_answered_straight_back_to_the_asker() {
    let mut engine = engine();
    let mut rng = lowest();
    let id = engine
        .publish(
            at(0),
            name("printer.local"),
            NameKind::Host,
            true,
            &[address(5)],
            &mut rng,
        )
        .expect("publishes");
    establish(&mut engine, id);

    let asked = query(
        &name("printer.local"),
        QuestionType::Record(RecordType::A),
        true,
    );
    let mut buf = [0u8; BUF];
    let emit = engine
        .on_message(
            at(5000),
            &asked,
            on_link(9),
            &mut rng,
            &mut buf,
            &mut |_| {},
        )
        .expect("answers at once");
    assert_eq!(
        emit.to,
        Destination::Peer {
            addr: peer(9),
            port: PORT
        }
    );
    let given = answers(&buf[..emit.len]);
    assert!(given.iter().any(|record| record.data == address(5)));
    assert!(given.iter().all(|record| record.cache_flush));
    assert!(questions(&buf[..emit.len]).is_empty(), "no question echoed");
}

#[test]
fn a_legacy_resolver_gets_its_question_echoed_and_a_short_ttl() {
    let mut engine = engine();
    let mut rng = lowest();
    let id = engine
        .publish(
            at(0),
            name("printer.local"),
            NameKind::Host,
            true,
            &[address(5)],
            &mut rng,
        )
        .expect("publishes");
    establish(&mut engine, id);

    let asked = message(0x1234, false, |writer| {
        assert!(writer.push_question(&Question::new(
            name("printer.local"),
            QuestionType::Record(RecordType::A),
        )));
    });
    let mut buf = [0u8; BUF];
    let legacy = Sender {
        addr: peer(9),
        port: 41234,
        on_link: true,
        to_group: true,
    };
    let emit = engine
        .on_message(at(5000), &asked, legacy, &mut rng, &mut buf, &mut |_| {})
        .expect("answers a legacy resolver");
    assert_eq!(
        emit.to,
        Destination::Peer {
            addr: peer(9),
            port: 41234
        }
    );
    let reply = &buf[..emit.len];
    let parsed = Message::parse(reply).expect("valid");
    assert_eq!(parsed.id, 0x1234, "a legacy reply echoes the query id");
    assert_eq!(questions(reply).len(), 1, "and echoes the question");
    let given = answers(reply);
    assert!(given.iter().all(|record| record.ttl <= 10));
    assert!(
        given.iter().all(|record| !record.cache_flush),
        "the flush bit means nothing to a resolver with no mDNS cache"
    );
}

#[test]
fn a_known_answer_the_asker_already_holds_is_not_repeated() {
    let mut engine = engine();
    let mut rng = lowest();
    let id = engine
        .publish(
            at(0),
            name("printer.local"),
            NameKind::Host,
            true,
            &[address(5)],
            &mut rng,
        )
        .expect("publishes");
    establish(&mut engine, id);

    let mut known = Record::unique(name("printer.local"), address(5));
    known.cache_flush = false;
    let asked = message(0, false, |writer| {
        let mut question =
            Question::new(name("printer.local"), QuestionType::Record(RecordType::A));
        question.unicast_response = true;
        assert!(writer.push_question(&question));
        assert!(writer.push_record(Section::Answer, &known));
    });
    let mut buf = [0u8; BUF];
    assert!(
        engine
            .on_message(
                at(5000),
                &asked,
                on_link(9),
                &mut rng,
                &mut buf,
                &mut |_| {}
            )
            .is_none(),
        "a fresh known answer suppresses ours"
    );

    // The same known answer with less than half its life left does not.
    let mut stale = known;
    stale.ttl = 10;
    let asked = message(0, false, |writer| {
        let mut question =
            Question::new(name("printer.local"), QuestionType::Record(RecordType::A));
        question.unicast_response = true;
        assert!(writer.push_question(&question));
        assert!(writer.push_record(Section::Answer, &stale));
    });
    assert!(engine
        .on_message(
            at(5100),
            &asked,
            on_link(9),
            &mut rng,
            &mut buf,
            &mut |_| {}
        )
        .is_some());
}

#[test]
fn a_query_for_something_we_do_not_publish_is_not_answered() {
    let mut engine = engine();
    let mut rng = lowest();
    let id = engine
        .publish(
            at(0),
            name("printer.local"),
            NameKind::Host,
            true,
            &[address(5)],
            &mut rng,
        )
        .expect("publishes");
    establish(&mut engine, id);
    let asked = query(&name("scanner.local"), QuestionType::Any, true);
    let mut buf = [0u8; BUF];
    assert!(engine
        .on_message(
            at(5000),
            &asked,
            on_link(9),
            &mut rng,
            &mut buf,
            &mut |_| {}
        )
        .is_none());
    assert_eq!(engine.next_deadline(), None);
}

#[test]
fn a_name_still_being_probed_for_is_not_answered_with() {
    let mut engine = engine();
    let mut rng = lowest();
    engine
        .publish(
            at(0),
            name("printer.local"),
            NameKind::Host,
            true,
            &[address(5)],
            &mut rng,
        )
        .expect("publishes");
    let asked = query(&name("printer.local"), QuestionType::Any, true);
    let mut buf = [0u8; BUF];
    assert!(
        engine
            .on_message(at(10), &asked, on_link(9), &mut rng, &mut buf, &mut |_| {})
            .is_none(),
        "the name is not ours until the probe succeeds"
    );
}

// -- off-link ------------------------------------------------------------

#[test]
fn a_query_from_off_link_is_never_answered() {
    let mut engine = engine();
    let mut rng = lowest();
    let id = engine
        .publish(
            at(0),
            name("printer.local"),
            NameKind::Host,
            true,
            &[address(5)],
            &mut rng,
        )
        .expect("publishes");
    establish(&mut engine, id);

    let asked = query(&name("printer.local"), QuestionType::Any, true);
    let mut buf = [0u8; BUF];
    let off_link = Sender {
        addr: IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7)),
        port: PORT,
        on_link: false,
        to_group: true,
    };
    assert!(engine
        .on_message(at(5000), &asked, off_link, &mut rng, &mut buf, &mut |_| {})
        .is_none());
    assert_eq!(engine.next_deadline(), None, "nothing was even scheduled");
}

/// A responder sends from 5353 (RFC 6762 §11): a response from any other
/// port never reaches the cache, and nothing answers a query from port 0.
#[test]
fn a_response_from_another_port_and_a_query_from_port_zero_are_ignored() {
    let mut engine = engine();
    let mut rng = lowest();
    let theirs = message(0, true, |writer| {
        assert!(writer.push_record(
            Section::Answer,
            &Record::unique(name("printer.local"), address(9)),
        ));
    });
    let mut buf = [0u8; BUF];
    for port in [0, 53, 41_234] {
        let from = Sender {
            addr: peer(9),
            port,
            on_link: true,
            to_group: true,
        };
        assert!(engine
            .on_message(at(10), &theirs, from, &mut rng, &mut buf, &mut |_| {})
            .is_none());
    }
    assert!(engine.cache().is_empty());
    assert!(engine
        .on_message(at(20), &theirs, on_link(9), &mut rng, &mut buf, &mut |_| {})
        .is_none());
    assert_eq!(engine.cache().len(), 1, "from 5353 it is held");

    let id = engine
        .publish(
            at(0),
            name("mine.local"),
            NameKind::Host,
            true,
            &[address(1)],
            &mut rng,
        )
        .expect("publishes");
    establish(&mut engine, id);
    let asked = query(&name("mine.local"), QuestionType::Any, true);
    let from_zero = Sender {
        addr: peer(7),
        port: 0,
        on_link: true,
        to_group: true,
    };
    assert!(engine
        .on_message(at(9000), &asked, from_zero, &mut rng, &mut buf, &mut |_| {})
        .is_none());
}

#[test]
fn a_response_from_off_link_never_reaches_the_cache() {
    let mut engine = engine();
    let mut rng = lowest();
    let theirs = message(0, true, |writer| {
        assert!(writer.push_record(
            Section::Answer,
            &Record::unique(name("elsewhere.local"), address(9)),
        ));
    });
    let mut buf = [0u8; BUF];
    let off_link = Sender {
        addr: IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1)),
        port: PORT,
        on_link: false,
        to_group: true,
    };
    engine.on_message(at(0), &theirs, off_link, &mut rng, &mut buf, &mut |_| {});
    assert!(engine.cache().is_empty());
}

// -- rate limiting -------------------------------------------------------

#[test]
fn a_record_is_multicast_at_most_once_a_second() {
    let mut engine = engine();
    let mut rng = highest();
    let id = engine
        .publish(
            at(0),
            name("_ipp._tcp.local"),
            NameKind::Instance,
            false,
            &[RData::Ptr(instance())],
            &mut rng,
        )
        .expect("publishes");
    establish(&mut engine, id);

    let asked = query(
        &name("_ipp._tcp.local"),
        QuestionType::Record(RecordType::Ptr),
        false,
    );
    let mut buf = [0u8; BUF];
    let mut rng = highest();
    engine.on_message(
        at(10_000),
        &asked,
        on_link(9),
        &mut rng,
        &mut buf,
        &mut |_| {},
    );
    let due = engine.next_deadline().expect("scheduled");
    assert!(
        poll(&mut engine, due).is_some(),
        "the first answer goes out"
    );

    // A second identical query inside the same second produces nothing.
    engine.on_message(
        at(10_200),
        &asked,
        on_link(9),
        &mut rng,
        &mut buf,
        &mut |_| {},
    );
    let due = engine.next_deadline().expect("scheduled again");
    assert!(
        poll(&mut engine, due).is_none(),
        "the record was multicast moments ago"
    );

    // A second later it may go again.
    engine.on_message(
        at(12_000),
        &asked,
        on_link(9),
        &mut rng,
        &mut buf,
        &mut |_| {},
    );
    let due = engine.next_deadline().expect("scheduled again");
    assert!(poll(&mut engine, due).is_some());
}

#[test]
fn unicast_replies_to_one_peer_are_budgeted() {
    let mut engine = engine();
    let mut rng = lowest();
    let id = engine
        .publish(
            at(0),
            name("printer.local"),
            NameKind::Host,
            true,
            &[address(5)],
            &mut rng,
        )
        .expect("publishes");
    establish(&mut engine, id);

    let asked = query(&name("printer.local"), QuestionType::Any, true);
    let mut buf = [0u8; BUF];
    let mut answered = 0usize;
    for _ in 0..40 {
        if engine
            .on_message(
                at(5000),
                &asked,
                on_link(9),
                &mut rng,
                &mut buf,
                &mut |_| {},
            )
            .is_some()
        {
            answered += 1;
        }
    }
    assert!(
        answered < 40,
        "a peer asking as fast as it likes is answered less, not more"
    );
    assert!(answered > 0, "and is still answered at all");
}

#[test]
fn a_peer_past_its_reply_budget_cannot_spend_the_interfaces() {
    // One flooding peer is refused by its own budget before the interface's
    // is charged, so a single asker cannot drain it and leave every other
    // peer on the link unanswered.
    let mut engine = engine();
    let mut rng = lowest();
    let id = engine
        .publish(
            at(0),
            name("printer.local"),
            NameKind::Host,
            true,
            &[address(5)],
            &mut rng,
        )
        .expect("publishes");
    establish(&mut engine, id);

    let asked = query(&name("printer.local"), QuestionType::Any, true);
    let mut buf = [0u8; BUF];
    for _ in 0..1_000 {
        let _ = engine.on_message(
            at(5000),
            &asked,
            on_link(9),
            &mut rng,
            &mut buf,
            &mut |_| {},
        );
    }
    assert!(
        engine
            .on_message(
                at(5000),
                &asked,
                on_link(10),
                &mut rng,
                &mut buf,
                &mut |_| {}
            )
            .is_some(),
        "a second peer on the link is still answered"
    );
}

#[test]
fn defending_our_own_name_is_never_charged_to_a_budget() {
    // The budget must not be a lever an attacker can pull to take a name:
    // a probe for a name we own is answered whatever the budget says.
    let mut engine = engine();
    let mut rng = lowest();
    let id = engine
        .publish(
            at(0),
            name("printer.local"),
            NameKind::Host,
            true,
            &[address(5)],
            &mut rng,
        )
        .expect("publishes");
    establish(&mut engine, id);

    let unicast = query(&name("printer.local"), QuestionType::Any, true);
    let mut buf = [0u8; BUF];
    for _ in 0..200 {
        engine.on_message(
            at(5000),
            &unicast,
            on_link(9),
            &mut rng,
            &mut buf,
            &mut |_| {},
        );
    }

    let probe = message(0, false, |writer| {
        let mut question = Question::new(name("printer.local"), QuestionType::Any);
        question.unicast_response = true;
        assert!(writer.push_question(&question));
        let mut proposed = Record::unique(name("printer.local"), address(9));
        proposed.cache_flush = false;
        assert!(writer.push_record(Section::Authority, &proposed));
    });
    let emit = engine
        .on_message(
            at(5000),
            &probe,
            on_link(9),
            &mut rng,
            &mut buf,
            &mut |_| {},
        )
        .expect("a probe for our name is always defended");
    assert_eq!(emit.to, Destination::Group, "and defended to everyone");
    assert!(answers(&buf[..emit.len])
        .iter()
        .any(|record| record.data == address(5)));
}

// -- withdrawal ----------------------------------------------------------

#[test]
fn withdrawing_sends_goodbyes_and_then_publishes_nothing() {
    let mut engine = engine();
    let mut rng = lowest();
    let id = engine
        .publish(
            at(0),
            name("printer.local"),
            NameKind::Host,
            true,
            &[address(5)],
            &mut rng,
        )
        .expect("publishes");
    establish(&mut engine, id);

    engine.withdraw(at(5000), id);
    assert_eq!(engine.state_of(id), Some(ServiceState::Retiring));
    let (to, bytes) = poll(&mut engine, at(5000)).expect("a goodbye");
    assert_eq!(to, Destination::Group);
    assert!(answers(&bytes).iter().all(|record| record.ttl == 0));
    let _ = poll(&mut engine, at(5250)).expect("the second goodbye");
    assert_eq!(engine.state_of(id), None, "withdrawn, and forgotten");

    let asked = query(&name("printer.local"), QuestionType::Any, true);
    let mut buf = [0u8; BUF];
    assert!(engine
        .on_message(
            at(6000),
            &asked,
            on_link(9),
            &mut rng,
            &mut buf,
            &mut |_| {}
        )
        .is_none());
}

#[test]
fn withdrawing_before_the_name_was_claimed_takes_nothing_back() {
    let mut engine = engine();
    let mut rng = lowest();
    let id = engine
        .publish(
            at(0),
            name("printer.local"),
            NameKind::Host,
            true,
            &[address(5)],
            &mut rng,
        )
        .expect("publishes");
    engine.withdraw(at(10), id);
    assert_eq!(engine.state_of(id), None, "withdrawn, and forgotten");
    assert!(poll(&mut engine, at(10)).is_none(), "nothing to take back");
}

#[test]
fn withdrawing_one_publication_leaves_the_others_answerable() {
    let mut engine = engine();
    let mut rng = lowest();
    let printer = engine
        .publish(
            at(0),
            name("printer.local"),
            NameKind::Host,
            true,
            &[address(5)],
            &mut rng,
        )
        .expect("publishes");
    let scanner = engine
        .publish(
            at(0),
            name("scanner.local"),
            NameKind::Host,
            true,
            &[address(6)],
            &mut rng,
        )
        .expect("publishes");
    establish(&mut engine, printer);
    establish(&mut engine, scanner);

    engine.withdraw(at(5000), printer);
    drain(&mut engine, at(5000));
    drain(&mut engine, at(5250));
    assert_eq!(engine.state_of(printer), None, "withdrawn, and forgotten");

    let asked = query(&name("scanner.local"), QuestionType::Any, true);
    let mut buf = [0u8; BUF];
    let emit = engine
        .on_message(
            at(6000),
            &asked,
            on_link(9),
            &mut rng,
            &mut buf,
            &mut |_| {},
        )
        .expect("the survivor still answers");
    assert!(answers(&buf[..emit.len])
        .iter()
        .any(|record| record.data == address(6)));
}

// -- asking --------------------------------------------------------------

#[test]
fn a_question_is_asked_then_re_asked_on_a_doubling_backoff() {
    let mut engine = engine();
    let mut rng = lowest();
    engine
        .ask(
            at(0),
            BROWSE,
            name("_ipp._tcp.local"),
            RecordType::Ptr,
            &mut rng,
            &mut |_| {},
        )
        .expect("asks");
    let (to, bytes) = poll(&mut engine, at(0)).expect("the first query");
    assert_eq!(to, Destination::Group);
    let asked = questions(&bytes);
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].name, name("_ipp._tcp.local"));

    assert!(poll(&mut engine, at(900)).is_none(), "at least one second");
    assert!(poll(&mut engine, at(1000)).is_some());
    assert!(poll(&mut engine, at(2500)).is_none(), "then two");
    assert!(poll(&mut engine, at(3000)).is_some());
}

#[test]
fn a_query_carries_what_we_already_know_so_responders_can_stay_quiet() {
    let mut engine = engine();
    let mut rng = lowest();
    engine
        .ask(
            at(0),
            BROWSE,
            name("_ipp._tcp.local"),
            RecordType::Ptr,
            &mut rng,
            &mut |_| {},
        )
        .expect("asks");
    let theirs = message(0, true, |writer| {
        assert!(writer.push_record(
            Section::Answer,
            &Record::shared(name("_ipp._tcp.local"), RData::Ptr(instance())),
        ));
    });
    let mut buf = [0u8; BUF];
    engine.on_message(at(0), &theirs, on_link(9), &mut rng, &mut buf, &mut |_| {});

    let (_, bytes) = poll(&mut engine, at(0)).expect("the first query");
    let known = answers(&bytes);
    assert_eq!(known.len(), 1);
    assert_eq!(known[0].data, RData::Ptr(instance()));
    assert!(
        !known[0].cache_flush,
        "a known answer asserts nothing about ownership"
    );
    assert!(known[0].ttl > 0 && known[0].ttl <= 4500);
}

#[test]
fn a_question_another_host_has_just_asked_slides_to_the_next_round() {
    let mut engine = engine();
    let mut rng = lowest();
    engine
        .ask(
            at(0),
            BROWSE,
            name("_ipp._tcp.local"),
            RecordType::Ptr,
            &mut rng,
            &mut |_| {},
        )
        .expect("asks");
    let theirs = query(
        &name("_ipp._tcp.local"),
        QuestionType::Record(RecordType::Ptr),
        false,
    );
    let mut buf = [0u8; BUF];
    engine.on_message(at(0), &theirs, on_link(9), &mut rng, &mut buf, &mut |_| {});
    assert!(
        poll(&mut engine, at(0)).is_none(),
        "someone else asked it for us"
    );
    assert!(poll(&mut engine, at(1000)).is_some());
}

/// Another querier's question stands for ours only if every responder heard
/// all of it as we did: a query cut short of its known answers, one sent to
/// this host alone, or a legacy resolver's leaves ours to be asked.
#[test]
fn only_a_whole_group_query_from_a_querier_stands_for_ours() {
    let ours = name("_ipp._tcp.local");
    let ptr = QuestionType::Record(RecordType::Ptr);
    // More questions than are read: its records — a known answer among
    // them — go unread, while responders read them all and stay quiet.
    let crowded: Vec<Name> = core::iter::once(ours)
        .chain((0..MAX_MESSAGE_QUESTIONS).map(|n| name(&alloc::format!("_s{n}._tcp.local"))))
        .collect();
    let crowded = crate::mdns::codec::raw_query(&crowded);
    let direct = Sender {
        to_group: false,
        ..on_link(9)
    };
    let legacy = Sender {
        port: 41_234,
        ..on_link(9)
    };
    for (bytes, from, stands) in [
        (query(&ours, ptr, false), on_link(9), true),
        (crowded, on_link(9), false),
        (query(&ours, ptr, false), direct, false),
        (query(&ours, ptr, false), legacy, false),
    ] {
        let mut engine = engine();
        let mut rng = lowest();
        engine
            .ask(at(0), BROWSE, ours, RecordType::Ptr, &mut rng, &mut |_| {})
            .expect("asks");
        let mut buf = [0u8; BUF];
        engine.on_message(at(0), &bytes, from, &mut rng, &mut buf, &mut |_| {});
        assert_eq!(poll(&mut engine, at(0)).is_none(), stands, "{from:?}");
    }
}

/// A query sent to this host alone is answered as a QU question is (RFC 6762
/// §5.5): straight back to its asker, the segment having heard the record
/// lately.
#[test]
fn a_query_sent_to_this_host_alone_is_answered_directly() {
    let mut engine = engine();
    live_host(&mut engine, "printer.local", &[address(5)]);
    let asked = query(
        &name("printer.local"),
        QuestionType::Record(RecordType::A),
        false,
    );
    let direct = Sender {
        to_group: false,
        ..on_link(9)
    };
    let mut rng = lowest();
    let mut buf = [0u8; BUF];
    let emit = engine
        .on_message(at(9000), &asked, direct, &mut rng, &mut buf, &mut |_| {})
        .expect("answered at once");
    assert_eq!(
        emit.to,
        Destination::Peer {
            addr: peer(9),
            port: PORT
        }
    );
}

/// A record not multicast within a quarter of its TTL is owed to the whole
/// segment, so a question wanting a unicast answer has it multicast instead
/// (RFC 6762 §5.4) — whether it set the QU bit or was sent to this host
/// alone — and the next such question, the segment now current, is answered
/// straight back.
#[test]
fn a_record_not_multicast_lately_answers_a_unicast_question_to_the_group() {
    let quarter = u64::from(TTL_HOST_SECS) * 1000 / 4;
    for direct in [false, true] {
        let mut engine = engine();
        live_host(&mut engine, "printer.local", &[address(5)]);
        let quiet = 10_000;
        assert!(
            drain(&mut engine, at(quiet)).is_empty(),
            "nothing left to send"
        );
        let asked = query(
            &name("printer.local"),
            QuestionType::Record(RecordType::A),
            !direct,
        );
        let from = Sender {
            to_group: !direct,
            ..on_link(9)
        };
        let mut rng = lowest();
        let mut buf = [0u8; BUF];
        let late = quiet + quarter;
        assert!(
            engine
                .on_message(at(late), &asked, from, &mut rng, &mut buf, &mut |_| {})
                .is_none(),
            "nothing straight back"
        );
        let due = engine.next_deadline().expect("a multicast answer");
        let (to, bytes) = poll(&mut engine, due).expect("the multicast answer");
        assert_eq!(to, Destination::Group);
        assert!(answers(&bytes)
            .iter()
            .any(|record| record.data == address(5)));

        let emit = engine
            .on_message(
                at(late + 1000),
                &asked,
                from,
                &mut rng,
                &mut buf,
                &mut |_| {},
            )
            .expect("answered at once");
        assert_eq!(
            emit.to,
            Destination::Peer {
                addr: peer(9),
                port: PORT
            },
            "direct: {direct}"
        );
    }
}

/// A legacy resolver hears nothing but its own unicast reply, so it is
/// answered straight back however long since the record was multicast.
#[test]
fn a_legacy_resolver_is_answered_straight_back_however_quiet_the_segment() {
    let mut engine = engine();
    live_host(&mut engine, "printer.local", &[address(5)]);
    let late = 10_000 + u64::from(TTL_HOST_SECS) * 1000;
    let asked = query(
        &name("printer.local"),
        QuestionType::Record(RecordType::A),
        false,
    );
    let legacy = Sender {
        port: 41_234,
        ..on_link(9)
    };
    let mut buf = [0u8; BUF];
    let emit = engine
        .on_message(
            at(late),
            &asked,
            legacy,
            &mut lowest(),
            &mut buf,
            &mut |_| {},
        )
        .expect("answered at once");
    assert_eq!(
        emit.to,
        Destination::Peer {
            addr: peer(9),
            port: 41_234
        }
    );
}

#[test]
fn a_cached_answer_about_to_expire_pulls_its_question_forward() {
    let mut engine = engine();
    let mut rng = lowest();
    engine
        .ask(
            at(0),
            BROWSE,
            name("_ipp._tcp.local"),
            RecordType::Ptr,
            &mut rng,
            &mut |_| {},
        )
        .expect("asks");
    let mut record = Record::shared(name("_ipp._tcp.local"), RData::Ptr(instance()));
    record.ttl = 100;
    let theirs = message(0, true, |writer| {
        assert!(writer.push_record(Section::Answer, &record));
    });
    let mut buf = [0u8; BUF];
    engine.on_message(at(0), &theirs, on_link(9), &mut rng, &mut buf, &mut |_| {});

    // Let the doubling backoff run out to its 127 s point.
    for millis in [0u64, 1_000, 3_000, 7_000, 15_000, 31_000, 63_000] {
        assert!(poll(&mut engine, at(millis)).is_some(), "{millis}");
    }
    // The record is 80 % spent at 80 s, spread by up to 2 % of its life, so
    // the refresh pulls the question forward from 127 s to the next whole
    // second after its point.
    let point = engine.next_deadline().expect("the refresh point");
    assert!(point >= at(80_000) && point < at(82_000), "{point:?}");
    assert!(
        poll(&mut engine, point).is_none(),
        "gathered to a whole second"
    );
    let second = at(point.saturating_total_nanos().div_ceil(1_000_000_000) * 1000);
    assert_eq!(engine.next_deadline(), Some(second));
    let (_, bytes) = poll(&mut engine, second).expect("the refreshing query");
    assert_eq!(questions(&bytes)[0].name, name("_ipp._tcp.local"));
}

/// However many of a question's records reach a refresh point together, and
/// however soon after it was asked, it is re-asked once, a second on at the
/// least.
#[test]
fn refresh_points_together_cost_one_query_a_second_apart() {
    let mut engine = engine();
    ask_browse(&mut engine, &mut Vec::new());
    learn_instances(&mut engine, 40, 100, at(0));
    for millis in [0u64, 1_000, 3_000, 7_000, 15_000, 31_000, 63_000] {
        drain(&mut engine, at(millis));
    }
    let mut queries = 0;
    let mut last: Option<u64> = None;
    for millis in (63_010..=100_000).step_by(10) {
        for (_, bytes) in drain(&mut engine, at(millis)) {
            if Message::parse(&bytes).is_some_and(|message| message.questions().count() > 0) {
                assert!(last.is_none_or(|last| millis - last >= 1000), "{millis}");
                assert_eq!(millis % 1000, 0, "on a whole second");
                last = Some(millis);
                queries += 1;
            }
        }
    }
    assert!(
        queries <= 8,
        "{queries} queries for forty records' four points"
    );
}

#[test]
fn dropping_a_question_stops_the_refreshing() {
    let mut engine = engine();
    let mut rng = lowest();
    engine
        .ask(
            at(0),
            BROWSE,
            name("_ipp._tcp.local"),
            RecordType::Ptr,
            &mut rng,
            &mut |_| {},
        )
        .expect("asks");
    let mut record = Record::shared(name("_ipp._tcp.local"), RData::Ptr(instance()));
    record.ttl = 100;
    let theirs = message(0, true, |writer| {
        assert!(writer.push_record(Section::Answer, &record));
    });
    let mut buf = [0u8; BUF];
    engine.on_message(at(0), &theirs, on_link(9), &mut rng, &mut buf, &mut |_| {});
    drain(&mut engine, at(0));

    engine.stop_asking(BROWSE);
    assert_eq!(
        engine.next_deadline(),
        Some(at(100_000)),
        "only the record's own expiry remains"
    );
    assert!(
        poll(&mut engine, at(99_999)).is_none(),
        "a stopped question is not asked"
    );
}

#[test]
fn questions_past_the_bound_are_refused() {
    let mut engine = engine();
    let mut rng = lowest();
    for index in 0..super::MAX_QUESTIONS {
        let owner = alloc::format!("q{index}.local");
        let id = QuestionId::new(u32::try_from(index).expect("small"));
        engine
            .ask(
                at(0),
                id,
                name(&owner),
                RecordType::A,
                &mut rng,
                &mut |_| {},
            )
            .expect("asks");
    }
    assert_eq!(
        engine.ask(
            at(0),
            QuestionId::new(u32::MAX),
            name("one-more.local"),
            RecordType::A,
            &mut rng,
            &mut |_| {}
        ),
        Err(PublishError::TooMany)
    );
}

#[test]
fn an_id_already_asked_is_refused_and_free_again_once_stopped() {
    let mut engine = engine();
    let mut rng = lowest();
    engine
        .ask(
            at(0),
            BROWSE,
            browse(),
            RecordType::Ptr,
            &mut rng,
            &mut |_| {},
        )
        .expect("asks");
    assert_eq!(
        engine.ask(
            at(0),
            BROWSE,
            name("printer.local"),
            RecordType::A,
            &mut rng,
            &mut |_| {}
        ),
        Err(PublishError::Duplicate)
    );
    engine.stop_asking(BROWSE);
    assert!(engine
        .ask(
            at(0),
            BROWSE,
            name("printer.local"),
            RecordType::A,
            &mut rng,
            &mut |_| {}
        )
        .is_ok());
}

#[test]
fn a_record_finds_its_question_among_thousands_by_one_comparison() {
    let mut engine = engine();
    let mut rng = lowest();
    for index in 0..super::MAX_QUESTIONS {
        let owner = alloc::format!("q{index}.local");
        let id = QuestionId::new(u32::try_from(index).expect("small"));
        engine
            .ask(
                at(0),
                id,
                name(&owner),
                RecordType::A,
                &mut rng,
                &mut |_| {},
            )
            .expect("asks");
    }
    engine.take_compared();
    let last = super::MAX_QUESTIONS - 1;
    let mut answer = Record::unique(name(&alloc::format!("q{last}.local")), address(5));
    answer.ttl = 120;
    let mut told = Told::new();
    hear(&mut engine, at(1), &announced(&[answer]), &mut told);
    assert_eq!(told.len(), 1);
    let expected = QuestionId::new(u32::try_from(last).expect("small"));
    assert_eq!(told[0].0, expected);
    assert_eq!(engine.take_compared(), 1);
}

#[test]
fn every_due_question_goes_out_however_many_queries_it_takes() {
    let mut engine = engine();
    let mut rng = lowest();
    let count = 3 * MAX_MESSAGE_QUESTIONS + 1;
    for index in 0..count {
        let owner = alloc::format!("q{index}.local");
        let id = QuestionId::new(u32::try_from(index).expect("small"));
        engine
            .ask(
                at(0),
                id,
                name(&owner),
                RecordType::A,
                &mut rng,
                &mut |_| {},
            )
            .expect("asks");
    }
    let mut asked = alloc::collections::BTreeSet::new();
    let mut queries = 0;
    while let Some((_, bytes)) = poll(&mut engine, at(0)) {
        let carried = questions(&bytes);
        assert!(
            carried.len() <= MAX_MESSAGE_QUESTIONS,
            "a peer reads all of it"
        );
        asked.extend(carried.into_iter().map(|question| question.name));
        queries += 1;
        assert!(queries <= count, "poll settles");
    }
    assert_eq!(asked.len(), count, "none is left behind");
    assert!(engine.next_deadline().is_some_and(|next| next > at(0)));
}

// -- the folded deadline -------------------------------------------------

#[test]
fn an_idle_engine_arms_nothing() {
    let engine = engine();
    assert_eq!(engine.next_deadline(), None);
}

#[test]
fn the_deadline_is_the_earliest_of_everything_pending() {
    let mut engine = engine();
    let mut rng = lowest();
    engine
        .publish(
            at(0),
            name("printer.local"),
            NameKind::Host,
            true,
            &[address(5)],
            &mut rng,
        )
        .expect("publishes");
    engine
        .ask(
            at(500),
            BROWSE,
            name("_ipp._tcp.local"),
            RecordType::Ptr,
            &mut rng,
            &mut |_| {},
        )
        .expect("asks");
    // The probe is due at zero and the question at 500 ms, so the earliest
    // of the two is what the caller arms.
    assert_eq!(engine.next_deadline(), Some(at(0)));
    drain(&mut engine, at(0));
    assert!(engine.next_deadline().expect("still pending") <= at(500));
}

#[test]
fn a_link_going_down_forgets_what_was_learned_there() {
    let mut engine = engine();
    let mut rng = lowest();
    let theirs = message(0, true, |writer| {
        assert!(writer.push_record(
            Section::Answer,
            &Record::shared(name("_ipp._tcp.local"), RData::Ptr(instance())),
        ));
    });
    let mut buf = [0u8; BUF];
    engine.on_message(at(0), &theirs, on_link(9), &mut rng, &mut buf, &mut |_| {});
    assert_eq!(engine.cache().len(), 1);
    engine.on_link_down(&mut |_| {});
    assert!(engine.cache().is_empty());
}

// -- suppression of our own pending answer -------------------------------

#[test]
fn another_responder_answering_first_drops_our_pending_answer() {
    let mut engine = engine();
    let mut rng = highest();
    let id = engine
        .publish(
            at(0),
            name("_ipp._tcp.local"),
            NameKind::Instance,
            false,
            &[RData::Ptr(instance())],
            &mut rng,
        )
        .expect("publishes");
    establish(&mut engine, id);

    let asked = query(
        &name("_ipp._tcp.local"),
        QuestionType::Record(RecordType::Ptr),
        false,
    );
    let mut buf = [0u8; BUF];
    let mut rng = highest();
    engine.on_message(
        at(10_000),
        &asked,
        on_link(9),
        &mut rng,
        &mut buf,
        &mut |_| {},
    );
    assert!(engine.next_deadline().is_some(), "an answer is scheduled");

    // Another host on the segment sends the same record before our jitter
    // elapses, so ours is no longer worth the airtime.
    let theirs = message(0, true, |writer| {
        assert!(writer.push_record(
            Section::Answer,
            &Record::shared(name("_ipp._tcp.local"), RData::Ptr(instance())),
        ));
    });
    engine.on_message(
        at(10_010),
        &theirs,
        on_link(8),
        &mut rng,
        &mut buf,
        &mut |_| {},
    );
    assert!(
        drain(&mut engine, at(10_200)).is_empty(),
        "the answer was already given"
    );
}

#[test]
fn a_service_instance_publishes_its_srv_and_txt_together() {
    let mut engine = engine();
    let mut rng = lowest();
    let id = engine
        .publish(
            at(0),
            instance(),
            NameKind::Instance,
            true,
            &[
                RData::Srv(Service {
                    priority: 0,
                    weight: 0,
                    port: 631,
                    target: name("printer.local"),
                }),
                RData::Txt(TxtRecord::new(b"\x07pdl=pdf").expect("well formed")),
            ],
            &mut rng,
        )
        .expect("publishes");
    establish(&mut engine, id);

    let asked = query(&instance(), QuestionType::Any, true);
    let mut buf = [0u8; BUF];
    let emit = engine
        .on_message(
            at(5000),
            &asked,
            on_link(9),
            &mut rng,
            &mut buf,
            &mut |_| {},
        )
        .expect("answers");
    let given = answers(&buf[..emit.len]);
    assert!(given
        .iter()
        .any(|record| record.record_type() == RecordType::Srv));
    assert!(given
        .iter()
        .any(|record| record.record_type() == RecordType::Txt));
}

// -- answers -------------------------------------------------------------

/// The edges a question was told, in order.
type Told = Vec<(QuestionId, AnswerChange, Record)>;

fn into(told: &mut Told) -> impl FnMut(&Answer<'_>) + '_ {
    move |answer| told.push((answer.question, answer.change, answer.record.record))
}

fn browse() -> Name {
    name("_ipp._tcp.local")
}

fn pointer(ttl: u32) -> Record {
    let mut record = Record::shared(browse(), RData::Ptr(instance()));
    record.ttl = ttl;
    record
}

fn announced(records: &[Record]) -> Vec<u8> {
    message(0, true, |writer| {
        for record in records {
            assert!(writer.push_record(Section::Answer, record));
        }
    })
}

fn hear(engine: &mut MdnsEngine, now: Duration64, bytes: &[u8], told: &mut Told) {
    let mut rng = lowest();
    let mut buf = [0u8; BUF];
    engine.on_message(now, bytes, on_link(9), &mut rng, &mut buf, &mut into(told));
}

fn tick(engine: &mut MdnsEngine, now: Duration64, told: &mut Told) {
    let mut buf = [0u8; BUF];
    while engine.poll(now, &mut buf, &mut into(told)).is_some() {}
}

fn ask_browse(engine: &mut MdnsEngine, told: &mut Told) -> QuestionId {
    let mut rng = lowest();
    engine
        .ask(
            at(0),
            BROWSE,
            browse(),
            RecordType::Ptr,
            &mut rng,
            &mut into(told),
        )
        .expect("asks");
    BROWSE
}

#[test]
fn a_question_is_told_each_answer_as_it_arrives_is_renewed_and_expires() {
    let mut engine = engine();
    let mut told = Told::new();
    let id = ask_browse(&mut engine, &mut told);
    assert!(told.is_empty(), "nothing is cached yet");

    hear(&mut engine, at(0), &announced(&[pointer(100)]), &mut told);
    hear(
        &mut engine,
        at(10_000),
        &announced(&[pointer(100)]),
        &mut told,
    );
    tick(&mut engine, at(110_000), &mut told);
    let changes: Vec<_> = told.iter().map(|(q, change, _)| (*q, *change)).collect();
    assert_eq!(
        changes,
        [
            (id, AnswerChange::Added),
            (id, AnswerChange::Refreshed),
            (id, AnswerChange::Retired),
        ]
    );
    assert!(told
        .iter()
        .all(|(_, _, record)| record.same_record(&pointer(100))));
}

#[test]
fn a_question_starts_with_what_the_cache_already_holds() {
    let mut engine = engine();
    let mut told = Told::new();
    hear(&mut engine, at(0), &announced(&[pointer(4500)]), &mut told);
    assert!(told.is_empty(), "no one asked yet");
    let id = ask_browse(&mut engine, &mut told);
    assert_eq!(told.len(), 1);
    assert_eq!((told[0].0, told[0].1), (id, AnswerChange::Added));
}

#[test]
fn a_goodbye_retires_its_answer_once_its_final_second_is_over() {
    let mut engine = engine();
    let mut told = Told::new();
    ask_browse(&mut engine, &mut told);
    hear(&mut engine, at(0), &announced(&[pointer(4500)]), &mut told);
    hear(&mut engine, at(5_000), &announced(&[pointer(0)]), &mut told);
    assert_eq!(told.len(), 1, "a goodbye's record is held one more second");
    tick(&mut engine, at(6_000), &mut told);
    assert_eq!(told.len(), 2);
    assert_eq!(told[1].1, AnswerChange::Retired);
}

#[test]
fn the_link_going_down_retires_every_answer_and_the_question_survives_it() {
    let mut engine = engine();
    let mut told = Told::new();
    let id = ask_browse(&mut engine, &mut told);
    hear(&mut engine, at(0), &announced(&[pointer(100)]), &mut told);
    engine.on_link_down(&mut into(&mut told));
    assert_eq!(told.len(), 2);
    assert_eq!((told[1].0, told[1].1), (id, AnswerChange::Retired));

    // Still asked, so what the link teaches next arms its refresh.
    hear(
        &mut engine,
        at(200_000),
        &announced(&[pointer(100)]),
        &mut told,
    );
    assert_eq!(told.len(), 3);
    assert!(engine.next_deadline().is_some_and(|at_| at_ <= at(280_000)));
}

#[test]
fn an_answer_the_bounds_evict_is_retired() {
    let mut engine = engine();
    let mut told = Told::new();
    ask_browse(&mut engine, &mut told);
    hear(&mut engine, at(0), &announced(&[pointer(4500)]), &mut told);
    // Past what one name holds, its own oldest record — the first answer —
    // makes room for the newest.
    for index in 0..crate::mdns::MAX_RECORDS_PER_KEY {
        let label = alloc::format!("i{index}");
        let target = Name::from_labels(&[label.as_bytes(), b"_ipp", b"_tcp", b"local"])
            .expect("an instance");
        let mut filler = Record::shared(browse(), RData::Ptr(target));
        filler.ttl = 4500;
        hear(
            &mut engine,
            at(u64::try_from(index + 1).expect("small")),
            &announced(&[filler]),
            &mut told,
        );
    }
    let retired: Vec<&Record> = told
        .iter()
        .filter(|(_, change, _)| *change == AnswerChange::Retired)
        .map(|(_, _, record)| record)
        .collect();
    assert_eq!(retired, [&pointer(4500)]);
}

#[test]
fn a_record_no_question_asks_for_is_never_told() {
    let mut engine = engine();
    let mut told = Told::new();
    ask_browse(&mut engine, &mut told);
    let mut other = Record::unique(name("printer.local"), address(5));
    other.ttl = 1;
    hear(&mut engine, at(0), &announced(&[other]), &mut told);
    tick(&mut engine, at(2_000), &mut told);
    assert!(told.is_empty());
}

#[test]
fn a_stopped_question_is_told_nothing_more() {
    let mut engine = engine();
    let mut told = Told::new();
    let id = ask_browse(&mut engine, &mut told);
    hear(&mut engine, at(0), &announced(&[pointer(100)]), &mut told);
    engine.stop_asking(id);
    tick(&mut engine, at(110_000), &mut told);
    assert_eq!(told.len(), 1, "only the edge from before it stopped");
}

#[test]
fn asking_a_name_and_type_already_asked_is_refused() {
    let mut engine = engine();
    let mut told = Told::new();
    ask_browse(&mut engine, &mut told);
    let mut rng = lowest();
    assert_eq!(
        engine.ask(
            at(0),
            QuestionId::new(2),
            browse(),
            RecordType::Ptr,
            &mut rng,
            &mut into(&mut told)
        ),
        Err(PublishError::Duplicate)
    );
    // The same name with another type is another question.
    assert!(engine
        .ask(
            at(0),
            QuestionId::new(2),
            browse(),
            RecordType::Txt,
            &mut rng,
            &mut into(&mut told)
        )
        .is_ok());
}

#[test]
fn questions_due_together_travel_in_one_query() {
    let mut engine = engine();
    let mut rng = lowest();
    for (id, owner) in [
        (1, "_ipp._tcp.local"),
        (2, "_http._tcp.local"),
        (3, "_smb._tcp.local"),
    ] {
        engine
            .ask(
                at(0),
                QuestionId::new(id),
                name(owner),
                RecordType::Ptr,
                &mut rng,
                &mut |_| {},
            )
            .expect("asks");
    }
    let (_, bytes) = poll(&mut engine, at(0)).expect("one query");
    assert_eq!(questions(&bytes).len(), 3);
    assert!(poll(&mut engine, at(0)).is_none(), "and only one");
}

#[test]
fn a_query_leaves_out_a_known_answer_past_half_its_life() {
    let mut engine = engine();
    let mut told = Told::new();
    ask_browse(&mut engine, &mut told);
    hear(&mut engine, at(0), &announced(&[pointer(100)]), &mut told);
    let (_, fresh) = poll(&mut engine, at(0)).expect("the first query");
    assert_eq!(answers(&fresh).len(), 1, "a fresh answer is known");
    for millis in [1_000u64, 3_000, 7_000, 15_000, 31_000] {
        drain(&mut engine, at(millis));
    }
    // The next round, at 63 s, finds the answer with 37 s of its 100 left.
    let (_, stale) = poll(&mut engine, at(63_000)).expect("a later query");
    assert!(answers(&stale).is_empty());
}

#[test]
fn a_poll_with_no_room_still_moves_every_schedule_on() {
    let mut engine = engine();
    let mut rng = lowest();
    engine
        .publish(
            at(0),
            name("printer.local"),
            NameKind::Host,
            true,
            &[address(5)],
            &mut rng,
        )
        .expect("publishes");
    engine
        .ask(
            at(0),
            BROWSE,
            browse(),
            RecordType::Ptr,
            &mut rng,
            &mut |_| {},
        )
        .expect("asks");
    for _ in 0..8 {
        assert!(engine.poll(at(0), &mut [], &mut |_| {}).is_none());
    }
    assert!(
        engine.next_deadline().is_some_and(|next| next > at(0)),
        "nothing is left due at an instant already reached"
    );
}

// -- known answers, continuations, and duplicate questions ---------------

fn browse_answer(instance: usize, ttl: u32) -> Record {
    let target = name(&alloc::format!("i{instance}._ipp._tcp.local"));
    let mut record = Record::shared(browse(), RData::Ptr(target));
    record.ttl = ttl;
    record
}

/// Answer the browse with `count` instances from one peer at `now`.
fn learn_instances(engine: &mut MdnsEngine, count: usize, ttl: u32, now: Duration64) {
    let mut rng = lowest();
    let mut buf = [0u8; BUF];
    let all: Vec<usize> = (0..count).collect();
    for batch in all.chunks(100) {
        let theirs = message(0, true, |writer| {
            for &instance in batch {
                assert!(writer.push_record(Section::Answer, &browse_answer(instance, ttl)));
            }
        });
        engine.on_message(now, &theirs, on_link(9), &mut rng, &mut buf, &mut |_| {});
    }
}

#[test]
fn a_query_walks_no_known_answer_past_the_first_stale_one() {
    let mut engine = engine();
    ask_browse(&mut engine, &mut Vec::new());
    learn_instances(&mut engine, 200, 100, at(0));
    let _ = engine.cache.take_looked();
    let mut buf = [0u8; BUF];
    let (len, carried) = engine
        .build_query(nanos(at(60_000)), &mut buf, &[0])
        .expect("the question fits");
    assert_eq!(carried, 1);
    assert!(
        answers(&buf[..len]).is_empty(),
        "nothing past half its lifetime is offered"
    );
    assert_eq!(
        engine.cache.take_looked(),
        1,
        "the walk ends at the first record past half its lifetime"
    );
}

#[test]
fn known_answers_too_many_for_one_query_follow_it_at_once() {
    let mut engine = engine();
    ask_browse(&mut engine, &mut Vec::new());
    learn_instances(&mut engine, 200, 4500, at(0));
    let sent = drain(&mut engine, at(0));
    assert_eq!(sent.len(), 2, "the query and one message of the rest");
    let first = Message::parse(&sent[0].1).expect("valid");
    let rest = Message::parse(&sent[1].1).expect("valid");
    assert!(first.truncated, "the query says more known answers follow");
    assert_eq!(first.questions().count(), 1);
    assert!(!rest.truncated, "the last message says none do");
    assert_eq!(rest.questions().count(), 0);
    let offered: usize = sent.iter().map(|(_, bytes)| answers(bytes).len()).sum();
    assert_eq!(offered, 200, "every known answer goes out once");
}

/// The rest of a known-answer list still goes within its grace of the query,
/// and is dropped after it, or once the record it resumes at is gone.
#[test]
fn a_known_answer_list_left_over_goes_within_its_grace_and_not_after() {
    for (later, cleared, goes) in [(50, false, true), (150, false, false), (10, true, false)] {
        let mut engine = engine();
        ask_browse(&mut engine, &mut Vec::new());
        learn_instances(&mut engine, 200, 4500, at(0));
        let (_, query) = poll(&mut engine, at(0)).expect("the query");
        assert!(Message::parse(&query).expect("valid").truncated);
        if cleared {
            engine.on_link_down(&mut |_| {});
        }
        assert_eq!(
            poll(&mut engine, at(later)).is_some(),
            goes,
            "{later} ms on, cache cleared: {cleared}"
        );
        if !goes {
            assert!(engine.next_deadline().is_none_or(|next| next > at(later)));
        }
    }
}

/// The grace runs from the query, whose responders hold their answer for the
/// whole list, not from each message of it.
#[test]
fn a_known_answer_list_over_several_messages_goes_within_one_grace() {
    let mut engine = engine();
    ask_browse(&mut engine, &mut Vec::new());
    learn_instances(&mut engine, MAX_RECORDS_PER_KEY, 4500, at(0));
    let mut datagram = [0u8; crate::mdns::MAX_MESSAGE_LEN];
    let mut truncated = |engine: &mut MdnsEngine, millis| {
        engine
            .poll(at(millis), &mut datagram, &mut |_| {})
            .map(|emit| {
                Message::parse(&datagram[..emit.len])
                    .expect("valid")
                    .truncated
            })
    };
    assert_eq!(truncated(&mut engine, 0), Some(true), "the query");
    assert_eq!(
        truncated(&mut engine, 60),
        Some(true),
        "within the grace, and more follow"
    );
    assert_eq!(
        truncated(&mut engine, 120),
        None,
        "the rest is dropped past the query's grace"
    );
}

#[test]
fn a_buffer_too_small_for_any_question_spends_every_due_round() {
    let mut engine = engine();
    let mut rng = lowest();
    for id in 1..=40 {
        let owner = name(&alloc::format!("h{id}.local"));
        engine
            .ask(
                at(0),
                QuestionId::new(id),
                owner,
                RecordType::A,
                &mut rng,
                &mut |_| {},
            )
            .expect("asks");
    }
    let mut tiny = [0u8; 16];
    assert!(engine.poll(at(0), &mut tiny, &mut |_| {}).is_none());
    assert!(
        engine.next_deadline().is_some_and(|next| next > at(0)),
        "no round is left in the past"
    );
}

/// Another host's QM query for the browse, carrying `known`.
fn their_browse(known: &[Record], truncated: bool, unicast: bool) -> Vec<u8> {
    message(0, false, |writer| {
        let mut question = Question::new(browse(), QuestionType::Record(RecordType::Ptr));
        question.unicast_response = unicast;
        assert!(writer.push_question(&question));
        for record in known {
            assert!(writer.push_record(Section::Answer, record));
        }
        if truncated {
            writer.set_truncated();
        }
    })
}

#[test]
fn a_question_asked_knowing_only_what_we_know_counts_as_ours() {
    let mut engine = engine();
    ask_browse(&mut engine, &mut Vec::new());
    learn_instances(&mut engine, 1, 4500, at(0));
    let mut rng = lowest();
    let mut buf = [0u8; BUF];
    let theirs = their_browse(&[browse_answer(0, 4500)], false, false);
    engine.on_message(at(0), &theirs, on_link(7), &mut rng, &mut buf, &mut |_| {});
    assert!(poll(&mut engine, at(0)).is_none(), "their query was ours");
}

#[test]
fn a_question_asked_knowing_more_than_we_do_is_still_asked() {
    let mut engine = engine();
    ask_browse(&mut engine, &mut Vec::new());
    let mut rng = lowest();
    let mut buf = [0u8; BUF];
    let theirs = their_browse(&[browse_answer(0, 4500)], false, false);
    engine.on_message(at(0), &theirs, on_link(7), &mut rng, &mut buf, &mut |_| {});
    assert!(
        poll(&mut engine, at(0)).is_some(),
        "responders stay quiet for what they know, which we do not"
    );
}

#[test]
fn a_unicast_or_truncated_question_never_counts_as_ours() {
    for (truncated, unicast) in [(true, false), (false, true)] {
        let mut engine = engine();
        ask_browse(&mut engine, &mut Vec::new());
        let mut rng = lowest();
        let mut buf = [0u8; BUF];
        let theirs = their_browse(&[], truncated, unicast);
        engine.on_message(at(0), &theirs, on_link(7), &mut rng, &mut buf, &mut |_| {});
        assert!(
            poll(&mut engine, at(0)).is_some(),
            "truncated {truncated}, unicast {unicast}"
        );
    }
}

#[test]
fn a_multicast_response_that_outgrows_a_datagram_goes_out_whole_and_untruncated() {
    let mut engine = engine();
    let mut rng = lowest();
    let addresses: Vec<RData> = (1..=6).map(address).collect();
    let id = engine
        .publish(
            at(0),
            name("printer.local"),
            NameKind::Host,
            true,
            &addresses,
            &mut rng,
        )
        .expect("publishes");
    establish(&mut engine, id);
    drain(&mut engine, at(30_000));
    let theirs = query(
        &name("printer.local"),
        QuestionType::Record(RecordType::A),
        false,
    );
    let mut buf = [0u8; BUF];
    assert!(engine
        .on_message(
            at(60_000),
            &theirs,
            on_link(9),
            &mut rng,
            &mut buf,
            &mut |_| {}
        )
        .is_none());
    let mut sent = Vec::new();
    let mut small = [0u8; 100];
    while let Some(emit) = engine.poll(at(61_000), &mut small, &mut |_| {}) {
        let bytes = &small[..emit.len];
        assert!(
            !Message::parse(bytes).expect("valid").truncated,
            "a multicast response never claims truncation"
        );
        sent.extend(answers(bytes));
        assert!(sent.len() <= 64, "poll settles");
    }
    assert!(
        addresses
            .iter()
            .all(|address| sent.iter().any(|record| record.data == *address)),
        "every address went out"
    );
}

/// A probe for `owner` proposing `address(9)`, as a rival claiming it sends.
fn probe_for(owner: &str) -> Vec<u8> {
    message(0, false, |writer| {
        let mut question = Question::new(name(owner), QuestionType::Any);
        question.unicast_response = true;
        assert!(writer.push_question(&question));
        let mut proposed = Record::unique(name(owner), address(9));
        proposed.cache_flush = false;
        assert!(writer.push_record(Section::Authority, &proposed));
    })
}

/// A host publication of `owner` at `addresses`, taken live.
fn live_host(engine: &mut MdnsEngine, owner: &str, addresses: &[RData]) -> PublishId {
    let id = engine
        .publish(
            at(0),
            name(owner),
            NameKind::Host,
            true,
            addresses,
            &mut lowest(),
        )
        .expect("publishes");
    establish(engine, id);
    id
}

/// A probe defence waits a quarter second since the record was last
/// multicast, where any other answer waits a second, and is deferred to that
/// instant rather than dropped (RFC 6762 §6).
#[test]
fn a_probe_defence_repeats_no_faster_than_a_quarter_second() {
    let mut engine = engine();
    live_host(&mut engine, "printer.local", &[address(5)]);
    let probe = probe_for("printer.local");
    let mut rng = lowest();
    let mut buf = [0u8; BUF];
    assert!(engine
        .on_message(
            at(5000),
            &probe,
            on_link(9),
            &mut rng,
            &mut buf,
            &mut |_| {}
        )
        .is_some());
    assert!(
        engine
            .on_message(
                at(5100),
                &probe,
                on_link(9),
                &mut rng,
                &mut buf,
                &mut |_| {}
            )
            .is_none(),
        "not again within a quarter second"
    );
    assert_eq!(
        engine.next_deadline(),
        Some(at(5250)),
        "deferred, not dropped"
    );
    let (to, bytes) = poll(&mut engine, at(5250)).expect("sent once it may");
    assert_eq!(to, Destination::Group);
    assert!(answers(&bytes)
        .iter()
        .any(|record| record.data == address(5)));
}

/// A defence's easing of the one-per-second rule is its own records', never
/// the rest of the answer pending beside them.
#[test]
fn a_defences_waiver_does_not_reach_the_rest_of_the_pending_answer() {
    let mut engine = engine();
    let printer = [address(5), address(6), address(7), address(8)];
    live_host(&mut engine, "printer.local", &printer);
    live_host(&mut engine, "scanner.local", &[address(4)]);
    let mut rng = lowest();
    let mut buf = [0u8; BUF];
    let asked = query(
        &name("scanner.local"),
        QuestionType::Record(RecordType::A),
        false,
    );
    engine.on_message(
        at(9000),
        &asked,
        on_link(9),
        &mut rng,
        &mut buf,
        &mut |_| {},
    );
    let first = engine.next_deadline().expect("an answer is due");
    poll(&mut engine, first).expect("the scanner is answered");
    // Past a defence's quarter second, inside everyone else's second.
    let later = Duration64::from_nanos(first.saturating_total_nanos() + 500_000_000);
    engine.on_message(later, &asked, on_link(9), &mut rng, &mut buf, &mut |_| {});
    // A datagram with room for one of the printer's records leaves the
    // defence's rest pending beside the scanner's second answer.
    let mut small = [0u8; 60];
    let defended = engine.on_message(
        later,
        &probe_for("printer.local"),
        on_link(9),
        &mut rng,
        &mut small,
        &mut |_| {},
    );
    assert!(defended.is_some());
    let due = engine.next_deadline().expect("the rest is due");
    let (_, bytes) = poll(&mut engine, due).expect("the defence goes on");
    let sent = answers(&bytes);
    assert!(sent
        .iter()
        .any(|record| record.name == name("printer.local")));
    assert!(
        !sent
            .iter()
            .any(|record| record.name == name("scanner.local")),
        "the scanner was multicast within the second"
    );
}

/// Another responder's copy suppresses our pending answer only when its
/// lifetime is no shorter than ours would be and the group heard it (RFC
/// 6762 §7.4): one sent to us alone reached nobody our answer is for.
#[test]
fn duplicate_answer_suppression_needs_a_lifetime_no_shorter_than_ours() {
    for (theirs, to_group, suppressed) in [
        (10, true, false),
        (TTL_HOST_SECS, true, true),
        (TTL_HOST_SECS, false, false),
    ] {
        let mut engine = engine();
        live_host(&mut engine, "printer.local", &[address(5)]);
        let mut rng = lowest();
        let mut buf = [0u8; BUF];
        let asked = query(
            &name("printer.local"),
            QuestionType::Record(RecordType::A),
            false,
        );
        engine.on_message(
            at(9000),
            &asked,
            on_link(9),
            &mut rng,
            &mut buf,
            &mut |_| {},
        );
        let due = engine.next_deadline().expect("an answer is due");
        let mut copy = Record::unique(name("printer.local"), address(5));
        copy.ttl = theirs;
        let response = message(0, true, |writer| {
            assert!(writer.push_record(Section::Answer, &copy));
        });
        let from = Sender {
            to_group,
            ..on_link(7)
        };
        engine.on_message(at(9001), &response, from, &mut rng, &mut buf, &mut |_| {});
        assert_eq!(
            poll(&mut engine, due).is_none(),
            suppressed,
            "ttl {theirs}, to the group: {to_group}"
        );
    }
}

/// However many known answers the cache holds for a question, one query
/// round sends at most the continuation budget's burst after it, and the last
/// message it sends says no more follow.
#[test]
fn a_known_answer_list_costs_no_more_datagrams_than_its_budget() {
    let mut engine = engine();
    ask_browse(&mut engine, &mut Vec::new());
    let mut rng = lowest();
    let mut buf = [0u8; BUF];
    for instance in 0..crate::mdns::MAX_RECORDS_PER_KEY {
        // Long, distinct labels, so the list takes many datagrams.
        let label = alloc::format!("{instance:0>60}");
        let target = Name::from_labels(&[label.as_bytes(), b"_ipp", b"_tcp", b"local"])
            .expect("an instance");
        let mut record = Record::shared(browse(), RData::Ptr(target));
        record.ttl = 4500;
        let theirs = announced(&[record]);
        engine.on_message(at(0), &theirs, on_link(9), &mut rng, &mut buf, &mut |_| {});
    }
    // Drained as the decoder does, a datagram of the link's size at a time.
    let round = |engine: &mut MdnsEngine, now| {
        let mut datagram = [0u8; crate::mdns::MAX_MESSAGE_LEN];
        let mut sent = Vec::new();
        while let Some(emit) = engine.poll(now, &mut datagram, &mut |_| {}) {
            sent.push(datagram[..emit.len].to_vec());
        }
        sent
    };
    let sent = round(&mut engine, at(0));
    assert_eq!(sent.len(), 1 + CONTINUATION_BURST as usize);
    let last = Message::parse(&sent[sent.len() - 1]).expect("valid");
    assert!(!last.truncated, "the last message promises nothing more");
    let next = round(&mut engine, at(1000));
    assert!(
        next.len() <= 1 + CONTINUATION_RATE as usize,
        "the next round pays at the rate: {}",
        next.len()
    );
}
