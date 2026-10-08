//! The `dmaengine-v1` endpoint, written over the [`DmaEngine`] and
//! [`DmaChannel`] class traits and nothing of any one controller.
//!
//! It holds every rule the protocol makes. A channel belongs to the process
//! instance that opened it. A request line and a FIFO count only once the
//! kernel attests the caller holds them, and every buffer is carved here. A
//! posted wait is answered at the first period boundary past the position it
//! names, with the position counted from which period the channel has reached,
//! so a boundary passed while no wait was posted is answered at once.
//!
//! Counting by period rather than by interrupt keeps coalesced interrupts
//! exact. The one thing it cannot see is a service late by a whole lap of the
//! buffer, which is indistinguishable from none; the consumer, which knows its
//! stream's rate, sees it in the service times it is sent.

use core::num::NonZeroU32;

use tairix_abi::driver::dmaengine::{
    encode_done_reply, encode_error_reply, encode_open_reply, encode_position_reply,
    encode_prepare_reply, encode_wait_reply, CyclicParams, CyclicTransfer, DmaBufferGrant,
    DmaChannel, DmaChannelEvent, DmaEngine, DmaEngineOp, DmaEngineRequest, DmaRequestLine, Halted,
    WaitEnd, WaitReport, DMA_ENGINE_MAX_REPLY, DMA_MAX_CHANNELS,
};
use tairix_abi::hwtree::HwResource;
use tairix_abi::time::Duration64;
use tairix_abi::{DriverError, Errno, ProcId};

use crate::CHANNEL_SLOTS;

/// A buffer carved for one channel.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Buffer {
    /// The kernel's id for the shared region.
    pub region: u64,
    /// Base of this process's own mapping.
    pub base: u64,
    /// Bytes of that mapping.
    pub len: usize,
    /// Bus address the controller reaches the first byte at.
    pub bus: u64,
}

/// A decision the endpoint records.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Record {
    /// A channel was claimed for a request line.
    Claimed {
        /// The channel.
        channel: u8,
        /// Its new owner.
        owner: ProcId,
    },
    /// A channel an ended instance still held was taken for its line's new
    /// holder.
    Reclaimed {
        /// The channel.
        channel: u8,
        /// The instance that had held it.
        from: ProcId,
    },
    /// A request was refused.
    Refused {
        /// The operation, when the frame named one.
        op: Option<DmaEngineOp>,
        /// The caller, when the kernel could say.
        caller: Option<ProcId>,
        /// Why.
        reason: Errno,
    },
    /// A channel faulted and was stopped.
    Faulted {
        /// The channel.
        channel: u8,
        /// The controller's error bits.
        bits: NonZeroU32,
    },
    /// A channel whose position could not be read, or lay outside its
    /// buffer, was stopped.
    Lost {
        /// The channel.
        channel: u8,
    },
    /// A channel's owner ended and the channel was released.
    Abandoned {
        /// The channel.
        channel: u8,
    },
    /// A channel did not drain before its reset.
    Undrained {
        /// The channel.
        channel: u8,
    },
    /// A channel would not take its reset. It is withdrawn, and what it may
    /// still reach is never freed.
    Unreset {
        /// The channel.
        channel: u8,
    },
}

/// What the endpoint needs from the kernel, about the call being served.
pub trait ControllerHost {
    /// The instance whose call `ticket` is in service.
    ///
    /// # Errors
    ///
    /// The kernel's refusal, for a call no longer in service.
    fn caller(&self, ticket: u64) -> Result<ProcId, Errno>;

    /// Whether that caller holds a grant covering `record`.
    ///
    /// # Errors
    ///
    /// The kernel's refusal, other than the answer "no".
    fn caller_holds(&self, ticket: u64, record: &HwResource) -> Result<bool, Errno>;

    /// Carve a buffer of `bytes` the controller reaches.
    ///
    /// # Errors
    ///
    /// The kernel's refusal.
    fn carve(&mut self, bytes: u32) -> Result<Buffer, Errno>;

    /// Drop this process's mapping of `buffer`: the device is done with it.
    fn release(&mut self, buffer: &Buffer);

    /// Mint the caller of `ticket` a mapping of `buffer`.
    ///
    /// # Errors
    ///
    /// The kernel's refusal.
    fn grant(&mut self, buffer: &Buffer, ticket: u64) -> Result<u64, Errno>;

    /// Answer the call `ticket` with `frame`.
    ///
    /// # Errors
    ///
    /// The kernel's refusal; a caller that has ended cannot be answered.
    fn reply(&mut self, ticket: u64, frame: &[u8]) -> Result<(), Errno>;

    /// Be told when `peer` ends. Idempotent.
    ///
    /// # Errors
    ///
    /// [`Errno::NotFound`] when it already has.
    fn watch(&mut self, peer: ProcId) -> Result<(), Errno>;

    /// Stop being told when `peer` ends.
    fn unwatch(&mut self, peer: ProcId);

    /// The monotonic clock.
    fn now(&self) -> Duration64;

    /// This process's own instance, which delegates every buffer.
    fn instance(&self) -> ProcId;

    /// Record a decision.
    fn record(&mut self, record: Record);
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Shape {
    period_bytes: u32,
    periods: u32,
}

/// What a channel has counted since it started.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
struct Progress {
    /// The period the channel was in at the last count.
    period: u32,
    boundaries: u64,
    /// The latest boundary's position and when it was serviced.
    last: Option<(u64, Duration64)>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Posted {
    ticket: u64,
    after: u64,
}

struct Slot {
    owner: ProcId,
    line: DmaRequestLine,
    buffer: Option<Buffer>,
    shape: Option<Shape>,
    running: bool,
    progress: Progress,
    wait: Option<Posted>,
}

impl Slot {
    const fn new(owner: ProcId, line: DmaRequestLine) -> Self {
        Self {
            owner,
            line,
            buffer: None,
            shape: None,
            running: false,
            progress: Progress {
                period: 0,
                boundaries: 0,
                last: None,
            },
            wait: None,
        }
    }

    fn position(&self) -> u64 {
        let period_bytes = self.shape.map_or(0, |shape| u64::from(shape.period_bytes));
        self.progress.boundaries.saturating_mul(period_bytes)
    }
}

enum Answer {
    Channel(u8),
    Buffer(DmaBufferGrant),
    Done(DmaEngineOp),
    Offset(u64),
    Report(WaitReport),
    Held,
}

/// The reply a refusal from the class traits carries.
fn refusal(err: DriverError) -> Errno {
    match err {
        DriverError::Unsupported => Errno::NotSupported,
        DriverError::Busy => Errno::Busy,
        other => other.as_errno(),
    }
}

fn channel_mask(count: u8) -> u64 {
    match count {
        0 => 0,
        count if count >= DMA_MAX_CHANNELS => u64::MAX,
        count => (1 << count) - 1,
    }
}

/// The channels whose bits are set in `mask`, lowest first.
fn channels(mut mask: u64) -> impl Iterator<Item = u8> {
    core::iter::from_fn(move || {
        if mask == 0 {
            return None;
        }
        let index = mask.trailing_zeros();
        mask &= mask - 1;
        u8::try_from(index).ok()
    })
}

/// A DMA controller's endpoint.
pub struct Controller<E: DmaEngine, H: ControllerHost> {
    engine: E,
    host: H,
    endpoint: u64,
    owned: u64,
    usable: u64,
    peripheral_window: Option<HwResource>,
    slots: [Option<Slot>; CHANNEL_SLOTS],
}

impl<E: DmaEngine, H: ControllerHost> Controller<E, H> {
    /// The endpoint `endpoint` for `engine`, resetting every channel set in
    /// `owned` and serving those set in `usable`, and translating FIFOs only
    /// through `peripheral_window`, the translated window covering the
    /// controller's own registers: a held region some other window reaches is
    /// memory, not a FIFO.
    ///
    /// Every owned channel is reset first, served or not, so a chain an
    /// earlier instance left running is stopped before any memory it could
    /// fetch leaves quarantine; `None` when a reset could not be issued, the
    /// memory then left quarantined.
    pub fn new(
        mut engine: E,
        mut host: H,
        endpoint: u64,
        (owned, usable): (u64, u64),
        peripheral_window: Option<HwResource>,
    ) -> Option<Self> {
        let owned = owned & channel_mask(engine.channel_count());
        for channel in channels(owned) {
            match engine.channel(channel)?.stop() {
                Ok(Halted::Drained) => {}
                Ok(Halted::Undrained) => host.record(Record::Undrained { channel }),
                Err(_) => {
                    host.record(Record::Unreset { channel });
                    return None;
                }
            }
        }
        Some(Self {
            engine,
            host,
            endpoint,
            owned,
            usable: usable & owned,
            peripheral_window,
            slots: [const { None }; CHANNEL_SLOTS],
        })
    }

    /// Whether `outcome`, a reset of `channel`, was issued, so what the
    /// channel reached may be freed; recording a reset that cut writes off,
    /// and withdrawing a channel that would not take its reset.
    fn settle(&mut self, channel: u8, outcome: Result<Halted, DriverError>) -> bool {
        match outcome {
            Ok(Halted::Drained) => true,
            Ok(Halted::Undrained) => {
                self.host.record(Record::Undrained { channel });
                true
            }
            Err(_) => {
                self.host.record(Record::Unreset { channel });
                self.usable &= !(1u64 << channel);
                false
            }
        }
    }

    /// The channels this endpoint serves.
    #[must_use]
    pub const fn usable(&self) -> u64 {
        self.usable
    }

    /// Serve the call `ticket` carrying `frame`, answering it now unless it
    /// is a wait to be answered at a later boundary.
    pub fn serve(&mut self, ticket: u64, frame: &[u8]) {
        let caller = match self.host.caller(ticket) {
            Ok(caller) => caller,
            Err(reason) => return self.refuse(ticket, None, None, reason),
        };
        let request = match DmaEngineRequest::decode(frame) {
            Ok(request) => request,
            Err(reason) => return self.refuse(ticket, None, Some(caller), reason),
        };
        let op = request.op();
        match self.answer(ticket, caller, &request) {
            Ok(Answer::Held) => {}
            Ok(answer) => self.send(ticket, &answer),
            Err(reason) => self.refuse(ticket, Some(op), Some(caller), reason),
        }
    }

    /// Service the channels in `fired` whose interrupt line was raised.
    pub fn interrupt(&mut self, fired: u64) {
        // A channel taken out of service is only quietened: its latched flags
        // would hold a shared line up for good.
        for channel in channels(fired & self.owned & !self.usable) {
            if let Some(engine_channel) = self.engine.channel(channel) {
                let _ = engine_channel.take_event();
            }
        }
        for channel in channels(fired & self.usable) {
            let index = usize::from(channel);
            let running = self.slots[index].as_ref().is_some_and(|slot| slot.running);
            let Some(engine_channel) = self.engine.channel(channel) else {
                continue;
            };
            match (engine_channel.take_event(), running) {
                (Ok(DmaChannelEvent::Quiet | DmaChannelEvent::Boundary), false)
                | (Ok(DmaChannelEvent::Quiet), true) => {}
                (Ok(DmaChannelEvent::Boundary), true) => self.advance(channel),
                (Ok(DmaChannelEvent::Faulted(bits)), true) => {
                    self.host.record(Record::Faulted { channel, bits });
                    self.halt(channel, WaitEnd::Faulted(bits));
                }
                (Err(_), true) => {
                    self.host.record(Record::Lost { channel });
                    self.halt(channel, WaitEnd::Stopped);
                }
                // A latched fault on a channel with nothing running would hold
                // its level-triggered line up; the reset clears it.
                (Ok(DmaChannelEvent::Faulted(_)) | Err(_), false) => {
                    let outcome = engine_channel.stop();
                    self.settle(channel, outcome);
                }
            }
        }
    }

    /// Release every channel `peer` held; it has ended.
    pub fn peer_exited(&mut self, peer: ProcId) {
        for index in 0..CHANNEL_SLOTS {
            if self.slots[index]
                .as_ref()
                .is_some_and(|slot| slot.owner == peer)
            {
                let Ok(channel) = u8::try_from(index) else {
                    continue;
                };
                self.host.record(Record::Abandoned { channel });
                self.retire(channel);
            }
        }
    }

    fn answer(
        &mut self,
        ticket: u64,
        caller: ProcId,
        request: &DmaEngineRequest,
    ) -> Result<Answer, Errno> {
        match *request {
            DmaEngineRequest::Open(line) => self.open(ticket, caller, &line),
            DmaEngineRequest::Prepare { channel, params } => {
                self.serving(caller, channel)?;
                self.prepare(ticket, channel, &params)
            }
            DmaEngineRequest::Start { channel } => {
                self.serving(caller, channel)?;
                self.start(channel)
            }
            DmaEngineRequest::Stop { channel } => {
                self.owned(caller, channel)?;
                // A channel that would not stop may still write its buffer,
                // which its client must then never reuse.
                if !self.halt(channel, WaitEnd::Stopped) {
                    return Err(Errno::DeviceFault);
                }
                Ok(Answer::Done(DmaEngineOp::Stop))
            }
            DmaEngineRequest::Position { channel } => {
                let slot = self.serving(caller, channel)?;
                if slot.shape.is_none() {
                    return Err(Errno::NotFound);
                }
                let engine_channel = self.engine.channel(channel).ok_or(Errno::NotFound)?;
                let offset = engine_channel.position().map_err(refusal)?;
                Ok(Answer::Offset(u64::from(offset)))
            }
            DmaEngineRequest::Close { channel } => {
                self.owned(caller, channel)?;
                self.retire(channel);
                Ok(Answer::Done(DmaEngineOp::Close))
            }
            DmaEngineRequest::Wait { channel, after } => {
                self.serving(caller, channel)?;
                self.wait(ticket, channel, after)
            }
        }
    }

    /// The slot of `channel`, when `caller` is the instance that opened it.
    fn owned(&self, caller: ProcId, channel: u8) -> Result<&Slot, Errno> {
        match self.slots.get(usize::from(channel)) {
            Some(Some(slot)) if slot.owner == caller => Ok(slot),
            _ => Err(Errno::PermissionDenied),
        }
    }

    /// The slot of `channel` as [`Self::owned`], for an operation a channel
    /// withdrawn for refusing its reset may no longer take: it may still be
    /// running a chain the controller can no longer stop.
    fn serving(&self, caller: ProcId, channel: u8) -> Result<&Slot, Errno> {
        let slot = self.owned(caller, channel)?;
        let usable = 1u64
            .checked_shl(u32::from(channel))
            .is_some_and(|bit| self.usable & bit != 0);
        if usable {
            Ok(slot)
        } else {
            Err(Errno::DeviceFault)
        }
    }

    fn slot_mut(&mut self, channel: u8) -> Option<&mut Slot> {
        self.slots.get_mut(usize::from(channel))?.as_mut()
    }

    fn open(
        &mut self,
        ticket: u64,
        caller: ProcId,
        line: &DmaRequestLine,
    ) -> Result<Answer, Errno> {
        if line.endpoint() != self.endpoint {
            return Err(Errno::OutOfRange);
        }
        if !self
            .host
            .caller_holds(ticket, &HwResource::dma_request(line))?
        {
            return Err(Errno::PermissionDenied);
        }
        self.engine.accept(line).map_err(refusal)?;
        let serving = (0..CHANNEL_SLOTS).find_map(|index| {
            let slot = self.slots[index].as_ref()?;
            (slot.line == *line).then_some((index, slot.owner))
        });
        if let Some((index, holder)) = serving {
            if holder == caller {
                return Err(Errno::AlreadyExists);
            }
            // Two nodes may carry the same line; it stays with a holder that
            // lives, and a watch refused as "gone" is the kernel saying it
            // does not.
            match self.host.watch(holder) {
                Ok(()) => return Err(Errno::Busy),
                Err(Errno::NotFound) => {}
                Err(reason) => return Err(reason),
            }
            let channel = u8::try_from(index).map_err(|_| Errno::OutOfRange)?;
            self.retire(channel);
            self.host.record(Record::Reclaimed {
                channel,
                from: holder,
            });
        }
        let channel = channels(self.usable)
            .find(|&channel| self.slots[usize::from(channel)].is_none())
            .ok_or(Errno::Busy)?;
        self.host.watch(caller)?;
        self.slots[usize::from(channel)] = Some(Slot::new(caller, *line));
        self.host.record(Record::Claimed {
            channel,
            owner: caller,
        });
        Ok(Answer::Channel(channel))
    }

    fn prepare(
        &mut self,
        ticket: u64,
        channel: u8,
        params: &CyclicParams,
    ) -> Result<Answer, Errno> {
        let slot = self.slot_mut(channel).ok_or(Errno::NotFound)?;
        if slot.running {
            return Err(Errno::Busy);
        }
        let line = slot.line;
        let access = self.engine.admit(&line, params).map_err(refusal)?;
        if !self
            .host
            .caller_holds(ticket, &HwResource::mmio(params.fifo, u64::from(access)))?
        {
            return Err(Errno::PermissionDenied);
        }
        let fifo = self
            .peripheral_window
            .and_then(|window| window.dma_bus_address(params.fifo, u64::from(access)))
            .ok_or(Errno::OutOfRange)?;
        let buffer = self.host.carve(params.buffer_bytes()?)?;
        let transfer = CyclicTransfer {
            buffer: buffer.bus,
            fifo,
            direction: params.direction,
            period_bytes: params.period_bytes,
            periods: params.periods,
        };
        let engine_channel = self.engine.channel(channel).ok_or(Errno::NotFound)?;
        if let Err(err) = engine_channel.prepare(&line, &transfer) {
            self.host.release(&buffer);
            return Err(refusal(err));
        }
        let grant = match self.host.grant(&buffer, ticket) {
            Ok(grant) => grant,
            Err(reason) => {
                // The new chain names the buffer being dropped, so it goes too,
                // and with it the one the slot held before: once the channel is
                // reset, for until then it may still write either.
                let outcome = engine_channel.release();
                let freed = self.settle(channel, outcome);
                if freed {
                    self.host.release(&buffer);
                }
                if let Some(slot) = self.slot_mut(channel) {
                    // The chain the engine holds is no longer the one it shapes.
                    slot.shape = None;
                    let previous = if freed { slot.buffer.take() } else { None };
                    if let Some(previous) = previous {
                        self.host.release(&previous);
                    }
                }
                return Err(reason);
            }
        };
        let slot = self.slot_mut(channel).ok_or(Errno::NotFound)?;
        let previous = slot.buffer.replace(buffer);
        slot.shape = Some(Shape {
            period_bytes: params.period_bytes,
            periods: params.periods,
        });
        slot.progress = Progress::default();
        // The channel is stopped, so nothing masters the buffer it replaces.
        if let Some(previous) = previous {
            self.host.release(&previous);
        }
        Ok(Answer::Buffer(DmaBufferGrant {
            grant,
            grantor: self.host.instance(),
        }))
    }

    fn start(&mut self, channel: u8) -> Result<Answer, Errno> {
        let slot = self.slot_mut(channel).ok_or(Errno::NotFound)?;
        if slot.shape.is_none() {
            return Err(Errno::NotFound);
        }
        if slot.running {
            return Err(Errno::Busy);
        }
        self.engine
            .channel(channel)
            .ok_or(Errno::NotFound)?
            .start()
            .map_err(refusal)?;
        let slot = self.slot_mut(channel).ok_or(Errno::NotFound)?;
        slot.running = true;
        slot.progress = Progress::default();
        Ok(Answer::Done(DmaEngineOp::Start))
    }

    fn wait(&mut self, ticket: u64, channel: u8, after: u64) -> Result<Answer, Errno> {
        let serviced = self.host.now();
        let slot = self.slot_mut(channel).ok_or(Errno::NotFound)?;
        if !slot.running {
            return Ok(Answer::Report(WaitReport {
                end: WaitEnd::Stopped,
                position: slot.position(),
                serviced,
            }));
        }
        if slot.wait.is_some() {
            return Err(Errno::Busy);
        }
        if let Some((position, serviced)) = slot.progress.last {
            if position > after {
                return Ok(Answer::Report(WaitReport {
                    end: WaitEnd::Boundary,
                    position,
                    serviced,
                }));
            }
        }
        slot.wait = Some(Posted { ticket, after });
        Ok(Answer::Held)
    }

    /// Count the boundaries `channel` has crossed and answer its wait if one
    /// is past what it asked for.
    fn advance(&mut self, channel: u8) {
        let Some(Ok(offset)) = self.engine.channel(channel).map(|engine| engine.position()) else {
            self.host.record(Record::Lost { channel });
            self.halt(channel, WaitEnd::Stopped);
            return;
        };
        let serviced = self.host.now();
        let Some(slot) = self.slot_mut(channel) else {
            return;
        };
        let Some(shape) = slot.shape else {
            return;
        };
        let period = offset / shape.period_bytes % shape.periods;
        let passed = (u64::from(period) + u64::from(shape.periods)
            - u64::from(slot.progress.period))
            % u64::from(shape.periods);
        if passed == 0 {
            return;
        }
        slot.progress.period = period;
        slot.progress.boundaries = slot.progress.boundaries.saturating_add(passed);
        let position = slot.position();
        slot.progress.last = Some((position, serviced));
        let Some(posted) = slot.wait.filter(|posted| position > posted.after) else {
            return;
        };
        slot.wait = None;
        let report = WaitReport {
            end: WaitEnd::Boundary,
            position,
            serviced,
        };
        if !self.send_report(posted.ticket, &report) {
            // A wait that cannot be answered is a consumer that has gone.
            self.halt(channel, WaitEnd::Stopped);
        }
    }

    /// Stop `channel` if it runs, and answer its posted wait with `end`,
    /// answering whether what the channel reached may be freed.
    fn halt(&mut self, channel: u8, end: WaitEnd) -> bool {
        let serviced = self.host.now();
        let Some(slot) = self.slot_mut(channel) else {
            return true;
        };
        let was_running = core::mem::replace(&mut slot.running, false);
        let posted = slot.wait.take();
        let position = slot.position();
        let mut settled = true;
        if was_running {
            if let Some(outcome) = self.engine.channel(channel).map(DmaChannel::stop) {
                settled = self.settle(channel, outcome);
            }
        }
        if let Some(posted) = posted {
            let report = WaitReport {
                end,
                position,
                serviced,
            };
            let _ = self.send_report(posted.ticket, &report);
        }
        settled
    }

    /// Stop `channel`, free its chain and its buffer, and forget its owner.
    fn retire(&mut self, channel: u8) {
        // A channel that would not stop is not reset again, and keeps all it
        // reached.
        let freed = self.halt(channel, WaitEnd::Stopped)
            && match self.engine.channel(channel).map(DmaChannel::release) {
                Some(outcome) => self.settle(channel, outcome),
                None => true,
            };
        let Some(slot) = self
            .slots
            .get_mut(usize::from(channel))
            .and_then(Option::take)
        else {
            return;
        };
        // A channel that would not take its reset may still write its buffer,
        // which is never freed lest the memory be carved anew beneath it.
        if let (true, Some(buffer)) = (freed, slot.buffer) {
            self.host.release(&buffer);
        }
        if !self
            .slots
            .iter()
            .flatten()
            .any(|other| other.owner == slot.owner)
        {
            self.host.unwatch(slot.owner);
        }
    }

    fn send(&mut self, ticket: u64, answer: &Answer) {
        let mut out = [0u8; DMA_ENGINE_MAX_REPLY];
        let encoded = match answer {
            Answer::Channel(channel) => encode_open_reply(&mut out, *channel),
            Answer::Buffer(buffer) => encode_prepare_reply(&mut out, buffer),
            Answer::Done(op) => encode_done_reply(&mut out, *op),
            Answer::Offset(offset) => encode_position_reply(&mut out, *offset),
            Answer::Report(report) => encode_wait_reply(&mut out, report),
            Answer::Held => return,
        };
        // A caller that ends before its answer arrives has its exit do the
        // cleaning up; there is nobody left to tell.
        if let Ok(len) = encoded {
            let _ = self.host.reply(ticket, &out[..len]);
        }
    }

    fn send_report(&mut self, ticket: u64, report: &WaitReport) -> bool {
        let mut out = [0u8; DMA_ENGINE_MAX_REPLY];
        encode_wait_reply(&mut out, report)
            .is_ok_and(|len| self.host.reply(ticket, &out[..len]).is_ok())
    }

    fn refuse(
        &mut self,
        ticket: u64,
        op: Option<DmaEngineOp>,
        caller: Option<ProcId>,
        reason: Errno,
    ) {
        self.host.record(Record::Refused { op, caller, reason });
        let mut out = [0u8; DMA_ENGINE_MAX_REPLY];
        if let Ok(len) = encode_error_reply(&mut out, reason) {
            let _ = self.host.reply(ticket, &out[..len]);
        }
    }
}

/// The node's translated `windows` as the engines use them: the one covering
/// the controller's own registers at `[base, base + len)`, through which they
/// reach peripherals, and the first other, through which they reach memory.
#[must_use]
pub fn split_windows<'r>(
    windows: impl IntoIterator<Item = &'r HwResource>,
    base: u64,
    len: u64,
) -> (Option<HwResource>, Option<HwResource>) {
    let (mut peripheral, mut memory) = (None, None);
    for window in windows {
        let slot = if window.dma_bus_address(base, len).is_some() {
            &mut peripheral
        } else {
            &mut memory
        };
        if slot.is_none() {
            *slot = Some(*window);
        }
    }
    (peripheral, memory)
}
