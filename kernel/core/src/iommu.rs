//! DMA translation: every unit discovery reported that a family here can
//! drive, and the domain each translated node's owner maps its carves into
//! (`plans/IOMMU.md`).
//!
//! A node is translated when its tree entry names an [`IommuStreams`] on a
//! unit brought up here. Its owner's first carve claims the node's isolation
//! group ([`tairix_abi::IommuGroup`]) and creates a domain translating every stream the
//! node's DMA arrives as, with the firmware windows they keep; each carve maps
//! there; the owner's end revokes it — streams blocked, the unit's caches
//! confirmed clean — after which every carve of that generation is unreachable
//! and is freed at once rather than quarantined. One owner holds a group at a
//! time: the fabric cannot keep its members apart, so a second node's owner is
//! refused until the first has ended. A stream no owner holds keeps only its
//! firmware windows, in a firmware domain of its own.
//!
//! A node's function masters DMA only while an owner holds its domain: bus
//! mastering is granted once the domain is attached and withdrawn before it
//! is destroyed, except for a stream firmware keeps a window for, which
//! firmware masters again (`mastering`).
//!
//! An end the unit cannot confirm proves nothing about what the device can
//! still reach, and nothing later can: the owner stays recorded for good, its
//! carves are never reused, and its group takes no successor. The facility is
//! the custody of translated carves for the same reason — a block reaches it
//! only when its unit could not confirm the device lost it, so it keeps the
//! frames for good.
//!
//! Locks nest owner state → owners → tree, owner state → firmware → unit, and
//! owner state → PCI host; no two owners' states are held at once. Neither
//! facility-wide lock is held across a wait on a unit: an adoption waits under
//! its own owner's state alone, which it holds from before anyone else can
//! reach the owner, so carves for other groups go on meanwhile.

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ops::Range;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicBool, Ordering};

use tairix_abi::driver::DriverBindKey;
use tairix_abi::{
    HwMatchKey, HwNode, HwProperty, HwResourceKind, IommuReservedWindow, IommuStreams,
    RegisterWindow, ReservedAccess, HW_NODE_MAX_RESOURCES,
};
use tairix_arch_api::PageTableFrames;
use tairix_collections::HashMap;
use tairix_devmatch::{DriverCandidate, MatchResolution};
use tairix_hash::BuildFastHash;
use tairix_inline::ArrayVec;
use tairix_kernel_iommu_api::{
    Access, Clock, Domain, IdentityWindow, IommuError, IommuUnit, TableCoherence,
};
use tairix_kernel_mem::{AllocError, DeviceTranslation, DmaBlock, DmaCustody, DmaError};
use tairix_log::{Field, FieldValue, Level, Sink};
use tairix_sync::SpinLock;

use crate::audit::{emit, AuditEvent};
use crate::hwtree::HwTreeSource;

mod faults;
mod mastering;
mod remap;

pub use faults::{FaultEnv, FAULT_LIMITS, FAULT_OWNER};
pub use mastering::{BusMastering, MasterChange, MasterOwner, MasterTarget, Mastering, Quiesced};
pub use remap::{InterruptRouting, RemapEntry, RemapError};
pub use tairix_kernel_iommu_api::{InterruptSource, InterruptTarget, Remapped};

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
    /// The wired line its node names for its faults; [`None`] for a unit
    /// that raises them as a message.
    pub faults: Option<WiredFaults>,
}

/// The wired line a unit raises its faults on.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct WiredFaults {
    /// The controller line.
    pub line: u32,
    /// How it signals.
    pub trigger: tairix_kernel_irq::Trigger,
    /// Its place among the node's interrupts.
    pub place: u32,
}

impl WiredFaults {
    /// The line `node` names for its unit's faults: the interrupt at the
    /// place its [`HwProperty::FaultInterrupt`] states. [`None`] where it
    /// names none, or a place it has no line at.
    #[must_use]
    pub fn of(node: &HwNode) -> Option<Self> {
        let place = node
            .resources()
            .iter()
            .find_map(|r| match r.property_value() {
                Ok((HwProperty::FaultInterrupt, place)) => u32::try_from(place).ok(),
                _ => None,
            })?;
        let irq = node
            .resources()
            .iter()
            .find(|r| r.interrupt_position() == Some(place) && !r.is_message())?;
        Some(Self {
            line: u32::try_from(irq.base()).ok()?,
            trigger: if irq.is_edge_triggered() {
                tairix_kernel_irq::Trigger::Edge
            } else {
                tairix_kernel_irq::Trigger::Level
            },
            place,
        })
    }
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
    /// The PCI functions units are, for a family that raises its faults
    /// through its own function's MSI, where the port owns them.
    pub function: Option<&'static dyn tairix_kernel_iommu_api::UnitFunction>,
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
    let key = |compatible| HwMatchKey::compatible(compatible).map_err(|_| Refusal::Unmatched);
    let bind = |key| {
        [DriverBindKey {
            priority: 1,
            reserved0: 0,
            key,
        }]
    };
    let keys = [
        bind(key(tairix_kernel_iommu_vtd::COMPATIBLE)?),
        bind(key(tairix_kernel_iommu_amdvi::COMPATIBLE)?),
        bind(key(tairix_kernel_iommu_smmuv3::COMPATIBLE)?),
        bind(key(tairix_kernel_iommu_riscv::COMPATIBLE)?),
    ];
    let families = [Family::Vtd, Family::AmdVi, Family::Smmuv3, Family::Riscv];
    let candidates = [
        DriverCandidate {
            path: "vtd",
            bind_keys: &keys[0],
        },
        DriverCandidate {
            path: "amdvi",
            bind_keys: &keys[1],
        },
        DriverCandidate {
            path: "smmuv3",
            bind_keys: &keys[2],
        },
        DriverCandidate {
            path: "riscv",
            bind_keys: &keys[3],
        },
    ];
    let MatchResolution::Winner { candidate, .. } =
        tairix_devmatch::resolve(node.match_keys(), &candidates)
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
    let faults = WiredFaults::of(node);
    let unit = match families[candidate] {
        Family::Vtd => {
            tairix_kernel_iommu_vtd::VtdUnit::new(regs, env.frames, env.coherence, env.clock)
                .and_then(kept)
        }
        Family::AmdVi => {
            let function = env.function.map(|function| (function, node.address()));
            tairix_kernel_iommu_amdvi::AmdViUnit::new(regs, env.frames, env.clock, function)
                .and_then(kept)
        }
        Family::Smmuv3 => {
            tairix_kernel_iommu_smmuv3::Smmuv3Unit::new(regs, env.frames, env.clock).and_then(kept)
        }
        Family::Riscv => {
            // Serving faults routes them to the wired line the node names,
            // or else as a message; the unit is told which as it is taken
            // over, the one time it may be.
            let signalling = if faults.is_some() {
                tairix_kernel_iommu_riscv::Signalling::Wired
            } else {
                tairix_kernel_iommu_riscv::Signalling::Message
            };
            tairix_kernel_iommu_riscv::RiscvUnit::new(
                regs,
                env.frames,
                env.coherence,
                env.clock,
                signalling,
            )
            .and_then(kept)
        }
    };
    Ok(Unit {
        node: node.id(),
        unit: unit.map_err(Refusal::Unit)?,
        reserved,
        faults,
    })
}

/// The families a unit is matched to.
#[derive(Copy, Clone)]
enum Family {
    Vtd,
    AmdVi,
    Smmuv3,
    Riscv,
}

/// `unit`, kept for the kernel's life: a unit's tables are never freed, so
/// neither is the driver holding them.
fn kept<U: IommuUnit + 'static>(unit: U) -> Result<&'static dyn IommuUnit, IommuError> {
    let mut slot = Vec::new();
    slot.try_reserve_exact(1)
        .map_err(|_| IommuError::Exhausted)?;
    slot.push(unit);
    let kept: &'static [U] = Box::leak(slot.into_boxed_slice());
    kept.first()
        .map(|unit| unit as &dyn IommuUnit)
        .ok_or(IommuError::Exhausted)
}

/// What became of a discovered unit.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum UnitOutcome {
    /// It translates, at the stage named; what telling the functions behind
    /// it found mastering DMA as it took over, though firmware keeps no window
    /// for them, to stop came to.
    Translating(Quiesced, tairix_kernel_iommu_api::Stage),
    /// It translates nothing, for the refusal named, so every function
    /// behind it was told to stop, firmware's windows kept for none.
    Stranded(Refusal, Quiesced),
}

/// How a node's DMA reaches memory.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DmaPath {
    /// Through units translating here: its carves map into its domain.
    Translated,
    /// Around every unit: its carves take physical addresses and the
    /// quarantine.
    Untranslated,
    /// Through `unit`, which translates nothing here, or a record that could
    /// not be read (`unit` [`None`]): it masters no DMA at all.
    Stranded {
        /// The unit its DMA would cross.
        unit: Option<u32>,
    },
}

/// Stream ranges one node names: its resources bound how many.
type Streams = ArrayVec<IommuStreams, HW_NODE_MAX_RESOURCES>;

/// A unit, by its index in the facility, and a group on it.
type GroupKey = (usize, u32);

/// What a translated node's tree entry says of its DMA.
struct Identity {
    unit: usize,
    group: u32,
    /// The streams naming the node's own functions.
    requester: Streams,
    /// The further streams the fabric delivers its DMA as.
    aliases: Streams,
}

impl Identity {
    fn group_key(&self) -> GroupKey {
        (self.unit, self.group)
    }

    /// Every stream the node's DMA arrives as, once each, ascending.
    fn streams(&self) -> Result<Vec<u32>, DmaError> {
        let ranges = || {
            self.requester
                .as_slice()
                .iter()
                .chain(self.aliases.as_slice())
        };
        let count = ranges().try_fold(0usize, |total, range| {
            total.checked_add(usize::try_from(range.count()).ok()?)
        });
        let mut streams = Vec::new();
        streams
            .try_reserve_exact(count.ok_or(DmaError::Translation)?)
            .map_err(|_| DmaError::Alloc(AllocError::OutOfMemory))?;
        streams.extend(ranges().flat_map(|range| stream_ids(*range)));
        streams.sort_unstable();
        streams.dedup();
        Ok(streams)
    }
}

/// The kernel's DMA translation.
pub struct Translation {
    units: Vec<Unit>,
    /// Units discovered, whether or not they translate: one that does not
    /// strands its devices, which master no DMA.
    discovered: usize,
    /// Every discovered unit's register window, whatever became of the unit.
    guarded: Vec<Range<u64>>,
    tree: &'static dyn HwTreeSource,
    audit: &'static (dyn Sink + Sync),
    mastering: Option<Mastering>,
    owners: SpinLock<Owners>,
    /// Streams no owner holds, each keeping its firmware windows.
    firmware: SpinLock<HashMap<(usize, u32), Domain<'static>, BuildFastHash>>,
    /// Every unit's remapping table is built.
    remap_prepared: AtomicBool,
}

struct Owners {
    /// Each node's latest owner. A revoked one stays, so its generation
    /// carves nothing more, until the node leaves the tree — and for good if
    /// its end was not confirmed.
    nodes: HashMap<u32, Arc<Owner>, BuildFastHash>,
    /// Each group's latest owner: while it lives no other node's owner may
    /// take the group, and one whose end was not confirmed keeps it for good.
    groups: HashMap<GroupKey, Arc<Owner>, BuildFastHash>,
    /// The node whose owner last attached each stream, by unit and stream,
    /// so a fault is laid at its device's door. Written only once an
    /// adoption has attached the stream.
    streams: HashMap<(usize, u32), u32, BuildFastHash>,
    /// Streams of owners published and not yet attached or restored, which
    /// `streams` holds room for beside its own.
    awaited: usize,
}

impl Owners {
    /// Record `owner` for its node and its group if neither record has moved
    /// since it was read — still `predecessor` and `holder` — answering
    /// whether it did. Room for every stream the owner will attach is taken
    /// here, beside that of every owner published before it and still
    /// adopting, so indexing them once attached cannot fail.
    fn publish(
        &mut self,
        owner: &Arc<Owner>,
        predecessor: Option<&Arc<Owner>>,
        holder: Option<&Arc<Owner>>,
    ) -> Result<bool, DmaError> {
        let key = owner.identity.group_key();
        if !same(self.nodes.get(&owner.node), predecessor) || !same(self.groups.get(&key), holder) {
            return Ok(false);
        }
        let out_of_memory = |_| DmaError::Alloc(AllocError::OutOfMemory);
        self.nodes.try_reserve(1).map_err(out_of_memory)?;
        self.groups.try_reserve(1).map_err(out_of_memory)?;
        let awaited = self.awaited + owner.streams.len();
        self.streams.try_reserve(awaited).map_err(out_of_memory)?;
        self.awaited = awaited;
        let _ = self.nodes.try_insert(owner.node, Arc::clone(owner));
        let _ = self.groups.try_insert(key, Arc::clone(owner));
        Ok(true)
    }

    /// Lay the streams `owner`'s adoption attached at its node's door.
    fn attached(&mut self, owner: &Owner) {
        for &stream in &owner.streams {
            // Room was taken at publication, so this cannot allocate.
            let _ = self
                .streams
                .try_insert((owner.identity.unit, stream), owner.node);
        }
        self.awaited = self.awaited.saturating_sub(owner.streams.len());
    }

    /// Put each record `owner`, whose adoption failed, holds back as it was.
    fn restore(
        &mut self,
        owner: &Arc<Owner>,
        predecessor: Option<Arc<Owner>>,
        holder: Option<Arc<Owner>>,
    ) {
        put_back(&mut self.nodes, owner.node, owner, predecessor);
        put_back(&mut self.groups, owner.identity.group_key(), owner, holder);
        self.awaited = self.awaited.saturating_sub(owner.streams.len());
    }

    /// Forget `owner`, whose node left the tree and whose end the unit
    /// confirmed, wherever it is still the record.
    fn forget(&mut self, owner: &Arc<Owner>) {
        if same(self.nodes.get(&owner.node), Some(owner)) {
            self.nodes.remove(&owner.node);
        }
        let key = owner.identity.group_key();
        if same(self.groups.get(&key), Some(owner)) {
            self.groups.remove(&key);
        }
        let (unit, node) = (owner.identity.unit, owner.node);
        self.streams
            .retain(|&(at, _), &mut holder| at != unit || holder != node);
    }
}

fn same(current: Option<&Arc<Owner>>, expected: Option<&Arc<Owner>>) -> bool {
    match (current, expected) {
        (None, None) => true,
        (Some(current), Some(expected)) => Arc::ptr_eq(current, expected),
        _ => false,
    }
}

/// Make `previous` `key`'s record again if `owner` still holds it, or forget
/// the key when there was none.
fn put_back<K: core::hash::Hash + Eq>(
    records: &mut HashMap<K, Arc<Owner>, BuildFastHash>,
    key: K,
    owner: &Arc<Owner>,
    previous: Option<Arc<Owner>>,
) {
    if !same(records.get(&key), Some(owner)) {
        return;
    }
    match previous {
        // The key is present, so the replacement cannot allocate.
        Some(previous) => {
            let _ = records.try_insert(key, previous);
        }
        None => {
            records.remove(&key);
        }
    }
}

struct Owner {
    generation: u64,
    node: u32,
    identity: Identity,
    /// Every stream its domain translates, ascending: the identity's, once
    /// each, worked out at its first carve so its end allocates nothing.
    streams: Vec<u32>,
    /// When it began, which orders its function's bus-mastering changes.
    epoch: u64,
    state: SpinLock<OwnerState>,
    /// Whether a carve of this owner was audited as unconfirmed: once per
    /// owner, so a driver retrying it cannot flood the log.
    unconfirmed: AtomicBool,
}

impl Owner {
    fn master(&self) -> MasterOwner {
        MasterOwner {
            node: self.node,
            generation: self.generation,
            epoch: self.epoch,
        }
    }
}

enum OwnerState {
    /// Held under the owner's lock while its domain is made; never seen.
    Adopting,
    Live(Domain<'static>),
    /// Its domain could not be made; the node's and the group's records went
    /// back to what was there before.
    Unadopted(DmaError),
    Revoked {
        confirmed: bool,
    },
}

impl Translation {
    /// Start translating through `units`: each stream firmware keeps a window
    /// for is attached to its firmware domain, the functions behind the unit
    /// still mastering without one are stopped, then each unit is enabled.
    /// `refused` are the units discovered and not taken over, each with why.
    /// `guarded` is every discovered unit's register window, kept from every
    /// process whether or not its unit translates; a carve the unit could not
    /// confirm is recorded to `audit`; bus mastering follows each owner
    /// through `mastering`, where the kernel owns configuration space. Each
    /// unit's outcome goes to `report`, by node. A unit refused, or one that
    /// did not enable, is stranded: every function behind it is told to stop,
    /// and none gets DMA, since nothing would confine it.
    #[must_use]
    pub fn start(
        units: Vec<Unit>,
        refused: &[(u32, Refusal)],
        guarded: Vec<Range<u64>>,
        tree: &'static dyn HwTreeSource,
        audit: &'static (dyn Sink + Sync),
        mastering: Option<Mastering>,
        report: &mut dyn FnMut(u32, UnitOutcome),
    ) -> Self {
        let strand =
            |node| mastering.map_or_else(Quiesced::default, |m| m.quiesce(node, &|_| false));
        for &(node, refusal) in refused {
            report(node, UnitOutcome::Stranded(refusal, strand(node)));
        }
        let mut translation = Self {
            discovered: refused.len() + units.len(),
            units,
            guarded,
            tree,
            audit,
            mastering,
            owners: SpinLock::new(Owners {
                nodes: HashMap::with_hasher(BuildFastHash::new()),
                groups: HashMap::with_hasher(BuildFastHash::new()),
                streams: HashMap::with_hasher(BuildFastHash::new()),
                awaited: 0,
            }),
            firmware: SpinLock::new(HashMap::with_hasher(BuildFastHash::new())),
            remap_prepared: AtomicBool::new(false),
        };
        // A unit that does not enable leaves the list in place, so the units
        // after it, whose firmware domains are not yet made, keep their index.
        let mut index = 0;
        while let Some(unit) = translation.units.get(index) {
            let (node, enable) = (unit.node, unit.unit);
            for window in 0..unit.reserved.len() {
                let stream = translation.units[index].reserved[window].stream();
                translation.restore_firmware(index, stream);
            }
            let quiesced = translation
                .mastering
                .map_or_else(Quiesced::default, |mastering| {
                    mastering.quiesce(node, &|stream| translation.keeps_firmware(index, stream))
                });
            let outcome = match enable.enable() {
                Ok(()) => {
                    index += 1;
                    UnitOutcome::Translating(quiesced, enable.profile().stage)
                }
                Err(err) => {
                    translation
                        .firmware
                        .lock()
                        .retain(|&(unit, _), _| unit != index);
                    translation.units.remove(index);
                    // What firmware's windows kept mastering stops too: no
                    // unit keeps them.
                    let rest = strand(node);
                    UnitOutcome::Stranded(
                        Refusal::Unit(err),
                        Quiesced {
                            stopped: quiesced.stopped + rest.stopped,
                            refused: rest.refused,
                        },
                    )
                }
            };
            report(node, outcome);
        }
        translation
    }

    /// [`Self::start`] over `units` with none refused, the outcomes kept.
    #[cfg(test)]
    pub(crate) fn started(
        units: Vec<Unit>,
        guarded: Vec<Range<u64>>,
        tree: &'static dyn HwTreeSource,
        audit: &'static (dyn Sink + Sync),
        mastering: Option<Mastering>,
    ) -> (Self, Vec<(u32, UnitOutcome)>) {
        let mut outcomes = Vec::new();
        let translation = Self::start(
            units,
            &[],
            guarded,
            tree,
            audit,
            mastering,
            &mut |node, outcome| {
                outcomes.push((node, outcome));
            },
        );
        (translation, outcomes)
    }

    /// How `node`'s DMA reaches memory: translated only where every unit it
    /// names translates here, and stranded where any it names does not, or
    /// its record cannot be read.
    #[must_use]
    pub fn dma_path(&self, node: u32) -> DmaPath {
        let Some(entry) = self.tree.node(node).ok().flatten() else {
            return DmaPath::Stranded { unit: None };
        };
        let mut path = DmaPath::Untranslated;
        for streams in entry
            .resources()
            .iter()
            .filter_map(|r| r.iommu_streams().ok())
        {
            if self.unit_index(streams.unit()).is_none() {
                return DmaPath::Stranded {
                    unit: Some(streams.unit()),
                };
            }
            path = DmaPath::Translated;
        }
        path
    }

    /// Whether some discovered unit translates nothing.
    #[must_use]
    pub fn strands(&self) -> bool {
        self.units.len() < self.discovered
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
        self.owners.lock().forget(&owner);
        true
    }

    /// The node whose owner last attached `stream` on unit `unit`, if any
    /// owner was ever recorded for it.
    fn node_of(&self, unit: usize, stream: u32) -> Option<u32> {
        self.owners.lock().streams.get(&(unit, stream)).copied()
    }

    fn unit_index(&self, node: u32) -> Option<usize> {
        self.units.iter().position(|unit| unit.node == node)
    }

    /// End `owner`'s domain once, handing its streams back to their firmware
    /// domains, and answer whether the unit confirmed it. An end the unit
    /// could not confirm is audited, once for the owner.
    ///
    /// The device stops mastering before its domain is destroyed, so a
    /// transfer in flight is not faulted by its own revocation; only the
    /// live arm withdraws, so a second retirer cannot stop what a successor
    /// was since granted. The end is published only once every stream is
    /// back with firmware, so a successor that sees it never adopts a stream
    /// a restore is still attaching.
    fn retire(&self, owner: &Owner) -> bool {
        let mut state = owner.state.lock();
        let confirmed =
            match core::mem::replace(&mut *state, OwnerState::Revoked { confirmed: false }) {
                OwnerState::Live(domain) => {
                    let requester = owner.identity.requester.as_slice();
                    if !self.keeps_any_firmware(owner.identity.unit, requester) {
                        if let Some(mastering) = self.mastering {
                            mastering.set(MasterTarget::Streams(requester), false, owner.master());
                        }
                    }
                    domain.destroy().is_ok()
                }
                OwnerState::Adopting | OwnerState::Unadopted(_) => true,
                OwnerState::Revoked { confirmed } => {
                    *state = OwnerState::Revoked { confirmed };
                    return confirmed;
                }
            };
        for &stream in &owner.streams {
            self.restore_firmware(owner.identity.unit, stream);
        }
        *state = OwnerState::Revoked { confirmed };
        drop(state);
        if !confirmed && !owner.unconfirmed.swap(true, Ordering::Relaxed) {
            audit_unconfirmed(self.audit, owner.node, owner.generation);
        }
        confirmed
    }

    /// Whether firmware keeps a window on `unit` for `stream`: it still
    /// masters it, so its function keeps its bus mastering.
    fn keeps_firmware(&self, unit: usize, stream: u32) -> bool {
        self.units[unit]
            .reserved
            .iter()
            .any(|window| window.stream() == stream)
    }

    fn keeps_any_firmware(&self, unit: usize, ranges: &[IommuStreams]) -> bool {
        self.units[unit]
            .reserved
            .iter()
            .any(|window| ranges.iter().any(|range| range.contains(window.stream())))
    }

    /// What `node`'s tree entry says of its DMA: one group, and every stream
    /// on that group's unit, the unit translating here. Anything else is not
    /// an identity a domain can be made for.
    fn identity_of(&self, node: u32) -> Result<Identity, DmaError> {
        let entry = self
            .tree
            .node(node)
            .ok()
            .flatten()
            .ok_or(DmaError::DeviceGone)?;
        let mut groups = entry
            .resources()
            .iter()
            .filter_map(|r| r.iommu_group().ok());
        let (Some(group), None) = (groups.next(), groups.next()) else {
            return Err(DmaError::Translation);
        };
        let unit = self.unit_index(group.unit()).ok_or(DmaError::Translation)?;
        let mut identity = Identity {
            unit,
            group: group.id(),
            requester: ArrayVec::new(),
            aliases: ArrayVec::new(),
        };
        for resource in entry.resources() {
            let (streams, into) = match resource.kind() {
                Some(HwResourceKind::IommuStream) => {
                    (resource.iommu_streams(), &mut identity.requester)
                }
                Some(HwResourceKind::IommuAlias) => {
                    (resource.iommu_aliases(), &mut identity.aliases)
                }
                _ => continue,
            };
            let streams = streams.map_err(|_| DmaError::Translation)?;
            if streams.unit() != group.unit() || into.try_push(streams).is_err() {
                return Err(DmaError::Translation);
            }
        }
        if identity.requester.is_empty() {
            return Err(DmaError::Translation);
        }
        Ok(identity)
    }

    /// The firmware windows `unit` keeps for any of `streams`, for the
    /// access firmware allows in each.
    fn windows(&self, unit: usize, streams: &[u32]) -> Result<Vec<IdentityWindow>, IommuError> {
        let reserved = &self.units[unit].reserved;
        let mut windows = Vec::new();
        windows
            .try_reserve(reserved.len())
            .map_err(|_| IommuError::Exhausted)?;
        windows.extend(
            reserved
                .iter()
                .filter(|window| streams.binary_search(&window.stream()).is_ok())
                .map(|window| IdentityWindow {
                    range: window.base()..window.base() + window.len(),
                    access: match window.access() {
                        ReservedAccess::Read => Access::READ,
                        ReservedAccess::Write => Access::WRITE,
                        ReservedAccess::ReadWrite => Access::READ_WRITE,
                    },
                }),
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
        let Ok(windows) = self.windows(unit, &[stream]) else {
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

    /// Whether the group `holder` holds is free for an owner of `node`
    /// admitted as `generation`: its owner ended, and the unit confirmed it.
    /// Waits for an adoption or an end in flight, holding nothing else.
    fn free(&self, holder: &Owner, node: u32, generation: u64) -> Result<(), DmaError> {
        match &*holder.state.lock() {
            OwnerState::Revoked { confirmed: true } | OwnerState::Unadopted(_) => return Ok(()),
            OwnerState::Revoked { confirmed: false } => return Err(DmaError::Translation),
            OwnerState::Live(_) | OwnerState::Adopting => {}
        }
        if holder.generation == KERNEL_OWNER {
            return Err(DmaError::KernelOwned);
        }
        // Written once the holder's lock is let go, so a refusal never holds
        // up the live owner's maps and unmaps.
        audit_group_refused(self.audit, node, generation, holder);
        Err(DmaError::GroupBusy)
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
            // group whose end was not confirmed takes no successor.
            if predecessor.as_ref().is_some_and(|p| !self.retire(p)) {
                return Err(DmaError::Translation);
            }
            let identity = self.identity_of(node)?;
            let streams = identity.streams()?;
            let holder = self
                .owners
                .lock()
                .groups
                .get(&identity.group_key())
                .cloned();
            if let Some(holder) = holder.as_ref().filter(|holder| holder.node != node) {
                self.free(holder, node, generation)?;
            }
            let owner = Arc::new(Owner {
                generation,
                node,
                identity,
                streams,
                epoch: self.mastering.map_or(0, |mastering| mastering.begin()),
                state: SpinLock::new(OwnerState::Adopting),
                unconfirmed: AtomicBool::new(false),
            });
            let mut state = owner.state.lock();
            let published = {
                let mut owners = self.owners.lock();
                // Checked under the lock `forget` takes after a removal, so
                // a node that left once `identity_of` saw it is never
                // attached: either `forget` finds this owner, or this finds
                // it gone.
                if !self.tree.is_live(node) {
                    return Err(DmaError::DeviceGone);
                }
                owners.publish(&owner, predecessor.as_ref(), holder.as_ref())?
            };
            if !published {
                continue;
            }
            match self.adopt(owner.identity.unit, &owner.streams) {
                Ok(domain) => {
                    self.owners.lock().attached(&owner);
                    *state = OwnerState::Live(domain);
                }
                Err(err) => {
                    *state = OwnerState::Unadopted(err);
                    drop(state);
                    self.owners.lock().restore(&owner, predecessor, holder);
                    return Err(err);
                }
            }
            // Made under the owner's lock, so its withdrawal can never land
            // ahead of it.
            if let Some(mastering) = self.mastering {
                mastering.set(
                    MasterTarget::Streams(owner.identity.requester.as_slice()),
                    true,
                    owner.master(),
                );
            }
            drop(state);
            return Ok(owner);
        }
    }

    /// A domain translating `streams` and holding the firmware windows they
    /// keep, their firmware domains given up for it. Nothing of the carve that
    /// asked is mapped yet, so a failure leaves no memory unconfirmed, and
    /// every stream it leaves blocked gets its firmware domain back.
    fn adopt(&self, unit: usize, streams: &[u32]) -> Result<Domain<'static>, DmaError> {
        let adopted = self.windows(unit, streams).and_then(|windows| {
            for &stream in streams {
                let firmware = self.firmware.lock().remove(&(unit, stream));
                if let Some(domain) = firmware {
                    domain.destroy()?;
                }
            }
            let mut domain = Domain::new(self.units[unit].unit, &windows)?;
            for &stream in streams {
                domain.attach(stream)?;
            }
            Ok(domain)
        });
        adopted.map_err(|err| {
            for &stream in streams {
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

/// Record that `node`'s owner admitted as `generation` was refused its first
/// carve: `holder`, another node's owner, holds their isolation group.
fn audit_group_refused(audit: &(dyn Sink + Sync), node: u32, generation: u64, holder: &Owner) {
    emit(
        audit,
        Level::Warn,
        AuditEvent::DmaGroupRefused,
        &[
            Field {
                key: "node",
                value: FieldValue::UnsignedInt(u64::from(node)),
            },
            Field {
                key: "generation",
                value: FieldValue::UnsignedInt(generation),
            },
            Field {
                key: "group",
                value: FieldValue::UnsignedInt(u64::from(holder.identity.group)),
            },
            Field {
                key: "holder",
                value: FieldValue::UnsignedInt(u64::from(holder.node)),
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

fn stream_ids(streams: IommuStreams) -> core::ops::RangeInclusive<u32> {
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
