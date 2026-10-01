extern crate std;

use std::cell::Cell;
use std::vec::Vec;

use tairix_kernel_iommu_api::conformance::{self, Fixture, TranslationProbe};
use tairix_kernel_iommu_api::hostmem::HostFrames;
use tairix_kernel_iommu_api::{Clock, Domain, FaultReason, IommuError, IommuUnit, TableCoherence};

use super::*;
use crate::model::{self, Model, Quirks};

/// A clock that moves a millisecond every time it is read, so a wait that
/// never completes runs out of budget in a thousand spins.
struct Ticking(Cell<u64>);

// SAFETY: every test drives its clock from one thread.
unsafe impl Sync for Ticking {}

impl Clock for Ticking {
    fn now_ns(&self) -> u64 {
        let now = self.0.get() + 1_000_000;
        self.0.set(now);
        now
    }
}

fn clock() -> Ticking {
    Ticking(Cell::new(0))
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
    assert_eq!(
        unit.profile().reserved,
        core::slice::from_ref(&INTERRUPT_WINDOW)
    );
    assert_eq!(unit.profile().input_bits, 48);
    assert_eq!(unit.profile().output_bits, 52);
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

#[test]
fn a_non_snooping_walker_gets_every_table_written_back() {
    struct Recorder(Cell<Vec<(u64, usize)>>);
    // SAFETY: the test drives the recorder from one thread.
    unsafe impl Sync for Recorder {}
    impl TableCoherence for Recorder {
        fn write_back(&self, phys: u64, len: usize) {
            let mut log = self.0.take();
            log.push((phys, len));
            self.0.set(log);
        }
    }
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, model::cap(2, 0), model::ecap(false));
    let clock = clock();
    let recorder = Recorder(Cell::new(Vec::new()));
    let unit = VtdUnit::new(&model, &frames, Some(&recorder), &clock).unwrap();
    unit.enable().unwrap();
    let mut domain = Domain::new(&unit, &[]).unwrap();
    domain.attach(STREAMS[0]).unwrap();
    let before = recorder.0.take().len();
    assert!(
        before >= 4,
        "the root, the context table, its entry and the domain root"
    );
    let iova = domain.map(PAGES[0], 0, 0).unwrap();
    assert!(!recorder.0.take().is_empty(), "the new tables and leaf");
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
    unit.route_faults(0xFEE0_1000, 0x41).unwrap();
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
    assert_eq!(decode_fault(0, 0x20 << 32).reason, FaultReason::Other(0x20));
    assert!(!decode_fault(0, FAULT_T2).write);
}
