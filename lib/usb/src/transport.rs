//! The bus-agnostic URB transport seam (`plans/USB.md` §1.3, U2).
//!
//! The host-controller driver (HCD) owns one controller and serves a URB
//! transport call endpoint per USB interface it emits; a class driver binds
//! that interface node and submits URBs over the endpoint. This module is the
//! protocol layer both sides share:
//!
//! * [`UrbEngine`] is the controller-side seam — the operations the HCD's
//!   real engine ([`crate::device::UsbDevice`]) performs to satisfy a URB. A
//!   host test drives it with a mock engine.
//! * [`drive_urb`] is the controller-side server transformation: decode a URB
//!   frame, validate it fail-closed against the interface, and drive the
//!   engine. It is **asynchronous**: an interrupt-IN report that has not
//!   arrived yet returns `Ok(None)`, so the HCD holds the caller's URB call
//!   outstanding and re-drives it on its next controller interrupt rather than
//!   busy-polling or blocking one interface inside another's handler
//!   (`plans/USB.md` §1.1, the async event loop). [`frame_completion`] frames
//!   the outcome into the in-band completion the HCD replies with.
//! * [`UrbClient`] is the class-side client over a [`UrbCall`] transport
//!   (the IPC call the class driver issues): it builds the URB, submits it,
//!   and decodes the completion. The call blocks in the kernel until the HCD
//!   replies (when the report arrives), so the class driver parks rather than
//!   spinning.
//!
//! Only the URB descriptor and the completion cross the endpoint; the
//! transfer's *data* lives in the separately-mapped shared-memory buffer the
//! URB names (the `data` slice the server is handed, and the buffer the
//! client reads back). No class driver ever sees a controller register or
//! another interface's buffer.

use alloc::vec::Vec;

use tairix_abi::reply::decode_status_reply;
use tairix_abi::usb_urb::{
    decode_completion, encode_completion, encode_error_completion, IsoGrant, IsoLayout,
    IsoStartParams, UrbRequest, UsbDirection, UsbRequest, UsbSpeed, UsbTransferType,
    USB_REPLY_MAX_LEN, USB_REQUEST_MAX_LEN,
};
use tairix_abi::{DriverError, Errno};
use tairix_inline::BitSet256;

use crate::descriptor::{ConfigurationHeader, Malformed, CONFIGURATION_HEADER_LEN};
use crate::device::{setup_get_configuration_descriptor, BULK_BUF_LEN, CTRL_DATA_LEN};

/// The controller-side operations the URB transport server drives.
///
/// The HCD's live engine implements this; a malformed transfer never reaches
/// it because [`drive_urb`] validates the URB first. The transfers the
/// served device classes need are present: a control-IN transfer (used
/// during enumeration and for class-IN requests), a **no-data** control-OUT
/// (a class request carrying its whole meaning in SETUP — the BOT Mass
/// Storage Reset, `plans/DEVICES.md` D2), a control-OUT **data stage** (a
/// class request carrying a payload — the CBI ADSC command channel,
/// `plans/DEVICES.md` D5), a non-blocking interrupt-IN poll (the HID report
/// and CBI completion paths), and non-blocking bulk IN/OUT (the
/// mass-storage data path, `plans/DEVICES.md` D1).
pub trait UrbEngine {
    /// Run a control-IN transfer (SETUP + IN data stage) into `data`,
    /// returning the bytes the device delivered.
    ///
    /// # Errors
    ///
    /// A [`DriverError`] from the controller/device (e.g.
    /// [`DriverError::DeviceFault`]).
    fn control_in(&mut self, setup: [u8; 8], data: &mut [u8]) -> Result<usize, DriverError>;

    /// Run a no-data control transfer (SETUP + status stage only, USB 2.0
    /// §9.3 `wLength == 0`): a class request whose whole meaning rides in
    /// `setup`, e.g. the BOT Bulk-Only Mass Storage Reset.
    ///
    /// # Errors
    ///
    /// A [`DriverError`] from the controller/device (e.g.
    /// [`DriverError::DeviceFault`]).
    fn control_no_data(&mut self, setup: [u8; 8]) -> Result<(), DriverError>;

    /// Run a control-OUT transfer (SETUP + OUT data stage carrying `data` +
    /// status stage): a class request with a payload, e.g. the CBI ADSC
    /// command block.
    ///
    /// # Errors
    ///
    /// * [`DriverError::EndpointStalled`] — the device refused the request
    ///   with a protocol STALL (the control endpoint recovers on the next
    ///   SETUP, USB 2.0 §8.5.3.4).
    /// * Any other [`DriverError`] from the controller/device.
    fn control_out(&mut self, setup: [u8; 8], data: &[u8]) -> Result<(), DriverError>;

    /// What the interface's control requests may reach; `None` once the
    /// interface is gone, which reaches nothing.
    fn scope(&self) -> Option<UrbScope>;

    /// Poll the interface's interrupt-IN endpoint, device endpoint number
    /// `endpoint`, for one pending report into `data`, the whole shared
    /// buffer. `request` is the longest report the class driver expects: the
    /// endpoint is armed to it, or to one service interval's payload if that
    /// is longer, so a report may exceed it. `Ok(Some(n))` if a report of `n`
    /// bytes arrived, `Ok(None)` if none is pending yet (the caller retries).
    ///
    /// # Errors
    ///
    /// [`DriverError::OutOfRange`] when `endpoint` is not the interface's
    /// interrupt-IN endpoint; a [`DriverError`] from the controller/device;
    /// or one for a `request` of zero, past `data`, or other than the one the
    /// endpoint was first armed to.
    fn interrupt_in(
        &mut self,
        endpoint: u8,
        request: usize,
        data: &mut [u8],
    ) -> Result<Option<usize>, DriverError>;

    /// Drive one bulk-IN transfer on device endpoint number `endpoint`
    /// reading into `data`: arm it if not yet armed, then reap its
    /// completion. `Ok(Some(n))` when the transfer finished (`n` bytes
    /// landed in `data`; a short packet yields `n < data.len()`),
    /// `Ok(None)` while it is still in flight (the caller re-drives on the
    /// next controller event).
    ///
    /// # Errors
    ///
    /// * [`DriverError::EndpointStalled`] — the device answered the
    ///   transfer with STALL; the engine has already recovered the
    ///   endpoint, so the caller may submit fresh transfers immediately.
    /// * [`DriverError::OutOfRange`] — `endpoint` is not the interface's
    ///   configured bulk-IN endpoint.
    /// * Any other [`DriverError`] for a hard controller/device fault.
    fn bulk_in(&mut self, endpoint: u8, data: &mut [u8]) -> Result<Option<usize>, DriverError>;

    /// Drive one bulk-OUT transfer on device endpoint number `endpoint`
    /// writing `data`: arm it if not yet armed, then reap its completion.
    /// `Ok(Some(n))` when the transfer finished (`n` bytes accepted by the
    /// device), `Ok(None)` while it is still in flight.
    ///
    /// # Errors
    ///
    /// As [`Self::bulk_in`].
    fn bulk_out(&mut self, endpoint: u8, data: &[u8]) -> Result<Option<usize>, DriverError>;

    /// Select `alternate` on `interface` — the node's own or one it claimed —
    /// reprogramming the controller for the endpoints the setting brings and
    /// reserving their bandwidth before the device is told.
    ///
    /// # Errors
    ///
    /// [`DriverError::NoBandwidth`] when the bus cannot schedule the setting,
    /// [`DriverError::NotFound`] for a setting the device lacks or an interface
    /// the node does not govern, [`DriverError::Busy`] while a stream runs on
    /// the interface, or the device's refusal. An engine serving no periodic
    /// endpoints refuses with [`DriverError::Unsupported`].
    fn set_interface(&mut self, _interface: u8, _alternate: u8) -> Result<(), DriverError> {
        Err(DriverError::Unsupported)
    }

    /// Govern `interface` of the node's device, which no node of its own
    /// serves.
    ///
    /// # Errors
    ///
    /// [`DriverError::NotFound`] for an interface the device lacks,
    /// [`DriverError::AlreadyExists`] for one another node serves or claimed,
    /// or [`DriverError::Unsupported`] from an engine that claims nothing.
    fn claim_interface(&mut self, _interface: u8) -> Result<(), DriverError> {
        Err(DriverError::Unsupported)
    }

    /// Start an isochronous stream of `layout` slots on `endpoint` in its
    /// interface's current setting, scheduling nothing until a slot is queued.
    ///
    /// # Errors
    ///
    /// [`DriverError::NotFound`] for an endpoint the governed settings do not
    /// bring, [`DriverError::OutOfRange`] for a layout whose intervals overrun
    /// the endpoint's payload or ring, [`DriverError::AlreadyExists`] for an
    /// endpoint already streaming, or [`DriverError::Unsupported`].
    fn iso_start(
        &mut self,
        _endpoint: u8,
        _layout: IsoLayout,
    ) -> Result<IsoStreamShape, DriverError> {
        Err(DriverError::Unsupported)
    }

    /// Hand slot `slot` of the stream on `endpoint` to the controller,
    /// reading an OUT slot's records and data out of `region`.
    ///
    /// # Errors
    ///
    /// [`DriverError::NotFound`] for no such stream, [`DriverError::Busy`]
    /// for a slot already queued, [`DriverError::OutOfRange`] for a record
    /// past its packet's budget, or [`DriverError::Unsupported`].
    fn iso_queue(&mut self, _endpoint: u8, _slot: u16, _region: &[u8]) -> Result<(), DriverError> {
        Err(DriverError::Unsupported)
    }

    /// Stop the stream on `endpoint`, discarding what it still had queued.
    ///
    /// # Errors
    ///
    /// [`DriverError::NotFound`] for no such stream, or
    /// [`DriverError::Unsupported`].
    fn iso_stop(&mut self, _endpoint: u8) -> Result<(), DriverError> {
        Err(DriverError::Unsupported)
    }

    /// Take the next slot the stream on `endpoint` finished, writing its
    /// records — and an IN slot's data — into `region`.
    ///
    /// # Errors
    ///
    /// [`DriverError::NotFound`] for no such stream, or the fault that halted
    /// it; [`DriverError::Unsupported`] from an engine with no streams.
    fn iso_take(
        &mut self,
        _endpoint: u8,
        _region: &mut [u8],
    ) -> Result<Option<IsoSlotDone>, DriverError> {
        Err(DriverError::Unsupported)
    }
}

/// What a started stream runs at, for its grant.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct IsoStreamShape {
    /// Microframes between two of the endpoint's service intervals.
    pub interval_microframes: u32,
    /// The device's bus speed.
    pub speed: UsbSpeed,
}

/// One finished slot, for its notification.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct IsoSlotDone {
    /// The slot.
    pub slot: u16,
    /// Intervals that passed carrying nothing before the slot began.
    pub skipped: u32,
    /// The extended bus microframe of the slot's first interval.
    pub microframe: u64,
}

/// What one interface's class driver may reach through control requests.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UrbScope {
    /// The interfaces the node governs: its own and those it claimed.
    pub interfaces: BitSet256,
    /// Their endpoints in their current settings: bit `n` set for Device
    /// Context Index `n` (twice the endpoint number, plus one for IN).
    pub endpoints: u32,
}

impl UrbScope {
    /// Whether `interface` is one the node governs.
    #[must_use]
    pub const fn governs(self, interface: u8) -> bool {
        self.interfaces.contains(interface as u16)
    }

    /// Whether endpoint address `address` (direction bit 7, number bits 0–3)
    /// is one of the interface's own.
    #[must_use]
    pub const fn owns_endpoint(self, address: u8) -> bool {
        let number = address & 0x0F;
        let dci = number * 2 + (address >> 7);
        number != 0 && address & 0x70 == 0 && self.endpoints & (1 << dci) != 0
    }
}

/// `bmRequestType` fields (USB 2.0 §9.3.1).
const REQUEST_DIRECTION_IN: u8 = 0x80;
const REQUEST_TYPE_SHIFT: u8 = 5;
const REQUEST_TYPE_STANDARD: u8 = 0;
const REQUEST_TYPE_CLASS: u8 = 1;
const REQUEST_TYPE_VENDOR: u8 = 2;
const RECIPIENT_MASK: u8 = 0x1F;
const RECIPIENT_DEVICE: u8 = 0;
const RECIPIENT_INTERFACE: u8 = 1;
const RECIPIENT_ENDPOINT: u8 = 2;

/// Standard `bRequest` codes a class driver may issue (USB 2.0 Table 9-4).
const GET_STATUS: u8 = 0;
const GET_DESCRIPTOR: u8 = 6;
const GET_CONFIGURATION: u8 = 8;
const GET_INTERFACE: u8 = 10;

/// Whether a class driver serving `scope` may send the control request
/// `setup` moving data `direction`.
///
/// It may read the device's descriptors and status, and do anything to its
/// own interface and its own endpoints, but never change the device's state:
/// the configuration, the address, an alternate setting, a halt or a power
/// feature are the host controller's, and reach every interface of the
/// device. `wLength` must be the URB's length, so the data stage the device
/// answers is the one the URB carries.
#[must_use]
pub fn control_permitted(
    setup: [u8; 8],
    direction: UsbDirection,
    length: u32,
    scope: UrbScope,
) -> bool {
    let [request_type, request, _, _, index_low, index_high, length_low, length_high] = setup;
    let reads = request_type & REQUEST_DIRECTION_IN != 0;
    if reads != (direction == UsbDirection::In)
        || u32::from(u16::from_le_bytes([length_low, length_high])) != length
    {
        return false;
    }
    let own_interface = scope.governs(index_low);
    match (
        request_type >> REQUEST_TYPE_SHIFT & 0x03,
        request_type & RECIPIENT_MASK,
    ) {
        (REQUEST_TYPE_STANDARD, RECIPIENT_DEVICE) => {
            reads && matches!(request, GET_STATUS | GET_DESCRIPTOR | GET_CONFIGURATION)
        }
        (REQUEST_TYPE_STANDARD, RECIPIENT_INTERFACE) => {
            reads
                && index_high == 0
                && own_interface
                && matches!(request, GET_STATUS | GET_DESCRIPTOR | GET_INTERFACE)
        }
        (REQUEST_TYPE_STANDARD, RECIPIENT_ENDPOINT) => {
            reads && request == GET_STATUS && index_high == 0 && scope.owns_endpoint(index_low)
        }
        (REQUEST_TYPE_CLASS | REQUEST_TYPE_VENDOR, RECIPIENT_INTERFACE) => own_interface,
        (REQUEST_TYPE_CLASS | REQUEST_TYPE_VENDOR, RECIPIENT_ENDPOINT) => {
            scope.owns_endpoint(index_low)
        }
        _ => false,
    }
}

/// Validate `urb` fail-closed against the interface and drive it on `engine`
/// over the shared `data` buffer, returning the transfer outcome.
///
/// This is the controller-side body the HCD runs for a
/// [`UsbRequest::Transfer`] it received. It is **asynchronous**:
///
/// * `Ok(Some(n))` — the transfer completed; `n` bytes landed in `data`. The
///   HCD frames a completion with [`frame_completion`] and replies now.
/// * `Ok(None)` — an interrupt-IN report has not arrived yet. The HCD leaves
///   the caller's URB call outstanding and re-drives this same `request` on
///   its next controller interrupt (the report path); it never busy-polls and
///   never blocks one interface inside another's handler.
/// * `Err(_)` — a malformed or illegal URB (a bad endpoint/direction/transfer
///   type, an oversize length, a control request outside the interface's
///   [`UrbScope`], which is [`Errno::PermissionDenied`]), refused **before**
///   the engine is touched, or a controller fault. The HCD frames an error
///   completion and replies now, so the blocked caller always fails closed.
///
/// A held URB is driven again on each controller event, re-validated against
/// the interface as it stands then.
pub fn drive_urb<E: UrbEngine>(
    urb: &UrbRequest,
    data: &mut [u8],
    engine: &mut E,
) -> Result<Option<u32>, Errno> {
    let length = urb.length as usize;
    // The transfer may never run past the mapped shared buffer.
    if length > data.len() {
        return Err(Errno::LengthOutOfRange);
    }
    let slice = &mut data[..length];
    match urb.transfer_type {
        UsbTransferType::Control => {
            // A control transfer is the endpoint-0 protocol; any other
            // endpoint number is illegal.
            if urb.endpoint != 0 {
                return Err(Errno::OutOfRange);
            }
            if !engine
                .scope()
                .is_some_and(|scope| control_permitted(urb.setup, urb.direction, urb.length, scope))
            {
                return Err(Errno::PermissionDenied);
            }
            // The served control-OUT shapes: the no-data form (SETUP only)
            // and the data-stage form carrying the shared buffer's bytes.
            if urb.direction == UsbDirection::Out {
                if urb.length == 0 {
                    engine
                        .control_no_data(urb.setup)
                        .map_err(DriverError::as_errno)?;
                    return Ok(Some(0));
                }
                engine
                    .control_out(urb.setup, slice)
                    .map_err(DriverError::as_errno)?;
                return Ok(Some(urb.length));
            }
            // A control transfer completes synchronously within the call.
            let transferred = engine
                .control_in(urb.setup, slice)
                .map_err(DriverError::as_errno)?;
            Ok(Some(
                u32::try_from(transferred).map_err(|_| Errno::LengthOutOfRange)?,
            ))
        }
        UsbTransferType::Interrupt => {
            // The interface's own interrupt-IN endpoint, whose report lands
            // anywhere in the shared buffer.
            if urb.direction != UsbDirection::In
                || !engine
                    .scope()
                    .is_some_and(|scope| scope.owns_endpoint(urb.endpoint | REQUEST_DIRECTION_IN))
            {
                return Err(Errno::OutOfRange);
            }
            match engine
                .interrupt_in(urb.endpoint, length, data)
                .map_err(DriverError::as_errno)?
            {
                Some(transferred) => Ok(Some(
                    u32::try_from(transferred).map_err(|_| Errno::LengthOutOfRange)?,
                )),
                // No report yet — hold the URB outstanding (Ok(None)), do not
                // fabricate a completion.
                None => Ok(None),
            }
        }
        UsbTransferType::Bulk => {
            // A bulk transfer targets a device endpoint, never the shared
            // control endpoint. Whether the endpoint is the interface's
            // configured bulk endpoint in that direction is the engine's
            // check (it owns the interface's endpoint map); both fail
            // closed before any ring is touched.
            if urb.endpoint == 0 {
                return Err(Errno::OutOfRange);
            }
            let outcome = match urb.direction {
                UsbDirection::In => engine.bulk_in(urb.endpoint, slice),
                UsbDirection::Out => engine.bulk_out(urb.endpoint, slice),
            }
            .map_err(DriverError::as_errno)?;
            match outcome {
                Some(transferred) => Ok(Some(
                    u32::try_from(transferred).map_err(|_| Errno::LengthOutOfRange)?,
                )),
                // Still in flight — hold the URB outstanding; the next
                // controller event re-drives it.
                None => Ok(None),
            }
        }
    }
}

/// Frame a completed transfer outcome into a URB completion in `reply`,
/// returning the reply length.
///
/// `Ok(n)` becomes a success completion carrying the bytes transferred; an
/// `Err` becomes a status-framed error completion, so the blocked caller is
/// always answered and fails closed. This is the wire transformation the HCD
/// runs immediately before
/// [`call_reply`](tairix_abi::SyscallNumber::CALL_REPLY).
///
/// # Errors
///
/// [`Errno::BufferTooSmall`] if `reply` cannot hold a completion frame (it
/// must be at least [`URB_COMPLETION_LEN`](tairix_abi::usb_urb::URB_COMPLETION_LEN)).
/// The caller sizes it so.
pub fn frame_completion(reply: &mut [u8], result: Result<u32, Errno>) -> Result<usize, Errno> {
    match result {
        Ok(transferred) => encode_completion(reply, transferred),
        Err(err) => encode_error_completion(reply, err),
    }
}

/// The class-side transport: one synchronous URB call to the HCD's endpoint.
///
/// A class driver implements this over the kernel
/// [`ipc_call`](tairix_abi::SyscallNumber::IPC_CALL) surface (`plans/USB.md`
/// U4); a host test implements it by routing the bytes through [`drive_urb`]
/// and [`frame_completion`].
pub trait UrbCall {
    /// Send the encoded URB `request` to the HCD and read the framed
    /// completion into `reply`, returning its length.
    ///
    /// # Errors
    ///
    /// An [`Errno`] from the underlying call transport (a dead endpoint, a
    /// truncated reply).
    fn call(&mut self, request: &[u8], reply: &mut [u8]) -> Result<usize, Errno>;
}

/// The class-side URB transport client: builds URBs, submits them over a
/// [`UrbCall`] transport, and decodes the completions.
pub struct UrbClient<T: UrbCall> {
    transport: T,
}

impl<T: UrbCall> UrbClient<T> {
    /// Wrap a call transport.
    pub fn new(transport: T) -> Self {
        Self { transport }
    }

    /// Borrow the underlying call transport, so a class driver can observe
    /// transport-level state it records there (e.g. that the served
    /// interface's endpoint has vanished after a hot-unplug).
    #[must_use]
    pub fn transport(&self) -> &T {
        &self.transport
    }

    /// Submit `urb` and decode the completion, returning the bytes
    /// transferred.
    ///
    /// # Errors
    ///
    /// The carried completion [`Errno`] (a controller/device fault, or a
    /// rejected URB), or an encode/transport error. The call blocks in the
    /// kernel until the HCD replies, so a not-yet-ready interrupt-IN report
    /// parks the caller rather than surfacing a retryable error.
    fn submit(&mut self, urb: &UrbRequest) -> Result<u32, Errno> {
        let mut reply = [0u8; USB_REPLY_MAX_LEN];
        let len = self.call(&UsbRequest::Transfer(*urb), &mut reply)?;
        decode_completion(&reply[..len])
    }

    /// Send `request` and read its reply into `reply`, returning its length.
    fn call(&mut self, request: &UsbRequest, reply: &mut [u8]) -> Result<usize, Errno> {
        let mut frame = [0u8; USB_REQUEST_MAX_LEN];
        let n = request.encode(&mut frame)?;
        self.transport.call(&frame[..n], reply)
    }

    /// Send a request answered by a status frame alone.
    fn call_for_status(&mut self, request: &UsbRequest) -> Result<(), Errno> {
        let mut reply = [0u8; USB_REPLY_MAX_LEN];
        let len = self.call(request, &mut reply)?;
        decode_status_reply(&reply[..len])
    }

    /// Select `alternate` on `interface` ([`UsbRequest::SetInterface`]).
    ///
    /// # Errors
    ///
    /// The HCD's refusal — [`Errno::NoBandwidth`] when the bus cannot
    /// schedule the setting — or a transport error.
    pub fn set_interface(&mut self, interface: u8, alternate: u8) -> Result<(), Errno> {
        self.call_for_status(&UsbRequest::SetInterface {
            interface,
            alternate,
        })
    }

    /// Govern `interface` of the same device ([`UsbRequest::ClaimInterface`]).
    ///
    /// # Errors
    ///
    /// The HCD's refusal, or a transport error.
    pub fn claim_interface(&mut self, interface: u8) -> Result<(), Errno> {
        self.call_for_status(&UsbRequest::ClaimInterface { interface })
    }

    /// Start an isochronous stream ([`UsbRequest::IsoStart`]), answering its
    /// grant. The caller binds its notify port before starting, since the HCD
    /// may report the first slot as soon as it is queued.
    ///
    /// # Errors
    ///
    /// The HCD's refusal, or a transport error.
    pub fn iso_start(&mut self, params: IsoStartParams) -> Result<IsoGrant, Errno> {
        let mut reply = [0u8; USB_REPLY_MAX_LEN];
        let len = self.call(&UsbRequest::IsoStart(params), &mut reply)?;
        IsoGrant::decode(&reply[..len])
    }

    /// Queue slot `slot` of the stream on `endpoint` ([`UsbRequest::IsoQueue`]).
    ///
    /// # Errors
    ///
    /// The HCD's refusal, or a transport error.
    pub fn iso_queue(&mut self, endpoint: u8, slot: u16) -> Result<(), Errno> {
        self.call_for_status(&UsbRequest::IsoQueue { endpoint, slot })
    }

    /// Stop the stream on `endpoint` ([`UsbRequest::IsoStop`]).
    ///
    /// # Errors
    ///
    /// The HCD's refusal, or a transport error.
    pub fn iso_stop(&mut self, endpoint: u8) -> Result<(), Errno> {
        self.call_for_status(&UsbRequest::IsoStop { endpoint })
    }

    /// Submit a control-IN URB on endpoint 0 reading `length` bytes into the
    /// shared buffer, returning the bytes the device delivered.
    ///
    /// # Errors
    ///
    /// The carried completion [`Errno`], or an encode/transport error.
    pub fn control_in(&mut self, setup: [u8; 8], length: u32) -> Result<u32, Errno> {
        self.submit(&UrbRequest {
            endpoint: 0,
            transfer_type: UsbTransferType::Control,
            direction: UsbDirection::In,
            length,
            setup,
        })
    }

    /// Submit a no-data control-OUT URB on endpoint 0 (SETUP + status stage
    /// only): a class request whose whole meaning rides in `setup`, e.g. the
    /// BOT Bulk-Only Mass Storage Reset.
    ///
    /// # Errors
    ///
    /// The carried completion [`Errno`], or an encode/transport error.
    pub fn control_no_data(&mut self, setup: [u8; 8]) -> Result<(), Errno> {
        self.submit(&UrbRequest {
            endpoint: 0,
            transfer_type: UsbTransferType::Control,
            direction: UsbDirection::Out,
            length: 0,
            setup,
        })
        .map(|_| ())
    }

    /// Submit a control-OUT URB on endpoint 0 whose OUT data stage carries
    /// `length` bytes from the shared buffer: a class request with a
    /// payload, e.g. the CBI ADSC command block.
    ///
    /// # Errors
    ///
    /// The carried completion [`Errno`] — notably [`Errno::EndpointStalled`]
    /// when the device refused the request with a protocol STALL — or an
    /// encode/transport error. A zero `length` is refused
    /// ([`Errno::LengthOutOfRange`]): the no-data form is
    /// [`Self::control_no_data`], and the two must not be conflated.
    pub fn control_out(&mut self, setup: [u8; 8], length: u32) -> Result<(), Errno> {
        if length == 0 {
            return Err(Errno::LengthOutOfRange);
        }
        self.submit(&UrbRequest {
            endpoint: 0,
            transfer_type: UsbTransferType::Control,
            direction: UsbDirection::Out,
            length,
            setup,
        })
        .map(|_| ())
    }

    /// Submit an interrupt-IN URB for `endpoint` reading one report into the
    /// shared buffer, returning the bytes transferred. `length` is the
    /// longest report expected; a report may run past it up to one service
    /// interval's payload, so the whole shared buffer receives it.
    ///
    /// # Errors
    ///
    /// The carried completion [`Errno`] (a controller/device fault), or an
    /// encode/transport error. The call blocks until a report arrives, so the
    /// class driver parks rather than busy-polling for the next report.
    pub fn interrupt_in(&mut self, endpoint: u8, length: u32) -> Result<u32, Errno> {
        self.submit(&UrbRequest {
            endpoint,
            transfer_type: UsbTransferType::Interrupt,
            direction: UsbDirection::In,
            length,
            setup: [0; 8],
        })
    }

    /// Submit a bulk-IN URB for `endpoint` reading up to `length` bytes into
    /// the shared buffer, returning the bytes the device delivered (a short
    /// packet yields fewer than `length`).
    ///
    /// # Errors
    ///
    /// The carried completion [`Errno`] — notably
    /// [`Errno::EndpointStalled`] when the device answered the transfer
    /// with STALL (the endpoint is already recovered; the caller runs its
    /// class-level recovery and may submit again) — or an encode/transport
    /// error. The call blocks until the transfer completes.
    pub fn bulk_in(&mut self, endpoint: u8, length: u32) -> Result<u32, Errno> {
        self.submit(&UrbRequest {
            endpoint,
            transfer_type: UsbTransferType::Bulk,
            direction: UsbDirection::In,
            length,
            setup: [0; 8],
        })
    }

    /// Submit a bulk-OUT URB for `endpoint` writing `length` bytes from the
    /// shared buffer, returning the bytes the device accepted.
    ///
    /// # Errors
    ///
    /// As [`Self::bulk_in`].
    pub fn bulk_out(&mut self, endpoint: u8, length: u32) -> Result<u32, Errno> {
        self.submit(&UrbRequest {
            endpoint,
            transfer_type: UsbTransferType::Bulk,
            direction: UsbDirection::Out,
            length,
            setup: [0; 8],
        })
    }
}

/// One control-IN transfer: its SETUP, the buffer its data stage fills, and
/// the bytes the device delivered.
pub type ControlIn<'a> = dyn FnMut([u8; 8], &mut [u8]) -> Result<usize, Errno> + 'a;

/// Why a class driver could not read its device's configuration descriptor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigurationError {
    /// A control transfer failed.
    Transfer(Errno),
    /// The header is malformed, the stream is longer than one data stage
    /// carries, or the device delivered less than its header stated.
    Malformed,
    /// Memory for the stream ran out.
    OutOfMemory,
}

/// Read the device's whole configuration descriptor: its header for the
/// stated total, then exactly that many bytes. `control_in` runs one
/// control-IN transfer of `data.len()` bytes and answers the bytes delivered.
///
/// A stream longer than one data stage carries ([`CTRL_DATA_LEN`]) is refused
/// rather than read cut short, since a truncated stream ends mid-descriptor.
///
/// # Errors
///
/// [`ConfigurationError`].
pub fn read_configuration(control_in: &mut ControlIn<'_>) -> Result<Vec<u8>, ConfigurationError> {
    let mut header = [0u8; CONFIGURATION_HEADER_LEN];
    let header_len = u16::try_from(header.len()).map_err(|_| ConfigurationError::Malformed)?;
    let read = control_in(setup_get_configuration_descriptor(header_len), &mut header)
        .map_err(ConfigurationError::Transfer)?;
    let total = ConfigurationHeader::decode(header.get(..read).unwrap_or_default())
        .map_err(|Malformed| ConfigurationError::Malformed)?
        .total;
    if total > CTRL_DATA_LEN {
        return Err(ConfigurationError::Malformed);
    }
    let total_len = u16::try_from(total).map_err(|_| ConfigurationError::Malformed)?;
    let mut config = Vec::new();
    config
        .try_reserve_exact(total)
        .map_err(|_| ConfigurationError::OutOfMemory)?;
    config.resize(total, 0);
    let read = control_in(setup_get_configuration_descriptor(total_len), &mut config)
        .map_err(ConfigurationError::Transfer)?;
    if read != total {
        return Err(ConfigurationError::Malformed);
    }
    Ok(config)
}

/// A class driver's link to its interface: the URB client and the driver's
/// mapping of the interface's shared data buffer, through which every
/// transfer's bytes move. Bulk transfers longer than the buffer are split
/// into per-URB chunks; a short chunk ends the transfer.
pub struct UrbLink<'a, T: UrbCall> {
    client: UrbClient<T>,
    shm: &'a mut [u8],
}

impl<'a, T: UrbCall> UrbLink<'a, T> {
    /// Link `client` to the shared buffer `shm` its URBs name.
    pub fn new(client: UrbClient<T>, shm: &'a mut [u8]) -> Self {
        Self { client, shm }
    }

    /// The URB client, so a driver can read transport state it records
    /// there.
    #[must_use]
    pub fn client(&self) -> &UrbClient<T> {
        &self.client
    }

    /// The URB client, for the interface and stream operations that move no
    /// bytes through the shared buffer.
    pub fn client_mut(&mut self) -> &mut UrbClient<T> {
        &mut self.client
    }

    /// The longest transfer one URB carries.
    fn chunk(&self) -> usize {
        self.shm.len().min(BULK_BUF_LEN)
    }

    /// Run a control-IN transfer of `data.len()` bytes into `data`, returning
    /// the bytes the device delivered.
    ///
    /// # Errors
    ///
    /// [`Errno::LengthOutOfRange`] for a data stage past the shared buffer,
    /// or the completion's [`Errno`].
    pub fn control_in(&mut self, setup: [u8; 8], data: &mut [u8]) -> Result<usize, Errno> {
        if data.len() > self.shm.len() {
            return Err(Errno::LengthOutOfRange);
        }
        let len = u32::try_from(data.len()).map_err(|_| Errno::LengthOutOfRange)?;
        let delivered = usize::try_from(self.client.control_in(setup, len)?)
            .map_err(|_| Errno::LengthOutOfRange)?
            .min(data.len());
        data[..delivered].copy_from_slice(&self.shm[..delivered]);
        Ok(delivered)
    }

    /// Run a control-OUT transfer whose data stage carries `data`.
    ///
    /// # Errors
    ///
    /// [`Errno::LengthOutOfRange`] for a data stage past the shared buffer,
    /// or the completion's [`Errno`].
    pub fn control_out(&mut self, setup: [u8; 8], data: &[u8]) -> Result<(), Errno> {
        let staged = self
            .shm
            .get_mut(..data.len())
            .ok_or(Errno::LengthOutOfRange)?;
        staged.copy_from_slice(data);
        let len = u32::try_from(data.len()).map_err(|_| Errno::LengthOutOfRange)?;
        self.client.control_out(setup, len)
    }

    /// Run a no-data control transfer.
    ///
    /// # Errors
    ///
    /// The completion's [`Errno`].
    pub fn control_no_data(&mut self, setup: [u8; 8]) -> Result<(), Errno> {
        self.client.control_no_data(setup)
    }

    /// Read up to `data.len()` bytes from bulk-IN `endpoint`, returning the
    /// bytes delivered.
    ///
    /// # Errors
    ///
    /// The completion's [`Errno`].
    pub fn bulk_in(&mut self, endpoint: u8, data: &mut [u8]) -> Result<usize, Errno> {
        let mut at = 0usize;
        while at < data.len() {
            let chunk = (data.len() - at).min(self.chunk());
            let len = u32::try_from(chunk).map_err(|_| Errno::LengthOutOfRange)?;
            let delivered = usize::try_from(self.client.bulk_in(endpoint, len)?)
                .map_err(|_| Errno::LengthOutOfRange)?
                .min(chunk);
            data[at..at + delivered].copy_from_slice(&self.shm[..delivered]);
            at += delivered;
            if delivered < chunk {
                break;
            }
        }
        Ok(at)
    }

    /// Write `data` to bulk-OUT `endpoint`, returning the bytes the device
    /// accepted.
    ///
    /// # Errors
    ///
    /// The completion's [`Errno`].
    pub fn bulk_out(&mut self, endpoint: u8, data: &[u8]) -> Result<usize, Errno> {
        let mut at = 0usize;
        while at < data.len() {
            let chunk = (data.len() - at).min(self.chunk());
            self.shm[..chunk].copy_from_slice(&data[at..at + chunk]);
            let len = u32::try_from(chunk).map_err(|_| Errno::LengthOutOfRange)?;
            let accepted = usize::try_from(self.client.bulk_out(endpoint, len)?)
                .map_err(|_| Errno::LengthOutOfRange)?
                .min(chunk);
            at += accepted;
            if accepted < chunk {
                break;
            }
        }
        Ok(at)
    }

    /// Read interrupt-IN `endpoint`'s next report into `data`, returning its
    /// length. `expected` is the longest report expected; the report may be
    /// longer, up to one service interval's payload ([`UrbEngine::interrupt_in`]).
    ///
    /// # Errors
    ///
    /// [`Errno::BufferTooSmall`] for a report longer than `data`, or the
    /// completion's [`Errno`].
    pub fn interrupt_in(
        &mut self,
        endpoint: u8,
        expected: usize,
        data: &mut [u8],
    ) -> Result<usize, Errno> {
        let len = u32::try_from(expected).map_err(|_| Errno::LengthOutOfRange)?;
        let delivered = usize::try_from(self.client.interrupt_in(endpoint, len)?)
            .map_err(|_| Errno::LengthOutOfRange)?;
        let report = self.shm.get(..delivered).ok_or(Errno::LengthOutOfRange)?;
        data.get_mut(..delivered)
            .ok_or(Errno::BufferTooSmall)?
            .copy_from_slice(report);
        Ok(delivered)
    }

    /// Zero the shared buffer, so nothing a transfer carried outlives it.
    pub fn scrub(&mut self) {
        self.shm.fill(0);
    }
}

#[cfg(test)]
mod tests;
