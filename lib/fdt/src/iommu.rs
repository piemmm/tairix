//! The IOMMU binding's per-unit and per-master facts (Linux
//! `Documentation/devicetree/bindings/iommu/iommu.txt` and
//! `reserved-memory/reserved-memory.yaml`): what marks a translation unit,
//! which id an `iommus` specifier names its master by, and the windows a
//! master's reserved-memory regions ask its translation to keep.

use crate::bus::{bus_level, translated_reg, BusLevel, MAX_WALK_DEPTH};
use crate::specifier::PhandleArgs;
use crate::{be_u32, phandle_ref, read_cells, Fdt, FdtError, Node};

/// A translation unit's specifier width, `#iommu-cells`: what marks a node as
/// one. [`None`] for a node without the property, or whose value is not a
/// single cell.
#[must_use]
pub fn iommu_cells(node: &Node<'_>) -> Option<u32> {
    let cells = node.property("#iommu-cells")?;
    if cells.value().len() != 4 {
        return None;
    }
    cells.read_be_u32(0).ok()
}

/// The id a one-cell `iommus` specifier names its master by. Every other
/// width means what only its unit's own binding says (an `SMMUv2`'s id and
/// mask, a unit with a single master), so none is read from it.
#[must_use]
pub fn stream_id(specifier: &PhandleArgs<'_>) -> Option<u32> {
    if specifier.len() == 1 {
        specifier.cell(0)
    } else {
        None
    }
}

/// A window a reserved-memory region asks a master's translation to keep.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct IommuAddress {
    /// The first I/O address, as the master addresses it.
    pub iova: u64,
    /// The window's length, at least one byte.
    pub len: u64,
    /// The region's own memory, CPU physical base and length: [`None`] for a
    /// region that only keeps the I/O range out of use.
    pub memory: Option<(u64, u64)>,
}

impl IommuAddress {
    /// Whether the window maps the region's memory at its own address: what
    /// firmware still mastering it needs.
    #[must_use]
    pub fn is_identity(&self) -> bool {
        self.memory == Some((self.iova, self.len))
    }
}

/// Every window the regions `master` lists in its `memory-region` ask its
/// translation to keep, handed to `visit`. A region only binds a master it is
/// listed by, so a master without a phandle or a `memory-region` keeps none.
///
/// # Errors
///
/// [`FdtError::BadProperty`] when a window cannot be read — a listed region
/// is unknown or its `reg` does not decode, or an `iommu-addresses` entry
/// does not frame. A window that cannot be read is never dropped: firmware
/// may still master it.
pub fn each_iommu_address<'a>(
    fdt: &Fdt<'a>,
    master: &Node<'a>,
    visit: &mut dyn FnMut(IommuAddress),
) -> Result<(), FdtError> {
    let (Some(own), Some(regions)) = (master.phandle(), master.property("memory-region")) else {
        return Ok(());
    };
    let regions = regions.value();
    if regions.len() % 4 != 0 {
        return Err(FdtError::BadProperty);
    }
    let own_cells = parent_cells(fdt, own).ok_or(FdtError::BadProperty)?;
    for &raw in regions.as_chunks::<4>().0 {
        let phandle = phandle_ref(u32::from_be_bytes(raw)).ok_or(FdtError::BadProperty)?;
        let region = locate(fdt, phandle).ok_or(FdtError::BadProperty)?;
        let Some(addresses) = region.node.property("iommu-addresses") else {
            continue;
        };
        if !region.node.is_enabled() {
            continue;
        }
        let memory = match region.node.property("reg") {
            None => None,
            Some(_) => Some(
                translated_reg(&region.node, region.depth, &region.levels, 0)
                    .ok_or(FdtError::BadProperty)?,
            ),
        };
        let value = addresses.value();
        let mut off = 0;
        while off < value.len() {
            let named = be_u32(value, off)
                .and_then(phandle_ref)
                .ok_or(FdtError::BadProperty)?;
            let (address_cells, size_cells) = if named == own {
                own_cells
            } else {
                parent_cells(fdt, named).ok_or(FdtError::BadProperty)?
            };
            let start = off + 4;
            // `read_cells` refuses a width past two cells, so the offsets
            // after each read cannot overflow.
            let iova = read_cells(value, start, address_cells).ok_or(FdtError::BadProperty)?;
            let size_at = start + 4 * address_cells as usize;
            let len = read_cells(value, size_at, size_cells).ok_or(FdtError::BadProperty)?;
            off = size_at + 4 * size_cells as usize;
            if named == own && len != 0 {
                visit(IommuAddress { iova, len, memory });
            }
        }
    }
    Ok(())
}

/// A node found by its phandle, with the bus levels above it.
struct Located<'a> {
    node: Node<'a>,
    depth: usize,
    levels: [BusLevel<'a>; MAX_WALK_DEPTH],
}

fn locate<'a>(fdt: &Fdt<'a>, phandle: u32) -> Option<Located<'a>> {
    let mut levels = [BusLevel::DEFAULT; MAX_WALK_DEPTH];
    for node in fdt.nodes() {
        let node = node.ok()?;
        let depth = node.depth() as usize;
        if depth >= MAX_WALK_DEPTH {
            return None;
        }
        if node.phandle() == Some(phandle) {
            return Some(Located {
                node,
                depth,
                levels,
            });
        }
        levels[depth] = bus_level(&node);
        if depth == 0 {
            levels[0].ranges = None;
        }
    }
    None
}

/// The `#address-cells` and `#size-cells` governing the `reg` of the node
/// whose phandle is `phandle`: its parent's.
fn parent_cells(fdt: &Fdt<'_>, phandle: u32) -> Option<(u32, u32)> {
    let located = locate(fdt, phandle)?;
    let parent = located.levels.get(located.depth.checked_sub(1)?)?;
    Some((parent.addr_cells, parent.size_cells))
}

#[cfg(test)]
mod tests {
    use super::{each_iommu_address, iommu_cells, stream_id, IommuAddress};
    use crate::fixture::DtbBuilder;
    use crate::specifier::phandle_args;
    use crate::{Fdt, FdtError, Node};
    use alloc::vec::Vec;

    const MASTER: u32 = 10;
    const OTHER: u32 = 11;
    const SCANOUT: u32 = 20;
    const HOLE: u32 = 21;

    fn cells(values: &[u32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_be_bytes()).collect()
    }

    /// A display controller under a one-cell bus whose firmware scans out of
    /// `scanout` through its unit, and another master on a two-cell bus.
    fn tree(regions: &[u32], addresses: &[u32], scanout_reg: bool) -> Vec<u8> {
        let mut b = DtbBuilder::new();
        b.begin_node("");
        b.prop_u32("#address-cells", 2);
        b.prop_u32("#size-cells", 2);
        b.begin_node("reserved-memory");
        b.prop_u32("#address-cells", 2);
        b.prop_u32("#size-cells", 2);
        b.prop("ranges", &[]);
        b.begin_node("framebuffer@80000000");
        if scanout_reg {
            b.prop("reg", &cells(&[0, 0x8000_0000, 0, 0x80_0000]));
        }
        b.prop("iommu-addresses", &cells(addresses));
        b.prop_u32("phandle", SCANOUT);
        b.end_node();
        b.begin_node("hole");
        b.prop("iommu-addresses", &cells(&[MASTER, 0x1000, 0x1000]));
        b.prop_u32("phandle", HOLE);
        b.end_node();
        b.end_node();
        b.begin_node("soc");
        b.prop_u32("#address-cells", 1);
        b.prop_u32("#size-cells", 1);
        b.prop("ranges", &[]);
        b.begin_node("display@1000");
        b.prop("memory-region", &cells(regions));
        b.prop_u32("phandle", MASTER);
        b.end_node();
        b.end_node();
        b.begin_node("dma@2000");
        b.prop_u32("phandle", OTHER);
        b.end_node();
        b.begin_node("smmu@3000");
        b.prop_u32("#iommu-cells", 1);
        b.end_node();
        b.end_node();
        b.build()
    }

    fn named<'a>(fdt: &Fdt<'a>, name: &[u8]) -> Node<'a> {
        fdt.nodes()
            .filter_map(Result::ok)
            .find(|node| node.name() == name)
            .expect("node")
    }

    fn windows(blob: &[u8]) -> Result<Vec<IommuAddress>, FdtError> {
        let fdt = Fdt::new(blob).expect("valid fdt");
        let master = named(&fdt, b"display@1000");
        let mut found = Vec::new();
        each_iommu_address(&fdt, &master, &mut |window| found.push(window))?;
        Ok(found)
    }

    #[test]
    fn a_listed_region_keeps_the_masters_window_at_its_own_address() {
        let found = windows(&tree(
            &[SCANOUT],
            &[OTHER, 0, 0x4000, 0, 0x1000, MASTER, 0x8000_0000, 0x80_0000],
            true,
        ))
        .expect("decodes");
        assert_eq!(
            found,
            [IommuAddress {
                iova: 0x8000_0000,
                len: 0x80_0000,
                memory: Some((0x8000_0000, 0x80_0000)),
            }]
        );
        assert!(found[0].is_identity());
    }

    #[test]
    fn a_region_without_memory_only_keeps_its_range_out_of_use() {
        let found = windows(&tree(&[HOLE], &[], true)).expect("decodes");
        assert_eq!(
            found,
            [IommuAddress {
                iova: 0x1000,
                len: 0x1000,
                memory: None,
            }]
        );
        assert!(!found[0].is_identity());
    }

    #[test]
    fn a_region_the_master_does_not_list_binds_it_to_nothing() {
        let found = windows(&tree(&[], &[MASTER, 0x8000_0000, 0x80_0000], true)).expect("decodes");
        assert!(found.is_empty());
    }

    #[test]
    fn a_window_mapping_other_memory_is_no_identity() {
        let found =
            windows(&tree(&[SCANOUT], &[MASTER, 0x4000_0000, 0x80_0000], true)).expect("decodes");
        assert!(!found[0].is_identity());
    }

    #[test]
    fn a_window_that_cannot_be_read_is_an_error_not_an_omission() {
        for (regions, addresses) in [
            (&[SCANOUT][..], &[MASTER, 0x8000_0000][..]),
            (&[SCANOUT][..], &[0x99, 0x8000_0000, 0x80_0000][..]),
            (&[0x77][..], &[][..]),
        ] {
            assert_eq!(
                windows(&tree(regions, addresses, true)),
                Err(FdtError::BadProperty),
                "{regions:?} {addresses:?}"
            );
        }
    }

    #[test]
    fn a_unit_is_marked_by_its_specifier_width_and_one_cell_names_an_id() {
        let blob = tree(&[], &[], true);
        let fdt = Fdt::new(&blob).expect("valid fdt");
        assert_eq!(iommu_cells(&named(&fdt, b"smmu@3000")), Some(1));
        assert_eq!(iommu_cells(&named(&fdt, b"dma@2000")), None);
        let list = cells(&[1, 0x42, 2, 0x7, 0xF]);
        let ids: Vec<Option<u32>> = phandle_args(&list, |phandle| Some(((), phandle)))
            .map(|entry| stream_id(&entry.expect("frames").1))
            .collect();
        assert_eq!(ids, [Some(0x42), None]);
    }
}
