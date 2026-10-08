//! The generic PCI host bridge a device tree describes
//! (`pci-host-ecam-generic`): its configuration region, buses and segment,
//! the windows it forwards, how its INTx lines reach their interrupt
//! controller, how its requester ids reach translation units and MSI
//! controllers, and the ports it marks external-facing.
//!
//! Layouts are the IEEE 1275 PCI bus binding's and those of the Devicetree
//! spec v0.4 §2.4.

use crate::bus::{translate, translated_reg, BusLevel, MAX_WALK_DEPTH};
use crate::idmap::IdMap;
use crate::{bus_level, read_cells, read_int_cells, Fdt, FdtError, Node};

/// The `compatible` of a host bridge with flat configuration space.
pub const ECAM_HOST: &[u8] = b"pci-host-ecam-generic";

/// Windows one host may describe: a validation bound on hostile input, not
/// a capacity — a host forwards one window of each space.
pub const MAX_PCI_WINDOWS: usize = 8;

/// The interrupt specifier cells a parent controller may take.
pub const MAX_INTERRUPT_CELLS: usize = 4;

/// A PCI address's three cells.
const PCI_ADDRESS_CELLS: u32 = 3;
const SPACE_SHIFT: u32 = 24;
const PREFETCHABLE: u32 = 1 << 30;

/// The space a PCI address names (`phys.hi` bits 25:24).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PciSpace {
    /// I/O ports.
    Io,
    /// Memory below the 4 GiB line.
    Memory32,
    /// Memory anywhere.
    Memory64,
}

/// One window a host forwards: PCI addresses `[pci, pci + size)` of
/// `space`, reached by the CPU from `cpu` up.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct PciWindow {
    /// What it forwards.
    pub space: PciSpace,
    /// Whether it is marked prefetchable.
    pub prefetchable: bool,
    /// Its first PCI address.
    pub pci: u64,
    /// The CPU physical address of `pci`.
    pub cpu: u64,
    /// Its length.
    pub size: u64,
}

/// An interrupt specifier: the controller it names and the cells it gives
/// it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct InterruptSpec {
    /// The controller's phandle.
    pub parent: u32,
    /// How many of `cells` it takes.
    pub len: usize,
    /// The specifier.
    pub cells: [u32; MAX_INTERRUPT_CELLS],
}

impl InterruptSpec {
    /// The specifier's cells.
    #[must_use]
    pub fn cells(&self) -> &[u32] {
        &self.cells[..self.len]
    }
}

/// One generic host bridge.
#[derive(Copy, Clone, Debug)]
pub struct PciHost<'a> {
    /// Its configuration region: CPU physical base and length.
    pub ecam: (u64, u64),
    /// The buses it decodes, first and last.
    pub buses: (u8, u8),
    /// Its segment (`linux,pci-domain`), 0 where none is named.
    pub segment: u16,
    /// Firmware set its resources out and they are to be kept: what a
    /// nonzero `/chosen/linux,pci-probe-only` says of every host.
    pub probe_only: bool,
    windows: [Option<PciWindow>; MAX_PCI_WINDOWS],
    /// Where its node sits, and how deep, so its ports are found in its
    /// subtree when asked rather than held in a list of fixed length.
    node: (usize, u32),
    interrupt_map: Option<InterruptMap<'a>>,
    iommu_map: Result<Option<IdMap<'a>>, FdtError>,
    msi_map: Result<Option<IdMap<'a>>, FdtError>,
    /// `msi-parent`'s controller; `Err` for one naming no node.
    msi_parent: Result<Option<u32>, FdtError>,
}

#[derive(Copy, Clone, Debug)]
struct InterruptMap<'a> {
    entries: &'a [u8],
    mask: [u32; 4],
}

impl<'a> PciHost<'a> {
    /// Its node's [`Node::offset`](crate::Node::offset), by which a pass
    /// over the tree finds it.
    #[must_use]
    pub const fn offset(&self) -> usize {
        self.node.0
    }

    /// The windows it forwards.
    pub fn windows(&self) -> impl Iterator<Item = PciWindow> + '_ {
        self.windows.iter().flatten().copied()
    }

    /// How its requester ids reach translation units (`iommu-map`,
    /// `iommu-map-mask`); `Ok(None)` for a host whose devices master DMA
    /// through none.
    ///
    /// # Errors
    ///
    /// [`FdtError::BadProperty`] for a map that does not decode: the host's
    /// devices are behind units no one can name their streams on.
    pub fn iommu_map(&self) -> Result<Option<IdMap<'a>>, FdtError> {
        self.iommu_map
    }

    /// The MSI controller the messages of requester id `requester` are
    /// written to, and the id they reach it as (`pci-msi.txt`): through
    /// `msi-map` where the host has one, a requester it maps none of raising
    /// none; else to `msi-parent`'s controller, as the requester id itself.
    /// `Ok(None)` for a host naming neither.
    ///
    /// # Errors
    ///
    /// [`FdtError::BadProperty`] for an `msi-map` that does not decode or an
    /// `msi-parent` naming no node: no controller can be told the host's
    /// messages apart.
    pub fn msi_target(&self, requester: u32) -> Result<Option<(u32, u32)>, FdtError> {
        match self.msi_map? {
            Some(map) => Ok(map.map(requester)),
            None => Ok(self.msi_parent?.map(|parent| (parent, requester))),
        }
    }

    /// Whether the port at configuration `address` — `bus << 16 | device <<
    /// 11 | function << 8` — is marked external-facing in `fdt`, the tree the
    /// host was read from. A tree that cannot be read says it is, so nothing
    /// behind it is trusted.
    #[must_use]
    pub fn external_facing(&self, fdt: &Fdt<'a>, address: u32) -> bool {
        let (offset, depth) = self.node;
        let mut nodes = fdt.operational_nodes();
        let found = nodes.any(|node| node.is_ok_and(|node| node.offset() == offset));
        if !found {
            return true;
        }
        for node in nodes {
            let Ok(node) = node else {
                return true;
            };
            if node.depth() <= depth {
                break;
            }
            let marked = node.property("external-facing").is_some();
            let at = node
                .property("reg")
                .and_then(|reg| reg.read_be_u32(0).ok())
                .is_some_and(|reg| reg & 0x00FF_FF00 == address & 0x00FF_FF00);
            if marked && at {
                return true;
            }
        }
        false
    }

    /// Visit each translation unit that is a function on the host's root bus
    /// — a child of its node holding `#iommu-cells`, as the `virtio,pci-iommu`
    /// binding places one — with its requester id: the root bus `bus-range`
    /// starts at, and the device and function its `reg` names. A unit below a
    /// bridge is no root-bus function, and is not visited.
    pub fn units(&self, fdt: &Fdt<'a>, visit: &mut dyn FnMut(u16, &Node<'a>)) {
        let (offset, depth) = self.node;
        let mut nodes = fdt.operational_nodes();
        if !nodes.any(|node| node.is_ok_and(|node| node.offset() == offset)) {
            return;
        }
        for node in nodes {
            let Ok(node) = node else {
                return;
            };
            if node.depth() <= depth {
                return;
            }
            if node.depth() != depth + 1 || crate::iommu::iommu_cells(&node).is_none() {
                continue;
            }
            // `reg`'s first cell is the PCI binding's `phys.hi`: its third
            // byte the device and function.
            let Some(devfn) = node
                .property("reg")
                .and_then(|reg| reg.read_be_u32(0).ok())
                .map(|reg| reg.to_be_bytes()[2])
            else {
                continue;
            };
            visit(u16::from_be_bytes([self.buses.0, devfn]), &node);
        }
    }

    /// The interrupt INTx pin `pin` (1 for INTA) of the device in slot
    /// `device` on the root bus raises, through `interrupt-map`; [`None`]
    /// where the map names none or a parent `fdt` does not describe.
    #[must_use]
    pub fn intx(&self, fdt: &Fdt<'a>, device: u8, pin: u8) -> Option<InterruptSpec> {
        let map = self.interrupt_map?;
        let wanted = [
            u32::from(self.buses.0) << 16 | u32::from(device) << 11,
            0,
            0,
            u32::from(pin),
        ];
        let mut parents: Option<(u32, usize, usize)> = None;
        let mut found = None;
        let mut off = 0;
        // Every entry is read, so a map that does not frame to its end answers
        // nothing at all rather than what its first entries say.
        while off < map.entries.len() {
            let child: [u32; 4] =
                core::array::from_fn(|i| cell(map.entries, off + 4 * i).unwrap_or(u32::MAX));
            let phandle = cell(map.entries, off + 16)?;
            let (address_cells, interrupt_cells) = match parents {
                Some((cached, a, i)) if cached == phandle => (a, i),
                _ => {
                    let parent = fdt.node_by_phandle(phandle)?;
                    let a =
                        usize::try_from(cells_of(&parent, "#address-cells").unwrap_or(0)).ok()?;
                    let i = usize::try_from(cells_of(&parent, "#interrupt-cells")?).ok()?;
                    parents = Some((phandle, a, i));
                    (a, i)
                }
            };
            if interrupt_cells > MAX_INTERRUPT_CELLS {
                return None;
            }
            let specifier = off + 20 + 4 * address_cells;
            let mut cells = [0; MAX_INTERRUPT_CELLS];
            for (index, slot) in cells.iter_mut().take(interrupt_cells).enumerate() {
                *slot = cell(map.entries, specifier + 4 * index)?;
            }
            let matches = child
                .iter()
                .zip(&wanted)
                .zip(&map.mask)
                .all(|((have, want), mask)| have & mask == want & mask);
            if matches && found.is_none() {
                found = Some(InterruptSpec {
                    parent: phandle,
                    len: interrupt_cells,
                    cells,
                });
            }
            off = specifier + 4 * interrupt_cells;
        }
        found.filter(|_| off == map.entries.len())
    }
}

fn cell(value: &[u8], off: usize) -> Option<u32> {
    let bytes = value.get(off..off.checked_add(4)?)?;
    Some(u32::from_be_bytes(bytes.try_into().ok()?))
}

fn cells_of(node: &Node<'_>, name: &str) -> Option<u32> {
    node.property(name)?.read_be_u32(0).ok()
}

/// Every operational generic host bridge in `fdt`, handed to `visit` once its
/// subtree is read. A host whose `reg`, `bus-range` or windows cannot be
/// decoded, or that names more windows or external-facing ports than the
/// bounds allow, is skipped: it is never half-described.
pub fn each_pci_host<'a>(fdt: &Fdt<'a>, mut visit: impl FnMut(PciHost<'a>)) {
    let probe_only = probe_only(fdt);
    let mut levels = [BusLevel::DEFAULT; MAX_WALK_DEPTH];
    // The depth of the host being walked, whose subtree holds no other.
    let mut inside: Option<usize> = None;
    for node in fdt.operational_nodes() {
        let Ok(node) = node else { break };
        let depth = node.depth() as usize;
        if depth >= MAX_WALK_DEPTH {
            break;
        }
        if inside.is_some_and(|host| depth <= host) {
            inside = None;
        }
        let level = bus_level(&node);
        if depth == 0 {
            levels[0] = BusLevel {
                ranges: None,
                ..level
            };
            continue;
        }
        if inside.is_none() && node.is_compatible(ECAM_HOST) {
            inside = Some(depth);
            if let Some(host) = host_of(&node, &levels, depth, probe_only) {
                visit(host);
            }
        }
        levels[depth] = level;
    }
}

/// `/chosen/linux,pci-probe-only`, reached through the node walk alone: a
/// whole-tree path lookup is not safe on a boot that reads the tree with its
/// MMU off.
fn probe_only(fdt: &Fdt<'_>) -> bool {
    fdt.nodes()
        .map_while(Result::ok)
        .find(|node| node.depth() == 1 && node.name() == b"chosen")
        .and_then(|chosen| chosen.property("linux,pci-probe-only"))
        .and_then(|keep| read_int_cells(keep.value()))
        .is_some_and(|keep| keep != 0)
}

fn host_of<'a>(
    node: &Node<'a>,
    levels: &[BusLevel<'a>],
    depth: usize,
    probe_only: bool,
) -> Option<PciHost<'a>> {
    let ecam = translated_reg(node, depth, levels, 0)?;
    let buses = match node.property("bus-range") {
        Some(range) => {
            let first = u8::try_from(range.read_be_u32(0).ok()?).ok()?;
            let last = u8::try_from(range.read_be_u32(4).ok()?).ok()?;
            (first <= last).then_some((first, last))?
        }
        None => (0, 0xFF),
    };
    let segment = match node.property("linux,pci-domain") {
        Some(domain) => u16::try_from(domain.read_be_u32(0).ok()?).ok()?,
        None => 0,
    };
    let own = bus_level(node);
    if own.addr_cells != PCI_ADDRESS_CELLS {
        return None;
    }
    let parent_cells = levels.get(depth - 1)?.addr_cells;
    let windows = windows_of(own, parent_cells, levels, depth)?;
    let interrupt_map = match (
        node.property("interrupt-map"),
        node.property("interrupt-map-mask"),
    ) {
        // A mask names every child cell: a short one would match everything.
        (Some(map), Some(mask))
            if cells_of(node, "#interrupt-cells") == Some(1) && mask.value().len() == 16 =>
        {
            let mask: [u32; 4] = core::array::from_fn(|i| cell(mask.value(), 4 * i).unwrap_or(0));
            Some(InterruptMap {
                entries: map.value(),
                mask,
            })
        }
        _ => None,
    };
    Some(PciHost {
        ecam,
        buses,
        segment,
        probe_only,
        windows,
        node: (node.offset(), node.depth()),
        interrupt_map,
        iommu_map: IdMap::of(node, "iommu-map", "iommu-map-mask"),
        msi_map: IdMap::of(node, "msi-map", "msi-map-mask"),
        msi_parent: msi_parent_of(node),
    })
}

/// The controller `node`'s `msi-parent` names: its first cell, any specifier
/// after it being one a PCI host's requester ids replace.
fn msi_parent_of(node: &Node<'_>) -> Result<Option<u32>, FdtError> {
    let Some(parent) = node.property("msi-parent") else {
        return Ok(None);
    };
    parent
        .read_be_u32(0)
        .ok()
        .and_then(crate::phandle_ref)
        .map(Some)
        .ok_or(FdtError::BadProperty)
}

/// The windows the host's `ranges` names, each CPU address translated to
/// the root; a configuration-space entry names none.
fn windows_of(
    own: BusLevel<'_>,
    parent_cells: u32,
    levels: &[BusLevel<'_>],
    depth: usize,
) -> Option<[Option<PciWindow>; MAX_PCI_WINDOWS]> {
    let ranges = own.ranges?;
    let child_cells = PCI_ADDRESS_CELLS as usize;
    let entry = 4 * (child_cells + parent_cells as usize + own.size_cells as usize);
    if entry == 0 || ranges.len() % entry != 0 {
        return None;
    }
    let mut windows = [None; MAX_PCI_WINDOWS];
    let mut slots = windows.iter_mut();
    for off in (0..ranges.len()).step_by(entry) {
        let high = cell(ranges, off)?;
        let space = match (high >> SPACE_SHIFT) & 0b11 {
            0b01 => PciSpace::Io,
            0b10 => PciSpace::Memory32,
            0b11 => PciSpace::Memory64,
            _ => continue,
        };
        let pci = read_cells(ranges, off + 4, 2)?;
        let parent = read_cells(ranges, off + 4 * child_cells, parent_cells)?;
        let size = read_cells(
            ranges,
            off + 4 * (child_cells + parent_cells as usize),
            own.size_cells,
        )?;
        let cpu = translate(levels, depth, parent)?;
        *slots.next()? = Some(PciWindow {
            space,
            prefetchable: high & PREFETCHABLE != 0,
            pci,
            cpu,
            size,
        });
    }
    Some(windows)
}

#[cfg(test)]
#[path = "pci_tests.rs"]
mod tests;
