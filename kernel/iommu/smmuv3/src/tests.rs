extern crate std;

use core::sync::atomic::{AtomicU64, Ordering};
use std::vec::Vec;

use tairix_kernel_iommu_api::conformance::{self, Fixture, TranslationProbe};
use tairix_kernel_iommu_api::hostmem::HostFrames;
use tairix_kernel_iommu_api::{
    Clock, Domain, Fault, FaultReason, FaultRoute, IommuError, IommuUnit,
};

use super::*;
use crate::model::{event, op, Features, Model, Quirks};

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

const STREAMS: [u32; 2] = [0x0010, 0x0118];
const PAGES: [u64; 2] = [0x8000_0000, 0x8000_1000];

/// QEMU's `virt` `SMMUv3` with `stage=2`: no messages, a two-level table over
/// sixteen stream-id bits, a 44-bit output.
const QEMU: Features = Features {
    stage2: true,
    msi: false,
    two_level: true,
    stream_bits: 16,
    output: 0b100,
};

fn fixture() -> Fixture {
    Fixture {
        streams: STREAMS,
        pages: PAGES,
    }
}

#[test]
fn a_stage_2_unit_confirming_by_consumption_passes_the_conformance_suite() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, QEMU);
    let clock = clock();
    let unit = Smmuv3Unit::new(&model, &frames, &clock).unwrap();
    unit.enable().unwrap();
    conformance::run_all(&unit, &model, &fixture());
    assert!(model.processed(op::TLBI_S12_VMALL) > 0);
}

#[test]
fn a_stage_1_unit_with_a_linear_table_and_stored_syncs_passes_the_conformance_suite() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(
        &frames,
        Features {
            stage2: false,
            msi: true,
            two_level: false,
            stream_bits: 10,
            output: 0b100,
        },
    );
    let clock = clock();
    let unit = Smmuv3Unit::new(&model, &frames, &clock).unwrap();
    unit.enable().unwrap();
    conformance::run_all(&unit, &model, &fixture());
    assert!(model.processed(op::TLBI_NH_ASID) > 0);
    assert!(model.processed(op::CFGI_CD_ALL) > 0);
}

/// Firmware may leave the unit translating, its interrupts raised and an
/// error standing: the hand-off aborts every transaction until translation is
/// ours, and each stream is blocked once it is.
#[test]
fn bring_up_aborts_everything_until_translation_is_ours_and_then_blocks_every_stream() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, QEMU);
    model.firmware_left_running();
    let clock = clock();
    let unit = Smmuv3Unit::new(&model, &frames, &clock).unwrap();
    assert!(!model.translating());
    assert!(model.aborting());
    assert_eq!(model.interrupts(), 0);
    assert_eq!(model.errors(), 0, "the error firmware left is acknowledged");
    assert_eq!(model.access(STREAMS[0], 0x1000, false), None);
    unit.enable().unwrap();
    assert!(model.translating());
    assert_eq!(model.access(STREAMS[0], 0x1000, true), None);
    let mut faults = Vec::new();
    unit.drain_faults(&mut |fault| faults.push(fault));
    assert_eq!(
        faults,
        [Fault {
            stream: STREAMS[0],
            iova: 0,
            write: false,
            reason: FaultReason::Blocked,
        }]
    );
}

/// An entry that translates is written word 0 last, and one that reads only
/// word 0 — invalid or aborting — word 0 first: silencing a translating
/// stream never leaves its stage-2 config naming the zeroed table pointer of
/// the entry replacing it.
#[test]
fn a_translating_entry_s_words_are_confirmed_before_word_0_makes_them_live() {
    let translating = format::stage2_ste(format::Stage2 {
        vmid: 1,
        root: 0x8000_0000,
        input_bits: 44,
        levels: 4,
        output: 4,
    });
    assert!(format::translates(&translating));
    assert!(format::translates(&format::stage1_ste(0x9000_0000, false)));
    assert!(!format::translates(&format::silent_ste()));
    assert!(!format::translates(&[0; format::STE_WORDS]));
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, QEMU);
    let clock = clock();
    let unit = Smmuv3Unit::new(&model, &frames, &clock).unwrap();
    unit.enable().unwrap();
    let domain = unit.create_domain().unwrap();
    let before = model.processed(op::CFGI_STE);
    unit.attach(STREAMS[0], domain).unwrap();
    assert_eq!(
        model.processed(op::CFGI_STE) - before,
        2,
        "once for the words behind word 0, once for word 0"
    );
    let before = model.processed(op::CFGI_STE);
    unit.block(STREAMS[0]).unwrap();
    assert_eq!(
        model.processed(op::CFGI_STE) - before,
        1,
        "an invalid entry is its word 0 alone"
    );
}

/// A silenced stream reaches nothing and raises nothing; a stream past the
/// streams the table covers is refused and its transactions recorded.
#[test]
fn a_silenced_stream_is_quiet_and_one_past_the_table_is_refused() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, QEMU);
    let clock = clock();
    let unit = Smmuv3Unit::new(&model, &frames, &clock).unwrap();
    unit.enable().unwrap();
    unit.silence(STREAMS[0]).unwrap();
    assert_eq!(model.access(STREAMS[0], 0x1000, true), None);
    let past = 1 << QEMU.stream_bits;
    let domain = unit.create_domain().unwrap();
    assert_eq!(unit.attach(past, domain), Err(IommuError::OutOfRange));
    assert_eq!(model.access(past, 0x1000, true), None);
    let mut faults = Vec::new();
    unit.drain_faults(&mut |fault| faults.push(fault));
    assert_eq!(faults.len(), 1, "the silenced stream recorded nothing");
    assert_eq!(
        (faults[0].stream, faults[0].reason),
        (past, FaultReason::Blocked)
    );
}

/// Streams in two spans of the level-1 table each link a second-level table,
/// and the first write through a new link forgets the descriptor too.
#[test]
fn streams_far_apart_each_link_their_own_second_level_table() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, QEMU);
    let clock = clock();
    let unit = Smmuv3Unit::new(&model, &frames, &clock).unwrap();
    unit.enable().unwrap();
    let mut domain = Domain::new(&unit, &[]).unwrap();
    for stream in [0x0001, 0x0002, 0xFF40] {
        domain.attach(stream).unwrap();
    }
    let iova = domain.map(PAGES[0], 0, 0).unwrap();
    for stream in [0x0001, 0x0002, 0xFF40] {
        assert_eq!(model.access(stream, iova, true), Some(PAGES[0]));
    }
    assert_eq!(
        model.processed(op::CFGI_STE),
        6,
        "each stream's entry confirmed behind word 0, then whole"
    );
}

#[test]
fn a_rejected_command_is_reported_and_the_queue_runs_on() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, QEMU);
    let clock = clock();
    let unit = Smmuv3Unit::new(&model, &frames, &clock).unwrap();
    unit.enable().unwrap();
    let domain = unit.create_domain().unwrap();
    model.quirk(Quirks {
        reject: Some(op::TLBI_S12_VMALL),
        ..Quirks::default()
    });
    assert_eq!(unit.sync(domain), Err(IommuError::Unconfirmed));
    assert_eq!(model.errors(), 0, "the error is acknowledged");
    unit.sync(domain).unwrap();
    unit.destroy_domain(domain).unwrap();
}

/// A unit that never gets past a sync confirms nothing: a domain it could not
/// confirm forgotten keeps its tables and its id, and an entry whose words the
/// unit never confirmed is never made live.
#[test]
fn a_unit_that_never_completes_a_sync_confirms_nothing() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, QEMU);
    let clock = clock();
    let unit = Smmuv3Unit::new(&model, &frames, &clock).unwrap();
    unit.enable().unwrap();
    let domain = unit.create_domain().unwrap();
    model.quirk(Quirks {
        stall_syncs: true,
        ..Quirks::default()
    });
    assert_eq!(unit.destroy_domain(domain), Err(IommuError::Unconfirmed));
    assert_eq!(
        unit.attach(STREAMS[0], domain),
        Err(IommuError::Unconfirmed)
    );
    assert_eq!(
        model.access(STREAMS[0], 0x1000, true),
        None,
        "its word 0 never named the domain"
    );
    assert_eq!(
        unit.destroy_domain(domain),
        Err(IommuError::Unconfirmed),
        "no stream holds the domain; the unit still confirms nothing"
    );
}

#[test]
fn a_unit_this_family_cannot_drive_is_refused() {
    struct Altered<'m> {
        model: &'m Model<'m>,
        offset: usize,
        value: u32,
        window: usize,
    }
    impl Registers for Altered<'_> {
        fn read32(&self, offset: usize) -> Result<u32, IommuError> {
            if offset == self.offset {
                return Ok(self.value);
            }
            (&self.model).read32(offset)
        }
        fn write32(&self, offset: usize, value: u32) -> Result<(), IommuError> {
            (&self.model).write32(offset, value)
        }
        fn read64(&self, offset: usize) -> Result<u64, IommuError> {
            (&self.model).read64(offset)
        }
        fn write64(&self, offset: usize, value: u64) -> Result<(), IommuError> {
            (&self.model).write64(offset, value)
        }
        fn window_len(&self) -> usize {
            self.window
        }
    }
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, QEMU);
    let idr0 = (&model).read32(regs::IDR0).unwrap();
    let idr1 = (&model).read32(regs::IDR1).unwrap();
    let idr5 = (&model).read32(regs::IDR5).unwrap();
    let clock = clock();
    for (offset, value, window) in [
        (regs::IDR0, idr0 & !0b11, regs::WINDOW),
        (regs::IDR0, idr0 & !(1 << 4), regs::WINDOW),
        (regs::IDR0, idr0 | 0b11 << 21, regs::WINDOW),
        (
            regs::IDR0,
            (idr0 & !(0b11 << 24)) | 0b10 << 24,
            regs::WINDOW,
        ),
        (regs::IDR0, idr0 & !(0b11 << 2), regs::WINDOW),
        (regs::IDR1, idr1 | 1 << 29, regs::WINDOW),
        (regs::IDR1, (idr1 & !(0x1F << 21)) | 7 << 21, regs::WINDOW),
        (regs::IDR5, idr5 & !(1 << 4), regs::WINDOW),
        (regs::IDR0, idr0, regs::WINDOW / 2),
    ] {
        let altered = Altered {
            model: &model,
            offset,
            value,
            window,
        };
        assert_eq!(
            Smmuv3Unit::new(altered, &frames, &clock).err(),
            Some(IommuError::OutOfRange),
            "{offset:#x} = {value:#x}, window {window:#x}"
        );
    }
}

#[test]
fn a_wired_route_raises_the_interrupts_and_a_message_needs_a_unit_that_sends_one() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, QEMU);
    let clock = clock();
    let unit = Smmuv3Unit::new(&model, &frames, &clock).unwrap();
    let message = FaultRoute::Message {
        address: 0x0802_0040,
        data: 0x41,
    };
    assert_eq!(unit.route_faults(message), Err(IommuError::OutOfRange));
    unit.route_faults(FaultRoute::Wired { place: 1 }).unwrap();
    assert_eq!(model.interrupts(), regs::IRQ_EVENTQ | regs::IRQ_GERROR);

    let frames = HostFrames::new(0x1_0000_0000);
    let sending = Model::new(&frames, Features { msi: true, ..QEMU });
    let unit = Smmuv3Unit::new(&sending, &frames, &clock).unwrap();
    unit.route_faults(message).unwrap();
    assert_eq!(sending.word(regs::EVENTQ_IRQ_CFG0), 0x0802_0040);
    assert_eq!(sending.word(regs::EVENTQ_IRQ_CFG1), 0x41);
    assert_eq!(sending.word(regs::GERROR_IRQ_CFG0), 0x0802_0040);
    unit.route_faults(FaultRoute::Wired { place: 1 }).unwrap();
    assert_eq!(
        sending.word(regs::EVENTQ_IRQ_CFG0),
        0,
        "the wire signals again"
    );
    assert_eq!(
        sending.configured_live(),
        0,
        "configured only with its interrupts off"
    );
    for address in [0x0802_0042, 1 << 52] {
        assert_eq!(
            unit.route_faults(FaultRoute::Message {
                address,
                data: 0x41
            }),
            Err(IommuError::OutOfRange),
            "{address:#x}"
        );
    }
    assert_eq!(sending.word(regs::EVENTQ_IRQ_CFG0), 0, "nothing written");
}

/// An unsupported upstream transaction is a fault against its address,
/// charged like any other, not a record passed over.
#[test]
fn an_unsupported_transaction_is_recorded_against_its_address() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, QEMU);
    let clock = clock();
    let unit = Smmuv3Unit::new(&model, &frames, &clock).unwrap();
    model.raise(1, event::UNSUPPORTED, 5, 0x7000_0042);
    let mut faults = Vec::new();
    unit.drain_faults(&mut |fault| faults.push(fault));
    assert_eq!(
        faults,
        [Fault {
            stream: 5,
            iova: 0x7000_0000,
            write: true,
            reason: FaultReason::Malformed,
        }]
    );
}

/// Only an attach ends silence: blocking a silenced stream leaves it quiet.
#[test]
fn blocking_a_silenced_stream_keeps_it_silent() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, QEMU);
    let clock = clock();
    let unit = Smmuv3Unit::new(&model, &frames, &clock).unwrap();
    unit.enable().unwrap();
    unit.silence(STREAMS[0]).unwrap();
    unit.block(STREAMS[0]).unwrap();
    assert_eq!(model.access(STREAMS[0], 0x1000, true), None);
    let mut faults = Vec::new();
    unit.drain_faults(&mut |fault| faults.push(fault));
    assert!(faults.is_empty(), "{faults:?}");
}

/// A record landing as the drain hands slots back raises no interrupt of its
/// own, so the drain finds it before it stops.
#[test]
fn a_record_landing_as_the_drain_hands_slots_back_is_drained_too() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, QEMU);
    let clock = clock();
    let unit = Smmuv3Unit::new(&model, &frames, &clock).unwrap();
    model.raise(1, event::TRANSLATION, 7, 0x1000);
    model.raise_when_consumed(event::TRANSLATION, 8, 0x2000);
    let mut streams = Vec::new();
    assert!(!unit.drain_faults(&mut |fault| streams.push(fault.stream)));
    assert_eq!(streams, [7, 8]);
}

/// Each event becomes the fault it reports, in order; a drain stops at a
/// batch's end and says more remain; an overflow is acknowledged.
#[test]
fn events_drain_as_faults_in_batches_and_an_overflow_is_acknowledged() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, QEMU);
    let clock = clock();
    let unit = Smmuv3Unit::new(&model, &frames, &clock).unwrap();
    model.raise(1, event::TRANSLATION, 7, 0x4000_1234);
    model.raise(1, event::PERMISSION, 8, 0x4000_5678);
    model.raise(1, event::TRANSL_FORBIDDEN, 9, 0x9000);
    let mut faults = Vec::new();
    assert!(!unit.drain_faults(&mut |fault| faults.push(fault)));
    assert_eq!(
        faults,
        [
            Fault {
                stream: 7,
                iova: 0x4000_1000,
                write: true,
                reason: FaultReason::Unmapped,
            },
            Fault {
                stream: 8,
                iova: 0x4000_5000,
                write: true,
                reason: FaultReason::Denied,
            },
            Fault {
                stream: 9,
                iova: 0x9000,
                write: true,
                reason: FaultReason::Translated,
            },
        ]
    );
    let slots = 1usize << EVENT_BITS;
    model.raise(slots + 3, event::TRANSLATION, 7, 0);
    let mut drained = 0;
    while unit.drain_faults(&mut |_| drained += 1) {}
    assert_eq!(
        drained, slots,
        "the wrap bit lets every slot hold a record; the rest overflowed"
    );
    model.raise(1, event::ACCESS, 7, 0);
    let mut after = Vec::new();
    unit.drain_faults(&mut |fault| after.push(fault));
    assert_eq!(
        after.len(),
        1,
        "the overflow was acknowledged and recording resumed"
    );
}

#[test]
fn a_stage_1_unit_refuses_a_mapping_a_device_could_write_but_not_read() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(
        &frames,
        Features {
            stage2: false,
            ..QEMU
        },
    );
    let clock = clock();
    let unit = Smmuv3Unit::new(&model, &frames, &clock).unwrap();
    let domain = unit.create_domain().unwrap();
    assert_eq!(
        unit.map(domain, 0x1000, PAGES[0], 0x1000, Access::WRITE),
        Err(IommuError::OutOfRange)
    );
    unit.map(domain, 0x1000, PAGES[0], 0x1000, Access::READ)
        .unwrap();
}

#[test]
fn the_fault_line_is_the_combined_one_else_the_event_queue_s() {
    let place = |list: &'static [&'static [u8]]| fault_interrupt(&list.iter().copied());
    assert_eq!(
        place(&[b"eventq", b"priq", b"cmdq-sync", b"gerror"]),
        Some(0)
    );
    assert_eq!(place(&[b"gerror", b"eventq"]), Some(1));
    assert_eq!(place(&[b"eventq", b"combined"]), Some(1));
    assert_eq!(place(&[b"gerror", b"priq"]), None);
    assert_eq!(place(&[]), None);
}

#[test]
fn a_table_entry_reads_back_as_written_at_every_level_of_either_stage() {
    use tairix_kernel_iommu_api::{Pte, PteFormat};
    for stage in [Stage::First, Stage::Second] {
        let tables = format::ArmTables {
            stage,
            root_order: 0,
        };
        for level in 0..3 {
            let leaf = tables.leaf(0x8020_0000, level, Access::READ_WRITE);
            assert_eq!(
                tables.decode(leaf, level),
                Pte::Leaf(0x8020_0000, Access::READ_WRITE)
            );
            let read = tables.leaf(0x8020_0000, level, Access::READ);
            assert_eq!(
                tables.decode(read, level),
                Pte::Leaf(0x8020_0000, Access::READ)
            );
        }
        for level in 1..4 {
            let table = tables.table(0x9000, level);
            assert_eq!(tables.decode(table, level), Pte::Table(0x9000));
        }
        assert_eq!(
            tables.decode(0x9001, 0),
            Pte::Absent,
            "a block at level 3 is reserved"
        );
        assert_eq!(tables.decode(0, 1), Pte::Absent);
    }
    let two = format::ArmTables {
        stage: Stage::Second,
        root_order: 0,
    };
    let write_only = two.leaf(0x8000_0000, 0, Access::WRITE);
    assert_eq!(
        two.decode(write_only, 0),
        Pte::Leaf(0x8000_0000, Access::WRITE)
    );
}

/// A stage 2 walk may start at level 0 only on a 44-bit output or wider, so a
/// narrower unit's domains start at level 1 over the tables concatenated
/// there, and the highest IOVA they hand out still reaches the device.
#[test]
fn a_narrow_output_s_domains_start_at_level_1_over_concatenated_tables() {
    for (output, bits) in [(0b010, 40), (0b011, 42), (0b100, 44), (0b111, 48)] {
        let frames = HostFrames::new(0x1_0000_0000);
        let model = Model::new(&frames, Features { output, ..QEMU });
        let clock = clock();
        let unit = Smmuv3Unit::new(&model, &frames, &clock).unwrap();
        assert_eq!(
            unit.profile().reach.input_bits,
            bits,
            "output {output:#05b}"
        );
        unit.enable().unwrap();
        let mut domain = Domain::new(&unit, &[]).unwrap();
        domain.attach(STREAMS[0]).unwrap();
        let iova = domain.map(PAGES[0], 0, 0).unwrap();
        assert!(
            iova >> (bits - 1) == 1,
            "{iova:#x} is the top of {bits} bits"
        );
        assert_eq!(model.access(STREAMS[0], iova, true), Some(PAGES[0]));
        let mut faults = Vec::new();
        unit.drain_faults(&mut |fault| faults.push(fault));
        assert!(faults.is_empty(), "{faults:?}");
    }
}

/// An update firmware left under way finishes before the hand-off writes its
/// own, which a unit ignores meanwhile.
#[test]
fn the_hand_off_waits_out_an_update_firmware_left_under_way() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, QEMU);
    model.firmware_left_running();
    let clock = clock();
    let _unit = Smmuv3Unit::new(&model, &frames, &clock).unwrap();
    assert!(model.aborting());
}

/// A unit in service-failure mode translates nothing whatever is
/// acknowledged, so it is refused.
#[test]
fn a_unit_in_service_failure_mode_is_refused() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, QEMU);
    model.fail_service();
    let clock = clock();
    assert_eq!(
        Smmuv3Unit::new(&model, &frames, &clock).err(),
        Some(IommuError::Hardware)
    );
}

/// A stage 2 leaf is execute-never at every privilege: `XN[1]` alone, as
/// `XN[1:0] = 0b11` lets a privileged fetch through where `XNX` is present.
#[test]
fn a_stage_2_leaf_is_execute_never_at_every_privilege() {
    use tairix_kernel_iommu_api::{Access, PteFormat};
    let leaf = format::ArmTables {
        stage: Stage::Second,
        root_order: 0,
    }
    .leaf(0x8000_0000, 0, Access::READ_WRITE);
    assert_eq!(leaf >> 53 & 0b11, 0b10);
    let leaf = format::ArmTables {
        stage: Stage::First,
        root_order: 0,
    }
    .leaf(0x8000_0000, 0, Access::READ_WRITE);
    assert_eq!(leaf >> 53 & 0b11, 0b11, "stage 1's PXN and UXN");
}

/// A stream silenced without the unit's confirmation still holds its domain,
/// and attached back to it is translated again rather than left silent.
#[test]
fn a_stream_silenced_unconfirmed_is_translated_again_when_attached_back() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, QEMU);
    let clock = clock();
    let unit = Smmuv3Unit::new(&model, &frames, &clock).unwrap();
    unit.enable().unwrap();
    let domain = unit.create_domain().unwrap();
    unit.map(domain, 0x1000, PAGES[0], 0x1000, Access::READ_WRITE)
        .unwrap();
    unit.attach(STREAMS[0], domain).unwrap();
    model.quirk(Quirks {
        stall_syncs: true,
        ..Quirks::default()
    });
    assert_eq!(unit.silence(STREAMS[0]), Err(IommuError::Unconfirmed));
    model.quirk(Quirks::default());
    unit.attach(STREAMS[0], domain).unwrap();
    assert_eq!(model.access(STREAMS[0], 0x1000, true), Some(PAGES[0]));
}

/// An attach whose last sync the unit never completes is taken back, so the
/// stream is left blocked rather than translating unconfirmed.
#[test]
fn an_attach_the_unit_cannot_confirm_is_taken_back() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, QEMU);
    let clock = clock();
    let unit = Smmuv3Unit::new(&model, &frames, &clock).unwrap();
    unit.enable().unwrap();
    let domain = unit.create_domain().unwrap();
    unit.map(domain, 0x1000, PAGES[0], 0x1000, Access::READ_WRITE)
        .unwrap();
    model.quirk(Quirks {
        syncs_before_stall: Some(1),
        ..Quirks::default()
    });
    assert_eq!(
        unit.attach(STREAMS[0], domain),
        Err(IommuError::Unconfirmed)
    );
    model.quirk(Quirks::default());
    assert_eq!(model.access(STREAMS[0], 0x1000, true), None);
    unit.block(STREAMS[0]).unwrap();
    unit.destroy_domain(domain).unwrap();
}

/// A refused ATS translation request is blocked, as every family classes it;
/// a device presenting an address as translated is classed as having done
/// so; a table walk the memory system aborted is no device's fault to class.
#[test]
fn each_event_is_classed_as_every_family_classes_its_cause() {
    let event = |kind: u64| [kind | (0x0010 << 32), 0, 0x4000_1234, 0];
    let reason = |kind| format::fault(&event(kind)).map(|fault| fault.reason);
    assert_eq!(reason(0x05), Some(FaultReason::Blocked));
    assert_eq!(reason(0x07), Some(FaultReason::Translated));
    assert_eq!(reason(0x0B), Some(FaultReason::Other(0x0B)));
    assert_eq!(reason(0x01), Some(FaultReason::Malformed));
    assert_eq!(
        format::fault(&event(0x05)).map(|fault| fault.iova),
        Some(0x4000_1000)
    );
}
