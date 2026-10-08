//! The registers an Arm `SMMUv3` family drives (Arm IHI 0070 §6.3): offsets,
//! control bits, and the identification fields a unit is judged by.

/// Identification register 0: the stages, formats and features.
pub const IDR0: usize = 0x0000;
/// Identification register 1: the table and queue sizes.
pub const IDR1: usize = 0x0004;
/// Identification register 5: the output size and the granules.
pub const IDR5: usize = 0x0014;
/// Global control: the unit and its queues.
pub const CR0: usize = 0x0020;
/// What [`CR0`] last took effect as.
pub const CR0ACK: usize = 0x0024;
/// The memory attributes the unit walks its tables and queues with.
pub const CR1: usize = 0x0028;
/// Further control.
pub const CR2: usize = 0x002C;
/// What the unit does with a transaction while it is disabled.
pub const GBPA: usize = 0x0044;
/// Which of the unit's interrupts are raised.
pub const IRQ_CTRL: usize = 0x0050;
/// What [`IRQ_CTRL`] last took effect as.
pub const IRQ_CTRLACK: usize = 0x0054;
/// Global errors: a bit differing from [`GERRORN`]'s is active.
pub const GERROR: usize = 0x0060;
/// Global errors acknowledged.
pub const GERRORN: usize = 0x0064;
/// The message the global-error interrupt is raised as.
pub const GERROR_IRQ_CFG0: usize = 0x0068;
/// Bits of message address an interrupt configuration holds.
pub const IRQ_ADDRESS_BITS: u32 = 52;
/// The global-error message's data.
pub const GERROR_IRQ_CFG1: usize = 0x0070;
/// The global-error message's memory attributes.
pub const GERROR_IRQ_CFG2: usize = 0x0074;
/// The stream table's base.
pub const STRTAB_BASE: usize = 0x0080;
/// The stream table's format and size.
pub const STRTAB_BASE_CFG: usize = 0x0088;
/// The command queue's base and size.
pub const CMDQ_BASE: usize = 0x0090;
/// The command queue's producer index.
pub const CMDQ_PROD: usize = 0x0098;
/// The command queue's consumer index and error.
pub const CMDQ_CONS: usize = 0x009C;
/// The event queue's base and size.
pub const EVENTQ_BASE: usize = 0x00A0;
/// The message the event-queue interrupt is raised as.
pub const EVENTQ_IRQ_CFG0: usize = 0x00B0;
/// The event-queue message's data.
pub const EVENTQ_IRQ_CFG1: usize = 0x00B8;
/// The event-queue message's memory attributes.
pub const EVENTQ_IRQ_CFG2: usize = 0x00BC;
/// The second register page, which holds the event queue's indices.
const PAGE1: usize = 0x1_0000;
/// The event queue's producer index and overflow flag.
pub const EVENTQ_PROD: usize = PAGE1 + 0x00A8;
/// The event queue's consumer index and overflow acknowledgement.
pub const EVENTQ_CONS: usize = PAGE1 + 0x00AC;
/// The register window: both pages.
pub const WINDOW: usize = 2 * PAGE1;

/// [`CR0`]: translation is on.
pub const CR0_SMMUEN: u32 = 1 << 0;
/// [`CR0`]: the event queue is written.
pub const CR0_EVENTQEN: u32 = 1 << 2;
/// [`CR0`]: the command queue is consumed.
pub const CR0_CMDQEN: u32 = 1 << 3;

/// [`CR2`]: a transaction from a stream past the table records an event.
pub const CR2_RECINVSID: u32 = 1 << 1;
/// [`CR2`]: broadcast TLB maintenance from the CPUs is ignored.
pub const CR2_PTM: u32 = 1 << 2;

/// [`GBPA`]: a write here takes effect, and reads set until it has.
pub const GBPA_UPDATE: u32 = 1 << 31;
/// [`GBPA`]: a transaction arriving while the unit is disabled is aborted.
pub const GBPA_ABORT: u32 = 1 << 20;

/// [`IRQ_CTRL`]: the global-error interrupt.
pub const IRQ_GERROR: u32 = 1 << 0;
/// [`IRQ_CTRL`]: the event-queue interrupt.
pub const IRQ_EVENTQ: u32 = 1 << 2;

/// [`GERROR`]: the command queue stopped on a command it rejected.
pub const GERROR_CMDQ: u32 = 1 << 0;
/// [`GERROR`]: an event-queue write was aborted.
pub const GERROR_EVENTQ_ABT: u32 = 1 << 2;
/// [`GERROR`]: a `CMD_SYNC`'s completion message was aborted.
pub const GERROR_MSI_CMDQ_ABT: u32 = 1 << 4;
/// [`GERROR`]: the unit entered service-failure mode and translates nothing.
pub const GERROR_SFM: u32 = 1 << 8;

/// A base register's read- or write-allocate hint.
pub const BASE_ALLOCATE: u64 = 1 << 62;
/// [`STRTAB_BASE_CFG`]: the table is two-level.
pub const STRTAB_TWO_LEVEL: u32 = 1 << 16;
/// [`STRTAB_BASE_CFG`]: the shift of the split between the levels.
pub const STRTAB_SPLIT_SHIFT: u32 = 6;

/// [`EVENTQ_PROD`]: the queue overflowed; [`EVENTQ_CONS`]: that overflow is
/// acknowledged.
pub const EVENTQ_OVERFLOW: u32 = 1 << 31;

/// Device-nGnRE: the memory attribute of a message to an interrupt
/// controller.
pub const MSI_DEVICE: u32 = 0x1;

/// [`IDR0`].
#[derive(Copy, Clone, Debug)]
pub struct Idr0(pub u32);

impl Idr0 {
    /// Stage 2 translation.
    #[must_use]
    pub const fn stage2(self) -> bool {
        self.0 & 1 != 0
    }

    /// Stage 1 translation.
    #[must_use]
    pub const fn stage1(self) -> bool {
        self.0 & (1 << 1) != 0
    }

    /// AArch64 translation tables.
    #[must_use]
    pub const fn aarch64_tables(self) -> bool {
        (self.0 >> 2) & 0b11 >= 0b10
    }

    /// Table and queue accesses are coherent with the CPUs' caches.
    #[must_use]
    pub const fn coherent(self) -> bool {
        self.0 & (1 << 4) != 0
    }

    /// Sixteen-bit ASIDs.
    #[must_use]
    pub const fn asid16(self) -> bool {
        self.0 & (1 << 12) != 0
    }

    /// Message-signalled interrupts, `CMD_SYNC` completions among them.
    #[must_use]
    pub const fn msi(self) -> bool {
        self.0 & (1 << 13) != 0
    }

    /// Sixteen-bit VMIDs.
    #[must_use]
    pub const fn vmid16(self) -> bool {
        self.0 & (1 << 18) != 0
    }

    /// Little-endian table walks.
    #[must_use]
    pub const fn little_endian(self) -> bool {
        matches!((self.0 >> 21) & 0b11, 0b00 | 0b10)
    }

    /// A fault may stall rather than abort; `0b10` forces it to.
    #[must_use]
    pub const fn stall_model(self) -> u32 {
        (self.0 >> 24) & 0b11
    }

    /// Two-level stream tables.
    #[must_use]
    pub const fn two_level(self) -> bool {
        (self.0 >> 27) & 0b11 == 0b01
    }
}

/// [`IDR1`].
#[derive(Copy, Clone, Debug)]
pub struct Idr1(pub u32);

impl Idr1 {
    /// Bits of stream id.
    #[must_use]
    pub const fn stream_bits(self) -> u32 {
        self.0 & 0x3F
    }

    /// `log2` of the most events the event queue can hold.
    #[must_use]
    pub const fn eventq_bits(self) -> u32 {
        (self.0 >> 16) & 0x1F
    }

    /// `log2` of the most commands the command queue can hold.
    #[must_use]
    pub const fn cmdq_bits(self) -> u32 {
        (self.0 >> 21) & 0x1F
    }

    /// The queue or table bases are fixed by the implementation.
    #[must_use]
    pub const fn preset(self) -> bool {
        self.0 & (0b11 << 29) != 0
    }
}

/// [`IDR5`].
#[derive(Copy, Clone, Debug)]
pub struct Idr5(pub u32);

/// The output sizes, in bits, by their encoding.
const OUTPUT_SIZES: [u32; 8] = [32, 36, 40, 42, 44, 48, 52, 56];

impl Idr5 {
    /// The widest output size the unit has that is no wider than `widest`
    /// bits: its encoding, which the tables' `PS` fields take, and its bits.
    #[must_use]
    pub fn output(self, widest: u32) -> Option<(u32, u32)> {
        let unit = (self.0 & 0b111) as usize;
        OUTPUT_SIZES[..=unit]
            .iter()
            .zip(0..8)
            .rev()
            .find(|&(&bits, _)| bits <= widest)
            .map(|(&bits, encoding)| (encoding, bits))
    }

    /// The 4 KiB translation granule.
    #[must_use]
    pub const fn granule_4k(self) -> bool {
        self.0 & (1 << 4) != 0
    }
}
