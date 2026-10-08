//! virtio-iommu: the paravirtual translation unit a hypervisor offers its
//! guests, driven over its request queue, its faults read from its event
//! queue.
//!
//! The unit keeps each domain's translations itself: every attach, map and
//! unmap is a request the hypervisor answers once it has applied it, so an
//! answered unmap is a confirmed one. The family keeps a shadow of every
//! domain — the same radix tree every table-walking family builds, walked by
//! no unit — because a device may forget a domain whose last endpoint
//! detaches; a domain attached again has its mappings replayed into it.
//!
//! Endpoints the device knows but no domain holds are blocked: bypass is
//! turned off as the unit is taken over, through `bypass` where the device
//! lets it be written, and by never accepting the feature that would let
//! unattached endpoints bypass.
//!
//! Reference: Virtual I/O Device (VIRTIO) Version 1.3, §5.13. The design and
//! its staging are `plans/IOMMU.md` IOM17.

#![no_std]
#![deny(missing_docs)]
#![forbid(unsafe_code)]

extern crate alloc;

mod format;

#[cfg(test)]
mod model;
#[cfg(test)]
mod tests;

use alloc::vec::Vec;
use core::ops::Range;

use tairix_arch_api::PageTableFrames;
use tairix_collections::HashMap;
use tairix_hash::BuildFastHash;
use tairix_inline::ArrayVec;
use tairix_kernel_iommu_api::{
    drain_in_batches, reach_bits, wait_for, Access, Binding, Bindings, Clock, DomainId, DomainMap,
    Fault, FaultBatch, FaultReason, FaultRoute, FrameRings, Ids, IoPageTable, IommuError,
    IommuUnit, Pte, PteFormat, Reach, Signalling, TableMemory, Tables, UnitFunction, UnitProfile,
    FAULT_QUEUE_RECORDS, IO_PAGE_SHIFT, IO_PAGE_SIZE, MAX_LEVELS,
};
use tairix_sync::SpinLock;
use tairix_virtio::{
    ChainSegment, Direction, DmaHost, DmaSlab, SplitQueue, Status as DeviceStatus, Transport,
    VirtioError, TRANSPORT_FEATURES, VIRTIO_F_VERSION_1,
};

use crate::format::{config, feature, Report, Request, Status};

/// The match key a virtio-iommu that is a PCI function carries, from a
/// device tree or the ACPI VIOT, and the kernel binds this family to. One in
/// a virtio-mmio slot carries the slot's,
/// [`tairix_virtio::transport_mmio::COMPATIBLE`].
pub const COMPATIBLE: &[u8] = b"virtio,pci-iommu";

/// The virtio device id of an IOMMU.
pub const DEVICE_ID: u32 = 23;

/// The place among a unit's interrupts of the line it raises its faults on:
/// its only one, a slot's line or a function's INTx pin.
pub const FAULT_INTERRUPT: u32 = 0;

/// Bytes of register window a virtio-mmio virtio-iommu needs: the
/// transport's registers and the device's configuration after them.
pub const MMIO_WINDOW: u64 = (tairix_virtio::transport_mmio::regs::CONFIG + config::LEN) as u64;

/// The MSI-X table entry a virtio-iommu that is a PCI function raises its
/// event queue's interrupt through.
pub const MSIX_ENTRY: u16 = 0;

/// Descriptors the request queue is made to hold. One request is in flight
/// at a time, its two descriptors.
const REQUEST_DEPTH: u16 = 2;

/// Fault reports the event queue is made to hold: as many as any family's
/// unit holds, where the device's queue is that deep.
const REPORT_DEPTH: u16 = 1 << FAULT_QUEUE_RECORDS.trailing_zeros();

const _: () = assert!(
    REPORT_DEPTH as u32 == FAULT_QUEUE_RECORDS,
    "the report queue's depth is a virtqueue size"
);

/// Where in the request slab a request's tail sits, clear of the longest
/// request.
const TAIL_AT: usize = 48;

/// Bytes of the request slab: one request and its tail.
const REQUEST_SLAB: usize = TAIL_AT + format::TAIL_LEN;

const _: () = assert!(format::REQUEST_MAX <= TAIL_AT);

/// One virtio-iommu.
///
/// Its queues and buffers are never freed: nothing proves the device
/// stopped reading them. Its state is split so a domain the device does not
/// hold maps, and a sync answers, while a request is in flight: the event
/// queue, then the lifecycle lock, then the domain map and a domain, then the
/// request channel, never the reverse.
pub struct VirtioIommuUnit<'f, T: Transport> {
    memory: TableMemory<'f>,
    clock: &'f dyn Clock,
    /// The unit's own PCI function, by node address, where it is one.
    function: Option<(&'f dyn UnitFunction, u32)>,
    profile: UnitProfile,
    /// Levels of a domain's shadow, enough for the input range.
    levels: u32,
    /// The IOVAs the device translates, `first..=last`.
    input: (u64, u64),
    /// What the input range leaves out of every domain's reach.
    outside: ArrayVec<Range<u64>, 2>,
    /// The bytes of properties a probe fills.
    probe_size: usize,
    /// How the device raises its interrupt, which the transport was built
    /// for.
    signalling: Signalling,
    channel: SpinLock<Channel<T>>,
    events: SpinLock<Events>,
    lifecycle: SpinLock<Lifecycle>,
    domains: DomainMap<DomainState<'f>>,
}

/// The request queue and the transport it is kicked through: one request is
/// in flight at a time, as the device answers them.
struct Channel<T> {
    transport: T,
    requests: SplitQueue,
    /// One request and its tail.
    request: DmaSlab,
    /// One probe request, its properties and its tail.
    probe: DmaSlab,
    /// The chain of a request the device left unanswered: the buffer it was
    /// given may still be written, so nothing more is sent and nothing it
    /// held is reported gone until its answer lands. What it did stays
    /// unconfirmed for whoever sent it.
    outstanding: Option<u16>,
}

/// Take the late answer to the request `channel` left outstanding, if it has
/// landed, answering whether the channel is free for the next.
fn reap<T>(channel: &mut Channel<T>) -> bool {
    let Some(late) = channel.outstanding else {
        return true;
    };
    if matches!(channel.requests.poll_used(), Ok(token) if token.head == late) {
        channel.outstanding = None;
    }
    channel.outstanding.is_none()
}

/// The event queue and its report buffers, which the one drain at a time
/// reads.
struct Events {
    queue: SplitQueue,
    /// The fault report buffers.
    reports: DmaSlab,
    /// The report buffer each event-queue descriptor heads.
    report_of: Vec<u16>,
}

/// What attaching an endpoint and silencing one change.
struct Lifecycle {
    bindings: Bindings,
    ids: Ids,
    /// Each endpoint's reserved regions, once probed.
    probed: HashMap<u32, Vec<Range<u64>>, BuildFastHash>,
}

struct DomainState<'f> {
    /// What the domain maps, as the family recorded it.
    shadow: IoPageTable<'f, Shadow>,
    /// Each mapping the device was asked for, by its first IOVA: the device
    /// keeps each whole, so an unmap names whole ones.
    maps: HashMap<u64, Mapping, BuildFastHash>,
    held: Held,
}

/// What of a domain's mappings the device holds.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Held {
    /// None: each is sent as an endpoint attaches.
    Nothing,
    /// Every one, each sent as it is made.
    Every,
    /// Some, after a replay or an emptying it did not finish: an unmap is
    /// still sent, a mapping it finds absent is no refusal, and an attach
    /// empties the domain before sending them all again.
    Some,
}

#[derive(Copy, Clone)]
struct Mapping {
    last: u64,
    phys: u64,
    access: Access,
}

impl DomainState<'_> {
    /// Whether `[iova, iova + len)` is exactly the mappings it covers: the
    /// specification forbids an unmap that splits one.
    fn covers_whole(&self, iova: u64, len: u64) -> Result<(), IommuError> {
        let end = iova
            .checked_add(len)
            .filter(|_| len != 0)
            .ok_or(IommuError::OutOfRange)?;
        let mut at = iova;
        while at < end {
            match self.maps.get(&at) {
                Some(map) if map.last < end => at = map.last + 1,
                Some(_) => return Err(IommuError::Split),
                None if self.shadow.leaf_at(at).is_some() => return Err(IommuError::Split),
                None => return Err(IommuError::NotMapped),
            }
        }
        Ok(())
    }

    /// Drop the record of the mappings [`Self::covers_whole`] found.
    fn forget(&mut self, iova: u64, end: u64) {
        let mut at = iova;
        while at < end {
            let Some(map) = self.maps.remove(&at) else {
                break;
            };
            at = map.last.saturating_add(1);
        }
    }
}

/// The entries of a domain's shadow: a tree no unit walks, so its format is
/// the family's own.
struct Shadow;

const PRESENT: u64 = 1 << 0;
const LEAF: u64 = 1 << 1;
const READ: u64 = 1 << 2;
const WRITE: u64 = 1 << 3;
const ADDRESS: u64 = !(IO_PAGE_SIZE - 1);

impl PteFormat for Shadow {
    fn leaf_allowed(&self, level: u32) -> bool {
        level <= 2
    }

    fn table(&self, phys: u64, _level: u32) -> u64 {
        phys | PRESENT
    }

    fn leaf(&self, phys: u64, _level: u32, access: Access) -> u64 {
        let mut entry = phys | PRESENT | LEAF;
        if access.read() {
            entry |= READ;
        }
        if access.write() {
            entry |= WRITE;
        }
        entry
    }

    fn decode(&self, entry: u64, _level: u32) -> Pte {
        if entry & PRESENT == 0 {
            return Pte::Absent;
        }
        if entry & LEAF == 0 {
            return Pte::Table(entry & ADDRESS);
        }
        let access = match (entry & READ != 0, entry & WRITE != 0) {
            (true, true) => Access::READ_WRITE,
            (false, true) => Access::WRITE,
            _ => Access::READ,
        };
        Pte::Leaf(entry & ADDRESS, access)
    }
}

/// What a device said of its queues, as the unit's error.
fn queue_error(err: VirtioError) -> IommuError {
    match err {
        VirtioError::QueueTooShallow
        | VirtioError::QueueSizeTooLarge
        | VirtioError::QueueIndexOutOfRange => IommuError::OutOfRange,
        VirtioError::OutOfMemory => IommuError::Exhausted,
        _ => IommuError::Hardware,
    }
}

/// What a request the device refused says of the operation.
fn refused(status: Status) -> IommuError {
    match status {
        Status::Invalid | Status::Range | Status::NoEntry => IommuError::OutOfRange,
        Status::NoMemory => IommuError::Exhausted,
        Status::Ok | Status::Failed => IommuError::Hardware,
    }
}

/// The negotiated device, before its queues exist.
struct Negotiated {
    input: (u64, u64),
    domains: (u32, u32),
    probe_size: usize,
}

impl<'f, T: Transport + Send> VirtioIommuUnit<'f, T> {
    /// Take over the virtio-iommu behind `transport`: reset it, negotiate
    /// with bypass off, build its queues from `frames`, and leave it running
    /// with every endpoint blocked, its event queue's interrupt quiet until
    /// [`IommuUnit::route_faults`]. `function` is the unit's own PCI
    /// function, by node address, where it is one; `signalling` says how it
    /// raises its interrupt, which the transport was built for.
    ///
    /// # Errors
    ///
    /// [`IommuError::OutOfRange`] for a device this family cannot drive: one
    /// that is no modern virtio-iommu, takes no map and unmap requests, maps
    /// no 4 KiB page, names an empty input or domain range, asks a probe to
    /// fill more than this family reads, or holds too few queues.
    /// [`IommuError::Hardware`] for one that refuses its features or keeps
    /// bypassing. [`IommuError::Exhausted`] when its rings cannot be had.
    pub fn new(
        transport: T,
        frames: &'f dyn PageTableFrames,
        clock: &'f dyn Clock,
        function: Option<(&'f dyn UnitFunction, u32)>,
        signalling: Signalling,
    ) -> Result<Self, IommuError> {
        Self::bring_up(transport, frames, clock, function, signalling).map_err(
            |(err, transport)| {
                fail(transport);
                err
            },
        )
    }

    fn bring_up(
        mut transport: T,
        frames: &'f dyn PageTableFrames,
        clock: &'f dyn Clock,
        function: Option<(&'f dyn UnitFunction, u32)>,
        signalling: Signalling,
    ) -> Result<Self, (IommuError, T)> {
        let negotiated = match negotiate(&mut transport) {
            Ok(negotiated) => negotiated,
            Err(err) => return Err((err, transport)),
        };
        let (first, last) = negotiated.input;
        let input_bits = (u64::BITS - last.leading_zeros()).max(IO_PAGE_SHIFT + 1);
        let Some(levels) = (1..=MAX_LEVELS).find(|&levels| reach_bits(levels) >= input_bits) else {
            return Err((IommuError::OutOfRange, transport));
        };
        let reach = Reach {
            input_bits,
            output_bits: u64::BITS,
        };
        let outside = outside_of(first, last, input_bits);
        let memory = TableMemory::new(frames, None);
        let rings = FrameRings::new(frames, clock);
        let queues = build_queues(&mut transport, &rings, negotiated.probe_size);
        let (requests, events, request, probe, reports) = match queues {
            Ok(queues) => queues,
            Err(err) => return Err((err, transport)),
        };
        let mut report_of = Vec::new();
        if report_of
            .try_reserve_exact(usize::from(events.size()))
            .is_err()
        {
            return Err((IommuError::Exhausted, transport));
        }
        report_of.resize(usize::from(events.size()), 0);
        let (domain_first, domain_last) = negotiated.domains;
        let unit = Self {
            memory,
            clock,
            function,
            profile: UnitProfile {
                tables: Tables::Kept,
                reach,
                reserved: &[],
                write_only: true,
            },
            levels,
            input: negotiated.input,
            outside,
            probe_size: negotiated.probe_size,
            signalling,
            channel: SpinLock::new(Channel {
                transport,
                requests,
                request,
                probe,
                outstanding: None,
            }),
            events: SpinLock::new(Events {
                queue: events,
                reports,
                report_of,
            }),
            lifecycle: SpinLock::new(Lifecycle {
                bindings: Bindings::new(),
                ids: Ids::new(domain_first, domain_last.saturating_add(1)),
                probed: HashMap::with_hasher(BuildFastHash::new()),
            }),
            domains: DomainMap::new(),
        };
        let mut channel = unit.channel.lock();
        let status = channel.transport.status().bits();
        channel
            .transport
            .set_status(DeviceStatus::from_bits(status | DeviceStatus::DRIVER_OK));
        if channel
            .transport
            .status()
            .contains(DeviceStatus::DEVICE_NEEDS_RESET)
        {
            drop(channel);
            return Err((IommuError::Hardware, unit.channel.into_inner().transport));
        }
        drop(channel);
        // Each report buffer is the event queue's from here; a report the
        // device writes before the interrupt is routed waits for the first
        // drain.
        let mut queue = unit.events.lock();
        let Events {
            queue: events,
            reports,
            report_of,
        } = &mut *queue;
        for slot in 0..events.size() {
            let Ok(head) = events.add_chain(&[report_segment(reports, slot)]) else {
                break;
            };
            if let Some(entry) = report_of.get_mut(usize::from(head)) {
                *entry = slot;
            }
        }
        events.kick(&mut unit.channel.lock().transport);
        drop(queue);
        Ok(unit)
    }

    /// Send `request` and wait for the device's answer, the channel held: one
    /// request is in flight at a time.
    fn request(&self, request: &Request) -> Result<Status, IommuError> {
        self.request_reading(request, |_, _| ())
            .map(|(status, ())| status)
    }

    /// [`Self::request`], `answer` reading the reply's status and the buffer
    /// it was written to.
    fn request_reading<A>(
        &self,
        request: &Request,
        answer: impl FnOnce(Status, &[u8]) -> A,
    ) -> Result<(Status, A), IommuError> {
        let mut channel = self.channel.lock();
        if !reap(&mut channel) {
            return Err(IommuError::Unconfirmed);
        }
        let Channel {
            transport,
            requests,
            request: buffer,
            probe,
            outstanding,
        } = &mut *channel;
        let (slab, readable, written) = match request {
            Request::Probe { .. } => (
                probe,
                format::PROBE_REQUEST_LEN,
                self.probe_size + format::TAIL_LEN,
            ),
            _ => (buffer, format::REQUEST_MAX, format::TAIL_LEN),
        };
        let reply_at = match request {
            Request::Probe { .. } => format::PROBE_REQUEST_LEN,
            _ => TAIL_AT,
        };
        let bytes = slab.as_bytes_mut();
        let used = request.encode(&mut bytes[..readable]);
        bytes[reply_at..reply_at + written].fill(0xFF);
        let base = slab.device_addr();
        let segments = [
            ChainSegment {
                device_addr: base,
                len: u32::try_from(used).map_err(|_| IommuError::OutOfRange)?,
                direction: Direction::DeviceRead,
            },
            ChainSegment {
                device_addr: base + reply_at as u64,
                len: u32::try_from(written).map_err(|_| IommuError::OutOfRange)?,
                direction: Direction::DeviceWrite,
            },
        ];
        let head = requests
            .add_chain(&segments)
            .map_err(|_| IommuError::Hardware)?;
        requests.kick(transport);
        let answered = wait_for(self.clock, || match requests.poll_used() {
            Ok(token) if token.head == head => Ok(true),
            Err(VirtioError::NoCompletion) => Ok(false),
            _ => Err(IommuError::Hardware),
        });
        // Unanswered, or answered for a chain it was not given: either way the
        // request is still the device's.
        if answered.is_err() {
            *outstanding = Some(head);
            return Err(IommuError::Unconfirmed);
        }
        let bytes = slab.as_bytes();
        let status = Status::of(bytes[reply_at + written - format::TAIL_LEN]);
        Ok((status, answer(status, bytes)))
    }

    /// Whether a request the device has not answered still holds the channel.
    fn stuck(&self) -> bool {
        !reap(&mut self.channel.lock())
    }

    /// Detach `stream` from the domain it holds, if any, and confirm it. The
    /// last endpoint's domain is emptied too, for a device that keeps a
    /// domain with no endpoint: attached again, it is replayed whole. The
    /// last endpoint holds its domain until the emptying is confirmed, so a
    /// refused unmap is met again by the next attempt, never skipped by it.
    fn detach(&self, life: &mut Lifecycle, stream: u32) -> Result<(), IommuError> {
        let Some(id) = life.bindings.held(stream) else {
            return Ok(());
        };
        life.bindings.unbind(stream);
        match self.request(&Request::Detach {
            domain: id,
            endpoint: stream,
        })? {
            // Not attached, or no endpoint by that id: neither reaches the
            // domain.
            Status::Ok | Status::Invalid | Status::NoEntry => {}
            status => return Err(refused(status)),
        }
        if life.bindings.holders(id) == 1 {
            // Under the domain's lock, so no mapping is sent meanwhile; one
            // the device refused to drop leaves it holding the domain.
            self.domains.with(id, |domain| self.empty(id, domain))?;
        }
        life.bindings.release(stream);
        Ok(())
    }

    /// Have the device drop every mapping of domain `id` it holds.
    fn empty(&self, id: u32, domain: &mut DomainState<'f>) -> Result<(), IommuError> {
        if domain.held == Held::Nothing {
            return Ok(());
        }
        // Unanswered or refused, it may have dropped any of them.
        domain.held = Held::Some;
        let (first, last) = self.input;
        match self.request(&Request::Unmap {
            domain: id,
            first,
            last,
        })? {
            Status::Ok | Status::NoEntry => {
                domain.held = Held::Nothing;
                Ok(())
            }
            status => Err(refused(status)),
        }
    }

    /// Have the device hold every mapping of domain `id`, emptying it first
    /// where it holds only some.
    fn fill(&self, id: u32, domain: &mut DomainState<'f>) -> Result<(), IommuError> {
        if domain.held == Held::Some {
            self.empty(id, domain)?;
        }
        if domain.held == Held::Nothing {
            domain.held = Held::Some;
            self.replay(id, domain)?;
            domain.held = Held::Every;
        }
        Ok(())
    }

    /// Send every mapping recorded for domain `id` to the device again, as its
    /// first endpoint attaches: one request each, as first asked, so the
    /// device holds exactly the mappings a later unmap names whole.
    fn replay(&self, id: u32, domain: &DomainState<'f>) -> Result<(), IommuError> {
        for (&first, map) in &domain.maps {
            let request = Request::Map {
                domain: id,
                first,
                last: map.last,
                phys: map.phys,
                access: map.access,
            };
            match self.request(&request)? {
                Status::Ok => {}
                status => return Err(refused(status)),
            }
        }
        Ok(())
    }

    /// Move pending fault reports into `batch`, oldest first, handing each
    /// buffer back to the device, and answer whether more may remain.
    fn take_reports(&self, batch: &mut FaultBatch) -> bool {
        let mut queue = self.events.lock();
        let Events {
            queue: events,
            reports,
            report_of,
        } = &mut *queue;
        self.channel.lock().transport.ack_interrupt();
        let mut reposted = false;
        // Every poll counts, malformed ones too, so a device spraying
        // completions cannot hold the drain.
        for _ in 0..events.size() {
            if batch.is_full() {
                break;
            }
            let token = match events.poll_used() {
                Ok(token) => token,
                Err(VirtioError::NoCompletion) => break,
                Err(_) => continue,
            };
            let Some(&slot) = report_of.get(usize::from(token.head)) else {
                continue;
            };
            let at = usize::from(slot) * format::FAULT_LEN;
            let mut record = [0; format::FAULT_LEN];
            record.copy_from_slice(&reports.as_bytes()[at..at + format::FAULT_LEN]);
            if let Ok(head) = events.add_chain(&[report_segment(reports, slot)]) {
                if let Some(entry) = report_of.get_mut(usize::from(head)) {
                    *entry = slot;
                }
                reposted = true;
            }
            if token.written < format::FAULT_BYTES {
                continue;
            }
            if let Some(fault) = format::report(&record).and_then(|report| self.classify(report)) {
                let _ = batch.try_push(fault);
            }
        }
        if reposted {
            events.kick(&mut self.channel.lock().transport);
        }
        batch.is_full()
    }

    /// A report as a fault, classed by the endpoint's domain, or [`None`] for
    /// a silenced endpoint's: silence is a filter here, as a device cannot be
    /// told to stop reporting.
    fn classify(&self, report: Report) -> Option<Fault> {
        let fault = match report {
            Report::Unattached(fault) | Report::Mapping(fault) | Report::Other(fault) => fault,
        };
        let (binding, held) = {
            let life = self.lifecycle.lock();
            (
                life.bindings.get(fault.stream),
                life.bindings.held(fault.stream),
            )
        };
        if binding == Some(Binding::Silenced) {
            return None;
        }
        let reason = match report {
            Report::Unattached(_) => FaultReason::Blocked,
            Report::Mapping(_) => {
                let mapped = held.is_some_and(|id| {
                    fault.iova != 0
                        && self
                            .domains
                            .with(id, |domain| {
                                Ok(domain.shadow.translate(fault.iova).is_some())
                            })
                            .unwrap_or(false)
                });
                if mapped {
                    FaultReason::Denied
                } else {
                    FaultReason::Unmapped
                }
            }
            Report::Other(_) if held.is_none() => FaultReason::Blocked,
            Report::Other(other) => other.reason,
        };
        Some(Fault { reason, ..fault })
    }
}

/// Tell the device the driver gave up on it, as virtio asks of a failed
/// initialisation.
fn fail<T: Transport>(mut transport: T) {
    let status = transport.status().bits();
    transport.set_status(DeviceStatus::from_bits(status | DeviceStatus::FAILED));
}

/// Every feature this family uses, taken wherever the device offers it.
const ACCEPTED: u64 = TRANSPORT_FEATURES
    | feature::INPUT_RANGE
    | feature::DOMAIN_RANGE
    | feature::MAP_UNMAP
    | feature::PROBE
    | feature::BYPASS_CONFIG;

const _: () = assert!(
    ACCEPTED & feature::BYPASS == 0,
    "an endpoint no domain holds never bypasses the unit"
);

/// Why negotiation left a device undriven: one this family cannot drive, or
/// one that would not be driven.
enum Undriven {
    Unsupported,
    Refused,
}

impl From<VirtioError> for Undriven {
    fn from(_: VirtioError) -> Self {
        Self::Refused
    }
}

/// Reset the device and negotiate: every feature this family uses that the
/// device offers, bypass never, and bypass turned off where the device lets
/// it be.
fn negotiate<T: Transport>(transport: &mut T) -> Result<Negotiated, IommuError> {
    // A field past the window reads zero and its write is dropped, so a bypass
    // turned off there would read back off while the device still bypassed.
    if transport.config_len() < config::LEN {
        return Err(IommuError::OutOfRange);
    }
    // A probe is how the device names the IOVA its interrupt messages land
    // at, which no domain may be handed.
    let required = VIRTIO_F_VERSION_1 | feature::MAP_UNMAP | feature::PROBE;
    let features = tairix_virtio::negotiate(
        transport,
        || (),
        |offered| {
            if offered & required == required {
                Ok(offered & ACCEPTED)
            } else {
                Err(Undriven::Unsupported)
            }
        },
    )
    .map_err(|undriven| match undriven {
        Undriven::Unsupported => IommuError::OutOfRange,
        Undriven::Refused => IommuError::Hardware,
    })?
    .features;
    let page_sizes = transport.read_config_u64(config::PAGE_SIZE_MASK);
    if page_sizes == 0 || page_sizes.trailing_zeros() > IO_PAGE_SHIFT {
        return Err(IommuError::OutOfRange);
    }
    let input = if features & feature::INPUT_RANGE != 0 {
        (
            transport.read_config_u64(config::INPUT_START),
            transport.read_config_u64(config::INPUT_END),
        )
    } else {
        (0, u64::MAX)
    };
    // A range holding no whole page past the null one translates nothing.
    let first_page = input
        .0
        .max(IO_PAGE_SIZE)
        .checked_next_multiple_of(IO_PAGE_SIZE)
        .and_then(|page| page.checked_add(IO_PAGE_SIZE - 1));
    if input.1 < input.0 || first_page.is_none_or(|last| last > input.1) {
        return Err(IommuError::OutOfRange);
    }
    let domains = if features & feature::DOMAIN_RANGE != 0 {
        (
            transport.read_config_u32(config::DOMAIN_START),
            transport.read_config_u32(config::DOMAIN_END),
        )
    } else {
        (0, u32::MAX)
    };
    if domains.1 < domains.0 {
        return Err(IommuError::OutOfRange);
    }
    let probe_size = match transport.read_config_u32(config::PROBE_SIZE) {
        size @ 1..=format::PROBE_SIZE_MAX => {
            usize::try_from(size).map_err(|_| IommuError::OutOfRange)?
        }
        _ => return Err(IommuError::OutOfRange),
    };
    if features & feature::BYPASS_CONFIG != 0 {
        transport.write_config(config::BYPASS, &[0]);
        let mut bypass = [0];
        transport.read_config(config::BYPASS, &mut bypass);
        if bypass[0] & 1 != 0 {
            return Err(IommuError::Hardware);
        }
    }
    Ok(Negotiated {
        input,
        domains,
        probe_size,
    })
}

type Queues = (SplitQueue, SplitQueue, DmaSlab, DmaSlab, DmaSlab);

/// The request and event queues, their interrupts asked off, and the
/// buffers requests and reports live in.
fn build_queues<T: Transport>(
    transport: &mut T,
    rings: &FrameRings<'_>,
    probe_size: usize,
) -> Result<Queues, IommuError> {
    if transport.num_queues() <= format::EVENT_QUEUE {
        return Err(IommuError::OutOfRange);
    }
    let exhausted = |_| IommuError::Exhausted;
    let mut requests = SplitQueue::new(transport, rings, format::REQUEST_QUEUE, REQUEST_DEPTH, 2)
        .map_err(queue_error)?;
    let mut events = SplitQueue::new(transport, rings, format::EVENT_QUEUE, REPORT_DEPTH, 1)
        .map_err(queue_error)?;
    // Requests are waited for, never signalled; reports are signalled only
    // once routed.
    requests.suppress_used_interrupts(true);
    events.suppress_used_interrupts(true);
    let request = rings.alloc_dma_zeroed(REQUEST_SLAB).map_err(exhausted)?;
    let probe = rings
        .alloc_dma_zeroed(format::PROBE_REQUEST_LEN + probe_size + format::TAIL_LEN)
        .map_err(exhausted)?;
    let reports = rings
        .alloc_dma_zeroed(usize::from(events.size()) * format::FAULT_LEN)
        .map_err(exhausted)?;
    Ok((requests, events, request, probe, reports))
}

/// Report buffer `slot`, as the device is handed it.
fn report_segment(reports: &DmaSlab, slot: u16) -> ChainSegment {
    ChainSegment {
        device_addr: reports.device_addr() + u64::from(slot) * u64::from(format::FAULT_BYTES),
        len: format::FAULT_BYTES,
        direction: Direction::DeviceWrite,
    }
}

/// What the input range `first..=last` leaves out of a domain of
/// `input_bits` bits of IOVA: below its first page, and from the page its
/// last byte ends in onwards, both page-aligned.
fn outside_of(first: u64, last: u64, input_bits: u32) -> ArrayVec<Range<u64>, 2> {
    let mut outside = ArrayVec::new();
    let top = if input_bits >= u64::BITS {
        ADDRESS
    } else {
        1 << input_bits
    };
    let low = first
        .checked_next_multiple_of(IO_PAGE_SIZE)
        .unwrap_or(top)
        .min(top);
    if low > 0 {
        let _ = outside.try_push(0..low);
    }
    // The page holding `last` is whole only where `last` ends it.
    let high = last
        .checked_add(1)
        .map_or(top, |end| end & ADDRESS)
        .min(top);
    if high < top {
        let _ = outside.try_push(high..top);
    }
    outside
}

impl<T: Transport + Send> IommuUnit for VirtioIommuUnit<'_, T> {
    fn profile(&self) -> UnitProfile {
        self.profile
    }

    /// Endpoints are blocked from the take-over on, bypass being off; the
    /// unit has nothing more to turn on.
    fn enable(&self) -> Result<(), IommuError> {
        if self.stuck() {
            return Err(IommuError::Unconfirmed);
        }
        Ok(())
    }

    fn create_domain(&self) -> Result<DomainId, IommuError> {
        let mut life = self.lifecycle.lock();
        let id = life.ids.take().ok_or(IommuError::Exhausted)?;
        let reach = self.profile.reach;
        let made = IoPageTable::new(Shadow, self.levels, self.memory, reach).and_then(|shadow| {
            self.domains
                .insert(
                    id,
                    DomainState {
                        shadow,
                        maps: HashMap::with_hasher(BuildFastHash::new()),
                        held: Held::Nothing,
                    },
                )
                .map_err(|(err, _domain)| err)
        });
        if let Err(err) = made {
            life.ids.release(id, true);
            return Err(err);
        }
        Ok(DomainId(id))
    }

    /// A domain no endpoint holds was emptied on the device as its last
    /// endpoint detached, so only the shadow is left to free.
    fn destroy_domain(&self, domain: DomainId) -> Result<(), IommuError> {
        let id = domain.0;
        let mut life = self.lifecycle.lock();
        if !self.domains.contains(id) {
            return Err(IommuError::OutOfRange);
        }
        if life.bindings.holders(id) != 0 {
            return Err(IommuError::DomainBusy);
        }
        self.domains.remove(id);
        life.ids.release(id, true);
        Ok(())
    }

    fn attach(&self, stream: u32, domain: DomainId) -> Result<(), IommuError> {
        let id = domain.0;
        let mut life = self.lifecycle.lock();
        if !self.domains.contains(id) {
            return Err(IommuError::OutOfRange);
        }
        let Some(room) = life.bindings.prepare_attach(stream, id)? else {
            return Ok(());
        };
        let attach = Request::Attach {
            domain: id,
            endpoint: stream,
        };
        match self.request(&attach) {
            Ok(Status::Ok) => {}
            Ok(Status::NoEntry) => return Err(IommuError::NoEndpoint),
            Ok(status) => return Err(refused(status)),
            Err(IommuError::Unconfirmed) => {
                // The device may have attached it: it holds the domain until
                // a confirmed detach.
                life.bindings.hold(room, stream, id);
                return Err(IommuError::Unconfirmed);
            }
            Err(err) => return Err(err),
        }
        life.bindings.hold(room, stream, id);
        // Under the domain's lock, so no mapping made meanwhile is either sent
        // twice or missed.
        if let Err(err) = self.domains.with(id, |domain| self.fill(id, domain)) {
            // Not left translating through a domain missing mappings.
            return match self.detach(&mut life, stream) {
                Ok(()) => Err(err),
                Err(_) => Err(IommuError::Unconfirmed),
            };
        }
        Ok(())
    }

    /// A silenced stream stays silent: only an attach gives it an owner
    /// again.
    fn block(&self, stream: u32) -> Result<(), IommuError> {
        self.detach(&mut self.lifecycle.lock(), stream)
    }

    fn silence(&self, stream: u32) -> Result<(), IommuError> {
        let mut life = self.lifecycle.lock();
        if life.bindings.get(stream) == Some(Binding::Silenced) {
            return Ok(());
        }
        let room = life.bindings.reserve()?;
        self.detach(&mut life, stream)?;
        life.bindings.silence(room, stream);
        Ok(())
    }

    fn map(
        &self,
        domain: DomainId,
        iova: u64,
        phys: u64,
        len: u64,
        access: Access,
    ) -> Result<(), IommuError> {
        let id = domain.0;
        let last = len
            .checked_sub(1)
            .and_then(|span| iova.checked_add(span))
            .ok_or(IommuError::OutOfRange)?;
        let (first, top) = self.input;
        if (!access.read() && !access.write()) || iova < first || last > top {
            return Err(IommuError::OutOfRange);
        }
        if self.stuck() {
            return Err(IommuError::Unconfirmed);
        }
        self.domains.with(id, |mapped| {
            mapped
                .maps
                .try_reserve(1)
                .map_err(|_| IommuError::Exhausted)?;
            mapped.shadow.map(iova, phys, len, access)?;
            let record = Mapping { last, phys, access };
            if mapped.held != Held::Every {
                let _ = mapped.maps.try_insert(iova, record);
                return Ok(());
            }
            let request = Request::Map {
                domain: id,
                first: iova,
                last,
                phys,
                access,
            };
            match self.request(&request) {
                Ok(Status::Ok) => {
                    let _ = mapped.maps.try_insert(iova, record);
                    Ok(())
                }
                Err(IommuError::Unconfirmed) => Err(IommuError::Unconfirmed),
                other => {
                    // Refused whole: the device installed none of it.
                    let _ = mapped.shadow.unmap(iova, len);
                    mapped.shadow.release_retired();
                    Err(match other {
                        Ok(Status::Invalid) => IommuError::AlreadyMapped,
                        Ok(status) => refused(status),
                        Err(err) => err,
                    })
                }
            }
        })
    }

    fn unmap(&self, domain: DomainId, iova: u64, len: u64) -> Result<(), IommuError> {
        let id = domain.0;
        if self.stuck() {
            return Err(IommuError::Unconfirmed);
        }
        self.domains.with(id, |domain| {
            domain.covers_whole(iova, len)?;
            if domain.held != Held::Nothing {
                let request = Request::Unmap {
                    domain: id,
                    first: iova,
                    last: iova + (len - 1),
                };
                match (self.request(&request)?, domain.held) {
                    (Status::Ok, _) | (Status::NoEntry, Held::Some) => {}
                    (status, _) => return Err(refused(status)),
                }
            }
            domain.forget(iova, iova + len);
            domain.shadow.unmap(iova, len)?;
            // No unit walks the shadow, so what an unmap emptied goes at once.
            domain.shadow.release_retired();
            Ok(())
        })
    }

    /// Every unmap was answered, and an answer confirms it: nothing waits.
    fn sync(&self, domain: DomainId) -> Result<(), IommuError> {
        if self.stuck() {
            return Err(IommuError::Unconfirmed);
        }
        if !self.domains.contains(domain.0) {
            return Err(IommuError::OutOfRange);
        }
        Ok(())
    }

    /// A PCI function's reports are raised through its MSI-X entry or its
    /// INTx pin; a virtio-mmio slot's on the line its node names.
    fn route_faults(&self, route: FaultRoute) -> Result<(), IommuError> {
        match (route, self.signalling, self.function) {
            (FaultRoute::Message { address, data }, Signalling::Message, Some((function, at))) => {
                function.route_msix(at, MSIX_ENTRY, address, data)?;
            }
            (FaultRoute::Wired { .. }, Signalling::Wired, Some((function, at))) => {
                function.set_intx(at, true)?;
            }
            (FaultRoute::Wired { .. }, Signalling::Wired, None) => {}
            _ => return Err(IommuError::OutOfRange),
        }
        self.events.lock().queue.suppress_used_interrupts(false);
        Ok(())
    }

    /// Asking the device not to signal is advice it may ignore, so only a
    /// function the kernel can mask is stopped; an MMIO unit's wire cannot be.
    fn unroute_faults(&self) -> Result<(), IommuError> {
        self.events.lock().queue.suppress_used_interrupts(true);
        match (self.signalling, self.function) {
            (Signalling::Message, Some((function, at))) => function.mask_msix(at, true),
            (Signalling::Wired, Some((function, at))) => function.set_intx(at, false),
            _ => Err(IommuError::OutOfRange),
        }
    }

    /// At most one queue's worth of reports per call.
    fn drain_faults(&self, sink: &mut dyn FnMut(Fault)) -> bool {
        drain_in_batches(
            FAULT_QUEUE_RECORDS as usize,
            |batch| self.take_reports(batch),
            sink,
        )
    }

    fn reserved_iova(
        &self,
        stream: u32,
        sink: &mut dyn FnMut(Range<u64>),
    ) -> Result<(), IommuError> {
        for range in self.outside.as_slice() {
            sink(range.clone());
        }
        let mut life = self.lifecycle.lock();
        if let Some(regions) = life.probed.get(&stream) {
            for region in regions {
                sink(region.clone());
            }
            return Ok(());
        }
        life.probed
            .try_reserve(1)
            .map_err(|_| IommuError::Exhausted)?;
        let mut regions = Vec::new();
        let (status, parsed) =
            self.request_reading(&Request::Probe { endpoint: stream }, |status, reply| {
                if status != Status::Ok {
                    return Ok(());
                }
                let properties =
                    &reply[format::PROBE_REQUEST_LEN..format::PROBE_REQUEST_LEN + self.probe_size];
                let mut room = true;
                let parsed = format::reserved_regions(properties, &mut |region| {
                    room &= regions.try_reserve(1).is_ok();
                    if room {
                        regions.push(region);
                    }
                });
                if room {
                    parsed
                } else {
                    Err(IommuError::Exhausted)
                }
            })?;
        match status {
            Status::Ok => parsed?,
            // An endpoint the device does not have keeps nothing out.
            Status::NoEntry => {
                let _ = life.probed.try_insert(stream, Vec::new());
                return Ok(());
            }
            status => return Err(refused(status)),
        }
        for region in &regions {
            sink(region.clone());
        }
        let _ = life.probed.try_insert(stream, regions);
        Ok(())
    }
}
