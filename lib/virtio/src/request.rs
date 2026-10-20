//! One batch of requests at a time on a split virtqueue.

use crate::host::{CompletionSignal, VirtioHost};
use crate::queue::{ChainSegment, SplitQueue, UsedToken};
use crate::transport::{Transport, VirtioError};
use tairix_abi::DriverError;

/// Most advisory completion wakes in a row that bring no completion a batch
/// tolerates before failing closed.
///
/// A healthy device posts its completions within a wake or two of the notify:
/// an early wake whose used-ring write is not yet visible, or a wake for a
/// shared line's other queue, costs one re-read. A count far above that turns
/// a storm of wakes with no completion — a stuck or mis-routed shared
/// interrupt — into a deterministic fault well before the batch's deadline
/// would.
pub const MAX_COMPLETION_WAKES: u32 = 1024;

/// A split virtqueue that carries one batch of requests at a time.
///
/// A batch the device leaves unanswered is failed to its caller, but each of
/// its chains — and every buffer the chain names — stays the device's until
/// the device hands it back. Until [`Self::settle`] sees the last of them come
/// back no later batch is published, so a late completion is never taken for a
/// later request's, and no request reuses memory the device may still be
/// using.
pub struct RequestQueue {
    queue: SplitQueue,
    abandoned: Option<Abandoned>,
}

/// A published batch whose completions were not all seen.
#[derive(Clone, Copy)]
struct Abandoned {
    /// When the device was last notified of it, on the host's clock.
    notified_ns: u64,
    /// The budget its batch was given, and so how often the device is
    /// reminded of it.
    budget_ns: u64,
    /// Chains of it the device still holds.
    out: u16,
}

impl RequestQueue {
    /// Carry requests on `queue`, which holds no chain yet.
    #[must_use]
    pub fn new(queue: SplitQueue) -> Self {
        Self {
            queue,
            abandoned: None,
        }
    }

    /// Whether chains an earlier batch abandoned are still with the device.
    #[must_use]
    pub fn is_abandoned(&self) -> bool {
        self.abandoned.is_some()
    }

    /// Look for the completions of a batch an earlier call abandoned.
    ///
    /// Returns the last completion when the batch's last chain has just come
    /// back, after which the buffers it named are the driver's again, and
    /// `None` when no chain was out. While chains are still out the device is
    /// notified again, in case the first notify was the one it missed — at
    /// most once per the abandoned batch's budget on `host`'s clock, however
    /// often this is called.
    ///
    /// # Errors
    ///
    /// [`DriverError::DeviceOffline`] while a chain is still with the device,
    /// or the mapped queue error for a completion the device had no right to
    /// post.
    pub fn settle<T: Transport>(
        &mut self,
        transport: &mut T,
        host: &dyn VirtioHost,
    ) -> Result<Option<UsedToken>, DriverError> {
        let Some(mut abandoned) = self.abandoned else {
            return Ok(None);
        };
        // The abandoned batch's chains are the only ones out, and the queue
        // answers only for chains it holds.
        let outcome = loop {
            match self.queue.poll_used() {
                Ok(token) => {
                    abandoned.out = abandoned.out.saturating_sub(1);
                    if abandoned.out == 0 {
                        transport.ack_interrupt();
                        self.abandoned = None;
                        return Ok(Some(token));
                    }
                }
                Err(VirtioError::NoCompletion) => {
                    let now_ns = host.now_ns();
                    if now_ns.saturating_sub(abandoned.notified_ns) >= abandoned.budget_ns {
                        abandoned.notified_ns = now_ns;
                        self.queue.kick(transport);
                    }
                    break Err(DriverError::DeviceOffline);
                }
                Err(err) => break Err(err.as_driver_error()),
            }
        };
        self.abandoned = Some(abandoned);
        outcome
    }

    /// Publish `segments`, notify the device, and wait for that chain's own
    /// completion, for at most `budget_ns` in all: a batch of one.
    ///
    /// # Errors
    ///
    /// As [`Self::submit_all_and_wait`].
    pub fn submit_and_wait<T: Transport>(
        &mut self,
        transport: &mut T,
        host: &dyn VirtioHost,
        segments: &[ChainSegment],
        budget_ns: u64,
    ) -> Result<UsedToken, DriverError> {
        self.run(transport, host, &[segments], budget_ns)
    }

    /// Publish one chain per request of `requests`, in order, notify the
    /// device once, and wait for every one of their completions, for at most
    /// `budget_ns` in all. An empty batch publishes nothing.
    ///
    /// Nothing is published unless the queue holds every chain at once, so a
    /// batch is never left half-published. A wake is only advisory — the
    /// used-ring write and the interrupt can be observed in either order, and
    /// a shared line can wake the wait for another queue — so the ring is
    /// drained after every wake, and each wait is given only what is left of
    /// the budget. A wait that times out, or could not be made at all, ends the
    /// batch once the ring has been read again. [`MAX_COMPLETION_WAKES`] ends a
    /// storm of wakes sooner. Nothing is published while the ring holds a
    /// completion: with no chain out, it answers nothing. The device's
    /// interrupt is acknowledged whatever the outcome, so its line is clear
    /// before the next batch re-arms it.
    ///
    /// # Errors
    ///
    /// [`DriverError::DeviceOffline`] while an earlier batch is still out, or
    /// once a wait times out, or the budget is spent, with a chain not yet
    /// back; [`DriverError::Busy`] for a batch the queue cannot hold at once;
    /// [`DriverError::DeviceFault`] for an empty chain, a completion posted
    /// with nothing out, or a wake storm; the mapped queue error for a
    /// completion the device had no right to post. Every failure after the
    /// batch was published leaves the chains not yet back abandoned.
    pub fn submit_all_and_wait<T: Transport>(
        &mut self,
        transport: &mut T,
        host: &dyn VirtioHost,
        requests: &[&[ChainSegment]],
        budget_ns: u64,
    ) -> Result<(), DriverError> {
        if requests.is_empty() {
            return Ok(());
        }
        self.run(transport, host, requests, budget_ns).map(|_| ())
    }

    /// Publish and wait for the non-empty batch `requests`, answering its last
    /// completion.
    fn run<T: Transport>(
        &mut self,
        transport: &mut T,
        host: &dyn VirtioHost,
        requests: &[&[ChainSegment]],
        budget_ns: u64,
    ) -> Result<UsedToken, DriverError> {
        if self.abandoned.is_some() {
            return Err(DriverError::DeviceOffline);
        }
        if requests.iter().any(|segments| segments.is_empty()) {
            return Err(VirtioError::DescriptorTableOverflow.as_driver_error());
        }
        // Every chain takes a descriptor, so this also bounds the chain count.
        let descriptors: usize = requests.iter().map(|segments| segments.len()).sum();
        if descriptors > usize::from(self.queue.free_count()) {
            return Err(VirtioError::QueueFull.as_driver_error());
        }
        if self.queue.poll_used() != Err(VirtioError::NoCompletion) {
            return Err(DriverError::DeviceFault);
        }
        let start_ns = host.now_ns();
        let mut out = 0u16;
        let mut refused = None;
        for segments in requests {
            match self.queue.add_chain(segments) {
                Ok(_) => out += 1,
                Err(err) => {
                    refused = Some(err.as_driver_error());
                    break;
                }
            }
        }
        if out == 0 {
            return Err(refused.unwrap_or(DriverError::DeviceFault));
        }
        self.queue.kick(transport);
        let completed = match refused {
            Some(err) => Err(err),
            None => self.await_completions(
                host,
                budget_ns,
                start_ns.saturating_add(budget_ns),
                &mut out,
            ),
        };
        if completed.is_err() && out != 0 {
            self.abandoned = Some(Abandoned {
                notified_ns: start_ns,
                budget_ns,
                out,
            });
        }
        transport.ack_interrupt();
        completed
    }

    /// Wait until the `out` chains still with the device come back, answering
    /// the last; `out` counts down as they do.
    fn await_completions(
        &mut self,
        host: &dyn VirtioHost,
        budget_ns: u64,
        deadline_ns: u64,
        out: &mut u16,
    ) -> Result<UsedToken, DriverError> {
        let mut left = budget_ns;
        let mut last_wait = None;
        let mut idle_wakes = 0;
        loop {
            loop {
                match self.queue.poll_used() {
                    Ok(token) => {
                        *out -= 1;
                        if *out == 0 {
                            return Ok(token);
                        }
                        idle_wakes = 0;
                    }
                    Err(VirtioError::NoCompletion) => break,
                    Err(err) => return Err(err.as_driver_error()),
                }
            }
            match last_wait {
                // Read only after a wake that left a chain out, so a batch
                // answered on its first wake costs one clock reading.
                Some(CompletionSignal::Fired) => left = deadline_ns.saturating_sub(host.now_ns()),
                // The ring has now been read once more — a completion whose
                // interrupt was lost is already there — so a wait that timed
                // out, or could not be made, ends the batch.
                Some(CompletionSignal::TimedOut) => left = 0,
                None => {}
            }
            if left == 0 {
                return Err(DriverError::DeviceOffline);
            }
            if idle_wakes == MAX_COMPLETION_WAKES {
                return Err(DriverError::DeviceFault);
            }
            idle_wakes += 1;
            last_wait = Some(host.notify_wait(self.queue.index(), left));
        }
    }

    /// Never return the rings to their pool: the device may still be using
    /// them.
    pub fn withhold(&mut self) {
        self.queue.withhold();
    }
}
