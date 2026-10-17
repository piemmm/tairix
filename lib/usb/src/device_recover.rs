//! Recovery operations: taking an endpoint back from a transfer that halted
//! it or overran its deadline, each a future of its own, so a device being
//! recovered never holds up another device's transfers.
//!
//! A ring is rebuilt only once the controller has confirmed its endpoint
//! stopped or reset, so no completion for a TD abandoned on the old ring can
//! land in a slot of the new one.

use tairix_abi::{DriverError, RegisterBlock};

use super::ops::Engine;
use super::port::{hub_port_status_change, hub_request};
use super::route::Settle;
use super::{
    hub_port_connected, hub_port_enabled, input_control_dwords, setup_clear_endpoint_halt,
    Abandoned, Attachment, BulkPipe, DmaBank, HubEvent, OnHalt, PortLink, Rung, Upstream,
    BULK_RING_TRBS, DCI_CONTROL, EP_CTX_STATE_MASK, INT_RING_TRBS, RING_TRBS,
};
use crate::ring::ProducerRing;
use crate::trb::{self, CompletionCode, Trb, TrbType};

/// Bring endpoint `dci` of `slot` to Stopped, trying first the command where
/// `abandoned` says it is needs. One the controller refuses on the
/// endpoint's state (Context State Error) is followed by the other, and one
/// refused by both finds it stopped already, so a recovery that failed
/// part-way can run again. A transfer abandoned on the endpoint at its
/// deadline is reported as soon as the stop makes its dequeue pointer the
/// controller's, before any ring is rebuilt over it.
pub(super) async fn stop_endpoint<H: RegisterBlock, M: DmaBank>(
    engine: &Engine<H, M>,
    slot: u8,
    dci: u8,
    abandoned: Abandoned,
) -> Result<(), DriverError> {
    let stopped = bring_to_stopped(engine, slot, dci, abandoned).await;
    if let Ok(mut core) = engine.lock() {
        core.settle_abandoned(slot, dci, stopped.is_ok());
    }
    stopped
}

async fn bring_to_stopped<H: RegisterBlock, M: DmaBank>(
    engine: &Engine<H, M>,
    slot: u8,
    dci: u8,
    abandoned: Abandoned,
) -> Result<(), DriverError> {
    let commands = match abandoned {
        Abandoned::Halted => [TrbType::ResetEndpoint, TrbType::StopEndpoint],
        Abandoned::Running => [TrbType::StopEndpoint, TrbType::ResetEndpoint],
    };
    for command in commands {
        let event = engine
            .issue(
                Trb::new(
                    command,
                    0,
                    0,
                    trb::control_slot(slot) | trb::control_endpoint(dci),
                ),
                Settle::Nothing,
            )
            .await?;
        match event.completion_code() {
            Ok(CompletionCode::Success) => return Ok(()),
            Ok(CompletionCode::ContextStateError) => {}
            _ => return Err(DriverError::DeviceFault),
        }
    }
    Ok(())
}

/// Rebuild stopped endpoint `dci` of `slot`'s `trbs`-slot ring at `ring_off`
/// with nothing on it and point the controller's dequeue at its base,
/// returning the ring for the caller to install now the controller has taken
/// it.
pub(super) async fn restart_ring<H: RegisterBlock, M: DmaBank>(
    engine: &Engine<H, M>,
    (slot, dci): (u8, u8),
    ring_off: usize,
    trbs: usize,
) -> Result<ProducerRing, DriverError> {
    let (ring, base) = {
        let mut core = engine.lock()?;
        let ring = core.build_ring(ring_off, trbs)?;
        (ring, core.device_addr_of(ring_off)?)
    };
    // Dequeue Cycle State 1, matching the fresh ring.
    engine
        .command(Trb::new(
            TrbType::SetTrDequeuePointer,
            base | 1,
            0,
            trb::control_slot(slot) | trb::control_endpoint(dci),
        ))
        .await?;
    Ok(ring)
}

/// Take `slot`'s control pipe back from the TD that failed on it: stop it,
/// rebuild its ring at the base and repoint the controller there (§4.6.10).
/// No device-side `CLEAR_FEATURE` is needed: a control pipe starts over at
/// its next SETUP (USB 2.0 §8.5.3.4). The pipe's queue resumes, or fails
/// whole when the controller would not give the endpoint back.
pub(super) async fn recover_pipe<H: RegisterBlock, M: DmaBank>(
    engine: Engine<H, M>,
    slot: u8,
    abandoned: Abandoned,
) {
    let recovered = take_back_pipe(&engine, slot, abandoned).await;
    if let Ok(mut core) = engine.lock() {
        core.pipe_recovered(slot, recovered);
    }
}

async fn take_back_pipe<H: RegisterBlock, M: DmaBank>(
    engine: &Engine<H, M>,
    slot: u8,
    abandoned: Abandoned,
) -> Result<(), DriverError> {
    stop_endpoint(engine, slot, DCI_CONTROL, abandoned).await?;
    let ring_off = engine.lock()?.pipe(slot)?.ring_off;
    let ring = restart_ring(engine, (slot, DCI_CONTROL), ring_off, RING_TRBS).await?;
    engine.lock()?.pipe_mut(slot)?.ring = ring;
    Ok(())
}

/// Whether the interface at `index` is still the one attached at `serial`.
fn same_device<H: RegisterBlock, M: DmaBank>(
    engine: &Engine<H, M>,
    index: usize,
    serial: u64,
) -> bool {
    engine.lock().is_ok_and(|core| {
        core.device(index)
            .is_some_and(|device| device.serial == serial)
    })
}

/// Recover the halted interrupt-IN endpoint of the interface at `index`, as
/// attached at `serial`: reset it, rebuild its ring and clear the device's
/// halt, re-arming the endpoint once it serves. A halt observed while the
/// recovery ran climbs the endpoint's ladder: recovered at once while it
/// allows, left owed to the driver's paced retry otherwise.
pub(super) async fn recover_reports<H: RegisterBlock, M: DmaBank>(
    engine: Engine<H, M>,
    index: usize,
    serial: u64,
) {
    loop {
        if !same_device(&engine, index, serial) {
            return;
        }
        let recovered = attempt_report_recovery(&engine, index).await;
        let Ok(mut core) = engine.lock() else {
            return;
        };
        match recovered {
            Ok(()) => {
                let _ = core.ensure_reports_armed(index);
            }
            Err(_) => {
                if let Some(device) = core.device_mut(index) {
                    device.recovery.failed();
                }
            }
        }
        let again = match core.device_mut(index) {
            Some(device) if device.int_recovery_pending => {
                device.int_recovery_pending = false;
                device.recovery.halted() == OnHalt::Recover
            }
            _ => false,
        };
        if !again {
            return;
        }
    }
}

/// One recovery of the interface at `index`'s interrupt-IN endpoint, with
/// captures for it held off while it runs.
async fn attempt_report_recovery<H: RegisterBlock, M: DmaBank>(
    engine: &Engine<H, M>,
    index: usize,
) -> Result<(), DriverError> {
    let (ring_off, dci, slot) = {
        let mut core = engine.lock()?;
        let device = core.device_mut(index).ok_or(DriverError::NotFound)?;
        device.int_recovery_pending = false;
        device.int_recovering = true;
        (device.region.int_ring, device.int_dci, device.slot)
    };
    let recovered = async {
        stop_endpoint(engine, slot, dci, Abandoned::Halted).await?;
        let ring = restart_ring(engine, (slot, dci), ring_off, INT_RING_TRBS).await?;
        engine
            .lock()?
            .device_mut(index)
            .ok_or(DriverError::NotFound)?
            .int_ring = Some(ring);
        // Clear the device-side halt, resetting its data toggle to match the
        // rebuilt ring.
        engine
            .control(slot, setup_clear_endpoint_halt(dci), &mut [])
            .await
            .map(|_| ())
    }
    .await;
    let mut core = engine.lock()?;
    let device = core.device_mut(index).ok_or(DriverError::NotFound)?;
    device.int_recovering = false;
    if recovered.is_ok() {
        device.last_report_fault_code = 0;
    }
    recovered
}

/// The paced retry of the interface at `index`'s owed report recovery: a
/// port read gone is torn down, a port its hub disabled escalates at once,
/// and otherwise the ladder decides between another recovery and a reset.
pub(super) async fn retry_reports<H: RegisterBlock, M: DmaBank>(
    engine: Engine<H, M>,
    index: usize,
    serial: u64,
) {
    if !same_device(&engine, index, serial) {
        return;
    }
    let link = device_port_link(&engine, index).await;
    let rung = {
        let Ok(mut core) = engine.lock() else {
            return;
        };
        let Some(device) = core.device_mut(index) else {
            return;
        };
        match link {
            Ok(PortLink::Vacant) => {
                core.detach_device(index);
                return;
            }
            // A port its hub disabled carries a device no endpoint reset
            // reaches.
            Ok(PortLink::Disabled) => Rung::Escalate(device.recovery.escalation()),
            Ok(PortLink::Enabled) | Err(_) => device.recovery.climb(),
        }
    };
    let escalation = match rung {
        Rung::Escalate(escalation) => escalation,
        Rung::Recover => {
            if link.is_ok() && attempt_report_recovery(&engine, index).await.is_ok() {
                if let Ok(mut core) = engine.lock() {
                    let _ = core.ensure_reports_armed(index);
                }
                return;
            }
            let Ok(mut core) = engine.lock() else {
                return;
            };
            let Some(escalation) = core
                .device_mut(index)
                .and_then(|device| device.recovery.retry_failed())
            else {
                return;
            };
            escalation
        }
    };
    if let Ok(mut core) = engine.lock() {
        let _ = core.escalate(Attachment::Device(index), escalation);
    }
}

/// The live connection of the port the interface at `index` hangs off.
async fn device_port_link<H: RegisterBlock, M: DmaBank>(
    engine: &Engine<H, M>,
    index: usize,
) -> Result<PortLink, DriverError> {
    let upstream = engine
        .lock()?
        .device(index)
        .ok_or(DriverError::NotFound)?
        .upstream();
    let (connected, enabled) = match upstream {
        Upstream::Root(port) => {
            let status = engine.lock()?.xhci.port_status(port)?;
            (status.connected(), status.enabled())
        }
        Upstream::Hub(hub_index, port) => {
            let (status, _change) = hub_port_status_change(engine, hub_index, port).await?;
            (hub_port_connected(status), hub_port_enabled(status))
        }
    };
    Ok(match (connected, enabled) {
        (false, _) => PortLink::Vacant,
        (true, false) => PortLink::Disabled,
        (true, true) => PortLink::Enabled,
    })
}

/// Whether the hub at `index` is still the one attached at `serial`.
fn same_hub<H: RegisterBlock, M: DmaBank>(
    engine: &Engine<H, M>,
    index: usize,
    serial: u64,
) -> bool {
    engine
        .lock()
        .is_ok_and(|core| core.hub(index).is_some_and(|hub| hub.serial == serial))
}

/// Recover the hub at `index`'s halted status-change endpoint, then hand
/// every port to its operation: a change the lost report carried is acted
/// on. A failed recovery is owed to the driver's paced retry.
pub(super) async fn recover_hub_watch<H: RegisterBlock, M: DmaBank>(
    engine: Engine<H, M>,
    index: usize,
    serial: u64,
) {
    if !same_hub(&engine, index, serial) {
        return;
    }
    let reset = reset_hub_watch(&engine, index).await;
    let Ok(mut core) = engine.lock() else {
        return;
    };
    match reset {
        Ok(()) => {
            core.reconcile_hub_ports(index);
            core.note_topology(HubEvent::HubRecovered(index));
        }
        Err(_) => {
            if let Some(hub) = core.hub_mut(index) {
                hub.watch.failed();
            }
        }
    }
}

/// The paced retry of the hub at `index`'s owed watch recovery: another
/// recovery while its ladder allows, a reset of its port beyond that.
pub(super) async fn retry_hub_watch<H: RegisterBlock, M: DmaBank>(
    engine: Engine<H, M>,
    index: usize,
    serial: u64,
) {
    if !same_hub(&engine, index, serial) {
        return;
    }
    let rung = match engine.lock() {
        Ok(mut core) => match core.hub_mut(index) {
            Some(hub) => hub.watch.climb(),
            None => return,
        },
        Err(_) => return,
    };
    let escalation = match rung {
        Rung::Escalate(escalation) => escalation,
        Rung::Recover => {
            if reset_hub_watch(&engine, index).await.is_ok() {
                if let Ok(mut core) = engine.lock() {
                    core.reconcile_hub_ports(index);
                    core.note_topology(HubEvent::HubRecovered(index));
                }
                return;
            }
            let Ok(mut core) = engine.lock() else {
                return;
            };
            let Some(escalation) = core.hub_mut(index).and_then(|hub| hub.watch.retry_failed())
            else {
                return;
            };
            escalation
        }
    };
    if let Ok(mut core) = engine.lock() {
        let _ = core.escalate(Attachment::Hub(index), escalation);
    }
}

/// Reset the hub at `index`'s status-change endpoint in place: bring it to
/// Stopped from its halt, rebuild its ring, clear the hub's own halt on it
/// and re-arm it.
async fn reset_hub_watch<H: RegisterBlock, M: DmaBank>(
    engine: &Engine<H, M>,
    index: usize,
) -> Result<(), DriverError> {
    let (slot, dci, ring_off) = {
        let core = engine.lock()?;
        let hub = core.hub(index).ok_or(DriverError::NotFound)?;
        (hub.slot, hub.int_dci, hub.region.int_ring)
    };
    stop_endpoint(engine, slot, dci, Abandoned::Halted).await?;
    let ring = restart_ring(engine, (slot, dci), ring_off, RING_TRBS).await?;
    engine
        .lock()?
        .hub_mut(index)
        .ok_or(DriverError::NotFound)?
        .int_ring = Some(ring);
    hub_request(engine, index, setup_clear_endpoint_halt(dci)).await?;
    engine.lock()?.rearm_hub_watch(index)
}

/// Recover the interface at `index`'s bulk `pipe` after a completion that
/// halted it (xHCI §4.8.3): answer every TD the halt abandoned with
/// `failure`, restart the pipe empty with the controller's toggle reset,
/// then clear the device-side halt so the device's toggle resets too (USB
/// 2.0 §9.4.5). The failed TD is answered once the pipe serves again.
pub(super) async fn recover_bulk<H: RegisterBlock, M: DmaBank>(
    engine: Engine<H, M>,
    index: usize,
    serial: u64,
    pipe: BulkPipe,
    failure: DriverError,
) {
    if !same_device(&engine, index, serial) {
        return;
    }
    let recovered = async {
        reset_bulk_endpoint(&engine, index, pipe, Some(failure)).await?;
        let (slot, dci) = {
            let core = engine.lock()?;
            let device = core.device(index).ok_or(DriverError::NotFound)?;
            (device.slot, device.bulk_dci(pipe))
        };
        // On the device's own control pipe, never a hub's.
        engine
            .control(slot, setup_clear_endpoint_halt(dci), &mut [])
            .await
            .map(|_| ())
    }
    .await;
    if let Ok(mut core) = engine.lock() {
        core.bulk_recovered(index, pipe, recovered);
    }
}

/// Restart the interface at `index`'s bulk `pipe` after it completed a TD it
/// was never given — its ring and the controller's dequeue disagree. The
/// oldest TD queued is answered with the fault and the rest abandoned with
/// it; the pipe restarts empty.
pub(super) async fn resync_bulk<H: RegisterBlock, M: DmaBank>(
    engine: Engine<H, M>,
    index: usize,
    serial: u64,
    pipe: BulkPipe,
) {
    if !same_device(&engine, index, serial) {
        return;
    }
    let resynced = async {
        let (_, _, dci, slot) = engine.lock()?.bulk_params(index, pipe)?;
        stop_endpoint(&engine, slot, dci, Abandoned::Running).await?;
        engine
            .lock()?
            .abandon_bulk_tds(index, pipe, Some(DriverError::DeviceFault))?;
        restart_bulk_ring(&engine, index, pipe).await
    }
    .await;
    if let Ok(mut core) = engine.lock() {
        core.bulk_recovered(index, pipe, resynced);
    }
}

/// Stop the interface at `index`'s bulk `pipe` and restart it empty,
/// answering nothing: the transfer queued on it was answered at its
/// deadline, and must never complete late into a later one.
pub(crate) async fn abort_bulk<H: RegisterBlock, M: DmaBank>(
    engine: &Engine<H, M>,
    index: usize,
    pipe: BulkPipe,
) -> Result<(), DriverError> {
    let (_, _, dci, slot) = engine.lock()?.bulk_params(index, pipe)?;
    stop_endpoint(engine, slot, dci, Abandoned::Running).await?;
    {
        let mut core = engine.lock()?;
        core.abandon_bulk_tds(index, pipe, None)?;
        if let Some(device) = core.device_mut(index) {
            device.last_bulk_fault_code = 0;
        }
    }
    restart_bulk_ring(engine, index, pipe).await
}

/// Clear the halt of the interface at `index`'s bulk endpoint at `address`,
/// on the device and in the controller, restarting the pipe empty with both
/// data toggles at `DATA0`.
pub(crate) async fn clear_bulk_halt<H: RegisterBlock, M: DmaBank>(
    engine: &Engine<H, M>,
    index: usize,
    pipe: BulkPipe,
) -> Result<(), DriverError> {
    reset_bulk_endpoint(engine, index, pipe, None).await?;
    let (slot, dci) = {
        let core = engine.lock()?;
        let device = core.device(index).ok_or(DriverError::NotFound)?;
        (device.slot, device.bulk_dci(pipe))
    };
    engine
        .control(slot, setup_clear_endpoint_halt(dci), &mut [])
        .await
        .map(|_| ())
}

async fn restart_bulk_ring<H: RegisterBlock, M: DmaBank>(
    engine: &Engine<H, M>,
    index: usize,
    pipe: BulkPipe,
) -> Result<(), DriverError> {
    let (ring_off, _, dci, slot) = engine.lock()?.bulk_params(index, pipe)?;
    let ring = restart_ring(engine, (slot, dci), ring_off, BULK_RING_TRBS).await?;
    engine
        .lock()?
        .device_mut(index)
        .ok_or(DriverError::NotFound)?
        .set_bulk_ring(pipe, ring);
    Ok(())
}

/// Bring the interface at `index`'s bulk `pipe` back to an empty, running
/// ring with the controller's data toggle at `DATA0`. A halted endpoint is
/// reset (Reset Endpoint, §4.6.8); one that is not halted — a Context State
/// Error — is stopped, dropped and re-added (§4.6.6), since only that resets
/// its toggle. Every TD it held is abandoned, answered with `dropped` if
/// given.
async fn reset_bulk_endpoint<H: RegisterBlock, M: DmaBank>(
    engine: &Engine<H, M>,
    index: usize,
    pipe: BulkPipe,
    dropped: Option<DriverError>,
) -> Result<(), DriverError> {
    let (ring_off, _, dci, slot) = engine.lock()?.bulk_params(index, pipe)?;
    let event = engine
        .issue(
            Trb::new(
                TrbType::ResetEndpoint,
                0,
                0,
                trb::control_slot(slot) | trb::control_endpoint(dci),
            ),
            Settle::Nothing,
        )
        .await?;
    let halted = match event.completion_code() {
        Ok(CompletionCode::Success) => true,
        Ok(CompletionCode::ContextStateError) => {
            stop_endpoint(engine, slot, dci, Abandoned::Running).await?;
            false
        }
        _ => return Err(DriverError::DeviceFault),
    };
    engine.lock()?.abandon_bulk_tds(index, pipe, dropped)?;
    if halted {
        return restart_bulk_ring(engine, index, pipe).await;
    }
    let ring = readd_bulk_endpoint(engine, index, (slot, dci), ring_off).await?;
    engine
        .lock()?
        .device_mut(index)
        .ok_or(DriverError::NotFound)?
        .set_bulk_ring(pipe, ring);
    Ok(())
}

/// Drop and re-add stopped endpoint `dci` of `slot` in one Configure
/// Endpoint, from its live output context with a fresh ring at `ring_off`:
/// the controller resets a re-added endpoint's toggle to `DATA0`.
async fn readd_bulk_endpoint<H: RegisterBlock, M: DmaBank>(
    engine: &Engine<H, M>,
    index: usize,
    (slot, dci): (u8, u8),
    ring_off: usize,
) -> Result<ProducerRing, DriverError> {
    let _input = engine.input_context(slot).await?;
    let (ring, command) = {
        let mut core = engine.lock()?;
        core.device(index).ok_or(DriverError::NotFound)?;
        let output_ctx = core.pipe(slot)?.output_ctx;
        let ring = core.build_ring(ring_off, BULK_RING_TRBS)?;
        let slot_ctx = core.read_ctx(output_ctx)?;
        let ctx_size = core.layout.ctx_size;
        let mut endpoint = core.read_ctx(output_ctx + usize::from(dci) * ctx_size)?;
        endpoint[0] &= !EP_CTX_STATE_MASK;
        let dequeue = core.device_addr_of(ring_off)? | 1;
        endpoint[2] = crate::low_dword(dequeue);
        endpoint[3] = crate::high_dword(dequeue);
        endpoint[5..].fill(0);
        let flag = 1u32 << dci;
        core.write_input_ctx(slot, 0, &input_control_dwords(flag, 1 | flag))?;
        core.write_input_ctx(slot, 1, &slot_ctx)?;
        core.write_input_ctx(slot, 1 + usize::from(dci), &endpoint)?;
        let command = Trb::new(
            TrbType::ConfigureEndpoint,
            core.input_ctx_addr(slot)?,
            0,
            trb::control_slot(slot),
        );
        (ring, command)
    };
    engine.command(command).await?;
    Ok(ring)
}
