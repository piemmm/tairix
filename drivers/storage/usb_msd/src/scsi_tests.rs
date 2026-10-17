//! Host tests for the transport-neutral SCSI command layer over a
//! scripted [`ScsiTransport`] (`plans/DEVICES.md` D5): per-set CDB
//! spelling, write-protect via the set's MODE SENSE form, flush
//! semantics, readiness answers, the medium probes, per-command exchange
//! budgets, in-band autosense, and the [`LunBlock`]
//! validation/chunking/scrub behaviour.

use super::*;
use alloc::collections::VecDeque;
use alloc::vec;
use alloc::vec::Vec;
use tairix_abi::blkio::{BlkDeviceClass, BlkReissue, BlkRemovabilityEvidence};

/// One scripted answer to an executed command.
struct Step {
    /// The device's verdict.
    passed: bool,
    /// Bytes delivered into an IN data phase (short if fewer than asked).
    data: Vec<u8>,
    /// In-band sense to stash for [`ScsiTransport::take_sense`] after this
    /// command (an autosense transport).
    sense: Option<Sense>,
}

/// A scripted command executor: records every `(lun, cdb, phase length)`
/// and answers from its step queue.
struct ScriptedExecutor {
    steps: VecDeque<Step>,
    /// Recorded executions: LUN, CDB bytes, data-phase length, is-IN.
    calls: Vec<(u8, Vec<u8>, usize, bool)>,
    /// The exchange budget each execution was given, in order.
    budgets: Vec<u64>,
    /// LUN count the transport reports.
    luns: u8,
    /// The pending autosense capture.
    sense: Option<Sense>,
    scrubs: usize,
    resets: usize,
    /// Transport failures the next executions meet, before any step.
    failures: VecDeque<Errno>,
    /// Whether the reset each scripted failure runs puts the transport back
    /// in step.
    failures_resync: bool,
    /// Whether the last execution broke with its transport resynced.
    resynced: bool,
}

impl ScriptedExecutor {
    fn new() -> Self {
        Self {
            steps: VecDeque::new(),
            calls: Vec::new(),
            budgets: Vec::new(),
            luns: 1,
            sense: None,
            scrubs: 0,
            resets: 0,
            failures: VecDeque::new(),
            failures_resync: false,
            resynced: false,
        }
    }

    fn pass_with(&mut self, data: Vec<u8>) {
        self.steps.push_back(Step {
            passed: true,
            data,
            sense: None,
        });
    }

    fn pass(&mut self) {
        self.pass_with(Vec::new());
    }

    fn fail(&mut self) {
        self.steps.push_back(Step {
            passed: false,
            data: Vec::new(),
            sense: None,
        });
    }

    fn fail_with_sense(&mut self, sense: Sense) {
        self.steps.push_back(Step {
            passed: false,
            data: Vec::new(),
            sense: Some(sense),
        });
    }

    /// A refusal whose sense the device answers on a `REQUEST SENSE`.
    fn fail_then_sense(&mut self, key: u8, asc: u8, ascq: u8) {
        self.fail();
        let mut sense = vec![0u8; 18];
        sense[0] = 0x70;
        sense[2] = key;
        sense[12] = asc;
        sense[13] = ascq;
        self.pass_with(sense);
    }
}

impl ScsiTransport for ScriptedExecutor {
    fn execute(
        &mut self,
        lun: u8,
        cdb: &[u8],
        data: DataPhase<'_>,
        exchange_ns: u64,
    ) -> Result<CommandOutcome, Errno> {
        // A new execution invalidates any stale capture (the trait's
        // documented contract).
        self.sense = None;
        self.resynced = false;
        let is_in = data.is_in();
        let len = data.len();
        self.calls.push((lun, cdb.to_vec(), len, is_in));
        self.budgets.push(exchange_ns);
        if let Some(failure) = self.failures.pop_front() {
            self.resynced = self.failures_resync;
            return Err(failure);
        }
        let step = self.steps.pop_front().ok_or(Errno::NotFound)?;
        let mut transferred = 0;
        if let DataPhase::In(buf) = data {
            let n = step.data.len().min(buf.len());
            buf[..n].copy_from_slice(&step.data[..n]);
            transferred = n;
        } else if let DataPhase::Out(buf) = data {
            transferred = buf.len();
        }
        self.sense = step.sense;
        Ok(CommandOutcome {
            passed: step.passed,
            transferred,
        })
    }

    fn lun_count(&mut self) -> Result<u8, Errno> {
        Ok(self.luns)
    }

    fn take_sense(&mut self) -> Option<Sense> {
        self.sense.take()
    }

    fn reset(&mut self) -> Result<(), Errno> {
        self.resets += 1;
        Ok(())
    }

    fn scrub(&mut self) {
        self.scrubs += 1;
    }

    fn take_resynced(&mut self) -> bool {
        core::mem::take(&mut self.resynced)
    }
}

fn transparent(executor: ScriptedExecutor) -> ScsiDevice<ScriptedExecutor> {
    ScsiDevice::new(executor, CommandSet::Transparent)
}

fn ufi(executor: ScriptedExecutor) -> ScsiDevice<ScriptedExecutor> {
    ScsiDevice::new(executor, CommandSet::Ufi)
}

#[test]
fn ufi_pads_every_command_block_to_twelve_bytes() {
    let mut executor = ScriptedExecutor::new();
    executor.pass();
    let mut scsi = ufi(executor);
    assert_eq!(scsi.test_unit_ready(0), Ok(Readiness::Ready));
    let (_, cdb, _, _) = &scsi.transport().calls[0];
    assert_eq!(cdb.len(), 12);
    assert!(cdb.iter().all(|&b| b == 0));
}

#[test]
fn transparent_sends_the_native_command_length() {
    let mut executor = ScriptedExecutor::new();
    executor.pass();
    let mut scsi = transparent(executor);
    assert_eq!(scsi.test_unit_ready(0), Ok(Readiness::Ready));
    let (_, cdb, _, _) = &scsi.transport().calls[0];
    assert_eq!(cdb.len(), 6);
}

#[test]
fn write_protect_uses_the_sets_own_mode_sense_form() {
    // Transparent: MODE SENSE(6), WP at header byte 2.
    let mut executor = ScriptedExecutor::new();
    executor.pass_with(vec![0x03, 0x00, 0x80, 0x00]);
    let mut scsi = transparent(executor);
    assert_eq!(scsi.write_protected(0), Ok(true));
    let (_, cdb, _, _) = &scsi.transport().calls[0];
    assert_eq!(cdb[0], 0x1A);

    // UFI: MODE SENSE(10), WP at header byte 3.
    let mut executor = ScriptedExecutor::new();
    executor.pass_with(vec![0x00, 0x46, 0x00, 0x80, 0, 0, 0, 0]);
    let mut scsi = ufi(executor);
    assert_eq!(scsi.write_protected(0), Ok(true));
    let (_, cdb, _, _) = &scsi.transport().calls[0];
    assert_eq!(cdb[0], 0x5A);

    // A device that fails MODE SENSE is reported write-enabled.
    let mut executor = ScriptedExecutor::new();
    executor.fail();
    let mut scsi = transparent(executor);
    assert_eq!(scsi.write_protected(0), Ok(false));
}

#[test]
fn ufi_flush_is_a_no_op_and_transparent_flush_hits_the_wire() {
    // UFI has no SYNCHRONIZE CACHE: the flush succeeds with no command.
    let executor = ScriptedExecutor::new();
    let mut scsi = ufi(executor);
    assert_eq!(scsi.synchronize_cache(0), Ok(()));
    assert!(scsi.transport().calls.is_empty());

    // Transparent issues SYNCHRONIZE CACHE(10).
    let mut executor = ScriptedExecutor::new();
    executor.pass();
    let mut scsi = transparent(executor);
    assert_eq!(scsi.synchronize_cache(0), Ok(()));
    let (_, cdb, _, _) = &scsi.transport().calls[0];
    assert_eq!(cdb[0], 0x35);
}

#[test]
fn a_flush_refused_as_illegal_request_means_no_cache() {
    let mut executor = ScriptedExecutor::new();
    executor.fail_with_sense(Sense {
        key: 0x05, // ILLEGAL REQUEST
        asc: 0x20,
        ascq: 0,
    });
    let mut scsi = transparent(executor);
    assert_eq!(scsi.synchronize_cache(0), Ok(()));
    // The autosense answered the question; no REQUEST SENSE round trip.
    assert_eq!(scsi.transport().calls.len(), 1);
}

#[test]
fn autosense_maps_a_failed_read_without_request_sense() {
    let mut executor = ScriptedExecutor::new();
    executor.fail_with_sense(Sense {
        key: 0x07, // DATA PROTECT
        asc: 0x27,
        ascq: 0,
    });
    let mut scsi = transparent(executor);
    let mut buf = [0u8; 512];
    assert_eq!(scsi.read(0, 0, 512, &mut buf), Err(Errno::PermissionDenied));
    // Exactly one execution: the sense came in-band.
    assert_eq!(scsi.transport().calls.len(), 1);
}

#[test]
fn a_read_whose_medium_is_gone_asks_the_device_and_records_it() {
    let mut executor = ScriptedExecutor::new();
    executor.fail_then_sense(0x02, 0x3A, 0x00);
    let mut scsi = transparent(executor);
    let mut buf = [0u8; 512];
    assert_eq!(scsi.read(0, 0, 512, &mut buf), Err(Errno::DeviceOffline));
    assert_eq!(scsi.transport().calls.len(), 2);
    let (_, cdb, _, _) = &scsi.transport().calls[1];
    assert_eq!(cdb[0], 0x03); // REQUEST SENSE
    assert_eq!(scsi.take_readiness(0), Some(Readiness::NoMedium));
    assert_eq!(scsi.take_readiness(0), None, "taken once");
}

#[test]
fn a_test_unit_ready_answers_why_the_unit_is_not_ready() {
    let mut executor = ScriptedExecutor::new();
    executor.fail_then_sense(0x02, 0x04, 0x01);
    executor.fail_then_sense(0x06, 0x29, 0x00);
    executor.pass();
    let mut scsi = transparent(executor);
    assert_eq!(scsi.test_unit_ready(0), Ok(Readiness::BecomingReady));
    assert_eq!(scsi.test_unit_ready(0), Ok(Readiness::Attention));
    assert_eq!(scsi.test_unit_ready(0), Ok(Readiness::Ready));
    // Each refusal cost its REQUEST SENSE, nothing more.
    assert_eq!(scsi.transport().calls.len(), 5);
}

#[test]
fn a_unit_needing_an_initializing_command_is_started_immediately() {
    let mut executor = ScriptedExecutor::new();
    executor.pass();
    let mut scsi = transparent(executor);
    assert_eq!(scsi.start_unit(0), Ok(Readiness::Ready));
    let (_, cdb, len, _) = &scsi.transport().calls[0];
    assert_eq!(
        cdb.as_slice(),
        &[0x1B, 0x01, 0, 0, 0x01, 0],
        "IMMED and START"
    );
    assert_eq!(*len, 0);
}

#[test]
fn a_unit_attention_is_consumed_and_the_command_retried_once() {
    let mut executor = ScriptedExecutor::new();
    executor.fail_then_sense(0x06, 0x29, 0x00);
    executor.pass_with(vec![0x5A; 512]);
    let mut scsi = transparent(executor);
    let mut buf = [0u8; 512];
    assert_eq!(scsi.read(0, 0, 512, &mut buf), Ok(()));
    assert!(buf.iter().all(|&byte| byte == 0x5A));

    // A second attention in a row is not retried again.
    let mut executor = ScriptedExecutor::new();
    executor.fail_then_sense(0x06, 0x29, 0x00);
    executor.fail_then_sense(0x06, 0x29, 0x00);
    let mut scsi = transparent(executor);
    assert_eq!(scsi.read(0, 0, 512, &mut buf), Err(Errno::WouldBlock));
    assert_eq!(scsi.transport().calls.len(), 4);
}

#[test]
fn inquiry_accepts_a_short_but_sufficient_answer() {
    let mut executor = ScriptedExecutor::new();
    executor.pass_with(vec![0x00, 0x80, 0x05, 0x02, 0x1F]);
    let mut scsi = transparent(executor);
    assert_eq!(
        scsi.inquiry(0),
        Ok(Inquiry {
            device_type: DEVICE_TYPE_DIRECT_ACCESS,
            removable: true,
        })
    );
}

#[test]
fn read_capacity_validates_the_block_size() {
    let mut executor = ScriptedExecutor::new();
    let mut payload = Vec::new();
    payload.extend_from_slice(&1999u32.to_be_bytes());
    payload.extend_from_slice(&512u32.to_be_bytes());
    executor.pass_with(payload);
    let mut scsi = transparent(executor);
    assert_eq!(
        scsi.read_capacity(0),
        Ok(BlockGeometry {
            block_size: 512,
            block_count: 2000,
        })
    );

    // A block size that is not a power of two in 512..=4096 fails closed.
    let mut executor = ScriptedExecutor::new();
    let mut payload = Vec::new();
    payload.extend_from_slice(&1999u32.to_be_bytes());
    payload.extend_from_slice(&520u32.to_be_bytes());
    executor.pass_with(payload);
    let mut scsi = transparent(executor);
    assert_eq!(scsi.read_capacity(0), Err(Errno::OutOfRange));
}

/// What a USB unit that reported nothing of its medium states.
fn unreported() -> UnitReport {
    UnitReport {
        medium: BlkMedium::of_transport(BlkTransport::Usb),
        max_transfer_blocks: None,
    }
}

/// Geometry + state used by the `LunBlock` tests.
fn small_lun_state(write_protected: bool) -> LunState {
    LunState {
        geometry: BlockGeometry {
            block_size: 512,
            block_count: 1 << 30,
        },
        write_protected,
        report: unreported(),
    }
}

#[test]
fn lun_block_refuses_writes_to_a_protected_medium_before_the_device() {
    let executor = ScriptedExecutor::new();
    let mut scsi = transparent(executor);
    let mut lun = LunBlock::new(&mut scsi, 0, small_lun_state(true));
    let buf = [0u8; 512];
    assert_eq!(
        lun.write_blocks(0, &buf),
        Err(DriverError::PermissionDenied)
    );
    // No command ever reached the device.
    assert!(scsi.transport().calls.is_empty());
}

#[test]
fn lun_block_validates_shape_and_range() {
    let executor = ScriptedExecutor::new();
    let mut scsi = transparent(executor);
    let state = LunState {
        geometry: BlockGeometry {
            block_size: 512,
            block_count: 8,
        },
        write_protected: false,
        report: unreported(),
    };
    let mut lun = LunBlock::new(&mut scsi, 0, state);
    let mut torn = [0u8; 100];
    assert_eq!(
        lun.read_blocks(0, &mut torn),
        Err(DriverError::BufferTooSmall)
    );
    let mut past_end = [0u8; 512];
    assert_eq!(
        lun.read_blocks(8, &mut past_end),
        Err(DriverError::LengthOutOfRange)
    );
    assert!(scsi.transport().calls.is_empty());
}

#[test]
fn lun_block_chunks_a_large_read_into_bounded_commands() {
    let mut executor = ScriptedExecutor::new();
    // Two full-window chunks: each is one command.
    for _ in 0..2 {
        executor.pass_with(vec![0xEEu8; MSD_MAX_TRANSFER_LEN]);
    }
    let mut scsi = transparent(executor);
    let mut lun = LunBlock::new(&mut scsi, 0, small_lun_state(false));
    let mut buf = vec![0u8; 2 * MSD_MAX_TRANSFER_LEN];
    lun.read_blocks(0, &mut buf).expect("chunked read passes");
    assert!(buf.iter().all(|&b| b == 0xEE));

    // Two READ(10)s, the second starting where the first ended.
    let blocks_per_chunk = u32::try_from(MSD_MAX_TRANSFER_LEN / 512).expect("fits");
    let (_, cdb0, _, _) = &scsi.transport().calls[0];
    let (_, cdb1, _, _) = &scsi.transport().calls[1];
    assert_eq!(&cdb0[2..6], &0u32.to_be_bytes());
    assert_eq!(&cdb1[2..6], &blocks_per_chunk.to_be_bytes());
}

#[test]
fn a_broken_exchange_reaches_the_block_engine_as_a_reissuable_outcome() {
    use tairix_abi::blkio::BlkStatus;
    for (transport, driver, status) in [
        // The transport reset in step again: reissue.
        (
            Errno::WouldBlock,
            DriverError::Busy,
            BlkStatus::TransientError,
        ),
        // The device did not answer its own reset: ridden out, then failed.
        (Errno::TimedOut, DriverError::TimedOut, BlkStatus::Timeout),
    ] {
        let mut executor = ScriptedExecutor::new();
        executor.failures.push_back(transport);
        let mut scsi = transparent(executor);
        let mut lun = LunBlock::new(&mut scsi, 0, small_lun_state(false));
        let mut buf = [0u8; 512];
        assert_eq!(lun.read_blocks(0, &mut buf), Err(driver), "{transport:?}");
        let health = BlkStatus::for_driver_health(driver);
        assert_eq!(health, Some(status));
        assert!(health.is_some_and(BlkStatus::is_retryable));
    }
}

#[test]
fn a_sensitive_transfer_scrubs_the_shared_window() {
    let mut executor = ScriptedExecutor::new();
    executor.pass_with(vec![0u8; 512]);
    let mut scsi = transparent(executor);
    let mut lun = LunBlock::new(&mut scsi, 0, small_lun_state(false));
    let mut buf = [0u8; 512];
    lun.read_blocks_with_class(0, &mut buf, BufferClass::Sensitive)
        .expect("read passes");
    assert_eq!(scsi.transport().scrubs, 1);
}

#[test]
fn an_oversize_or_empty_command_block_is_refused() {
    let executor = ScriptedExecutor::new();
    let mut scsi = transparent(executor);
    let outcome = scsi.command(0, &[], DataPhase::None, CommandKind::Query);
    assert_eq!(outcome.err(), Some(Errno::OutOfRange));
    let seventeen = [0u8; 17];
    let outcome = scsi.command(0, &seventeen, DataPhase::None, CommandKind::Query);
    assert_eq!(outcome.err(), Some(Errno::OutOfRange));
    assert!(scsi.transport().calls.is_empty());
}

/// A VPD page answer: header, then `body`.
fn vpd_page(page: u8, body: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0x00, page];
    bytes.extend_from_slice(&u16::try_from(body.len()).expect("short").to_be_bytes());
    bytes.extend_from_slice(body);
    bytes
}

/// A Block Device Characteristics page reporting `rate`.
fn characteristics(rate: u16) -> Vec<u8> {
    let mut body = vec![0u8; 0x3C];
    body[..2].copy_from_slice(&rate.to_be_bytes());
    vpd_page(0xB1, &body)
}

/// IDENTIFY DEVICE data with word 217 set to `rate` and a valid integrity
/// word.
fn identify(rate: u16) -> Vec<u8> {
    let mut data = vec![0u8; 512];
    data[434..436].copy_from_slice(&rate.to_le_bytes());
    data[510] = 0xA5;
    let sum = data.iter().fold(0u8, |sum, &byte| sum.wrapping_add(byte));
    data[511] = sum.wrapping_neg();
    data
}

fn removable_inquiry() -> Inquiry {
    Inquiry {
        device_type: DEVICE_TYPE_DIRECT_ACCESS,
        removable: true,
    }
}

const RMB: BlkRemovability = BlkRemovability::Removable(BlkRemovabilityEvidence::Inquiry);

#[test]
fn a_bridge_reporting_its_disks_rate_is_rotating_whatever_its_removable_bit() {
    let mut executor = ScriptedExecutor::new();
    executor.pass_with(vpd_page(0x00, &[0x00, 0x80, 0xB1]));
    executor.pass_with(characteristics(7200));
    let mut scsi = transparent(executor);
    let report = scsi.probe(0, removable_inquiry()).expect("probed");
    assert_eq!(
        report.medium,
        BlkMedium::of_transport(BlkTransport::Usb)
            .with_rotation(BlkRotation::from_rate_field(
                7200,
                BlkRotationEvidence::RotationRateVpd
            ))
            .with_removability(RMB)
    );
    assert_eq!(
        report.medium.class(),
        BlkDeviceClass::Rotational,
        "a disk behind a bridge setting RMB is served as a disk"
    );
}

#[test]
fn a_bridge_with_no_vpd_is_asked_its_disks_identify_data() {
    let mut executor = ScriptedExecutor::new();
    executor.fail_then_sense(0x05, 0x24, 0x00);
    executor.pass_with(identify(7200));
    let mut scsi = transparent(executor);
    let report = scsi.probe(0, removable_inquiry()).expect("probed");
    assert_eq!(
        report.medium.rotation,
        BlkRotation::from_rate_field(7200, BlkRotationEvidence::AtaIdentify)
    );
    let (_, cdb, len, is_in) = &scsi.transport().calls[2];
    assert_eq!(cdb[0], 0x85, "ATA PASS-THROUGH(16)");
    assert_eq!(cdb[14], 0xEC, "IDENTIFY DEVICE");
    assert_eq!((*len, *is_in), (512, true));
}

#[test]
fn a_rate_of_one_is_a_non_rotating_medium() {
    let mut executor = ScriptedExecutor::new();
    executor.pass_with(vpd_page(0x00, &[0x00, 0xB1]));
    executor.pass_with(characteristics(0x0001));
    let mut scsi = transparent(executor);
    let report = scsi.probe(0, removable_inquiry()).expect("probed");
    assert_eq!(
        report.medium.rotation,
        BlkRotation::NonRotating(BlkRotationEvidence::RotationRateVpd)
    );
    assert_eq!(report.medium.class(), BlkDeviceClass::Removable);
}

#[test]
fn a_unit_refusing_every_probe_is_unknown_and_served_as_the_slowest_medium() {
    let mut executor = ScriptedExecutor::new();
    executor.fail_then_sense(0x05, 0x24, 0x00);
    executor.fail_then_sense(0x05, 0x20, 0x00);
    let mut scsi = transparent(executor);
    let report = scsi.probe(0, removable_inquiry()).expect("probed");
    assert_eq!(report.medium.rotation, BlkRotation::Unknown);
    assert_eq!(report.medium.removability, RMB);
    assert_eq!(report.medium.class(), BlkDeviceClass::Rotational);
}

#[test]
fn a_probe_that_hangs_is_recorded_as_not_reporting_and_the_unit_still_served() {
    let mut executor = ScriptedExecutor::new();
    // The bridge hangs on the first VPD request; the transport has reset it.
    executor.failures.push_back(Errno::WouldBlock);
    let mut scsi = transparent(executor);
    let report = scsi.probe(0, removable_inquiry()).expect("still served");
    assert_eq!(report.medium.rotation, BlkRotation::Unknown);
    assert_eq!(
        scsi.transport().calls.len(),
        1,
        "probing stopped at the hang"
    );

    let mut executor = ScriptedExecutor::new();
    executor.failures.push_back(Errno::NotFound);
    let mut scsi = transparent(executor);
    assert_eq!(
        scsi.probe(0, removable_inquiry()),
        Err(Errno::NotFound),
        "a device gone is not a refusal"
    );
}

#[test]
fn a_unit_that_ignores_evpd_is_not_read_as_listing_pages() {
    let mut executor = ScriptedExecutor::new();
    // Standard INQUIRY data in place of the supported-pages list.
    executor.pass_with(vec![0x00, 0x00, 0x06, 0x02, 0x1F, 0xB1, 0xB0]);
    executor.fail_then_sense(0x05, 0x20, 0x00);
    let mut scsi = transparent(executor);
    let report = scsi.probe(0, removable_inquiry()).expect("probed");
    assert_eq!(report.max_transfer_blocks, None);
    let (_, cdb, _, _) = &scsi.transport().calls[1];
    assert_eq!(cdb[0], 0x85, "no VPD page was asked for after a bogus list");
}

#[test]
fn identify_data_that_fails_its_integrity_word_reports_nothing() {
    let mut data = identify(7200);
    data[0] ^= 1;
    assert_eq!(rotation_of_identify(&data), None);
    let mut atapi = identify(7200);
    atapi[1] = 0x80;
    assert_eq!(rotation_of_identify(&atapi), None);
    assert_eq!(rotation_of_identify(&data[..100]), None);
}

#[test]
fn a_ufi_unit_is_not_probed() {
    let executor = ScriptedExecutor::new();
    let mut scsi = ufi(executor);
    let report = scsi.probe(0, removable_inquiry()).expect("probed");
    assert_eq!(report.medium.rotation, BlkRotation::Unknown);
    assert!(scsi.transport().calls.is_empty());
}

#[test]
fn the_block_limits_bound_each_command() {
    let mut executor = ScriptedExecutor::new();
    executor.pass_with(vpd_page(0x00, &[0x00, 0xB0]));
    let mut limits = vec![0u8; 0x3C];
    limits[4..8].copy_from_slice(&16u32.to_be_bytes());
    executor.pass_with(vpd_page(0xB0, &limits));
    executor.fail_then_sense(0x05, 0x20, 0x00);
    let mut scsi = transparent(executor);
    let report = scsi.probe(0, removable_inquiry()).expect("probed");
    assert_eq!(report.max_transfer_blocks, NonZeroU32::new(16));

    let mut executor = ScriptedExecutor::new();
    for _ in 0..4 {
        executor.pass_with(vec![0x11; 16 * 512]);
    }
    let mut scsi = transparent(executor);
    let mut state = small_lun_state(false);
    state.report = report;
    let mut lun = LunBlock::new(&mut scsi, 0, state);
    let mut buf = vec![0u8; 64 * 512];
    lun.read_blocks(0, &mut buf).expect("read in four commands");
    assert_eq!(scsi.transport().calls.len(), 4);
}

#[test]
fn each_command_is_given_its_own_kinds_budget() {
    let mut executor = ScriptedExecutor::new();
    executor.pass();
    executor.pass();
    executor.pass_with(vec![0u8; 512]);
    executor.pass_with(vec![0u8; MSD_MAX_TRANSFER_LEN]);
    let mut scsi = transparent(executor);
    let budget = scsi.budget(0);
    scsi.test_unit_ready(0).expect("tested");
    scsi.synchronize_cache(0).expect("flushed");
    let mut small = [0u8; 512];
    scsi.read(0, 0, 512, &mut small).expect("read");
    let mut large = vec![0u8; MSD_MAX_TRANSFER_LEN];
    scsi.read(0, 0, 512, &mut large).expect("read");
    let budgets = &scsi.transport().budgets;
    assert_eq!(budgets[0], budget.exchange_ns(CommandKind::Query, 0));
    assert_eq!(budgets[1], budget.exchange_ns(CommandKind::Flush, 0));
    assert_eq!(budgets[2], budget.exchange_ns(CommandKind::Media, 512));
    assert!(
        budgets[3] > budgets[2],
        "the budget grows with the transfer"
    );
    assert!(
        budgets[2] >= 30_000_000_000,
        "a unit not known not to rotate may be spinning up on the read"
    );

    // Once the unit reports flash, it is served as flash.
    scsi.set_medium(
        0,
        BlkMedium::of_transport(BlkTransport::Usb).with_rotation(BlkRotation::NonRotating(
            BlkRotationEvidence::RotationRateVpd,
        )),
    );
    assert!(scsi.budget(0).media_ns < budget.media_ns);
}

/// A read whose exchange breaks, its transport's reset having put the
/// transport back in step, over an executor that then answers per `script`.
fn broken_read_after(script: impl FnOnce(&mut ScriptedExecutor)) -> ScsiDevice<ScriptedExecutor> {
    let mut executor = ScriptedExecutor::new();
    executor.failures.push_back(Errno::WouldBlock);
    executor.failures_resync = true;
    script(&mut executor);
    transparent(executor)
}

#[test]
fn a_unit_ready_after_its_reset_has_its_request_come_back_at_once() {
    use tairix_abi::driver::block::Block;
    let mut scsi = broken_read_after(ScriptedExecutor::pass);
    let mut lun = LunBlock::new(&mut scsi, 0, small_lun_state(false));
    let mut buf = [0u8; 512];
    assert_eq!(lun.read_blocks(0, &mut buf), Err(DriverError::Busy));
    assert_eq!(lun.reissue(), BlkReissue::Now);
    assert_eq!(lun.reissue(), BlkReissue::Paced, "taken once");
    let calls = &scsi.transport().calls;
    assert_eq!(calls.len(), 2, "the read, then whether the unit is ready");
    assert_eq!(calls[0].1[0], 0x28, "READ(10)");
    assert_eq!(calls[1].1, [0u8; 6], "TEST UNIT READY");

    // A unit attention the test consumed leaves the unit serving.
    let mut scsi = broken_read_after(|executor| executor.fail_then_sense(0x06, 0x29, 0x00));
    let mut buf = [0u8; 512];
    assert_eq!(scsi.read(0, 0, 512, &mut buf), Err(Errno::WouldBlock));
    assert_eq!(scsi.take_reissue(0), BlkReissue::Now);
}

#[test]
fn a_unit_not_ready_after_its_reset_is_held_or_retracted() {
    use tairix_abi::driver::block::Block;
    for (asc, readiness, error) in [
        (0x04, Readiness::BecomingReady, DriverError::NotReady),
        (0x3A, Readiness::NoMedium, DriverError::DeviceOffline),
    ] {
        let mut scsi = broken_read_after(|executor| executor.fail_then_sense(0x02, asc, 0x01));
        let mut lun = LunBlock::new(&mut scsi, 0, small_lun_state(false));
        let mut buf = [0u8; 512];
        assert_eq!(lun.read_blocks(0, &mut buf), Err(error), "{readiness:?}");
        assert_eq!(lun.reissue(), BlkReissue::Paced, "{readiness:?}");
        assert_eq!(scsi.take_readiness(0), Some(readiness));
    }
    // A unit whose test breaks the exchange again says nothing.
    let mut scsi = broken_read_after(|executor| executor.failures.push_back(Errno::WouldBlock));
    let mut buf = [0u8; 512];
    assert_eq!(scsi.read(0, 0, 512, &mut buf), Err(Errno::WouldBlock));
    assert_eq!(scsi.take_reissue(0), BlkReissue::Paced);
    assert_eq!(scsi.take_readiness(0), None);
}

#[test]
fn a_reset_that_did_not_resync_the_transport_asks_the_unit_nothing() {
    let mut executor = ScriptedExecutor::new();
    executor.failures.push_back(Errno::WouldBlock);
    let mut scsi = transparent(executor);
    let mut buf = [0u8; 512];
    assert_eq!(scsi.read(0, 0, 512, &mut buf), Err(Errno::WouldBlock));
    assert_eq!(scsi.transport().calls.len(), 1, "no test was sent");
    assert_eq!(scsi.take_reissue(0), BlkReissue::Paced);
}

#[test]
fn a_unit_coming_ready_holds_its_request_and_one_losing_its_medium_goes_offline() {
    use tairix_abi::driver::block::Block;
    for (asc, ascq, readiness, error) in [
        (0x04, 0x01, Readiness::BecomingReady, DriverError::NotReady),
        (0x04, 0x02, Readiness::NeedsStart, DriverError::NotReady),
        (0x3A, 0x00, Readiness::NoMedium, DriverError::DeviceOffline),
    ] {
        let mut executor = ScriptedExecutor::new();
        executor.fail_then_sense(0x02, asc, ascq);
        let mut scsi = transparent(executor);
        let mut lun = LunBlock::new(&mut scsi, 0, small_lun_state(false));
        let mut buf = [0u8; 512];
        assert_eq!(lun.read_blocks(0, &mut buf), Err(error), "{readiness:?}");
        assert_eq!(scsi.take_readiness(0), Some(readiness));
    }
    let mut executor = ScriptedExecutor::new();
    executor.fail_then_sense(0x06, 0x28, 0x00);
    let mut scsi = transparent(executor);
    let mut lun = LunBlock::new(&mut scsi, 0, small_lun_state(false));
    assert_eq!(lun.flush(), Err(DriverError::DeviceOffline));
    assert_eq!(scsi.take_readiness(0), Some(Readiness::MediumChanged));
}
