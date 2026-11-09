//! Unit tests for the artwork desk's policy.
//!
//! Every rule an embedder depends on is exercised here with no thread and no
//! lock: the ask/answer handshake, the deduplication that stops one asset
//! being decoded twice, the hand-out order, the round rule that stops a
//! landing chasing its own tail, and the teardown.

use super::*;

use alloc::string::String;

fn asset(path: &str) -> ArtworkKey {
    ArtworkKey::Asset(String::from(path))
}

fn job(path: &str, side: u32) -> ArtworkJob {
    ArtworkJob {
        key: asset(path),
        side,
    }
}

/// A draw site's request for the asset at `path`, whose first tier is the
/// key the desk is asked for.
fn request(path: &str) -> IconRequest<'_> {
    IconRequest::asset(crate::IconKind::File, path)
}

/// A distinguishable picture, so a test can tell one delivery from another.
fn picture(side: u32) -> Artwork {
    Artwork::new(tairix_raster::Surface::new(side, side).expect("a square surface"))
}

fn is_pending(resolved: &Resolved) -> bool {
    matches!(resolved, Resolved::Pending)
}

#[test]
fn a_first_ask_records_the_decode_and_answers_pending() {
    let mut desk = ArtworkDesk::new(TEST_KEYS);
    assert!(is_pending(&desk.collect(&asset("/a.png"), 32).0));
    assert!(desk.has_work());
    assert_eq!(desk.next_job(), Some(job("/a.png", 32)));
}

#[test]
fn asking_again_for_the_same_asset_starts_no_second_decode() {
    let mut desk = ArtworkDesk::new(TEST_KEYS);
    let _ = desk.collect(&asset("/a.png"), 32).0;
    let _ = desk.collect(&asset("/a.png"), 32).0;
    assert!(desk.next_job().is_some());
    assert!(
        desk.next_job().is_none(),
        "a decode already in flight was handed out twice"
    );
    assert!(!desk.has_work());
}

/// The pixel side is part of the key, so the same asset at two sides is two
/// decodes — a scale change must not be served a resized copy of the old one.
#[test]
fn the_same_asset_at_a_different_side_is_a_different_decode() {
    let mut desk = ArtworkDesk::new(TEST_KEYS);
    let _ = desk.collect(&asset("/a.png"), 32).0;
    let _ = desk.collect(&asset("/a.png"), 48).0;
    assert_eq!(desk.next_job(), Some(job("/a.png", 32)));
    assert_eq!(desk.next_job(), Some(job("/a.png", 48)));
}

#[test]
fn decodes_are_handed_out_in_the_order_they_were_asked_for() {
    let mut desk = ArtworkDesk::new(TEST_KEYS);
    for name in ["/c.png", "/a.png", "/b.png"] {
        let _ = desk.collect(&asset(name), 16).0;
    }
    assert_eq!(desk.next_job(), Some(job("/c.png", 16)));
    assert_eq!(desk.next_job(), Some(job("/a.png", 16)));
    assert_eq!(desk.next_job(), Some(job("/b.png", 16)));
}

#[test]
fn a_delivered_decode_is_collected_once_and_reports_a_landing() {
    let mut desk = ArtworkDesk::new(TEST_KEYS);
    let _ = desk.collect(&asset("/a.png"), 8).0;
    let running = desk.next_job().expect("a job");
    assert!(desk.deliver(&running, Some(picture(8))).kept());
    assert!(
        desk.take_landed().resolves(request("/a.png"), 8),
        "a delivery names the request it answers"
    );
    assert!(
        desk.take_landed().is_empty(),
        "the landing is reported once"
    );

    match desk.collect(&asset("/a.png"), 8).0 {
        Resolved::Done(Some(art)) => assert_eq!(art.surface().width(), 8),
        _ => panic!("the delivered picture was not served"),
    }
    assert!(
        is_pending(&desk.collect(&asset("/a.png"), 8).0),
        "the answer is moved out to the cache, never served twice"
    );
}

/// The prefetch half: asking early records the decode without collecting
/// anything, so a surface that knows what it is about to draw starts the wait
/// before the frame that needs it.
#[test]
fn a_want_records_a_decode_without_collecting_it() {
    let mut desk = ArtworkDesk::new(TEST_KEYS);
    desk.want(&asset("/a.png"), 32);
    assert!(desk.has_work());
    assert_eq!(desk.next_job(), Some(job("/a.png", 32)));
    assert!(
        desk.take_landed().is_empty(),
        "asking for a decode is not the same as one landing"
    );
}

/// A prefetch must never disturb what a draw is about to collect: the answer
/// stays put, and is still served once.
#[test]
fn a_want_never_consumes_an_answer_a_draw_is_about_to_collect() {
    let mut desk = ArtworkDesk::new(TEST_KEYS);
    desk.want(&asset("/a.png"), 8);
    let running = desk.next_job().expect("a job");
    assert!(desk.deliver(&running, Some(picture(8))).kept());

    desk.want(&asset("/a.png"), 8);
    assert!(
        !desk.has_work(),
        "a delivered answer was queued for a second decode"
    );
    assert!(matches!(
        desk.collect(&asset("/a.png"), 8).0,
        Resolved::Done(Some(_))
    ));
}

/// Warming the same set repeatedly — the desktop re-reads its catalog on every
/// launcher press — must cost nothing beyond the lookup.
#[test]
fn warming_the_same_key_twice_starts_one_decode() {
    let mut desk = ArtworkDesk::new(TEST_KEYS);
    desk.want(&asset("/a.png"), 8);
    desk.want(&asset("/a.png"), 8);
    assert!(desk.next_job().is_some());
    assert!(desk.next_job().is_none());
}

/// Warming a key whose answer has been handed over starts a decode again: the
/// cache owns that answer now and is the only thing that knows whether it still
/// holds it, so the desk cannot second-guess a miss. A cache that does still
/// hold it never asks (it peeks before warming).
#[test]
fn a_want_for_a_key_already_handed_over_starts_a_decode_again() {
    let mut desk = ArtworkDesk::new(TEST_KEYS);
    desk.want(&asset("/a.png"), 8);
    let running = desk.next_job().expect("a job");
    assert!(desk.deliver(&running, Some(picture(8))).kept());
    assert!(matches!(
        desk.collect(&asset("/a.png"), 8).0,
        Resolved::Done(_)
    ));

    desk.want(&asset("/a.png"), 8);
    assert!(desk.has_work());
}

#[test]
fn a_stopping_desk_warms_nothing() {
    let mut desk = ArtworkDesk::new(TEST_KEYS);
    desk.stop();
    desk.want(&asset("/a.png"), 8);
    assert!(!desk.has_work());
    assert!(desk.next_job().is_none());
}

/// A refusal is an answer like any other: it is delivered, collected, and
/// retained by the cache, so a broken asset is read once rather than each frame.
#[test]
fn a_refusal_is_delivered_and_collected_like_a_picture() {
    let mut desk = ArtworkDesk::new(TEST_KEYS);
    let _ = desk.collect(&asset("/broken.png"), 8).0;
    let running = desk.next_job().expect("a job");
    assert!(desk.deliver(&running, None).kept());
    assert!(matches!(
        desk.collect(&asset("/broken.png"), 8).0,
        Resolved::Done(None)
    ));
}

/// The reported file-manager defect: an answer the cache took and then evicted
/// must be produced again the next time a paint misses on it. Remembering the
/// key as answered instead left the tile drawing its built-in glyph with
/// nothing left to re-decode it, so the window sat wrong until unrelated input
/// arrived.
#[test]
fn a_key_the_cache_dropped_is_decoded_again() {
    let mut desk = ArtworkDesk::new(TEST_KEYS);
    let _ = desk.collect(&asset("/a.png"), 8).0;
    let running = desk.next_job().expect("a job");
    assert!(desk.deliver(&running, Some(picture(8))).kept());
    assert!(matches!(
        desk.collect(&asset("/a.png"), 8).0,
        Resolved::Done(_)
    ));

    // The cache took that answer and then dropped it under its budget, so the
    // next paint misses on the very same key.
    assert!(is_pending(&desk.collect(&asset("/a.png"), 8).0));
    assert!(is_pending(&desk.collect(&asset("/a.png"), 8).0));
    assert_eq!(
        desk.next_job(),
        Some(job("/a.png", 8)),
        "a miss on a key the cache no longer holds is a genuine miss"
    );
    assert!(
        desk.next_job().is_none(),
        "and it is queued once, not once per asking paint"
    );
}

/// A refusal the *cache* made — no room the band allows — is not a reason to
/// decode again. Without this the desktop read and decoded every icon it drew
/// on every repaint, spending the disk and the parser sandbox precisely when
/// the machine was short of the memory that would have held the answer.
#[test]
fn a_declined_answer_is_not_offered_again_until_the_band_moves() {
    let mut desk = ArtworkDesk::new(TEST_KEYS);
    let _ = desk.collect(&asset("/a.png"), 8).0;
    let running = desk.next_job().expect("a job");
    assert!(desk.deliver(&running, Some(picture(8))).kept());
    assert!(matches!(
        desk.collect(&asset("/a.png"), 8).0,
        Resolved::Done(_)
    ));
    desk.decline(&asset("/a.png"), 8, 64);

    // Every later paint asks for nothing: the draw takes its glyph.
    for _ in 0..3 {
        assert!(is_pending(&desk.collect(&asset("/a.png"), 8).0));
        assert!(!desk.has_work(), "a declined key queues no decode");
        assert!(desk.next_job().is_none());
    }

    // The cache coming to hold it is what makes the answer worth having again.
    desk.retry_declined(|_| true);
    assert!(is_pending(&desk.collect(&asset("/a.png"), 8).0));
    assert_eq!(desk.next_job(), Some(job("/a.png", 8)));
}

/// The collect that hands an answer over forgets the key, so the refusal that
/// follows it names a key the desk is no longer holding. It must still be
/// recorded — that report is the whole of what stops the refusal renewing
/// itself.
#[test]
fn declining_a_key_the_collect_forgot_still_records_the_refusal() {
    let mut desk = ArtworkDesk::new(TEST_KEYS);
    desk.decline(&asset("/gone.png"), 8, 64);
    assert!(!desk.has_work());
    assert!(is_pending(&desk.collect(&asset("/gone.png"), 8).0));
    assert!(
        desk.next_job().is_none(),
        "a refused key was offered for decoding again"
    );

    desk.retry_declined(|_| true);
    assert!(is_pending(&desk.collect(&asset("/gone.png"), 8).0));
    assert_eq!(desk.next_job(), Some(job("/gone.png", 8)));
}

/// A refusal reported while a producer is midway through that key leaves the
/// decode alone: it has yet to be answered, and the collect that answers it
/// will report the refusal again if it still stands.
#[test]
fn declining_a_key_in_flight_leaves_the_decode_to_land() {
    let mut desk = ArtworkDesk::new(TEST_KEYS);
    let _ = desk.collect(&asset("/a.png"), 8).0;
    let running = desk.next_job().expect("a job");

    desk.decline(&asset("/a.png"), 8, 64);
    assert!(
        desk.deliver(&running, Some(picture(8))).kept(),
        "the decode in flight was stranded"
    );
    assert!(matches!(
        desk.collect(&asset("/a.png"), 8).0,
        Resolved::Done(Some(_))
    ));
}

/// Asking again never throws work away: an answer nobody has collected yet is
/// still there to collect, and a decode in flight is awaited rather than
/// re-recorded.
#[test]
fn asking_again_keeps_work_in_flight_and_answers_not_yet_collected() {
    let mut desk = ArtworkDesk::new(TEST_KEYS);
    let _ = desk.collect(&asset("/done.png"), 8).0;
    let _ = desk.collect(&asset("/running.png"), 8).0;
    let done = desk.next_job().expect("the first job");
    let running = desk.next_job().expect("the second job");
    assert!(desk.deliver(&done, Some(picture(8))).kept());

    assert!(
        matches!(
            desk.collect(&asset("/done.png"), 8).0,
            Resolved::Done(Some(_))
        ),
        "an uncollected answer was discarded"
    );
    assert!(
        is_pending(&desk.collect(&asset("/running.png"), 8).0),
        "a decode in flight was re-recorded rather than awaited"
    );
    assert!(!desk.has_work(), "and it was not queued a second time");
    assert!(desk.deliver(&running, Some(picture(8))).kept());
}

/// A key re-asked after its answer was handed over is queued afresh, and
/// exactly once: the entry the first hand-out left behind must not become a
/// second one.
#[test]
fn a_key_re_asked_after_its_answer_is_handed_out_exactly_once() {
    let mut desk = ArtworkDesk::new(TEST_KEYS);
    let _ = desk.collect(&asset("/a.png"), 8).0;
    let running = desk.next_job().expect("a job");
    assert!(desk.deliver(&running, Some(picture(8))).kept());
    assert!(matches!(
        desk.collect(&asset("/a.png"), 8).0,
        Resolved::Done(_)
    ));

    let _ = desk.collect(&asset("/a.png"), 8).0;
    assert_eq!(desk.next_job(), Some(job("/a.png", 8)));
    assert!(desk.next_job().is_none());
}

/// Teardown releases a decode nobody collected — overwriting its pixels first,
/// on the same terms the artwork cache wipes its own, so one user's rendered
/// artwork does not outlive their session in reusable heap.
#[test]
fn teardown_releases_a_decode_it_is_still_holding() {
    let mut desk = ArtworkDesk::new(TEST_KEYS);
    let _ = desk.collect(&asset("/a.png"), 4).0;
    let running = desk.next_job().expect("a job");
    assert!(desk.deliver(&running, Some(picture(4))).kept());

    desk.stop();
    assert!(
        matches!(desk.collect(&asset("/a.png"), 4).0, Resolved::Pending),
        "a torn-down desk still holding an answer would serve wiped pixels"
    );
}

/// An answer for a job the desk is no longer holding — the embedder tore down
/// between the hand-out and the delivery — is dropped, and owes no wake.
#[test]
fn an_answer_the_desk_is_not_holding_is_dropped() {
    let mut desk = ArtworkDesk::new(TEST_KEYS);
    let _ = desk.collect(&asset("/a.png"), 8).0;
    let running = desk.next_job().expect("a job");
    desk.stop();
    assert!(!desk.deliver(&running, Some(picture(8))).kept());
    assert!(desk.take_landed().is_empty());
}

#[test]
fn a_stopping_desk_records_nothing_and_hands_out_nothing() {
    let mut desk = ArtworkDesk::new(TEST_KEYS);
    let _ = desk.collect(&asset("/a.png"), 8).0;
    desk.stop();
    assert!(desk.stopping());
    assert!(!desk.has_work());
    assert!(desk.next_job().is_none());
    assert!(is_pending(&desk.collect(&asset("/b.png"), 8).0));
    assert!(
        !desk.has_work(),
        "a stopping desk recorded a decode nobody will run"
    );
}

/// A bundle key and an asset key of the same spelling are different slots, so
/// resolving one can never serve the other's manifest-derived picture.
#[test]
fn a_bundle_and_an_asset_of_the_same_spelling_are_different_decodes() {
    let mut desk = ArtworkDesk::new(TEST_KEYS);
    let _ = desk
        .collect(
            &ArtworkKey::Asset(String::from("/System/Applications/E.app")),
            8,
        )
        .0;
    let _ = desk
        .collect(
            &ArtworkKey::Bundle {
                dir: String::from("/System/Applications/E.app"),
                version: None,
            },
            8,
        )
        .0;
    assert!(desk.next_job().is_some());
    assert!(desk.next_job().is_some());
    assert!(desk.next_job().is_none());
}

/// The desk's own [`ArtworkResolver`] impl is the deferring resolver a
/// single-threaded embedder hands the cache: a miss records the decode and
/// answers `Pending`, and the answer is served once a job has been delivered.
#[test]
fn the_desk_answers_as_a_deferring_resolver() {
    let mut desk = ArtworkDesk::new(TEST_KEYS);
    let key = asset("/a.png");
    assert!(is_pending(&ArtworkResolver::resolve(&mut desk, &key, 16)));

    let running = desk.next_job().expect("the miss recorded a decode");
    assert_eq!(running, job("/a.png", 16));
    assert!(desk.deliver(&running, Some(picture(16))).kept());
    assert!(matches!(
        ArtworkResolver::resolve(&mut desk, &key, 16),
        Resolved::Done(Some(_))
    ));
}

/// `prefetch` and `declined` reach the same policy the inherent
/// [`ArtworkDesk::want`] / [`ArtworkDesk::decline`] do, so a cache driving the
/// desk through the trait cannot get different behaviour from one driving it
/// directly.
#[test]
fn the_resolver_impl_prefetches_and_declines_through_the_same_policy() {
    let mut desk = ArtworkDesk::new(TEST_KEYS);
    let key = asset("/a.png");
    ArtworkResolver::prefetch(&mut desk, &key, 16);
    assert!(desk.has_work(), "a prefetch records the decode");

    let running = desk.next_job().expect("a job");
    assert!(desk.deliver(&running, Some(picture(16))).kept());
    assert!(matches!(
        ArtworkResolver::resolve(&mut desk, &key, 16),
        Resolved::Done(Some(_))
    ));

    ArtworkResolver::declined(&mut desk, &key, 16, 64);
    assert!(is_pending(&ArtworkResolver::resolve(&mut desk, &key, 16)));
    assert!(!desk.has_work(), "a declined key must not be re-offered");
}

/// A batch of asks costs the loop one wake, not one per icon: a producer that
/// takes and delivers one job at a time is owed nothing until the queue it is
/// working through is empty.
#[test]
fn a_batch_of_decodes_owes_one_wake_on_the_last_of_them() {
    let mut desk = ArtworkDesk::new(TEST_KEYS);
    for name in ["/a.png", "/b.png", "/c.png"] {
        desk.want(&asset(name), 16);
    }
    for expected in ["/a.png", "/b.png"] {
        let job = desk.next_job().expect("a queued job");
        assert_eq!(job.key, asset(expected));
        assert!(
            !desk.deliver(&job, Some(picture(16))).wake(),
            "a wake mid-batch costs the loop a repaint per icon"
        );
    }
    let last = desk.next_job().expect("the last job");
    assert!(
        desk.deliver(&last, Some(picture(16))).wake(),
        "the drained batch owes the loop its one wake"
    );
    let landed = desk.take_landed();
    assert!(
        ["/a.png", "/b.png", "/c.png"]
            .iter()
            .all(|name| landed.resolves(request(name), 16)),
        "all three landings are one repaint, and it names each of them"
    );
}

/// A lone ask is its own drained batch, so an icon still lands the moment it
/// is ready rather than waiting for company.
#[test]
fn a_lone_decode_owes_its_wake_at_once() {
    let mut desk = ArtworkDesk::new(TEST_KEYS);
    let _ = desk.collect(&asset("/a.png"), 16).0;
    let job = desk.next_job().expect("a job");
    assert!(desk.deliver(&job, Some(picture(16))).wake());
}

/// The debt outlives the delivery that incurred it, so a delivery the desk
/// keeps nothing from still discharges what an earlier one is owed — a batch
/// drained without a wake is never left unshown.
///
/// This is also why a delivery answers two things rather than one: here the
/// second call keeps nothing *and* owes the wake, which no single flag can
/// say. A producer reading one for the other would either strand the first
/// answer unshown or believe a spent job had been accepted.
#[test]
fn a_delivery_that_keeps_nothing_still_discharges_an_outstanding_debt() {
    let mut desk = ArtworkDesk::new(TEST_KEYS);
    desk.want(&asset("/a.png"), 16);
    desk.want(&asset("/b.png"), 16);
    let first = desk.next_job().expect("the first job");
    let mid_batch = desk.deliver(&first, Some(picture(16)));
    assert!(mid_batch.kept(), "the first answer was refused");
    assert!(!mid_batch.wake(), "a wake mid-batch is a repaint per icon");
    let _ = desk.next_job().expect("the second job");
    let spent = desk.deliver(&first, Some(picture(16)));
    assert!(
        !spent.kept(),
        "a key already answered was overwritten by a second decode"
    );
    assert!(
        spent.wake(),
        "the wake the first delivery is owed was forgotten"
    );
}

/// The share a decode's path puts it in: the stick's, or home's.
fn share_of(job: &ArtworkJob) -> &'static str {
    let on_the_stick = job
        .key
        .read_path()
        .is_some_and(|path| path.starts_with("/Storage/"));
    if on_the_stick {
        "stick"
    } else {
        "home"
    }
}

fn home(share: &&str) -> bool {
    *share == "home"
}

fn stick(share: &&str) -> bool {
    *share == "stick"
}

/// Producers serving their own shares take only what they admit, and a job
/// passed over keeps its place for its own producer.
#[test]
fn a_producer_takes_only_the_decodes_its_share_admits() {
    let mut desk = ArtworkDesk::new(TEST_KEYS);
    for name in [
        "/Storage/stick/a.png",
        "/Users/b.png",
        "/Storage/stick/c.png",
    ] {
        desk.want(&asset(name), 16);
    }
    assert_eq!(
        desk.next_job_where(share_of, home),
        Some(("home", job("/Users/b.png", 16)))
    );
    assert_eq!(desk.next_job_where(share_of, home), None);
    assert_eq!(
        desk.next_job_where(share_of, |_| true),
        Some(("stick", job("/Storage/stick/a.png", 16)))
    );
    assert_eq!(
        desk.next_job_where(share_of, stick),
        Some(("stick", job("/Storage/stick/c.png", 16)))
    );
}

/// Across the shares a producer admits, the decode asked for first goes first.
#[test]
fn across_the_shares_admitted_the_first_asked_goes_first() {
    let mut desk = ArtworkDesk::new(TEST_KEYS);
    for name in ["/Users/x.png", "/Storage/stick/a.png", "/Users/y.png"] {
        desk.want(&asset(name), 16);
    }
    let taken: Vec<_> = core::iter::from_fn(|| desk.next_job_where(share_of, |_| true)).collect();
    assert_eq!(
        taken,
        [
            ("home", job("/Users/x.png", 16)),
            ("stick", job("/Storage/stick/a.png", 16)),
            ("home", job("/Users/y.png", 16)),
        ]
    );
}

/// A share's batch wakes the loop once that share has none left, whatever
/// another share's producer, held up on its own device, still has queued.
#[test]
fn a_share_drained_owes_its_wake_beside_another_shares_queue() {
    let mut desk = ArtworkDesk::new(TEST_KEYS);
    for name in ["/Storage/stick/a.png", "/Users/b.png", "/Users/c.png"] {
        desk.want(&asset(name), 16);
    }
    let (_, first) = desk
        .next_job_where(share_of, home)
        .expect("the first home icon");
    assert!(
        !desk
            .deliver_where(&first, Some(picture(16)), share_of, &"home")
            .wake(),
        "the home share still has an icon queued"
    );
    let (_, last) = desk
        .next_job_where(share_of, home)
        .expect("the last home icon");
    assert!(
        desk.deliver_where(&last, Some(picture(16)), share_of, &"home")
            .wake(),
        "the stick's queued icon holds no wake of the home share's back"
    );
    assert_eq!(
        desk.next_job_where(share_of, stick),
        Some(("stick", job("/Storage/stick/a.png", 16))),
        "the stick's icon still waits for its own producer"
    );
}

/// Only the ask that records a decode says so: it alone is owed a worker's
/// wake, so a paint asking again for what is already queued wakes nobody.
#[test]
fn only_the_ask_that_records_a_decode_says_so() {
    let mut desk: ArtworkDesk = ArtworkDesk::new(TEST_KEYS);
    assert!(desk.want(&asset("/a.png"), 8), "the first ask records it");
    assert!(!desk.want(&asset("/a.png"), 8));
    let (answer, recorded) = desk.collect(&asset("/a.png"), 8);
    assert!(
        is_pending(&answer) && !recorded,
        "a queued decode is asked again"
    );
    let (answer, recorded) = desk.collect(&asset("/b.png"), 8);
    assert!(is_pending(&answer) && recorded);
    let running = desk.next_job().expect("a job");
    assert!(!desk.want(&running.key, running.side), "a decode under way");
    assert!(desk.deliver(&running, Some(picture(8))).kept());
    let (answer, recorded) = desk.collect(&running.key, running.side);
    assert!(matches!(answer, Resolved::Done(Some(_))) && !recorded);
    desk.hold_under("/held");
    assert!(
        !desk.want(&asset("/held/c.png"), 8),
        "nothing is recorded under a held tree"
    );
    desk.stop();
    assert!(
        !desk.want(&asset("/d.png"), 8),
        "a stopping desk records nothing"
    );
}

/// Each decode is routed to its share once, however many takes pass it over,
/// and again only after a reroute, which keeps the order asked.
#[test]
fn a_decode_is_routed_once_however_often_it_is_passed_over() {
    let mut desk = ArtworkDesk::new(TEST_KEYS);
    for name in [
        "/Storage/stick/a.png",
        "/Storage/stick/b.png",
        "/Users/c.png",
    ] {
        desk.want(&asset(name), 16);
    }
    let routed = core::cell::Cell::new(0);
    let route = |job: &ArtworkJob| {
        routed.set(routed.get() + 1);
        share_of(job)
    };
    for _ in 0..3 {
        assert_eq!(desk.next_job_where(&route, |_| false), None);
    }
    let (_, last) = desk.next_job_where(&route, home).expect("home's icon");
    assert!(desk.deliver_where(&last, None, &route, &"home").wake());
    assert_eq!(routed.get(), 3, "each decode routed once");
    desk.reroute();
    assert_eq!(
        desk.next_job_where(&route, |_| true),
        Some(("stick", job("/Storage/stick/a.png", 16)))
    );
    assert_eq!(routed.get(), 5, "a reroute routes both decodes waiting");
    assert_eq!(
        desk.next_job_where(&route, |_| true),
        Some(("stick", job("/Storage/stick/b.png", 16))),
        "the order asked survives the reroute"
    );
    assert_eq!(routed.get(), 5);
}

/// Past the bound the thumbnail asked for longest ago goes, whichever share
/// it was routed to.
#[test]
fn the_oldest_thumbnail_goes_past_the_bound_whatever_its_share() {
    let mut desk = ArtworkDesk::new(TEST_KEYS);
    desk.want(&thumbnail_at("/Storage/stick/first.png"), 64);
    desk.want(&thumbnail("second.png"), 64);
    assert_eq!(desk.next_thumbnail_where(share_of, |_| false), None);
    for n in 2..=MAX_WANTED_THUMBNAILS {
        desk.want(&thumbnail(&alloc::format!("{n}.png")), 64);
    }
    assert_eq!(
        desk.next_thumbnail_where(share_of, stick),
        None,
        "the oldest, the stick's, was withdrawn"
    );
    assert_eq!(
        desk.next_thumbnail_where(share_of, home)
            .map(|(_, job)| job.key),
        Some(thumbnail("second.png"))
    );
}

/// A job declined while it waited is no icon still to come: it holds no
/// batch's wake back, and is dropped from the hand-out order as it is passed.
#[test]
fn a_job_declined_while_queued_is_passed_and_holds_no_wake() {
    let mut desk = ArtworkDesk::new(TEST_KEYS);
    desk.want(&asset("/Users/a.png"), 16);
    desk.want(&asset("/Users/b.png"), 16);
    let first = desk.next_job().expect("the first job");
    desk.decline(&asset("/Users/b.png"), 16, 64);
    assert!(
        desk.deliver(&first, Some(picture(16))).wake(),
        "the batch's last icon to come has landed"
    );
    assert_eq!(desk.next_job(), None);
    assert!(
        desk.queue.is_empty(),
        "the declined job was dropped as it was passed"
    );
}

/// Teardown discards the debt with the answers it would have shown.
#[test]
fn a_stopped_desk_owes_no_wake_for_what_it_wiped() {
    let mut desk = ArtworkDesk::new(TEST_KEYS);
    desk.want(&asset("/a.png"), 16);
    desk.want(&asset("/b.png"), 16);
    let first = desk.next_job().expect("the first job");
    assert!(!desk.deliver(&first, Some(picture(16))).wake());
    let second = desk.next_job().expect("the second job");
    desk.stop();
    assert!(!desk.deliver(&second, Some(picture(16))).wake());
}

/// A landing names the decode it answered and nothing else, which is the
/// whole of what lets a surface repaint one item instead of all of them.
#[test]
fn a_landing_names_only_what_it_answered() {
    let mut desk = ArtworkDesk::new(TEST_KEYS);
    let _ = desk.collect(&asset("/a.png"), 8).0;
    let _ = desk.collect(&asset("/b.png"), 8).0;
    let running = desk.next_job().expect("a job");
    let _ = desk.deliver(&running, Some(picture(8)));

    let landed = desk.take_landed();
    assert!(landed.resolves(request("/a.png"), 8));
    assert!(
        !landed.resolves(request("/b.png"), 8),
        "a decode still being produced has not changed anything"
    );
    assert!(
        !landed.resolves(request("/a.png"), 16),
        "a picture at another side is another decode"
    );
}

/// A refusal is an answer: the item that asked draws the tier below it, so it
/// is named exactly as a picture would be.
#[test]
fn a_refusal_lands_like_a_picture() {
    let mut desk = ArtworkDesk::new(TEST_KEYS);
    let _ = desk.collect(&asset("/a.png"), 8).0;
    let running = desk.next_job().expect("a job");
    let _ = desk.deliver(&running, None);
    assert!(desk.take_landed().resolves(request("/a.png"), 8));
}

/// A request is named by *any* tier that landed, because the tier that draws
/// today depends on which of them have answered.
#[test]
fn a_landed_class_picture_names_the_request_that_falls_back_to_it() {
    let mut desk = ArtworkDesk::new(TEST_KEYS);
    let _ = desk
        .collect(&ArtworkKey::Raster(crate::IconKind::AppBundle), 8)
        .0;
    let running = desk.next_job().expect("a job");
    let _ = desk.deliver(&running, Some(picture(8)));

    let landed = desk.take_landed();
    assert!(
        landed.resolves(
            IconRequest::asset(crate::IconKind::AppBundle, "/own.png"),
            8
        ),
        "the picture this request draws while its own icon is pending"
    );
}

/// Tearing down wipes every answer still held, so the batch that would have
/// shown them is dropped with them rather than driving a repaint of pixels
/// that no longer exist.
#[test]
fn a_teardown_drops_the_unreported_batch() {
    let mut desk = ArtworkDesk::new(TEST_KEYS);
    let _ = desk.collect(&asset("/a.png"), 8).0;
    let running = desk.next_job().expect("a job");
    let _ = desk.deliver(&running, Some(picture(8)));
    desk.stop();
    assert!(desk.take_landed().is_empty());
}

fn thumbnail(name: &str) -> ArtworkKey {
    thumbnail_at(&alloc::format!("/Users/ann/Home/Pictures/{name}"))
}

fn thumbnail_at(path: &str) -> ArtworkKey {
    ArtworkKey::Thumbnail(crate::Thumbnail {
        path: String::from(path),
        stamp: tairix_abi::fs::DocumentStamp {
            size: 1,
            modified: tairix_abi::time::Time64::UNIX_EPOCH,
            id: tairix_abi::fs::FileId {
                volume: [1; 16],
                node: 1,
            },
            content_gen: 1,
        },
    })
}

/// A sweep withdraws every thumbnail no surface asked for since the last one,
/// so a picture scrolled out of view is never decoded; one still asked for
/// stays queued.
#[test]
fn a_sweep_withdraws_the_thumbnails_no_surface_asked_for() {
    let mut desk = ArtworkDesk::new(TEST_KEYS);
    let (shown, scrolled) = (thumbnail("shown.png"), thumbnail("scrolled.png"));
    desk.want(&scrolled, 64);
    desk.want(&shown, 64);
    desk.sweep_thumbnails();
    assert!(is_pending(&desk.collect(&shown, 64).0));
    desk.sweep_thumbnails();
    assert_eq!(
        desk.next_thumbnail().map(|job| job.key),
        Some(shown.clone())
    );
    assert_eq!(desk.next_thumbnail(), None, "the unasked one was withdrawn");
    assert!(is_pending(&desk.collect(&scrolled, 64).0));
    assert_eq!(
        desk.next_thumbnail().map(|job| job.key),
        Some(scrolled),
        "asked again, it is queued again"
    );
}

/// A thumbnail produced and never collected goes at the next sweep, its
/// pixels wiped, rather than held for the session; one in production is left
/// to land.
#[test]
fn an_uncollected_thumbnail_goes_at_the_next_sweep() {
    let mut desk = ArtworkDesk::new(TEST_KEYS);
    let (landed, running) = (thumbnail("landed.png"), thumbnail("running.png"));
    desk.want(&landed, 64);
    desk.want(&running, 64);
    desk.sweep_thumbnails();
    let first = desk.next_thumbnail().expect("queued");
    let second = desk.next_thumbnail().expect("queued");
    assert!(desk.deliver(&first, Some(picture(64))).kept());
    desk.sweep_thumbnails();
    assert!(
        is_pending(&desk.collect(&landed, 64).0),
        "the picture went with the sweep"
    );
    assert!(
        desk.deliver(&second, Some(picture(64))).kept(),
        "landed after all"
    );
}

/// Icons are never swept: their decode is cheap and every surface shares it.
#[test]
fn a_sweep_leaves_icons_alone() {
    let mut desk = ArtworkDesk::new(TEST_KEYS);
    desk.want(&asset("/System/Graphics/Icons/file.png"), 32);
    desk.sweep_thumbnails();
    desk.sweep_thumbnails();
    assert_eq!(
        desk.next_job(),
        Some(job("/System/Graphics/Icons/file.png", 32))
    );
}

/// Past the bound the longest-waiting thumbnail is withdrawn, so scrolling
/// between two sweeps cannot queue without limit.
#[test]
fn the_thumbnails_waiting_are_bounded() {
    let mut desk = ArtworkDesk::new(TEST_KEYS);
    for n in 0..=MAX_WANTED_THUMBNAILS {
        desk.want(&thumbnail(&alloc::format!("{n}.png")), 64);
    }
    assert_eq!(
        desk.next_thumbnail().map(|job| job.key),
        Some(thumbnail("1.png")),
        "the first was withdrawn"
    );
    let mut queued = 1;
    while desk.next_thumbnail().is_some() {
        queued += 1;
    }
    assert_eq!(queued, MAX_WANTED_THUMBNAILS);
}

/// What a surface that repainted only part of itself says about the rest keeps
/// a waiting thumbnail through the next sweep, and starts nothing for a
/// picture the desk does not hold.
#[test]
fn keeping_spares_a_thumbnail_from_the_sweep_and_records_nothing_new() {
    let mut desk = ArtworkDesk::new(TEST_KEYS);
    let shown = thumbnail("shown.png");
    desk.want(&shown, 64);
    desk.sweep_thumbnails();
    desk.keep(&shown, 64);
    desk.sweep_thumbnails();
    assert_eq!(
        desk.next_thumbnail().map(|job| job.key),
        Some(shown),
        "a thumbnail a surface still shows was withdrawn"
    );

    desk.keep(&thumbnail("evicted.png"), 64);
    desk.keep(&asset("/evicted.png"), 32);
    assert!(
        !desk.has_work(),
        "keeping a picture the desk holds nothing for queued its decode"
    );
}

/// A band that moved without giving the cache room for a declined decode
/// leaves it declined: offering it would cost a read and a decode only to be
/// refused again. One that did give room offers it.
///
/// Regression: every band change offered every declined decode again, though
/// between the deepest bands the room for them is the same, so each change
/// re-read and re-decoded every icon refused before, to refuse it again.
#[test]
fn a_decline_is_retried_only_once_the_cache_would_hold_it() {
    let mut desk: ArtworkDesk = ArtworkDesk::new(TEST_KEYS);
    desk.decline(&asset("/small.png"), 8, 64);
    desk.decline(&asset("/large.png"), 8, 4096);
    assert!(
        desk.retry_declined(|bytes| bytes <= 1024),
        "the small one fits"
    );
    let landed = desk.take_landed();
    assert!(landed.resolves(request("/small.png"), 8));
    assert!(!landed.resolves(request("/large.png"), 8));
    assert!(is_pending(&desk.collect(&asset("/large.png"), 8).0));
    assert!(!desk.has_work(), "the large one stayed declined");
    assert!(
        !desk.retry_declined(|bytes| bytes <= 1024),
        "still too large"
    );
    assert!(desk.retry_declined(|_| true), "until the cache holds it");
}

/// A decode the band refused to keep is named as landed once the band moves,
/// so the item drawn without it asks again: nothing else would, since what it
/// shows is already on screen.
#[test]
fn retrying_the_declined_names_them_as_landed() {
    let mut desk: ArtworkDesk = ArtworkDesk::new(TEST_KEYS);
    assert!(!desk.retry_declined(|_| true), "nothing was declined");
    desk.decline(&asset("/a.png"), 8, 64);
    assert!(desk.retry_declined(|_| true));
    assert!(desk.take_landed().resolves(request("/a.png"), 8));
    assert!(
        !desk.retry_declined(|_| true),
        "a retried decline was retried again"
    );
    assert!(is_pending(&desk.collect(&asset("/a.png"), 8).0));
    assert!(
        desk.has_work(),
        "the retried decode was not asked for again"
    );
}

/// The version a listing named a bundle at as `n`.
fn stamp(n: u64) -> DocumentStamp {
    DocumentStamp {
        size: n,
        modified: tairix_abi::time::Time64::UNIX_EPOCH,
        id: tairix_abi::fs::FileId {
            volume: [1; 16],
            node: 7,
        },
        content_gen: n,
    }
}

/// A listed bundle's own icon at version `n`, asked for before its files are
/// read.
fn listed(dir: &str, n: u64) -> ArtworkKey {
    ArtworkKey::Bundle {
        dir: String::from(dir),
        version: Some(BundleVersion::Listed(stamp(n))),
    }
}

/// The bundle at `dir` a listing names at `n`.
fn listed_bundle(dir: &str, n: u64) -> ListedBundle {
    ListedBundle {
        dir: String::from(dir),
        listed: stamp(n),
    }
}

/// What a bundle's picture is read from: a manifest and an icon at the
/// versions named, each `None` where absent.
fn files(manifest: Option<u64>, icon: Option<u64>) -> BundleFiles {
    BundleFiles {
        manifest: manifest.map(stamp),
        icon: icon.map(stamp),
    }
}

/// The copy defect: a bundle read while the copy is still filling it would
/// picture, or refuse, the half-written bundle, and that answer would stand.
/// While its tree is held nothing under it is run — and nothing is recorded,
/// so however much of it is shown the hold costs one path.
#[test]
fn nothing_under_a_held_tree_is_run_or_recorded() {
    let mut desk = ArtworkDesk::new(TEST_KEYS);
    desk.hold_under("/Storage/B/One.app");
    for stamp in 1..=64 {
        assert!(is_pending(
            &desk.collect(&listed("/Storage/B/One.app", stamp), 32).0
        ));
    }
    desk.want(&asset("/Storage/B/One.app/Resources/icon.png"), 32);
    assert!(!desk.has_work(), "a decode under a held tree was queued");
    assert!(desk.next_job().is_none());
    assert!(
        desk.slots.is_empty(),
        "a held tree recorded what was asked under it"
    );

    desk.want(&asset("/Storage/B/One.apps/icon.png"), 32);
    assert_eq!(
        desk.next_job(),
        Some(job("/Storage/B/One.apps/icon.png", 32)),
        "a sibling whose name begins the same way was held"
    );
}

/// Releasing the tree names it, so every item drawn from inside it repaints at
/// whatever side; each is produced when it asks again, so a version the
/// listing has since replaced costs nothing.
#[test]
fn releasing_a_tree_names_it_for_every_item_drawn_from_inside() {
    let mut desk = ArtworkDesk::new(TEST_KEYS);
    desk.hold_under("/Storage/B/One.app");
    let current = listed("/Storage/B/One.app", 2);
    let _ = desk.collect(&current, 32).0;

    assert!(desk.release_under("/Storage/B/One.app"));
    let landed = desk.take_landed();
    let inside = IconRequest::bundle(crate::IconKind::AppBundle, "/Storage/B/One.app");
    for side in [16, 32, 96] {
        assert!(landed.resolves(inside, side), "side {side}");
    }
    assert!(!landed.resolves(
        IconRequest::bundle(crate::IconKind::AppBundle, "/Storage/B/Two.app"),
        32
    ));
    assert!(
        !desk.has_work(),
        "a released decode ran before it was asked for"
    );

    assert!(is_pending(&desk.collect(&current, 32).0));
    assert_eq!(
        desk.next_job(),
        Some(ArtworkJob {
            key: current,
            side: 32
        })
    );
    assert!(desk.take_landed().is_empty(), "the tree was named twice");
    assert!(!desk.release_under("/Storage/B/One.app"), "released twice");
}

/// A tree inside another held tree is still held by the outer one once it is
/// released itself.
#[test]
fn a_tree_inside_a_held_tree_stays_held_until_both_are_released() {
    let mut desk: ArtworkDesk = ArtworkDesk::new(TEST_KEYS);
    desk.hold_under("/Storage/B/Apps");
    desk.hold_under("/Storage/B/Apps/One.app");
    let inner = listed("/Storage/B/Apps/One.app", 1);

    assert!(desk.release_under("/Storage/B/Apps/One.app"));
    let _ = desk.take_landed();
    let _ = desk.collect(&inner, 32).0;
    assert!(!desk.has_work(), "the outer tree's hold was lost");

    assert!(desk.release_under("/Storage/B/Apps"));
    let _ = desk.collect(&inner, 32).0;
    assert!(desk.has_work());
}

/// A tree never held releases nothing, and a stopping desk holds nothing and
/// reports no release.
#[test]
fn a_tree_never_held_releases_nothing_and_a_stop_drops_every_hold() {
    let mut desk: ArtworkDesk = ArtworkDesk::new(TEST_KEYS);
    assert!(!desk.release_under("/Storage/B/One.app"));
    desk.hold_under("/Storage/B/One.app");
    assert!(desk.release_under("/Storage/B/One.app"));
    desk.hold_under("/Storage/B/One.app");
    desk.stop();
    assert!(
        desk.take_landed().is_empty(),
        "a stop kept a release to report"
    );
    assert!(!desk.release_under("/Storage/B/One.app"));
}

/// The root holds everything named by a path.
#[test]
fn holding_the_root_holds_every_path() {
    let mut desk: ArtworkDesk = ArtworkDesk::new(TEST_KEYS);
    desk.hold_under("/");
    let _ = desk
        .collect(&asset("/System/Graphics/Icons/file.png"), 32)
        .0;
    assert!(!desk.has_work());
    assert!(desk.release_under("/"));
    assert!(desk
        .take_landed()
        .resolves(request("/System/Graphics/Icons/file.png"), 32));
}

/// A listed bundle is pictured by the files its first read found: asked for
/// at the version listed until then, and at those files' version after, under
/// which the decode the read made is kept.
#[test]
fn a_listed_bundle_is_pictured_by_the_files_its_first_read_found() {
    let mut desk = ArtworkDesk::new(TEST_KEYS);
    let one = listed_bundle("/Users/ann/Desktop/One.app", 1);
    assert_eq!(
        desk.version_of(&one.dir, one.listed, 32),
        BundleVersion::Listed(one.listed)
    );
    assert!(is_pending(&desk.collect(&listed(&one.dir, 1), 32).0));
    let first = desk.next_job().expect("the first read");
    let read = files(Some(2), Some(3));
    assert!(desk
        .deliver_listed(&first, Some(picture(32)), read, |_| (), &())
        .kept());
    assert_eq!(
        desk.version_of(&one.dir, one.listed, 32),
        BundleVersion::Files(read)
    );
    assert_eq!(desk.files_of(&one), Some(read));
    let request = IconRequest::listed_bundle(crate::IconKind::AppBundle, &one.dir, one.listed);
    assert!(desk.take_landed().resolves(request, 32));
    let keyed = ArtworkKey::Bundle {
        dir: one.dir.clone(),
        version: Some(BundleVersion::Files(read)),
    };
    assert!(
        matches!(desk.collect(&keyed, 32).0, Resolved::Done(Some(_))),
        "the answer is kept under the files' version"
    );
}

/// A bundle read again with its files as they stood decodes nothing, and
/// nothing lands for it.
#[test]
fn a_bundle_read_again_with_its_files_unchanged_decodes_nothing() {
    let mut desk = ArtworkDesk::new(TEST_KEYS);
    desk.follow_bundles();
    let one = listed_bundle("/Users/ann/Desktop/One.app", 1);
    let _ = desk.version_of(&one.dir, one.listed, 32);
    let _ = desk.collect(&listed(&one.dir, 1), 32);
    let first = desk.next_job().expect("the first read");
    let read = files(Some(2), Some(3));
    let _ = desk.deliver_listed(&first, Some(picture(32)), read, |_| (), &());
    let _ = desk.take_landed();
    assert!(desk.moved(&one));
    let again = desk.next_job().expect("the read again");
    assert_eq!(
        desk.files_of(&one),
        Some(read),
        "what a producer holds a read against"
    );
    let _ = desk.unchanged(&again, |_| (), &());
    assert!(
        desk.take_landed().is_empty(),
        "an unchanged bundle repaints nothing"
    );
    assert_eq!(
        desk.version_of(&one.dir, one.listed, 32),
        BundleVersion::Files(read)
    );
    assert!(!desk.has_work());
}

/// A change reported while a bundle's files are being read is read again once
/// that read is answered, since the read may have found them before it.
#[test]
fn a_change_during_a_read_is_read_again_after_it() {
    let mut desk = ArtworkDesk::new(TEST_KEYS);
    let one = listed_bundle("/Users/ann/Desktop/One.app", 1);
    let _ = desk.version_of(&one.dir, one.listed, 32);
    let _ = desk.collect(&listed(&one.dir, 1), 32);
    let first = desk.next_job().expect("the first read");
    assert!(!desk.moved(&one), "a read is under way");
    assert!(!desk.has_work());
    let _ = desk.deliver_listed(&first, None, files(None, None), |_| (), &());
    assert_eq!(
        desk.next_job(),
        Some(first),
        "read again after the read under way"
    );
}

/// A desk follows the bundles asked about once its embedder follows bundles,
/// as many as the bound allows, and a sweep lets go of each no surface asked
/// about since the last.
#[test]
fn a_bundle_is_followed_while_it_is_shown_and_as_far_as_the_bound_allows() {
    let mut desk: ArtworkDesk = ArtworkDesk::new(TEST_KEYS);
    let early = listed_bundle("/Users/ann/Desktop/Early.app", 1);
    let _ = desk.version_of(&early.dir, early.listed, 32);
    assert!(
        !desk.follows(&early),
        "followed before its embedder follows bundles"
    );

    desk.follow_bundles();
    let shown: Vec<ListedBundle> = (0..=MAX_FOLLOWED_BUNDLES)
        .map(|n| listed_bundle(&alloc::format!("/Users/ann/Desktop/{n}.app"), 1))
        .collect();
    for bundle in &shown {
        let _ = desk.version_of(&bundle.dir, bundle.listed, 32);
    }
    let followed = shown.iter().filter(|bundle| desk.follows(bundle)).count();
    assert_eq!(
        followed, MAX_FOLLOWED_BUNDLES,
        "one past the bound is not followed"
    );

    assert!(
        desk.sweep_follows().is_empty(),
        "every bundle was just asked about"
    );
    let still = &shown[0];
    let _ = desk.version_of(&still.dir, still.listed, 32);
    let gone = desk.sweep_follows();
    assert_eq!(gone.len(), MAX_FOLLOWED_BUNDLES - 1);
    assert!(
        !gone.contains(still),
        "a bundle still shown keeps its follow"
    );
    assert!(desk.follows(still));
}

/// Past the bound the bundle asked about least recently is forgotten, its
/// follow let go and its files read again once it is next shown.
#[test]
fn the_bundle_asked_about_least_recently_is_forgotten_past_the_bound() {
    let mut desk: ArtworkDesk = ArtworkDesk::new(TEST_KEYS);
    desk.follow_bundles();
    let oldest = listed_bundle("/Users/ann/Desktop/Oldest.app", 1);
    let _ = desk.version_of(&oldest.dir, oldest.listed, 32);
    assert!(desk.follows(&oldest));
    for n in 1..MAX_LISTED_BUNDLE_DIRS {
        let _ = desk.version_of(&alloc::format!("/Users/ann/Desktop/{n}.app"), stamp(1), 32);
    }
    assert!(
        desk.take_unfollowed().next().is_none(),
        "nothing is forgotten within the bound"
    );
    let _ = desk.version_of("/Users/ann/Desktop/Newest.app", stamp(1), 32);
    assert_eq!(
        desk.take_unfollowed().collect::<Vec<_>>(),
        core::slice::from_ref(&oldest)
    );
    assert_eq!(desk.files_of(&oldest), None);
}

/// Past the bound the bundle forgotten is the one asked about least recently,
/// however long ago the others were first remembered: one asked about again
/// outlasts those asked about since.
#[test]
fn a_bundle_asked_about_again_outlasts_those_asked_about_since() {
    let mut desk: ArtworkDesk = ArtworkDesk::new(TEST_KEYS);
    let first = listed_bundle("/Users/ann/Desktop/First.app", 1);
    let second = listed_bundle("/Users/ann/Desktop/Second.app", 1);
    for bundle in [&first, &second] {
        let _ = desk.version_of(&bundle.dir, bundle.listed, 32);
    }
    for n in 2..MAX_LISTED_BUNDLE_DIRS {
        let _ = desk.version_of(&alloc::format!("/Users/ann/Desktop/{n}.app"), stamp(1), 32);
    }
    let _ = desk.version_of(&first.dir, first.listed, 32);
    let _ = desk.version_of("/Users/ann/Desktop/Newest.app", stamp(1), 32);
    assert!(
        desk.record_mut(&first.dir, first.listed).is_some(),
        "the bundle asked about again was forgotten"
    );
    assert!(
        desk.record_mut(&second.dir, second.listed).is_none(),
        "the bundle asked about least recently was kept"
    );
    assert_eq!(desk.bundles.len(), MAX_LISTED_BUNDLE_DIRS);
}

/// Forgetting the bundle asked about least recently allocates nothing: the
/// room for the follow it hands back was kept before it began.
#[test]
fn forgetting_the_least_recent_bundle_allocates_nothing() {
    let mut desk: ArtworkDesk = ArtworkDesk::new(TEST_KEYS);
    desk.follow_bundles();
    let oldest = listed_bundle("/Users/ann/Desktop/Oldest.app", 1);
    let _ = desk.version_of(&oldest.dir, oldest.listed, 32);
    assert!(desk.follows(&oldest));
    for n in 1..MAX_LISTED_BUNDLE_DIRS {
        let _ = desk.version_of(&alloc::format!("/Users/ann/Desktop/{n}.app"), stamp(1), 32);
    }
    assert!(tairix_util::fallible::reserve(&mut desk.unfollowed, 1));
    let ((), metering) = tairix_fuzzseed::meter::metered(|| desk.forget_least_recent());
    assert_eq!(metering.allocations, 0, "{metering:?}");
    assert_eq!(desk.take_unfollowed().collect::<Vec<_>>(), [oldest]);
}

/// A bundle read before a copy held its tree is read again once the copy is
/// done, since the copy may have written its files meanwhile.
#[test]
fn a_bundle_read_before_its_tree_was_held_is_read_again_on_release() {
    let mut desk: ArtworkDesk = ArtworkDesk::new(TEST_KEYS);
    let one = listed_bundle("/Users/ann/Desktop/One.app", 1);
    let _ = desk.version_of(&one.dir, one.listed, 32);
    let _ = desk.collect(&listed(&one.dir, 1), 32);
    let first = desk.next_job().expect("the first read");
    let _ = desk.deliver_listed(
        &first,
        Some(picture(32)),
        files(Some(2), Some(3)),
        |_| (),
        &(),
    );
    desk.hold_under(&one.dir);
    assert!(
        !desk.moved(&one),
        "nothing is read while the copy writes it"
    );
    assert!(desk.release_under(&one.dir));
    assert_eq!(
        desk.next_job(),
        Some(first),
        "read again once its writing is done"
    );
}

/// A producer reads a listed bundle's files before it decodes the bundle,
/// decodes nothing where they are as its picture was last read from, and
/// makes any other decode without reading a bundle's files at all.
#[test]
fn a_producer_decodes_a_listed_bundle_only_where_its_files_moved() {
    let (mut reader, mut rasteriser) = (crate::NoArtworkSeam, crate::NoArtworkSeam);
    let read = files(Some(2), Some(3));
    let one = ArtworkJob {
        key: listed("/Users/ann/Desktop/One.app", 1),
        side: 32,
    };
    let produced = one.produce((&mut reader, &mut rasteriser), |_, _| read, |_| Some(read));
    assert!(matches!(produced, Produced::Unchanged), "{produced:?}");
    let produced = one.produce((&mut reader, &mut rasteriser), |_, _| read, |_| None);
    assert!(
        matches!(produced, Produced::Listed(found, None) if found == read),
        "{produced:?}"
    );

    let mut read_files = false;
    let produced = job("/a.png", 32).produce(
        (&mut reader, &mut rasteriser),
        |_, _| {
            read_files = true;
            read
        },
        |_| None,
    );
    assert!(matches!(produced, Produced::Decoded(None)), "{produced:?}");
    assert!(!read_files, "an asset's decode read a bundle's files");
}

/// What a producer made is recorded as its kind is delivered: a listed
/// bundle's picture under the files it was read from, files that stood as
/// nothing landing, and any other decode as itself.
#[test]
fn a_produced_decode_is_recorded_by_what_its_producer_read() {
    let mut desk = ArtworkDesk::new(TEST_KEYS);
    let one = listed_bundle("/Users/ann/Desktop/One.app", 1);
    let _ = desk.version_of(&one.dir, one.listed, 32);
    let _ = desk.collect(&listed(&one.dir, 1), 32);
    let first = desk.next_job().expect("the first read");
    let read = files(Some(2), Some(3));
    let produced = Produced::Listed(read, Some(picture(32)));
    assert!(desk.deliver_produced(&first, produced, |_| (), &()).kept());
    assert_eq!(
        desk.version_of(&one.dir, one.listed, 32),
        BundleVersion::Files(read)
    );
    let _ = desk.take_landed();

    assert!(desk.moved(&one));
    let again = desk.next_job().expect("the read again");
    let delivered = desk.deliver_produced(&again, Produced::Unchanged, |_| (), &());
    assert!(!delivered.kept());
    assert!(
        desk.take_landed().is_empty(),
        "files that stood repainted something"
    );

    let _ = desk.collect(&asset("/a.png"), 32);
    let plain = desk.next_job().expect("the asset");
    let _ = desk.deliver_produced(&plain, Produced::Decoded(None), |_| (), &());
    assert!(desk.take_landed().resolves(request("/a.png"), 32));
}

/// A bundle unfollowed — let go by a sweep while off screen, or remembered
/// before its embedder followed bundles — is followed again once asked about
/// with room: its files are read anew, so that read arms the follow.
#[test]
fn a_bundle_shown_again_is_followed_again() {
    let mut desk: ArtworkDesk = ArtworkDesk::new(TEST_KEYS);
    desk.follow_bundles();
    let one = listed_bundle("/Users/ann/Desktop/One.app", 1);
    let _ = desk.version_of(&one.dir, one.listed, 32);
    let _ = desk.collect(&listed(&one.dir, 1), 32);
    let first = desk.next_job().expect("the first read");
    assert!(desk.follows(&one), "its first read arms its follow");
    let read = files(Some(2), Some(3));
    let _ = desk.deliver_listed(&first, Some(picture(32)), read, |_| (), &());
    assert!(desk.sweep_follows().is_empty(), "it was just asked about");
    assert_eq!(
        desk.sweep_follows(),
        core::slice::from_ref(&one),
        "the bundle that left the screen kept its follow"
    );
    let recorded = desk.recorded();
    assert_eq!(
        desk.version_of(&one.dir, one.listed, 32),
        BundleVersion::Files(read),
        "shown again, it is pictured from the files it was read from"
    );
    assert_ne!(desk.recorded(), recorded, "nothing read it anew");
    assert_eq!(desk.next_job(), Some(first), "its files are read anew");
    assert!(desk.follows(&one), "that read does not arm its follow");

    let mut desk: ArtworkDesk = ArtworkDesk::new(TEST_KEYS);
    let early = listed_bundle("/Users/ann/Desktop/Early.app", 1);
    let _ = desk.version_of(&early.dir, early.listed, 32);
    desk.follow_bundles();
    let _ = desk.version_of(&early.dir, early.listed, 32);
    assert!(
        desk.follows(&early),
        "remembered before its embedder followed bundles, it is never followed"
    );
}

/// A bundle past the bound is followed once a sweep makes room and it is next
/// asked about.
#[test]
fn a_bundle_past_the_bound_is_followed_once_room_frees() {
    let mut desk: ArtworkDesk = ArtworkDesk::new(TEST_KEYS);
    desk.follow_bundles();
    let shown: Vec<ListedBundle> = (0..=MAX_FOLLOWED_BUNDLES)
        .map(|n| listed_bundle(&alloc::format!("/Users/ann/Desktop/{n}.app"), 1))
        .collect();
    for bundle in &shown {
        let _ = desk.version_of(&bundle.dir, bundle.listed, 32);
    }
    let past = &shown[MAX_FOLLOWED_BUNDLES];
    assert!(
        desk.sweep_follows().is_empty(),
        "every bundle was just asked about"
    );
    let _ = desk.version_of(&past.dir, past.listed, 32);
    assert!(!desk.follows(past), "followed past the bound");
    assert!(
        desk.sweep_follows().is_empty(),
        "a bundle only wanted followed was handed back"
    );
    let _ = desk.version_of(&past.dir, past.listed, 32);
    assert!(desk.follows(past), "not followed once room freed");
}

/// A bundle whose follow was refused is not followed again: its next read
/// would only be refused again.
#[test]
fn a_bundle_whose_follow_was_refused_is_not_followed_again() {
    let mut desk: ArtworkDesk = ArtworkDesk::new(TEST_KEYS);
    desk.follow_bundles();
    let one = listed_bundle("/Users/ann/Desktop/One.app", 1);
    let _ = desk.version_of(&one.dir, one.listed, 32);
    assert!(desk.follows(&one));
    desk.unfollowable(&one);
    let recorded = desk.recorded();
    let _ = desk.version_of(&one.dir, one.listed, 32);
    assert_eq!(
        desk.recorded(),
        recorded,
        "its files were read anew to be refused again"
    );
    assert!(!desk.follows(&one));
}

/// The bundles on a volume about to be ejected are followed no more, so a
/// read of one under way arms no follow that stands; each is followed again
/// once asked about.
#[test]
fn the_bundles_on_a_volume_ejected_are_followed_no_more() {
    let mut desk: ArtworkDesk = ArtworkDesk::new(TEST_KEYS);
    desk.follow_bundles();
    let ejected = listed_bundle("/Storage/usb1/Games.app", 1);
    let kept = listed_bundle("/Users/ann/Desktop/One.app", 1);
    for bundle in [&ejected, &kept] {
        let _ = desk.version_of(&bundle.dir, bundle.listed, 32);
        assert!(desk.follows(bundle));
    }
    desk.stop_following_where(|dir| dir.starts_with("/Storage/usb1/"));
    assert!(
        !desk.follows(&ejected),
        "a bundle on the volume is still followed"
    );
    assert!(desk.follows(&kept));
    assert!(
        desk.sweep_follows().is_empty(),
        "a bundle let go was handed back"
    );
    let _ = desk.version_of(&ejected.dir, ejected.listed, 32);
    assert!(
        desk.follows(&ejected),
        "not followed again once asked about"
    );
}

/// A directory's versions are remembered and forgotten together: asking
/// about one keeps every version of it, and past the bound the directory
/// asked about least recently goes with every version, each follow handed
/// back.
#[test]
fn a_directory_s_versions_are_remembered_and_forgotten_together() {
    let mut desk: ArtworkDesk = ArtworkDesk::new(TEST_KEYS);
    desk.follow_bundles();
    let old = listed_bundle("/Users/ann/Desktop/One.app", 1);
    let new = listed_bundle("/Users/ann/Desktop/One.app", 2);
    for version in [&old, &new] {
        let _ = desk.version_of(&version.dir, version.listed, 32);
        assert!(desk.follows(version));
    }
    for n in 1..MAX_LISTED_BUNDLE_DIRS {
        let _ = desk.version_of(&alloc::format!("/Users/ann/Desktop/{n}.app"), stamp(1), 32);
    }
    let _ = desk.version_of(&new.dir, new.listed, 32);
    let _ = desk.version_of("/Users/ann/Desktop/Newest.app", stamp(1), 32);
    assert!(
        desk.record_mut(&old.dir, old.listed).is_some(),
        "asking about one version forgot another"
    );
    assert!(desk
        .record_mut("/Users/ann/Desktop/1.app", stamp(1))
        .is_none());
    assert!(desk.take_unfollowed().next().is_none());
    for n in 0..MAX_LISTED_BUNDLE_DIRS {
        let _ = desk.version_of(&alloc::format!("/Users/ann/Later/{n}.app"), stamp(1), 32);
    }
    let gone: Vec<ListedBundle> = desk.take_unfollowed().collect();
    assert!(gone.contains(&old) && gone.contains(&new), "{gone:?}");
    assert_eq!(desk.files_of(&old), None);
}

/// A directory listed at more versions than the bound forgets the version
/// asked about least recently, handing its follow back.
#[test]
fn a_directory_past_its_versions_forgets_the_one_asked_about_least_recently() {
    let mut desk: ArtworkDesk = ArtworkDesk::new(TEST_KEYS);
    desk.follow_bundles();
    let dir = "/Users/ann/Desktop/One.app";
    let versions: Vec<ListedBundle> = (1..)
        .take(MAX_LISTED_VERSIONS)
        .map(|n| listed_bundle(dir, n))
        .collect();
    for version in &versions {
        let _ = desk.version_of(&version.dir, version.listed, 32);
        assert!(desk.follows(version));
    }
    let _ = desk.version_of(dir, versions[0].listed, 32);
    let past = listed_bundle(dir, 100);
    let _ = desk.version_of(&past.dir, past.listed, 32);
    assert_eq!(
        desk.take_unfollowed().collect::<Vec<_>>(),
        core::slice::from_ref(&versions[1])
    );
    assert!(desk.record_mut(dir, versions[0].listed).is_some());
    assert!(desk.record_mut(dir, past.listed).is_some());
    assert!(desk.record_mut(dir, versions[1].listed).is_none());
}

/// Asks, a collect and a want under an exhausted heap answer without
/// aborting and record nothing the heap refused, and the desk stays whole: a
/// bundle refused is remembered once the heap has room, and one remembered
/// still answers the files it was read from.
#[test]
fn a_refused_heap_answers_every_ask_and_keeps_the_desk_whole() {
    use tairix_fuzzseed::meter::{refusing_above, refusing_after};
    let mut desk: ArtworkDesk = ArtworkDesk::new(TEST_KEYS);
    desk.follow_bundles();
    let known = listed_bundle("/Users/ann/Desktop/Known.app", 1);
    let _ = desk.version_of(&known.dir, known.listed, 32);
    let _ = desk.collect(&listed(&known.dir, 1), 32);
    let first = desk.next_job().expect("the first read");
    let read = files(Some(2), Some(3));
    let _ = desk.deliver_listed(&first, Some(picture(32)), read, |_| (), &());
    let _ = desk.take_landed();
    let fresh = listed_bundle("/Users/ann/Desktop/Fresh.app", 1);
    let (collected, wanted) = (asset("/a.png"), asset("/b.png"));
    refusing_above(0, || {
        assert_eq!(
            desk.version_of(&known.dir, known.listed, 32),
            BundleVersion::Files(read)
        );
        assert_eq!(
            desk.version_of(&fresh.dir, fresh.listed, 32),
            BundleVersion::Listed(fresh.listed)
        );
        assert!(matches!(
            desk.collect(&collected, 32),
            (Resolved::Pending, false)
        ));
        assert!(!desk.want(&wanted, 32));
    });
    assert!(!desk.has_work(), "a refused ask queued a decode");
    assert!(desk.record_mut(&fresh.dir, fresh.listed).is_none());
    let _ = desk.version_of(&fresh.dir, fresh.listed, 32);
    assert!(desk.record_mut(&fresh.dir, fresh.listed).is_some());
    for granted in 0..8 {
        let mut desk: ArtworkDesk = ArtworkDesk::new(TEST_KEYS);
        let (_, recorded) = refusing_after(granted, || desk.collect(&collected, 32));
        assert_eq!(desk.has_work(), recorded, "{granted}");
        assert_eq!(desk.collect(&collected, 32).1, !recorded, "{granted}");
        assert!(desk.next_job().is_some(), "{granted}");
        assert!(desk.next_job().is_none(), "recorded twice: {granted}");
    }
    for granted in 0..8 {
        let mut desk: ArtworkDesk = ArtworkDesk::new(TEST_KEYS);
        desk.follow_bundles();
        let answer = refusing_after(granted, || desk.version_of(&known.dir, known.listed, 32));
        assert_eq!(answer, BundleVersion::Listed(known.listed), "{granted}");
        let _ = desk.version_of(&known.dir, known.listed, 32);
        assert!(
            desk.record_mut(&known.dir, known.listed).is_some(),
            "{granted}"
        );
        assert!(desk.follows(&known), "{granted}");
        assert_eq!(desk.following.len(), 1, "{granted}");
    }
}

/// A listed bundle's answer the heap has no room to name lands as everything
/// having landed, so the item drawn from it repaints and asks again rather
/// than keeping what it showed until unrelated input.
#[test]
fn a_listed_answer_the_heap_cannot_name_repaints_everything() {
    let mut desk = ArtworkDesk::new(TEST_KEYS);
    let one = listed_bundle("/Users/ann/Desktop/One.app", 1);
    let _ = desk.version_of(&one.dir, one.listed, 32);
    let _ = desk.collect(&listed(&one.dir, 1), 32);
    let first = desk.next_job().expect("the first read");
    let (read, art) = (files(Some(2), Some(3)), Some(picture(32)));
    let delivered = tairix_fuzzseed::meter::refusing_above(0, || {
        desk.deliver_listed(&first, art, read, |_| (), &())
    });
    assert!(delivered.wake(), "the loop is not woken to repaint");
    let request = IconRequest::listed_bundle(crate::IconKind::AppBundle, &one.dir, one.listed);
    assert!(desk.take_landed().resolves(request, 32));
    assert!(
        desk.take_landed().is_empty(),
        "the landing was reported twice"
    );
    assert_eq!(
        desk.version_of(&one.dir, one.listed, 32),
        BundleVersion::Files(read)
    );
}
