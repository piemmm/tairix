//! Unit tests for the bounded per-interface record cache.

use super::*;
use crate::addr::{Ipv4Addr, Ipv6Addr};
use crate::mdns::{RData, TxtRecord, MAX_RECORDS, MAX_RECORDS_PER_KEY, MAX_RECORDS_PER_SOURCE};
use alloc::vec::Vec;

/// The question every watched record in these tests answers.
const WATCHER: QuestionId = QuestionId::new(7);

fn cache() -> RecordCache {
    RecordCache::new(HashSeed::UNKEYED)
}

fn at(secs: i64) -> Duration64 {
    Duration64::from_secs(secs)
}

fn host(n: u8) -> IpAddr {
    IpAddr::V4(Ipv4Addr::new(192, 0, 2, n))
}

fn name(dotted: &str) -> Name {
    Name::encode(dotted).expect("a test name encodes")
}

fn a_record(dotted: &str, last: u8, ttl: u32) -> Record {
    let mut record = Record::unique(name(dotted), RData::A(Ipv4Addr::new(10, 0, 0, last)));
    record.ttl = ttl;
    record
}

fn ptr_record(owner: &str, target: &str, ttl: u32) -> Record {
    let mut record = Record::shared(name(owner), RData::Ptr(name(target)));
    record.ttl = ttl;
    record
}

/// Fold `record` in with no question watching it.
fn learn(cache: &mut RecordCache, now: Duration64, record: &Record, source: IpAddr) -> Learned {
    cache.learn(now, cache.keyed(record), source, None, &mut |_, _| {})
}

/// Fold `record` in on behalf of [`WATCHER`].
fn learn_watched(
    cache: &mut RecordCache,
    now: Duration64,
    record: &Record,
    source: IpAddr,
) -> Learned {
    cache.learn(
        now,
        cache.keyed(record),
        source,
        Some(WATCHER),
        &mut |_, _| {},
    )
}

fn matches<'c>(cache: &'c RecordCache, dotted: &str, record_type: RecordType) -> Matches<'c> {
    let owner = name(dotted);
    cache.lookup(cache.key_of(&owner, record_type), &owner, record_type)
}

fn held(cache: &RecordCache, dotted: &str, record_type: RecordType) -> Vec<Record> {
    matches(cache, dotted, record_type)
        .map(|cached| cached.record)
        .collect()
}

fn watch(
    cache: &mut RecordCache,
    dotted: &str,
    record_type: RecordType,
    watcher: Option<QuestionId>,
) {
    let owner = name(dotted);
    cache.set_watched(
        cache.key_of(&owner, record_type),
        &owner,
        record_type,
        watcher,
    );
}

fn advance(cache: &mut RecordCache, now: Duration64) {
    cache.advance(now, &mut |_| {}, &mut |_, _| {});
}

// -- learning and lookup -------------------------------------------------

#[test]
fn a_learned_record_is_found_by_name_and_type() {
    let mut cache = cache();
    let record = a_record("printer.local", 5, 120);
    assert_eq!(learn(&mut cache, at(0), &record, host(1)), Learned::Added);
    assert_eq!(
        held(&cache, "printer.local", RecordType::A),
        alloc::vec![record]
    );
    assert_eq!(cache.len(), 1);
    assert!(held(&cache, "printer.local", RecordType::Aaaa).is_empty());
    assert!(held(&cache, "other.local", RecordType::A).is_empty());
}

#[test]
fn lookup_is_case_insensitive_because_dns_names_are() {
    let mut cache = cache();
    learn(
        &mut cache,
        at(0),
        &a_record("Printer.LOCAL", 5, 120),
        host(1),
    );
    assert_eq!(held(&cache, "printer.local", RecordType::A).len(), 1);
}

#[test]
fn two_shared_records_at_one_name_both_live_there() {
    let mut cache = cache();
    let first = ptr_record("_ipp._tcp.local", "a._ipp._tcp.local", 4500);
    let second = ptr_record("_ipp._tcp.local", "b._ipp._tcp.local", 4500);
    learn(&mut cache, at(0), &first, host(1));
    learn(&mut cache, at(0), &second, host(2));
    assert_eq!(held(&cache, "_ipp._tcp.local", RecordType::Ptr).len(), 2);
}

#[test]
fn relearning_a_held_record_renews_it_rather_than_duplicating_it() {
    let mut cache = cache();
    let record = a_record("printer.local", 5, 120);
    learn(&mut cache, at(0), &record, host(1));
    assert_eq!(
        learn(&mut cache, at(30), &record, host(1)),
        Learned::Refreshed
    );
    assert_eq!(cache.len(), 1);
    let entry = matches(&cache, "printer.local", RecordType::A)
        .next()
        .expect("held");
    assert_eq!(entry.expires, at(150));
}

#[test]
fn a_record_spelled_in_another_case_is_the_record_already_held() {
    let mut cache = cache();
    let lower = ptr_record("_ipp._tcp.local", "hall._ipp._tcp.local", 4500);
    let upper = ptr_record("_IPP._tcp.local", "HALL._ipp._tcp.local", 4500);
    learn(&mut cache, at(0), &lower, host(1));
    assert_eq!(
        learn(&mut cache, at(1), &upper, host(1)),
        Learned::Refreshed
    );
    assert_eq!(cache.len(), 1);
}

// -- goodbyes and the cache-flush bit ------------------------------------

#[test]
fn a_goodbye_retires_the_record_after_the_one_second_grace() {
    let mut cache = cache();
    let record = a_record("printer.local", 5, 120);
    learn(&mut cache, at(0), &record, host(1));
    let mut goodbye = record;
    goodbye.ttl = 0;
    assert_eq!(
        learn(&mut cache, at(10), &goodbye, host(1)),
        Learned::Retired
    );
    // Still held during the grace RFC 6762 §10.1 asks for.
    assert_eq!(cache.len(), 1);
    advance(&mut cache, at(11));
    assert_eq!(cache.len(), 0);
}

#[test]
fn a_goodbye_from_another_host_cannot_delete_a_neighbours_record() {
    let mut cache = cache();
    let record = a_record("printer.local", 5, 120);
    learn(&mut cache, at(0), &record, host(1));
    let mut goodbye = record;
    goodbye.ttl = 0;
    assert_eq!(
        learn(&mut cache, at(10), &goodbye, host(2)),
        Learned::Ignored
    );
    advance(&mut cache, at(20));
    assert_eq!(cache.len(), 1);
}

/// The cache-flush bit retires every other record at the name and type,
/// whoever asserted it (RFC 6762 §10.2): a host that moves to a new address
/// announces from it, and its old record was another address's.
#[test]
fn the_cache_flush_bit_retires_every_other_record_at_the_name() {
    let mut cache = cache();
    let old = a_record("printer.local", 5, 120);
    let other_host = a_record("printer.local", 9, 120);
    learn(&mut cache, at(0), &old, host(1));
    learn(&mut cache, at(0), &other_host, host(2));

    let fresh = a_record("printer.local", 6, 120);
    learn(&mut cache, at(60), &fresh, host(3));
    advance(&mut cache, at(62));

    assert_eq!(held(&cache, "printer.local", RecordType::A), [fresh]);
}

/// A pointer at a service type's name is every instance's responder's, so a
/// cache-flush bit on one retires nothing.
#[test]
fn a_flush_bit_on_a_shared_type_pointer_retires_nothing() {
    let mut cache = cache();
    let first = ptr_record("_ipp._tcp.local", "a._ipp._tcp.local", 4500);
    learn(&mut cache, at(0), &first, host(1));
    let mut claim = ptr_record("_ipp._tcp.local", "b._ipp._tcp.local", 4500);
    claim.cache_flush = true;
    learn(&mut cache, at(60), &claim, host(2));
    advance(&mut cache, at(62));
    assert_eq!(held(&cache, "_ipp._tcp.local", RecordType::Ptr).len(), 2);
}

#[test]
fn the_cache_flush_bit_spares_a_record_received_in_the_last_second() {
    // A host announcing two addresses sends two messages; flushing on the
    // first would delete the second.
    let mut cache = cache();
    let first = a_record("printer.local", 5, 120);
    let second = a_record("printer.local", 6, 120);
    learn(&mut cache, at(0), &first, host(1));
    learn(&mut cache, at(0), &second, host(1));
    advance(&mut cache, at(2));
    assert_eq!(held(&cache, "printer.local", RecordType::A).len(), 2);
}

#[test]
fn the_cache_flush_bit_leaves_the_sources_records_at_other_names_alone() {
    let mut cache = cache();
    let elsewhere = a_record("scanner.local", 5, 120);
    learn(&mut cache, at(0), &elsewhere, host(1));
    learn(
        &mut cache,
        at(60),
        &a_record("printer.local", 6, 120),
        host(1),
    );
    advance(&mut cache, at(62));
    assert_eq!(
        held(&cache, "scanner.local", RecordType::A),
        alloc::vec![elsewhere]
    );
}

// -- expiry and the refresh schedule -------------------------------------

#[test]
fn a_record_expires_at_its_ttl() {
    let mut cache = cache();
    learn(
        &mut cache,
        at(0),
        &a_record("printer.local", 5, 100),
        host(1),
    );
    advance(&mut cache, at(99));
    assert_eq!(cache.len(), 1);
    advance(&mut cache, at(100));
    assert_eq!(cache.len(), 0);
}

#[test]
fn an_unwatched_record_arms_nothing_before_its_expiry() {
    let mut cache = cache();
    learn(
        &mut cache,
        at(0),
        &a_record("printer.local", 5, 100),
        host(1),
    );
    assert_eq!(cache.next_deadline(), Some(at(100)));
}

/// Whether `deadline` falls in the RFC 6762 §5.2 window for the refresh
/// point at `percent` of a 100-second lifetime: the point, spread by up to
/// two percent.
fn in_refresh_window(deadline: Option<Duration64>, percent: i64) -> bool {
    deadline.is_some_and(|at_| at_ >= at(percent) && at_ < at(percent + 2))
}

#[test]
fn a_watched_record_is_refreshed_at_eighty_five_ninety_and_ninety_five_percent() {
    let mut cache = cache();
    learn_watched(
        &mut cache,
        at(0),
        &a_record("printer.local", 5, 100),
        host(1),
    );
    assert!(in_refresh_window(cache.next_deadline(), 80));

    let mut points = Vec::new();
    for second in 0..=99 {
        cache.advance(
            at(second),
            &mut |watcher| {
                assert_eq!(watcher, WATCHER);
                points.push(second);
            },
            &mut |_, _| {},
        );
    }
    assert_eq!(points.len(), 4);
    for (&second, percent) in points.iter().zip([80, 85, 90, 95]) {
        assert!((percent..=percent + 2).contains(&second), "{points:?}");
    }
}

/// Each record's spread is its own, so records at one name refresh apart.
#[test]
fn records_at_one_name_spread_their_refreshes() {
    let points: Vec<Duration64> = (1..=16)
        .map(|last| {
            let mut alone = cache();
            let record = a_record("printer.local", last, 100);
            learn_watched(&mut alone, at(0), &record, host(1));
            alone.next_deadline().expect("armed")
        })
        .collect();
    assert!(points
        .iter()
        .all(|&point| in_refresh_window(Some(point), 80)));
    let mut distinct = points.clone();
    distinct.sort_unstable();
    distinct.dedup();
    assert!(distinct.len() > 8, "{points:?}");
}

#[test]
fn refresh_points_a_late_sweep_passes_are_reported_once() {
    let mut cache = cache();
    learn_watched(
        &mut cache,
        at(0),
        &a_record("printer.local", 5, 100),
        host(1),
    );
    let mut refreshes = 0usize;
    cache.advance(at(94), &mut |_| refreshes += 1, &mut |_, _| {});
    assert_eq!(refreshes, 1, "one re-query covers every point passed");
    assert!(in_refresh_window(cache.next_deadline(), 95));
}

#[test]
fn a_question_raised_after_the_record_arms_its_refresh() {
    let mut cache = cache();
    learn(
        &mut cache,
        at(0),
        &a_record("printer.local", 5, 100),
        host(1),
    );
    watch(&mut cache, "printer.local", RecordType::A, Some(WATCHER));
    assert!(in_refresh_window(cache.next_deadline(), 80));
}

#[test]
fn two_records_at_one_watched_name_each_keep_their_schedule() {
    let mut cache = cache();
    learn_watched(
        &mut cache,
        at(0),
        &a_record("printer.local", 5, 100),
        host(1),
    );
    learn_watched(
        &mut cache,
        at(0),
        &a_record("printer.local", 6, 100),
        host(1),
    );
    let mut refreshes = 0usize;
    for second in 0..=99 {
        cache.advance(at(second), &mut |_| refreshes += 1, &mut |_, _| {});
    }
    assert_eq!(refreshes, 8, "four points for each of two records");
}

#[test]
fn dropping_the_question_disarms_the_refresh() {
    let mut cache = cache();
    learn_watched(
        &mut cache,
        at(0),
        &a_record("printer.local", 5, 100),
        host(1),
    );
    watch(&mut cache, "printer.local", RecordType::A, None);
    let mut refreshed = false;
    for second in 0..=99 {
        cache.advance(at(second), &mut |_| refreshed = true, &mut |_, _| {});
    }
    assert!(!refreshed);
    assert_eq!(
        cache.next_deadline(),
        Some(at(100)),
        "the sweep made it exact"
    );
}

#[test]
fn a_question_that_stops_disarms_its_refresh_points_at_once() {
    let mut cache = cache();
    learn_watched(
        &mut cache,
        at(0),
        &a_record("printer.local", 5, 100),
        host(1),
    );
    assert!(
        in_refresh_window(cache.next_deadline(), 80),
        "the first refresh point"
    );
    watch(&mut cache, "printer.local", RecordType::A, None);
    assert_eq!(
        cache.next_deadline(),
        Some(at(100)),
        "only the expiry is left"
    );
}

#[test]
fn a_sweep_handles_only_what_is_due() {
    let mut cache = cache();
    for n in 0..MAX_RECORDS {
        let source = host(u8::try_from(n / MAX_RECORDS_PER_SOURCE + 1).unwrap());
        let ttl = if n == 7 { 10 } else { 100 };
        let owner = alloc::format!("host{n}.local");
        assert_eq!(
            learn(&mut cache, at(0), &a_record(&owner, 1, ttl), source),
            Learned::Added
        );
    }
    assert_eq!(cache.next_deadline(), Some(at(10)));
    let _ = cache.take_swept();
    advance(&mut cache, at(10));
    assert_eq!(cache.take_swept(), 1, "one record was due of {MAX_RECORDS}");
    assert_eq!(cache.len(), MAX_RECORDS - 1);
    assert_eq!(cache.next_deadline(), Some(at(100)));
}

// -- known-answer freshness ----------------------------------------------

#[test]
fn a_record_is_offered_as_a_known_answer_while_half_its_own_lifetime_is_left() {
    let mut cache = cache();
    let record = a_record("printer.local", 5, 100);
    learn(&mut cache, at(0), &record, host(1));
    assert!(
        cache.would_offer(at(0), cache.keyed(&record)),
        "full lifetime left"
    );
    let mut longer = record;
    longer.ttl = 4500;
    assert!(
        cache.would_offer(at(10), cache.keyed(&longer)),
        "judged by the lifetime held, not the one a peer quotes"
    );
    assert!(
        !cache.would_offer(at(60), cache.keyed(&record)),
        "less than half left is not offered"
    );
    let unheld = a_record("printer.local", 9, 100);
    assert!(
        !cache.would_offer(at(0), cache.keyed(&unheld)),
        "a record the cache does not hold is not offered"
    );
}

#[test]
fn a_lookup_comes_freshest_first_with_a_cut_lifetime_last() {
    let mut cache = cache();
    // Shared, so no arrival flushes another.
    let shared = |last| {
        let mut record = a_record("printer.local", last, 100);
        record.cache_flush = false;
        record
    };
    for last in 1..=3 {
        learn(&mut cache, at(i64::from(last)), &shared(last), host(last));
    }
    learn(&mut cache, at(5), &shared(1), host(1));
    let order = |cache: &RecordCache| -> Vec<u8> {
        held(cache, "printer.local", RecordType::A)
            .iter()
            .map(|record| match record.data {
                RData::A(addr) => addr.octets()[3],
                _ => 0,
            })
            .collect()
    };
    assert_eq!(
        order(&cache),
        [1, 3, 2],
        "renewed first, then newest arrival"
    );
    let mut goodbye = a_record("printer.local", 3, 0);
    goodbye.ttl = 0;
    learn(&mut cache, at(6), &goodbye, host(3));
    assert_eq!(order(&cache), [1, 2, 3], "a goodbye sends it last");
}

// -- the bounds ----------------------------------------------------------

#[test]
fn one_source_past_its_ceiling_evicts_its_own_oldest_record() {
    let mut cache = cache();
    let neighbour = a_record("neighbour.local", 1, 4500);
    learn(&mut cache, at(0), &neighbour, host(2));

    for index in 0..MAX_RECORDS_PER_SOURCE {
        let owner = alloc::format!("flood{index}.local");
        learn(&mut cache, at(1), &a_record(&owner, 5, 4500), host(1));
    }
    assert_eq!(cache.len_from(host(1)), MAX_RECORDS_PER_SOURCE);

    learn(
        &mut cache,
        at(2),
        &a_record("one-more.local", 5, 4500),
        host(1),
    );
    assert_eq!(
        cache.len_from(host(1)),
        MAX_RECORDS_PER_SOURCE,
        "a source cannot grow past its own ceiling"
    );
    assert!(
        held(&cache, "flood0.local", RecordType::A).is_empty(),
        "the flooder's own oldest record is what went"
    );
    assert_eq!(
        held(&cache, "neighbour.local", RecordType::A),
        alloc::vec![neighbour],
        "a neighbour's record is untouched by someone else's flood"
    );
}

#[test]
fn a_record_at_a_full_name_from_a_source_at_its_share_pays_both_bounds() {
    let mut cache = cache();
    for index in 0..MAX_RECORDS_PER_KEY {
        let target = alloc::format!("i{index}._ipp._tcp.local");
        let record = ptr_record("printer.local", &target, 4500);
        learn(&mut cache, at(0), &record, host(2));
    }
    for index in 0..MAX_RECORDS_PER_SOURCE {
        let owner = alloc::format!("flood{index}.local");
        learn(&mut cache, at(1), &a_record(&owner, 5, 4500), host(1));
    }
    let newest = ptr_record("printer.local", "newest._ipp._tcp.local", 4500);
    assert_eq!(learn(&mut cache, at(2), &newest, host(1)), Learned::Added);
    assert_eq!(
        held(&cache, "printer.local", RecordType::Ptr).len(),
        MAX_RECORDS_PER_KEY,
        "the name stays within its bound"
    );
    assert_eq!(
        cache.len_from(host(1)),
        MAX_RECORDS_PER_SOURCE,
        "and the source within its share"
    );
    assert!(held(&cache, "flood0.local", RecordType::A).is_empty());
    cache.assert_consistent();
}

#[test]
fn records_a_question_stops_watching_count_against_their_source() {
    let mut cache = cache();
    for index in 0..MAX_RECORDS_PER_SOURCE {
        let owner = alloc::format!("flood{index}.local");
        learn(&mut cache, at(0), &a_record(&owner, 5, 4500), host(1));
    }
    for index in 0..4 {
        let target = alloc::format!("i{index}._ipp._tcp.local");
        let record = ptr_record("_ipp._tcp.local", &target, 4500);
        learn_watched(&mut cache, at(1), &record, host(1));
    }
    assert_eq!(
        cache.len_from(host(1)),
        MAX_RECORDS_PER_SOURCE,
        "a watched record is no source's share"
    );
    watch(&mut cache, "_ipp._tcp.local", RecordType::Ptr, None);
    assert_eq!(
        cache.len_from(host(1)),
        MAX_RECORDS_PER_SOURCE,
        "the source stays within its share once nobody watches"
    );
    assert_eq!(held(&cache, "_ipp._tcp.local", RecordType::Ptr).len(), 4);
    for index in 0..4 {
        let owner = alloc::format!("flood{index}.local");
        assert!(
            held(&cache, &owner, RecordType::A).is_empty(),
            "the source's own oldest made room"
        );
    }
    cache.assert_consistent();
}

/// A record its question stops watching that cannot join its source's chain
/// leaves the cache whole, rather than half-moved between the lists.
#[test]
fn a_record_refused_its_source_chain_on_unwatching_leaves_cleanly() {
    let mut cache = cache();
    let kept = a_record("printer.local", 5, 4500);
    learn(&mut cache, at(0), &kept, host(1));
    let record = ptr_record("_ipp._tcp.local", "i0._ipp._tcp.local", 4500);
    learn_watched(&mut cache, at(0), &record, host(1));
    cache.refuse_sources = true;
    watch(&mut cache, "_ipp._tcp.local", RecordType::Ptr, None);
    assert!(
        held(&cache, "_ipp._tcp.local", RecordType::Ptr).is_empty(),
        "not kept uncounted"
    );
    assert_eq!(held(&cache, "printer.local", RecordType::A), [kept]);
    cache.assert_consistent();
}

/// A record another source would rescue, when that source's chain cannot be
/// made, stays its owner's and goes at its goodbye's second as it would have.
#[test]
fn a_rescue_refused_its_source_chain_changes_nothing() {
    let mut cache = cache();
    let record = a_record("printer.local", 5, 120);
    learn(&mut cache, at(0), &record, host(1));
    let goodbye = a_record("printer.local", 5, 0);
    learn(&mut cache, at(10), &goodbye, host(1));
    cache.refuse_sources = true;
    assert_eq!(
        learn(&mut cache, at(10), &record, host(2)),
        Learned::Ignored
    );
    cache.assert_consistent();
    advance(&mut cache, at(12));
    assert!(held(&cache, "printer.local", RecordType::A).is_empty());
    cache.assert_consistent();
}

#[test]
fn a_rescued_record_counts_against_its_rescuer() {
    let mut cache = cache();
    let record = a_record("printer.local", 5, 120);
    learn(&mut cache, at(0), &record, host(1));
    for index in 0..MAX_RECORDS_PER_SOURCE {
        let owner = alloc::format!("flood{index}.local");
        learn(&mut cache, at(1), &a_record(&owner, 5, 4500), host(2));
    }
    let goodbye = a_record("printer.local", 5, 0);
    assert_eq!(
        learn(&mut cache, at(10), &goodbye, host(1)),
        Learned::Retired
    );
    assert_eq!(
        learn(&mut cache, at(10), &record, host(2)),
        Learned::Refreshed
    );
    assert_eq!(cache.len_from(host(1)), 0);
    assert_eq!(
        cache.len_from(host(2)),
        MAX_RECORDS_PER_SOURCE,
        "the rescuer stays within its share"
    );
    assert!(held(&cache, "flood0.local", RecordType::A).is_empty());
    cache.assert_consistent();
}

#[test]
fn a_placement_that_evicts_the_last_record_of_its_own_chains_is_still_found() {
    // The source's oldest record is the only one at the name the new record
    // takes, so evicting it empties every chain the new record joins.
    let mut cache = cache();
    let oldest = a_record("printer.local", 5, 4500);
    learn(&mut cache, at(0), &oldest, host(1));
    for index in 1..MAX_RECORDS_PER_SOURCE {
        let owner = alloc::format!("filler{index}.local");
        learn(&mut cache, at(1), &a_record(&owner, 5, 4500), host(1));
    }
    let mut newest = a_record("printer.local", 6, 4500);
    newest.cache_flush = false;
    assert_eq!(learn(&mut cache, at(2), &newest, host(1)), Learned::Added);
    assert_eq!(
        held(&cache, "printer.local", RecordType::A),
        alloc::vec![newest]
    );
    assert_eq!(
        learn(&mut cache, at(3), &newest, host(1)),
        Learned::Refreshed,
        "it is found again, not placed twice"
    );
}

#[test]
fn a_full_cache_evicts_the_least_recently_used_record() {
    let mut cache = cache();
    let sources = MAX_RECORDS / MAX_RECORDS_PER_SOURCE;
    let mut second = 0i64;
    for source in 0..sources {
        for index in 0..MAX_RECORDS_PER_SOURCE {
            let owner = alloc::format!("h{source}-{index}.local");
            let peer = IpAddr::V6(Ipv6Addr::new(
                0xfe80,
                0,
                0,
                0,
                0,
                0,
                0,
                u16::try_from(source).expect("small"),
            ));
            learn(&mut cache, at(second), &a_record(&owner, 5, 4500), peer);
            second += 1;
        }
    }
    assert_eq!(cache.len(), MAX_RECORDS);

    learn(
        &mut cache,
        at(second),
        &a_record("newcomer.local", 5, 4500),
        host(9),
    );
    assert_eq!(cache.len(), MAX_RECORDS, "the ceiling holds");
    assert_eq!(held(&cache, "newcomer.local", RecordType::A).len(), 1);
    assert!(
        held(&cache, "h0-0.local", RecordType::A).is_empty(),
        "the oldest record is the one that went"
    );
}

/// The resident size the docs give a full table: under four mebibytes of
/// slots per interface, whatever the segment sends.
#[test]
fn a_full_slot_table_stays_under_four_mebibytes() {
    assert!(core::mem::size_of::<Option<Slot>>() * MAX_RECORDS < 4 << 20);
}

#[test]
fn a_record_past_the_txt_bound_never_reaches_the_cache() {
    // The bound is enforced where the octets are validated, so an
    // over-long TXT cannot be built into a record at all.
    let oversize = alloc::vec![0u8; super::super::MAX_TXT_LEN + 1];
    assert!(TxtRecord::new(&oversize).is_err());
}

#[test]
fn a_cleared_cache_holds_nothing_and_arms_nothing() {
    let mut cache = cache();
    learn(
        &mut cache,
        at(0),
        &a_record("printer.local", 5, 120),
        host(1),
    );
    cache.clear(&mut |_, _| {});
    assert!(cache.is_empty());
    assert_eq!(cache.next_deadline(), None);
    assert_eq!(cache.len_from(host(1)), 0);
}

#[test]
fn a_freed_slot_is_reused_rather_than_growing_the_table() {
    let mut cache = cache();
    learn(&mut cache, at(0), &a_record("first.local", 1, 10), host(1));
    advance(&mut cache, at(10));
    assert!(cache.is_empty());
    learn(
        &mut cache,
        at(11),
        &a_record("second.local", 2, 10),
        host(1),
    );
    assert_eq!(held(&cache, "second.local", RecordType::A).len(), 1);
    assert!(held(&cache, "first.local", RecordType::A).is_empty());
}

// -- what a peer can make a record cost ----------------------------------

/// As many distinct `PTR` records at one name as it may hold, from one
/// source.
fn stacked() -> RecordCache {
    let mut cache = cache();
    for index in 0..MAX_RECORDS_PER_KEY {
        let target = alloc::format!("i{index}._http._tcp.local");
        learn(
            &mut cache,
            at(0),
            &ptr_record("printer.local", &target, 4500),
            host(0),
        );
    }
    assert_eq!(cache.len(), MAX_RECORDS_PER_KEY);
    cache
}

#[test]
fn a_record_stacked_under_one_name_is_found_by_one_comparison() {
    let mut cache = stacked();
    cache.take_walked();
    let again = ptr_record("printer.local", "i5._http._tcp.local", 4500);
    assert_eq!(
        learn(&mut cache, at(1), &again, host(0)),
        Learned::Refreshed
    );
    assert_eq!(cache.take_walked(), 1);
    let new = ptr_record("printer.local", "new._http._tcp.local", 4500);
    learn(&mut cache, at(1), &new, host(0));
    assert_eq!(
        cache.take_walked(),
        0,
        "nothing to compare a new record with"
    );
}

#[test]
fn a_cache_flush_retires_each_record_once_and_walks_nothing_younger() {
    let mut cache = stacked();
    cache.take_walked();
    let mut claim = ptr_record("printer.local", "claim._http._tcp.local", 4500);
    claim.cache_flush = true;
    learn(&mut cache, at(2), &claim, host(3));
    assert_eq!(
        cache.take_walked(),
        MAX_RECORDS_PER_KEY,
        "every other record, each retired once"
    );
    let mut again = ptr_record("printer.local", "again._http._tcp.local", 4500);
    again.cache_flush = true;
    learn(&mut cache, at(2), &again, host(3));
    assert_eq!(
        cache.take_walked(),
        1,
        "what was retired is off the chain, and the walk stops at the claim just made"
    );
}

// -- telling a question its answers left ---------------------------------

/// Every record `gone` was told of, with the question it was told for.
fn gone_into(seen: &mut Vec<(QuestionId, Record)>) -> impl FnMut(QuestionId, &CachedRecord) + '_ {
    move |watcher, gone| seen.push((watcher, gone.record))
}

#[test]
fn a_watched_record_that_expires_is_reported_gone_to_its_question() {
    let mut cache = cache();
    let record = a_record("printer.local", 5, 100);
    learn_watched(&mut cache, at(0), &record, host(1));
    let mut seen = Vec::new();
    cache.advance(at(100), &mut |_| {}, &mut gone_into(&mut seen));
    assert_eq!(seen, alloc::vec![(WATCHER, record)]);
}

#[test]
fn a_record_no_question_watches_leaves_without_a_word() {
    let mut cache = cache();
    learn(
        &mut cache,
        at(0),
        &a_record("printer.local", 5, 100),
        host(1),
    );
    let mut seen = Vec::new();
    cache.advance(at(100), &mut |_| {}, &mut gone_into(&mut seen));
    assert!(cache.is_empty());
    assert!(seen.is_empty());
}

#[test]
fn a_watched_record_evicted_for_room_is_reported_gone() {
    let mut cache = cache();
    let first = ptr_record("printer.local", "i0._http._tcp.local", 4500);
    learn_watched(&mut cache, at(0), &first, host(1));
    let mut seen = Vec::new();
    for index in 1..=MAX_RECORDS_PER_KEY {
        let target = alloc::format!("i{index}._http._tcp.local");
        let record = ptr_record("printer.local", &target, 4500);
        cache.learn(
            at(i64::try_from(index).expect("small")),
            cache.keyed(&record),
            host(u8::try_from(index % 200).expect("small")),
            Some(WATCHER),
            &mut gone_into(&mut seen),
        );
    }
    // The name's own oldest record made room for its newest.
    assert_eq!(seen, alloc::vec![(WATCHER, first)]);
    assert_eq!(
        held(&cache, "printer.local", RecordType::Ptr).len(),
        MAX_RECORDS_PER_KEY
    );
}

/// However many unsolicited records a peer sends, from however many forged
/// addresses, a record a question watches is never what makes room for them.
#[test]
fn unsolicited_records_never_displace_a_watched_one() {
    let mut cache = cache();
    let watched = a_record("q.local", 5, 4500);
    learn_watched(&mut cache, at(0), &watched, host(1));
    let mut seen = Vec::new();
    for index in 0..MAX_RECORDS + 64 {
        let owner = alloc::format!("f{index}.local");
        let record = a_record(&owner, 5, 4500);
        cache.learn(
            at(1),
            cache.keyed(&record),
            host(u8::try_from(index % 32).expect("small")),
            None,
            &mut gone_into(&mut seen),
        );
    }
    assert!(seen.is_empty());
    assert_eq!(held(&cache, "q.local", RecordType::A), [watched]);
    assert_eq!(cache.len(), MAX_RECORDS);
}

/// Only a record's own source renews it on its say: another can extend its
/// life but never shorten it, so resending a neighbour's record with a TTL
/// of one deletes nothing.
#[test]
fn another_sources_copy_extends_a_record_and_never_shortens_it() {
    let mut cache = cache();
    let record = a_record("printer.local", 5, 120);
    learn(&mut cache, at(0), &record, host(1));
    let short = a_record("printer.local", 5, 1);
    assert_eq!(
        learn(&mut cache, at(10), &short, host(2)),
        Learned::Corroborated
    );
    advance(&mut cache, at(12));
    let cached = matches(&cache, "printer.local", RecordType::A)
        .next()
        .expect("held");
    assert_eq!((cached.source, cached.expires), (host(1), at(120)));
    assert_eq!(
        learn(&mut cache, at(60), &record, host(2)),
        Learned::Refreshed
    );
    let cached = matches(&cache, "printer.local", RecordType::A)
        .next()
        .expect("held");
    assert_eq!(
        (cached.source, cached.expires),
        (host(1), at(180)),
        "extended, and still its owner's"
    );
}

/// Another responder still asserting a record its owner withdrew rescues it
/// (RFC 6762 §10.1), and owns it from then on.
#[test]
fn a_record_another_source_asserts_in_its_last_second_is_rescued() {
    let mut cache = cache();
    let record = a_record("printer.local", 5, 120);
    learn(&mut cache, at(0), &record, host(1));
    let goodbye = a_record("printer.local", 5, 0);
    assert_eq!(
        learn(&mut cache, at(10), &goodbye, host(1)),
        Learned::Retired
    );
    assert_eq!(
        learn(&mut cache, at(10), &record, host(2)),
        Learned::Refreshed
    );
    advance(&mut cache, at(12));
    let cached = matches(&cache, "printer.local", RecordType::A)
        .next()
        .expect("rescued");
    assert_eq!(cached.source, host(2));
    assert_eq!(
        learn(&mut cache, at(20), &goodbye, host(1)),
        Learned::Ignored,
        "no longer the first source's to withdraw"
    );
}

/// A record past half its lifetime goes behind the rest, so the walk a
/// known-answer list makes stops at the first record not worth offering.
#[test]
fn a_record_past_half_its_life_goes_behind_the_rest() {
    let mut cache = cache();
    let mut short = a_record("printer.local", 1, 10);
    short.cache_flush = false;
    let mut long = a_record("printer.local", 2, 100);
    long.cache_flush = false;
    learn(&mut cache, at(0), &long, host(1));
    learn(&mut cache, at(1), &short, host(2));
    assert_eq!(held(&cache, "printer.local", RecordType::A), [short, long]);
    advance(&mut cache, at(7));
    assert_eq!(held(&cache, "printer.local", RecordType::A), [long, short]);
    let offered: Vec<Record> = matches(&cache, "printer.local", RecordType::A)
        .take_while(|cached| cached.worth_offering(at(7)))
        .map(|cached| cached.record)
        .collect();
    assert_eq!(offered, [long]);
}

#[test]
fn clearing_reports_every_watched_record() {
    let mut cache = cache();
    learn_watched(
        &mut cache,
        at(0),
        &a_record("printer.local", 5, 100),
        host(1),
    );
    learn(&mut cache, at(0), &a_record("other.local", 6, 100), host(1));
    let mut seen = Vec::new();
    cache.clear(&mut gone_into(&mut seen));
    assert_eq!(seen.len(), 1, "only the watched record is reported");
    assert!(cache.is_empty());
}

/// A lifetime past the longest a responder gives is held as that, so a peer
/// cannot plant a record for decades.
#[test]
fn a_record_is_held_no_longer_than_a_responder_ever_asks() {
    let mut cache = cache();
    let planted = a_record("printer.local", 1, 3_000_000);
    assert_eq!(learn(&mut cache, at(0), &planted, host(1)), Learned::Added);
    let cached = matches(&cache, "printer.local", RecordType::A)
        .next()
        .expect("held");
    assert_eq!(cached.record.ttl, crate::mdns::TTL_HELD_MAX_SECS);
    assert_eq!(
        cached.expires,
        at(i64::from(crate::mdns::TTL_HELD_MAX_SECS))
    );
}

/// A question raised after a record was cut short arms no refresh for it: it
/// is about to go, and a re-query would only ask after it again.
#[test]
fn watching_a_record_cut_short_arms_no_refresh() {
    let mut cache = cache();
    let record = a_record("printer.local", 5, 120);
    learn(&mut cache, at(0), &record, host(1));
    let goodbye = a_record("printer.local", 5, 0);
    assert_eq!(
        learn(&mut cache, at(10), &goodbye, host(1)),
        Learned::Retired
    );
    watch(&mut cache, "printer.local", RecordType::A, Some(WATCHER));
    assert_eq!(cache.next_deadline(), Some(at(11)), "only its expiry");
}
