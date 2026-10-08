//! A virtio-iommu device, modelled from the specification (virtio 1.3
//! §5.13) as a hypervisor implements one: feature negotiation and its
//! configuration, the request queue read from the rings the driver builds in
//! memory, the endpoints and domains it keeps, and the fault reports it
//! writes into the event queue's buffers.

use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::vec::Vec;

use tairix_kernel_iommu_api::conformance::TranslationProbe;
use tairix_kernel_iommu_api::hostmem::HostFrames;
use tairix_sync::SpinLock;
use tairix_virtio::{Status, Transport, VirtioError, VIRTIO_F_ACCESS_PLATFORM, VIRTIO_F_VERSION_1};

use crate::format::{config, feature};

const S_OK: u8 = 0;
const S_DEVERR: u8 = 3;
const S_INVAL: u8 = 4;
const S_RANGE: u8 = 5;
const S_NOENT: u8 = 6;

const VRING_DESC_F_NEXT: u16 = 1;
const VRING_DESC_F_WRITE: u16 = 2;
const VIRTQ_AVAIL_F_NO_INTERRUPT: u16 = 1;

/// How the model behaves: as QEMU's device does, or departing from it in
/// one way, to prove the family copes.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub(crate) enum Behaviour {
    /// A domain whose last endpoint detaches is destroyed, as QEMU's is.
    #[default]
    Correct,
    /// A domain whose last endpoint detaches is kept, mappings and all.
    KeepsDomains,
    /// As [`Self::KeepsDomains`], but every unmap fails on the device.
    RefusesUnmaps,
    /// As [`Self::Correct`], but every map fails on the device.
    RefusesMaps,
    /// Requests are never answered.
    Silent,
    /// Only the bypass feature older devices offer, not its configuration.
    LegacyBypass,
    /// `bypass` ignores the driver's write.
    BypassStuck,
    /// Each answer names a chain other than the one it answers.
    MisnamesChains,
}

/// One endpoint's reserved region, as a probe reports it.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Reserved {
    pub subtype: u8,
    pub first: u64,
    pub last: u64,
}

#[derive(Clone, Default)]
struct Queue {
    size: u16,
    desc: u64,
    avail: u64,
    used: u64,
    /// The avail-ring index the device consumes next.
    next: u16,
    /// The used-ring index it publishes next.
    used_idx: u16,
}

#[derive(Default)]
struct DeviceDomain {
    endpoints: BTreeSet<u32>,
    /// Mappings by first IOVA: their last IOVA, physical address and flags.
    maps: BTreeMap<u64, (u64, u64, u32)>,
}

struct State {
    offered: u64,
    accepted: u64,
    status: u8,
    page_sizes: u64,
    input: (u64, u64),
    domain_range: (u32, u32),
    probe_size: u32,
    bypass: u8,
    /// Bytes of configuration the transport exposes.
    config_len: usize,
    queues: [Queue; 2],
    selected: u16,
    queue_max: [u16; 2],
    endpoints: BTreeMap<u32, Option<u32>>,
    reserved: BTreeMap<u32, Vec<Reserved>>,
    domains: BTreeMap<u32, DeviceDomain>,
    behaviour: Behaviour,
    /// Maps installed before every further one fails, where limited.
    maps_left: Option<usize>,
    /// Requests answered, by type.
    answered: BTreeMap<u8, usize>,
    /// Reports the device could not write for want of a buffer.
    dropped: usize,
    /// Its interrupt is raised and not yet acknowledged.
    raised: bool,
    pending: VecDeque<[u8; 24]>,
}

/// A modelled virtio-iommu over host memory.
pub(crate) struct Device<'f> {
    frames: &'f HostFrames,
    state: SpinLock<State>,
}

impl<'f> Device<'f> {
    /// A device behind which `endpoints` sit, behaving as `behaviour` says,
    /// translating `input`, mapping 4 KiB pages and up.
    pub(crate) fn new(
        frames: &'f HostFrames,
        endpoints: &[u32],
        input: (u64, u64),
        behaviour: Behaviour,
    ) -> Self {
        let bypass = if behaviour == Behaviour::LegacyBypass {
            feature::BYPASS
        } else {
            feature::BYPASS_CONFIG
        };
        Self {
            frames,
            state: SpinLock::new(State {
                offered: VIRTIO_F_VERSION_1
                    | VIRTIO_F_ACCESS_PLATFORM
                    | feature::INPUT_RANGE
                    | feature::DOMAIN_RANGE
                    | feature::MAP_UNMAP
                    | feature::PROBE
                    | feature::MMIO
                    | bypass,
                accepted: 0,
                status: 0,
                page_sizes: !0xFFF,
                input,
                domain_range: (1, 0xFFFF),
                probe_size: 512,
                bypass: 1,
                config_len: config::LEN,
                queues: [Queue::default(), Queue::default()],
                selected: 0,
                queue_max: [256, 256],
                endpoints: endpoints.iter().map(|&endpoint| (endpoint, None)).collect(),
                reserved: BTreeMap::new(),
                domains: BTreeMap::new(),
                behaviour,
                maps_left: None,
                answered: BTreeMap::new(),
                dropped: 0,
                raised: false,
                pending: VecDeque::new(),
            }),
        }
    }

    /// Offer `features` in place of the default set.
    pub(crate) fn offer(&self, features: u64) {
        self.state.lock().offered = features;
    }

    /// Expose only the first `len` bytes of configuration: the rest read
    /// zero and writes to them are dropped.
    pub(crate) fn expose_config(&self, len: usize) {
        self.state.lock().config_len = len;
    }

    /// Hold each queue to at most `max` descriptors.
    pub(crate) fn queue_max(&self, max: [u16; 2]) {
        self.state.lock().queue_max = max;
    }

    /// Have probes of `endpoint` report `regions`.
    pub(crate) fn reserve(&self, endpoint: u32, regions: &[Reserved]) {
        self.state
            .lock()
            .reserved
            .insert(endpoint, regions.to_vec());
    }

    /// Behave as `behaviour` says from now on, installing maps without limit.
    pub(crate) fn behave(&self, behaviour: Behaviour) {
        let mut state = self.state.lock();
        state.behaviour = behaviour;
        state.maps_left = None;
    }

    /// Install only `maps` more mappings: every map after them fails.
    pub(crate) fn fail_maps_after(&self, maps: usize) {
        self.state.lock().maps_left = Some(maps);
    }

    /// Answer, late, what was asked while silent, behaving correctly from now
    /// on.
    pub(crate) fn catch_up(&self) {
        let mut state = self.state.lock();
        state.behaviour = Behaviour::Correct;
        self.serve_requests(&mut state);
    }

    /// Map pages of `2^shift` bytes and up, and no smaller.
    pub(crate) fn granule(&self, shift: u32) {
        self.state.lock().page_sizes = !((1 << shift) - 1);
    }

    /// Report `bytes` as the room a probe's properties may fill.
    pub(crate) fn probe_size(&self, bytes: u32) {
        self.state.lock().probe_size = bytes;
    }

    /// The features the driver accepted.
    pub(crate) fn accepted(&self) -> u64 {
        self.state.lock().accepted
    }

    /// The device's status byte.
    pub(crate) fn status(&self) -> u8 {
        self.state.lock().status
    }

    /// The `bypass` configuration byte.
    pub(crate) fn bypass(&self) -> u8 {
        self.state.lock().bypass
    }

    /// Requests of type `kind` the device answered.
    pub(crate) fn answered(&self, kind: u8) -> usize {
        self.state.lock().answered.get(&kind).copied().unwrap_or(0)
    }

    /// Domains the device keeps.
    pub(crate) fn domains(&self) -> usize {
        self.state.lock().domains.len()
    }

    /// Mappings domain `domain` holds on the device.
    pub(crate) fn mappings(&self, domain: u32) -> usize {
        self.state
            .lock()
            .domains
            .get(&domain)
            .map_or(0, |domain| domain.maps.len())
    }

    /// Reports dropped for want of a buffer.
    pub(crate) fn dropped(&self) -> usize {
        self.state.lock().dropped
    }

    /// Whether the device's interrupt is raised.
    pub(crate) fn raised(&self) -> bool {
        self.state.lock().raised
    }

    /// The transport the family drives the device through.
    pub(crate) fn transport(&self) -> ModelTransport<'_, 'f> {
        ModelTransport { device: self }
    }

    /// Read `out.len()` bytes at `address`, as the driver shares them: plainly.
    fn read(&self, address: u64, out: &mut [u8]) -> bool {
        self.frames.read_bytes(address, out)
    }

    /// Write `bytes` at `address`, as [`Self::read`] reads them.
    fn write(&self, address: u64, bytes: &[u8]) -> bool {
        self.frames.write_bytes(address, bytes)
    }

    fn read_u16(&self, address: u64) -> u16 {
        let mut bytes = [0; 2];
        self.read(address, &mut bytes);
        u16::from_le_bytes(bytes)
    }

    /// The chain at `head` of queue `queue`: what the device may read, and
    /// the buffers it may write, as `(address, length)`.
    fn chain(&self, queue: &Queue, head: u16) -> (Vec<u8>, Vec<(u64, u32)>) {
        let mut readable = Vec::new();
        let mut writable = Vec::new();
        let mut at = head;
        for _ in 0..queue.size {
            let base = queue.desc + u64::from(at) * 16;
            let mut descriptor = [0; 16];
            self.read(base, &mut descriptor);
            let address = u64::from_le_bytes(descriptor[..8].try_into().unwrap());
            let len = u32::from_le_bytes(descriptor[8..12].try_into().unwrap());
            let flags = u16::from_le_bytes(descriptor[12..14].try_into().unwrap());
            if flags & VRING_DESC_F_WRITE != 0 {
                writable.push((address, len));
            } else {
                let mut bytes = alloc::vec![0; len as usize];
                self.read(address, &mut bytes);
                readable.extend_from_slice(&bytes);
            }
            if flags & VRING_DESC_F_NEXT == 0 {
                break;
            }
            at = u16::from_le_bytes(descriptor[14..16].try_into().unwrap());
        }
        (readable, writable)
    }

    /// Publish `head` on queue `queue`'s used ring, `written` bytes long.
    fn publish(&self, queue: &mut Queue, head: u16, written: u32) {
        let slot = u64::from(queue.used_idx % queue.size);
        let mut entry = [0; 8];
        entry[..4].copy_from_slice(&u32::from(head).to_le_bytes());
        entry[4..].copy_from_slice(&written.to_le_bytes());
        self.write(queue.used + 4 + slot * 8, &entry);
        queue.used_idx = queue.used_idx.wrapping_add(1);
        self.write(queue.used + 2, &queue.used_idx.to_le_bytes());
    }

    /// Answer every request the driver made available.
    fn serve_requests(&self, state: &mut State) {
        if state.behaviour == Behaviour::Silent {
            return;
        }
        let mut queue = state.queues[0].clone();
        let available = self.read_u16(queue.avail + 2);
        while queue.next != available {
            let head = self.read_u16(queue.avail + 4 + u64::from(queue.next % queue.size) * 2);
            queue.next = queue.next.wrapping_add(1);
            let (request, buffers) = self.chain(&queue, head);
            let reply = answer(state, &request);
            let mut written = 0;
            let mut rest = reply.as_slice();
            for (address, len) in buffers {
                let take = rest.len().min(len as usize);
                self.write(address, &rest[..take]);
                rest = &rest[take..];
                written += take;
            }
            let named = if state.behaviour == Behaviour::MisnamesChains {
                head ^ 1
            } else {
                head
            };
            self.publish(&mut queue, named, u32::try_from(written).unwrap());
        }
        state.queues[0] = queue;
    }

    /// Write `report` into the next buffer the driver made available, or
    /// keep it until one is; raise the interrupt unless asked not to.
    fn report(&self, state: &mut State, report: [u8; 24]) {
        state.pending.push_back(report);
        self.deliver(state);
    }

    fn deliver(&self, state: &mut State) {
        let mut queue = state.queues[1].clone();
        if queue.size == 0 {
            return;
        }
        let available = self.read_u16(queue.avail + 2);
        let mut delivered = false;
        while let Some(report) = state.pending.front().copied() {
            if queue.next == available {
                break;
            }
            let head = self.read_u16(queue.avail + 4 + u64::from(queue.next % queue.size) * 2);
            queue.next = queue.next.wrapping_add(1);
            let (_, buffers) = self.chain(&queue, head);
            let written = match buffers.first() {
                Some(&(address, len)) if len >= 24 => {
                    self.write(address, &report);
                    24
                }
                _ => 0,
            };
            self.publish(&mut queue, head, written);
            state.pending.pop_front();
            delivered = true;
        }
        state.queues[1] = queue.clone();
        // Reports past the buffers the driver gave are dropped, as a device
        // with no buffer must.
        state.dropped += state.pending.len();
        state.pending.clear();
        let quiet = self.read_u16(queue.avail) & VIRTQ_AVAIL_F_NO_INTERRUPT != 0;
        if delivered && !quiet {
            state.raised = true;
        }
    }

    fn fault(&self, state: &mut State, reason: u8, endpoint: u32, iova: u64, write: bool) {
        let mut report = [0; 24];
        report[0] = reason;
        let flags: u32 = (1 << 8) | if write { 1 << 1 } else { 1 << 0 };
        report[4..8].copy_from_slice(&flags.to_le_bytes());
        report[8..12].copy_from_slice(&endpoint.to_le_bytes());
        report[16..24].copy_from_slice(&iova.to_le_bytes());
        self.report(state, report);
    }

    fn bypasses(state: &State) -> bool {
        if state.accepted & feature::BYPASS_CONFIG != 0 {
            state.bypass & 1 != 0
        } else {
            state.accepted & feature::BYPASS != 0
        }
    }
}

/// The device's reply to `request`: its properties, if a probe, then its
/// tail.
fn answer(state: &mut State, request: &[u8]) -> Vec<u8> {
    let u32_at = |at: usize| u32::from_le_bytes(request[at..at + 4].try_into().unwrap());
    let u64_at = |at: usize| u64::from_le_bytes(request[at..at + 8].try_into().unwrap());
    let kind = request[0];
    *state.answered.entry(kind).or_default() += 1;
    let mut reply = Vec::new();
    let status = match kind {
        1 => attach(state, u32_at(4), u32_at(8), u32_at(12) | u32_at(16)),
        2 => detach(state, u32_at(4), u32_at(8)),
        3 => map(
            state,
            u32_at(4),
            u64_at(8),
            u64_at(16),
            u64_at(24),
            u32_at(32),
        ),
        4 => unmap(state, u32_at(4), u64_at(8), u64_at(16)),
        5 => {
            let mut properties = alloc::vec![0; state.probe_size as usize];
            let status = probe(state, u32_at(4), &mut properties);
            reply.extend_from_slice(&properties);
            status
        }
        _ => 2,
    };
    reply.extend_from_slice(&[status, 0, 0, 0]);
    reply
}

fn attach(state: &mut State, domain: u32, endpoint: u32, reserved: u32) -> u8 {
    if reserved != 0 {
        return S_INVAL;
    }
    let Some(&current) = state.endpoints.get(&endpoint) else {
        return S_NOENT;
    };
    if let Some(old) = current.filter(|&old| old != domain) {
        let _ = detach(state, old, endpoint);
    }
    state
        .domains
        .entry(domain)
        .or_default()
        .endpoints
        .insert(endpoint);
    state.endpoints.insert(endpoint, Some(domain));
    S_OK
}

fn detach(state: &mut State, domain: u32, endpoint: u32) -> u8 {
    if !state.endpoints.contains_key(&endpoint) {
        return S_NOENT;
    }
    if state.endpoints.get(&endpoint) != Some(&Some(domain)) {
        return S_INVAL;
    }
    state.endpoints.insert(endpoint, None);
    let keeps = matches!(
        state.behaviour,
        Behaviour::KeepsDomains | Behaviour::RefusesUnmaps
    );
    if let Some(held) = state.domains.get_mut(&domain) {
        held.endpoints.remove(&endpoint);
        if held.endpoints.is_empty() && !keeps {
            state.domains.remove(&domain);
        }
    }
    S_OK
}

fn map(state: &mut State, domain: u32, first: u64, last: u64, phys: u64, flags: u32) -> u8 {
    if state.behaviour == Behaviour::RefusesMaps || state.maps_left == Some(0) {
        return S_DEVERR;
    }
    let granule = 1u64 << state.page_sizes.trailing_zeros();
    if flags & !0b111 != 0 {
        return S_INVAL;
    }
    if last <= first
        || !first.is_multiple_of(granule)
        || !phys.is_multiple_of(granule)
        || !last.wrapping_add(1).is_multiple_of(granule)
    {
        return S_RANGE;
    }
    let reserved = state.reserved.clone();
    let Some(held) = state.domains.get_mut(&domain) else {
        return S_NOENT;
    };
    let overlaps = held
        .maps
        .range(..=last)
        .next_back()
        .is_some_and(|(_, &(end, _, _))| end >= first);
    let claimed = held.endpoints.iter().any(|endpoint| {
        reserved.get(endpoint).is_some_and(|regions| {
            regions
                .iter()
                .any(|region| region.first <= last && first <= region.last)
        })
    });
    if overlaps || claimed {
        return S_INVAL;
    }
    held.maps.insert(first, (last, phys, flags));
    if let Some(left) = state.maps_left.as_mut() {
        *left -= 1;
    }
    S_OK
}

fn unmap(state: &mut State, domain: u32, first: u64, last: u64) -> u8 {
    if state.behaviour == Behaviour::RefusesUnmaps {
        return S_DEVERR;
    }
    let Some(held) = state.domains.get_mut(&domain) else {
        return S_NOENT;
    };
    let affected: Vec<u64> = held
        .maps
        .iter()
        .filter(|(&start, &(end, _, _))| start <= last && first <= end)
        .map(|(&start, _)| start)
        .collect();
    if affected.iter().any(|start| {
        let (end, _, _) = held.maps[start];
        *start < first || end > last
    }) {
        return S_RANGE;
    }
    for start in affected {
        held.maps.remove(&start);
    }
    S_OK
}

fn probe(state: &State, endpoint: u32, properties: &mut [u8]) -> u8 {
    if !state.endpoints.contains_key(&endpoint) {
        return S_NOENT;
    }
    let mut at = 0;
    for region in state.reserved.get(&endpoint).into_iter().flatten() {
        let Some(slot) = properties.get_mut(at..at + 24) else {
            return S_INVAL;
        };
        slot[..2].copy_from_slice(&1u16.to_le_bytes());
        slot[2..4].copy_from_slice(&20u16.to_le_bytes());
        slot[4] = region.subtype;
        slot[8..16].copy_from_slice(&region.first.to_le_bytes());
        slot[16..24].copy_from_slice(&region.last.to_le_bytes());
        at += 24;
    }
    S_OK
}

/// The model as the transport the family drives it through.
pub(crate) struct ModelTransport<'d, 'f> {
    device: &'d Device<'f>,
}

impl Transport for ModelTransport<'_, '_> {
    fn reset(&mut self) -> Result<(), VirtioError> {
        let mut state = self.device.state.lock();
        state.status = 0;
        state.accepted = 0;
        state.queues = [Queue::default(), Queue::default()];
        state.domains.clear();
        for domain in state.endpoints.values_mut() {
            *domain = None;
        }
        state.raised = false;
        Ok(())
    }

    fn status(&self) -> Status {
        Status::from_bits(self.device.state.lock().status)
    }

    fn set_status(&mut self, status: Status) {
        let mut state = self.device.state.lock();
        let mut bits = status.bits();
        // Features the device never offered are not ones it can be OK with.
        if bits & Status::FEATURES_OK != 0 && state.accepted & !state.offered != 0 {
            bits &= !Status::FEATURES_OK;
        }
        state.status = bits;
    }

    fn device_features(&self) -> u64 {
        self.device.state.lock().offered
    }

    fn set_driver_features(&mut self, features: u64) {
        self.device.state.lock().accepted = features;
    }

    fn num_queues(&self) -> u16 {
        2
    }

    fn queue_select(&mut self, queue: u16) -> Result<(), VirtioError> {
        if queue >= 2 {
            return Err(VirtioError::QueueIndexOutOfRange);
        }
        self.device.state.lock().selected = queue;
        Ok(())
    }

    fn queue_max_size(&self) -> u16 {
        let state = self.device.state.lock();
        state.queue_max[usize::from(state.selected)]
    }

    fn queue_set(
        &mut self,
        size: u16,
        desc: u64,
        avail: u64,
        used: u64,
    ) -> Result<(), VirtioError> {
        let mut state = self.device.state.lock();
        let selected = usize::from(state.selected);
        if size == 0 || size > state.queue_max[selected] {
            return Err(VirtioError::QueueSizeTooLarge);
        }
        state.queues[selected] = Queue {
            size,
            desc,
            avail,
            used,
            next: 0,
            used_idx: 0,
        };
        Ok(())
    }

    fn notify(&mut self, queue: u16) {
        let mut state = self.device.state.lock();
        if state.status & Status::DRIVER_OK == 0 {
            return;
        }
        match queue {
            0 => self.device.serve_requests(&mut state),
            _ => self.device.deliver(&mut state),
        }
    }

    fn config_len(&self) -> usize {
        self.device.state.lock().config_len
    }

    fn read_config(&self, offset: usize, buf: &mut [u8]) {
        let state = self.device.state.lock();
        let mut bytes = [0u8; config::LEN];
        bytes[config::PAGE_SIZE_MASK..][..8].copy_from_slice(&state.page_sizes.to_le_bytes());
        bytes[config::INPUT_START..][..8].copy_from_slice(&state.input.0.to_le_bytes());
        bytes[config::INPUT_END..][..8].copy_from_slice(&state.input.1.to_le_bytes());
        bytes[config::DOMAIN_START..][..4].copy_from_slice(&state.domain_range.0.to_le_bytes());
        bytes[config::DOMAIN_END..][..4].copy_from_slice(&state.domain_range.1.to_le_bytes());
        bytes[config::PROBE_SIZE..][..4].copy_from_slice(&state.probe_size.to_le_bytes());
        bytes[config::BYPASS] = state.bypass;
        let exposed = &bytes[..state.config_len.min(config::LEN)];
        for (index, byte) in buf.iter_mut().enumerate() {
            *byte = exposed.get(offset + index).copied().unwrap_or(0);
        }
    }

    fn write_config(&mut self, offset: usize, data: &[u8]) {
        let mut state = self.device.state.lock();
        if offset == config::BYPASS
            && offset < state.config_len
            && state.accepted & feature::BYPASS_CONFIG != 0
            && state.behaviour != Behaviour::BypassStuck
        {
            if let Some(&value) = data.first() {
                state.bypass = value & 1;
            }
        }
    }

    fn ack_interrupt(&mut self) {
        self.device.state.lock().raised = false;
    }
}

impl TranslationProbe for Device<'_> {
    fn access(&self, stream: u32, iova: u64, write: bool) -> Option<u64> {
        let mut state = self.state.lock();
        let page = iova & !0xFFF;
        let Some(&endpoint) = state.endpoints.get(&stream) else {
            if Self::bypasses(&state) {
                return Some(iova);
            }
            self.fault(&mut state, 0, stream, page, write);
            return None;
        };
        let Some(domain) = endpoint else {
            if Self::bypasses(&state) {
                return Some(iova);
            }
            self.fault(&mut state, 1, stream, iova, write);
            return None;
        };
        let hit = state.domains.get(&domain).and_then(|held| {
            held.maps
                .range(..=iova)
                .next_back()
                .filter(|(_, &(last, _, _))| iova <= last)
                .map(|(&first, &(_, phys, flags))| (phys + (iova - first), flags))
        });
        match hit {
            Some((phys, flags)) if flags & if write { 0b10 } else { 0b01 } != 0 => Some(phys),
            _ => {
                self.fault(&mut state, 2, stream, iova, write);
                None
            }
        }
    }

    fn translated(&self, _stream: u32, _address: u64, _write: bool) -> Option<u64> {
        None
    }

    fn carries_translated(&self) -> bool {
        false
    }
}
