//! The interface nodes the HCD publishes and the URB transports they ride.
//!
//! Each served interface gets a node carrying a transport: a call endpoint
//! its class driver submits URBs on and a shared buffer the data moves
//! through. A node is kept while the device it was built from is served,
//! wherever the device table now places it. A node id is never
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
//! completions go to the notify port its attested pid names. A notice the port
//! refuses for room, this process's IPC budget or kernel memory is held, with
//! every notice after it, until the port's room wake or a timed retry, and a
//! stream holds no more completions than it has slots. A stream ends with its
//! device, with a controller reset or on a halt, its class driver owed why, or
//! once its port is gone or refuses it for good.

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::num::NonZeroU32;

use tairix_abi::ipc::{Reoffer, SendRefusal};
use tairix_abi::usb_urb::{
    iso_notify_endpoint_for, IsoGrant, IsoNotify, IsoStartParams, UsbRequest, UsbTransferType,
    USB_REQUEST_MAX_LEN,
};
use tairix_abi::{DriverError, Errno, HwNode, ProcId};
use tairix_usb::device::{DeviceIdentity, BULK_BUF_LEN};
use tairix_usb::transport::{on_own_endpoint, UrbEngine};

use crate::domain::{ControllerDomainEvent, ControllerHealth};
use crate::serve::{attach_transport_grants, Operation, Reach, UrbOutcome, UrbReply, UrbService};

/// Calls a transport endpoint's class driver may have queued before it is
/// served; only the class driver the endpoint was granted to can call it. It
/// submits one at a time and blocks on the reply, so a small depth absorbs a
/// resubmit racing the previous reply.
pub const ENDPOINT_DEPTH: usize = 4;

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
    /// A URB completed with an error, or was abandoned at its deadline.
    UrbFailed {
        /// The device-table index of the device it was for.
        index: usize,
        /// Its transfer type, which names the endpoint whose fault it was.
        transfer: UsbTransferType,
        /// What its class driver is answered.
        errno: Errno,
    },
    /// A faulted transfer was its device leaving: the device is detached and
    /// its nodes retracted.
    FaultDetached,
    /// Whether a faulting device is still attached could not be asked.
    DetachUnconfirmed(DriverError),
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
    /// Notices a stream's notify port refused for good were given up.
    NoticesLost {
        /// The port.
        port: u64,
        /// How many.
        lost: usize,
        /// The port's refusal.
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
    /// Have the port the device at `index` hangs off looked at again, so a
    /// device that left is torn down.
    ///
    /// # Errors
    ///
    /// The engine's refusal, such as no device served there.
    fn confirm_departure(&mut self, index: usize) -> Result<(), DriverError>;
    /// Whether the look [`Self::confirm_departure`] asked for at `index` is
    /// done.
    fn departure_settled(&self, index: usize) -> bool;
    /// Whether the controller has latched a fatal error or halted.
    fn faulted(&mut self) -> bool;
    /// Reset the controller and start enumerating it afresh, the walk's end
    /// reported by the engine's service.
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
    /// The kernel's refusal, which [`SendRefusal::of`] reads.
    fn notify(&mut self, port: u64, notice: &IsoNotify) -> Result<(), Errno>;
    /// Have the event loop woken by room at `port` — a receive there, or a
    /// release of this process's IPC budget — returning whether it took.
    fn watch_room(&mut self, port: u64) -> bool;
    /// Stop the event loop waking for room at `port`.
    fn unwatch_room(&mut self, port: u64);
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
    /// A controller reset's walk is running: the recovery ends with it.
    resetting: bool,
}

struct Transport<B> {
    endpoint: u64,
    /// Whether a submit on the endpoint wakes the event loop; a transport
    /// whose endpoint does not is never offered to a node.
    watched: bool,
    service: UrbService,
    node: Option<Node<B>>,
    /// A faulted URB's answer, held while the engine looks at whether its
    /// device left: answered [`Errno::NotFound`] if it did.
    confirming: Option<UrbReply>,
    /// What ended streams still owe their ports, kept past the stream and its
    /// node. Its capacity covers every stream the node runs, so a stream
    /// ending never allocates.
    ended: Vec<Notices>,
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
    /// Its region's slots: the most completions it holds unsent.
    slots: usize,
    notices: Notices,
}

/// What one notify port is owed, oldest first: the notice it refused and
/// those that came after.
///
/// Room for a stream's whole debt, a completion per slot and its halt
/// (`IsoLayout::notify_capacity`), is taken as the stream starts, so holding
/// a notice never allocates.
struct Notices {
    port: u64,
    held: VecDeque<IsoNotify>,
    /// When the oldest held notice is offered again.
    reoffer: Option<Reoffer>,
}

/// What offering a port its notices left.
#[derive(Clone, Copy)]
enum Offered {
    /// It took everything, or holds the rest for what refused it.
    Open,
    /// It is gone with its class driver, and what it was owed with it.
    Gone,
    /// It refused a notice for good, and the `lost` it was owed are given up.
    Refused { reason: Errno, lost: usize },
}

/// Why a stream ends.
#[derive(Clone, Copy)]
enum End {
    /// It halted, or its device or the controller took it: its class driver
    /// is owed the reason.
    Halted(Errno),
    /// Its port is gone with its class driver.
    Gone,
    /// Its port refuses its notices for good, and `lost` were given up.
    Refused { reason: Errno, lost: usize },
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
            resetting: false,
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

    /// Serve the call that woke transport `slot`: answered at once, or held
    /// until the engine finishes it.
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
        let outcome = if let UsbRequest::Transfer(urb) = request {
            let served = transport.node.as_ref().map(|node| node.index);
            let (index, shm): (usize, &mut [u8]) = match transport.node.as_mut() {
                None => (0, &mut []),
                Some(node) => (node.index, node.buffer.bytes()),
            };
            let now_ns = seam.now_ns();
            let outcome = transport.service.on_submit(
                reach,
                ticket,
                now_ns,
                &urb,
                shm,
                &mut seam.engine(index),
            );
            if let (UrbOutcome::Reply(reply), Some(index)) = (&outcome, served) {
                if let Some(errno) = reply.errno() {
                    seam.note(Note::UrbFailed {
                        index,
                        transfer: urb.transfer_type,
                        errno,
                    });
                }
            }
            outcome
        } else {
            transport.operate(reach, ticket, request, seam)
        };
        if let UrbOutcome::Reply(reply) = outcome {
            seam.reply(transport.endpoint, reply);
        }
    }

    /// Offer every stream's class driver what it is owed and, while the
    /// controller is served, each slot its stream finished, ending a stream
    /// that halted or whose class driver can no longer be told.
    ///
    /// A notice refused for room or this process's IPC budget waits on its
    /// port's room wake ([`Seam::watch_room`]), and one refused for kernel
    /// memory on a pause ([`Self::next_notice_retry`]). Run on every pass: a
    /// port watched for room wakes the loop until it is given what it is owed.
    pub fn deliver_streams<S: Seam<Buffer = B>>(
        &mut self,
        health: &ControllerHealth,
        seam: &mut S,
    ) {
        let serving = !(health.is_recovering() || health.is_failed_closed());
        for transport in &mut self.transports {
            transport.deliver(serving, seam);
        }
    }

    /// The soonest instant a held URB is abandoned at, if any is bounded.
    #[must_use]
    pub fn next_deadline(&self) -> Option<u64> {
        self.transports
            .iter()
            .filter_map(|transport| transport.service.deadline())
            .min()
    }

    /// The soonest instant a held notice is offered again after a pause, if
    /// one waits on a pause rather than its port's room wake.
    #[must_use]
    pub fn next_notice_retry(&self) -> Option<u64> {
        self.transports
            .iter()
            .flat_map(Transport::notices)
            .filter_map(Notices::retry_at)
            .min()
    }

    /// Answer every held URB whose deadline has passed at `now_ns`
    /// [`Errno::TimedOut`], abandoning its transfer so it can never complete
    /// late into a later one. Every held URB is driven first, so one the
    /// controller finished in time is delivered rather than abandoned. A
    /// recovering controller is neither driven nor asked to abandon anything:
    /// its reset discards what the URBs had queued.
    pub fn expire_overdue<S: Seam<Buffer = B>>(
        &mut self,
        now_ns: u64,
        health: &ControllerHealth,
        seam: &mut S,
    ) {
        let serving = !(health.is_recovering() || health.is_failed_closed());
        if serving {
            self.drive_busy(seam);
        }
        for transport in &mut self.transports {
            let Some(node) = transport.node.as_ref() else {
                continue;
            };
            let index = node.index;
            let Some(transfer) = transport.service.held_transfer() else {
                continue;
            };
            let outcome = if serving {
                transport.service.expire(now_ns, &mut seam.engine(index))
            } else {
                transport.service.expire_unserved(now_ns)
            };
            let UrbOutcome::Reply(reply) = outcome else {
                continue;
            };
            if let Some(errno) = reply.errno() {
                seam.note(Note::UrbFailed {
                    index,
                    transfer,
                    errno,
                });
            }
            seam.reply(transport.endpoint, reply);
        }
    }

    /// Drive every held call against what the controller has done. A URB
    /// that faulted is held while the engine looks at whether its device
    /// left, and answered once it knows: [`Errno::NotFound`] with the node it
    /// retracts if the device left, its own fault otherwise.
    pub fn drive_busy<S: Seam<Buffer = B>>(&mut self, seam: &mut S) {
        for transport in &mut self.transports {
            transport.settle_departure(seam);
            if !transport.service.is_busy() {
                continue;
            }
            let Some(node) = transport.node.as_mut() else {
                transport.answer_held(Errno::NotFound, seam);
                continue;
            };
            let index = node.index;
            let transfer = transport.service.held_transfer();
            let outcome = transport
                .service
                .on_event(node.buffer.bytes(), &mut seam.engine(index));
            let UrbOutcome::Reply(reply) = outcome else {
                continue;
            };
            let errno = reply.errno();
            if let (Some(errno), Some(transfer)) = (errno, transfer) {
                seam.note(Note::UrbFailed {
                    index,
                    transfer,
                    errno,
                });
            }
            if transfer.is_some() && errno == Some(Errno::DeviceFault) {
                match seam.confirm_departure(index) {
                    Ok(()) => {
                        transport.confirming = Some(reply);
                        continue;
                    }
                    Err(err) => seam.note(Note::DetachUnconfirmed(err)),
                }
            }
            seam.reply(transport.endpoint, reply);
        }
    }

    /// Reset a controller that has faulted, or whose last reset did not
    /// bring it back, returning whether an attempt started. The attempt ends
    /// with its walk ([`Self::finish_recovery`]); while it runs nothing is
    /// driven, and every call is held.
    ///
    /// A faulted controller raises no interrupt (xHCI §4.24.1), so the caller
    /// runs a failed attempt's successor on [`ControllerHealth::wait_timeout`].
    /// Once the grace window has elapsed every node is retracted and the
    /// controller is not tried again.
    pub fn recover<S: Seam<Buffer = B>>(
        &mut self,
        health: &mut ControllerHealth,
        seam: &mut S,
    ) -> bool {
        if self.resetting
            || health.is_failed_closed()
            || !(health.is_recovering() || seam.faulted())
        {
            return false;
        }
        let owner = health.owner();
        if let Some(event) = health.begin_recovery(seam.now_ns()) {
            seam.note(Note::Domain { event, owner });
        }
        if seam.reset().is_ok() {
            self.resetting = true;
        } else {
            self.note_attempt(false, health, seam);
        }
        true
    }

    /// End the recovery whose walk has just finished. The nodes stayed
    /// published through it, so a device that came back keeps its node and
    /// its driver; a held call is answered only now its device's fate is
    /// known — `NotFound` with its node, the reissuable `WouldBlock` where
    /// the engine had been asked for it, and the rest driven afresh — since a
    /// resubmit prompted sooner could reach a device that replaced its own.
    pub fn finish_recovery<S: Seam<Buffer = B>>(
        &mut self,
        health: &mut ControllerHealth,
        seam: &mut S,
    ) {
        if !core::mem::take(&mut self.resetting) {
            return;
        }
        let serving = !seam.faulted();
        if serving {
            // The reset took every stream with it; a node kept across it
            // governs its own interface alone again.
            for transport in &mut self.transports {
                if let Some(node) = transport.node.as_mut() {
                    node.end_streams(Errno::WouldBlock, &mut transport.ended, seam);
                }
            }
            self.reconcile_as(Matching::Reenumerated, seam);
            for transport in &mut self.transports {
                if let Some(reply) = transport.confirming.take() {
                    seam.reply(transport.endpoint, reply);
                }
                if let UrbOutcome::Reply(reply) = transport.service.reissue_after_reset() {
                    seam.reply(transport.endpoint, reply);
                }
            }
        }
        self.note_attempt(serving, health, seam);
        if serving {
            self.drive_busy(seam);
        }
    }

    /// Whether a controller reset's walk is running.
    #[must_use]
    pub const fn is_resetting(&self) -> bool {
        self.resetting
    }

    fn note_attempt<S: Seam<Buffer = B>>(
        &mut self,
        serving: bool,
        health: &mut ControllerHealth,
        seam: &mut S,
    ) {
        if let Some(event) = health.note_reset(serving, seam.now_ns()) {
            seam.note(Note::Domain {
                event,
                owner: health.owner(),
            });
        }
        if health.is_failed_closed() {
            self.retract_all(seam);
        }
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
            confirming: None,
            ended: Vec::new(),
        }
    }

    /// Every port debt the transport holds: its node's streams' and the
    /// ended ones'.
    fn notices(&self) -> impl Iterator<Item = &Notices> + '_ {
        self.node
            .iter()
            .flat_map(|node| node.streams.iter().map(|stream| &stream.notices))
            .chain(&self.ended)
    }

    fn answer_held<S: Seam>(&mut self, errno: Errno, seam: &mut S) {
        if let UrbOutcome::Reply(reply) = self.service.abort_outstanding(errno) {
            seam.reply(self.endpoint, reply);
        }
    }
}

impl<B: UrbBuffer> Transport<B> {
    /// Settle the faulted URB a departure check holds once the engine has
    /// looked: its own fault if its device is still served; if it left, the
    /// node goes and the URB is answered `NotFound`.
    fn settle_departure<S: Seam<Buffer = B>>(&mut self, seam: &mut S) {
        let (Some(node), Some(faulted)) = (self.node.as_ref(), self.confirming) else {
            return;
        };
        let served = seam
            .identity(node.index)
            .is_some_and(|current| current == node.identity);
        if !served {
            self.retract(seam);
        } else if seam.departure_settled(node.index) {
            self.confirming = None;
            seam.reply(self.endpoint, faulted);
        }
    }

    /// Retract the node, then release the HCD's mapping of its buffer and
    /// answer its held calls `NotFound`, so a driver being unloaded is never
    /// left parked on a device that is gone. Its streams end first.
    fn retract<S: Seam<Buffer = B>>(&mut self, seam: &mut S) {
        if let Some(mut node) = self.node.take() {
            node.end_streams(Errno::NotFound, &mut self.ended, seam);
            seam.remove(node.id);
        }
        if let Some(faulted) = self.confirming.take() {
            seam.reply(
                self.endpoint,
                UrbReply::new(faulted.ticket, Err(Errno::NotFound)),
            );
            seam.note(Note::FaultDetached);
        }
        self.answer_held(Errno::NotFound, seam);
    }

    /// Offer what the ended streams and the node's own owe their ports and,
    /// while the controller is served, every slot the node's streams finished.
    fn deliver<S: Seam<Buffer = B>>(&mut self, serving: bool, seam: &mut S) {
        self.ended.retain_mut(|notices| match notices.flush(seam) {
            Offered::Open => notices.owes(),
            Offered::Gone => false,
            Offered::Refused { reason, lost } => {
                seam.note(Note::NoticesLost {
                    port: notices.port,
                    lost,
                    reason,
                });
                false
            }
        });
        if let Some(node) = self.node.as_mut() {
            node.deliver(serving, &mut self.ended, seam);
        }
    }

    /// Perform an interface or stream operation for the node: one the engine
    /// runs over several commands is held as a URB is, the rest answered at
    /// once. While the controller recovers every operation is held, and
    /// answered once it is back.
    fn operate<S: Seam<Buffer = B>>(
        &mut self,
        reach: Reach,
        ticket: u64,
        request: UsbRequest,
        seam: &mut S,
    ) -> UrbOutcome {
        let Some(node) = self.node.as_mut().filter(|_| reach != Reach::Retracted) else {
            return UrbOutcome::Reply(UrbReply::status(ticket, Err(Errno::NotFound)));
        };
        let index = node.index;
        let operation = match request {
            UsbRequest::SetInterface {
                interface,
                alternate,
            } => Operation::SetInterface {
                interface,
                alternate,
            },
            UsbRequest::ClearHalt { endpoint } => Operation::ClearHalt { endpoint },
            UsbRequest::IsoStop { endpoint } if reach == Reach::Served => {
                if let Err(err) = node.forget_stream(endpoint, seam) {
                    return UrbOutcome::Reply(UrbReply::status(ticket, Err(err.as_errno())));
                }
                Operation::IsoStop { endpoint }
            }
            _ if reach == Reach::Recovering => return self.service.on_deferred(ticket),
            other => return UrbOutcome::Reply(self.answer_at_once(ticket, other, seam)),
        };
        self.service
            .on_operation(reach, ticket, operation, &mut seam.engine(index))
    }

    /// Carry out an operation that needs nothing the engine runs.
    fn answer_at_once<S: Seam<Buffer = B>>(
        &mut self,
        ticket: u64,
        request: UsbRequest,
        seam: &mut S,
    ) -> UrbReply {
        let endpoint = self.endpoint;
        let Some(node) = self.node.as_mut() else {
            return UrbReply::status(ticket, Err(Errno::NotFound));
        };
        let index = node.index;
        let result = match request {
            UsbRequest::ClaimInterface { interface } => {
                seam.engine(index).claim_interface(interface)
            }
            UsbRequest::IsoStart(params) => {
                return match node.start_stream(params, endpoint, ticket, &mut self.ended, seam) {
                    Ok(grant) => UrbReply::grant(ticket, &grant),
                    Err(errno) => UrbReply::status(ticket, Err(errno)),
                };
            }
            UsbRequest::IsoQueue { endpoint, slot } => node.queue(endpoint, slot, seam),
            // Discarding its reports under a held report poll would starve
            // it; a class driver holds one call at a time, so this is no
            // driver's own call.
            UsbRequest::DiscardReports { .. } if self.service.is_busy() => {
                return UrbReply::status(ticket, Err(Errno::AlreadyExists));
            }
            UsbRequest::DiscardReports { endpoint } => {
                let engine = &mut seam.engine(index);
                let discarded = on_own_endpoint(endpoint, engine, UrbEngine::discard_reports);
                return UrbReply::status(ticket, discarded);
            }
            UsbRequest::Transfer(_)
            | UsbRequest::SetInterface { .. }
            | UsbRequest::ClearHalt { .. }
            | UsbRequest::IsoStop { .. } => Err(DriverError::OutOfRange),
        };
        UrbReply::status(ticket, result.map_err(DriverError::as_errno))
    }
}

impl<B: UrbBuffer> Node<B> {
    /// Start a stream for the caller of `ticket` on transport `endpoint`: a
    /// region of its own, granted to that caller, its notifications sent to
    /// the port that caller's attested pid names. Room for all it can come to
    /// owe, there and once ended in `ended`, is taken first, so no notice it
    /// owes is lost to this process's memory.
    fn start_stream<S: Seam<Buffer = B>>(
        &mut self,
        params: IsoStartParams,
        endpoint: u64,
        ticket: u64,
        ended: &mut Vec<Notices>,
        seam: &mut S,
    ) -> Result<IsoGrant, Errno> {
        let notify = iso_notify_endpoint_for(seam.caller_pid(endpoint, ticket)?, params.endpoint);
        self.streams
            .try_reserve(1)
            .map_err(|_| Errno::OutOfMemory)?;
        ended
            .try_reserve(self.streams.len() + 1)
            .map_err(|_| Errno::OutOfMemory)?;
        let notices = Notices::new(notify, params.layout.notify_capacity())?;
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
        // Whatever ran there before was replaced with the engine's stream, and
        // what it or an ended predecessor still owes the port is of a stream
        // its class driver no longer holds.
        self.streams.retain_mut(|stream| {
            let replaced = stream.endpoint == params.endpoint;
            if replaced {
                stream.notices.release(seam);
            }
            !replaced
        });
        ended.retain_mut(|owed| {
            let superseded = owed.port == notify;
            if superseded {
                owed.release(seam);
            }
            !superseded
        });
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
            slots: usize::from(params.layout.slots),
            notices,
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

    /// Forget the stream on `endpoint`, which its class driver is stopping,
    /// with what its port is still owed: notices of a stream its class driver
    /// no longer holds.
    fn forget_stream<S: Seam>(&mut self, endpoint: u8, seam: &mut S) -> Result<(), DriverError> {
        let at = self
            .streams
            .iter()
            .position(|stream| stream.endpoint == endpoint)
            .ok_or(DriverError::NotFound)?;
        let mut stream = self.streams.swap_remove(at);
        stream.notices.release(seam);
        Ok(())
    }

    /// Offer each stream's class driver what it is owed and, while the
    /// controller is served, every slot its stream finished, ending a stream
    /// that halted or whose class driver can no longer be told; what an ended
    /// stream still owes is kept in `ended`.
    fn deliver<S: Seam<Buffer = B>>(
        &mut self,
        serving: bool,
        ended: &mut Vec<Notices>,
        seam: &mut S,
    ) {
        let index = self.index;
        let mut at = 0;
        while let Some(stream) = self.streams.get_mut(at) {
            let Some(end) = stream.deliver(index, serving, seam) else {
                at += 1;
                continue;
            };
            let stream = self.streams.swap_remove(at);
            // A recovering controller is not driven; the reset ending its
            // recovery takes the stream.
            if serving {
                let _ = seam.engine(index).iso_stop(stream.endpoint);
            }
            end_stream(index, stream, end, ended, seam);
        }
    }

    /// End every stream, owing each class driver `reason`. The engine's
    /// streams are already gone with the device or the controller state.
    fn end_streams<S: Seam<Buffer = B>>(
        &mut self,
        reason: Errno,
        ended: &mut Vec<Notices>,
        seam: &mut S,
    ) {
        for stream in core::mem::take(&mut self.streams) {
            end_stream(self.index, stream, End::Halted(reason), ended, seam);
        }
    }
}

impl<B: UrbBuffer> Stream<B> {
    /// Offer the class driver what it is owed and, while the controller is
    /// served, each slot the stream has since finished, returning why the
    /// stream ends if it does.
    ///
    /// No more completions are held than the stream has slots: the rest stay
    /// finished in the engine, where none can be queued again, so a class
    /// driver that stops reading holds the stream at its own depth.
    fn deliver<S: Seam<Buffer = B>>(
        &mut self,
        index: usize,
        serving: bool,
        seam: &mut S,
    ) -> Option<End> {
        let mut offered = self.notices.flush(seam);
        loop {
            match offered {
                Offered::Open => {}
                Offered::Gone => return Some(End::Gone),
                Offered::Refused { reason, lost } => return Some(End::Refused { reason, lost }),
            }
            if !serving || self.notices.held.len() >= self.slots {
                return None;
            }
            let taken = seam
                .engine(index)
                .iso_take(self.endpoint, self.region.bytes());
            let done = match taken {
                Ok(Some(done)) => done,
                Ok(None) => return None,
                Err(err) => return Some(End::Halted(err.as_errno())),
            };
            let completed = IsoNotify::SlotDone {
                endpoint: self.endpoint,
                stream: self.number,
                slot: done.slot,
                skipped: done.skipped,
                microframe: done.microframe,
                completed_at: seam.now_ns(),
            };
            offered = self.notices.post(completed, seam);
        }
    }
}

/// End `stream`, owing its class driver why while its port remains, and
/// record it. What the port is still owed is kept in `ended`, whose room the
/// stream's start took.
fn end_stream<B, S: Seam>(
    index: usize,
    stream: Stream<B>,
    end: End,
    ended: &mut Vec<Notices>,
    seam: &mut S,
) {
    let Stream {
        endpoint,
        number,
        mut notices,
        ..
    } = stream;
    let (why, told) = match end {
        End::Halted(reason) => {
            let halted = IsoNotify::Halted {
                endpoint,
                stream: number,
                reason,
            };
            (reason, notices.post(halted, seam))
        }
        End::Gone => (Errno::NotFound, Offered::Gone),
        End::Refused { reason, lost } => (reason, Offered::Refused { reason, lost }),
    };
    if let Offered::Refused { reason, lost } = told {
        seam.note(Note::NoticesLost {
            port: notices.port,
            lost,
            reason,
        });
    }
    seam.note(Note::StreamEnded {
        index,
        endpoint,
        reason: why,
    });
    if notices.owes() {
        ended.push(notices);
    }
}

impl Notices {
    /// Notices for `port`, with room for `capacity` of them.
    fn new(port: u64, capacity: usize) -> Result<Self, Errno> {
        let mut held = VecDeque::new();
        held.try_reserve_exact(capacity)
            .map_err(|_| Errno::OutOfMemory)?;
        Ok(Self {
            port,
            held,
            reoffer: None,
        })
    }

    /// Whether the port is owed anything.
    fn owes(&self) -> bool {
        !self.held.is_empty()
    }

    /// When what is held is next offered, if a pause decides it.
    fn retry_at(&self) -> Option<u64> {
        self.reoffer.and_then(Reoffer::deadline)
    }

    /// Owe `notice`, offering it at once unless something held must reach
    /// the port first.
    fn post<S: Seam>(&mut self, notice: IsoNotify, seam: &mut S) -> Offered {
        self.held.push_back(notice);
        if self.reoffer.is_some() {
            return Offered::Open;
        }
        self.flush(seam)
    }

    /// Offer what is held, oldest first, until the port refuses one. A pause
    /// is waited out; a room wait is offered on every pass, since its member
    /// wakes the loop only once the port has room.
    fn flush<S: Seam>(&mut self, seam: &mut S) -> Offered {
        if self.retry_at().is_some_and(|at| seam.now_ns() < at) {
            return Offered::Open;
        }
        while let Some(notice) = self.held.front() {
            match seam.notify(self.port, notice) {
                Ok(()) => {
                    self.held.pop_front();
                }
                Err(errno) => return self.refused(errno, seam),
            }
        }
        self.wait_on(None, seam);
        Offered::Open
    }

    /// Hold what is owed for what refused the oldest of it, or give it up.
    fn refused<S: Seam>(&mut self, errno: Errno, seam: &mut S) -> Offered {
        match SendRefusal::of(errno) {
            SendRefusal::Full | SendRefusal::Budget => {
                if self.reoffer != Some(Reoffer::OnRoom) {
                    let reoffer = if seam.watch_room(self.port) {
                        Reoffer::OnRoom
                    } else {
                        Reoffer::pause(seam.now_ns())
                    };
                    self.reoffer = Some(reoffer);
                }
                Offered::Open
            }
            SendRefusal::Memory => {
                self.wait_on(Some(Reoffer::pause(seam.now_ns())), seam);
                Offered::Open
            }
            SendRefusal::Gone => {
                self.release(seam);
                Offered::Gone
            }
            SendRefusal::Final(reason) => {
                let lost = self.held.len();
                self.release(seam);
                Offered::Refused { reason, lost }
            }
        }
    }

    /// Give up everything held.
    fn release<S: Seam>(&mut self, seam: &mut S) {
        self.held.clear();
        self.wait_on(None, seam);
    }

    /// Be offered again at `reoffer`, unwatching the port's room once that no
    /// longer decides: its member is ready while the port has room, so one
    /// left armed would wake the loop for ever.
    fn wait_on<S: Seam>(&mut self, reoffer: Option<Reoffer>, seam: &mut S) {
        if self.reoffer == Some(Reoffer::OnRoom) && reoffer != Some(Reoffer::OnRoom) {
            seam.unwatch_room(self.port);
        }
        self.reoffer = reoffer;
    }
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

/// Answer `NotFound` to every call queued on `endpoint`: only its granted
/// class driver calls it, so its per-caller depth bounds them.
fn refuse_queued<S: Seam>(endpoint: u64, seam: &mut S) {
    let mut request = [0u8; USB_REQUEST_MAX_LEN];
    for _ in 0..ENDPOINT_DEPTH {
        let Ok(Some((ticket, _))) = seam.receive(endpoint, &mut request) else {
            return;
        };
        seam.reply(endpoint, UrbReply::new(ticket, Err(Errno::NotFound)));
    }
}

#[cfg(test)]
#[path = "interfaces_tests.rs"]
mod tests;
