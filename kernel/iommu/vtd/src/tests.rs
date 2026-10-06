extern crate std;

use core::sync::atomic::{AtomicU64, Ordering};
use std::vec::Vec;

use tairix_kernel_iommu_api::conformance::{self, Fixture, InterruptProbe, TranslationProbe};
use tairix_kernel_iommu_api::hostmem::HostFrames;
use tairix_kernel_iommu_api::{
    Clock, Domain, Fault, FaultReason, InterruptRemapping, InterruptSource, InterruptTarget,
    IommuError, IommuUnit, TableCoherence,
};

use super::*;
use crate::model::{self, Model, Quirks};

/// A clock that moves a millisecond every time it is read, so a wait that
/// never completes runs out of budget in a thousand spins.
struct Ticking(AtomicU64);

impl Clock for Ticking {
    fn now_ns(&self) -> u64 {
        self.0.fetch_add(1_000_000, Ordering::Relaxed) + 1_000_000
    }
}

fn clock() -> Ticking {
    Ticking(AtomicU64::new(0))
}

const STREAMS: [u32; 2] = [0x0010, 0x0018];
const PAGES: [u64; 2] = [0x8000_0000, 0x8000_1000];

fn coherent_model(frames: &HostFrames) -> Model<'_> {
    Model::new(frames, model::cap(2, 0), model::ecap(true))
}

#[test]
fn a_coherent_unit_passes_the_conformance_suite() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = coherent_model(&frames);
    let clock = clock();
    let unit = VtdUnit::new(&model, &frames, None, &clock).unwrap();
    unit.enable().unwrap();
    conformance::run_all(
        &unit,
        &model,
        &Fixture {
            streams: STREAMS,
            pages: PAGES,
        },
    );
}

#[test]
fn a_caching_mode_unit_invalidates_every_new_entry_and_passes() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, model::cap(2, 1 << 7), model::ecap(true));
    let clock = clock();
    let unit = VtdUnit::new(&model, &frames, None, &clock).unwrap();
    unit.enable().unwrap();
    // Cache a miss for the stream and the page before either is mapped: only
    // an invalidation after each new entry lets the access through.
    assert_eq!(model.access(STREAMS[0], 0x4000_0000, true), None);
    conformance::run_all(
        &unit,
        &model,
        &Fixture {
            streams: STREAMS,
            pages: PAGES,
        },
    );
}

#[test]
fn bring_up_blocks_every_stream_once_translation_is_enabled() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = coherent_model(&frames);
    let clock = clock();
    let unit = VtdUnit::new(&model, &frames, None, &clock).unwrap();
    assert!(
        !model.translating(),
        "translation waits for the reserved windows"
    );
    assert_eq!(
        regs::low32(model.register(regs::FECTL)) & regs::FECTL_IM,
        regs::FECTL_IM
    );
    unit.enable().unwrap();
    assert!(model.translating());
    for stream in [0, 0x0010, 0xFFFF] {
        assert_eq!(model.access(stream, 0x1000, false), None);
    }
    assert_eq!(unit.profile().reserved, &RESERVED[..]);
    assert_eq!(unit.profile().reach.input_bits, 48);
    assert_eq!(unit.profile().reach.output_bits, 52);
}

#[test]
fn a_unit_firmware_left_running_is_taken_over() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, model::cap(2, 1 << 5), model::ecap(true));
    model.firmware_left_running();
    let clock = clock();
    let unit = VtdUnit::new(&model, &frames, None, &clock).unwrap();
    assert!(
        !model.translating(),
        "firmware's translation is stopped first"
    );
    unit.enable().unwrap();
    assert_eq!(
        regs::low32(model.register(regs::PMEN)) & regs::PMEN_EPM,
        0,
        "protected memory is retired once translation is on"
    );
    let mut domain = Domain::new(&unit, &[]).unwrap();
    domain.attach(STREAMS[0]).unwrap();
    let iova = domain.map(PAGES[0], 0, 0).unwrap();
    assert_eq!(model.access(STREAMS[0], iova, true), Some(PAGES[0]));
}

#[test]
fn a_unit_this_family_cannot_drive_is_refused() {
    let frames = HostFrames::new(0x1_0000_0000);
    let clock = clock();
    let no_queue = Model::new(&frames, model::cap(2, 0), model::ecap(true) & !(1 << 1));
    assert_eq!(
        VtdUnit::new(&no_queue, &frames, None, &clock).err(),
        Some(IommuError::OutOfRange)
    );
    let no_width = Model::new(&frames, model::cap(2, 0) & !(0x1F << 8), model::ecap(true));
    assert_eq!(
        VtdUnit::new(&no_width, &frames, None, &clock).err(),
        Some(IommuError::OutOfRange)
    );
    let non_snooping = Model::new(&frames, model::cap(2, 0), model::ecap(false));
    assert_eq!(
        VtdUnit::new(&non_snooping, &frames, None, &clock).err(),
        Some(IommuError::OutOfRange),
        "a walker that does not snoop needs a way to write tables back"
    );
    let records_past_window =
        Model::new(&frames, model::cap(2, 0) | (0xFF << 40), model::ecap(true));
    assert_eq!(
        VtdUnit::new(&records_past_window, &frames, None, &clock).err(),
        Some(IommuError::OutOfRange)
    );
}

/// What the constructor took is given back when it fails before the unit
/// was pointed at any of it.
#[test]
fn a_constructor_that_fails_gives_back_what_it_took() {
    for budget in [1, 2] {
        let frames = HostFrames::new(0x1_0000_0000);
        frames.limit(budget);
        let model = coherent_model(&frames);
        let clock = clock();
        assert_eq!(
            VtdUnit::new(&model, &frames, None, &clock).err(),
            Some(IommuError::Exhausted)
        );
        assert_eq!(frames.live(), 0, "a budget of {budget} frames leaked");
    }
}

#[test]
fn a_non_snooping_walker_gets_every_table_written_back() {
    struct Recorder(SpinLock<Vec<(u64, usize)>>);
    impl TableCoherence for Recorder {
        fn write_back(&self, phys: u64, len: usize) {
            self.0.lock().push((phys, len));
        }
    }
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, model::cap(2, 0), model::ecap(false));
    let clock = clock();
    let recorder = Recorder(SpinLock::new(Vec::new()));
    let unit = VtdUnit::new(&model, &frames, Some(&recorder), &clock).unwrap();
    unit.enable().unwrap();
    let mut domain = Domain::new(&unit, &[]).unwrap();
    domain.attach(STREAMS[0]).unwrap();
    let before = core::mem::take(&mut *recorder.0.lock()).len();
    assert!(
        before >= 4,
        "the root, the context table, its entry and the domain root"
    );
    let iova = domain.map(PAGES[0], 0, 0).unwrap();
    assert!(
        !core::mem::take(&mut *recorder.0.lock()).is_empty(),
        "the new tables and leaf"
    );
    assert_eq!(model.access(STREAMS[0], iova, false), Some(PAGES[0]));
}

#[test]
fn a_rejected_descriptor_is_reported_and_the_queue_runs_on() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = coherent_model(&frames);
    let clock = clock();
    let unit = VtdUnit::new(&model, &frames, None, &clock).unwrap();
    unit.enable().unwrap();
    let domain = unit.create_domain().unwrap();
    model.quirk(Quirks {
        reject_next_iotlb: true,
        ..Quirks::default()
    });
    assert_eq!(unit.sync(domain), Err(IommuError::Hardware));
    assert_eq!(
        regs::low32(model.register(regs::FSTS)) & regs::FSTS_IQE,
        0,
        "the error is cleared"
    );
    unit.sync(domain).unwrap();
    unit.destroy_domain(domain).unwrap();
}

#[test]
fn a_unit_that_never_confirms_leaves_the_removal_unconfirmed() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = coherent_model(&frames);
    let clock = clock();
    let unit = VtdUnit::new(&model, &frames, None, &clock).unwrap();
    unit.enable().unwrap();
    let mut domain = Domain::new(&unit, &[]).unwrap();
    domain.attach(STREAMS[0]).unwrap();
    let iova = domain.map(PAGES[0], 0, 0).unwrap();
    model.quirk(Quirks {
        ignore_waits: true,
        ..Quirks::default()
    });
    assert_eq!(domain.unmap(iova), Err(IommuError::Unconfirmed));
    assert_eq!(domain.destroy(), Err(IommuError::Unconfirmed));
}

#[test]
fn a_refused_access_is_decoded_cleared_and_reported_once() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = coherent_model(&frames);
    let clock = clock();
    let unit = VtdUnit::new(&model, &frames, None, &clock).unwrap();
    unit.enable().unwrap();
    assert_eq!(model.access(0x00A8, 0x1234_5678, true), None);
    assert_eq!(model.access(0x00B0, 0x9000, false), None);
    let mut faults = Vec::new();
    assert!(!unit.drain_faults(&mut |fault| faults.push(fault)));
    assert_eq!(faults.len(), 2);
    assert_eq!(faults[0].stream, 0x00A8);
    assert_eq!(faults[0].iova, 0x1234_5000);
    assert!(faults[0].write);
    assert_eq!(faults[0].reason, FaultReason::Blocked);
    assert_eq!(faults[1].stream, 0x00B0);
    assert!(!faults[1].write);
    let mut again = Vec::new();
    assert!(!unit.drain_faults(&mut |fault| again.push(fault)));
    assert!(again.is_empty());
    assert_eq!(regs::low32(model.register(regs::FSTS)) & regs::FSTS_PPF, 0);
}

#[test]
fn an_overflow_of_the_fault_records_is_cleared_by_the_drain() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = coherent_model(&frames);
    let clock = clock();
    let unit = VtdUnit::new(&model, &frames, None, &clock).unwrap();
    unit.enable().unwrap();
    for page in 0..6u64 {
        assert_eq!(model.access(0x0040, page << 12, false), None);
    }
    let mut faults = Vec::new();
    assert!(!unit.drain_faults(&mut |fault| faults.push(fault)));
    assert_eq!(faults.len(), 4, "the unit holds four records");
    assert_eq!(regs::low32(model.register(regs::FSTS)) & regs::FSTS_PFO, 0);
    assert_eq!(model.access(0x0040, 0x7000, false), None);
    let mut after = Vec::new();
    assert!(!unit.drain_faults(&mut |fault| after.push(fault.iova)));
    assert_eq!(
        after,
        [0x7000],
        "recording resumed once the overflow cleared"
    );
}

#[test]
fn more_faults_than_one_batch_are_all_drained() {
    const RECORDS: u64 = 80;
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(
        &frames,
        model::cap_with_records(2, RECORDS, 0),
        model::ecap(true),
    );
    let clock = clock();
    let unit = VtdUnit::new(&model, &frames, None, &clock).unwrap();
    unit.enable().unwrap();
    for page in 0..RECORDS {
        assert_eq!(model.access(0x0040, page << 12, false), None);
    }
    let mut faults = Vec::new();
    assert!(!unit.drain_faults(&mut |fault| faults.push(fault.iova)));
    assert_eq!(
        faults,
        (0..RECORDS).map(|page| page << 12).collect::<Vec<_>>()
    );
    assert_eq!(regs::low32(model.register(regs::FSTS)) & regs::FSTS_PPF, 0);
}

/// Faults recorded while a drain runs keep the unit's run from ending: a call
/// stops after a ring's worth, says records remain, and the next resumes
/// where it stopped, which `FSTS.FRI` no longer names.
#[test]
fn a_drain_stopping_short_says_so_and_the_next_resumes() {
    const RECORDS: u64 = 80;
    const PROVOKED: u64 = 96;
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(
        &frames,
        model::cap_with_records(2, RECORDS, 0),
        model::ecap(true),
    );
    let clock = clock();
    let unit = VtdUnit::new(&model, &frames, None, &clock).unwrap();
    unit.enable().unwrap();
    for page in 0..RECORDS {
        assert_eq!(model.access(0x0040, page << 12, false), None);
    }
    let mut faults = Vec::new();
    let mut next = RECORDS;
    let mut sink = |fault: Fault| {
        faults.push(fault.iova);
        if next < RECORDS + PROVOKED {
            assert_eq!(model.access(0x0040, next << 12, false), None);
            next += 1;
        }
    };
    assert!(unit.drain_faults(&mut sink), "records remain");
    assert!(!unit.drain_faults(&mut sink), "the rest are drained");
    assert_eq!(
        faults,
        (0..RECORDS + PROVOKED)
            .map(|page| page << 12)
            .collect::<Vec<_>>(),
        "every fault, oldest first"
    );
    assert_eq!(regs::FSTS_PFO & regs::low32(model.register(regs::FSTS)), 0);
}

/// Records firmware left mid-ring, and faults recorded behind them before
/// the first drain, are two runs: both are drained.
#[test]
fn records_firmware_left_are_drained_beside_new_ones() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(
        &frames,
        model::cap_with_records(2, 16, 0),
        model::ecap(true),
    );
    model.firmware_left_faults(5, &[0x00A0, 0x00A8, 0x00B0]);
    let clock = clock();
    let unit = VtdUnit::new(&model, &frames, None, &clock).unwrap();
    unit.enable().unwrap();
    assert_eq!(model.access(0x0040, 0x1000, false), None);
    assert_eq!(model.access(0x0048, 0x2000, false), None);
    let mut streams = Vec::new();
    assert!(!unit.drain_faults(&mut |fault| streams.push(fault.stream)));
    assert_eq!(streams, [0x0040, 0x0048, 0x00A0, 0x00A8, 0x00B0]);
    assert_eq!(regs::low32(model.register(regs::FSTS)) & regs::FSTS_PPF, 0);
}

#[test]
fn the_fault_event_is_routed_and_unmasked() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = coherent_model(&frames);
    let clock = clock();
    let unit = VtdUnit::new(&model, &frames, None, &clock).unwrap();
    unit.route_faults(FaultRoute::Message {
        address: 0xFEE0_1000,
        data: 0x41,
    })
    .unwrap();
    assert_eq!(
        unit.route_faults(FaultRoute::Wired { place: 0 }),
        Err(IommuError::OutOfRange)
    );
    assert_eq!(model.register(regs::FEDATA), 0x41);
    assert_eq!(model.register(regs::FEADDR), 0xFEE0_1000);
    assert_eq!(model.register(regs::FEUADDR), 0);
    assert_eq!(model.register(regs::FECTL), 0);
}

#[test]
fn domain_ids_start_above_zero_rotate_and_run_out() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, model::cap(0, 0), model::ecap(true));
    let clock = clock();
    let unit = VtdUnit::new(&model, &frames, None, &clock).unwrap();
    let ids: Vec<_> = (0..15).map(|_| unit.create_domain().unwrap()).collect();
    assert_eq!(ids.first(), Some(&DomainId(1)));
    assert_eq!(ids.last(), Some(&DomainId(15)));
    assert_eq!(unit.create_domain(), Err(IommuError::Exhausted));
    unit.destroy_domain(DomainId(4)).unwrap();
    assert_eq!(unit.create_domain(), Ok(DomainId(4)));
    for id in ids {
        unit.destroy_domain(id).unwrap();
    }
}

#[test]
fn a_domain_with_a_stream_attached_is_not_destroyed() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = coherent_model(&frames);
    let clock = clock();
    let unit = VtdUnit::new(&model, &frames, None, &clock).unwrap();
    let domain = unit.create_domain().unwrap();
    unit.attach(STREAMS[0], domain).unwrap();
    assert_eq!(unit.destroy_domain(domain), Err(IommuError::DomainBusy));
    let other = unit.create_domain().unwrap();
    assert_eq!(unit.attach(STREAMS[0], other), Err(IommuError::StreamBusy));
    assert_eq!(unit.attach(0x1_0000, other), Err(IommuError::OutOfRange));
    unit.block(STREAMS[0]).unwrap();
    unit.destroy_domain(domain).unwrap();
    unit.destroy_domain(other).unwrap();
    assert!(
        model.processed(0x1) >= 1,
        "the block invalidated the context cache"
    );
}

#[test]
fn a_frame_past_the_entry_format_s_reach_is_refused() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = coherent_model(&frames);
    let clock = clock();
    let unit = VtdUnit::new(&model, &frames, None, &clock).unwrap();
    let domain = unit.create_domain().unwrap();
    assert_eq!(
        unit.map(domain, 0x1000, 1 << 52, 0x1000, Access::READ_WRITE),
        Err(IommuError::OutOfRange)
    );
    unit.destroy_domain(domain).unwrap();
}

#[test]
fn fault_records_decode_their_stream_address_access_and_reason() {
    let write_blocked = decode_fault(0x1234_5678, (1 << 63) | (0x2 << 32) | 0x00A8);
    assert_eq!(write_blocked.stream, 0x00A8);
    assert_eq!(write_blocked.iova, 0x1234_5000);
    assert!(write_blocked.write);
    assert_eq!(write_blocked.reason, FaultReason::Blocked);
    let read_unmapped = decode_fault(0, (1 << 63) | FAULT_T1 | (0x6 << 32));
    assert!(!read_unmapped.write);
    assert_eq!(read_unmapped.reason, FaultReason::Unmapped);
    assert_eq!(decode_fault(0, 0x9 << 32).reason, FaultReason::Malformed);
    assert_eq!(decode_fault(0, 0xD << 32).reason, FaultReason::Translated);
    for code in 0x20..=0x27u64 {
        let refused = decode_fault(0x0123 << 48, code << 32);
        assert_eq!(refused.reason, FaultReason::Interrupt, "reason {code:#x}");
        assert_eq!(refused.iova, 0x0123, "the entry the request named");
    }
    assert_eq!(decode_fault(0, 0x30 << 32).reason, FaultReason::Other(0x30));
    assert!(!decode_fault(0, FAULT_T2).write);
}

fn caching_model(frames: &HostFrames) -> Model<'_> {
    Model::new(frames, model::cap(2, 1 << 7), model::ecap(true))
}

const IOVA: u64 = 0x4000_0000;

/// An error firmware left standing halts the queue, so a unit that kept it
/// would refuse the first batch and replace the descriptor at the head.
#[test]
fn errors_firmware_left_are_cleared_before_the_first_batch() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = coherent_model(&frames);
    model.firmware_left_errors();
    let clock = clock();
    let unit = VtdUnit::new(&model, &frames, None, &clock).unwrap();
    assert_eq!(
        regs::low32(model.register(regs::FSTS)) & regs::FSTS_ERRORS,
        0
    );
    unit.enable().unwrap();
    let domain = unit.create_domain().unwrap();
    unit.sync(domain).unwrap();
    unit.destroy_domain(domain).unwrap();
}

/// The caller frees the frames of a map the unit refused, so no leaf may stay
/// behind to reach them.
#[test]
fn a_map_whose_flush_is_refused_leaves_no_leaf_behind() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = caching_model(&frames);
    let clock = clock();
    let unit = VtdUnit::new(&model, &frames, None, &clock).unwrap();
    unit.enable().unwrap();
    let domain = unit.create_domain().unwrap();
    unit.attach(STREAMS[0], domain).unwrap();
    model.quirk(Quirks {
        reject_next_iotlb: true,
        ..Quirks::default()
    });
    assert_eq!(
        unit.map(domain, IOVA, PAGES[0], IO_PAGE_SIZE, Access::READ_WRITE),
        Err(IommuError::Hardware)
    );
    unit.sync(domain).unwrap();
    assert_eq!(model.access(STREAMS[0], IOVA, true), None);
    unit.block(STREAMS[0]).unwrap();
    unit.destroy_domain(domain).unwrap();
}

/// An attach whose flush is refused takes its entry back, so the stream
/// translates nothing and its domain is not held.
#[test]
fn an_attach_whose_flush_is_refused_takes_its_entry_back() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = caching_model(&frames);
    let clock = clock();
    let unit = VtdUnit::new(&model, &frames, None, &clock).unwrap();
    unit.enable().unwrap();
    let domain = unit.create_domain().unwrap();
    unit.map(domain, IOVA, PAGES[0], IO_PAGE_SIZE, Access::READ_WRITE)
        .unwrap();
    model.quirk(Quirks {
        reject_next_iotlb: true,
        ..Quirks::default()
    });
    assert_eq!(unit.attach(STREAMS[0], domain), Err(IommuError::Hardware));
    assert_eq!(model.access(STREAMS[0], IOVA, true), None);
    unit.unmap(domain, IOVA, IO_PAGE_SIZE).unwrap();
    unit.destroy_domain(domain).unwrap();
}

/// A detach the unit cannot confirm keeps its stream counted, so the domain's
/// tables outlive any walk the unit may still hold, and a later block
/// confirms it.
#[test]
fn an_unconfirmed_detach_keeps_the_domain_until_a_later_block_confirms_it() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = coherent_model(&frames);
    let clock = clock();
    let unit = VtdUnit::new(&model, &frames, None, &clock).unwrap();
    unit.enable().unwrap();
    let domain = unit.create_domain().unwrap();
    unit.attach(STREAMS[0], domain).unwrap();
    model.quirk(Quirks {
        ignore_waits: true,
        ..Quirks::default()
    });
    assert_eq!(unit.block(STREAMS[0]), Err(IommuError::Unconfirmed));
    model.quirk(Quirks::default());
    assert_eq!(unit.destroy_domain(domain), Err(IommuError::DomainBusy));
    assert_eq!(
        unit.attach(STREAMS[0], unit.create_domain().unwrap()),
        Err(IommuError::StreamBusy),
        "an unconfirmed stream takes no other domain"
    );
    unit.block(STREAMS[0]).unwrap();
    unit.destroy_domain(domain).unwrap();
}

/// An attach whose flush and whose undo are both unconfirmed answers so, and
/// keeps its stream counted against the domain.
#[test]
fn an_attach_that_cannot_be_confirmed_either_way_holds_its_domain() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = caching_model(&frames);
    let clock = clock();
    let unit = VtdUnit::new(&model, &frames, None, &clock).unwrap();
    unit.enable().unwrap();
    let domain = unit.create_domain().unwrap();
    model.quirk(Quirks {
        ignore_waits: true,
        ..Quirks::default()
    });
    assert_eq!(
        unit.attach(STREAMS[0], domain),
        Err(IommuError::Unconfirmed)
    );
    model.quirk(Quirks::default());
    assert_eq!(unit.destroy_domain(domain), Err(IommuError::DomainBusy));
    unit.block(STREAMS[0]).unwrap();
    unit.destroy_domain(domain).unwrap();
}

/// Caching mode reserves domain id 0, so the silent table carries an id of its
/// own, and every stream silenced shares it.
#[test]
fn silenced_streams_carry_an_id_of_their_own_never_zero() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = caching_model(&frames);
    let clock = clock();
    let unit = VtdUnit::new(&model, &frames, None, &clock).unwrap();
    unit.enable().unwrap();
    let domain = unit.create_domain().unwrap();
    unit.attach(STREAMS[0], domain).unwrap();
    unit.silence(STREAMS[0]).unwrap();
    unit.silence(STREAMS[1]).unwrap();
    let silent = model.context_domain(0x0010);
    assert!(silent.is_some_and(|id| id != 0));
    assert_eq!(model.context_domain(0x0018), silent);
    assert_ne!(silent, Some(u16::try_from(domain.0).unwrap()));
    assert_eq!(model.access(STREAMS[0], IOVA, true), None);
    unit.destroy_domain(domain).unwrap();
}

/// A fresh id is handed out before a freed one, and freed ones in the order
/// they were freed.
#[test]
fn freed_domain_ids_are_reused_last_and_in_order() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, model::cap(0, 0), model::ecap(true));
    let clock = clock();
    let unit = VtdUnit::new(&model, &frames, None, &clock).unwrap();
    let first = unit.create_domain().unwrap();
    let second = unit.create_domain().unwrap();
    unit.destroy_domain(second).unwrap();
    unit.destroy_domain(first).unwrap();
    assert_eq!(unit.create_domain(), Ok(DomainId(3)), "fresh first");
    let rest: Vec<_> = (0..12).map(|_| unit.create_domain().unwrap()).collect();
    assert_eq!(rest.last(), Some(&DomainId(15)));
    assert_eq!(unit.create_domain(), Ok(second), "then the oldest freed");
    assert_eq!(unit.create_domain(), Ok(first));
    assert_eq!(unit.create_domain(), Err(IommuError::Exhausted));
}

fn remapping_model(frames: &HostFrames, extra: u64) -> Model<'_> {
    Model::new(
        frames,
        model::cap(2, 0),
        model::ecap(true) | model::ECAP_IR | extra,
    )
}

#[test]
fn a_remapping_unit_passes_the_interrupt_suite() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = remapping_model(&frames, 0);
    let clock = clock();
    let unit = VtdUnit::new(&model, &frames, None, &clock).unwrap();
    unit.enable().unwrap();
    conformance::run_interrupts(&unit, &model, [0x0010, 0x0208]);
}

#[test]
fn a_unit_without_remapping_offers_none() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = coherent_model(&frames);
    let clock = clock();
    let unit = VtdUnit::new(&model, &frames, None, &clock).unwrap();
    assert!(unit.interrupt_remapping().is_none());
    assert_eq!(
        InterruptRemapping::prepare_remapping(&unit, false, 16),
        Err(IommuError::OutOfRange)
    );
}

/// The table holds at least the entries asked for, a power of two of them,
/// no fewer than one frame's; 32-bit destinations need the unit to say it
/// takes them.
#[test]
fn the_table_is_sized_to_the_machine_and_its_destinations_to_the_unit() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = remapping_model(&frames, 0);
    let clock = clock();
    let unit = VtdUnit::new(&model, &frames, None, &clock).unwrap();
    assert!(!InterruptRemapping::supports_extended(&unit));
    assert_eq!(
        InterruptRemapping::prepare_remapping(&unit, true, 16),
        Err(IommuError::OutOfRange),
        "no extended interrupt mode"
    );
    InterruptRemapping::prepare_remapping(&unit, false, 1000).unwrap();
    assert_eq!(
        model.register(regs::IRTA) & 0xF,
        9,
        "1024 entries: two to the size field plus one"
    );
    assert_eq!(model.register(regs::IRTA) & regs::IRTA_EIME, 0);

    let frames = HostFrames::new(0x1_0000_0000);
    let model = remapping_model(&frames, model::ECAP_EIM);
    let unit = VtdUnit::new(&model, &frames, None, &clock).unwrap();
    assert!(InterruptRemapping::supports_extended(&unit));
    InterruptRemapping::prepare_remapping(&unit, true, 1).unwrap();
    assert_eq!(
        model.register(regs::IRTA) & 0xF,
        7,
        "one frame's 256 entries"
    );
    InterruptRemapping::enable_remapping(&unit).unwrap();
    let far = InterruptTarget {
        vector: 0x70,
        destination: 0x0001_0203,
        level: false,
    };
    let entry =
        InterruptRemapping::remap_interrupt(&unit, InterruptSource::Requester(0x10), far).unwrap();
    assert_eq!(model.interrupt(0x10, entry.address, entry.data), Some(far));
}

/// A machine asking for more entries than a table holds gets the largest
/// table the unit can point at.
#[test]
fn a_table_never_outgrows_what_the_unit_can_point_at() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = remapping_model(&frames, 0);
    let clock = clock();
    let unit = VtdUnit::new(&model, &frames, None, &clock).unwrap();
    InterruptRemapping::prepare_remapping(&unit, false, u32::MAX).unwrap();
    assert_eq!(
        model.register(regs::IRTA) & 0xF,
        15,
        "2^16 entries, the most S names"
    );
}

#[test]
fn remapping_firmware_left_on_is_turned_off_at_take_over() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = remapping_model(&frames, 0);
    model.firmware_left_running();
    let clock = clock();
    let _unit = VtdUnit::new(&model, &frames, None, &clock).unwrap();
    assert_eq!(
        regs::low32(model.register(regs::GSTS)) & regs::GSTS_IRES,
        0,
        "firmware's table delivers nothing once the unit is taken over"
    );
}

/// An enabled table refuses compatibility interrupts even where firmware had
/// allowed them, and a refused request is reported against its source with
/// the entry it named.
#[test]
fn a_refused_interrupt_is_reported_with_its_source_and_entry() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = remapping_model(&frames, 0);
    model.firmware_left_running();
    let clock = clock();
    let unit = VtdUnit::new(&model, &frames, None, &clock).unwrap();
    InterruptRemapping::prepare_remapping(&unit, false, 16).unwrap();
    let target = InterruptTarget {
        vector: 0x44,
        destination: 0,
        level: false,
    };
    let entry =
        InterruptRemapping::remap_interrupt(&unit, InterruptSource::Requester(0x18), target)
            .unwrap();
    InterruptRemapping::enable_remapping(&unit).unwrap();
    assert_eq!(model.gcmd_overloaded(), 0, "one command per write");
    assert_eq!(
        model.interrupt(0x18, 0xFEE0_0000, 0x44),
        None,
        "compatibility blocked"
    );
    assert_eq!(model.interrupt(0x20, entry.address, entry.data), None);
    let mut faults = Vec::new();
    while unit.drain_faults(&mut |fault| faults.push(fault)) {}
    assert_eq!(
        faults,
        [
            Fault {
                stream: 0x18,
                iova: 0,
                write: true,
                reason: FaultReason::Interrupt,
            },
            Fault {
                stream: 0x20,
                iova: u64::from(entry.entry),
                write: true,
                reason: FaultReason::Interrupt,
            },
        ]
    );
}

/// An entry the unit could not confirm written is taken back and never
/// handed out again, so no source is ever programmed with it.
#[test]
fn an_entry_the_unit_cannot_confirm_is_never_handed_out() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = remapping_model(&frames, 0);
    let clock = clock();
    let unit = VtdUnit::new(&model, &frames, None, &clock).unwrap();
    InterruptRemapping::prepare_remapping(&unit, false, 16).unwrap();
    let target = InterruptTarget {
        vector: 0x44,
        destination: 0,
        level: false,
    };
    model.quirk(Quirks {
        ignore_waits: true,
        ..Quirks::default()
    });
    assert_eq!(
        InterruptRemapping::remap_interrupt(&unit, InterruptSource::Requester(1), target),
        Err(IommuError::Unconfirmed)
    );
    model.quirk(Quirks::default());
    let next =
        InterruptRemapping::remap_interrupt(&unit, InterruptSource::Requester(1), target).unwrap();
    assert_ne!(next.entry, 0, "the unconfirmed entry is not reused");
}

/// A silenced stream whose attach failed after its context was cleared is
/// blocked, not silent, so silencing it again silences it.
/// A context change the unit never confirmed may leave it caching the entry
/// it replaced, under a domain id the next context does not name, so the next
/// context published flushes every cached one rather than trusting it gone.
#[test]
fn a_context_change_left_unconfirmed_is_flushed_before_the_next_is_trusted() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = coherent_model(&frames);
    let clock = clock();
    let unit = VtdUnit::new(&model, &frames, None, &clock).unwrap();
    unit.enable().unwrap();
    let first = unit.create_domain().unwrap();
    let second = unit.create_domain().unwrap();
    unit.map(second, IOVA, PAGES[0], IO_PAGE_SIZE, Access::READ_WRITE)
        .unwrap();
    unit.silence(STREAMS[0]).unwrap();
    assert_eq!(model.access(STREAMS[0], IOVA, true), None);
    model.quirk(Quirks {
        ignore_waits: true,
        lose_context_invalidations: true,
        ..Quirks::default()
    });
    assert_eq!(unit.attach(STREAMS[0], first), Err(IommuError::Unconfirmed));
    model.quirk(Quirks::default());
    unit.attach(STREAMS[0], second).unwrap();
    assert_eq!(model.access(STREAMS[0], IOVA, true), Some(PAGES[0]));
}

#[test]
fn a_silenced_stream_whose_attach_failed_can_be_silenced_again() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = caching_model(&frames);
    let clock = clock();
    let unit = VtdUnit::new(&model, &frames, None, &clock).unwrap();
    unit.enable().unwrap();
    let domain = unit.create_domain().unwrap();
    unit.silence(STREAMS[0]).unwrap();
    model.quirk(Quirks {
        ignore_waits: true,
        ..Quirks::default()
    });
    assert_eq!(
        unit.attach(STREAMS[0], domain),
        Err(IommuError::Unconfirmed)
    );
    model.quirk(Quirks::default());
    unit.silence(STREAMS[0]).unwrap();
    assert_eq!(model.access(STREAMS[0], 0x1000, true), None);
    let mut faults = Vec::new();
    unit.drain_faults(&mut |fault| faults.push(fault));
    assert!(faults.is_empty(), "{faults:?}");
}

/// A reserved domain-count encoding names no more ids than the 16-bit domain
/// field holds.
#[test]
fn a_unit_s_domain_ids_fit_the_domain_field() {
    assert_eq!(regs::Cap(model::cap(2, 0)).domains(), 256);
    assert_eq!(regs::Cap(model::cap(6, 0)).domains(), 1 << 16);
    assert_eq!(regs::Cap(model::cap(7, 0)).domains(), 1 << 16);
}
