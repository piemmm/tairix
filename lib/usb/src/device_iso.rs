//! Alternate settings and isochronous streams on the device engine
//! (`plans/SOUND.md` SND6).
//!
//! A node governs its own interface and the streaming siblings it claims.
//! Selecting a setting reserves the bus bandwidth its isochronous endpoints
//! need before the device is told, and is undone if the device refuses it. A
//! stream then places each queued slot's service intervals ahead of the
//! controller at the microframe they are due, and accounts for every interval
//! as moved, missed or failed: a late packet is a gap, never a packet sent
//! late.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use tairix_abi::usb_urb::{IsoLayout, IsoPacket, IsoPacketStatus, UsbDirection, ISO_MAX_PACKETS};
use tairix_abi::{DriverError, RegisterBlock};
use tairix_inline::BitSet256;

use super::{
    bus_speed, ep_ctx_dwords, input_control_dwords, publish, DmaBank, Periodic, SlotHold,
    UsbDevice, CTX_DWORDS, DCI_CONTROL, DMA_CHUNK_ALIGN, EP_TYPE_ISOCH_IN, EP_TYPE_ISOCH_OUT,
    SLOT_CTX_CONTEXT_ENTRIES_MASK, SLOT_CTX_CONTEXT_ENTRIES_SHIFT,
};
use crate::alternate::{alternate_setting, interface_numbers};
use crate::periodic::{EndpointDescriptor, PeriodicBudget, ServiceInterval};
use crate::regs;
use crate::ring::ProducerRing;
use crate::transport::{IsoSlotDone, IsoStreamShape};
use crate::trb::{self, CompletionCode, Trb, TrbType};

/// TRBs in an isochronous transfer ring: one page, so the ring never crosses
/// the 64 KiB boundary no ring segment may (xHCI Table 6-1).
const ISO_RING_TRBS: usize = DMA_CHUNK_ALIGN / trb::TRB_LEN;

/// TRBs one ring holds in flight: all but its link and the slot that tells a
/// full ring from an empty one.
const ISO_RING_CAPACITY: usize = ISO_RING_TRBS - 2;

const _: () = assert!(
    tairix_abi::usb_urb::ISO_MAX_INTERVALS as usize * 2 <= ISO_RING_CAPACITY,
    "every layout the ABI admits fits one ring at two TRBs an interval"
);

/// Microframes past the scheduling threshold a stream starts at, covering the
/// read of `MFINDEX`, the writes of a slot's TDs and the doorbell.
const ISO_LEAD_MICROFRAMES: u64 = 8;

/// How far past `MFINDEX` a TD may be scheduled: 895 frames (xHCI §4.11.2.5).
const ISO_HORIZON_MICROFRAMES: u64 = 895 * MICROFRAMES_PER_FRAME;

/// The boundary no TRB's buffer may cross (xHCI §4.11.7.1).
const TRB_BUFFER_BOUNDARY: u64 = 64 * 1024;

/// Microframes in one frame.
const MICROFRAMES_PER_FRAME: u64 = 8;

/// Frames a Frame ID counts before it wraps.
const FRAME_ID_SPAN: u64 = 2048;

/// Microframes `MFINDEX` counts before it wraps.
const MFINDEX_SPAN: u64 = regs::MFINDEX_MASK as u64 + 1;

/// Microseconds in one microframe.
const MICROSECONDS_PER_MICROFRAME: u64 = 125;

/// `SET_INTERFACE` (USB 2.0 Table 9-4).
const SET_INTERFACE: u8 = 11;

/// `SET_INTERFACE`: select `alternate` on `interface` (USB 2.0 §9.4.10).
const fn setup_set_interface(interface: u8, alternate: u8) -> [u8; 8] {
    [0x01, SET_INTERFACE, alternate, 0, interface, 0, 0, 0]
}

/// The controller's microframe count, extended past `MFINDEX`'s 2.048 s wrap
/// by the monotonic clock: each reading takes the candidate with `MFINDEX`'s
/// low bits nearest the clock's prediction and never behind the last reading.
/// It is exact while two readings are less than the two clocks' drift away
/// from half a wrap apart — hours, at any real oscillator's tolerance.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) struct BusClock {
    microframe: u64,
    at_us: u64,
}

impl BusClock {
    /// The first reading: `MFINDEX` `raw` at `now_us`.
    pub(super) const fn start(raw: u32, now_us: u64) -> Self {
        Self {
            microframe: raw as u64,
            at_us: now_us,
        }
    }

    /// The reading `raw` taken at `now_us`, after this one.
    pub(super) fn advance(self, raw: u32, now_us: u64) -> Self {
        let elapsed = now_us.saturating_sub(self.at_us) / MICROSECONDS_PER_MICROFRAME;
        let predicted = self.microframe.saturating_add(elapsed);
        let epoch = predicted - predicted % MFINDEX_SPAN;
        let raw = u64::from(raw) % MFINDEX_SPAN;
        let above = epoch + MFINDEX_SPAN + raw;
        let microframe = [
            epoch.checked_sub(MFINDEX_SPAN),
            Some(epoch),
            Some(epoch + MFINDEX_SPAN),
        ]
        .into_iter()
        .flatten()
        .map(|base| base + raw)
        .filter(|&candidate| candidate >= self.microframe)
        .min_by_key(|&candidate| candidate.abs_diff(predicted))
        .unwrap_or(above);
        Self {
            microframe,
            at_us: now_us,
        }
    }

    /// The extended microframe.
    pub(super) const fn microframe(self) -> u64 {
        self.microframe
    }
}

/// What a node governs beyond its own interface: the siblings it claimed,
/// and the setting selected on each governed interface that brought
/// endpoints.
#[derive(Default)]
pub(super) struct Streaming {
    claimed: BitSet256,
    selected: Vec<Selected>,
}

impl Streaming {
    /// Whether the node claimed `interface`.
    pub(super) const fn claimed(&self, interface: u8) -> bool {
        self.claimed.contains(interface as u16)
    }

    /// The interfaces the node claimed.
    pub(super) const fn claimed_set(&self) -> BitSet256 {
        self.claimed
    }

    /// Every endpoint the selected settings brought, as a mask of Device
    /// Context Indices.
    pub(super) fn dci_mask(&self) -> u32 {
        self.selected
            .iter()
            .flat_map(|selected| selected.endpoints.iter())
            .fold(0, |mask, endpoint| mask | 1 << endpoint.dci())
    }

    /// The selected endpoint at Device Context Index `dci`, its setting's
    /// position and its own.
    fn locate_dci(&self, dci: u8) -> Option<(usize, usize)> {
        self.selected.iter().enumerate().find_map(|(at, selected)| {
            selected
                .endpoints
                .iter()
                .position(|endpoint| endpoint.dci() == dci)
                .map(|position| (at, position))
        })
    }

    fn endpoint_mut(&mut self, address: u8) -> Option<&mut IsoEndpoint> {
        self.selected
            .iter_mut()
            .flat_map(|selected| selected.endpoints.iter_mut())
            .find(|endpoint| endpoint.descriptor.address == address)
    }

    /// Every chunk the controller was handed for these settings and their
    /// streams.
    pub(super) fn chunks(&self) -> impl Iterator<Item = usize> + '_ {
        self.selected
            .iter()
            .flat_map(|selected| selected.endpoints.iter())
            .flat_map(|endpoint| {
                core::iter::once(endpoint.ring_chunk)
                    .chain(endpoint.stream.as_ref().map(|stream| stream.data_chunk))
            })
    }
}

/// A governed interface's selected setting and the endpoints it brought.
struct Selected {
    interface: u8,
    endpoints: Vec<IsoEndpoint>,
}

/// One isochronous endpoint a selected setting added to the slot.
struct IsoEndpoint {
    descriptor: EndpointDescriptor,
    budget: PeriodicBudget,
    interval: ServiceInterval,
    /// The chunk holding the transfer ring, and the ring's device address.
    ring_chunk: usize,
    ring_base: u64,
    /// `None` once a stop could not reposition the ring: the endpoint then
    /// runs nothing until a setting is selected afresh.
    ring: Option<ProducerRing>,
    stream: Option<Stream>,
}

impl IsoEndpoint {
    const fn dci(&self) -> u8 {
        self.descriptor.dci()
    }

    /// The ring slot `address` names, if it lies in this ring.
    fn ring_slot(&self, address: u64) -> Option<u16> {
        let offset = address.checked_sub(self.ring_base)?;
        let slot = offset / trb::TRB_LEN as u64;
        (offset % trb::TRB_LEN as u64 == 0 && slot < ISO_RING_TRBS as u64)
            .then(|| u16::try_from(slot).ok())
            .flatten()
    }
}

/// One TD on the ring: the slot and interval it serves, the ring slots its
/// TRBs occupy, and the bytes it moves — its first TRB's share, when a
/// 64 KiB boundary splits it in two.
#[derive(Copy, Clone, Debug)]
struct Td {
    slot: u16,
    packet: u16,
    first: u16,
    last: u16,
    length: u32,
    first_length: u32,
}

impl Td {
    /// Whether the TD's TRBs occupy ring slot `at`, following the wrap.
    const fn occupies(&self, at: u16) -> bool {
        if self.first <= self.last {
            self.first <= at && at <= self.last
        } else {
            at >= self.first || at <= self.last
        }
    }

    /// TRBs the TD occupies.
    const fn trbs(&self) -> u16 {
        if self.first == self.last {
            1
        } else {
            2
        }
    }

    /// Bytes an IN TD moved, from the residual its event at ring slot `at`
    /// reported.
    fn received(&self, at: u16, residual: u32) -> u32 {
        if at == self.first {
            self.first_length.saturating_sub(residual)
        } else {
            self.first_length + (self.length - self.first_length).saturating_sub(residual)
        }
    }
}

/// Where a queued slot stands.
#[derive(Copy, Clone, Debug)]
struct SlotState {
    /// Intervals of the slot still on the ring; `None` while the class
    /// driver holds it.
    pending: Option<u16>,
    skipped: u32,
    microframe: u64,
}

/// A running stream: its slots, the TDs on the ring, and where the schedule
/// stands.
struct Stream {
    layout: IsoLayout,
    direction: UsbDirection,
    /// The bytes one interval moves at most.
    packet_max: u32,
    /// The chunk holding every interval's buffer, and the bytes from one
    /// buffer to the next.
    data_chunk: usize,
    stride: usize,
    /// The extended microframe the next queued interval is due in; `None`
    /// before the first slot.
    next: Option<u64>,
    tds: VecDeque<Td>,
    slots: Vec<SlotState>,
    /// Queued slots, oldest first.
    order: VecDeque<u16>,
    outcomes: Vec<IsoPacket>,
    halted: Option<DriverError>,
}

impl Stream {
    /// TRBs one interval's TD may need: a buffer under a page never crosses
    /// a page, and a larger one crosses at most one 64 KiB boundary.
    const fn trbs_per_td(stride: usize) -> usize {
        if stride <= DMA_CHUNK_ALIGN {
            1
        } else {
            2
        }
    }

    /// Bytes from one interval's buffer to the next for intervals of up to
    /// `packet_max` bytes: a power of two under a page, so no buffer
    /// straddles one.
    fn stride_for(packet_max: u32) -> usize {
        let bytes = usize::try_from(packet_max).unwrap_or(usize::MAX).max(1);
        if bytes <= DMA_CHUNK_ALIGN {
            bytes.next_power_of_two().max(64)
        } else {
            bytes.next_multiple_of(64)
        }
    }

    fn outcome_index(&self, slot: u16, packet: u16) -> usize {
        self.staging().interval(slot, packet)
    }

    /// The stream's geometry and buffers, as a slot is laid out in them.
    const fn staging(&self) -> Staging {
        Staging {
            layout: self.layout,
            direction: self.direction,
            packet_max: self.packet_max,
            data_chunk: self.data_chunk,
            stride: self.stride,
        }
    }

    /// Record how TD `td` went and retire its TRBs from `ring`.
    fn finish(&mut self, td: Td, outcome: IsoPacket, ring: &mut ProducerRing) {
        for _ in 0..td.trbs() {
            // The TD is the ring's oldest, so its TRBs are in flight.
            let _ = ring.retire_one();
        }
        let at = self.outcome_index(td.slot, td.packet);
        if let Some(record) = self.outcomes.get_mut(at) {
            *record = outcome;
        }
        if let Some(state) = self.slots.get_mut(usize::from(td.slot)) {
            state.pending = state.pending.map(|left| left.saturating_sub(1));
        }
    }

    /// The outcome of a TD the controller passed without running it.
    const fn missed(td: &Td) -> IsoPacket {
        IsoPacket {
            length: td.length,
            status: IsoPacketStatus::Missed,
        }
    }

    /// Account for one transfer event at ring slot `at` (`None` for an event
    /// naming no TRB of this ring).
    ///
    /// TDs older than the one the event names were passed without an event of
    /// their own — what the controller does after a Missed Service Error — so
    /// they are missed. An event naming no TD still on the ring is a late
    /// second word on a TD already finished, and is dropped.
    fn on_event(
        &mut self,
        at: Option<u16>,
        code: Result<CompletionCode, DriverError>,
        residual: u32,
        ring: &mut ProducerRing,
    ) {
        match code {
            Ok(CompletionCode::RingUnderrun | CompletionCode::RingOverrun) => {
                // The controller found the ring empty, so every TD still
                // recorded is behind it.
                while let Some(td) = self.tds.pop_front() {
                    self.finish(td, Self::missed(&td), ring);
                }
                return;
            }
            Ok(
                CompletionCode::Stopped
                | CompletionCode::StoppedLengthInvalid
                | CompletionCode::StoppedShortPacket,
            ) => return,
            _ => {}
        }
        let Some(at) = at else {
            if code != Ok(CompletionCode::MissedService) {
                self.halted.get_or_insert(DriverError::DeviceFault);
            }
            return;
        };
        let Some(named) = self.tds.iter().position(|td| td.occupies(at)) else {
            return;
        };
        for _ in 0..named {
            if let Some(td) = self.tds.pop_front() {
                self.finish(td, Self::missed(&td), ring);
            }
        }
        let Some(td) = self.tds.pop_front() else {
            return;
        };
        let outcome = match code {
            Ok(CompletionCode::Success | CompletionCode::ShortPacket) => IsoPacket {
                length: match self.direction {
                    UsbDirection::In => td.received(at, residual),
                    UsbDirection::Out => td.length,
                },
                status: IsoPacketStatus::Moved,
            },
            Ok(CompletionCode::MissedService) => Self::missed(&td),
            Ok(
                CompletionCode::UsbTransactionError
                | CompletionCode::SplitTransactionError
                | CompletionCode::BabbleDetected
                | CompletionCode::DataBufferError
                | CompletionCode::IsochBufferOverrun
                | CompletionCode::BandwidthOverrun
                | CompletionCode::StallError,
            ) => IsoPacket {
                length: 0,
                status: IsoPacketStatus::Failed,
            },
            _ => {
                self.halted.get_or_insert(DriverError::DeviceFault);
                IsoPacket {
                    length: 0,
                    status: IsoPacketStatus::Failed,
                }
            }
        };
        self.finish(td, outcome, ring);
    }

    /// Where a slot of `packets` intervals goes when queued at bus
    /// microframe `now`, with the controller's scheduling threshold `ist` and
    /// whether it honours every TD's Frame ID: its first interval's
    /// microframe and the intervals the schedule jumps to reach it.
    ///
    /// A stream that has fallen behind restarts on the first frame — and
    /// service interval — it can still make. A controller that runs a busy
    /// ring's TDs back to back regardless of their Frame IDs cannot be given
    /// a gap, so there the slot follows on and the controller reports what it
    /// misses.
    fn place(&self, interval: u64, now: u64, ist: u64, cfc: bool) -> (u64, u32) {
        let align = interval.max(MICROFRAMES_PER_FRAME);
        let earliest = now + ist + ISO_LEAD_MICROFRAMES;
        let restart = earliest.next_multiple_of(align);
        match self.next {
            None => (restart, 0),
            Some(next) if next >= earliest => (next, 0),
            Some(next) if cfc || self.tds.is_empty() => {
                let skipped = (restart - next) / interval;
                (restart, u32::try_from(skipped).unwrap_or(u32::MAX))
            }
            Some(next) => (next, 0),
        }
    }
}

/// What putting a refused setting back needs: whose it was, where its
/// record sits, and the slot's endpoints either side of it.
#[derive(Copy, Clone)]
struct Undo {
    index: usize,
    interface: u8,
    current: Option<usize>,
    slot: u8,
    output_ctx: usize,
    added_mask: u32,
    others: u32,
}

/// Where a slot being queued goes: the stream's geometry and buffers.
#[derive(Copy, Clone)]
struct Staging {
    layout: IsoLayout,
    direction: UsbDirection,
    packet_max: u32,
    data_chunk: usize,
    stride: usize,
}

impl Staging {
    /// The position of `slot`'s interval `packet` among all the stream's
    /// intervals.
    const fn interval(&self, slot: u16, packet: u16) -> usize {
        slot as usize * self.layout.packets as usize + packet as usize
    }

    /// The bank offset of `slot`'s interval `packet`'s buffer.
    const fn buffer(&self, slot: u16, packet: u16) -> usize {
        self.data_chunk + self.interval(slot, packet) * self.stride
    }
}

/// One interval's TD as queued: where its bytes are and what it carries.
struct Planned {
    packet: u16,
    buffer: u64,
    length: u32,
    microframe: u64,
}

impl<H: RegisterBlock, M: DmaBank> UsbDevice<'_, H, M> {
    /// The controller's extended microframe now.
    fn bus_microframe(&mut self) -> Result<u64, DriverError> {
        let raw = self.xhci.microframe_index()?;
        let now_us = self.wait.now_us();
        let clock = match self.bus_clock {
            Some(clock) => clock.advance(raw, now_us),
            None => BusClock::start(raw, now_us),
        };
        self.bus_clock = Some(clock);
        Ok(clock.microframe())
    }

    /// Every endpoint live on `slot` across the entries serving it, as a mask
    /// of Device Context Indices.
    fn slot_endpoint_mask(&self, slot: u8) -> u32 {
        self.devices
            .iter()
            .flatten()
            .filter(|device| device.slot == slot)
            .fold(1 << DCI_CONTROL, |mask, device| {
                mask | device.pipe_dci_mask() | device.streaming.dci_mask()
            })
    }

    /// Govern `interface` of the device served at `index`, which no node of
    /// its own serves.
    pub(super) fn claim_interface(
        &mut self,
        index: usize,
        interface: u8,
    ) -> Result<(), DriverError> {
        let device = self.device(index).ok_or(DriverError::NotFound)?;
        if !interface_numbers(&device.config)?.contains(u16::from(interface)) {
            return Err(DriverError::NotFound);
        }
        if device.governs(interface) {
            return Ok(());
        }
        let slot = device.slot;
        let taken = self
            .devices
            .iter()
            .flatten()
            .filter(|other| other.slot == slot)
            .any(|other| other.governs(interface));
        if taken {
            return Err(DriverError::AlreadyExists);
        }
        let device = self.device_mut(index).ok_or(DriverError::NotFound)?;
        device.streaming.claimed.insert(u16::from(interface));
        Ok(())
    }

    /// Select `alternate` on governed `interface` of the device served at
    /// `index`: reserve the setting's isochronous endpoints with the
    /// controller, then tell the device, putting the old setting back if it
    /// refuses.
    pub(super) fn set_interface(
        &mut self,
        index: usize,
        interface: u8,
        alternate: u8,
    ) -> Result<(), DriverError> {
        let device = self.device(index).ok_or(DriverError::NotFound)?;
        if !device.governs(interface) {
            return Err(DriverError::NotFound);
        }
        if interface == device.identity.interface_number && device.pipe_dci_mask() != 0 {
            // The engine's own pipes ride the interface's default setting.
            return Err(DriverError::Unsupported);
        }
        let speed = bus_speed(device.speed);
        let setting = alternate_setting(&device.config, interface, alternate)?;
        let mut wanted = Vec::new();
        wanted
            .try_reserve_exact(setting.endpoints().count())
            .map_err(|_| DriverError::OutOfMemory)?;
        for descriptor in setting.endpoints() {
            if !descriptor.is_isochronous() {
                return Err(DriverError::Unsupported);
            }
            wanted.push((
                *descriptor,
                PeriodicBudget::isochronous(descriptor, speed)?,
                ServiceInterval::isochronous(speed, descriptor.interval)?,
            ));
        }
        let current = device
            .streaming
            .selected
            .iter()
            .position(|selected| selected.interface == interface);
        let old_mask = current.map_or(0, |at| {
            device.streaming.selected[at]
                .endpoints
                .iter()
                .fold(0, |mask, endpoint| mask | 1 << endpoint.dci())
        });
        if current.is_some_and(|at| {
            device.streaming.selected[at]
                .endpoints
                .iter()
                .any(|endpoint| endpoint.stream.is_some())
        }) {
            return Err(DriverError::Busy);
        }
        let new_mask = setting.dci_mask();
        let (slot, output_ctx) = (device.slot, device.output_ctx);
        let others = self.slot_endpoint_mask(slot) & !old_mask;
        if new_mask & others != 0 {
            return Err(DriverError::Busy);
        }

        let mut added = Vec::new();
        added
            .try_reserve_exact(wanted.len())
            .map_err(|_| DriverError::OutOfMemory)?;
        for (descriptor, budget, interval) in wanted {
            match self.build_iso_endpoint(descriptor, budget, interval) {
                Ok(endpoint) => added.push(endpoint),
                Err(err) => {
                    self.release_iso_endpoints(&added, SlotHold::Released);
                    return Err(err);
                }
            }
        }
        let reserved = self.configure_iso_endpoints(slot, output_ctx, old_mask, &added, others);
        if let Err((err, hold)) = reserved {
            self.release_iso_endpoints(&added, hold);
            return Err(err);
        }

        match self.device_control(index, setup_set_interface(interface, alternate), &mut []) {
            Ok(_) => {}
            // A device with no setting but the default may refuse to be told
            // to keep it (USB 2.0 §9.4.10).
            Err(DriverError::EndpointStalled)
                if alternate == 0
                    && self.device(index).is_some_and(|device| {
                        alternate_setting(&device.config, interface, 1)
                            == Err(DriverError::NotFound)
                    }) => {}
            Err(err) => {
                let undo = Undo {
                    index,
                    interface,
                    current,
                    slot,
                    output_ctx,
                    added_mask: new_mask,
                    others,
                };
                self.revert_setting(undo, added);
                return Err(err);
            }
        }
        self.commit_setting(index, interface, current, added);
        Ok(())
    }

    /// Lay out a fresh transfer ring for one endpoint a setting brings.
    fn build_iso_endpoint(
        &mut self,
        descriptor: EndpointDescriptor,
        budget: PeriodicBudget,
        interval: ServiceInterval,
    ) -> Result<IsoEndpoint, DriverError> {
        let ring_chunk = self.dma.grow(DMA_CHUNK_ALIGN)?;
        let built = self
            .build_ring(ring_chunk, ISO_RING_TRBS)
            .and_then(|ring| Ok((ring, self.device_addr_of(ring_chunk)?)));
        match built {
            Ok((ring, ring_base)) => Ok(IsoEndpoint {
                descriptor,
                budget,
                interval,
                ring_chunk,
                ring_base,
                ring: Some(ring),
                stream: None,
            }),
            Err(err) => {
                let _ = self.dma.release(ring_chunk);
                Err(err)
            }
        }
    }

    /// Give back the chunks of endpoints the controller holds as `hold`
    /// says.
    fn release_iso_endpoints(&mut self, endpoints: &[IsoEndpoint], hold: SlotHold) {
        for endpoint in endpoints {
            self.retire_slot_chunk(endpoint.ring_chunk, hold);
            if let Some(stream) = endpoint.stream.as_ref() {
                self.retire_slot_chunk(stream.data_chunk, hold);
            }
        }
    }

    /// Configure Endpoint dropping `drop` and adding `endpoints`, the slot
    /// keeping `kept`.
    ///
    /// # Errors
    ///
    /// The refusal, and whether the controller may still reach the added
    /// endpoints' rings: a command that went unanswered may have taken them.
    fn configure_iso_endpoints(
        &mut self,
        slot: u8,
        output_ctx: usize,
        drop: u32,
        endpoints: &[IsoEndpoint],
        kept: u32,
    ) -> Result<(), (DriverError, SlotHold)> {
        let input = self
            .stage_iso_endpoints(output_ctx, drop, endpoints, kept)
            .map_err(|err| (err, SlotHold::Released))?;
        let event = self
            .issue_command(Trb::new(
                TrbType::ConfigureEndpoint,
                input,
                0,
                trb::control_slot(slot),
            ))
            .map_err(|err| (err, SlotHold::Held))?;
        match event.completion_code() {
            Ok(CompletionCode::Success) => Ok(()),
            Ok(CompletionCode::BandwidthError | CompletionCode::SecondaryBandwidthError) => {
                Err((DriverError::NoBandwidth, SlotHold::Released))
            }
            _ => Err((DriverError::DeviceFault, SlotHold::Released)),
        }
    }

    /// Write the input context of a Configure Endpoint dropping `drop` and
    /// adding `endpoints`, the slot keeping `kept`, returning its address.
    /// Context Entries covers the highest endpoint left live.
    fn stage_iso_endpoints(
        &mut self,
        output_ctx: usize,
        drop: u32,
        endpoints: &[IsoEndpoint],
        kept: u32,
    ) -> Result<u64, DriverError> {
        let add = endpoints
            .iter()
            .fold(0, |mask, endpoint| mask | 1 << endpoint.dci());
        let live = kept | add;
        let entries = u32::BITS - 1 - live.leading_zeros();
        let mut slot_ctx = self.read_ctx(output_ctx)?;
        slot_ctx[0] = (slot_ctx[0] & !SLOT_CTX_CONTEXT_ENTRIES_MASK)
            | (entries << SLOT_CTX_CONTEXT_ENTRIES_SHIFT);
        self.write_input_ctx(0, &input_control_dwords(drop, 1 | add))?;
        self.write_input_ctx(1, &slot_ctx)?;
        for endpoint in endpoints {
            self.write_input_ctx(1 + usize::from(endpoint.dci()), &iso_ep_ctx(endpoint))?;
        }
        self.device_addr_of(self.layout.input_ctx)
    }

    /// Put `interface`'s setting at `current` back after the device refused
    /// the one whose endpoints, `added`, the controller already took.
    ///
    /// If the controller will not take the old setting back it keeps the new
    /// endpoints, and so does the record of them, which a later selection
    /// replaces; a command that went unanswered keeps both sets' memory.
    fn revert_setting(&mut self, undo: Undo, added: Vec<IsoEndpoint>) {
        let Undo {
            index,
            interface,
            current,
            slot,
            output_ctx,
            added_mask,
            others,
        } = undo;
        let mut previous = current
            .and_then(|at| {
                self.device_mut(index)
                    .map(|device| core::mem::take(&mut device.streaming.selected[at].endpoints))
            })
            .unwrap_or_default();
        let rebuilt = previous.iter_mut().try_for_each(|endpoint| {
            endpoint.ring = Some(self.build_ring(endpoint.ring_chunk, ISO_RING_TRBS)?);
            Ok(())
        });
        let restored = match rebuilt {
            Ok(()) => self.configure_iso_endpoints(slot, output_ctx, added_mask, &previous, others),
            Err(err) => Err((err, SlotHold::Released)),
        };
        let (keep, give_back, hold) = match restored {
            Ok(()) => (previous, added, SlotHold::Released),
            Err((_, hold)) => (added, previous, hold),
        };
        self.release_iso_endpoints(&give_back, hold);
        self.record_setting(index, interface, current, keep);
    }

    /// Record `endpoints` as `interface`'s setting at `current`, or as a new
    /// one; with no room for the record the controller still holds them, so
    /// their memory is kept until a reset.
    fn record_setting(
        &mut self,
        index: usize,
        interface: u8,
        current: Option<usize>,
        endpoints: Vec<IsoEndpoint>,
    ) {
        let Some(device) = self.device_mut(index) else {
            self.release_iso_endpoints(&endpoints, SlotHold::Held);
            return;
        };
        match current {
            Some(at) => device.streaming.selected[at].endpoints = endpoints,
            None if endpoints.is_empty() => {}
            None if device.streaming.selected.try_reserve(1).is_ok() => {
                device.streaming.selected.push(Selected {
                    interface,
                    endpoints,
                });
            }
            None => self.release_iso_endpoints(&endpoints, SlotHold::Held),
        }
    }

    /// Record `added` as `interface`'s selected setting, giving back the
    /// rings of the setting the controller dropped.
    fn commit_setting(
        &mut self,
        index: usize,
        interface: u8,
        current: Option<usize>,
        added: Vec<IsoEndpoint>,
    ) {
        let dropped = match (current, self.device_mut(index)) {
            (Some(at), Some(device)) if added.is_empty() => {
                device.streaming.selected.swap_remove(at).endpoints
            }
            (Some(at), Some(device)) => {
                core::mem::take(&mut device.streaming.selected[at].endpoints)
            }
            _ => Vec::new(),
        };
        self.release_iso_endpoints(&dropped, SlotHold::Released);
        let current = current.filter(|_| !added.is_empty());
        self.record_setting(index, interface, current, added);
    }

    /// Start a stream of `layout` on endpoint `address` of the device served
    /// at `index`, replacing any stream already there.
    pub(super) fn iso_start(
        &mut self,
        index: usize,
        address: u8,
        layout: IsoLayout,
    ) -> Result<IsoStreamShape, DriverError> {
        let ist = u64::from(self.xhci.ist_microframes());
        let device = self.device_mut(index).ok_or(DriverError::NotFound)?;
        let speed = bus_speed(device.speed);
        let endpoint = device
            .streaming
            .endpoint_mut(address)
            .ok_or(DriverError::NotFound)?;
        let direction = UsbDirection::of_address(address);
        let esit = endpoint.budget.max_esit_payload;
        if direction == UsbDirection::In && layout.packet_bytes < esit {
            // A device sending its whole interval budget would babble.
            return Err(DriverError::OutOfRange);
        }
        let packet_max = layout.packet_bytes.min(esit);
        let stride = Stream::stride_for(packet_max);
        let intervals = usize::from(layout.slots) * usize::from(layout.packets);
        let interval_microframes = endpoint.interval.microframes();
        let interval = u64::from(interval_microframes);
        let span = u64::try_from(intervals)
            .unwrap_or(u64::MAX)
            .saturating_mul(interval);
        let lead = ist + ISO_LEAD_MICROFRAMES + interval.max(MICROFRAMES_PER_FRAME);
        if intervals * Stream::trbs_per_td(stride) > ISO_RING_CAPACITY
            || span.saturating_add(lead) > ISO_HORIZON_MICROFRAMES
        {
            return Err(DriverError::OutOfRange);
        }
        if endpoint.ring.is_none() {
            return Err(DriverError::DeviceFault);
        }
        if endpoint.stream.is_some() {
            self.iso_stop(index, address)?;
        }
        let data_len = intervals
            .checked_mul(stride)
            .ok_or(DriverError::LengthOutOfRange)?;
        let data_chunk = self.dma.grow(data_len)?;
        let stream = match new_stream(layout, direction, packet_max, data_chunk, stride) {
            Ok(stream) => stream,
            Err(err) => {
                let _ = self.dma.release(data_chunk);
                return Err(err);
            }
        };
        let endpoint = self
            .device_mut(index)
            .and_then(|device| device.streaming.endpoint_mut(address));
        let Some(endpoint) = endpoint else {
            let _ = self.dma.release(data_chunk);
            return Err(DriverError::NotFound);
        };
        endpoint.stream = Some(stream);
        Ok(IsoStreamShape {
            interval_microframes,
            speed,
        })
    }

    /// Hand slot `slot` of the stream on `address` to the controller, an OUT
    /// slot's records and data read out of `region`.
    pub(super) fn iso_queue(
        &mut self,
        index: usize,
        address: u8,
        slot: u16,
        region: &[u8],
    ) -> Result<(), DriverError> {
        // Completions already posted decide whether the ring is empty, and so
        // where the slot can go.
        self.drain_events()?;
        let ist = u64::from(self.xhci.ist_microframes());
        let cfc = self.xhci.contiguous_frame_ids();
        let now = self.bus_microframe()?;
        let device = self.device(index).ok_or(DriverError::NotFound)?;
        let device_slot = device.slot;
        let endpoint = device
            .streaming
            .selected
            .iter()
            .flat_map(|selected| selected.endpoints.iter())
            .find(|endpoint| endpoint.descriptor.address == address)
            .ok_or(DriverError::NotFound)?;
        let stream = endpoint.stream.as_ref().ok_or(DriverError::NotFound)?;
        let layout = stream.layout;
        if slot >= layout.slots {
            return Err(DriverError::OutOfRange);
        }
        if stream.slots[usize::from(slot)].pending.is_some() {
            return Err(DriverError::Busy);
        }
        if region.len() < layout.region_len() {
            return Err(DriverError::OutOfRange);
        }
        let ring = endpoint.ring.as_ref().ok_or(DriverError::DeviceFault)?;
        let trbs = usize::from(layout.packets) * Stream::trbs_per_td(stream.stride);
        if ISO_RING_CAPACITY - ring.in_flight() < trbs {
            return Err(DriverError::Busy);
        }
        let interval = u64::from(endpoint.interval.microframes());
        let (start, skipped) = stream.place(interval, now, ist, cfc);
        let end = start + u64::from(layout.packets) * interval;
        if end > now + ISO_HORIZON_MICROFRAMES {
            return Err(DriverError::OutOfRange);
        }
        let staging = stream.staging();
        let direction = stream.direction;
        let budget = endpoint.budget;
        let (ring_chunk, dci) = (endpoint.ring_chunk, endpoint.dci());
        let lengths = self.stage_slot(staging, slot, region)?;

        let Self { devices, dma, .. } = self;
        let endpoint = devices
            .get_mut(index)
            .and_then(Option::as_mut)
            .and_then(|device| device.streaming.endpoint_mut(address))
            .ok_or(DriverError::NotFound)?;
        let (Some(ring), Some(stream)) = (endpoint.ring.as_mut(), endpoint.stream.as_mut()) else {
            return Err(DriverError::NotFound);
        };
        for packet in 0..layout.packets {
            let td = Planned {
                packet,
                buffer: dma.device_addr_of(staging.buffer(slot, packet))?,
                length: lengths[usize::from(packet)],
                microframe: start + u64::from(packet) * interval,
            };
            let last = packet + 1 == layout.packets;
            let queued = push_td(dma, ring, ring_chunk, budget, direction, &td, last)?;
            stream.tds.push_back(Td {
                slot,
                packet,
                ..queued
            });
        }
        stream.slots[usize::from(slot)] = SlotState {
            pending: Some(layout.packets),
            skipped,
            microframe: start,
        };
        stream.order.push_back(slot);
        stream.next = Some(end);
        self.xhci.ring_doorbell(device_slot, u32::from(dci))
    }

    /// Lay slot `slot`'s intervals out in the stream's buffers, answering
    /// each interval's length: an OUT slot's records checked and its data
    /// copied in from `region` — each record read once, so the class driver
    /// rewriting it meanwhile changes nothing queued — and an IN slot's
    /// intervals each sized to the endpoint's whole budget.
    fn stage_slot(
        &mut self,
        staging: Staging,
        slot: u16,
        region: &[u8],
    ) -> Result<[u32; ISO_MAX_PACKETS as usize], DriverError> {
        let Staging {
            layout,
            direction,
            packet_max,
            ..
        } = staging;
        let mut lengths = [packet_max; ISO_MAX_PACKETS as usize];
        if direction == UsbDirection::In {
            return Ok(lengths);
        }
        for packet in 0..layout.packets {
            let record = layout
                .record(region, slot, packet)
                .map_err(|_| DriverError::OutOfRange)?;
            if record.length > packet_max || record.status != IsoPacketStatus::Moved {
                return Err(DriverError::OutOfRange);
            }
            lengths[usize::from(packet)] = record.length;
            if record.length != 0 {
                let data = layout
                    .data(region, slot, packet)
                    .map_err(|_| DriverError::OutOfRange)?;
                let bytes = data
                    .get(..usize::try_from(record.length).map_err(|_| DriverError::OutOfRange)?)
                    .ok_or(DriverError::OutOfRange)?;
                self.dma.write(staging.buffer(slot, packet), bytes)?;
            }
        }
        Ok(lengths)
    }

    /// Take the oldest finished slot of the stream on `address`, writing its
    /// records — and an IN slot's data — into `region`.
    pub(super) fn iso_take(
        &mut self,
        index: usize,
        address: u8,
        region: &mut [u8],
    ) -> Result<Option<IsoSlotDone>, DriverError> {
        let Self { devices, dma, .. } = self;
        let stream = devices
            .get_mut(index)
            .and_then(Option::as_mut)
            .and_then(|device| device.streaming.endpoint_mut(address))
            .and_then(|endpoint| endpoint.stream.as_mut())
            .ok_or(DriverError::NotFound)?;
        if let Some(err) = stream.halted {
            return Err(err);
        }
        let Some(&slot) = stream.order.front() else {
            return Ok(None);
        };
        let state = stream.slots[usize::from(slot)];
        if state.pending != Some(0) {
            return Ok(None);
        }
        let layout = stream.layout;
        if region.len() < layout.region_len() {
            return Err(DriverError::OutOfRange);
        }
        for packet in 0..layout.packets {
            let outcome = stream.outcomes[stream.outcome_index(slot, packet)];
            if stream.direction == UsbDirection::In
                && outcome.status == IsoPacketStatus::Moved
                && outcome.length != 0
            {
                let at = stream.staging().buffer(slot, packet);
                let length =
                    usize::try_from(outcome.length).map_err(|_| DriverError::OutOfRange)?;
                let data = layout
                    .data_mut(region, slot, packet)
                    .map_err(|_| DriverError::OutOfRange)?;
                dma.read(at, data.get_mut(..length).ok_or(DriverError::OutOfRange)?)?;
            }
            layout
                .set_record(region, slot, packet, outcome)
                .map_err(|_| DriverError::OutOfRange)?;
        }
        stream.order.pop_front();
        stream.slots[usize::from(slot)].pending = None;
        Ok(Some(IsoSlotDone {
            slot,
            skipped: state.skipped,
            microframe: state.microframe,
        }))
    }

    /// Stop the stream on `address`, discarding what it still had queued.
    ///
    /// Its buffers go back once the controller has stopped the endpoint; one
    /// that does not answer keeps them. The ring is repositioned for the
    /// next stream, or left unusable until a setting is selected afresh if
    /// the controller will not take the new dequeue.
    pub(super) fn iso_stop(&mut self, index: usize, address: u8) -> Result<(), DriverError> {
        let device = self.device_mut(index).ok_or(DriverError::NotFound)?;
        let device_slot = device.slot;
        let endpoint = device
            .streaming
            .endpoint_mut(address)
            .ok_or(DriverError::NotFound)?;
        let stream = endpoint.stream.take().ok_or(DriverError::NotFound)?;
        endpoint.ring = None;
        let (dci, ring_chunk, ring_base) =
            (endpoint.dci(), endpoint.ring_chunk, endpoint.ring_base);
        let stopped = self.stop_iso_endpoint(device_slot, dci);
        if stopped.is_err() {
            self.retire_slot_chunk(stream.data_chunk, SlotHold::Held);
            return Err(DriverError::DeviceFault);
        }
        let _ = self.dma.release(stream.data_chunk);
        let ring = self.build_ring(ring_chunk, ISO_RING_TRBS)?;
        self.command(Trb::new(
            TrbType::SetTrDequeuePointer,
            ring_base | 1,
            0,
            trb::control_slot(device_slot) | trb::control_endpoint(dci),
        ))?;
        if let Some(endpoint) = self
            .device_mut(index)
            .and_then(|device| device.streaming.endpoint_mut(address))
        {
            endpoint.ring = Some(ring);
        }
        Ok(())
    }

    /// Bring endpoint `dci` of `slot` to Stopped: a running endpoint is
    /// stopped, a halted one reset, and one already stopped left so.
    fn stop_iso_endpoint(&mut self, slot: u8, dci: u8) -> Result<(), DriverError> {
        for command in [TrbType::StopEndpoint, TrbType::ResetEndpoint] {
            let event = self.issue_command(Trb::new(
                command,
                0,
                0,
                trb::control_slot(slot) | trb::control_endpoint(dci),
            ))?;
            match event.completion_code() {
                Ok(CompletionCode::Success) => return Ok(()),
                Ok(CompletionCode::ContextStateError) => {}
                _ => return Err(DriverError::DeviceFault),
            }
        }
        Ok(())
    }

    /// The entry and endpoint a transfer event on a selected isochronous
    /// endpoint belongs to.
    pub(super) fn iso_async_index(&self, event: Trb) -> Option<(usize, usize, usize)> {
        self.devices.iter().enumerate().find_map(|(index, entry)| {
            let device = entry
                .as_ref()
                .filter(|device| device.slot == event.slot_id())?;
            let (selected, position) = device.streaming.locate_dci(event.endpoint_id())?;
            Some((index, selected, position))
        })
    }

    /// Account for a transfer event on a selected isochronous endpoint: a
    /// stream's TD finished, missed or failed. An event for an endpoint with
    /// no stream belongs to one already stopped.
    pub(super) fn capture_iso_event(&mut self, at: (usize, usize, usize), event: Trb) {
        let (index, selected, position) = at;
        let Some(endpoint) = self
            .device_mut(index)
            .and_then(|device| device.streaming.selected.get_mut(selected))
            .and_then(|selected| selected.endpoints.get_mut(position))
        else {
            return;
        };
        let ring_slot = endpoint.ring_slot(event.parameter);
        let (Some(ring), Some(stream)) = (endpoint.ring.as_mut(), endpoint.stream.as_mut()) else {
            return;
        };
        stream.on_event(
            ring_slot,
            event.completion_code(),
            event.transfer_residual(),
            ring,
        );
    }
}

/// A stream with every slot free.
fn new_stream(
    layout: IsoLayout,
    direction: UsbDirection,
    packet_max: u32,
    data_chunk: usize,
    stride: usize,
) -> Result<Stream, DriverError> {
    let intervals = usize::from(layout.slots) * usize::from(layout.packets);
    let mut tds = VecDeque::new();
    tds.try_reserve_exact(intervals)
        .map_err(|_| DriverError::OutOfMemory)?;
    let mut slots = Vec::new();
    slots
        .try_reserve_exact(usize::from(layout.slots))
        .map_err(|_| DriverError::OutOfMemory)?;
    slots.resize(
        usize::from(layout.slots),
        SlotState {
            pending: None,
            skipped: 0,
            microframe: 0,
        },
    );
    let mut order = VecDeque::new();
    order
        .try_reserve_exact(usize::from(layout.slots))
        .map_err(|_| DriverError::OutOfMemory)?;
    let mut outcomes = Vec::new();
    outcomes
        .try_reserve_exact(intervals)
        .map_err(|_| DriverError::OutOfMemory)?;
    outcomes.resize(
        intervals,
        IsoPacket {
            length: 0,
            status: IsoPacketStatus::Missed,
        },
    );
    Ok(Stream {
        layout,
        direction,
        packet_max,
        data_chunk,
        stride,
        next: None,
        tds,
        slots,
        order,
        outcomes,
        halted: None,
    })
}

/// The endpoint context of a selected isochronous endpoint (xHCI §6.2.3).
fn iso_ep_ctx(endpoint: &IsoEndpoint) -> [u32; CTX_DWORDS] {
    let ep_type = if endpoint.descriptor.is_in() {
        EP_TYPE_ISOCH_IN
    } else {
        EP_TYPE_ISOCH_OUT
    };
    ep_ctx_dwords(
        ep_type,
        u32::from(endpoint.budget.max_packet),
        u32::from(endpoint.budget.max_burst),
        endpoint.ring_base,
        Some(Periodic {
            interval: u32::from(endpoint.interval.exponent()),
            payload: endpoint.budget.max_esit_payload,
            mult: u32::from(endpoint.budget.mult),
        }),
    )
}

/// Write one interval's TD onto `ring`: an Isoch TRB placed at its frame,
/// and a Normal TRB for the rest when its buffer crosses a 64 KiB boundary.
/// Every TD interrupts on completion, so a missed one is accounted for
/// exactly; all but the slot's `last` block the interrupt, so a slot raises
/// one.
fn push_td<M: DmaBank>(
    dma: &mut M,
    ring: &mut ProducerRing,
    ring_chunk: usize,
    budget: PeriodicBudget,
    direction: UsbDirection,
    td: &Planned,
    last: bool,
) -> Result<Td, DriverError> {
    let to_boundary = TRB_BUFFER_BOUNDARY - td.buffer % TRB_BUFFER_BOUNDARY;
    let first_length = u32::try_from(to_boundary.min(u64::from(td.length))).unwrap_or(td.length);
    let split = first_length < td.length;
    let max_packet = u32::from(budget.max_packet.max(1));
    let packets = td.length.div_ceil(max_packet).max(1);
    let (tbc, tlbpc) = budget.burst_counts(td.length);
    let frame_id = u16::try_from((td.microframe / MICROFRAMES_PER_FRAME) % FRAME_ID_SPAN)
        .map_err(|_| DriverError::OutOfRange)?;
    let isp = if direction == UsbDirection::In {
        trb::CONTROL_ISP
    } else {
        0
    };
    let complete = trb::CONTROL_IOC | if last { 0 } else { trb::CONTROL_BEI };
    let (head_flags, head_size) = if split {
        (
            trb::CONTROL_CHAIN,
            packets.saturating_sub(first_length / max_packet),
        )
    } else {
        (complete, 0)
    };
    let head = Trb::new(
        TrbType::Isoch,
        td.buffer,
        trb::transfer_status(first_length, head_size),
        isp | head_flags | trb::isoch_fields(tbc, tlbpc, frame_id),
    );
    let first = ring.enqueue_slot();
    let outcome = ring.push(head)?;
    publish(dma, ring_chunk, ring.link_slot(), &outcome)?;
    let mut last_slot = first;
    if split {
        let tail = Trb::new(
            TrbType::Normal,
            td.buffer + u64::from(first_length),
            trb::transfer_status(td.length - first_length, 0),
            isp | complete,
        );
        last_slot = ring.enqueue_slot();
        let outcome = ring.push(tail)?;
        publish(dma, ring_chunk, ring.link_slot(), &outcome)?;
    }
    Ok(Td {
        slot: 0,
        packet: td.packet,
        first: u16::try_from(first).map_err(|_| DriverError::OutOfRange)?,
        last: u16::try_from(last_slot).map_err(|_| DriverError::OutOfRange)?,
        length: td.length,
        first_length,
    })
}

#[cfg(test)]
#[path = "device_iso_tests.rs"]
mod tests;
