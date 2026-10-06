//! Second-level page-table entries, the root and context entries that lead
//! to them, and invalidation descriptors (VT-d rev. 4.1 §9.3, §9.4, §9.8,
//! §6.5.2).

use tairix_kernel_iommu_api::{Access, Pte, PteFormat, MESSAGE_WINDOW};

const READ: u64 = 1 << 0;
const WRITE: u64 = 1 << 1;
const READ_WRITE: u64 = READ | WRITE;
/// A leaf above level 0.
const PAGE_SIZE: u64 = 1 << 7;
/// Address bits 51:12 of an entry.
pub(crate) const ADDRESS: u64 = 0x000F_FFFF_FFFF_F000;

/// The second-level entry encoding, with the large leaves the unit supports.
pub(crate) struct SecondLevel {
    pub large_2m: bool,
    pub large_1g: bool,
}

impl PteFormat for SecondLevel {
    fn leaf_allowed(&self, level: u32) -> bool {
        match level {
            0 => true,
            1 => self.large_2m,
            2 => self.large_1g,
            _ => false,
        }
    }

    fn table(&self, phys: u64, _level: u32) -> u64 {
        // Access is the conjunction of every level's bits, so a table grants
        // both and the leaf decides.
        (phys & ADDRESS) | READ_WRITE
    }

    fn leaf(&self, phys: u64, level: u32, access: Access) -> u64 {
        let mut entry = phys & ADDRESS;
        if access.read() {
            entry |= READ;
        }
        if access.write() {
            entry |= WRITE;
        }
        if level > 0 {
            entry |= PAGE_SIZE;
        }
        entry
    }

    fn decode(&self, entry: u64, level: u32) -> Pte {
        if entry & READ_WRITE == 0 {
            Pte::Absent
        } else if level > 0 && entry & PAGE_SIZE == 0 {
            Pte::Table(entry & ADDRESS)
        } else {
            let access = match entry & READ_WRITE {
                READ => Access::READ,
                WRITE => Access::WRITE,
                _ => Access::READ_WRITE,
            };
            Pte::Leaf(entry & ADDRESS, access)
        }
    }
}

/// Root and context entries: a present bit and a table pointer in the low
/// word.
pub(crate) const PRESENT: u64 = 1 << 0;

/// The low word of a context entry: present, translating untranslated
/// requests through the second level (`TT = 00`, so a translated request —
/// ATS — is blocked), rooted at `table`.
pub(crate) fn context_low(table: u64) -> u64 {
    (table & ADDRESS) | PRESENT
}

/// The high word of a context entry: the domain id and the address width
/// its `levels`-level tables translate.
pub(crate) fn context_high(domain: u16, levels: u32) -> u64 {
    (u64::from(domain) << 8) | u64::from(levels - 2)
}

/// Invalidation descriptors: two 64-bit words each.
pub(crate) type Descriptor = [u64; 2];

const DESC_CONTEXT: u64 = 0x1;
const DESC_IOTLB: u64 = 0x2;
const DESC_IEC: u64 = 0x4;
const DESC_WAIT: u64 = 0x5;
/// An interrupt entry cache invalidation of one entry rather than all.
const IEC_INDEX: u64 = 1 << 4;
const GRANULARITY_GLOBAL: u64 = 0b01 << 4;
const GRANULARITY_DOMAIN: u64 = 0b10 << 4;
const GRANULARITY_DEVICE: u64 = 0b11 << 4;
const IOTLB_DRAIN_WRITES: u64 = 1 << 6;
const IOTLB_DRAIN_READS: u64 = 1 << 7;
const WAIT_STATUS_WRITE: u64 = 1 << 5;
const WAIT_FENCE: u64 = 1 << 6;

/// Invalidate every cached context entry.
pub(crate) const fn context_global() -> Descriptor {
    [DESC_CONTEXT | GRANULARITY_GLOBAL, 0]
}

/// Invalidate the cached context entry of one source id.
pub(crate) fn context_device(domain: u16, source: u16) -> Descriptor {
    [
        DESC_CONTEXT | GRANULARITY_DEVICE | (u64::from(domain) << 16) | (u64::from(source) << 32),
        0,
    ]
}

/// Invalidate every cached translation and paging-structure entry of one
/// domain, draining the translated requests already in flight where the
/// unit can.
pub(crate) fn iotlb_domain(domain: u16, drain_reads: bool, drain_writes: bool) -> Descriptor {
    let mut low = DESC_IOTLB | GRANULARITY_DOMAIN | (u64::from(domain) << 16);
    if drain_reads {
        low |= IOTLB_DRAIN_READS;
    }
    if drain_writes {
        low |= IOTLB_DRAIN_WRITES;
    }
    [low, 0]
}

/// Once every earlier descriptor has completed, write `status` to the
/// 4-byte-aligned physical `address`.
pub(crate) fn wait(status: u32, address: u64) -> Descriptor {
    [
        DESC_WAIT | WAIT_STATUS_WRITE | WAIT_FENCE | (u64::from(status) << 32),
        address & !0b11,
    ]
}

/// A wait that writes no status: what stands in for a descriptor the unit
/// rejected, so the queue runs on past it.
pub(crate) fn fence() -> Descriptor {
    [DESC_WAIT | WAIT_FENCE, 0]
}

/// Invalidate the cached copy of every interrupt remapping entry.
pub(crate) fn iec_global() -> Descriptor {
    [DESC_IEC, 0]
}

/// Invalidate the cached copy of interrupt remapping entry `entry`.
pub(crate) fn iec_entry(entry: u16) -> Descriptor {
    [DESC_IEC | IEC_INDEX | (u64::from(entry) << 32), 0]
}

/// An interrupt remapping table entry (rev. 4.1 §9.10): its two words, the
/// low one carrying the present bit, so it is written last.
pub(crate) type Irte = [u64; 2];

const IRTE_PRESENT: u64 = 1 << 0;
const IRTE_LEVEL: u64 = 1 << 4;
/// Source validation: the requester id must equal the entry's (all 16 bits
/// compared), or its bus must lie in the entry's range.
const SVT_REQUESTER: u64 = 0b01 << 18;
const SVT_BUSES: u64 = 0b10 << 18;

/// The entry delivering `target` from `source`: physical destination mode,
/// fixed delivery, no redirection hint. In extended mode the destination is
/// the whole 32-bit x2APIC id; otherwise an 8-bit APIC id in bits 47:40.
pub(crate) fn irte(
    source: tairix_kernel_iommu_api::InterruptSource,
    target: tairix_kernel_iommu_api::InterruptTarget,
    extended: bool,
) -> Option<Irte> {
    use tairix_kernel_iommu_api::InterruptSource;

    let destination = if extended {
        u64::from(target.destination) << 32
    } else {
        u64::from(u8::try_from(target.destination).ok()?) << 40
    };
    let mut low = IRTE_PRESENT | (u64::from(target.vector) << 16) | destination;
    if target.level {
        low |= IRTE_LEVEL;
    }
    let high = match source {
        InterruptSource::Requester(id) => SVT_REQUESTER | u64::from(id),
        InterruptSource::Buses { first, last } if source.names_any() => {
            SVT_BUSES | (u64::from(first) << 8) | u64::from(last)
        }
        InterruptSource::Buses { .. } => return None,
    };
    Some([low, high])
}

/// The remappable-format MSI address naming entry `entry` (rev. 4.1
/// §5.1.2.2): the handle in bits 19:5 and 2, the format bit 4, no
/// sub-handle, so the data is not part of the index.
pub(crate) fn remappable_msi_address(entry: u16) -> u64 {
    MESSAGE_WINDOW.start
        | (u64::from(entry & 0x7FFF) << 5)
        | (1 << 4)
        | (u64::from(entry >> 15) << 2)
}

/// The remappable-format IO-APIC redirection entry naming entry `entry`
/// (rev. 4.1 §5.1.5.1): the handle in bits 63:49 and 11, the format bit 48,
/// and the entry's vector and trigger mode, which a level-triggered pin's
/// end-of-interrupt is matched by.
pub(crate) fn remappable_redirection(entry: u16, vector: u8, level: bool) -> u64 {
    (u64::from(entry & 0x7FFF) << 49)
        | (1 << 48)
        | (u64::from(level) << 15)
        | (u64::from(entry >> 15) << 11)
        | u64::from(vector)
}
