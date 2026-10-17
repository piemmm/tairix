//! The per-port operation: reconcile one root or hub port with what it
//! carries — enumerate a device that connected, tear down one that left —
//! waking only on the port's own changes, its transfers and its timers.
//!
//! One operation runs per port, so ports enumerate side by side; the bus's
//! default address is the one thing they take turns at, from a port's reset
//! until Address Device moves its device off it.

use alloc::vec::Vec;
use core::future::{pending, poll_fn, Future};
use core::pin::pin;
use core::task::Poll;

use tairix_abi::{DriverError, RegisterBlock};
use tairix_inline::ArrayVec;

use super::ops::{Engine, Held};
use super::route::{Settle, Work};
use super::{
    command_outcome, ep0_max_packet, ep0_max_packet_from_descriptor, ep_ctx_dwords,
    hub_port_connected, hub_port_enabled, hub_port_resetting, hub_port_speed, input_control_dwords,
    interrupt_interval, is_control_only, setup_clear_port_feature,
    setup_get_configuration_descriptor, setup_get_device_descriptor, setup_get_hub_descriptor,
    setup_get_port_status, setup_get_string_descriptor, setup_set_configuration,
    setup_set_hub_depth, setup_set_port_feature, slot_ctx_dwords, AttachFault, Attachment,
    BulkRings, ConfigurationHeader, ConfiguredRings, ControlFault, DeviceDescriptor, DeviceRegion,
    DmaBank, EnumStage, HubDescriptor, HubEvent, HubRegion, InterfaceInfo, Ladder, Malformed,
    Periodic, SerialNumber, SlotCtxBase, SlotHold, StringHeader, Upstream, UsbDevice,
    CONFIGURATION_HEADER_LEN, CTRL_DATA_LEN, DCI_CONTROL, DEVICE_DESCRIPTOR_PREFIX_LEN,
    ENUM_ATTEMPTS, EP_TYPE_BULK_IN, EP_TYPE_BULK_OUT, EP_TYPE_CONTROL, EP_TYPE_INTERRUPT_IN,
    HUB_DESC_ATTEMPTS, HUB_DESC_REQUEST, HUB_POWER_ON_GOOD_US, MAX_INTERFACES, PORT_CHANGE_CONNECT,
    PORT_CHANGE_FEATURES, PORT_CHANGE_RESET, PORT_FEATURE_C_RESET, PORT_FEATURE_POWER,
    PORT_FEATURE_RESET, PORT_RESET_POLLS, PORT_RESET_POLL_US, PORT_RESET_SETTLE_US, SLOT_CTX_HUB,
    SLOT_CTX_MTT, SLOT_CTX_NUM_PORTS_SHIFT, SLOT_CTX_TTT_MASK, SLOT_CTX_TTT_SHIFT, SPEED_SUPER,
    SS_PORT_CHANGE_FEATURES, STRING_DESCRIPTOR_MAX_LEN,
};
use crate::ring::ProducerRing;
use crate::trb::{self, CompletionCode, Trb, TrbType};
use crate::PortStatus;

/// Give the powered root ports the connect window, parked until the first
/// connect or the window's end, then hand every root port to its own
/// operation.
pub(super) async fn walk<H: RegisterBlock, M: DmaBank>(engine: Engine<H, M>) {
    let Ok(deadline) = engine
        .now()
        .map(|now| now.saturating_add(super::CONNECT_WINDOW_US * 1_000))
    else {
        return;
    };
    loop {
        let (connected, epoch, now) = match engine.lock() {
            Ok(mut core) => {
                let ports = core.xhci.max_ports();
                let connected = (1..=ports)
                    .any(|port| core.xhci.port_status(port).is_ok_and(PortStatus::connected));
                (connected, core.root_epoch(), core.now_ns)
            }
            Err(_) => return,
        };
        if connected || now >= deadline {
            break;
        }
        if engine.root_change(epoch, deadline).await.is_err() {
            break;
        }
    }
    if let Ok(mut core) = engine.lock() {
        for port in 1..=core.xhci.max_ports() {
            core.ask(Work::Port(Upstream::Root(port)));
        }
    }
}

/// The most passes one operation makes over its port before it leaves the
/// rest to the port's next change: a port that flaps for ever cannot hold
/// an operation for ever.
const PORT_PASSES: usize = 8;

/// What one attach attempt last saw, for the fault its failure reports.
/// Each operation keeps its own, so ports attaching side by side never
/// overwrite each other's.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Crumb {
    pub(crate) stage: EnumStage,
    pub(crate) completion: u8,
    pub(crate) event_type: u8,
    pub(crate) timed_out: bool,
    pub(crate) port_status: u16,
}

impl Default for Crumb {
    fn default() -> Self {
        Self {
            stage: EnumStage::Scan,
            completion: 0,
            event_type: 0,
            timed_out: false,
            port_status: 0,
        }
    }
}

impl Crumb {
    /// Record the event a command or transfer ended with.
    fn saw(&mut self, event: &Trb) {
        self.completion = event.completion_code_raw();
        self.event_type = event.trb_type_raw();
        self.timed_out = false;
    }

    /// Record the control fault a transfer ended with: no completion code
    /// means its deadline ended it.
    fn faulted(&mut self, fault: ControlFault) {
        self.completion = fault.completion;
        self.event_type = if fault.completion == 0 {
            0
        } else {
            TrbType::TransferEvent.as_u8()
        };
        self.timed_out = fault.completion == 0;
    }
}

/// Reconcile `port` until it stops changing under the operation, or the
/// pass bound is spent.
pub(super) async fn serve_port<H: RegisterBlock, M: DmaBank>(engine: Engine<H, M>, port: Upstream) {
    for _ in 0..PORT_PASSES {
        if let Ok(mut core) = engine.lock() {
            core.take_port_change(port);
        }
        // A port that cannot be read keeps its state; its next change looks
        // again.
        let _ = match port {
            Upstream::Root(root) => reconcile_root(&engine, root).await,
            Upstream::Hub(hub, down) => reconcile_hub_port(&engine, hub, down).await,
        };
        if !engine
            .lock()
            .is_ok_and(|mut core| core.take_port_change(port))
        {
            break;
        }
    }
}

/// Reconcile root port `port`: consume its latches, then act on what it
/// carries now — tear down what left, attach what arrived. A glitch on a
/// served port is left to its transfers, which detach it if the device
/// really changed.
async fn reconcile_root<H: RegisterBlock, M: DmaBank>(
    engine: &Engine<H, M>,
    port: u8,
) -> Result<(), DriverError> {
    let (connected, attachment, reenumerate) = {
        let mut core = engine.lock()?;
        let status = core.xhci.port_status(port)?;
        core.xhci.clear_port_changes(port, status.changes())?;
        let connected = core.xhci.port_status(port)?.connected();
        let upstream = Upstream::Root(port);
        (
            connected,
            core.root_attachment_on(port),
            core.take_reenumeration(upstream),
        )
    };
    match (attachment, connected) {
        (Some(Attachment::Hub(hub)), false) => engine.lock()?.detach_hub(hub),
        (Some(Attachment::Device(index)), false) => engine.lock()?.detach_device(index),
        (None, true) => {
            let _ = attach(engine, Upstream::Root(port), reenumerate).await;
        }
        (Some(_), true) | (None, false) => {}
    }
    Ok(())
}

/// Reconcile downstream `port` of the hub at `hub_index`, as
/// [`reconcile_root`] does a root port.
async fn reconcile_hub_port<H: RegisterBlock, M: DmaBank>(
    engine: &Engine<H, M>,
    hub_index: usize,
    port: u8,
) -> Result<(), DriverError> {
    let (status, change) = hub_port_status_change(engine, hub_index, port).await?;
    if change != 0 {
        clear_hub_port_changes(engine, hub_index, port, change).await?;
    }
    let (device, child, reenumerate) = {
        let mut core = engine.lock()?;
        (
            core.device_index_for_hub_and_port(hub_index, port),
            core.hub_index_for_hub_and_port(hub_index, port),
            core.take_reenumeration(Upstream::Hub(hub_index, port)),
        )
    };
    if hub_port_connected(status) && device.is_none() && child.is_none() {
        let _ = attach(engine, Upstream::Hub(hub_index, port), reenumerate).await;
        return Ok(());
    }
    if !hub_port_connected(status) {
        let mut core = engine.lock()?;
        if let Some(index) = device {
            core.detach_device(index);
        } else if let Some(child) = child {
            core.detach_hub(child);
        }
    }
    Ok(())
}

/// Attach what `port` carries, recording a failure as a skipped port with
/// its fault. An attachment a re-enumeration produced is given up rather
/// than reset again should it fail once more. A device that leaves mid-way
/// ends the attach at once, and the port's next pass settles what it carries.
pub(crate) async fn attach<H: RegisterBlock, M: DmaBank>(
    engine: &Engine<H, M>,
    port: Upstream,
    reenumerate: bool,
) -> Result<Attachment, DriverError> {
    let mut crumb = Crumb::default();
    let outcome = {
        let attaching = async {
            match port {
                Upstream::Root(root) => attach_root(engine, root, reenumerate, &mut crumb).await,
                Upstream::Hub(hub, down) => attach_hub_port(engine, hub, down, &mut crumb).await,
            }
        };
        while_connected(engine, port, attaching).await
    };
    let mut core = engine.lock()?;
    match outcome {
        Ok(attachment) => {
            if reenumerate {
                core.mark_reset_produced(attachment);
            }
            core.note_topology(match attachment {
                Attachment::Device(index) => HubEvent::Attached(index),
                Attachment::Hub(hub) => HubEvent::HubAttached(hub),
            });
        }
        Err(DriverError::DeviceOffline) => core.port_changed(port),
        Err(error) => {
            let number = match port {
                Upstream::Root(root) => root,
                Upstream::Hub(_, down) => down,
            };
            core.port_skipped(AttachFault {
                port: number,
                error,
                stage: crumb.stage,
                completion: crumb.completion,
                event_type: crumb.event_type,
                timed_out: crumb.timed_out,
                port_status: crumb.port_status,
            });
        }
    }
    outcome
}

/// Run `attaching` on `port` until it ends, or until the port's device
/// leaves: the attach then ends at whatever step it stands, answering
/// [`DriverError::DeviceOffline`], and its claims go back through their
/// guards.
async fn while_connected<H: RegisterBlock, M: DmaBank>(
    engine: &Engine<H, M>,
    port: Upstream,
    attaching: impl Future<Output = Result<Attachment, DriverError>>,
) -> Result<Attachment, DriverError> {
    let _watch = DepartureWatch::start(engine, port)?;
    let mut attaching = pin!(attaching);
    let mut departure = pin!(departure(engine, port));
    poll_fn(|cx| {
        if let Poll::Ready(outcome) = attaching.as_mut().poll(cx) {
            return Poll::Ready(outcome);
        }
        departure
            .as_mut()
            .poll(cx)
            .map(|()| Err(DriverError::DeviceOffline))
    })
    .await
}

/// Resolve once a change finds `port` disconnected, or its connection
/// changed since the attach cleared it. A port that cannot be read is left
/// to the attach's own transfers.
async fn departure<H: RegisterBlock, M: DmaBank>(engine: &Engine<H, M>, port: Upstream) {
    while engine.departure_note(port).await.is_ok() {
        let left = match port {
            Upstream::Root(root) => engine
                .lock()
                .and_then(|mut core| core.xhci.port_status(root))
                .map(|status| !status.connected() || status.connect_changed()),
            Upstream::Hub(hub, down) => {
                hub_port_status_change(engine, hub, down)
                    .await
                    .map(|(status, change)| {
                        !hub_port_connected(status) || change & PORT_CHANGE_CONNECT != 0
                    })
            }
        };
        match left {
            Ok(true) => return,
            Ok(false) => {}
            Err(_) => break,
        }
    }
    pending::<()>().await;
}

/// A port's departure watch, ended with the attach it serves.
struct DepartureWatch<'e, H: RegisterBlock, M: DmaBank> {
    engine: &'e Engine<H, M>,
    port: Upstream,
}

impl<'e, H: RegisterBlock, M: DmaBank> DepartureWatch<'e, H, M> {
    fn start(engine: &'e Engine<H, M>, port: Upstream) -> Result<Self, DriverError> {
        engine.lock()?.watch_departure(port)?;
        Ok(Self { engine, port })
    }
}

impl<H: RegisterBlock, M: DmaBank> Drop for DepartureWatch<'_, H, M> {
    fn drop(&mut self) {
        if let Ok(mut core) = self.engine.lock() {
            core.unwatch_departure(self.port);
        }
    }
}

/// A hub an attach installed, torn down unless its descent completes, so a
/// failed or abandoned attach leaves no half-walked tier. It was never
/// reported, so nothing reports it gone; a hub its parent's teardown took
/// first, whose index another may hold by now, is left alone.
struct Descent<'e, H: RegisterBlock, M: DmaBank> {
    engine: &'e Engine<H, M>,
    hub: usize,
    serial: u64,
    done: bool,
}

impl<'e, H: RegisterBlock, M: DmaBank> Descent<'e, H, M> {
    fn begin(engine: &'e Engine<H, M>, hub: usize) -> Result<Self, DriverError> {
        let serial = engine.lock()?.hub(hub).ok_or(DriverError::NotFound)?.serial;
        Ok(Self {
            engine,
            hub,
            serial,
            done: false,
        })
    }
}

impl<H: RegisterBlock, M: DmaBank> Drop for Descent<'_, H, M> {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        if let Ok(mut core) = self.engine.lock() {
            let ours = core
                .hub(self.hub)
                .is_some_and(|hub| hub.serial == self.serial);
            if ours && !core.resetting {
                core.tear_down_hub(self.hub);
            }
        }
    }
}

/// Descend the hub at `hub` an attach just installed.
async fn descend<H: RegisterBlock, M: DmaBank>(
    engine: &Engine<H, M>,
    hub: usize,
    crumb: &mut Crumb,
) -> Result<(), DriverError> {
    let mut descent = Descent::begin(engine, hub)?;
    descend_hub(engine, hub, crumb).await?;
    descent.done = true;
    Ok(())
}

/// Attach whatever is connected on root port `port`: reset it when the
/// protocol requires (a USB2 port enables only through a reset; a
/// `SuperSpeed` port trains and enables on its own), or when a
/// re-enumeration owes one, enumerate the device, and descend a hub.
pub(crate) async fn attach_root<H: RegisterBlock, M: DmaBank>(
    engine: &Engine<H, M>,
    port: u8,
    reenumerate: bool,
    crumb: &mut Crumb,
) -> Result<Attachment, DriverError> {
    let outcome = enumerate_root(engine, port, reenumerate, crumb).await?;
    if let Attachment::Hub(hub) = outcome {
        descend(engine, hub, crumb).await?;
    }
    Ok(outcome)
}

/// [`attach_root`] short of descending a hub it installs.
pub(crate) async fn enumerate_root<H: RegisterBlock, M: DmaBank>(
    engine: &Engine<H, M>,
    port: u8,
    reenumerate: bool,
    crumb: &mut Crumb,
) -> Result<Attachment, DriverError> {
    crumb.stage = EnumStage::PortReset;
    let status = {
        let mut core = engine.lock()?;
        if core.root_attachment_on(port).is_some() {
            return Err(DriverError::Busy);
        }
        core.xhci.port_status(port)?
    };
    if !status.connected() {
        return Err(DriverError::DeviceOffline);
    }
    let address0 = engine.address0().await?;
    let status = if status.enabled() && !reenumerate {
        // Trained on its own, but only just: the device is owed the same
        // recovery interval as one that was reset.
        engine.lock()?.xhci.clear_port_reset_change(port)?;
        engine.sleep_us(PORT_RESET_SETTLE_US).await?;
        status
    } else {
        reset_root_port(engine, port).await?
    };
    let speed = status.speed();
    ep0_max_packet(speed)?;
    enumerate_on_port(engine, None, port, speed, Some(address0), crumb).await
}

/// Attach whatever is connected on downstream `port` of the hub at
/// `hub_index`: reset the port, enumerate the device, and clear every change
/// the attach latched, whatever its outcome — a latch left set has the hub
/// re-report the same stale change for ever, and each report re-run the
/// failing attach.
async fn attach_hub_port<H: RegisterBlock, M: DmaBank>(
    engine: &Engine<H, M>,
    hub_index: usize,
    port: u8,
    crumb: &mut Crumb,
) -> Result<Attachment, DriverError> {
    crumb.stage = EnumStage::PortReset;
    let root_port = engine
        .lock()?
        .hub(hub_index)
        .ok_or(DriverError::DeviceFault)?
        .root_port;
    let address0 = engine.address0().await?;
    let attached = match reset_downstream_port(engine, hub_index, port, crumb).await {
        Ok(speed) => {
            enumerate_on_port(
                engine,
                Some((hub_index, port)),
                root_port,
                speed,
                Some(address0),
                crumb,
            )
            .await
        }
        Err(err) => Err(err),
    };
    let descent = match attached {
        Ok(Attachment::Hub(new_hub)) => Some(Descent::begin(engine, new_hub)?),
        _ => None,
    };
    let drained = drain_hub_port(engine, hub_index, port).await;
    let outcome = attached?;
    drained?;
    if let (Attachment::Hub(new_hub), Some(mut descent)) = (outcome, descent) {
        descend_hub(engine, new_hub, crumb).await?;
        descent.done = true;
    }
    Ok(outcome)
}

/// Reset root port `port` and await its completion and recovery interval:
/// the reset's Port Status Change wakes the wait, a re-read at the
/// hardware's poll interval bounds it.
pub(crate) async fn reset_root_port<H: RegisterBlock, M: DmaBank>(
    engine: &Engine<H, M>,
    port: u8,
) -> Result<PortStatus, DriverError> {
    let deadline = {
        let mut core = engine.lock()?;
        core.xhci.begin_port_reset(port)?;
        core.now_ns.saturating_add(reset_budget_ns())
    };
    loop {
        let reset = {
            let mut core = engine.lock()?;
            let status = core.xhci.port_status(port)?;
            if !status.connected() {
                return Err(DriverError::DeviceOffline);
            }
            let reset = !status.resetting() && status.enabled();
            if reset {
                core.xhci.clear_port_reset_change(port)?;
            }
            reset.then_some(status)
        };
        if let Some(status) = reset {
            engine.sleep_us(PORT_RESET_SETTLE_US).await?;
            return Ok(status);
        }
        let now = engine.now()?;
        if now >= deadline {
            return Err(DriverError::DeviceFault);
        }
        engine
            .port_change(Upstream::Root(port), poll_bound(now, deadline))
            .await?;
    }
}

/// Reset downstream `port` of the hub at `hub_index` and await its
/// completion and recovery interval, answering the speed the device came
/// back at. The hub's status-change report wakes the wait early; a hub polls
/// that endpoint at its own interval, which can be ten times the reset, so a
/// re-read at the reset's own poll interval bounds it.
async fn reset_downstream_port<H: RegisterBlock, M: DmaBank>(
    engine: &Engine<H, M>,
    hub_index: usize,
    port: u8,
    crumb: &mut Crumb,
) -> Result<u8, DriverError> {
    hub_request(
        engine,
        hub_index,
        setup_set_port_feature(PORT_FEATURE_RESET, port),
    )
    .await?;
    let deadline = engine.now()?.saturating_add(reset_budget_ns());
    loop {
        let now = engine.now()?;
        if now >= deadline {
            return Err(DriverError::DeviceFault);
        }
        engine
            .port_change(Upstream::Hub(hub_index, port), poll_bound(now, deadline))
            .await?;
        let (status, change) = hub_port_status_change(engine, hub_index, port).await?;
        crumb.port_status = status;
        if !hub_port_connected(status) {
            return Err(DriverError::DeviceOffline);
        }
        if !hub_port_resetting(status) && hub_port_enabled(status) {
            // Cleared at once, so a later report naming the port is a fresh
            // change.
            if change & PORT_CHANGE_RESET != 0 {
                hub_request(
                    engine,
                    hub_index,
                    setup_clear_port_feature(PORT_FEATURE_C_RESET, port),
                )
                .await?;
            }
            engine.sleep_us(PORT_RESET_SETTLE_US).await?;
            // A `SuperSpeed` hub's ports carry only `SuperSpeed` devices, and
            // its `wPortStatus` reserves the USB 2.0 speed bits as zero
            // (USB 3.2 §10.16.2.6).
            let hub_speed = engine
                .lock()?
                .hub(hub_index)
                .ok_or(DriverError::DeviceFault)?
                .speed;
            return Ok(if hub_speed == SPEED_SUPER {
                SPEED_SUPER
            } else {
                hub_port_speed(status)
            });
        }
    }
}

/// The wall-clock bound on a port reset completing.
const fn reset_budget_ns() -> u64 {
    PORT_RESET_POLL_US as u64 * PORT_RESET_POLLS as u64 * 1_000
}

/// The next re-read of a resetting port: one poll interval on, never past
/// the reset's deadline.
const fn poll_bound(now: u64, deadline: u64) -> u64 {
    let next = now.saturating_add(PORT_RESET_POLL_US as u64 * 1_000);
    if next < deadline {
        next
    } else {
        deadline
    }
}

/// One enumeration attempt's claims: the device-table entries it took, and
/// the slot once it has one. Dropped unkept — the operation cancelled, or
/// the attempt failed — it gives everything back: the slot's Disable Slot is
/// posted with the entries' regions withheld until it confirms, since the
/// controller may still reach them; entries no slot was handed go back at
/// once.
struct Attempt<'e, H: RegisterBlock, M: DmaBank> {
    engine: &'e Engine<H, M>,
    entries: ArrayVec<usize, MAX_INTERFACES>,
    slot: Option<u8>,
    kept: bool,
}

impl<'e, H: RegisterBlock, M: DmaBank> Attempt<'e, H, M> {
    /// Claim a device-table entry and its region.
    fn claim(engine: &'e Engine<H, M>) -> Result<Self, DriverError> {
        let index = engine.lock()?.claim_device_entry()?;
        let mut entries = ArrayVec::new();
        let _ = entries.try_push(index);
        Ok(Self {
            engine,
            entries,
            slot: None,
            kept: false,
        })
    }

    /// The entry the device's first served interface takes.
    fn primary(&self) -> usize {
        self.entries.first().copied().unwrap_or(usize::MAX)
    }

    /// Give the attempt's slot back and await the answer. A slot confirmed
    /// disabled leaves the primary entry's region claimed for the next
    /// attempt, since the controller no longer reaches it; any further entry
    /// goes back. One the controller would not confirm keeps every region
    /// withheld, since it may still reach them.
    async fn disable(&mut self) -> SlotHold {
        let Some(slot) = self.slot.take() else {
            return SlotHold::Released;
        };
        let posted = self.engine.lock().and_then(|mut core| {
            core.forget_entries(&self.entries);
            core.post_disable_slot(slot)
        });
        let confirmed = match posted {
            Ok(command) => matches!(
                self.engine.await_command(command).await,
                Ok(event) if event.completion_code() == Ok(CompletionCode::Success)
            ),
            Err(_) => false,
        };
        let Ok(mut core) = self.engine.lock() else {
            return SlotHold::Held;
        };
        if !confirmed {
            core.release_entries(&self.entries, SlotHold::Held);
            self.entries.clear();
            return SlotHold::Held;
        }
        core.clear_dcbaa_entry(slot);
        let further = self.entries.get(1..).unwrap_or_default();
        core.release_entries(further, SlotHold::Released);
        self.entries.truncate(1);
        SlotHold::Released
    }
}

impl<H: RegisterBlock, M: DmaBank> Drop for Attempt<'_, H, M> {
    fn drop(&mut self) {
        if self.kept {
            return;
        }
        let Ok(mut core) = self.engine.lock() else {
            return;
        };
        if core.resetting {
            return;
        }
        match self.slot {
            Some(slot) => {
                let _ = core.tear_down_attempt(slot, &self.entries);
            }
            None => core.release_entries(&self.entries, SlotHold::Released),
        }
    }
}

/// Enumerate the device on a port just reset (or trained): Enable Slot,
/// open its control pipe, Address Device with the topology `parent` dictates
/// (a downstream route string and TT behind a hub, the bare root topology on
/// `root_port` otherwise), and complete enumeration. `address0`, the bus's
/// default address, is held until Address Device moved the device off it.
///
/// A fault bringing the control pipe up ([`EnumStage::is_pipe_bringup`])
/// that leaves the device untouched — a USB or split transaction error, or a
/// command the controller refused because its own slot or port state
/// disagreed with software's — is a present but disturbed device, not a
/// broken one. Once its slot is confirmed disabled the port is reset, which
/// returns the device to Default state, and a fresh slot re-drives it at the
/// speed it came back at, up to [`ENUM_ATTEMPTS`] times. Anything else is
/// surfaced on the first attempt.
pub(crate) async fn enumerate_on_port<H: RegisterBlock, M: DmaBank>(
    engine: &Engine<H, M>,
    parent: Option<(usize, u8)>,
    root_port: u8,
    speed: u8,
    address0: Option<Held<'_, H, M>>,
    crumb: &mut Crumb,
) -> Result<Attachment, DriverError> {
    let (hub_index, down_port) = parent.unwrap_or((0, 0));
    let mut speed = speed;
    let mut address0 = address0;
    let mut claim = Attempt::claim(engine)?;
    for attempt in 1..=ENUM_ATTEMPTS {
        let attached = attempt_enumeration(
            engine,
            &mut claim,
            (parent, root_port, speed),
            &mut address0,
            crumb,
        )
        .await;
        let err = match attached {
            Ok(outcome) => {
                claim.kept = true;
                return Ok(outcome);
            }
            Err(err) => err,
        };
        drop(address0.take());
        let failed = *crumb;
        let hold = claim.disable().await;
        *crumb = failed;
        let transient = hold == SlotHold::Released
            && failed.stage.is_pipe_bringup()
            && CompletionCode::from_raw(u32::from(failed.completion)).is_ok_and(|code| {
                code.indicates_device_unreachable() || code.indicates_state_disagreement()
            });
        if !transient || attempt == ENUM_ATTEMPTS || claim.entries.is_empty() {
            return Err(err);
        }
        crumb.stage = EnumStage::PortReset;
        address0 = Some(engine.address0().await?);
        speed = match parent {
            Some(_) => reset_downstream_port(engine, hub_index, down_port, crumb).await?,
            None => reset_root_port(engine, root_port).await?.speed(),
        };
    }
    Err(DriverError::DeviceFault)
}

/// One enumeration attempt on `claim`'s entry.
async fn attempt_enumeration<'e, H: RegisterBlock, M: DmaBank>(
    engine: &'e Engine<H, M>,
    claim: &mut Attempt<'e, H, M>,
    (parent, root_port, speed): (Option<(usize, u8)>, u8, u8),
    address0: &mut Option<Held<'_, H, M>>,
    crumb: &mut Crumb,
) -> Result<Attachment, DriverError> {
    let (hub_index, down_port) = parent.unwrap_or((0, 0));
    let (base, parent_slot, max_slots) = {
        let core = engine.lock()?;
        let (base, parent_slot) = core.slot_topology(parent, root_port, speed)?;
        (base, parent_slot, core.xhci.max_slots())
    };
    let max_packet = ep0_max_packet(speed)?;
    crumb.stage = EnumStage::EnableSlot;
    let event = engine
        .issue(Trb::new(TrbType::EnableSlot, 0, 0, 0), Settle::ReturnSlot)
        .await
        .inspect_err(|_| crumb.timed_out = true)?;
    crumb.saw(&event);
    let event = command_outcome(event)?;
    let slot = event.slot_id();
    if slot == 0 || slot > max_slots {
        return Err(DriverError::DeviceFault);
    }
    claim.slot = Some(slot);
    if parent.is_some() && slot == parent_slot {
        return Err(DriverError::DeviceFault);
    }
    let index = claim.primary();
    engine.lock()?.open_pipe(index, slot)?;
    address_device(engine, base, slot, max_packet, crumb).await?;
    drop(address0.take());
    let (descriptor, hub_endpoint) =
        finish_enumeration(engine, claim, slot, base, down_port, hub_index, crumb).await?;
    if descriptor.is_hub() {
        // The child is itself a hub, enumerated on the region its contexts
        // live in; the caller descends it.
        let hub = install_hub(
            engine,
            slot,
            (parent.map(|(hub_index, _)| hub_index), down_port),
            base,
            index,
            hub_endpoint,
            crumb,
        )
        .await?;
        return Ok(Attachment::Hub(hub));
    }
    Ok(Attachment::Device(index))
}

/// Post `command` holding `slot`'s input context, which the caller staged it
/// in, and await its successful completion.
async fn configure<H: RegisterBlock, M: DmaBank>(
    engine: &Engine<H, M>,
    slot: u8,
    stage: impl FnOnce(&mut UsbDevice<H, M>) -> Result<Trb, DriverError>,
    crumb: &mut Crumb,
) -> Result<Trb, DriverError> {
    let _input = engine.input_context(slot).await?;
    let command = {
        let mut core = engine.lock()?;
        stage(&mut core)?
    };
    let event = engine
        .issue(command, Settle::Nothing)
        .await
        .inspect_err(|_| crumb.timed_out = true)?;
    crumb.saw(&event);
    command_outcome(event)
}

/// Address the device in `slot` (§4.3.4).
async fn address_device<H: RegisterBlock, M: DmaBank>(
    engine: &Engine<H, M>,
    base: SlotCtxBase,
    slot: u8,
    max_packet: u32,
    crumb: &mut Crumb,
) -> Result<(), DriverError> {
    crumb.stage = EnumStage::AddressDevice;
    configure(
        engine,
        slot,
        |core| core.stage_address_device(base, slot, max_packet),
        crumb,
    )
    .await
    .map(|_| ())
}

/// One control transfer on `slot`'s pipe for the enumeration, its fault
/// recorded in `crumb`.
async fn request<H: RegisterBlock, M: DmaBank>(
    engine: &Engine<H, M>,
    slot: u8,
    setup: [u8; 8],
    data: &mut [u8],
    crumb: &mut Crumb,
) -> Result<usize, ControlFault> {
    engine
        .control_in(slot, setup, data)
        .await
        .inspect_err(|fault| crumb.faulted(*fault))
}

/// Read and decode the 18-byte device descriptor in two steps (USB 2.0
/// §5.5.3): Address Device assumed the speed's worst-case EP0 packet size,
/// and with an overstated context any EP0 IN transfer longer than one device
/// packet ends short at the first. So one packet ending at
/// `bMaxPacketSize0` is read first, the context re-evaluated to the honest
/// size, and only then the whole descriptor.
async fn read_device_descriptor<H: RegisterBlock, M: DmaBank>(
    engine: &Engine<H, M>,
    slot: u8,
    base: SlotCtxBase,
    crumb: &mut Crumb,
) -> Result<DeviceDescriptor, DriverError> {
    crumb.stage = EnumStage::GetDeviceDescriptor;
    let prefix_len =
        u16::try_from(DEVICE_DESCRIPTOR_PREFIX_LEN).map_err(|_| DriverError::LengthOutOfRange)?;
    let mut prefix = [0u8; DEVICE_DESCRIPTOR_PREFIX_LEN];
    let read = request(
        engine,
        slot,
        setup_get_device_descriptor(prefix_len),
        &mut prefix,
        crumb,
    )
    .await
    .map_err(|fault| fault.error)?;
    if read != prefix.len() {
        return Err(DriverError::DeviceFault);
    }
    if usize::from(prefix[0]) < DeviceDescriptor::LEN || prefix[1] != 0x01 {
        return Err(DriverError::BadMagic);
    }
    let ep0_max = ep0_max_packet_from_descriptor(base.speed, prefix[7])?;
    if ep0_max != ep0_max_packet(base.speed)? {
        configure(
            engine,
            slot,
            |core| core.stage_evaluate_ep0(slot, ep0_max),
            crumb,
        )
        .await?;
    }
    let descriptor_len =
        u16::try_from(DeviceDescriptor::LEN).map_err(|_| DriverError::LengthOutOfRange)?;
    let mut bytes = [0u8; DeviceDescriptor::LEN];
    let read = request(
        engine,
        slot,
        setup_get_device_descriptor(descriptor_len),
        &mut bytes,
        crumb,
    )
    .await
    .map_err(|fault| fault.error)?;
    if read != bytes.len() {
        return Err(DriverError::DeviceFault);
    }
    DeviceDescriptor::decode(&bytes)
}

/// Read the configuration descriptor at its exact advertised length into
/// `config_bytes`: the 9-byte header first for `wTotalLength`, then
/// precisely that many bytes (clamped to [`CTRL_DATA_LEN`], a validation
/// bound on device-supplied data). Real receivers have been caught
/// mishandling an over-long request, so only bytes the device advertised are
/// ever asked for.
async fn read_configuration<H: RegisterBlock, M: DmaBank>(
    engine: &Engine<H, M>,
    slot: u8,
    config_bytes: &mut [u8],
    crumb: &mut Crumb,
) -> Result<usize, DriverError> {
    crumb.stage = EnumStage::GetConfigDescriptor;
    let header_len =
        u16::try_from(CONFIGURATION_HEADER_LEN).map_err(|_| DriverError::LengthOutOfRange)?;
    let mut header = [0u8; CONFIGURATION_HEADER_LEN];
    let read = request(
        engine,
        slot,
        setup_get_configuration_descriptor(header_len),
        &mut header,
        crumb,
    )
    .await
    .map_err(|fault| fault.error)?;
    if read != header.len() {
        return Err(DriverError::DeviceFault);
    }
    let total = ConfigurationHeader::decode(&header)
        .map_err(|Malformed| DriverError::BadMagic)?
        .total
        .min(config_bytes.len());
    let total_u16 = u16::try_from(total).map_err(|_| DriverError::LengthOutOfRange)?;
    let read = request(
        engine,
        slot,
        setup_get_configuration_descriptor(total_u16),
        &mut config_bytes[..total],
        crumb,
    )
    .await
    .map_err(|fault| fault.error)?;
    if read != total {
        return Err(DriverError::DeviceFault);
    }
    Ok(total)
}

/// Read the serial number string `index` names, in the first language the
/// device's LANGID table lists. `None` when `index` is `0`, or when either
/// read is refused, faults, or answers malformed: the serial is optional
/// identity, so a device whose string NAKs or babbles is still served.
///
/// # Errors
///
/// A fault the control pipe could not be taken back from, after which the
/// device can be configured no further.
async fn read_serial_number<H: RegisterBlock, M: DmaBank>(
    engine: &Engine<H, M>,
    slot: u8,
    index: u8,
    crumb: &mut Crumb,
) -> Result<Option<SerialNumber>, DriverError> {
    if index == 0 {
        return Ok(None);
    }
    crumb.stage = EnumStage::GetStringDescriptor;
    let mut descriptor = [0u8; STRING_DESCRIPTOR_MAX_LEN];
    let langid = read_string_descriptor(engine, slot, (0, 0), &mut descriptor, crumb)
        .await?
        .and_then(super::first_langid);
    let Some(langid) = langid else {
        return Ok(None);
    };
    Ok(
        read_string_descriptor(engine, slot, (index, langid), &mut descriptor, crumb)
            .await?
            .and_then(SerialNumber::decode),
    )
}

/// Read string descriptor `index` in language `langid` into `buf` — its
/// header, then exactly the length that header claims — and return what
/// follows the header. `None` for a read that did not complete but left the
/// pipe serving, or an answer [`StringHeader`] refuses.
async fn read_string_descriptor<'b, H: RegisterBlock, M: DmaBank>(
    engine: &Engine<H, M>,
    slot: u8,
    (index, langid): (u8, u16),
    buf: &'b mut [u8; STRING_DESCRIPTOR_MAX_LEN],
    crumb: &mut Crumb,
) -> Result<Option<&'b [u8]>, DriverError> {
    let Some(read) = read_string(
        engine,
        slot,
        (index, langid),
        &mut buf[..StringHeader::LEN],
        crumb,
    )
    .await?
    else {
        return Ok(None);
    };
    let Some(header) = StringHeader::decode(&buf[..read]) else {
        return Ok(None);
    };
    let len = header.descriptor_len();
    let read = if len == StringHeader::LEN {
        read
    } else {
        let Some(read) = read_string(engine, slot, (index, langid), &mut buf[..len], crumb).await?
        else {
            return Ok(None);
        };
        read
    };
    Ok(header.payload(&buf[..read]))
}

/// One `GET_DESCRIPTOR(string)` for `buf.len()` bytes: how many the device
/// delivered, or `None` when the read did not complete but left the pipe
/// serving.
async fn read_string<H: RegisterBlock, M: DmaBank>(
    engine: &Engine<H, M>,
    slot: u8,
    (index, langid): (u8, u16),
    buf: &mut [u8],
    crumb: &mut Crumb,
) -> Result<Option<usize>, DriverError> {
    let len = u8::try_from(buf.len()).map_err(|_| DriverError::LengthOutOfRange)?;
    match request(
        engine,
        slot,
        setup_get_string_descriptor(index, langid, len),
        buf,
        crumb,
    )
    .await
    {
        Ok(read) => Ok(Some(read)),
        Err(ControlFault {
            endpoint_serves: true,
            ..
        }) => Ok(None),
        Err(ControlFault { error, .. }) => Err(error),
    }
}

/// Complete enumeration of the device addressed in `slot` with topology
/// `base`: read its descriptors — and the serial number of one serving a
/// mass-storage interface — configure each servable interface's endpoints,
/// then `SET_CONFIGURATION`, installing one table entry per served
/// interface. The first takes `claim`'s entry; each further one, a composite
/// device's sibling function, claims its own and shares the slot's pipe.
/// A hub creates no entry, and its status-change endpoint is answered for
/// the hub install to watch.
///
/// # Errors
///
/// * [`DriverError::Unsupported`] for a device that is not a hub and carries
///   no interface this engine serves, before anything is configured.
/// * [`DriverError::BadMagic`] if a descriptor is forged.
/// * [`DriverError::DeviceFault`] for any controller/device failure.
async fn finish_enumeration<'e, H: RegisterBlock, M: DmaBank>(
    engine: &'e Engine<H, M>,
    claim: &mut Attempt<'e, H, M>,
    slot: u8,
    base: SlotCtxBase,
    hub_port: u8,
    parent_hub: usize,
    crumb: &mut Crumb,
) -> Result<(DeviceDescriptor, Option<(u8, u32, u32)>), DriverError> {
    let descriptor = read_device_descriptor(engine, slot, base, crumb).await?;
    let mut config_bytes = Vec::new();
    config_bytes
        .try_reserve_exact(CTRL_DATA_LEN)
        .map_err(|_| DriverError::OutOfMemory)?;
    config_bytes.resize(CTRL_DATA_LEN, 0);
    let total = read_configuration(engine, slot, &mut config_bytes, crumb).await?;
    let config_bytes = &config_bytes[..total];
    let interfaces = InterfaceInfo::decode_all(config_bytes)?;
    let first = interfaces[0].ok_or(DriverError::BadMagic)?;
    let hub_endpoint = if descriptor.is_hub() {
        (first.int_dci != DCI_CONTROL).then(|| {
            (
                first.int_dci,
                u32::from(first.int_shape.max_packet_at(base.speed)),
                interrupt_interval(base.speed, first.int_b_interval),
            )
        })
    } else {
        None
    };
    let index = claim.primary();
    let published = |iface: &InterfaceInfo| {
        iface.is_servable() || is_control_only(config_bytes, iface.interface_number) == Ok(true)
    };
    let plan = if descriptor.is_hub() {
        [None; MAX_INTERFACES]
    } else if interfaces.iter().flatten().any(published) {
        let plan = engine
            .lock()?
            .plan_interfaces(index, &interfaces, published);
        for (target, _) in plan.iter().flatten() {
            if *target != index {
                let _ = claim.entries.try_push(*target);
            }
        }
        plan
    } else {
        return Err(DriverError::Unsupported);
    };
    // Only a storage interface's identity rests on the serial.
    let serial_number = if plan
        .iter()
        .flatten()
        .any(|(_, iface)| iface.is_mass_storage())
    {
        read_serial_number(engine, slot, descriptor.serial_number_index, crumb).await?
    } else {
        None
    };
    // Every Configure Endpoint rewrites the slot context, so its Context
    // Entries must cover the highest DCI any served sibling uses (xHCI
    // §6.2.2).
    let mut max_dci = DCI_CONTROL;
    for (_, iface) in plan.iter().flatten() {
        max_dci = max_dci
            .max(iface.int_dci)
            .max(iface.bulk_in.dci)
            .max(iface.bulk_out.dci)
            .max(iface.bulk_in2.dci)
            .max(iface.bulk_out2.dci);
    }
    let mut rings: [Option<ConfiguredRings>; MAX_INTERFACES] = [const { None }; MAX_INTERFACES];
    for (entry, rings_slot) in plan.iter().zip(rings.iter_mut()) {
        let Some((target, iface)) = entry else {
            continue;
        };
        *rings_slot =
            Some(configure_interface(engine, slot, base, *target, iface, max_dci, crumb).await?);
    }
    crumb.stage = EnumStage::SetConfiguration;
    request(
        engine,
        slot,
        setup_set_configuration(first.configuration_value),
        &mut [],
        crumb,
    )
    .await
    .map_err(|fault| fault.error)?;
    engine.lock()?.install_planned(
        &plan,
        &mut rings,
        &Enumerated {
            slot,
            base,
            hub_port,
            parent_hub,
            descriptor,
            serial_number,
            config: config_bytes,
        },
    )?;
    crumb.stage = EnumStage::Configured;
    Ok((descriptor, hub_endpoint))
}

/// Configure one planned interface's endpoints in its `target` entry's own
/// ring region, returning the built rings: its bulk endpoints when it
/// carries the pair, and its interrupt-IN endpoint when it has one.
async fn configure_interface<H: RegisterBlock, M: DmaBank>(
    engine: &Engine<H, M>,
    slot: u8,
    base: SlotCtxBase,
    target: usize,
    iface: &InterfaceInfo,
    max_dci: u8,
    crumb: &mut Crumb,
) -> Result<ConfiguredRings, DriverError> {
    let region = engine.lock()?.device_region(target)?;
    crumb.stage = EnumStage::ConfigureEndpoint;
    let bulk_rings = if iface.has_bulk_pair() {
        let mut rings = None;
        configure(
            engine,
            slot,
            |core| {
                let (built, command) =
                    core.stage_bulk_endpoints(slot, base, iface, region, max_dci)?;
                rings = Some(built);
                Ok(command)
            },
            crumb,
        )
        .await?;
        rings
    } else {
        None
    };
    let int_ring = if iface.int_dci == DCI_CONTROL {
        None
    } else {
        let mut ring = None;
        configure(
            engine,
            slot,
            |core| {
                let (built, command) =
                    core.stage_interrupt_endpoint(slot, base, iface, region, max_dci)?;
                ring = Some(built);
                Ok(command)
            },
            crumb,
        )
        .await?;
        ring
    };
    Ok(ConfiguredRings {
        int_ring,
        bulk_rings,
    })
}

/// Install the hub addressed on `slot` and tell the controller the slot is a
/// hub, so devices addressed behind it are routed and their split
/// transactions scheduled (xHCI §6.2.2). The hub keeps the region it was
/// enumerated on (`device_region`); `base` is the topology it was addressed
/// with, which its downstream devices inherit. Its table entry is taken only
/// once every step that waits is done, so no other operation sees it
/// half-installed.
///
/// # Errors
///
/// * [`DriverError::DeviceFault`] if `slot` has no pipe, or the controller
///   rejects the Configure Endpoint.
/// * [`DriverError::NoSpace`] if the hub table is full.
/// * [`DriverError::NotFound`] if the parent names no live hub.
/// * [`DriverError::BadMagic`] if the hub descriptor is forged.
async fn install_hub<H: RegisterBlock, M: DmaBank>(
    engine: &Engine<H, M>,
    slot: u8,
    (parent, parent_port): (Option<usize>, u8),
    base: SlotCtxBase,
    device_region: usize,
    int_endpoint: Option<(u8, u32, u32)>,
    crumb: &mut Crumb,
) -> Result<usize, DriverError> {
    let depth = {
        let core = engine.lock()?;
        core.pipe(slot)?;
        match parent {
            None => 0,
            Some(parent) => core.hub(parent).ok_or(DriverError::NotFound)?.depth + 1,
        }
    };
    let superspeed = base.speed == SPEED_SUPER;
    let descriptor = read_hub_topology(engine, slot, superspeed, crumb).await?;
    crumb.stage = EnumStage::ConfigureEndpoint;
    configure(
        engine,
        slot,
        |core| core.stage_hub_slot(slot, descriptor),
        crumb,
    )
    .await?;
    // A `SuperSpeed` hub must be told its tier depth before it can decode
    // downstream route strings (USB 3.2 §10.16.2.7).
    if superspeed {
        request(engine, slot, setup_set_hub_depth(depth), &mut [], crumb)
            .await
            .map_err(|fault| fault.error)?;
    }
    engine.lock()?.install_hub_entry(
        slot,
        (parent, parent_port),
        base,
        (depth, descriptor),
        device_region,
        int_endpoint,
    )
}

/// Read the hub class descriptor (USB 2.0 §11.23.2.1, USB 3.2 §10.15.2.1)
/// the hub on `slot` serves. A refusal STALL is retried a bounded number of
/// times; a transport fault is not, since a timeout compounds and a fault
/// will not heal.
pub(crate) async fn read_hub_topology<H: RegisterBlock, M: DmaBank>(
    engine: &Engine<H, M>,
    slot: u8,
    superspeed: bool,
    crumb: &mut Crumb,
) -> Result<HubDescriptor, DriverError> {
    let (desc_type, length) = HubDescriptor::request(superspeed);
    let want = u16::try_from(length).map_err(|_| DriverError::LengthOutOfRange)?;
    let mut last = DriverError::BadMagic;
    for _ in 0..HUB_DESC_ATTEMPTS {
        let mut desc = [0u8; HUB_DESC_REQUEST];
        let transferred = match request(
            engine,
            slot,
            setup_get_hub_descriptor(desc_type, want),
            &mut desc[..length],
            crumb,
        )
        .await
        {
            Ok(transferred) => transferred,
            Err(fault) if fault.error == DriverError::EndpointStalled => {
                last = fault.error;
                continue;
            }
            Err(fault) => return Err(fault.error),
        };
        match HubDescriptor::decode(&desc[..transferred], superspeed) {
            Some(hub) => return Ok(hub),
            None => last = DriverError::BadMagic,
        }
    }
    Err(last)
}

/// Power, watch and walk the freshly installed hub at `hub_index`: assert
/// `PORT_POWER` on every downstream port, wait the power-on-good window, arm
/// its status-change watch, and hand each port to its own operation. The
/// watch is armed even with nothing connected, so the first later hot-plug
/// is seen.
///
/// # Errors
///
/// A fault powering a port or arming the watch.
pub(crate) async fn descend_hub<H: RegisterBlock, M: DmaBank>(
    engine: &Engine<H, M>,
    hub_index: usize,
    crumb: &mut Crumb,
) -> Result<(), DriverError> {
    let ports = engine
        .lock()?
        .hub(hub_index)
        .ok_or(DriverError::NotFound)?
        .num_ports;
    for port in 1..=ports {
        hub_request(
            engine,
            hub_index,
            setup_set_port_feature(PORT_FEATURE_POWER, port),
        )
        .await?;
    }
    engine.sleep_us(HUB_POWER_ON_GOOD_US).await?;
    configure_hub_watch(engine, hub_index, crumb).await?;
    let mut core = engine.lock()?;
    for port in 1..=ports {
        core.ask(Work::Port(Upstream::Hub(hub_index, port)));
    }
    Ok(())
}

/// Configure and arm the hub at `hub_index`'s status-change endpoint (USB
/// 2.0 §11.12.3), so a downstream connect or disconnect on that tier is
/// delivered on the event ring. A hub that reports no such endpoint is
/// served without hot-plug on that tier rather than failing; a compliant hub
/// always has one.
pub(crate) async fn configure_hub_watch<H: RegisterBlock, M: DmaBank>(
    engine: &Engine<H, M>,
    hub_index: usize,
    crumb: &mut Crumb,
) -> Result<(), DriverError> {
    let Some(slot) = engine.lock()?.hub_watch_slot(hub_index)? else {
        return Ok(());
    };
    crumb.stage = EnumStage::ConfigureEndpoint;
    configure(engine, slot, |core| core.stage_hub_watch(hub_index), crumb).await?;
    engine.lock()?.rearm_hub_watch(hub_index)
}

/// A class request with no data stage to the hub at `hub_index`.
pub(crate) async fn hub_request<H: RegisterBlock, M: DmaBank>(
    engine: &Engine<H, M>,
    hub_index: usize,
    setup: [u8; 8],
) -> Result<(), DriverError> {
    let slot = engine
        .lock()?
        .hub(hub_index)
        .ok_or(DriverError::NotFound)?
        .slot;
    engine.control(slot, setup, &mut []).await.map(|_| ())
}

/// The hub at `hub_index`'s downstream `port` `wPortStatus` and
/// `wPortChange` (USB 2.0 §11.24.2.7).
pub(crate) async fn hub_port_status_change<H: RegisterBlock, M: DmaBank>(
    engine: &Engine<H, M>,
    hub_index: usize,
    port: u8,
) -> Result<(u16, u16), DriverError> {
    let slot = engine
        .lock()?
        .hub(hub_index)
        .ok_or(DriverError::NotFound)?
        .slot;
    let mut buf = [0u8; 4];
    if engine
        .control(slot, setup_get_port_status(port), &mut buf)
        .await?
        < buf.len()
    {
        return Err(DriverError::DeviceFault);
    }
    Ok((
        u16::from_le_bytes([buf[0], buf[1]]),
        u16::from_le_bytes([buf[2], buf[3]]),
    ))
}

/// Clear each latched change `change` names on the hub at `hub_index`'s
/// downstream `port`. A `SuperSpeed` hub latches a different set — warm
/// reset, link state and config error, and no enable or suspend — and a
/// latch left set keeps its status-change watch firing for ever.
pub(super) async fn clear_hub_port_changes<H: RegisterBlock, M: DmaBank>(
    engine: &Engine<H, M>,
    hub_index: usize,
    port: u8,
    change: u16,
) -> Result<(), DriverError> {
    let superspeed = engine
        .lock()?
        .hub(hub_index)
        .is_some_and(|hub| hub.speed == SPEED_SUPER);
    let features: &[(u16, u8)] = if superspeed {
        &SS_PORT_CHANGE_FEATURES
    } else {
        &PORT_CHANGE_FEATURES
    };
    for &(bit, feature) in features {
        if change & bit != 0 {
            hub_request(engine, hub_index, setup_clear_port_feature(feature, port)).await?;
        }
    }
    Ok(())
}

/// Read and clear every change latched on the hub at `hub_index`'s
/// downstream `port`.
async fn drain_hub_port<H: RegisterBlock, M: DmaBank>(
    engine: &Engine<H, M>,
    hub_index: usize,
    port: u8,
) -> Result<(), DriverError> {
    let (_, change) = hub_port_status_change(engine, hub_index, port).await?;
    clear_hub_port_changes(engine, hub_index, port, change).await
}

/// What enumeration learnt of a device, for its interfaces' table entries.
struct Enumerated<'c> {
    slot: u8,
    base: SlotCtxBase,
    hub_port: u8,
    parent_hub: usize,
    descriptor: DeviceDescriptor,
    serial_number: Option<SerialNumber>,
    config: &'c [u8],
}

impl<H: RegisterBlock, M: DmaBank> UsbDevice<H, M> {
    /// Install one table entry per planned interface, each with the rings
    /// its endpoints were configured on, sharing the device's slot.
    fn install_planned(
        &mut self,
        plan: &[Option<(usize, InterfaceInfo)>; MAX_INTERFACES],
        rings: &mut [Option<ConfiguredRings>; MAX_INTERFACES],
        device: &Enumerated<'_>,
    ) -> Result<(), DriverError> {
        let removable =
            self.attached_removable(device.base.root_port, device.hub_port, device.parent_hub);
        for (entry, rings_slot) in plan.iter().zip(rings.iter_mut()) {
            let (Some((target, iface)), Some(configured)) = (entry, rings_slot.take()) else {
                continue;
            };
            let region = self.device_region(*target)?;
            let int_dci = if configured.int_ring.is_some() {
                iface.int_dci
            } else {
                DCI_CONTROL
            };
            let mut config = Vec::new();
            config
                .try_reserve_exact(device.config.len())
                .map_err(|_| DriverError::OutOfMemory)?;
            config.extend_from_slice(device.config);
            self.install_device_entry(
                *target,
                device.slot,
                device.hub_port,
                device.base,
                device.parent_hub,
                region,
                device.descriptor,
                device.serial_number,
                removable,
                config,
                iface,
                int_dci,
                configured.int_ring,
                configured.bulk_rings,
            )?;
        }
        Ok(())
    }

    /// Stage Address Device for `slot` (§4.3.4): the input control context
    /// (A0 | A1), the slot context from `base`, the EP0 context, and the
    /// DCBAA pointed at the slot's output context. The input context is
    /// cleared first: §6.2.5.1 requires software to initialise it, and a
    /// reused region still holds an earlier device's.
    fn stage_address_device(
        &mut self,
        base: SlotCtxBase,
        slot: u8,
        max_packet: u32,
    ) -> Result<Trb, DriverError> {
        let pipe = self.pipe(slot)?;
        let (ring_off, output_ctx_off) = (pipe.ring_off, pipe.output_ctx);
        self.zero_input_ctx(slot)?;
        self.write_input_ctx(slot, 0, &input_control_dwords(0, 0b11))?;
        self.write_input_ctx(slot, 1, &slot_ctx_dwords(base, u32::from(DCI_CONTROL)))?;
        let ring = self.device_addr_of(ring_off)?;
        self.write_input_ctx(
            slot,
            1 + usize::from(DCI_CONTROL),
            &ep_ctx_dwords(EP_TYPE_CONTROL, max_packet, 0, ring, None),
        )?;
        let output_ctx = self.device_addr_of(output_ctx_off)?;
        self.dma.write(
            self.layout.dcbaa + usize::from(slot) * 8,
            &output_ctx.to_le_bytes(),
        )?;
        Ok(Trb::new(
            TrbType::AddressDevice,
            self.input_ctx_addr(slot)?,
            0,
            trb::control_slot(slot),
        ))
    }

    /// Stage Evaluate Context re-sizing `slot`'s EP0 to the device-reported
    /// `bMaxPacketSize0` (§4.6.7). The input context names only the EP0
    /// context (A1); the controller evaluates just its Max Packet Size
    /// (§6.2.3.3).
    fn stage_evaluate_ep0(&mut self, slot: u8, max_packet: u32) -> Result<Trb, DriverError> {
        let ring_off = self.pipe(slot)?.ring_off;
        self.write_input_ctx(slot, 0, &input_control_dwords(0, 0b10))?;
        let ring = self.device_addr_of(ring_off)?;
        self.write_input_ctx(
            slot,
            1 + usize::from(DCI_CONTROL),
            &ep_ctx_dwords(EP_TYPE_CONTROL, max_packet, 0, ring, None),
        )?;
        Ok(Trb::new(
            TrbType::EvaluateContext,
            self.input_ctx_addr(slot)?,
            0,
            trb::control_slot(slot),
        ))
    }

    /// Build the interface's interrupt-IN ring in `region` and stage the
    /// Configure Endpoint adding it as the descriptor reports it (DCI, max
    /// packet size, service interval — never assumed).
    fn stage_interrupt_endpoint(
        &mut self,
        slot: u8,
        base: SlotCtxBase,
        iface: &InterfaceInfo,
        region: DeviceRegion,
        max_dci: u8,
    ) -> Result<(ProducerRing, Trb), DriverError> {
        let ring = self.build_ring(region.int_ring, super::INT_RING_TRBS)?;
        let ring_base = self.device_addr_of(region.int_ring)?;
        let (max_burst, payload) = iface.int_shape.payload(base.speed);
        self.write_input_ctx(
            slot,
            0,
            &input_control_dwords(0, 1 | (1u32 << u32::from(iface.int_dci))),
        )?;
        self.write_input_ctx(slot, 1, &slot_ctx_dwords(base, u32::from(max_dci)))?;
        self.write_input_ctx(
            slot,
            1 + usize::from(iface.int_dci),
            &ep_ctx_dwords(
                EP_TYPE_INTERRUPT_IN,
                u32::from(iface.int_shape.max_packet_at(base.speed)),
                max_burst,
                ring_base,
                Some(Periodic {
                    interval: interrupt_interval(base.speed, iface.int_b_interval),
                    payload,
                    mult: 0,
                }),
            ),
        )?;
        let command = Trb::new(
            TrbType::ConfigureEndpoint,
            self.input_ctx_addr(slot)?,
            0,
            trb::control_slot(slot),
        );
        Ok((ring, command))
    }

    /// Build the interface's bulk rings in `region` — the IN/OUT pair every
    /// bulk interface carries, and the second pair a UAS interface's four
    /// pipes need, configured only whole — and stage the one Configure
    /// Endpoint adding them, Context Entries raised to `max_dci`.
    fn stage_bulk_endpoints(
        &mut self,
        slot: u8,
        base: SlotCtxBase,
        interface: &InterfaceInfo,
        region: DeviceRegion,
        max_dci: u8,
    ) -> Result<(BulkRings, Trb), DriverError> {
        let in_ring = self.build_ring(region.bulk_in_ring, super::BULK_RING_TRBS)?;
        let out_ring = self.build_ring(region.bulk_out_ring, super::BULK_RING_TRBS)?;
        let secondary = interface.bulk_in2.dci != 0 && interface.bulk_out2.dci != 0;
        let (second_in, second_out) = if secondary {
            (
                Some(self.build_ring(region.bulk_in2_ring, super::BULK_RING_TRBS)?),
                Some(self.build_ring(region.bulk_out2_ring, super::BULK_RING_TRBS)?),
            )
        } else {
            (None, None)
        };
        let pipes = [
            (EP_TYPE_BULK_IN, interface.bulk_in, region.bulk_in_ring),
            (EP_TYPE_BULK_OUT, interface.bulk_out, region.bulk_out_ring),
            (EP_TYPE_BULK_IN, interface.bulk_in2, region.bulk_in2_ring),
            (EP_TYPE_BULK_OUT, interface.bulk_out2, region.bulk_out2_ring),
        ];
        let served = &pipes[..if secondary { 4 } else { 2 }];
        let add_flags = served.iter().fold(1, |flags, (_, pipe, _)| {
            flags | (1u32 << u32::from(pipe.dci))
        });
        self.write_input_ctx(slot, 0, &input_control_dwords(0, add_flags))?;
        self.write_input_ctx(slot, 1, &slot_ctx_dwords(base, u32::from(max_dci)))?;
        for &(ep_type, pipe, ring) in served {
            let ring = self.device_addr_of(ring)?;
            self.write_input_ctx(
                slot,
                1 + usize::from(pipe.dci),
                &ep_ctx_dwords(
                    ep_type,
                    u32::from(pipe.max_packet),
                    pipe.burst(base.speed),
                    ring,
                    None,
                ),
            )?;
        }
        let command = Trb::new(
            TrbType::ConfigureEndpoint,
            self.input_ctx_addr(slot)?,
            0,
            trb::control_slot(slot),
        );
        Ok((
            BulkRings {
                in_ring,
                out_ring,
                in2_ring: second_in,
                out2_ring: second_out,
            },
            command,
        ))
    }

    /// Stage the `A0`-only Configure Endpoint that marks `slot` a hub: the
    /// live output slot context with the Hub bit, Number of Ports and TT
    /// Think Time set from `hub` (single-TT).
    fn stage_hub_slot(&mut self, slot: u8, hub: HubDescriptor) -> Result<Trb, DriverError> {
        let mut slot_ctx = self.read_ctx(self.pipe(slot)?.output_ctx)?;
        slot_ctx[0] = (slot_ctx[0] | SLOT_CTX_HUB) & !SLOT_CTX_MTT;
        slot_ctx[1] = (slot_ctx[1] & !(0xFFu32 << SLOT_CTX_NUM_PORTS_SHIFT))
            | (u32::from(hub.ports) << SLOT_CTX_NUM_PORTS_SHIFT);
        slot_ctx[2] = (slot_ctx[2] & !SLOT_CTX_TTT_MASK)
            | (u32::from(hub.tt_think_time) << SLOT_CTX_TTT_SHIFT);
        self.write_input_ctx(slot, 0, &input_control_dwords(0, 1))?;
        self.write_input_ctx(slot, 1, &slot_ctx)?;
        Ok(Trb::new(
            TrbType::ConfigureEndpoint,
            self.input_ctx_addr(slot)?,
            0,
            trb::control_slot(slot),
        ))
    }

    /// The slot of the hub at `hub_index`, when it has a status-change
    /// endpoint to watch.
    fn hub_watch_slot(&self, hub_index: usize) -> Result<Option<u8>, DriverError> {
        let hub = self.hub(hub_index).ok_or(DriverError::NotFound)?;
        Ok(hub.int_endpoint.map(|_| hub.slot))
    }

    /// Build the hub at `hub_index`'s status-change ring and stage the
    /// Configure Endpoint (A0 | A(dci)) adding the endpoint, copying the live
    /// slot context and raising its Context Entries to cover the new DCI.
    fn stage_hub_watch(&mut self, hub_index: usize) -> Result<Trb, DriverError> {
        let hub = self.hub(hub_index).ok_or(DriverError::NotFound)?;
        let (dci, max_packet, interval) = hub.int_endpoint.ok_or(DriverError::NotFound)?;
        let (slot, region) = (hub.slot, hub.region);
        let output_ctx = self.pipe(slot)?.output_ctx;
        let ring = self.build_ring(region.int_ring, super::RING_TRBS)?;
        let ring_base = self.device_addr_of(region.int_ring)?;
        if let Some(hub) = self.hub_mut(hub_index) {
            hub.int_ring = Some(ring);
            hub.int_dci = dci;
        }
        let mut slot_ctx = self.read_ctx(output_ctx)?;
        slot_ctx[0] = (slot_ctx[0] & !super::SLOT_CTX_CONTEXT_ENTRIES_MASK)
            | (u32::from(dci) << super::SLOT_CTX_CONTEXT_ENTRIES_SHIFT);
        self.write_input_ctx(
            slot,
            0,
            &input_control_dwords(0, 1 | (1u32 << u32::from(dci))),
        )?;
        self.write_input_ctx(slot, 1, &slot_ctx)?;
        self.write_input_ctx(
            slot,
            1 + usize::from(dci),
            &ep_ctx_dwords(
                EP_TYPE_INTERRUPT_IN,
                max_packet,
                0,
                ring_base,
                Some(Periodic {
                    interval,
                    payload: max_packet,
                    mult: 0,
                }),
            ),
        )?;
        Ok(Trb::new(
            TrbType::ConfigureEndpoint,
            self.input_ctx_addr(slot)?,
            0,
            trb::control_slot(slot),
        ))
    }

    /// Record the hub addressed on `slot`, its descriptor read and its slot
    /// configured, in a hub-table entry of its own, answering its index.
    fn install_hub_entry(
        &mut self,
        slot: u8,
        (parent, parent_port): (Option<usize>, u8),
        base: SlotCtxBase,
        (depth, descriptor): (u8, HubDescriptor),
        device_region: usize,
        int_endpoint: Option<(u8, u32, u32)>,
    ) -> Result<usize, DriverError> {
        let hub_index = self.claim_hub_entry()?;
        let region = HubRegion::at(self.dma.grow(HubRegion::layout_len())?);
        let serial = self.next_attach_serial();
        self.hubs[hub_index] = Some(super::HubState {
            slot,
            serial,
            parent,
            parent_port,
            root_port: base.root_port,
            route_string: base.route_string,
            depth,
            speed: base.speed,
            num_ports: descriptor.ports,
            fixed_ports: descriptor.fixed_ports,
            tt_hub_slot: base.tt_hub_slot,
            tt_port: base.tt_port,
            region,
            device_region,
            int_endpoint,
            int_dci: 0,
            int_ring: None,
            pending: None,
            watch: Ladder::default(),
        });
        Ok(hub_index)
    }

    /// Give `slot`'s attempt up: close its pipe, drop any entry the attempt
    /// installed on it, and post its Disable Slot with the entries' regions
    /// withheld until it confirms.
    ///
    /// # Errors
    ///
    /// As [`Self::post_command`]: the regions are then withheld until a
    /// controller reset.
    fn tear_down_attempt(
        &mut self,
        slot: u8,
        entries: &[usize],
    ) -> Result<super::route::CommandId, DriverError> {
        self.forget_entries(entries);
        match self.post_disable_slot(slot) {
            Ok(command) => {
                self.release_entries(entries, SlotHold::Pending { slot, command });
                Ok(command)
            }
            Err(err) => {
                self.release_entries(entries, SlotHold::Held);
                Err(err)
            }
        }
    }

    /// Drop any table entry an attempt installed on `entries`: their slot is
    /// going.
    fn forget_entries(&mut self, entries: &[usize]) {
        for &index in entries {
            if let Some(entry) = self.devices.get_mut(index) {
                *entry = None;
            }
        }
    }

    /// Clear `slot`'s DCBAA entry: the controller confirmed it disabled, and
    /// re-enabling it rewrites the entry first.
    fn clear_dcbaa_entry(&mut self, slot: u8) {
        let _ = self.dma.write(
            self.layout.dcbaa + usize::from(slot) * 8,
            &0u64.to_le_bytes(),
        );
    }

    /// Retire the regions of `entries` under `hold`.
    fn release_entries(&mut self, entries: &[usize], hold: SlotHold) {
        for &index in entries {
            self.retire_device_region(index, hold);
        }
    }

    /// Record a port the walk could not serve, keeping the first fault since
    /// the driver last took one.
    fn port_skipped(&mut self, fault: AttachFault) {
        self.skipped_ports = self.skipped_ports.saturating_add(1);
        self.attach_fault.get_or_insert(fault);
    }

    /// Whether `port`'s attachment was torn down by a recovery that owes it a
    /// fresh enumeration, consuming the debt.
    fn take_reenumeration(&mut self, port: Upstream) -> bool {
        let before = self.reenumerations.len();
        self.reenumerations.retain(|&owed| owed != port);
        self.reenumerations.len() != before
    }

    /// Mark the attachment a re-enumeration produced, so a recovery that
    /// fails it again gives it up rather than resetting it once more.
    fn mark_reset_produced(&mut self, attachment: Attachment) {
        match attachment {
            Attachment::Hub(hub_index) => {
                if let Some(hub) = self.hub_mut(hub_index) {
                    hub.watch = Ladder::after_reset();
                }
            }
            Attachment::Device(index) => {
                let slot = self.device(index).map(|device| device.slot);
                for device in self.devices.iter_mut().flatten() {
                    if Some(device.slot) == slot {
                        device.recovery = Ladder::after_reset();
                    }
                }
            }
        }
    }
}
