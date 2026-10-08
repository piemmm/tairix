//! GICv3: the affinity-routed distributor, one redistributor per CPU, and
//! the system-register CPU interface (Arm IHI 0069H).
//!
//! Chosen at boot by discovery beside the GICv2 driver; [`crate::gic`] owns
//! the version-neutral entry points the rest of the port calls. An SGI or a
//! PPI lives in the calling CPU's redistributor and an SPI in the
//! distributor, at the same per-interrupt register offsets, so every
//! per-interrupt operation names the redistributor it would use.

use tairix_arch_api::{PageTableFrames, StuckInterrupt};

#[cfg(all(target_arch = "aarch64", target_os = "none"))]
use crate::gic::SPURIOUS_INTID;
use crate::gic::{
    first_stuck_spi, icenabler_offset, icfgr_edge_bit, icfgr_offset, igroupr_offset, isenabler_bit,
    isenabler_offset, GICD_CTLR, GICD_ICACTIVER, GICD_ICENABLER, GICD_IGROUPR, GICD_IPRIORITYR,
    GICD_TYPER, GICD_TYPER_ITLINES_MASK, MAX_INTID, MID_RANGE_PRIORITY, MIN_SPI_INTID,
};

const GICD_IGRPMODR: usize = 0x0D00;
const GICD_IROUTER: usize = 0x6000;
const PIDR2: usize = 0xFFE8;

/// `GICD_CTLR.EnableGrp0` in the single-Security-state view.
const GICD_CTLR_ENABLE_GRP0: u32 = 1 << 0;
/// `EnableGrp1` in the single-Security-state view, `EnableGrp1A` (Group 1
/// Non-secure) in the Non-secure view of a two-state distributor.
const GICD_CTLR_ENABLE_GRP1: u32 = 1 << 1;
/// `ARE`, or `ARE_NS` in the Non-secure view.
const GICD_CTLR_ARE: u32 = 1 << 4;
/// Reads one only where Group 0 is the kernel's to use.
const GICD_CTLR_DS: u32 = 1 << 6;
const GICD_CTLR_RWP: u32 = 1 << 31;
const GICD_TYPER_RSS: u32 = 1 << 26;
const GICD_TYPER_LPIS: u32 = 1 << 17;
const GICD_TYPER_IDBITS_SHIFT: u32 = 19;

const GICR_CTLR: usize = 0x0000;
const GICR_CTLR_ENABLE_LPIS: u32 = 1 << 0;
const GICR_CTLR_RWP: u32 = 1 << 3;
const GICR_TYPER: usize = 0x0008;
const GICR_TYPER_PLPIS: u64 = 1 << 0;
const GICR_TYPER_VLPIS: u64 = 1 << 1;
const GICR_TYPER_LAST: u64 = 1 << 4;
const GICR_TYPER_PROCESSOR_SHIFT: u32 = 8;
const GICR_PROPBASER: usize = 0x0070;
const GICR_PENDBASER: usize = 0x0078;
/// Writes as one that firmware or no earlier owner left the pending table
/// set; reads as zero.
const GICR_PENDBASER_PTZ: u64 = 1 << 62;

/// `GICR_PROPBASER` and `GICR_PENDBASER` inner cacheability: Normal memory,
/// write-back, read- and write-allocate.
const TABLE_INNER_WRITE_BACK: u64 = 0b111 << 7;
/// The same, non-cacheable.
const TABLE_INNER_NON_CACHEABLE: u64 = 0b001 << 7;
/// The shareability of an LPI table register, laid out alike in the
/// redistributor's and the ITS's.
pub(crate) const TABLE_SHAREABILITY: u64 = 0b11 << 10;
pub(crate) const TABLE_INNER_SHAREABLE: u64 = 0b01 << 10;

/// The first LPI's INTID.
pub const FIRST_LPI: u32 = 8192;

/// An LPI configuration table entry enabling its LPI at the mid-range
/// priority: the priority's top six bits, bit 1 reserved as one, bit 0 the
/// enable.
const LPI_ENABLED: u8 = (MID_RANGE_PRIORITY & 0xFC) | 0b10 | 0b1;
/// A pending table's alignment, as an order of frames.
const PENDING_TABLE_MIN_ORDER: u32 = 4;
const GICR_WAKER: usize = 0x0014;
const GICR_WAKER_PROCESSOR_SLEEP: u32 = 1 << 1;
const GICR_WAKER_CHILDREN_ASLEEP: u32 = 1 << 2;

/// The SGI and PPI registers, 64 KiB past each redistributor's control frame
/// and laid out as the distributor's first word of each bank.
const SGI_FRAME: usize = 0x1_0000;
/// A redistributor's control and SGI frames.
const GICV3_FRAMES: u64 = 0x2_0000;
/// The same with the two GICv4 virtual-LPI frames.
const GICV4_FRAMES: u64 = 0x4_0000;

const ICC_SRE_SRE: u64 = 1 << 0;
/// Disable the FIQ and IRQ bypass: only the interface signals interrupts.
const ICC_SRE_DFB: u64 = 1 << 1;
const ICC_SRE_DIB: u64 = 1 << 2;
const ICC_CTLR_EOIMODE: u64 = 1 << 1;
const ICC_CTLR_RSS: u64 = 1 << 18;

const INTID_MASK: u32 = 0x00FF_FFFF;
/// `1020..=1023` acknowledge nothing and take no end-of-interrupt.
const SPECIAL_INTIDS: core::ops::RangeInclusive<u32> = 1020..=1023;

/// The most iterations a register handshake is given before the controller
/// is declared unresponsive: generous, because each completes in a few
/// hundred cycles on working hardware.
const REGISTER_WAIT_SPINS: u32 = 1_000_000;

/// Why a GICv3 could not be brought up or asked something of.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Gicv3Error {
    /// The distributor or a redistributor identifies as neither GICv3 nor
    /// GICv4.
    NotGicv3,
    /// A register write never completed, or a redistributor would not wake.
    Unresponsive,
    /// A higher exception level keeps the system-register interface off.
    NoSystemRegisters,
    /// No redistributor in the described regions answers to the CPU.
    NoRedistributor,
    /// An SGI cannot name the CPU: its `Aff0` is above 15 and the controller
    /// has no range selectors.
    Unaddressable,
    /// The redistributor implements no physical LPIs.
    NoLpis,
    /// Firmware left the redistributor's LPIs on, pointed at tables the
    /// kernel cannot know.
    LpisInUse,
}

/// The LPI tables a redistributor is pointed at.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct LpiTables {
    /// The configuration table's physical address: a byte per LPI from
    /// [`FIRST_LPI`] up, shared by every redistributor.
    pub properties: u64,
    /// This redistributor's pending table: its physical address, 64 KiB
    /// aligned, its bits zero.
    pub pending: u64,
    /// The INTID bits both cover, at least 14.
    pub id_bits: u32,
}

impl LpiTables {
    /// Tables of `id_bits` INTID bits from `frames`, the first `enabled`
    /// LPIs enabled at the mid-range priority and the rest disabled, every
    /// byte at the point of coherency so a redistributor that does not snoop
    /// reads them. The tables are the redistributor's for the kernel's life.
    /// [`None`] for bits that cover no LPI or more than 32, `enabled` past
    /// them, or no memory.
    #[must_use]
    pub fn allocate(frames: &dyn PageTableFrames, id_bits: u32, enabled: u32) -> Option<Self> {
        let intids = 1u64.checked_shl(id_bits).filter(|_| id_bits <= 32)?;
        let lpis = intids
            .checked_sub(u64::from(FIRST_LPI))
            .filter(|&lpis| lpis > 0)?;
        if u64::from(enabled) > lpis {
            return None;
        }
        let property_order = crate::its::order_of(lpis);
        let properties = frames.alloc_block(property_order)?;
        let Some(entries) = frames.block_at(properties, property_order) else {
            frames.free_block(properties, property_order);
            return None;
        };
        let entries = entries.cast::<u8>();
        for index in 0..usize::try_from(enabled).ok()? {
            // SAFETY: `index` is below `lpis`, the bytes the block of
            // `property_order` frames holds, and nothing else holds the
            // fresh block yet.
            unsafe { entries.add(index).write_volatile(LPI_ENABLED) };
        }
        crate::paging::clean_range_to_poc(entries as u64, lpis);
        let pending_order = crate::its::order_of(intids / 8).max(PENDING_TABLE_MIN_ORDER);
        let Some(pending) = frames.alloc_block(pending_order) else {
            frames.free_block(properties, property_order);
            return None;
        };
        if let Some(bits) = frames.block_at(pending, pending_order) {
            crate::paging::clean_range_to_poc(bits as u64, intids / 8);
        }
        Some(Self {
            properties,
            pending,
            id_bits,
        })
    }
}

/// One redistributor region the firmware describes.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct RedistributorRegion {
    /// Where the first redistributor's frames start.
    pub base: u64,
    /// The region's length in bytes.
    pub len: u64,
}

/// A CPU's affinity as `MPIDR_EL1` and `GICD_IROUTER<n>` both lay it out:
/// `Aff3` in bits `[39:32]`, `Aff2`, `Aff1` and `Aff0` in bits `[23:0]`.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Affinity(u64);

impl Affinity {
    const MASK: u64 = 0xFF_00FF_FFFF;

    /// The affinity fields of an `MPIDR_EL1` reading.
    #[must_use]
    pub const fn of_mpidr(mpidr: u64) -> Self {
        Self(mpidr & Self::MASK)
    }

    /// The affinity a redistributor's `GICR_TYPER` names, which packs
    /// `Aff3.Aff2.Aff1.Aff0` into bits `[63:32]`.
    #[must_use]
    pub const fn of_redistributor(typer: u64) -> Self {
        let packed = typer >> 32;
        Self((packed & 0x00FF_FFFF) | ((packed >> 24) << 32))
    }

    /// The `GICD_IROUTER<n>` value routing to this CPU alone.
    #[must_use]
    pub const fn routing(self) -> u64 {
        self.0
    }

    const fn aff0(self) -> u64 {
        self.0 & 0xFF
    }

    const fn needs_range_selector(self) -> bool {
        self.aff0() > 0xF
    }

    /// The `ICC_SGI1R_EL1` value raising SGI `intid` on this CPU alone.
    #[must_use]
    pub fn sgi(self, intid: u32) -> u64 {
        let aff0 = self.aff0();
        (((self.0 >> 32) & 0xFF) << 48)
            | ((aff0 >> 4) << 44)
            | (((self.0 >> 16) & 0xFF) << 32)
            | (u64::from(intid & 0xF) << 24)
            | (((self.0 >> 8) & 0xFF) << 16)
            | (1 << (aff0 & 0xF))
    }
}

/// Volatile access to the distributor and to a redistributor's frames.
pub trait Gicv3Mmio {
    /// Read the 32-bit distributor register at `off`.
    fn distributor_read(&self, off: usize) -> u32;
    /// Write the 32-bit distributor register at `off`.
    fn distributor_write(&self, off: usize, value: u32);
    /// Write the 64-bit distributor register at `off`.
    fn distributor_write_u64(&self, off: usize, value: u64);
    /// Write the distributor byte register at `off`.
    fn distributor_write_byte(&self, off: usize, value: u8);
    /// Read the 32-bit register at `off` from the redistributor at `rd`.
    fn redistributor_read(&self, rd: usize, off: usize) -> u32;
    /// Read the 64-bit register at `off` from the redistributor at `rd`.
    fn redistributor_read_u64(&self, rd: usize, off: usize) -> u64;
    /// Write the 32-bit register at `off` of the redistributor at `rd`.
    fn redistributor_write(&self, rd: usize, off: usize, value: u32);
    /// Write the 64-bit register at `off` of the redistributor at `rd`.
    fn redistributor_write_u64(&self, rd: usize, off: usize, value: u64);
    /// Write the byte register at `off` of the redistributor at `rd`.
    fn redistributor_write_byte(&self, rd: usize, off: usize, value: u8);
}

/// The calling CPU's `ICC_*_EL1` system registers.
pub trait CpuInterface {
    /// `ICC_SRE_EL1`.
    fn sre(&self) -> u64;
    /// Write `ICC_SRE_EL1`.
    fn set_sre(&self, value: u64);
    /// Write `ICC_PMR_EL1`.
    fn set_priority_mask(&self, value: u64);
    /// Write `ICC_BPR1_EL1`.
    fn set_binary_point(&self, value: u64);
    /// `ICC_CTLR_EL1`.
    fn control(&self) -> u64;
    /// Write `ICC_CTLR_EL1`.
    fn set_control(&self, value: u64);
    /// `ICC_IGRPEN0_EL1`.
    fn group0_enable(&self) -> u64;
    /// Write `ICC_IGRPEN0_EL1`.
    fn set_group0_enable(&self, value: u64);
    /// Write `ICC_IGRPEN1_EL1`.
    fn set_group1_enable(&self, value: u64);
    /// Read `ICC_IAR1_EL1`, acknowledging the highest pending Group 1
    /// interrupt.
    fn acknowledge_group1(&self) -> u32;
    /// Write `ICC_EOIR1_EL1`.
    fn end_group1(&self, intid: u32);
    /// Read `ICC_IAR0_EL1`.
    fn acknowledge_group0(&self) -> u32;
    /// Write `ICC_EOIR0_EL1`.
    fn end_group0(&self, intid: u32);
    /// Write `ICC_SGI1R_EL1`, after this CPU's earlier stores are visible to
    /// the inner-shareable domain, so a woken CPU never reads a run queue
    /// older than the IPI.
    fn raise_group1_sgi(&self, value: u64);
    /// This CPU's `MPIDR_EL1`.
    fn mpidr(&self) -> u64;
    /// Complete the system-register writes above (`isb`).
    fn synchronize(&self);
}

/// What a fault-sample route changed, so a probe that found the route
/// undelivered can put it back.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct FiqRoute {
    distributor: u32,
    group0_enable: u64,
}

/// A GICv3 driver over its register seams. Policy — range checks, which
/// lines a caller may touch — is the caller's.
pub struct Gicv3<M, C> {
    mmio: M,
    cpu: C,
}

const fn revision_ok(pidr2: u32) -> bool {
    matches!((pidr2 >> 4) & 0xF, 3 | 4)
}

fn wait_until(mut done: impl FnMut() -> bool) -> Result<(), Gicv3Error> {
    for _ in 0..REGISTER_WAIT_SPINS {
        if done() {
            return Ok(());
        }
        core::hint::spin_loop();
    }
    Err(Gicv3Error::Unresponsive)
}

impl<M: Gicv3Mmio, C: CpuInterface> Gicv3<M, C> {
    /// Bind a driver to its register seams.
    pub const fn new(mmio: M, cpu: C) -> Self {
        Self { mmio, cpu }
    }

    /// The calling CPU's affinity.
    #[must_use]
    pub fn local_affinity(&self) -> Affinity {
        Affinity::of_mpidr(self.cpu.mpidr())
    }

    /// Whether the distributor presents a single Security state, in which
    /// Group 0 is signalled to this kernel as FIQ.
    #[must_use]
    pub fn single_security_state(&self) -> bool {
        self.mmio.distributor_read(GICD_CTLR) & GICD_CTLR_DS != 0
    }

    /// One past the highest SPI the distributor implements.
    fn line_count(&self) -> u32 {
        let words = (self.mmio.distributor_read(GICD_TYPER) & GICD_TYPER_ITLINES_MASK) + 1;
        (32 * words).min(MAX_INTID + 1)
    }

    fn wait_distributor(&self) -> Result<(), Gicv3Error> {
        wait_until(|| self.mmio.distributor_read(GICD_CTLR) & GICD_CTLR_RWP == 0)
    }

    fn wait_redistributor(&self, rd: usize) -> Result<(), Gicv3Error> {
        wait_until(|| self.mmio.redistributor_read(rd, GICR_CTLR) & GICR_CTLR_RWP == 0)
    }

    /// Reset every SPI to disabled, inactive, Group 1, mid priority, level
    /// and routed to `route_to`, then forward Group 1 with affinity routing
    /// on. Run once, on the boot CPU.
    ///
    /// # Errors
    ///
    /// [`Gicv3Error::NotGicv3`] for a distributor of another revision and
    /// [`Gicv3Error::Unresponsive`] for one that never completes a write.
    pub fn init_distributor(&self, route_to: Affinity) -> Result<(), Gicv3Error> {
        if !revision_ok(self.mmio.distributor_read(PIDR2)) {
            return Err(Gicv3Error::NotGicv3);
        }
        self.mmio.distributor_write(GICD_CTLR, 0);
        self.wait_distributor()?;
        let lines = self.line_count();
        for first in (MIN_SPI_INTID..lines).step_by(32) {
            let word = (first / 32) as usize * 4;
            self.mmio.distributor_write(GICD_ICENABLER + word, u32::MAX);
            self.mmio.distributor_write(GICD_ICACTIVER + word, u32::MAX);
            self.mmio.distributor_write(GICD_IGROUPR + word, u32::MAX);
            self.mmio.distributor_write(GICD_IGRPMODR + word, 0);
        }
        for first in (MIN_SPI_INTID..lines).step_by(16) {
            self.mmio.distributor_write(icfgr_offset(first), 0);
        }
        for intid in MIN_SPI_INTID..lines {
            self.mmio
                .distributor_write_byte(GICD_IPRIORITYR + intid as usize, MID_RANGE_PRIORITY);
        }
        self.wait_distributor()?;
        self.mmio.distributor_write(GICD_CTLR, GICD_CTLR_ARE);
        self.wait_distributor()?;
        // `GICD_IROUTER<n>` is RES0 until affinity routing is on.
        for intid in MIN_SPI_INTID..lines {
            self.route_spi(intid, route_to);
        }
        self.mmio
            .distributor_write(GICD_CTLR, GICD_CTLR_ARE | GICD_CTLR_ENABLE_GRP1);
        self.wait_distributor()
    }

    /// The redistributor among `regions` serving `affinity`, walking each
    /// region's frames `stride` apart, or the architecture's own spacing
    /// where the firmware names none.
    #[must_use]
    pub fn find_redistributor(
        &self,
        regions: &[RedistributorRegion],
        stride: Option<u64>,
        affinity: Affinity,
    ) -> Option<usize> {
        regions.iter().find_map(|region| {
            let end = region.base.checked_add(region.len)?;
            let mut frame = region.base;
            while frame
                .checked_add(GICV3_FRAMES)
                .is_some_and(|top| top <= end)
            {
                let rd = usize::try_from(frame).ok()?;
                if !revision_ok(self.mmio.redistributor_read(rd, PIDR2)) {
                    return None;
                }
                let typer = self.mmio.redistributor_read_u64(rd, GICR_TYPER);
                if Affinity::of_redistributor(typer) == affinity {
                    return Some(rd);
                }
                if typer & GICR_TYPER_LAST != 0 {
                    return None;
                }
                let step = stride.unwrap_or(if typer & GICR_TYPER_VLPIS != 0 {
                    GICV4_FRAMES
                } else {
                    GICV3_FRAMES
                });
                frame = frame.checked_add(step)?;
            }
            None
        })
    }

    /// Wake the redistributor at `rd` and reset its SGIs and PPIs to
    /// disabled, inactive, Group 1 and mid priority. Run on its own CPU.
    ///
    /// # Errors
    ///
    /// [`Gicv3Error::NotGicv3`] or [`Gicv3Error::Unresponsive`].
    pub fn init_redistributor(&self, rd: usize) -> Result<(), Gicv3Error> {
        if !revision_ok(self.mmio.redistributor_read(rd, PIDR2)) {
            return Err(Gicv3Error::NotGicv3);
        }
        let waker = self.mmio.redistributor_read(rd, GICR_WAKER);
        self.mmio
            .redistributor_write(rd, GICR_WAKER, waker & !GICR_WAKER_PROCESSOR_SLEEP);
        wait_until(|| {
            self.mmio.redistributor_read(rd, GICR_WAKER) & GICR_WAKER_CHILDREN_ASLEEP == 0
        })?;
        let private = |off: usize, value: u32| {
            self.mmio.redistributor_write(rd, SGI_FRAME + off, value);
        };
        private(GICD_ICENABLER, u32::MAX);
        private(GICD_ICACTIVER, u32::MAX);
        private(GICD_IGROUPR, u32::MAX);
        private(GICD_IGRPMODR, 0);
        for intid in 0..MIN_SPI_INTID {
            self.mmio.redistributor_write_byte(
                rd,
                SGI_FRAME + GICD_IPRIORITYR + intid as usize,
                MID_RANGE_PRIORITY,
            );
        }
        self.wait_redistributor(rd)
    }

    /// The INTID bits the distributor supports where it implements LPIs;
    /// [`None`] where it implements none.
    #[must_use]
    pub fn lpi_id_bits(&self) -> Option<u32> {
        let typer = self.mmio.distributor_read(GICD_TYPER);
        (typer & GICD_TYPER_LPIS != 0).then_some(((typer >> GICD_TYPER_IDBITS_SHIFT) & 0x1F) + 1)
    }

    /// Whether the redistributor at `rd` can take physical LPIs now: it
    /// implements them and firmware did not leave them on.
    #[must_use]
    pub fn lpis_available(&self, rd: usize) -> bool {
        self.mmio.redistributor_read_u64(rd, GICR_TYPER) & GICR_TYPER_PLPIS != 0
            && self.mmio.redistributor_read(rd, GICR_CTLR) & GICR_CTLR_ENABLE_LPIS == 0
    }

    /// The redistributor at `rd` as an interrupt translation service names
    /// it in a collection: its physical base where the service takes
    /// addresses, else its processor number, in the layout of the commands'
    /// target field (bits 16 and up).
    #[must_use]
    pub fn collection_target(&self, rd: usize, physical: bool) -> u64 {
        if physical {
            rd as u64
        } else {
            let typer = self.mmio.redistributor_read_u64(rd, GICR_TYPER);
            ((typer >> GICR_TYPER_PROCESSOR_SHIFT) & 0xFFFF) << 16
        }
    }

    /// Point the redistributor at `rd` at `tables` and turn its LPIs on,
    /// answering whether its table reads snoop the CPU's caches: where they
    /// do not, the tables are programmed non-cacheable and every later
    /// change to them must be cleaned to the point of coherency first.
    ///
    /// # Errors
    ///
    /// [`Gicv3Error::NoLpis`] for a redistributor with no physical LPIs or
    /// one that will not turn them on, [`Gicv3Error::LpisInUse`] for one
    /// firmware left them on, or [`Gicv3Error::Unresponsive`].
    pub fn enable_lpis(&self, rd: usize, tables: LpiTables) -> Result<bool, Gicv3Error> {
        if self.mmio.redistributor_read_u64(rd, GICR_TYPER) & GICR_TYPER_PLPIS == 0 {
            return Err(Gicv3Error::NoLpis);
        }
        if self.mmio.redistributor_read(rd, GICR_CTLR) & GICR_CTLR_ENABLE_LPIS != 0 {
            return Err(Gicv3Error::LpisInUse);
        }
        let id_bits = u64::from(tables.id_bits.saturating_sub(1) & 0x1F);
        let properties = self.program_table(rd, GICR_PROPBASER, tables.properties | id_bits);
        let pending = self.program_table(rd, GICR_PENDBASER, tables.pending | GICR_PENDBASER_PTZ);
        let ctlr = self.mmio.redistributor_read(rd, GICR_CTLR);
        self.mmio
            .redistributor_write(rd, GICR_CTLR, ctlr | GICR_CTLR_ENABLE_LPIS);
        self.wait_redistributor(rd)?;
        if self.mmio.redistributor_read(rd, GICR_CTLR) & GICR_CTLR_ENABLE_LPIS == 0 {
            return Err(Gicv3Error::NoLpis);
        }
        Ok(properties && pending)
    }

    /// Write the LPI table register at `off` cacheable and inner shareable,
    /// falling back to non-cacheable where it shares nothing, answering
    /// whether it shares.
    fn program_table(&self, rd: usize, off: usize, value: u64) -> bool {
        let shared = value | TABLE_INNER_SHAREABLE | TABLE_INNER_WRITE_BACK;
        self.mmio.redistributor_write_u64(rd, off, shared);
        if self.mmio.redistributor_read_u64(rd, off) & TABLE_SHAREABILITY != 0 {
            return true;
        }
        self.mmio
            .redistributor_write_u64(rd, off, value | TABLE_INNER_NON_CACHEABLE);
        false
    }

    /// Turn the calling CPU's system-register interface on, open its
    /// priority mask and enable Group 1, each acknowledgement then dropping
    /// priority and deactivating at once.
    ///
    /// # Errors
    ///
    /// [`Gicv3Error::NoSystemRegisters`] where a higher exception level
    /// holds the interface off.
    pub fn init_cpu_interface(&self) -> Result<(), Gicv3Error> {
        self.cpu
            .set_sre(self.cpu.sre() | ICC_SRE_SRE | ICC_SRE_DFB | ICC_SRE_DIB);
        self.cpu.synchronize();
        if self.cpu.sre() & ICC_SRE_SRE == 0 {
            return Err(Gicv3Error::NoSystemRegisters);
        }
        self.cpu.set_priority_mask(0xFF);
        self.cpu.set_binary_point(0);
        self.cpu.set_control(self.cpu.control() & !ICC_CTLR_EOIMODE);
        self.cpu.set_group1_enable(1);
        self.cpu.synchronize();
        Ok(())
    }

    fn read(&self, rd: usize, intid: u32, off: usize) -> u32 {
        if intid < MIN_SPI_INTID {
            self.mmio.redistributor_read(rd, SGI_FRAME + off)
        } else {
            self.mmio.distributor_read(off)
        }
    }

    fn write(&self, rd: usize, intid: u32, off: usize, value: u32) {
        if intid < MIN_SPI_INTID {
            self.mmio.redistributor_write(rd, SGI_FRAME + off, value);
        } else {
            self.mmio.distributor_write(off, value);
        }
    }

    /// Set `intid`'s priority byte; a private interrupt's on the CPU that
    /// owns `rd`.
    pub fn set_priority(&self, rd: usize, intid: u32, priority: u8) {
        let off = GICD_IPRIORITYR + intid as usize;
        if intid < MIN_SPI_INTID {
            self.mmio
                .redistributor_write_byte(rd, SGI_FRAME + off, priority);
        } else {
            self.mmio.distributor_write_byte(off, priority);
        }
    }

    /// Give `intid` the mid-range priority and enable it.
    pub fn enable(&self, rd: usize, intid: u32) {
        self.set_priority(rd, intid, MID_RANGE_PRIORITY);
        self.write(rd, intid, isenabler_offset(intid), isenabler_bit(intid));
    }

    /// Disable `intid`.
    pub fn disable(&self, rd: usize, intid: u32) {
        self.write(rd, intid, icenabler_offset(intid), isenabler_bit(intid));
    }

    /// Whether `intid` is enabled.
    #[must_use]
    pub fn is_enabled(&self, rd: usize, intid: u32) -> bool {
        self.read(rd, intid, isenabler_offset(intid)) & isenabler_bit(intid) != 0
    }

    /// Whether `intid` is edge-triggered.
    #[must_use]
    pub fn is_edge_triggered(&self, rd: usize, intid: u32) -> bool {
        self.read(rd, intid, icfgr_offset(intid)) & icfgr_edge_bit(intid) != 0
    }

    /// Make `intid` edge- or level-triggered. Its configuration register
    /// holds fifteen other interrupts' fields, so the caller serialises
    /// changes.
    pub fn set_edge_triggered(&self, rd: usize, intid: u32, edge: bool) {
        let off = icfgr_offset(intid);
        let word = self.read(rd, intid, off);
        let bit = icfgr_edge_bit(intid);
        self.write(rd, intid, off, if edge { word | bit } else { word & !bit });
    }

    /// Put `intid` in Group 0, signalled as FIQ, or back in Group 1.
    pub fn set_group(&self, rd: usize, intid: u32, group0: bool) {
        let off = igroupr_offset(intid);
        let word = self.read(rd, intid, off);
        let bit = isenabler_bit(intid);
        self.write(
            rd,
            intid,
            off,
            if group0 { word & !bit } else { word | bit },
        );
    }

    /// Route SPI `intid` to the CPU at `to`; a private interrupt has no
    /// route.
    pub fn route_spi(&self, intid: u32, to: Affinity) {
        if (MIN_SPI_INTID..=MAX_INTID).contains(&intid) {
            self.mmio
                .distributor_write_u64(GICD_IROUTER + 8 * intid as usize, to.routing());
        }
    }

    /// Acknowledge the highest-priority pending Group 1 interrupt, or
    /// [`None`] when nothing is.
    #[must_use]
    pub fn acknowledge(&self) -> Option<u32> {
        let intid = self.cpu.acknowledge_group1() & INTID_MASK;
        (!SPECIAL_INTIDS.contains(&intid)).then_some(intid)
    }

    /// End the Group 1 interrupt [`Self::acknowledge`] returned.
    pub fn end_of_interrupt(&self, intid: u32) {
        self.cpu.end_group1(intid);
    }

    /// Acknowledge the highest-priority pending Group 0 interrupt, or
    /// [`None`] when nothing is.
    #[must_use]
    pub fn acknowledge_fiq(&self) -> Option<u32> {
        let intid = self.cpu.acknowledge_group0() & INTID_MASK;
        (!SPECIAL_INTIDS.contains(&intid)).then_some(intid)
    }

    /// End the Group 0 interrupt [`Self::acknowledge_fiq`] returned.
    pub fn end_of_fiq(&self, intid: u32) {
        self.cpu.end_group0(intid);
    }

    /// Raise SGI `intid` on the CPU at `to`.
    ///
    /// # Errors
    ///
    /// [`Gicv3Error::Unaddressable`] for an `Aff0` the controller's SGIs
    /// cannot reach.
    pub fn send_sgi(&self, intid: u32, to: Affinity) -> Result<(), Gicv3Error> {
        if to.needs_range_selector()
            && (self.cpu.control() & ICC_CTLR_RSS == 0
                || self.mmio.distributor_read(GICD_TYPER) & GICD_TYPER_RSS == 0)
        {
            return Err(Gicv3Error::Unaddressable);
        }
        self.cpu.raise_group1_sgi(to.sgi(intid));
        Ok(())
    }

    /// The lowest SPI up to `max_intid` stuck active, or pending while
    /// enabled; the distributor's SPI banks keep the GICv2 layout.
    #[must_use]
    pub fn stuck_spi(&self, max_intid: u32) -> Option<StuckInterrupt> {
        first_stuck_spi(max_intid, |off| self.mmio.distributor_read(off))
    }

    /// What [`Self::route_fiq`] changes, to put back with
    /// [`Self::restore_fiq`].
    #[must_use]
    pub fn fiq_route(&self) -> FiqRoute {
        FiqRoute {
            distributor: self.mmio.distributor_read(GICD_CTLR) & !GICD_CTLR_RWP,
            group0_enable: self.cpu.group0_enable(),
        }
    }

    /// Deliver private interrupt `intid` of the CPU at `rd` alone as FIQ:
    /// Group 0, forwarded by the distributor and signalled by this CPU's
    /// interface. Group 1 interrupts stay IRQs.
    ///
    /// # Errors
    ///
    /// [`Gicv3Error::Unresponsive`] for a distributor that never completes
    /// the enable.
    pub fn route_fiq(&self, rd: usize, intid: u32) -> Result<(), Gicv3Error> {
        self.set_group(rd, intid, true);
        let ctlr = self.mmio.distributor_read(GICD_CTLR) & !GICD_CTLR_RWP;
        self.mmio
            .distributor_write(GICD_CTLR, ctlr | GICD_CTLR_ENABLE_GRP0);
        self.wait_distributor()?;
        self.cpu.set_group0_enable(1);
        self.cpu.synchronize();
        Ok(())
    }

    /// Undo [`Self::route_fiq`] for `intid` from the state saved before it.
    ///
    /// # Errors
    ///
    /// [`Gicv3Error::Unresponsive`].
    pub fn restore_fiq(&self, rd: usize, intid: u32, saved: FiqRoute) -> Result<(), Gicv3Error> {
        self.set_group(rd, intid, false);
        self.cpu.set_group0_enable(saved.group0_enable);
        self.cpu.synchronize();
        self.mmio.distributor_write(GICD_CTLR, saved.distributor);
        self.wait_distributor()
    }
}

/// Bare-metal [`Gicv3Mmio`] over the discovered distributor and any
/// redistributor named by its identity-mapped address.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
pub struct VolatileGicv3Mmio;

#[cfg(all(target_arch = "aarch64", target_os = "none"))]
impl Gicv3Mmio for VolatileGicv3Mmio {
    fn distributor_read(&self, off: usize) -> u32 {
        // SAFETY: `off` is a register inside the discovered distributor
        // window, identity-mapped Device memory the kernel alone drives.
        unsafe { core::ptr::read_volatile((crate::gic::current().0 + off) as *const u32) }
    }
    fn distributor_write(&self, off: usize, value: u32) {
        // SAFETY: as `distributor_read`, a 32-bit store.
        unsafe { core::ptr::write_volatile((crate::gic::current().0 + off) as *mut u32, value) }
    }
    fn distributor_write_u64(&self, off: usize, value: u64) {
        // SAFETY: as `distributor_read`; `GICD_IROUTER<n>` takes a 64-bit
        // store at its 8-byte-aligned offset.
        unsafe { core::ptr::write_volatile((crate::gic::current().0 + off) as *mut u64, value) }
    }
    fn distributor_write_byte(&self, off: usize, value: u8) {
        // SAFETY: as `distributor_read`; the priority registers are
        // byte-accessible.
        unsafe { core::ptr::write_volatile((crate::gic::current().0 + off) as *mut u8, value) }
    }
    fn redistributor_read(&self, rd: usize, off: usize) -> u32 {
        // SAFETY: `rd` is a redistributor inside a discovered region,
        // identity-mapped Device memory, and `off` lies within its frames.
        unsafe { core::ptr::read_volatile((rd + off) as *const u32) }
    }
    fn redistributor_read_u64(&self, rd: usize, off: usize) -> u64 {
        // SAFETY: as `redistributor_read`; `GICR_TYPER` takes a 64-bit load.
        unsafe { core::ptr::read_volatile((rd + off) as *const u64) }
    }
    fn redistributor_write(&self, rd: usize, off: usize, value: u32) {
        // SAFETY: as `redistributor_read`, a 32-bit store.
        unsafe { core::ptr::write_volatile((rd + off) as *mut u32, value) }
    }
    fn redistributor_write_u64(&self, rd: usize, off: usize, value: u64) {
        // SAFETY: as `redistributor_read`; `GICR_PROPBASER` and
        // `GICR_PENDBASER` take a 64-bit store.
        unsafe { core::ptr::write_volatile((rd + off) as *mut u64, value) }
    }
    fn redistributor_write_byte(&self, rd: usize, off: usize, value: u8) {
        // SAFETY: as `redistributor_read`; the priority registers are
        // byte-accessible.
        unsafe { core::ptr::write_volatile((rd + off) as *mut u8, value) }
    }
}

/// The calling CPU's GICv3 system registers.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
pub struct SystemCpuInterface;

/// `mrs` of a system register named by its encoding, which assemblers
/// accept whatever GIC extension they were told of.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
macro_rules! read_sysreg {
    ($encoding:literal) => {{
        let value: u64;
        // SAFETY: an `mrs` of a GIC CPU-interface register, readable at EL1
        // once the interface is enabled; it touches no memory.
        unsafe {
            core::arch::asm!(concat!("mrs {}, ", $encoding), out(reg) value, options(nomem, nostack, preserves_flags));
        }
        value
    }};
}

#[cfg(all(target_arch = "aarch64", target_os = "none"))]
macro_rules! write_sysreg {
    ($encoding:literal, $value:expr) => {{
        let value: u64 = $value;
        // SAFETY: an `msr` to a GIC CPU-interface register at EL1; its
        // effect is the interface's own and it touches no memory.
        unsafe {
            core::arch::asm!(concat!("msr ", $encoding, ", {}"), in(reg) value, options(nomem, nostack, preserves_flags));
        }
    }};
}

#[cfg(all(target_arch = "aarch64", target_os = "none"))]
impl CpuInterface for SystemCpuInterface {
    fn sre(&self) -> u64 {
        read_sysreg!("S3_0_C12_C12_5")
    }
    fn set_sre(&self, value: u64) {
        write_sysreg!("S3_0_C12_C12_5", value);
    }
    fn set_priority_mask(&self, value: u64) {
        write_sysreg!("S3_0_C4_C6_0", value);
    }
    fn set_binary_point(&self, value: u64) {
        write_sysreg!("S3_0_C12_C12_3", value);
    }
    fn control(&self) -> u64 {
        read_sysreg!("S3_0_C12_C12_4")
    }
    fn set_control(&self, value: u64) {
        write_sysreg!("S3_0_C12_C12_4", value);
    }
    fn group0_enable(&self) -> u64 {
        read_sysreg!("S3_0_C12_C12_6")
    }
    fn set_group0_enable(&self, value: u64) {
        write_sysreg!("S3_0_C12_C12_6", value);
    }
    fn set_group1_enable(&self, value: u64) {
        write_sysreg!("S3_0_C12_C12_7", value);
    }
    fn acknowledge_group1(&self) -> u32 {
        let iar = read_sysreg!("S3_0_C12_C12_0");
        acknowledged();
        u32::try_from(iar & u64::from(INTID_MASK)).unwrap_or(SPURIOUS_INTID)
    }
    fn end_group1(&self, intid: u32) {
        write_sysreg!("S3_0_C12_C12_1", u64::from(intid));
    }
    fn acknowledge_group0(&self) -> u32 {
        let iar = read_sysreg!("S3_0_C12_C8_0");
        acknowledged();
        u32::try_from(iar & u64::from(INTID_MASK)).unwrap_or(SPURIOUS_INTID)
    }
    fn end_group0(&self, intid: u32) {
        write_sysreg!("S3_0_C12_C8_1", u64::from(intid));
    }
    fn raise_group1_sgi(&self, value: u64) {
        // SAFETY: `dsb ishst` only orders this CPU's earlier stores before
        // the SGI is generated; it touches no memory itself.
        unsafe {
            core::arch::asm!("dsb ishst", options(nostack, preserves_flags));
        }
        write_sysreg!("S3_0_C12_C11_5", value);
        self.synchronize();
    }
    fn mpidr(&self) -> u64 {
        read_sysreg!("mpidr_el1")
    }
    fn synchronize(&self) {
        // SAFETY: `isb` completes the preceding system-register writes and
        // has no other effect.
        unsafe {
            core::arch::asm!("isb", options(nostack, preserves_flags));
        }
    }
}

/// Complete an acknowledge: its effect on the redistributor is not guaranteed
/// visible before a `dsb` (GIC Architecture Specification, IHI 0069), so the
/// masking and end that follow could otherwise act on an interrupt still
/// pending.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
fn acknowledged() {
    // SAFETY: `dsb sy` waits for earlier accesses to complete and touches no
    // memory itself.
    unsafe {
        core::arch::asm!("dsb sy", options(nostack, preserves_flags));
    }
}

#[cfg(test)]
#[path = "gicv3_tests.rs"]
mod tests;
