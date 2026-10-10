//! Host tests for the interface table ([`super::Interfaces`]) over a mock
//! controller and kernel.
//!
//! The mock journals every kernel-visible step in the order the table took
//! it, so a test asserts sequencing as well as outcome, and it retires the
//! region a removed node carried, refusing any later node that carries it, as
//! `hw_emit_node` does.

extern crate alloc;

use alloc::rc::Rc;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::num::NonZeroU32;

use alloc::collections::VecDeque;

use super::{Interfaces, Note, Seam, UrbBuffer, ENDPOINT_CAPACITY};
use crate::domain::{ControllerDomainEvent, ControllerHealth, CONTROLLER_GRACE_NS};
use crate::serve::UrbReply;
use tairix_abi::hwtree::{HwResourceKind, HW_NODE_ROOT};
use tairix_abi::reply::{decode_status_reply, STATUS_REPLY_LEN};
use tairix_abi::usb_urb::{
    decode_completion, iso_notify_endpoint_for, IsoGrant, IsoLayout, IsoNotify, IsoStartParams,
    UrbRequest, UsbDirection, UsbRequest, UsbSpeed, UsbTransferType, ISO_GRANT_REPLY_LEN,
    USB_REQUEST_MAX_LEN,
};
use tairix_abi::{
    DriverError, Errno, HwDeviceClass, HwMatchKey, HwNode, HwResource, ProcId, PROC_ID_LEN,
};
use tairix_inline::BitSet256;
use tairix_usb::device::{DeviceIdentity, HubEvent, SerialNumber};
use tairix_usb::transport::{IsoSlotDone, IsoStreamShape, UrbEngine, UrbScope};

/// The id the mock binds transport slot 0's endpoint at; slot `n` is
/// `ENDPOINT_BASE + n`.
const ENDPOINT_BASE: u64 = 0x5500;

/// One kernel-visible step.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Step {
    Open(usize),
    Watch(usize),
    Map(u64),
    Unmap(u64),
    Emit {
        node: u32,
        index: usize,
        endpoint: u64,
        region: u64,
    },
    Remove(u32),
    Reply {
        endpoint: u64,
        ticket: u64,
        result: Result<u32, Errno>,
    },
    Granted {
        endpoint: u64,
        ticket: u64,
        grant: IsoGrant,
    },
    Delegated {
        region: u64,
        ticket: u64,
    },
    Notified {
        port: u64,
        notice: IsoNotify,
    },
    Reset,
}

type Journal = Rc<RefCell<Vec<Step>>>;

struct Buffer {
    region: u64,
    bytes: Vec<u8>,
    journal: Journal,
}

impl UrbBuffer for Buffer {
    fn region(&self) -> u64 {
        self.region
    }

    fn bytes(&mut self) -> &mut [u8] {
        &mut self.bytes
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        self.journal.borrow_mut().push(Step::Unmap(self.region));
    }
}

/// The device a device-table index serves, as its transfers see it.
#[derive(Default)]
struct Script {
    reports: Vec<Vec<u8>>,
    fault: Option<DriverError>,
    gone: bool,
    interrupt_calls: usize,
    /// The interface and stream operations the engine was asked for.
    operations: Vec<UsbRequest>,
    /// What the next interface operation answers.
    refusal: Option<DriverError>,
    /// What each `iso_take` answers, oldest first; then `Ok(None)`.
    takes: VecDeque<Result<Option<IsoSlotDone>, DriverError>>,
    /// The region bytes each `iso_queue` was handed, by length.
    queued_regions: Vec<usize>,
}

struct Engine<'a>(&'a mut Script);

impl UrbEngine for Engine<'_> {
    fn control_in(&mut self, _setup: [u8; 8], _data: &mut [u8]) -> Result<usize, DriverError> {
        Ok(0)
    }

    fn control_no_data(&mut self, _setup: [u8; 8]) -> Result<(), DriverError> {
        Err(DriverError::NotFound)
    }

    fn control_out(&mut self, _setup: [u8; 8], _data: &[u8]) -> Result<(), DriverError> {
        Err(DriverError::NotFound)
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
        self.0.interrupt_calls += 1;
        if let Some(err) = self.0.fault.take() {
            return Err(err);
        }
        if self.0.reports.is_empty() {
            return Ok(None);
        }
        let report = self.0.reports.remove(0);
        let n = report.len().min(data.len());
        data[..n].copy_from_slice(&report[..n]);
        Ok(Some(n))
    }

    fn bulk_in(&mut self, _endpoint: u8, _data: &mut [u8]) -> Result<Option<usize>, DriverError> {
        Err(DriverError::NotFound)
    }

    fn bulk_out(&mut self, _endpoint: u8, _data: &[u8]) -> Result<Option<usize>, DriverError> {
        Err(DriverError::NotFound)
    }

    fn set_interface(&mut self, interface: u8, alternate: u8) -> Result<(), DriverError> {
        self.0.operations.push(UsbRequest::SetInterface {
            interface,
            alternate,
        });
        self.0.refusal.take().map_or(Ok(()), Err)
    }

    fn claim_interface(&mut self, interface: u8) -> Result<(), DriverError> {
        self.0
            .operations
            .push(UsbRequest::ClaimInterface { interface });
        self.0.refusal.take().map_or(Ok(()), Err)
    }

    fn iso_start(
        &mut self,
        endpoint: u8,
        layout: IsoLayout,
    ) -> Result<IsoStreamShape, DriverError> {
        self.0
            .operations
            .push(UsbRequest::IsoStart(IsoStartParams { endpoint, layout }));
        self.0.refusal.take().map_or(
            Ok(IsoStreamShape {
                interval_microframes: 8,
                speed: UsbSpeed::Full,
            }),
            Err,
        )
    }

    fn iso_queue(&mut self, endpoint: u8, slot: u16, region: &[u8]) -> Result<(), DriverError> {
        self.0
            .operations
            .push(UsbRequest::IsoQueue { endpoint, slot });
        self.0.queued_regions.push(region.len());
        self.0.refusal.take().map_or(Ok(()), Err)
    }

    fn iso_stop(&mut self, endpoint: u8) -> Result<(), DriverError> {
        self.0.operations.push(UsbRequest::IsoStop { endpoint });
        Ok(())
    }

    fn iso_take(
        &mut self,
        _endpoint: u8,
        _region: &mut [u8],
    ) -> Result<Option<IsoSlotDone>, DriverError> {
        self.0.takes.pop_front().unwrap_or(Ok(None))
    }
}

/// The pid the mock kernel attests for every caller.
const CALLER_PID: u64 = 0x2A;

/// The instance the mock HCD runs as.
const INSTANCE: ProcId = ProcId::from_raw([0x7E; PROC_ID_LEN]);

/// What the mock kernel turns down, standing in for exhausted resources.
#[derive(Default)]
struct Refusals {
    open: bool,
    /// How many endpoint watches to refuse before taking one.
    watches: usize,
    buffers: bool,
    emits: bool,
    /// What a delegation answers, when refused.
    grant: Option<Errno>,
    /// Ports whose notifications the kernel refuses.
    notices: Vec<u64>,
}

#[derive(Default)]
struct Mock {
    journal: Journal,
    table: Vec<Option<DeviceIdentity>>,
    /// What the next reset that succeeds enumerates.
    reset_table: Vec<Option<DeviceIdentity>>,
    scripts: Vec<Script>,
    queued: Vec<(u64, u64, Vec<u8>)>,
    faulted: bool,
    reset_fails: bool,
    /// A detach latches a controller fault, as the VL805's does.
    detach_faults: bool,
    refuse: Refusals,
    /// The region each live node carries.
    carried: Vec<(u32, u64)>,
    retired: Vec<u64>,
    next_node: u32,
    next_region: u64,
    now: u64,
    notes: Vec<Note>,
}

impl Mock {
    fn new(table: Vec<Option<DeviceIdentity>>) -> Self {
        Self {
            reset_table: table.clone(),
            table,
            ..Self::default()
        }
    }

    /// The steps taken since the last call.
    fn steps(&self) -> Vec<Step> {
        self.journal.borrow_mut().drain(..).collect()
    }

    fn post(&mut self, endpoint: u64, ticket: u64, request: Vec<u8>) {
        self.queued.push((endpoint, ticket, request));
    }

    fn script(&mut self, index: usize) -> &mut Script {
        if index >= self.scripts.len() {
            self.scripts.resize_with(index + 1, Script::default);
        }
        &mut self.scripts[index]
    }

    fn domain_notes(&self) -> Vec<ControllerDomainEvent> {
        self.notes
            .iter()
            .filter_map(|note| match note {
                Note::Domain { event, .. } => Some(*event),
                _ => None,
            })
            .collect()
    }
}

impl Seam for Mock {
    type Buffer = Buffer;
    type Engine<'a> = Engine<'a>;

    fn table_len(&self) -> usize {
        self.table.len()
    }

    fn identity(&self, index: usize) -> Option<DeviceIdentity> {
        self.table.get(index).copied().flatten()
    }

    fn describe(&self, index: usize) -> Result<HwNode, DriverError> {
        let identity = self.identity(index).ok_or(DriverError::NotFound)?;
        let mut node = HwNode::new(0, HW_NODE_ROOT, HwDeviceClass::Other);
        // The mock names the index a node was built for in its address.
        node.set_address(u32::try_from(index).map_err(|_| DriverError::OutOfRange)?);
        node.push_match_key(HwMatchKey::usb(
            identity.vendor_id,
            identity.product_id,
            identity.interface_class,
        ))
        .map_err(|_| DriverError::DeviceFault)?;
        Ok(node)
    }

    fn engine(&mut self, index: usize) -> Engine<'_> {
        Engine(self.script(index))
    }

    fn detach_if_gone(&mut self, index: usize) -> Result<bool, DriverError> {
        if !self.script(index).gone {
            return Ok(false);
        }
        self.table[index] = None;
        self.faulted |= self.detach_faults;
        Ok(true)
    }

    fn next_hub_change(&mut self) -> Result<HubEvent, DriverError> {
        Ok(HubEvent::None)
    }

    fn faulted(&mut self) -> bool {
        self.faulted
    }

    fn reset(&mut self) -> Result<(), DriverError> {
        self.journal.borrow_mut().push(Step::Reset);
        if self.reset_fails {
            return Err(DriverError::DeviceFault);
        }
        self.faulted = false;
        self.table.clone_from(&self.reset_table);
        Ok(())
    }

    fn open_endpoint(&mut self, slot: usize) -> Option<u64> {
        if self.refuse.open {
            return None;
        }
        self.journal.borrow_mut().push(Step::Open(slot));
        Some(ENDPOINT_BASE + u64::try_from(slot).ok()?)
    }

    fn watch_endpoint(&mut self, slot: usize, _endpoint: u64) -> bool {
        if self.refuse.watches > 0 {
            self.refuse.watches -= 1;
            return false;
        }
        self.journal.borrow_mut().push(Step::Watch(slot));
        true
    }

    fn create_buffer(&mut self, len: usize) -> Option<Buffer> {
        if self.refuse.buffers {
            return None;
        }
        self.next_region += 1;
        self.journal.borrow_mut().push(Step::Map(self.next_region));
        Some(Buffer {
            region: self.next_region,
            bytes: vec![0; len],
            journal: Rc::clone(&self.journal),
        })
    }

    fn caller_pid(&mut self, _endpoint: u64, _ticket: u64) -> Result<u64, Errno> {
        Ok(CALLER_PID)
    }

    fn grant_peer(&mut self, region: u64, _endpoint: u64, ticket: u64) -> Result<u64, Errno> {
        if let Some(errno) = self.refuse.grant {
            return Err(errno);
        }
        self.journal
            .borrow_mut()
            .push(Step::Delegated { region, ticket });
        Ok(0x100 + region)
    }

    fn self_instance(&self) -> ProcId {
        INSTANCE
    }

    fn notify(&mut self, port: u64, notice: &IsoNotify) -> Result<(), Errno> {
        if self.refuse.notices.contains(&port) {
            return Err(Errno::WouldBlock);
        }
        self.journal.borrow_mut().push(Step::Notified {
            port,
            notice: *notice,
        });
        Ok(())
    }

    fn receive(
        &mut self,
        endpoint: u64,
        request: &mut [u8],
    ) -> Result<Option<(u64, usize)>, Errno> {
        let Some(at) = self.queued.iter().position(|call| call.0 == endpoint) else {
            return Ok(None);
        };
        let (_, ticket, bytes) = self.queued.remove(at);
        let n = bytes.len().min(request.len());
        request[..n].copy_from_slice(&bytes[..n]);
        Ok(Some((ticket, n)))
    }

    fn reply(&mut self, endpoint: u64, reply: UrbReply) {
        let bytes = &reply.bytes[..reply.len];
        let step = match (reply.len, IsoGrant::decode(bytes)) {
            (ISO_GRANT_REPLY_LEN, Ok(grant)) => Step::Granted {
                endpoint,
                ticket: reply.ticket,
                grant,
            },
            (STATUS_REPLY_LEN, _) => Step::Reply {
                endpoint,
                ticket: reply.ticket,
                result: decode_status_reply(bytes).map(|()| 0),
            },
            _ => Step::Reply {
                endpoint,
                ticket: reply.ticket,
                result: decode_completion(bytes),
            },
        };
        self.journal.borrow_mut().push(step);
    }

    fn emit(&mut self, node: &HwNode) -> Option<u32> {
        let grant = |kind| {
            node.resources()
                .iter()
                .find(|resource| resource.kind() == Some(kind))
                .map(HwResource::base)
        };
        let (endpoint, region) = (
            grant(HwResourceKind::Endpoint)?,
            grant(HwResourceKind::Shared)?,
        );
        if self.refuse.emits || self.retired.contains(&region) {
            return None;
        }
        self.next_node += 1;
        self.carried.push((self.next_node, region));
        self.journal.borrow_mut().push(Step::Emit {
            node: self.next_node,
            index: usize::try_from(node.address()).ok()?,
            endpoint,
            region,
        });
        Some(self.next_node)
    }

    fn remove(&mut self, id: u32) {
        self.journal.borrow_mut().push(Step::Remove(id));
        if let Some(at) = self.carried.iter().position(|carried| carried.0 == id) {
            self.retired.push(self.carried.remove(at).1);
        }
    }

    fn now_ns(&self) -> u64 {
        self.now
    }

    fn note(&mut self, note: Note) {
        self.notes.push(note);
    }
}

/// A node the steps published: `(node, index, endpoint, region)`.
type Emitted = (u32, usize, u64, u64);

fn emits(steps: &[Step]) -> Vec<Emitted> {
    steps
        .iter()
        .filter_map(|step| match *step {
            Step::Emit {
                node,
                index,
                endpoint,
                region,
            } => Some((node, index, endpoint, region)),
            _ => None,
        })
        .collect()
}

fn removes(steps: &[Step]) -> Vec<u32> {
    steps
        .iter()
        .filter_map(|step| match *step {
            Step::Remove(node) => Some(node),
            _ => None,
        })
        .collect()
}

fn position(steps: &[Step], wanted: &Step) -> usize {
    steps
        .iter()
        .position(|step| step == wanted)
        .unwrap_or_else(|| panic!("{wanted:?} not in {steps:?}"))
}

fn first_emit(steps: &[Step]) -> usize {
    steps
        .iter()
        .position(|step| matches!(step, Step::Emit { .. }))
        .unwrap_or_else(|| panic!("nothing published in {steps:?}"))
}

fn slot_of(endpoint: u64) -> usize {
    usize::try_from(endpoint - ENDPOINT_BASE).expect("a mock endpoint")
}

fn reply(endpoint: u64, ticket: u64, result: Result<u32, Errno>) -> Step {
    Step::Reply {
        endpoint,
        ticket,
        result,
    }
}

fn urb(transfer_type: UsbTransferType, endpoint: u8) -> Vec<u8> {
    request(UsbRequest::Transfer(UrbRequest {
        endpoint,
        transfer_type,
        direction: UsbDirection::In,
        length: 8,
        setup: [0x80, 0x06, 0x00, 0x01, 0x00, 0x00, 0x08, 0x00],
    }))
}

/// The frame `request` travels as.
fn request(request: UsbRequest) -> Vec<u8> {
    let mut buf = [0u8; USB_REQUEST_MAX_LEN];
    let n = request.encode(&mut buf).expect("encodes");
    buf[..n].to_vec()
}

/// A report poll: an interrupt-IN URB.
fn report_poll() -> Vec<u8> {
    urb(UsbTransferType::Interrupt, 1)
}

/// A control-IN URB, which runs synchronously.
fn control_transfer() -> Vec<u8> {
    urb(UsbTransferType::Control, 0)
}

const fn identity(route: u32, product_id: u16, interface_class: u32) -> DeviceIdentity {
    DeviceIdentity {
        root_port: 1,
        route_string: route,
        vendor_id: 0x046D,
        product_id,
        device_release: 0x0110,
        device_class: 0,
        device_subclass: 0,
        device_protocol: 0,
        interface_number: 0,
        interface_class,
        serial_number: None,
    }
}

const fn keyboard(route: u32) -> DeviceIdentity {
    identity(route, 0xC31C, 0x03_01_01)
}

const fn mouse(route: u32) -> DeviceIdentity {
    identity(route, 0xC077, 0x03_01_02)
}

const fn stick(route: u32) -> DeviceIdentity {
    identity(route, 0x5567, 0x08_06_50)
}

fn serial(text: &str) -> SerialNumber {
    let units: Vec<u16> = text.encode_utf16().collect();
    SerialNumber::new(&units).expect("a test serial fits")
}

fn serial_stick(route: u32, text: &str) -> DeviceIdentity {
    DeviceIdentity {
        serial_number: Some(serial(text)),
        ..stick(route)
    }
}

/// A table with `devices` published.
fn published(devices: Vec<Option<DeviceIdentity>>) -> (Mock, Interfaces<Buffer>, Vec<Emitted>) {
    let mut mock = Mock::new(devices);
    let mut interfaces = Interfaces::new();
    interfaces.reconcile(&mut mock);
    let emitted = emits(&mock.steps());
    (mock, interfaces, emitted)
}

#[test]
fn a_device_a_reset_moved_to_another_index_keeps_its_node_and_buffer() {
    // Index 0 is a hub's own entry, which serves no interface.
    let (mut mock, mut interfaces, emitted) = published(vec![
        None,
        Some(keyboard(2)),
        Some(mouse(3)),
        Some(serial_stick(4, "AA0001")),
    ]);
    let mut health = ControllerHealth::new(0);
    let (_, _, stick_endpoint, stick_region) = emitted[2];

    // The mouse is unplugged, leaving a hole the reset's port walk closes.
    mock.table[2] = None;
    interfaces.reconcile(&mut mock);
    mock.steps();
    mock.post(stick_endpoint, 0x51, report_poll());
    interfaces.serve_submit(slot_of(stick_endpoint), &mut health, &mut mock);
    assert!(mock.steps().is_empty(), "the report poll is held");

    mock.faulted = true;
    mock.reset_table = vec![None, Some(keyboard(2)), Some(serial_stick(4, "AA0001"))];
    assert!(interfaces.recover(&mut health, &mut mock));
    let steps = mock.steps();
    assert_eq!(
        steps,
        [
            Step::Reset,
            reply(stick_endpoint, 0x51, Err(Errno::WouldBlock))
        ],
        "the stick keeps its node, its driver, and its region"
    );
    assert!(!steps.contains(&Step::Unmap(stick_region)));

    // Its driver's next poll is served by the device at its new index.
    mock.script(2).reports = vec![vec![0xAB; 8]];
    mock.post(stick_endpoint, 0x52, report_poll());
    interfaces.serve_submit(slot_of(stick_endpoint), &mut health, &mut mock);
    assert_eq!(mock.steps(), [reply(stick_endpoint, 0x52, Ok(8))]);
    assert_eq!(mock.script(2).interrupt_calls, 1);
    assert_eq!(
        mock.script(3).interrupt_calls,
        1,
        "only the poll before the reset"
    );
}

#[test]
fn devices_a_reset_reordered_keep_their_nodes() {
    // Plugged in out of port order, then walked in port order by the reset.
    let (mut mock, mut interfaces, _) =
        published(vec![Some(serial_stick(4, "AA0001")), Some(keyboard(2))]);
    let mut health = ControllerHealth::new(0);
    mock.faulted = true;
    mock.reset_table = vec![Some(keyboard(2)), Some(serial_stick(4, "AA0001"))];
    assert!(interfaces.recover(&mut health, &mut mock));
    assert_eq!(mock.steps(), [Step::Reset]);
}

#[test]
fn a_moving_node_never_takes_an_index_another_node_serves() {
    // Two interfaces no identity fact tells apart.
    let twin = keyboard(4);
    let (mut mock, mut interfaces, emitted) = published(vec![Some(twin), Some(twin)]);
    let mut health = ControllerHealth::new(0);
    let (was_first, was_second) = (emitted[0].2, emitted[1].2);
    mock.faulted = true;
    mock.reset_table = vec![None, Some(twin), Some(twin)];
    assert!(interfaces.recover(&mut health, &mut mock));
    assert_eq!(mock.steps(), [Step::Reset]);

    mock.script(1).reports = vec![vec![1; 8]];
    mock.script(2).reports = vec![vec![2; 8]];
    mock.post(was_first, 0x61, report_poll());
    interfaces.serve_submit(slot_of(was_first), &mut health, &mut mock);
    mock.post(was_second, 0x62, report_poll());
    interfaces.serve_submit(slot_of(was_second), &mut health, &mut mock);
    assert_eq!(
        mock.steps(),
        [
            reply(was_first, 0x61, Ok(8)),
            reply(was_second, 0x62, Ok(8))
        ]
    );
    assert_eq!(mock.script(2).interrupt_calls, 1, "the node that moved");
    assert_eq!(mock.script(1).interrupt_calls, 1, "the node that stayed");
}

#[test]
fn a_device_replugged_where_one_left_is_published_on_a_region_no_node_carried() {
    let (mut mock, mut interfaces, emitted) = published(vec![Some(keyboard(2))]);
    let [(first, 0, endpoint, region)] = emitted[..] else {
        panic!("one node published: {emitted:?}");
    };
    mock.table[0] = None;
    interfaces.reconcile(&mut mock);
    assert_eq!(mock.steps(), [Step::Remove(first), Step::Unmap(region)]);

    // The same keyboard comes back at the same index.
    mock.table[0] = Some(keyboard(2));
    interfaces.reconcile(&mut mock);
    let steps = mock.steps();
    let emitted = emits(&steps);
    let [(second, 0, again, fresh)] = emitted[..] else {
        panic!("the re-plug is published: {steps:?}");
    };
    assert_ne!(second, first);
    assert_eq!(again, endpoint, "the endpoint is reused");
    assert_ne!(fresh, region, "the region is not: it retired with its node");
}

#[test]
fn a_node_is_published_only_once_its_endpoint_is_drained() {
    let (mut mock, mut interfaces, emitted) = published(vec![Some(keyboard(2))]);
    let endpoint = emitted[0].2;
    mock.table[0] = None;
    interfaces.reconcile(&mut mock);
    // Posted by the retracted node's driver before its grant was revoked.
    mock.post(endpoint, 0x77, report_poll());
    mock.table[0] = Some(stick(3));
    interfaces.reconcile(&mut mock);
    let steps = mock.steps();
    let refused = position(&steps, &reply(endpoint, 0x77, Err(Errno::NotFound)));
    assert!(refused < first_emit(&steps), "{steps:?}");
}

#[test]
fn the_drain_before_a_publish_is_bounded_by_the_endpoint_depth() {
    let (mut mock, mut interfaces, emitted) = published(vec![Some(keyboard(2))]);
    let endpoint = emitted[0].2;
    mock.table[0] = None;
    interfaces.reconcile(&mut mock);
    mock.steps();
    let tickets = 0..=u64::try_from(ENDPOINT_CAPACITY).expect("a small depth");
    for ticket in tickets {
        mock.post(endpoint, ticket, report_poll());
    }
    mock.table[0] = Some(keyboard(2));
    interfaces.reconcile(&mut mock);
    let refusals = mock
        .steps()
        .iter()
        .filter(|step| matches!(step, Step::Reply { .. }))
        .count();
    assert_eq!(refusals, ENDPOINT_CAPACITY);
    assert_eq!(mock.queued.len(), 1, "a call past the depth is left alone");
}

#[test]
fn a_changed_device_is_retracted_before_its_replacement_is_published() {
    let (mut mock, mut interfaces, emitted) = published(vec![Some(keyboard(2))]);
    mock.table[0] = Some(stick(2));
    interfaces.reconcile(&mut mock);
    let steps = mock.steps();
    assert!(position(&steps, &Step::Remove(emitted[0].0)) < first_emit(&steps));
}

#[test]
fn every_node_is_decided_even_when_nothing_new_can_be_published() {
    // Memory exhausted: no transport, buffer, or node can be had. A node
    // whose index now serves another device is still retracted rather than
    // left bound to it.
    let (mut mock, mut interfaces, emitted) = published(vec![Some(keyboard(2)), Some(stick(3))]);
    mock.refuse.open = true;
    mock.refuse.buffers = true;
    mock.table = vec![Some(mouse(2)), None, Some(stick(5)), Some(keyboard(6))];
    interfaces.reconcile(&mut mock);
    let steps = mock.steps();
    assert_eq!(removes(&steps), [emitted[0].0, emitted[1].0]);
    assert!(emits(&steps).is_empty());
    assert!(!steps.iter().any(|step| matches!(step, Step::Open(_))));

    mock.refuse.open = false;
    mock.refuse.buffers = false;
    interfaces.reconcile(&mut mock);
    let indices: Vec<usize> = emits(&mock.steps()).iter().map(|e| e.1).collect();
    assert_eq!(indices, [0, 2, 3], "published once there is memory again");
}

#[test]
fn a_node_the_kernel_refuses_releases_its_region() {
    let mut mock = Mock::new(vec![Some(keyboard(2))]);
    let mut interfaces = Interfaces::new();
    mock.refuse.emits = true;
    interfaces.reconcile(&mut mock);
    assert_eq!(
        mock.steps(),
        [Step::Open(0), Step::Watch(0), Step::Map(1), Step::Unmap(1)]
    );
    mock.refuse.emits = false;
    interfaces.reconcile(&mut mock);
    assert_eq!(
        mock.steps(),
        [
            Step::Map(2),
            Step::Emit {
                node: 1,
                index: 0,
                endpoint: ENDPOINT_BASE,
                region: 2
            }
        ]
    );
}

#[test]
fn an_endpoint_the_event_loop_could_not_watch_is_watched_again_never_bound_again() {
    // A bound endpoint stays bound, so binding it again would fail forever.
    let mut mock = Mock::new(vec![Some(keyboard(2))]);
    let mut interfaces = Interfaces::new();
    mock.refuse.watches = 1;
    interfaces.reconcile(&mut mock);
    assert_eq!(mock.steps(), [Step::Open(0)]);
    interfaces.reconcile(&mut mock);
    assert_eq!(
        mock.steps(),
        [
            Step::Watch(0),
            Step::Map(1),
            Step::Emit {
                node: 1,
                index: 0,
                endpoint: ENDPOINT_BASE,
                region: 1
            }
        ]
    );
}

#[test]
fn a_device_that_came_back_unchanged_keeps_its_node() {
    let (mut mock, mut interfaces, _) = published(vec![
        None,
        Some(serial_stick(2, "AA0001")),
        Some(keyboard(4)),
    ]);
    let mut health = ControllerHealth::new(0);
    mock.faulted = true;
    assert!(interfaces.recover(&mut health, &mut mock));
    assert_eq!(mock.steps(), [Step::Reset]);
    assert_eq!(
        mock.domain_notes(),
        [
            ControllerDomainEvent::Recovering,
            ControllerDomainEvent::Recovered
        ]
    );
}

#[test]
fn a_change_in_any_identity_fact_replaces_the_node() {
    let original = keyboard(4);
    let changed = [
        DeviceIdentity {
            root_port: 2,
            ..original
        },
        DeviceIdentity {
            route_string: 3,
            ..original
        },
        DeviceIdentity {
            vendor_id: 0x1234,
            ..original
        },
        DeviceIdentity {
            product_id: 0x0001,
            ..original
        },
        DeviceIdentity {
            device_release: 0x0200,
            ..original
        },
        DeviceIdentity {
            device_class: 0xEF,
            ..original
        },
        DeviceIdentity {
            device_subclass: 0x02,
            ..original
        },
        DeviceIdentity {
            device_protocol: 0x01,
            ..original
        },
        DeviceIdentity {
            interface_number: 1,
            ..original
        },
        DeviceIdentity {
            interface_class: 0x03_01_02,
            ..original
        },
        DeviceIdentity {
            serial_number: Some(serial("7A31")),
            ..original
        },
    ];
    for served in changed {
        let (mut mock, mut interfaces, emitted) = published(vec![Some(original)]);
        mock.table[0] = Some(served);
        interfaces.reconcile(&mut mock);
        let steps = mock.steps();
        assert_eq!(
            removes(&steps),
            [emitted[0].0],
            "{served:?} is not the device the node was built from"
        );
        assert_eq!(emits(&steps).len(), 1);
    }
}

#[test]
fn two_devices_of_one_model_that_traded_places_are_told_apart_by_serial_number() {
    // By model and position alone each index looks unchanged across the
    // reset, and would keep a node — a mounted filesystem's driver — bound to
    // the other stick.
    let (mut mock, mut interfaces, emitted) = published(vec![
        Some(serial_stick(2, "AA0001")),
        Some(serial_stick(4, "AA0002")),
    ]);
    let mut health = ControllerHealth::new(0);
    mock.faulted = true;
    mock.reset_table = vec![
        Some(serial_stick(2, "AA0002")),
        Some(serial_stick(4, "AA0001")),
    ];
    assert!(interfaces.recover(&mut health, &mut mock));
    let steps = mock.steps();
    assert_eq!(removes(&steps), [emitted[0].0, emitted[1].0]);
    assert_eq!(emits(&steps).len(), 2);
}

#[test]
fn a_storage_device_without_a_serial_number_is_replaced_across_a_reset() {
    // Model and position alone cannot tell it from another stick swapped into
    // its port, and binding its driver to another medium corrupts it.
    let (mut mock, mut interfaces, emitted) = published(vec![Some(stick(2)), Some(keyboard(4))]);
    let mut health = ControllerHealth::new(0);
    mock.faulted = true;
    assert!(interfaces.recover(&mut health, &mut mock));
    let steps = mock.steps();
    assert_eq!(removes(&steps), [emitted[0].0], "only the stick");
    assert_eq!(emits(&steps).len(), 1, "republished on a fresh node");
}

#[test]
fn a_storage_device_without_a_serial_number_keeps_its_node_across_a_hot_plug() {
    let (mut mock, mut interfaces, _) = published(vec![Some(stick(2))]);
    mock.table.push(Some(keyboard(4)));
    interfaces.reconcile(&mut mock);
    let steps = mock.steps();
    assert!(
        removes(&steps).is_empty(),
        "within one enumeration it is the same device"
    );
    assert_eq!(emits(&steps).len(), 1, "only the new keyboard");
}

#[test]
fn a_device_back_with_the_same_serial_number_keeps_its_node() {
    let (mut mock, mut interfaces, _) = published(vec![Some(serial_stick(2, "AA0001"))]);
    let mut health = ControllerHealth::new(0);
    mock.faulted = true;
    assert!(interfaces.recover(&mut health, &mut mock));
    assert_eq!(mock.steps(), [Step::Reset]);
}

#[test]
fn a_serial_number_that_no_longer_reads_replaces_the_node() {
    // Unconfirmed is not the same: a driver reload costs less than a driver
    // bound to another device.
    let (mut mock, mut interfaces, emitted) = published(vec![Some(serial_stick(2, "AA0001"))]);
    mock.table[0] = Some(stick(2));
    interfaces.reconcile(&mut mock);
    assert_eq!(removes(&mock.steps()), [emitted[0].0]);
}

#[test]
fn a_device_no_longer_served_has_its_node_retracted_and_its_poll_answered() {
    let (mut mock, mut interfaces, emitted) = published(vec![Some(keyboard(2))]);
    let mut health = ControllerHealth::new(0);
    let (node, _, endpoint, region) = emitted[0];
    mock.post(endpoint, 0x31, report_poll());
    interfaces.serve_submit(slot_of(endpoint), &mut health, &mut mock);
    mock.table[0] = None;
    interfaces.reconcile(&mut mock);
    assert_eq!(
        mock.steps(),
        [
            Step::Remove(node),
            Step::Unmap(region),
            reply(endpoint, 0x31, Err(Errno::NotFound))
        ]
    );
}

#[test]
fn a_newly_served_device_is_published() {
    let (mut mock, mut interfaces, emitted) = published(vec![None]);
    assert!(emitted.is_empty());
    mock.table = vec![None, Some(keyboard(4))];
    interfaces.reconcile(&mut mock);
    assert_eq!(
        mock.steps(),
        [
            Step::Open(0),
            Step::Watch(0),
            Step::Map(1),
            Step::Emit {
                node: 1,
                index: 1,
                endpoint: ENDPOINT_BASE,
                region: 1
            }
        ]
    );
}

#[test]
fn a_shrunken_table_retracts_every_node_past_its_end() {
    // A reset rebuilds the device table from empty, so it can come back
    // shorter than the one the nodes were published from.
    let (mut mock, mut interfaces, emitted) = published(vec![
        None,
        Some(serial_stick(2, "AA0001")),
        Some(keyboard(4)),
    ]);
    let mut health = ControllerHealth::new(0);
    mock.faulted = true;
    mock.reset_table = vec![None, Some(serial_stick(2, "AA0001"))];
    assert!(interfaces.recover(&mut health, &mut mock));
    assert_eq!(removes(&mock.steps()), [emitted[1].0]);
}

#[test]
fn a_composite_devices_interfaces_are_decided_one_by_one() {
    // A keyboard+mouse receiver serves two interfaces on one slot; the mouse
    // interface re-enumerated under another class.
    let keyboard_interface = keyboard(4);
    let mouse_interface = DeviceIdentity {
        interface_number: 1,
        interface_class: 0x03_01_02,
        ..keyboard_interface
    };
    let (mut mock, mut interfaces, emitted) =
        published(vec![Some(keyboard_interface), Some(mouse_interface)]);
    mock.table[1] = Some(DeviceIdentity {
        interface_class: 0x03_00_00,
        ..mouse_interface
    });
    interfaces.reconcile(&mut mock);
    let steps = mock.steps();
    assert_eq!(removes(&steps), [emitted[1].0]);
    assert_eq!(emits(&steps).len(), 1);
}

#[test]
fn a_device_leaving_on_the_submit_path_recovers_the_controller_it_halted() {
    let (mut mock, mut interfaces, emitted) = published(vec![Some(keyboard(2)), Some(stick(3))]);
    let mut health = ControllerHealth::new(0);
    let (keyboard_node, _, keyboard_endpoint, _) = emitted[0];
    let stick_endpoint = emitted[1].2;
    mock.post(keyboard_endpoint, 0x11, report_poll());
    interfaces.serve_submit(slot_of(keyboard_endpoint), &mut health, &mut mock);

    // Unplugged: its next completion faults, and tearing its slot down
    // halts the controller.
    mock.script(0).fault = Some(DriverError::DeviceFault);
    mock.script(0).gone = true;
    mock.detach_faults = true;
    mock.reset_table = vec![None, Some(stick(3))];
    // The stick's control transfer runs synchronously, so the held poll is
    // driven after it.
    mock.post(stick_endpoint, 0x22, control_transfer());
    interfaces.serve_submit(slot_of(stick_endpoint), &mut health, &mut mock);
    let steps = mock.steps();
    let retracted = position(&steps, &Step::Remove(keyboard_node));
    let answered = position(
        &steps,
        &reply(keyboard_endpoint, 0x11, Err(Errno::NotFound)),
    );
    let reset = position(&steps, &Step::Reset);
    assert!(retracted < answered && answered < reset, "{steps:?}");
    assert!(mock.notes.contains(&Note::FaultDetached));
    assert_eq!(
        mock.domain_notes(),
        [
            ControllerDomainEvent::Recovering,
            ControllerDomainEvent::Recovered
        ]
    );
}

#[test]
fn a_transfer_fault_from_a_device_still_attached_reaches_its_driver() {
    let (mut mock, mut interfaces, emitted) = published(vec![Some(keyboard(2))]);
    let mut health = ControllerHealth::new(0);
    let endpoint = emitted[0].2;
    mock.post(endpoint, 0x12, report_poll());
    interfaces.serve_submit(slot_of(endpoint), &mut health, &mut mock);
    mock.script(0).fault = Some(DriverError::DeviceFault);
    interfaces.drive_busy(&mut health, &mut mock);
    assert_eq!(
        mock.steps(),
        [reply(endpoint, 0x12, Err(Errno::DeviceFault))]
    );
    assert!(mock.notes.contains(&Note::UrbFailed {
        index: 0,
        errno: Errno::DeviceFault
    }));
}

#[test]
fn stopping_retracts_every_node_and_answers_its_held_urb() {
    let (mut mock, mut interfaces, emitted) = published(vec![Some(keyboard(2)), Some(stick(3))]);
    let mut health = ControllerHealth::new(0);
    let endpoint = emitted[0].2;
    mock.post(endpoint, 0x42, report_poll());
    interfaces.serve_submit(slot_of(endpoint), &mut health, &mut mock);

    interfaces.retract_all(&mut mock);
    let steps = mock.steps();
    assert_eq!(removes(&steps), [emitted[0].0, emitted[1].0]);
    assert!(steps.contains(&reply(endpoint, 0x42, Err(Errno::NotFound))));
}

#[test]
fn a_controller_that_misses_its_grace_window_retracts_every_node_and_is_never_reset_again() {
    let (mut mock, mut interfaces, emitted) = published(vec![Some(keyboard(2)), Some(stick(3))]);
    let mut health = ControllerHealth::new(0);
    let endpoint = emitted[0].2;
    mock.post(endpoint, 0x41, report_poll());
    interfaces.serve_submit(slot_of(endpoint), &mut health, &mut mock);

    mock.faulted = true;
    mock.reset_fails = true;
    assert!(interfaces.recover(&mut health, &mut mock));
    assert_eq!(
        mock.steps(),
        [Step::Reset],
        "the nodes and the held poll wait for the next attempt"
    );
    assert!(health.is_recovering());

    mock.now = CONTROLLER_GRACE_NS + 1;
    assert!(interfaces.recover(&mut health, &mut mock));
    let steps = mock.steps();
    assert_eq!(removes(&steps), [emitted[0].0, emitted[1].0]);
    assert!(steps.contains(&reply(endpoint, 0x41, Err(Errno::NotFound))));
    assert!(health.is_failed_closed());
    assert_eq!(
        mock.domain_notes(),
        [
            ControllerDomainEvent::Recovering,
            ControllerDomainEvent::FailedClosed
        ]
    );

    assert!(!interfaces.recover(&mut health, &mut mock));
    assert!(
        mock.steps().is_empty(),
        "a failed-closed controller is left alone"
    );
}

#[test]
fn a_submit_on_a_transport_without_a_node_is_refused_without_reaching_a_device() {
    let (mut mock, mut interfaces, emitted) = published(vec![Some(keyboard(2))]);
    let mut health = ControllerHealth::new(0);
    let endpoint = emitted[0].2;
    mock.table[0] = None;
    interfaces.reconcile(&mut mock);
    mock.steps();
    mock.post(endpoint, 0x81, report_poll());
    interfaces.serve_submit(slot_of(endpoint), &mut health, &mut mock);
    assert_eq!(mock.steps(), [reply(endpoint, 0x81, Err(Errno::NotFound))]);
    assert_eq!(mock.script(0).interrupt_calls, 0);
}

#[test]
fn a_report_poll_while_the_controller_recovers_waits_without_reaching_it() {
    let (mut mock, mut interfaces, emitted) = published(vec![Some(keyboard(2))]);
    let mut health = ControllerHealth::new(0);
    let endpoint = emitted[0].2;
    mock.faulted = true;
    mock.reset_fails = true;
    assert!(interfaces.recover(&mut health, &mut mock));
    mock.steps();
    mock.post(endpoint, 0x91, report_poll());
    interfaces.serve_submit(slot_of(endpoint), &mut health, &mut mock);
    assert!(mock.steps().is_empty(), "held for the next attempt");
    assert_eq!(mock.script(0).interrupt_calls, 0);
}

/// Setting 1's OUT data endpoint, and a layout for it.
const DATA: u8 = 0x01;

fn layout() -> IsoLayout {
    IsoLayout::new(4, 8, 192).expect("a valid layout")
}

/// A table publishing one audio function, with a stream started on `DATA`
/// by `ticket` 0x70: the mock, the table, the node's endpoint and the
/// stream's notify port.
fn streaming() -> (Mock, Interfaces<Buffer>, ControllerHealth, u64, u64) {
    let (mut mock, mut interfaces, emitted) =
        published(vec![Some(identity(0, 0x0A51, 0x01_01_00))]);
    let mut health = ControllerHealth::new(0);
    let (_, _, endpoint, _) = emitted[0];
    mock.post(
        endpoint,
        0x70,
        request(UsbRequest::IsoStart(IsoStartParams {
            endpoint: DATA,
            layout: layout(),
        })),
    );
    interfaces.serve_submit(slot_of(endpoint), &mut health, &mut mock);
    mock.steps();
    (
        mock,
        interfaces,
        health,
        endpoint,
        iso_notify_endpoint_for(CALLER_PID, DATA),
    )
}

#[test]
fn a_malformed_frame_is_answered_fail_closed_and_reaches_nothing() {
    let (mut mock, mut interfaces, emitted) = published(vec![Some(keyboard(1))]);
    let mut health = ControllerHealth::new(0);
    let (_, _, endpoint, _) = emitted[0];
    let poll = report_poll();
    let truncated = poll[..poll.len() - 1].to_vec();
    mock.post(endpoint, 0x61, truncated.clone());
    interfaces.serve_submit(slot_of(endpoint), &mut health, &mut mock);
    assert_eq!(
        mock.steps(),
        [reply(endpoint, 0x61, Err(Errno::LengthOutOfRange))]
    );
    mock.faulted = true;
    mock.reset_fails = true;
    assert!(interfaces.recover(&mut health, &mut mock));
    mock.steps();
    mock.post(endpoint, 0x62, truncated);
    interfaces.serve_submit(slot_of(endpoint), &mut health, &mut mock);
    assert_eq!(
        mock.steps(),
        [reply(endpoint, 0x62, Err(Errno::LengthOutOfRange))]
    );
    assert_eq!(mock.script(0).interrupt_calls, 0);
}

#[test]
fn interface_operations_are_answered_with_their_outcome() {
    let (mut mock, mut interfaces, emitted) =
        published(vec![Some(identity(0, 0x0A51, 0x01_01_00))]);
    let mut health = ControllerHealth::new(0);
    let (_, _, endpoint, _) = emitted[0];
    mock.post(
        endpoint,
        1,
        request(UsbRequest::ClaimInterface { interface: 1 }),
    );
    interfaces.serve_submit(slot_of(endpoint), &mut health, &mut mock);
    mock.script(0).refusal = Some(DriverError::NoBandwidth);
    mock.post(
        endpoint,
        2,
        request(UsbRequest::SetInterface {
            interface: 1,
            alternate: 1,
        }),
    );
    interfaces.serve_submit(slot_of(endpoint), &mut health, &mut mock);
    assert_eq!(
        mock.steps(),
        [
            reply(endpoint, 1, Ok(0)),
            reply(endpoint, 2, Err(Errno::NoBandwidth))
        ]
    );
    assert_eq!(
        mock.script(0).operations,
        [
            UsbRequest::ClaimInterface { interface: 1 },
            UsbRequest::SetInterface {
                interface: 1,
                alternate: 1
            }
        ]
    );
}

#[test]
fn a_started_stream_is_granted_a_region_of_its_own_and_its_callers_port() {
    let (mut mock, mut interfaces, emitted) =
        published(vec![Some(identity(0, 0x0A51, 0x01_01_00))]);
    let mut health = ControllerHealth::new(0);
    let (_, _, endpoint, node_region) = emitted[0];
    let params = IsoStartParams {
        endpoint: DATA,
        layout: layout(),
    };
    mock.post(endpoint, 0x70, request(UsbRequest::IsoStart(params)));
    interfaces.serve_submit(slot_of(endpoint), &mut health, &mut mock);
    let steps = mock.steps();
    let region = node_region + 1;
    assert_eq!(
        steps,
        [
            Step::Map(region),
            Step::Delegated {
                region,
                ticket: 0x70
            },
            Step::Granted {
                endpoint,
                ticket: 0x70,
                grant: IsoGrant {
                    region_grant: 0x100 + region,
                    grantor: INSTANCE,
                    notify: iso_notify_endpoint_for(CALLER_PID, DATA),
                    interval_microframes: 8,
                    speed: UsbSpeed::Full,
                    stream: NonZeroU32::MIN,
                },
            },
        ]
    );
    // Queuing reads the stream's own region, never the node's URB buffer.
    mock.post(
        endpoint,
        0x71,
        request(UsbRequest::IsoQueue {
            endpoint: DATA,
            slot: 2,
        }),
    );
    interfaces.serve_submit(slot_of(endpoint), &mut health, &mut mock);
    assert_eq!(mock.steps(), [reply(endpoint, 0x71, Ok(0))]);
    assert_eq!(mock.script(0).queued_regions, [layout().region_len()]);
}

#[test]
fn a_stream_whose_region_cannot_be_delegated_is_stopped_again() {
    let (mut mock, mut interfaces, emitted) =
        published(vec![Some(identity(0, 0x0A51, 0x01_01_00))]);
    let mut health = ControllerHealth::new(0);
    let (_, _, endpoint, _) = emitted[0];
    mock.refuse.grant = Some(Errno::NotFound);
    mock.post(
        endpoint,
        0x70,
        request(UsbRequest::IsoStart(IsoStartParams {
            endpoint: DATA,
            layout: layout(),
        })),
    );
    interfaces.serve_submit(slot_of(endpoint), &mut health, &mut mock);
    let steps = mock.steps();
    assert!(steps.contains(&reply(endpoint, 0x70, Err(Errno::NotFound))));
    assert!(
        steps.contains(&Step::Unmap(emitted[0].3 + 1)),
        "the region went"
    );
    assert_eq!(
        mock.script(0).operations.last(),
        Some(&UsbRequest::IsoStop { endpoint: DATA })
    );
}

#[test]
fn finished_slots_are_notified_in_order() {
    let (mut mock, mut interfaces, _health, _endpoint, port) = streaming();
    let done = |slot| IsoSlotDone {
        slot,
        skipped: u32::from(slot),
        microframe: 1000 + u64::from(slot) * 64,
    };
    mock.script(0).takes = VecDeque::from([Ok(Some(done(0))), Ok(Some(done(1)))]);
    mock.now = 55;
    interfaces.deliver_streams(&mut mock);
    let notices: Vec<_> = mock
        .steps()
        .into_iter()
        .map(|step| match step {
            Step::Notified { port: to, notice } => {
                assert_eq!(to, port);
                notice
            }
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    assert_eq!(
        notices,
        [0, 1].map(|slot| IsoNotify::SlotDone {
            endpoint: DATA,
            stream: NonZeroU32::MIN,
            slot,
            skipped: u32::from(slot),
            microframe: 1000 + u64::from(slot) * 64,
            completed_at: 55,
        })
    );
}

#[test]
fn a_stream_started_again_on_an_endpoint_is_numbered_afresh() {
    let (mut mock, mut interfaces, mut health, endpoint, port) = streaming();
    mock.post(
        endpoint,
        0x71,
        request(UsbRequest::IsoStop { endpoint: DATA }),
    );
    interfaces.serve_submit(slot_of(endpoint), &mut health, &mut mock);
    mock.steps();
    mock.post(
        endpoint,
        0x72,
        request(UsbRequest::IsoStart(IsoStartParams {
            endpoint: DATA,
            layout: layout(),
        })),
    );
    interfaces.serve_submit(slot_of(endpoint), &mut health, &mut mock);
    let granted = mock.steps().into_iter().find_map(|step| match step {
        Step::Granted { grant, .. } => Some(grant.stream),
        _ => None,
    });
    let second = NonZeroU32::MIN.saturating_add(1);
    assert_eq!(granted, Some(second));
    mock.script(0).takes = VecDeque::from([Ok(Some(IsoSlotDone {
        slot: 0,
        skipped: 0,
        microframe: 0,
    }))]);
    interfaces.deliver_streams(&mut mock);
    assert_eq!(
        mock.steps(),
        [Step::Notified {
            port,
            notice: IsoNotify::SlotDone {
                endpoint: DATA,
                stream: second,
                slot: 0,
                skipped: 0,
                microframe: 0,
                completed_at: mock.now,
            },
        }]
    );
}

#[test]
fn a_stream_that_cannot_be_told_or_that_halts_ends() {
    let (mut mock, mut interfaces, _health, _endpoint, port) = streaming();
    mock.refuse.notices.push(port);
    mock.script(0).takes = VecDeque::from([Ok(Some(IsoSlotDone {
        slot: 0,
        skipped: 0,
        microframe: 0,
    }))]);
    interfaces.deliver_streams(&mut mock);
    assert_eq!(
        mock.script(0).operations.last(),
        Some(&UsbRequest::IsoStop { endpoint: DATA })
    );
    assert!(mock.notes.contains(&Note::StreamEnded {
        index: 0,
        endpoint: DATA,
        reason: Errno::WouldBlock
    }));
    let steps = mock.steps();
    assert_eq!(steps.len(), 1, "only the region's release: {steps:?}");
    assert!(matches!(steps[0], Step::Unmap(_)));

    let (mut mock, mut interfaces, _health, _endpoint, port) = streaming();
    mock.script(0).takes = VecDeque::from([Err(DriverError::DeviceFault)]);
    interfaces.deliver_streams(&mut mock);
    let steps = mock.steps();
    assert_eq!(
        steps[0],
        Step::Notified {
            port,
            notice: IsoNotify::Halted {
                endpoint: DATA,
                stream: NonZeroU32::MIN,
                reason: Errno::DeviceFault
            }
        }
    );
    // A stream that ended answers no more queues.
    mock.script(0).takes.clear();
    interfaces.deliver_streams(&mut mock);
    assert!(mock.steps().is_empty());
}

#[test]
fn a_departing_device_ends_its_streams_before_its_node_goes() {
    let (mut mock, mut interfaces, _health, _endpoint, port) = streaming();
    mock.table[0] = None;
    interfaces.reconcile(&mut mock);
    let steps = mock.steps();
    let halted = position(
        &steps,
        &Step::Notified {
            port,
            notice: IsoNotify::Halted {
                endpoint: DATA,
                stream: NonZeroU32::MIN,
                reason: Errno::NotFound,
            },
        },
    );
    assert!(halted < position(&steps, &Step::Remove(1)));
}

#[test]
fn a_controller_reset_ends_every_stream_reissuably() {
    let (mut mock, mut interfaces, mut health, endpoint, port) = streaming();
    mock.faulted = true;
    assert!(interfaces.recover(&mut health, &mut mock));
    let steps = mock.steps();
    assert!(steps.contains(&Step::Notified {
        port,
        notice: IsoNotify::Halted {
            endpoint: DATA,
            stream: NonZeroU32::MIN,
            reason: Errno::WouldBlock
        }
    }));
    mock.post(
        endpoint,
        0x72,
        request(UsbRequest::IsoQueue {
            endpoint: DATA,
            slot: 0,
        }),
    );
    interfaces.serve_submit(slot_of(endpoint), &mut health, &mut mock);
    assert_eq!(
        mock.steps(),
        [reply(endpoint, 0x72, Err(Errno::NotFound))],
        "the stream went with the reset"
    );
}

#[test]
fn stream_operations_wait_out_a_recovery() {
    let (mut mock, mut interfaces, mut health, endpoint, _port) = streaming();
    mock.faulted = true;
    mock.reset_fails = true;
    assert!(interfaces.recover(&mut health, &mut mock));
    mock.steps();
    mock.post(
        endpoint,
        0x73,
        request(UsbRequest::IsoQueue {
            endpoint: DATA,
            slot: 0,
        }),
    );
    interfaces.serve_submit(slot_of(endpoint), &mut health, &mut mock);
    assert_eq!(
        mock.steps(),
        [reply(endpoint, 0x73, Err(Errno::WouldBlock))]
    );
}
