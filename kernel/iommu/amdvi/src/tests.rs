extern crate std;

use core::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::vec::Vec;

use tairix_kernel_iommu_api::conformance::{self, Fixture, InterruptProbe, TranslationProbe};
use tairix_kernel_iommu_api::hostmem::HostFrames;
use tairix_kernel_iommu_api::{
    Clock, Fault, FaultReason, InterruptSource, InterruptTarget, IommuError, IommuUnit, FAULT_BATCH,
};

use super::*;
use crate::model::{self, Kind, Model, Quirks};

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
const FIXTURE: Fixture = Fixture {
    streams: STREAMS,
    pages: PAGES,
};

fn enabled<'f>(
    model: &'f Model<'f>,
    frames: &'f HostFrames,
    clock: &'f Ticking,
) -> AmdViUnit<'f, &'f Model<'f>> {
    let unit = AmdViUnit::new(model, frames, clock, None).unwrap();
    unit.enable().unwrap();
    unit
}

fn drain(unit: &dyn IommuUnit) -> Vec<Fault> {
    let mut faults = Vec::new();
    while unit.drain_faults(&mut |fault| faults.push(fault)) {}
    faults
}

#[test]
fn a_unit_passes_the_conformance_suite() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, model::FEATURE_INVALIDATE_ALL);
    let clock = clock();
    let unit = enabled(&model, &frames, &clock);
    conformance::run_all(&unit, &model, &FIXTURE);
}

#[test]
fn a_unit_that_cannot_flush_at_once_flushes_every_device_and_passes() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, 0);
    let clock = clock();
    let unit = enabled(&model, &frames, &clock);
    assert_eq!(model.processed(model::OPCODE_ALL), 0);
    assert_eq!(model.processed(model::OPCODE_DEVICE), DEVICES);
    assert_eq!(model.processed(model::OPCODE_INTERRUPTS), DEVICES);
    assert_eq!(
        model.processed(model::OPCODE_PAGES),
        DEVICES,
        "every domain id, whichever firmware left cached"
    );
    conformance::run_all(&unit, &model, &FIXTURE);
}

#[test]
fn a_unit_caching_misses_flushes_every_map_and_passes() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::of_kind(
        &frames,
        model::FEATURE_INVALIDATE_ALL,
        Kind {
            caches_misses: true,
            ..Kind::default()
        },
    );
    let clock = clock();
    let unit = enabled(&model, &frames, &clock);
    conformance::run_all(&unit, &model, &FIXTURE);
}

/// A map writes only entries that were absent, so a unit whose IOMMU
/// capability header says it caches no absent entry is sent no flush for one;
/// a header of another type says nothing.
#[test]
fn a_map_is_flushed_only_on_a_unit_that_may_cache_absent_entries() {
    let frames = HostFrames::new(0x1_0000_0000);
    let clock = clock();
    for (header, flushes) in [
        (Some(0x000B_000F), 0),
        (Some(0x000B_000F | CAPABILITY_NP_CACHE), 1),
        (Some(0x0009_000F), 1),
        (None, 1),
    ] {
        let model = Model::new(&frames, model::FEATURE_INVALIDATE_ALL);
        let function = Function(Mutex::new(None), header);
        let unit = AmdViUnit::new(&model, &frames, &clock, Some((&function, 2))).unwrap();
        unit.enable().unwrap();
        let domain = unit.create_domain().unwrap();
        unit.attach(STREAMS[0], domain).unwrap();
        let before = model.processed(model::OPCODE_PAGES);
        unit.map(domain, 0x10_0000, PAGES[0], 0x1000, Access::READ_WRITE)
            .unwrap();
        assert_eq!(model.processed(model::OPCODE_PAGES) - before, flushes);
        unit.unmap(domain, 0x10_0000, 0x1000).unwrap();
        unit.sync(domain).unwrap();
        unit.block(STREAMS[0]).unwrap();
        unit.destroy_domain(domain).unwrap();
        if flushes == 0 {
            conformance::run_all(&unit, &model, &FIXTURE);
        }
    }
}

/// An unmap's sync names its range, so what the domain still maps stays
/// cached.
#[test]
fn a_range_sync_keeps_the_domain_s_other_translations() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, model::FEATURE_INVALIDATE_ALL);
    let clock = clock();
    let unit = enabled(&model, &frames, &clock);
    let domain = unit.create_domain().unwrap();
    unit.attach(STREAMS[0], domain).unwrap();
    let tag = domain.sixteen_bits().unwrap();
    let iova = 0x10_0000;
    for (at, phys) in [(iova, PAGES[0]), (iova + 0x10_0000, PAGES[1])] {
        unit.map(domain, at, phys, 0x1000, Access::READ_WRITE)
            .unwrap();
        assert_eq!(model.access(STREAMS[0], at, false), Some(phys));
    }
    unit.unmap(domain, iova, 0x1000).unwrap();
    unit.sync_range(domain, iova, 0x1000).unwrap();
    assert!(!model.caches(tag, iova));
    assert_eq!(model.access(STREAMS[0], iova, false), None);
    assert!(model.caches(tag, iova + 0x10_0000));
    unit.unmap(domain, iova + 0x10_0000, 0x1000).unwrap();
    unit.sync(domain).unwrap();
    assert!(!model.caches(tag, iova + 0x10_0000));
    unit.block(STREAMS[0]).unwrap();
    unit.destroy_domain(domain).unwrap();
}

#[test]
fn an_attach_flushes_the_device_s_entry_and_none_of_its_domain() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, model::FEATURE_INVALIDATE_ALL);
    let clock = clock();
    let unit = enabled(&model, &frames, &clock);
    let domain = unit.create_domain().unwrap();
    let (pages, devices) = (
        model.processed(model::OPCODE_PAGES),
        model.processed(model::OPCODE_DEVICE),
    );
    unit.attach(STREAMS[0], domain).unwrap();
    assert_eq!(model.processed(model::OPCODE_PAGES), pages);
    assert_eq!(model.processed(model::OPCODE_DEVICE), devices + 1);
    unit.block(STREAMS[0]).unwrap();
    unit.destroy_domain(domain).unwrap();
}

/// QEMU runs commands only while translation is on: the unit issues none
/// before it, and every one after.
#[test]
fn a_unit_running_commands_only_once_translating_passes() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::of_kind(
        &frames,
        model::FEATURE_INVALIDATE_ALL,
        Kind {
            commands_need_translation: true,
            ..Kind::default()
        },
    );
    let clock = clock();
    let unit = AmdViUnit::new(&model, &frames, &clock, None).unwrap();
    let domain = unit.create_domain().unwrap();
    unit.attach(STREAMS[1], domain).unwrap();
    assert_eq!(
        model.processed(model::OPCODE_DEVICE),
        0,
        "nothing ran before translation"
    );
    unit.enable().unwrap();
    assert_eq!(model.processed(model::OPCODE_ALL), 1);
    unit.block(STREAMS[1]).unwrap();
    unit.destroy_domain(domain).unwrap();
    conformance::run_all(&unit, &model, &FIXTURE);
    conformance::run_interrupts(&unit, &model, [0x0010, 0x0110]);
}

#[test]
fn bring_up_blocks_every_device_once_translation_is_enabled() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, model::FEATURE_INVALIDATE_ALL);
    let clock = clock();
    let unit = AmdViUnit::new(&model, &frames, &clock, None).unwrap();
    assert!(!model.translating());
    for device in [0, 0x0010, 0xFFFF] {
        assert_eq!(
            model.device_entry(device),
            Some([format::DTE_BLOCKED, 0, 0, 0])
        );
    }
    unit.enable().unwrap();
    assert!(model.translating());
    for stream in [0, 0x0010, 0xFFFF] {
        assert_eq!(model.access(stream, 0x1000, false), None);
    }
    let profile = unit.profile();
    assert_eq!(profile.reserved, &RESERVED[..]);
    assert_eq!(profile.reach.input_bits, 48);
    assert_eq!(profile.reach.output_bits, 52);
    assert_eq!(
        model.register(regs::CONTROL) & regs::CONTROL_COHERENT,
        regs::CONTROL_COHERENT,
        "the unit is told to snoop"
    );
}

#[test]
fn a_unit_firmware_left_running_is_taken_over() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, model::FEATURE_INVALIDATE_ALL);
    model.firmware_left_running();
    let clock = clock();
    let unit = AmdViUnit::new(&model, &frames, &clock, None).unwrap();
    assert!(!model.translating(), "translation stops at take-over");
    assert_eq!(
        model.register(regs::EXCLUSION_BASE),
        0,
        "no range passes untranslated"
    );
    assert_eq!(model.register(regs::EXCLUSION_LIMIT), 0);
    assert_ne!(
        model.register(regs::DEVICE_TABLE) & format::ADDRESS,
        0xDEAD_0000
    );
    assert_eq!(model.register(regs::STATUS) & regs::STATUS_CLEAR, 0);
    unit.enable().unwrap();
    conformance::run_all(&unit, &model, &FIXTURE);
}

#[test]
fn a_unit_without_host_translation_is_refused() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, model::FEATURE_NO_HOST);
    let clock = clock();
    assert!(matches!(
        AmdViUnit::new(&model, &frames, &clock, None),
        Err(IommuError::OutOfRange)
    ));
}

/// What the constructor took is given back when it fails before the unit
/// was pointed at any of it.
#[test]
fn a_constructor_that_fails_gives_back_what_it_took() {
    for budget in [1, 2, 3] {
        let frames = HostFrames::new(0x1_0000_0000);
        frames.limit(budget);
        let model = Model::new(&frames, model::FEATURE_INVALIDATE_ALL);
        let clock = clock();
        assert!(matches!(
            AmdViUnit::new(&model, &frames, &clock, None),
            Err(IommuError::Exhausted)
        ));
        assert_eq!(frames.live(), 0, "a budget of {budget} frames leaked");
    }
}

#[test]
fn a_rejected_command_is_reported_and_the_buffer_runs_on() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, model::FEATURE_INVALIDATE_ALL);
    let clock = clock();
    let unit = enabled(&model, &frames, &clock);
    let domain = unit.create_domain().unwrap();
    unit.attach(STREAMS[0], domain).unwrap();
    model.quirk(Quirks {
        reject_next: Some(model::OPCODE_PAGES),
        ..Quirks::default()
    });
    assert_eq!(
        unit.map(domain, 0x4000_0000, PAGES[0], 0x1000, Access::READ_WRITE),
        Err(IommuError::Hardware)
    );
    assert_eq!(
        model.access(STREAMS[0], 0x4000_0000, true),
        None,
        "the refused map left no leaf behind"
    );
    unit.map(domain, 0x4000_0000, PAGES[0], 0x1000, Access::READ_WRITE)
        .unwrap();
    assert_eq!(model.access(STREAMS[0], 0x4000_0000, true), Some(PAGES[0]));
    unit.block(STREAMS[0]).unwrap();
    unit.destroy_domain(domain).unwrap();
    assert!(
        drain(&unit).iter().all(|fault| fault.stream == STREAMS[0]),
        "a rejected command named a device"
    );
}

#[test]
fn a_unit_that_never_confirms_leaves_the_removal_unconfirmed() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, model::FEATURE_INVALIDATE_ALL);
    let clock = clock();
    let unit = enabled(&model, &frames, &clock);
    let domain = unit.create_domain().unwrap();
    unit.attach(STREAMS[0], domain).unwrap();
    model.quirk(Quirks {
        ignore_waits: true,
        ..Quirks::default()
    });
    assert_eq!(unit.block(STREAMS[0]), Err(IommuError::Unconfirmed));
    assert_eq!(
        unit.destroy_domain(domain),
        Err(IommuError::DomainBusy),
        "an unconfirmed detach keeps the stream counted"
    );
    model.quirk(Quirks::default());
    unit.block(STREAMS[0]).unwrap();
    unit.destroy_domain(domain).unwrap();
}

#[test]
fn an_attach_whose_flush_is_refused_takes_its_entry_back() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, model::FEATURE_INVALIDATE_ALL);
    let clock = clock();
    let unit = enabled(&model, &frames, &clock);
    let domain = unit.create_domain().unwrap();
    model.quirk(Quirks {
        reject_next: Some(model::OPCODE_DEVICE),
        ..Quirks::default()
    });
    assert_eq!(unit.attach(STREAMS[0], domain), Err(IommuError::Hardware));
    assert_eq!(
        model.device_entry(0x0010).map(|entry| entry[0]),
        Some(format::DTE_BLOCKED)
    );
    unit.destroy_domain(domain).unwrap();
}

#[test]
fn a_refused_access_is_decoded_cleared_and_reported_once() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, model::FEATURE_INVALIDATE_ALL);
    let clock = clock();
    let unit = enabled(&model, &frames, &clock);
    let domain = unit.create_domain().unwrap();
    unit.attach(STREAMS[1], domain).unwrap();
    assert_eq!(model.access(STREAMS[1], 0x4000_0123, true), None);
    assert_eq!(
        drain(&unit),
        [Fault {
            stream: STREAMS[1],
            iova: 0x4000_0000,
            write: true,
            reason: FaultReason::Unmapped,
        }]
    );
    assert!(drain(&unit).is_empty(), "a record was reported twice");
    unit.block(STREAMS[1]).unwrap();
    unit.destroy_domain(domain).unwrap();
}

#[test]
fn an_overflowed_log_is_drained_and_logging_restarts() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, model::FEATURE_INVALIDATE_ALL);
    let clock = clock();
    let unit = enabled(&model, &frames, &clock);
    model.raise_events(0x0042, EVENT_SLOTS + 20);
    assert_ne!(
        model.register(regs::STATUS) & regs::STATUS_EVENT_OVERFLOW,
        0
    );
    let faults = drain(&unit);
    assert_eq!(
        faults.len(),
        EVENT_SLOTS - 1,
        "a full log holds one fewer than its slots"
    );
    assert!(faults.iter().all(|fault| fault.stream == 0x0042));
    assert_eq!(
        model.register(regs::STATUS) & regs::STATUS_EVENT_OVERFLOW,
        0
    );
    model.raise_events(0x0043, 1);
    assert_eq!(
        drain(&unit)
            .iter()
            .map(|fault| fault.stream)
            .collect::<Vec<_>>(),
        [0x0043],
        "logging restarted"
    );
}

#[test]
fn a_record_that_never_lands_is_skipped() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, model::FEATURE_INVALIDATE_ALL);
    let clock = clock();
    let unit = enabled(&model, &frames, &clock);
    model.lose_next_record();
    model.raise_events(0x0044, 1);
    assert_eq!(
        drain(&unit)
            .iter()
            .map(|fault| fault.stream)
            .collect::<Vec<_>>(),
        [0x0044]
    );
}

/// The unit's lock is held while a drain waits for a record to land, so it
/// waits for the first it misses alone: three that never land cost one short
/// wait, not three handshakes.
#[test]
fn a_drain_waits_once_for_records_that_never_land() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, model::FEATURE_INVALIDATE_ALL);
    let clock = clock();
    let unit = enabled(&model, &frames, &clock);
    for _ in 0..3 {
        model.lose_next_record();
    }
    model.raise_events(0x0044, 1);
    let before = clock.now_ns();
    let streams: Vec<u32> = drain(&unit).iter().map(|fault| fault.stream).collect();
    assert_eq!(streams, [0x0044]);
    let waited = clock.now_ns() - before;
    assert!(
        waited < 10 * RECORD_LANDING_NS,
        "{waited} ns on the clock, which moves a millisecond a read"
    );
}

#[test]
fn more_records_than_one_batch_are_all_drained() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, model::FEATURE_INVALIDATE_ALL);
    let clock = clock();
    let unit = enabled(&model, &frames, &clock);
    model.raise_events(0x0045, 3 * FAULT_BATCH + 1);
    let mut seen = Vec::new();
    assert!(
        !unit.drain_faults(&mut |fault| seen.push(fault)),
        "one call drains a log's worth"
    );
    assert_eq!(seen.len(), 3 * FAULT_BATCH + 1);
}

struct Function(Mutex<Option<(u32, u64, u32)>>, Option<u32>);

/// The unit's own function, recording the one MSI an AMD-Vi family raises
/// through it and refusing what such a unit never asks for.
impl UnitFunction for Function {
    fn route_msi(&self, address: u32, message_address: u64, data: u32) -> Result<(), IommuError> {
        *self.0.lock().unwrap() = Some((address, message_address, data));
        Ok(())
    }

    fn route_msix(&self, _: u32, _: u16, _: u64, _: u32) -> Result<(), IommuError> {
        Err(IommuError::Hardware)
    }

    fn msix_entries(&self, _: u32) -> Result<u16, IommuError> {
        Err(IommuError::Hardware)
    }

    fn mask_msix(&self, _: u32, _: bool) -> Result<(), IommuError> {
        Err(IommuError::Hardware)
    }

    fn capability_header(&self, _: u32, id: u8) -> Result<Option<u32>, IommuError> {
        match self.1 {
            Some(header) if id == SECURE_DEVICE_CAPABILITY => Ok(Some(header)),
            Some(_) => Ok(None),
            None => Err(IommuError::Hardware),
        }
    }

    fn set_master(&self, _: u32, _: bool) -> Result<(), IommuError> {
        Err(IommuError::Hardware)
    }

    fn set_intx(&self, _: u32, _: bool) -> Result<(), IommuError> {
        Err(IommuError::Hardware)
    }

    fn virtio_windows(
        &self,
        _: u32,
    ) -> Result<tairix_kernel_iommu_api::VirtioPciWindows, IommuError> {
        Err(IommuError::Hardware)
    }
}

#[test]
fn the_fault_interrupt_is_raised_through_the_unit_s_own_function() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, model::FEATURE_INVALIDATE_ALL);
    let clock = clock();
    let function = Function(Mutex::new(None), None);
    let unit = AmdViUnit::new(&model, &frames, &clock, Some((&function, 0x0000_0002))).unwrap();
    let message = FaultRoute::Message {
        address: 0xFEE0_0000,
        data: 0x4F,
    };
    unit.route_faults(message).unwrap();
    assert_eq!(
        unit.route_faults(FaultRoute::Wired { place: 0 }),
        Err(IommuError::OutOfRange)
    );
    assert_eq!(
        *function.0.lock().unwrap(),
        Some((0x0000_0002, 0xFEE0_0000, 0x4F))
    );
    assert_ne!(
        model.register(regs::CONTROL) & regs::CONTROL_EVENT_INTERRUPT,
        0
    );
    let control = model.register(regs::CONTROL);
    unit.unroute_faults().unwrap();
    assert_eq!(
        model.register(regs::CONTROL),
        control & !regs::CONTROL_EVENT_INTERRUPT,
        "only the event interrupt is turned off"
    );
    let orphan = AmdViUnit::new(&model, &frames, &clock, None).unwrap();
    assert_eq!(orphan.route_faults(message), Err(IommuError::Hardware));
}

#[test]
fn a_frame_past_the_entry_format_s_reach_is_refused() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, model::FEATURE_INVALIDATE_ALL);
    let clock = clock();
    let unit = enabled(&model, &frames, &clock);
    let domain = unit.create_domain().unwrap();
    assert_eq!(
        unit.map(domain, 0x4000_0000, 1 << 52, 0x1000, Access::READ_WRITE),
        Err(IommuError::OutOfRange)
    );
    unit.destroy_domain(domain).unwrap();
}

#[test]
fn domain_ids_start_above_zero_and_are_reused_last() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, model::FEATURE_INVALIDATE_ALL);
    let clock = clock();
    let unit = enabled(&model, &frames, &clock);
    let first = unit.create_domain().unwrap();
    let second = unit.create_domain().unwrap();
    assert_eq!((first, second), (DomainId(1), DomainId(2)));
    unit.destroy_domain(first).unwrap();
    assert_eq!(
        unit.create_domain().unwrap(),
        DomainId(3),
        "a freed id waits its turn"
    );
}

fn remapping_unit<'f>(
    model: &'f Model<'f>,
    frames: &'f HostFrames,
    clock: &'f Ticking,
) -> AmdViUnit<'f, &'f Model<'f>> {
    enabled(model, frames, clock)
}

#[test]
fn a_remapping_unit_passes_the_interrupt_suite() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, model::FEATURE_INVALIDATE_ALL);
    let clock = clock();
    let unit = remapping_unit(&model, &frames, &clock);
    conformance::run_interrupts(&unit, &model, [0x0010, 0x0110]);
}

#[test]
fn a_unit_flushing_every_device_passes_the_interrupt_suite() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, 0);
    let clock = clock();
    let unit = remapping_unit(&model, &frames, &clock);
    conformance::run_interrupts(&unit, &model, [0x0010, 0x0110]);
}

#[test]
fn extended_entries_deliver_32_bit_destinations() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(
        &frames,
        model::FEATURE_INVALIDATE_ALL | model::FEATURE_EXTENDED,
    );
    let clock = clock();
    let unit = remapping_unit(&model, &frames, &clock);
    let remapping = unit.interrupt_remapping().unwrap();
    assert!(remapping.supports_extended());
    remapping.prepare_remapping(true, 1).unwrap();
    let target = InterruptTarget {
        vector: 0x61,
        destination: 0x0102_0304,
        level: false,
    };
    let entry = remapping
        .remap_interrupt(InterruptSource::Requester(0x0010), target)
        .unwrap();
    remapping.enable_remapping().unwrap();
    assert_eq!(
        model.interrupt(0x0010, entry.address, entry.data),
        Some(target)
    );
    remapping.release_interrupt(entry.entry).unwrap();
    assert_eq!(model.interrupt(0x0010, entry.address, entry.data), None);
}

#[test]
fn a_unit_without_extended_interrupts_refuses_them() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, model::FEATURE_INVALIDATE_ALL);
    let clock = clock();
    let unit = remapping_unit(&model, &frames, &clock);
    let remapping = unit.interrupt_remapping().unwrap();
    assert!(!remapping.supports_extended());
    assert_eq!(
        remapping.prepare_remapping(true, 1),
        Err(IommuError::OutOfRange)
    );
    remapping.prepare_remapping(false, 1).unwrap();
}

/// An IO-APIC pin's entry is raised by its redirection entry: compatibility
/// format, the entry's index in the vector field, its trigger its own.
#[test]
fn an_io_apic_pin_raises_its_entry_by_index() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, model::FEATURE_INVALIDATE_ALL);
    let clock = clock();
    let unit = remapping_unit(&model, &frames, &clock);
    let remapping = unit.interrupt_remapping().unwrap();
    remapping.prepare_remapping(false, 1).unwrap();
    let pins: Vec<_> = (0..3u8)
        .map(|pin| {
            let target = InterruptTarget {
                vector: 0x40 + pin,
                destination: 1,
                level: pin == 2,
            };
            let entry = remapping
                .remap_interrupt(InterruptSource::Requester(0x00A0), target)
                .unwrap();
            (target, entry)
        })
        .collect();
    remapping.enable_remapping().unwrap();
    for (target, entry) in pins {
        let [index, ..] = entry.redirection.to_le_bytes();
        assert_eq!(entry.redirection & (1 << 15) != 0, target.level);
        // The IO-APIC's message: the vector field as data, its trigger bit.
        let data = u32::from(index) | if target.level { 1 << 15 } else { 0 };
        assert_eq!(model.interrupt(0x00A0, 0xFEE0_0000, data), Some(target));
    }
}

#[test]
fn two_tables_never_merge() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, model::FEATURE_INVALIDATE_ALL);
    let clock = clock();
    let unit = remapping_unit(&model, &frames, &clock);
    let remapping = unit.interrupt_remapping().unwrap();
    remapping.prepare_remapping(false, 1).unwrap();
    let target = InterruptTarget {
        vector: 0x50,
        destination: 0,
        level: false,
    };
    remapping
        .remap_interrupt(InterruptSource::Requester(0x0300), target)
        .unwrap();
    remapping
        .remap_interrupt(InterruptSource::Requester(0x0308), target)
        .unwrap();
    assert_eq!(
        remapping.remap_interrupt(InterruptSource::Buses { first: 3, last: 3 }, target),
        Err(IommuError::OutOfRange),
        "a bus holding two sources' tables"
    );
}

#[test]
fn a_full_table_is_exhausted() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, model::FEATURE_INVALIDATE_ALL);
    let clock = clock();
    let unit = remapping_unit(&model, &frames, &clock);
    let remapping = unit.interrupt_remapping().unwrap();
    remapping.prepare_remapping(false, 1).unwrap();
    let target = InterruptTarget {
        vector: 0x50,
        destination: 0,
        level: false,
    };
    for _ in 0..1 << INTERRUPT_TABLE_LENGTH {
        remapping
            .remap_interrupt(InterruptSource::Requester(0x0010), target)
            .unwrap();
    }
    assert_eq!(
        remapping.remap_interrupt(InterruptSource::Requester(0x0010), target),
        Err(IommuError::Exhausted)
    );
}

/// A device newly pointed at an interrupt table counts as covered only once
/// the unit confirms it dropped the entry it cached, so a retry after an
/// unconfirmed attempt points it again rather than trusting the stale entry.
#[test]
fn a_device_s_table_pointer_is_written_again_until_its_flush_is_confirmed() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, model::FEATURE_INVALIDATE_ALL);
    let clock = clock();
    let unit = remapping_unit(&model, &frames, &clock);
    let remapping = unit.interrupt_remapping().unwrap();
    remapping.prepare_remapping(false, 1).unwrap();
    remapping.enable_remapping().unwrap();
    assert_eq!(model.interrupt(0x0020, 0xFEE0_0000, 0), None);
    let target = InterruptTarget {
        vector: 0x51,
        destination: 0,
        level: false,
    };
    model.quirk(Quirks {
        ignore_waits: true,
        lose_device_invalidations: true,
        ..Quirks::default()
    });
    assert_eq!(
        remapping.remap_interrupt(InterruptSource::Requester(0x0020), target),
        Err(IommuError::Unconfirmed)
    );
    model.quirk(Quirks::default());
    let entry = remapping
        .remap_interrupt(InterruptSource::Requester(0x0020), target)
        .unwrap();
    assert_eq!(
        model.interrupt(0x0020, 0xFEE0_0000, entry.data),
        Some(target)
    );
}

#[test]
fn an_entry_the_unit_cannot_confirm_is_never_handed_out() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, model::FEATURE_INVALIDATE_ALL);
    let clock = clock();
    let unit = remapping_unit(&model, &frames, &clock);
    let remapping = unit.interrupt_remapping().unwrap();
    remapping.prepare_remapping(false, 1).unwrap();
    remapping.enable_remapping().unwrap();
    let target = InterruptTarget {
        vector: 0x50,
        destination: 0,
        level: false,
    };
    let first = remapping
        .remap_interrupt(InterruptSource::Requester(0x0010), target)
        .unwrap();
    model.quirk(Quirks {
        ignore_waits: true,
        ..Quirks::default()
    });
    assert_eq!(
        remapping.remap_interrupt(InterruptSource::Requester(0x0010), target),
        Err(IommuError::Unconfirmed)
    );
    model.quirk(Quirks::default());
    let entry = remapping
        .remap_interrupt(InterruptSource::Requester(0x0010), target)
        .unwrap();
    assert_eq!(
        (first.data, entry.data),
        (0, 2),
        "the unconfirmed entry 1 is never reused"
    );
    assert_eq!(
        model.interrupt(0x0010, entry.address, 1),
        None,
        "and raises nothing"
    );
    assert_eq!(
        model.interrupt(0x0010, entry.address, entry.data),
        Some(target)
    );
}

/// A device that gains a table once remapping is on is pointed at it at
/// once; every other device stays refused.
#[test]
fn a_source_remapped_after_the_switch_is_pointed_at_once() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, model::FEATURE_INVALIDATE_ALL);
    let clock = clock();
    let unit = remapping_unit(&model, &frames, &clock);
    let remapping = unit.interrupt_remapping().unwrap();
    remapping.prepare_remapping(false, 1).unwrap();
    remapping.enable_remapping().unwrap();
    // Warm the unit's copy of the device's refusing entry.
    assert_eq!(model.interrupt(0x0020, 0xFEE0_0000, 0), None);
    let target = InterruptTarget {
        vector: 0x70,
        destination: 2,
        level: false,
    };
    let entry = remapping
        .remap_interrupt(InterruptSource::Requester(0x0020), target)
        .unwrap();
    assert_eq!(
        model.interrupt(0x0020, entry.address, entry.data),
        Some(target)
    );
    assert_eq!(model.interrupt(0x0021, entry.address, entry.data), None);
    let refused = drain(&unit);
    assert!(refused
        .iter()
        .all(|fault| fault.reason == FaultReason::Interrupt));
    assert_eq!(refused.len(), 2);
}

/// A silenced device whose attach failed after its entry was blocked is
/// blocked, not silent, so silencing it again silences it.
#[test]
fn a_silenced_device_whose_attach_failed_can_be_silenced_again() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, model::FEATURE_INVALIDATE_ALL);
    let clock = clock();
    let unit = enabled(&model, &frames, &clock);
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
    assert!(drain(&unit).is_empty());
}

/// A table a bridge's buses share is forgotten at once by a unit that can,
/// not device by device; one device's, by its device alone.
#[test]
fn a_bridge_s_table_is_forgotten_at_once_where_the_unit_can() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, model::FEATURE_INVALIDATE_ALL);
    let clock = clock();
    let unit = remapping_unit(&model, &frames, &clock);
    let remapping = unit.interrupt_remapping().unwrap();
    remapping.prepare_remapping(false, 1).unwrap();
    remapping.enable_remapping().unwrap();
    let target = InterruptTarget {
        vector: 0x50,
        destination: 0,
        level: false,
    };
    let (all, one) = (
        model.processed(model::OPCODE_ALL),
        model.processed(model::OPCODE_INTERRUPTS),
    );
    remapping
        .remap_interrupt(InterruptSource::Buses { first: 4, last: 5 }, target)
        .unwrap();
    assert_eq!(model.processed(model::OPCODE_ALL), all + 1);
    assert_eq!(model.processed(model::OPCODE_INTERRUPTS), one);
    remapping
        .remap_interrupt(InterruptSource::Requester(0x0010), target)
        .unwrap();
    assert_eq!(model.processed(model::OPCODE_ALL), all + 1);
    assert_eq!(model.processed(model::OPCODE_INTERRUPTS), one + 1);
}

/// A range of buses ending before it starts names no device, so it is
/// refused rather than given a table no device reads.
#[test]
fn an_inverted_bus_range_is_refused() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, model::FEATURE_INVALIDATE_ALL);
    let clock = clock();
    let unit = remapping_unit(&model, &frames, &clock);
    let remapping = unit.interrupt_remapping().unwrap();
    remapping.prepare_remapping(false, 1).unwrap();
    let target = InterruptTarget {
        vector: 0x50,
        destination: 0,
        level: false,
    };
    assert_eq!(
        remapping.remap_interrupt(InterruptSource::Buses { first: 5, last: 3 }, target),
        Err(IommuError::OutOfRange)
    );
}
