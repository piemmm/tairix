//! Operations: every procedure that waits — on a command, a control
//! transfer, a port or a deadline — is a future the host's service step polls
//! only once what it waits on has moved ([`super::route`]). Nothing in here
//! blocks: a wait that has not moved answers `Pending`, and the step serves
//! everything else meanwhile.
//!
//! An operation borrows the engine only between its waits, never across one,
//! so the service step and every other operation see the engine whole; an
//! overlapping borrow fails closed rather than panicking.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::{RefCell, RefMut};
use core::future::{poll_fn, Future};
use core::pin::Pin;
use core::task::{Context, Poll, Waker};

use tairix_abi::{DriverError, RegisterBlock};

use super::route::{CommandId, ControlDone, ControlId, DataStage, OpId, Owner, Settle, Work};
use super::{port, recover, ControlFault, DmaBank, HubEvent, Upstream, UsbDevice};
use crate::trb::Trb;

/// How long a control transfer an operation issues may run: the USB request
/// ceiling.
pub(crate) const CONTROL_CEILING_NS: u64 =
    tairix_abi::usb_urb::USB_CONTROL_TIMEOUT_MAX_MS as u64 * 1_000_000;

/// The engine the service step and every operation share.
pub(crate) struct Engine<H: RegisterBlock, M: DmaBank>(Rc<RefCell<UsbDevice<H, M>>>);

impl<H: RegisterBlock, M: DmaBank> Clone for Engine<H, M> {
    fn clone(&self) -> Self {
        Self(Rc::clone(&self.0))
    }
}

impl<H: RegisterBlock, M: DmaBank> Engine<H, M> {
    /// Borrow the engine.
    ///
    /// # Errors
    ///
    /// [`DriverError::Busy`] for a borrow that would overlap another.
    pub(crate) fn lock(&self) -> Result<RefMut<'_, UsbDevice<H, M>>, DriverError> {
        self.0.try_borrow_mut().map_err(|_| DriverError::Busy)
    }

    /// The instant the service step is running at.
    pub(crate) fn now(&self) -> Result<u64, DriverError> {
        Ok(self.lock()?.now_ns)
    }

    /// Post `command` and await its completion, whatever its code. An
    /// operation that goes away first leaves `settle` to say what the
    /// completion still does.
    ///
    /// # Errors
    ///
    /// [`DriverError::DeviceFault`] once the controller would not stop a hung
    /// ring, or the posting's refusal.
    pub(crate) async fn issue(&self, command: Trb, settle: Settle) -> Result<Trb, DriverError> {
        let id = self.lock()?.post_command(command, settle)?;
        self.await_command(id).await
    }

    /// Await command `id`, posted already, whatever its code; it is orphaned
    /// if the wait is dropped first.
    ///
    /// # Errors
    ///
    /// As [`Self::issue`].
    pub(crate) async fn await_command(&self, id: CommandId) -> Result<Trb, DriverError> {
        let _orphan = OrphanCommand(self, id);
        poll_fn(|_| match self.lock() {
            Ok(mut core) => {
                let waiter = core.waiter();
                core.adopt_command(id, waiter);
                core.take_command(id).map_or(Poll::Pending, Poll::Ready)
            }
            Err(err) => Poll::Ready(Err(err)),
        })
        .await
    }

    /// [`Self::issue`], answering its successful completion.
    ///
    /// # Errors
    ///
    /// [`DriverError::NoBandwidth`] when the controller cannot schedule the
    /// periodic endpoints a Configure Endpoint adds, else
    /// [`DriverError::DeviceFault`] for any other refusal or a dead controller.
    pub(crate) async fn command(&self, command: Trb) -> Result<Trb, DriverError> {
        let event = self.issue(command, Settle::Nothing).await?;
        super::command_outcome(event)
    }

    /// Run one control transfer on `slot`'s pipe, bounded by the request
    /// ceiling: a TD that overruns it is taken back, and the fault says
    /// whether the pipe serves again.
    ///
    /// # Errors
    ///
    /// The transfer's [`ControlFault`].
    pub(crate) async fn transfer(
        &self,
        slot: u8,
        setup: [u8; 8],
        stage: DataStage,
    ) -> Result<ControlDone, ControlFault> {
        let (id, deadline) = {
            let mut core = self.lock().map_err(ControlFault::serving)?;
            let id = core
                .submit_control(slot, setup, stage)
                .map_err(ControlFault::serving)?;
            (id, core.now_ns.saturating_add(CONTROL_CEILING_NS))
        };
        let _orphan = OrphanControl(self, id);
        let mut abandoned = false;
        poll_fn(|_| {
            let mut core = match self.lock() {
                Ok(core) => core,
                Err(err) => return Poll::Ready(Err(ControlFault::serving(err))),
            };
            if let Some(answer) = core.take_control(id) {
                if let Some(op) = core.waiter() {
                    core.forget_timer(deadline, op);
                }
                return Poll::Ready(answer);
            }
            if !abandoned && core.now_ns >= deadline {
                abandoned = true;
                core.abandon_control(id);
                if let Some(answer) = core.take_control(id) {
                    return Poll::Ready(answer);
                }
            }
            if !abandoned {
                if let Err(err) = core.wake_at(deadline) {
                    return Poll::Ready(Err(ControlFault::serving(err)));
                }
            }
            Poll::Pending
        })
        .await
    }

    /// A control transfer with an IN data stage filling `data`, returning how
    /// many bytes the device delivered, or one with none when `data` is
    /// empty.
    ///
    /// # Errors
    ///
    /// The transfer's [`ControlFault`].
    pub(crate) async fn control_in(
        &self,
        slot: u8,
        setup: [u8; 8],
        data: &mut [u8],
    ) -> Result<usize, ControlFault> {
        let stage = if data.is_empty() {
            DataStage::None
        } else {
            DataStage::In(
                u32::try_from(data.len())
                    .map_err(|_| ControlFault::serving(DriverError::LengthOutOfRange))?,
            )
        };
        let done = self.transfer(slot, setup, stage).await?;
        let delivered = data
            .get_mut(..done.data.len())
            .ok_or(ControlFault::serving(DriverError::DeviceFault))?;
        delivered.copy_from_slice(&done.data);
        Ok(done.data.len())
    }

    /// [`Self::control_in`] with the fault's error alone.
    ///
    /// # Errors
    ///
    /// The transfer's error.
    pub(crate) async fn control(
        &self,
        slot: u8,
        setup: [u8; 8],
        data: &mut [u8],
    ) -> Result<usize, DriverError> {
        self.control_in(slot, setup, data)
            .await
            .map_err(|fault| fault.error)
    }

    /// Wait until `at`.
    ///
    /// # Errors
    ///
    /// A borrow that overlapped, or no room to record the timer.
    pub(crate) async fn sleep_until(&self, at: u64) -> Result<(), DriverError> {
        poll_fn(|_| {
            let mut core = match self.lock() {
                Ok(core) => core,
                Err(err) => return Poll::Ready(Err(err)),
            };
            if core.now_ns >= at {
                return Poll::Ready(Ok(()));
            }
            match core.wake_at(at) {
                Ok(()) => Poll::Pending,
                Err(err) => Poll::Ready(Err(err)),
            }
        })
        .await
    }

    /// Wait `us` microseconds.
    ///
    /// # Errors
    ///
    /// As [`Self::sleep_until`].
    pub(crate) async fn sleep_us(&self, us: u32) -> Result<(), DriverError> {
        let at = self.now()?.saturating_add(u64::from(us) * 1_000);
        self.sleep_until(at).await
    }

    /// Wait until `port` reports a change or `at` passes, answering whether
    /// it changed.
    ///
    /// # Errors
    ///
    /// As [`Self::sleep_until`].
    pub(crate) async fn port_change(&self, port: Upstream, at: u64) -> Result<bool, DriverError> {
        poll_fn(|_| {
            let mut core = match self.lock() {
                Ok(core) => core,
                Err(err) => return Poll::Ready(Err(err)),
            };
            if core.take_port_change(port) {
                return Poll::Ready(Ok(true));
            }
            if core.now_ns >= at {
                return Poll::Ready(Ok(false));
            }
            match core.wake_at(at) {
                Ok(()) => Poll::Pending,
                Err(err) => Poll::Ready(Err(err)),
            }
        })
        .await
    }

    /// Wait for the next change on `port` its departure watch has not
    /// looked at. The port's own operation is woken by each one.
    ///
    /// # Errors
    ///
    /// A borrow that overlapped.
    pub(crate) async fn departure_note(&self, port: Upstream) -> Result<(), DriverError> {
        poll_fn(|_| match self.lock() {
            Ok(mut core) => {
                if core.take_departure_note(port) {
                    Poll::Ready(Ok(()))
                } else {
                    Poll::Pending
                }
            }
            Err(err) => Poll::Ready(Err(err)),
        })
        .await
    }

    /// Wait until a root-port scan finds a change after the one numbered
    /// `epoch`, or `at` passes, answering whether one did.
    ///
    /// # Errors
    ///
    /// As [`Self::sleep_until`].
    pub(crate) async fn root_change(&self, epoch: u64, at: u64) -> Result<bool, DriverError> {
        poll_fn(|_| {
            let mut core = match self.lock() {
                Ok(core) => core,
                Err(err) => return Poll::Ready(Err(err)),
            };
            if core.root_epoch() != epoch {
                return Poll::Ready(Ok(true));
            }
            if core.now_ns >= at {
                return Poll::Ready(Ok(false));
            }
            match core.watch_root().and_then(|()| core.wake_at(at)) {
                Ok(()) => Poll::Pending,
                Err(err) => Poll::Ready(Err(err)),
            }
        })
        .await
    }

    /// Take the bus's default address: one device at a time may answer at
    /// address 0, from its port's reset until Address Device moves it
    /// (USB 2.0 §9.1.2). Granted in the order asked.
    ///
    /// # Errors
    ///
    /// A borrow that overlapped, or no room to queue the request.
    pub(crate) async fn address0(&self) -> Result<Held<'_, H, M>, DriverError> {
        self.take_lock(LockName::Address0).await
    }

    /// Take `slot`'s input context: a command reads it when it executes, so
    /// no other command for the slot may stage into it until that one has
    /// completed. Granted in the order asked.
    ///
    /// # Errors
    ///
    /// As [`Self::address0`], or [`DriverError::DeviceFault`] for a slot
    /// with no pipe.
    pub(crate) async fn input_context(&self, slot: u8) -> Result<Held<'_, H, M>, DriverError> {
        self.take_lock(LockName::Input(slot)).await
    }

    async fn take_lock(&self, name: LockName) -> Result<Held<'_, H, M>, DriverError> {
        let ticket = self.lock()?.ticket_lock(name)?.ask()?;
        let mut waiting = LockWait {
            engine: self,
            name,
            ticket,
            granted: false,
        };
        poll_fn(|_| {
            let mut core = match self.lock() {
                Ok(core) => core,
                Err(err) => return Poll::Ready(Err(err)),
            };
            let waiter = core.waiter();
            match core.ticket_lock(name) {
                Ok(lock) => {
                    if lock.try_take(ticket, waiter) {
                        Poll::Ready(Ok(()))
                    } else {
                        Poll::Pending
                    }
                }
                Err(err) => Poll::Ready(Err(err)),
            }
        })
        .await?;
        waiting.granted = true;
        Ok(Held {
            engine: self,
            name,
            ticket,
        })
    }
}

/// Orphans command `.1` on drop: its completion is then settled and dropped.
struct OrphanCommand<'e, H: RegisterBlock, M: DmaBank>(&'e Engine<H, M>, CommandId);

impl<H: RegisterBlock, M: DmaBank> Drop for OrphanCommand<'_, H, M> {
    fn drop(&mut self) {
        if let Ok(mut core) = self.0.lock() {
            core.orphan_command(self.1);
        }
    }
}

/// Orphans control transfer `.1` on drop: a queued one is dropped, a running
/// one's answer is dropped when it comes.
struct OrphanControl<'e, H: RegisterBlock, M: DmaBank>(&'e Engine<H, M>, ControlId);

impl<H: RegisterBlock, M: DmaBank> Drop for OrphanControl<'_, H, M> {
    fn drop(&mut self) {
        if let Ok(mut core) = self.0.lock() {
            core.orphan_control(self.1);
        }
    }
}

/// Which lock a ticket queues for.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum LockName {
    /// The bus's default address.
    Address0,
    /// A slot's input context.
    Input(u8),
}

/// A lock granted in the order asked.
#[derive(Default)]
pub(super) struct TicketLock {
    holder: Option<u64>,
    queue: VecDeque<(u64, Option<OpId>)>,
    next: u64,
}

impl TicketLock {
    /// Queue for the lock, answering the ticket.
    fn ask(&mut self) -> Result<u64, DriverError> {
        self.queue
            .try_reserve(1)
            .map_err(|_| DriverError::OutOfMemory)?;
        let ticket = self.next;
        self.next += 1;
        self.queue.push_back((ticket, None));
        Ok(ticket)
    }

    /// Take the lock for `ticket` if it is free and `ticket` is next, else
    /// record `waiter` to be woken when it may be.
    fn try_take(&mut self, ticket: u64, waiter: Option<OpId>) -> bool {
        if self.holder.is_none() && self.queue.front().is_some_and(|&(next, _)| next == ticket) {
            self.queue.pop_front();
            self.holder = Some(ticket);
            return true;
        }
        if let Some(entry) = self.queue.iter_mut().find(|(next, _)| *next == ticket) {
            entry.1 = waiter;
        }
        false
    }

    /// Release the lock `ticket` holds, or drop its place in the queue;
    /// whom to wake: the next in line, if it may now take it.
    fn release(&mut self, ticket: u64) -> Option<OpId> {
        if self.holder == Some(ticket) {
            self.holder = None;
        } else {
            self.queue.retain(|&(queued, _)| queued != ticket);
        }
        if self.holder.is_some() {
            return None;
        }
        self.queue.front().and_then(|&(_, waiter)| waiter)
    }
}

/// A wait for a lock, leaving the queue if dropped before it was granted.
struct LockWait<'e, H: RegisterBlock, M: DmaBank> {
    engine: &'e Engine<H, M>,
    name: LockName,
    ticket: u64,
    granted: bool,
}

impl<H: RegisterBlock, M: DmaBank> Drop for LockWait<'_, H, M> {
    fn drop(&mut self) {
        if !self.granted {
            release(self.engine, self.name, self.ticket);
        }
    }
}

/// A lock, held until dropped.
pub(crate) struct Held<'e, H: RegisterBlock, M: DmaBank> {
    engine: &'e Engine<H, M>,
    name: LockName,
    ticket: u64,
}

impl<H: RegisterBlock, M: DmaBank> Drop for Held<'_, H, M> {
    fn drop(&mut self) {
        release(self.engine, self.name, self.ticket);
    }
}

fn release<H: RegisterBlock, M: DmaBank>(engine: &Engine<H, M>, name: LockName, ticket: u64) {
    if let Ok(mut core) = engine.lock() {
        let next = core
            .ticket_lock(name)
            .ok()
            .and_then(|lock| lock.release(ticket));
        core.wake(next);
    }
}

/// An operation's future.
type OpFuture<'h> = Pin<Box<dyn Future<Output = ()> + 'h>>;

/// One operation in the host's table.
struct Operation<'h> {
    owner: Owner,
    generation: u32,
    /// Out of the table while it is polled.
    future: Option<OpFuture<'h>>,
}

/// What one service step did.
#[derive(Debug, Default, Eq, PartialEq)]
pub struct Serviced {
    /// What the topology did, oldest first.
    pub topology: Vec<HubEvent>,
    /// Whether a walk of the ports — the bring-up walk, or a retry of the
    /// ports it could not serve — has just finished: every device it found
    /// is served or skipped.
    pub settled: bool,
    /// The soonest instant an operation is due, which the caller's park is
    /// bounded by.
    pub next_deadline: Option<u64>,
}

/// The rounds of routing and polling one service step runs before it yields
/// to the caller, which serves its own work and calls again at once: one
/// device's chain of completions cannot hold the loop.
const SERVICE_ROUNDS: usize = 64;

/// The engine and the operations it runs: what a host-controller driver
/// drives from its event loop.
///
/// [`Self::service`] is the whole of it: it routes every event the
/// controller posted, polls the operations that moved, and answers the
/// soonest deadline the loop's park is bounded by. No call blocks.
pub struct UsbHost<'h, H: RegisterBlock, M: DmaBank> {
    engine: Engine<H, M>,
    ops: Vec<Option<Operation<'h>>>,
    /// The generation each table slot is at, kept past its operation.
    generations: Vec<u32>,
    /// Reused for the ready list, so a quiet step allocates nothing.
    ready: Vec<OpId>,
    /// A walk of the ports is under way and has not yet settled.
    walking: bool,
}

impl<'h, H: RegisterBlock + 'h, M: DmaBank + 'h> UsbHost<'h, H, M> {
    /// Host `device`, running no operation yet.
    pub(super) fn new(device: UsbDevice<H, M>) -> Self {
        Self {
            engine: Engine(Rc::new(RefCell::new(device))),
            ops: Vec::new(),
            generations: Vec::new(),
            ready: Vec::new(),
            walking: false,
        }
    }

    /// The engine itself, for what needs no operation.
    ///
    /// # Errors
    ///
    /// [`DriverError::Busy`] while an operation is being polled.
    pub fn device(&self) -> Result<RefMut<'_, UsbDevice<H, M>>, DriverError> {
        self.engine.lock()
    }

    /// The engine, to read: any number of views may be held at once.
    ///
    /// # Errors
    ///
    /// [`DriverError::Busy`] while an operation is being polled.
    pub fn view(&self) -> Result<core::cell::Ref<'_, UsbDevice<H, M>>, DriverError> {
        self.engine.0.try_borrow().map_err(|_| DriverError::Busy)
    }

    /// The shared engine handle.
    pub(crate) const fn engine(&self) -> &Engine<H, M> {
        &self.engine
    }

    /// How many operations are live.
    #[cfg(test)]
    pub(crate) fn live_operations(&self) -> usize {
        self.ops.iter().flatten().count()
    }

    /// Route what the controller posted at `now_ns`, run every operation
    /// that moved, and report what changed.
    ///
    /// # Errors
    ///
    /// A fault reading the event ring, or the engine borrowed elsewhere.
    pub fn service(&mut self, now_ns: u64) -> Result<Serviced, DriverError> {
        for _ in 0..SERVICE_ROUNDS {
            self.engine.lock()?.advance(now_ns)?;
            self.apply_requests(None);
            let mut ready = core::mem::take(&mut self.ready);
            self.engine.lock()?.take_ready(&mut ready);
            let polled = !ready.is_empty();
            for &id in &ready {
                self.poll(id);
            }
            self.ready = ready;
            // With no operation run the ring is drained and nothing new was
            // posted: what comes next comes with the next interrupt. Past the
            // bound, the next deadline says more is owed and the caller
            // serves its own work and calls again at once.
            if !polled {
                break;
            }
        }
        let mut core = self.engine.lock()?;
        let settled = self.walking && !self.ops.iter().flatten().any(Operation::walks);
        if settled {
            self.walking = false;
        }
        Ok(Serviced {
            topology: core.take_topology(),
            settled,
            next_deadline: core.next_deadline(),
        })
    }

    /// Grow the controller's shared chunk out of `dma`, sized exactly to the
    /// geometry the silicon reports, program it, start the controller with
    /// its completion interrupter enabled, and host it, serving nothing yet.
    /// `budget` bounds the register-handshake polls the silicon dictates.
    ///
    /// The event loop parks on the controller's interrupt, so its line must
    /// be routed and bound before this runs.
    ///
    /// # Errors
    ///
    /// * [`DriverError::OutOfRange`] if the granted chunk's device-visible
    ///   base is zero, not 64-byte aligned, or (when the controller needs
    ///   scratchpad) leaves the scratchpad pages off a controller-page
    ///   boundary.
    /// * [`DriverError::OutOfMemory`] if the bank cannot supply the shared
    ///   chunk.
    /// * [`DriverError::DeviceFault`] if the controller does not start within
    ///   `budget` polls.
    /// * The bank's refusal to place chunks where a controller without AC64
    ///   reaches them.
    pub fn start(xhci: crate::Xhci<H>, dma: M, budget: u32) -> Result<Self, DriverError> {
        Ok(Self::new(UsbDevice::start(xhci, dma, budget)?))
    }

    /// Bring the controller up to serve every device reachable through it:
    /// assert Port Power on every root port (the [`crate::Xhci::open`] reset
    /// cleared `PORTSC`, and a port-power-controlled controller reports a
    /// powered-off port as disconnected, xHCI 1.2 §4.19.1.1), give the ports
    /// the connect window, then reconcile every port, each by its own
    /// operation — a hub is installed, watched and its ports walked in turn,
    /// a leaf device served. [`Serviced::settled`] reports the walk's end.
    ///
    /// Nothing connected is a first-class state, and a device that will not
    /// enumerate is a counted skip ([`UsbDevice::skipped_port_count`]), never
    /// a failure here: throwing a healthy controller away because one device
    /// would not answer takes every other port and every later hot-plug with
    /// it.
    ///
    /// # Errors
    ///
    /// Only a fault of the controller itself powering a port, or no room to
    /// start the walk.
    pub fn bring_up(&mut self) -> Result<(), DriverError> {
        {
            let mut core = self.engine.lock()?;
            core.skipped_ports = 0;
            core.attach_fault = None;
            for port in 1..=core.xhci.max_ports() {
                core.xhci.set_port_power(port)?;
            }
        }
        self.walking = true;
        let engine = self.engine.clone();
        self.spawn(Owner::Walk, port::walk(engine))
    }

    /// Give every port that is not served — on the root hub and on every
    /// watched hub tier — one more look, and with it any device that failed
    /// to enumerate there. A device whose attach failed had its port's
    /// latches consumed, so nothing will wake that port again: without this
    /// it stays dead until it is re-plugged, a poor answer when the device
    /// that lost the boot race is the keyboard. A served port is left alone.
    /// [`Serviced::settled`] reports the retry's end.
    ///
    /// The caller bounds how often this runs; the engine does not retry on
    /// its own, so a permanently broken device can never loop.
    pub fn retry_skipped_ports(&mut self) {
        let mut ports = Vec::new();
        if let Ok(mut core) = self.engine.lock() {
            core.skipped_ports = 0;
            core.attach_fault = None;
            ports = core.unserved_ports();
        }
        self.walk_ports(ports);
    }

    /// Whether an endpoint recovery failed and is owed a paced retry, or a
    /// port a re-enumeration.
    #[must_use]
    pub fn recovery_owed(&self) -> bool {
        self.engine.lock().is_ok_and(|core| core.recovery_owed())
    }

    /// Run every owed recovery once: re-enumerate each port an escalation
    /// tore down, and retry each hub watch and report endpoint whose
    /// recovery failed, climbing its ladder.
    pub fn retry_owed_recoveries(&mut self) {
        let Ok((ports, hubs, devices)) = self.engine.lock().map(|mut core| core.owed_recoveries())
        else {
            return;
        };
        if let Ok(mut core) = self.engine.lock() {
            for port in ports {
                core.ask(Work::Port(port));
            }
        }
        self.apply_requests(None);
        for (index, serial) in hubs {
            let owner = Owner::Hub { index, serial };
            if !self.runs(owner) {
                let engine = self.engine.clone();
                let _ = self.spawn(owner, recover::retry_hub_watch(engine, index, serial));
            }
        }
        for (index, serial) in devices {
            let owner = Owner::Device {
                index,
                serial,
                task: super::route::Task::Reports,
            };
            if !self.runs(owner) {
                let engine = self.engine.clone();
                let _ = self.spawn(owner, recover::retry_reports(engine, index, serial));
            }
        }
    }

    /// Reset the controller and bring it up afresh: every operation ends and
    /// every device is enumerated anew, the controller's reset releasing
    /// everything they held.
    ///
    /// # Errors
    ///
    /// The reset or its re-programming failed.
    pub fn reset_and_reenumerate(&mut self) -> Result<(), DriverError> {
        self.cancel_all();
        self.engine.lock()?.reset_controller()?;
        self.bring_up()
    }

    /// Whether the controller would not stop a hung command ring: what the
    /// service step learnt, read with no register access.
    #[must_use]
    pub fn command_ring_dead(&self) -> bool {
        self.engine
            .lock()
            .is_ok_and(|core| core.command_ring_dead())
    }

    /// Whether the controller has latched a fatal error, halted, or would not
    /// stop a hung command ring: it serves nothing until it is reset. Reads
    /// `USBSTS`.
    pub fn controller_faulted(&mut self) -> bool {
        self.engine
            .lock()
            .map_or(true, |mut core| core.controller_faulted())
    }

    /// Have the port the interface at `index` hangs off looked at again, so
    /// a device that left is torn down. One owed a recovery is left to that
    /// recovery's paced retry, which reads its port itself.
    ///
    /// # Errors
    ///
    /// [`DriverError::NotFound`] for no interface at `index`.
    pub fn confirm_departure(&mut self, index: usize) -> Result<(), DriverError> {
        {
            let mut core = self.engine.lock()?;
            let device = core.device(index).ok_or(DriverError::NotFound)?;
            if device.recovery.owed() {
                return Ok(());
            }
            let upstream = device.upstream();
            core.port_changed(upstream);
        }
        self.apply_requests(None);
        Ok(())
    }

    /// Whether the look [`Self::confirm_departure`] asked for at `index` is
    /// done: no device is served there, or its port's operation has nothing
    /// left to do.
    #[must_use]
    pub fn departure_settled(&self, index: usize) -> bool {
        let Ok(core) = self.engine.lock() else {
            return false;
        };
        let Some(port) = core.device(index).map(super::DeviceState::upstream) else {
            return true;
        };
        !core.port_pending(port) && !self.runs(Owner::Port(port))
    }

    /// Whether an operation is ready to run, so the caller services the
    /// engine again before it parks.
    #[must_use]
    pub fn work_ready(&self) -> bool {
        self.engine.lock().is_ok_and(|core| core.has_ready())
    }

    /// Start the walk of every port: each is reconciled with what it carries
    /// by its own operation. [`Serviced::settled`] reports its end.
    pub(super) fn walk_ports(&mut self, ports: impl IntoIterator<Item = Upstream>) {
        self.walking = true;
        if let Ok(mut core) = self.engine.lock() {
            for port in ports {
                core.ask(Work::Port(port));
            }
        }
        self.apply_requests(None);
    }

    /// Start `future` for `owner`.
    ///
    /// # Errors
    ///
    /// [`DriverError::OutOfMemory`] when the operation cannot be recorded.
    pub(crate) fn spawn(
        &mut self,
        owner: Owner,
        future: impl Future<Output = ()> + 'h,
    ) -> Result<(), DriverError> {
        let future: OpFuture<'h> =
            Box::into_pin(Box::try_new(future).map_err(|_| DriverError::OutOfMemory)?);
        let slot = if let Some(free) = self.ops.iter().position(Option::is_none) {
            free
        } else {
            self.ops
                .try_reserve(1)
                .map_err(|_| DriverError::OutOfMemory)?;
            self.generations
                .try_reserve(1)
                .map_err(|_| DriverError::OutOfMemory)?;
            self.ops.push(None);
            self.generations.push(0);
            self.ops.len() - 1
        };
        let generation = self.generations[slot].wrapping_add(1);
        self.generations[slot] = generation;
        let id = OpId {
            slot: u32::try_from(slot).map_err(|_| DriverError::NoSpace)?,
            generation,
        };
        self.ready
            .try_reserve(self.ops.len().saturating_sub(self.ready.len()))
            .map_err(|_| DriverError::OutOfMemory)?;
        {
            let mut core = self.engine.lock()?;
            core.reserve_wakes(self.ops.len())?;
            core.wake(Some(id));
        }
        self.ops[slot] = Some(Operation {
            owner,
            generation,
            future: Some(future),
        });
        Ok(())
    }

    /// The live operation working for `owner`.
    fn live(&self, owner: Owner) -> Option<OpId> {
        self.ops.iter().enumerate().find_map(|(slot, op)| {
            let op = op.as_ref().filter(|op| op.owner == owner)?;
            Some(OpId {
                slot: u32::try_from(slot).ok()?,
                generation: op.generation,
            })
        })
    }

    /// Whether an operation working for `owner` is live.
    pub(super) fn runs(&self, owner: Owner) -> bool {
        self.live(owner).is_some()
    }

    /// Poll operation `id` once, if it is still the one its slot holds.
    fn poll(&mut self, id: OpId) {
        let Ok(slot) = usize::try_from(id.slot) else {
            return;
        };
        let Some(op) = self.ops.get_mut(slot).and_then(Option::as_mut) else {
            return;
        };
        if op.generation != id.generation {
            return;
        }
        let Some(mut future) = op.future.take() else {
            return;
        };
        if let Ok(mut core) = self.engine.lock() {
            core.wakes.polling = Some(id);
        } else {
            op.future = Some(future);
            return;
        }
        let done = future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_ready();
        if let Ok(mut core) = self.engine.lock() {
            core.wakes.polling = None;
        }
        if done {
            self.ops[slot] = None;
            drop(future);
        } else if let Some(op) = self.ops.get_mut(slot).and_then(Option::as_mut) {
            op.future = Some(future);
        }
        self.apply_requests(Some(id));
    }

    /// Start the work routing found owed, waking an operation already
    /// running for its owner instead, and end what a teardown cancelled —
    /// all but `polled`, the operation that asked.
    fn apply_requests(&mut self, polled: Option<OpId>) {
        // Each cancellation drops futures whose guards may ask for more;
        // bounded, since a guard asks only for what its own teardown owes.
        for _ in 0..SERVICE_ROUNDS {
            let Ok((work, cancels)) = self.engine.lock().map(|mut core| core.take_requests())
            else {
                return;
            };
            if work.is_empty() && cancels.is_empty() {
                return;
            }
            for owner in cancels {
                self.cancel(owner, polled);
            }
            for work in work {
                if let Some(id) = self.live(work.owner()) {
                    if let Ok(mut core) = self.engine.lock() {
                        core.wake(Some(id));
                    }
                    continue;
                }
                let engine = self.engine.clone();
                let started = match work {
                    Work::Port(upstream) => {
                        self.spawn(work.owner(), port::serve_port(engine, upstream))
                    }
                    Work::RecoverPipe { slot, abandoned } => {
                        self.spawn(work.owner(), recover::recover_pipe(engine, slot, abandoned))
                    }
                    Work::RecoverReports { index, serial } => self.spawn(
                        work.owner(),
                        recover::recover_reports(engine, index, serial),
                    ),
                    Work::RecoverBulk {
                        index,
                        serial,
                        pipe,
                        failure,
                    } => self.spawn(
                        work.owner(),
                        recover::recover_bulk(engine, index, serial, pipe, failure),
                    ),
                    Work::ResyncBulk {
                        index,
                        serial,
                        pipe,
                    } => self.spawn(
                        work.owner(),
                        recover::resync_bulk(engine, index, serial, pipe),
                    ),
                    Work::RecoverHubWatch { index, serial } => self.spawn(
                        work.owner(),
                        recover::recover_hub_watch(engine, index, serial),
                    ),
                };
                if started.is_err() {
                    if let Ok(mut core) = self.engine.lock() {
                        core.work_refused(work);
                    }
                }
            }
        }
    }

    /// End every operation `owner` covers but `spared`, dropping its future:
    /// its guards give back what it held.
    fn cancel(&mut self, owner: Owner, spared: Option<OpId>) {
        for slot in 0..self.ops.len() {
            let covered = self.ops[slot].as_ref().is_some_and(|op| {
                owner.covers(op.owner)
                    && spared.is_none_or(|spared| {
                        usize::try_from(spared.slot) != Ok(slot)
                            || spared.generation != op.generation
                    })
            });
            if covered {
                let op = self.ops[slot].take();
                drop(op);
            }
        }
    }

    /// End every operation: the controller is being reset, which takes
    /// everything they held with it.
    pub(super) fn cancel_all(&mut self) {
        if let Ok(mut core) = self.engine.lock() {
            core.resetting = true;
        }
        for op in &mut self.ops {
            drop(op.take());
        }
        self.walking = false;
        if let Ok(mut core) = self.engine.lock() {
            core.resetting = false;
            core.clear_wakes();
        }
    }
}

impl Operation<'_> {
    /// Whether the operation is part of a walk of the ports.
    const fn walks(&self) -> bool {
        matches!(self.owner, Owner::Port(_) | Owner::Walk)
    }
}

impl Owner {
    /// Whether ending what `self` names ends `other` too: a hub's end ends
    /// every operation on its ports, and an interface's end every task of
    /// its.
    fn covers(self, other: Self) -> bool {
        match (self, other) {
            (Self::Hub { index, .. }, Self::Port(Upstream::Hub(hub, _))) => index == hub,
            (
                Self::Hub { index, serial },
                Self::Hub {
                    index: other_index,
                    serial: other_serial,
                },
            )
            | (
                Self::Device { index, serial, .. },
                Self::Device {
                    index: other_index,
                    serial: other_serial,
                    ..
                },
            ) => index == other_index && serial == other_serial,
            _ => self == other,
        }
    }
}
