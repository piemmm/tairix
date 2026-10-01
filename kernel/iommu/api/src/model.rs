//! A reference unit over the generic engine, with a modelled translation
//! cache: an access that walked the tables once keeps hitting the cache until
//! a sync (or a block) removes the entry. It proves the domain layer, it
//! proves the conformance suite fails a unit that forgets to invalidate, and
//! it stands in for hardware wherever the kernel's use of a unit is tested.
//! Like hardware, it translates nothing until enabled.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec::Vec;

use tairix_sync::SpinLock;

use crate::conformance::TranslationProbe;
use crate::hostmem::HostFrames;
use crate::pagetable::{IoPageTable, Pte, PteFormat};
use crate::{
    Access, DomainId, Fault, FaultReason, IommuError, IommuUnit, TableMemory, UnitProfile,
    IO_PAGE_SIZE,
};

const ADDRESS: u64 = 0x000F_FFFF_FFFF_F000;
const LARGE: u64 = 1 << 7;
const READ_WRITE: u64 = 0b11;

/// The x86 interrupt window, as a reserved-range fixture.
static INTERRUPT_WINDOW: core::ops::Range<u64> = 0xFEE0_0000..0xFEF0_0000;

struct ModelFormat;

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
    /// `enable` refuses.
    RefusesEnable,
    /// `route_faults` refuses.
    RefusesRoute,
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
    routed: Option<(u64, u32)>,
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
            }),
        }
    }

    /// Behave as `behaviour` says from now on.
    pub fn behave(&self, behaviour: Behaviour) {
        self.state.lock().behaviour = behaviour;
    }

    /// The message the unit's fault interrupt was routed to, if any.
    #[must_use]
    pub fn routed(&self) -> Option<(u64, u32)> {
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
            input_bits: 39,
            output_bits: 46,
            reserved: core::slice::from_ref(&INTERRUPT_WINDOW),
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
        let table = IoPageTable::new(ModelFormat, 3, TableMemory::new(self.frames, None))?;
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
        if state.streams.contains_key(&stream) {
            return Err(IommuError::StreamBusy);
        }
        state.silenced.remove(&stream);
        state.streams.insert(stream, domain.0);
        Ok(())
    }

    fn block(&self, stream: u32) -> Result<(), IommuError> {
        let mut state = self.state.lock();
        if state.behaviour == Behaviour::UnconfirmedBlock {
            return Err(IommuError::Unconfirmed);
        }
        state.streams.remove(&stream);
        state.silenced.remove(&stream);
        Ok(())
    }

    fn silence(&self, stream: u32) -> Result<(), IommuError> {
        let mut state = self.state.lock();
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

    fn route_faults(&self, address: u64, data: u32) -> Result<(), IommuError> {
        let mut state = self.state.lock();
        if state.behaviour == Behaviour::RefusesRoute {
            return Err(IommuError::Hardware);
        }
        state.routed = Some((address, data));
        Ok(())
    }

    fn drain_faults(&self, sink: &mut dyn FnMut(Fault)) -> bool {
        let faults = core::mem::take(&mut self.state.lock().faults);
        for fault in faults {
            sink(fault);
        }
        false
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
}
