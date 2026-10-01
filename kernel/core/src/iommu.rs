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
//! An end the unit cannot confirm proves nothing about what the device can
//! still reach, and nothing later can: the owner stays recorded for good, its
//! carves are never reused, and the node takes no successor. The facility is
//! the custody of translated carves for the same reason — a block reaches it
//! only when its unit could not confirm the device lost it, so it keeps the
//! frames for good.

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ops::{Range, RangeInclusive};
use core::ptr::NonNull;

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
use tairix_sync::SpinLock;

use crate::hwtree::HwTreeSource;

/// The generation the kernel's own bootstrap-floor drivers carve as. Every
/// user driver's is later, and none can take a node from the kernel.
pub const KERNEL_OWNER: u64 = 0;

/// A unit its family has taken over, blocked and not yet translating.
pub struct Unit {
    /// Its hardware-tree node.
    pub node: u32,
    /// Its family's driver.
    pub unit: &'static dyn IommuUnit,
    /// Its register window, which no process may map.
    pub registers: Range<u64>,
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
    let end = base
        .checked_add(window.length())
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
        registers: base..end,
        reserved,
    })
}

/// A unit's node, and whether it started translating.
pub type UnitOutcome = (u32, Result<(), IommuError>);

/// The kernel's DMA translation.
pub struct Translation {
    units: Vec<Unit>,
    tree: &'static dyn HwTreeSource,
    /// Each node's latest owner. A revoked one stays, so its generation
    /// carves nothing more, until the node leaves the tree — and for good if
    /// its end was not confirmed.
    owners: SpinLock<HashMap<u32, Arc<Owner>, BuildFastHash>>,
    /// Streams no owner holds, each keeping its firmware windows.
    firmware: SpinLock<HashMap<(usize, u32), Domain<'static>, BuildFastHash>>,
}

struct Owner {
    generation: u64,
    unit: usize,
    streams: IommuStreams,
    state: SpinLock<OwnerState>,
}

enum OwnerState {
    Live(Domain<'static>),
    Revoked { confirmed: bool },
}

impl Translation {
    /// Start translating through `units`: each stream firmware keeps a window
    /// for is attached to its firmware domain, then each unit is enabled.
    /// Beside the facility, each unit's node and whether it enabled; one that
    /// did not is dropped, and its devices stay untranslated.
    #[must_use]
    pub fn start(units: Vec<Unit>, tree: &'static dyn HwTreeSource) -> (Self, Vec<UnitOutcome>) {
        let mut translation = Self {
            units: Vec::with_capacity(units.len()),
            tree,
            owners: SpinLock::new(HashMap::with_hasher(BuildFastHash::new())),
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
            let outcome = enable.enable();
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

    /// Whether `[base, base + len)` reaches into any unit's registers.
    #[must_use]
    pub fn guards(&self, base: u64, len: u64) -> bool {
        let end = base.saturating_add(len);
        self.units
            .iter()
            .any(|unit| base < unit.registers.end && unit.registers.start < end)
    }

    /// Units translating.
    #[must_use]
    pub fn units(&self) -> usize {
        self.units.len()
    }

    /// End the domain of `node`'s owner admitted as `generation`: its streams
    /// are blocked and the unit's caches confirmed clean, so nothing it
    /// mapped stays reachable, and the generation carves nothing more.
    /// Returns whether the unit confirmed it.
    pub fn revoke(&self, node: u32, generation: u64) -> bool {
        let owner = self.owners.lock().get(&node).cloned();
        owner
            .filter(|owner| owner.generation == generation)
            .is_none_or(|owner| self.retire(&owner))
    }

    /// `node` has left the tree: end its owner's domain, whatever its
    /// generation, and forget the node. The owner's generation when the unit
    /// could not confirm the end, which keeps it recorded.
    ///
    /// The unit's waits run outside the owners lock: a node gone from the
    /// tree takes no new owner, so the entry can only still be this one.
    pub fn forget(&self, node: u32) -> Option<u64> {
        let owner = self.owners.lock().get(&node).cloned()?;
        if !self.retire(&owner) {
            return Some(owner.generation);
        }
        let mut owners = self.owners.lock();
        if owners
            .get(&node)
            .is_some_and(|recorded| Arc::ptr_eq(recorded, &owner))
        {
            owners.remove(&node);
        }
        None
    }

    /// End `owner`'s domain once, handing its streams back to their firmware
    /// domains, and answer whether the unit confirmed it.
    fn retire(&self, owner: &Owner) -> bool {
        let mut state = owner.state.lock();
        let confirmed =
            match core::mem::replace(&mut *state, OwnerState::Revoked { confirmed: false }) {
                OwnerState::Live(domain) => domain.destroy().is_ok(),
                OwnerState::Revoked { confirmed } => {
                    *state = OwnerState::Revoked { confirmed };
                    return confirmed;
                }
            };
        *state = OwnerState::Revoked { confirmed };
        drop(state);
        for stream in stream_range(owner.streams) {
            self.restore_firmware(owner.unit, stream);
        }
        confirmed
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
    /// it and it has none. A stream that cannot have one stays blocked.
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
            let _ = self.firmware.lock().try_insert((unit, stream), domain);
        }
    }

    /// The domain of `node`'s owner admitted as `generation`, created and
    /// attached at the owner's first carve.
    fn owner(&self, node: u32, generation: u64) -> Result<Arc<Owner>, DmaError> {
        let mut owners = self.owners.lock();
        if let Some(existing) = owners.get(&node).cloned() {
            if existing.generation == generation {
                return Ok(existing);
            }
            if existing.generation == KERNEL_OWNER {
                return Err(DmaError::KernelOwned);
            }
            if existing.generation > generation {
                return Err(DmaError::DeviceGone);
            }
            // An earlier owner's end precedes any carve of a later one, and a
            // stream whose end was not confirmed takes no successor.
            if !self.retire(&existing) {
                return Err(DmaError::Translation);
            }
        }
        let (unit, streams) = self.stream_of(node)?;
        owners
            .try_reserve(1)
            .map_err(|_| DmaError::Alloc(AllocError::OutOfMemory))?;
        let domain = self.adopt(unit, streams)?;
        let owner = Arc::new(Owner {
            generation,
            unit,
            streams,
            state: SpinLock::new(OwnerState::Live(domain)),
        });
        let _ = owners.try_insert(node, Arc::clone(&owner));
        Ok(owner)
    }

    /// A domain holding `streams` and the firmware windows they keep, their
    /// firmware domains given up for it. Nothing of the carve that asked is
    /// mapped yet, so a failure leaves no memory unconfirmed, and every
    /// stream it leaves blocked gets its firmware domain back.
    fn adopt(&self, unit: usize, streams: IommuStreams) -> Result<Domain<'static>, DmaError> {
        let ids = stream_range(streams);
        let adopted = self.windows(unit, &ids).and_then(|windows| {
            let mut firmware = self.firmware.lock();
            for stream in ids.clone() {
                if let Some(domain) = firmware.remove(&(unit, stream)) {
                    domain.destroy()?;
                }
            }
            drop(firmware);
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
        let OwnerState::Live(domain) = &mut *state else {
            return Err(DmaError::DeviceGone);
        };
        domain
            .map(block.frame.start().as_u64(), block.order, limit)
            .map_err(dma_error)
    }

    fn unmap(
        &self,
        node: u32,
        generation: u64,
        iova: u64,
        _block: DmaBlock,
    ) -> Result<(), DmaError> {
        let owner = self.owners.lock().get(&node).cloned();
        // Any other generation was retired and confirmed: one that was not
        // would still be the node's owner.
        let Some(owner) = owner.filter(|owner| owner.generation == generation) else {
            return Ok(());
        };
        let mut state = owner.state.lock();
        match &mut *state {
            OwnerState::Live(domain) => match domain.unmap(iova) {
                // A block the domain no longer maps is already out of reach.
                Ok(()) | Err(IommuError::NotMapped) => Ok(()),
                Err(_) => Err(DmaError::Unconfirmed),
            },
            OwnerState::Revoked { confirmed: true } => Ok(()),
            OwnerState::Revoked { confirmed: false } => Err(DmaError::Unconfirmed),
        }
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
