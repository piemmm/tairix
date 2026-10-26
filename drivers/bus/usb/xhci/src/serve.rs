//! The HCD's per-interface URB-service state machine and interface-node
//! builder (`plans/USB.md` §1.1, §1.3 — the asynchronous event loop).
//!
//! The host-controller driver serves one URB transport call endpoint per USB
//! interface it emits. A class driver submits an interrupt-IN URB (a blocking
//! `ipc_call`) to read the next report; the HCD does **not** reply until the
//! controller's completion interrupt delivers that report, so the class driver
//! parks in the kernel rather than busy-polling (the charter forbids spinning
//! a core). [`UrbService`] is the per-interface state that makes this work: it
//! holds at most one outstanding URB and drives it on the controller event.
//!
//! The data path is the U3a2 shared-memory buffer: the report bytes the
//! controller wrote into the HCD's own DMA ring are copied into the shared
//! buffer (the `shm` slice here — the HCD's mapping of the region the class
//! driver also maps) by the engine's [`interrupt_in`](UrbEngine::interrupt_in),
//! and the class driver reads them from its own mapping. The class driver
//! holds no DMA grant.
//!
//! This module is pure and alloc-free, so it is proven host-side over a mock
//! [`UrbEngine`]; the live wait-set loop that drives it is in `main.rs` and is
//! the on-metal acceptance item (QEMU models no Pi USB).

use core::num::NonZeroU32;

use tairix_abi::reply::encode_status_reply;
use tairix_abi::usb_urb::{
    decode_completion, IsoGrant, UrbRequest, UsbTransferType, USB_CONTROL_TIMEOUT_MAX_MS,
    USB_REPLY_MAX_LEN,
};
use tairix_abi::{DriverError, Errno, HwNode, HwResource};
use tairix_log::Level;
use tairix_usb::transport::{drive_urb, frame_completion, on_own_endpoint, UrbDone, UrbEngine};

/// Whether a URB submitted on an interface's transport can reach its device.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Reach {
    /// The interface's device is served: the URB is driven.
    Served,
    /// The interface is published but its controller is recovering, so no
    /// transfer can run until a reset brings the controller back.
    Recovering,
    /// No interface node is published on the transport.
    Retracted,
}

/// A framed reply ready for `call_reply`: a URB completion, a status, or a
/// stream's grant, paired with the ticket it answers.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct UrbReply {
    /// The in-service call ticket this reply answers.
    pub ticket: u64,
    /// The framed bytes.
    pub bytes: [u8; USB_REPLY_MAX_LEN],
    /// The number of valid bytes in [`Self::bytes`].
    pub len: usize,
}

impl UrbReply {
    /// A URB's completion, `result`, framed for `ticket`.
    ///
    /// Framing into a [`USB_REPLY_MAX_LEN`] buffer cannot fail, since the
    /// buffer outsizes every completion; a failure would frame an empty
    /// reply, which the caller decodes as malformed rather than as a success.
    pub(crate) fn new(ticket: u64, result: Result<UrbDone, Errno>) -> Self {
        let mut bytes = [0u8; USB_REPLY_MAX_LEN];
        let len = frame_completion(&mut bytes, result).unwrap_or(0);
        Self { ticket, bytes, len }
    }

    /// An interface or stream operation's outcome, framed for `ticket`.
    pub(crate) fn status(ticket: u64, result: Result<(), Errno>) -> Self {
        Self::framed(ticket, &encode_status_reply(result))
    }

    /// A started stream's grant, framed for `ticket`.
    pub(crate) fn grant(ticket: u64, grant: &IsoGrant) -> Self {
        Self::framed(ticket, &grant.encode())
    }

    fn framed(ticket: u64, frame: &[u8]) -> Self {
        let mut bytes = [0u8; USB_REPLY_MAX_LEN];
        let len = frame.len().min(USB_REPLY_MAX_LEN);
        bytes[..len].copy_from_slice(&frame[..len]);
        Self { ticket, bytes, len }
    }

    /// The error a URB completion carries, if it carries one.
    pub(crate) fn errno(&self) -> Option<Errno> {
        decode_completion(self.bytes.get(..self.len).unwrap_or_default()).err()
    }
}

/// What the HCD does after servicing one wait-set wake-up.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UrbOutcome {
    /// Reply now to the named ticket with the framed completion.
    Reply(UrbReply),
    /// The submitted URB is held outstanding; no reply is sent until a later
    /// controller event completes it.
    Held,
    /// A controller event arrived but no URB is outstanding — nothing to do.
    Idle,
}

/// One USB interface's URB-service state: at most one outstanding URB or
/// interface operation.
///
/// The class driver submits one call at a time (it blocks on the reply), so a
/// single outstanding slot is the whole protocol. A second submit arriving
/// while one is already outstanding is a class-driver protocol violation; it
/// is answered fail-closed with [`Errno::AlreadyExists`] and never displaces
/// the one in flight (a hostile class driver cannot steal another submit's
/// completion).
pub struct UrbService {
    /// The call in flight, re-driven on each controller event. `None` when
    /// idle.
    outstanding: Option<Held>,
}

/// An interface operation the engine carries out over several commands, held
/// as a URB is.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Operation {
    /// Select `alternate` on `interface`.
    SetInterface {
        /// The interface.
        interface: u8,
        /// The setting.
        alternate: u8,
    },
    /// Clear the halt on one of the interface's own endpoints.
    ClearHalt {
        /// The endpoint address.
        endpoint: u8,
    },
    /// Stop the stream on an endpoint.
    IsoStop {
        /// The endpoint address.
        endpoint: u8,
    },
}

/// A call held for a later controller event.
#[derive(Copy, Clone, Debug)]
struct Held {
    /// The `call_recv` ticket its reply answers.
    ticket: u64,
    work: Work,
    /// The monotonic instant it is abandoned at, when it is bounded.
    deadline_ns: Option<u64>,
    /// Whether the engine has been asked for it. One asked before a
    /// controller reset is gone with the reset.
    driven: bool,
}

/// What a held call carries out.
#[derive(Copy, Clone, Debug)]
enum Work {
    Transfer(UrbRequest),
    Operation(Operation),
    /// An operation that needs no engine work, received while the
    /// controller recovers: answered reissuable once it is back.
    Deferred,
}

impl Held {
    /// Its answer, framed as its kind is: a completion for a transfer, a
    /// status for an operation.
    fn reply(&self, result: Result<UrbDone, Errno>) -> UrbReply {
        match self.work {
            Work::Transfer(_) => UrbReply::new(self.ticket, result),
            Work::Operation(_) | Work::Deferred => {
                UrbReply::status(self.ticket, result.map(|_| ()))
            }
        }
    }

    /// Ask `engine` for it, recording that it was asked.
    fn drive<E: UrbEngine>(
        &mut self,
        shm: &mut [u8],
        engine: &mut E,
    ) -> Result<Option<UrbDone>, Errno> {
        self.driven = true;
        match self.work {
            Work::Transfer(urb) => drive_urb(&urb, shm, engine),
            Work::Operation(operation) => {
                drive_operation(operation, engine).map(|done| done.map(|()| UrbDone::Moved(0)))
            }
            Work::Deferred => Err(Errno::WouldBlock),
        }
    }
}

/// Carry `operation` out through `engine`: `None` while it runs.
fn drive_operation<E: UrbEngine>(
    operation: Operation,
    engine: &mut E,
) -> Result<Option<()>, Errno> {
    match operation {
        Operation::SetInterface {
            interface,
            alternate,
        } => engine
            .set_interface(interface, alternate)
            .map_err(DriverError::as_errno),
        Operation::ClearHalt { endpoint } => {
            on_own_endpoint(endpoint, engine, UrbEngine::clear_halt)
        }
        Operation::IsoStop { endpoint } => engine.iso_stop(endpoint).map_err(DriverError::as_errno),
    }
}

/// When a URB received at `now_ns` is abandoned: at its own deadline, a
/// control transfer's never past the protocol's ceiling on a request.
fn deadline_of(urb: &UrbRequest, now_ns: u64) -> Option<u64> {
    let ms = match urb.transfer_type {
        UsbTransferType::Control => Some(urb.timeout_ms.map_or(USB_CONTROL_TIMEOUT_MAX_MS, |ms| {
            ms.get().min(USB_CONTROL_TIMEOUT_MAX_MS)
        })),
        UsbTransferType::Bulk | UsbTransferType::Interrupt => urb.timeout_ms.map(NonZeroU32::get),
    };
    ms.map(|ms| now_ns.saturating_add(u64::from(ms) * 1_000_000))
}

impl Default for UrbService {
    fn default() -> Self {
        Self::new()
    }
}

impl UrbService {
    /// A service with no URB outstanding.
    #[must_use]
    pub const fn new() -> Self {
        Self { outstanding: None }
    }

    /// Whether a URB is currently in flight (a controller event will drive
    /// it).
    #[must_use]
    pub const fn is_busy(&self) -> bool {
        self.outstanding.is_some()
    }

    /// The instant the held URB is abandoned at, if one is held and bounded.
    #[must_use]
    pub fn deadline(&self) -> Option<u64> {
        self.outstanding.and_then(|held| held.deadline_ns)
    }

    /// The transfer type of the held URB, if a URB is held.
    #[must_use]
    pub fn held_transfer(&self) -> Option<UsbTransferType> {
        self.outstanding.and_then(|held| match held.work {
            Work::Transfer(urb) => Some(urb.transfer_type),
            Work::Operation(_) | Work::Deferred => None,
        })
    }

    /// Service a freshly received URB (`ticket` + its decoded `urb`) over the
    /// shared `shm` buffer and the controller `engine`, at `now_ns`.
    ///
    /// A [`Reach::Served`] URB is driven once: replied at once when it
    /// completes synchronously, [`UrbOutcome::Held`] — until its own deadline
    /// at most — when it has not. A [`Reach::Recovering`] URB is held without
    /// touching the controller, since answering it would have its class
    /// driver submit again at once for as long as the recovery lasts. A
    /// [`Reach::Retracted`] submit is refused [`Errno::NotFound`], and one
    /// arriving while a call is outstanding [`Errno::AlreadyExists`], leaving
    /// the held call alone.
    pub fn on_submit<E: UrbEngine>(
        &mut self,
        reach: Reach,
        ticket: u64,
        now_ns: u64,
        urb: &UrbRequest,
        shm: &mut [u8],
        engine: &mut E,
    ) -> UrbOutcome {
        let held = Held {
            ticket,
            work: Work::Transfer(*urb),
            deadline_ns: deadline_of(urb, now_ns),
            driven: false,
        };
        self.accept(reach, held, shm, engine)
    }

    /// Service a freshly received interface `operation` for `ticket`, as
    /// [`Self::on_submit`] a URB: carried out at once when it can be, held
    /// while the engine runs it.
    pub fn on_operation<E: UrbEngine>(
        &mut self,
        reach: Reach,
        ticket: u64,
        operation: Operation,
        engine: &mut E,
    ) -> UrbOutcome {
        let held = Held {
            ticket,
            work: Work::Operation(operation),
            deadline_ns: None,
            driven: false,
        };
        self.accept(reach, held, &mut [], engine)
    }

    /// Hold an operation that needs no engine work, received for `ticket`
    /// while the controller recovers.
    pub fn on_deferred(&mut self, ticket: u64) -> UrbOutcome {
        let held = Held {
            ticket,
            work: Work::Deferred,
            deadline_ns: None,
            driven: false,
        };
        if self.outstanding.is_some() {
            return UrbOutcome::Reply(held.reply(Err(Errno::AlreadyExists)));
        }
        self.outstanding = Some(held);
        UrbOutcome::Held
    }

    fn accept<E: UrbEngine>(
        &mut self,
        reach: Reach,
        mut held: Held,
        shm: &mut [u8],
        engine: &mut E,
    ) -> UrbOutcome {
        if reach == Reach::Retracted {
            return UrbOutcome::Reply(held.reply(Err(Errno::NotFound)));
        }
        if self.outstanding.is_some() {
            return UrbOutcome::Reply(held.reply(Err(Errno::AlreadyExists)));
        }
        if reach == Reach::Recovering {
            self.outstanding = Some(held);
            return UrbOutcome::Held;
        }
        match held.drive(shm, engine) {
            Ok(Some(done)) => UrbOutcome::Reply(held.reply(Ok(done))),
            Ok(None) => {
                self.outstanding = Some(held);
                UrbOutcome::Held
            }
            Err(err) => UrbOutcome::Reply(held.reply(Err(err))),
        }
    }

    /// Service a controller event over the shared `shm` buffer and `engine`.
    ///
    /// The held call is driven: a finished one is [`UrbOutcome::Reply`]-now
    /// (clearing the slot), one still running stays [`UrbOutcome::Held`], a
    /// fault is answered fail-closed (clearing the slot). With nothing
    /// outstanding the event is [`UrbOutcome::Idle`].
    pub fn on_event<E: UrbEngine>(&mut self, shm: &mut [u8], engine: &mut E) -> UrbOutcome {
        let Some(mut held) = self.outstanding.take() else {
            return UrbOutcome::Idle;
        };
        match held.drive(shm, engine) {
            Ok(Some(done)) => UrbOutcome::Reply(held.reply(Ok(done))),
            Ok(None) => {
                self.outstanding = Some(held);
                UrbOutcome::Held
            }
            Err(err) => UrbOutcome::Reply(held.reply(Err(err))),
        }
    }

    /// Answer the held URB [`Errno::TimedOut`] if its deadline has passed at
    /// `now_ns`, abandoning it through `engine` when the engine was asked for
    /// it — [`Errno::DeviceFault`] when the controller would not let it go,
    /// its pipe now in a state no later transfer should trust. Anything else
    /// is left as it is.
    pub fn expire<E: UrbEngine>(&mut self, now_ns: u64, engine: &mut E) -> UrbOutcome {
        let Some(held) = self.take_overdue(now_ns) else {
            return UrbOutcome::Idle;
        };
        let errno = match held.work {
            Work::Transfer(urb) if held.driven => match engine.abandon(&urb) {
                Ok(()) => Errno::TimedOut,
                Err(_) => Errno::DeviceFault,
            },
            Work::Transfer(_) | Work::Operation(_) | Work::Deferred => Errno::TimedOut,
        };
        UrbOutcome::Reply(held.reply(Err(errno)))
    }

    /// Answer the held URB [`Errno::TimedOut`] if its deadline has passed at
    /// `now_ns`, leaving the controller alone: one that is recovering has
    /// nothing queued its reset does not discard.
    pub fn expire_unserved(&mut self, now_ns: u64) -> UrbOutcome {
        match self.take_overdue(now_ns) {
            Some(held) => UrbOutcome::Reply(held.reply(Err(Errno::TimedOut))),
            None => UrbOutcome::Idle,
        }
    }

    fn take_overdue(&mut self, now_ns: u64) -> Option<Held> {
        self.outstanding
            .take_if(|held| held.deadline_ns.is_some_and(|at| at <= now_ns))
    }

    /// Answer the held call, if any, with `errno`, clearing the slot:
    /// [`Errno::NotFound`] when the interface is retracted, since its
    /// endpoint outlives it and a stale request must not block the next class
    /// driver served there.
    #[must_use]
    pub fn abort_outstanding(&mut self, errno: Errno) -> UrbOutcome {
        let Some(held) = self.outstanding.take() else {
            return UrbOutcome::Idle;
        };
        UrbOutcome::Reply(held.reply(Err(errno)))
    }

    /// Answer the held call the reissuable [`Errno::WouldBlock`] if the
    /// engine was asked for it before a controller reset discarded it, or it
    /// needs no engine work. One held without touching the controller stays
    /// held, to be driven now that it is back.
    #[must_use]
    pub fn reissue_after_reset(&mut self) -> UrbOutcome {
        match self.outstanding {
            Some(held) if held.driven || matches!(held.work, Work::Deferred) => {
                self.abort_outstanding(Errno::WouldBlock)
            }
            Some(_) => UrbOutcome::Held,
            None => UrbOutcome::Idle,
        }
    }
}

/// The level a failed URB is logged at. A STALL is the device's own answer,
/// which its class driver reads — a single-LUN stick refusing GET MAX LUN is
/// no failure of the bus — so it is information; anything else is a warning.
#[must_use]
pub const fn urb_failure_level(errno: Errno) -> Level {
    match errno {
        Errno::EndpointStalled => Level::Info,
        _ => Level::Warn,
    }
}

/// Extend the enumerated device's interface [`HwNode`] (from
/// [`describe_device`](tairix_usb::device::UsbDevice::describe_device)) with
/// the URB-transport grants the autoloaded class driver inherits: the
/// per-endpoint call grant for `endpoint_id` and the per-region shared-memory
/// grant for `shm_id`.
///
/// The node already carries the USB `vid:pid:class` match keys; adding these
/// two resources is what lets the kernel mint the class driver exactly the
/// authority to submit URBs on this one interface and to map this one shared
/// buffer — and no controller register, no DMA, no other interface's buffer
/// (least privilege). The kernel's `hw_emit_node` coverage check admits the
/// node because the HCD holds both grants (minted when it created the endpoint
/// and the region).
///
/// # Errors
///
/// [`DriverError::NoSpace`] if the node cannot carry both grants.
pub fn attach_transport_grants(
    mut node: HwNode,
    endpoint_id: u64,
    shm_id: u64,
) -> Result<HwNode, DriverError> {
    node.push_resource(HwResource::endpoint(endpoint_id))
        .map_err(|_| DriverError::NoSpace)?;
    node.push_resource(HwResource::shared(shm_id))
        .map_err(|_| DriverError::NoSpace)?;
    Ok(node)
}

#[cfg(test)]
#[path = "serve_tests.rs"]
mod tests;
