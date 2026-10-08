//! The Virtual I/O Translation Table (VIOT): the virtio-iommu units a
//! platform has, and the endpoints each translates — ranges of PCI functions,
//! and single platform devices by their register base.
//!
//! Pure byte-slice parsing. [`Viot::parse`] validates every node before
//! anything is read out, so a table that is wrong anywhere is refused whole
//! and never half-applied.
//!
//! Reference: ACPI Specification 6.5, *Virtual I/O Translation Table*; the
//! endpoint arithmetic is the one Linux's `drivers/acpi/viot.c` applies.

use tairix_abi::{HwDeviceClass, HwMatchKey, HwNode, HwResource, HW_NODE_ROOT_ID};
use tairix_arch_api::{DiscoveryError, HwNodeSink};

use crate::acpi::{read_u16, read_u32, read_u64, unit_node_id, AcpiError, SdtHeader, UnitNodes};

/// 4-byte ASCII signature of the Virtual I/O Translation Table.
pub const VIOT_SIGNATURE: [u8; 4] = *b"VIOT";

/// The SDT header, the node count and offset, and eight reserved bytes.
const HEADER_LEN: usize = 48;
const NODE_HEADER_LEN: usize = 4;

/// More nodes than any machine's topology names: it bounds the claim checks,
/// which compare nodes pairwise.
const MAX_NODES: u16 = 256;

const NODE_PCI_RANGE: u8 = 1;
const NODE_MMIO: u8 = 2;
const NODE_VIRTIO_PCI: u8 = 3;
const NODE_VIRTIO_MMIO: u8 = 4;

const ENDPOINT_NODE_LEN: usize = 24;
const UNIT_NODE_LEN: usize = 16;

/// Where an endpoint node names its unit, by the unit node's offset.
const OUTPUT_NODE_AT: usize = 16;

/// A validated VIOT.
#[derive(Copy, Clone, Debug)]
pub struct Viot<'a> {
    table: &'a [u8],
    first: usize,
    count: u16,
}

/// One virtio-iommu, as the table names it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ViotUnit {
    /// A PCI function.
    Pci {
        /// Its segment.
        segment: u16,
        /// Its requester id.
        requester: u16,
    },
    /// A virtio-mmio device.
    Mmio {
        /// Its register base.
        base: u64,
    },
}

/// The PCI functions `requesters` on each of `segments` one unit
/// translates, numbered as endpoints from `first_endpoint`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct PciRange {
    segments: (u16, u16),
    requesters: (u16, u16),
    first_endpoint: u32,
    /// The unit's node, by its offset in the table.
    unit: usize,
}

impl PciRange {
    fn decode(node: &[u8]) -> Self {
        Self {
            first_endpoint: read_u32(node, 4),
            segments: (read_u16(node, 8), read_u16(node, 10)),
            requesters: (read_u16(node, 12), read_u16(node, 14)),
            unit: usize::from(read_u16(node, OUTPUT_NODE_AT)),
        }
    }

    fn covers_segment(&self, segment: u16) -> bool {
        (self.segments.0..=self.segments.1).contains(&segment)
    }

    /// The endpoint id the function `requester` on `segment` masters DMA as.
    fn endpoint(&self, segment: u16, requester: u16) -> Option<u32> {
        if !self.covers_segment(segment)
            || !(self.requesters.0..=self.requesters.1).contains(&requester)
        {
            return None;
        }
        let offset =
            (u32::from(segment - self.segments.0) << 16) + u32::from(requester - self.requesters.0);
        self.first_endpoint.checked_add(offset)
    }

    /// The segment whose function masters DMA as endpoint `endpoint`, where
    /// one in the range does: the arithmetic of [`Self::endpoint`] undone,
    /// unique because a segment's requesters span at most 16 bits.
    fn segment_of(&self, endpoint: u32) -> Option<u16> {
        let offset = endpoint.checked_sub(self.first_endpoint)?;
        let segment = u16::try_from(offset >> 16).ok()?;
        let requester = offset & 0xFFFF;
        (segment <= self.segments.1 - self.segments.0
            && requester <= u32::from(self.requesters.1 - self.requesters.0))
        .then(|| self.segments.0 + segment)
    }

    fn overlaps(&self, other: &Self) -> bool {
        let meet = |(a, b): (u16, u16), (c, d): (u16, u16)| a <= d && c <= b;
        meet(self.segments, other.segments) && meet(self.requesters, other.requesters)
    }
}

/// A platform device one unit translates, as a single endpoint.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct MmioEndpoint {
    base: u64,
    endpoint: u32,
    unit: usize,
}

impl MmioEndpoint {
    fn decode(node: &[u8]) -> Self {
        Self {
            endpoint: read_u32(node, 4),
            base: read_u64(node, 8),
            unit: usize::from(read_u16(node, OUTPUT_NODE_AT)),
        }
    }
}

impl<'a> Viot<'a> {
    /// Validate the VIOT in `bytes`: its header and checksum, and every node
    /// it counts.
    ///
    /// # Errors
    ///
    /// [`AcpiError`] for any structural defect: a bad signature, checksum or
    /// length; a node that does not fit where it claims to; an endpoint whose
    /// unit is no unit node, whose range runs backwards, or whose endpoint
    /// ids overflow; two ranges naming one function, or two endpoints one
    /// device; or two units naming one function or register base.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, AcpiError> {
        let header = SdtHeader::validate(bytes, &VIOT_SIGNATURE)?;
        let len = header.length as usize;
        if len < HEADER_LEN {
            return Err(AcpiError::BadLength);
        }
        let first = usize::from(read_u16(bytes, 38));
        if first < HEADER_LEN || first > len {
            return Err(AcpiError::BadLength);
        }
        let count = read_u16(bytes, 36);
        if count > MAX_NODES {
            return Err(AcpiError::BadLength);
        }
        let viot = Self {
            table: &bytes[..len],
            first,
            count,
        };
        viot.validate()?;
        Ok(viot)
    }

    fn validate(&self) -> Result<(), AcpiError> {
        // An endpoint names its unit by offset, so every node is proved whole
        // before any is read through another.
        let mut at = self.first;
        for _ in 0..self.count {
            let node = split_node(self.table, at)?;
            let fixed = match node[0] {
                NODE_PCI_RANGE | NODE_MMIO => ENDPOINT_NODE_LEN,
                NODE_VIRTIO_PCI | NODE_VIRTIO_MMIO => UNIT_NODE_LEN,
                // A node a later revision defines is skipped by its own
                // length, which `split_node` has bounded.
                _ => NODE_HEADER_LEN,
            };
            if node.len() < fixed {
                return Err(AcpiError::BadLength);
            }
            at += node.len();
        }
        for (_, node) in self.nodes() {
            match node[0] {
                NODE_PCI_RANGE => {
                    let range = PciRange::decode(node);
                    let span = range
                        .segments
                        .1
                        .checked_sub(range.segments.0)
                        .zip(range.requesters.1.checked_sub(range.requesters.0))
                        .and_then(|(segments, requesters)| {
                            range
                                .first_endpoint
                                .checked_add((u32::from(segments) << 16) + u32::from(requesters))
                        });
                    if span.is_none() || !self.is_unit_node(range.unit) {
                        return Err(AcpiError::BadLength);
                    }
                }
                NODE_MMIO if !self.is_unit_node(MmioEndpoint::decode(node).unit) => {
                    return Err(AcpiError::BadLength);
                }
                _ => {}
            }
        }
        self.validate_claims()
    }

    /// A function two ranges name, or a device two endpoints name, would
    /// master DMA as whichever one a reader took first; and two units
    /// claiming one function or window would each believe it alone drives
    /// it.
    fn validate_claims(&self) -> Result<(), AcpiError> {
        for (index, range) in self.pci_ranges().enumerate() {
            if self
                .pci_ranges()
                .skip(index + 1)
                .any(|other| range.overlaps(&other))
            {
                return Err(AcpiError::BadLength);
            }
        }
        for (index, endpoint) in self.mmio_endpoints().enumerate() {
            if self
                .mmio_endpoints()
                .skip(index + 1)
                .any(|other| other.base == endpoint.base)
            {
                return Err(AcpiError::BadLength);
            }
        }
        for (index, unit) in self.units().enumerate() {
            if self.units().skip(index + 1).any(|other| other == unit) {
                return Err(AcpiError::BadLength);
            }
        }
        Ok(())
    }

    /// Whether `offset` is where a counted unit node starts.
    fn is_unit_node(&self, offset: usize) -> bool {
        self.unit_nodes().any(|(at, _)| at == offset)
    }

    fn nodes(&self) -> Nodes<'a> {
        Nodes {
            table: self.table,
            at: self.first,
            left: self.count,
        }
    }

    /// Every unit node, by its offset.
    fn unit_nodes(&self) -> impl Iterator<Item = (usize, ViotUnit)> + 'a {
        self.nodes().filter_map(|(at, node)| {
            let unit = match node[0] {
                NODE_VIRTIO_PCI => ViotUnit::Pci {
                    segment: read_u16(node, 4),
                    requester: read_u16(node, 6),
                },
                NODE_VIRTIO_MMIO => ViotUnit::Mmio {
                    base: read_u64(node, 8),
                },
                _ => return None,
            };
            Some((at, unit))
        })
    }

    /// Every unit, in table order.
    pub fn units(&self) -> impl Iterator<Item = ViotUnit> + 'a {
        self.unit_nodes().map(|(_, unit)| unit)
    }

    fn pci_ranges(&self) -> impl Iterator<Item = PciRange> + 'a {
        self.nodes()
            .filter(|(_, node)| node[0] == NODE_PCI_RANGE)
            .map(|(_, node)| PciRange::decode(node))
    }

    fn mmio_endpoints(&self) -> impl Iterator<Item = MmioEndpoint> + 'a {
        self.nodes()
            .filter(|(_, node)| node[0] == NODE_MMIO)
            .map(|(_, node)| MmioEndpoint::decode(node))
    }

    /// Whether a unit translates any function on `segment`.
    #[must_use]
    pub fn covers(&self, segment: u16) -> bool {
        self.pci_ranges().any(|range| range.covers_segment(segment))
    }

    /// The index among [`Self::units`] of the unit whose node is at
    /// `offset`: units are counted in table order, so their indices follow
    /// their offsets.
    fn unit_index(&self, offset: usize) -> usize {
        self.unit_nodes().take_while(|&(at, _)| at < offset).count()
    }

    /// Whether a unit translating functions on `segment` was left without a
    /// node, so nothing on the segment can be confined.
    #[must_use]
    pub fn strands(&self, nodes: UnitNodes, segment: u16) -> bool {
        let Some((unplaced, _)) = self.unit_nodes().nth(nodes.emitted) else {
            return false;
        };
        self.pci_ranges()
            .any(|range| range.covers_segment(segment) && range.unit >= unplaced)
    }
}

/// Walks nodes a successful [`Viot::parse`] already proved well formed; it
/// stops rather than panics on anything it could not split.
#[derive(Clone)]
struct Nodes<'a> {
    table: &'a [u8],
    at: usize,
    left: u16,
}

impl<'a> Iterator for Nodes<'a> {
    type Item = (usize, &'a [u8]);

    fn next(&mut self) -> Option<Self::Item> {
        self.left = self.left.checked_sub(1)?;
        let node = split_node(self.table, self.at).ok()?;
        let at = self.at;
        self.at += node.len();
        Some((at, node))
    }
}

/// The node at `at` in `table`, bounded by its own length.
fn split_node(table: &[u8], at: usize) -> Result<&[u8], AcpiError> {
    let rest = table.get(at..).ok_or(AcpiError::Truncated)?;
    if rest.len() < NODE_HEADER_LEN {
        return Err(AcpiError::Truncated);
    }
    let len = usize::from(read_u16(rest, 2));
    if len < NODE_HEADER_LEN || len > rest.len() {
        return Err(AcpiError::BadLength);
    }
    Ok(&rest[..len])
}

/// Emit one [`HwDeviceClass::Iommu`] node per unit, numbered from `first_id`
/// in table order: a unit that is a PCI function keyed `pci` with its
/// function as its address, one in a virtio-mmio slot keyed `mmio` with
/// `window` bytes of registers from its base.
///
/// # Errors
///
/// [`DiscoveryError::MalformedSource`] when a unit's node cannot be built;
/// a window that would wrap or an id past the last is found before any node
/// is emitted.
pub fn emit_unit_nodes(
    viot: &Viot<'_>,
    first_id: u32,
    pci: &[u8],
    mmio: &[u8],
    window: u64,
    sink: &mut dyn HwNodeSink,
) -> Result<UnitNodes, DiscoveryError> {
    let malformed = |_| DiscoveryError::MalformedSource;
    let pci = HwMatchKey::compatible(pci).map_err(malformed)?;
    let mmio = HwMatchKey::compatible(mmio).map_err(malformed)?;
    let wraps =
        |unit| matches!(unit, ViotUnit::Mmio { base } if base.checked_add(window).is_none());
    let units = viot.units().count();
    if viot.units().any(wraps)
        || units
            .checked_sub(1)
            .is_some_and(|last| unit_node_id(first_id, last).is_none())
    {
        return Err(DiscoveryError::MalformedSource);
    }
    let mut placed = UnitNodes::default();
    for (index, unit) in viot.units().enumerate() {
        let id = unit_node_id(first_id, index).ok_or(DiscoveryError::MalformedSource)?;
        let mut node = HwNode::new(id, HW_NODE_ROOT_ID, HwDeviceClass::Iommu);
        node.push_resource(crate::acpi::unit_dma())
            .map_err(malformed)?;
        match unit {
            ViotUnit::Pci { segment, requester } => {
                node.set_address((u32::from(segment) << 16) | u32::from(requester));
                node.push_match_key(pci).map_err(malformed)?;
            }
            ViotUnit::Mmio { base } => {
                node.push_match_key(mmio)
                    .and_then(|()| node.push_resource(HwResource::mmio(base, window)))
                    .map_err(malformed)?;
            }
        }
        if sink.emit(node).is_err() {
            break;
        }
        placed.emitted += 1;
    }
    Ok(placed)
}

/// The node of the unit [`emit_unit_nodes`] numbered from `first_id` that
/// translates the function `requester` on `segment`, and the endpoint id it
/// masters DMA as; [`None`] where no unit with a node translates it, the
/// unit's own function among them.
#[must_use]
pub fn endpoint(
    viot: &Viot<'_>,
    first_id: u32,
    nodes: UnitNodes,
    segment: u16,
    requester: u16,
) -> Option<(u32, u32)> {
    let (range, endpoint) = viot
        .pci_ranges()
        .find_map(|range| Some((range, range.endpoint(segment, requester)?)))?;
    let own = viot
        .unit_nodes()
        .any(|(at, unit)| at == range.unit && unit == ViotUnit::Pci { segment, requester });
    let index = viot.unit_index(range.unit);
    if own || index >= nodes.emitted {
        return None;
    }
    Some((unit_node_id(first_id, index)?, endpoint))
}

/// Whether endpoint `stream` on the unit at node `unit`, numbered from
/// `first_id`, is mastered by a device besides the one function on `segment`
/// a range numbers so: a function another segment's or a second range numbers
/// so, or a platform device.
#[must_use]
pub fn contested(viot: &Viot<'_>, first_id: u32, segment: u16, unit: u32, stream: u32) -> bool {
    let Some(index) = unit
        .checked_sub(first_id)
        .and_then(|index| usize::try_from(index).ok())
    else {
        return false;
    };
    let Some((offset, _)) = viot.unit_nodes().nth(index) else {
        return false;
    };
    let mut numbering = viot
        .pci_ranges()
        .filter(|range| range.unit == offset)
        .filter_map(|range| range.segment_of(stream));
    let first = numbering.next();
    // A range numbers each of its ids once, so a second range numbering this
    // one names a second master.
    viot.mmio_endpoints()
        .any(|device| device.unit == offset && device.endpoint == stream)
        || first.is_some_and(|other| other != segment)
        || numbering.next().is_some()
}

#[cfg(test)]
#[path = "viot_tests.rs"]
mod tests;
