//! The I/O Virtualization Reporting Structure (IVRS): the AMD-Vi translation
//! units a platform has, the device ids each covers and how their requests
//! arrive, and the memory firmware keeps mastering through them.
//!
//! Pure byte-slice parsing. [`Ivrs::parse`] validates every block and every
//! device entry before anything is read out, so a table that is wrong
//! anywhere is refused whole and never half-applied.
//!
//! Reference: AMD I/O Virtualization Technology (IOMMU) Specification,
//! rev. 3.08, chapter 5.

use tairix_abi::{
    HwDeviceClass, HwMatchKey, HwNode, HwResource, IommuReservedWindow, ReservedAccess,
    HW_NODE_ROOT_ID,
};
use tairix_arch_api::{DiscoveryError, HwNodeSink};

use crate::acpi::{read_u16, read_u64, unit_node_id, AcpiError, SdtHeader, UnitNodes};
use crate::dmar::{Fabric, SourceId};

/// 4-byte ASCII signature of the I/O Virtualization Reporting Structure.
pub const IVRS_SIGNATURE: [u8; 4] = *b"IVRS";

/// Offset of the first block: the SDT header, `IVinfo` and eight reserved
/// bytes.
const BLOCKS_OFFSET: usize = 48;

const BLOCK_HEADER_LEN: usize = 4;
const PAGE: u64 = 0x1000;

const IVHD_LEGACY: u8 = 0x10;
const IVHD_EFR: u8 = 0x11;
const IVHD_ACPI: u8 = 0x40;
const IVMD_ALL: u8 = 0x20;
const IVMD_SELECT: u8 = 0x21;
const IVMD_RANGE: u8 = 0x22;

/// Where an IVHD's device entries start, by its type.
const IVHD_LEGACY_ENTRIES: usize = 24;
const IVHD_EFR_ENTRIES: usize = 40;
const IVMD_LEN: usize = 32;

const IVMD_UNITY: u8 = 1 << 0;
/// `IR` and `IW`: what a unity window lets its devices do.
const IVMD_READ: u8 = 1 << 1;
const IVMD_WRITE: u8 = 1 << 2;
const IVMD_EXCLUSION: u8 = 1 << 3;

const ENTRY_ALL: u8 = 0x01;
const ENTRY_SELECT: u8 = 0x02;
const ENTRY_RANGE_START: u8 = 0x03;
const ENTRY_RANGE_END: u8 = 0x04;
const ENTRY_ALIAS_SELECT: u8 = 0x42;
const ENTRY_ALIAS_RANGE: u8 = 0x43;
const ENTRY_EXTENDED_SELECT: u8 = 0x46;
const ENTRY_EXTENDED_RANGE: u8 = 0x47;
const ENTRY_SPECIAL: u8 = 0x48;
const ENTRY_ACPI_HID: u8 = 0xF0;

/// Bytes of an ACPI HID device entry before its UID.
const ACPI_HID_FIXED: usize = 22;

const SPECIAL_IOAPIC: u8 = 1;
const SPECIAL_HPET: u8 = 2;

/// The register window an AMD-Vi unit is mapped with: the control,
/// buffer-base and head/tail registers the kernel drives.
pub const UNIT_REGISTER_LEN: u64 = 0x4000;

/// A validated IVRS.
#[derive(Copy, Clone, Debug)]
pub struct Ivrs<'a> {
    blocks: &'a [u8],
}

impl<'a> Ivrs<'a> {
    /// Validate the IVRS in `bytes`: its header and checksum, and every block
    /// and device entry it carries.
    ///
    /// # Errors
    ///
    /// [`AcpiError`] for any structural defect: a bad signature, checksum or
    /// length, a block or entry that does not fit where it claims to, a range
    /// left open or closed twice, a unit register window that is not
    /// page-aligned or that two units share, or a memory definition that is
    /// not whole pages.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, AcpiError> {
        let header = SdtHeader::validate(bytes, &IVRS_SIGNATURE)?;
        let len = header.length as usize;
        if len < BLOCKS_OFFSET {
            return Err(AcpiError::BadLength);
        }
        let ivrs = Self {
            blocks: &bytes[BLOCKS_OFFSET..len],
        };
        ivrs.validate()?;
        Ok(ivrs)
    }

    /// Every translation unit, in table order: for a unit firmware describes
    /// in several blocks, the one of the highest type the kernel reads, as
    /// Linux takes it.
    pub fn units(&self) -> impl Iterator<Item = Ivhd<'a>> + 'a {
        let blocks = self.blocks();
        blocks.clone().filter_map(Ivhd::decode).filter(move |unit| {
            !blocks
                .clone()
                .filter_map(Ivhd::decode)
                .any(|other| other.names_same_unit(unit) && other.preferred_over(unit))
        })
    }

    /// Every memory definition, in table order.
    pub fn memory_definitions(&self) -> impl Iterator<Item = Ivmd> + 'a {
        self.blocks().filter_map(Ivmd::decode)
    }

    /// The index among [`Self::units`] of the unit covering device `device`
    /// on `segment`, or [`None`] when none does.
    #[must_use]
    pub fn unit_for(&self, segment: u16, device: u16) -> Option<usize> {
        self.units()
            .position(|unit| unit.segment() == segment && unit.covers(device))
    }

    fn blocks(&self) -> Blocks<'a> {
        Blocks { rest: self.blocks }
    }

    fn validate(&self) -> Result<(), AcpiError> {
        let mut rest = self.blocks;
        while !rest.is_empty() {
            let (block, tail) = split_block(rest)?;
            rest = tail;
            match block[0] {
                IVHD_LEGACY | IVHD_EFR | IVHD_ACPI => {
                    let start = entries_offset(block[0]);
                    if block.len() < start {
                        return Err(AcpiError::BadLength);
                    }
                    let base = read_u64(block, 8);
                    if base == 0 || !base.is_multiple_of(PAGE) {
                        return Err(AcpiError::BadLength);
                    }
                    validate_entries(&block[start..])?;
                }
                IVMD_ALL | IVMD_SELECT | IVMD_RANGE => {
                    if block.len() < IVMD_LEN {
                        return Err(AcpiError::BadLength);
                    }
                    let start = read_u64(block, 16);
                    let len = read_u64(block, 24);
                    let end = start.checked_add(len).ok_or(AcpiError::BadLength)?;
                    let first = read_u16(block, 4);
                    let last = read_u16(block, 6);
                    if !start.is_multiple_of(PAGE)
                        || !end.is_multiple_of(PAGE)
                        || end <= start
                        || (block[0] == IVMD_RANGE && last < first)
                    {
                        return Err(AcpiError::BadLength);
                    }
                }
                // A block a later revision defines is skipped by its own
                // length, which `split_block` has bounded.
                _ => {}
            }
        }
        self.validate_units()
    }

    /// Two units claiming one register window would each believe it alone
    /// drives it.
    fn validate_units(&self) -> Result<(), AcpiError> {
        for (index, unit) in self.units().enumerate() {
            let window = unit.register_window()?;
            for other in self.units().skip(index + 1) {
                let theirs = other.register_window()?;
                if window.start < theirs.end && theirs.start < window.end {
                    return Err(AcpiError::BadLength);
                }
            }
        }
        Ok(())
    }
}

fn entries_offset(kind: u8) -> usize {
    if kind == IVHD_LEGACY {
        IVHD_LEGACY_ENTRIES
    } else {
        IVHD_EFR_ENTRIES
    }
}

fn split_block(bytes: &[u8]) -> Result<(&[u8], &[u8]), AcpiError> {
    if bytes.len() < BLOCK_HEADER_LEN {
        return Err(AcpiError::Truncated);
    }
    let len = usize::from(read_u16(bytes, 2));
    if len < BLOCK_HEADER_LEN || len > bytes.len() {
        return Err(AcpiError::BadLength);
    }
    Ok(bytes.split_at(len))
}

/// Walks blocks a successful [`Ivrs::parse`] already proved well formed; it
/// stops rather than panics on anything it could not split.
#[derive(Clone)]
struct Blocks<'a> {
    rest: &'a [u8],
}

impl<'a> Iterator for Blocks<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<&'a [u8]> {
        let (block, rest) = split_block(self.rest).ok()?;
        self.rest = rest;
        Some(block)
    }
}

/// The length of the device entry of type `kind` at the head of `bytes`:
/// four bytes shifted by the type's top two bits, but for the one type
/// carrying its own length.
fn entry_len(kind: u8, bytes: &[u8]) -> Result<usize, AcpiError> {
    match kind {
        0x00..=0x3F => Ok(4),
        0x40..=0x7F => Ok(8),
        0x80..=0xBF => Ok(16),
        ENTRY_ACPI_HID => {
            let uid = *bytes.get(ACPI_HID_FIXED - 1).ok_or(AcpiError::Truncated)?;
            Ok(ACPI_HID_FIXED + usize::from(uid))
        }
        _ => Ok(32),
    }
}

fn validate_entries(mut bytes: &[u8]) -> Result<(), AcpiError> {
    // The first device of the range open, if one is.
    let mut open = None;
    while !bytes.is_empty() {
        let kind = bytes[0];
        let len = entry_len(kind, bytes)?;
        if len > bytes.len() {
            return Err(AcpiError::Truncated);
        }
        match kind {
            ENTRY_RANGE_START | ENTRY_ALIAS_RANGE | ENTRY_EXTENDED_RANGE => {
                if open.is_some() {
                    return Err(AcpiError::BadLength);
                }
                open = Some(read_u16(bytes, 1));
            }
            ENTRY_RANGE_END => match open.take() {
                // A range ending before it starts names no device firmware
                // could mean.
                Some(first) if read_u16(bytes, 1) >= first => {}
                _ => return Err(AcpiError::BadLength),
            },
            ENTRY_SPECIAL if !matches!(bytes[7], SPECIAL_IOAPIC | SPECIAL_HPET) => {
                return Err(AcpiError::BadLength);
            }
            _ => {}
        }
        bytes = &bytes[len..];
    }
    if open.is_some() {
        Err(AcpiError::BadLength)
    } else {
        Ok(())
    }
}

/// One AMD-Vi translation unit's definition block.
#[derive(Copy, Clone, Debug)]
pub struct Ivhd<'a> {
    kind: u8,
    device: u16,
    base: u64,
    segment: u16,
    entries: &'a [u8],
}

impl<'a> Ivhd<'a> {
    fn decode(block: &'a [u8]) -> Option<Self> {
        let kind = block[0];
        if !matches!(kind, IVHD_LEGACY | IVHD_EFR | IVHD_ACPI) {
            return None;
        }
        let start = entries_offset(kind);
        Some(Self {
            kind,
            device: read_u16(block, 4),
            base: read_u64(block, 8),
            segment: read_u16(block, 16),
            entries: block.get(start..)?,
        })
    }

    fn names_same_unit(&self, other: &Self) -> bool {
        self.segment == other.segment && self.device == other.device
    }

    fn preferred_over(&self, other: &Self) -> bool {
        self.kind > other.kind
    }

    /// The unit's own PCI function, as a device id on its segment.
    #[must_use]
    pub const fn device(&self) -> u16 {
        self.device
    }

    /// The physical base of the unit's registers.
    #[must_use]
    pub const fn register_base(&self) -> u64 {
        self.base
    }

    /// The PCI segment the unit's devices sit on.
    #[must_use]
    pub const fn segment(&self) -> u16 {
        self.segment
    }

    fn register_window(&self) -> Result<core::ops::Range<u64>, AcpiError> {
        self.base
            .checked_add(UNIT_REGISTER_LEN)
            .map(|end| self.base..end)
            .ok_or(AcpiError::BadLength)
    }

    /// The unit's device entries.
    #[must_use]
    pub fn entries(&self) -> Entries<'a> {
        Entries {
            rest: self.entries,
            start: None,
        }
    }

    /// Whether the unit translates the DMA of device `device`: one an entry
    /// names, as its own id or the id it arrives as.
    #[must_use]
    pub fn covers(&self, device: u16) -> bool {
        self.entries().any(|entry| entry.names(device))
    }

    /// Whether the unit covers any device of `first..=last`.
    fn covers_any(&self, first: u16, last: u16) -> bool {
        self.entries().any(|entry| entry.names_any(first, last))
    }

    /// The id the requests of device `device` arrive at the unit as, where
    /// firmware says it is another's.
    #[must_use]
    pub fn alias_of(&self, device: u16) -> Option<u16> {
        self.entries().find_map(|entry| match entry {
            DeviceEntry::Alias { devices, alias } if devices.contains(&device) => Some(alias),
            _ => None,
        })
    }

    /// Each I/O APIC the unit remaps: its APIC id and the device id its
    /// interrupt messages carry.
    pub fn ioapics(&self) -> impl Iterator<Item = (u8, u16)> + 'a {
        self.entries().filter_map(|entry| match entry {
            DeviceEntry::Special {
                kind: SpecialKind::IoApic,
                handle,
                device,
            } => Some((handle, device)),
            _ => None,
        })
    }
}

/// What a special device entry names.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SpecialKind {
    /// An I/O APIC, by its APIC id.
    IoApic,
    /// An HPET, by its block id.
    Hpet,
}

/// One device entry, ranges joined.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeviceEntry {
    /// Every device id on the segment.
    All,
    /// Device ids `first..=last`, each arriving as itself.
    Devices(core::ops::RangeInclusive<u16>),
    /// Device ids `devices`, each arriving as `alias`.
    Alias {
        /// The devices.
        devices: core::ops::RangeInclusive<u16>,
        /// The id their requests carry.
        alias: u16,
    },
    /// A device that is no PCI function, and the id its messages carry.
    Special {
        /// What it is.
        kind: SpecialKind,
        /// Its APIC or block id.
        handle: u8,
        /// The id its messages carry.
        device: u16,
    },
    /// An ACPI namespace device, by the id its requests carry.
    Acpi(u16),
}

impl DeviceEntry {
    /// Whether the entry names `device`, as one of its own ids or the id
    /// they arrive as.
    #[must_use]
    pub fn names(&self, device: u16) -> bool {
        self.names_any(device, device)
    }

    /// Whether the entry names any device of `first..=last`.
    fn names_any(&self, first: u16, last: u16) -> bool {
        let overlaps = |devices: &core::ops::RangeInclusive<u16>| {
            *devices.start() <= last && first <= *devices.end()
        };
        match self {
            Self::All => true,
            Self::Devices(devices) => overlaps(devices),
            Self::Alias { devices, alias } => overlaps(devices) || (first..=last).contains(alias),
            Self::Special { device: own, .. } | Self::Acpi(own) => (first..=last).contains(own),
        }
    }
}

/// Iterates a validated block's device entries, joining each range.
#[derive(Clone, Debug)]
pub struct Entries<'a> {
    rest: &'a [u8],
    /// The open range's first id, and its alias where it has one.
    start: Option<(u16, Option<u16>)>,
}

impl Iterator for Entries<'_> {
    type Item = DeviceEntry;

    fn next(&mut self) -> Option<DeviceEntry> {
        loop {
            let kind = *self.rest.first()?;
            let len = entry_len(kind, self.rest).ok()?;
            let (entry, rest) = self.rest.split_at_checked(len)?;
            self.rest = rest;
            let device = read_u16(entry, 1);
            match kind {
                ENTRY_ALL => return Some(DeviceEntry::All),
                ENTRY_SELECT | ENTRY_EXTENDED_SELECT => {
                    return Some(DeviceEntry::Devices(device..=device))
                }
                ENTRY_RANGE_START | ENTRY_EXTENDED_RANGE => self.start = Some((device, None)),
                ENTRY_ALIAS_RANGE => self.start = Some((device, Some(read_u16(entry, 5)))),
                ENTRY_RANGE_END => {
                    let (first, alias) = self.start.take()?;
                    let devices = first..=device;
                    return Some(match alias {
                        Some(alias) => DeviceEntry::Alias { devices, alias },
                        None => DeviceEntry::Devices(devices),
                    });
                }
                ENTRY_ALIAS_SELECT => {
                    return Some(DeviceEntry::Alias {
                        devices: device..=device,
                        alias: read_u16(entry, 5),
                    })
                }
                ENTRY_SPECIAL => {
                    return Some(DeviceEntry::Special {
                        kind: if entry[7] == SPECIAL_IOAPIC {
                            SpecialKind::IoApic
                        } else {
                            SpecialKind::Hpet
                        },
                        handle: entry[4],
                        device: read_u16(entry, 5),
                    })
                }
                ENTRY_ACPI_HID => return Some(DeviceEntry::Acpi(device)),
                // Padding, and the types a later revision defines.
                _ => {}
            }
        }
    }
}

/// One memory definition: memory firmware keeps mastering through the
/// devices it names, which each keep an identity window of it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Ivmd {
    /// The PCI segment its devices are on.
    segment: u16,
    /// The first and last device ids named; [`None`] for every device.
    devices: Option<(u16, u16)>,
    base: u64,
    len: u64,
    access: ReservedAccess,
}

impl Ivmd {
    fn decode(block: &[u8]) -> Option<Self> {
        let kind = block[0];
        let flags = block[1];
        if !matches!(kind, IVMD_ALL | IVMD_SELECT | IVMD_RANGE) {
            return None;
        }
        // An exclusion range passes its devices' DMA untranslated, whatever
        // it reads or writes; a unity window allows what it says, and one
        // allowing nothing keeps nothing.
        let access = if flags & IVMD_EXCLUSION != 0 {
            ReservedAccess::ReadWrite
        } else if flags & IVMD_UNITY != 0 {
            match (flags & IVMD_READ != 0, flags & IVMD_WRITE != 0) {
                (true, true) => ReservedAccess::ReadWrite,
                (true, false) => ReservedAccess::Read,
                (false, true) => ReservedAccess::Write,
                (false, false) => return None,
            }
        } else {
            return None;
        };
        let first = read_u16(block, 4);
        Some(Self {
            segment: read_u16(block, 8),
            devices: match kind {
                IVMD_ALL => None,
                IVMD_SELECT => Some((first, first)),
                _ => Some((first, read_u16(block, 6))),
            },
            base: read_u64(block, 16),
            len: read_u64(block, 24),
            access,
        })
    }

    /// Whether the window is kept for device `device` on `segment`.
    #[must_use]
    pub fn names(&self, segment: u16, device: u16) -> bool {
        self.segment == segment
            && self
                .devices
                .is_none_or(|(first, last)| (first..=last).contains(&device))
    }

    /// What its devices may do there.
    #[must_use]
    pub const fn access(&self) -> ReservedAccess {
        self.access
    }

    /// Whether it names a device `unit` covers.
    fn reaches(&self, unit: &Ivhd<'_>) -> bool {
        self.segment == unit.segment()
            && self
                .devices
                .is_none_or(|(first, last)| unit.covers_any(first, last))
    }

    /// The window's first byte.
    #[must_use]
    pub const fn base(&self) -> u64 {
        self.base
    }

    /// The window's length in bytes: whole pages, since [`Ivrs::parse`]
    /// proved both ends page-aligned.
    #[must_use]
    pub const fn len(&self) -> u64 {
        self.len
    }

    /// Always `false`: a validated window spans at least one page.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        false
    }
}

/// What a segment's walk says of the functions a unit's memory definitions
/// name: every function it found, and how each one's DMA is tagged.
pub trait Walk: Fabric {
    /// Visit every function the walk found.
    fn functions(&self, visit: &mut dyn FnMut(SourceId));
}

/// Emit one [`HwDeviceClass::Iommu`] node per unit, numbered from `first_id`
/// in table order and keyed `compatible`, carrying its register window, its
/// own function as its address, and each memory definition's window for
/// every function the unit translates that the definition names: kept for
/// the function's own id, the id firmware says it arrives as, and every
/// alias the walk finds, never for a function below an external-facing port.
/// `walk` answers each segment's walk; a segment it has none for keeps no
/// window.
///
/// # Errors
///
/// [`DiscoveryError::MalformedSource`] when a unit's node cannot be built.
pub fn emit_unit_nodes<'w>(
    ivrs: &Ivrs<'_>,
    first_id: u32,
    compatible: &[u8],
    walk: &dyn Fn(u16) -> Option<&'w dyn Walk>,
    sink: &mut dyn HwNodeSink,
) -> Result<UnitNodes, DiscoveryError> {
    let key = HwMatchKey::compatible(compatible).map_err(|_| DiscoveryError::MalformedSource)?;
    let mut placed = UnitNodes::default();
    for (index, unit) in ivrs.units().enumerate() {
        let id = unit_node_id(first_id, index).ok_or(DiscoveryError::MalformedSource)?;
        let mut node = HwNode::new(id, HW_NODE_ROOT_ID, HwDeviceClass::Iommu);
        node.set_address((u32::from(unit.segment()) << 16) | u32::from(unit.device()));
        node.push_match_key(key)
            .and_then(|()| {
                node.push_resource(HwResource::mmio(unit.register_base(), UNIT_REGISTER_LEN))
            })
            .map_err(|_| DiscoveryError::MalformedSource)?;
        match walk(unit.segment()) {
            Some(walk) => keep_windows(ivrs, (index, &unit), walk, &mut node, &mut placed),
            None => {
                placed.dropped += ivrs
                    .memory_definitions()
                    .filter(|window| window.reaches(&unit))
                    .count();
            }
        }
        if sink.emit(node).is_err() {
            break;
        }
        placed.emitted += 1;
    }
    Ok(placed)
}

/// Keep on `node` each memory definition's window for every function the
/// unit `index` translates that it names, counting in `placed` each the
/// node has no room for, and each refused for a function below an
/// external-facing port.
fn keep_windows(
    ivrs: &Ivrs<'_>,
    (index, unit): (usize, &Ivhd<'_>),
    walk: &dyn Walk,
    node: &mut HwNode,
    placed: &mut UnitNodes,
) {
    for window in ivrs.memory_definitions() {
        walk.functions(&mut |source| {
            let device = source.raw();
            if !window.names(unit.segment(), device)
                || ivrs.unit_for(unit.segment(), device) != Some(index)
            {
                return;
            }
            if walk.untrusted(source) {
                placed.untrusted += 1;
                return;
            }
            let mut keep = |stream: SourceId| {
                let Ok(kept) = IommuReservedWindow::new(
                    u32::from(stream.raw()),
                    window.base(),
                    window.len(),
                    window.access(),
                )
                .map(HwResource::iommu_reserved_window) else {
                    placed.dropped += 1;
                    return;
                };
                // Functions behind one bridge share its alias.
                if !node.resources().contains(&kept) && node.push_resource(kept).is_err() {
                    placed.dropped += 1;
                }
            };
            keep(source);
            if let Some(alias) = unit.alias_of(device) {
                keep(SourceId::from_raw(alias));
            }
            walk.aliases(source, &mut keep);
        });
    }
}

/// The node of the unit [`emit_unit_nodes`] numbered from `first_id` that
/// translates device `device` on `segment`, or [`None`] when no unit with a
/// node does.
#[must_use]
pub fn unit_node(
    ivrs: &Ivrs<'_>,
    first_id: u32,
    nodes: UnitNodes,
    segment: u16,
    device: u16,
) -> Option<u32> {
    let index = ivrs
        .unit_for(segment, device)
        .filter(|&index| index < nodes.emitted)?;
    unit_node_id(first_id, index)
}

/// Visit each I/O APIC the unit with a node names, as
/// [`crate::dmar::ioapic_sources`] does for a DMAR: its APIC id, that unit's
/// node, and the device id its interrupt messages carry.
pub fn ioapic_sources(
    ivrs: &Ivrs<'_>,
    first_id: u32,
    nodes: UnitNodes,
    visit: &mut dyn FnMut(u8, u32, u16),
) {
    for (index, unit) in ivrs.units().enumerate().take(nodes.emitted) {
        let Some(node) = unit_node_id(first_id, index) else {
            continue;
        };
        for (id, device) in unit.ioapics() {
            visit(id, node, device);
        }
    }
}

#[cfg(test)]
#[path = "ivrs_tests.rs"]
mod tests;
