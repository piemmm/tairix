//! The engine's one consumer of the controller's event ring.
//!
//! Each event is handed to its owner — a posted command, the control
//! transfer running on a slot's pipe, a report, bulk, isochronous or hub
//! status endpoint — and the operation waiting on it is marked ready. No
//! procedure waits on the ring: an operation waits on its own completion,
//! which the service step routes here ([`super::ops`]).

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use tairix_abi::{DriverError, RegisterBlock};
use tairix_inline::ArrayVec;

use super::{
    publish, Abandoned, BulkPipe, ControlFault, ControlPipe, ControlTd, DmaBank, HubEvent, OnHalt,
    TdSpan, Upstream, UsbDevice, CTRL_DATA_LEN, DCI_CONTROL, EVENT_DRAIN_BOUND, HUB_REPORT_LEN,
};
use crate::trb::{self, CompletionCode, Trb, TrbType};

/// How long a command may run once it heads the ring before it is aborted.
/// A controller executes commands in order and none runs more than a few
/// milliseconds, so this only catches a hung one. Command timeouts are the
/// driver's to set (xHCI 1.2 §4.6); five seconds is the figure §4.6.1.2 gives
/// for an abort.
const COMMAND_BUDGET_NS: u64 = 5_000_000_000;

/// How long an aborted ring has to report itself stopped (xHCI 1.2
/// §4.6.1.2) before the controller is taken for dead.
const ABORT_BUDGET_NS: u64 = 5_000_000_000;

/// One operation's identity in the host's table: its slot there and the
/// generation the slot was at, so a wake meant for an operation that has
/// ended never reaches the one that took its slot.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) struct OpId {
    pub(super) slot: u32,
    pub(super) generation: u32,
}

/// Whom an operation works for. A port runs at most one operation, and a
/// teardown cancels exactly the operations acting on what it removes.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Owner {
    /// A root or hub port: its enumeration, teardown and reconciliation.
    Port(Upstream),
    /// A slot's control pipe, being taken back from a TD that failed.
    Pipe(u8),
    /// The interface served at a device-table index, as attached at
    /// `serial`, for one `task`. Ending one ends all its tasks.
    Device {
        index: usize,
        serial: u64,
        task: Task,
    },
    /// The hub at a hub-table index, as attached at `serial`.
    Hub { index: usize, serial: u64 },
    /// The bring-up walk.
    Walk,
}

/// What an operation on a served interface does.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Task {
    /// Recover its interrupt-IN endpoint.
    Reports,
    /// Recover or abandon one of its bulk pipes.
    Bulk(BulkPipe),
    /// Carry out a request its URB holds.
    Request,
}

/// Work the core asks the host to start, once routing found it owed.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum Work {
    /// Reconcile a port with what it now carries.
    Port(Upstream),
    /// Take a slot's control pipe back from the TD that failed on it.
    RecoverPipe { slot: u8, abandoned: Abandoned },
    /// Recover an interface's halted interrupt-IN endpoint.
    RecoverReports { index: usize, serial: u64 },
    /// Recover a halted bulk pipe, answering its TDs with `failure`.
    RecoverBulk {
        index: usize,
        serial: u64,
        pipe: BulkPipe,
        failure: DriverError,
    },
    /// Restart a bulk pipe that completed a TD it was never given.
    ResyncBulk {
        index: usize,
        serial: u64,
        pipe: BulkPipe,
    },
    /// Recover a hub's halted status-change endpoint.
    RecoverHubWatch { index: usize, serial: u64 },
}

impl Work {
    /// The owner the work runs as.
    pub(super) const fn owner(self) -> Owner {
        match self {
            Self::Port(upstream) => Owner::Port(upstream),
            Self::RecoverPipe { slot, .. } => Owner::Pipe(slot),
            Self::RecoverReports { index, serial } => Owner::Device {
                index,
                serial,
                task: Task::Reports,
            },
            Self::RecoverBulk {
                index,
                serial,
                pipe,
                ..
            }
            | Self::ResyncBulk {
                index,
                serial,
                pipe,
            } => Owner::Device {
                index,
                serial,
                task: Task::Bulk(pipe),
            },
            Self::RecoverHubWatch { index, serial } => Owner::Hub { index, serial },
        }
    }
}

/// The readiness the host's executor works from.
#[derive(Default)]
pub(super) struct Wakes {
    /// The operation being polled: a wait registered now is its.
    pub(super) polling: Option<OpId>,
    /// Operations whose wait has moved since they were last polled.
    ready: Vec<OpId>,
    /// One-shot deadlines, each waking its operation once due.
    timers: Vec<(u64, OpId)>,
    /// Work owed, for the host to start.
    work: Vec<Work>,
    /// Owners whose operations a teardown ended.
    cancels: Vec<Owner>,
    /// Ports something changed on since their operation last looked.
    changed_ports: Vec<Upstream>,
    /// Ports whose attach watches for its device leaving, each with whether
    /// a change landed since the watch last looked.
    departures: Vec<(Upstream, bool)>,
    /// What the topology did, oldest first, for the host to report.
    topology: Vec<HubEvent>,
    /// Counts the root-port scans that found a change, for a wait on one.
    root_epoch: u64,
    /// Operations waiting for the next root-port change.
    root_watchers: Vec<OpId>,
}

/// A command's ticket, stable from its posting even while it waits for room
/// on the ring.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct CommandId(u64);

/// One command, posted in ring order.
struct Posted {
    id: CommandId,
    trb: Trb,
    /// The TRB address its completion names, once it is on the ring.
    address: Option<u64>,
    waiter: Option<OpId>,
    answer: Option<Trb>,
    /// Whether its waiter went away, so its answer is settled and dropped.
    orphaned: bool,
    settle: Settle,
}

/// What a command still does when it completes with no one left to take its
/// answer.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Settle {
    /// Nothing: its outcome was its waiter's concern alone.
    Nothing,
    /// An Enable Slot: a slot it hands out to no one is disabled again.
    ReturnSlot,
}

/// The posted commands and the ring's abort state.
#[derive(Default)]
pub(super) struct Commands {
    posted: VecDeque<Posted>,
    next_id: u64,
    /// When the command at the ring's head is aborted, set as it got there.
    head_deadline: Option<u64>,
    /// When an aborted ring must have reported itself stopped by.
    aborting: Option<u64>,
    /// The controller would not stop its ring: nothing more is posted until
    /// it is reset.
    dead: bool,
}

/// A control transfer's ticket: its place in the request table and the
/// serial that tells it from a later request in the same place.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) struct ControlId {
    at: usize,
    serial: u64,
}

/// The data stage a control transfer moves.
pub(crate) enum DataStage {
    /// None: SETUP and the status stage alone.
    None,
    /// IN, of up to this many bytes.
    In(u32),
    /// OUT, carrying these bytes.
    Out(Vec<u8>),
}

impl DataStage {
    fn len(&self) -> Result<u32, DriverError> {
        match self {
            Self::None => Ok(0),
            Self::In(len) => Ok(*len),
            Self::Out(bytes) => {
                u32::try_from(bytes.len()).map_err(|_| DriverError::LengthOutOfRange)
            }
        }
    }
}

/// A finished control transfer: what its data stage moved, and an IN stage's
/// bytes.
pub(crate) struct ControlDone {
    pub(crate) transferred: u32,
    pub(crate) data: Vec<u8>,
}

/// One control transfer, queued on its slot's pipe until it runs.
pub(super) struct ControlRequest {
    serial: u64,
    slot: u8,
    setup: [u8; 8],
    stage: DataStage,
    waiter: Option<OpId>,
    answer: Option<Result<ControlDone, ControlFault>>,
    /// Whether whoever asked went away, so its answer is dropped.
    orphaned: bool,
}

/// Where a pipe stands.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum PipeState {
    /// Running its queue, one TD at a time.
    Serving,
    /// Being taken back from a TD that failed; its queue waits.
    Recovering,
    /// It could not be taken back: the controller may still own a TD on it,
    /// so it runs nothing more.
    Dead,
}

/// The TD a pipe is running, and what it has reported so far.
pub(super) struct RunningTd {
    id: ControlId,
    td: ControlTd,
    residual: u32,
}

/// A pipe's queue: the transfers waiting, the one running, the one whose
/// failure a recovery is settling.
#[derive(Default)]
pub(super) struct PipeQueue {
    waiting: VecDeque<ControlId>,
    running: Option<RunningTd>,
    /// The request whose TD failed, answered with this fault and the
    /// completion code that ended it once the recovery says whether the pipe
    /// serves again.
    failed: Option<(ControlId, DriverError, u8)>,
}

impl<H: RegisterBlock, M: DmaBank> UsbDevice<H, M> {
    /// Mark the operation `waiter` names ready to be polled.
    pub(super) fn wake(&mut self, waiter: Option<OpId>) {
        if let Some(op) = waiter {
            if !self.wakes.ready.contains(&op) && self.wakes.ready.try_reserve(1).is_ok() {
                self.wakes.ready.push(op);
            }
        }
    }

    /// The operation a wait registered now belongs to.
    pub(super) const fn waiter(&self) -> Option<OpId> {
        self.wakes.polling
    }

    /// Wake the polling operation at `at`. Idempotent per deadline.
    ///
    /// # Errors
    ///
    /// [`DriverError::OutOfMemory`] when the timer cannot be recorded.
    pub(super) fn wake_at(&mut self, at: u64) -> Result<(), DriverError> {
        let Some(op) = self.wakes.polling else {
            return Ok(());
        };
        if self.wakes.timers.contains(&(at, op)) {
            return Ok(());
        }
        self.wakes
            .timers
            .try_reserve(1)
            .map_err(|_| DriverError::OutOfMemory)?;
        self.wakes.timers.push((at, op));
        Ok(())
    }

    /// Forget the polling operation's timer at `at`: its wait ended.
    pub(super) fn forget_timer(&mut self, at: u64, op: OpId) {
        self.wakes.timers.retain(|&timer| timer != (at, op));
    }

    /// Ask the host to start `work`, once.
    pub(super) fn ask(&mut self, work: Work) {
        if !self.wakes.work.contains(&work) && self.wakes.work.try_reserve(1).is_ok() {
            self.wakes.work.push(work);
        }
    }

    /// Ask the host to end every operation working for `owner`.
    pub(super) fn cancel(&mut self, owner: Owner) {
        if !self.wakes.cancels.contains(&owner) && self.wakes.cancels.try_reserve(1).is_ok() {
            self.wakes.cancels.push(owner);
        }
    }

    /// Note a change on `port` for its operation, and ask for one.
    pub(super) fn port_changed(&mut self, port: Upstream) {
        if !self.wakes.changed_ports.contains(&port)
            && self.wakes.changed_ports.try_reserve(1).is_ok()
        {
            self.wakes.changed_ports.push(port);
        }
        for (watched, changed) in &mut self.wakes.departures {
            *changed |= *watched == port;
        }
        self.ask(Work::Port(port));
    }

    /// Whether `port` has a change or a reconcile owed that its operation
    /// has not yet taken up.
    pub(super) fn port_pending(&self, port: Upstream) -> bool {
        self.wakes.changed_ports.contains(&port) || self.wakes.work.contains(&Work::Port(port))
    }

    /// Start watching `port` for its device leaving.
    ///
    /// # Errors
    ///
    /// [`DriverError::OutOfMemory`] when the watch cannot be recorded.
    pub(super) fn watch_departure(&mut self, port: Upstream) -> Result<(), DriverError> {
        if let Some(watch) = self
            .wakes
            .departures
            .iter_mut()
            .find(|(watched, _)| *watched == port)
        {
            watch.1 = false;
            return Ok(());
        }
        self.wakes
            .departures
            .try_reserve(1)
            .map_err(|_| DriverError::OutOfMemory)?;
        self.wakes.departures.push((port, false));
        Ok(())
    }

    /// Whether `port` changed since its departure watch last looked,
    /// consuming the note.
    pub(super) fn take_departure_note(&mut self, port: Upstream) -> bool {
        self.wakes
            .departures
            .iter_mut()
            .find(|(watched, _)| *watched == port)
            .is_some_and(|(_, changed)| core::mem::take(changed))
    }

    /// Stop watching `port` for its device leaving.
    pub(super) fn unwatch_departure(&mut self, port: Upstream) {
        if let Some(at) = self
            .wakes
            .departures
            .iter()
            .position(|(watched, _)| *watched == port)
        {
            self.wakes.departures.swap_remove(at);
        }
    }

    /// Whether `port` changed since its operation last asked, consuming the
    /// note.
    pub(super) fn take_port_change(&mut self, port: Upstream) -> bool {
        let before = self.wakes.changed_ports.len();
        self.wakes.changed_ports.retain(|&changed| changed != port);
        self.wakes.changed_ports.len() != before
    }

    /// Record what the topology did, for the host to report. One that cannot
    /// be recorded is lost to the report alone: the table is the truth.
    pub(super) fn note_topology(&mut self, event: HubEvent) {
        if self.wakes.topology.try_reserve(1).is_ok() {
            self.wakes.topology.push(event);
        }
    }

    /// Move the ready operations into `into`, leaving none.
    pub(super) fn take_ready(&mut self, into: &mut Vec<OpId>) {
        into.clear();
        core::mem::swap(into, &mut self.wakes.ready);
    }

    /// Take the owed work and the cancellations.
    pub(super) fn take_requests(&mut self) -> (Vec<Work>, Vec<Owner>) {
        (
            core::mem::take(&mut self.wakes.work),
            core::mem::take(&mut self.wakes.cancels),
        )
    }

    /// Take what the topology did since the last call.
    pub(super) fn take_topology(&mut self) -> Vec<HubEvent> {
        core::mem::take(&mut self.wakes.topology)
    }

    /// Drop every wake, timer and request: the operations they name are
    /// gone with a controller reset.
    pub(super) fn clear_wakes(&mut self) {
        self.wakes = Wakes::default();
    }

    /// The soonest instant something here is due: now while an operation is
    /// ready, else a timer, or the command ring's head or abort.
    pub(super) fn next_deadline(&self) -> Option<u64> {
        if self.has_ready() {
            return Some(self.now_ns);
        }
        let timers = self.wakes.timers.iter().map(|&(at, _)| at).min();
        let commands = self.commands.aborting.or(self.commands.head_deadline);
        [timers, commands].into_iter().flatten().min()
    }

    /// Advance everything to `now`: route the events the controller posted,
    /// fire due timers and command deadlines, scan the root ports a change
    /// latched on and service the hubs that reported.
    ///
    /// # Errors
    ///
    /// A fault reading the event ring.
    pub(super) fn advance(&mut self, now: u64) -> Result<(), DriverError> {
        self.now_ns = now;
        for _ in 0..EVENT_DRAIN_BOUND {
            let Some(event) = self.poll_event()? else {
                break;
            };
            self.route_event(event);
        }
        self.fire_timers();
        self.check_commands();
        self.service_hub_reports();
        self.scan_root_ports();
        self.ensure_every_report_armed();
        Ok(())
    }

    fn fire_timers(&mut self) {
        let now = self.now_ns;
        let mut at = 0;
        while at < self.wakes.timers.len() {
            let (deadline, op) = self.wakes.timers[at];
            if deadline <= now {
                self.wakes.timers.swap_remove(at);
                self.wake(Some(op));
            } else {
                at += 1;
            }
        }
    }

    /// Hand one event to its owner. An event nothing owns — a completion for
    /// a slot just freed, or for a TD a recovery took back — is dropped as
    /// stale: the shared ring is no security boundary, and every owner
    /// validates what it is handed.
    fn route_event(&mut self, event: Trb) {
        match event.trb_type() {
            Ok(TrbType::CommandCompletion) => self.on_command_completion(event),
            Ok(TrbType::TransferEvent) => self.on_transfer_event(event),
            // `poll_event` armed the root-port scan.
            _ => {}
        }
    }

    fn on_transfer_event(&mut self, event: Trb) {
        if event.endpoint_id() == DCI_CONTROL && self.on_control_event(event) {
            return;
        }
        if let Some(at) = self.iso_async_index(event) {
            self.capture_iso_event(at, event);
            return;
        }
        if let Some(index) = self.report_async_index(event) {
            self.capture_report_event(index, event);
            return;
        }
        if let Some(hub_index) = self.hub_async_index(event) {
            if let Some(hub) = self.hub_mut(hub_index) {
                // One status transfer is armed at a time, so a second
                // completion before the first is serviced names no TD.
                hub.pending.get_or_insert(event);
            }
            return;
        }
        if let Some(index) = self.bulk_async_index(event) {
            if let Some(device) = self.device_mut(index) {
                // Sized to both rings' in-flight bound: one that does not fit
                // completes a TD never queued, and is dropped as stale.
                let _ = device.pending_bulk.try_push_back(event);
            }
        }
    }

    /// Service every hub whose status-change transfer completed: re-arm the
    /// endpoint first, so a change while the ports are serviced is reported
    /// again, then hand each changed port to its operation. A transfer that
    /// halted the endpoint owes its recovery instead.
    fn service_hub_reports(&mut self) {
        for hub_index in 0..self.hubs.len() {
            let Some((completion, serial)) = self
                .hub_mut(hub_index)
                .and_then(|hub| Some((hub.pending.take()?, hub.serial)))
            else {
                continue;
            };
            if let Some(ring) = self
                .hub_mut(hub_index)
                .and_then(|hub| hub.int_ring.as_mut())
            {
                let _ = ring.retire_one();
            }
            let succeeded = matches!(
                completion.completion_code(),
                Ok(CompletionCode::Success | CompletionCode::ShortPacket)
            );
            if !succeeded {
                let recover = self
                    .hub_mut(hub_index)
                    .is_some_and(|hub| hub.watch.halted() == OnHalt::Recover);
                if recover {
                    self.ask(Work::RecoverHubWatch {
                        index: hub_index,
                        serial,
                    });
                }
                continue;
            }
            if let Some(hub) = self.hub_mut(hub_index) {
                hub.watch.completed();
            }
            let Some((ports, report_off)) = self
                .hub(hub_index)
                .map(|hub| (hub.num_ports, hub.region.report))
            else {
                continue;
            };
            let mut bitmap = [0u8; HUB_REPORT_LEN];
            let read = self.dma.read(report_off, &mut bitmap);
            if self.rearm_hub_watch(hub_index).is_err() {
                let recover = self
                    .hub_mut(hub_index)
                    .is_some_and(|hub| hub.watch.halted() == OnHalt::Recover);
                if recover {
                    self.ask(Work::RecoverHubWatch {
                        index: hub_index,
                        serial,
                    });
                }
            }
            for port in 1..=ports {
                let (byte, bit) = (usize::from(port / 8), port % 8);
                // A report the bank could not read names every port.
                let changed =
                    read.is_err() || bitmap.get(byte).is_some_and(|byte| byte & (1 << bit) != 0);
                if changed {
                    self.port_changed(Upstream::Hub(hub_index, port));
                }
            }
        }
    }

    /// Re-arm the hub at `hub_index`'s status-change transfer and ring its
    /// doorbell.
    pub(super) fn rearm_hub_watch(&mut self, hub_index: usize) -> Result<(), DriverError> {
        self.arm_hub_report(hub_index)?;
        let (slot, dci) = self
            .hub(hub_index)
            .map(|hub| (hub.slot, hub.int_dci))
            .ok_or(DriverError::NotFound)?;
        self.xhci.ring_doorbell(slot, u32::from(dci))
    }

    /// Scan the root ports once a change latched, handing each port that
    /// latched one to its operation. The operation reads and clears the
    /// port's latches itself.
    fn scan_root_ports(&mut self) {
        if !self.root_change_pending {
            return;
        }
        self.root_change_pending = false;
        let mut changed = false;
        for port in 1..=self.xhci.max_ports() {
            if self
                .xhci
                .port_status(port)
                .is_ok_and(|status| status.changes() != 0)
            {
                changed = true;
                self.port_changed(Upstream::Root(port));
            }
        }
        if changed {
            self.wakes.root_epoch = self.wakes.root_epoch.wrapping_add(1);
            for op in core::mem::take(&mut self.wakes.root_watchers) {
                self.wake(Some(op));
            }
        }
    }

    /// The count of root-port scans that found a change.
    pub(super) const fn root_epoch(&self) -> u64 {
        self.wakes.root_epoch
    }

    /// Wake the polling operation at the next root-port change.
    ///
    /// # Errors
    ///
    /// [`DriverError::OutOfMemory`] when the watch cannot be recorded.
    pub(super) fn watch_root(&mut self) -> Result<(), DriverError> {
        let Some(op) = self.wakes.polling else {
            return Ok(());
        };
        if self.wakes.root_watchers.contains(&op) {
            return Ok(());
        }
        self.wakes
            .root_watchers
            .try_reserve(1)
            .map_err(|_| DriverError::OutOfMemory)?;
        self.wakes.root_watchers.push(op);
        Ok(())
    }

    /// Whether an operation is ready to run, or work or a cancellation is
    /// owed: the service step has something to do.
    pub(super) fn has_ready(&self) -> bool {
        !(self.wakes.ready.is_empty()
            && self.wakes.work.is_empty()
            && self.wakes.cancels.is_empty())
    }

    /// Room for `ops` operations' wakes and timers, so recording one never
    /// allocates.
    ///
    /// # Errors
    ///
    /// [`DriverError::OutOfMemory`] when the room cannot be had.
    pub(super) fn reserve_wakes(&mut self, ops: usize) -> Result<(), DriverError> {
        let ready = ops.saturating_sub(self.wakes.ready.len());
        self.wakes
            .ready
            .try_reserve(ready)
            .map_err(|_| DriverError::OutOfMemory)?;
        let timers = ops
            .saturating_mul(2)
            .saturating_sub(self.wakes.timers.len());
        self.wakes
            .timers
            .try_reserve(timers)
            .map_err(|_| DriverError::OutOfMemory)
    }

    /// `work` could not be started: what it was owed fails as its own
    /// failure would.
    pub(super) fn work_refused(&mut self, work: Work) {
        match work {
            Work::Port(_) => self.skipped_ports = self.skipped_ports.saturating_add(1),
            Work::RecoverPipe { slot, .. } => {
                self.pipe_recovered(slot, Err(DriverError::OutOfMemory));
            }
            Work::RecoverReports { index, .. } => {
                if let Some(device) = self.device_mut(index) {
                    device.recovery.failed();
                }
            }
            Work::RecoverBulk { index, pipe, .. } | Work::ResyncBulk { index, pipe, .. } => {
                self.bulk_recovered(index, pipe, Err(DriverError::OutOfMemory));
            }
            Work::RecoverHubWatch { index, .. } => {
                if let Some(hub) = self.hub_mut(index) {
                    hub.watch.failed();
                }
            }
        }
    }

    /// The lock `name` names.
    ///
    /// # Errors
    ///
    /// [`DriverError::DeviceFault`] for a slot's input context with no pipe
    /// open.
    pub(super) fn ticket_lock(
        &mut self,
        name: super::ops::LockName,
    ) -> Result<&mut super::ops::TicketLock, DriverError> {
        match name {
            super::ops::LockName::Address0 => Ok(&mut self.address0),
            super::ops::LockName::Input(slot) => Ok(&mut self.pipe_mut(slot)?.input),
        }
    }

    /// Keep every report endpoint armed to depth, as a capture does after
    /// each report it takes.
    fn ensure_every_report_armed(&mut self) {
        for index in 0..self.devices.len() {
            let _ = self.ensure_reports_armed(index);
        }
    }

    /// Post `command`, answering its ticket. It goes on the ring at once, or
    /// as soon as the ring has room; `settle` says what its completion still
    /// does if no one is left to take it.
    ///
    /// # Errors
    ///
    /// [`DriverError::DeviceFault`] once the controller would not stop a hung
    /// ring, [`DriverError::OutOfMemory`] when it cannot be recorded, or a DMA
    /// fault writing the ring.
    pub(super) fn post_command(
        &mut self,
        command: Trb,
        settle: Settle,
    ) -> Result<CommandId, DriverError> {
        if self.commands.dead {
            return Err(DriverError::DeviceFault);
        }
        self.commands
            .posted
            .try_reserve(1)
            .map_err(|_| DriverError::OutOfMemory)?;
        let id = CommandId(self.commands.next_id);
        self.commands.next_id += 1;
        self.commands.posted.push_back(Posted {
            id,
            trb: command,
            address: None,
            waiter: self.wakes.polling,
            answer: None,
            orphaned: false,
            settle,
        });
        self.write_commands()?;
        Ok(id)
    }

    /// Put the posted commands waiting for room on the ring, in order, and
    /// tell the controller. Nothing goes on while the ring is being aborted:
    /// its restart takes them.
    fn write_commands(&mut self) -> Result<(), DriverError> {
        if self.commands.aborting.is_some() {
            return Ok(());
        }
        let idle = self.command_ring.in_flight() == 0;
        let mut wrote = false;
        for at in 0..self.commands.posted.len() {
            let Some(posted) = self.commands.posted.get(at) else {
                break;
            };
            if posted.address.is_some() {
                continue;
            }
            let outcome = match self.command_ring.push(posted.trb) {
                Ok(outcome) => outcome,
                Err(DriverError::Busy) => break,
                Err(err) => return Err(err),
            };
            publish(
                &mut self.dma,
                self.layout.command_ring,
                self.command_ring.link_slot(),
                &outcome,
            )?;
            if let Some(posted) = self.commands.posted.get_mut(at) {
                posted.address = Some(outcome.address);
            }
            wrote = true;
        }
        if wrote {
            if idle {
                self.commands.head_deadline = Some(self.now_ns.saturating_add(COMMAND_BUDGET_NS));
            }
            self.xhci.ring_doorbell(0, 0)?;
        }
        Ok(())
    }

    /// Take command `id`'s completion once it arrived.
    ///
    /// # Errors
    ///
    /// [`DriverError::DeviceFault`] once the controller would not stop a hung
    /// ring: no answer will come.
    pub(super) fn take_command(&mut self, id: CommandId) -> Option<Result<Trb, DriverError>> {
        let at = self
            .commands
            .posted
            .iter()
            .position(|posted| posted.id == id)?;
        if let Some(answer) = self
            .commands
            .posted
            .get(at)
            .and_then(|posted| posted.answer)
        {
            self.commands.posted.remove(at);
            return Some(Ok(answer));
        }
        if self.commands.dead {
            self.commands.posted.remove(at);
            return Some(Err(DriverError::DeviceFault));
        }
        None
    }

    /// Make `waiter` the operation command `id` wakes, when it is one.
    pub(super) fn adopt_command(&mut self, id: CommandId, waiter: Option<OpId>) {
        if waiter.is_none() {
            return;
        }
        if let Some(posted) = self
            .commands
            .posted
            .iter_mut()
            .find(|posted| posted.id == id)
        {
            posted.waiter = waiter;
        }
    }

    /// Command `id`'s waiter went away: its completion is settled when it
    /// comes, and dropped.
    pub(super) fn orphan_command(&mut self, id: CommandId) {
        let Some(at) = self
            .commands
            .posted
            .iter()
            .position(|posted| posted.id == id)
        else {
            return;
        };
        let answered = self
            .commands
            .posted
            .get(at)
            .and_then(|posted| posted.answer);
        match answered {
            Some(answer) => {
                if let Some(posted) = self.commands.posted.remove(at) {
                    self.settle(posted.settle, answer);
                }
            }
            None => {
                if let Some(posted) = self.commands.posted.get_mut(at) {
                    posted.orphaned = true;
                    posted.waiter = None;
                }
            }
        }
    }

    fn on_command_completion(&mut self, event: Trb) {
        if event.completion_code() == Ok(CompletionCode::CommandRingStopped) {
            self.command_ring_stopped();
            return;
        }
        let Some(at) =
            self.commands.posted.iter().position(|posted| {
                posted.answer.is_none() && posted.address == Some(event.parameter)
            })
        else {
            return;
        };
        // The controller consumed the TRB whatever its code says.
        let _ = self.command_ring.retire_one();
        let (waiter, orphaned, settle, id) = match self.commands.posted.get_mut(at) {
            Some(posted) => {
                posted.answer = Some(event);
                (posted.waiter, posted.orphaned, posted.settle, posted.id)
            }
            None => return,
        };
        self.settle_awaited_disable(id, event);
        if orphaned {
            self.commands.posted.remove(at);
            self.settle(settle, event);
        } else {
            self.wake(waiter);
        }
        self.commands.head_deadline = (self.command_ring.in_flight() != 0)
            .then(|| self.now_ns.saturating_add(COMMAND_BUDGET_NS));
        let _ = self.write_commands();
    }

    /// The ring stopped after an abort: what is left on it restarts.
    fn command_ring_stopped(&mut self) {
        if self.commands.aborting.take().is_none() {
            return;
        }
        if self.command_ring.in_flight() != 0 {
            self.commands.head_deadline = Some(self.now_ns.saturating_add(COMMAND_BUDGET_NS));
            let _ = self.xhci.ring_doorbell(0, 0);
        }
        let _ = self.write_commands();
    }

    /// Abort the ring's head once it overran its budget, and take the
    /// controller for dead once an abort went unanswered: every waiter is
    /// then woken to its failure.
    fn check_commands(&mut self) {
        if self.commands.dead {
            return;
        }
        let now = self.now_ns;
        if let Some(at) = self.commands.aborting {
            if now >= at {
                self.commands.dead = true;
                self.commands.aborting = None;
                let waiters: Vec<Option<OpId>> = self
                    .commands
                    .posted
                    .iter()
                    .map(|posted| posted.waiter)
                    .collect();
                for waiter in waiters {
                    self.wake(waiter);
                }
            }
            return;
        }
        if self.commands.head_deadline.is_some_and(|at| now >= at) {
            self.commands.head_deadline = None;
            self.commands.aborting = Some(now.saturating_add(ABORT_BUDGET_NS));
            let aborted = self
                .command_ring
                .after_head()
                .and_then(|(resume, cycle)| self.xhci.abort_command_ring(resume, cycle));
            if aborted.is_err() {
                self.commands.aborting = Some(now);
            }
        }
    }

    /// Whether the controller would not stop a hung command ring.
    pub(super) const fn command_ring_dead(&self) -> bool {
        self.commands.dead
    }

    fn settle(&mut self, settle: Settle, answer: Trb) {
        match settle {
            Settle::Nothing => {}
            Settle::ReturnSlot => {
                let slot = answer.slot_id();
                if answer.completion_code() == Ok(CompletionCode::Success) && slot != 0 {
                    let _ = self.post_disable_slot(slot);
                }
            }
        }
    }

    /// Post Disable Slot for `slot` (xHCI §6.4.3.3), closing its pipe first:
    /// nothing more runs on a slot being disabled. The chunks recorded
    /// against the ticket are returned when it confirms
    /// ([`Self::settle_awaited_disable`]).
    ///
    /// # Errors
    ///
    /// As [`Self::post_command`].
    pub(super) fn post_disable_slot(&mut self, slot: u8) -> Result<CommandId, DriverError> {
        self.close_pipe(slot);
        self.post_command(
            Trb::new(TrbType::DisableSlot, 0, 0, trb::control_slot(slot)),
            Settle::Nothing,
        )
    }

    /// Submit a control transfer on `slot`'s pipe, answering its ticket. It
    /// runs once every transfer queued before it has.
    ///
    /// # Errors
    ///
    /// [`DriverError::LengthOutOfRange`] for a data stage past the pipe's
    /// buffer, [`DriverError::DeviceFault`] for a slot with no pipe or one
    /// that is dead, [`DriverError::OutOfMemory`] when it cannot be recorded.
    pub(super) fn submit_control(
        &mut self,
        slot: u8,
        setup: [u8; 8],
        stage: DataStage,
    ) -> Result<ControlId, DriverError> {
        if stage.len()? as usize > CTRL_DATA_LEN {
            return Err(DriverError::LengthOutOfRange);
        }
        if self.pipe(slot)?.state == PipeState::Dead {
            return Err(DriverError::DeviceFault);
        }
        let at = if let Some(free) = self.controls.iter().position(Option::is_none) {
            free
        } else {
            self.controls
                .try_reserve(1)
                .map_err(|_| DriverError::OutOfMemory)?;
            self.controls.push(None);
            self.controls.len() - 1
        };
        let pipe = self.pipe_mut(slot)?;
        pipe.queue
            .waiting
            .try_reserve(1)
            .map_err(|_| DriverError::OutOfMemory)?;
        let serial = self.control_serial;
        self.control_serial += 1;
        let id = ControlId { at, serial };
        self.pipe_mut(slot)?.queue.waiting.push_back(id);
        self.controls[at] = Some(ControlRequest {
            serial,
            slot,
            setup,
            stage,
            waiter: self.wakes.polling,
            answer: None,
            orphaned: false,
        });
        self.kick_pipe(slot);
        Ok(id)
    }

    fn control_mut(&mut self, id: ControlId) -> Option<&mut ControlRequest> {
        self.controls
            .get_mut(id.at)
            .and_then(Option::as_mut)
            .filter(|request| request.serial == id.serial)
    }

    /// Take control transfer `id`'s answer once it has one. `None` while it
    /// waits or runs; a ticket no request answers to is a closed pipe's.
    pub(super) fn take_control(
        &mut self,
        id: ControlId,
    ) -> Option<Result<ControlDone, ControlFault>> {
        let Some(request) = self.control_mut(id) else {
            return Some(Err(ControlFault::dead()));
        };
        let answer = request.answer.take()?;
        self.controls[id.at] = None;
        Some(answer)
    }

    /// Whoever asked for control transfer `id` went away: a queued one is
    /// dropped, a running one's answer is dropped when it comes.
    pub(super) fn orphan_control(&mut self, id: ControlId) {
        let Some(request) = self.control_mut(id) else {
            return;
        };
        if request.answer.is_some() {
            self.controls[id.at] = None;
            return;
        }
        request.orphaned = true;
        request.waiter = None;
        let slot = request.slot;
        let queued = self.pipe_mut(slot).is_ok_and(|pipe| {
            let before = pipe.queue.waiting.len();
            pipe.queue.waiting.retain(|&waiting| waiting != id);
            pipe.queue.waiting.len() != before
        });
        if queued {
            self.controls[id.at] = None;
        }
    }

    /// The slot of control transfer `id`, and where it lies on that slot's
    /// pipe, while it runs.
    pub(super) fn running_control_span(&self, id: ControlId) -> Option<(u8, TdSpan)> {
        let slot = self.controls.get(id.at)?.as_ref()?.slot;
        let pipe = self.pipe(slot).ok()?;
        let running = pipe.queue.running.as_ref()?;
        if running.id != id {
            return None;
        }
        let td = running.td;
        let mut trbs = ArrayVec::new();
        for trb in [Some(td.setup), td.data, Some(td.status)]
            .into_iter()
            .flatten()
        {
            let _ = trbs.try_push(trb);
        }
        Some((slot, TdSpan::on(&pipe.ring, trbs)?))
    }

    /// Give up on control transfer `id`, whose deadline passed: a queued one
    /// is answered at once, the pipe untouched; a running one's TD is taken
    /// back, and it is answered once the recovery says whether the pipe
    /// serves again.
    pub(super) fn abandon_control(&mut self, id: ControlId) {
        let Some(slot) = self.control_mut(id).map(|request| request.slot) else {
            return;
        };
        let running = self
            .pipe(slot)
            .ok()
            .and_then(|pipe| pipe.queue.running.as_ref())
            .is_some_and(|running| running.id == id);
        if running {
            self.fail_running_td(slot, (DriverError::DeviceFault, 0), Abandoned::Running);
            return;
        }
        if let Ok(pipe) = self.pipe_mut(slot) {
            pipe.queue.waiting.retain(|&waiting| waiting != id);
        }
        self.answer_control(id, Err(ControlFault::serving(DriverError::DeviceFault)));
    }

    fn answer_control(&mut self, id: ControlId, answer: Result<ControlDone, ControlFault>) {
        let Some(request) = self.control_mut(id) else {
            return;
        };
        if request.orphaned {
            self.controls[id.at] = None;
            return;
        }
        request.answer = Some(answer);
        let waiter = request.waiter;
        self.wake(waiter);
    }

    /// Start the next transfer queued on `slot`'s pipe when it serves and
    /// runs none.
    fn kick_pipe(&mut self, slot: u8) {
        loop {
            let Ok(pipe) = self.pipe_mut(slot) else {
                return;
            };
            if pipe.state != PipeState::Serving || pipe.queue.running.is_some() {
                return;
            }
            let Some(id) = pipe.queue.waiting.pop_front() else {
                return;
            };
            let Some(request) = self.control_mut(id) else {
                continue;
            };
            let setup = request.setup;
            // An OUT payload is staged once; the stage keeps its direction.
            let stage = match &mut request.stage {
                DataStage::Out(bytes) => DataStage::Out(core::mem::take(bytes)),
                DataStage::In(len) => DataStage::In(*len),
                DataStage::None => DataStage::None,
            };
            match self.start_td(slot, id, setup, &stage) {
                Ok(()) => return,
                // Nothing reached the ring, or a refused doorbell left it there.
                Err((error, None)) => {
                    self.answer_control(id, Err(ControlFault::serving(error)));
                }
                Err((error, Some(td))) => {
                    if let Ok(pipe) = self.pipe_mut(slot) {
                        pipe.queue.running = Some(RunningTd {
                            id,
                            td,
                            residual: 0,
                        });
                    }
                    self.fail_running_td(slot, (error, 0), Abandoned::Running);
                    return;
                }
            }
        }
    }

    /// Stage and arm one TD on `slot`'s pipe and ring its doorbell. On
    /// failure, the TD if any of it reached the ring.
    fn start_td(
        &mut self,
        slot: u8,
        id: ControlId,
        setup: [u8; 8],
        stage: &DataStage,
    ) -> Result<(), (DriverError, Option<ControlTd>)> {
        let ctrl_data = self.pipe(slot).map_err(|err| (err, None))?.ctrl_data;
        let data_len = stage.len().map_err(|err| (err, None))?;
        if let DataStage::Out(bytes) = stage {
            if !bytes.is_empty() {
                self.dma
                    .write(ctrl_data, bytes)
                    .map_err(|err| (err, None))?;
            }
        }
        let out = matches!(stage, DataStage::Out(_));
        let td = self
            .arm_control_td(slot, setup, data_len, out, ctrl_data)
            .map_err(|err| (err, None))?;
        if let Err(err) = self.xhci.ring_doorbell(slot, u32::from(DCI_CONTROL)) {
            return Err((err, Some(td)));
        }
        if let Ok(pipe) = self.pipe_mut(slot) {
            pipe.queue.running = Some(RunningTd {
                id,
                td,
                residual: 0,
            });
        }
        Ok(())
    }

    /// Route a completion on a slot's default control endpoint to the TD it
    /// runs, returning whether the slot has a pipe (one that does owns the
    /// event, stale or not).
    fn on_control_event(&mut self, event: Trb) -> bool {
        let slot = event.slot_id();
        let Ok(pipe) = self.pipe_mut(slot) else {
            return false;
        };
        let Some(running) = pipe.queue.running.as_mut() else {
            return true;
        };
        let td = running.td;
        let addresses = [td.setup, td.data.unwrap_or(td.status), td.status];
        if !addresses.contains(&event.parameter) {
            return true;
        }
        let completion = event.completion_code_raw();
        match event.completion_code() {
            Ok(CompletionCode::Success | CompletionCode::ShortPacket) => {}
            // A protocol STALL: the device refused the request, which a class
            // driver may treat as an answer. The device side clears at the
            // next SETUP (USB 2.0 §8.5.3.4).
            Ok(CompletionCode::StallError) => {
                self.fail_running_td(
                    slot,
                    (DriverError::EndpointStalled, completion),
                    Abandoned::Halted,
                );
                return true;
            }
            // Every other error halts the control endpoint (xHCI §4.8.3), and a
            // code nothing names ended the TD as one.
            _ => {
                self.fail_running_td(
                    slot,
                    (DriverError::DeviceFault, completion),
                    Abandoned::Halted,
                );
                return true;
            }
        }
        if td.data == Some(event.parameter) {
            if let Ok(Some(running)) = self.pipe_mut(slot).map(|pipe| pipe.queue.running.as_mut()) {
                running.residual = event.transfer_residual();
            }
            return true;
        }
        if event.parameter != td.status {
            // A setup stage asks for no completion of its own.
            self.fail_running_td(
                slot,
                (DriverError::DeviceFault, completion),
                Abandoned::Running,
            );
            return true;
        }
        self.finish_running_td(slot);
        true
    }

    /// The running TD on `slot`'s pipe completed: retire it, answer it with
    /// what its data stage moved, and start the next.
    fn finish_running_td(&mut self, slot: u8) {
        let Ok(pipe) = self.pipe_mut(slot) else {
            return;
        };
        let Some(running) = pipe.queue.running.take() else {
            return;
        };
        while pipe.ring.in_flight() > 0 {
            if pipe.ring.retire_one().is_err() {
                break;
            }
        }
        let ctrl_data = pipe.ctrl_data;
        let answer = self.collect_td(running.id, running.td, running.residual, ctrl_data);
        self.answer_control(running.id, answer);
        self.kick_pipe(slot);
    }

    /// What a completed TD's data stage moved: `data_len` less the residual
    /// its short-packet event reported, and an IN stage's bytes, copied out
    /// before the pipe's buffer stages another TD.
    fn collect_td(
        &mut self,
        id: ControlId,
        td: ControlTd,
        residual: u32,
        ctrl_data: usize,
    ) -> Result<ControlDone, ControlFault> {
        let transferred = td
            .data_len
            .checked_sub(residual)
            .ok_or(ControlFault::serving(DriverError::DeviceFault))?;
        let reads = self
            .control_mut(id)
            .is_some_and(|request| matches!(request.stage, DataStage::In(_)));
        let mut data = Vec::new();
        if reads && transferred != 0 {
            let len = transferred as usize;
            data.try_reserve_exact(len)
                .map_err(|_| ControlFault::serving(DriverError::OutOfMemory))?;
            data.resize(len, 0);
            self.dma
                .read(ctrl_data, &mut data)
                .map_err(ControlFault::serving)?;
        }
        Ok(ControlDone { transferred, data })
    }

    /// The running TD on `slot`'s pipe failed with `error`, its event
    /// carrying `completion` (`0` for none), leaving the endpoint as
    /// `abandoned` says: the pipe waits while it is taken back, and the TD's
    /// request is answered once that settles.
    fn fail_running_td(
        &mut self,
        slot: u8,
        (error, completion): (DriverError, u8),
        abandoned: Abandoned,
    ) {
        let Ok(pipe) = self.pipe_mut(slot) else {
            return;
        };
        let Some(running) = pipe.queue.running.take() else {
            return;
        };
        pipe.state = PipeState::Recovering;
        pipe.queue.failed = Some((running.id, error, completion));
        self.ask(Work::RecoverPipe { slot, abandoned });
    }

    /// A recovery took `slot`'s pipe back from its failed TD, or could not:
    /// answer that TD, and start the queue again or fail it whole.
    pub(super) fn pipe_recovered(&mut self, slot: u8, recovered: Result<(), DriverError>) {
        let Ok(pipe) = self.pipe_mut(slot) else {
            return;
        };
        let failed = pipe.queue.failed.take();
        pipe.state = if recovered.is_ok() {
            PipeState::Serving
        } else {
            PipeState::Dead
        };
        let serves = recovered.is_ok();
        let waiting: Vec<ControlId> = if serves {
            Vec::new()
        } else {
            core::mem::take(&mut pipe.queue.waiting)
                .into_iter()
                .collect()
        };
        if let Some((id, error, completion)) = failed {
            // A refusal promises an endpoint already recovered.
            let error = if error == DriverError::EndpointStalled && !serves {
                DriverError::DeviceFault
            } else {
                error
            };
            self.answer_control(
                id,
                Err(ControlFault {
                    error,
                    endpoint_serves: serves,
                    completion,
                }),
            );
        }
        for id in waiting {
            self.answer_control(id, Err(ControlFault::dead()));
        }
        if serves {
            self.kick_pipe(slot);
        }
    }

    /// Answer every transfer on `slot`'s pipe as dead: the pipe is closing.
    pub(super) fn fail_pipe_queue(&mut self, slot: u8) {
        let Ok(pipe) = self.pipe_mut(slot) else {
            return;
        };
        let mut ids: Vec<ControlId> = core::mem::take(&mut pipe.queue.waiting)
            .into_iter()
            .collect();
        if let Some(running) = pipe.queue.running.take() {
            ids.push(running.id);
        }
        if let Some((id, _, _)) = pipe.queue.failed.take() {
            ids.push(id);
        }
        for id in ids {
            self.answer_control(id, Err(ControlFault::dead()));
        }
    }
}

impl ControlPipe {}
