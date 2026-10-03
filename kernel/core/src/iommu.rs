//! DMA translation: every unit discovery reported that a family here can
//! drive, and the domain each translated node's owner maps its carves into
//! (`plans/IOMMU.md`).
//!
//! A node is translated when its tree entry names an [`IommuStreams`] on a
//! unit brought up here. Its owner's first carve creates a domain holding the
//! node's streams and the firmware windows they keep; each carve maps there;
//! the owner's end revokes it — streams blocked, the unit's caches confirmed
//! clean — after which every carve of that generation is unreachable and is
//! freed at once rather than quarantined. A stream no owner holds keeps only
//! its firmware windows, in a firmware domain of its own.
//!
//! A node's function masters DMA only while an owner holds its domain: bus
//! mastering is granted once the domain is attached and withdrawn before it
//! is destroyed, except for a stream firmware keeps a window for, which
//! firmware masters again (`mastering`).
//!
//! An end the unit cannot confirm proves nothing about what the device can
//! still reach, and nothing later can: the owner stays recorded for good, its
//! carves are never reused, and the node takes no successor. The facility is
//! the custody of translated carves for the same reason — a block reaches it
//! only when its unit could not confirm the device lost it, so it keeps the
//! frames for good.
//!
//! Locks are taken in the order owners → owner state → firmware → unit, and
//! neither facility-wide lock is held across a wait on a unit: an adoption
//! waits under its own owner's state alone, which it holds from before anyone
//! else can reach the owner, so carves for other nodes go on meanwhile.

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ops::{Range, RangeInclusive};
use core::ptr::NonNull;
use core::sync::atomic::{AtomicBool, Ordering};

use tairix_abi::driver::DriverBindKey;
use tairix_abi::{
    HwMatchKey, HwNode, HwResourceKind, IommuReservedWindow, IommuStreams, RegisterWindow,
};
use tairix_arch_api::PageTableFrames;
use tairix_collections::HashMap;
use tairix_devmatch::{DriverCandidate, MatchResolution};
use tairix_hash::BuildFastHash;
use tairix_kernel_iommu_api::{Clock, Domain, IommuError, IommuUnit, TableCoherence};
use tairix_kernel_mem::{AllocError, DeviceTranslation, DmaBlock, DmaCustody, DmaError};
use tairix_log::{Field, FieldValue, Level, Sink};
use tairix_sync::SpinLock;

use crate::audit::{emit, AuditEvent};
use crate::hwtree::HwTreeSource;

mod faults;
mod mastering;

pub use faults::{FaultEnv, FAULT_LIMITS, FAULT_OWNER};
pub use mastering::{BusMastering, MasterChange, MasterTarget, Mastering};

/// The generation the kernel's own bootstrap-floor drivers carve as. Every
/// user driver's is later, and none can take a node from the kernel.
pub const KERNEL_OWNER: u64 = 0;

/// A unit its family has taken over, blocked and not yet translating.
pub struct Unit {
    /// Its hardware-tree node.
    pub node: u32,
    /// Its family's driver.
    pub unit: &'static dyn IommuUnit,
    /// The firmware reserved windows it keeps.
    pub reserved: Vec<IommuReservedWindow>,
}

/// What a family needs to take a unit over.
pub struct UnitEnv<'a> {
    /// Maps a register window uncached for the kernel.
    pub mmio: &'a dyn Fn(u64, usize) -> Option<NonNull<u8>>,
    /// Where the unit's tables come from.
    pub frames: &'static dyn PageTableFrames,
    /// Table write-back for a walker that does not snoop, where the port has
    /// one.
    pub coherence: Option<&'static dyn TableCoherence>,
    /// The clock a family bounds its waits against.
    pub clock: &'static dyn Clock,
}

/// Why a unit discovery reported is not translating.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Refusal {
    /// No family here drives it.
    Unmatched,
    /// Its node names no register window, or one the port cannot map.
    NoRegisters,
    /// Its family could not take it over or enable it.
    Unit(IommuError),
}

/// Every register window a unit's node names: what no process may map,
/// whether or not a family brings the unit up.
pub fn register_windows(node: &HwNode) -> impl Iterator<Item = Range<u64>> + '_ {
    node.resources()
        .iter()
        .filter(|r| r.kind() == Some(HwResourceKind::Mmio))
        .map(|r| r.base()..r.base().saturating_add(r.length()))
}

/// Take over the unit at `node` with the family whose bind table matches it.
///
/// # Errors
///
/// The [`Refusal`] saying why the unit stays untranslated.
pub fn take_over(node: &HwNode, env: &UnitEnv<'_>) -> Result<Unit, Refusal> {
    let vtd = HwMatchKey::compatible(tairix_kernel_iommu_vtd::COMPATIBLE)
        .map_err(|_| Refusal::Unmatched)?;
    let families = [DriverCandidate {
        path: "vtd",
        bind_keys: &[DriverBindKey {
            priority: 1,
            reserved0: 0,
            key: vtd,
        }],
    }];
    let MatchResolution::Winner { .. } = tairix_devmatch::resolve(node.match_keys(), &families)
    else {
        return Err(Refusal::Unmatched);
    };
    let window = node
        .resources()
        .iter()
        .find(|r| r.kind() == Some(HwResourceKind::Mmio))
        .ok_or(Refusal::NoRegisters)?;
    let base = window.base();
    let len = usize::try_from(window.length()).map_err(|_| Refusal::NoRegisters)?;
    base.checked_add(window.length())
        .ok_or(Refusal::NoRegisters)?;
    let mut reserved = Vec::new();
    reserved
        .try_reserve(node.resources().len())
        .map_err(|_| Refusal::Unit(IommuError::Exhausted))?;
    reserved.extend(
        node.resources()
            .iter()
            .filter_map(|r| r.iommu_reserved().ok()),
    );
    let registers = (env.mmio)(base, len).ok_or(Refusal::NoRegisters)?;
    // SAFETY: the port mapped exactly `len` bytes of the unit's register set
    // uncached for the kernel's life, and the kernel maps a unit's registers
    // nowhere else (no process may map them), so this window is their only
    // owner.
    let regs = unsafe { RegisterWindow::from_mapping(base, registers, len) };
    let unit = tairix_kernel_iommu_vtd::VtdUnit::new(regs, env.frames, env.coherence, env.clock)
        .map_err(Refusal::Unit)?;
    Ok(Unit {
        node: node.id(),
        unit: Box::leak(Box::new(unit)),
        reserved,
    })
}

/// A unit's node, and whether it started translating: if it did, how many
/// functions behind it were found mastering DMA as it took over though
/// firmware keeps no window for them.
pub type UnitOutcome = (u32, Result<usize, IommuError>);

/// The kernel's DMA translation.
pub struct Translation {
    units: Vec<Unit>,
    /// Every discovered unit's register window, whatever became of the unit.
    guarded: Vec<Range<u64>>,
    tree: &'static dyn HwTreeSource,
    audit: &'static (dyn Sink + Sync),
    mastering: Mastering,
    owners: SpinLock<Owners>,
    /// Streams no owner holds, each keeping its firmware windows.
    firmware: SpinLock<HashMap<(usize, u32), Domain<'static>, BuildFastHash>>,
}

struct Owners {
    /// Each node's latest owner. A revoked one stays, so its generation
    /// carves nothing more, until the node leaves the tree — and for good if
    /// its end was not confirmed.
    nodes: HashMap<u32, Arc<Owner>, BuildFastHash>,
    /// The node each recorded owner's streams belong to, by unit and stream,
    /// so a fault is laid at its device's door.
    streams: HashMap<(usize, u32), u32, BuildFastHash>,
}

impl Owners {
    /// Record `owner` for `node` if the node's record is still `expected`,
    /// answering whether it did.
    fn replace(
        &mut self,
        node: u32,
        expected: Option<&Arc<Owner>>,
        owner: &Arc<Owner>,
    ) -> Result<bool, DmaError> {
        let unchanged = match (self.nodes.get(&node), expected) {
            (None, None) => true,
            (Some(current), Some(expected)) => Arc::ptr_eq(current, expected),
            _ => false,
        };
        if !unchanged {
            return Ok(false);
        }
        let out_of_memory = |_| DmaError::Alloc(AllocError::OutOfMemory);
        if expected.is_none() {
            let count =
                usize::try_from(owner.streams.count()).map_err(|_| DmaError::Translation)?;
            self.streams.try_reserve(count).map_err(out_of_memory)?;
            self.nodes.try_reserve(1).map_err(out_of_memory)?;
            for stream in stream_range(owner.streams) {
                let _ = self.streams.try_insert((owner.unit, stream), node);
            }
        }
        let _ = self.nodes.try_insert(node, Arc::clone(owner));
        Ok(true)
    }

    /// Put `predecessor` back as `node`'s record if `owner` still holds it,
    /// or forget the node when there was none.
    fn restore(&mut self, node: u32, owner: &Arc<Owner>, predecessor: Option<Arc<Owner>>) {
        if !self
            .nodes
            .get(&node)
            .is_some_and(|current| Arc::ptr_eq(current, owner))
        {
            return;
        }
        match predecessor {
            Some(predecessor) => {
                let _ = self.nodes.try_insert(node, predecessor);
            }
            None => self.remove(node, owner),
        }
    }

    fn remove(&mut self, node: u32, owner: &Owner) {
        self.nodes.remove(&node);
        for stream in stream_range(owner.streams) {
            self.streams.remove(&(owner.unit, stream));
        }
    }
}

struct Owner {
    generation: u64,
    node: u32,
    unit: usize,
    streams: IommuStreams,
    state: SpinLock<OwnerState>,
    /// Whether a carve of this owner was audited as unconfirmed: once per
    /// owner, so a driver retrying it cannot flood the log.
    unconfirmed: AtomicBool,
}

enum OwnerState {
    /// Held under the owner's lock while its domain is made; never seen.
    Adopting,
    Live(Domain<'static>),
    /// Its domain could not be made; the node's record went back to what
    /// was there before.
    Unadopted(DmaError),
    Revoked {
        confirmed: bool,
    },
}

impl Translation {
    /// Start translating through `units`: each stream firmware keeps a window
    /// for is attached to its firmware domain, the functions behind the unit
    /// still mastering without one are counted, then each unit is enabled.
    /// `guarded` is every discovered unit's register window, kept from every
    /// process whether or not its unit translates; a carve the unit could not
    /// confirm is recorded to `audit`; bus mastering follows each owner
    /// through `mastering`. Beside the facility, each unit's outcome; one that
    /// did not enable is dropped, and its devices stay untranslated.
    #[must_use]
    pub fn start(
        units: Vec<Unit>,
        guarded: Vec<Range<u64>>,
        tree: &'static dyn HwTreeSource,
        audit: &'static (dyn Sink + Sync),
        mastering: Mastering,
    ) -> (Self, Vec<UnitOutcome>) {
        let mut translation = Self {
            units: Vec::with_capacity(units.len()),
            guarded,
            tree,
            audit,
            mastering,
            owners: SpinLock::new(Owners {
                nodes: HashMap::with_hasher(BuildFastHash::new()),
                streams: HashMap::with_hasher(BuildFastHash::new()),
            }),
            firmware: SpinLock::new(HashMap::with_hasher(BuildFastHash::new())),
        };
        let mut outcomes = Vec::with_capacity(units.len());
        for unit in units {
            let index = translation.units.len();
            let node = unit.node;
            let enable = unit.unit;
            translation.units.push(unit);
            for window in 0..translation.units[index].reserved.len() {
                let stream = translation.units[index].reserved[window].stream();
                translation.restore_firmware(index, stream);
            }
            let masters = translation.mastering.strays(node, &|stream| {
                translation.keeps_firmware(index, stream..=stream)
            });
            let outcome = enable.enable().map(|()| masters);
            if outcome.is_err() {
                translation
                    .firmware
                    .lock()
                    .retain(|&(unit, _), _| unit != index);
                translation.units.pop();
            }
            outcomes.push((node, outcome));
        }
        (translation, outcomes)
    }

    /// Whether `node`'s DMA is translated here.
    #[must_use]
    pub fn translates(&self, node: u32) -> bool {
        self.stream_of(node).is_ok()
    }

    /// Whether `[base, base + len)` reaches into any discovered unit's
    /// registers.
    #[must_use]
    pub fn guards(&self, base: u64, len: u64) -> bool {
        let end = base.saturating_add(len);
        self.guarded
            .iter()
            .any(|window| base < window.end && window.start < end)
    }

    /// Units translating.
    #[must_use]
    pub fn units(&self) -> usize {
        self.units.len()
    }

    /// End the domain of `node`'s owner admitted as `generation`: its
    /// function stops mastering, its streams are blocked and the unit's
    /// caches confirmed clean, so nothing it mapped stays reachable, and the
    /// generation carves nothing more. Returns whether the unit confirmed it;
    /// an end it could not confirm is audited.
    pub fn revoke(&self, node: u32, generation: u64) -> bool {
        let owner = self.owners.lock().nodes.get(&node).cloned();
        owner
            .filter(|owner| owner.generation == generation)
            .is_none_or(|owner| self.retire(&owner))
    }

    /// `node` has left the tree: end its owner's domain, whatever its
    /// generation, and forget the node. Whether the unit confirmed the end;
    /// one it could not keeps the owner recorded.
    ///
    /// The unit's waits run outside the owners lock: a node gone from the
    /// tree takes no new owner, so the entry can only still be this one.
    pub fn forget(&self, node: u32) -> bool {
        let Some(owner) = self.owners.lock().nodes.get(&node).cloned() else {
            return true;
        };
        if !self.retire(&owner) {
            return false;
        }
        let mut owners = self.owners.lock();
        if owners
            .nodes
            .get(&node)
            .is_some_and(|recorded| Arc::ptr_eq(recorded, &owner))
        {
            owners.remove(node, &owner);
        }
        true
    }

    /// The node whose owner holds `stream` on unit `unit`, if any owner was
    /// ever recorded for it.
    fn node_of(&self, unit: usize, stream: u32) -> Option<u32> {
        self.owners.lock().streams.get(&(unit, stream)).copied()
    }

    /// End `owner`'s domain once, handing its streams back to their firmware
    /// domains, and answer whether the unit confirmed it. An end the unit
    /// could not confirm is audited, once for the owner.
    ///
    /// The device stops mastering before its domain is destroyed, so a
    /// transfer in flight is not faulted by its own revocation; only the
    /// live arm withdraws, so a second retirer cannot stop what a successor
    /// was since granted.
    fn retire(&self, owner: &Owner) -> bool {
        let mut state = owner.state.lock();
        let (confirmed, stopped) =
            match core::mem::replace(&mut *state, OwnerState::Revoked { confirmed: false }) {
                OwnerState::Live(domain) => {
                    let stopped = if self.keeps_firmware(owner.unit, stream_range(owner.streams)) {
                        None
                    } else {
                        self.mastering
                            .withdraw(MasterTarget::Streams(owner.streams), owner.generation)
                    };
                    (domain.destroy().is_ok(), stopped)
                }
                OwnerState::Adopting | OwnerState::Unadopted(_) => (true, None),
                OwnerState::Revoked { confirmed } => {
                    *state = OwnerState::Revoked { confirmed };
                    return confirmed;
                }
            };
        *state = OwnerState::Revoked { confirmed };
        self.mastering.record(owner.node, false, stopped);
        drop(state);
        for stream in stream_range(owner.streams) {
            self.restore_firmware(owner.unit, stream);
        }
        if !confirmed && !owner.unconfirmed.swap(true, Ordering::Relaxed) {
            audit_unconfirmed(self.audit, owner.node, owner.generation);
        }
        confirmed
    }

    /// Whether firmware keeps a window on `unit` for any of `streams`: it
    /// still masters them, so their function keeps its bus mastering.
    fn keeps_firmware(&self, unit: usize, streams: RangeInclusive<u32>) -> bool {
        self.units[unit]
            .reserved
            .iter()
            .any(|window| streams.contains(&window.stream()))
    }

    fn stream_of(&self, node: u32) -> Result<(usize, IommuStreams), DmaError> {
        let entry = self
            .tree
            .node(node)
            .ok()
            .flatten()
            .ok_or(DmaError::DeviceGone)?;
        let streams = entry
            .resources()
            .iter()
            .find_map(|r| r.iommu_streams().ok())
            .ok_or(DmaError::Translation)?;
        let unit = self
            .units
            .iter()
            .position(|u| u.node == streams.unit())
            .ok_or(DmaError::Translation)?;
        Ok((unit, streams))
    }

    /// The firmware windows `unit` keeps for any of `streams`.
    fn windows(
        &self,
        unit: usize,
        streams: &RangeInclusive<u32>,
    ) -> Result<Vec<Range<u64>>, IommuError> {
        let reserved = &self.units[unit].reserved;
        let mut windows = Vec::new();
        windows
            .try_reserve(reserved.len())
            .map_err(|_| IommuError::Exhausted)?;
        windows.extend(
            reserved
                .iter()
                .filter(|window| streams.contains(&window.stream()))
                .map(|window| window.base()..window.base() + window.len()),
        );
        Ok(windows)
    }

    /// Give `stream` a firmware domain again if firmware keeps a window for
    /// it and it has none. A stream that cannot have one stays blocked. The
    /// unit's waits run outside the firmware lock; a stream two restorers
    /// race for is attached by one, and the other's domain is dropped.
    fn restore_firmware(&self, unit: usize, stream: u32) {
        if self.firmware.lock().contains_key(&(unit, stream)) {
            return;
        }
        let Ok(windows) = self.windows(unit, &(stream..=stream)) else {
            return;
        };
        if windows.is_empty() {
            return;
        }
        let Ok(mut domain) = Domain::new(self.units[unit].unit, &windows) else {
            return;
        };
        if domain.attach(stream).is_ok() {
            let mut firmware = self.firmware.lock();
            if firmware.try_reserve(1).is_ok() && !firmware.contains_key(&(unit, stream)) {
                let _ = firmware.try_insert((unit, stream), domain);
            }
        }
    }

    /// The domain of `node`'s owner admitted as `generation`, created and
    /// attached at the owner's first carve.
    fn owner(&self, node: u32, generation: u64) -> Result<Arc<Owner>, DmaError> {
        loop {
            let predecessor = {
                let owners = self.owners.lock();
                match owners.nodes.get(&node) {
                    Some(existing) if existing.generation == generation => {
                        return Ok(Arc::clone(existing));
                    }
                    Some(existing) if existing.generation == KERNEL_OWNER => {
                        return Err(DmaError::KernelOwned);
                    }
                    Some(existing) if existing.generation > generation => {
                        return Err(DmaError::DeviceGone);
                    }
                    other => other.cloned(),
                }
            };
            // An earlier owner's end precedes any carve of a later one, and a
            // stream whose end was not confirmed takes no successor.
            if predecessor.as_ref().is_some_and(|p| !self.retire(p)) {
                return Err(DmaError::Translation);
            }
            let (unit, streams) = self.stream_of(node)?;
            let owner = Arc::new(Owner {
                generation,
                node,
                unit,
                streams,
                state: SpinLock::new(OwnerState::Adopting),
                unconfirmed: AtomicBool::new(false),
            });
            let mut state = owner.state.lock();
            let published = {
                let mut owners = self.owners.lock();
                // Checked under the lock `forget` takes after a removal, so
                // a node that left once `stream_of` saw it is never attached:
                // either `forget` finds this owner, or this finds it gone.
                if !self.tree.is_live(node) {
                    return Err(DmaError::DeviceGone);
                }
                owners.replace(node, predecessor.as_ref(), &owner)?
            };
            if !published {
                continue;
            }
            match self.adopt(unit, streams) {
                Ok(domain) => *state = OwnerState::Live(domain),
                Err(err) => {
                    *state = OwnerState::Unadopted(err);
                    drop(state);
                    self.owners.lock().restore(node, &owner, predecessor);
                    return Err(err);
                }
            }
            // Recorded under the owner's lock, so its withdrawal can never be
            // logged ahead of it.
            let mastered = self
                .mastering
                .grant(MasterTarget::Streams(streams), generation);
            self.mastering.record(node, true, mastered);
            drop(state);
            return Ok(owner);
        }
    }

    /// A domain holding `streams` and the firmware windows they keep, their
    /// firmware domains given up for it. Nothing of the carve that asked is
    /// mapped yet, so a failure leaves no memory unconfirmed, and every
    /// stream it leaves blocked gets its firmware domain back.
    fn adopt(&self, unit: usize, streams: IommuStreams) -> Result<Domain<'static>, DmaError> {
        let ids = stream_range(streams);
        let adopted = self.windows(unit, &ids).and_then(|windows| {
            for stream in ids.clone() {
                let firmware = self.firmware.lock().remove(&(unit, stream));
                if let Some(domain) = firmware {
                    domain.destroy()?;
                }
            }
            let mut domain = Domain::new(self.units[unit].unit, &windows)?;
            for stream in ids.clone() {
                domain.attach(stream)?;
            }
            Ok(domain)
        });
        adopted.map_err(|err| {
            for stream in ids {
                self.restore_firmware(unit, stream);
            }
            match err {
                IommuError::Exhausted => DmaError::Alloc(AllocError::OutOfMemory),
                _ => DmaError::Translation,
            }
        })
    }
}

/// Record that a translation `node`'s owner admitted as `generation` held
/// could not be confirmed gone, so what it reached is kept for good.
pub(crate) fn audit_unconfirmed(audit: &(dyn Sink + Sync), node: u32, generation: u64) {
    emit(
        audit,
        Level::Error,
        AuditEvent::DmaTranslationUnconfirmed,
        &[
            Field {
                key: "node",
                value: FieldValue::UnsignedInt(u64::from(node)),
            },
            Field {
                key: "generation",
                value: FieldValue::UnsignedInt(generation),
            },
        ],
    );
}

impl Translation {
    /// Audit `result` if it is `owner`'s first carve the unit could not
    /// confirm.
    fn note<T>(
        &self,
        node: u32,
        owner: &Owner,
        result: Result<T, DmaError>,
    ) -> Result<T, DmaError> {
        if result
            .as_ref()
            .is_err_and(|err| *err == DmaError::Unconfirmed)
            && !owner.unconfirmed.swap(true, Ordering::Relaxed)
        {
            audit_unconfirmed(self.audit, node, owner.generation);
        }
        result
    }
}

fn stream_range(streams: IommuStreams) -> RangeInclusive<u32> {
    streams.first()..=streams.first() + (streams.count() - 1)
}

fn dma_error(err: IommuError) -> DmaError {
    match err {
        IommuError::Exhausted => DmaError::Alloc(AllocError::OutOfMemory),
        IommuError::Unconfirmed => DmaError::Unconfirmed,
        _ => DmaError::Translation,
    }
}

impl DeviceTranslation for Translation {
    fn map(
        &self,
        node: u32,
        generation: u64,
        block: DmaBlock,
        limit: u64,
    ) -> Result<u64, DmaError> {
        let owner = self.owner(node, generation)?;
        let mut state = owner.state.lock();
        let result = match &mut *state {
            OwnerState::Live(domain) => domain
                .map(block.frame.start().as_u64(), block.order, limit)
                .map_err(dma_error),
            OwnerState::Unadopted(err) => Err(*err),
            OwnerState::Adopting | OwnerState::Revoked { .. } => Err(DmaError::DeviceGone),
        };
        drop(state);
        self.note(node, &owner, result)
    }

    fn unmap(
        &self,
        node: u32,
        generation: u64,
        iova: u64,
        _block: DmaBlock,
    ) -> Result<(), DmaError> {
        let owner = self.owners.lock().nodes.get(&node).cloned();
        // Any other generation was retired and confirmed: one that was not
        // would still be the node's owner.
        let Some(owner) = owner.filter(|owner| owner.generation == generation) else {
            return Ok(());
        };
        let mut state = owner.state.lock();
        let result = match &mut *state {
            OwnerState::Live(domain) => match domain.unmap(iova) {
                // A block the domain never handed out was never reachable.
                Ok(()) | Err(IommuError::NotMapped) => Ok(()),
                Err(_) => Err(DmaError::Unconfirmed),
            },
            OwnerState::Revoked { confirmed: true } | OwnerState::Unadopted(_) => Ok(()),
            OwnerState::Adopting | OwnerState::Revoked { confirmed: false } => {
                Err(DmaError::Unconfirmed)
            }
        };
        drop(state);
        self.note(node, &owner, result)
    }

    fn end(&self, node: u32, generation: u64) {
        self.revoke(node, generation);
    }
}

impl DmaCustody for Translation {
    fn reserve(&self, node: u32) -> Result<(), DmaError> {
        if self.tree.is_live(node) {
            Ok(())
        } else {
            Err(DmaError::DeviceGone)
        }
    }

    fn unreserve(&self, _node: u32) {}

    /// Keeps the frames for good: nothing after an unconfirmed end can prove
    /// the device lost them.
    fn hold(&self, _node: u32, _generation: u64, _block: DmaBlock) {}
}

#[cfg(test)]
mod tests;
