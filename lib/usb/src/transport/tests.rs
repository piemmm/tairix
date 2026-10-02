//! Unit tests for the URB transport seam: a control-IN and an interrupt-IN
//! round-trip through the client → `drive_urb`/`frame_completion` → mock
//! engine path, and the fail-closed validation `drive_urb` applies before the
//! engine is ever touched.
//!
//! The host double is *synchronous* (an in-process call cannot wait for a
//! controller interrupt), so it maps `drive_urb`'s asynchronous `Ok(None)`
//! ("interrupt-IN report not arrived yet") to a retryable
//! [`Errno::WouldBlock`] completion. In the live HCD that same `Ok(None)`
//! holds the caller's URB call outstanding until the completion interrupt
//! fires (`plans/USB.md` §1.1).

extern crate alloc;

use alloc::rc::Rc;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::RefCell;

use super::{
    control_permitted, drive_urb, frame_completion, UrbCall, UrbClient, UrbEngine, UrbLink,
    UrbScope,
};
use tairix_abi::usb_urb::{
    UrbRequest, UsbDirection, UsbTransferType, URB_COMPLETION_LEN, URB_REQUEST_LEN,
};
use tairix_abi::{DriverError, Errno};

/// An arbitrary shared-buffer handle the URB names; the in-process transport
/// ignores it and uses its own shared buffer (it stands in for the mapped
/// shared memory both the class driver and the HCD would see).
const BUFFER_HANDLE: u64 = 0x0BAD_F00D_0000_0001;

/// A controllable [`UrbEngine`] double: a control-IN transfer copies a fixed
/// response into the caller's buffer, interrupt-IN delivers queued reports
/// once each then reports "nothing pending", and the bulk pair mirrors the
/// live engine's arm-then-reap shape (first drive arms and returns `None`,
/// a later drive completes) with a one-shot STALL knob. Records its call
/// counts so a test can prove a rejected URB never reaches it.
struct MockEngine {
    control_response: Vec<u8>,
    reports: Vec<Vec<u8>>,
    control_calls: usize,
    interrupt_calls: usize,
    /// SETUP packets delivered by completed no-data control-OUT transfers.
    no_data_setups: Vec<[u8; 8]>,
    /// Completed control-OUT data-stage transfers: SETUP + payload.
    control_out_transfers: Vec<([u8; 8], Vec<u8>)>,
    /// Queued device responses for bulk-IN, delivered one per completed TD.
    bulk_in_data: Vec<Vec<u8>>,
    /// Bytes each completed bulk-OUT TD delivered to the device.
    bulk_out_sink: Vec<Vec<u8>>,
    /// An armed bulk-IN TD's requested length, `None` when idle.
    bulk_in_armed: Option<usize>,
    /// An armed bulk-OUT TD's staged bytes, `None` when idle.
    bulk_out_armed: Option<Vec<u8>>,
    /// When set, the next reaped bulk TD STALLs (consumed once).
    stall_next_bulk: bool,
    bulk_calls: usize,
    /// The interface the mock serves.
    scope: UrbScope,
    /// The `request` each interrupt-IN poll carried, and the buffer it was
    /// handed.
    interrupt_requests: Vec<(usize, usize)>,
}

/// The interface's bulk endpoint numbers the mock serves; its interrupt-IN
/// endpoint is endpoint 3.
const BULK_IN_ENDPOINT: u8 = 1;
const BULK_OUT_ENDPOINT: u8 = 2;
const INTERRUPT_ENDPOINT: u8 = 3;

/// Device Context Indices of the mock interface's three endpoints.
const MOCK_ENDPOINTS: u32 = 1 << 3 | 1 << 4 | 1 << 7;

impl MockEngine {
    fn new() -> Self {
        Self {
            control_response: Vec::new(),
            reports: Vec::new(),
            control_calls: 0,
            interrupt_calls: 0,
            no_data_setups: Vec::new(),
            control_out_transfers: Vec::new(),
            bulk_in_data: Vec::new(),
            bulk_out_sink: Vec::new(),
            bulk_in_armed: None,
            bulk_out_armed: None,
            stall_next_bulk: false,
            bulk_calls: 0,
            scope: UrbScope {
                interface: 0,
                endpoints: MOCK_ENDPOINTS,
            },
            interrupt_requests: Vec::new(),
        }
    }
}

impl UrbEngine for MockEngine {
    fn control_in(&mut self, _setup: [u8; 8], data: &mut [u8]) -> Result<usize, DriverError> {
        self.control_calls += 1;
        let n = self.control_response.len().min(data.len());
        data[..n].copy_from_slice(&self.control_response[..n]);
        Ok(n)
    }

    fn control_no_data(&mut self, setup: [u8; 8]) -> Result<(), DriverError> {
        self.no_data_setups.push(setup);
        Ok(())
    }

    fn control_out(&mut self, setup: [u8; 8], data: &[u8]) -> Result<(), DriverError> {
        self.control_out_transfers.push((setup, data.to_vec()));
        Ok(())
    }

    fn scope(&self) -> Option<UrbScope> {
        Some(self.scope)
    }

    fn interrupt_in(
        &mut self,
        request: usize,
        data: &mut [u8],
    ) -> Result<Option<usize>, DriverError> {
        self.interrupt_calls += 1;
        self.interrupt_requests.push((request, data.len()));
        if self.reports.is_empty() {
            return Ok(None);
        }
        let report = self.reports.remove(0);
        let n = report.len().min(data.len());
        data[..n].copy_from_slice(&report[..n]);
        Ok(Some(n))
    }

    fn bulk_in(&mut self, endpoint: u8, data: &mut [u8]) -> Result<Option<usize>, DriverError> {
        self.bulk_calls += 1;
        if endpoint != BULK_IN_ENDPOINT {
            return Err(DriverError::OutOfRange);
        }
        if self.bulk_in_armed.is_none() {
            self.bulk_in_armed = Some(data.len());
            return Ok(None);
        }
        self.bulk_in_armed = None;
        if self.stall_next_bulk {
            self.stall_next_bulk = false;
            return Err(DriverError::EndpointStalled);
        }
        if self.bulk_in_data.is_empty() {
            return Ok(None);
        }
        let response = self.bulk_in_data.remove(0);
        let n = response.len().min(data.len());
        data[..n].copy_from_slice(&response[..n]);
        Ok(Some(n))
    }

    fn bulk_out(&mut self, endpoint: u8, data: &[u8]) -> Result<Option<usize>, DriverError> {
        self.bulk_calls += 1;
        if endpoint != BULK_OUT_ENDPOINT {
            return Err(DriverError::OutOfRange);
        }
        if self.bulk_out_armed.is_none() {
            self.bulk_out_armed = Some(data.to_vec());
            return Ok(None);
        }
        let staged = self.bulk_out_armed.take().unwrap_or_default();
        if self.stall_next_bulk {
            self.stall_next_bulk = false;
            return Err(DriverError::EndpointStalled);
        }
        let n = staged.len();
        self.bulk_out_sink.push(staged);
        Ok(Some(n))
    }
}

/// An in-process [`UrbCall`] that routes a URB straight through
/// [`drive_urb`]/[`frame_completion`] over a shared buffer — the host stand-in
/// for the kernel IPC call the class driver issues. The shared buffer is the
/// single memory both sides see.
struct DirectCall {
    engine: Rc<RefCell<MockEngine>>,
    buffer: Rc<RefCell<Vec<u8>>>,
}

impl UrbCall for DirectCall {
    fn call(&mut self, request: &[u8], reply: &mut [u8]) -> Result<usize, Errno> {
        let mut buffer = self.buffer.borrow_mut();
        let mut engine = self.engine.borrow_mut();
        // The synchronous host double surfaces a not-yet-ready interrupt-IN
        // (`Ok(None)`) as the retryable `WouldBlock`; the live HCD instead
        // holds the call outstanding until its completion interrupt.
        let result = match drive_urb(request, &mut buffer, &mut *engine) {
            Ok(Some(transferred)) => Ok(transferred),
            Ok(None) => Err(Errno::WouldBlock),
            Err(err) => Err(err),
        };
        frame_completion(reply, result)
    }
}

/// Serve one URB directly (no client), returning the decoded completion. Used
/// by the fail-closed tests, which assert on the in-band errno.
fn serve_one(urb: &UrbRequest, buffer_len: usize, engine: &mut MockEngine) -> Result<u32, Errno> {
    let mut request = [0u8; URB_REQUEST_LEN];
    let n = urb.encode(&mut request).expect("encodes");
    let mut buffer = vec![0u8; buffer_len];
    let mut reply = [0u8; URB_COMPLETION_LEN];
    let result = match drive_urb(&request[..n], &mut buffer, engine) {
        Ok(Some(transferred)) => Ok(transferred),
        Ok(None) => Err(Errno::WouldBlock),
        Err(err) => Err(err),
    };
    let len = frame_completion(&mut reply, result).expect("frames a reply");
    tairix_abi::usb_urb::decode_completion(&reply[..len])
}

#[test]
fn control_in_round_trips_through_the_client() {
    let engine = Rc::new(RefCell::new(MockEngine::new()));
    let descriptor = vec![0x12, 0x01, 0x00, 0x02, 0x00, 0x00, 0x00, 0x40];
    engine.borrow_mut().control_response = descriptor.clone();
    let buffer = Rc::new(RefCell::new(vec![0u8; 64]));

    let mut client = UrbClient::new(DirectCall {
        engine: engine.clone(),
        buffer: buffer.clone(),
    });

    // A GET_DESCRIPTOR(device) SETUP packet for its first eight bytes.
    let setup = [0x80, 0x06, 0x00, 0x01, 0x00, 0x00, 0x08, 0x00];
    let transferred = client
        .control_in(setup, BUFFER_HANDLE, 8)
        .expect("control-IN completes");
    assert_eq!(transferred, 8);
    // The device's bytes landed in the shared buffer the class driver reads.
    assert_eq!(&buffer.borrow()[..8], &descriptor[..]);
    assert_eq!(engine.borrow().control_calls, 1);
}

#[test]
fn interrupt_in_round_trips_and_then_reports_would_block() {
    let engine = Rc::new(RefCell::new(MockEngine::new()));
    let report = vec![0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00];
    engine.borrow_mut().reports = vec![report.clone()];
    let buffer = Rc::new(RefCell::new(vec![0u8; 8]));

    let mut client = UrbClient::new(DirectCall {
        engine: engine.clone(),
        buffer: buffer.clone(),
    });

    // The first poll delivers the queued report.
    let transferred = client
        .interrupt_in(INTERRUPT_ENDPOINT, BUFFER_HANDLE, 8)
        .expect("interrupt-IN completes");
    assert_eq!(transferred, 8);
    assert_eq!(&buffer.borrow()[..8], &report[..]);

    // With nothing pending, a non-blocking poll fails closed with the
    // retryable `WouldBlock` rather than fabricating a report.
    assert_eq!(
        client.interrupt_in(INTERRUPT_ENDPOINT, BUFFER_HANDLE, 8),
        Err(Errno::WouldBlock)
    );
    assert_eq!(engine.borrow().interrupt_calls, 2);
}

#[test]
fn an_interrupt_poll_names_its_length_and_receives_the_whole_buffer() {
    let mut engine = MockEngine::new();
    engine.reports = vec![vec![7; 40]];
    let urb = UrbRequest {
        endpoint: INTERRUPT_ENDPOINT,
        transfer_type: UsbTransferType::Interrupt,
        direction: UsbDirection::In,
        buffer: BUFFER_HANDLE,
        length: 9,
        setup: [0; 8],
    };
    assert_eq!(
        serve_one(&urb, 64, &mut engine),
        Ok(40),
        "a report may outrun the request"
    );
    assert_eq!(engine.interrupt_requests, [(9, 64)]);
}

#[test]
fn an_interrupt_poll_on_another_interfaces_endpoint_is_refused() {
    let mut engine = MockEngine::new();
    for endpoint in [4, 2] {
        let urb = UrbRequest {
            endpoint,
            transfer_type: UsbTransferType::Interrupt,
            direction: UsbDirection::In,
            buffer: BUFFER_HANDLE,
            length: 8,
            setup: [0; 8],
        };
        assert_eq!(
            serve_one(&urb, 8, &mut engine),
            Err(Errno::OutOfRange),
            "endpoint {endpoint}"
        );
    }
    assert_eq!(engine.interrupt_calls, 0);
}

/// A control-IN URB carrying `setup` for its own `wLength`.
fn control(setup: [u8; 8]) -> UrbRequest {
    let length = u32::from(u16::from_le_bytes([setup[6], setup[7]]));
    UrbRequest {
        endpoint: 0,
        transfer_type: UsbTransferType::Control,
        direction: if setup[0] & 0x80 != 0 {
            UsbDirection::In
        } else {
            UsbDirection::Out
        },
        buffer: BUFFER_HANDLE,
        length,
        setup,
    }
}

#[test]
fn a_class_driver_may_read_the_device_and_drive_its_own_interface() {
    let scope = UrbScope {
        interface: 2,
        endpoints: MOCK_ENDPOINTS,
    };
    let permitted = |setup: [u8; 8]| {
        let urb = control(setup);
        control_permitted(setup, urb.direction, urb.length, scope)
    };
    // Device, configuration and string descriptors; device and own endpoint status.
    assert!(permitted([0x80, 0x06, 0x00, 0x02, 0x00, 0x00, 0x22, 0x00]));
    assert!(permitted([0x80, 0x06, 0x02, 0x03, 0x09, 0x04, 0xFF, 0x00]));
    assert!(permitted([0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00]));
    assert!(permitted([0x82, 0x00, 0x00, 0x00, 0x83, 0x00, 0x02, 0x00]));
    // The HID report descriptor, and HID class requests, on its own interface.
    assert!(permitted([0x81, 0x06, 0x00, 0x22, 0x02, 0x00, 0x40, 0x01]));
    assert!(permitted([0x21, 0x0B, 0x01, 0x00, 0x02, 0x00, 0x00, 0x00]));
    assert!(permitted([0xA1, 0x01, 0x05, 0x03, 0x02, 0x00, 0x03, 0x00]));
    assert!(permitted([0x21, 0x09, 0x03, 0x03, 0x02, 0x00, 0x02, 0x00]));
    // A vendor request to its own interface.
    assert!(permitted([0xC1, 0x01, 0x00, 0x00, 0x02, 0x00, 0x04, 0x00]));
}

#[test]
fn a_class_driver_may_not_change_the_device_or_reach_a_sibling() {
    let scope = UrbScope {
        interface: 2,
        endpoints: MOCK_ENDPOINTS,
    };
    let refused = [
        // SET_CONFIGURATION, SET_ADDRESS, SET_FEATURE(remote wakeup), SET_DESCRIPTOR.
        [0x00, 0x09, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00],
        [0x00, 0x05, 0x07, 0x00, 0x00, 0x00, 0x00, 0x00],
        [0x00, 0x03, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00],
        [0x00, 0x07, 0x00, 0x01, 0x00, 0x00, 0x12, 0x00],
        // SET_INTERFACE and SET_FEATURE on its own interface.
        [0x01, 0x0B, 0x01, 0x00, 0x02, 0x00, 0x00, 0x00],
        [0x01, 0x03, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00],
        // CLEAR_FEATURE(halt) on its own endpoint: halt recovery is the host's.
        [0x02, 0x01, 0x00, 0x00, 0x83, 0x00, 0x00, 0x00],
        // A sibling interface's report descriptor and class request.
        [0x81, 0x06, 0x00, 0x22, 0x01, 0x00, 0x40, 0x00],
        [0x21, 0x0B, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00],
        // A sibling's endpoint, a class or vendor request to the device, a
        // reserved request type.
        [0x82, 0x00, 0x00, 0x00, 0x85, 0x00, 0x02, 0x00],
        [0x20, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00],
        [0xC0, 0x01, 0x00, 0x00, 0x02, 0x00, 0x04, 0x00],
        [0xE1, 0x01, 0x00, 0x00, 0x02, 0x00, 0x04, 0x00],
    ];
    for setup in refused {
        let urb = control(setup);
        assert!(
            !control_permitted(setup, urb.direction, urb.length, scope),
            "{setup:02x?}"
        );
    }
}

#[test]
fn a_control_request_must_state_the_urbs_own_data_stage() {
    let scope = UrbScope {
        interface: 0,
        endpoints: MOCK_ENDPOINTS,
    };
    let setup = [0x80, 0x06, 0x00, 0x01, 0x00, 0x00, 0x12, 0x00];
    assert!(control_permitted(setup, UsbDirection::In, 18, scope));
    assert!(
        !control_permitted(setup, UsbDirection::In, 8, scope),
        "a shorter URB"
    );
    assert!(
        !control_permitted(setup, UsbDirection::Out, 18, scope),
        "a reversed direction"
    );
}

#[test]
fn a_refused_control_request_never_reaches_the_engine() {
    let mut engine = MockEngine::new();
    let urb = control([0x00, 0x09, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00]);
    assert_eq!(
        serve_one(&urb, 8, &mut engine),
        Err(Errno::PermissionDenied)
    );
    assert!(engine.no_data_setups.is_empty());
    let urb = control([0x81, 0x06, 0x00, 0x22, 0x05, 0x00, 0x08, 0x00]);
    assert_eq!(
        serve_one(&urb, 8, &mut engine),
        Err(Errno::PermissionDenied)
    );
    assert_eq!(engine.control_calls, 0);
}

#[test]
fn rejects_oversize_length_before_the_engine() {
    let mut engine = MockEngine::new();
    let urb = UrbRequest {
        endpoint: 1,
        transfer_type: UsbTransferType::Interrupt,
        direction: UsbDirection::In,
        buffer: BUFFER_HANDLE,
        // One byte past the 8-byte shared buffer.
        length: 9,
        setup: [0; 8],
    };
    assert_eq!(
        serve_one(&urb, 8, &mut engine),
        Err(Errno::LengthOutOfRange)
    );
    // The transfer never reached the engine.
    assert_eq!(engine.interrupt_calls, 0);
}

#[test]
fn rejects_interrupt_on_the_control_endpoint() {
    let mut engine = MockEngine::new();
    let urb = UrbRequest {
        endpoint: 0,
        transfer_type: UsbTransferType::Interrupt,
        direction: UsbDirection::In,
        buffer: BUFFER_HANDLE,
        length: 8,
        setup: [0; 8],
    };
    assert_eq!(serve_one(&urb, 8, &mut engine), Err(Errno::OutOfRange));
    assert_eq!(engine.interrupt_calls, 0);
}

#[test]
fn rejects_control_on_a_device_endpoint() {
    let mut engine = MockEngine::new();
    let urb = UrbRequest {
        endpoint: 3,
        transfer_type: UsbTransferType::Control,
        direction: UsbDirection::In,
        buffer: BUFFER_HANDLE,
        length: 8,
        setup: [0; 8],
    };
    assert_eq!(serve_one(&urb, 8, &mut engine), Err(Errno::OutOfRange));
    assert_eq!(engine.control_calls, 0);
}

#[test]
fn rejects_illegal_direction() {
    let mut engine = MockEngine::new();
    // An interrupt-OUT is not a boot-report transfer; refuse it.
    let interrupt_out = UrbRequest {
        endpoint: 1,
        transfer_type: UsbTransferType::Interrupt,
        direction: UsbDirection::Out,
        buffer: BUFFER_HANDLE,
        length: 8,
        setup: [0; 8],
    };
    assert_eq!(
        serve_one(&interrupt_out, 8, &mut engine),
        Err(Errno::OutOfRange)
    );
    assert_eq!(engine.interrupt_calls, 0);

    // A control-OUT data stage on a device endpoint is illegal (control
    // transfers are the endpoint-0 protocol).
    let control_out_bad_endpoint = UrbRequest {
        endpoint: 1,
        transfer_type: UsbTransferType::Control,
        direction: UsbDirection::Out,
        buffer: BUFFER_HANDLE,
        length: 8,
        setup: [0; 8],
    };
    assert_eq!(
        serve_one(&control_out_bad_endpoint, 8, &mut engine),
        Err(Errno::OutOfRange)
    );
    assert_eq!(engine.control_calls, 0);
    assert!(engine.no_data_setups.is_empty());
    assert!(engine.control_out_transfers.is_empty());
}

#[test]
fn control_out_data_stage_delivers_the_shared_buffers_bytes() {
    // The CBI ADSC path: a control-OUT whose data stage carries the shared
    // buffer's bytes to the engine, completing with the full length.
    let mut engine = MockEngine::new();
    engine.scope.interface = 1;
    let setup = [0x21, 0x00, 0, 0, 1, 0, 12, 0];
    let request = UrbRequest {
        endpoint: 0,
        transfer_type: UsbTransferType::Control,
        direction: UsbDirection::Out,
        buffer: BUFFER_HANDLE,
        length: 12,
        setup,
    };
    let mut data = [0u8; 12];
    data[0] = 0x28;
    let mut frame = [0u8; URB_REQUEST_LEN];
    let n = request.encode(&mut frame).expect("encodes");
    let outcome = drive_urb(&frame[..n], &mut data, &mut engine).expect("served");
    assert_eq!(outcome, Some(12));
    assert_eq!(engine.control_out_transfers.len(), 1);
    let (seen_setup, seen_data) = &engine.control_out_transfers[0];
    assert_eq!(seen_setup, &setup);
    assert_eq!(seen_data.as_slice(), &data[..]);
}

#[test]
fn control_no_data_round_trips_through_the_client() {
    let engine = Rc::new(RefCell::new(MockEngine::new()));
    let buffer = Rc::new(RefCell::new(vec![0u8; 8]));

    let mut client = UrbClient::new(DirectCall {
        engine: engine.clone(),
        buffer,
    });

    // A BOT Bulk-Only Mass Storage Reset SETUP packet.
    let setup = [0x21, 0xFF, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
    client
        .control_no_data(setup)
        .expect("no-data control-OUT completes");
    // The engine received exactly the SETUP packet, once.
    assert_eq!(engine.borrow().no_data_setups, vec![setup]);
    assert_eq!(engine.borrow().control_calls, 0);
}

#[test]
fn bulk_in_round_trips_through_the_client() {
    let engine = Rc::new(RefCell::new(MockEngine::new()));
    let payload = vec![0xA5u8; 16];
    engine.borrow_mut().bulk_in_data = vec![payload.clone()];
    let buffer = Rc::new(RefCell::new(vec![0u8; 16]));

    let mut client = UrbClient::new(DirectCall {
        engine: engine.clone(),
        buffer: buffer.clone(),
    });

    // The first drive arms the TD (the synchronous double surfaces the held
    // URB as the retryable `WouldBlock`); the re-drive reaps its completion.
    assert_eq!(
        client.bulk_in(BULK_IN_ENDPOINT, BUFFER_HANDLE, 16),
        Err(Errno::WouldBlock)
    );
    let transferred = client
        .bulk_in(BULK_IN_ENDPOINT, BUFFER_HANDLE, 16)
        .expect("bulk-IN completes");
    assert_eq!(transferred, 16);
    assert_eq!(&buffer.borrow()[..16], &payload[..]);
}

#[test]
fn bulk_out_round_trips_through_the_client() {
    let engine = Rc::new(RefCell::new(MockEngine::new()));
    let buffer = Rc::new(RefCell::new(vec![0x5Au8; 12]));

    let mut client = UrbClient::new(DirectCall {
        engine: engine.clone(),
        buffer: buffer.clone(),
    });

    assert_eq!(
        client.bulk_out(BULK_OUT_ENDPOINT, BUFFER_HANDLE, 12),
        Err(Errno::WouldBlock)
    );
    let transferred = client
        .bulk_out(BULK_OUT_ENDPOINT, BUFFER_HANDLE, 12)
        .expect("bulk-OUT completes");
    assert_eq!(transferred, 12);
    // The device received exactly the shared buffer's bytes.
    assert_eq!(engine.borrow().bulk_out_sink, vec![vec![0x5Au8; 12]]);
}

#[test]
fn rejects_bulk_on_the_control_endpoint() {
    let mut engine = MockEngine::new();
    let urb = UrbRequest {
        endpoint: 0,
        transfer_type: UsbTransferType::Bulk,
        direction: UsbDirection::In,
        buffer: BUFFER_HANDLE,
        length: 8,
        setup: [0; 8],
    };
    assert_eq!(serve_one(&urb, 8, &mut engine), Err(Errno::OutOfRange));
    assert_eq!(engine.bulk_calls, 0);
}

#[test]
fn a_wrong_bulk_endpoint_fails_closed_in_band() {
    // The engine owns the interface's endpoint map; a bulk URB naming an
    // endpoint that is not the configured one in that direction is refused
    // and the refusal framed in band.
    let mut engine = MockEngine::new();
    let urb = UrbRequest {
        endpoint: 7,
        transfer_type: UsbTransferType::Bulk,
        direction: UsbDirection::In,
        buffer: BUFFER_HANDLE,
        length: 8,
        setup: [0; 8],
    };
    assert_eq!(serve_one(&urb, 8, &mut engine), Err(Errno::OutOfRange));
}

#[test]
fn a_stalled_bulk_transfer_surfaces_endpoint_stalled_in_band() {
    let engine = Rc::new(RefCell::new(MockEngine::new()));
    engine.borrow_mut().stall_next_bulk = true;
    let buffer = Rc::new(RefCell::new(vec![0u8; 8]));

    let mut client = UrbClient::new(DirectCall {
        engine: engine.clone(),
        buffer,
    });

    // Arm, then reap the STALL: the completion carries the distinct
    // `EndpointStalled` so a class driver can run its own (BOT) recovery.
    assert_eq!(
        client.bulk_in(BULK_IN_ENDPOINT, BUFFER_HANDLE, 8),
        Err(Errno::WouldBlock)
    );
    assert_eq!(
        client.bulk_in(BULK_IN_ENDPOINT, BUFFER_HANDLE, 8),
        Err(Errno::EndpointStalled)
    );
}

#[test]
fn malformed_request_is_framed_in_band() {
    // A truncated request never reaches the engine and is answered with a
    // status-framed error completion the client decodes.
    let mut engine = MockEngine::new();
    let short = [0u8; URB_REQUEST_LEN - 1];
    let mut buffer = [0u8; 8];
    let mut reply = [0u8; URB_COMPLETION_LEN];
    // A truncated request fails `drive_urb` decode before the engine; the
    // error is framed in band exactly as the HCD would reply it.
    let result = match drive_urb(&short, &mut buffer, &mut engine) {
        Ok(Some(transferred)) => Ok(transferred),
        Ok(None) => Err(Errno::WouldBlock),
        Err(err) => Err(err),
    };
    let len = frame_completion(&mut reply, result).expect("frames a reply");
    assert_eq!(
        tairix_abi::usb_urb::decode_completion(&reply[..len]),
        Err(Errno::LengthOutOfRange)
    );
    assert_eq!(engine.control_calls, 0);
    assert_eq!(engine.interrupt_calls, 0);
}

/// A [`UrbCall`] answering each URB from a script, recording what it was sent.
struct ScriptedCall {
    replies: Vec<Result<u32, Errno>>,
    sent: Rc<RefCell<Vec<UrbRequest>>>,
}

impl UrbCall for ScriptedCall {
    fn call(&mut self, request: &[u8], reply: &mut [u8]) -> Result<usize, Errno> {
        self.sent
            .borrow_mut()
            .push(UrbRequest::decode(request).expect("a well-formed URB"));
        let result = if self.replies.is_empty() {
            Err(Errno::WouldBlock)
        } else {
            self.replies.remove(0)
        };
        frame_completion(reply, result)
    }
}

/// A link over `shm` whose URBs complete with `replies`, and what it sends.
fn scripted_link(
    shm: &mut [u8],
    replies: Vec<Result<u32, Errno>>,
) -> (UrbLink<'_, ScriptedCall>, Rc<RefCell<Vec<UrbRequest>>>) {
    let sent = Rc::new(RefCell::new(Vec::new()));
    let call = ScriptedCall {
        replies,
        sent: sent.clone(),
    };
    (UrbLink::new(UrbClient::new(call), shm), sent)
}

#[test]
fn a_link_copies_out_what_a_control_read_delivered() {
    let mut shm: Vec<u8> = (1..=32).collect();
    let (mut link, sent) = scripted_link(&mut shm, vec![Ok(6)]);
    let mut data = [0u8; 8];
    assert_eq!(
        link.control_in([0x80, 0x06, 0, 1, 0, 0, 8, 0], &mut data),
        Ok(6)
    );
    assert_eq!(data, [1, 2, 3, 4, 5, 6, 0, 0]);
    assert_eq!(sent.borrow()[0].length, 8);
    let mut long = [0u8; 33];
    assert_eq!(
        link.control_in([0x80, 0x06, 0, 1, 0, 0, 33, 0], &mut long),
        Err(Errno::LengthOutOfRange)
    );
}

#[test]
fn a_link_stages_a_control_writes_data_before_it_sends_it() {
    let mut shm = vec![0u8; 16];
    let (mut link, sent) = scripted_link(&mut shm, vec![Ok(3)]);
    assert_eq!(
        link.control_out([0x21, 0x09, 0, 3, 0, 0, 3, 0], &[7, 8, 9]),
        Ok(())
    );
    assert_eq!(sent.borrow()[0].length, 3);
    drop(link);
    assert_eq!(shm[..3], [7, 8, 9]);
}

#[test]
fn a_link_takes_a_report_longer_than_it_asked_for_but_never_truncates_one() {
    let mut shm = vec![5u8; 64];
    let (mut link, sent) = scripted_link(&mut shm, vec![Ok(20), Ok(20)]);
    let mut short = [0u8; 8];
    assert_eq!(
        link.interrupt_in(INTERRUPT_ENDPOINT, 8, &mut short),
        Err(Errno::BufferTooSmall)
    );
    let mut room = [0u8; 32];
    assert_eq!(link.interrupt_in(INTERRUPT_ENDPOINT, 8, &mut room), Ok(20));
    assert_eq!(room[..20], [5; 20]);
    assert!(sent.borrow().iter().all(|urb| urb.length == 8));
}

#[test]
fn a_link_splits_a_bulk_read_into_buffer_sized_urbs_and_stops_at_a_short_one() {
    let mut shm = vec![9u8; 16];
    let (mut link, sent) = scripted_link(&mut shm, vec![Ok(16), Ok(16), Ok(5)]);
    let mut data = [0u8; 64];
    assert_eq!(link.bulk_in(BULK_IN_ENDPOINT, &mut data), Ok(37));
    assert_eq!(
        sent.borrow().len(),
        3,
        "the short third chunk ends the transfer"
    );
    assert!(sent.borrow().iter().all(|urb| urb.length == 16));
}
