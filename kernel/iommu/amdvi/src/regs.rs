//! The AMD-Vi register set.
//!
//! Offsets and fields are those of the AMD I/O Virtualization Technology
//! (IOMMU) Specification rev. 3.08 §3.4.

pub(crate) const DEVICE_TABLE: usize = 0x0000;
pub(crate) const COMMAND_BUFFER: usize = 0x0008;
pub(crate) const EVENT_LOG: usize = 0x0010;
pub(crate) const CONTROL: usize = 0x0018;
pub(crate) const EXCLUSION_BASE: usize = 0x0020;
pub(crate) const EXCLUSION_LIMIT: usize = 0x0028;
pub(crate) const FEATURES: usize = 0x0030;
pub(crate) const COMMAND_HEAD: usize = 0x2000;
pub(crate) const COMMAND_TAIL: usize = 0x2008;
pub(crate) const EVENT_HEAD: usize = 0x2010;
pub(crate) const EVENT_TAIL: usize = 0x2018;
pub(crate) const STATUS: usize = 0x2020;

/// Bytes of register set the family reaches.
pub(crate) const WINDOW: usize = STATUS + 8;

pub(crate) const CONTROL_IOMMU: u64 = 1 << 0;
pub(crate) const CONTROL_EVENT_LOG: u64 = 1 << 2;
pub(crate) const CONTROL_EVENT_INTERRUPT: u64 = 1 << 3;
/// One second, the invalidation timeout Linux sets.
pub(crate) const CONTROL_TIMEOUT_1S: u64 = 0b100 << 5;
pub(crate) const CONTROL_COHERENT: u64 = 1 << 10;
pub(crate) const CONTROL_COMMANDS: u64 = 1 << 12;
/// 128-bit remapping entries.
pub(crate) const CONTROL_GUEST_APIC: u64 = 1 << 17;
/// 32-bit destinations in them.
pub(crate) const CONTROL_X2APIC: u64 = 1 << 50;

pub(crate) const STATUS_EVENT_OVERFLOW: u64 = 1 << 0;
pub(crate) const STATUS_EVENT_INTERRUPT: u64 = 1 << 1;
pub(crate) const STATUS_COMPLETION_INTERRUPT: u64 = 1 << 2;
pub(crate) const STATUS_EVENT_LOG_RUNNING: u64 = 1 << 3;
pub(crate) const STATUS_COMMANDS_RUNNING: u64 = 1 << 4;
/// The status bits software clears by writing one.
pub(crate) const STATUS_CLEAR: u64 =
    STATUS_EVENT_OVERFLOW | STATUS_EVENT_INTERRUPT | STATUS_COMPLETION_INTERRUPT;

/// A ring base register naming `slots` sixteen-byte entries at `phys`.
pub(crate) fn ring(phys: u64, slots: usize) -> u64 {
    phys | (u64::from(slots.trailing_zeros()) << 56)
}

/// The device table base register naming `frames` frames at `phys`.
pub(crate) fn device_table(phys: u64, frames: usize) -> u64 {
    phys | (frames as u64 - 1)
}

/// The ring slot a head or tail register names: a byte offset in bits 18:4.
pub(crate) fn ring_index(register: u64, slots: usize) -> usize {
    ((register >> 4) & 0x7FFF) as usize % slots
}

/// The head or tail register value naming ring slot `slot`.
pub(crate) fn ring_offset(slot: usize) -> u64 {
    (slot as u64) << 4
}

/// Extended feature register fields.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Features(pub u64);

impl Features {
    /// 32-bit x2APIC destinations: XT, through 128-bit guest-APIC entries.
    pub fn extended_interrupts(self) -> bool {
        self.0 & (1 << 2) != 0 && self.0 & (1 << 7) != 0
    }

    /// `INVALIDATE_IOMMU_ALL`.
    pub fn invalidate_all(self) -> bool {
        self.0 & (1 << 6) != 0
    }

    /// Host translation, which every unit that has it walks at least four
    /// levels deep.
    pub fn host_translation(self) -> bool {
        (self.0 >> 10) & 0b11 != 0b11
    }
}
