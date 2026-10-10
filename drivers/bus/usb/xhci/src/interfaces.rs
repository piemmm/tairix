//! The interface nodes the HCD publishes and the URB transports they ride.
//!
//! Each served interface gets a node carrying a transport: a call endpoint
//! its class driver submits URBs on and a shared buffer the data moves
//! through. A node is kept while the device it was built from is served,
//! wherever the device table now places it — the Linux USB core's
//! reset-and-verify rule (`usb_reset_and_verify_device`). A node id is never
//! reissued, so keeping the node is what lets a device survive a controller
//! reset with its driver bound.
//!
//! A node's buffer is created for it and carried by no other: the kernel
//! retires every region a removed node conferred, and a fresh region holds
//! nothing of another device's. Endpoints are reused. A removed node's grant
//! to one is revoked with it, and whatever its driver queued before that is
//! answered `NotFound` before the endpoint carries another node.
//!
//! A node's isochronous streams each ride a region of their own, created for
//! the stream and granted to the caller that started it, whose slot
//! completions go to the notify port its attested pid names. A stream ends
//! with its device, with a controller reset, or when its notification cannot
//! be delivered; its class driver is told why when it still can be.

use alloc::vec::Vec;
use core::num::NonZeroU32;

use tairix_abi::usb_urb::{
    iso_notify_endpoint_for, IsoGrant, IsoNotify, IsoStartParams, UsbRequest, USB_REQUEST_MAX_LEN,
};
use tairix_abi::{DriverError, Errno, HwNode, ProcId};
use tairix_usb::device::{DeviceIdentity, HubEvent, BULK_BUF_LEN};
use tairix_usb::transport::UrbEngine;

use crate::domain::{ControllerDomainEvent, ControllerHealth};
use crate::serve::{attach_transport_grants, Reach, UrbOutcome, UrbReply, UrbService};

/// Calls a transport endpoint queues before it is served. A class driver
/// submits one at a time and blocks on the reply, so a small queue absorbs a
/// resubmit racing the previous reply.
pub const ENDPOINT_CAPACITY: usize = 4;

/// Bytes of a node's URB buffer: one bulk chunk, the engine's per-TD ceiling,
/// which also holds a boot report and any control-IN descriptor a class driver
/// reads. One page, so the mass-storage data path costs the keyboard path
/// nothing extra.
pub const URB_BUFFER_LEN: usize = BULK_BUF_LEN;

/// The HCD's own mapping of one node's shared buffer, released when dropped.
pub trait UrbBuffer {
    /// The region's kernel id, which the node carries as its grant.
    fn region(&self) -> u64;
    /// The mapped bytes.
    fn bytes(&mut self) -> &mut [u8];
}

/// Something the table did or saw that a diagnostic should record.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Note {
    /// A node was published.
    Published {
        /// The id the kernel assigned it.
        node: u32,
    },
    /// A URB completed with an error.
    UrbFailed {
        /// The device-table index of the device it was for.
        index: usize,
        /// What its class driver is answered.
        errno: Errno,
    },
    /// A faulted transfer was its device leaving: the device is detached and
    /// its nodes retracted.
    FaultDetached,
    /// Whether a faulting device is still attached could not be read.
    DetachUnconfirmed(DriverError),
    /// The hub change a fault detach left could not be serviced.
    HubServiceFailed(DriverError),
    /// A woken endpoint's queue could not be read.
    ReceiveFailed(Errno),
    /// The controller's fault domain moved.
    Domain {
        /// The edge.
        event: ControllerDomainEvent,
        /// The controller's owner id.
        owner: u32,
    },
    /// An isochronous stream ended other than by its class driver's stop.
    StreamEnded {
        /// The device-table index of its device.
        index: usize,
        /// Its endpoint address.
        endpoint: u8,
        /// Why.
        reason: Errno,
    },
}

/// The controller engine and kernel calls the table drives: the live engine
/// and syscalls in the `Run` binary, a mock in the tests.
pub trait Seam {
    /// A node's shared buffer.
    type Buffer: UrbBuffer;
    /// One served device's transfer engine.
    type Engine<'a>: UrbEngine
    where
        Self: 'a;

    /// Entries in the controller's device table, live or free.
    fn table_len(&self) -> usize;
    /// The device served at device-table `index`, if any.
    fn identity(&self, index: usize) -> Option<DeviceIdentity>;
    /// The node describing the device served at `index`, before any
    /// transport grant.
    ///
    /// # Errors
    ///
    /// The engine's refusal, such as no device served there.
    fn describe(&self, index: usize) -> Result<HwNode, DriverError>;
    /// The transfer engine of the device served at `index`.
    fn engine(&mut self, index: usize) -> Self::Engine<'_>;
    /// Detach the device at `index` if its port reads it gone, returning
    /// whether it did.
    ///
    /// # Errors
    ///
    /// The port could not be read.
    fn detach_if_gone(&mut self, index: usize) -> Result<bool, DriverError>;
    /// Service one pending hub status change.
    ///
    /// # Errors
    ///
    /// The engine's refusal.
    fn next_hub_change(&mut self) -> Result<HubEvent, DriverError>;
    /// Whether the controller has latched a fatal error or halted.
    fn faulted(&mut self) -> bool;
    /// Reset the controller and enumerate it afresh.
    ///
    /// # Errors
    ///
    /// The reset or its re-programming failed.
    fn reset(&mut self) -> Result<(), DriverError>;
    /// Bind transport `slot`'s call endpoint, returning its id.
    fn open_endpoint(&mut self, slot: usize) -> Option<u64>;
    /// Register transport `slot`'s `endpoint` with the event loop, returning
    /// whether it took.
    fn watch_endpoint(&mut self, slot: usize, endpoint: u64) -> bool;
    /// A fresh, zeroed shared buffer of at least `len` bytes.
    fn create_buffer(&mut self, len: usize) -> Option<Self::Buffer>;
    /// The kernel-attested pid of the caller whose call `ticket` on
    /// `endpoint` is in service.
    ///
    /// # Errors
    ///
    /// The kernel's refusal, such as a caller that has ended.
    fn caller_pid(&mut self, endpoint: u64, ticket: u64) -> Result<u64, Errno>;
    /// Delegate `region` to the caller whose call `ticket` on `endpoint` is
    /// in service, returning the handle it maps.
    ///
    /// # Errors
    ///
    /// The kernel's refusal.
    fn grant_peer(&mut self, region: u64, endpoint: u64, ticket: u64) -> Result<u64, Errno>;
    /// This process's instance, the grantor of every region it delegates.
    fn self_instance(&self) -> ProcId;
    /// Send `notice` to the port `port`, without waiting.
    ///
    /// # Errors
    ///
    /// The kernel's refusal: no such port, or a full one.
    fn notify(&mut self, port: u64, notice: &IsoNotify) -> Result<(), Errno>;
    /// The next call queued on `endpoint`, without blocking: its ticket and
    /// the length of the request copied into `request`.
    ///
    /// # Errors
    ///
    /// The kernel's refusal to read the queue.
    fn receive(&mut self, endpoint: u64, request: &mut [u8])
        -> Result<Option<(u64, usize)>, Errno>;
    /// Answer a received call.
    fn reply(&mut self, endpoint: u64, reply: UrbReply);
    /// Publish `node`, returning the id the kernel assigned it.
    fn emit(&mut self, node: &HwNode) -> Option<u32>;
    /// Retract node `id`.
    fn remove(&mut self, id: u32);
    /// The monotonic clock, in nanoseconds.
    fn now_ns(&self) -> u64;
    /// Record `note`.
    fn note(&mut self, note: Note);
}

/// Every transport the HCD opened and the node each carries.
///
/// A transport's slot is its position, fixed for life: the event loop's wake
/// names it, and an endpoint once bound is never unbound. No two nodes serve
/// one device-table index.
pub struct Interfaces<B> {
    transports: Vec<Transport<B>>,
}

struct Transport<B> {
    endpoint: u64,
    /// Whether a submit on the endpoint wakes the event loop; a transport
    /// whose endpoint does not is never offered to a node.
    watched: bool,
    service: UrbService,
    node: Option<Node<B>>,
}

struct Node<B> {
    id: u32,
    identity: DeviceIdentity,
    /// Where the device is served; a controller reset can move it.
    index: usize,
    buffer: B,
    streams: Vec<Stream<B>>,
    /// The number the node's latest stream took.
    last_stream: Option<NonZeroU32>,
}

/// One isochronous stream a node's class driver started.
struct Stream<B> {
    endpoint: u8,
    number: NonZeroU32,
    region: B,
    notify: u64,
}

impl<B> Default for Interfaces<B> {
    fn default() -> Self {
        Self::new()
    }
}

impl<B> Interfaces<B> {
    /// A table with no transport opened.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            transports: Vec::new(),
        }
    }

    /// Whether a node built from `identity` serves `index`.
    fn covers(&self, index: usize, identity: &DeviceIdentity) -> bool {
        self.transports
            .iter()
            .filter_map(|transport| transport.node.as_ref())
            .any(|node| node.index == index && node.identity == *identity)
    }
}

impl<B: UrbBuffer> Interfaces<B> {
    /// True the published nodes up with the device table: keep each node
    /// whose device is still served, at the index now serving it; retract
    /// the rest; publish one for each served device no node covers.
    ///
    /// Retraction needs no memory and precedes every publication, so a
    /// device replaced at an index is never left bound to the old device's
    /// node. A device no transport or buffer can be had for stays
    /// unpublished until the next reconcile.
    pub fn reconcile<S: Seam<Buffer = B>>(&mut self, seam: &mut S) {
        self.reconcile_as(Matching::Same, seam);
    }

    /// [`Self::reconcile`], telling a node's device by `matching`.
    fn reconcile_as<S: Seam<Buffer = B>>(&mut self, matching: Matching, seam: &mut S) {
        self.follow_moved(matching, seam);
        for transport in &mut self.transports {
            if transport
                .node
                .as_ref()
                .is_some_and(|node| !node.is_served(matching, seam))
            {
                transport.retract(seam);
            }
        }
        for index in 0..seam.table_len() {
            let Some(identity) = seam.identity(index) else {
                continue;
            };
            if !self.covers(index, &identity) {
                self.publish(index, &identity, seam);
            }
        }
    }

    /// Point each node whose device a reset enumerated at another index at
    /// that index, never at one another node already serves.
    ///
    /// Only a controller reset moves a device, so a hot-plug reconcile finds
    /// every node in place and searches nothing.
    fn follow_moved<S: Seam>(&mut self, matching: Matching, seam: &S) {
        for slot in 0..self.transports.len() {
            let Some(identity) = self
                .transports
                .get(slot)
                .and_then(|transport| transport.node.as_ref())
                .filter(|node| !node.is_served(matching, seam))
                .map(|node| node.identity)
            else {
                continue;
            };
            let moved_to = (0..seam.table_len()).find(|&index| {
                seam.identity(index)
                    .is_some_and(|current| matching.keeps(&identity, &current))
                    && !self.covers(index, &identity)
            });
            let node = self
                .transports
                .get_mut(slot)
                .and_then(|transport| transport.node.as_mut());
            if let (Some(index), Some(node)) = (moved_to, node) {
                node.index = index;
            }
        }
    }

    /// Publish a node for the device `identity` names, served at `index`, on
    /// a free transport and a buffer of its own. Drained first: a call still
    /// queued on the endpoint can only be from a driver of a node it carried
    /// before.
    fn publish<S: Seam<Buffer = B>>(
        &mut self,
        index: usize,
        identity: &DeviceIdentity,
        seam: &mut S,
    ) {
        let Some(slot) = self.free_transport(seam) else {
            return;
        };
        let Some(transport) = self.transports.get_mut(slot) else {
            return;
        };
        refuse_queued(transport.endpoint, seam);
        let Some(buffer) = seam.create_buffer(URB_BUFFER_LEN) else {
            return;
        };
        let node = seam
            .describe(index)
            .and_then(|node| attach_transport_grants(node, transport.endpoint, buffer.region()));
        let Some(id) = node.ok().and_then(|node| seam.emit(&node)) else {
            return;
        };
        transport.node = Some(Node {
            id,
            identity: *identity,
            index,
            buffer,
            streams: Vec::new(),
            last_stream: None,
        });
        seam.note(Note::Published { node: id });
    }

    /// A watched transport carrying no node, opening one when none is free.
    ///
    /// One whose endpoint is bound but not watched is watched again rather
    /// than bound anew, since a bound endpoint stays bound.
    fn free_transport<S: Seam>(&mut self, seam: &mut S) -> Option<usize> {
        let free = self
            .transports
            .iter()
            .position(|transport| transport.node.is_none() && transport.watched)
            .or_else(|| {
                self.transports
                    .iter()
                    .position(|transport| transport.node.is_none())
            });
        let slot = if let Some(slot) = free {
            slot
        } else {
            self.transports.try_reserve(1).ok()?;
            let slot = self.transports.len();
            let endpoint = seam.open_endpoint(slot)?;
            self.transports.push(Transport::new(endpoint));
            slot
        };
        let transport = self.transports.get_mut(slot)?;
        if !transport.watched {
            transport.watched = seam.watch_endpoint(slot, transport.endpoint);
        }
        transport.watched.then_some(slot)
    }

    /// Serve the call that woke transport `slot`.
    ///
    /// A transfer or operation that ran synchronously parked on the
    /// controller's interrupt line and may have taken another transport's
    /// completion off it, so every held URB is driven, and every finished
    /// slot delivered, after one.
    pub fn serve_submit<S: Seam<Buffer = B>>(
        &mut self,
        slot: usize,
        health: &mut ControllerHealth,
        seam: &mut S,
    ) {
        let Some(transport) = self.transports.get_mut(slot) else {
            return;
        };
        let mut frame = [0u8; USB_REQUEST_MAX_LEN];
        let (ticket, len) = match seam.receive(transport.endpoint, &mut frame) {
            Ok(Some(call)) => call,
            // Its poster exited between the wake and the receive.
            Ok(None) => return,
            Err(errno) => {
                seam.note(Note::ReceiveFailed(errno));
                return;
            }
        };
        let request = match UsbRequest::decode(frame.get(..len).unwrap_or_default()) {
            Ok(request) => request,
            Err(errno) => {
                seam.reply(transport.endpoint, UrbReply::new(ticket, Err(errno)));
                return;
            }
        };
        let reach = match transport.node {
            None => Reach::Retracted,
            Some(_) if health.is_recovering() => Reach::Recovering,
            Some(_) => Reach::Served,
        };
        let reply = if let UsbRequest::Transfer(urb) = request {
            let (index, shm): (usize, &mut [u8]) = match transport.node.as_mut() {
                None => (0, &mut []),
                Some(node) => (node.index, node.buffer.bytes()),
            };
            match transport
                .service
                .on_submit(reach, ticket, &urb, shm, &mut seam.engine(index))
            {
                UrbOutcome::Reply(reply) => reply,
                UrbOutcome::Held | UrbOutcome::Idle => return,
            }
        } else {
            transport.operate(reach, ticket, request, seam)
        };
        seam.reply(transport.endpoint, reply);
        if reach == Reach::Served {
            self.drive_busy(health, seam);
            self.deliver_streams(seam);
        }
    }

    /// Tell every stream's class driver of each slot its stream finished,
    /// ending a stream that halted or whose notification could not be
    /// delivered.
    pub fn deliver_streams<S: Seam<Buffer = B>>(&mut self, seam: &mut S) {
        for transport in &mut self.transports {
            if let Some(node) = transport.node.as_mut() {
                node.deliver(seam);
            }
        }
    }

    /// Drive every held URB against what the controller has buffered, then
    /// recover the controller if a device's departure left it faulted.
    pub fn drive_busy<S: Seam<Buffer = B>>(&mut self, health: &mut ControllerHealth, seam: &mut S) {
        let mut detached = false;
        for slot in 0..self.transports.len() {
            let Some(transport) = self.transports.get_mut(slot) else {
                continue;
            };
            if !transport.service.is_busy() {
                continue;
            }
            let Some(node) = transport.node.as_mut() else {
                transport.answer_held(Errno::NotFound, seam);
                continue;
            };
            let index = node.index;
            let outcome = transport
                .service
                .on_event(node.buffer.bytes(), &mut seam.engine(index));
            let UrbOutcome::Reply(reply) = outcome else {
                continue;
            };
            let endpoint = transport.endpoint;
            let errno = reply.errno();
            if let Some(errno) = errno {
                seam.note(Note::UrbFailed { index, errno });
            }
            if errno == Some(Errno::DeviceFault)
                && self.fault_detach(slot, index, reply.ticket, seam)
            {
                detached = true;
                continue;
            }
            seam.reply(endpoint, reply);
        }
        if detached {
            self.recover(health, seam);
        }
    }

    /// Detach the device at `index` if its faulted URB, `ticket` on transport
    /// `slot`, was it leaving: retract every node it carried, answer the URB
    /// `NotFound`, and service the hub change it may already have posted,
    /// re-plug included. Returns whether it left.
    fn fault_detach<S: Seam<Buffer = B>>(
        &mut self,
        slot: usize,
        index: usize,
        ticket: u64,
        seam: &mut S,
    ) -> bool {
        match seam.detach_if_gone(index) {
            Ok(true) => {}
            Ok(false) => return false,
            Err(err) => {
                seam.note(Note::DetachUnconfirmed(err));
                return false;
            }
        }
        self.reconcile(seam);
        if let Some(transport) = self.transports.get(slot) {
            seam.reply(
                transport.endpoint,
                UrbReply::new(ticket, Err(Errno::NotFound)),
            );
        }
        seam.note(Note::FaultDetached);
        match seam.next_hub_change() {
            Ok(HubEvent::None) => {}
            Ok(_) => self.reconcile(seam),
            Err(err) => seam.note(Note::HubServiceFailed(err)),
        }
        true
    }

    /// Reset a controller that has faulted, or whose last reset left it
    /// faulted, returning whether an attempt ran.
    ///
    /// The nodes stay published through the reset, so a device that comes
    /// back keeps its node and its driver. A held URB is answered only once
    /// its device's fate is known — `NotFound` with its node, the reissuable
    /// `WouldBlock` where its device came back — since a resubmit prompted
    /// sooner could reach a device that replaced its own. A failed attempt
    /// keeps held report polls for the next, which the caller runs on
    /// [`ControllerHealth::wait_timeout`] because a faulted controller raises
    /// no interrupt (xHCI §4.24.1). Once the grace window has elapsed every
    /// node is retracted and the controller is not tried again.
    pub fn recover<S: Seam<Buffer = B>>(
        &mut self,
        health: &mut ControllerHealth,
        seam: &mut S,
    ) -> bool {
        if health.is_failed_closed() || !(health.is_recovering() || seam.faulted()) {
            return false;
        }
        let owner = health.owner();
        if let Some(event) = health.begin_recovery(seam.now_ns()) {
            seam.note(Note::Domain { event, owner });
        }
        let serving = seam.reset().is_ok() && !seam.faulted();
        if serving {
            // The reset took every stream with it; a node kept across it
            // governs its own interface alone again.
            for node in self.transports.iter_mut().filter_map(|t| t.node.as_mut()) {
                node.end_streams(Errno::WouldBlock, seam);
            }
            self.reconcile_as(Matching::Reenumerated, seam);
        }
        for transport in &mut self.transports {
            let outcome = if serving {
                transport.service.abort_outstanding(Errno::WouldBlock)
            } else {
                transport.service.reissue_held_transfer()
            };
            if let UrbOutcome::Reply(reply) = outcome {
                seam.reply(transport.endpoint, reply);
            }
        }
        if let Some(event) = health.note_reset(serving, seam.now_ns()) {
            seam.note(Note::Domain { event, owner });
        }
        if health.is_failed_closed() {
            self.retract_all(seam);
        }
        true
    }

    /// Retract every node, answering each one's URBs `NotFound`, as the HCD
    /// stops serving the controller.
    pub fn retract_all<S: Seam<Buffer = B>>(&mut self, seam: &mut S) {
        for transport in &mut self.transports {
            transport.retract(seam);
        }
    }
}

impl<B> Transport<B> {
    const fn new(endpoint: u64) -> Self {
        Self {
            endpoint,
            watched: false,
            service: UrbService::new(),
            node: None,
        }
    }

    fn answer_held<S: Seam>(&mut self, errno: Errno, seam: &mut S) {
        if let UrbOutcome::Reply(reply) = self.service.abort_outstanding(errno) {
            seam.reply(self.endpoint, reply);
        }
    }
}

impl<B: UrbBuffer> Transport<B> {
    /// Retract the node, then release the HCD's mapping of its buffer and
    /// answer its held URB `NotFound`, so a driver being unloaded is never
    /// left parked on a device that is gone. Its streams end first.
    fn retract<S: Seam<Buffer = B>>(&mut self, seam: &mut S) {
        if let Some(mut node) = self.node.take() {
            node.end_streams(Errno::NotFound, seam);
            seam.remove(node.id);
        }
        self.answer_held(Errno::NotFound, seam);
    }

    /// Perform an interface or stream operation for the node, answering at
    /// once.
    fn operate<S: Seam<Buffer = B>>(
        &mut self,
        reach: Reach,
        ticket: u64,
        request: UsbRequest,
        seam: &mut S,
    ) -> UrbReply {
        let endpoint = self.endpoint;
        let node = match (reach, self.node.as_mut()) {
            (Reach::Served, Some(node)) => node,
            (Reach::Recovering, Some(_)) => {
                return UrbReply::status(ticket, Err(Errno::WouldBlock));
            }
            _ => return UrbReply::status(ticket, Err(Errno::NotFound)),
        };
        let index = node.index;
        let result = match request {
            UsbRequest::SetInterface {
                interface,
                alternate,
            } => seam.engine(index).set_interface(interface, alternate),
            UsbRequest::ClaimInterface { interface } => {
                seam.engine(index).claim_interface(interface)
            }
            UsbRequest::IsoStart(params) => {
                return match node.start_stream(params, endpoint, ticket, seam) {
                    Ok(grant) => UrbReply::grant(ticket, &grant),
                    Err(errno) => UrbReply::status(ticket, Err(errno)),
                };
            }
            UsbRequest::IsoQueue { endpoint, slot } => node.queue(endpoint, slot, seam),
            UsbRequest::IsoStop { endpoint } => node.stop(endpoint, seam),
            UsbRequest::Transfer(_) => Err(DriverError::OutOfRange),
        };
        UrbReply::status(ticket, result.map_err(DriverError::as_errno))
    }
}

impl<B: UrbBuffer> Node<B> {
    /// Start a stream for the caller of `ticket` on transport `endpoint`: a
    /// region of its own, granted to that caller, its notifications sent to
    /// the port that caller's attested pid names.
    fn start_stream<S: Seam<Buffer = B>>(
        &mut self,
        params: IsoStartParams,
        endpoint: u64,
        ticket: u64,
        seam: &mut S,
    ) -> Result<IsoGrant, Errno> {
        let notify = iso_notify_endpoint_for(seam.caller_pid(endpoint, ticket)?, params.endpoint);
        self.streams
            .try_reserve(1)
            .map_err(|_| Errno::OutOfMemory)?;
        let mut region = seam
            .create_buffer(params.layout.region_len())
            .ok_or(Errno::OutOfMemory)?;
        if region.bytes().len() < params.layout.region_len() {
            return Err(Errno::OutOfMemory);
        }
        let shape = seam
            .engine(self.index)
            .iso_start(params.endpoint, params.layout)
            .map_err(DriverError::as_errno)?;
        // Whatever ran there before was replaced with the engine's stream.
        self.streams
            .retain(|stream| stream.endpoint != params.endpoint);
        let region_grant = match seam.grant_peer(region.region(), endpoint, ticket) {
            Ok(grant) => grant,
            Err(errno) => {
                let _ = seam.engine(self.index).iso_stop(params.endpoint);
                return Err(errno);
            }
        };
        let number = self.next_stream_number();
        self.streams.push(Stream {
            endpoint: params.endpoint,
            number,
            region,
            notify,
        });
        Ok(IsoGrant {
            region_grant,
            grantor: seam.self_instance(),
            notify,
            interval_microframes: shape.interval_microframes,
            speed: shape.speed,
            stream: number,
        })
    }

    /// The next stream's number: never zero, and never its predecessor's on
    /// any endpoint of the node.
    fn next_stream_number(&mut self) -> NonZeroU32 {
        let number = self
            .last_stream
            .and_then(|last| last.checked_add(1))
            .unwrap_or(NonZeroU32::MIN);
        self.last_stream = Some(number);
        number
    }

    /// Queue slot `slot` of the stream on `endpoint` from its region.
    fn queue<S: Seam<Buffer = B>>(
        &mut self,
        endpoint: u8,
        slot: u16,
        seam: &mut S,
    ) -> Result<(), DriverError> {
        let stream = self
            .streams
            .iter_mut()
            .find(|stream| stream.endpoint == endpoint)
            .ok_or(DriverError::NotFound)?;
        seam.engine(self.index)
            .iso_queue(endpoint, slot, stream.region.bytes())
    }

    /// Stop the stream on `endpoint` at its class driver's word.
    fn stop<S: Seam<Buffer = B>>(&mut self, endpoint: u8, seam: &mut S) -> Result<(), DriverError> {
        let at = self
            .streams
            .iter()
            .position(|stream| stream.endpoint == endpoint)
            .ok_or(DriverError::NotFound)?;
        self.streams.swap_remove(at);
        seam.engine(self.index).iso_stop(endpoint)
    }

    /// Notify each stream's finished slots, ending a stream that halted or
    /// cannot be told.
    fn deliver<S: Seam<Buffer = B>>(&mut self, seam: &mut S) {
        let index = self.index;
        let mut at = 0;
        while let Some(stream) = self.streams.get_mut(at) {
            let ended = loop {
                let taken = seam
                    .engine(index)
                    .iso_take(stream.endpoint, stream.region.bytes());
                match taken {
                    Ok(Some(done)) => {
                        let notice = IsoNotify::SlotDone {
                            endpoint: stream.endpoint,
                            stream: stream.number,
                            slot: done.slot,
                            skipped: done.skipped,
                            microframe: done.microframe,
                            completed_at: seam.now_ns(),
                        };
                        if let Err(errno) = seam.notify(stream.notify, &notice) {
                            break Some(errno);
                        }
                    }
                    Ok(None) => break None,
                    Err(err) => break Some(err.as_errno()),
                }
            };
            let Some(reason) = ended else {
                at += 1;
                continue;
            };
            let stream = self.streams.swap_remove(at);
            let _ = seam.engine(index).iso_stop(stream.endpoint);
            end_stream(index, &stream, reason, seam);
        }
    }

    /// End every stream, telling each class driver `reason`. The engine's
    /// streams are already gone with the device or the controller state.
    fn end_streams<S: Seam<Buffer = B>>(&mut self, reason: Errno, seam: &mut S) {
        for stream in core::mem::take(&mut self.streams) {
            end_stream(self.index, &stream, reason, seam);
        }
    }
}

/// Tell `stream`'s class driver it ended, as best the port still allows, and
/// record why.
fn end_stream<B, S: Seam<Buffer = B>>(
    index: usize,
    stream: &Stream<B>,
    reason: Errno,
    seam: &mut S,
) {
    let _ = seam.notify(
        stream.notify,
        &IsoNotify::Halted {
            endpoint: stream.endpoint,
            stream: stream.number,
            reason,
        },
    );
    seam.note(Note::StreamEnded {
        index,
        endpoint: stream.endpoint,
        reason,
    });
}

impl<B> Node<B> {
    /// Whether the device the node was built from is served where it points.
    fn is_served<S: Seam>(&self, matching: Matching, seam: &S) -> bool {
        seam.identity(self.index)
            .is_some_and(|current| matching.keeps(&self.identity, &current))
    }
}

/// How a reconcile tells that an index still serves a node's device.
#[derive(Clone, Copy)]
enum Matching {
    /// Within one enumeration: an equal identity is the same device.
    Same,
    /// Across a controller reset's re-enumeration, where a storage device
    /// must also prove itself by its serial number
    /// (`DeviceIdentity::recognises`).
    Reenumerated,
}

impl Matching {
    fn keeps(self, node: &DeviceIdentity, current: &DeviceIdentity) -> bool {
        match self {
            Self::Same => node == current,
            Self::Reenumerated => node.recognises(current),
        }
    }
}

/// Answer `NotFound` to every call queued on `endpoint`, which the endpoint's
/// depth bounds.
fn refuse_queued<S: Seam>(endpoint: u64, seam: &mut S) {
    let mut request = [0u8; USB_REQUEST_MAX_LEN];
    for _ in 0..ENDPOINT_CAPACITY {
        let Ok(Some((ticket, _))) = seam.receive(endpoint, &mut request) else {
            return;
        };
        seam.reply(endpoint, UrbReply::new(ticket, Err(Errno::NotFound)));
    }
}

#[cfg(test)]
#[path = "interfaces_tests.rs"]
mod tests;
