extern crate std;

use core::sync::atomic::{AtomicU64, Ordering};
use std::vec::Vec;

use tairix_kernel_iommu_api::conformance::{self, Fixture, TranslationProbe};
use tairix_kernel_iommu_api::hostmem::HostFrames;
use tairix_kernel_iommu_api::{
    Clock, Domain, Fault, FaultReason, FaultRoute, IommuError, IommuUnit,
};

use super::*;
use crate::model::{cause, op, Features, Interrupts, Model, Quirks};

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

/// QEMU's `virt` `iommu-sys`: both stages in every mode, extended contexts,
/// every directory depth, wired and message-signalled interrupts, 56 bits of
/// physical address.
const QEMU: Features = Features {
    second_stage: true,
    modes: [true; 3],
    extended: true,
    depths: [true; 3],
    interrupts: Interrupts::Both,
    physical_bits: 56,
    hpm: true,
};

fn fixture() -> Fixture {
    Fixture {
        streams: STREAMS,
        pages: PAGES,
    }
}

fn driven<'m>(
    model: &'m Model<'m>,
    frames: &'m HostFrames,
    clock: &'m Ticking,
) -> RiscvUnit<'m, &'m Model<'m>> {
    RiscvUnit::new(model, frames, None, clock, Signalling::Wired).unwrap()
}

#[test]
fn a_second_stage_unit_with_a_three_level_directory_passes_the_conformance_suite() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, QEMU);
    let clock = clock();
    let unit = driven(&model, &frames, &clock);
    unit.enable().unwrap();
    assert_eq!(model.directory_mode(), regs::DDTP_DEPTHS[2].1);
    conformance::run_all(&unit, &model, &fixture());
    assert!(model.processed(op::IOTINVAL) > 0);
}

#[test]
fn a_first_stage_unit_with_base_contexts_one_level_deep_passes_the_conformance_suite() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(
        &frames,
        Features {
            second_stage: false,
            extended: false,
            depths: [true, false, false],
            ..QEMU
        },
    );
    let clock = clock();
    let unit = driven(&model, &frames, &clock);
    unit.enable().unwrap();
    assert_eq!(model.directory_mode(), regs::DDTP_DEPTHS[0].1);
    let fixture = Fixture {
        streams: [0x10, 0x7F],
        pages: PAGES,
    };
    conformance::run_all(&unit, &model, &fixture);
    assert_eq!(
        unit.attach(0x80, unit.create_domain().unwrap()),
        Err(IommuError::OutOfRange),
        "one table of 128 contexts is the whole directory"
    );
}

/// Firmware may leave the unit passing everything through, its queues on,
/// interrupts pending and an error standing: the hand-off refuses every
/// transaction until translation is ours, and each device is blocked once it
/// is.
#[test]
fn bring_up_refuses_everything_until_translation_is_ours_and_then_blocks_every_device() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, QEMU);
    model.firmware_left_running();
    let clock = clock();
    let unit = driven(&model, &frames, &clock);
    assert_eq!(model.directory_mode(), regs::DDTP_OFF);
    assert!(!model.interrupt_pending());
    assert_eq!(
        model.fctl() & regs::FCTL_GXL,
        0,
        "guest addresses are 64-bit"
    );
    assert_eq!(
        model.word(regs::IOCOUNTINH),
        u64::from(u32::MAX),
        "no counter firmware left running can overflow"
    );
    assert_eq!(model.fctl_written_live(), 0);
    let [commands, faults, requests] = model.queue_controls();
    assert_eq!(
        commands & regs::CQCSR_CMD_ILL,
        0,
        "the error firmware left is gone"
    );
    assert_ne!(commands & regs::QUEUE_ON, 0);
    assert_ne!(faults & regs::QUEUE_ON, 0);
    assert_eq!(requests & regs::QUEUE_ON, 0);
    assert_eq!(model.access(STREAMS[0], 0x1000, false), None);
    unit.enable().unwrap();
    assert_eq!(model.access(STREAMS[0], 0x1000, true), None);
    let mut faults = Vec::new();
    unit.drain_faults(&mut |fault| faults.push(fault));
    let blocked = Fault {
        stream: STREAMS[0],
        iova: 0,
        write: false,
        reason: FaultReason::Blocked,
    };
    assert_eq!(
        faults,
        [blocked, blocked],
        "refused while off, and while blocked"
    );
}

/// A silenced device reaches nothing and raises nothing; one past the devices
/// the directory covers is refused.
#[test]
fn a_silenced_device_is_quiet_and_one_past_the_directory_is_refused() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(
        &frames,
        Features {
            depths: [true, true, false],
            ..QEMU
        },
    );
    let clock = clock();
    let unit = driven(&model, &frames, &clock);
    unit.enable().unwrap();
    unit.silence(STREAMS[0]).unwrap();
    assert_eq!(model.access(STREAMS[0], 0x1000, true), None);
    // Two levels of extended contexts resolve fifteen bits.
    let past = 1 << 15;
    let domain = unit.create_domain().unwrap();
    assert_eq!(unit.attach(past, domain), Err(IommuError::OutOfRange));
    assert_eq!(model.access(past, 0x1000, true), None);
    let mut faults = Vec::new();
    unit.drain_faults(&mut |fault| faults.push(fault));
    assert_eq!(faults.len(), 1, "the silenced device recorded nothing");
    assert_eq!(
        (faults[0].stream, faults[0].reason),
        (past, FaultReason::Blocked)
    );
}

/// Only an attach ends silence: blocking a silenced device leaves it quiet.
#[test]
fn blocking_a_silenced_device_keeps_it_silent() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, QEMU);
    let clock = clock();
    let unit = driven(&model, &frames, &clock);
    unit.enable().unwrap();
    unit.silence(STREAMS[0]).unwrap();
    unit.block(STREAMS[0]).unwrap();
    assert_eq!(model.access(STREAMS[0], 0x1000, true), None);
    let mut faults = Vec::new();
    unit.drain_faults(&mut |fault| faults.push(fault));
    assert!(faults.is_empty(), "{faults:?}");
}

/// A silenced device whose attach fails after its context was broken is
/// blocked, not silent, so silencing it again silences it.
#[test]
fn a_silenced_device_whose_attach_failed_can_be_silenced_again() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, QEMU);
    let clock = clock();
    let unit = driven(&model, &frames, &clock);
    unit.enable().unwrap();
    let domain = unit.create_domain().unwrap();
    unit.silence(STREAMS[0]).unwrap();
    model.quirk(Quirks {
        stall_fences: true,
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

/// A device whose block the unit could not confirm still counts as its
/// domain's, so the domain's tables outlive any walk the unit may hold, and a
/// later block confirms it gone.
#[test]
fn an_unconfirmed_block_keeps_the_device_its_domain_s_until_one_is_confirmed() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, QEMU);
    let clock = clock();
    let unit = driven(&model, &frames, &clock);
    unit.enable().unwrap();
    let domain = unit.create_domain().unwrap();
    unit.attach(STREAMS[0], domain).unwrap();
    model.quirk(Quirks {
        stall_fences: true,
        ..Quirks::default()
    });
    assert_eq!(unit.block(STREAMS[0]), Err(IommuError::Unconfirmed));
    assert_eq!(unit.destroy_domain(domain), Err(IommuError::DomainBusy));
    model.quirk(Quirks::default());
    model.take_forgotten(STREAMS[0]);
    unit.block(STREAMS[0]).unwrap();
    assert_eq!(
        model.take_forgotten(STREAMS[0]),
        [false],
        "confirmed this time"
    );
    unit.destroy_domain(domain).unwrap();
}

/// A valid context is never rewritten in place: moving a device from silence
/// to a domain, or from a domain to silence, first makes its context invalid
/// and has the unit forget it, then has the unit confirm the new context's
/// other words while its first still says invalid, and only then makes it
/// valid.
#[test]
fn a_valid_context_is_replaced_only_through_an_invalid_one() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, QEMU);
    let clock = clock();
    let unit = driven(&model, &frames, &clock);
    unit.enable().unwrap();
    let mut domain = Domain::new(&unit, &[]).unwrap();
    unit.silence(STREAMS[0]).unwrap();
    assert_eq!(
        model.take_forgotten(STREAMS[0]),
        [false, true],
        "nothing valid to break; its other words, then its first"
    );
    domain.attach(STREAMS[0]).unwrap();
    assert_eq!(
        model.take_forgotten(STREAMS[0]),
        [false, false, true],
        "broken, its other words, then its first"
    );
    let iova = domain.map(PAGES[0], 0, 0).unwrap();
    assert_eq!(model.access(STREAMS[0], iova, true), Some(PAGES[0]));
    unit.silence(STREAMS[0]).unwrap();
    assert_eq!(model.take_forgotten(STREAMS[0]), [false, false, true]);
    assert_eq!(model.access(STREAMS[0], iova, true), None);
    unit.attach(STREAMS[0], domain.id()).unwrap();
    assert_eq!(model.take_forgotten(STREAMS[0]), [false, false, true]);
    unit.block(STREAMS[0]).unwrap();
    assert_eq!(model.take_forgotten(STREAMS[0]), [false], "invalid alone");
}

/// More commands than the ring holds run through it, so the unit and the
/// family agree on where it wraps.
#[test]
fn the_command_ring_wraps_where_the_unit_does() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, QEMU);
    let clock = clock();
    let unit = driven(&model, &frames, &clock);
    unit.enable().unwrap();
    let domain = unit.create_domain().unwrap();
    let before = model.processed(op::IOFENCE);
    for _ in 0..CommandQueue::SLOTS {
        unit.sync(domain).unwrap();
    }
    assert_eq!(model.processed(op::IOFENCE), before + CommandQueue::SLOTS);
}

/// Devices in different spans of each level link their own tables; one
/// sharing a span links nothing new.
#[test]
fn devices_far_apart_each_link_their_own_tables() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, QEMU);
    let clock = clock();
    let unit = driven(&model, &frames, &clock);
    unit.enable().unwrap();
    let live = frames.live();
    let mut domain = Domain::new(&unit, &[]).unwrap();
    let iova = domain.map(PAGES[0], 0, 0).unwrap();
    let tables = frames.live();
    for device in [0x00_0001, 0x00_0002, 0x00_FF40, 0xFF_0000] {
        domain.attach(device).unwrap();
    }
    // A middle table resolves 2^15 ids and a leaf 2^6: the first two share
    // both, and each of the others needs its own.
    assert_eq!(frames.live(), tables + 6);
    for device in [0x00_0001, 0x00_0002, 0x00_FF40, 0xFF_0000] {
        assert_eq!(model.access(device, iova, true), Some(PAGES[0]));
    }
    drop(domain);
    assert!(frames.live() > live, "the directory's tables are kept");
}

#[test]
fn a_rejected_command_is_reported_and_the_queue_runs_on() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, QEMU);
    let clock = clock();
    let unit = driven(&model, &frames, &clock);
    unit.enable().unwrap();
    let domain = unit.create_domain().unwrap();
    model.quirk(Quirks {
        reject: Some(op::IOTINVAL),
        ..Quirks::default()
    });
    assert_eq!(unit.sync(domain), Err(IommuError::Unconfirmed));
    let [commands, ..] = model.queue_controls();
    assert_eq!(
        commands & regs::CQCSR_CMD_ILL,
        0,
        "the error is acknowledged"
    );
    unit.sync(domain).unwrap();
    unit.destroy_domain(domain).unwrap();
}

/// A unit that never gets past a fence confirms nothing: a domain it could
/// not confirm forgotten keeps its tables and its id, and a context whose
/// words it never confirmed is never made live.
#[test]
fn a_unit_that_never_completes_a_fence_confirms_nothing() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, QEMU);
    let clock = clock();
    let unit = driven(&model, &frames, &clock);
    unit.enable().unwrap();
    let domain = unit.create_domain().unwrap();
    model.quirk(Quirks {
        stall_fences: true,
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
        "its first word never named the domain"
    );
    assert_eq!(
        unit.destroy_domain(domain),
        Err(IommuError::Unconfirmed),
        "no device holds the domain; the unit still confirms nothing"
    );
}

#[test]
fn a_unit_this_family_cannot_drive_is_refused() {
    struct Altered<'m> {
        model: &'m Model<'m>,
        capabilities: u64,
        window: usize,
    }
    impl Registers for Altered<'_> {
        fn read32(&self, offset: usize) -> Result<u32, IommuError> {
            (&self.model).read32(offset)
        }
        fn write32(&self, offset: usize, value: u32) -> Result<(), IommuError> {
            (&self.model).write32(offset, value)
        }
        fn read64(&self, offset: usize) -> Result<u64, IommuError> {
            if offset == regs::CAPABILITIES {
                return Ok(self.capabilities);
            }
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
    let caps = (&model).read64(regs::CAPABILITIES).unwrap();
    let clock = clock();
    for (capabilities, window) in [
        ((caps & !0xFF) | 0x20, regs::WINDOW),
        (caps & !(0b111 << 17 | 0b111 << 9), regs::WINDOW),
        (caps, regs::WINDOW / 2),
    ] {
        let altered = Altered {
            model: &model,
            capabilities,
            window,
        };
        assert_eq!(
            RiscvUnit::new(altered, &frames, None, &clock, Signalling::Wired).err(),
            Some(IommuError::OutOfRange),
            "{capabilities:#x}, window {window:#x}"
        );
    }
    for (quirks, what) in [
        (
            Quirks {
                big_endian: true,
                ..Quirks::default()
            },
            "a unit fixed to big-endian",
        ),
        (
            Quirks {
                gxl_stuck: true,
                ..Quirks::default()
            },
            "a unit fixed to a 32-bit guest's addresses",
        ),
        (
            Quirks {
                queue_log2_max: Some(6),
                ..Quirks::default()
            },
            "a unit whose queues hold 128 entries at most",
        ),
    ] {
        let fixed = Model::new(&frames, QEMU);
        fixed.quirk(quirks);
        assert_eq!(
            RiscvUnit::new(&fixed, &frames, None, &clock, Signalling::Wired).err(),
            Some(IommuError::OutOfRange),
            "{what}"
        );
    }
    let depthless = Model::new(
        &frames,
        Features {
            depths: [false; 3],
            ..QEMU
        },
    );
    assert_eq!(
        RiscvUnit::new(&depthless, &frames, None, &clock, Signalling::Wired).err(),
        Some(IommuError::OutOfRange),
        "a unit taking no directory"
    );
}

/// The mode a domain walks is the shallowest wide enough for an identity
/// window anywhere the unit reaches: the second stage's root resolving two
/// bits more, a first-stage address only the half below its sign bit. The
/// highest IOVA a domain hands out reaches the device.
#[test]
fn the_walk_is_as_shallow_as_the_physical_address_space_allows() {
    let clock = clock();
    for (features, bits) in [
        (
            Features {
                physical_bits: 40,
                ..QEMU
            },
            41,
        ),
        (
            Features {
                physical_bits: 44,
                ..QEMU
            },
            50,
        ),
        (
            Features {
                physical_bits: 56,
                ..QEMU
            },
            59,
        ),
        (
            Features {
                physical_bits: 56,
                modes: [true, true, false],
                ..QEMU
            },
            50,
        ),
        (
            Features {
                second_stage: false,
                physical_bits: 44,
                ..QEMU
            },
            47,
        ),
        (
            Features {
                second_stage: false,
                physical_bits: 56,
                ..QEMU
            },
            56,
        ),
    ] {
        let frames = HostFrames::new(0x1_0000_0000);
        let model = Model::new(&frames, features);
        let unit = RiscvUnit::new(&model, &frames, None, &clock, Signalling::Wired).unwrap();
        assert_eq!(unit.profile().reach.input_bits, bits);
        unit.enable().unwrap();
        let mut domain = Domain::new(&unit, &[]).unwrap();
        domain.attach(STREAMS[0]).unwrap();
        let iova = domain.map(PAGES[0], 0, 0).unwrap();
        assert_eq!(model.access(STREAMS[0], iova, false), Some(PAGES[0]));
    }
}

/// The unit signals as it was taken over to, and only a route that way is
/// taken: the signalling mode is never changed while it translates or a queue
/// runs. Nothing is written for a route refused.
#[test]
fn a_route_is_taken_only_as_the_unit_was_taken_over_to_signal() {
    let clock = clock();
    let message = FaultRoute::Message {
        address: 0x2800_0040,
        data: 0x41,
    };
    let frames = HostFrames::new(0x1_0000_0000);
    let wired = Model::new(
        &frames,
        Features {
            interrupts: Interrupts::Wired,
            ..QEMU
        },
    );
    let unit = driven(&wired, &frames, &clock);
    unit.enable().unwrap();
    assert_eq!(unit.route_faults(message), Err(IommuError::OutOfRange));
    assert_eq!(
        unit.route_faults(FaultRoute::Wired { place: 16 }),
        Err(IommuError::OutOfRange),
        "past the sixteen vectors"
    );
    assert_eq!(wired.word(regs::MSI_CFG_TBL), 0);
    unit.route_faults(FaultRoute::Wired { place: 2 }).unwrap();
    assert_ne!(wired.fctl() & regs::FCTL_WSI, 0);
    assert_eq!(wired.word(regs::ICVEC), 0x2222);
    let [_, faults, _] = wired.queue_controls();
    assert_ne!(faults & regs::QUEUE_IE, 0);
    assert_eq!(wired.fctl_written_live(), 0);

    let frames = HostFrames::new(0x1_0000_0000);
    let sending = Model::new(&frames, QEMU);
    let unit = RiscvUnit::new(&sending, &frames, None, &clock, Signalling::Message).unwrap();
    unit.enable().unwrap();
    assert_eq!(
        unit.route_faults(FaultRoute::Wired { place: 0 }),
        Err(IommuError::OutOfRange),
        "taken over to send messages"
    );
    for address in [0x2800_0042, 1 << 56] {
        assert_eq!(
            unit.route_faults(FaultRoute::Message {
                address,
                data: 0x41
            }),
            Err(IommuError::OutOfRange),
            "{address:#x} is no address the unit can send to"
        );
    }
    unit.route_faults(message).unwrap();
    assert_eq!(sending.fctl() & regs::FCTL_WSI, 0);
    assert_eq!(sending.word(regs::MSI_CFG_TBL), 0x2800_0040);
    assert_eq!(sending.word(regs::MSI_CFG_TBL + 8), 0x41);
    assert_eq!(sending.word(regs::ICVEC), 0);
    assert_eq!(sending.fctl_written_live(), 0);

    let frames = HostFrames::new(0x1_0000_0000);
    let unwired = Model::new(
        &frames,
        Features {
            interrupts: Interrupts::Messages,
            ..QEMU
        },
    );
    let unit = driven(&unwired, &frames, &clock);
    assert_eq!(
        unit.route_faults(FaultRoute::Wired { place: 0 }),
        Err(IommuError::OutOfRange),
        "a unit with no wires"
    );
}

/// What a firmware's translation left in the unit's caches is gone before
/// any directory is made live, the depth probe's included, so no device ever
/// reaches memory through it.
#[test]
fn nothing_firmware_left_cached_is_live_once_a_directory_is() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, QEMU);
    model.firmware_left_running();
    model.firmware_cached(STREAMS[0], FIRST_DOMAIN);
    let clock = clock();
    let unit = driven(&model, &frames, &clock);
    assert!(!model.stale_exposed());
    unit.enable().unwrap();
    assert_eq!(model.access(STREAMS[0], 0, true), None);
}

/// A unit with no performance counters ignores the write that stops them.
#[test]
fn a_unit_without_counters_is_taken_over_alike() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, Features { hpm: false, ..QEMU });
    let clock = clock();
    let unit = driven(&model, &frames, &clock);
    unit.enable().unwrap();
    assert_eq!(model.word(regs::IOCOUNTINH), 0);
}

/// Each record becomes the fault it reports, in order, a page fault told
/// apart by the device's domain: denied where it maps the page without the
/// access, unmapped otherwise. A drain says when more remain; an overflow is
/// acknowledged and recording resumes.
#[test]
fn records_drain_as_faults_in_batches_and_an_overflow_is_acknowledged() {
    use tairix_kernel_iommu_api::Access;
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, QEMU);
    let clock = clock();
    let unit = driven(&model, &frames, &clock);
    unit.enable().unwrap();
    let domain = unit.create_domain().unwrap();
    unit.attach(7, domain).unwrap();
    let (writable, readable) = (0x4000_0000, 0x4000_1000);
    unit.map(domain, writable, PAGES[0], 0x1000, Access::READ_WRITE)
        .unwrap();
    unit.map(domain, readable, PAGES[1], 0x1000, Access::READ)
        .unwrap();
    unit.route_faults(FaultRoute::Wired { place: 0 }).unwrap();
    model.raise(1, cause::WRITE_GUEST_PAGE, 7, readable + 0x234, true);
    model.raise(1, cause::WRITE_GUEST_PAGE, 7, writable, true);
    model.raise(1, cause::READ_GUEST_PAGE, 7, 0x7000_5678, false);
    model.raise(1, cause::DDT_INVALID, 9, 0, false);
    assert!(model.interrupt_pending());
    let mut faults = Vec::new();
    assert!(!unit.drain_faults(&mut |fault| faults.push(fault)));
    assert!(!model.interrupt_pending(), "the interrupt is acknowledged");
    let fault = |iova, write, reason| Fault {
        stream: 7,
        iova,
        write,
        reason,
    };
    assert_eq!(
        faults,
        [
            fault(readable, true, FaultReason::Denied),
            fault(writable, true, FaultReason::Unmapped),
            fault(0x7000_5000, false, FaultReason::Unmapped),
            Fault {
                stream: 9,
                iova: 0,
                write: false,
                reason: FaultReason::Blocked,
            },
        ]
    );
    model.raise(
        FAULT_QUEUE_RECORDS as usize + 3,
        cause::DDT_INVALID,
        9,
        0,
        false,
    );
    let mut drained = 0;
    while unit.drain_faults(&mut |_| drained += 1) {}
    assert_eq!(
        drained,
        FAULT_QUEUE_RECORDS as usize - 1,
        "a full queue keeps one slot free"
    );
    model.raise(1, cause::DDT_INVALID, 9, 0, false);
    let mut after = Vec::new();
    unit.drain_faults(&mut |fault| after.push(fault));
    assert_eq!(
        after.len(),
        1,
        "the overflow was acknowledged and recording resumed"
    );
}

/// A first-stage address with its top bit set is sign-extended by the unit,
/// so no domain maps one: the device could never use it.
#[test]
fn a_first_stage_domain_maps_only_the_half_below_the_sign_bit() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(
        &frames,
        Features {
            second_stage: false,
            physical_bits: 32,
            ..QEMU
        },
    );
    let clock = clock();
    let unit = driven(&model, &frames, &clock);
    assert_eq!(unit.profile().reach.input_bits, 38, "Sv39");
    let domain = unit.create_domain().unwrap();
    let read = tairix_kernel_iommu_api::Access::READ;
    assert_eq!(
        unit.map(domain, 1 << 38, PAGES[0], 0x1000, read),
        Err(IommuError::OutOfRange)
    );
    unit.map(domain, (1 << 38) - 0x1000, PAGES[0], 0x1000, read)
        .unwrap();
}

#[test]
fn a_mapping_a_device_could_write_but_not_read_is_refused() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, QEMU);
    let clock = clock();
    let unit = driven(&model, &frames, &clock);
    let domain = unit.create_domain().unwrap();
    assert_eq!(
        unit.map(
            domain,
            0x1000,
            PAGES[0],
            0x1000,
            tairix_kernel_iommu_api::Access::WRITE
        ),
        Err(IommuError::OutOfRange)
    );
}

/// What a table entry's encoder writes, its decoder reads back, at every
/// level a leaf may sit at and as a pointer above.
#[test]
fn a_table_entry_reads_back_as_written_at_every_level_of_either_stage() {
    use tairix_kernel_iommu_api::{Access, Pte, PteFormat};
    for stage in [Stage::First, Stage::Second] {
        let format = RiscvTables { stage };
        for level in 0..=2 {
            let phys = 0x8_0000_0000 + (1 << (12 + 9 * level));
            for access in [Access::READ, Access::READ_WRITE] {
                assert_eq!(
                    format.decode(format.leaf(phys, level, access), level),
                    Pte::Leaf(phys, access)
                );
            }
            let table = 0x8_0000_3000;
            let pointer = format.decode(format.table(table, level), level);
            if level == 0 {
                assert_eq!(pointer, Pte::Absent, "no table below the last level");
            } else {
                assert_eq!(pointer, Pte::Table(table));
            }
        }
        assert_eq!(format.decode(0, 1), Pte::Absent);
    }
    assert_eq!(
        RiscvTables {
            stage: Stage::Second
        }
        .root_order(),
        2
    );
    assert_eq!(
        RiscvTables {
            stage: Stage::First
        }
        .root_order(),
        0
    );
}

/// A record's cause decodes to the reason the unit meant: an ATS translation
/// request asks for a translation rather than carrying one, a device beyond
/// the directory is blocked, and every misaligned or access fault names its
/// address.
#[test]
fn a_record_decodes_to_the_reason_its_cause_and_type_mean() {
    let record = |cause: u64, kind: u64| {
        format::record(&[cause | kind << 34 | 0x42 << 40, 0, 0x1234_5678, 0])
    };
    assert_eq!(
        record(260, 6).cause,
        format::Cause::Known(FaultReason::Translated, true)
    );
    for kind in [2, 3, 8] {
        assert_eq!(
            record(260, kind).cause,
            format::Cause::Known(FaultReason::Blocked, false),
            "type {kind}"
        );
    }
    assert_eq!(
        record(0, 1).cause,
        format::Cause::Known(FaultReason::Other(0), true)
    );
    assert_eq!(record(0, 1).iova, 0x1234_5000);
}

/// An entry the unit would fault on decodes as nothing mapped: writable but
/// unreadable is reserved, and execute-only reads nothing.
#[test]
fn an_entry_the_unit_faults_on_decodes_as_absent() {
    use tairix_kernel_iommu_api::{Pte, PteFormat};
    let format = RiscvTables {
        stage: Stage::Second,
    };
    for permissions in [0b0100, 0b1000, 0b1100] {
        let entry = 1 | permissions | 0x8_0000 << 10;
        assert_eq!(format.decode(entry, 0), Pte::Absent, "{permissions:#06b}");
    }
}

/// A context's tag holds the stage's id width and no more: a wider id would
/// spill into the walk mode or the next field.
#[test]
fn a_context_is_never_made_for_an_id_wider_than_its_tag() {
    let root = 0x8000_0000;
    assert!(format::context(Stage::Second, (1 << 16) - 1, root, 8, false).is_some());
    assert!(format::context(Stage::Second, 1 << 16, root, 8, false).is_none());
    assert!(format::context(Stage::First, (1 << 20) - 1, root, 8, false).is_some());
    assert!(format::context(Stage::First, 1 << 20, root, 8, false).is_none());
}

/// A device blocked without the unit's confirmation still holds its domain,
/// and attached back to it is translated again rather than left blocked.
#[test]
fn a_device_blocked_unconfirmed_is_translated_again_when_attached_back() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, QEMU);
    let clock = clock();
    let unit = driven(&model, &frames, &clock);
    unit.enable().unwrap();
    let domain = unit.create_domain().unwrap();
    unit.map(domain, 0x1000, PAGES[0], 0x1000, Access::READ_WRITE)
        .unwrap();
    unit.attach(STREAMS[0], domain).unwrap();
    model.quirk(Quirks {
        stall_fences: true,
        ..Quirks::default()
    });
    assert_eq!(unit.block(STREAMS[0]), Err(IommuError::Unconfirmed));
    model.quirk(Quirks::default());
    unit.attach(STREAMS[0], domain).unwrap();
    assert_eq!(model.access(STREAMS[0], 0x1000, true), Some(PAGES[0]));
}

/// An attach whose fence the unit never completes is taken back, so the
/// device is left blocked rather than translating unconfirmed.
#[test]
fn an_attach_the_unit_cannot_confirm_is_taken_back() {
    let frames = HostFrames::new(0x1_0000_0000);
    let model = Model::new(&frames, QEMU);
    let clock = clock();
    let unit = driven(&model, &frames, &clock);
    unit.enable().unwrap();
    let domain = unit.create_domain().unwrap();
    unit.map(domain, 0x1000, PAGES[0], 0x1000, Access::READ_WRITE)
        .unwrap();
    model.quirk(Quirks {
        fences_before_stall: Some(1),
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
