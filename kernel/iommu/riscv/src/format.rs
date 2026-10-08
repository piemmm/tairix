//! What a RISC-V IOMMU reads in memory (The RISC-V IOMMU Architecture
//! Specification, version 1.0, chapters 2 to 4): device contexts, the device
//! directory's non-leaf entries, commands, fault records, and the page-table
//! entries of either stage.

use tairix_kernel_iommu_api::{
    Access, Command, FaultReason, Pte, PteFormat, Stage, IO_PAGE_SHIFT, IO_PAGE_SIZE,
};

use crate::regs::PPN_SHIFT;

/// Words of a device context in the extended format; a unit without MSI
/// translation reads the base format's first four.
pub const CONTEXT_WORDS: usize = 8;

/// Words of one fault record.
pub const FAULT_WORDS: usize = 4;

/// One device context.
pub type Context = [u64; CONTEXT_WORDS];

const VALID: u64 = 1 << 0;
/// `tc.DTF`: faults met translating the device's requests are not recorded.
const DISABLE_TRANSLATION_FAULTS: u64 = 1 << 4;
const MODE_SHIFT: u32 = 60;
const GSCID_SHIFT: u32 = 44;
const PSCID_SHIFT: u32 = 12;

/// Bits of the id a second-stage context tags its translations with.
pub const GSCID_BITS: u32 = 16;
/// Bits of the id a first-stage context tags its translations with.
pub const PSCID_BITS: u32 = 20;

/// A context translating through `stage`'s tables at `root`, walked in
/// `mode` and tagged `id`; `silent`, its translation faults go unrecorded.
/// [`None`] for an id wider than the stage's tag, which would spill into the
/// fields beside it.
#[must_use]
pub const fn context(stage: Stage, id: u32, root: u64, mode: u64, silent: bool) -> Option<Context> {
    let tc = VALID
        | if silent {
            DISABLE_TRANSLATION_FAULTS
        } else {
            0
        };
    let atp = mode << MODE_SHIFT | root >> IO_PAGE_SHIFT;
    let id = id as u64;
    match stage {
        Stage::Second if id >> GSCID_BITS == 0 => {
            Some([tc, atp | id << GSCID_SHIFT, 0, 0, 0, 0, 0, 0])
        }
        Stage::First if id >> PSCID_BITS == 0 => Some([tc, 0, id << PSCID_SHIFT, atp, 0, 0, 0, 0]),
        _ => None,
    }
}

/// `msiptp.MODE`: a flat MSI page table, indexed by interrupt file number.
const MSI_FLAT: u64 = 1;
/// `msipte.M`: memory-resident interrupt file mode.
const MRIF_MODE: u64 = 1 << 1;

/// The largest identity an interrupt file holds.
pub const MAX_IDENTITY: u32 = 2047;

/// `context` with its messages written to the page at `doorbell` confined
/// through the one-entry MSI page table at `table`: `msiptp`, then a
/// `msi_addr_mask` of no bits, so the doorbell's page alone is recognised.
#[must_use]
pub const fn with_messages(mut context: Context, table: u64, doorbell: u64) -> Context {
    context[4] = MSI_FLAT << MODE_SHIFT | table >> IO_PAGE_SHIFT;
    context[5] = 0;
    context[6] = doorbell >> IO_PAGE_SHIFT;
    context
}

/// The MSI page-table entry setting the identity a message writes in the
/// 512-byte file at `file`, the unit then writing `notice` to the page at
/// `page`.
#[must_use]
pub const fn mrif_entry(file: u64, page: u64, notice: u32) -> [u64; 2] {
    let notice = notice as u64;
    [
        VALID | MRIF_MODE | (file >> 9) << 7,
        (page >> IO_PAGE_SHIFT) << 10 | notice & 0x3FF | (notice >> 10 & 1) << 60,
    ]
}

/// Whether `context` is valid.
#[must_use]
pub const fn is_valid(context: &Context) -> bool {
    context[0] & VALID != 0
}

/// The non-leaf directory entry leading to the table at `table`.
#[must_use]
pub const fn directory_entry(table: u64) -> u64 {
    VALID | (table >> IO_PAGE_SHIFT) << PPN_SHIFT
}

const IOTINVAL: u64 = 1;
const IOFENCE: u64 = 2;
const IODIR: u64 = 3;
/// `IOTINVAL.GVMA`, where `IOTINVAL.VMA` is function 0.
const SECOND_STAGE: u64 = 1 << 7;
const PSCID_VALID: u64 = 1 << 32;
const GSCID_VALID: u64 = 1 << 33;
const DEVICE_VALID: u64 = 1 << 33;
const DEVICE_SHIFT: u32 = 40;
/// `IOFENCE.C.AV`: `DATA` is stored at `ADDR` once the fence completes.
const FENCE_STORE: u64 = 1 << 10;
/// `IOFENCE.C.PR` and `PW`: the devices' earlier reads and writes complete
/// first.
const FENCE_REQUESTS: u64 = 0b11 << 12;

/// `IOTINVAL.AV`: only the page at `ADDR` is forgotten.
const ADDRESS_VALID: u64 = 1 << 10;

/// `ADDR`'s place in an `IOTINVAL`'s second word: the page number from bit 10.
const fn page_operand(address: u64) -> u64 {
    (address >> IO_PAGE_SHIFT) << 10
}

/// Forget the second-stage leaf cached for the page at `gpa` under `gscid`.
#[must_use]
pub const fn forget_second_stage_page(gscid: u16, gpa: u64) -> Command {
    [
        IOTINVAL | SECOND_STAGE | ADDRESS_VALID | GSCID_VALID | (gscid as u64) << GSCID_SHIFT,
        page_operand(gpa),
    ]
}

/// Forget the first-stage leaf cached for the page at `iova` under `pscid`.
#[must_use]
pub const fn forget_first_stage_page(pscid: u32, iova: u64) -> Command {
    [
        IOTINVAL | ADDRESS_VALID | PSCID_VALID | (pscid as u64) << PSCID_SHIFT,
        page_operand(iova),
    ]
}

/// Forget the second-stage translations cached for `gscid`, or for every one.
#[must_use]
pub const fn forget_second_stage(gscid: Option<u16>) -> Command {
    match gscid {
        Some(gscid) => [
            IOTINVAL | SECOND_STAGE | GSCID_VALID | (gscid as u64) << GSCID_SHIFT,
            0,
        ],
        None => [IOTINVAL | SECOND_STAGE, 0],
    }
}

/// Forget the first-stage translations cached for `pscid`, or for every one,
/// of contexts with no second stage.
#[must_use]
pub const fn forget_first_stage(pscid: Option<u32>) -> Command {
    match pscid {
        Some(pscid) => [IOTINVAL | PSCID_VALID | (pscid as u64) << PSCID_SHIFT, 0],
        None => [IOTINVAL, 0],
    }
}

/// Forget the context cached for `device`, or every directory entry cached.
/// A device's forgets its leaf entry alone: the entries above it are linked
/// once and never change.
#[must_use]
pub const fn forget_context(device: Option<u32>) -> Command {
    match device {
        Some(device) => [IODIR | DEVICE_VALID | (device as u64) << DEVICE_SHIFT, 0],
        None => [IODIR, 0],
    }
}

/// The fence a batch ends with: once every command before it is done and the
/// devices' earlier requests are complete, `token` is stored at `status`.
#[must_use]
pub const fn fence(token: u32, status: u64) -> Command {
    [
        IOFENCE | FENCE_STORE | FENCE_REQUESTS | (token as u64) << 32,
        status >> 2,
    ]
}

/// A fence that stores nothing: what a rejected command is replaced by.
#[must_use]
pub const fn fence_quietly() -> Command {
    [IOFENCE, 0]
}

/// Why a fault record's request was refused.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Cause {
    /// A reason, and whether the record names the page.
    Known(FaultReason, bool),
    /// A page fault, which the format does not split into nothing mapped and
    /// a mapping not allowing the access: the domain's tables tell them apart.
    Page,
}

/// One decoded fault record.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Record {
    /// Why.
    pub cause: Cause,
    /// The device id the request carried.
    pub device: u32,
    /// The page it fell in.
    pub iova: u64,
    /// Whether it was a write.
    pub write: bool,
}

/// Decode a fault record.
#[must_use]
pub const fn record(words: &[u64; FAULT_WORDS]) -> Record {
    let cause = (words[0] & 0xFFF) as u16;
    let kind = (words[0] >> 34) & 0x3F;
    // Transaction types 5 to 7 carry an address the device already
    // translated; 8 asks for a translation.
    let translated = kind >= 5 && kind <= 7;
    let cause = match cause {
        12 | 13 | 15 | 20 | 21 | 23 => Cause::Page,
        260 if translated => Cause::Known(FaultReason::Translated, true),
        // A device id wider than the directory reaches is disallowed too.
        256 | 258 | 260 => Cause::Known(FaultReason::Blocked, false),
        257 | 259 | 261..=274 => Cause::Known(FaultReason::Malformed, false),
        0 | 1 | 4..=7 => Cause::Known(FaultReason::Other(cause), true),
        other => Cause::Known(FaultReason::Other(other), false),
    };
    Record {
        cause,
        device: (words[0] >> DEVICE_SHIFT) as u32,
        iova: words[2] & !(IO_PAGE_SIZE - 1),
        write: kind == 3 || kind == 7,
    }
}

const READ: u64 = 1 << 1;
const WRITE: u64 = 1 << 2;
const EXECUTE: u64 = 1 << 3;
const USER: u64 = 1 << 4;
const ACCESSED: u64 = 1 << 6;
const DIRTY: u64 = 1 << 7;
const PPN: u64 = ((1 << 44) - 1) << PPN_SHIFT;

/// The `Sv39`/`Sv48`/`Sv57` entries of one stage at a 4 KiB granule, the
/// second stage's root resolving two bits more in 16 KiB. A leaf is reachable
/// unprivileged and already accessed, and dirty where writable, so a unit
/// that does not update them never faults on them. A device writing what it
/// may not read has no encoding.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct RiscvTables {
    /// The stage.
    pub stage: Stage,
}

const fn ppn(phys: u64) -> u64 {
    (phys >> IO_PAGE_SHIFT) << PPN_SHIFT & PPN
}

impl PteFormat for RiscvTables {
    fn leaf_allowed(&self, level: u32) -> bool {
        level <= 2
    }

    fn table(&self, phys: u64, _level: u32) -> u64 {
        VALID | ppn(phys)
    }

    fn leaf(&self, phys: u64, _level: u32, access: Access) -> u64 {
        let read = if access.read() { READ } else { 0 };
        let write = if access.write() { WRITE | DIRTY } else { 0 };
        VALID | USER | ACCESSED | read | write | ppn(phys)
    }

    fn decode(&self, entry: u64, level: u32) -> Pte {
        if entry & VALID == 0 {
            return Pte::Absent;
        }
        let phys = (entry & PPN) >> PPN_SHIFT << IO_PAGE_SHIFT;
        if entry & (READ | WRITE | EXECUTE) == 0 {
            // A pointer at the last level is a fault, not a table.
            return if level == 0 {
                Pte::Absent
            } else {
                Pte::Table(phys)
            };
        }
        let access = match (entry & READ != 0, entry & WRITE != 0) {
            (true, true) => Access::READ_WRITE,
            (true, false) => Access::READ,
            // Writable unreadable is reserved, and execute-only reads nothing:
            // the unit faults on either.
            (false, _) => return Pte::Absent,
        };
        Pte::Leaf(phys, access)
    }

    fn root_order(&self) -> u32 {
        match self.stage {
            Stage::First => 0,
            Stage::Second => 2,
        }
    }
}
