//! The generic flattened-device-tree → hardware-tree walk every FDT-based
//! port shares.
//!
//! Turning a device tree into [`HwNode`]s is the same job on every such
//! port: emit the root, walk the tree in document order tracking each
//! depth's bus cells and `ranges` ([`tairix_fdt::bus`]), classify each node,
//! decode its `compatible` list into match keys, translate its `reg`
//! windows, and splice out the nodes no driver could ever bind. Only two
//! things genuinely differ, and [`FdtPlatform`] is exactly those two:
//!
//! * **the interrupt specifier** — its cell width and the mapping from its
//!   cells to the line number a granted driver binds (the GIC's type-relative
//!   SPI/PPI offsets, the PLIC's bare source number);
//! * **board augmentation** — extra resources only the platform's own tree
//!   can describe (a firmware mailbox's DMA carve, a root complex's
//!   windows, a vendor spelling of a generic property). Ports with none
//!   leave the defaults.
//!
//! A port therefore contributes an [`FdtPlatform`] impl and spells its
//! discovery type as [`FdtDiscovery`] over it; the walk itself has one
//! definition. The impl is a *value* built from the tree once
//! ([`FdtPlatform::from_tree`]), so a port whose interrupt mapping depends
//! on a tree-wide fact — the RISC-V PLIC's `riscv,ndev` source count — reads
//! it before the walk rather than per node.
//!
//! The generic DMA binding is read here for every port: a node with
//! `#dma-cells` is a controller carrying a [`LinkDuty`] and its
//! bus's DMA windows, and each `dmas` entry of a consumer becomes a
//! [`LinkRequest`] naming the controller's endpoint (`plans/SOUND.md`
//! SND5).
//!
//! So is the generic IOMMU binding (`plans/IOMMU.md` IOM14): a node with
//! `#iommu-cells` is a translation unit, and a master's `iommus` names the
//! streams it masters DMA through, which also makes it a DMA master carrying
//! its bus's windows. A master whose translation the tree cannot describe is
//! given no DMA authority at all, never an untranslated one.
//!
//! Only nodes a consumer may use are walked ([`Fdt::operational_nodes`]): a
//! disabled, reserved or failed node, and everything below it, is spliced out
//! with the nodes the matcher could never bind.

use tairix_abi::driver::clock::CLOCK_CONTROLLER_ENDPOINTS;
use tairix_abi::driver::codec::{ClockInversion, DaiFormat, DaiLink, CODEC_ENDPOINTS};
use tairix_abi::driver::dmaengine::DMA_CONTROLLER_ENDPOINTS;
use tairix_abi::driver::net::MAC_ADDRESS_LEN;
use tairix_abi::hwlink::{LinkDuty, LinkRequest, LinkRole, LINK_NAME_MAX, LINK_SELECTOR_MAX_CELLS};
use tairix_abi::hwtree::BUS_CHILD_ENDPOINTS;
use tairix_abi::{
    DmaCoherence, HwDeviceClass, HwMatchKey, HwNode, HwProperty, HwResource, IommuStreams,
    HW_NODE_MAX_RESOURCES, HW_NODE_ROOT, HW_NODE_ROOT_ID,
};
use tairix_fdt::iommu::{iommu_cells, stream_id};
use tairix_fdt::{
    bus_level, dma_reach, name_stem, phandle_args, phandle_ref, read_cells, reg_entry_count,
    translated_reg, BusLevel, Fdt, Node, OperationalNodes, Property, MAX_WALK_DEPTH,
};

use crate::platform::{DiscoveryError, HwNodeSink, PlatformDiscovery};

/// Bytes in one device-tree cell.
const CELL_BYTES: usize = 4;

/// The id the walk gives the first node it emits after the root.
const FIRST_EMITTED_ID: u32 = HW_NODE_ROOT_ID + 1;

/// The per-port half of the device-tree walk.
pub trait FdtPlatform {
    /// Cells in one `interrupts` specifier on the root interrupt controller
    /// the tree describes (three for a GIC, one for a PLIC, two for an
    /// APLIC).
    fn interrupt_cells(&self) -> usize;

    /// Read whatever tree-wide facts the interrupt mapping needs, once,
    /// before the walk starts.
    fn from_tree(fdt: &Fdt<'_>) -> Self;

    /// Map one whole specifier — exactly [`Self::interrupt_cells`] cells — to the
    /// line number a granted driver binds, or `None` for a specifier this
    /// port cannot represent or its controller cannot raise.
    ///
    /// A `None` drops that specifier and leaves the rest of the list; the
    /// walk never guesses a line.
    fn interrupt_line(&self, specifier: &[u8]) -> Option<u32>;

    /// Whether the line `specifier` names is raised by edges, which its grant
    /// then says so the controller latches a pulse while the line is masked.
    /// A controller whose specifiers name no trigger signals by level.
    fn edge_triggered(&self, _specifier: &[u8]) -> bool {
        false
    }

    /// The phandle of the controller [`Self::interrupt_line`] decodes
    /// specifiers for, read from the tree once.
    ///
    /// A node whose effective interrupt parent is any other controller keeps
    /// none of its specifiers: decoding them as this controller's would grant
    /// its driver another device's line.
    fn root_interrupt_controller(&self) -> Option<u32>;

    /// The channels DMA controller `node` leaves to this system, numbered
    /// from the node's own first channel, when the tree states them.
    ///
    /// The default reads the generic [`dma_channel_mask`]; a port whose
    /// vendor binding states the mask another way overrides it.
    fn dma_channel_mask(
        &self,
        node: &Node<'_>,
        _depth: usize,
        _levels: &[BusLevel<'_>],
    ) -> Option<u64> {
        dma_channel_mask(node)
    }

    /// Push any resource only this platform's tree can describe onto a node
    /// the walk has already built, whose DMA is `coherence` wherever it
    /// masters any. Ports with no board augmentation leave the default.
    fn augment(
        &self,
        _node: &Node<'_>,
        _depth: usize,
        _levels: &[BusLevel<'_>],
        _coherence: DmaCoherence,
        _hw: &mut HwNode,
    ) {
    }

    /// How a master's DMA meets the CPU's caches where neither it nor any
    /// node above it says: the architecture's devicetree convention.
    const DEFAULT_DMA_COHERENCE: DmaCoherence;

    /// Whether the kernel drives `node`'s device itself — the port's
    /// interrupt controllers — so the walk marks it
    /// [`HwProperty::KernelDriven`] and no driver is loaded for it. The walk
    /// marks a generic ECAM PCI host so on every port.
    fn kernel_driven(&self, _node: &Node<'_>) -> bool {
        false
    }
}

/// A DMA controller's generic `dma-channel-mask`: bit `n` for its channel
/// `n`, one cell per thirty-two channels, lowest channels first. `None` when
/// the property is absent or not one or two whole cells.
#[must_use]
pub fn dma_channel_mask(node: &Node<'_>) -> Option<u64> {
    let property = node.property("dma-channel-mask")?;
    let low = u64::from(property.read_be_u32(0).ok()?);
    match property.value().len() {
        4 => Some(low),
        8 => Some(low | (u64::from(property.read_be_u32(4).ok()?) << 32)),
        _ => None,
    }
}

/// The [`PlatformDiscovery`] implementation over a validated device tree,
/// parameterised by the port's [`FdtPlatform`].
pub struct FdtDiscovery<'a, P> {
    fdt: Fdt<'a>,
    platform: P,
}

impl<'a, P: FdtPlatform> FdtDiscovery<'a, P> {
    /// Wrap an already-validated [`Fdt`] reader, reading the port's
    /// tree-wide facts from it once.
    #[must_use]
    pub fn new(fdt: Fdt<'a>) -> Self {
        let platform = P::from_tree(&fdt);
        Self { fdt, platform }
    }
}

impl<P: FdtPlatform> PlatformDiscovery for FdtDiscovery<'_, P> {
    fn discover(&self, sink: &mut dyn HwNodeSink) -> Result<(), DiscoveryError> {
        // Root first so every later node's parent is already emitted. Its
        // id is the shared `HW_NODE_ROOT_ID`; its parent is the
        // `HW_NODE_ROOT` sentinel, so it alone is `is_root`.
        sink.emit(HwNode::new(
            HW_NODE_ROOT_ID,
            HW_NODE_ROOT,
            HwDeviceClass::Root,
        ))?;
        let mut next_id = FIRST_EMITTED_ID;
        // The shared per-depth bus state plus this walk's own per-depth
        // facts: the hardware-tree id of the nearest *emitted* ancestor,
        // which is the parent a child at depth + 1 names, and the
        // bus-child bookkeeping of the node at that depth — how many
        // duties its resource list took, and how many of its addressed
        // children have been reached. A tree nested beyond the tracked
        // depth is refused as malformed rather than silently
        // under-enumerated.
        let mut levels = [BusLevel::DEFAULT; MAX_WALK_DEPTH];
        let mut ancestors = [0u32; MAX_WALK_DEPTH];
        let mut duties = [0usize; MAX_WALK_DEPTH];
        let mut children_seen = [0usize; MAX_WALK_DEPTH];
        // The interrupt parent the children of the node at each depth inherit.
        let mut interrupt_parents = [None; MAX_WALK_DEPTH];
        let mut stated = StatedCoherence::new();

        let mut nodes = self.fdt.operational_nodes();
        while let Some(node) = nodes.next() {
            // Cloned before anything else so the look-ahead below starts
            // exactly where this node's subtree does.
            let subtree = nodes.clone();
            let node = node.map_err(|_| DiscoveryError::MalformedSource)?;
            let depth = node.depth() as usize;
            if depth >= MAX_WALK_DEPTH {
                return Err(DiscoveryError::MalformedSource);
            }

            // This node's own cell counts and `ranges` govern its
            // *children*; record them whether or not the node is emitted.
            let mut level = bus_level(&node);
            if depth == 0 {
                level.ranges = None;
                levels[0] = level;
                ancestors[0] = HW_NODE_ROOT_ID;
                duties[0] = 0;
                children_seen[0] = 0;
                interrupt_parents[0] =
                    children_interrupt_parent(&node, own_interrupt_parent(&node, None));
                stated.reach(&node, 0);
                continue;
            }

            let interrupt_parent = own_interrupt_parent(&node, interrupt_parents[depth - 1]);
            let coherence = stated
                .reach(&node, depth)
                .unwrap_or(P::DEFAULT_DMA_COHERENCE);
            let mut ancestor = ancestors[depth - 1];
            let mut accepted = 0;
            let placed = Placement {
                depth,
                parent: ancestor,
                id: next_id,
                interrupt_parent,
                coherence,
            };
            if let Some(mut emitted) = self.build_node(&node, &levels, placed) {
                // A child of an addressed, non-enumerable bus carries the
                // *authority* half of its existence: an endpoint grant
                // naming the id its bus driver will serve it on. The index
                // counts only *emitted* children, exactly as the duty
                // look-ahead did, and a child past what the bus node could
                // hold gets none — so the two halves can never disagree.
                if is_bus_child(&node, depth, &levels) {
                    let index = children_seen[depth - 1];
                    children_seen[depth - 1] = index + 1;
                    if index < duties[depth - 1] {
                        let _ = emitted.push_resource(HwResource::endpoint(
                            BUS_CHILD_ENDPOINTS.endpoint(next_id),
                        ));
                    }
                }
                // The *duty* half: this node's own children, if it is such
                // a bus. Their ids are the ones this walk is about to
                // assign, so they are read ahead here — the emitted parent
                // cannot be amended once the sink has it.
                if declares_addressed_bus(&level) {
                    accepted = push_bus_child_duties(subtree, depth, next_id, &mut emitted);
                }
                sink.emit(emitted)?;
                ancestor = next_id;
                next_id = next_id
                    .checked_add(1)
                    .ok_or(DiscoveryError::MalformedSource)?;
            }
            levels[depth] = level;
            ancestors[depth] = ancestor;
            duties[depth] = accepted;
            children_seen[depth] = 0;
            interrupt_parents[depth] = children_interrupt_parent(&node, interrupt_parent);
        }

        Ok(())
    }
}

impl<P: FdtPlatform> FdtDiscovery<'_, P> {
    /// Build the hardware-tree node for one device-tree node, or `None` when
    /// the node describes nothing the tree can carry (no representable match
    /// key and not a memory node — the matcher could never bind it).
    fn build_node(
        &self,
        node: &Node<'_>,
        levels: &[BusLevel<'_>],
        placed: Placement,
    ) -> Option<HwNode> {
        let Placement {
            depth,
            parent,
            id,
            interrupt_parent,
            coherence,
        } = placed;
        if !is_emitted(node) {
            return None;
        }
        let class = classify(node);
        let mut hw = HwNode::new(id, parent, class);
        // Stated first, so the node always has room for it: one the kernel
        // drives that could not say so would be a load target. A generic ECAM
        // host is the kernel's on every port: it enumerates the functions
        // below and owns their configuration space.
        if (self.platform.kernel_driven(node) || node.is_compatible(tairix_fdt::pci::ECAM_HOST))
            && hw
                .push_resource(HwResource::property(HwProperty::KernelDriven, 1))
                .is_err()
        {
            return None;
        }

        if let Some(compat) = node.property("compatible") {
            for s in compat.iter_strings() {
                // A string longer than the ABI's bound is rejected on *both*
                // sides — a driver bind key could never carry it either — so
                // skipping it provably loses no match. Keys past the node
                // capacity are dropped most-specific-first preserved (the
                // devicetree list order).
                let Ok(key) = HwMatchKey::compatible(s) else {
                    continue;
                };
                if hw.push_match_key(key).is_err() {
                    break;
                }
            }
        }

        push_mmio_resources(node, depth, levels, &mut hw);
        if interrupt_parent.is_some()
            && interrupt_parent == self.platform.root_interrupt_controller()
        {
            push_irq_resources(&self.platform, node, &mut hw);
        }

        // A NIC's own hardware address, where its node carries one: the
        // standard ethernet-controller binding, so it is read for every node
        // rather than gated on a board.
        if let Some(octets) = local_mac_address(node) {
            let _ = hw.push_resource(HwResource::link_address(octets));
        }

        let mut streams = StreamRanges::new();
        let translation = match master_translation(&self.fdt, node, &mut streams) {
            MasterTranslation::Translated if !streams.push_onto(&mut hw) => {
                MasterTranslation::Refused
            }
            translation => translation,
        };
        let mut masters = translation.names_a_master();
        if class == HwDeviceClass::Iommu {
            // A unit masters memory itself, for its tables, queues and
            // records, which the kernel places with no regard to its bus.
            let _ = hw.push_resource(HwResource::dma(0, 0, coherence));
        }
        if class == HwDeviceClass::Dma {
            let channels = self.platform.dma_channel_mask(node, depth, levels);
            masters = LinkDuty::new(DMA_CONTROLLER_ENDPOINTS.endpoint(id), channels)
                .is_ok_and(|duty| hw.push_resource(HwResource::duty(&duty)).is_ok());
        }
        if masters && translation != MasterTranslation::Refused {
            push_dma_windows(depth, levels, coherence, &mut hw);
        }
        if node.property(CLOCK_BINDING.cells).is_some()
            && !node.is_compatible(FIXED_CLOCK_COMPATIBLE)
        {
            if let Ok(duty) = LinkDuty::new(CLOCK_CONTROLLER_ENDPOINTS.endpoint(id), None) {
                let _ = hw.push_resource(HwResource::duty(&duty));
            }
        }
        push_link_requests(&self.fdt, node, DMA_BINDING, &mut hw);
        push_link_requests(&self.fdt, node, CLOCK_BINDING, &mut hw);
        if node.property("#sound-dai-cells").is_some() {
            push_codec_links(&self.fdt, node, id, &mut hw);
        }

        self.platform
            .augment(node, depth, levels, coherence, &mut hw);

        Some(hw)
    }
}

/// Where the walk reached a node: its depth, the id of the emitted node it
/// hangs from, the id it would take, and what it inherits.
#[derive(Copy, Clone)]
struct Placement {
    depth: usize,
    parent: u32,
    id: u32,
    interrupt_parent: Option<u32>,
    coherence: DmaCoherence,
}

/// What `node` itself says of its DMA's coherence, by the Devicetree Spec's
/// `dma-coherent` and `dma-noncoherent`: [`None`] for neither, and unsnooped
/// for both, which claims nothing it could be trusted for.
fn own_dma_coherence(node: &Node<'_>) -> Option<DmaCoherence> {
    match (
        node.property("dma-coherent").is_some(),
        node.property("dma-noncoherent").is_some(),
    ) {
        (true, false) => Some(DmaCoherence::Snooped),
        (false, false) => None,
        (_, true) => Some(DmaCoherence::Unsnooped),
    }
}

/// The coherence each depth's node states, or inherits from the nearest node
/// above it that does, as a walk reaches nodes in tree order.
#[derive(Clone)]
struct StatedCoherence([Option<DmaCoherence>; MAX_WALK_DEPTH]);

impl StatedCoherence {
    const fn new() -> Self {
        Self([None; MAX_WALK_DEPTH])
    }

    /// What `node`, reached at `depth` below [`MAX_WALK_DEPTH`], states or
    /// inherits; [`None`] where no node on its path says.
    fn reach(&mut self, node: &Node<'_>, depth: usize) -> Option<DmaCoherence> {
        let inherited = depth.checked_sub(1).and_then(|above| self.0[above]);
        let coherence = own_dma_coherence(node).or(inherited);
        self.0[depth] = coherence;
        coherence
    }
}

/// A node's effective interrupt parent (Devicetree Spec v0.4 §2.4.1): its own
/// `interrupt-parent`, else the one its tree parent hands down. A present but
/// malformed property names no parent rather than inheriting one.
fn own_interrupt_parent(node: &Node<'_>, inherited: Option<u32>) -> Option<u32> {
    match node.property("interrupt-parent") {
        None => inherited,
        Some(property) if property.value().len() == 4 => {
            property.read_be_u32(0).ok().and_then(phandle_ref)
        }
        Some(_) => None,
    }
}

/// The interrupt parent `node`'s children inherit: `node` itself when it is
/// an interrupt controller or nexus, which `#interrupt-cells` marks, else
/// `node`'s own. A node's `#interrupt-cells` never makes it its *own* parent:
/// a nexus's own interrupts go to the controller above it.
fn children_interrupt_parent(node: &Node<'_>, own: Option<u32>) -> Option<u32> {
    if node.property("#interrupt-cells").is_some() {
        node.phandle()
    } else {
        own
    }
}

/// Push the windows a bus master at `depth` — a DMA controller, a unit, or a
/// device its port knows masters DMA itself — reaches memory through, each
/// composed through every bus between it and the root, carrying the bus
/// address it starts at and the master's `coherence`. With nothing on the
/// way that translates, it reaches memory untranslated: one unconstrained
/// window. A bus that maps nothing leaves it no window.
pub fn push_dma_windows(
    depth: usize,
    levels: &[BusLevel<'_>],
    coherence: DmaCoherence,
    hw: &mut HwNode,
) {
    if depth == 0 {
        return;
    }
    let Some(reach) = dma_reach(levels, depth) else {
        return;
    };
    match reach.windows() {
        None => {
            let _ = hw.push_resource(HwResource::dma(0, 0, coherence));
        }
        Some(windows) => {
            for window in windows {
                let Some(top) = window.cpu.checked_add(window.size) else {
                    continue;
                };
                if hw
                    .push_resource(HwResource::dma_translated(
                        top,
                        window.size,
                        window.bus,
                        coherence,
                    ))
                    .is_err()
                {
                    return;
                }
            }
        }
    }
}

/// A devicetree binding naming link suppliers: the list a consumer names them
/// in, the names paired with its entries, and the property stating how many
/// cells a supplier's specifier takes.
#[derive(Copy, Clone)]
struct LinkBinding {
    role: LinkRole,
    list: &'static str,
    names: &'static str,
    cells: &'static str,
}

/// The generic DMA binding.
const DMA_BINDING: LinkBinding = LinkBinding {
    role: LinkRole::Dma,
    list: "dmas",
    names: "dma-names",
    cells: "#dma-cells",
};

/// The generic clock binding.
const CLOCK_BINDING: LinkBinding = LinkBinding {
    role: LinkRole::Clock,
    list: "clocks",
    names: "clock-names",
    cells: "#clock-cells",
};

/// The `compatible` of a fixed-rate clock: no driver serves one, its rate
/// being its whole description.
const FIXED_CLOCK_COMPATIBLE: &[u8] = b"fixed-clock";

/// What an entry of a consumer's list resolves to.
#[derive(Copy, Clone)]
enum Supplier {
    /// A node the walk emits, by the id it gives it.
    Emitted(u32),
    /// A fixed-rate clock, at its rate in Hz.
    Fixed(u64),
    /// A node no consumer may use, or one the walk does not describe.
    Absent,
}

/// Push what each entry of `node`'s `binding` list names: a [`LinkRequest`]
/// to the supplier the entry's phandle resolves to, paired with the name in
/// the same position, or the rate of a fixed clock.
///
/// An entry is as wide as its supplier's cell count, so one whose supplier
/// node cannot be found ends the list: nothing after it can be framed. One
/// naming a supplier no consumer may use is framed and skipped. An entry
/// wider than a record carries is dropped whole, never truncated, and a name
/// longer than a record holds leaves its request unnamed.
fn push_link_requests(fdt: &Fdt<'_>, node: &Node<'_>, binding: LinkBinding, hw: &mut HwNode) {
    let Some(entries) = node.property(binding.list) else {
        return;
    };
    let mut names = node.property(binding.names).map(|p| p.iter_strings());
    // A node's entries usually all name one supplier, so the last
    // resolution is kept rather than replayed.
    let mut last: Option<(u32, (Supplier, u32))> = None;
    let supplier = |phandle: u32| {
        if let Some((_, known)) = last.filter(|&(seen, _)| seen == phandle) {
            return Some(known);
        }
        let resolved = link_supplier(fdt, binding, phandle)?;
        last = Some((phandle, resolved));
        Some(resolved)
    };
    for (index, entry) in (0..=u8::MAX).zip(phandle_args(entries.value(), supplier)) {
        let Ok((supplier, specifier)) = entry else {
            return;
        };
        let name = names
            .as_mut()
            .and_then(Iterator::next)
            .filter(|name| name.len() <= LINK_NAME_MAX)
            .unwrap_or_default();
        let record = match supplier {
            Supplier::Emitted(id) => {
                if specifier.len() > LINK_SELECTOR_MAX_CELLS {
                    continue;
                }
                let mut cells = [0u32; LINK_SELECTOR_MAX_CELLS];
                for (slot, cell) in cells.iter_mut().zip(specifier.cells()) {
                    *slot = cell;
                }
                let endpoint = binding.role.endpoints().endpoint(id);
                match LinkRequest::new(endpoint, index, &cells[..specifier.len()], name) {
                    Ok(request) => HwResource::request(&request),
                    Err(_) => continue,
                }
            }
            Supplier::Fixed(hz) => match HwResource::fixed_clock_rate(index, hz) {
                Some(fact) => fact,
                None => continue,
            },
            Supplier::Absent => continue,
        };
        if hw.push_resource(record).is_err() {
            return;
        }
    }
}

/// What `phandle` names under `binding`, and the cell count its specifiers
/// take: [`None`] where no node carries the phandle or it states no single
/// cell count, so no entry naming it can be framed.
fn link_supplier(fdt: &Fdt<'_>, binding: LinkBinding, phandle: u32) -> Option<(Supplier, u32)> {
    let (id, node) = match provider(fdt, phandle)? {
        Provider::Emitted(id, node) => (Some(id), node),
        Provider::Unusable(node) | Provider::Undescribed(node) => (None, node),
    };
    let cells = node.property(binding.cells)?;
    if cells.value().len() != CELL_BYTES {
        return None;
    }
    let count = cells.read_be_u32(0).ok()?;
    let supplier = match id {
        None => Supplier::Absent,
        Some(_)
            if binding.role == LinkRole::Clock && node.is_compatible(FIXED_CLOCK_COMPATIBLE) =>
        {
            fixed_clock_hz(&node).map_or(Supplier::Absent, Supplier::Fixed)
        }
        Some(id) => Supplier::Emitted(id),
    };
    Some((supplier, count))
}

/// A fixed clock's `clock-frequency`, one cell or two.
fn fixed_clock_hz(node: &Node<'_>) -> Option<u64> {
    let frequency = node.property("clock-frequency")?.value();
    match frequency.len() {
        4 => read_cells(frequency, 0, 1),
        8 => read_cells(frequency, 0, 2),
        _ => None,
    }
}

/// The `compatible` of the generic sound card binding, which describes the
/// links between digital audio interfaces rather than a device of its own.
const SIMPLE_AUDIO_CARD: &[u8] = b"simple-audio-card";

/// One end of a sound card link: the `sound-dai` entry of a `cpu` or `codec`
/// sub-node, and the sub-node's own phandle, which the card's
/// `bitclock-master` and `frame-master` name.
#[derive(Copy, Clone)]
struct DaiEnd {
    target: u32,
    dai: u32,
    sub_node: Option<u32>,
    claims_bit_clock: bool,
    claims_frame_clock: bool,
    /// The legacy binding's inversions, stated on the sub-node.
    inversion: ClockInversion,
}

/// One link a sound card describes between a CPU's digital audio interface
/// and a codec's.
#[derive(Copy, Clone)]
struct CardLink {
    cpu: DaiEnd,
    codec: DaiEnd,
    format: DaiFormat,
    bit_clock_master: Option<u32>,
    frame_clock_master: Option<u32>,
    inversion: ClockInversion,
}

impl CardLink {
    /// The selector the CPU side's request carries, or [`None`] for a CPU
    /// interface index past what it holds.
    fn selector(&self) -> Option<DaiLink> {
        let codec_drives = |master: Option<u32>, claimed: bool| match master {
            Some(phandle) => self.codec.sub_node == Some(phandle),
            None => claimed,
        };
        // Without a link-level master the binding is the legacy one, whose
        // codec sub-node may state the inversions too, as Linux reads it.
        let legacy = self.bit_clock_master.is_none() && self.frame_clock_master.is_none();
        let codec = if legacy {
            self.codec.inversion
        } else {
            ClockInversion::Normal
        };
        Some(DaiLink {
            format: self.format,
            codec_drives_bit_clock: codec_drives(
                self.bit_clock_master,
                self.codec.claims_bit_clock,
            ),
            codec_drives_frame_clock: codec_drives(
                self.frame_clock_master,
                self.codec.claims_frame_clock,
            ),
            inversion: ClockInversion::of(
                self.inversion.bit_clock() || codec.bit_clock(),
                self.inversion.frame_clock() || codec.frame_clock(),
            ),
            cpu_dai: u8::try_from(self.cpu.dai).ok()?,
            codec_dai: self.codec.dai,
        })
    }
}

/// The two ends a link node's `cpu` and `codec` children name, as far as
/// they have been read.
#[derive(Copy, Clone, Default)]
struct LinkReading {
    cpu: Option<DaiEnd>,
    codec: Option<DaiEnd>,
}

/// Visit every link the operational `simple-audio-card` nodes describe: the
/// card's own `cpu`/`codec` pair, and each `dai-link` sub-node's.
fn for_each_card_link(fdt: &Fdt<'_>, mut visit: impl FnMut(&CardLink)) {
    let mut nodes = fdt.operational_nodes();
    while let Some(Ok(card)) = nodes.next() {
        if !card.is_compatible(SIMPLE_AUDIO_CARD) {
            continue;
        }
        let depth = card.depth();
        let mut own = LinkReading::default();
        let mut sub: Option<(Node<'_>, LinkReading)> = None;
        let finish = |link: Option<(Node<'_>, LinkReading)>, visit: &mut dyn FnMut(&CardLink)| {
            if let Some((node, reading)) = link {
                emit_card_link(&node, "", reading, visit);
            }
        };
        let mut subtree = nodes.clone();
        while let Some(Ok(child)) = subtree.next() {
            if child.depth() <= depth {
                break;
            }
            let stem = name_stem(child.name());
            if child.depth() == depth + 1 {
                finish(sub.take(), &mut visit);
                match stem {
                    b"simple-audio-card,cpu" => own.cpu = dai_end(fdt, &child),
                    b"simple-audio-card,codec" => own.codec = dai_end(fdt, &child),
                    b"simple-audio-card,dai-link" => sub = Some((child, LinkReading::default())),
                    _ => {}
                }
            } else if child.depth() == depth + 2 {
                if let Some((_, reading)) = sub.as_mut() {
                    match stem {
                        b"cpu" => reading.cpu = dai_end(fdt, &child),
                        b"codec" => reading.codec = dai_end(fdt, &child),
                        _ => {}
                    }
                }
            }
        }
        finish(sub.take(), &mut visit);
        emit_card_link(&card, "simple-audio-card,", own, &mut visit);
    }
}

/// Visit the link `reading` holds, its format and clock masters read from
/// `node`'s properties under `prefix`. A link missing either end, or with no
/// format the binding names, describes nothing.
fn emit_card_link(
    node: &Node<'_>,
    prefix: &str,
    reading: LinkReading,
    visit: &mut dyn FnMut(&CardLink),
) {
    let (Some(cpu), Some(codec)) = (reading.cpu, reading.codec) else {
        return;
    };
    let format = link_property(node, prefix, "format")
        .and_then(|value| value.iter_strings().next())
        .map_or(Some(DaiFormat::I2s), DaiFormat::from_binding);
    let Some(format) = format else {
        return;
    };
    let master =
        |key: &str| link_property(node, prefix, key).and_then(|value| value.read_be_u32(0).ok());
    let stated = |key: &str| link_property(node, prefix, key).is_some();
    visit(&CardLink {
        cpu,
        codec,
        format,
        bit_clock_master: master("bitclock-master"),
        frame_clock_master: master("frame-master"),
        inversion: ClockInversion::of(stated("bitclock-inversion"), stated("frame-inversion")),
    });
}

/// `node`'s property `key` under `prefix`, or [`None`] where it has none or
/// the joined name is longer than any the binding defines.
fn link_property<'a>(node: &Node<'a>, prefix: &str, key: &str) -> Option<Property<'a>> {
    let mut buffer = [0u8; 40];
    let total = prefix.len().checked_add(key.len())?;
    let name = buffer.get_mut(..total)?;
    name[..prefix.len()].copy_from_slice(prefix.as_bytes());
    name[prefix.len()..].copy_from_slice(key.as_bytes());
    node.property(core::str::from_utf8(name).ok()?)
}

/// The `sound-dai` a `cpu` or `codec` sub-node names: the interface node, by
/// phandle, and which of its interfaces, as its `#sound-dai-cells` frames it.
/// The legacy `bitclock-master`/`frame-master` flags on a codec sub-node say
/// it drives those clocks.
fn dai_end(fdt: &Fdt<'_>, sub_node: &Node<'_>) -> Option<DaiEnd> {
    let value = sub_node.property("sound-dai")?.value();
    let cells = |phandle: u32| {
        let target = fdt.node_by_phandle(phandle)?;
        let cells = target.property("#sound-dai-cells")?;
        (cells.value().len() == CELL_BYTES)
            .then(|| cells.read_be_u32(0).ok())
            .flatten()
            .map(|count| ((), count))
    };
    let ((), args) = phandle_args(value, cells).next()?.ok()?;
    let dai = match args.len() {
        0 => 0,
        1 => args.cell(0)?,
        _ => return None,
    };
    Some(DaiEnd {
        target: args.phandle,
        dai,
        sub_node: sub_node.phandle(),
        claims_bit_clock: sub_node.property("bitclock-master").is_some(),
        claims_frame_clock: sub_node.property("frame-master").is_some(),
        inversion: ClockInversion::of(
            sub_node.property("bitclock-inversion").is_some(),
            sub_node.property("frame-inversion").is_some(),
        ),
    })
}

/// Push what the sound cards say of `node`, a digital audio interface the walk
/// gives `id`: a codec [`LinkRequest`] for each link it is the CPU side of,
/// naming the codec's endpoint and carrying the link's selector, and the
/// codec duty when any link names it as the codec.
fn push_codec_links(fdt: &Fdt<'_>, node: &Node<'_>, id: u32, hw: &mut HwNode) {
    let Some(own) = node.phandle() else {
        return;
    };
    let mut is_codec = false;
    let mut index = 0u8;
    for_each_card_link(fdt, |link| {
        is_codec |= link.codec.target == own;
        if link.cpu.target != own {
            return;
        }
        let Some(Provider::Emitted(codec, _)) = provider(fdt, link.codec.target) else {
            return;
        };
        let Some(selector) = link.selector() else {
            return;
        };
        if let Ok(request) = LinkRequest::new(
            CODEC_ENDPOINTS.endpoint(codec),
            index,
            &selector.to_cells(),
            b"",
        ) {
            let _ = hw.push_resource(HwResource::request(&request));
        }
        index = index.saturating_add(1);
    });
    if is_codec {
        if let Ok(duty) = LinkDuty::new(CODEC_ENDPOINTS.endpoint(id), None) {
            let _ = hw.push_resource(HwResource::duty(&duty));
        }
    }
}

/// What a master's `iommus` says of its DMA.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum MasterTranslation {
    /// It names no unit.
    Unnamed,
    /// It masters DMA through units, as the streams gathered beside it.
    Translated,
    /// It names only units translating nothing it can be named by: ones a
    /// consumer may not use, or whose specifiers carry no stream id.
    Untranslated,
    /// The tree cannot describe its DMA: a list that does not frame, a
    /// provider that is no unit the walk describes, DMA reaching memory both
    /// through a unit and around one, or more streams than a node carries.
    Refused,
}

impl MasterTranslation {
    /// Whether the statement makes the node a DMA master.
    fn names_a_master(self) -> bool {
        matches!(self, Self::Translated | Self::Untranslated)
    }
}

/// Through what the DMA an `iommus` entry names passes.
#[derive(Copy, Clone)]
enum Through {
    /// The unit with this id, which knows the master by a stream id.
    Unit(u32),
    /// A unit translating nothing the entry names.
    Nothing,
}

/// The streams one master names, consecutive ids on one unit coalesced into
/// a range: no more than a node can carry.
struct StreamRanges {
    ranges: [Option<IommuStreams>; HW_NODE_MAX_RESOURCES],
    held: usize,
}

impl StreamRanges {
    const fn new() -> Self {
        Self {
            ranges: [None; HW_NODE_MAX_RESOURCES],
            held: 0,
        }
    }

    /// Add stream `id` on `unit`; `false` when a node could carry no further
    /// range.
    fn add(&mut self, unit: u32, id: u32) -> bool {
        let last = self.held.checked_sub(1).and_then(|at| self.ranges[at]);
        if let Some(last) = last.filter(|last| {
            last.unit() == unit && last.first().checked_add(last.count()) == Some(id)
        }) {
            if let Ok(grown) = IommuStreams::new(unit, last.first(), last.count() + 1) {
                self.ranges[self.held - 1] = Some(grown);
                return true;
            }
        }
        let (Some(slot), Ok(range)) = (
            self.ranges.get_mut(self.held),
            IommuStreams::new(unit, id, 1),
        ) else {
            return false;
        };
        *slot = Some(range);
        self.held += 1;
        true
    }

    /// Push every range onto `hw`, or none when they do not all fit: a
    /// master is never published with part of its identity.
    fn push_onto(&self, hw: &mut HwNode) -> bool {
        if HW_NODE_MAX_RESOURCES - hw.resources().len() < self.held {
            return false;
        }
        self.ranges[..self.held]
            .iter()
            .flatten()
            .all(|&range| hw.push_resource(HwResource::iommu_stream(range)).is_ok())
    }
}

/// Read `node`'s `iommus`, gathering into `streams` the streams it names.
fn master_translation(
    fdt: &Fdt<'_>,
    node: &Node<'_>,
    streams: &mut StreamRanges,
) -> MasterTranslation {
    let Some(iommus) = node.property("iommus") else {
        return MasterTranslation::Unnamed;
    };
    // A master's entries usually all name one unit.
    let mut last: Option<(u32, (Through, u32))> = None;
    let unit = |phandle: u32| {
        if let Some((_, unit)) = last.filter(|&(known, _)| known == phandle) {
            return Some(unit);
        }
        let unit = translation_unit(fdt, phandle)?;
        last = Some((phandle, unit));
        Some(unit)
    };
    let mut bypassed = false;
    for entry in phandle_args(iommus.value(), unit) {
        match entry {
            Ok((Through::Unit(unit), specifier)) => {
                let Some(id) = stream_id(&specifier) else {
                    return MasterTranslation::Refused;
                };
                if !streams.add(unit, id) {
                    return MasterTranslation::Refused;
                }
            }
            Ok((Through::Nothing, _)) => bypassed = true,
            Err(_) => return MasterTranslation::Refused,
        }
    }
    match (streams.held, bypassed) {
        (0, false) => MasterTranslation::Unnamed,
        (0, true) => MasterTranslation::Untranslated,
        (_, false) => MasterTranslation::Translated,
        (_, true) => MasterTranslation::Refused,
    }
}

/// What a master's DMA passes through at the unit `phandle` names, and how
/// many cells that unit's specifiers take. A unit a consumer may not use
/// translates nothing; neither does one whose specifiers carry no stream id,
/// the only form every family reads. A provider without `#iommu-cells`, or
/// one the walk cannot describe, frames no entry.
fn translation_unit(fdt: &Fdt<'_>, phandle: u32) -> Option<(Through, u32)> {
    match provider(fdt, phandle)? {
        Provider::Emitted(id, unit) => {
            let cells = iommu_cells(&unit)?;
            Some((
                if cells == 1 {
                    Through::Unit(id)
                } else {
                    Through::Nothing
                },
                cells,
            ))
        }
        Provider::Unusable(unit) => Some((Through::Nothing, iommu_cells(&unit)?)),
        Provider::Undescribed(_) => None,
    }
}

/// What a phandle names, as the walk numbers the tree.
#[derive(Copy, Clone)]
pub enum Provider<'a> {
    /// A node the walk emits, and the id it gives it.
    Emitted(u32, Node<'a>),
    /// A node no consumer may use: it, or an ancestor, is disabled, reserved
    /// or failed.
    Unusable(Node<'a>),
    /// A usable node the walk does not emit, as it carries nothing the
    /// matcher could bind.
    Undescribed(Node<'a>),
}

/// The node `phandle` names and how the walk treats it, found by replaying
/// the walk's own numbering, so a consumer met before its provider names the
/// id the provider will get. [`None`] where no node carries the phandle or
/// the tree is malformed before it.
#[must_use]
pub fn provider<'a>(fdt: &Fdt<'a>, phandle: u32) -> Option<Provider<'a>> {
    let mut id = FIRST_EMITTED_ID;
    for node in fdt.nodes_in_use() {
        let (node, usable) = node.ok()?;
        let depth = node.depth() as usize;
        // The walk refuses a tree only for a usable node too deep to track;
        // it never enters a subtree no consumer may use.
        if usable && depth >= MAX_WALK_DEPTH {
            return None;
        }
        let emitted = usable && depth > 0 && is_emitted(&node);
        if node.phandle() == Some(phandle) {
            return Some(match (usable, emitted) {
                (_, true) => Provider::Emitted(id, node),
                (false, _) => Provider::Unusable(node),
                (true, false) => Provider::Undescribed(node),
            });
        }
        if emitted {
            id = id.checked_add(1)?;
        }
    }
    None
}

/// Every node the walk emits with the id it gives it, in emission order — the
/// walk's own numbering, which a pass reading the tree after the walk names
/// nodes by — and the DMA coherence it states or inherits, [`None`] where the
/// architecture's convention holds.
#[must_use]
pub fn emitted<'a>(fdt: &Fdt<'a>) -> Emitted<'a> {
    Emitted {
        nodes: fdt.operational_nodes(),
        next_id: Some(FIRST_EMITTED_ID),
        stated: StatedCoherence::new(),
    }
}

/// Iterator over the nodes the walk emits, produced by [`emitted`]. It ends
/// where the walk would refuse the tree.
#[derive(Clone)]
pub struct Emitted<'a> {
    nodes: OperationalNodes<'a>,
    next_id: Option<u32>,
    stated: StatedCoherence,
}

impl<'a> Iterator for Emitted<'a> {
    type Item = (u32, Node<'a>, Option<DmaCoherence>);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let id = self.next_id?;
            let Some(Ok(node)) = self.nodes.next() else {
                self.next_id = None;
                return None;
            };
            let depth = node.depth() as usize;
            if depth >= MAX_WALK_DEPTH {
                self.next_id = None;
                return None;
            }
            let coherence = self.stated.reach(&node, depth);
            if depth > 0 && is_emitted(&node) {
                self.next_id = id.checked_add(1);
                return Some((id, node, coherence));
            }
        }
    }
}

impl core::iter::FusedIterator for Emitted<'_> {}

/// Whether the walk emits a hardware-tree node for this device-tree node.
///
/// A node with no representable match key and no memory `device_type` is one
/// the matcher could never bind, so it is spliced out. The single definition
/// of that rule: [`FdtDiscovery::build_node`] applies it, and the bus-child
/// look-ahead, [`provider`] and [`emitted`] replay it to predict the ids the
/// walk assigns — a second spelling would let them disagree and pair a chip, a
/// DMA consumer or a translated master with another node's id.
fn is_emitted(node: &Node<'_>) -> bool {
    if classify(node) == HwDeviceClass::Memory {
        return true;
    }
    node.property("compatible").is_some_and(|compat| {
        compat
            .iter_strings()
            .any(|s| HwMatchKey::compatible(s).is_ok())
    })
}

/// Whether a node's own cell counts declare it an **addressed,
/// non-enumerable bus**: one address cell and no size cells, so a child's
/// single `reg` cell is its address *on this bus* rather than a window in
/// the parent's address space (Devicetree spec v0.4 §2.3.5).
///
/// I²C, SPI chip-select, and 1-Wire all spell themselves this way, so the
/// walk recognises the convention rather than any board or bus name.
fn declares_addressed_bus(level: &BusLevel<'_>) -> bool {
    level.addr_cells == 1 && level.size_cells == 0
}

/// Whether `node` is an addressed child of such a bus.
///
/// The `/cpus` container uses the same cell convention for CPU numbering
/// (Devicetree spec v0.4 §3.7), so a CPU node is excluded: its `reg` is a
/// hart/MPIDR identifier, not a device address on a transfer bus.
fn is_bus_child(node: &Node<'_>, depth: usize, levels: &[BusLevel<'_>]) -> bool {
    depth
        .checked_sub(1)
        .and_then(|parent| levels.get(parent))
        .is_some_and(declares_addressed_bus)
        && classify(node) != HwDeviceClass::Cpu
        && bus_child_address(node).is_some()
}

/// The address an addressed bus child answers to: the first cell of its
/// `reg`, which its parent's `#address-cells = <1>` declares to be exactly
/// one cell wide.
fn bus_child_address(node: &Node<'_>) -> Option<u64> {
    read_cells(node.property("reg")?.value(), 0, 1)
}

/// Push one [`HwResource::bus_child`] duty per addressed child of the bus
/// node being built, and report how many its resource list could hold.
///
/// `subtree` is the walk's own iterator cloned just after the bus node, and
/// `bus_id` the id the walk is assigning that node — so replaying
/// [`is_emitted`] over the subtree in document order yields exactly the ids
/// the walk will assign, and a child's duty here names the same endpoint its
/// own node will later claim.
///
/// A child past the node's resource capacity is left without a duty (and so,
/// by the caller's matching bound, without an endpoint): a bus whose tree
/// declares more children than one node can carry serves the ones it can and
/// leaves the rest unbound, rather than handing a chip driver authority no
/// bus driver was told to serve.
fn push_bus_child_duties(
    subtree: OperationalNodes<'_>,
    bus_depth: usize,
    bus_id: u32,
    hw: &mut HwNode,
) -> usize {
    let mut accepted = 0;
    let mut next_id = bus_id;
    for child in subtree {
        let Ok(child) = child else { return accepted };
        let depth = child.depth() as usize;
        if depth <= bus_depth {
            return accepted;
        }
        if !is_emitted(&child) {
            continue;
        }
        let Some(id) = next_id.checked_add(1) else {
            return accepted;
        };
        next_id = id;
        if depth != bus_depth + 1 || classify(&child) == HwDeviceClass::Cpu {
            continue;
        }
        let Some(address) = bus_child_address(&child) else {
            continue;
        };
        if hw
            .push_resource(HwResource::bus_child(
                BUS_CHILD_ENDPOINTS.endpoint(id),
                address,
            ))
            .is_err()
        {
            return accepted;
        }
        accepted += 1;
    }
    accepted
}

/// Decode each `reg` entry with the parent's cell counts, translate it
/// through the ancestor buses' `ranges`, and push it as an MMIO resource.
///
/// Entries that cannot be decoded (out-of-range cell counts, a length that
/// is not a whole number of entries) or translated (an ancestor bus without
/// usable `ranges`) are dropped — the tree never carries an invented or
/// untranslated window. Entries past the node's resource capacity are
/// dropped likewise (an ABI bound).
fn push_mmio_resources(node: &Node<'_>, depth: usize, levels: &[BusLevel<'_>], hw: &mut HwNode) {
    let Some(entries) = reg_entry_count(node, depth, levels) else {
        return;
    };
    for index in 0..entries {
        if let Some((base, len)) = translated_reg(node, depth, levels, index) {
            if hw.push_resource(HwResource::mmio(base, len)).is_err() {
                return;
            }
        }
    }
}

/// Push one IRQ resource per `interrupts` specifier, carrying the line
/// number the port's [`FdtPlatform::interrupt_line`] mapped it to and the
/// specifier's position in the list.
///
/// A property whose length is not a whole number of specifiers is refused
/// entire — a partial list is a malformed one, and guessing where it ends
/// would invent a line. A single specifier the port cannot represent is
/// skipped and the rest still emitted, each at its own position.
fn push_irq_resources<P: FdtPlatform>(platform: &P, node: &Node<'_>, hw: &mut HwNode) {
    let specifier_len = platform.interrupt_cells() * CELL_BYTES;
    let Some(interrupts) = node.property("interrupts") else {
        return;
    };
    let value = interrupts.value();
    if specifier_len == 0 || value.is_empty() || value.len() % specifier_len != 0 {
        return;
    }
    for (position, specifier) in (0u32..).zip(value.chunks_exact(specifier_len)) {
        if let Some(line) = platform.interrupt_line(specifier) {
            let line = u64::from(line);
            let irq = if platform.edge_triggered(specifier) {
                HwResource::edge_irq_at(line, position)
            } else {
                HwResource::irq_at(line, position)
            };
            if hw.push_resource(irq).is_err() {
                return;
            }
        }
    }
}

/// A node's own hardware address from the standard ethernet-controller
/// binding.
///
/// `mac-address` (the current address) takes precedence over
/// `local-mac-address` (the address programmed at manufacture), matching the
/// binding's own precedence. A property that is not exactly one address is
/// ignored rather than truncated or padded, and an all-zero address is
/// refused (it is neither a valid unicast nor the broadcast address, so it
/// carries no identity) — fail closed, never a guessed MAC.
fn local_mac_address(node: &Node<'_>) -> Option<[u8; MAC_ADDRESS_LEN]> {
    for name in ["mac-address", "local-mac-address"] {
        let Some(property) = node.property(name) else {
            continue;
        };
        let value = property.value();
        if value.len() != MAC_ADDRESS_LEN {
            continue;
        }
        let mut octets = [0u8; MAC_ADDRESS_LEN];
        octets.copy_from_slice(value);
        if octets != [0u8; MAC_ADDRESS_LEN] {
            return Some(octets);
        }
    }
    None
}

/// Derive the device class from the node's own data, most authoritative
/// source first: `device_type` (the spec keeps it for `memory` and `cpu`),
/// the `#iommu-cells` a translation unit's binding requires, the
/// `#dma-cells` a DMA controller's, the `interrupt-controller` marker
/// property, then the spec-recommended
/// generic node-name stem. Anything else is honestly
/// [`HwDeviceClass::Other`] — the class is advisory; binding is by match
/// key.
fn classify(node: &Node<'_>) -> HwDeviceClass {
    if let Some(device_type) = node.property("device_type") {
        match device_type.iter_strings().next() {
            Some(b"memory") => return HwDeviceClass::Memory,
            Some(b"cpu") => return HwDeviceClass::Cpu,
            _ => {}
        }
    }
    if node.property("#iommu-cells").is_some() {
        return HwDeviceClass::Iommu;
    }
    if node.property("#dma-cells").is_some() {
        return HwDeviceClass::Dma;
    }
    if node.property("interrupt-controller").is_some() {
        return HwDeviceClass::InterruptController;
    }
    match name_stem(node.name()) {
        b"memory" => HwDeviceClass::Memory,
        b"cpu" => HwDeviceClass::Cpu,
        b"timer" => HwDeviceClass::Timer,
        b"interrupt-controller" | b"intc" | b"gic" | b"plic" => HwDeviceClass::InterruptController,
        b"serial" | b"uart" => HwDeviceClass::Serial,
        b"rtc" => HwDeviceClass::Rtc,
        b"ethernet" => HwDeviceClass::Network,
        b"mmc" | b"sdhci" | b"emmc2" => HwDeviceClass::Storage,
        b"keyboard" | b"mouse" | b"touchscreen" => HwDeviceClass::Input,
        b"display" | b"gpu" | b"hdmi" | b"framebuffer" => HwDeviceClass::Display,
        // The three generic names the devicetree spec offers for a device
        // that computes rather than moves: a crypto offload, a signal
        // processor, a media decode/encode engine.
        b"crypto" | b"dsp" | b"video-codec" => HwDeviceClass::Accelerator,
        // The devicetree names for a sound device: the controller itself, a
        // codec behind it, and the machine-level graph that binds the two.
        b"sound" | b"audio" | b"codec" | b"i2s" => HwDeviceClass::Audio,
        b"soc" | b"bus" | b"pci" | b"pcie" | b"usb" | b"axi" => HwDeviceClass::Bus,
        _ => HwDeviceClass::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::{FdtDiscovery, FdtPlatform};
    use crate::platform::{DiscoveryError, HwNodeSink, PlatformDiscovery};
    use std::vec::Vec;
    use tairix_abi::driver::clock::CLOCK_CONTROLLER_ENDPOINTS;
    use tairix_abi::driver::codec::{ClockInversion, DaiFormat, DaiLink, CODEC_ENDPOINTS};
    use tairix_abi::driver::dmaengine::DMA_CONTROLLER_ENDPOINTS;
    use tairix_abi::hwlink::{LinkDuty, LinkRequest};
    use tairix_abi::hwtree::BUS_CHILD_ENDPOINTS;
    use tairix_abi::{
        DmaCoherence, HwDeviceClass, HwNode, HwResource, HwResourceKind, IommuStreams,
        HW_NODE_MAX_RESOURCES,
    };
    use tairix_fdt::write::FdtWriter;
    use tairix_fdt::{BusLevel, Fdt, Node};

    /// The phandle every fixture gives its root interrupt controller.
    const ROOT_INTC: u32 = 1;

    /// The smallest honest port: one interrupt cell mapped straight through,
    /// no board augmentation. Enough to exercise the shared walk.
    struct BarePlatform;

    impl FdtPlatform for BarePlatform {
        const DEFAULT_DMA_COHERENCE: DmaCoherence = DmaCoherence::Snooped;

        fn interrupt_cells(&self) -> usize {
            1
        }

        fn from_tree(_fdt: &Fdt<'_>) -> Self {
            Self
        }

        fn interrupt_line(&self, specifier: &[u8]) -> Option<u32> {
            let bytes: [u8; 4] = specifier.try_into().ok()?;
            Some(u32::from_be_bytes(bytes))
        }

        fn root_interrupt_controller(&self) -> Option<u32> {
            Some(ROOT_INTC)
        }
    }

    #[derive(Default)]
    struct CollectingSink {
        nodes: Vec<HwNode>,
    }

    impl HwNodeSink for CollectingSink {
        fn emit(&mut self, node: HwNode) -> Result<(), DiscoveryError> {
            self.nodes.push(node);
            Ok(())
        }
    }

    fn discover(blob: &[u8]) -> Vec<HwNode> {
        discover_on::<BarePlatform>(blob)
    }

    fn discover_on<P: FdtPlatform>(blob: &[u8]) -> Vec<HwNode> {
        let fdt = Fdt::new(blob).expect("valid fdt");
        let mut sink = CollectingSink::default();
        FdtDiscovery::<P>::new(fdt)
            .discover(&mut sink)
            .expect("discovery succeeds");
        sink.nodes
    }

    /// The line [`GappedPlatform`] cannot represent.
    const UNREPRESENTABLE_LINE: u32 = 0xDEAD;

    /// [`BarePlatform`] with one line its controller cannot take.
    struct GappedPlatform;

    impl FdtPlatform for GappedPlatform {
        const DEFAULT_DMA_COHERENCE: DmaCoherence = DmaCoherence::Snooped;

        fn interrupt_cells(&self) -> usize {
            1
        }

        fn from_tree(_fdt: &Fdt<'_>) -> Self {
            Self
        }

        fn interrupt_line(&self, specifier: &[u8]) -> Option<u32> {
            let bytes: [u8; 4] = specifier.try_into().ok()?;
            let line = u32::from_be_bytes(bytes);
            (line != UNREPRESENTABLE_LINE).then_some(line)
        }

        fn root_interrupt_controller(&self) -> Option<u32> {
            Some(ROOT_INTC)
        }
    }

    #[test]
    fn a_line_the_port_cannot_represent_shifts_no_other_lines_place() {
        let cells =
            |values: &[u32]| -> Vec<u8> { values.iter().flat_map(|v| v.to_be_bytes()).collect() };
        let mut b = FdtWriter::new();
        b.begin_node("");
        b.prop_u32("#address-cells", 1);
        b.prop_u32("#size-cells", 1);
        b.prop_u32("interrupt-parent", ROOT_INTC);
        b.begin_node("intc");
        b.prop_str("compatible", "test,root-intc");
        b.prop("interrupt-controller", &[]);
        b.prop_u32("#interrupt-cells", 1);
        b.prop_u32("phandle", ROOT_INTC);
        b.end_node();
        b.begin_node("gapped");
        b.prop_str("compatible", "test,gapped");
        b.prop("interrupts", &cells(&[5, UNREPRESENTABLE_LINE, 6]));
        b.end_node();
        b.end_node();
        let nodes = discover_on::<GappedPlatform>(&b.build());
        let gapped = by_key(&nodes, b"test,gapped");
        let lines: Vec<HwResource> = gapped
            .resources()
            .iter()
            .copied()
            .filter(|r| r.kind() == Some(HwResourceKind::Irq))
            .collect();
        assert_eq!(
            lines,
            std::vec![HwResource::irq_at(5, 0), HwResource::irq_at(6, 2)]
        );
    }

    fn by_key<'a>(nodes: &'a [HwNode], compatible: &[u8]) -> &'a HwNode {
        nodes
            .iter()
            .find(|n| {
                n.match_keys()
                    .iter()
                    .any(|k| k.compatible_bytes() == compatible)
            })
            .unwrap_or_else(|| panic!("a node matching {compatible:?}"))
    }

    fn duties(node: &HwNode) -> Vec<(u64, u64)> {
        node.resources()
            .iter()
            .filter_map(HwResource::bus_child_pair)
            .collect()
    }

    fn endpoints(node: &HwNode) -> Vec<u64> {
        node.resources()
            .iter()
            .filter(|r| r.kind() == Some(HwResourceKind::Endpoint))
            .map(HwResource::base)
            .collect()
    }

    /// A tree with one memory-mapped I²C controller carrying two addressed
    /// children plus, optionally, `extra` further children at successive
    /// addresses (to overrun the node's resource capacity).
    fn i2c_tree(extra: u32) -> Vec<u8> {
        let mut b = FdtWriter::new();
        b.begin_node("");
        b.prop_u32("#address-cells", 2);
        b.prop_u32("#size-cells", 2);
        b.prop_u32("interrupt-parent", ROOT_INTC);
        b.begin_node("memory@40000000");
        b.prop_str("device_type", "memory");
        b.prop(
            "reg",
            &[
                &0x4000_0000u64.to_be_bytes()[..],
                &0x1000_0000u64.to_be_bytes()[..],
            ]
            .concat(),
        );
        b.end_node();
        b.begin_node("i2c@fe804000");
        b.prop_str("compatible", "brcm,bcm2835-i2c");
        b.prop(
            "reg",
            &[
                &0xFE80_4000u64.to_be_bytes()[..],
                &0x200u64.to_be_bytes()[..],
            ]
            .concat(),
        );
        b.prop("interrupts", &53u32.to_be_bytes());
        b.prop_u32("#address-cells", 1);
        b.prop_u32("#size-cells", 0);
        b.begin_node("rtc@68");
        b.prop_str("compatible", "maxim,ds3231");
        b.prop("reg", &0x68u32.to_be_bytes());
        b.end_node();
        b.begin_node("rtc@51");
        b.prop_str("compatible", "nxp,pcf85063a");
        b.prop("reg", &0x51u32.to_be_bytes());
        b.end_node();
        for i in 0..extra {
            let address = 0x10 + i;
            b.begin_node("sensor");
            b.prop_str("compatible", "vendor,sensor");
            b.prop("reg", &address.to_be_bytes());
            b.end_node();
        }
        b.end_node();
        b.end_node();
        b.build()
    }

    /// The three devicetree generic names for a device that computes rather
    /// than moves are classed [`HwDeviceClass::Accelerator`]. Without the
    /// class an offload engine is discovered as `Other`, so nothing above
    /// discovery can tell it apart from an unmodelled device.
    #[test]
    fn an_offload_engine_node_is_classed_as_an_accelerator() {
        let mut b = FdtWriter::new();
        b.begin_node("");
        b.prop_u32("#address-cells", 2);
        b.prop_u32("#size-cells", 2);
        for (name, compatible) in [
            ("crypto@ff8b0000", "vendor,crypto"),
            ("dsp@ff8c0000", "vendor,dsp"),
            ("video-codec@ff8d0000", "vendor,vpu"),
            // A name outside the generic set stays honestly unmodelled.
            ("widget@ff8e0000", "vendor,widget"),
        ] {
            b.begin_node(name);
            b.prop_str("compatible", compatible);
            b.prop(
                "reg",
                &[
                    &0xFF8B_0000u64.to_be_bytes()[..],
                    &0x1000u64.to_be_bytes()[..],
                ]
                .concat(),
            );
            b.end_node();
        }
        b.end_node();
        let nodes = discover(&b.build());
        for compatible in [
            &b"vendor,crypto"[..],
            &b"vendor,dsp"[..],
            &b"vendor,vpu"[..],
        ] {
            assert_eq!(
                by_key(&nodes, compatible).class(),
                Some(HwDeviceClass::Accelerator),
                "{}",
                core::str::from_utf8(compatible).unwrap_or("?")
            );
        }
        assert_eq!(
            by_key(&nodes, b"vendor,widget").class(),
            Some(HwDeviceClass::Other)
        );
    }

    #[test]
    fn a_bus_child_gets_the_endpoint_its_parent_was_given_the_duty_for() {
        let nodes = discover(&i2c_tree(0));
        let bus = by_key(&nodes, b"brcm,bcm2835-i2c");
        let ds3231 = by_key(&nodes, b"maxim,ds3231");
        let pcf = by_key(&nodes, b"nxp,pcf85063a");

        // The duty half names each child's endpoint *and* its bus address.
        assert_eq!(
            duties(bus),
            std::vec![
                (BUS_CHILD_ENDPOINTS.endpoint(ds3231.id()), 0x68),
                (BUS_CHILD_ENDPOINTS.endpoint(pcf.id()), 0x51),
            ]
        );
        // The authority half names only the endpoint: a chip driver never
        // learns a bus address, so it cannot address a neighbour.
        assert_eq!(
            endpoints(ds3231),
            std::vec![BUS_CHILD_ENDPOINTS.endpoint(ds3231.id())]
        );
        assert_eq!(
            endpoints(pcf),
            std::vec![BUS_CHILD_ENDPOINTS.endpoint(pcf.id())]
        );
        assert!(duties(ds3231).is_empty());
        // The two halves agree, and the two children never share an id.
        assert_ne!(ds3231.id(), pcf.id());
    }

    #[test]
    fn a_bus_child_gets_no_memory_window_from_its_bus_address() {
        let nodes = discover(&i2c_tree(0));
        for compatible in [&b"maxim,ds3231"[..], b"nxp,pcf85063a"] {
            let child = by_key(&nodes, compatible);
            assert!(
                child
                    .resources()
                    .iter()
                    .all(|r| r.kind() == Some(HwResourceKind::Endpoint)),
                "{compatible:?} must carry nothing but its endpoint"
            );
        }
        // The bus itself still gets its own window and line.
        let bus = by_key(&nodes, b"brcm,bcm2835-i2c");
        assert!(bus
            .resources()
            .iter()
            .any(|r| r.kind() == Some(HwResourceKind::Mmio) && r.base() == 0xFE80_4000));
        assert!(bus
            .resources()
            .iter()
            .any(|r| r.kind() == Some(HwResourceKind::Irq) && r.base() == 53));
    }

    #[test]
    fn a_child_whose_duty_did_not_fit_is_left_without_authority() {
        // The bus already spends two resource slots on its window and line,
        // so past that the node cannot hold every duty.
        let extra = u32::try_from(HW_NODE_MAX_RESOURCES).expect("small");
        let nodes = discover(&i2c_tree(extra));
        let bus = by_key(&nodes, b"brcm,bcm2835-i2c");
        let granted = duties(bus);
        assert_eq!(granted.len(), HW_NODE_MAX_RESOURCES - 2);

        // Every child the bus was told to serve holds exactly the matching
        // endpoint, and every child past the bound holds none — the halves
        // never disagree.
        let served: Vec<u64> = granted.iter().map(|(endpoint, _)| *endpoint).collect();
        let mut children: Vec<&HwNode> = nodes.iter().filter(|n| n.id() > bus.id()).collect();
        children.sort_by_key(|n| n.id());
        assert!(children.len() > served.len());
        for (index, child) in children.iter().enumerate() {
            let expected: Vec<u64> = if index < served.len() {
                std::vec![BUS_CHILD_ENDPOINTS.endpoint(child.id())]
            } else {
                Vec::new()
            };
            assert_eq!(endpoints(child), expected, "child {index}");
        }
        for (endpoint, child) in served.iter().zip(children.iter()) {
            assert_eq!(*endpoint, BUS_CHILD_ENDPOINTS.endpoint(child.id()));
        }
    }

    #[test]
    fn a_cpu_node_is_not_a_bus_child() {
        // `/cpus` spells itself with the same cell counts an addressed bus
        // does, so a CPU must not be handed a transfer endpoint.
        let mut b = FdtWriter::new();
        b.begin_node("");
        b.prop_u32("#address-cells", 2);
        b.prop_u32("#size-cells", 2);
        b.begin_node("cpus");
        b.prop_u32("#address-cells", 1);
        b.prop_u32("#size-cells", 0);
        b.begin_node("cpu@0");
        b.prop_str("device_type", "cpu");
        b.prop_str("compatible", "arm,cortex-a72");
        b.prop("reg", &0u32.to_be_bytes());
        b.end_node();
        b.end_node();
        b.end_node();
        let nodes = discover(&b.build());
        let cpu = by_key(&nodes, b"arm,cortex-a72");
        assert!(endpoints(cpu).is_empty());
        assert!(nodes.iter().all(|n| duties(n).is_empty()));
    }

    #[test]
    fn a_bus_whose_own_node_is_unbindable_hands_out_no_authority() {
        // No `compatible` on the bus, so no driver could ever serve it and
        // the walk splices it out; its children must not be left calling an
        // endpoint nothing will bind.
        let mut b = FdtWriter::new();
        b.begin_node("");
        b.prop_u32("#address-cells", 2);
        b.prop_u32("#size-cells", 2);
        b.begin_node("i2c@fe804000");
        b.prop_u32("#address-cells", 1);
        b.prop_u32("#size-cells", 0);
        b.begin_node("rtc@68");
        b.prop_str("compatible", "maxim,ds3231");
        b.prop("reg", &0x68u32.to_be_bytes());
        b.end_node();
        b.end_node();
        b.end_node();
        let nodes = discover(&b.build());
        assert!(endpoints(by_key(&nodes, b"maxim,ds3231")).is_empty());
    }

    #[test]
    fn an_unbindable_sibling_does_not_shift_the_ids_the_duties_name() {
        // The look-ahead must splice out exactly what the walk does: a
        // child with no representable match key consumes no id, and one
        // nested deeper consumes one without being a duty of this bus.
        let mut b = FdtWriter::new();
        b.begin_node("");
        b.prop_u32("#address-cells", 2);
        b.prop_u32("#size-cells", 2);
        b.begin_node("i2c@fe804000");
        b.prop_str("compatible", "brcm,bcm2835-i2c");
        b.prop_u32("#address-cells", 1);
        b.prop_u32("#size-cells", 0);
        b.begin_node("unbindable@10");
        b.prop("reg", &0x10u32.to_be_bytes());
        b.end_node();
        b.begin_node("mux@70");
        b.prop_str("compatible", "vendor,mux");
        b.prop("reg", &0x70u32.to_be_bytes());
        b.begin_node("nested");
        b.prop_str("compatible", "vendor,nested");
        b.end_node();
        b.end_node();
        b.begin_node("rtc@68");
        b.prop_str("compatible", "maxim,ds3231");
        b.prop("reg", &0x68u32.to_be_bytes());
        b.end_node();
        b.end_node();
        b.end_node();
        let nodes = discover(&b.build());
        let bus = by_key(&nodes, b"brcm,bcm2835-i2c");
        let mux = by_key(&nodes, b"vendor,mux");
        let ds3231 = by_key(&nodes, b"maxim,ds3231");
        assert_eq!(
            duties(bus),
            std::vec![
                (BUS_CHILD_ENDPOINTS.endpoint(mux.id()), 0x70),
                (BUS_CHILD_ENDPOINTS.endpoint(ds3231.id()), 0x68),
            ]
        );
        assert_eq!(
            endpoints(mux),
            std::vec![BUS_CHILD_ENDPOINTS.endpoint(mux.id())]
        );
        assert_eq!(
            endpoints(ds3231),
            std::vec![BUS_CHILD_ENDPOINTS.endpoint(ds3231.id())]
        );
        // The nested grandchild is not this bus's child and gets nothing.
        assert!(endpoints(by_key(&nodes, b"vendor,nested")).is_empty());
    }

    #[test]
    fn an_ordinary_memory_mapped_child_is_untouched() {
        // A `#size-cells = <1>` bus is a memory-mapped one: its children
        // keep their windows and gain no endpoint.
        let mut b = FdtWriter::new();
        b.begin_node("");
        b.prop_u32("#address-cells", 1);
        b.prop_u32("#size-cells", 1);
        b.begin_node("serial@9000000");
        b.prop_str("compatible", "arm,pl011");
        b.prop(
            "reg",
            &[
                &0x0900_0000u32.to_be_bytes()[..],
                &0x1000u32.to_be_bytes()[..],
            ]
            .concat(),
        );
        b.end_node();
        b.end_node();
        let nodes = discover(&b.build());
        let uart = by_key(&nodes, b"arm,pl011");
        assert!(endpoints(uart).is_empty());
        assert!(uart
            .resources()
            .iter()
            .any(|r| r.kind() == Some(HwResourceKind::Mmio) && r.base() == 0x0900_0000));
    }

    #[test]
    fn the_addressed_bus_convention_is_read_from_the_cell_counts_alone() {
        let addressed = BusLevel {
            addr_cells: 1,
            size_cells: 0,
            ranges: None,
            dma_ranges: None,
        };
        assert!(super::declares_addressed_bus(&addressed));
        assert!(!super::declares_addressed_bus(&BusLevel::DEFAULT));
        assert!(!super::declares_addressed_bus(&BusLevel {
            size_cells: 1,
            ..addressed
        }));
        assert!(!super::declares_addressed_bus(&BusLevel {
            addr_cells: 2,
            ..addressed
        }));
    }

    fn irqs(node: &HwNode) -> Vec<u64> {
        node.resources()
            .iter()
            .filter(|r| r.kind() == Some(HwResourceKind::Irq))
            .map(HwResource::base)
            .collect()
    }

    /// A device carrying `interrupts = <line>` and, optionally, its own
    /// `interrupt-parent`.
    fn device(b: &mut FdtWriter, name: &str, compatible: &str, line: u32, parent: Option<u32>) {
        b.begin_node(name);
        b.prop_str("compatible", compatible);
        b.prop("interrupts", &line.to_be_bytes());
        if let Some(parent) = parent {
            b.prop_u32("interrupt-parent", parent);
        }
        b.end_node();
    }

    /// A tree whose root names the root controller, with a nested
    /// one-cell controller (phandle 2) that has devices of its own.
    fn nested_interrupt_tree() -> Vec<u8> {
        let mut b = FdtWriter::new();
        b.begin_node("");
        b.prop_u32("#address-cells", 1);
        b.prop_u32("#size-cells", 1);
        b.prop_u32("interrupt-parent", ROOT_INTC);
        b.begin_node("intc");
        b.prop_str("compatible", "test,root-intc");
        b.prop("interrupt-controller", &[]);
        b.prop_u32("#interrupt-cells", 1);
        b.prop_u32("phandle", ROOT_INTC);
        b.end_node();
        // A nested controller: its own line goes to the root controller,
        // its children's to it.
        b.begin_node("gpio");
        b.prop_str("compatible", "test,gpio");
        b.prop("interrupts", &40u32.to_be_bytes());
        b.prop("interrupt-controller", &[]);
        b.prop_u32("#interrupt-cells", 1);
        b.prop_u32("phandle", 2);
        device(&mut b, "button", "test,button", 3, None);
        b.end_node();
        device(&mut b, "inherits", "test,inherits", 50, None);
        device(&mut b, "rewired", "test,rewired", 0, Some(2));
        device(&mut b, "named-root", "test,named-root", 51, Some(ROOT_INTC));
        b.begin_node("malformed");
        b.prop_str("compatible", "test,malformed");
        b.prop("interrupts", &52u32.to_be_bytes());
        b.prop("interrupt-parent", &[0, 1]);
        b.end_node();
        b.end_node();
        b.build()
    }

    #[test]
    fn a_specifier_is_mapped_only_under_the_root_interrupt_controller() {
        let nodes = discover(&nested_interrupt_tree());
        assert_eq!(irqs(by_key(&nodes, b"test,inherits")), std::vec![50]);
        assert_eq!(irqs(by_key(&nodes, b"test,named-root")), std::vec![51]);
        // A nested controller's own line is the root controller's.
        assert_eq!(irqs(by_key(&nodes, b"test,gpio")), std::vec![40]);
        // Its children's lines, and a device wired to it by phandle, are
        // numbers in the nested controller's space: decoding them as the
        // root's would grant another device's line.
        assert!(irqs(by_key(&nodes, b"test,button")).is_empty());
        assert!(irqs(by_key(&nodes, b"test,rewired")).is_empty());
        // A malformed reference names no parent rather than inheriting one.
        assert!(irqs(by_key(&nodes, b"test,malformed")).is_empty());
    }

    #[test]
    fn a_tree_that_names_no_interrupt_parent_maps_no_specifier() {
        let mut b = FdtWriter::new();
        b.begin_node("");
        b.prop_u32("#address-cells", 1);
        b.prop_u32("#size-cells", 1);
        device(&mut b, "orphan", "test,orphan", 9, None);
        b.end_node();
        let nodes = discover(&b.build());
        assert!(irqs(by_key(&nodes, b"test,orphan")).is_empty());
    }

    /// The legacy controller's `/soc` in the shape of the pinned Pi 4 tree:
    /// a one-cell bus under a two-cell root, translating the peripherals and
    /// reaching RAM and the peripherals through two `dma-ranges` windows.
    /// A consumer sits ahead of its controller in document order, so its
    /// requests name an id the walk has not yet assigned.
    fn dma_tree() -> Vec<u8> {
        let cells =
            |values: &[u32]| -> Vec<u8> { values.iter().flat_map(|v| v.to_be_bytes()).collect() };
        let mut b = FdtWriter::new();
        b.begin_node("");
        b.prop_u32("#address-cells", 2);
        b.prop_u32("#size-cells", 1);
        b.prop_u32("interrupt-parent", ROOT_INTC);
        b.begin_node("soc");
        b.prop_str("compatible", "simple-bus");
        b.prop_u32("#address-cells", 1);
        b.prop_u32("#size-cells", 1);
        b.prop(
            "ranges",
            &cells(&[0x7e00_0000, 0, 0xfe00_0000, 0x0180_0000]),
        );
        b.prop(
            "dma-ranges",
            &cells(&[
                0xc000_0000,
                0,
                0,
                0x4000_0000,
                0x7c00_0000,
                0,
                0xfc00_0000,
                0x0380_0000,
            ]),
        );
        b.begin_node("i2s@7e203000");
        b.prop_str("compatible", "brcm,bcm2835-i2s");
        b.prop("reg", &cells(&[0x7e20_3000, 0x24]));
        b.prop("dmas", &cells(&[0x0c, 2, 0x0c, 3]));
        b.prop("dma-names", b"tx\0rx\0");
        b.end_node();
        b.begin_node("dma-controller@7e007000");
        b.prop_str("compatible", "brcm,bcm2835-dma");
        b.prop("reg", &cells(&[0x7e00_7000, 0xb00]));
        b.prop(
            "interrupts",
            &cells(&[80, 81, 82, 83, 84, 85, 86, 87, 87, 88, 88]),
        );
        b.prop_u32("#dma-cells", 1);
        b.prop_u32("dma-channel-mask", 0x7f5);
        b.prop_u32("phandle", 0x0c);
        b.end_node();
        b.begin_node("mmc@7e202000");
        b.prop_str("compatible", "brcm,bcm2835-sdhost");
        b.prop("reg", &cells(&[0x7e20_2000, 0x100]));
        b.prop("dmas", &cells(&[0x0c, 0x2000_000d]));
        b.prop("dma-names", b"rx-tx-and-more\0");
        b.end_node();
        // A controller whose binding is wider than a record: its entries are
        // dropped, but the entry after one still parses.
        b.begin_node("wide-dma");
        b.prop_str("compatible", "test,wide-dma");
        b.prop_u32("#dma-cells", 3);
        b.prop_u32("phandle", 0x20);
        b.end_node();
        b.begin_node("mixed");
        b.prop_str("compatible", "test,mixed");
        b.prop("dmas", &cells(&[0x20, 1, 2, 3, 0x0c, 6]));
        b.prop("dma-names", b"wide\0narrow\0");
        b.end_node();
        b.begin_node("dangling");
        b.prop_str("compatible", "test,dangling");
        b.prop("dmas", &cells(&[0x0c, 7, 0x99, 1, 0x0c, 8]));
        b.end_node();
        b.end_node();
        b.end_node();
        b.build()
    }

    fn requests(node: &HwNode) -> Vec<LinkRequest> {
        node.resources()
            .iter()
            .filter_map(|r| r.link_request().ok())
            .collect()
    }

    /// A clock manager over a fixed oscillator and a disabled clock source,
    /// and two consumers, one of whose `clocks` names the disabled source
    /// first.
    fn clock_tree() -> Vec<u8> {
        let cells =
            |values: &[u32]| -> Vec<u8> { values.iter().flat_map(|v| v.to_be_bytes()).collect() };
        let mut b = FdtWriter::new();
        b.begin_node("");
        b.prop_u32("#address-cells", 1);
        b.prop_u32("#size-cells", 1);
        b.begin_node("osc");
        b.prop_str("compatible", "fixed-clock");
        b.prop_u32("#clock-cells", 0);
        b.prop_u32("clock-frequency", 54_000_000);
        b.prop_u32("phandle", 3);
        b.end_node();
        b.begin_node("dsi@1000");
        b.prop_str("compatible", "test,dsi");
        b.prop_str("status", "disabled");
        b.prop_u32("#clock-cells", 1);
        b.prop_u32("phandle", 4);
        b.end_node();
        b.begin_node("cprman@2000");
        b.prop_str("compatible", "brcm,bcm2711-cprman");
        b.prop_u32("#clock-cells", 1);
        b.prop("clocks", &cells(&[3, 4, 0]));
        b.prop_u32("phandle", 8);
        b.end_node();
        b.begin_node("pwm@3000");
        b.prop_str("compatible", "brcm,bcm2835-pwm");
        b.prop("clocks", &cells(&[8, 0x1e]));
        b.prop("clock-names", b"pwm\0");
        b.end_node();
        b.begin_node("i2s@4000");
        b.prop_str("compatible", "brcm,bcm2835-i2s");
        b.prop("clocks", &cells(&[4, 0, 8, 0x1f]));
        b.prop("clock-names", b"dsi\0pcm\0");
        b.end_node();
        b.end_node();
        b.build()
    }

    #[test]
    fn a_clock_supplier_carries_its_duty_and_each_consumer_its_clocks_or_their_fixed_rates() {
        let nodes = discover(&clock_tree());
        let cprman = by_key(&nodes, b"brcm,bcm2711-cprman");
        let endpoint = CLOCK_CONTROLLER_ENDPOINTS.endpoint(cprman.id());
        let duties: Vec<LinkDuty> = cprman
            .resources()
            .iter()
            .filter_map(|r| r.link_duty().ok())
            .collect();
        assert_eq!(
            duties,
            std::vec![LinkDuty::new(endpoint, None).expect("valid")]
        );
        let fixed: Vec<(u8, u64)> = cprman
            .resources()
            .iter()
            .filter_map(HwResource::fixed_clock)
            .collect();
        assert_eq!(
            fixed,
            [(0, 54_000_000)],
            "the oscillator is a fact, not a link"
        );
        assert!(
            requests(cprman).is_empty(),
            "the disabled source is no supplier"
        );
        assert!(
            by_key(&nodes, b"fixed-clock")
                .resources()
                .iter()
                .all(|r| r.link_duty().is_err()),
            "no driver serves a fixed clock"
        );
        assert_eq!(
            requests(by_key(&nodes, b"brcm,bcm2835-pwm")),
            std::vec![LinkRequest::new(endpoint, 0, &[0x1e], b"pwm").expect("valid")]
        );
        assert_eq!(
            requests(by_key(&nodes, b"brcm,bcm2835-i2s")),
            std::vec![LinkRequest::new(endpoint, 1, &[0x1f], b"pcm").expect("valid")],
            "an entry naming an unusable supplier is framed and skipped"
        );
        assert!(nodes.iter().all(|n| n
            .match_keys()
            .iter()
            .all(|k| k.compatible_bytes() != b"test,dsi")));
    }

    /// Two sound cards: one described on the card itself, its codec driving
    /// both clocks and its bit clock inverted, and one by a `dai-link`
    /// sub-node, left justified, on the second interface of a two-interface
    /// CPU, its codec claiming the bit clock and inverting the frame clock by
    /// the legacy flags. The first card's codec states a legacy inversion its
    /// link-level masters override.
    fn sound_tree() -> Vec<u8> {
        let cells =
            |values: &[u32]| -> Vec<u8> { values.iter().flat_map(|v| v.to_be_bytes()).collect() };
        let mut b = FdtWriter::new();
        b.begin_node("");
        b.prop_u32("#address-cells", 1);
        b.prop_u32("#size-cells", 1);
        for (name, compatible, cells_count, phandle) in [
            ("i2s@1000", "brcm,bcm2835-i2s", 0, 0x20),
            ("dac", "ti,pcm5102a", 0, 0x21),
            ("tdm@2000", "test,tdm", 1, 0x22),
            ("adc", "ti,pcm5122", 0, 0x23),
        ] {
            b.begin_node(name);
            b.prop_str("compatible", compatible);
            b.prop_u32("#sound-dai-cells", cells_count);
            b.prop_u32("phandle", phandle);
            b.end_node();
        }
        b.begin_node("sound");
        b.prop_str("compatible", "simple-audio-card");
        b.prop("simple-audio-card,format", b"i2s\0");
        b.prop_u32("simple-audio-card,bitclock-master", 0x30);
        b.prop_u32("simple-audio-card,frame-master", 0x30);
        b.prop("simple-audio-card,bitclock-inversion", b"");
        b.begin_node("simple-audio-card,cpu");
        b.prop("sound-dai", &cells(&[0x20]));
        b.end_node();
        b.begin_node("simple-audio-card,codec");
        b.prop("sound-dai", &cells(&[0x21]));
        b.prop_u32("phandle", 0x30);
        b.prop("frame-inversion", b"");
        b.end_node();
        b.end_node();
        b.begin_node("sound-2");
        b.prop_str("compatible", "simple-audio-card");
        b.begin_node("simple-audio-card,dai-link@0");
        b.prop("format", b"left_j\0");
        b.begin_node("cpu");
        b.prop("sound-dai", &cells(&[0x22, 1]));
        b.end_node();
        b.begin_node("codec");
        b.prop("sound-dai", &cells(&[0x23]));
        b.prop("bitclock-master", b"");
        b.prop("frame-inversion", b"");
        b.end_node();
        b.end_node();
        b.end_node();
        b.end_node();
        b.build()
    }

    #[test]
    fn a_sound_card_links_each_cpu_interface_to_its_codec_and_gives_the_codec_its_duty() {
        let nodes = discover(&sound_tree());
        let codec_duties = |key: &[u8]| -> Vec<LinkDuty> {
            by_key(&nodes, key)
                .resources()
                .iter()
                .filter_map(|r| r.link_duty().ok())
                .collect()
        };
        let dac = by_key(&nodes, b"ti,pcm5102a").id();
        let adc = by_key(&nodes, b"ti,pcm5122").id();
        assert_eq!(
            codec_duties(b"ti,pcm5102a"),
            std::vec![LinkDuty::new(CODEC_ENDPOINTS.endpoint(dac), None).expect("valid")]
        );
        assert_eq!(
            codec_duties(b"ti,pcm5122"),
            std::vec![LinkDuty::new(CODEC_ENDPOINTS.endpoint(adc), None).expect("valid")]
        );
        assert!(
            codec_duties(b"brcm,bcm2835-i2s").is_empty(),
            "a CPU side is no codec"
        );
        let link = |key: &[u8]| -> (u64, DaiLink) {
            let found = requests(by_key(&nodes, key));
            let [request] = found.as_slice() else {
                panic!("one codec link, got {found:?}");
            };
            (
                request.endpoint(),
                DaiLink::from_cells(request.selector()).expect("a link"),
            )
        };
        assert_eq!(
            link(b"brcm,bcm2835-i2s"),
            (
                CODEC_ENDPOINTS.endpoint(dac),
                DaiLink {
                    format: DaiFormat::I2s,
                    codec_drives_bit_clock: true,
                    codec_drives_frame_clock: true,
                    inversion: ClockInversion::BitClock,
                    cpu_dai: 0,
                    codec_dai: 0,
                }
            )
        );
        assert_eq!(
            link(b"test,tdm"),
            (
                CODEC_ENDPOINTS.endpoint(adc),
                DaiLink {
                    format: DaiFormat::LeftJustified,
                    codec_drives_bit_clock: true,
                    codec_drives_frame_clock: false,
                    inversion: ClockInversion::FrameClock,
                    cpu_dai: 1,
                    codec_dai: 0,
                }
            )
        );
    }

    #[test]
    fn a_dma_controller_carries_its_duty_and_each_window_it_reaches_memory_through() {
        let nodes = discover(&dma_tree());
        let dma = by_key(&nodes, b"brcm,bcm2835-dma");
        assert_eq!(dma.class(), Some(HwDeviceClass::Dma));
        let duties: Vec<LinkDuty> = dma
            .resources()
            .iter()
            .filter_map(|r| r.link_duty().ok())
            .collect();
        assert_eq!(
            duties,
            std::vec![
                LinkDuty::new(DMA_CONTROLLER_ENDPOINTS.endpoint(dma.id()), Some(0x7f5))
                    .expect("valid")
            ]
        );
        let windows: Vec<HwResource> = dma
            .resources()
            .iter()
            .copied()
            .filter(|r| r.kind() == Some(HwResourceKind::Dma))
            .collect();
        assert_eq!(
            windows,
            std::vec![
                HwResource::dma_translated(
                    0x4000_0000,
                    0x4000_0000,
                    0xc000_0000,
                    tairix_abi::DmaCoherence::Snooped
                ),
                HwResource::dma_translated(
                    0xff80_0000,
                    0x0380_0000,
                    0x7c00_0000,
                    tairix_abi::DmaCoherence::Snooped
                ),
            ]
        );
        // Every line fits: the window, eleven lines, the duty and two windows.
        assert_eq!(irqs(dma).len(), 11);
        assert_eq!(dma.resources().len(), 15);
        // Channels 7/8 and 9/10 share a line, and each entry keeps the place
        // that says which channel it serves.
        let placed: Vec<(u64, u32)> = dma
            .resources()
            .iter()
            .filter_map(|r| Some((r.base(), r.interrupt_position()?)))
            .collect();
        assert_eq!(
            placed,
            std::vec![
                (80, 0),
                (81, 1),
                (82, 2),
                (83, 3),
                (84, 4),
                (85, 5),
                (86, 6),
                (87, 7),
                (87, 8),
                (88, 9),
                (88, 10)
            ]
        );
        assert!(dma
            .resources()
            .iter()
            .any(|r| r.kind() == Some(HwResourceKind::Mmio) && r.base() == 0xfe00_7000));
    }

    #[test]
    fn a_controller_below_an_identity_bus_reaches_memory_through_the_soc_above_it() {
        let cells =
            |values: &[u32]| -> Vec<u8> { values.iter().flat_map(|v| v.to_be_bytes()).collect() };
        let mut b = FdtWriter::new();
        b.begin_node("");
        b.prop_u32("#address-cells", 2);
        b.prop_u32("#size-cells", 1);
        b.begin_node("soc");
        b.prop_str("compatible", "simple-bus");
        b.prop_u32("#address-cells", 1);
        b.prop_u32("#size-cells", 1);
        b.prop("ranges", &[]);
        b.prop("dma-ranges", &cells(&[0xc000_0000, 0, 0, 0x4000_0000]));
        b.begin_node("sub");
        b.prop_str("compatible", "simple-bus");
        b.prop_u32("#address-cells", 1);
        b.prop_u32("#size-cells", 1);
        b.prop("ranges", &[]);
        b.prop("dma-ranges", &[]);
        b.begin_node("dma-controller@7e007000");
        b.prop_str("compatible", "brcm,bcm2835-dma");
        b.prop("reg", &cells(&[0x7e00_7000, 0xb00]));
        b.prop_u32("#dma-cells", 1);
        b.end_node();
        b.end_node();
        b.end_node();
        b.end_node();
        let nodes = discover(&b.build());
        let windows: Vec<HwResource> = by_key(&nodes, b"brcm,bcm2835-dma")
            .resources()
            .iter()
            .copied()
            .filter(|r| r.kind() == Some(HwResourceKind::Dma))
            .collect();
        assert_eq!(
            windows,
            std::vec![HwResource::dma_translated(
                0x4000_0000,
                0x4000_0000,
                0xc000_0000,
                tairix_abi::DmaCoherence::Snooped
            )]
        );
    }

    #[test]
    fn a_consumer_names_its_controller_even_before_the_walk_reaches_it() {
        let nodes = discover(&dma_tree());
        let dma = by_key(&nodes, b"brcm,bcm2835-dma");
        let i2s = by_key(&nodes, b"brcm,bcm2835-i2s");
        assert!(i2s.id() < dma.id());
        let endpoint = DMA_CONTROLLER_ENDPOINTS.endpoint(dma.id());
        assert_eq!(
            requests(i2s),
            std::vec![
                LinkRequest::new(endpoint, 0, &[2], b"tx").expect("valid"),
                LinkRequest::new(endpoint, 1, &[3], b"rx").expect("valid"),
            ]
        );
        // A name longer than a record holds leaves the line unnamed rather
        // than truncated; the specifier's serving bits ride along whole.
        let mmc = by_key(&nodes, b"brcm,bcm2835-sdhost");
        assert_eq!(
            requests(mmc),
            std::vec![LinkRequest::new(endpoint, 0, &[0x2000_000d], b"").expect("valid")]
        );
    }

    #[test]
    fn an_entry_wider_than_a_record_is_dropped_and_a_dangling_one_ends_the_list() {
        let nodes = discover(&dma_tree());
        let endpoint = DMA_CONTROLLER_ENDPOINTS.endpoint(by_key(&nodes, b"brcm,bcm2835-dma").id());
        // The three-cell entry is dropped whole; the next keeps its position
        // and its own name.
        assert_eq!(
            requests(by_key(&nodes, b"test,mixed")),
            std::vec![LinkRequest::new(endpoint, 1, &[6], b"narrow").expect("valid")]
        );
        // Past an unresolvable phandle nothing can be found, not even the
        // well-formed entry after it.
        assert_eq!(
            requests(by_key(&nodes, b"test,dangling")),
            std::vec![LinkRequest::new(endpoint, 0, &[7], b"").expect("valid")]
        );
        // The wide controller is still a controller, with its own duty.
        let wide = by_key(&nodes, b"test,wide-dma");
        assert_eq!(wide.class(), Some(HwDeviceClass::Dma));
    }

    /// [`BarePlatform`] on an architecture whose masters do not snoop unless
    /// their tree says so.
    struct UnsnoopedPlatform;

    impl FdtPlatform for UnsnoopedPlatform {
        const DEFAULT_DMA_COHERENCE: DmaCoherence = DmaCoherence::Unsnooped;

        fn interrupt_cells(&self) -> usize {
            1
        }

        fn from_tree(_fdt: &Fdt<'_>) -> Self {
            Self
        }

        fn interrupt_line(&self, specifier: &[u8]) -> Option<u32> {
            BarePlatform.interrupt_line(specifier)
        }

        fn root_interrupt_controller(&self) -> Option<u32> {
            Some(ROOT_INTC)
        }
    }

    /// A DMA controller `name` whose own node states `statements`.
    fn stating_controller(b: &mut FdtWriter, name: &str, statements: &[&str]) {
        b.begin_node(name);
        b.prop_str("compatible", &std::format!("test,{name}"));
        b.prop_u32("#dma-cells", 1);
        for statement in statements {
            b.prop(statement, &[]);
        }
        b.end_node();
    }

    /// A tree stating coherence at every level a master inherits it from,
    /// the root stating `root`.
    fn coherence_tree(root: &[&str]) -> Vec<u8> {
        let mut b = FdtWriter::new();
        b.begin_node("");
        b.prop_u32("#address-cells", 1);
        b.prop_u32("#size-cells", 1);
        for statement in root {
            b.prop(statement, &[]);
        }
        stating_controller(&mut b, "unstated", &[]);
        for (bus, statement) in [
            ("unsnooped-bus", "dma-noncoherent"),
            ("snooped-bus", "dma-coherent"),
        ] {
            b.begin_node(bus);
            b.prop_str("compatible", "simple-bus");
            b.prop("ranges", &[]);
            b.prop("dma-ranges", &[]);
            b.prop(statement, &[]);
            stating_controller(&mut b, &std::format!("{bus}-child"), &[]);
            stating_controller(&mut b, &std::format!("{bus}-snooper"), &["dma-coherent"]);
            stating_controller(
                &mut b,
                &std::format!("{bus}-both"),
                &["dma-coherent", "dma-noncoherent"],
            );
            b.begin_node(&std::format!("{bus}-unit"));
            b.prop_str("compatible", &std::format!("test,{bus}-unit"));
            b.prop_u32("#iommu-cells", 1);
            b.end_node();
            b.end_node();
        }
        b.end_node();
        b.build()
    }

    /// The coherence each DMA window of the node `compatible` names states.
    fn coherence_of(nodes: &[HwNode], compatible: &str) -> Vec<Option<DmaCoherence>> {
        by_key(nodes, compatible.as_bytes())
            .resources()
            .iter()
            .filter(|r| r.kind() == Some(HwResourceKind::Dma))
            .map(HwResource::dma_coherence)
            .collect()
    }

    #[test]
    fn a_master_s_dma_is_as_coherent_as_the_nearest_node_stating_it() {
        let nodes = discover(&coherence_tree(&[]));
        let (snooped, unsnooped) = (Some(DmaCoherence::Snooped), Some(DmaCoherence::Unsnooped));
        for (master, coherence) in [
            ("test,unstated", snooped),
            ("test,unsnooped-bus-child", unsnooped),
            ("test,unsnooped-bus-snooper", snooped),
            ("test,snooped-bus-child", snooped),
            ("test,snooped-bus-snooper", snooped),
            // Both statements at once claim nothing a master is trusted for.
            ("test,unsnooped-bus-both", unsnooped),
            ("test,snooped-bus-both", unsnooped),
            // A unit's own DMA is its tables', queues' and records'.
            ("test,unsnooped-bus-unit", unsnooped),
            ("test,snooped-bus-unit", snooped),
        ] {
            assert_eq!(
                coherence_of(&nodes, master),
                std::vec![coherence],
                "{master}"
            );
        }
    }

    #[test]
    fn a_pass_after_the_walk_reads_each_node_s_stated_coherence() {
        let blob = coherence_tree(&[]);
        let fdt = Fdt::new(&blob).expect("valid fdt");
        let stated = |wanted: &str| {
            super::emitted(&fdt)
                .find(|(_, node, _)| node.name() == wanted.as_bytes())
                .map(|(_, _, coherence)| coherence)
                .expect("emitted")
        };
        assert_eq!(stated("unstated"), None, "the convention is the caller's");
        assert_eq!(stated("unsnooped-bus-child"), Some(DmaCoherence::Unsnooped));
        assert_eq!(stated("unsnooped-bus-snooper"), Some(DmaCoherence::Snooped));
        assert_eq!(stated("snooped-bus-both"), Some(DmaCoherence::Unsnooped));
    }

    #[test]
    fn where_no_node_states_it_the_architecture_s_convention_holds() {
        let unsnooped = discover_on::<UnsnoopedPlatform>(&coherence_tree(&[]));
        assert_eq!(
            coherence_of(&unsnooped, "test,unstated"),
            std::vec![Some(DmaCoherence::Unsnooped)]
        );
        assert_eq!(
            coherence_of(&unsnooped, "test,snooped-bus-child"),
            std::vec![Some(DmaCoherence::Snooped)]
        );
        let stated = discover_on::<UnsnoopedPlatform>(&coherence_tree(&["dma-coherent"]));
        assert_eq!(
            coherence_of(&stated, "test,unstated"),
            std::vec![Some(DmaCoherence::Snooped)],
            "the root's own statement is inherited"
        );
    }

    #[test]
    fn a_controller_off_any_translating_bus_reaches_memory_untranslated() {
        let mut b = FdtWriter::new();
        b.begin_node("");
        b.prop_u32("#address-cells", 1);
        b.prop_u32("#size-cells", 1);
        b.begin_node("top-dma");
        b.prop_str("compatible", "test,top-dma");
        b.prop_u32("#dma-cells", 1);
        b.end_node();
        b.begin_node("identity-bus");
        b.prop_str("compatible", "simple-bus");
        b.prop("ranges", &[]);
        b.prop("dma-ranges", &[]);
        b.begin_node("identity-dma");
        b.prop_str("compatible", "test,identity-dma");
        b.prop_u32("#dma-cells", 1);
        b.end_node();
        b.end_node();
        b.begin_node("opaque-bus");
        b.prop_str("compatible", "simple-bus");
        b.prop("ranges", &[]);
        b.begin_node("opaque-dma");
        b.prop_str("compatible", "test,opaque-dma");
        b.prop_u32("#dma-cells", 1);
        b.end_node();
        b.end_node();
        b.end_node();
        let nodes = discover(&b.build());
        let windows = |compatible: &[u8]| -> Vec<HwResource> {
            by_key(&nodes, compatible)
                .resources()
                .iter()
                .copied()
                .filter(|r| r.kind() == Some(HwResourceKind::Dma))
                .collect()
        };
        assert_eq!(
            windows(b"test,top-dma"),
            std::vec![HwResource::dma(0, 0, tairix_abi::DmaCoherence::Snooped)]
        );
        assert_eq!(
            windows(b"test,identity-dma"),
            std::vec![HwResource::dma(0, 0, tairix_abi::DmaCoherence::Snooped)]
        );
        // A bus with no `dma-ranges` maps nothing for its children.
        assert!(windows(b"test,opaque-dma").is_empty());
        // No mask stated, so the duty says so.
        let duty = by_key(&nodes, b"test,top-dma")
            .resources()
            .iter()
            .find_map(|r| r.link_duty().ok())
            .expect("a duty");
        assert_eq!(duty.channels(), None);
    }

    #[test]
    fn the_generic_channel_mask_reads_one_cell_per_thirty_two_channels() {
        let mut b = FdtWriter::new();
        b.begin_node("");
        for (name, value) in [
            ("one", &[0u8, 0, 0x07, 0xf5][..]),
            ("two", &[0, 0, 0, 1, 0x80, 0, 0, 0][..]),
            ("ragged", &[0, 0, 1][..]),
        ] {
            b.begin_node(name);
            b.prop("dma-channel-mask", value);
            b.end_node();
        }
        b.begin_node("none");
        b.end_node();
        b.end_node();
        let blob = b.build();
        let fdt = Fdt::new(&blob).expect("valid fdt");
        let masks: Vec<Option<u64>> = fdt
            .nodes()
            .skip(1)
            .map(|n| super::dma_channel_mask(&n.expect("well formed")))
            .collect();
        assert_eq!(
            masks,
            std::vec![Some(0x7f5), Some(0x8000_0000_0000_0001), None, None]
        );
    }

    /// Silence the unused-import warning `Node` would otherwise raise while
    /// still proving the address decode reads exactly one cell.
    #[test]
    fn a_bus_child_address_is_one_cell_wide() {
        let mut b = FdtWriter::new();
        b.begin_node("");
        b.prop("reg", &[0x00, 0x00, 0x00, 0x68, 0xDE, 0xAD, 0xBE, 0xEF]);
        b.end_node();
        let blob = b.build();
        let fdt = Fdt::new(&blob).expect("valid fdt");
        let root: Node<'_> = fdt.nodes().next().expect("a node").expect("well formed");
        assert_eq!(super::bus_child_address(&root), Some(0x68));
    }

    const SMMU: u32 = 0x40;
    const WIDE_UNIT: u32 = 0x41;
    const OFF_UNIT: u32 = 0x42;
    const NOT_A_UNIT: u32 = 0x43;

    /// Masters of every shape ahead of the units they name: one taking
    /// one-cell specifiers, one taking two, a disabled one, and a node that
    /// is no unit at all.
    fn iommu_tree() -> Vec<u8> {
        let cells =
            |values: &[u32]| -> Vec<u8> { values.iter().flat_map(|v| v.to_be_bytes()).collect() };
        let master = |b: &mut FdtWriter, name: &str, compatible: &str, iommus: &[u32]| {
            b.begin_node(name);
            b.prop_str("compatible", compatible);
            b.prop("iommus", &cells(iommus));
            b.end_node();
        };
        let mut b = FdtWriter::new();
        b.begin_node("");
        b.prop_u32("#address-cells", 1);
        b.prop_u32("#size-cells", 1);
        master(
            &mut b,
            "display@1000",
            "test,display",
            &[SMMU, 0x100, SMMU, 0x101, SMMU, 0x105],
        );
        b.begin_node("dma@2000");
        b.prop_str("compatible", "test,translated-dma");
        b.prop_u32("#dma-cells", 1);
        b.prop("iommus", &cells(&[SMMU, 0x200]));
        b.end_node();
        master(
            &mut b,
            "codec@3000",
            "test,bypassing",
            &[WIDE_UNIT, 1, 0xF, OFF_UNIT, 7],
        );
        master(
            &mut b,
            "mixed@4000",
            "test,mixed",
            &[SMMU, 0x300, OFF_UNIT, 8],
        );
        master(&mut b, "dangling@5000", "test,dangling", &[0x99, 1]);
        master(&mut b, "short@6000", "test,short", &[WIDE_UNIT, 1]);
        master(&mut b, "odd@7000", "test,not-a-unit", &[NOT_A_UNIT, 1]);
        b.begin_node("dma@8000");
        b.prop_str("compatible", "test,refused-dma");
        b.prop_u32("#dma-cells", 1);
        b.prop("iommus", &cells(&[0x99, 1]));
        b.end_node();
        let many: Vec<u32> = (0..=u32::try_from(HW_NODE_MAX_RESOURCES).expect("small"))
            .flat_map(|i| [SMMU, i * 2])
            .collect();
        master(&mut b, "many@9000", "test,many", &many);
        b.begin_node("smmu@a000");
        b.prop_str("compatible", "arm,smmu-v3");
        b.prop("reg", &cells(&[0xA000, 0x2_0000]));
        b.prop_u32("#iommu-cells", 1);
        b.prop_u32("phandle", SMMU);
        b.end_node();
        b.begin_node("iommu@b000");
        b.prop_str("compatible", "arm,mmu-500");
        b.prop_u32("#iommu-cells", 2);
        b.prop_u32("phandle", WIDE_UNIT);
        b.end_node();
        b.begin_node("iommu@c000");
        b.prop_str("compatible", "arm,smmu-v3");
        b.prop_str("status", "disabled");
        b.prop_u32("#iommu-cells", 1);
        b.prop_u32("phandle", OFF_UNIT);
        b.end_node();
        b.begin_node("gpio@d000");
        b.prop_str("compatible", "test,gpio");
        b.prop_u32("phandle", NOT_A_UNIT);
        b.end_node();
        b.end_node();
        b.build()
    }

    fn streams(node: &HwNode) -> Vec<IommuStreams> {
        node.resources()
            .iter()
            .filter_map(|r| r.iommu_streams().ok())
            .collect()
    }

    fn dma_windows(node: &HwNode) -> usize {
        node.resources()
            .iter()
            .filter(|r| r.kind() == Some(HwResourceKind::Dma))
            .count()
    }

    #[test]
    fn a_unit_is_classed_by_its_specifier_width_and_a_disabled_one_is_not_emitted() {
        let nodes = discover(&iommu_tree());
        let smmu = by_key(&nodes, b"arm,smmu-v3");
        assert_eq!(smmu.class(), Some(HwDeviceClass::Iommu));
        assert_eq!(
            smmu.resources()[0],
            HwResource::mmio(0xA000, 0x2_0000),
            "its register window"
        );
        assert_eq!(
            by_key(&nodes, b"arm,mmu-500").class(),
            Some(HwDeviceClass::Iommu)
        );
        let smmus = nodes
            .iter()
            .filter(|n| {
                n.match_keys()
                    .iter()
                    .any(|k| k.compatible_bytes() == b"arm,smmu-v3")
            })
            .count();
        assert_eq!(smmus, 1, "the disabled unit is spliced out");
    }

    #[test]
    fn a_master_carries_its_streams_on_the_unit_it_names_and_masters_dma() {
        let nodes = discover(&iommu_tree());
        let unit = by_key(&nodes, b"arm,smmu-v3").id();
        let display = by_key(&nodes, b"test,display");
        assert!(
            display.id() < unit,
            "named before the walk reaches the unit"
        );
        assert_eq!(
            streams(display),
            std::vec![
                IommuStreams::new(unit, 0x100, 2).expect("valid"),
                IommuStreams::new(unit, 0x105, 1).expect("valid"),
            ],
            "consecutive ids on one unit are one range"
        );
        assert_eq!(
            dma_windows(display),
            1,
            "a master behind a unit masters DMA"
        );
        let dma = by_key(&nodes, b"test,translated-dma");
        assert_eq!(
            streams(dma),
            std::vec![IommuStreams::new(unit, 0x200, 1).expect("valid")]
        );
        assert_eq!(dma_windows(dma), 1);
    }

    #[test]
    fn a_master_naming_only_units_that_translate_nothing_masters_untranslated() {
        let nodes = discover(&iommu_tree());
        let codec = by_key(&nodes, b"test,bypassing");
        assert!(streams(codec).is_empty());
        assert_eq!(dma_windows(codec), 1);
    }

    #[test]
    fn a_master_whose_translation_cannot_be_described_gets_no_dma_authority() {
        let nodes = discover(&iommu_tree());
        for refused in [
            &b"test,mixed"[..],
            b"test,dangling",
            b"test,short",
            b"test,not-a-unit",
            b"test,many",
        ] {
            let node = by_key(&nodes, refused);
            assert!(streams(node).is_empty(), "{refused:?}");
            assert_eq!(dma_windows(node), 0, "{refused:?}");
        }
        // A controller keeps its duty, so its consumers can still find it,
        // but may carve nothing.
        let controller = by_key(&nodes, b"test,refused-dma");
        assert!(controller.resources().iter().any(|r| r.link_duty().is_ok()));
        assert_eq!(dma_windows(controller), 0);
    }

    #[test]
    fn a_spliced_subtree_shifts_no_id_a_request_a_duty_or_a_stream_names() {
        let cells =
            |values: &[u32]| -> Vec<u8> { values.iter().flat_map(|v| v.to_be_bytes()).collect() };
        let mut b = FdtWriter::new();
        b.begin_node("");
        b.prop_u32("#address-cells", 1);
        b.prop_u32("#size-cells", 1);
        b.begin_node("i2s@1000");
        b.prop_str("compatible", "test,consumer");
        b.prop("dmas", &cells(&[7, 3]));
        b.prop("iommus", &cells(&[SMMU, 0x10]));
        b.end_node();
        b.begin_node("bus@2000");
        b.prop_str("compatible", "simple-bus");
        b.prop_str("status", "disabled");
        b.begin_node("uart@0");
        b.prop_str("compatible", "arm,pl011");
        b.end_node();
        b.end_node();
        b.begin_node("i2c@3000");
        b.prop_str("compatible", "test,i2c");
        b.prop_u32("#address-cells", 1);
        b.prop_u32("#size-cells", 0);
        b.begin_node("rtc@68");
        b.prop_str("compatible", "test,disabled-rtc");
        b.prop_str("status", "disabled");
        b.prop("reg", &0x68u32.to_be_bytes());
        b.end_node();
        b.begin_node("codec@1a");
        b.prop_str("compatible", "test,codec");
        b.prop("reg", &0x1Au32.to_be_bytes());
        b.end_node();
        b.end_node();
        b.begin_node("dma@4000");
        b.prop_str("compatible", "test,controller");
        b.prop_u32("#dma-cells", 1);
        b.prop_u32("phandle", 7);
        b.end_node();
        b.begin_node("smmu@5000");
        b.prop_str("compatible", "arm,smmu-v3");
        b.prop_u32("#iommu-cells", 1);
        b.prop_u32("phandle", SMMU);
        b.end_node();
        b.end_node();
        let blob = b.build();
        let nodes = discover(&blob);
        assert!(
            nodes.iter().all(|n| n.match_keys().iter().all(|k| {
                k.compatible_bytes() != b"arm,pl011" && k.compatible_bytes() != b"test,disabled-rtc"
            })),
            "nothing below a disabled node, and no disabled node, is emitted"
        );
        let controller = by_key(&nodes, b"test,controller");
        let consumer = by_key(&nodes, b"test,consumer");
        assert_eq!(
            requests(consumer),
            std::vec![LinkRequest::new(
                DMA_CONTROLLER_ENDPOINTS.endpoint(controller.id()),
                0,
                &[3],
                b""
            )
            .expect("valid")]
        );
        let unit = by_key(&nodes, b"arm,smmu-v3").id();
        assert_eq!(
            streams(consumer),
            std::vec![IommuStreams::new(unit, 0x10, 1).expect("valid")]
        );
        let codec = by_key(&nodes, b"test,codec");
        assert_eq!(
            duties(by_key(&nodes, b"test,i2c")),
            std::vec![(BUS_CHILD_ENDPOINTS.endpoint(codec.id()), 0x1A)]
        );
        assert_eq!(
            endpoints(codec),
            std::vec![BUS_CHILD_ENDPOINTS.endpoint(codec.id())]
        );
        // The numbering a later pass reads the tree by is the walk's own.
        let fdt = Fdt::new(&blob).expect("valid fdt");
        let numbered: Vec<u32> = super::emitted(&fdt).map(|(id, _, _)| id).collect();
        let walked: Vec<u32> = nodes.iter().skip(1).map(HwNode::id).collect();
        assert_eq!(numbered, walked);
    }

    #[test]
    fn a_provider_is_named_by_how_the_walk_treats_it() {
        let blob = iommu_tree();
        let fdt = Fdt::new(&blob).expect("valid fdt");
        let nodes = discover(&blob);
        let smmu = by_key(&nodes, b"arm,smmu-v3").id();
        assert!(
            matches!(super::provider(&fdt, SMMU), Some(super::Provider::Emitted(id, _)) if id == smmu)
        );
        assert!(matches!(
            super::provider(&fdt, OFF_UNIT),
            Some(super::Provider::Unusable(_))
        ));
        assert!(super::provider(&fdt, 0x99).is_none());
        let mut b = FdtWriter::new();
        b.begin_node("");
        b.begin_node("bare");
        b.prop_u32("phandle", 3);
        b.end_node();
        b.end_node();
        let bare = b.build();
        let fdt = Fdt::new(&bare).expect("valid fdt");
        assert!(matches!(
            super::provider(&fdt, 3),
            Some(super::Provider::Undescribed(_))
        ));
    }

    /// A disabled subtree deeper than the walk tracks is one it never
    /// enters, so a provider after it is still found.
    #[test]
    fn a_provider_after_a_deep_disabled_subtree_is_found() {
        let mut b = FdtWriter::new();
        b.begin_node("");
        b.begin_node("off");
        b.prop_str("status", "disabled");
        for level in 0..super::MAX_WALK_DEPTH {
            b.begin_node(if level % 2 == 0 { "a" } else { "b" });
        }
        for _ in 0..super::MAX_WALK_DEPTH {
            b.end_node();
        }
        b.end_node();
        b.begin_node("unit");
        b.prop_str("compatible", "arm,smmu-v3");
        b.prop_u32("phandle", 7);
        b.end_node();
        b.end_node();
        let blob = b.build();
        let fdt = Fdt::new(&blob).expect("valid fdt");
        assert!(matches!(
            super::provider(&fdt, 7),
            Some(super::Provider::Emitted(..))
        ));
    }

    /// [`BarePlatform`], reading a specifier's second cell as its trigger:
    /// non-zero for an edge.
    struct TriggeredPlatform;

    impl FdtPlatform for TriggeredPlatform {
        const DEFAULT_DMA_COHERENCE: DmaCoherence = DmaCoherence::Snooped;

        fn interrupt_cells(&self) -> usize {
            2
        }

        fn from_tree(_fdt: &Fdt<'_>) -> Self {
            Self
        }

        fn interrupt_line(&self, specifier: &[u8]) -> Option<u32> {
            Some(u32::from_be_bytes(specifier.get(..4)?.try_into().ok()?))
        }

        fn edge_triggered(&self, specifier: &[u8]) -> bool {
            specifier.get(4..8).is_some_and(|cell| cell != [0; 4])
        }

        fn root_interrupt_controller(&self) -> Option<u32> {
            Some(ROOT_INTC)
        }
    }

    #[test]
    fn a_line_its_specifier_raises_by_edges_is_granted_as_one() {
        let cells =
            |values: &[u32]| -> Vec<u8> { values.iter().flat_map(|v| v.to_be_bytes()).collect() };
        let mut b = FdtWriter::new();
        b.begin_node("");
        b.prop_u32("interrupt-parent", ROOT_INTC);
        b.begin_node("intc");
        b.prop_str("compatible", "test,root-intc");
        b.prop("interrupt-controller", &[]);
        b.prop_u32("#interrupt-cells", 2);
        b.prop_u32("phandle", ROOT_INTC);
        b.end_node();
        b.begin_node("smmu");
        b.prop_str("compatible", "test,pulsing");
        b.prop("interrupts", &cells(&[74, 1, 75, 0]));
        b.end_node();
        b.end_node();
        let nodes = discover_on::<TriggeredPlatform>(&b.build());
        let irqs: Vec<(u64, bool, Option<u32>)> = by_key(&nodes, b"test,pulsing")
            .resources()
            .iter()
            .filter(|r| r.kind() == Some(HwResourceKind::Irq))
            .map(|r| (r.base(), r.is_edge_triggered(), r.interrupt_position()))
            .collect();
        assert_eq!(irqs, std::vec![(74, true, Some(0)), (75, false, Some(1))]);
    }
}
