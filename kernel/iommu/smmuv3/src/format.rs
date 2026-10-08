//! What an Arm `SMMUv3` reads from memory and its queues (Arm IHI 0070): stream
//! table entries and descriptors, context descriptors, commands, event
//! records, and the VMSAv8-64 stage 1 and stage 2 table entries its walker
//! reads at a 4 KiB granule.

use tairix_kernel_iommu_api::{
    Access, Command, Fault, FaultReason, Pte, PteFormat, Stage, IO_PAGE_SIZE,
};

/// Bits of output address a table entry names at a 4 KiB granule without the
/// large-address extensions, which this family leaves off.
pub const OUTPUT_BITS: u32 = 48;

/// Bytes in one stream table entry.
pub const STE_BYTES: usize = 64;
/// Words in one stream table entry or context descriptor.
pub const STE_WORDS: usize = STE_BYTES / 8;
/// Words in one event record.
pub const EVENT_WORDS: usize = 4;
/// `log2` of the stream table entries one second-level table holds: one
/// table frame of them.
pub const SPLIT: u32 = 6;

/// A stream table entry's or context descriptor's eight words.
pub type Entry = [u64; STE_WORDS];

const ADDRESS_51_6: u64 = 0x000F_FFFF_FFFF_FFC0;
const ADDRESS_51_4: u64 = 0x000F_FFFF_FFFF_FFF0;
const ADDRESS_51_2: u64 = 0x000F_FFFF_FFFF_FFFC;
/// A table entry's output address at a 4 KiB granule.
const ADDRESS_47_12: u64 = (1 << OUTPUT_BITS) - IO_PAGE_SIZE;

/// A cacheability field (`IR`, `OR`, `CIR`, …): write-back, read- and
/// write-allocate. Every unit driven is coherent with the CPUs' caches.
const WRITE_BACK: u32 = 0b01;
/// A shareability field: inner shareable.
const INNER_SHAREABLE: u32 = 0b11;
/// A memory-attribute field (a stage 2 leaf's `MemAttr`, a sync's
/// `MSIAttr`): normal memory, inner and outer write-back.
const NORMAL_WRITE_BACK: u64 = 0b1111;

/// `SMMU_CR1`: the unit walks its queues and tables write-back, inner
/// shareable (queue IC, OC, SH, then table IC, OC, SH, two bits each).
pub const CR1: u32 = WRITE_BACK
    | WRITE_BACK << 2
    | INNER_SHAREABLE << 4
    | WRITE_BACK << 6
    | WRITE_BACK << 8
    | INNER_SHAREABLE << 10;

/// A stream table level-1 descriptor: a second-level table of
/// `2^SPLIT` entries at `table`.
#[must_use]
pub const fn level1(table: u64) -> u64 {
    (table & ADDRESS_51_6) | (SPLIT as u64 + 1)
}

/// A valid stream table entry's word 0: valid, and its configuration.
const STE_VALID: u64 = 1;
const STE_CONFIG_SHIFT: u32 = 1;
const STE_ABORT: u64 = 0b000;
const STE_STAGE1: u64 = 0b101;
const STE_STAGE2: u64 = 0b110;

/// A stream table entry that aborts its stream's transactions and records
/// nothing: a silenced stream. A blocked stream's entry is invalid, so each
/// of its transactions records `C_BAD_STE`.
#[must_use]
pub const fn silent_ste() -> Entry {
    [
        STE_VALID | STE_ABORT << STE_CONFIG_SHIFT,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
    ]
}

/// Whether `entry` translates, and so is read past its word 0: an invalid or
/// aborting entry is its word 0 alone.
#[must_use]
pub const fn translates(entry: &Entry) -> bool {
    entry[0] & STE_VALID != 0 && (entry[0] >> STE_CONFIG_SHIFT) & 0b111 != STE_ABORT
}

/// How a domain is translated, which its streams' entries name.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Stage2 {
    /// The domain's VMID.
    pub vmid: u16,
    /// Its tables' root.
    pub root: u64,
    /// Bits of IOVA its tables translate.
    pub input_bits: u32,
    /// Levels its walk starts its tables at.
    pub levels: u32,
    /// The output size's encoding.
    pub output: u32,
}

/// The stream table entry translating its stream through `stage`'s tables at
/// stage 2, untranslated at stage 1. Translated (ATS) requests are refused
/// (`EATS` zero) and faults are recorded and abort rather than stall.
#[must_use]
pub const fn stage2_ste(stage: Stage2) -> Entry {
    const S2AA64: u64 = 1 << 51;
    const S2PTW: u64 = 1 << 54;
    const S2R: u64 = 1 << 58;
    let t0sz = 64 - stage.input_bits as u64;
    // A walk of four levels starts at level 0, of three at level 1.
    let sl0 = stage.levels as u64 - 2;
    let vtcr = t0sz
        | sl0 << 6
        | (WRITE_BACK as u64) << 8
        | (WRITE_BACK as u64) << 10
        | (INNER_SHAREABLE as u64) << 12
        | (stage.output as u64) << 16;
    [
        STE_VALID | STE_STAGE2 << STE_CONFIG_SHIFT,
        0,
        stage.vmid as u64 | vtcr << 32 | S2AA64 | S2PTW | S2R,
        stage.root & ADDRESS_51_4,
        0,
        0,
        0,
        0,
    ]
}

/// The stream table entry translating its stream at stage 1 through the one
/// context descriptor at `cd`. `stalls` disables stalling where the unit
/// could stall a fault rather than abort it.
#[must_use]
pub const fn stage1_ste(cd: u64, stalls: bool) -> Entry {
    const S1STALLD: u64 = 1 << 27;
    let word1 = (WRITE_BACK as u64) << 2
        | (WRITE_BACK as u64) << 4
        | (INNER_SHAREABLE as u64) << 6
        | if stalls { S1STALLD } else { 0 };
    [
        STE_VALID | STE_STAGE1 << STE_CONFIG_SHIFT | (cd & ADDRESS_51_6),
        word1,
        0,
        0,
        0,
        0,
        0,
        0,
    ]
}

/// A stage 1 domain's context descriptor.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Stage1 {
    /// The domain's ASID.
    pub asid: u16,
    /// Its tables' root.
    pub root: u64,
    /// Bits of IOVA its tables translate.
    pub input_bits: u32,
    /// The output size's encoding.
    pub output: u32,
}

/// `MAIR`: attribute 0 is normal memory, write-back read/write-allocate,
/// which every stage 1 entry names.
const MAIR: u64 = 0xFF;

/// `stage`'s context descriptor: translation through its tables from TTB0,
/// TTB1 walks disabled, faults recorded and aborted, its ASID private to the
/// unit.
#[must_use]
pub const fn context_descriptor(stage: Stage1) -> Entry {
    const EPD1: u64 = 1 << 30;
    const VALID: u64 = 1 << 31;
    const AA64: u64 = 1 << 41;
    const RECORD: u64 = 1 << 45;
    const ABORT: u64 = 1 << 46;
    const ASET: u64 = 1 << 47;
    let word0 = (64 - stage.input_bits as u64)
        | (WRITE_BACK as u64) << 8
        | (WRITE_BACK as u64) << 10
        | (INNER_SHAREABLE as u64) << 12
        | EPD1
        | VALID
        | (stage.output as u64) << 32
        | AA64
        | RECORD
        | ABORT
        | ASET
        | (stage.asid as u64) << 48;
    [word0, stage.root & ADDRESS_51_4, 0, MAIR, 0, 0, 0, 0]
}

const CMD_CFGI_STE: u64 = 0x03;
const CMD_CFGI_STE_RANGE: u64 = 0x04;
const CMD_CFGI_CD_ALL: u64 = 0x06;
const CMD_TLBI_NH_ASID: u64 = 0x11;
const CMD_TLBI_NH_VA: u64 = 0x12;
const CMD_TLBI_S12_VMALL: u64 = 0x28;
const CMD_TLBI_S2_IPA: u64 = 0x2A;
/// A VA TLBI's address, bits 63:12, and an IPA TLBI's, bits 51:12; the
/// `Leaf` bit below them left clear, so the walk caches for the address go
/// too.
const TLBI_VA: u64 = !0xFFF;
const TLBI_IPA: u64 = 0x000F_FFFF_FFFF_F000;
const CMD_TLBI_NSNH_ALL: u64 = 0x30;
const CMD_SYNC: u64 = 0x46;

/// Forget the cached entry of `stream`, and with `leaf` false, the level-1
/// descriptor leading to it.
#[must_use]
pub const fn cfgi_ste(stream: u32, leaf: bool) -> Command {
    [CMD_CFGI_STE | (stream as u64) << 32, leaf as u64]
}

/// Forget every cached stream table entry and descriptor.
#[must_use]
pub const fn cfgi_all() -> Command {
    [CMD_CFGI_STE_RANGE, 31]
}

/// Forget every context descriptor cached for `stream`.
#[must_use]
pub const fn cfgi_cd_all(stream: u32) -> Command {
    [CMD_CFGI_CD_ALL | (stream as u64) << 32, 0]
}

/// Forget every stage 1 translation cached under `asid`.
#[must_use]
pub const fn tlbi_asid(asid: u16) -> Command {
    [CMD_TLBI_NH_ASID | (asid as u64) << 48, 0]
}

/// Forget the stage 1 translation of the page at `iova` cached under `asid`,
/// and every walk cache entry leading to it.
#[must_use]
pub const fn tlbi_va(asid: u16, iova: u64) -> Command {
    [CMD_TLBI_NH_VA | (asid as u64) << 48, iova & TLBI_VA]
}

/// Forget the stage 2 translation of the page at `ipa` cached under `vmid`,
/// and every walk cache entry leading to it.
#[must_use]
pub const fn tlbi_ipa(vmid: u16, ipa: u64) -> Command {
    [CMD_TLBI_S2_IPA | (vmid as u64) << 32, ipa & TLBI_IPA]
}

/// Forget every stage 1 and stage 2 translation cached under `vmid`.
#[must_use]
pub const fn tlbi_vmid(vmid: u16) -> Command {
    [CMD_TLBI_S12_VMALL | (vmid as u64) << 32, 0]
}

/// Forget every non-secure, non-hypervisor translation cached.
#[must_use]
pub const fn tlbi_all() -> Command {
    [CMD_TLBI_NSNH_ALL, 0]
}

/// A `CMD_SYNC` that writes `token` to `status` once every command before it
/// is done: for a unit that sends messages.
#[must_use]
pub const fn sync_stored(token: u32, status: u64) -> Command {
    const SIGNAL_MESSAGE: u64 = 0b01 << 12;
    [
        CMD_SYNC
            | SIGNAL_MESSAGE
            | (INNER_SHAREABLE as u64) << 22
            | NORMAL_WRITE_BACK << 24
            | (token as u64) << 32,
        status & ADDRESS_51_2,
    ]
}

/// A `CMD_SYNC` signalling nothing, which the unit consumes only once every
/// command before it is done.
#[must_use]
pub const fn sync_consumed(_token: u32, _status: u64) -> Command {
    [CMD_SYNC, 0]
}

/// The fault an event record reports, or [`None`] for a record that is none:
/// a page request, which no stream is allowed to make.
#[must_use]
pub fn fault(record: &[u64; EVENT_WORDS]) -> Option<Fault> {
    const READ: u64 = 1 << 35;
    let kind = (record[0] & 0xFF) as u16;
    let stream = (record[0] >> 32) as u32;
    let (reason, addressed) = match kind {
        0x02 | 0x04 | 0x06 => (FaultReason::Blocked, false),
        // A translation request, refused as ATS is off.
        0x05 => (FaultReason::Blocked, true),
        0x07 => (FaultReason::Translated, true),
        0x10 | 0x11 => (FaultReason::Unmapped, true),
        0x12 | 0x13 => (FaultReason::Denied, true),
        0x03 | 0x08 | 0x09 | 0x0A | 0x21 => (FaultReason::Malformed, false),
        // A transaction the unit does not support, at its address.
        0x01 => (FaultReason::Malformed, true),
        // A table walk the memory system aborted, or a TLB conflict.
        0x0B | 0x20 => (FaultReason::Other(kind), true),
        0x24 => return None,
        other => (FaultReason::Other(other), false),
    };
    Some(Fault {
        stream,
        iova: if addressed {
            record[2] & !(IO_PAGE_SIZE - 1)
        } else {
            0
        },
        write: addressed && record[1] & READ == 0,
        reason,
    })
}

/// The VMSAv8-64 entries of one stage at a 4 KiB granule, mapping memory
/// write-back and inner shareable.
///
/// The engine counts levels from the leaf (0 maps 4 KiB) where Arm counts
/// from the root (level 3 maps 4 KiB), and Arm marks a level-3 page as it
/// marks a table above it, so an entry is read by the level it sits at.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ArmTables {
    /// The stage.
    pub stage: Stage,
    /// `log2` of the tables a stage 2 walk's first level concatenates.
    pub root_order: u32,
}

const DESCRIPTOR_VALID: u64 = 0b01;
const DESCRIPTOR_TABLE: u64 = 0b11;
const ACCESS_FLAG: u64 = 1 << 10;
const SHAREABILITY_SHIFT: u32 = 8;
/// Stage 1 `AP[1]`, stage 2 `S2AP[0]`: readable (stage 2), or reachable
/// unprivileged (stage 1).
const AP_LOW: u64 = 1 << 6;
/// Stage 1 `AP[2]`, stage 2 `S2AP[1]`: read-only (stage 1), or writable
/// (stage 2).
const AP_HIGH: u64 = 1 << 7;
const NOT_GLOBAL: u64 = 1 << 11;
/// Stage 1 `PXN` and `UXN`: a device fetches no code, privileged or not.
const STAGE1_EXECUTE_NEVER: u64 = 0b11 << 53;
/// Stage 2 `XN[1:0] = 0b10`: no fetch at any privilege, where `0b11` would
/// let a privileged one through on a unit with `XNX`.
const STAGE2_EXECUTE_NEVER: u64 = 1 << 54;

impl ArmTables {
    fn attributes(self, access: Access) -> u64 {
        let shared = u64::from(INNER_SHAREABLE) << SHAREABILITY_SHIFT;
        match self.stage {
            Stage::Second => {
                let read = if access.read() { AP_LOW } else { 0 };
                let write = if access.write() { AP_HIGH } else { 0 };
                NORMAL_WRITE_BACK << 2 | read | write | shared | ACCESS_FLAG | STAGE2_EXECUTE_NEVER
            }
            Stage::First => {
                // `MAIR` attribute 0, in `AttrIndx` at bits 4:2, is zero.
                let read_only = if access.write() { 0 } else { AP_HIGH };
                AP_LOW | read_only | shared | ACCESS_FLAG | NOT_GLOBAL | STAGE1_EXECUTE_NEVER
            }
        }
    }

    fn access(self, entry: u64) -> Access {
        match self.stage {
            Stage::Second => match (entry & AP_LOW != 0, entry & AP_HIGH != 0) {
                (true, true) => Access::READ_WRITE,
                (false, true) => Access::WRITE,
                _ => Access::READ,
            },
            Stage::First if entry & AP_HIGH != 0 => Access::READ,
            Stage::First => Access::READ_WRITE,
        }
    }
}

impl PteFormat for ArmTables {
    /// A 4 KiB page, a 2 MiB block and a 1 GiB block; nothing at level 0.
    fn leaf_allowed(&self, level: u32) -> bool {
        level <= 2
    }

    fn table(&self, phys: u64, _level: u32) -> u64 {
        (phys & ADDRESS_47_12) | DESCRIPTOR_TABLE
    }

    fn leaf(&self, phys: u64, level: u32, access: Access) -> u64 {
        let kind = if level == 0 {
            DESCRIPTOR_TABLE
        } else {
            DESCRIPTOR_VALID
        };
        (phys & ADDRESS_47_12) | kind | self.attributes(access)
    }

    fn decode(&self, entry: u64, level: u32) -> Pte {
        let leaf = if level == 0 {
            DESCRIPTOR_TABLE
        } else {
            DESCRIPTOR_VALID
        };
        match entry & DESCRIPTOR_TABLE {
            kind if kind == leaf => Pte::Leaf(entry & ADDRESS_47_12, self.access(entry)),
            DESCRIPTOR_TABLE => Pte::Table(entry & ADDRESS_47_12),
            _ => Pte::Absent,
        }
    }

    fn root_order(&self) -> u32 {
        self.root_order
    }
}
