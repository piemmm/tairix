//! Many requests on a split virtqueue at once, each answered in its own time.

use alloc::vec::Vec;

use tairix_abi::DriverError;
use tairix_collections::{Key, SlotKey, SlotMap};

use crate::host::VirtioHost;
use crate::queue::{ChainSegment, SplitQueue, UsedToken};
use crate::request::await_completions;
use crate::transport::{Transport, VirtioError};

/// A request a [`TicketQueue`] carries.
///
/// It names that request alone: a ticket that outlived its request is
/// refused, never taken for a later request's.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Ticket(SlotKey);

impl Key for Ticket {
    fn from_slot(slot: SlotKey) -> Self {
        Self(slot)
    }

    fn slot(self) -> SlotKey {
        self.0
    }
}

/// Where a request is.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum State {
    /// With the device.
    Out,
    /// Handed back, the device saying it wrote this many bytes into it.
    Answered(u32),
}

/// The requests a queue carries, and the chain each one is.
struct Requests {
    states: SlotMap<Ticket, State>,
    /// The ticket of the chain each descriptor heads while the device holds
    /// it.
    heads: Vec<Option<Ticket>>,
    out: u16,
}

impl Requests {
    /// Record the device's answer `token`, answering whose it is.
    fn answer(&mut self, token: UsedToken) -> Result<Ticket, DriverError> {
        let ticket = self
            .heads
            .get_mut(usize::from(token.head))
            .and_then(Option::take)
            .ok_or(DriverError::DeviceFault)?;
        let state = self
            .states
            .get_mut(ticket)
            .ok_or(DriverError::DeviceFault)?;
        *state = State::Answered(token.written);
        self.out = self.out.saturating_sub(1);
        Ok(ticket)
    }
}

/// A split virtqueue carrying many requests at once, each answered when the
/// device hands its chain back, in whatever order it does.
///
/// A request's buffers are the device's from [`Self::publish`] until its
/// ticket is answered. An answer is its own request's however late it comes,
/// so no request waits on another's, and no buffer is reused while the device
/// may still write it.
pub struct TicketQueue {
    queue: SplitQueue,
    requests: Requests,
}

impl TicketQueue {
    /// Carry requests on `queue`, which holds no chain yet.
    ///
    /// # Errors
    ///
    /// [`VirtioError::OutOfMemory`] when its bookkeeping cannot be had.
    pub fn new(queue: SplitQueue) -> Result<Self, VirtioError> {
        let size = usize::from(queue.size());
        let states = SlotMap::try_with_capacity(size).map_err(|_| VirtioError::OutOfMemory)?;
        let mut heads = Vec::new();
        heads
            .try_reserve_exact(size)
            .map_err(|_| VirtioError::OutOfMemory)?;
        heads.resize(size, None);
        Ok(Self {
            queue,
            requests: Requests {
                states,
                heads,
                out: 0,
            },
        })
    }

    /// The queue's descriptors.
    #[must_use]
    pub fn size(&self) -> u16 {
        self.queue.size()
    }

    /// Descriptors no request holds.
    #[must_use]
    pub fn free_descriptors(&self) -> u16 {
        self.queue.free_count()
    }

    /// Requests the device holds.
    #[must_use]
    pub fn out(&self) -> u16 {
        self.requests.out
    }

    /// Publish `segments` as one request, without notifying the device.
    ///
    /// Nothing is published unless the queue holds the whole chain now.
    ///
    /// # Errors
    ///
    /// [`DriverError::Busy`] for a chain the free descriptors do not hold, or
    /// while as many requests as the queue has descriptors are yet to be
    /// retired; [`DriverError::DeviceFault`] for an empty chain or one longer
    /// than the queue; [`DriverError::OutOfMemory`] for a record that cannot
    /// be had.
    pub fn publish(&mut self, segments: &[ChainSegment]) -> Result<Ticket, DriverError> {
        if segments.is_empty() || segments.len() > usize::from(self.queue.size()) {
            return Err(DriverError::DeviceFault);
        }
        if segments.len() > usize::from(self.queue.free_count())
            || self.requests.states.len() >= self.requests.heads.len()
        {
            return Err(DriverError::Busy);
        }
        let ticket = self
            .requests
            .states
            .try_insert(State::Out)
            .map_err(|_| DriverError::OutOfMemory)?;
        let head = match self.queue.add_chain(segments) {
            Ok(head) => head,
            Err(err) => {
                self.requests.states.remove(ticket);
                return Err(err.as_driver_error());
            }
        };
        // A head is a descriptor index, so it is within the table.
        if let Some(slot) = self.requests.heads.get_mut(usize::from(head)) {
            *slot = Some(ticket);
        }
        self.requests.out += 1;
        Ok(ticket)
    }

    /// Notify the device of what was published.
    pub fn kick<T: Transport>(&self, transport: &mut T) {
        self.queue.kick(transport);
    }

    /// Take every answer the device has posted, answering how many.
    ///
    /// # Errors
    ///
    /// [`DriverError::DeviceFault`] for an answer the device had no right to
    /// post; the answers before it are kept, and it is skipped.
    pub fn drain(&mut self) -> Result<u16, DriverError> {
        let mut answered = 0;
        loop {
            match self.queue.poll_used() {
                Ok(token) => {
                    self.requests.answer(token)?;
                    answered += 1;
                }
                Err(VirtioError::NoCompletion) => return Ok(answered),
                Err(err) => return Err(err.as_driver_error()),
            }
        }
    }

    /// The bytes the device says it wrote into `ticket`'s request, once it
    /// has answered it.
    ///
    /// # Errors
    ///
    /// [`DriverError::NotFound`] for a ticket naming no request this queue
    /// carries.
    pub fn answer(&self, ticket: Ticket) -> Result<Option<u32>, DriverError> {
        match self.requests.states.get(ticket) {
            Some(State::Out) => Ok(None),
            Some(State::Answered(written)) => Ok(Some(*written)),
            None => Err(DriverError::NotFound),
        }
    }

    /// Let go of `ticket`'s answered request, answering the bytes the device
    /// says it wrote.
    ///
    /// # Errors
    ///
    /// [`DriverError::Busy`] while the device still holds it,
    /// [`DriverError::NotFound`] for a ticket naming no request.
    pub fn retire(&mut self, ticket: Ticket) -> Result<u32, DriverError> {
        let written = self.answer(ticket)?.ok_or(DriverError::Busy)?;
        self.requests.states.remove(ticket);
        Ok(written)
    }

    /// Wait on `host` until every one of `tickets` is answered, taking each
    /// other answer that comes meanwhile, for at most `budget_ns`.
    ///
    /// Once it has waited, the device's interrupt is acknowledged whatever
    /// the outcome.
    ///
    /// # Errors
    ///
    /// [`DriverError::NotFound`] for a ticket naming no request;
    /// [`DriverError::DeviceOffline`] once the budget is spent with one still
    /// out, which stays out; [`DriverError::DeviceFault`] for a wake storm or
    /// an answer the device had no right to post.
    pub fn wait<T: Transport>(
        &mut self,
        transport: &mut T,
        host: &dyn VirtioHost,
        tickets: &[Ticket],
        budget_ns: u64,
    ) -> Result<(), DriverError> {
        let mut out = 0usize;
        for &ticket in tickets {
            if self.answer(ticket)?.is_none() {
                out += 1;
            }
        }
        if out == 0 {
            return Ok(());
        }
        let deadline_ns = host.now_ns().saturating_add(budget_ns);
        let requests = &mut self.requests;
        let waited = await_completions(&mut self.queue, host, budget_ns, deadline_ns, |token| {
            let answered = requests.answer(token)?;
            out -= tickets.iter().filter(|&&ticket| ticket == answered).count();
            Ok(out == 0)
        });
        transport.ack_interrupt();
        waited.map(|_| ())
    }

    /// Never return the rings to their pool: the device may still be using
    /// them.
    pub fn withhold(&mut self) {
        self.queue.withhold();
    }
}

#[cfg(test)]
#[path = "ticket_tests.rs"]
mod tests;
