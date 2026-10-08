//! The VT-d register set.
//!
//! Offsets and fields are those of Intel VT-d rev. 4.1 §11.4.

/// Bits of `CAP.NFR`, the fault recording registers less one.
const NFR_BITS: u32 = 8;
/// The most fault recording registers a unit can have.
pub(crate) const MOST_FAULT_RECORDS: usize = 1 << NFR_BITS;

pub(crate) const VER: usize = 0x000;
pub(crate) const CAP: usize = 0x008;
pub(crate) const ECAP: usize = 0x010;
pub(crate) const GCMD: usize = 0x018;
pub(crate) const GSTS: usize = 0x01C;
pub(crate) const RTADDR: usize = 0x020;
pub(crate) const CCMD: usize = 0x028;
pub(crate) const FSTS: usize = 0x034;
pub(crate) const FECTL: usize = 0x038;
pub(crate) const FEDATA: usize = 0x03C;
pub(crate) const FEADDR: usize = 0x040;
pub(crate) const FEUADDR: usize = 0x044;
pub(crate) const PMEN: usize = 0x064;
pub(crate) const IQH: usize = 0x080;
pub(crate) const IQT: usize = 0x088;
pub(crate) const IQA: usize = 0x090;
pub(crate) const IRTA: usize = 0x0B8;

/// The IOTLB invalidate register, at this offset past `ECAP.IRO * 16`.
pub(crate) const IOTLB_REG: usize = 0x8;
/// Bytes one fault recording register spans.
pub(crate) const FAULT_RECORD_LEN: usize = 16;

/// Global command and status bits: one command bit, one status bit each.
pub(crate) const GCMD_TE: u32 = 1 << 31;
pub(crate) const GCMD_SRTP: u32 = 1 << 30;
pub(crate) const GCMD_WBF: u32 = 1 << 27;
pub(crate) const GCMD_QIE: u32 = 1 << 26;
pub(crate) const GCMD_IRE: u32 = 1 << 25;
pub(crate) const GCMD_SIRTP: u32 = 1 << 24;
pub(crate) const GCMD_CFI: u32 = 1 << 23;
pub(crate) const GSTS_TES: u32 = 1 << 31;
pub(crate) const GSTS_RTPS: u32 = 1 << 30;
pub(crate) const GSTS_WBFS: u32 = 1 << 27;
pub(crate) const GSTS_QIES: u32 = 1 << 26;
pub(crate) const GSTS_IRES: u32 = 1 << 25;
pub(crate) const GSTS_IRTPS: u32 = 1 << 24;
pub(crate) const GSTS_CFIS: u32 = 1 << 23;
/// Status bits whose command is one-shot: writing them back would re-issue
/// the command, so a read-modify-write of GCMD masks them out.
pub(crate) const GSTS_ONE_SHOT: u32 = (1 << 30) | (1 << 29) | (1 << 27) | (1 << 24);
/// The status bits that mirror a persistent command.
pub(crate) const GSTS_PERSISTENT: u32 = 0xFF80_0000 & !GSTS_ONE_SHOT;

pub(crate) const CCMD_ICC: u64 = 1 << 63;
pub(crate) const CCMD_GLOBAL: u64 = 0b01 << 61;

pub(crate) const IOTLB_IVT: u64 = 1 << 63;
pub(crate) const IOTLB_GLOBAL: u64 = 0b01 << 60;
pub(crate) const IOTLB_DRAIN_READS: u64 = 1 << 49;
pub(crate) const IOTLB_DRAIN_WRITES: u64 = 1 << 48;

pub(crate) const FSTS_PFO: u32 = 1 << 0;
pub(crate) const FSTS_PPF: u32 = 1 << 1;
pub(crate) const FSTS_IQE: u32 = 1 << 4;
pub(crate) const FSTS_ICE: u32 = 1 << 5;
pub(crate) const FSTS_ITE: u32 = 1 << 6;
/// Every status bit software clears by writing it back.
pub(crate) const FSTS_ERRORS: u32 = FSTS_PFO | FSTS_IQE | FSTS_ICE | FSTS_ITE;
pub(crate) const FECTL_IM: u32 = 1 << 31;

/// The interrupt remapping table address register's extended interrupt
/// mode bit: 32-bit x2APIC destinations.
pub(crate) const IRTA_EIME: u64 = 1 << 11;

pub(crate) const PMEN_EPM: u32 = 1 << 31;
pub(crate) const PMEN_PRS: u32 = 1 << 0;

/// Bits `[shift, shift + width)` of `value`, `width` at most 32.
pub(crate) fn field(value: u64, shift: u32, width: u32) -> u32 {
    u32::try_from((value >> shift) & ((1 << width) - 1)).unwrap_or(u32::MAX)
}

/// [`field`], as an index or byte count.
pub(crate) fn field_usize(value: u64, shift: u32, width: u32) -> usize {
    usize::try_from(field(value, shift, width)).unwrap_or(usize::MAX)
}

/// The low 32 bits of `value`.
pub(crate) fn low32(value: u64) -> u32 {
    field(value, 0, 32)
}

/// Descriptors one queue page holds: two entries each.
pub(crate) const QUEUE_SLOTS: usize = tairix_kernel_iommu_api::CommandQueue::SLOTS;

/// The descriptor slot a queue head or tail register names.
pub(crate) fn queue_index(register: u64) -> usize {
    field_usize(register, 4, 15) % QUEUE_SLOTS
}

/// Capability register fields.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Cap(pub u64);

impl Cap {
    /// The domain ids the unit takes: no more than its 16-bit domain field
    /// names, whatever a reserved encoding claims.
    pub fn domains(self) -> u32 {
        (1u32 << (4 + 2 * field(self.0, 0, 3))).min(1 << 16)
    }
    pub fn required_write_buffer_flush(self) -> bool {
        self.0 & (1 << 4) != 0
    }
    pub fn protected_low_memory(self) -> bool {
        self.0 & (1 << 5) != 0
    }
    pub fn protected_high_memory(self) -> bool {
        self.0 & (1 << 6) != 0
    }
    pub fn caching_mode(self) -> bool {
        self.0 & (1 << 7) != 0
    }
    /// Supported adjusted guest address widths: bit 1 is 3-level (39-bit),
    /// bit 2 4-level (48-bit), bit 3 5-level (57-bit).
    pub fn sagaw(self) -> u64 {
        (self.0 >> 8) & 0x1F
    }
    /// Maximum guest address width, in bits.
    pub fn mgaw(self) -> u32 {
        field(self.0, 16, 6) + 1
    }
    pub fn fault_record_offset(self) -> usize {
        field_usize(self.0, 24, 10) * 16
    }
    pub fn large_page_2m(self) -> bool {
        self.0 & (1 << 34) != 0
    }
    pub fn large_page_1g(self) -> bool {
        self.0 & (1 << 35) != 0
    }
    /// Page-selective-within-domain IOTLB invalidation.
    pub fn page_selective(self) -> bool {
        self.0 & (1 << 39) != 0
    }
    pub fn fault_records(self) -> usize {
        field_usize(self.0, 40, NFR_BITS) + 1
    }
    /// The largest address mask one page-selective invalidation takes.
    pub fn max_address_mask(self) -> u32 {
        field(self.0, 48, 6)
    }
    pub fn drain_writes(self) -> bool {
        self.0 & (1 << 54) != 0
    }
    pub fn drain_reads(self) -> bool {
        self.0 & (1 << 55) != 0
    }
}

/// Extended capability register fields.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Ecap(pub u64);

impl Ecap {
    pub fn coherent(self) -> bool {
        self.0 & (1 << 0) != 0
    }
    pub fn queued_invalidation(self) -> bool {
        self.0 & (1 << 1) != 0
    }
    pub fn iotlb_offset(self) -> usize {
        field_usize(self.0, 8, 10) * 16
    }
    pub fn interrupt_remapping(self) -> bool {
        self.0 & (1 << 3) != 0
    }
    pub fn extended_interrupts(self) -> bool {
        self.0 & (1 << 4) != 0
    }
}
