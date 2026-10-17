//! Host tests for the HCD's per-interface URB-service state machine
//! ([`super::UrbService`]) and the interface-node grant builder
//! ([`super::attach_transport_grants`]).
//!
//! The state machine is driven over a mock [`UrbEngine`] and a heap "shared
//! buffer" standing in for the U3a2 region both the HCD and the class driver
//! map. The live wait-set loop that calls these on real `call_recv`/`irq_wait`
//! wake-ups is in `main.rs` and is the on-metal acceptance item.

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;

use super::{
    attach_transport_grants, urb_failure_level, Operation, Reach, UrbOutcome, UrbReply, UrbService,
};
use tairix_abi::hwtree::{HwResourceKind, HW_NODE_ROOT};
use tairix_abi::reply::decode_status_reply;
use tairix_abi::usb_urb::{
    decode_completion, IsoGrant, UrbRequest, UsbDirection, UsbSpeed, UsbTransferType,
    USB_CONTROL_TIMEOUT_MAX_MS,
};
use tairix_abi::{DriverError, Errno, HwDeviceClass, HwMatchKey, HwNode, ProcId, PROC_ID_LEN};
use tairix_inline::BitSet256;
use tairix_usb::transport::{UrbEngine, UrbScope};

/// A controllable [`UrbEngine`] double: interrupt-IN delivers queued reports
/// once each, then "nothing pending"; control-IN and the bulk pair mirror the
/// live engine's queue-then-reap shape (the first drive queues and returns
/// `None`, a later drive completes — control-IN with a fixed response, bulk
/// from the queued device data); an interface operation runs for
/// `operation_drives` drives. Records its call counts so a test can prove a
/// rejected URB never reaches it.
struct MockEngine {
    control_response: Vec<u8>,
    reports: Vec<Vec<u8>>,
    interrupt_fault: Option<DriverError>,
    control_calls: usize,
    interrupt_calls: usize,
    /// Queued device responses for bulk-IN, delivered one per completed TD.
    bulk_in_data: Vec<Vec<u8>>,
    /// An armed bulk-IN TD's requested length, `None` when idle.
    bulk_in_armed: Option<usize>,
    /// The URBs it was told to abandon, and whether it lets them go.
    abandoned: Vec<UrbRequest>,
    abandon_fault: Option<DriverError>,
    /// A control-IN is queued and not yet reaped.
    control_queued: bool,
    /// Drives a set-interface stays running for, and the drives it took.
    operation_drives: usize,
    operation_calls: usize,
}

/// The mock interface's bulk-IN endpoint number.
const BULK_IN_ENDPOINT: u8 = 2;

impl MockEngine {
    fn new() -> Self {
        Self {
            control_response: Vec::new(),
            reports: Vec::new(),
            interrupt_fault: None,
            control_calls: 0,
            interrupt_calls: 0,
            bulk_in_data: Vec::new(),
            bulk_in_armed: None,
            abandoned: Vec::new(),
            abandon_fault: None,
            control_queued: false,
            operation_drives: 0,
            operation_calls: 0,
        }
    }
}

impl UrbEngine for MockEngine {
    fn abandon(&mut self, urb: &UrbRequest) -> Result<(), DriverError> {
        self.abandoned.push(*urb);
        self.bulk_in_armed = None;
        self.abandon_fault.take().map_or(Ok(()), Err)
    }

    fn control_in(
        &mut self,
        _setup: [u8; 8],
        data: &mut [u8],
    ) -> Result<Option<usize>, DriverError> {
        self.control_calls += 1;
        if !core::mem::replace(&mut self.control_queued, true) {
            return Ok(None);
        }
        self.control_queued = false;
        let n = self.control_response.len().min(data.len());
        data[..n].copy_from_slice(&self.control_response[..n]);
        Ok(Some(n))
    }

    fn control_no_data(&mut self, _setup: [u8; 8]) -> Result<Option<()>, DriverError> {
        // The seam-level round-trip lives in `tairix_usb::transport::tests`.
        Err(DriverError::NotFound)
    }

    fn control_out(&mut self, _setup: [u8; 8], _data: &[u8]) -> Result<Option<()>, DriverError> {
        // As `control_no_data`.
        Err(DriverError::NotFound)
    }

    fn set_interface(&mut self, _interface: u8, _alternate: u8) -> Result<Option<()>, DriverError> {
        self.operation_calls += 1;
        if self.operation_drives > 0 {
            self.operation_drives -= 1;
            return Ok(None);
        }
        Ok(Some(()))
    }

    fn scope(&self) -> Option<UrbScope> {
        let mut interfaces = BitSet256::new();
        interfaces.insert(0);
        Some(UrbScope {
            interfaces,
            endpoints: u32::MAX << 2,
        })
    }

    fn interrupt_in(
        &mut self,
        _endpoint: u8,
        _request: usize,
        data: &mut [u8],
    ) -> Result<Option<usize>, DriverError> {
        self.interrupt_calls += 1;
        if let Some(err) = self.interrupt_fault.take() {
            return Err(err);
        }
        if self.reports.is_empty() {
            return Ok(None);
        }
        let report = self.reports.remove(0);
        let n = report.len().min(data.len());
        data[..n].copy_from_slice(&report[..n]);
        Ok(Some(n))
    }

    fn bulk_in(&mut self, endpoint: u8, data: &mut [u8]) -> Result<Option<usize>, DriverError> {
        if endpoint != BULK_IN_ENDPOINT {
            return Err(DriverError::OutOfRange);
        }
        if self.bulk_in_armed.is_none() {
            self.bulk_in_armed = Some(data.len());
            return Ok(None);
        }
        if self.bulk_in_data.is_empty() {
            return Ok(None);
        }
        self.bulk_in_armed = None;
        let response = self.bulk_in_data.remove(0);
        let n = response.len().min(data.len());
        data[..n].copy_from_slice(&response[..n]);
        Ok(Some(n))
    }

    fn bulk_out(&mut self, _endpoint: u8, _data: &[u8]) -> Result<Option<usize>, DriverError> {
        // No serve-level test drives bulk-OUT through this double yet; the
        // seam-level round-trip lives in `tairix_usb::transport::tests`.
        Err(DriverError::NotFound)
    }
}

/// An interrupt-IN URB on `endpoint` reading `length` bytes.
fn interrupt_urb(endpoint: u8, length: u32) -> UrbRequest {
    UrbRequest {
        endpoint,
        transfer_type: UsbTransferType::Interrupt,
        direction: UsbDirection::In,
        length,
        setup: [0; 8],
        timeout_ms: None,
    }
}

/// The deadline a bulk URB here carries.
const BULK_TIMEOUT_MS: u32 = 3750;

/// Decode the completion carried by a [`UrbOutcome::Reply`].
fn reply_result(outcome: &UrbOutcome) -> Result<u32, Errno> {
    match outcome {
        UrbOutcome::Reply(reply) => decode_completion(&reply.bytes[..reply.len]),
        other => panic!("expected a Reply, got {other:?}"),
    }
}

#[test]
fn an_interrupt_in_is_held_until_a_controller_event_completes_it() {
    let mut engine = MockEngine::new();
    let mut shm = vec![0u8; 8];
    let mut service = UrbService::new();
    let request = interrupt_urb(1, 8);

    // No report queued yet: the submit is held outstanding, not replied.
    let outcome = service.on_submit(Reach::Served, 0x11, 0, &request, &mut shm, &mut engine);
    assert_eq!(outcome, UrbOutcome::Held);
    assert!(service.is_busy());
    assert_eq!(engine.interrupt_calls, 1);

    // The report arrives; the next controller event completes the held URB,
    // landing the bytes in the shared buffer and replying to its ticket.
    let report = vec![0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00];
    engine.reports = vec![report.clone()];
    let outcome = service.on_event(&mut shm, &mut engine);
    match outcome {
        UrbOutcome::Reply(reply) => {
            assert_eq!(reply.ticket, 0x11);
            assert_eq!(decode_completion(&reply.bytes[..reply.len]), Ok(8));
        }
        other => panic!("expected a Reply, got {other:?}"),
    }
    assert_eq!(&shm[..8], &report[..]);
    assert!(!service.is_busy());
}

/// A bulk-IN URB on `endpoint` reading `length` bytes.
fn bulk_in_urb(endpoint: u8, length: u32) -> UrbRequest {
    UrbRequest {
        endpoint,
        transfer_type: UsbTransferType::Bulk,
        direction: UsbDirection::In,
        length,
        setup: [0; 8],
        timeout_ms: core::num::NonZeroU32::new(BULK_TIMEOUT_MS),
    }
}

#[test]
fn a_bulk_in_is_held_until_a_controller_event_completes_it() {
    // Bulk completions are delivered asynchronously exactly like interrupt
    // completions: the submit arms the TD and is held, the controller event
    // reaps it and replies with the device's bytes in the shared buffer.
    let mut engine = MockEngine::new();
    let mut shm = vec![0u8; 16];
    let mut service = UrbService::new();
    let request = bulk_in_urb(BULK_IN_ENDPOINT, 16);

    let outcome = service.on_submit(Reach::Served, 0x21, 0, &request, &mut shm, &mut engine);
    assert_eq!(outcome, UrbOutcome::Held);
    assert!(service.is_busy());

    let payload = vec![0xC3u8; 16];
    engine.bulk_in_data = vec![payload.clone()];
    let outcome = service.on_event(&mut shm, &mut engine);
    match outcome {
        UrbOutcome::Reply(reply) => {
            assert_eq!(reply.ticket, 0x21);
            assert_eq!(decode_completion(&reply.bytes[..reply.len]), Ok(16));
        }
        other => panic!("expected a Reply, got {other:?}"),
    }
    assert_eq!(&shm[..16], &payload[..]);
    assert!(!service.is_busy());
}

#[test]
fn a_held_bulk_urb_is_abandoned_at_its_deadline_and_answered_timed_out() {
    let mut engine = MockEngine::new();
    let mut shm = vec![0u8; 16];
    let mut service = UrbService::new();
    let request = bulk_in_urb(BULK_IN_ENDPOINT, 16);
    let submitted = 7_000;
    assert_eq!(
        service.on_submit(
            Reach::Served,
            0x41,
            submitted,
            &request,
            &mut shm,
            &mut engine
        ),
        UrbOutcome::Held
    );
    let due = submitted + u64::from(BULK_TIMEOUT_MS) * 1_000_000;
    assert_eq!(service.deadline(), Some(due));
    assert_eq!(service.expire(due - 1, &mut engine), UrbOutcome::Idle);
    assert!(engine.abandoned.is_empty(), "nothing is abandoned early");

    let outcome = service.expire(due, &mut engine);
    assert_eq!(reply_result(&outcome), Err(Errno::TimedOut));
    assert_eq!(engine.abandoned, [request]);
    assert!(!service.is_busy());
    assert_eq!(service.deadline(), None);
}

#[test]
fn a_urb_the_controller_will_not_let_go_is_answered_as_a_fault() {
    let mut engine = MockEngine::new();
    engine.abandon_fault = Some(DriverError::DeviceFault);
    let mut shm = vec![0u8; 16];
    let mut service = UrbService::new();
    let request = bulk_in_urb(BULK_IN_ENDPOINT, 16);
    service.on_submit(Reach::Served, 0x42, 0, &request, &mut shm, &mut engine);
    let outcome = service.expire(u64::MAX, &mut engine);
    assert_eq!(reply_result(&outcome), Err(Errno::DeviceFault));
}

#[test]
fn a_report_poll_with_no_deadline_waits_for_its_report_however_long() {
    let mut engine = MockEngine::new();
    let mut shm = vec![0u8; 8];
    let mut service = UrbService::new();
    service.on_submit(
        Reach::Served,
        0x43,
        0,
        &interrupt_urb(1, 8),
        &mut shm,
        &mut engine,
    );
    assert_eq!(service.deadline(), None);
    assert_eq!(service.expire(u64::MAX, &mut engine), UrbOutcome::Idle);
    assert!(service.is_busy());
}

/// A `GET_DESCRIPTOR(device)` control-IN URB reading `length` bytes.
fn control_in_urb(length: u8) -> UrbRequest {
    UrbRequest {
        endpoint: 0,
        transfer_type: UsbTransferType::Control,
        direction: UsbDirection::In,
        length: u32::from(length),
        setup: [0x80, 0x06, 0x00, 0x01, 0x00, 0x00, length, 0x00],
        timeout_ms: core::num::NonZeroU32::new(500),
    }
}

#[test]
fn a_control_in_is_held_until_a_controller_event_completes_it() {
    let mut engine = MockEngine::new();
    engine.control_response = vec![0x12, 0x01, 0x00, 0x02, 0x00, 0x00, 0x00, 0x40];
    let mut shm = vec![0u8; 64];
    let mut service = UrbService::new();

    let outcome = service.on_submit(
        Reach::Served,
        0x22,
        0,
        &control_in_urb(8),
        &mut shm,
        &mut engine,
    );
    assert_eq!(outcome, UrbOutcome::Held, "queued, never answered inline");
    assert_eq!(service.deadline(), Some(500_000_000));
    let outcome = service.on_event(&mut shm, &mut engine);
    assert_eq!(reply_result(&outcome), Ok(8));
    assert!(!service.is_busy());
    assert_eq!(&shm[..8], &engine.control_response[..]);
    assert_eq!(engine.control_calls, 2);
}

#[test]
fn a_control_urb_is_held_no_longer_than_any_device_may_take() {
    let mut engine = MockEngine::new();
    let mut shm = vec![0u8; 64];
    let mut service = UrbService::new();
    let patient = UrbRequest {
        timeout_ms: core::num::NonZeroU32::new(60_000),
        ..control_in_urb(8)
    };
    let ceiling = u64::from(USB_CONTROL_TIMEOUT_MAX_MS) * 1_000_000;
    assert_eq!(
        service.on_submit(Reach::Served, 0x23, 1_000, &patient, &mut shm, &mut engine),
        UrbOutcome::Held
    );
    assert_eq!(service.deadline(), Some(1_000 + ceiling));
    assert_eq!(
        service.expire(1_000 + ceiling - 1, &mut engine),
        UrbOutcome::Idle
    );
    assert_eq!(
        reply_result(&service.expire(1_000 + ceiling, &mut engine)),
        Err(Errno::TimedOut)
    );
    assert_eq!(engine.abandoned, [patient], "its TD is taken back");
}

#[test]
fn an_interface_operation_is_held_until_the_engine_finishes_it() {
    let mut engine = MockEngine::new();
    engine.operation_drives = 2;
    let mut shm = vec![0u8; 8];
    let mut service = UrbService::new();
    let select = Operation::SetInterface {
        interface: 1,
        alternate: 1,
    };
    assert_eq!(
        service.on_operation(Reach::Served, 0x61, select, &mut engine),
        UrbOutcome::Held
    );
    assert_eq!(service.on_event(&mut shm, &mut engine), UrbOutcome::Held);
    match service.on_event(&mut shm, &mut engine) {
        UrbOutcome::Reply(reply) => {
            assert_eq!(reply.ticket, 0x61);
            assert_eq!(decode_status_reply(&reply.bytes[..reply.len]), Ok(()));
        }
        other => panic!("expected the operation answered, got {other:?}"),
    }
    assert_eq!(engine.operation_calls, 3);
    assert_eq!(service.held_transfer(), None);
}

#[test]
fn a_second_submit_while_one_is_outstanding_is_rejected_without_displacing_it() {
    let mut engine = MockEngine::new();
    let mut shm = vec![0u8; 8];
    let mut service = UrbService::new();
    let request = interrupt_urb(1, 8);

    // First submit is held (no report yet).
    assert_eq!(
        service.on_submit(Reach::Served, 0x11, 0, &request, &mut shm, &mut engine),
        UrbOutcome::Held
    );
    // A second concurrent submit is fail-closed `AlreadyExists` and does not
    // touch the engine or the in-flight URB.
    let before = engine.interrupt_calls;
    let outcome = service.on_submit(Reach::Served, 0x22, 0, &request, &mut shm, &mut engine);
    assert_eq!(reply_result(&outcome), Err(Errno::AlreadyExists));
    assert_eq!(engine.interrupt_calls, before);
    assert!(service.is_busy());

    // The first URB's ticket — not the rejected one — is completed by the
    // event.
    engine.reports = vec![vec![1, 2, 3, 4, 5, 6, 7, 8]];
    match service.on_event(&mut shm, &mut engine) {
        UrbOutcome::Reply(reply) => assert_eq!(reply.ticket, 0x11),
        other => panic!("expected a Reply to the held ticket, got {other:?}"),
    }
}

#[test]
fn aborting_an_outstanding_urb_replies_and_unblocks_a_replugged_driver() {
    let mut engine = MockEngine::new();
    let mut shm = vec![0u8; 8];
    let mut service = UrbService::new();
    let request = interrupt_urb(1, 8);

    assert_eq!(
        service.on_submit(Reach::Served, 0x11, 0, &request, &mut shm, &mut engine),
        UrbOutcome::Held
    );

    let outcome = service.abort_outstanding(Errno::NotFound);
    match outcome {
        UrbOutcome::Reply(reply) => {
            assert_eq!(reply.ticket, 0x11);
            assert_eq!(
                decode_completion(&reply.bytes[..reply.len]),
                Err(Errno::NotFound)
            );
        }
        other => panic!("expected a Reply to the aborted ticket, got {other:?}"),
    }
    assert!(!service.is_busy());

    assert_eq!(
        service.on_submit(Reach::Served, 0x22, 0, &request, &mut shm, &mut engine),
        UrbOutcome::Held
    );
}

#[test]
fn disconnect_abort_wins_over_a_stale_transfer_fault() {
    let mut engine = MockEngine::new();
    let mut shm = vec![0u8; 8];
    let mut service = UrbService::new();
    let request = interrupt_urb(1, 8);

    assert_eq!(
        service.on_submit(Reach::Served, 0x11, 0, &request, &mut shm, &mut engine),
        UrbOutcome::Held
    );
    engine.interrupt_fault = Some(DriverError::DeviceFault);

    let outcome = service.abort_outstanding(Errno::NotFound);
    assert_eq!(reply_result(&outcome), Err(Errno::NotFound));
    assert!(!service.is_busy());
    assert_eq!(service.on_event(&mut shm, &mut engine), UrbOutcome::Idle);
    assert_eq!(
        engine.interrupt_calls, 1,
        "disconnect abort must not drain a stale transfer fault after replying NotFound"
    );
}

#[test]
fn submit_after_interface_removal_is_rejected_without_touching_the_engine() {
    let mut engine = MockEngine::new();
    let mut shm = vec![0u8; 8];
    let mut service = UrbService::new();
    let request = interrupt_urb(1, 8);

    let outcome = service.on_submit(Reach::Retracted, 0x33, 0, &request, &mut shm, &mut engine);
    match outcome {
        UrbOutcome::Reply(reply) => {
            assert_eq!(reply.ticket, 0x33);
            assert_eq!(
                decode_completion(&reply.bytes[..reply.len]),
                Err(Errno::NotFound)
            );
        }
        other => panic!("expected a NotFound reply for an absent interface, got {other:?}"),
    }
    assert!(!service.is_busy());
    assert_eq!(engine.interrupt_calls, 0);
}

#[test]
fn an_illegal_urb_is_replied_fail_closed_without_reaching_the_engine() {
    let mut engine = MockEngine::new();
    let mut shm = vec![0u8; 8];
    let mut service = UrbService::new();

    // A bulk transfer must target a device endpoint, never the shared
    // control endpoint.
    let urb = UrbRequest {
        endpoint: 0,
        ..bulk_in_urb(BULK_IN_ENDPOINT, 8)
    };

    let outcome = service.on_submit(Reach::Served, 0x33, 0, &urb, &mut shm, &mut engine);
    assert_eq!(reply_result(&outcome), Err(Errno::OutOfRange));
    assert!(!service.is_busy());
    assert_eq!(engine.control_calls, 0);
    assert_eq!(engine.interrupt_calls, 0);
}

#[test]
fn a_controller_event_with_nothing_outstanding_is_idle() {
    let mut engine = MockEngine::new();
    let mut shm = vec![0u8; 8];
    let mut service = UrbService::new();
    assert_eq!(service.on_event(&mut shm, &mut engine), UrbOutcome::Idle);
    assert_eq!(engine.interrupt_calls, 0);
}

#[test]
fn attach_transport_grants_adds_the_endpoint_and_shared_resources() {
    let mut node = HwNode::new(0, HW_NODE_ROOT, HwDeviceClass::Input);
    node.push_match_key(HwMatchKey::usb(0x046d, 0xc31c, 0x03_01_01))
        .expect("usb match key");

    let node = attach_transport_grants(node, 0xD012_5701, 0x5147).expect("attaches both grants");

    let kinds: Vec<Option<HwResourceKind>> = node
        .resources()
        .iter()
        .map(tairix_abi::HwResource::kind)
        .collect();
    assert!(kinds.contains(&Some(HwResourceKind::Endpoint)));
    assert!(kinds.contains(&Some(HwResourceKind::Shared)));
}

#[test]
fn a_report_poll_during_recovery_waits_for_the_reset_without_touching_the_controller() {
    // While the controller recovers the device table is not trusted, so even
    // a buffered report is not read. The poll is held through the recovery,
    // and driven once the controller is back.
    let mut engine = MockEngine::new();
    engine.reports = vec![vec![0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00]];
    let mut shm = vec![0u8; 8];
    let mut service = UrbService::new();

    let outcome = service.on_submit(
        Reach::Recovering,
        0x41,
        0,
        &interrupt_urb(1, 8),
        &mut shm,
        &mut engine,
    );
    assert_eq!(outcome, UrbOutcome::Held);
    assert!(service.is_busy());
    assert_eq!(
        engine.interrupt_calls, 0,
        "nothing reaches the controller while it recovers"
    );
    assert_eq!(
        reply_result(&service.on_submit(
            Reach::Recovering,
            0x42,
            0,
            &interrupt_urb(1, 8),
            &mut shm,
            &mut engine,
        )),
        Err(Errno::AlreadyExists),
        "the one held poll is never displaced"
    );
    assert_eq!(engine.interrupt_calls, 0);
    assert_eq!(
        service.reissue_after_reset(),
        UrbOutcome::Held,
        "a poll the controller never saw is driven, not resubmitted"
    );
    match service.on_event(&mut shm, &mut engine) {
        UrbOutcome::Reply(reply) => {
            assert_eq!(reply.ticket, 0x41);
            assert_eq!(decode_completion(&reply.bytes[..reply.len]), Ok(8));
        }
        other => panic!("expected the held poll answered, got {other:?}"),
    }
    assert!(!service.is_busy());
}

#[test]
fn a_transfer_during_recovery_is_held_without_touching_the_controller() {
    // Answered at once, its class driver would submit again at once for as
    // long as the recovery lasts.
    for (ticket, request) in [
        (0x51, bulk_in_urb(BULK_IN_ENDPOINT, 16)),
        (0x52, control_in_urb(8)),
    ] {
        let mut engine = MockEngine::new();
        engine.control_response = vec![0xAA; 8];
        engine.bulk_in_data = vec![vec![0xC3; 16]];
        let mut shm = vec![0u8; 16];
        let mut service = UrbService::new();
        let outcome = service.on_submit(
            Reach::Recovering,
            ticket,
            0,
            &request,
            &mut shm,
            &mut engine,
        );
        assert_eq!(outcome, UrbOutcome::Held);
        assert_eq!(engine.control_calls, 0);
        assert_eq!(engine.bulk_in_armed, None, "no bulk TD was armed");
        assert_eq!(service.reissue_after_reset(), UrbOutcome::Held);
        assert_eq!(service.on_event(&mut shm, &mut engine), UrbOutcome::Held);
        assert!(matches!(
            service.on_event(&mut shm, &mut engine),
            UrbOutcome::Reply(reply) if reply.ticket == ticket
        ));
    }
}

#[test]
fn a_reset_answers_a_transfer_it_discarded_reissuably() {
    // A bulk TD armed before the controller faulted is gone with the reset,
    // so its class driver is told to reissue it once the controller is back.
    let mut engine = MockEngine::new();
    let mut shm = vec![0u8; 16];
    let mut service = UrbService::new();
    assert_eq!(
        service.on_submit(
            Reach::Served,
            0x71,
            0,
            &bulk_in_urb(BULK_IN_ENDPOINT, 16),
            &mut shm,
            &mut engine,
        ),
        UrbOutcome::Held
    );

    match service.reissue_after_reset() {
        UrbOutcome::Reply(reply) => {
            assert_eq!(reply.ticket, 0x71);
            assert_eq!(
                decode_completion(&reply.bytes[..reply.len]),
                Err(Errno::WouldBlock)
            );
        }
        other => panic!("expected the held transfer answered, got {other:?}"),
    }
    assert!(!service.is_busy());
    assert_eq!(service.reissue_after_reset(), UrbOutcome::Idle);
}

#[test]
fn an_operation_answers_with_its_status_or_its_grant() {
    let ok = UrbReply::status(7, Ok(()));
    assert_eq!(ok.ticket, 7);
    assert_eq!(decode_status_reply(&ok.bytes[..ok.len]), Ok(()));
    let refused = UrbReply::status(8, Err(Errno::NoBandwidth));
    assert_eq!(
        decode_status_reply(&refused.bytes[..refused.len]),
        Err(Errno::NoBandwidth)
    );
    let grant = IsoGrant {
        region_grant: 3,
        grantor: ProcId::from_raw([0x77; PROC_ID_LEN]),
        notify: 0x5553_0000_0000_2A81,
        interval_microframes: 8,
        speed: UsbSpeed::Full,
        stream: core::num::NonZeroU32::MIN,
    };
    let reply = UrbReply::grant(9, &grant);
    assert_eq!(IsoGrant::decode(&reply.bytes[..reply.len]), Ok(grant));
}

#[test]
fn a_stall_is_logged_as_the_devices_answer_and_a_bus_failure_as_a_warning() {
    use tairix_log::Level;
    assert_eq!(urb_failure_level(Errno::EndpointStalled), Level::Info);
    for errno in [Errno::TimedOut, Errno::DeviceFault, Errno::PermissionDenied] {
        assert_eq!(urb_failure_level(errno), Level::Warn, "{errno:?}");
    }
}
