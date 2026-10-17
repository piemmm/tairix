//! The engine's operations against the mock controller: nothing the serve
//! loop runs waits on another device, enumerations run side by side, and a
//! disconnect, a hung command or a controller reset at any step leaks
//! nothing.

use super::*;
use alloc::vec;
use tairix_inline::ArrayVec;

/// The stick and the keyboard behind the hub on root port 1, both served.
const STICK: usize = 1;
const KEYBOARD: usize = 2;

/// A hub on root port 1 carrying a mass-storage stick on its port 2 and a
/// keyboard on its port 4.
fn stick_and_keyboard_hub(mem: &SharedMem) -> MockXhci {
    let mut mock = MockXhci::with_hub(mem, 4, 4);
    mock.msd_downstream_port = 2;
    mock
}

/// Serve as the HCD's loop does, the clock moving to each deadline it would
/// park until, until `reached` holds.
fn serve_until(device: &mut TestHost, mut reached: impl FnMut(&mut TestHost) -> bool) {
    for _ in 0..SETTLE_STEPS {
        let serviced = device.serve();
        if reached(device) {
            return;
        }
        if device.busy() {
            continue;
        }
        match serviced.next_deadline {
            Some(at) => device.now_ns = device.now_ns.max(at),
            None => break,
        }
    }
    panic!("the engine settled short of the state the test waits for");
}

/// [`serve_until`] `event` is reported.
fn serve_until_reported(device: &mut TestHost, event: HubEvent) {
    serve_until(device, |device| device.events.contains(&event));
}

/// Start the descent of the hub installed at `hub`, as its attach does.
fn start_descent(device: &mut TestHost, hub: usize) {
    device.start(move |engine| async move {
        let mut crumb = device::port::Crumb::default();
        device::port::descend_hub(&engine, hub, &mut crumb).await
    });
}

/// The bulk-IN URB the stick's class driver would hold.
fn stick_read(length: u32) -> UrbRequest {
    UrbRequest {
        endpoint: 3,
        transfer_type: UsbTransferType::Bulk,
        direction: UsbDirection::In,
        length,
        setup: [0; 8],
        timeout_ms: core::num::NonZeroU32::new(1_000),
    }
}

/// The control-IN URB a class driver holds for `GET_DESCRIPTOR(device)`.
fn descriptor_read() -> UrbRequest {
    UrbRequest {
        endpoint: 0,
        transfer_type: UsbTransferType::Control,
        direction: UsbDirection::In,
        length: 18,
        setup: GET_DEVICE_DESCRIPTOR,
        timeout_ms: core::num::NonZeroU32::new(1_000),
    }
}

#[test]
fn a_control_urb_its_device_never_answers_holds_up_nothing_else() {
    let mem = shared_mem();
    let mut device = started_device(stick_and_keyboard_hub(&mem), &mem);
    device.bring_up().expect("both devices are served");
    arm_report_request_for(&mut device, KEYBOARD);
    let start = device.elapsed_us();

    // The stick NAKs its next control-IN for ever.
    device.host_mut().model_mut().stall_next_control_in = Some(Vec::new());
    let mut descriptor = [0u8; 18];
    assert_eq!(
        device
            .host
            .engine_for(STICK)
            .control_in(GET_DEVICE_DESCRIPTOR, &mut descriptor),
        Ok(None),
        "the URB is held, not answered inline"
    );
    device.serve();

    // While it is held: a keystroke lands, one stick read completes and
    // another stick read reaches its deadline.
    device
        .host_mut()
        .model_mut()
        .pending_reports
        .push_back(vec![0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00]);
    device.host_mut().model_mut().process_int_ring();
    device
        .host_mut()
        .model_mut()
        .bulk_in_responses
        .push_back(vec![0x42; 8]);
    device
        .core_mut()
        .queue_bulk_in(STICK, IN_PIPE, 8)
        .expect("the read queues");
    device
        .core_mut()
        .queue_bulk_in(STICK, IN_PIPE, 8)
        .expect("the second queues");
    device.serve();
    let mut buf = [0u8; 8];
    let read = device
        .core_mut()
        .poll_bulk(STICK, &mut buf)
        .expect("the pipe polls")
        .expect("the first read completed");
    assert_eq!((read.result, buf), (Ok(8), [0x42; 8]));
    device
        .host
        .engine_for(STICK)
        .abandon(&stick_read(8))
        .expect("the second read is abandoned at its deadline");
    let serviced = device.serve();

    let mut report = [0u8; BOOT_REPORT_LEN];
    assert_eq!(
        device
            .core_mut()
            .next_report(KEYBOARD, BOOT_REPORT_LEN, &mut report),
        Ok(Some(BOOT_REPORT_LEN))
    );
    assert_eq!(report[2], 0x04, "the keystroke was delivered");
    assert_eq!(
        device.core().bulk_in_flight(STICK, IN_PIPE),
        0,
        "the abandoned read was taken back"
    );
    assert!(serviced.topology.is_empty());
    assert_eq!(device.elapsed_us(), start, "nothing waited on the held URB");
    assert_eq!(
        device
            .host
            .engine_for(STICK)
            .control_in(GET_DEVICE_DESCRIPTOR, &mut descriptor),
        Ok(None),
        "the control URB is still held"
    );

    // At its own deadline the held URB goes, and the stick's control pipe
    // serves the next request.
    device.advance_us(1_000_000);
    device
        .host
        .engine_for(STICK)
        .abandon(&descriptor_read())
        .expect("the URB is abandoned");
    device.settle();
    assert!(device.host_mut().model_mut().ep0_unanswered.is_none());
    assert_control_endpoint_serves(&mut device, STICK);
    assert_eq!(
        device.core().recorded_controls(),
        0,
        "no answer is left untaken"
    );
}

#[test]
fn one_device_stalled_mid_enumeration_holds_up_none_beside_it() {
    // The stick's control pipe answers nothing until released. Its
    // enumeration stops at its first descriptor read, holding nothing the
    // keyboard's needs, so the keyboard enumerates and serves meanwhile.
    let mem = shared_mem();
    let mut mock = stick_and_keyboard_hub(&mem);
    mock.hub_downstream_status = 1 << 0;
    let mut device = started_device(mock, &mem);
    let hub = install_root_hub_on_port_1(&mut device);
    let start = device.elapsed_us();
    device.host_mut().model_mut().hold_control_of_next_slot = true;
    start_descent(&mut device, hub);
    serve_until_reported(&mut device, HubEvent::Attached(KEYBOARD));
    assert!(
        !device.core().device_live(STICK),
        "the stick is still enumerating"
    );
    // Stepped, never settled: a settle would run the clock out to the stick's
    // transfer ceiling.
    let mut report = [0u8; BOOT_REPORT_LEN];
    assert_eq!(
        device
            .core_mut()
            .next_report(KEYBOARD, BOOT_REPORT_LEN, &mut report),
        Ok(None)
    );
    device.serve();
    device
        .host_mut()
        .model_mut()
        .pending_reports
        .push_back(vec![0x00, 0x00, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00]);
    device.host_mut().model_mut().process_int_ring();
    device.serve();
    assert_eq!(
        device
            .core_mut()
            .next_report(KEYBOARD, BOOT_REPORT_LEN, &mut report),
        Ok(Some(BOOT_REPORT_LEN)),
        "the keyboard serves while the stick enumerates"
    );

    device.host_mut().model_mut().release_held_control();
    device.settle();
    assert!(
        device.core().device_live(STICK),
        "the stick then enumerates too"
    );
    assert!(device.core().device_live(KEYBOARD));
    assert!(
        device.elapsed_us() - start < 1_000_000,
        "no transfer ran out its ceiling"
    );
}

/// The doorbells a clean bring-up of what `mock` builds rings.
fn bring_up_doorbells(mock: fn(&SharedMem) -> MockXhci) -> usize {
    let mem = shared_mem();
    let mut device = started_device(mock(&mem), &mem);
    device.bring_up().expect("the topology is reached");
    let rung = device.host_mut().model_mut().doorbells.len();
    rung
}

/// The device on root port 1 pulled before each doorbell its bring-up
/// rings, in turn: each time nothing is left behind, and plugged back in it
/// is served.
fn unplug_at_every_step(mock: fn(&SharedMem) -> MockXhci, served: &[HubEvent]) {
    for step in 0..bring_up_doorbells(mock) {
        let mem = shared_mem();
        let mut mock = mock(&mem);
        mock.unplug_after_doorbells = Some(step);
        let mut device = started_device(mock, &mem);
        let shared = device.dma_ref().live_chunks();
        device.bring_up().expect("the walk runs");
        assert!(
            device.host_mut().model_mut().root_device_pulled,
            "step {step}"
        );

        assert!(
            !device.core().any_device_live(),
            "step {step}: nothing is served"
        );
        assert!(
            !device.core().hub_watch_active(),
            "step {step}: no hub is watched"
        );
        assert_eq!(
            device.host.live_operations(),
            0,
            "step {step}: no operation lingers"
        );
        assert_eq!(
            device.core().open_pipes(),
            0,
            "step {step}: every pipe closed"
        );
        assert_eq!(device.core().command_ring_in_flight(), 0, "step {step}");
        assert_eq!(device.core().recorded_controls(), 0, "step {step}");
        assert_eq!(
            device.dma_ref().live_chunks(),
            shared,
            "step {step}: every region went back"
        );
        assert_eq!(device.dma_ref().withheld_chunks(), 0, "step {step}");
        assert!(
            device.host_mut().model_mut().enabled_slots.is_empty(),
            "step {step}: every slot was disabled"
        );
        assert!(
            device.elapsed_us() < 1_000_000,
            "step {step}: the unplug ended the attach, not a deadline"
        );

        device.host_mut().model_mut().replug_root_device();
        assert_eq!(
            device.changes(),
            served,
            "step {step}: what was plugged back in is served"
        );
    }
}

#[test]
fn an_unplug_at_every_enumeration_step_leaks_nothing() {
    unplug_at_every_step(MockXhci::with_device, &[HubEvent::Attached(0)]);
}

/// A hub on root port 1 with a keyboard on its port 4.
fn keyboard_hub(mem: &SharedMem) -> MockXhci {
    let mut mock = MockXhci::with_hub(mem, 4, 4);
    mock.hub_downstream_status = 1 << 0;
    mock
}

#[test]
fn an_unplugged_hub_at_every_step_leaves_no_tier_behind() {
    unplug_at_every_step(
        keyboard_hub,
        &[HubEvent::HubAttached(0), HubEvent::Attached(1)],
    );
}

#[test]
fn a_hub_whose_connection_bounces_at_every_step_comes_back_whole() {
    // A bounce mid-attach, the hub's descent included, ends that attach; the
    // port's next pass finds it connected and attaches it afresh, never
    // leaving a half-walked tier standing in for it.
    let (served_chunks, served_slots) = {
        let mem = shared_mem();
        let mut device = started_device(keyboard_hub(&mem), &mem);
        device.bring_up().expect("the hub and keyboard are reached");
        let slots = device.host_mut().model_mut().enabled_slots.len();
        let chunks = device.dma_ref().live_chunks();
        (chunks, slots)
    };
    for step in 0..bring_up_doorbells(keyboard_hub) {
        let mem = shared_mem();
        let mut mock = keyboard_hub(&mem);
        mock.bounce_after_doorbells = Some(step);
        let mut device = started_device(mock, &mem);
        device.bring_up().expect("the walk runs");
        assert!(
            device.core().device_live(1),
            "step {step}: the keyboard is served"
        );
        assert!(
            device.core().hub_watch_active(),
            "step {step}: the hub is watched"
        );
        assert_eq!(device.host.live_operations(), 0, "step {step}");
        assert_eq!(device.dma_ref().live_chunks(), served_chunks, "step {step}");
        assert_eq!(device.dma_ref().withheld_chunks(), 0, "step {step}");
        assert_eq!(
            device.host_mut().model_mut().enabled_slots.len(),
            served_slots,
            "step {step}: no slot is left over"
        );
    }
}

#[test]
fn an_unplug_behind_a_hub_ends_the_attach_at_once() {
    // The keyboard's device descriptor read is never answered, so its attach
    // would wait out the transfer ceiling; its unplug ends it instead.
    let mem = shared_mem();
    let mut mock = MockXhci::with_hub(&mem, 4, 4);
    mock.hub_downstream_status = 1 << 0;
    let mut device = started_device(mock, &mem);
    let hub = install_root_hub_on_port_1(&mut device);
    let held = device.dma_ref().live_chunks();
    device.host_mut().model_mut().withhold_next_descriptor_read = Some(0x01);
    start_descent(&mut device, hub);
    serve_until(&mut device, |device| {
        device.host_mut().model_mut().ep0_unanswered.is_some()
    });
    let start = device.elapsed_us();

    device.host_mut().model_mut().hub_downstream_status = 0;
    device.host_mut().model_mut().hub_downstream_change = PORT_CHANGE_CONNECTION;
    device
        .host_mut()
        .model_mut()
        .post_hub_status_change(&[1 << 4]);
    device.settle();
    assert!(
        device.elapsed_us() - start < 1_000_000,
        "the unplug ended the attach, not the transfer ceiling"
    );
    assert!(!device.core().device_live(1));
    assert_eq!(device.core().open_pipes(), 1, "only the hub's pipe is open");
    assert_eq!(
        device.host_mut().model_mut().enabled_slots,
        [1],
        "only the hub's slot"
    );
    assert_eq!(device.dma_ref().live_chunks(), held);
    assert_eq!(device.dma_ref().withheld_chunks(), 0);
    assert_eq!(device.core().recorded_controls(), 0);
    assert!(
        device.core().hub_watch_active(),
        "the hub still watches its ports"
    );
}

#[test]
fn a_hung_command_is_aborted_failing_only_its_owner() {
    let mem = shared_mem();
    let mut device = started_device(MockXhci::with_device(&mem), &mem);
    let index = attach_root_device(&mut device, 1).expect("the keyboard enumerates");
    arm_report_request_for(&mut device, index);
    let start = device.elapsed_us();
    device.host_mut().model_mut().hang_command = Some(TrbType::NoOpCommand);
    let hung = device.start(|engine| async move {
        engine
            .command(Trb::new(TrbType::NoOpCommand, 0, 0, 0))
            .await
    });
    let behind = device.start(|engine| async move {
        engine
            .command(Trb::new(TrbType::NoOpCommand, 0, 0, 0))
            .await
    });
    device.serve();

    // A report lands while the command hangs, and is delivered at once.
    device
        .host_mut()
        .model_mut()
        .pending_reports
        .push_back(vec![0x00, 0x00, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00]);
    device.host_mut().model_mut().process_int_ring();
    device.serve();
    let mut report = [0u8; BOOT_REPORT_LEN];
    assert_eq!(
        device
            .core_mut()
            .next_report(index, BOOT_REPORT_LEN, &mut report),
        Ok(Some(BOOT_REPORT_LEN))
    );
    assert_eq!(device.elapsed_us(), start);
    assert!(hung.borrow().is_none() && behind.borrow().is_none());

    device.settle();
    assert_eq!(
        hung.borrow_mut().take(),
        Some(Err(DriverError::DeviceFault)),
        "the hung command fails"
    );
    assert!(
        matches!(behind.borrow_mut().take(), Some(Ok(_))),
        "the command queued behind it ran once the ring restarted"
    );
    assert_eq!(
        device.elapsed_us() - start,
        5_000_000,
        "the hung command's budget, once"
    );
    assert!(
        !device.controller_faulted(),
        "an aborted command is not a dead ring"
    );
    let mut host = device.host_mut();
    let mock = host.model_mut();
    assert!(
        !mock.cmd_pointer_lost,
        "the abort named where the ring stopped, so a ring that stopped first kept its place"
    );
    drop(host);
    assert!(device
        .command(Trb::new(TrbType::NoOpCommand, 0, 0, 0))
        .is_ok());
    assert_control_endpoint_serves(&mut device, index);
}

/// The chunks the stick-and-keyboard hub holds once served, from a clean
/// bring-up.
fn served_hub_chunks() -> usize {
    let mem = shared_mem();
    let mut device = started_device(stick_and_keyboard_hub(&mem), &mem);
    device.bring_up().expect("both devices are served");
    let chunks = device.dma_ref().live_chunks();
    chunks
}

#[test]
fn a_controller_reset_mid_enumeration_ends_every_operation_and_leaks_nothing() {
    let mem = shared_mem();
    let mut device = started_device(stick_and_keyboard_hub(&mem), &mem);
    let hub = install_root_hub_on_port_1(&mut device);
    device.host_mut().model_mut().hold_control_of_next_slot = true;
    start_descent(&mut device, hub);
    serve_until_reported(&mut device, HubEvent::Attached(KEYBOARD));
    assert!(
        !device.core().device_live(STICK),
        "the stick is mid-enumeration"
    );

    device
        .reset_and_reenumerate()
        .expect("the controller resets and walks again");
    assert!(device.core().device_live(STICK) && device.core().device_live(KEYBOARD));
    assert_eq!(device.host.live_operations(), 0);
    assert_eq!(device.dma_ref().withheld_chunks(), 0);
    assert_eq!(device.core().recorded_controls(), 0);
    assert_eq!(device.dma_ref().live_chunks(), served_hub_chunks());
}

#[test]
fn a_late_answer_to_an_abandoned_control_td_never_completes_the_next() {
    let mem = shared_mem();
    let mut device = started_device(MockXhci::with_device(&mem), &mem);
    let index = attach_root_device(&mut device, 1).expect("the keyboard enumerates");
    // The device answers just as the stop lands, with bytes no descriptor
    // carries.
    device.host_mut().model_mut().stall_next_control_in = Some(vec![0xEE; 18]);
    let mut descriptor = [0u8; 18];
    assert_eq!(
        device.urb_control_in(index, GET_DEVICE_DESCRIPTOR, &mut descriptor),
        Err(DriverError::TimedOut)
    );
    let mut descriptor = [0u8; 18];
    assert_eq!(
        device.urb_control_in(index, GET_DEVICE_DESCRIPTOR, &mut descriptor),
        Ok(18)
    );
    assert_eq!(
        descriptor[..2],
        [18, 0x01],
        "the next transfer reads the device descriptor, not the late answer"
    );
}

#[test]
fn an_unplugged_device_leaves_no_answer_behind_for_its_held_urb() {
    let mem = shared_mem();
    let mut device = started_device(MockXhci::with_device(&mem), &mem);
    let index = attach_root_device(&mut device, 1).expect("the keyboard enumerates");
    device.host_mut().model_mut().stall_next_control_in = Some(Vec::new());
    let mut descriptor = [0u8; 18];
    assert_eq!(
        device
            .host
            .engine_for(index)
            .control_in(GET_DEVICE_DESCRIPTOR, &mut descriptor),
        Ok(None)
    );
    device.serve();
    device.host_mut().model_mut().pull_root_device();
    assert_eq!(device.next_change(), Some(HubEvent::Detached(index)));
    assert_eq!(
        device
            .host
            .engine_for(index)
            .control_in(GET_DEVICE_DESCRIPTOR, &mut descriptor),
        Err(DriverError::NotFound),
        "its next drive learns the device is gone"
    );
    assert_eq!(
        device.core().recorded_controls(),
        0,
        "no answer waits for ever"
    );
}

#[test]
fn an_abandoned_transfer_reports_where_its_stopped_endpoint_stood() {
    // A device that never answers leaves the stopped controller on the
    // transfer's first TRB; one whose completion was lost leaves it on the TRB
    // after. Only the stop makes the context's dequeue pointer the
    // controller's, so nothing is reported before it.
    let mem = shared_mem();
    let mut device = started_msd(&mem);
    let dci = 7u8;
    let read = UrbRequest {
        endpoint: 3,
        ..stick_read(8)
    };
    device
        .core_mut()
        .queue_bulk_in(0, IN_PIPE, 8)
        .expect("the read queues");
    device.serve();
    device.host.engine_for(0).abandon(&read).expect("abandoned");
    assert_eq!(device.core().abandoned_ep_state(0), Some(1), "running then");
    assert_eq!(
        device.core_mut().take_abandon_report(),
        None,
        "nothing is read before the stop"
    );
    device.settle();
    assert_eq!(
        device.core_mut().take_abandon_report(),
        Some((
            0,
            device::AbandonedTransfer {
                dci,
                ep_state: 1,
                progress: device::TdProgress::NotStarted,
            }
        )),
        "the device was still working on it"
    );
    assert_eq!(device.core_mut().take_abandon_report(), None, "taken once");
    assert_eq!(device.core().abandoned_ep_state(0), None, "settled");

    // The controller finishes the next read, and its completion never comes.
    device.host_mut().model_mut().bulk_in.lost_completions = 1;
    device
        .host_mut()
        .model_mut()
        .bulk_in_responses
        .push_back(vec![0x42; 8]);
    device
        .core_mut()
        .queue_bulk_in(0, IN_PIPE, 8)
        .expect("the next read queues");
    device.serve();
    device.host.engine_for(0).abandon(&read).expect("abandoned");
    device.settle();
    assert_eq!(
        device
            .core_mut()
            .take_abandon_report()
            .map(|(index, said)| (index, said.progress)),
        Some((0, device::TdProgress::Completed)),
        "its completion was lost"
    );
}

#[test]
fn an_abandoned_control_transfer_reports_the_stage_it_stopped_in() {
    // The device took the SETUP and never answered the data stage: the
    // stopped controller stands on the data TRB.
    let mem = shared_mem();
    let mut device = started_msd(&mem);
    device.host_mut().model_mut().stall_next_control_in = Some(Vec::new());
    let mut descriptor = [0u8; 18];
    assert_eq!(
        device
            .host
            .engine_for(0)
            .control_in(GET_DEVICE_DESCRIPTOR, &mut descriptor),
        Ok(None),
        "held"
    );
    device.serve();
    device
        .host
        .engine_for(0)
        .abandon(&descriptor_read())
        .expect("abandoned");
    device.settle();
    assert_eq!(
        device
            .core_mut()
            .take_abandon_report()
            .map(|(index, said)| (index, said.dci, said.progress)),
        Some((0, 1, device::TdProgress::Underway)),
        "stopped in its data stage"
    );
}

#[test]
fn a_dequeue_pointer_is_read_against_the_transfer_and_the_ring_wrap() {
    // Five data slots and the link in slot 5.
    let (ring, _link) = ProducerRing::new(6, 0x1000).expect("ring fits");
    let slot = |n: u64| 0x1000 + n * TRB_LEN as u64;
    let span = |trbs: &[u64]| {
        let mut held = ArrayVec::new();
        for &trb in trbs {
            held.try_push(trb).expect("three TRBs fit");
        }
        device::TdSpan::on(&ring, held).expect("a span on the ring")
    };
    // A control transfer whose status stage sat in the last data slot:
    // finishing it leaves the controller at the base, or on the link that
    // leads there.
    let control = span(&[slot(2), slot(3), slot(4)]);
    assert_eq!(control.progress(slot(2)), device::TdProgress::NotStarted);
    assert_eq!(control.progress(slot(3)), device::TdProgress::Underway);
    assert_eq!(control.progress(slot(4)), device::TdProgress::Underway);
    assert_eq!(control.progress(slot(0)), device::TdProgress::Completed);
    assert_eq!(control.progress(slot(5)), device::TdProgress::Completed);
    assert_eq!(control.progress(slot(1)), device::TdProgress::Elsewhere);
    let bulk = span(&[slot(1)]);
    assert_eq!(bulk.progress(slot(2)), device::TdProgress::Completed);
    assert_eq!(bulk.progress(slot(0)), device::TdProgress::Elsewhere);
    assert!(
        device::TdSpan::on(&ring, ArrayVec::new()).is_none(),
        "a transfer has a TRB"
    );
}
