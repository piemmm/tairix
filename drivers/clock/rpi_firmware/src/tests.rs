//! Host unit tests for the Raspberry Pi firmware-clocks driver, driven
//! against the protocol-faithful `lib/vcmailbox` mock firmware.
//!
//! QEMU models no `VideoCore`, so the range discovery, the applied-rate
//! reporting, the linked clocks' floors and every fail-closed path are proven
//! here; the live property channel is the on-metal acceptance item
//! (`plans/CPUFREQ.md`, `plans/PI.md`).

use core::cell::RefCell;

use tairix_abi::driver::clock::CLOCK_CONTROLLER_ENDPOINTS;
use tairix_abi::driver::mailbox::{MailboxChannel, MAILBOX_PROPERTY_WORDS};
use tairix_abi::hwlink::LinkRequest;
use tairix_abi::{
    CapabilityId, DriverError, DriverHost, DriverKind, Errno, HwMatchKey, ProcId, PROC_ID_LEN,
};
use tairix_clock::testkit::Rig;
use tairix_clock::{ClockHardware, Policy, Record};
use tairix_vcmailbox::mock::MockFirmware;
use tairix_vcmailbox::{FirmwareClock, MailboxTransport};

use super::{register, ArmClock, LinkedClocks, BIND_KEYS, RATE_INTERVAL_HZ};

/// A channel backed by the mock firmware, adapting its `&mut self` transport
/// onto the `&self` channel the driver uses — the same `RefCell` shape the
/// production `vcmailbox` service uses for the same reason.
struct MockChannel(RefCell<MockFirmware>);

impl MockChannel {
    fn new(firmware: MockFirmware) -> Self {
        Self(RefCell::new(firmware))
    }

    fn firmware(&self) -> MockFirmware {
        *self.0.borrow()
    }
}

impl MailboxChannel for MockChannel {
    fn exchange(&self, message: &mut [u32; MAILBOX_PROPERTY_WORDS]) -> Result<(), DriverError> {
        self.0
            .borrow_mut()
            .exchange(message)
            .map_err(tairix_vcmailbox::MailboxError::as_driver_error)
    }
}

/// A channel whose transport always fails closed, modelling a mailbox service
/// that is absent or a doorbell that timed out.
struct DeadChannel;

impl MailboxChannel for DeadChannel {
    fn exchange(&self, _message: &mut [u32; MAILBOX_PROPERTY_WORDS]) -> Result<(), DriverError> {
        Err(DriverError::DeviceFault)
    }
}

/// A channel that returns `Ok` but leaves the message untouched, modelling
/// the firmware revisions that stamp success while never processing the tag.
struct SilentChannel;

impl MailboxChannel for SilentChannel {
    fn exchange(&self, _message: &mut [u32; MAILBOX_PROPERTY_WORDS]) -> Result<(), DriverError> {
        Ok(())
    }
}

struct MockHost {
    granted: bool,
}

impl DriverHost for MockHost {
    fn has_capability(&self, cap: CapabilityId) -> bool {
        self.granted && cap == CapabilityId::DRV_LOAD
    }
    fn kind(&self) -> DriverKind {
        DriverKind::UserSpace
    }
}

/// A Pi 4B's stock range: 600 MHz to 1.5 GHz, running at the minimum — the
/// state the board is actually found in, and the defect this driver closes.
fn pi4b_at_minimum() -> MockFirmware {
    let mut fw = MockFirmware::healthy();
    fw.arm.min_hz = 600_000_000;
    fw.arm.max_hz = 1_500_000_000;
    fw.arm.hz = 600_000_000;
    fw
}

#[test]
fn register_requires_the_load_capability() {
    assert!(register(&MockHost { granted: true }).is_ok());
    assert_eq!(
        register(&MockHost { granted: false }),
        Err(DriverError::PermissionDenied)
    );
}

#[test]
fn the_bind_table_names_the_device_tree_identity() {
    assert_eq!(BIND_KEYS.len(), 1);
    let expected = HwMatchKey::compatible(b"raspberrypi,firmware-clocks").expect("fits");
    assert_eq!(BIND_KEYS[0].key, expected);
}

#[test]
fn the_range_comes_from_the_firmware_not_a_board_constant() {
    let channel = MockChannel::new(pi4b_at_minimum());
    let driver = ArmClock::new(&channel);
    let limits = driver.limits().expect("the firmware reports its range");
    assert_eq!(limits.min_hz, 600_000_000);
    assert_eq!(limits.max_hz, 1_500_000_000);
    assert_eq!(limits.step_hz, RATE_INTERVAL_HZ);
}

#[test]
fn an_overclocked_board_reports_its_own_ceiling() {
    // A `config.txt` that raises `arm_freq` must be driven over the range it
    // actually has, which is why neither end is a constant here.
    let mut fw = pi4b_at_minimum();
    fw.arm.max_hz = 2_000_000_000;
    let channel = MockChannel::new(fw);
    let driver = ArmClock::new(&channel);
    assert_eq!(driver.limits().expect("range").max_hz, 2_000_000_000);
}

#[test]
fn a_board_with_one_usable_rate_declares_a_legal_range() {
    // A range narrower than the interval worth asking at must still produce
    // limits the governor accepts, rather than a step wider than the range.
    let mut fw = pi4b_at_minimum();
    fw.arm.min_hz = 1_000_000_000;
    fw.arm.max_hz = 1_000_000_000;
    fw.arm.hz = 1_000_000_000;
    let channel = MockChannel::new(fw);
    let driver = ArmClock::new(&channel);
    let limits = driver.limits().expect("a one-rate range is legal");
    assert_eq!(limits.min_hz, limits.max_hz);
}

#[test]
fn applying_a_rate_reports_what_the_firmware_actually_did() {
    // The firmware clamps and rounds, so echoing the request back would
    // report a frequency the cores are not running at.
    let channel = MockChannel::new(pi4b_at_minimum());
    let driver = ArmClock::new(&channel);
    assert_eq!(driver.apply(1_500_000_000), Ok(1_500_000_000));
    assert_eq!(channel.firmware().arm.hz, 1_500_000_000);
    // Above the clock's range but inside the property interface's 32-bit rate
    // field, so the firmware — not this driver — is what clamps it.
    assert_eq!(
        driver.apply(3_000_000_000),
        Ok(1_500_000_000),
        "a target above the range is clamped by the firmware, and reported so"
    );
}

#[test]
fn the_rate_the_governor_asks_for_reaches_the_hardware() {
    // The end the reported defect turns on: the board sits at 600 MHz and
    // must actually move when the governor asks for full speed.
    let mut fw = pi4b_at_minimum();
    fw.arm.grain_hz = 1;
    let channel = MockChannel::new(fw);
    let driver = ArmClock::new(&channel);
    assert_eq!(
        channel.firmware().arm.hz,
        600_000_000,
        "as the board is found"
    );
    assert_eq!(driver.apply(1_500_000_000), Ok(1_500_000_000));
    assert_eq!(channel.firmware().arm.hz, 1_500_000_000);
}

#[test]
fn the_governor_can_lower_the_clock_as_well_as_raise_it() {
    // Every other apply test asks for the ceiling, because the subsystem was
    // built to raise a board the firmware had left at its floor — so nothing
    // exercised the one direction that matters once the machine goes quiet,
    // and a reduction the firmware answered with the ceiling looked like a
    // success. A request framed without its turbo-skip word runs the
    // firmware's turbo transition, which is exactly that.
    let mut fw = pi4b_at_minimum();
    fw.arm.grain_hz = 1;
    fw.arm.hz = 1_500_000_000;
    let channel = MockChannel::new(fw);
    let driver = ArmClock::new(&channel);
    assert_eq!(
        channel.firmware().arm.hz,
        1_500_000_000,
        "as a board that has been busy is found"
    );
    assert_eq!(
        driver.apply(600_000_000),
        Ok(600_000_000),
        "down to the floor"
    );
    assert_eq!(channel.firmware().arm.hz, 600_000_000);
    // A governor walks back down a step at a time, so an intermediate rate
    // has to stick too.
    assert_eq!(driver.apply(1_000_000_000), Ok(1_000_000_000));
    assert_eq!(channel.firmware().arm.hz, 1_000_000_000);
}

#[test]
fn a_target_wider_than_the_firmware_rate_field_is_refused() {
    // The property interface carries a 32-bit rate; a target that does not
    // fit is refused rather than truncated into a rate the firmware would
    // apply.
    let channel = MockChannel::new(pi4b_at_minimum());
    let driver = ArmClock::new(&channel);
    assert_eq!(
        driver.apply(u64::from(u32::MAX) + 1),
        Err(DriverError::OutOfRange)
    );
}

#[test]
fn an_unreachable_firmware_fails_closed() {
    let driver = ArmClock::new(&DeadChannel);
    assert_eq!(driver.limits(), Err(DriverError::DeviceFault));
    assert_eq!(driver.apply(1_500_000_000), Err(DriverError::DeviceFault));
}

#[test]
fn a_firmware_that_never_processes_the_tag_is_a_fault_not_a_reading() {
    // The documented Pi fault: the top-level success code is stamped while
    // the tag goes unprocessed. Reading that as a rate would report the
    // request's own zero slot as a stopped clock.
    let driver = ArmClock::new(&SilentChannel);
    assert_eq!(driver.limits(), Err(DriverError::BadMagic));
    assert_eq!(driver.apply(1_500_000_000), Err(DriverError::BadMagic));
}

#[test]
fn a_firmware_that_does_not_know_the_clock_is_a_fault_not_a_stopped_clock() {
    // Zero is how the property interface spells "no such clock". Declaring a
    // range with a zero end would ask the governor to pick a stopped rate.
    let mut fw = pi4b_at_minimum();
    fw.arm.min_hz = 0;
    fw.arm.max_hz = 0;
    fw.arm.hz = 0;
    let channel = MockChannel::new(fw);
    let driver = ArmClock::new(&channel);
    assert_eq!(driver.limits(), Err(DriverError::DeviceFault));
    assert_eq!(driver.apply(1_500_000_000), Err(DriverError::DeviceFault));
}

#[test]
fn an_inverted_firmware_range_is_refused_rather_than_second_guessed() {
    let mut fw = pi4b_at_minimum();
    fw.arm.min_hz = 1_500_000_000;
    fw.arm.max_hz = 600_000_000;
    let channel = MockChannel::new(fw);
    let driver = ArmClock::new(&channel);
    assert_eq!(driver.limits(), Err(DriverError::OutOfRange));
}

const fn instance(byte: u8) -> ProcId {
    ProcId::from_raw([byte; PROC_ID_LEN])
}

const HDMI0: ProcId = instance(1);
const HDMI1: ProcId = instance(2);
const HVS: ProcId = instance(3);

fn endpoint() -> u64 {
    CLOCK_CONTROLLER_ENDPOINTS.endpoint(6)
}

fn link(clock: FirmwareClock, index: u8) -> LinkRequest {
    LinkRequest::new(endpoint(), index, &[clock.as_u32()], b"").expect("valid")
}

/// The endpoint over the linked clocks behind `channel`, the HDMI blocks
/// holding their links to the two HDMI clocks and the display engine its
/// link to the core clock.
fn rig(channel: &MockChannel) -> Rig<LinkedClocks<'_, MockChannel>> {
    Rig::new(
        LinkedClocks::new(channel).expect("the firmware reports the ranges"),
        endpoint(),
        &[
            (HDMI0, link(FirmwareClock::M2mc, 0)),
            (HDMI0, link(FirmwareClock::PixelBvb, 1)),
            (HDMI1, link(FirmwareClock::M2mc, 0)),
            (HDMI1, link(FirmwareClock::PixelBvb, 1)),
            (HVS, link(FirmwareClock::Core, 0)),
            (HVS, link(FirmwareClock::Arm, 1)),
        ],
    )
}

#[test]
fn the_linked_clocks_are_floored_and_each_link_reaches_only_its_own() {
    let channel = MockChannel::new(MockFirmware::healthy());
    let clocks = LinkedClocks::new(&channel).expect("ranges");
    assert_eq!(clocks.served(&[FirmwareClock::Core.as_u32()]), Some(0));
    assert_eq!(clocks.served(&[FirmwareClock::M2mc.as_u32()]), Some(1));
    assert_eq!(clocks.served(&[FirmwareClock::PixelBvb.as_u32()]), Some(2));
    for unserved in [
        &[FirmwareClock::Arm.as_u32()][..],
        &[FirmwareClock::Emmc2.as_u32()],
        &[FirmwareClock::Core.as_u32(), 0],
        &[],
    ] {
        assert_eq!(clocks.served(unserved), None, "{unserved:?}");
    }
    assert!((0..clocks.count()).all(|at| clocks.policy(at) == Policy::Floor));

    let mut rig = rig(&channel);
    assert_eq!(
        rig.floor(HVS, link(FirmwareClock::Arm, 1), 0),
        Err(Errno::NotSupported),
        "the ARM clock is the frequency mechanism's"
    );
    assert_eq!(channel.firmware().arm.hz, 600_000_000);
}

#[test]
fn the_hdmi_state_machine_clock_runs_at_the_highest_floor_and_rests_at_its_least() {
    // A board booted with no monitor leaves the clock unset.
    let channel = MockChannel::new(MockFirmware::healthy());
    let mut rig = rig(&channel);
    let m2mc = link(FirmwareClock::M2mc, 0);
    assert_eq!(rig.floor(HDMI0, m2mc, 150_000_000), Ok(150_000_000));
    assert_eq!(rig.floor(HDMI1, m2mc, 120_000_000), Ok(150_000_000));
    assert_eq!(channel.firmware().m2mc.hz, 150_000_000);
    assert_eq!(rig.release(HDMI0, m2mc), Ok(()));
    assert_eq!(
        channel.firmware().m2mc.hz,
        120_000_000,
        "down to the floor left"
    );
    rig.end(HDMI1);
    assert_eq!(
        channel.firmware().m2mc.hz,
        120_000_000,
        "a clock the firmware had not started rests at the least its blocks need"
    );
}

/// The Pi 4's firmware reports the state-machine clock unset with a least of
/// zero: its return is never a request for zero, which the property interface
/// answers as no such clock.
#[test]
fn the_state_machine_clock_never_falls_below_what_its_blocks_need() {
    let mut firmware = MockFirmware::healthy();
    firmware.m2mc = tairix_vcmailbox::mock::MockClock::new(0, 0, 600_000_000, 1_000_000);
    let channel = MockChannel::new(firmware);
    let mut rig = rig(&channel);
    let m2mc = link(FirmwareClock::M2mc, 0);
    assert_eq!(rig.floor(HDMI0, m2mc, 25_000_000), Ok(120_000_000));
    assert_eq!(rig.release(HDMI0, m2mc), Ok(()));
    assert_eq!(channel.firmware().m2mc.hz, 120_000_000);
    assert!(rig
        .records()
        .iter()
        .all(|record| !matches!(record, Record::Stuck { .. } | Record::Unreturned { .. })));
}

/// A clock whose least is zero and which the firmware had not started has no
/// rate to return to: it is left where its last holder had it.
#[test]
fn a_clock_with_no_rate_to_return_to_is_left_running() {
    let mut firmware = MockFirmware::healthy();
    firmware.pixel_bvb = tairix_vcmailbox::mock::MockClock::new(0, 0, 300_000_000, 1_000_000);
    let channel = MockChannel::new(firmware);
    let mut rig = rig(&channel);
    let bvb = link(FirmwareClock::PixelBvb, 1);
    assert_eq!(rig.floor(HDMI0, bvb, 75_000_000), Ok(75_000_000));
    assert_eq!(rig.release(HDMI0, bvb), Ok(()));
    assert_eq!(channel.firmware().pixel_bvb.hz, 75_000_000);
    assert!(rig
        .records()
        .iter()
        .all(|record| !matches!(record, Record::Stuck { .. } | Record::Unreturned { .. })));
}

/// The display engine bounds the modes its pipe carries by how fast the core
/// clock can go, which is the firmware's to say: `hdmi_enable_4kp60` raises
/// it.
#[test]
fn a_linked_clock_is_described_with_the_ceiling_the_firmware_reports() {
    let channel = MockChannel::new(MockFirmware::healthy());
    let mut rig = rig(&channel);
    let state = rig
        .describe(HVS, link(FirmwareClock::Core, 0))
        .expect("described");
    assert_eq!((state.hz, state.max_hz), (500_000_000, 550_000_000));
}

#[test]
fn the_core_clock_returns_to_the_firmware_s_rate_with_its_last_holder() {
    let channel = MockChannel::new(MockFirmware::healthy());
    let mut rig = rig(&channel);
    let core = link(FirmwareClock::Core, 0);
    assert_eq!(
        rig.floor(HVS, core, 100_000_000),
        Ok(250_000_000),
        "never below its least"
    );
    assert_eq!(rig.floor(HVS, core, 540_000_000), Ok(540_000_000));
    assert_eq!(rig.release(HVS, core), Ok(()));
    assert_eq!(
        channel.firmware().core.hz,
        500_000_000,
        "as the firmware left it"
    );
}

#[test]
fn a_floor_the_firmware_cannot_make_is_refused_and_a_rounded_rate_reported() {
    let mut firmware = MockFirmware::healthy();
    firmware.pixel_bvb.grain_hz = 75_000_000;
    let channel = MockChannel::new(firmware);
    let mut rig = rig(&channel);
    let bvb = link(FirmwareClock::PixelBvb, 1);
    assert_eq!(rig.floor(HDMI0, bvb, 300_000_001), Err(Errno::OutOfRange));
    assert_eq!(
        rig.floor(HDMI0, bvb, 160_000_000),
        Ok(150_000_000),
        "the firmware's own answer, never the request echoed"
    );
}

#[test]
fn a_clock_the_firmware_does_not_know_is_not_served() {
    let mut firmware = MockFirmware::healthy();
    firmware.pixel_bvb = tairix_vcmailbox::mock::MockClock::new(0, 0, 0, 1);
    let channel = MockChannel::new(firmware);
    let mut rig = rig(&channel);
    assert_eq!(
        rig.floor(HDMI0, link(FirmwareClock::PixelBvb, 1), 75_000_000),
        Err(Errno::NotSupported)
    );
    assert_eq!(
        rig.floor(HDMI0, link(FirmwareClock::M2mc, 0), 150_000_000),
        Ok(150_000_000),
        "the clocks it knows are still served"
    );
}

#[test]
fn a_firmware_that_cannot_be_asked_serves_no_linked_clock() {
    assert!(LinkedClocks::new(&DeadChannel).is_err());
    assert!(LinkedClocks::new(&SilentChannel).is_err());
}
