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
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use tairix_abi::driver::DriverBindKey;
use tairix_abi::sysinfo::{
    DmaFaultSignal, DmaGroupRecord, DmaNodeRecord, DmaOwnerState, DmaTables, DmaUnitFamily,
    DmaUnitRecord, DmaUnitState,
};
use tairix_abi::{
    DmaCoherence, Errno, HwMatchKey, HwNode, HwProperty, HwResource, HwResourceKind,
    IommuReservedWindow, IommuStreams, RegisterWindow, ReservedAccess, HW_NODE_MAX_RESOURCES,
};
use tairix_arch_api::PageTableFrames;
use tairix_collections::{HashMap, SmallVec};
use tairix_devmatch::{DriverCandidate, MatchResolution};
use tairix_hash::BuildFastHash;
use tairix_inline::ArrayVec;
use tairix_kernel_iommu_api::{
    Access, Clock, Domain, FrameRun, IdentityWindow, IommuError, IommuUnit, Signalling, Stage,
    TableCoherence, Tables, IO_PAGE_SIZE,
};
use tairix_kernel_mem::{AllocError, Chunks, DeviceTranslation, DmaCustody, DmaError, FrameBlock};
use tairix_log::{Field, FieldValue, Level, Sink};
use tairix_sync::SpinLock;

use crate::audit::{emit, AuditEvent};
use crate::hwtree::HwTreeSource;

mod deferred;
mod faults;
mod mastering;
mod remap;

pub use deferred::{Release, BATCH_CARVES, BATCH_WINDOW_NS};

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
    /// How it raises its faults.
    pub faults: FaultSignal,
    /// The family driving it.
    pub family: DmaUnitFamily,
    /// What its faults came to since boot.
    pub counts: FaultCounts,
}

/// What a unit's faults came to since boot, for an administrator: counts
/// alone, read and written relaxed.
#[derive(Debug, Default)]
pub struct FaultCounts {
    recorded: AtomicU64,
    dropped: AtomicU64,
    silenced: AtomicU64,
    /// A task drains them: set before it is admitted, cleared whenever their
    /// routing fails or the task stops.
    heard: AtomicBool,
}

impl FaultCounts {
    pub(crate) fn note_heard(&self, heard: bool) {
        self.heard.store(heard, Ordering::Relaxed);
    }

    pub(crate) fn heard(&self) -> bool {
        self.heard.load(Ordering::Relaxed)
    }

    pub(crate) fn note_recorded(&self) {
        self.recorded.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn note_dropped(&self) {
        self.dropped.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn note_silenced(&self) {
        self.silenced.fetch_add(1, Ordering::Relaxed);
    }
}

/// How a unit raises its faults.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FaultSignal {
    /// On the wired line its node names.
    Wired(WiredFaults),
    /// As a message the kernel gives it.
    Message,
    /// Nowhere the kernel hears: on a line the platform does not describe,
    /// the unit raising no message. It translates with its faults unrouted.
    Unheard,
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
    /// Its node does not say it snoops the CPU's caches, through which every
    /// family writes its tables and reads its queues and records.
    Unsnooped,
    /// Its family could not take it over or enable it.
    Unit(IommuError),
}

/// Whether a unit's node states that its own DMA snoops the CPU's caches, and
/// states nothing else.
fn snoops(node: &HwNode) -> bool {
    let mut stated = node
        .resources()
        .iter()
        .filter_map(HwResource::dma_coherence);
    stated.next() == Some(DmaCoherence::Snooped) && stated.all(|c| c == DmaCoherence::Snooped)
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
    let family = family_of(node)?;
    if !snoops(node) {
        return Err(Refusal::Unsnooped);
    }
    let mut reserved = Vec::new();
    reserved
        .try_reserve(node.resources().len())
        .map_err(|_| Refusal::Unit(IommuError::Exhausted))?;
    reserved.extend(
        node.resources()
            .iter()
            .filter_map(|r| r.iommu_reserved().ok()),
    );
    let faults = WiredFaults::of(node);
    // Serving faults routes them to the wired line the node names, or else
    // as a message; a family that must know is told as the unit is taken
    // over, the one time it may be.
    let signalling = if faults.is_some() {
        Signalling::Wired
    } else {
        Signalling::Message
    };
    let unit = match family {
        DmaUnitFamily::Unmatched => return Err(Refusal::Unmatched),
        DmaUnitFamily::Vtd => tairix_kernel_iommu_vtd::VtdUnit::new(
            first_window(node, env)?,
            env.frames,
            env.coherence,
            env.clock,
        )
        .and_then(kept),
        DmaUnitFamily::AmdVi => {
            let function = env.function.map(|function| (function, node.address()));
            tairix_kernel_iommu_amdvi::AmdViUnit::new(
                first_window(node, env)?,
                env.frames,
                env.clock,
                function,
            )
            .and_then(kept)
        }
        DmaUnitFamily::Smmuv3 => tairix_kernel_iommu_smmuv3::Smmuv3Unit::new(
            first_window(node, env)?,
            env.frames,
            env.clock,
        )
        .and_then(kept),
        DmaUnitFamily::Riscv => tairix_kernel_iommu_riscv::RiscvUnit::new(
            first_window(node, env)?,
            env.frames,
            env.coherence,
            env.clock,
            signalling,
        )
        .and_then(kept),
        DmaUnitFamily::VirtioPci => return virtio_function(node, env, reserved, faults),
        DmaUnitFamily::VirtioMmio => {
            let window = first_window(node, env)?;
            let transport = tairix_virtio::MmioTransport::new(window)
                .ok()
                .filter(|transport| {
                    transport
                        .window()
                        .read_u32(tairix_virtio::transport_mmio::regs::DEVICE_ID)
                        == Ok(tairix_kernel_iommu_virtio::DEVICE_ID)
                })
                .ok_or(Refusal::Unit(IommuError::Hardware))?;
            // A slot raises its interrupt on its line alone.
            tairix_kernel_iommu_virtio::VirtioIommuUnit::new(
                transport,
                env.frames,
                env.clock,
                None,
                Signalling::Wired,
            )
            .and_then(kept)
        }
    };
    Ok(Unit {
        node: node.id(),
        unit: unit.map_err(Refusal::Unit)?,
        reserved,
        faults: fault_signal(family, faults, || Ok(0)).map_err(Refusal::Unit)?,
        family,
        counts: FaultCounts::default(),
    })
}

/// The families a unit is matched to, and the name each is matched as.
const FAMILIES: [(DmaUnitFamily, &str); 6] = [
    (DmaUnitFamily::Vtd, "vtd"),
    (DmaUnitFamily::AmdVi, "amdvi"),
    (DmaUnitFamily::Smmuv3, "smmuv3"),
    (DmaUnitFamily::Riscv, "riscv"),
    (DmaUnitFamily::VirtioPci, "virtio-pci"),
    (DmaUnitFamily::VirtioMmio, "virtio-mmio"),
];

/// The family `node` is matched to.
///
/// # Errors
///
/// [`Refusal::Unmatched`] where none is.
fn family_of(node: &HwNode) -> Result<DmaUnitFamily, Refusal> {
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
        bind(key(tairix_kernel_iommu_virtio::COMPATIBLE)?),
        bind(key(tairix_virtio::transport_mmio::COMPATIBLE.as_bytes())?),
    ];
    let candidates: [DriverCandidate<'_>; FAMILIES.len()] =
        core::array::from_fn(|at| DriverCandidate {
            path: FAMILIES[at].1,
            bind_keys: &keys[at],
        });
    match tairix_devmatch::resolve(node.match_keys(), &candidates) {
        MatchResolution::Winner { candidate, .. } => Ok(FAMILIES[candidate].0),
        _ => Err(Refusal::Unmatched),
    }
}

/// The first register window `node` names, mapped for the kernel.
fn first_window(node: &HwNode, env: &UnitEnv<'_>) -> Result<RegisterWindow, Refusal> {
    let window = node
        .resources()
        .iter()
        .find(|r| r.kind() == Some(HwResourceKind::Mmio))
        .ok_or(Refusal::NoRegisters)?;
    map_registers(env, window.base(), window.length())
}

/// The unit's registers at `[base, base + length)`, mapped uncached for the
/// kernel alone.
fn map_registers(env: &UnitEnv<'_>, base: u64, length: u64) -> Result<RegisterWindow, Refusal> {
    let len = usize::try_from(length).map_err(|_| Refusal::NoRegisters)?;
    base.checked_add(length).ok_or(Refusal::NoRegisters)?;
    let registers = (env.mmio)(base, len).ok_or(Refusal::NoRegisters)?;
    // SAFETY: the port mapped exactly `len` bytes of the unit's register set
    // uncached for the kernel's life, and the kernel maps a unit's registers
    // nowhere else (no process may map them), so this window is their only
    // owner.
    Ok(unsafe { RegisterWindow::from_mapping(base, registers, len) })
}

/// Take over the virtio-iommu that is the PCI function `node` names: its
/// four configuration windows, which discovery placed on the node and so
/// guards from every process, mapped for the kernel; its own bus mastering
/// on, its queues being its DMA; and its interrupt raised on the line
/// `faults` names, else through its MSI-X entry where it has one, else on its
/// pin, which nothing hears.
fn virtio_function(
    node: &HwNode,
    env: &UnitEnv<'_>,
    reserved: Vec<IommuReservedWindow>,
    faults: Option<WiredFaults>,
) -> Result<Unit, Refusal> {
    let function = env.function.ok_or(Refusal::NoRegisters)?;
    let address = node.address();
    let faults = fault_signal(DmaUnitFamily::VirtioPci, faults, || {
        function.msix_entries(address)
    })
    .map_err(Refusal::Unit)?;
    let signalling = if faults == FaultSignal::Message {
        Signalling::Message
    } else {
        Signalling::Wired
    };
    let regions = function.virtio_windows(address).map_err(Refusal::Unit)?;
    let guarded = |(base, len): (u64, usize)| {
        let end = base.saturating_add(len as u64);
        register_windows(node).any(|window| window.start <= base && end <= window.end)
    };
    let all = [regions.common, regions.notify, regions.isr, regions.device];
    if !all.into_iter().all(guarded) {
        return Err(Refusal::NoRegisters);
    }
    let map = |(base, len): (u64, usize)| map_registers(env, base, len as u64);
    let windows = tairix_virtio::PciTransportWindows {
        common: map(regions.common)?,
        notify: map(regions.notify)?,
        isr: map(regions.isr)?,
        device: map(regions.device)?,
        notify_off_multiplier: regions.notify_off_multiplier,
    };
    let entry =
        (signalling == Signalling::Message).then_some(tairix_kernel_iommu_virtio::MSIX_ENTRY);
    let mut transport = tairix_virtio::PciTransport::new(windows, entry)
        .map_err(|_| Refusal::Unit(IommuError::Hardware))?;
    // Whatever firmware left its queues pointing at is forgotten before the
    // function may master anything.
    tairix_virtio::transport::Transport::reset(&mut transport)
        .map_err(|_| Refusal::Unit(IommuError::Hardware))?;
    function.set_master(address, true).map_err(Refusal::Unit)?;
    let unit = tairix_kernel_iommu_virtio::VirtioIommuUnit::new(
        transport,
        env.frames,
        env.clock,
        Some((function, address)),
        signalling,
    )
    .and_then(kept)
    .map_err(|err| {
        // The device was given up on: nothing of the kernel's is its to reach.
        let _ = function.set_master(address, false);
        Refusal::Unit(err)
    })?;
    Ok(Unit {
        node: node.id(),
        unit,
        reserved,
        faults,
        family: DmaUnitFamily::VirtioPci,
        counts: FaultCounts::default(),
    })
}

/// How a unit of `family` raises its faults: on the line its node names,
/// else as a message — a virtio-iommu function only through the MSI-X entry
/// `msix_entries` says it has, a virtio-mmio slot never, its one line being
/// all it has — else on a line nothing hears.
fn fault_signal(
    family: DmaUnitFamily,
    wired: Option<WiredFaults>,
    msix_entries: impl FnOnce() -> Result<u16, IommuError>,
) -> Result<FaultSignal, IommuError> {
    Ok(match (wired, family) {
        (Some(wired), _) => FaultSignal::Wired(wired),
        (None, DmaUnitFamily::VirtioPci)
            if msix_entries()? > tairix_kernel_iommu_virtio::MSIX_ENTRY =>
        {
            FaultSignal::Message
        }
        (None, DmaUnitFamily::VirtioPci | DmaUnitFamily::VirtioMmio) => FaultSignal::Unheard,
        (None, _) => FaultSignal::Message,
    })
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
    /// It translates, keeping its domains' translations where named; what
    /// telling the functions behind it found mastering DMA as it took over,
    /// though firmware keeps no window for them, to stop came to.
    Translating(Quiesced, Tables),
    /// It translates nothing, for the refusal named, so every function
    /// behind it was told to stop, firmware's windows kept for none.
    Stranded(Refusal, Quiesced),
}

/// A discovered unit that translates nothing.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Stranded {
    /// Its node.
    pub node: u32,
    /// The family its node matched, [`DmaUnitFamily::Unmatched`] for none.
    pub family: DmaUnitFamily,
    /// Why it translates nothing.
    pub refusal: Refusal,
}

impl Stranded {
    /// The unit at `node`, which translates nothing for `refusal`: a unit no
    /// family drives names none.
    #[must_use]
    pub fn of(node: &HwNode, refusal: Refusal) -> Self {
        let family = match refusal {
            Refusal::Unmatched => Err(refusal),
            Refusal::NoRegisters | Refusal::Unsnooped | Refusal::Unit(_) => family_of(node),
        };
        Self {
            node: node.id(),
            family: family.unwrap_or(DmaUnitFamily::Unmatched),
            refusal,
        }
    }
}

/// How a node's DMA reaches memory.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DmaPath {
    /// Through units translating here: its carves map into its domain, from
    /// frames wholly below `output_limit`.
    Translated {
        /// The exclusive physical address every unit it crosses can name up
        /// to.
        output_limit: u64,
    },
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

/// The doorbells one node's interrupt messages are written to.
type Doorbells = ArrayVec<Range<u64>, HW_NODE_MAX_RESOURCES>;

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
    /// Where its interrupt messages are written, which its domain maps so
    /// they arrive.
    doorbells: Doorbells,
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
    /// The discovered units that translate nothing, and why.
    stranded: Vec<Stranded>,
    /// Every discovered unit's register window, whatever became of the unit.
    guarded: Vec<Range<u64>>,
    /// What the platform's PCI segments decode, in whole pages, ascending and
    /// apart: no domain hands out an IOVA there.
    peers: Vec<Range<u64>>,
    tree: &'static dyn HwTreeSource,
    audit: &'static (dyn Sink + Sync),
    mastering: Option<Mastering>,
    owners: SpinLock<Owners>,
    /// Streams no owner holds, each keeping its firmware windows.
    firmware: SpinLock<HashMap<(usize, u32), Domain<'static>, BuildFastHash>>,
    /// Every unit's remapping table is built.
    remap_prepared: AtomicBool,
    /// Carves freed while their owners live, awaiting the invalidation that
    /// confirms them gone.
    batch: SpinLock<deferred::Batch>,
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
    /// The last admission refused its group, audited once each for the same
    /// reason.
    refused: AtomicU64,
}

/// No admission's generation: they count up from the kernel's.
const NONE_REFUSED: u64 = u64::MAX;

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
        mut refused: Vec<Stranded>,
        guarded: Vec<Range<u64>>,
        tree: &'static dyn HwTreeSource,
        audit: &'static (dyn Sink + Sync),
        mastering: Option<Mastering>,
        report: &mut dyn FnMut(u32, UnitOutcome),
    ) -> Self {
        let strand =
            |node| mastering.map_or_else(Quiesced::default, |m| m.quiesce(node, &|_| false));
        for &Stranded { node, refusal, .. } in &refused {
            report(node, UnitOutcome::Stranded(refusal, strand(node)));
        }
        // Room for every unit that may not enable, so each is listed; one the
        // heap could not make room for is still reported.
        let _ = refused.try_reserve(units.len());
        let mut translation = Self {
            discovered: refused.len() + units.len(),
            stranded: refused,
            units,
            guarded,
            peers: Vec::new(),
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
            batch: SpinLock::new(deferred::Batch::new()),
        };
        // A unit that does not enable leaves the list in place, so the units
        // after it, whose firmware domains are not yet made, keep their index.
        let mut index = 0;
        while let Some(unit) = translation.units.get(index) {
            let (node, enable, family) = (unit.node, unit.unit, unit.family);
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
                    UnitOutcome::Translating(quiesced, enable.profile().tables)
                }
                Err(err) => {
                    translation
                        .firmware
                        .lock()
                        .retain(|&(unit, _), _| unit != index);
                    translation.units.remove(index);
                    if translation.stranded.try_reserve(1).is_ok() {
                        translation.stranded.push(Stranded {
                            node,
                            family,
                            refusal: Refusal::Unit(err),
                        });
                    }
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

    /// The translation, handing no domain an IOVA in `windows`: what the
    /// platform's PCI segments decode, where a device's DMA may reach a peer
    /// before its unit sees it. Each is widened to whole pages.
    #[must_use]
    pub fn avoiding(mut self, mut windows: Vec<Range<u64>>) -> Self {
        let last_page = !(IO_PAGE_SIZE - 1);
        for window in &mut windows {
            window.start &= last_page;
            window.end = window
                .end
                .checked_next_multiple_of(IO_PAGE_SIZE)
                .unwrap_or(last_page);
        }
        windows.retain(|window| window.start < window.end);
        windows.sort_unstable_by_key(|window| window.start);
        windows.dedup_by(|later, kept| {
            let joins = later.start <= kept.end;
            if joins {
                kept.end = kept.end.max(later.end);
            }
            joins
        });
        self.peers = windows;
        self
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
            Vec::new(),
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
            let Some(unit) = self.unit_index(streams.unit()) else {
                return DmaPath::Stranded {
                    unit: Some(streams.unit()),
                };
            };
            let reach = self.units[unit].unit.profile().reach.output_limit();
            path = match path {
                DmaPath::Translated { output_limit } => DmaPath::Translated {
                    output_limit: output_limit.min(reach),
                },
                _ => DmaPath::Translated {
                    output_limit: reach,
                },
            };
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
            doorbells: ArrayVec::new(),
        };
        for resource in entry.resources() {
            if resource.kind() == Some(HwResourceKind::MsiDoorbell) {
                let window = resource
                    .doorbell_window()
                    .map_err(|_| DmaError::Translation)?;
                identity
                    .doorbells
                    .try_push(window)
                    .map_err(|_| DmaError::Translation)?;
                continue;
            }
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

    /// What unit `unit` claims for any of `streams`: IOVAs a domain
    /// translating them hands out none of.
    fn claimed(&self, unit: usize, streams: &[u32]) -> Result<Vec<Range<u64>>, IommuError> {
        let mut claimed = Vec::new();
        let mut room = true;
        for &stream in streams {
            self.units[unit].unit.reserved_iova(stream, &mut |range| {
                room &= claimed.try_reserve(1).is_ok();
                if room {
                    claimed.push(range);
                }
            })?;
        }
        if room {
            Ok(claimed)
        } else {
            Err(IommuError::Exhausted)
        }
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
        let Ok(claimed) = self.claimed(unit, &[stream]) else {
            return;
        };
        let Ok(mut domain) = Domain::new(self.units[unit].unit, &windows, &claimed) else {
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
        if holder.refused.swap(generation, Ordering::Relaxed) != generation {
            audit_group_refused(self.audit, node, generation, holder);
        }
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
                refused: AtomicU64::new(NONE_REFUSED),
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
            match self.adopt(
                owner.identity.unit,
                &owner.streams,
                owner.identity.doorbells.as_slice(),
            ) {
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
    /// keep and `doorbells`, their firmware domains given up for it. Nothing
    /// of the carve that asked is mapped yet, so a failure leaves no memory
    /// unconfirmed, and every stream it leaves blocked gets its firmware
    /// domain back.
    fn adopt(
        &self,
        unit: usize,
        streams: &[u32],
        doorbells: &[Range<u64>],
    ) -> Result<Domain<'static>, DmaError> {
        let adopted = self.windows(unit, streams).and_then(|mut windows| {
            let mut claimed = self.claimed(unit, streams)?;
            claimed
                .try_reserve(self.peers.len())
                .map_err(|_| IommuError::Exhausted)?;
            claimed.extend_from_slice(&self.peers);
            let intercepted = self.units[unit].unit.message_files().is_some();
            if intercepted {
                // The unit takes a write to the doorbell as a message, so the
                // domain only keeps the page clear of carves.
                claimed
                    .try_reserve(doorbells.len())
                    .map_err(|_| IommuError::Exhausted)?;
                claimed.extend_from_slice(doorbells);
            } else {
                // A doorbell takes writes alone, read access coming with it
                // only where the unit's tables cannot leave it out.
                let access = if self.units[unit].unit.profile().write_only {
                    Access::WRITE
                } else {
                    Access::READ_WRITE
                };
                windows
                    .try_reserve(doorbells.len())
                    .map_err(|_| IommuError::Exhausted)?;
                windows.extend(doorbells.iter().map(|range| IdentityWindow {
                    range: range.clone(),
                    access,
                }));
            }
            for &stream in streams {
                let firmware = self.firmware.lock().remove(&(unit, stream));
                if let Some(domain) = firmware {
                    domain.destroy()?;
                }
            }
            let mut domain = Domain::new(self.units[unit].unit, &windows, &claimed)?;
            let mut attached = false;
            for &stream in streams {
                match domain.attach(stream) {
                    Ok(()) => attached = true,
                    Err(IommuError::NoEndpoint) => {}
                    Err(err) => return Err(err),
                }
            }
            // A node none of whose streams the unit has is not behind it, and
            // its carves' addresses would be taken as physical.
            if attached {
                Ok(domain)
            } else {
                Err(IommuError::NoEndpoint)
            }
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

/// The DMA translation units a malformed firmware table described: the
/// family it describes, and whether the PCI functions it would have described
/// were withheld or, by the administrator's choice, published untranslated.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct MalformedUnits {
    /// The family the table describes units of.
    pub family: DmaUnitFamily,
    /// The functions were published untranslated, their DMA unconfined.
    pub unconfined: bool,
}

impl MalformedUnits {
    /// The units' one listing: under the tree's root, which is all a
    /// malformed table leaves to name them by.
    #[must_use]
    pub const fn record(self) -> DmaUnitRecord {
        DmaUnitRecord {
            node: tairix_abi::hwtree::HW_NODE_ROOT_ID,
            family: self.family,
            state: if self.unconfined {
                DmaUnitState::Unconfined
            } else {
                DmaUnitState::Withheld
            },
            faults: DmaFaultSignal::None,
            tables: DmaTables::None,
            owners: 0,
            firmware_streams: 0,
            faults_recorded: 0,
            faults_dropped: 0,
            streams_silenced: 0,
        }
    }

    /// Record what the table came to on `audit`.
    pub fn audit(self, audit: &(dyn Sink + Sync)) {
        let record = self.record();
        emit(
            audit,
            if self.unconfined {
                Level::Error
            } else {
                Level::Warn
            },
            AuditEvent::DmaUnitsMalformed,
            &[
                Field {
                    key: "family",
                    value: FieldValue::Str(record.family.name()),
                },
                Field {
                    key: "outcome",
                    value: FieldValue::Str(record.state.name()),
                },
            ],
        );
    }
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

impl Translation {
    /// Up to `max` packed [`DmaUnitRecord`]s from the `first`th: the units
    /// translating, in discovery order, then those stranded.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfMemory`] when the page cannot be built.
    pub fn unit_records(&self, first: u64, max: usize) -> Result<Vec<u8>, Errno> {
        let per_unit = || {
            let mut counts = Vec::new();
            counts
                .try_reserve_exact(self.units.len())
                .map_err(|_| Errno::OutOfMemory)?;
            counts.resize(self.units.len(), 0_usize);
            Ok::<_, Errno>(counts)
        };
        let mut live = per_unit()?;
        let mut kept = per_unit()?;
        // Each owner's state is read with no other lock held, as an adoption
        // takes the firmware domains under it.
        for owner in self.owner_snapshot()? {
            if matches!(*owner.state.lock(), OwnerState::Live(_)) {
                if let Some(count) = live.get_mut(owner.identity.unit) {
                    *count += 1;
                }
            }
        }
        for &(unit, _) in self.firmware.lock().keys() {
            if let Some(count) = kept.get_mut(unit) {
                *count += 1;
            }
        }
        let translating =
            self.units
                .iter()
                .zip(live.iter().zip(&kept))
                .map(|(unit, (&owners, &firmware))| Listed::Translating {
                    unit,
                    owners,
                    firmware,
                });
        let listed = translating.chain(self.stranded.iter().map(Listed::Stranded));
        pack(listed, first, max, |entry| {
            match entry {
                Listed::Translating {
                    unit,
                    owners,
                    firmware,
                } => DmaUnitRecord {
                    node: unit.node,
                    family: unit.family,
                    state: DmaUnitState::Translating,
                    faults: match unit.faults {
                        _ if !unit.counts.heard() => DmaFaultSignal::Unheard,
                        FaultSignal::Wired(_) => DmaFaultSignal::Wired,
                        FaultSignal::Message => DmaFaultSignal::Message,
                        FaultSignal::Unheard => DmaFaultSignal::Unheard,
                    },
                    tables: match unit.unit.profile().tables {
                        Tables::Walked(Stage::First) => DmaTables::FirstStage,
                        Tables::Walked(Stage::Second) => DmaTables::SecondStage,
                        Tables::Kept => DmaTables::Kept,
                    },
                    owners: u32::try_from(owners).unwrap_or(u32::MAX),
                    firmware_streams: u32::try_from(firmware).unwrap_or(u32::MAX),
                    faults_recorded: unit.counts.recorded.load(Ordering::Relaxed),
                    faults_dropped: unit.counts.dropped.load(Ordering::Relaxed),
                    streams_silenced: unit.counts.silenced.load(Ordering::Relaxed),
                },
                Listed::Stranded(stranded) => DmaUnitRecord {
                    node: stranded.node,
                    family: stranded.family,
                    state: match stranded.refusal {
                        Refusal::Unmatched => DmaUnitState::Unmatched,
                        Refusal::NoRegisters => DmaUnitState::NoRegisters,
                        Refusal::Unsnooped => DmaUnitState::Unsnooped,
                        Refusal::Unit(_) => DmaUnitState::Failed,
                    },
                    faults: DmaFaultSignal::None,
                    tables: DmaTables::None,
                    owners: 0,
                    firmware_streams: 0,
                    faults_recorded: 0,
                    faults_dropped: 0,
                    streams_silenced: 0,
                },
            }
            .to_le_bytes()
        })
    }

    /// Up to `max` packed [`DmaGroupRecord`]s from the `first`th, ascending
    /// by unit node and group: each group an owner has taken, and its holder.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfMemory`] when the page cannot be built.
    pub fn group_records(&self, first: u64, max: usize) -> Result<Vec<u8>, Errno> {
        let mut groups = Vec::new();
        {
            let owners = self.owners.lock();
            groups
                .try_reserve_exact(owners.groups.len())
                .map_err(|_| Errno::OutOfMemory)?;
            groups.extend(owners.groups.iter().filter_map(|(&(unit, group), owner)| {
                Some((self.units.get(unit)?.node, group, Arc::clone(owner)))
            }));
        }
        groups.sort_unstable_by_key(|&(unit, group, _)| (unit, group));
        pack(groups.iter(), first, max, |(unit, group, owner)| {
            DmaGroupRecord {
                unit: *unit,
                group: *group,
                holder: owner.node,
                state: owner.state.lock().standing(),
                generation: owner.generation,
            }
            .to_le_bytes()
        })
    }

    /// Up to `max` packed [`DmaNodeRecord`]s from the `first`th, ascending
    /// by node: each node a unit translates for an owner.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfMemory`] when the page cannot be built.
    pub fn node_records(&self, first: u64, max: usize) -> Result<Vec<u8>, Errno> {
        let mut owners = self.owner_snapshot()?;
        owners.sort_unstable_by_key(|owner| owner.node);
        let translated = owners
            .iter()
            .filter_map(|owner| Some((owner, self.units.get(owner.identity.unit)?.node)));
        pack(translated, first, max, |(owner, unit)| {
            let state = owner.state.lock();
            let (mappings, mapped_bytes) = match &*state {
                OwnerState::Live(domain) => (
                    u32::try_from(domain.mapped()).unwrap_or(u32::MAX),
                    domain.mapped_bytes(),
                ),
                _ => (0, 0),
            };
            DmaNodeRecord {
                node: owner.node,
                unit,
                group: owner.identity.group,
                state: state.standing(),
                streams: u16::try_from(owner.streams.len()).unwrap_or(u16::MAX),
                generation: owner.generation,
                mappings,
                mapped_bytes,
            }
            .to_le_bytes()
        })
    }

    /// Every node's latest owner, taken out from under the owners' lock so
    /// each owner's own state is read without it.
    fn owner_snapshot(&self) -> Result<Vec<Arc<Owner>>, Errno> {
        let owners = self.owners.lock();
        let mut snapshot = Vec::new();
        snapshot
            .try_reserve_exact(owners.nodes.len())
            .map_err(|_| Errno::OutOfMemory)?;
        snapshot.extend(owners.nodes.values().cloned());
        Ok(snapshot)
    }
}

impl OwnerState {
    /// Where the owner stands, as an administrator reads it.
    const fn standing(&self) -> DmaOwnerState {
        match self {
            Self::Adopting => DmaOwnerState::Adopting,
            Self::Live(_) => DmaOwnerState::Live,
            Self::Unadopted(_) => DmaOwnerState::Unadopted,
            Self::Revoked { confirmed: true } => DmaOwnerState::Ended,
            Self::Revoked { confirmed: false } => DmaOwnerState::Unconfirmed,
        }
    }
}

/// A unit as the administrator's listing reads it.
enum Listed<'t> {
    /// Translating, with how many live owners and firmware streams it has.
    Translating {
        unit: &'t Unit,
        owners: usize,
        firmware: usize,
    },
    Stranded(&'t Stranded),
}

/// Up to `max` of `items` from the `first`th, packed end to end, each
/// encoded only once it is in the page.
fn pack<T, const N: usize>(
    items: impl Iterator<Item = T>,
    first: u64,
    max: usize,
    mut encode: impl FnMut(T) -> [u8; N],
) -> Result<Vec<u8>, Errno> {
    let skip = usize::try_from(first).unwrap_or(usize::MAX);
    let page = items.skip(skip).take(max);
    let mut out = Vec::new();
    let held = page.size_hint().1.unwrap_or(max);
    out.try_reserve_exact(held.saturating_mul(N))
        .map_err(|_| Errno::OutOfMemory)?;
    for item in page {
        out.try_reserve(N).map_err(|_| Errno::OutOfMemory)?;
        out.extend_from_slice(&encode(item));
    }
    Ok(out)
}

// A frame block's order is the order of the I/O pages it maps as.
const _: () = assert!(tairix_kernel_mem::PAGE_SIZE as u64 == tairix_kernel_iommu_api::IO_PAGE_SIZE);

impl DeviceTranslation for Translation {
    fn map(
        &self,
        node: u32,
        generation: u64,
        blocks: &[FrameBlock],
        limit: u64,
    ) -> Result<u64, DmaError> {
        let mut runs = SmallVec::<FrameRun, 1>::try_with_capacity(blocks.len())
            .map_err(|_| DmaError::Alloc(AllocError::OutOfMemory))?;
        for block in blocks {
            // Room for every block was had above.
            let _ = runs.try_push(FrameRun {
                phys: block.frame.start().as_u64(),
                order: block.order,
            });
        }
        let owner = self.owner(node, generation)?;
        let mut state = owner.state.lock();
        let result = match &mut *state {
            OwnerState::Live(domain) => domain.map(&runs, limit).map_err(dma_error),
            OwnerState::Unadopted(err) => Err(*err),
            OwnerState::Adopting | OwnerState::Revoked { .. } => Err(DmaError::DeviceGone),
        };
        drop(state);
        self.note(node, &owner, result)
    }

    fn unmap(&self, node: u32, generation: u64, iova: u64) -> Result<(), DmaError> {
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

    fn defer_free(
        &self,
        node: u32,
        generation: u64,
        iova: u64,
        blocks: &mut Chunks,
    ) -> Result<bool, DmaError> {
        let owner = self.owners.lock().nodes.get(&node).cloned();
        // Another generation's carves were confirmed gone with its end.
        match owner.filter(|owner| owner.generation == generation) {
            Some(owner) => self.defer(node, &owner, iova, blocks),
            None => Ok(false),
        }
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
    fn hold(&self, _node: u32, _generation: u64, _block: FrameBlock) {}
}

#[cfg(test)]
mod tests;
