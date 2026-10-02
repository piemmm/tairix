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

use tairix_abi::usb_urb::{
    decode_completion, encode_completion, encode_error_completion, UrbRequest, UsbDirection,
    UsbTransferType, URB_COMPLETION_LEN, URB_REQUEST_LEN,
};
use tairix_abi::{DriverError, Errno};

use crate::device::BULK_BUF_LEN;

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

    /// Poll the interface's interrupt-IN endpoint for one pending report into
    /// `data`, the whole shared buffer. `request` is the longest report the
    /// class driver expects: the endpoint is armed to it, or to one service
    /// interval's payload if that is longer, so a report may exceed it.
    /// `Ok(Some(n))` if a report of `n` bytes arrived, `Ok(None)` if none is
    /// pending yet (the caller retries).
    ///
    /// # Errors
    ///
    /// A [`DriverError`] from the controller/device, or for a `request` of
    /// zero, past `data`, or other than the one the endpoint was first armed
    /// to.
    fn interrupt_in(
        &mut self,
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
}

/// What one interface's class driver may reach through control requests.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UrbScope {
    /// The served interface's `bInterfaceNumber`.
    pub interface: u8,
    /// The interface's endpoints: bit `n` set for Device Context Index `n`
    /// (twice the endpoint number, plus one for IN).
    pub endpoints: u32,
}

impl UrbScope {
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
    let index = u16::from_le_bytes([index_low, index_high]);
    let own_interface = index_low == scope.interface;
    match (
        request_type >> REQUEST_TYPE_SHIFT & 0x03,
        request_type & RECIPIENT_MASK,
    ) {
        (REQUEST_TYPE_STANDARD, RECIPIENT_DEVICE) => {
            reads && matches!(request, GET_STATUS | GET_DESCRIPTOR | GET_CONFIGURATION)
        }
        (REQUEST_TYPE_STANDARD, RECIPIENT_INTERFACE) => {
            reads
                && index == u16::from(scope.interface)
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

/// Decode `request`, validate it fail-closed against the interface, and drive
/// it on `engine` over the shared `data` buffer, returning the transfer
/// outcome.
///
/// This is the controller-side body the HCD runs after
/// [`call_recv`](tairix_abi::SyscallNumber::CALL_RECV). It is **asynchronous**:
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
/// Re-decoding the stored `request` each time it is driven keeps the
/// validation in one place and costs only a fixed-size parse.
pub fn drive_urb<E: UrbEngine>(
    request: &[u8],
    data: &mut [u8],
    engine: &mut E,
) -> Result<Option<u32>, Errno> {
    let urb = UrbRequest::decode(request)?;
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
                .interrupt_in(length, data)
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
/// must be at least [`URB_COMPLETION_LEN`]). The caller sizes it so.
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
        let mut request = [0u8; URB_REQUEST_LEN];
        let n = urb.encode(&mut request)?;
        let mut reply = [0u8; URB_COMPLETION_LEN];
        let len = self.transport.call(&request[..n], &mut reply)?;
        decode_completion(&reply[..len])
    }

    /// Submit a control-IN URB on endpoint 0 reading into the shared `buffer`
    /// of `length` bytes, returning the bytes the device delivered.
    ///
    /// # Errors
    ///
    /// The carried completion [`Errno`], or an encode/transport error.
    pub fn control_in(&mut self, setup: [u8; 8], buffer: u64, length: u32) -> Result<u32, Errno> {
        self.submit(&UrbRequest {
            endpoint: 0,
            transfer_type: UsbTransferType::Control,
            direction: UsbDirection::In,
            buffer,
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
            buffer: 0,
            length: 0,
            setup,
        })
        .map(|_| ())
    }

    /// Submit a control-OUT URB on endpoint 0 whose OUT data stage carries
    /// `length` bytes from the shared `buffer`: a class request with a
    /// payload, e.g. the CBI ADSC command block.
    ///
    /// # Errors
    ///
    /// The carried completion [`Errno`] — notably [`Errno::EndpointStalled`]
    /// when the device refused the request with a protocol STALL — or an
    /// encode/transport error. A zero `length` is refused
    /// ([`Errno::LengthOutOfRange`]): the no-data form is
    /// [`Self::control_no_data`], and the two must not be conflated.
    pub fn control_out(&mut self, setup: [u8; 8], buffer: u64, length: u32) -> Result<(), Errno> {
        if length == 0 {
            return Err(Errno::LengthOutOfRange);
        }
        self.submit(&UrbRequest {
            endpoint: 0,
            transfer_type: UsbTransferType::Control,
            direction: UsbDirection::Out,
            buffer,
            length,
            setup,
        })
        .map(|_| ())
    }

    /// Submit an interrupt-IN URB for `endpoint` reading one report into the
    /// shared `buffer`, returning the bytes transferred. `length` is the
    /// longest report expected; a report may run past it up to one service
    /// interval's payload, so the whole shared buffer receives it.
    ///
    /// # Errors
    ///
    /// The carried completion [`Errno`] (a controller/device fault), or an
    /// encode/transport error. The call blocks until a report arrives, so the
    /// class driver parks rather than busy-polling for the next report.
    pub fn interrupt_in(&mut self, endpoint: u8, buffer: u64, length: u32) -> Result<u32, Errno> {
        self.submit(&UrbRequest {
            endpoint,
            transfer_type: UsbTransferType::Interrupt,
            direction: UsbDirection::In,
            buffer,
            length,
            setup: [0; 8],
        })
    }

    /// Submit a bulk-IN URB for `endpoint` reading up to `length` bytes into
    /// the shared `buffer`, returning the bytes the device delivered (a
    /// short packet yields fewer than `length`).
    ///
    /// # Errors
    ///
    /// The carried completion [`Errno`] — notably
    /// [`Errno::EndpointStalled`] when the device answered the transfer
    /// with STALL (the endpoint is already recovered; the caller runs its
    /// class-level recovery and may submit again) — or an encode/transport
    /// error. The call blocks until the transfer completes.
    pub fn bulk_in(&mut self, endpoint: u8, buffer: u64, length: u32) -> Result<u32, Errno> {
        self.submit(&UrbRequest {
            endpoint,
            transfer_type: UsbTransferType::Bulk,
            direction: UsbDirection::In,
            buffer,
            length,
            setup: [0; 8],
        })
    }

    /// Submit a bulk-OUT URB for `endpoint` writing `length` bytes from the
    /// shared `buffer`, returning the bytes the device accepted.
    ///
    /// # Errors
    ///
    /// As [`Self::bulk_in`].
    pub fn bulk_out(&mut self, endpoint: u8, buffer: u64, length: u32) -> Result<u32, Errno> {
        self.submit(&UrbRequest {
            endpoint,
            transfer_type: UsbTransferType::Bulk,
            direction: UsbDirection::Out,
            buffer,
            length,
            setup: [0; 8],
        })
    }
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
        let delivered = usize::try_from(self.client.control_in(setup, 0, len)?)
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
        self.client.control_out(setup, 0, len)
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
            let delivered = usize::try_from(self.client.bulk_in(endpoint, 0, len)?)
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
            let accepted = usize::try_from(self.client.bulk_out(endpoint, 0, len)?)
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
        let delivered = usize::try_from(self.client.interrupt_in(endpoint, 0, len)?)
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
