//! The engine a class driver's URBs drive: one served interface's view of
//! the controller. Nothing here waits. A control transfer is queued on its
//! slot's pipe and reaped on a later drive; a request that takes the
//! controller several commands — clearing a halt, selecting a setting,
//! stopping a stream, abandoning a transfer — runs as an operation whose
//! result a later drive collects.

use alloc::vec::Vec;
use core::future::Future;

use tairix_abi::usb_urb::{IsoLayout, ReportCompletion, UrbRequest, UsbDirection, UsbTransferType};
use tairix_abi::{DriverError, RegisterBlock};
use tairix_inline::ArrayVec;

use super::ops::{Engine, UsbHost};
use super::recover::{abort_bulk, clear_bulk_halt};
use super::route::{DataStage, Owner, Task};
use super::{address_dci, endpoint_dci, BulkDirection, BulkPipe, DmaBank, TdSpan, DCI_CONTROL};
use crate::transport::{IsoSlotDone, IsoStreamShape, UrbEngine, UrbScope};

/// A request a served interface holds while its operation runs, and the
/// result the operation left.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) struct Request {
    kind: RequestKind,
    result: Option<Result<(), DriverError>>,
}

/// What a held request does.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum RequestKind {
    /// Clear a bulk pipe's halt.
    ClearHalt(BulkPipe),
    /// Select an alternate setting.
    SetInterface { interface: u8, alternate: u8 },
    /// Stop the stream on an endpoint.
    IsoStop(u8),
}

/// One served interface's transfer engine.
pub struct DeviceEngine<'a, 'h, H: RegisterBlock, M: DmaBank> {
    host: &'a mut UsbHost<'h, H, M>,
    index: usize,
}

impl<'h, H: RegisterBlock + 'h, M: DmaBank + 'h> UsbHost<'h, H, M> {
    /// The transfer engine of the interface served at `index`.
    pub fn engine_for(&mut self, index: usize) -> DeviceEngine<'_, 'h, H, M> {
        DeviceEngine { host: self, index }
    }
}

impl<'h, H: RegisterBlock + 'h, M: DmaBank + 'h> DeviceEngine<'_, 'h, H, M> {
    /// Drive control transfer `setup` moving `stage`: queue it on the first
    /// drive, reap it on a later one, its IN bytes landing in `data`.
    fn control(
        &mut self,
        setup: [u8; 8],
        stage: impl FnOnce() -> Result<DataStage, DriverError>,
        data: &mut [u8],
    ) -> Result<Option<usize>, DriverError> {
        let mut core = self.host.device()?;
        let device = core.device(self.index).ok_or(DriverError::NotFound)?;
        let slot = device.slot;
        let Some(id) = device.urb_control else {
            let id = core.submit_control(slot, setup, stage()?)?;
            if let Some(device) = core.device_mut(self.index) {
                device.urb_control = Some(id);
            }
            return Ok(None);
        };
        let Some(answer) = core.take_control(id) else {
            return Ok(None);
        };
        if let Some(device) = core.device_mut(self.index) {
            device.urb_control = None;
        }
        let done = answer.map_err(|fault| fault.error)?;
        data.get_mut(..done.data.len())
            .ok_or(DriverError::DeviceFault)?
            .copy_from_slice(&done.data);
        usize::try_from(done.transferred)
            .map(Some)
            .map_err(|_| DriverError::DeviceFault)
    }

    /// Collect the held request `kind`'s result, or start `run` for it when
    /// none is held. The interface holds one request at a time.
    fn request<F>(
        &mut self,
        kind: RequestKind,
        run: impl FnOnce(Engine<H, M>, usize) -> F,
    ) -> Result<Option<()>, DriverError>
    where
        F: Future<Output = Result<(), DriverError>> + 'h,
    {
        let index = self.index;
        let serial = {
            let mut core = self.host.device()?;
            let device = core.device_mut(index).ok_or(DriverError::NotFound)?;
            match device.request {
                Some(Request {
                    kind: held,
                    result: Some(result),
                }) if held == kind => {
                    device.request = None;
                    return result.map(Some);
                }
                // One runs: this one waits its turn.
                Some(Request { result: None, .. }) => return Ok(None),
                // None, or another's result no drive came back for.
                _ => {}
            }
            device.request = Some(Request { kind, result: None });
            device.serial
        };
        let engine = self.host.engine().clone();
        let work = run(engine.clone(), index);
        let spawned = self.host.spawn(
            Owner::Device {
                index,
                serial,
                task: Task::Request,
            },
            async move {
                let result = work.await;
                if let Ok(mut core) = engine.lock() {
                    if let Some(device) = core
                        .device_mut(index)
                        .filter(|device| device.serial == serial)
                    {
                        device.request = Some(Request {
                            kind,
                            result: Some(result),
                        });
                    }
                }
            },
        );
        if let Err(err) = spawned {
            if let Some(device) = self.host.device()?.device_mut(index) {
                device.request = None;
            }
            return Err(err);
        }
        Ok(None)
    }

    fn bulk_pipe_for(&self, endpoint: u8, direction: BulkDirection) -> Option<BulkPipe> {
        let dci = endpoint_dci(endpoint, direction == BulkDirection::In)?;
        self.host
            .device()
            .ok()?
            .device(self.index)?
            .bulk_pipe_of_dci(dci)
    }

    fn owns_report_endpoint(&self, dci: u8) -> bool {
        dci != DCI_CONTROL
            && self.host.device().is_ok_and(|core| {
                core.device(self.index)
                    .is_some_and(|device| device.int_dci == dci)
            })
    }

    /// Drive one bulk TD on `pipe`, OUT carrying `out` or IN into `data`:
    /// arm it on the first drive, reap it on a later one. Nothing is armed on
    /// a pipe being recovered, and an answer its recovery left is collected
    /// before anything new is armed.
    fn bulk(
        &mut self,
        pipe: BulkPipe,
        data: &mut [u8],
        out: Option<&[u8]>,
    ) -> Result<Option<usize>, DriverError> {
        let mut core = self.host.device()?;
        if core.bulk_recovering(self.index, pipe) {
            return Ok(None);
        }
        if core.bulk_in_flight(self.index, pipe) == 0 && !core.bulk_answer_waiting(self.index, pipe)
        {
            match out {
                Some(bytes) => core.queue_bulk_out(self.index, pipe, bytes)?,
                None => core.queue_bulk_in(self.index, pipe, data.len())?,
            };
            return Ok(None);
        }
        match core.poll_bulk(self.index, data)? {
            Some(complete) if complete.pipe == pipe => complete
                .result
                .and_then(|n| usize::try_from(n).map_err(|_| DriverError::DeviceFault))
                .map(Some),
            // A completion for another pipe cannot belong to the one URB the
            // interface holds.
            Some(_) => Err(DriverError::DeviceFault),
            None => Ok(None),
        }
    }
}

impl<'h, H: RegisterBlock + 'h, M: DmaBank + 'h> UrbEngine for DeviceEngine<'_, 'h, H, M> {
    fn control_in(
        &mut self,
        setup: [u8; 8],
        data: &mut [u8],
    ) -> Result<Option<usize>, DriverError> {
        let len = u32::try_from(data.len()).map_err(|_| DriverError::LengthOutOfRange)?;
        let stage = if len == 0 {
            DataStage::None
        } else {
            DataStage::In(len)
        };
        self.control(setup, || Ok(stage), data)
    }

    fn control_no_data(&mut self, setup: [u8; 8]) -> Result<Option<()>, DriverError> {
        self.control(setup, || Ok(DataStage::None), &mut [])
            .map(|done| done.map(|_| ()))
    }

    fn control_out(&mut self, setup: [u8; 8], data: &[u8]) -> Result<Option<()>, DriverError> {
        let stage = || {
            let mut bytes = Vec::new();
            bytes
                .try_reserve_exact(data.len())
                .map_err(|_| DriverError::OutOfMemory)?;
            bytes.extend_from_slice(data);
            Ok(DataStage::Out(bytes))
        };
        self.control(setup, stage, &mut [])
            .map(|done| done.map(|_| ()))
    }

    fn scope(&self) -> Option<UrbScope> {
        let core = self.host.device().ok()?;
        let device = core.device(self.index)?;
        let mut interfaces = device.streaming.claimed_set();
        interfaces.insert(u16::from(device.identity.interface_number));
        Some(UrbScope {
            interfaces,
            endpoints: device.pipe_dci_mask() | device.streaming.dci_mask(),
        })
    }

    fn interrupt_in(
        &mut self,
        endpoint: u8,
        request: usize,
        data: &mut [u8],
    ) -> Result<Option<ReportCompletion>, DriverError> {
        // The URB names an endpoint *number*; only the interface's own
        // interrupt-IN endpoint is read.
        if !endpoint_dci(endpoint, true).is_some_and(|dci| self.owns_report_endpoint(dci)) {
            return Err(DriverError::OutOfRange);
        }
        self.host.device()?.next_report(self.index, request, data)
    }

    fn set_interface(&mut self, interface: u8, alternate: u8) -> Result<Option<()>, DriverError> {
        self.request(
            RequestKind::SetInterface {
                interface,
                alternate,
            },
            move |engine, index| async move {
                super::iso::select_setting(&engine, index, interface, alternate).await
            },
        )
    }

    fn claim_interface(&mut self, interface: u8) -> Result<(), DriverError> {
        self.host.device()?.claim_interface(self.index, interface)
    }

    fn iso_start(
        &mut self,
        endpoint: u8,
        layout: IsoLayout,
    ) -> Result<IsoStreamShape, DriverError> {
        self.host.device()?.iso_start(self.index, endpoint, layout)
    }

    fn iso_queue(&mut self, endpoint: u8, slot: u16, region: &[u8]) -> Result<(), DriverError> {
        self.host
            .device()?
            .iso_queue(self.index, endpoint, slot, region)
    }

    fn iso_stop(&mut self, endpoint: u8) -> Result<Option<()>, DriverError> {
        self.request(RequestKind::IsoStop(endpoint), move |engine, index| async move {
            super::iso::stop_stream(&engine, index, endpoint).await
        })
    }

    fn iso_take(
        &mut self,
        endpoint: u8,
        region: &mut [u8],
    ) -> Result<Option<IsoSlotDone>, DriverError> {
        self.host.device()?.iso_take(self.index, endpoint, region)
    }

    fn bulk_in(&mut self, endpoint: u8, data: &mut [u8]) -> Result<Option<usize>, DriverError> {
        // An IN DCI is `2n + 1`: the endpoint must be one of the interface's
        // configured bulk-IN endpoints.
        let pipe = self
            .bulk_pipe_for(endpoint, BulkDirection::In)
            .ok_or(DriverError::OutOfRange)?;
        self.bulk(pipe, data, None)
    }

    fn bulk_out(&mut self, endpoint: u8, data: &[u8]) -> Result<Option<usize>, DriverError> {
        let pipe = self
            .bulk_pipe_for(endpoint, BulkDirection::Out)
            .ok_or(DriverError::OutOfRange)?;
        self.bulk(pipe, &mut [], Some(data))
    }

    fn abandon(&mut self, urb: &UrbRequest) -> Result<(), DriverError> {
        match urb.transfer_type {
            UsbTransferType::Control => {
                let mut core = self.host.device()?;
                let held = core
                    .device_mut(self.index)
                    .and_then(|device| device.urb_control.take());
                if let Some(id) = held {
                    // Only a running transfer has its pipe stopped, and the
                    // stop runs on a later service step, after this note.
                    let running = core.running_control_span(id);
                    core.abandon_control(id);
                    core.orphan_control(id);
                    if let Some((slot, span)) = running {
                        core.note_abandoned(self.index, slot, DCI_CONTROL, span);
                    }
                }
                Ok(())
            }
            // A report poll's endpoint stays armed: its reports keep landing
            // in the device's queue for the next poll to collect.
            UsbTransferType::Interrupt => Ok(()),
            UsbTransferType::Bulk => {
                let direction = match urb.direction {
                    UsbDirection::In => BulkDirection::In,
                    UsbDirection::Out => BulkDirection::Out,
                };
                let pipe = self
                    .bulk_pipe_for(urb.endpoint, direction)
                    .ok_or(DriverError::OutOfRange)?;
                let index = self.index;
                let (serial, abandoned) = {
                    let mut core = self.host.device()?;
                    let device = core.device(index).ok_or(DriverError::NotFound)?;
                    let (slot, dci) = (device.slot, device.bulk_dci(pipe));
                    let span = device.bulk_ring(pipe).and_then(|ring| {
                        let mut trbs = ArrayVec::new();
                        trbs.try_push(ring.head()?).ok()?;
                        TdSpan::on(ring, trbs)
                    });
                    core.begin_bulk_recovery(index, pipe, None)?;
                    (
                        core.device(index).ok_or(DriverError::NotFound)?.serial,
                        span.map(|span| (slot, dci, span)),
                    )
                };
                let engine = self.host.engine().clone();
                let spawned = self.host.spawn(
                    Owner::Device {
                        index,
                        serial,
                        task: Task::Bulk(pipe),
                    },
                    async move {
                        let aborted = abort_bulk(&engine, index, pipe).await;
                        if let Ok(mut core) = engine.lock() {
                            core.bulk_recovered(index, pipe, aborted);
                        }
                    },
                );
                if let Err(err) = spawned {
                    self.host.device()?.bulk_recovered(index, pipe, Err(err));
                    return Err(err);
                }
                // Noted once the stop is sure to run, on a later service step.
                if let (Some((slot, dci, span)), Ok(mut core)) = (abandoned, self.host.device()) {
                    core.note_abandoned(index, slot, dci, span);
                }
                Ok(())
            }
        }
    }

    fn clear_halt(&mut self, endpoint: u8) -> Result<Option<()>, DriverError> {
        let pipe = self
            .host
            .device()?
            .device(self.index)
            .ok_or(DriverError::NotFound)?
            .bulk_pipe_of_dci(address_dci(endpoint))
            .ok_or(DriverError::OutOfRange)?;
        self.request(
            RequestKind::ClearHalt(pipe),
            move |engine, index| async move { clear_bulk_halt(&engine, index, pipe).await },
        )
    }

    fn discard_reports(&mut self, endpoint: u8) -> Result<(), DriverError> {
        if !self.owns_report_endpoint(address_dci(endpoint)) {
            return Err(DriverError::OutOfRange);
        }
        self.host.device()?.discard_reports(self.index)
    }
}
