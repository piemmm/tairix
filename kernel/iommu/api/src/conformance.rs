//! The conformance suite every family passes against its register model.
//!
//! The suite drives the unit only through [`IommuUnit`] and asks the model,
//! through [`TranslationProbe`], what the modelled hardware would do with a
//! device's access — including hits on a translation it still caches. A unit
//! that forgets an invalidation, or blocks a stream without confirming it,
//! fails here.

use alloc::vec::Vec;

use crate::domain::Domain;
use crate::{
    Access, Fault, InterruptRemapping, InterruptSource, InterruptTarget, IommuUnit, IO_PAGE_SIZE,
};

/// What the modelled hardware would do with one access.
pub trait TranslationProbe {
    /// Issue `stream`'s access to `iova` as a device would: the physical
    /// address it reaches, or [`None`] when the unit refuses it and records
    /// the fault.
    fn access(&self, stream: u32, iova: u64, write: bool) -> Option<u64>;

    /// Issue `stream`'s access to `address` as a device with ATS would,
    /// marked already translated: the physical address it reaches, or
    /// [`None`] when the unit refuses it and records the fault.
    fn translated(&self, stream: u32, address: u64, write: bool) -> Option<u64>;

    /// Whether a request can reach the unit marked already translated at
    /// all. A virtio-iommu's transport carries no such mark: every request
    /// is translated, so there is nothing to refuse.
    fn carries_translated(&self) -> bool {
        true
    }
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
    a_range_synced_unmap_keeps_its_neighbour(unit, probe, fixture);
    a_map_of_many_runs_lands_and_unmaps_whole(unit, probe, fixture);
    a_block_ends_every_translation_at_once(unit, probe, fixture);
    a_refused_access_is_reported_against_its_stream(unit, probe, fixture);
    two_streams_reach_only_their_own_domains(unit, probe, fixture);
    a_stream_attached_again_to_its_domain_stays_and_elsewhere_is_busy(unit, probe, fixture);
    a_destroyed_domain_reaches_nothing(unit, probe, fixture);
    a_silenced_stream_reaches_nothing_and_raises_nothing(unit, probe, fixture);
    a_translated_request_is_refused_and_reported(unit, probe, fixture);
    what_a_unit_claims_for_a_stream_is_a_range_a_domain_keeps_clear(unit, probe, fixture);
    a_write_only_mapping_is_made_exactly_where_the_unit_says_it_can_be(unit, probe, fixture);
    drain(unit);
}

/// A unit maps a write it may not read wherever its profile says its tables
/// express one, and the device then writes there and reads nothing; anywhere
/// else the map is refused and leaves nothing mapped. The kernel picks the
/// access it asks a doorbell for by the profile, so a profile that lies
/// either strands a device's messages or refuses its domain outright.
fn a_write_only_mapping_is_made_exactly_where_the_unit_says_it_can_be(
    unit: &dyn IommuUnit,
    probe: &dyn TranslationProbe,
    fixture: &Fixture,
) {
    let [stream, _] = fixture.streams;
    let domain = unit.create_domain().expect("create a domain");
    let mapped = unit.map(domain, IOVA, fixture.pages[0], IO_PAGE_SIZE, Access::WRITE);
    assert_eq!(
        mapped.is_ok(),
        unit.profile().write_only,
        "a write-only map disagreed with the profile: {mapped:?}"
    );
    unit.attach(stream, domain).expect("attach");
    let written = probe.access(stream, IOVA, true);
    if mapped.is_ok() {
        assert_eq!(
            written,
            Some(fixture.pages[0]),
            "a write-only page refused a write"
        );
        assert_eq!(
            probe.access(stream, IOVA, false),
            None,
            "a write-only page was read"
        );
    } else {
        assert_eq!(written, None, "a refused map left the page reachable");
    }
    unit.block(stream).expect("block");
    unit.destroy_domain(domain).expect("destroy");
    drain(unit);
}

/// Every IOVA range a unit claims for a stream is page-aligned and holds a
/// page, so a domain can be made around it, and a domain made around them
/// hands out no IOVA inside one.
fn what_a_unit_claims_for_a_stream_is_a_range_a_domain_keeps_clear(
    unit: &dyn IommuUnit,
    probe: &dyn TranslationProbe,
    fixture: &Fixture,
) {
    let [stream, _] = fixture.streams;
    let mut claimed = Vec::new();
    unit.reserved_iova(stream, &mut |range| claimed.push(range))
        .expect("the unit names what it claims for a covered stream");
    for range in &claimed {
        assert!(
            range.start < range.end
                && range.start.is_multiple_of(IO_PAGE_SIZE)
                && range.end.is_multiple_of(IO_PAGE_SIZE),
            "a claimed range is no page-aligned range: {range:#x?}"
        );
    }
    let mut domain = Domain::new(unit, &[], &claimed).expect("a domain clear of the claims");
    domain.attach(stream).expect("attach");
    let iova = domain
        .map(
            &[crate::FrameRun {
                phys: fixture.pages[0],
                order: 0,
            }],
            0,
        )
        .expect("map");
    assert!(
        !claimed
            .iter()
            .any(|range| range.start < iova + IO_PAGE_SIZE && iova < range.end),
        "a domain handed out a claimed IOVA"
    );
    assert_eq!(probe.access(stream, iova, false), Some(fixture.pages[0]));
    domain.destroy().expect("destroy");
    drain(unit);
}

const IOVA: u64 = 0x4000_0000;

/// Attaching a stream to the domain it is attached to changes nothing;
/// attaching it to another is refused, and leaves it where it was.
fn a_stream_attached_again_to_its_domain_stays_and_elsewhere_is_busy(
    unit: &dyn IommuUnit,
    probe: &dyn TranslationProbe,
    fixture: &Fixture,
) {
    let [stream, _] = fixture.streams;
    let held = unit.create_domain().expect("create a domain");
    let other = unit.create_domain().expect("create a second domain");
    unit.map(
        held,
        IOVA,
        fixture.pages[0],
        IO_PAGE_SIZE,
        Access::READ_WRITE,
    )
    .expect("map");
    unit.attach(stream, held).expect("attach");
    unit.attach(stream, held)
        .expect("attaching a stream to its own domain again");
    assert_eq!(
        unit.attach(stream, other),
        Err(crate::IommuError::StreamBusy),
        "a stream attached elsewhere"
    );
    assert_eq!(
        probe.access(stream, IOVA, true),
        Some(fixture.pages[0]),
        "the stream left its domain"
    );
    unit.block(stream).expect("block");
    assert_eq!(
        unit.destroy_domain(held),
        Ok(()),
        "a second attach is not a second hold"
    );
    unit.destroy_domain(other).expect("destroy");
    drain(unit);
}

/// A device presenting an address as already translated reaches nothing,
/// even one its domain maps, and the refusal names its stream: no device is
/// trusted to cache translations.
fn a_translated_request_is_refused_and_reported(
    unit: &dyn IommuUnit,
    probe: &dyn TranslationProbe,
    fixture: &Fixture,
) {
    if !probe.carries_translated() {
        return;
    }
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
    for address in [fixture.pages[0], IOVA] {
        assert_eq!(
            probe.translated(stream, address, true),
            None,
            "a translated request reached memory"
        );
    }
    let faults = drain(unit);
    assert!(
        faults
            .iter()
            .any(|fault| fault.stream == stream && fault.reason == crate::FaultReason::Translated),
        "the refusal was not reported as a translated request"
    );
    assert!(
        probe.access(stream, IOVA, true).is_some(),
        "untranslated still reaches"
    );
    unit.block(stream).expect("block");
    unit.destroy_domain(domain).expect("destroy");
    drain(unit);
}

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

/// A range sync confirms what an unmap took inside the range, and the
/// mapping beside it is still reached and still unmapped whole.
fn a_range_synced_unmap_keeps_its_neighbour(
    unit: &dyn IommuUnit,
    probe: &dyn TranslationProbe,
    fixture: &Fixture,
) {
    let [stream, _] = fixture.streams;
    let domain = unit.create_domain().expect("create a domain");
    unit.attach(stream, domain).expect("attach");
    for (at, page) in [
        (IOVA, fixture.pages[0]),
        (IOVA + IO_PAGE_SIZE, fixture.pages[1]),
    ] {
        unit.map(domain, at, page, IO_PAGE_SIZE, Access::READ_WRITE)
            .expect("map");
        assert!(probe.access(stream, at, false).is_some());
    }
    unit.unmap(domain, IOVA, IO_PAGE_SIZE)
        .expect("unmap the first");
    unit.sync_range(domain, IOVA, IO_PAGE_SIZE)
        .expect("sync the first");
    assert_eq!(
        probe.access(stream, IOVA, false),
        None,
        "a translation survived a confirmed range sync"
    );
    assert_eq!(
        probe.access(stream, IOVA + IO_PAGE_SIZE, false),
        Some(fixture.pages[1]),
        "a range sync took the mapping beside it"
    );
    unit.unmap(domain, IOVA + IO_PAGE_SIZE, IO_PAGE_SIZE)
        .expect("the neighbour still unmaps whole");
    unit.sync_range(domain, IOVA + IO_PAGE_SIZE, IO_PAGE_SIZE)
        .expect("sync the second");
    assert_eq!(probe.access(stream, IOVA + IO_PAGE_SIZE, false), None);
    unit.block(stream).expect("block");
    unit.destroy_domain(domain).expect("destroy");
    drain(unit);
}

/// The runs of one map land back to back, and the map unmaps whole.
fn a_map_of_many_runs_lands_and_unmaps_whole(
    unit: &dyn IommuUnit,
    probe: &dyn TranslationProbe,
    fixture: &Fixture,
) {
    let [stream, _] = fixture.streams;
    let domain = unit.create_domain().expect("create a domain");
    unit.attach(stream, domain).expect("attach");
    let runs = fixture.pages.map(|phys| crate::FrameRun { phys, order: 0 });
    unit.map_runs(domain, IOVA, &runs, Access::READ_WRITE)
        .expect("map the runs");
    for (at, page) in [
        (IOVA, fixture.pages[0]),
        (IOVA + IO_PAGE_SIZE, fixture.pages[1]),
    ] {
        assert_eq!(
            probe.access(stream, at + 0x10, true),
            Some(page + 0x10),
            "a run did not land where it was placed"
        );
    }
    unit.unmap(domain, IOVA, 2 * IO_PAGE_SIZE)
        .expect("unmap the map whole");
    unit.sync_range(domain, IOVA, 2 * IO_PAGE_SIZE)
        .expect("sync");
    assert_eq!(probe.access(stream, IOVA, false), None);
    assert_eq!(probe.access(stream, IOVA + IO_PAGE_SIZE, false), None);
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
    let mut one = Domain::new(unit, &[], &[]).expect("create the first domain");
    let mut two = Domain::new(unit, &[], &[]).expect("create the second domain");
    one.attach(first).expect("attach the first stream");
    two.attach(second).expect("attach the second stream");
    let at_one = one
        .map(
            &[crate::FrameRun {
                phys: fixture.pages[0],
                order: 0,
            }],
            0,
        )
        .expect("map into the first");
    let at_two = two
        .map(
            &[crate::FrameRun {
                phys: fixture.pages[1],
                order: 0,
            }],
            0,
        )
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
    let mut domain = Domain::new(unit, &[], &[]).expect("create a domain");
    domain.attach(stream).expect("attach");
    let iova = domain
        .map(
            &[crate::FrameRun {
                phys: fixture.pages[0],
                order: 0,
            }],
            0,
        )
        .expect("map");
    assert!(probe.access(stream, iova, false).is_some());
    domain.destroy().expect("destroy");
    assert_eq!(
        probe.access(stream, iova, false),
        None,
        "a destroyed domain's mapping was still reachable"
    );
    drain(unit);
}

/// What the modelled hardware would do with one interrupt message.
pub trait InterruptProbe {
    /// Raise the MSI `address`/`data` as requester id `source` would: the
    /// interrupt the unit delivers, or [`None`] when it refuses it and
    /// records the fault.
    fn interrupt(&self, source: u16, address: u64, data: u32) -> Option<InterruptTarget>;
}

/// A compatibility-format MSI to APIC id 0: what a device that ignores
/// remapping writes.
const COMPATIBILITY: (u64, u32) = (0xFEE0_0000, 0x41);

/// Run every interrupt remapping check against a unit translating nothing
/// yet, its remapping not prepared, with `sources` two requester ids on bus
/// `sources[0] >> 8`, `sources[1]` on another bus, leaving remapping off.
/// Each panics naming what the unit got wrong.
pub fn run_interrupts(unit: &dyn IommuUnit, probe: &dyn InterruptProbe, sources: [u16; 2]) {
    let remapping = unit
        .interrupt_remapping()
        .expect("a unit under the interrupt suite remaps");
    let [own, other] = sources;
    assert!(
        probe
            .interrupt(own, COMPATIBILITY.0, COMPATIBILITY.1)
            .is_some(),
        "compatibility interrupts pass until remapping is on"
    );
    remapping.prepare_remapping(false, 1).expect("prepare");
    assert!(
        remapping.prepare_remapping(false, 1).is_err(),
        "a table is prepared once"
    );
    let target = InterruptTarget {
        vector: 0x51,
        destination: 3,
        level: false,
    };
    let entry = remapping
        .remap_interrupt(InterruptSource::Requester(own), target)
        .expect("remap");
    assert!(
        probe
            .interrupt(own, COMPATIBILITY.0, COMPATIBILITY.1)
            .is_some(),
        "nothing is refused before remapping is enabled"
    );
    remapping.enable_remapping().expect("enable");
    drain(unit);
    assert_eq!(
        probe.interrupt(own, entry.address, entry.data),
        Some(target),
        "an entry delivers its target to its source"
    );
    assert_eq!(
        probe.interrupt(other, entry.address, entry.data),
        None,
        "another source raised an entry it was not given"
    );
    assert_eq!(
        probe.interrupt(own, COMPATIBILITY.0, COMPATIBILITY.1),
        None,
        "a compatibility-format interrupt passed with remapping on"
    );
    let refused = drain(unit);
    assert!(
        refused
            .iter()
            .filter(|fault| fault.reason == crate::FaultReason::Interrupt)
            .count()
            >= 2,
        "refused interrupts were not reported: {refused:?}"
    );
    a_bridge_s_buses_raise_its_entry_and_no_others(unit, remapping, probe, sources);
    a_released_entry_raises_nothing_even_where_cached(unit, remapping, probe, own, entry);
    an_entry_names_the_trigger_mode_and_destination_it_was_given(remapping, probe, own);
    remapping.disable_remapping().expect("disable");
    assert!(
        probe
            .interrupt(own, COMPATIBILITY.0, COMPATIBILITY.1)
            .is_some(),
        "compatibility interrupts pass once remapping is off again"
    );
    drain(unit);
}

fn a_bridge_s_buses_raise_its_entry_and_no_others(
    unit: &dyn IommuUnit,
    remapping: &dyn InterruptRemapping,
    probe: &dyn InterruptProbe,
    [own, other]: [u16; 2],
) {
    let bus = own.to_be_bytes()[0];
    let target = InterruptTarget {
        vector: 0x52,
        destination: 1,
        level: false,
    };
    let entry = remapping
        .remap_interrupt(
            InterruptSource::Buses {
                first: bus,
                last: bus,
            },
            target,
        )
        .expect("remap a bridge's buses");
    assert_eq!(
        probe.interrupt(own, entry.address, entry.data),
        Some(target)
    );
    let neighbour = own ^ 0x0001;
    assert_eq!(
        probe.interrupt(neighbour, entry.address, entry.data),
        Some(target),
        "any function on the bridge's bus raises it"
    );
    assert_eq!(
        probe.interrupt(other, entry.address, entry.data),
        None,
        "a function on another bus raised it"
    );
    remapping.release_interrupt(entry.entry).expect("release");
    drain(unit);
}

fn a_released_entry_raises_nothing_even_where_cached(
    unit: &dyn IommuUnit,
    remapping: &dyn InterruptRemapping,
    probe: &dyn InterruptProbe,
    own: u16,
    entry: crate::Remapped,
) {
    // Warm whatever cache the unit keeps of the entry.
    assert!(probe.interrupt(own, entry.address, entry.data).is_some());
    remapping.release_interrupt(entry.entry).expect("release");
    assert_eq!(
        probe.interrupt(own, entry.address, entry.data),
        None,
        "a released entry still delivered"
    );
    assert!(
        remapping.release_interrupt(entry.entry).is_err(),
        "an entry released twice"
    );
    drain(unit);
}

fn an_entry_names_the_trigger_mode_and_destination_it_was_given(
    remapping: &dyn InterruptRemapping,
    probe: &dyn InterruptProbe,
    own: u16,
) {
    let level = InterruptTarget {
        vector: 0x60,
        destination: 0xFF,
        level: true,
    };
    let entry = remapping
        .remap_interrupt(InterruptSource::Requester(own), level)
        .expect("remap");
    assert_eq!(probe.interrupt(own, entry.address, entry.data), Some(level));
    assert!(
        remapping
            .remap_interrupt(
                InterruptSource::Requester(own),
                InterruptTarget {
                    destination: 0x100,
                    ..level
                },
            )
            .is_err(),
        "an 8-bit table took a destination past 255"
    );
    remapping.release_interrupt(entry.entry).expect("release");
}
