//! The `i2c-v1` endpoint, over any I²C controller's bus.
//!
//! A request counts only once the kernel attests the caller holds the link it
//! quotes. Each holder opens its own channel per link and is granted a buffer
//! made for that channel; a transaction runs only when every message names an
//! address the link reaches and the bus can issue every repeated START it
//! needs. Closing the channel, or the holder's end, drops it and its buffer.

use alloc::vec::Vec;

use tairix_abi::driver::i2c::{
    encode_close_reply, encode_error_reply, encode_open_reply, encode_transfer_reply, BusFacts,
    I2cChannel, I2cFault, I2cOp, I2cReach, I2cRequest, I2cTransaction, I2C_BUFFER_LEN,
    I2C_MAX_REPLY,
};
use tairix_abi::hwlink::{LinkRequest, RegionGrant};
use tairix_abi::hwtree::HwResource;
use tairix_abi::{DriverError, Errno, ProcId};
use tairix_drvrt::{refusal, Recorder, Supplier, SupplierHost};

/// A controller's bus, as the endpoint drives it.
pub trait I2cBus {
    /// What the bus states about itself.
    fn facts(&self) -> BusFacts;

    /// Run `transaction` as one, each message moving its bytes through its
    /// span of `buffer`, answering how it ended on the bus.
    ///
    /// The endpoint has already held every message to an address the
    /// caller's link reaches, every span to `buffer`, and the transaction to
    /// the repeated STARTs [`facts`](Self::facts) states.
    ///
    /// # Errors
    ///
    /// A controller that could not run it: the bus was not touched, or the
    /// controller faulted and was reset.
    fn transfer(
        &mut self,
        transaction: &I2cTransaction,
        buffer: &mut [u8],
    ) -> Result<Result<(), I2cFault>, DriverError>;
}

/// Where each channel's buffer comes from, and how its client is granted it.
pub trait BufferHost {
    /// A channel's buffer, shared with its client.
    type Buffer: AsMut<[u8]>;

    /// Make a buffer of [`I2C_BUFFER_LEN`] bytes.
    ///
    /// # Errors
    ///
    /// The kernel's refusal.
    fn make(&mut self) -> Result<Self::Buffer, Errno>;

    /// Grant the caller of `ticket` a mapping of `buffer`, answering the
    /// handle it maps it by.
    ///
    /// # Errors
    ///
    /// The kernel's refusal.
    fn grant(&mut self, buffer: &Self::Buffer, ticket: u64) -> Result<u64, Errno>;

    /// This process's own instance, which grants every buffer.
    fn instance(&self) -> ProcId;
}

/// A decision the endpoint records.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Record {
    /// A holder opened a channel.
    Opened {
        /// The holder.
        holder: ProcId,
        /// What its link reaches.
        reach: I2cReach,
    },
    /// A holder closed a channel.
    Closed {
        /// The holder.
        holder: ProcId,
    },
    /// A holder ended with channels open; they were dropped.
    Abandoned {
        /// The holder that ended.
        holder: ProcId,
    },
    /// A transaction reached the bus and faulted.
    Faulted {
        /// The holder whose transaction it was.
        holder: ProcId,
        /// How it ended.
        fault: I2cFault,
    },
    /// A request was refused.
    Refused {
        /// The operation, when the frame named one.
        op: Option<I2cOp>,
        /// The caller, when the kernel could say.
        caller: Option<ProcId>,
        /// Why.
        reason: Errno,
    },
}

struct Channel<B> {
    holder: ProcId,
    link: LinkRequest,
    reach: I2cReach,
    buffer: B,
}

enum Answer {
    Opened(I2cChannel),
    Transferred(Result<(), I2cFault>),
    Closed,
}

/// An I²C controller's endpoint.
///
/// Channels live in a list searched per request: one per link a holder opens,
/// and the links are those the tree grants the targets on one bus.
pub struct Controller<B: I2cBus, H: BufferHost, S: SupplierHost, L: Recorder<Record>> {
    bus: B,
    buffers: H,
    supplier: S,
    log: L,
    endpoint: u64,
    channels: Vec<Channel<H::Buffer>>,
}

impl<B: I2cBus, H: BufferHost, S: SupplierHost, L: Recorder<Record>> Controller<B, H, S, L> {
    /// The endpoint `endpoint` for `bus`, making each channel's buffer
    /// through `buffers`, asking about its callers through `supplier` and
    /// recording to `log`. It touches the bus only when asked to.
    pub fn new(bus: B, buffers: H, supplier: S, log: L, endpoint: u64) -> Self {
        Self {
            bus,
            buffers,
            supplier,
            log,
            endpoint,
            channels: Vec::new(),
        }
    }

    fn answer(
        &mut self,
        ticket: u64,
        caller: ProcId,
        request: &I2cRequest,
    ) -> Result<Answer, Errno> {
        let link = request.link();
        if link.endpoint() != self.endpoint {
            return Err(Errno::OutOfRange);
        }
        if !self
            .supplier
            .caller_holds(ticket, &HwResource::request(link))?
        {
            return Err(Errno::PermissionDenied);
        }
        match request {
            I2cRequest::Open(link) => self.open(ticket, caller, *link),
            I2cRequest::Transfer { link, transaction } => self.transfer(caller, link, transaction),
            I2cRequest::Close(link) => self.close(caller, link),
        }
    }

    fn open(&mut self, ticket: u64, caller: ProcId, link: LinkRequest) -> Result<Answer, Errno> {
        let reach = I2cReach::of(&link)?;
        if self.find(caller, &link).is_some() {
            return Err(Errno::AlreadyExists);
        }
        self.channels
            .try_reserve(1)
            .map_err(|_| Errno::OutOfMemory)?;
        let mut buffer = self.buffers.make()?;
        if buffer.as_mut().len() < I2C_BUFFER_LEN {
            return Err(Errno::OutOfMemory);
        }
        self.watch(caller)?;
        let grant = match self.buffers.grant(&buffer, ticket) {
            Ok(grant) => grant,
            Err(reason) => {
                self.unwatch_if_idle(caller);
                return Err(reason);
            }
        };
        self.channels.push(Channel {
            holder: caller,
            link,
            reach,
            buffer,
        });
        self.log.record(Record::Opened {
            holder: caller,
            reach,
        });
        Ok(Answer::Opened(I2cChannel {
            facts: self.bus.facts(),
            buffer: RegionGrant {
                grant,
                grantor: self.buffers.instance(),
            },
        }))
    }

    fn transfer(
        &mut self,
        caller: ProcId,
        link: &LinkRequest,
        transaction: &I2cTransaction,
    ) -> Result<Answer, Errno> {
        let at = self.find(caller, link).ok_or(Errno::NotFound)?;
        let channel = &mut self.channels[at];
        let reach = channel.reach;
        if !transaction
            .messages()
            .iter()
            .all(|message| reach.reaches(message.address))
        {
            return Err(Errno::PermissionDenied);
        }
        if !self.bus.facts().repeated_start.admits(transaction) {
            return Err(Errno::NotSupported);
        }
        let outcome = self
            .bus
            .transfer(transaction, channel.buffer.as_mut())
            .map_err(refusal)?;
        if let Err(fault) = outcome {
            self.log.record(Record::Faulted {
                holder: caller,
                fault,
            });
        }
        Ok(Answer::Transferred(outcome))
    }

    fn close(&mut self, caller: ProcId, link: &LinkRequest) -> Result<Answer, Errno> {
        let at = self.find(caller, link).ok_or(Errno::NotFound)?;
        self.channels.swap_remove(at);
        self.unwatch_if_idle(caller);
        self.log.record(Record::Closed { holder: caller });
        Ok(Answer::Closed)
    }

    fn find(&self, holder: ProcId, link: &LinkRequest) -> Option<usize> {
        self.channels
            .iter()
            .position(|channel| channel.holder == holder && channel.link == *link)
    }

    fn holds_any(&self, instance: ProcId) -> bool {
        self.channels
            .iter()
            .any(|channel| channel.holder == instance)
    }

    /// Watch `instance` for its end, which its first channel needs.
    fn watch(&mut self, instance: ProcId) -> Result<(), Errno> {
        if self.holds_any(instance) {
            Ok(())
        } else {
            self.supplier.watch(instance)
        }
    }

    fn unwatch_if_idle(&mut self, instance: ProcId) {
        if !self.holds_any(instance) {
            self.supplier.unwatch(instance);
        }
    }

    fn send(&mut self, ticket: u64, answer: &Answer) {
        let mut out = [0u8; I2C_MAX_REPLY];
        let encoded = match answer {
            Answer::Opened(channel) => encode_open_reply(&mut out, channel),
            Answer::Transferred(outcome) => encode_transfer_reply(&mut out, *outcome),
            Answer::Closed => encode_close_reply(&mut out),
        };
        // A caller that ends before its answer arrives has its exit do the
        // cleaning up; there is nobody left to tell.
        if let Ok(len) = encoded {
            let _ = self.supplier.reply(ticket, &out[..len]);
        }
    }

    fn refuse(&mut self, ticket: u64, op: Option<I2cOp>, caller: Option<ProcId>, reason: Errno) {
        self.log.record(Record::Refused { op, caller, reason });
        let mut out = [0u8; I2C_MAX_REPLY];
        if let Ok(len) = encode_error_reply(&mut out, reason) {
            let _ = self.supplier.reply(ticket, &out[..len]);
        }
    }
}

impl<B: I2cBus, H: BufferHost, S: SupplierHost, L: Recorder<Record>> Supplier
    for Controller<B, H, S, L>
{
    fn serve(&mut self, ticket: u64, frame: &[u8]) {
        let caller = match self.supplier.caller(ticket) {
            Ok(caller) => caller,
            Err(reason) => return self.refuse(ticket, None, None, reason),
        };
        let request = match I2cRequest::decode(frame) {
            Ok(request) => request,
            Err(reason) => return self.refuse(ticket, None, Some(caller), reason),
        };
        match self.answer(ticket, caller, &request) {
            Ok(answer) => self.send(ticket, &answer),
            Err(reason) => self.refuse(ticket, Some(request.op()), Some(caller), reason),
        }
    }

    /// Drop every channel `peer` had, and its buffer.
    fn peer_exited(&mut self, peer: ProcId) {
        let before = self.channels.len();
        self.channels.retain(|channel| channel.holder != peer);
        if self.channels.len() != before {
            self.log.record(Record::Abandoned { holder: peer });
        }
        self.supplier.unwatch(peer);
    }
}
