//! A reference unit over the generic engine, with a modelled translation
//! cache: an access that walked the tables once keeps hitting the cache until
//! a sync (or a block) removes the entry. It proves the domain layer, it
//! proves the conformance suite fails a unit that forgets to invalidate, and
//! it stands in for hardware wherever the kernel's use of a unit is tested.
//! Like hardware, it translates nothing until enabled.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec::Vec;

use tairix_sync::SpinLock;

use crate::conformance::{InterruptProbe, TranslationProbe};
use crate::hostmem::HostFrames;
use crate::pagetable::{IoPageTable, Pte, PteFormat};
use crate::{
    Access, DomainId, Fault, FaultReason, FaultRoute, InterruptRemapping, InterruptSource,
    InterruptTarget, IommuError, IommuUnit, MessageFiles, Notice, Reach, Remapped, TableMemory,
    UnitProfile, IO_PAGE_SIZE,
};

const ADDRESS: u64 = 0x000F_FFFF_FFFF_F000;
pub(crate) const REACH: Reach = Reach {
    input_bits: 39,
    output_bits: 46,
};
const LARGE: u64 = 1 << 7;
const READ_WRITE: u64 = 0b11;

pub(crate) struct ModelFormat;

impl PteFormat for ModelFormat {
    fn leaf_allowed(&self, level: u32) -> bool {
        level <= 1
    }

    fn table(&self, phys: u64, _level: u32) -> u64 {
        phys | READ_WRITE
    }

    fn leaf(&self, phys: u64, level: u32, access: Access) -> u64 {
        let size = if level > 0 { LARGE } else { 0 };
        phys | u64::from(access.read()) | (u64::from(access.write()) << 1) | size
    }

    fn decode(&self, entry: u64, level: u32) -> Pte {
        if entry & READ_WRITE == 0 {
            Pte::Absent
        } else if level == 0 || entry & LARGE != 0 {
            let access = match entry & READ_WRITE {
                0b01 => Access::READ,
                0b10 => Access::WRITE,
                _ => Access::READ_WRITE,
            };
            Pte::Leaf(entry & ADDRESS, access)
        } else {
            Pte::Table(entry & ADDRESS)
        }
    }
}

/// How the model behaves: correctly, or wrong in one way, to prove a check
/// notices.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum Behaviour {
    /// As a unit must.
    #[default]
    Correct,
    /// `sync` succeeds but leaves the cache as it was.
    StaleSync,
    /// `sync` answers that the unit did not confirm.
    UnconfirmedSync,
    /// `block` answers that the unit did not confirm.
    UnconfirmedBlock,
    /// `attach` translates the stream but answers that the unit did not
    /// confirm.
    UnconfirmedAttach,
    /// `enable` refuses.
    RefusesEnable,
    /// `route_faults` refuses.
    RefusesRoute,
    /// `unroute_faults` refuses.
    RefusesUnroute,
    /// `release_interrupt` leaves the copy an interrupt cached.
    StaleRelease,
    /// `enable_remapping` refuses.
    RefusesRemapping,
    /// Every drain says records remain.
    EndlessFaults,
    /// `silence` refuses.
    RefusesSilence,
    /// Its tables grant no write without a read, as a stage 1 `SMMUv3`'s do
    /// not: `map` refuses [`Access::WRITE`] alone.
    NoWriteOnly,
    /// It confines streams' messages to interrupt files, taking a confined
    /// stream's write to its doorbell as a message rather than translating it.
    ConfinesMessages,
    /// It has no endpoint for a stream at or past this one: `attach` answers
    /// [`IommuError::NoEndpoint`].
    NoEndpointFrom(u32),
}

struct State<'f> {
    domains: BTreeMap<u32, IoPageTable<'f, ModelFormat>>,
    next_domain: u32,
    streams: BTreeMap<u32, u32>,
    /// Streams blocked without their faults recorded.
    silenced: BTreeSet<u32>,
    /// Cached translations, keyed by domain and page.
    cache: BTreeMap<(u32, u64), (u64, Access)>,
    faults: Vec<Fault>,
    enabled: bool,
    behaviour: Behaviour,
    routed: Option<FaultRoute>,
    remap: Option<Remap>,
    /// Each confined stream's doorbell, file and notice.
    confined: BTreeMap<u32, (u64, u64, Notice)>,
    /// Invalidations asked of it.
    syncs: usize,
}

/// The reference unit's remapping table: its entries, the copies an
/// interrupt cached, and whether it refuses compatibility interrupts yet.
struct Remap {
    extended: bool,
    entries: Vec<Option<(InterruptSource, InterruptTarget)>>,
    cached: BTreeMap<u32, (InterruptSource, InterruptTarget)>,
    enabled: bool,
}

/// The reference unit's remappable MSI address for `entry`: a format bit and
/// the entry, as VT-d lays them out.
fn remap_address(entry: u32) -> u64 {
    crate::MESSAGE_WINDOW.start | (u64::from(entry) << 5) | (1 << 4)
}

/// The reference unit.
pub struct ModelUnit<'f> {
    frames: &'f HostFrames,
    state: SpinLock<State<'f>>,
}

impl<'f> ModelUnit<'f> {
    /// A disabled unit drawing its tables from `frames`, behaving as
    /// `behaviour` says.
    #[must_use]
    pub const fn new(frames: &'f HostFrames, behaviour: Behaviour) -> Self {
        Self {
            frames,
            state: SpinLock::new(State {
                domains: BTreeMap::new(),
                next_domain: 1,
                streams: BTreeMap::new(),
                silenced: BTreeSet::new(),
                cache: BTreeMap::new(),
                faults: Vec::new(),
                enabled: false,
                behaviour,
                routed: None,
                remap: None,
                confined: BTreeMap::new(),
                syncs: 0,
            }),
        }
    }

    /// Behave as `behaviour` says from now on.
    pub fn behave(&self, behaviour: Behaviour) {
        self.state.lock().behaviour = behaviour;
    }

    /// How the unit's fault interrupt was routed, if it was.
    #[must_use]
    pub fn routed(&self) -> Option<FaultRoute> {
        self.state.lock().routed
    }

    /// Whether `stream` is silenced.
    #[must_use]
    pub fn silenced(&self, stream: u32) -> bool {
        self.state.lock().silenced.contains(&stream)
    }

    /// Live domains.
    #[must_use]
    pub fn domains(&self) -> usize {
        self.state.lock().domains.len()
    }

    /// Whether the unit has been enabled.
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.state.lock().enabled
    }

    /// Whether the unit refuses interrupts its remapping table does not
    /// name.
    #[must_use]
    pub fn remapping(&self) -> bool {
        self.state
            .lock()
            .remap
            .as_ref()
            .is_some_and(|remap| remap.enabled)
    }

    /// Invalidations asked of the unit, confirmed or not.
    #[must_use]
    pub fn syncs(&self) -> usize {
        self.state.lock().syncs
    }

    /// The doorbell, file and notice `stream`'s messages were confined to.
    #[must_use]
    pub fn confined(&self, stream: u32) -> Option<(u64, u64, Notice)> {
        self.state.lock().confined.get(&stream).copied()
    }

    /// The domain `stream` is attached to, if any.
    #[must_use]
    pub fn attached(&self, stream: u32) -> Option<DomainId> {
        self.state
            .lock()
            .streams
            .get(&stream)
            .copied()
            .map(DomainId)
    }
}

impl IommuUnit for ModelUnit<'_> {
    fn profile(&self) -> UnitProfile {
        UnitProfile {
            tables: crate::Tables::Walked(crate::Stage::Second),
            reach: REACH,
            reserved: core::slice::from_ref(&crate::MESSAGE_WINDOW),
            write_only: self.state.lock().behaviour != Behaviour::NoWriteOnly,
        }
    }

    fn enable(&self) -> Result<(), IommuError> {
        let mut state = self.state.lock();
        if state.behaviour == Behaviour::RefusesEnable {
            return Err(IommuError::Hardware);
        }
        state.enabled = true;
        Ok(())
    }

    fn create_domain(&self) -> Result<DomainId, IommuError> {
        let table = IoPageTable::new(ModelFormat, 3, TableMemory::new(self.frames, None), REACH)?;
        let mut state = self.state.lock();
        let id = state.next_domain;
        state.next_domain += 1;
        state.domains.insert(id, table);
        Ok(DomainId(id))
    }

    fn destroy_domain(&self, domain: DomainId) -> Result<(), IommuError> {
        let mut state = self.state.lock();
        if state.streams.values().any(|&d| d == domain.0) {
            return Err(IommuError::DomainBusy);
        }
        state
            .domains
            .remove(&domain.0)
            .ok_or(IommuError::OutOfRange)?;
        state.cache.retain(|&(d, _), _| d != domain.0);
        Ok(())
    }

    fn attach(&self, stream: u32, domain: DomainId) -> Result<(), IommuError> {
        let mut state = self.state.lock();
        if !state.domains.contains_key(&domain.0) {
            return Err(IommuError::OutOfRange);
        }
        if matches!(state.behaviour, Behaviour::NoEndpointFrom(first) if stream >= first) {
            return Err(IommuError::NoEndpoint);
        }
        match state.streams.get(&stream) {
            Some(&held) if held == domain.0 => return Ok(()),
            Some(_) => return Err(IommuError::StreamBusy),
            None => {}
        }
        state.silenced.remove(&stream);
        state.streams.insert(stream, domain.0);
        if state.behaviour == Behaviour::UnconfirmedAttach {
            return Err(IommuError::Unconfirmed);
        }
        Ok(())
    }

    fn block(&self, stream: u32) -> Result<(), IommuError> {
        let mut state = self.state.lock();
        if state.behaviour == Behaviour::UnconfirmedBlock {
            return Err(IommuError::Unconfirmed);
        }
        // Only an attach ends silence.
        state.streams.remove(&stream);
        Ok(())
    }

    fn silence(&self, stream: u32) -> Result<(), IommuError> {
        let mut state = self.state.lock();
        if state.behaviour == Behaviour::RefusesSilence {
            return Err(IommuError::Hardware);
        }
        state.streams.remove(&stream);
        state.silenced.insert(stream);
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
        let mut state = self.state.lock();
        if state.behaviour == Behaviour::NoWriteOnly && !access.read() {
            return Err(IommuError::OutOfRange);
        }
        let table = state
            .domains
            .get_mut(&domain.0)
            .ok_or(IommuError::OutOfRange)?;
        table.map(iova, phys, len, access)
    }

    fn unmap(&self, domain: DomainId, iova: u64, len: u64) -> Result<(), IommuError> {
        let mut state = self.state.lock();
        let table = state
            .domains
            .get_mut(&domain.0)
            .ok_or(IommuError::OutOfRange)?;
        table.unmap(iova, len)
    }

    fn sync(&self, domain: DomainId) -> Result<(), IommuError> {
        let mut state = self.state.lock();
        state.syncs += 1;
        match state.behaviour {
            Behaviour::UnconfirmedSync => return Err(IommuError::Unconfirmed),
            Behaviour::StaleSync => {}
            _ => state.cache.retain(|&(d, _), _| d != domain.0),
        }
        let table = state
            .domains
            .get_mut(&domain.0)
            .ok_or(IommuError::OutOfRange)?;
        table.release_retired();
        Ok(())
    }

    fn route_faults(&self, route: FaultRoute) -> Result<(), IommuError> {
        let mut state = self.state.lock();
        if state.behaviour == Behaviour::RefusesRoute {
            return Err(IommuError::Hardware);
        }
        state.routed = Some(route);
        Ok(())
    }

    fn unroute_faults(&self) -> Result<(), IommuError> {
        let mut state = self.state.lock();
        if state.behaviour == Behaviour::RefusesUnroute {
            return Err(IommuError::Hardware);
        }
        state.routed = None;
        Ok(())
    }

    fn drain_faults(&self, sink: &mut dyn FnMut(Fault)) -> bool {
        let (faults, endless) = {
            let mut state = self.state.lock();
            (
                core::mem::take(&mut state.faults),
                state.behaviour == Behaviour::EndlessFaults,
            )
        };
        for fault in faults {
            sink(fault);
        }
        endless
    }

    fn interrupt_remapping(&self) -> Option<&dyn InterruptRemapping> {
        Some(self)
    }

    fn message_files(&self) -> Option<&dyn MessageFiles> {
        (self.state.lock().behaviour == Behaviour::ConfinesMessages)
            .then_some(self as &dyn MessageFiles)
    }
}

impl MessageFiles for ModelUnit<'_> {
    fn atomic_files(&self) -> bool {
        true
    }

    fn confine_messages(
        &self,
        stream: u32,
        doorbell: u64,
        file: u64,
        notice: Notice,
    ) -> Result<(), IommuError> {
        let mut state = self.state.lock();
        if !file.is_multiple_of(crate::MESSAGE_FILE_BYTES)
            || !doorbell.is_multiple_of(IO_PAGE_SIZE)
            || state.confined.contains_key(&stream)
        {
            return Err(IommuError::OutOfRange);
        }
        state.confined.insert(stream, (doorbell, file, notice));
        Ok(())
    }
}

impl InterruptRemapping for ModelUnit<'_> {
    fn supports_extended(&self) -> bool {
        true
    }

    fn prepare_remapping(&self, extended: bool, entries: u32) -> Result<(), IommuError> {
        let mut state = self.state.lock();
        if state.remap.is_some() {
            return Err(IommuError::OutOfRange);
        }
        let mut table = Vec::new();
        table.resize(entries.clamp(16, 1 << 15) as usize, None);
        state.remap = Some(Remap {
            extended,
            entries: table,
            cached: BTreeMap::new(),
            enabled: false,
        });
        Ok(())
    }

    fn remap_interrupt(
        &self,
        source: InterruptSource,
        target: InterruptTarget,
    ) -> Result<Remapped, IommuError> {
        let mut state = self.state.lock();
        let remap = state.remap.as_mut().ok_or(IommuError::OutOfRange)?;
        if !remap.extended && target.destination > 0xFF {
            return Err(IommuError::OutOfRange);
        }
        let entry = remap
            .entries
            .iter()
            .position(Option::is_none)
            .ok_or(IommuError::Exhausted)?;
        remap.entries[entry] = Some((source, target));
        let entry = u32::try_from(entry).map_err(|_| IommuError::Exhausted)?;
        Ok(Remapped {
            entry,
            address: remap_address(entry),
            data: 0,
            redirection: (u64::from(entry) << 49) | (1 << 48) | u64::from(target.vector),
        })
    }

    fn release_interrupt(&self, entry: u32) -> Result<(), IommuError> {
        let mut state = self.state.lock();
        let remap = state.remap.as_mut().ok_or(IommuError::NotMapped)?;
        let slot = remap
            .entries
            .get_mut(entry as usize)
            .filter(|slot| slot.is_some())
            .ok_or(IommuError::NotMapped)?;
        *slot = None;
        if state.behaviour != Behaviour::StaleRelease {
            if let Some(remap) = state.remap.as_mut() {
                remap.cached.remove(&entry);
            }
        }
        Ok(())
    }

    fn enable_remapping(&self) -> Result<(), IommuError> {
        let mut state = self.state.lock();
        if state.behaviour == Behaviour::RefusesRemapping {
            return Err(IommuError::Hardware);
        }
        let remap = state.remap.as_mut().ok_or(IommuError::OutOfRange)?;
        remap.enabled = true;
        Ok(())
    }

    fn disable_remapping(&self) -> Result<(), IommuError> {
        let mut state = self.state.lock();
        let remap = state.remap.as_mut().ok_or(IommuError::OutOfRange)?;
        remap.enabled = false;
        Ok(())
    }
}

impl InterruptProbe for ModelUnit<'_> {
    fn interrupt(&self, source: u16, address: u64, data: u32) -> Option<InterruptTarget> {
        let mut state = self.state.lock();
        // A compatibility message names its APIC id in address bits 19:12.
        let compatibility = InterruptTarget {
            vector: data.to_le_bytes()[0],
            destination: u32::try_from((address >> 12) & 0xFF).unwrap_or(0),
            level: false,
        };
        let Some(remap) = state.remap.as_mut().filter(|remap| remap.enabled) else {
            return Some(compatibility);
        };
        let entry = u32::try_from((address >> 5) & 0x7FFF).ok()?;
        let named = (address & (1 << 4) != 0)
            .then(|| {
                remap
                    .cached
                    .get(&entry)
                    .copied()
                    .or_else(|| remap.entries.get(entry as usize).copied().flatten())
            })
            .flatten();
        let admitted = named.filter(|&(admits, _)| match admits {
            InterruptSource::Requester(id) => id == source,
            InterruptSource::Buses { first, last } => {
                (first..=last).contains(&source.to_be_bytes()[0])
            }
        });
        if let Some(found) = admitted {
            remap.cached.insert(entry, found);
            return Some(found.1);
        }
        state.faults.push(Fault {
            stream: u32::from(source),
            iova: u64::from(entry),
            write: true,
            reason: FaultReason::Interrupt,
        });
        None
    }
}

impl TranslationProbe for ModelUnit<'_> {
    fn access(&self, stream: u32, iova: u64, write: bool) -> Option<u64> {
        let mut state = self.state.lock();
        if !state.enabled {
            return Some(iova);
        }
        let page = iova & !(IO_PAGE_SIZE - 1);
        let offset = iova - page;
        let refuse = |state: &mut State<'_>, reason| {
            state.faults.push(Fault {
                stream,
                iova: page,
                write,
                reason,
            });
            None
        };
        if state.silenced.contains(&stream) {
            return None;
        }
        let message = state
            .confined
            .get(&stream)
            .is_some_and(|&(doorbell, _, _)| doorbell == page);
        if write && message && state.streams.contains_key(&stream) {
            return None;
        }
        let Some(&domain) = state.streams.get(&stream) else {
            return refuse(&mut state, FaultReason::Blocked);
        };
        let hit = state.cache.get(&(domain, page)).copied();
        let translated = hit.or_else(|| {
            state
                .domains
                .get(&domain)
                .and_then(|table| table.translate(page))
        });
        match translated {
            Some((phys, access)) if (write && access.write()) || (!write && access.read()) => {
                state.cache.insert((domain, page), (phys, access));
                Some(phys + offset)
            }
            Some(_) => refuse(&mut state, FaultReason::Denied),
            None => refuse(&mut state, FaultReason::Unmapped),
        }
    }

    fn translated(&self, stream: u32, address: u64, write: bool) -> Option<u64> {
        let mut state = self.state.lock();
        if !state.enabled {
            return Some(address);
        }
        if !state.silenced.contains(&stream) {
            state.faults.push(Fault {
                stream,
                iova: address & !(IO_PAGE_SIZE - 1),
                write,
                reason: FaultReason::Translated,
            });
        }
        None
    }
}
