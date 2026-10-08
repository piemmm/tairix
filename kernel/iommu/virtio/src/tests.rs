extern crate std;

use core::sync::atomic::{AtomicU64, Ordering};
use std::vec::Vec;

use tairix_kernel_iommu_api::conformance::{self, Fixture, TranslationProbe};
use tairix_kernel_iommu_api::hostmem::HostFrames;
use tairix_kernel_iommu_api::{FAULT_BATCH, IO_PAGE_SIZE};

use super::*;
use crate::model::{Behaviour, Device, ModelTransport, Reserved};
use tairix_kernel_iommu_api::FrameRun;

const PAGE: u64 = IO_PAGE_SIZE;
const INPUT: (u64, u64) = (0, (1 << 48) - 1);
const ENDPOINTS: [u32; 3] = [0x10, 0x18, 0x20];

/// A clock that moves on with every reading, so a wait the device never
/// ends runs out.
struct Ticking(AtomicU64);

impl Clock for Ticking {
    fn now_ns(&self) -> u64 {
        self.0.fetch_add(1_000_000, Ordering::Relaxed)
    }
}

fn clock() -> Ticking {
    Ticking(AtomicU64::new(0))
}

type Unit<'d, 'f> = VirtioIommuUnit<'f, ModelTransport<'d, 'f>>;

fn unit<'d, 'f>(
    device: &'d Device<'f>,
    frames: &'f HostFrames,
    clock: &'f Ticking,
) -> Unit<'d, 'f> {
    VirtioIommuUnit::new(device.transport(), frames, clock, None, Signalling::Wired).unwrap()
}

fn fixture() -> Fixture {
    Fixture {
        streams: [0x10, 0x18],
        pages: [0x8000_0000, 0x8000_1000],
    }
}

fn drained(unit: &dyn IommuUnit) -> Vec<Fault> {
    let mut faults = Vec::new();
    while unit.drain_faults(&mut |fault| faults.push(fault)) {}
    faults
}

#[test]
fn the_unit_passes_the_conformance_suite() {
    let frames = HostFrames::new(0x1_0000_0000);
    let clock = clock();
    let device = Device::new(&frames, &ENDPOINTS, INPUT, Behaviour::Correct);
    let unit = unit(&device, &frames, &clock);
    unit.enable().unwrap();
    conformance::run_all(&unit, &device, &fixture());
    assert_eq!(device.domains(), 0, "every domain left the device");
    assert_eq!(frames.live(), 0, "every shadow was freed");
}

/// An attach whose replay the device refuses is undone, so the endpoint is
/// never left translating through a domain missing its mappings.
#[test]
fn an_attach_whose_replay_is_refused_leaves_the_endpoint_holding_nothing() {
    let frames = HostFrames::new(0x1_0000_0000);
    let clock = clock();
    let device = Device::new(&frames, &ENDPOINTS, INPUT, Behaviour::Correct);
    let unit = unit(&device, &frames, &clock);
    unit.enable().unwrap();
    let domain = unit.create_domain().unwrap();
    unit.attach(0x10, domain).unwrap();
    let iova = INPUT.0 + 0x10_0000;
    unit.map(domain, iova, 0x8000_0000, 0x1000, Access::READ_WRITE)
        .unwrap();
    unit.block(0x10).unwrap();
    assert_eq!(device.domains(), 0, "the device let the empty domain go");
    device.behave(Behaviour::RefusesMaps);
    assert_eq!(unit.attach(0x10, domain), Err(IommuError::Hardware));
    assert_eq!(device.access(0x10, iova, false), None);
    unit.destroy_domain(domain)
        .expect("no endpoint holds what the attach undid");
}

/// A last endpoint holds its domain until the device confirms the domain
/// emptied: a refused unmap leaves the domain held, so it is neither freed
/// with its mappings in it nor skipped when the block is tried again.
#[test]
fn a_last_endpoint_holds_its_domain_until_the_device_empties_it() {
    let frames = HostFrames::new(0x1_0000_0000);
    let clock = clock();
    let device = Device::new(&frames, &ENDPOINTS, INPUT, Behaviour::KeepsDomains);
    let unit = unit(&device, &frames, &clock);
    unit.enable().unwrap();
    let domain = unit.create_domain().unwrap();
    unit.attach(0x10, domain).unwrap();
    device.behave(Behaviour::RefusesUnmaps);
    assert_eq!(unit.block(0x10), Err(IommuError::Hardware));
    assert_eq!(unit.destroy_domain(domain), Err(IommuError::DomainBusy));
    device.behave(Behaviour::KeepsDomains);
    unit.block(0x10).expect("emptied at last");
    unit.destroy_domain(domain)
        .expect("no endpoint holds it now");
}

/// A replay the device took part of, and then would not drop, leaves it
/// holding some of the domain: an unmap still reaches it, and the next attach
/// empties the domain before sending every mapping again.
#[test]
fn a_replay_the_device_could_not_undo_is_still_unmapped_on_it() {
    let frames = HostFrames::new(0x1_0000_0000);
    let clock = clock();
    let device = Device::new(&frames, &ENDPOINTS, INPUT, Behaviour::KeepsDomains);
    let unit = unit(&device, &frames, &clock);
    unit.enable().unwrap();
    let domain = unit.create_domain().unwrap();
    let iovas = [0x10_0000, 0x20_0000, 0x30_0000];
    for iova in &iovas[..2] {
        unit.map(domain, *iova, 0x8000_0000, PAGE, Access::READ_WRITE)
            .unwrap();
    }
    device.behave(Behaviour::RefusesUnmaps);
    device.fail_maps_after(1);
    assert_eq!(unit.attach(0x10, domain), Err(IommuError::Unconfirmed));
    assert_eq!(device.mappings(domain.0), 1, "it took one and kept it");
    device.behave(Behaviour::KeepsDomains);
    for iova in &iovas[..2] {
        unit.unmap(domain, *iova, PAGE).unwrap();
    }
    assert_eq!(device.mappings(domain.0), 0);
    unit.map(domain, iovas[2], 0x8000_0000, PAGE, Access::READ_WRITE)
        .unwrap();
    unit.attach(0x20, domain).unwrap();
    assert_eq!(device.mappings(domain.0), 1);
    assert_eq!(device.access(0x20, iovas[2], true), Some(0x8000_0000));
    unit.block(0x10).expect("its hold let go");
    unit.block(0x20).unwrap();
    assert_eq!(device.mappings(domain.0), 0);
    unit.destroy_domain(domain).unwrap();
}

/// A device may keep a domain whose last endpoint detached: emptied as it
/// goes, it takes its mappings back whole when attached again.
#[test]
fn the_unit_passes_the_suite_on_a_device_that_keeps_empty_domains() {
    let frames = HostFrames::new(0x1_0000_0000);
    let clock = clock();
    let device = Device::new(&frames, &ENDPOINTS, INPUT, Behaviour::KeepsDomains);
    let unit = unit(&device, &frames, &clock);
    unit.enable().unwrap();
    conformance::run_all(&unit, &device, &fixture());
}

#[test]
fn bring_up_takes_every_feature_used_and_turns_bypass_off() {
    let frames = HostFrames::new(0x1_0000_0000);
    let clock = clock();
    let device = Device::new(&frames, &ENDPOINTS, INPUT, Behaviour::Correct);
    let unit = unit(&device, &frames, &clock);
    let wanted = VIRTIO_F_VERSION_1
        | tairix_virtio::VIRTIO_F_ACCESS_PLATFORM
        | feature::INPUT_RANGE
        | feature::DOMAIN_RANGE
        | feature::MAP_UNMAP
        | feature::PROBE
        | feature::BYPASS_CONFIG;
    assert_eq!(
        device.accepted(),
        wanted,
        "every feature used and offered, no more"
    );
    assert_eq!(device.bypass(), 0, "unattached endpoints are blocked");
    assert_ne!(device.status() & DeviceStatus::DRIVER_OK, 0);
    assert_eq!(unit.profile().tables, Tables::Kept);
    assert_eq!(unit.profile().reach.input_bits, 48);
    assert_eq!(device.access(0x10, 0x4000_0000, true), None);
    assert!(
        drained(&unit)
            .iter()
            .any(|fault| fault.stream == 0x10 && fault.reason == FaultReason::Blocked),
        "the refusal is reported"
    );
}

/// The older bypass feature is never taken, so an unattached endpoint stays
/// blocked on a device that offers nothing else.
#[test]
fn bypass_is_never_accepted_where_only_the_older_feature_is_offered() {
    let frames = HostFrames::new(0x1_0000_0000);
    let clock = clock();
    let device = Device::new(&frames, &ENDPOINTS, INPUT, Behaviour::LegacyBypass);
    let _unit = unit(&device, &frames, &clock);
    assert_eq!(device.accepted() & feature::BYPASS, 0);
    assert_eq!(device.access(0x18, 0x1000, false), None);
}

#[test]
fn a_device_that_keeps_bypassing_is_refused() {
    let frames = HostFrames::new(0x1_0000_0000);
    let clock = clock();
    let device = Device::new(&frames, &ENDPOINTS, INPUT, Behaviour::BypassStuck);
    let refused =
        VirtioIommuUnit::new(device.transport(), &frames, &clock, None, Signalling::Wired);
    assert_eq!(refused.err(), Some(IommuError::Hardware));
    assert_ne!(
        device.status() & DeviceStatus::FAILED,
        0,
        "told it was given up on"
    );
}

/// A configuration window ending before the bypass field would swallow the
/// write turning bypass off and read it back as off.
#[test]
fn a_device_whose_configuration_window_is_short_is_refused() {
    let frames = HostFrames::new(0x1_0000_0000);
    let clock = clock();
    let device = Device::new(&frames, &ENDPOINTS, INPUT, Behaviour::Correct);
    device.expose_config(config::BYPASS);
    let refused =
        VirtioIommuUnit::new(device.transport(), &frames, &clock, None, Signalling::Wired);
    assert_eq!(refused.err(), Some(IommuError::OutOfRange));
}

/// An answer naming another chain leaves the request with the device, so
/// the attach it carried is held until a confirmed detach and nothing more
/// is sent.
#[test]
fn an_answer_for_another_chain_leaves_the_request_unconfirmed() {
    let frames = HostFrames::new(0x1_0000_0000);
    let clock = clock();
    let device = Device::new(&frames, &ENDPOINTS, INPUT, Behaviour::Correct);
    let unit = unit(&device, &frames, &clock);
    let domain = unit.create_domain().unwrap();
    device.behave(Behaviour::MisnamesChains);
    assert_eq!(unit.attach(0x10, domain), Err(IommuError::Unconfirmed));
    assert_eq!(
        unit.map(domain, 0x4000_0000, 0x9000_0000, PAGE, Access::READ_WRITE),
        Err(IommuError::Unconfirmed)
    );
    assert_eq!(unit.destroy_domain(domain), Err(IommuError::DomainBusy));
}

/// An input range must hold a whole page past the null one: two halves of
/// neighbouring pages translate nothing a domain could map.
#[test]
fn an_input_range_holding_no_whole_page_is_refused() {
    let frames = HostFrames::new(0x1_0000_0000);
    let clock = clock();
    let attempt = |input: (u64, u64)| {
        let device = Device::new(&frames, &ENDPOINTS, input, Behaviour::Correct);
        VirtioIommuUnit::new(device.transport(), &frames, &clock, None, Signalling::Wired).err()
    };
    assert_eq!(attempt((0x1800, 0x27FF)), Some(IommuError::OutOfRange));
    assert_eq!(
        attempt((0, 0xFFF)),
        Some(IommuError::OutOfRange),
        "the null page alone"
    );
    assert_eq!(attempt((0x1800, 0x2FFF)), None);
}

/// Rings the host cannot supply are exhaustion, as the take-over promises,
/// never the device's fault.
#[test]
fn rings_that_cannot_be_had_are_exhaustion() {
    assert_eq!(
        super::queue_error(tairix_virtio::VirtioError::OutOfMemory),
        IommuError::Exhausted
    );
}

#[test]
fn a_device_this_family_cannot_drive_is_refused() {
    let frames = HostFrames::new(0x1_0000_0000);
    let clock = clock();
    let attempt = |device: &Device<'_>| {
        VirtioIommuUnit::new(device.transport(), &frames, &clock, None, Signalling::Wired).err()
    };
    let without = |features: u64| {
        let device = Device::new(&frames, &ENDPOINTS, INPUT, Behaviour::Correct);
        let drivable =
            VIRTIO_F_VERSION_1 | feature::MAP_UNMAP | feature::PROBE | feature::BYPASS_CONFIG;
        device.offer(drivable & !features);
        device
    };
    assert!(attempt(&without(0)).is_none(), "every feature it needs");
    // Without a probe nothing names the IOVA the device's interrupt messages
    // land at, so a domain could be handed it.
    let unprobed = without(0);
    unprobed.probe_size(0);
    for device in [
        without(feature::MAP_UNMAP),
        without(VIRTIO_F_VERSION_1),
        without(feature::PROBE),
        unprobed,
    ] {
        assert_eq!(attempt(&device), Some(IommuError::OutOfRange));
        assert_ne!(device.status() & DeviceStatus::FAILED, 0);
    }
    let coarse = Device::new(&frames, &ENDPOINTS, INPUT, Behaviour::Correct);
    coarse.granule(16);
    assert_eq!(
        attempt(&coarse),
        Some(IommuError::OutOfRange),
        "no 4 KiB page"
    );
    let shallow = Device::new(&frames, &ENDPOINTS, INPUT, Behaviour::Correct);
    shallow.queue_max([1, 256]);
    assert_eq!(
        attempt(&shallow),
        Some(IommuError::OutOfRange),
        "no room for a request"
    );
    let empty = Device::new(&frames, &ENDPOINTS, (0x5000, 0x4FFF), Behaviour::Correct);
    assert_eq!(
        attempt(&empty),
        Some(IommuError::OutOfRange),
        "an empty input range"
    );
}

/// A request answered past its budget is unconfirmed for good, but once its
/// answer lands the channel is the next request's again.
#[test]
fn a_late_answer_frees_the_channel_for_the_next_request() {
    let frames = HostFrames::new(0x1_0000_0000);
    let clock = clock();
    let device = Device::new(&frames, &ENDPOINTS, INPUT, Behaviour::Correct);
    let unit = unit(&device, &frames, &clock);
    let domain = unit.create_domain().unwrap();
    device.behave(Behaviour::Silent);
    assert_eq!(unit.attach(0x10, domain), Err(IommuError::Unconfirmed));
    assert_eq!(
        unit.sync(domain),
        Err(IommuError::Unconfirmed),
        "still the device's"
    );
    device.catch_up();
    assert_eq!(unit.sync(domain), Ok(()));
    assert_eq!(unit.attach(0x18, domain), Ok(()));
    assert_eq!(device.answered(1), 2, "the late attach, then the next");
}

/// A request the device never answers is unconfirmed, and its buffer may
/// still be written, so nothing more is sent.
#[test]
fn an_unanswered_request_is_unconfirmed_and_nothing_more_is_sent() {
    let frames = HostFrames::new(0x1_0000_0000);
    let clock = clock();
    let device = Device::new(&frames, &ENDPOINTS, INPUT, Behaviour::Correct);
    let unit = unit(&device, &frames, &clock);
    let domain = unit.create_domain().unwrap();
    device.behave(Behaviour::Silent);
    assert_eq!(unit.attach(0x10, domain), Err(IommuError::Unconfirmed));
    device.behave(Behaviour::Correct);
    assert_eq!(
        unit.map(domain, 0x4000_0000, 0x9000_0000, PAGE, Access::READ_WRITE),
        Err(IommuError::Unconfirmed)
    );
    assert_eq!(unit.block(0x10), Err(IommuError::Unconfirmed));
    assert_eq!(unit.sync(domain), Err(IommuError::Unconfirmed));
    assert_eq!(unit.enable(), Err(IommuError::Unconfirmed));
    assert_eq!(
        unit.destroy_domain(domain),
        Err(IommuError::DomainBusy),
        "the endpoint may hold it still"
    );
    assert_eq!(device.answered(1), 0);
}

/// A domain whose last endpoint leaves is forgotten by a device like QEMU's,
/// so its next first endpoint has every mapping replayed into it.
#[test]
fn a_domain_is_replayed_when_its_first_endpoint_attaches_again() {
    // QEMU's riscv64 `virt` translates every address, which takes a shadow
    // six levels deep.
    let every_address = (0, u64::MAX);
    for (input, behaviour) in [
        (INPUT, Behaviour::Correct),
        (INPUT, Behaviour::KeepsDomains),
        (every_address, Behaviour::Correct),
    ] {
        let frames = HostFrames::new(0x1_0000_0000);
        let clock = clock();
        let device = Device::new(&frames, &ENDPOINTS, input, behaviour);
        let unit = unit(&device, &frames, &clock);
        let domain = unit.create_domain().unwrap();
        unit.map(
            domain,
            0x4000_0000,
            0x9000_0000,
            2 << 20,
            Access::READ_WRITE,
        )
        .unwrap();
        assert_eq!(
            device.answered(3),
            0,
            "nothing is sent for a domain no endpoint holds"
        );
        unit.attach(0x10, domain).unwrap();
        assert_eq!(
            device.mappings(domain.0),
            1,
            "replayed as its first endpoint attached"
        );
        unit.attach(0x18, domain).unwrap();
        assert_eq!(device.answered(3), 1, "a second endpoint replays nothing");
        unit.block(0x10).unwrap();
        unit.block(0x18).unwrap();
        assert_eq!(device.mappings(domain.0), 0);
        unit.attach(0x18, domain).unwrap();
        assert_eq!(
            device.access(0x18, 0x4010_0123, true),
            Some(0x9010_0123),
            "{behaviour:?}"
        );
        unit.block(0x18).unwrap();
        unit.destroy_domain(domain).unwrap();
    }
}

#[test]
fn what_a_probe_reserves_is_claimed_for_the_stream_once() {
    let frames = HostFrames::new(0x1_0000_0000);
    let clock = clock();
    let device = Device::new(&frames, &ENDPOINTS, INPUT, Behaviour::Correct);
    device.reserve(
        0x10,
        &[Reserved {
            subtype: 1,
            first: 0xFEE0_0000,
            last: 0xFEEF_FFFF,
        }],
    );
    let unit = unit(&device, &frames, &clock);
    for _ in 0..2 {
        let mut claimed = Vec::new();
        unit.reserved_iova(0x10, &mut |range| claimed.push(range))
            .unwrap();
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0], 0xFEE0_0000..0xFEF0_0000);
    }
    assert_eq!(device.answered(5), 1, "probed once");
    let mut phantom = Vec::new();
    unit.reserved_iova(0x99, &mut |range| phantom.push(range))
        .unwrap();
    assert!(
        phantom.is_empty(),
        "an endpoint the device does not have reserves nothing"
    );
    let domain = unit.create_domain().unwrap();
    unit.attach(0x10, domain).unwrap();
    assert_eq!(
        unit.map(domain, 0xFEE0_0000, 0x9000_0000, PAGE, Access::READ_WRITE),
        Err(IommuError::AlreadyMapped),
        "the device refuses a map over what it reserved"
    );
    unit.block(0x10).unwrap();
    unit.destroy_domain(domain).unwrap();
}

#[test]
fn the_input_range_bounds_every_domain() {
    let frames = HostFrames::new(0x1_0000_0000);
    let clock = clock();
    let device = Device::new(
        &frames,
        &ENDPOINTS,
        (0x1_0000, 0x6FFF_FFFF),
        Behaviour::Correct,
    );
    let unit = unit(&device, &frames, &clock);
    let mut claimed = Vec::new();
    unit.reserved_iova(0x20, &mut |range| claimed.push(range))
        .unwrap();
    assert_eq!(claimed, [0..0x1_0000, 0x7000_0000..0x8000_0000]);
    let domain = unit.create_domain().unwrap();
    for iova in [0xF000, 0x6FFF_F000 + PAGE] {
        assert_eq!(
            unit.map(domain, iova, 0x9000_0000, PAGE, Access::READ_WRITE),
            Err(IommuError::OutOfRange),
            "{iova:#x}"
        );
    }
    unit.destroy_domain(domain).unwrap();
}

#[test]
fn faults_are_classed_by_the_endpoints_domain() {
    let frames = HostFrames::new(0x1_0000_0000);
    let clock = clock();
    let device = Device::new(&frames, &ENDPOINTS, INPUT, Behaviour::Correct);
    let unit = unit(&device, &frames, &clock);
    let domain = unit.create_domain().unwrap();
    unit.map(domain, 0x4000_0000, 0x9000_0000, PAGE, Access::READ)
        .unwrap();
    unit.attach(0x10, domain).unwrap();
    assert_eq!(device.access(0x18, 0x4000_0000, false), None);
    assert_eq!(device.access(0x10, 0x4000_0010, true), None);
    assert_eq!(device.access(0x10, 0x5000_0000, false), None);
    let reasons: Vec<_> = drained(&unit)
        .iter()
        .map(|fault| (fault.stream, fault.iova, fault.write, fault.reason))
        .collect();
    assert_eq!(
        reasons,
        [
            (0x18, 0x4000_0000, false, FaultReason::Blocked),
            (0x10, 0x4000_0000, true, FaultReason::Denied),
            (0x10, 0x5000_0000, false, FaultReason::Unmapped),
        ]
    );
}

/// Silence is the family's own filter: a device cannot be told to stop
/// reporting, so a silenced endpoint's reports are dropped until an attach
/// gives it an owner again, a block included.
#[test]
fn a_silenced_endpoint_stays_silent_until_it_is_attached_again() {
    let frames = HostFrames::new(0x1_0000_0000);
    let clock = clock();
    let device = Device::new(&frames, &ENDPOINTS, INPUT, Behaviour::Correct);
    let unit = unit(&device, &frames, &clock);
    let domain = unit.create_domain().unwrap();
    unit.attach(0x20, domain).unwrap();
    unit.silence(0x20).unwrap();
    unit.block(0x20).unwrap();
    assert_eq!(device.access(0x20, 0x1000, true), None);
    assert!(drained(&unit).is_empty());
    unit.attach(0x20, domain).unwrap();
    assert_eq!(device.access(0x20, 0x1000, true), None);
    assert_eq!(drained(&unit).len(), 1, "heard again once owned");
    unit.block(0x20).unwrap();
    unit.destroy_domain(domain).unwrap();
}

/// Each report buffer goes back to the device once read, so reports keep
/// arriving however many there are, and one drain takes a queue's worth.
#[test]
fn every_report_buffer_is_handed_back() {
    let frames = HostFrames::new(0x1_0000_0000);
    let clock = clock();
    let device = Device::new(&frames, &ENDPOINTS, INPUT, Behaviour::Correct);
    let unit = unit(&device, &frames, &clock);
    for round in 0..3 {
        for page in 0..256 {
            assert_eq!(device.access(0x18, page * PAGE, false), None);
        }
        let mut seen = Vec::new();
        assert!(
            !unit.drain_faults(&mut |fault| seen.push(fault)),
            "one call drains a queue's worth"
        );
        assert_eq!(seen.len(), 256, "round {round}");
    }
    assert_eq!(device.dropped(), 0);
    for _ in 0..3 * FAULT_BATCH {
        assert_eq!(device.access(0x18, 0, false), None);
    }
    assert_eq!(drained(&unit).len(), 3 * FAULT_BATCH);
}

#[derive(Debug, Eq, PartialEq)]
enum Call {
    Msix(u32, u16, u64, u32),
    MaskMsix(u32, bool),
    Intx(u32, bool),
}

#[derive(Default)]
struct Function(SpinLock<Vec<Call>>);

impl UnitFunction for Function {
    fn route_msi(&self, _: u32, _: u64, _: u32) -> Result<(), IommuError> {
        Err(IommuError::Hardware)
    }

    fn route_msix(
        &self,
        address: u32,
        entry: u16,
        message: u64,
        data: u32,
    ) -> Result<(), IommuError> {
        self.0
            .lock()
            .push(Call::Msix(address, entry, message, data));
        Ok(())
    }

    fn msix_entries(&self, _: u32) -> Result<u16, IommuError> {
        Err(IommuError::Hardware)
    }

    fn mask_msix(&self, address: u32, masked: bool) -> Result<(), IommuError> {
        self.0.lock().push(Call::MaskMsix(address, masked));
        Ok(())
    }

    fn capability_header(&self, _: u32, _: u8) -> Result<Option<u32>, IommuError> {
        Err(IommuError::Hardware)
    }

    fn set_master(&self, _: u32, _: bool) -> Result<(), IommuError> {
        Err(IommuError::Hardware)
    }

    fn set_intx(&self, address: u32, raise: bool) -> Result<(), IommuError> {
        self.0.lock().push(Call::Intx(address, raise));
        Ok(())
    }

    fn virtio_windows(
        &self,
        _: u32,
    ) -> Result<tairix_kernel_iommu_api::VirtioPciWindows, IommuError> {
        Err(IommuError::Hardware)
    }
}

/// A PCI function's reports are raised as it was taken over to: its MSI-X
/// entry, or its INTx pin; a virtio-mmio slot only on its line. Unrouted,
/// the function is masked, and a slot's line cannot be.
#[test]
fn faults_are_routed_as_the_unit_was_taken_over_to_raise_them() {
    let frames = HostFrames::new(0x1_0000_0000);
    let clock = clock();
    let function = Function::default();
    let message = FaultRoute::Message {
        address: 0xFEE0_0000,
        data: 0x41,
    };
    let wired = FaultRoute::Wired { place: 0 };
    for (signalling, routed, refused) in [
        (Signalling::Message, message, wired),
        (Signalling::Wired, wired, message),
    ] {
        let device = Device::new(&frames, &ENDPOINTS, INPUT, Behaviour::Correct);
        let unit = VirtioIommuUnit::new(
            device.transport(),
            &frames,
            &clock,
            Some((&function, 0x0001_0010)),
            signalling,
        )
        .unwrap();
        assert_eq!(unit.route_faults(refused), Err(IommuError::OutOfRange));
        unit.route_faults(routed).unwrap();
        unit.unroute_faults().unwrap();
    }
    assert_eq!(
        *function.0.lock(),
        [
            Call::Msix(0x0001_0010, MSIX_ENTRY, 0xFEE0_0000, 0x41),
            Call::MaskMsix(0x0001_0010, true),
            Call::Intx(0x0001_0010, true),
            Call::Intx(0x0001_0010, false),
        ]
    );
    let device = Device::new(&frames, &ENDPOINTS, INPUT, Behaviour::Correct);
    let slot = unit(&device, &frames, &clock);
    assert_eq!(slot.route_faults(message), Err(IommuError::OutOfRange));
    slot.route_faults(wired).unwrap();
    assert_eq!(
        slot.unroute_faults(),
        Err(IommuError::OutOfRange),
        "nothing can stop a slot's line"
    );
}

/// The event queue raises nothing until its interrupt is routed, and a
/// drain acknowledges what it raised.
#[test]
fn reports_raise_the_interrupt_only_once_routed() {
    let frames = HostFrames::new(0x1_0000_0000);
    let clock = clock();
    let device = Device::new(&frames, &ENDPOINTS, INPUT, Behaviour::Correct);
    let unit = unit(&device, &frames, &clock);
    assert_eq!(device.access(0x10, 0, false), None);
    assert!(!device.raised());
    unit.route_faults(FaultRoute::Wired { place: 0 }).unwrap();
    assert_eq!(
        drained(&unit).len(),
        1,
        "a report from before is drained still"
    );
    assert_eq!(device.access(0x10, 0, false), None);
    assert!(device.raised());
    assert_eq!(drained(&unit).len(), 1);
    assert!(!device.raised(), "acknowledged as drained");
}

/// An unmap names whole mappings: a hole is refused before the device is
/// asked, and half of one map the device refuses whole, leaving it mapped.
#[test]
fn an_unmap_must_name_whole_mappings() {
    let frames = HostFrames::new(0x1_0000_0000);
    let clock = clock();
    let device = Device::new(&frames, &ENDPOINTS, INPUT, Behaviour::Correct);
    let unit = unit(&device, &frames, &clock);
    let domain = unit.create_domain().unwrap();
    unit.attach(0x10, domain).unwrap();
    unit.map(
        domain,
        0x4000_0000,
        0x9000_0000,
        2 * PAGE,
        Access::READ_WRITE,
    )
    .unwrap();
    unit.map(domain, 0x4000_2000, 0x9100_0000, PAGE, Access::READ_WRITE)
        .unwrap();
    assert_eq!(
        unit.unmap(domain, 0x4000_3000, PAGE),
        Err(IommuError::NotMapped)
    );
    assert_eq!(device.answered(4), 0, "a hole asks the device nothing");
    assert_eq!(
        unit.unmap(domain, 0x4000_0000, PAGE),
        Err(IommuError::Split)
    );
    assert_eq!(
        unit.unmap(domain, 0x4000_1000, 2 * PAGE),
        Err(IommuError::Split)
    );
    assert_eq!(
        unit.unmap(domain, 0x4000_0000, 0),
        Err(IommuError::OutOfRange)
    );
    assert_eq!(
        device.answered(4),
        0,
        "a split or empty unmap asks the device nothing"
    );
    assert_eq!(device.access(0x10, 0x4000_1000, false), Some(0x9000_1000));
    unit.unmap(domain, 0x4000_0000, 3 * PAGE).unwrap();
    assert_eq!(device.mappings(domain.0), 0, "one unmap of both mappings");
    unit.sync(domain).unwrap();
    assert_eq!(device.access(0x10, 0x4000_2000, false), None);
    unit.block(0x10).unwrap();
    unit.destroy_domain(domain).unwrap();
}

/// Unmapping one mapping keeps the record of the one beside it, so that one
/// still unmaps whole and is replayed onto a re-attach.
#[test]
fn unmapping_a_mapping_keeps_its_neighbour() {
    let frames = HostFrames::new(0x1_0000_0000);
    let clock = clock();
    let device = Device::new(&frames, &ENDPOINTS, INPUT, Behaviour::Correct);
    let unit = unit(&device, &frames, &clock);
    let domain = unit.create_domain().unwrap();
    unit.attach(0x10, domain).unwrap();
    unit.map(domain, 0x4000_0000, 0x9000_0000, PAGE, Access::READ_WRITE)
        .unwrap();
    unit.map(domain, 0x4000_1000, 0x9100_0000, PAGE, Access::READ_WRITE)
        .unwrap();
    unit.unmap(domain, 0x4000_0000, PAGE).unwrap();
    unit.sync(domain).unwrap();
    unit.block(0x10).unwrap();
    unit.attach(0x10, domain).unwrap();
    assert_eq!(
        device.access(0x10, 0x4000_1000, false),
        Some(0x9100_0000),
        "the neighbour is replayed"
    );
    unit.unmap(domain, 0x4000_1000, PAGE).unwrap();
    unit.sync(domain).unwrap();
    assert_eq!(device.access(0x10, 0x4000_1000, false), None);
    assert_eq!(device.mappings(domain.0), 0);
}

/// A domain over a device translating every address takes its IOVAs from the
/// top of the 64-bit space, and its mappings reach the device.
#[test]
fn a_domain_over_every_address_maps_from_the_top() {
    let frames = HostFrames::new(0x1_0000_0000);
    let clock = clock();
    let device = Device::new(&frames, &ENDPOINTS, (0, u64::MAX), Behaviour::Correct);
    let unit = unit(&device, &frames, &clock);
    let mut domain = tairix_kernel_iommu_api::Domain::new(&unit, &[], &[]).unwrap();
    domain.attach(0x10).unwrap();
    let iova = domain
        .map(
            &[FrameRun {
                phys: 0x9000_0000,
                order: 0,
            }],
            0,
        )
        .unwrap();
    assert!(iova > 1 << 63, "{iova:#x}");
    assert_eq!(device.access(0x10, iova + 0x40, true), Some(0x9000_0040));
    domain.unmap(iova).unwrap();
    assert_eq!(device.access(0x10, iova, false), None);
    domain.destroy().unwrap();
}

/// A requester id the device has no endpoint for — one the fabric delivers
/// under an alias — is answered as no endpoint and probed as nothing: no DMA
/// arrives as it, and the endpoint it arrives as is translated.
#[test]
fn an_endpoint_the_device_does_not_have_is_attached_as_nothing() {
    let frames = HostFrames::new(0x1_0000_0000);
    let clock = clock();
    let device = Device::new(&frames, &ENDPOINTS, INPUT, Behaviour::Correct);
    let unit = unit(&device, &frames, &clock);
    let mut claimed = Vec::new();
    unit.reserved_iova(0x99, &mut |range| claimed.push(range))
        .unwrap();
    assert!(claimed.is_empty());
    let domain = unit.create_domain().unwrap();
    unit.map(domain, 0x4000_0000, 0x9000_0000, IO_PAGE_SIZE, Access::READ)
        .unwrap();
    assert_eq!(unit.attach(0x99, domain), Err(IommuError::NoEndpoint));
    assert_eq!(device.domains(), 0, "nothing was attached");
    unit.attach(0x10, domain).unwrap();
    assert_eq!(
        device.mappings(domain.0),
        1,
        "replayed for the real endpoint"
    );
    assert_eq!(device.access(0x10, 0x4000_0000, false), Some(0x9000_0000));
    unit.block(0x99).unwrap();
    unit.block(0x10).unwrap();
    unit.destroy_domain(domain).unwrap();
}
