//! The DMA Remapping Reporting table (DMAR): the VT-d translation units a
//! platform has, the PCI functions each covers, and the memory firmware keeps
//! mastering through them.
//!
//! Pure byte-slice parsing. [`Dmar::parse`] validates every remapping
//! structure and every device scope before anything is read out, so a table
//! that is wrong anywhere is refused whole and never half-applied.
//!
//! Reference: Intel Virtualization Technology for Directed I/O, Architecture
//! Specification, rev. 4.1, chapter 8.

use tairix_abi::{
    HwDeviceClass, HwMatchKey, HwNode, HwResource, IommuReservedWindow, HW_NODE_ROOT_ID,
};
use tairix_arch_api::{DiscoveryError, HwNodeSink};

use crate::acpi::{AcpiError, SdtHeader};

/// 4-byte ASCII signature of the DMA Remapping Reporting table.
pub const DMAR_SIGNATURE: [u8; 4] = *b"DMAR";

/// Offset of the first remapping structure: the SDT header, the host address
/// width, the flags and ten reserved bytes.
const REMAPPING_OFFSET: usize = 48;

const STRUCTURE_HEADER_LEN: usize = 4;
const SCOPE_HEADER_LEN: usize = 6;
const PAGE: u64 = 0x1000;

const TYPE_DRHD: u16 = 0;
const TYPE_RMRR: u16 = 1;
const TYPE_ATSR: u16 = 2;
const TYPE_RHSA: u16 = 3;
const TYPE_ANDD: u16 = 4;
const TYPE_SATC: u16 = 5;
const TYPE_SIDP: u16 = 6;

const DRHD_SCOPES: usize = 16;
const RMRR_SCOPES: usize = 24;
const SEGMENT_SCOPES: usize = 8;
const RHSA_LEN: usize = 20;
const ANDD_MIN_LEN: usize = 8;

const DRHD_INCLUDE_PCI_ALL: u8 = 1 << 0;
const DRHD_SIZE_MASK: u8 = 0x0F;

/// A PCI requester id: the bus, device and function a function's requests
/// carry, which is the id a VT-d unit knows it by.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct SourceId(u16);

impl SourceId {
    /// The requester id of `device`.`function` on `bus`, or [`None`] for a
    /// device or function number past the configuration-space limits.
    #[must_use]
    pub fn new(bus: u8, device: u8, function: u8) -> Option<Self> {
        tairix_abi::driver::pci::function_address(bus, device, function).map(Self::at)
    }

    /// The requester id of the function at PCI configuration `address`.
    #[must_use]
    pub fn at(address: u64) -> Self {
        Self(tairix_abi::driver::pci::requester_id(address))
    }

    /// The id as the unit reads it.
    #[must_use]
    pub const fn raw(self) -> u16 {
        self.0
    }

    /// The bus the function sits on.
    #[must_use]
    pub const fn bus(self) -> u8 {
        (self.0 >> 8) as u8
    }
}

/// The one configuration-space fact a scope's path needs: which buses a
/// bridge forwards to.
pub trait BridgeBuses {
    /// The secondary and subordinate bus numbers of the bridge at `bridge`,
    /// or [`None`] when no bridge answers there.
    fn bus_range(&self, bridge: SourceId) -> Option<(u8, u8)>;
}

/// Platform-wide DMAR flags.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct DmarFlags(u8);

impl DmarFlags {
    /// The platform supports interrupt remapping.
    #[must_use]
    pub const fn interrupt_remapping(self) -> bool {
        self.0 & (1 << 0) != 0
    }

    /// Firmware asks the OS not to enable x2APIC.
    #[must_use]
    pub const fn x2apic_opt_out(self) -> bool {
        self.0 & (1 << 1) != 0
    }

    /// Firmware protected pre-boot DMA and expects the OS to keep doing so.
    #[must_use]
    pub const fn dma_control_opt_in(self) -> bool {
        self.0 & (1 << 2) != 0
    }
}

/// A validated DMAR.
#[derive(Copy, Clone, Debug)]
pub struct Dmar<'a> {
    host_address_bits: u8,
    flags: DmarFlags,
    structures: &'a [u8],
}

impl<'a> Dmar<'a> {
    /// Validate the DMAR in `bytes`: its header and checksum, its host address
    /// width, and every remapping structure and device scope it carries.
    ///
    /// # Errors
    ///
    /// [`AcpiError`] for any structural defect: a bad signature, checksum or
    /// length, a structure or scope that does not fit where it claims to, a
    /// unit register window that is not page-aligned, or a reserved window
    /// that is not whole pages.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, AcpiError> {
        let header = SdtHeader::validate(bytes, &DMAR_SIGNATURE)?;
        let len = header.length as usize;
        if len < REMAPPING_OFFSET {
            return Err(AcpiError::BadLength);
        }
        let host_address_bits = bytes[36].checked_add(1).ok_or(AcpiError::BadLength)?;
        if host_address_bits > 64 {
            return Err(AcpiError::BadLength);
        }
        let dmar = Self {
            host_address_bits,
            flags: DmarFlags(bytes[37]),
            structures: &bytes[REMAPPING_OFFSET..len],
        };
        dmar.validate()?;
        Ok(dmar)
    }

    /// The platform's DMA physical address width, in bits.
    #[must_use]
    pub const fn host_address_bits(&self) -> u8 {
        self.host_address_bits
    }

    /// The platform-wide flags.
    #[must_use]
    pub const fn flags(&self) -> DmarFlags {
        self.flags
    }

    /// Every translation unit, in table order.
    pub fn units(&self) -> impl Iterator<Item = Drhd<'a>> + 'a {
        self.remapping()
            .filter(|s| s.kind == TYPE_DRHD)
            .map(|s| Drhd::decode(s.body))
    }

    /// Every firmware reserved window, in table order.
    pub fn reserved_regions(&self) -> impl Iterator<Item = Rmrr<'a>> + 'a {
        self.remapping()
            .filter(|s| s.kind == TYPE_RMRR)
            .map(|s| Rmrr::decode(s.body))
    }

    /// The index among [`Self::units`] of the unit that translates the PCI
    /// function `source` on `segment`, or [`None`] when no unit covers it.
    ///
    /// A unit whose scopes name the function, or a bridge above it, claims it
    /// first; the segment's `INCLUDE_PCI_ALL` unit takes everything else.
    #[must_use]
    pub fn unit_for(
        &self,
        segment: u16,
        source: SourceId,
        bridges: &dyn BridgeBuses,
    ) -> Option<usize> {
        let mut catch_all = None;
        for (index, unit) in self.units().enumerate() {
            if unit.segment() != segment {
                continue;
            }
            if unit.include_pci_all() {
                catch_all = catch_all.or(Some(index));
                continue;
            }
            if unit.scopes().any(|scope| scope.covers(source, bridges)) {
                return Some(index);
            }
        }
        catch_all
    }

    fn remapping(&self) -> Structures<'a> {
        Structures {
            rest: self.structures,
        }
    }

    fn validate(&self) -> Result<(), AcpiError> {
        let mut rest = self.structures;
        while !rest.is_empty() {
            let (structure, tail) = split_structure(rest)?;
            rest = tail;
            match structure.kind {
                TYPE_DRHD => {
                    let body = structure.body;
                    if body.len() < DRHD_SCOPES {
                        return Err(AcpiError::BadLength);
                    }
                    let unit = Drhd::decode(body);
                    if unit.register_base == 0 || !unit.register_base.is_multiple_of(PAGE) {
                        return Err(AcpiError::BadLength);
                    }
                    validate_scopes(&body[DRHD_SCOPES..])?;
                }
                TYPE_RMRR => {
                    let body = structure.body;
                    if body.len() < RMRR_SCOPES {
                        return Err(AcpiError::BadLength);
                    }
                    let region = Rmrr::decode(body);
                    let end = region.limit.checked_add(1).ok_or(AcpiError::BadLength)?;
                    if !region.base.is_multiple_of(PAGE)
                        || !end.is_multiple_of(PAGE)
                        || end <= region.base
                    {
                        return Err(AcpiError::BadLength);
                    }
                    validate_scopes(&body[RMRR_SCOPES..])?;
                }
                TYPE_ATSR | TYPE_SATC | TYPE_SIDP => {
                    if structure.body.len() < SEGMENT_SCOPES {
                        return Err(AcpiError::BadLength);
                    }
                    validate_scopes(&structure.body[SEGMENT_SCOPES..])?;
                }
                TYPE_RHSA if structure.body.len() < RHSA_LEN => return Err(AcpiError::BadLength),
                TYPE_ANDD if structure.body.len() < ANDD_MIN_LEN => {
                    return Err(AcpiError::BadLength)
                }
                // A structure a later revision defines is skipped by its own
                // length, which `split_structure` has bounded.
                _ => {}
            }
        }
        self.validate_units()
    }

    /// Two units claiming one register window, or one segment's every
    /// function, would each believe it alone confines what the other does.
    fn validate_units(&self) -> Result<(), AcpiError> {
        let window = |unit: &Drhd<'_>| {
            let base = unit.register_base();
            base.checked_add(unit.register_len())
                .map(|end| base..end)
                .ok_or(AcpiError::BadLength)
        };
        for (index, unit) in self.units().enumerate() {
            let registers = window(&unit)?;
            for other in self.units().skip(index + 1) {
                let theirs = window(&other)?;
                let shared_catch_all = unit.include_pci_all()
                    && other.include_pci_all()
                    && unit.segment() == other.segment();
                if registers.start < theirs.end && theirs.start < registers.end || shared_catch_all
                {
                    return Err(AcpiError::BadLength);
                }
            }
        }
        Ok(())
    }
}

/// One remapping structure: its type and its whole record, header included.
#[derive(Copy, Clone)]
struct Structure<'a> {
    kind: u16,
    body: &'a [u8],
}

fn split_structure(bytes: &[u8]) -> Result<(Structure<'_>, &[u8]), AcpiError> {
    if bytes.len() < STRUCTURE_HEADER_LEN {
        return Err(AcpiError::Truncated);
    }
    let kind = u16::from_le_bytes([bytes[0], bytes[1]]);
    let len = usize::from(u16::from_le_bytes([bytes[2], bytes[3]]));
    if len < STRUCTURE_HEADER_LEN || len > bytes.len() {
        return Err(AcpiError::BadLength);
    }
    let (body, rest) = bytes.split_at(len);
    Ok((Structure { kind, body }, rest))
}

/// Walks remapping structures a successful [`Dmar::parse`] already proved
/// well formed; it stops rather than panics on anything it could not split.
struct Structures<'a> {
    rest: &'a [u8],
}

impl<'a> Iterator for Structures<'a> {
    type Item = Structure<'a>;

    fn next(&mut self) -> Option<Structure<'a>> {
        let (structure, rest) = split_structure(self.rest).ok()?;
        self.rest = rest;
        Some(structure)
    }
}

fn validate_scopes(mut bytes: &[u8]) -> Result<(), AcpiError> {
    while !bytes.is_empty() {
        let (_, rest) = split_scope(bytes)?;
        bytes = rest;
    }
    Ok(())
}

fn split_scope(bytes: &[u8]) -> Result<(DeviceScope<'_>, &[u8]), AcpiError> {
    if bytes.len() < SCOPE_HEADER_LEN {
        return Err(AcpiError::Truncated);
    }
    let len = usize::from(bytes[1]);
    if len < SCOPE_HEADER_LEN || len > bytes.len() || !(len - SCOPE_HEADER_LEN).is_multiple_of(2) {
        return Err(AcpiError::BadLength);
    }
    let scope = DeviceScope {
        kind: ScopeKind::from_raw(bytes[0]),
        enumeration_id: bytes[4],
        start_bus: bytes[5],
        path: &bytes[SCOPE_HEADER_LEN..len],
    };
    Ok((scope, &bytes[len..]))
}

/// A DMA remapping hardware unit definition: one VT-d unit.
#[derive(Copy, Clone, Debug)]
pub struct Drhd<'a> {
    flags: u8,
    size: u8,
    segment: u16,
    register_base: u64,
    scopes: &'a [u8],
}

impl<'a> Drhd<'a> {
    fn decode(body: &'a [u8]) -> Self {
        Self {
            flags: body[4],
            size: body[5],
            segment: u16::from_le_bytes([body[6], body[7]]),
            register_base: read_u64(body, 8),
            scopes: &body[DRHD_SCOPES..],
        }
    }

    /// Whether the unit covers every function of its segment no other unit
    /// claims.
    #[must_use]
    pub const fn include_pci_all(&self) -> bool {
        self.flags & DRHD_INCLUDE_PCI_ALL != 0
    }

    /// The PCI segment the unit's functions sit on.
    #[must_use]
    pub const fn segment(&self) -> u16 {
        self.segment
    }

    /// The physical base of the unit's register set.
    #[must_use]
    pub const fn register_base(&self) -> u64 {
        self.register_base
    }

    /// The length of the unit's register set: `2^n` pages, where firmware
    /// predating the field reports `n = 0`.
    #[must_use]
    pub const fn register_len(&self) -> u64 {
        PAGE << (self.size & DRHD_SIZE_MASK)
    }

    /// The functions, bridges and interrupt sources the unit's scopes name.
    #[must_use]
    pub fn scopes(&self) -> Scopes<'a> {
        Scopes { rest: self.scopes }
    }
}

/// A reserved memory region report: memory firmware keeps mastering through
/// the functions its scopes name, so each keeps an identity mapping of it.
#[derive(Copy, Clone, Debug)]
pub struct Rmrr<'a> {
    segment: u16,
    base: u64,
    limit: u64,
    scopes: &'a [u8],
}

impl<'a> Rmrr<'a> {
    fn decode(body: &'a [u8]) -> Self {
        Self {
            segment: u16::from_le_bytes([body[6], body[7]]),
            base: read_u64(body, 8),
            limit: read_u64(body, 16),
            scopes: &body[RMRR_SCOPES..],
        }
    }

    /// The PCI segment of the functions the region is kept for.
    #[must_use]
    pub const fn segment(&self) -> u16 {
        self.segment
    }

    /// The region's first byte.
    #[must_use]
    pub const fn base(&self) -> u64 {
        self.base
    }

    /// The region's length in bytes: whole pages, since [`Dmar::parse`] proved
    /// both ends page-aligned.
    #[must_use]
    pub const fn len(&self) -> u64 {
        self.limit - self.base + 1
    }

    /// Always `false`: a validated region spans at least one page.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        false
    }

    /// The functions the region is kept for.
    #[must_use]
    pub fn scopes(&self) -> Scopes<'a> {
        Scopes { rest: self.scopes }
    }
}

/// Iterates the device scopes of a structure [`Dmar::parse`] validated.
#[derive(Clone, Debug)]
pub struct Scopes<'a> {
    rest: &'a [u8],
}

impl<'a> Iterator for Scopes<'a> {
    type Item = DeviceScope<'a>;

    fn next(&mut self) -> Option<DeviceScope<'a>> {
        let (scope, rest) = split_scope(self.rest).ok()?;
        self.rest = rest;
        Some(scope)
    }
}

/// What a device scope names.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ScopeKind {
    /// A PCI endpoint function.
    Endpoint,
    /// A PCI-PCI bridge and every function below it.
    Bridge,
    /// An I/O APIC, by its APIC id.
    IoApic,
    /// An MSI-capable HPET, by its block id.
    Hpet,
    /// An ACPI namespace device, by its ANDD number.
    Namespace,
    /// A type a later revision defines.
    Other(u8),
}

impl ScopeKind {
    const fn from_raw(raw: u8) -> Self {
        match raw {
            1 => Self::Endpoint,
            2 => Self::Bridge,
            3 => Self::IoApic,
            4 => Self::Hpet,
            5 => Self::Namespace,
            other => Self::Other(other),
        }
    }
}

/// One device scope: what it names, and the path to it from its start bus.
#[derive(Copy, Clone, Debug)]
pub struct DeviceScope<'a> {
    kind: ScopeKind,
    enumeration_id: u8,
    start_bus: u8,
    path: &'a [u8],
}

impl DeviceScope<'_> {
    /// What the scope names.
    #[must_use]
    pub const fn kind(&self) -> ScopeKind {
        self.kind
    }

    /// The APIC, HPET or ANDD id an interrupt-source or namespace scope names.
    #[must_use]
    pub const fn enumeration_id(&self) -> u8 {
        self.enumeration_id
    }

    /// The function the path ends at: each hop but the last is a bridge whose
    /// secondary bus the next hop is on. [`None`] for an empty path, a hop
    /// past the configuration-space limits, or a bridge that does not answer.
    #[must_use]
    pub fn resolve(&self, bridges: &dyn BridgeBuses) -> Option<SourceId> {
        let (hops, _) = self.path.as_chunks::<2>();
        let (&[device, function], rest) = hops.split_first()?;
        let mut at = SourceId::new(self.start_bus, device, function)?;
        for &[device, function] in rest {
            let (secondary, _) = bridges.bus_range(at)?;
            at = SourceId::new(secondary, device, function)?;
        }
        Some(at)
    }

    /// Whether this scope claims the function `source`: an endpoint scope
    /// that ends at it, or a bridge scope at it or above its bus.
    fn covers(&self, source: SourceId, bridges: &dyn BridgeBuses) -> bool {
        let Some(target) = self.resolve(bridges) else {
            return false;
        };
        match self.kind {
            ScopeKind::Endpoint => target == source,
            ScopeKind::Bridge => {
                target == source
                    || bridges
                        .bus_range(target)
                        .is_some_and(|(secondary, subordinate)| {
                            (secondary..=subordinate).contains(&source.bus())
                        })
            }
            _ => false,
        }
    }
}

/// The requester ids the fabric tags a function's DMA with besides its own:
/// a bridge that takes ownership of the requests below it.
pub trait DmaAliases {
    /// Visit each requester id `source`'s DMA also arrives as.
    fn aliases(&self, source: SourceId, visit: &mut dyn FnMut(SourceId));
}

/// What [`emit_unit_nodes`] placed in the tree.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct UnitNodes {
    /// Units given a node, in table order: a unit past them has no node, so
    /// nothing brings it up and nothing behind it can be confined.
    pub emitted: usize,
    /// Reserved windows no unit's node carries — past its room, or on a
    /// segment `bridges` does not reach: those functions lose their firmware
    /// DMA rather than bypass translation.
    pub dropped: usize,
}

impl UnitNodes {
    /// Whether a unit of `dmar` on `segment` was left without a node, so
    /// nothing on the segment behind it can be confined.
    #[must_use]
    pub fn strand(&self, dmar: &Dmar<'_>, segment: u16) -> bool {
        dmar.units()
            .skip(self.emitted)
            .any(|unit| unit.segment() == segment)
    }
}

/// Emit one [`HwDeviceClass::Iommu`] node per unit, numbered from `first_id`
/// in table order and keyed `compatible`, carrying its register window and
/// each firmware reserved window of a function it translates — kept for the
/// function's own stream and for every alias of it, since firmware's DMA
/// arrives under whichever the fabric tags it with. `fabric` — its bridges'
/// buses and the aliases its DMA arrives under — describes `segment`, so a
/// window on another segment is never resolved through it. With no fabric, a
/// hierarchy that formed no tree, no window is kept: every unit comes up
/// blocking every stream. A full sink ends the emission: the units before it
/// keep their nodes.
///
/// # Errors
///
/// [`DiscoveryError::MalformedSource`] for a unit numbering past `u32` or a
/// compatible string no match key can hold.
pub fn emit_unit_nodes(
    dmar: &Dmar<'_>,
    first_id: u32,
    compatible: &[u8],
    segment: u16,
    fabric: Option<(&dyn BridgeBuses, &dyn DmaAliases)>,
    sink: &mut dyn HwNodeSink,
) -> Result<UnitNodes, DiscoveryError> {
    let key = HwMatchKey::compatible(compatible).map_err(|_| DiscoveryError::MalformedSource)?;
    let endpoints = |region: &Rmrr<'_>| {
        region
            .scopes()
            .filter(|scope| scope.kind() == ScopeKind::Endpoint)
            .count()
    };
    let mut placed = UnitNodes {
        emitted: 0,
        dropped: dmar
            .reserved_regions()
            .filter(|region| fabric.is_none() || region.segment() != segment)
            .map(|region| endpoints(&region))
            .sum(),
    };
    for (index, unit) in dmar.units().enumerate() {
        let id = unit_node_id(first_id, index).ok_or(DiscoveryError::MalformedSource)?;
        let mut node = HwNode::new(id, HW_NODE_ROOT_ID, HwDeviceClass::Iommu);
        node.push_match_key(key)
            .and_then(|()| {
                node.push_resource(HwResource::mmio(unit.register_base(), unit.register_len()))
            })
            .map_err(|_| DiscoveryError::MalformedSource)?;
        if let Some(fabric) = fabric {
            keep_windows(
                dmar,
                (index, segment),
                fabric,
                &mut node,
                &mut placed.dropped,
            );
        }
        if sink.emit(node).is_err() {
            break;
        }
        placed.emitted += 1;
    }
    Ok(placed)
}

/// Keep on `node` each firmware reserved window on `segment` of a function
/// unit `index` translates, for its own stream and every alias of it,
/// counting in `dropped` each the node has no room for.
fn keep_windows(
    dmar: &Dmar<'_>,
    (index, segment): (usize, u16),
    (bridges, aliases): (&dyn BridgeBuses, &dyn DmaAliases),
    node: &mut HwNode,
    dropped: &mut usize,
) {
    for region in dmar
        .reserved_regions()
        .filter(|region| region.segment() == segment)
    {
        for scope in region.scopes().filter(|s| s.kind() == ScopeKind::Endpoint) {
            let Some(source) = scope.resolve(bridges) else {
                continue;
            };
            if dmar.unit_for(region.segment(), source, bridges) != Some(index) {
                continue;
            }
            let mut keep = |stream: SourceId| {
                let Ok(window) =
                    IommuReservedWindow::new(u32::from(stream.raw()), region.base(), region.len())
                        .map(HwResource::iommu_reserved_window)
                else {
                    *dropped += 1;
                    return;
                };
                // Firmware may name one window for a function twice, and
                // functions behind one bridge share its alias.
                if !node.resources().contains(&window) && node.push_resource(window).is_err() {
                    *dropped += 1;
                }
            };
            keep(source);
            aliases.aliases(source, &mut keep);
        }
    }
}

/// The node of the unit [`emit_unit_nodes`] numbered from `first_id` that
/// translates the PCI function `source` on `segment`, or [`None`] when no
/// unit with a node does.
#[must_use]
pub fn unit_node(
    dmar: &Dmar<'_>,
    first_id: u32,
    nodes: UnitNodes,
    segment: u16,
    source: SourceId,
    bridges: &dyn BridgeBuses,
) -> Option<u32> {
    let index = dmar
        .unit_for(segment, source, bridges)
        .filter(|&index| index < nodes.emitted)?;
    unit_node_id(first_id, index)
}

fn unit_node_id(first_id: u32, index: usize) -> Option<u32> {
    first_id.checked_add(u32::try_from(index).ok()?)
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    let mut raw = [0u8; 8];
    raw.copy_from_slice(&bytes[offset..offset + 8]);
    u64::from_le_bytes(raw)
}

#[cfg(test)]
#[path = "dmar_tests.rs"]
mod tests;
