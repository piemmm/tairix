//! Id maps: how a bus's requester ids reach a target. `iommu-map` and
//! `iommu-map-mask` name translation units, `msi-map` and `msi-map-mask` MSI
//! controllers (Linux `Documentation/devicetree/bindings/pci/pci-iommu.txt`
//! and `pci-msi.txt`).

use core::ops::RangeInclusive;

use crate::{phandle_ref, FdtError, Node};

/// Bytes in one `(id-base, target, target-base, length)` entry.
const ENTRY_BYTES: usize = 16;

/// One entry: ids `[id_base, id_base + len)` reach `target` as
/// `[target_base, target_base + len)`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct IdMapEntry {
    /// The first id the entry maps.
    pub id_base: u32,
    /// The target's phandle.
    pub target: u32,
    /// The id `id_base` reaches the target as.
    pub target_base: u32,
    /// How many ids it maps; an entry mapping none is allowed and matches
    /// nothing.
    pub len: u32,
}

impl IdMapEntry {
    /// The id `id` reaches the target as, where the entry maps it.
    #[must_use]
    pub fn map(&self, id: u32) -> Option<u32> {
        let offset = id.checked_sub(self.id_base)?;
        if offset < self.len {
            self.target_base.checked_add(offset)
        } else {
            None
        }
    }

    /// The target ids the entry reaches; [`None`] for one mapping nothing.
    #[must_use]
    pub fn targets(&self) -> Option<RangeInclusive<u32>> {
        let last = self.target_base.checked_add(self.len.checked_sub(1)?)?;
        Some(self.target_base..=last)
    }

    fn decode(raw: &[u8; ENTRY_BYTES]) -> Self {
        let cell = |at: usize| u32::from_be_bytes([raw[at], raw[at + 1], raw[at + 2], raw[at + 3]]);
        Self {
            id_base: cell(0),
            target: cell(4),
            target_base: cell(8),
            len: cell(12),
        }
    }

    /// Whether the binding admits the entry under `mask`: it names a node,
    /// the mask keeps its base (an id it lists could otherwise never be
    /// looked up), and neither its ids nor its targets run past the last id.
    fn admitted(&self, mask: u32) -> bool {
        let Some(span) = self.len.checked_sub(1) else {
            return phandle_ref(self.target).is_some() && self.id_base & !mask == 0;
        };
        phandle_ref(self.target).is_some()
            && self.id_base & !mask == 0
            && self.id_base.checked_add(span).is_some()
            && self.target_base.checked_add(span).is_some()
    }
}

/// A validated id map.
#[derive(Copy, Clone, Debug)]
pub struct IdMap<'a> {
    entries: &'a [u8],
    mask: u32,
}

impl<'a> IdMap<'a> {
    /// `node`'s map in property `map`, its ids masked by property `mask`
    /// (every bit kept where it has none); `Ok(None)` when `node` has no map.
    ///
    /// # Errors
    ///
    /// [`FdtError::BadProperty`] for a map that is empty or not whole
    /// entries, a mask that is not one cell, or an entry the binding does not
    /// admit: one naming no node, one whose base the mask changes, or one
    /// whose ids or targets run past the last id. Such a map is refused
    /// whole, never half-applied.
    pub fn of(node: &Node<'a>, map: &str, mask: &str) -> Result<Option<Self>, FdtError> {
        let Some(map) = node.property(map) else {
            return Ok(None);
        };
        let mask = match node.property(mask) {
            None => u32::MAX,
            Some(mask) if mask.value().len() == 4 => {
                mask.read_be_u32(0).map_err(|_| FdtError::BadProperty)?
            }
            Some(_) => return Err(FdtError::BadProperty),
        };
        let entries = map.value();
        if entries.is_empty() || entries.len() % ENTRY_BYTES != 0 {
            return Err(FdtError::BadProperty);
        }
        let parsed = Self { entries, mask };
        if parsed.entries().all(|entry| entry.admitted(mask)) {
            Ok(Some(parsed))
        } else {
            Err(FdtError::BadProperty)
        }
    }

    /// The mask an id is reduced by before it is looked up.
    #[must_use]
    pub fn mask(&self) -> u32 {
        self.mask
    }

    /// Every entry, in the map's order.
    pub fn entries(&self) -> impl Iterator<Item = IdMapEntry> + '_ {
        self.entries
            .as_chunks::<ENTRY_BYTES>()
            .0
            .iter()
            .map(IdMapEntry::decode)
    }

    /// The target and the id there that `id` reaches, through the first
    /// entry mapping it once masked; [`None`] where no entry does, which the
    /// binding leaves untranslated.
    #[must_use]
    pub fn map(&self, id: u32) -> Option<(u32, u32)> {
        let masked = id & self.mask;
        self.entries()
            .find_map(|entry| Some((entry.target, entry.map(masked)?)))
    }
}

#[cfg(test)]
mod tests {
    use super::{IdMap, IdMapEntry};
    use crate::fixture::DtbBuilder;
    use crate::{Fdt, FdtError};
    use alloc::vec::Vec;

    const UNIT: u32 = 0x8004;

    fn cells(values: &[u32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_be_bytes()).collect()
    }

    fn host_with(map: Option<&[u32]>, mask: Option<&[u8]>) -> Vec<u8> {
        let mut b = DtbBuilder::new();
        b.begin_node("");
        b.begin_node("pcie@10000000");
        if let Some(map) = map {
            b.prop("iommu-map", &cells(map));
        }
        if let Some(mask) = mask {
            b.prop("iommu-map-mask", mask);
        }
        b.end_node();
        b.end_node();
        b.build()
    }

    /// A parsed map's entries, its mask, and what each of a fixed set of
    /// probe ids reaches.
    type Parsed = (Vec<IdMapEntry>, u32, Vec<Option<(u32, u32)>>);

    fn parse(blob: &[u8]) -> Result<Option<Parsed>, FdtError> {
        let fdt = Fdt::new(blob).expect("valid fdt");
        let host = fdt
            .nodes()
            .filter_map(Result::ok)
            .find(|node| node.depth() == 1)
            .expect("host");
        let map = IdMap::of(&host, "iommu-map", "iommu-map-mask")?;
        Ok(map.map(|map| {
            let probes = [0, 0x8, 0xF, 0x10, 0x11, 0xFFFE, 0xFFFF, 0x1_0000]
                .iter()
                .map(|&id| map.map(id))
                .collect();
            (map.entries().collect(), map.mask(), probes)
        }))
    }

    #[test]
    fn a_whole_bus_map_reaches_one_unit_at_the_requester_id() {
        // QEMU `virt`'s SMMUv3 map.
        let (entries, mask, probes) = parse(&host_with(Some(&[0, UNIT, 0, 0x1_0000]), None))
            .expect("admitted")
            .expect("present");
        assert_eq!((entries.len(), mask), (1, u32::MAX));
        assert_eq!(probes[1], Some((UNIT, 0x8)));
        assert_eq!(probes[6], Some((UNIT, 0xFFFF)));
        assert_eq!(probes[7], None);
        assert_eq!(entries[0].targets(), Some(0..=0xFFFF));
    }

    #[test]
    fn an_id_the_map_leaves_out_is_untranslated() {
        // QEMU's virtio-iommu at 00:02.0 keeps its own requester id unmapped.
        let (_, _, probes) = parse(&host_with(
            Some(&[0, UNIT, 0, 0x10, 0x11, UNIT, 0x11, 0xFFEF]),
            None,
        ))
        .expect("admitted")
        .expect("present");
        assert_eq!(probes[2], Some((UNIT, 0xF)));
        assert_eq!(probes[3], None);
        assert_eq!(probes[4], Some((UNIT, 0x11)));
    }

    #[test]
    fn an_entry_mapping_nothing_is_admitted_and_matches_nothing() {
        // QEMU `virt`'s RISC-V IOMMU map opens with one.
        let (entries, _, probes) =
            parse(&host_with(Some(&[0, UNIT, 0, 0, 0, UNIT, 0, 0xFFFF]), None))
                .expect("admitted")
                .expect("present");
        assert_eq!(entries[0].targets(), None);
        assert_eq!(probes[0], Some((UNIT, 0)));
        assert_eq!(probes[5], Some((UNIT, 0xFFFE)));
        assert_eq!(probes[6], None);
    }

    #[test]
    fn the_mask_folds_ids_before_they_are_looked_up() {
        let (_, mask, probes) = parse(&host_with(
            Some(&[0, UNIT, 0x100, 0x1_0000]),
            Some(&0xFFF8u32.to_be_bytes()),
        ))
        .expect("admitted")
        .expect("present");
        assert_eq!(mask, 0xFFF8);
        assert_eq!(probes[1], Some((UNIT, 0x108)));
        assert_eq!(probes[2], Some((UNIT, 0x108)));
        assert_eq!(probes[3], Some((UNIT, 0x110)));
    }

    #[test]
    fn a_host_with_no_map_has_none() {
        assert!(parse(&host_with(None, None))
            .expect("no map is no error")
            .is_none());
    }

    #[test]
    fn a_map_the_binding_does_not_admit_is_refused_whole() {
        for (map, mask) in [
            (Some(&[][..]), None),
            (Some(&[0, UNIT, 0][..]), None),
            (Some(&[0, 0, 0, 1][..]), None),
            (Some(&[0, u32::MAX, 0, 1][..]), None),
            (
                Some(&[0x9, UNIT, 0, 1][..]),
                Some(&0xFFF8u32.to_be_bytes()[..]),
            ),
            (Some(&[0xFFFF_FFFF, UNIT, 0, 2][..]), None),
            (Some(&[0, UNIT, 0xFFFF_FFFF, 2][..]), None),
            (Some(&[0, UNIT, 0, 1, 1, 0, 0, 1][..]), None),
            (Some(&[0, UNIT, 0, 1][..]), Some(&[0u8, 0][..])),
        ] {
            assert_eq!(
                parse(&host_with(map, mask)).map(|_| ()),
                Err(FdtError::BadProperty),
                "{map:?} masked by {mask:?}"
            );
        }
    }
}
