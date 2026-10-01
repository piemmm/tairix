//! The conformance suite every family passes against its register model.
//!
//! The suite drives the unit only through [`IommuUnit`] and asks the model,
//! through [`TranslationProbe`], what the modelled hardware would do with a
//! device's access — including hits on a translation it still caches. A unit
//! that forgets an invalidation, or blocks a stream without confirming it,
//! fails here.

use alloc::vec::Vec;

use crate::domain::Domain;
use crate::{Access, Fault, IommuUnit, IO_PAGE_SIZE};

/// What the modelled hardware would do with one access.
pub trait TranslationProbe {
    /// Issue `stream`'s access to `iova` as a device would: the physical
    /// address it reaches, or [`None`] when the unit refuses it and records
    /// the fault.
    fn access(&self, stream: u32, iova: u64, write: bool) -> Option<u64>;
}

/// What the suite may use: two streams the unit covers and has blocked, and
/// two pages of memory to map.
#[derive(Copy, Clone, Debug)]
pub struct Fixture {
    /// Streams the unit covers, both blocked when the suite starts.
    pub streams: [u32; 2],
    /// Page-aligned physical pages to map.
    pub pages: [u64; 2],
}

/// Run every check. Each panics naming what the unit got wrong.
pub fn run_all(unit: &dyn IommuUnit, probe: &dyn TranslationProbe, fixture: &Fixture) {
    a_blocked_stream_reaches_nothing(unit, probe, fixture);
    an_attached_stream_reaches_what_its_domain_maps(unit, probe, fixture);
    a_synced_unmap_leaves_no_cached_translation(unit, probe, fixture);
    a_block_ends_every_translation_at_once(unit, probe, fixture);
    a_refused_access_is_reported_against_its_stream(unit, probe, fixture);
    two_streams_reach_only_their_own_domains(unit, probe, fixture);
    a_destroyed_domain_reaches_nothing(unit, probe, fixture);
    a_silenced_stream_reaches_nothing_and_raises_nothing(unit, probe, fixture);
    drain(unit);
}

const IOVA: u64 = 0x4000_0000;

/// Drain until the unit says nothing remains. With no access arriving, each
/// call that answers `true` has taken a record, so no check needs more calls
/// than it provoked faults.
fn drain(unit: &dyn IommuUnit) -> Vec<Fault> {
    let mut faults = Vec::new();
    for _ in 0..DRAIN_CALLS {
        if !unit.drain_faults(&mut |fault| faults.push(fault)) {
            return faults;
        }
    }
    panic!("the unit reported records remaining that no drain took");
}

/// More drain calls than any check provokes faults.
const DRAIN_CALLS: usize = 64;

fn a_blocked_stream_reaches_nothing(
    unit: &dyn IommuUnit,
    probe: &dyn TranslationProbe,
    fixture: &Fixture,
) {
    for stream in fixture.streams {
        assert_eq!(
            probe.access(stream, IOVA, false),
            None,
            "a blocked stream's access translated"
        );
    }
    drain(unit);
}

fn an_attached_stream_reaches_what_its_domain_maps(
    unit: &dyn IommuUnit,
    probe: &dyn TranslationProbe,
    fixture: &Fixture,
) {
    let [stream, _] = fixture.streams;
    let domain = unit.create_domain().expect("create a domain");
    unit.map(
        domain,
        IOVA,
        fixture.pages[0],
        IO_PAGE_SIZE,
        Access::READ_WRITE,
    )
    .expect("map one page");
    unit.attach(stream, domain).expect("attach");
    assert_eq!(
        probe.access(stream, IOVA + 0x10, true),
        Some(fixture.pages[0] + 0x10),
        "an attached stream did not reach its mapping"
    );
    assert_eq!(
        probe.access(stream, IOVA + IO_PAGE_SIZE, true),
        None,
        "an attached stream reached past its mapping"
    );
    unit.block(stream).expect("block");
    unit.destroy_domain(domain).expect("destroy");
    drain(unit);
}

fn a_synced_unmap_leaves_no_cached_translation(
    unit: &dyn IommuUnit,
    probe: &dyn TranslationProbe,
    fixture: &Fixture,
) {
    let [stream, _] = fixture.streams;
    let domain = unit.create_domain().expect("create a domain");
    unit.attach(stream, domain).expect("attach");
    unit.map(
        domain,
        IOVA,
        fixture.pages[0],
        IO_PAGE_SIZE,
        Access::READ_WRITE,
    )
    .expect("map");
    // Warm the unit's cache before taking the mapping away.
    assert!(probe.access(stream, IOVA, false).is_some());
    unit.unmap(domain, IOVA, IO_PAGE_SIZE).expect("unmap");
    unit.sync(domain).expect("sync");
    assert_eq!(
        probe.access(stream, IOVA, false),
        None,
        "a translation survived a confirmed sync"
    );
    unit.block(stream).expect("block");
    unit.destroy_domain(domain).expect("destroy");
    drain(unit);
}

fn a_block_ends_every_translation_at_once(
    unit: &dyn IommuUnit,
    probe: &dyn TranslationProbe,
    fixture: &Fixture,
) {
    let [stream, _] = fixture.streams;
    let domain = unit.create_domain().expect("create a domain");
    unit.map(
        domain,
        IOVA,
        fixture.pages[0],
        IO_PAGE_SIZE,
        Access::READ_WRITE,
    )
    .expect("map");
    unit.attach(stream, domain).expect("attach");
    assert!(probe.access(stream, IOVA, true).is_some());
    unit.block(stream).expect("block");
    assert_eq!(
        probe.access(stream, IOVA, true),
        None,
        "a blocked stream still reached a mapping it had cached"
    );
    unit.destroy_domain(domain).expect("destroy");
    drain(unit);
}

fn a_refused_access_is_reported_against_its_stream(
    unit: &dyn IommuUnit,
    probe: &dyn TranslationProbe,
    fixture: &Fixture,
) {
    let [stream, _] = fixture.streams;
    drain(unit);
    let domain = unit.create_domain().expect("create a domain");
    unit.attach(stream, domain).expect("attach");
    assert_eq!(probe.access(stream, IOVA + 0x123, true), None);
    let faults = drain(unit);
    assert!(
        faults
            .iter()
            .any(|f| f.stream == stream && f.iova == IOVA && f.write),
        "the refused write was not reported against its stream: {faults:?}"
    );
    assert!(drain(unit).is_empty(), "a drained fault was reported again");
    unit.block(stream).expect("block");
    unit.destroy_domain(domain).expect("destroy");
    drain(unit);
}

fn two_streams_reach_only_their_own_domains(
    unit: &dyn IommuUnit,
    probe: &dyn TranslationProbe,
    fixture: &Fixture,
) {
    let [first, second] = fixture.streams;
    let mut one = Domain::new(unit, &[]).expect("create the first domain");
    let mut two = Domain::new(unit, &[]).expect("create the second domain");
    one.attach(first).expect("attach the first stream");
    two.attach(second).expect("attach the second stream");
    let at_one = one.map(fixture.pages[0], 0, 0).expect("map into the first");
    let at_two = two
        .map(fixture.pages[1], 0, 0)
        .expect("map into the second");
    assert_eq!(probe.access(first, at_one, true), Some(fixture.pages[0]));
    assert_eq!(probe.access(second, at_two, true), Some(fixture.pages[1]));
    if at_one != at_two {
        assert_eq!(
            probe.access(first, at_two, true),
            None,
            "a stream reached another domain's mapping"
        );
    }
    one.destroy().expect("destroy the first");
    two.destroy().expect("destroy the second");
    drain(unit);
}

fn a_silenced_stream_reaches_nothing_and_raises_nothing(
    unit: &dyn IommuUnit,
    probe: &dyn TranslationProbe,
    fixture: &Fixture,
) {
    let [stream, _] = fixture.streams;
    let domain = unit.create_domain().expect("create a domain");
    unit.map(
        domain,
        IOVA,
        fixture.pages[0],
        IO_PAGE_SIZE,
        Access::READ_WRITE,
    )
    .expect("map");
    unit.attach(stream, domain).expect("attach");
    assert!(probe.access(stream, IOVA, true).is_some());
    drain(unit);
    unit.silence(stream).expect("silence");
    assert_eq!(
        probe.access(stream, IOVA, true),
        None,
        "a silenced stream still reached its old mapping"
    );
    assert!(
        drain(unit).is_empty(),
        "a silenced stream's fault was recorded"
    );
    unit.attach(stream, domain)
        .expect("a silenced stream takes an owner again");
    assert_eq!(probe.access(stream, IOVA, true), Some(fixture.pages[0]));
    unit.block(stream).expect("block");
    unit.destroy_domain(domain).expect("destroy");
    drain(unit);
}

fn a_destroyed_domain_reaches_nothing(
    unit: &dyn IommuUnit,
    probe: &dyn TranslationProbe,
    fixture: &Fixture,
) {
    let [stream, _] = fixture.streams;
    let mut domain = Domain::new(unit, &[]).expect("create a domain");
    domain.attach(stream).expect("attach");
    let iova = domain.map(fixture.pages[0], 0, 0).expect("map");
    assert!(probe.access(stream, iova, false).is_some());
    domain.destroy().expect("destroy");
    assert_eq!(
        probe.access(stream, iova, false),
        None,
        "a destroyed domain's mapping was still reachable"
    );
    drain(unit);
}
