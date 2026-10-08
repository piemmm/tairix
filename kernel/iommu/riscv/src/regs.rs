//! The registers a RISC-V IOMMU family drives (The RISC-V IOMMU Architecture
//! Specification, version 1.0, chapter 6): offsets, control bits, and the
//! capability fields a unit is judged by.

/// What the unit implements.
pub const CAPABILITIES: usize = 0x000;
/// Features software controls: byte order, wired interrupts.
pub const FCTL: usize = 0x008;
/// The device directory's root and mode.
pub const DDTP: usize = 0x010;
/// The command queue's base and size.
pub const CQB: usize = 0x018;
/// The command queue's head, which the unit advances.
pub const CQH: usize = 0x020;
/// The command queue's tail, which software advances.
pub const CQT: usize = 0x024;
/// The fault queue's base and size.
pub const FQB: usize = 0x028;
/// The fault queue's head, which software advances.
pub const FQH: usize = 0x030;
/// The fault queue's tail, which the unit advances.
pub const FQT: usize = 0x034;
/// The page-request queue's control.
pub const PQCSR: usize = 0x050;
/// Which performance counters are stopped, one bit each; an absent counter's
/// bit, and the whole register on a unit with none, ignores a write.
pub const IOCOUNTINH: usize = 0x05C;
/// The command queue's control and status.
pub const CQCSR: usize = 0x048;
/// The fault queue's control and status.
pub const FQCSR: usize = 0x04C;
/// Interrupts pending, one bit per cause, written one to clear.
pub const IPSR: usize = 0x054;
/// Which interrupt vector each cause raises.
pub const ICVEC: usize = 0x2F8;
/// The first of the message-signalled vectors' address, data and control.
pub const MSI_CFG_TBL: usize = 0x300;
/// The register window.
pub const WINDOW: usize = 0x1000;

/// Where [`DDTP`], [`CQB`] and [`FQB`] hold their page number.
pub const PPN_SHIFT: u32 = 10;

/// [`FCTL`]: the unit accesses memory big-endian.
pub const FCTL_BE: u32 = 1 << 0;
/// [`FCTL`]: interrupts are wired rather than message-signalled.
pub const FCTL_WSI: u32 = 1 << 1;
/// [`FCTL`]: guest physical addresses are translated as a 32-bit guest's.
pub const FCTL_GXL: u32 = 1 << 2;

/// [`DDTP`]: no inbound transaction is allowed.
pub const DDTP_OFF: u64 = 0;
/// [`DDTP`]: the directory depths it takes, one to three levels, each with
/// the mode naming it.
pub const DDTP_DEPTHS: [(u32, u64); 3] = [(1, 2), (2, 3), (3, 4)];
/// [`DDTP`]: the mode field.
pub const DDTP_MODE: u64 = 0xF;
/// [`DDTP`]: a write is still taking effect.
pub const DDTP_BUSY: u64 = 1 << 4;

/// A queue control register: the queue runs.
pub const QUEUE_EN: u32 = 1 << 0;
/// A queue control register: it raises its interrupt.
pub const QUEUE_IE: u32 = 1 << 1;
/// A queue control register: the unit faulted on the queue's memory.
pub const QUEUE_MF: u32 = 1 << 8;
/// [`CQCSR`]: a command timed out.
pub const CQCSR_CMD_TO: u32 = 1 << 9;
/// [`CQCSR`]: the unit rejected a command.
pub const CQCSR_CMD_ILL: u32 = 1 << 10;
/// [`CQCSR`]: a fence's wired interrupt is pending.
pub const CQCSR_FENCE_W_IP: u32 = 1 << 11;
/// [`CQCSR`]: every error and pending bit, each written one to clear.
pub const CQCSR_ERRORS: u32 = QUEUE_MF | CQCSR_CMD_TO | CQCSR_CMD_ILL | CQCSR_FENCE_W_IP;
/// [`FQCSR`]: a record was lost to a full queue.
pub const FQCSR_FQOF: u32 = 1 << 9;
/// [`FQCSR`] and [`PQCSR`], which share its layout: every error bit, each
/// written one to clear.
pub const FQCSR_ERRORS: u32 = QUEUE_MF | FQCSR_FQOF;
/// A queue control register: the queue is running.
pub const QUEUE_ON: u32 = 1 << 16;
/// A queue control register: a change is still taking effect.
pub const QUEUE_BUSY: u32 = 1 << 17;

/// [`IPSR`]: the fault queue's interrupt.
pub const IPSR_FIP: u32 = 1 << 1;
/// [`IPSR`]: every cause.
pub const IPSR_ALL: u32 = 0xF;

/// [`CAPABILITIES`].
#[derive(Copy, Clone, Debug)]
pub struct Capabilities(pub u64);

impl Capabilities {
    /// The specification version, major in the high nibble.
    #[must_use]
    pub const fn version(self) -> u64 {
        self.0 & 0xFF
    }

    /// Whether the second stage walks `Sv39x4`, `Sv48x4` and `Sv57x4`
    /// tables, in that order.
    #[must_use]
    pub const fn second_stage(self) -> [bool; 3] {
        [
            self.0 & (1 << 17) != 0,
            self.0 & (1 << 18) != 0,
            self.0 & (1 << 19) != 0,
        ]
    }

    /// Whether the first stage walks `Sv39`, `Sv48` and `Sv57` tables, in
    /// that order.
    #[must_use]
    pub const fn first_stage(self) -> [bool; 3] {
        [
            self.0 & (1 << 9) != 0,
            self.0 & (1 << 10) != 0,
            self.0 & (1 << 11) != 0,
        ]
    }

    /// Device contexts are the 64-byte extended format, which MSI
    /// translation needs.
    #[must_use]
    pub const fn extended_contexts(self) -> bool {
        self.0 & (1 << 22) != 0
    }

    /// A device's messages can be confined to a memory-resident interrupt
    /// file through an MSI page table, which needs the extended contexts and
    /// a second stage to translate through.
    #[must_use]
    pub const fn message_files(self) -> bool {
        let second = self.second_stage();
        self.extended_contexts() && self.0 & (1 << 23) != 0 && (second[0] || second[1] || second[2])
    }

    /// The unit sets an interrupt file's pending bit atomically.
    #[must_use]
    pub const fn atomic_files(self) -> bool {
        self.0 & (1 << 21) != 0
    }

    /// Interrupts can be message-signalled.
    #[must_use]
    pub const fn msi(self) -> bool {
        matches!((self.0 >> 28) & 0b11, 0b00 | 0b10)
    }

    /// Interrupts can be wired.
    #[must_use]
    pub const fn wired(self) -> bool {
        matches!((self.0 >> 28) & 0b11, 0b01 | 0b10)
    }

    /// Bits of physical address the unit can name.
    #[must_use]
    pub const fn physical_bits(self) -> u32 {
        ((self.0 >> 32) & 0x3F) as u32
    }
}
