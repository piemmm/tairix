//! The Arm Generic Interrupt Controller: device-tree discovery, the
//! version-neutral entry points the port calls, and the GICv2 driver.
//!
//! A board's GIC is a GICv2 (QEMU `virt`'s default, the Pi 4's GIC-400) or a
//! GICv3 ([`crate::gicv3`]), told apart by its device-tree `compatible`. Every
//! entry point here dispatches on the discovered version, so interrupt entry,
//! IPIs and line routing are written once (`plans/IOMMU.md` IOM18.1).
//!
//! INTIDs `0..16` are SGIs and `16..32` PPIs, both private to each CPU, and
//! `32..1020` are SPIs.

#[cfg(any(test, all(target_arch = "aarch64", target_os = "none")))]
use core::sync::atomic::AtomicU64;
use core::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

#[cfg(any(test, all(target_arch = "aarch64", target_os = "none")))]
use tairix_arch_api::CpuId;
use tairix_arch_api::StuckInterrupt;
use tairix_fdt::Fdt;
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
use tairix_sync::once::OnceCell;
use tairix_sync::SpinLock;

#[cfg(all(target_arch = "aarch64", target_os = "none"))]
use crate::gicv3::Affinity;
use crate::gicv3::{CpuInterface, Gicv3, Gicv3Error, Gicv3Mmio, RedistributorRegion};

/// The distributor base before discovery runs: QEMU `virt`'s.
pub const DEFAULT_GICD_BASE: usize = 0x0800_0000;

/// The GICv2 CPU-interface base before discovery runs: QEMU `virt`'s.
pub const DEFAULT_GICC_BASE: usize = 0x0801_0000;

// Discovery runs with the MMU off, where an atomic read-modify-write is
// unpredictable, so these are set by plain stores.
static GICD_BASE: AtomicUsize = AtomicUsize::new(DEFAULT_GICD_BASE);
static GICC_BASE: AtomicUsize = AtomicUsize::new(DEFAULT_GICC_BASE);
static VERSION: AtomicU8 = AtomicU8::new(GicVersion::V2 as u8);

/// The GIC architecture a board's controller implements.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum GicVersion {
    /// A memory-mapped CPU interface, eight CPUs at most.
    V2 = 2,
    /// Affinity routing, a redistributor per CPU, a system-register CPU
    /// interface.
    V3 = 3,
}

/// Point the driver at a GICv2's distributor and CPU interface.
pub fn configure(distributor: usize, cpu_iface: usize) {
    GICD_BASE.store(distributor, Ordering::Release);
    GICC_BASE.store(cpu_iface, Ordering::Release);
    VERSION.store(GicVersion::V2 as u8, Ordering::Release);
}

/// Point the driver at a GICv3's distributor; its redistributors are found
/// from the `GicTopology` `init` is given.
pub fn configure_v3(distributor: usize) {
    GICD_BASE.store(distributor, Ordering::Release);
    VERSION.store(GicVersion::V3 as u8, Ordering::Release);
}

/// The distributor and GICv2 CPU-interface bases in effect.
#[must_use]
pub fn current() -> (usize, usize) {
    (
        GICD_BASE.load(Ordering::Acquire),
        GICC_BASE.load(Ordering::Acquire),
    )
}

/// The discovered GIC architecture.
#[must_use]
pub fn version() -> GicVersion {
    if VERSION.load(Ordering::Acquire) == GicVersion::V3 as u8 {
        GicVersion::V3
    } else {
        GicVersion::V2
    }
}

/// GICv2-class controllers: the GIC-400 and the Cortex-A cores' integrated
/// GICs share the register layout.
const GICV2_COMPATIBLES: &[&[u8]] = &[
    b"arm,gic-400",
    b"arm,cortex-a15-gic",
    b"arm,cortex-a7-gic",
    b"arm,cortex-a9-gic",
    b"arm,gic-v2",
];

const GICV3_COMPATIBLE: &[u8] = b"arm,gic-v3";

/// A GICv3 interrupt translation service, a child of its GIC's node.
pub const ITS_COMPATIBLE: &[u8] = b"arm,gic-v3-its";

fn version_of(compatible: &[u8]) -> Option<GicVersion> {
    if GICV2_COMPATIBLES.contains(&compatible) {
        Some(GicVersion::V2)
    } else if compatible == GICV3_COMPATIBLE {
        Some(GicVersion::V3)
    } else {
        None
    }
}

/// Whether `compatible` names a GIC this driver speaks.
#[must_use]
pub fn is_gic_compatible(compatible: &[u8]) -> bool {
    version_of(compatible).is_some()
}

/// The `compatible` string of `node` naming a GIC, and its version.
fn gic_of<'a>(node: &tairix_fdt::Node<'a>) -> Option<(&'a [u8], GicVersion)> {
    node.property("compatible")?
        .iter_strings()
        .find_map(|name| Some((name, version_of(name)?)))
}

/// How many redistributor regions a GICv3 node's `reg` names after its
/// distributor: its `#redistributor-regions`, never more than `reg` holds.
fn redistributor_region_count(
    node: &tairix_fdt::Node<'_>,
    depth: usize,
    levels: &[tairix_fdt::BusLevel<'_>],
) -> usize {
    let declared = node
        .property("#redistributor-regions")
        .and_then(|cells| cells.read_be_u32(0).ok())
        .map_or(1, |count| count as usize);
    let present = tairix_fdt::reg_entry_count(node, depth, levels).unwrap_or(0);
    declared.min(present.saturating_sub(1))
}

/// A GIC located in a flattened device tree.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct DiscoveredGic<'a> {
    /// The `compatible` string that selected it.
    pub compatible: &'a [u8],
    /// Its architecture.
    pub version: GicVersion,
    /// The distributor's CPU-physical base.
    pub gicd_base: u64,
    /// A GICv2's CPU interface; a GICv3's is system registers.
    pub gicc_base: Option<u64>,
    /// What a device's effective `interrupt-parent` names for its
    /// specifiers to be this controller's.
    pub phandle: Option<u32>,
}

/// Locate the GIC in `fdt`: the first node naming one this driver speaks,
/// whose distributor (and, for a GICv2, CPU interface) decodes through the
/// ancestor buses' `ranges`, and which, as a GICv3, describes at least one
/// redistributor region. The walk returns at that node, so it is safe with
/// the MMU off.
#[must_use]
pub fn find_gic<'a>(fdt: &Fdt<'a>) -> Option<DiscoveredGic<'a>> {
    tairix_fdt::scan_translated(fdt, |node, levels, depth| {
        let (compatible, version) = gic_of(node)?;
        let (gicd_base, _) = tairix_fdt::translated_reg(node, depth, levels, 0)?;
        let second = tairix_fdt::translated_reg(node, depth, levels, 1)?;
        Some(DiscoveredGic {
            compatible,
            version,
            gicd_base,
            gicc_base: (version == GicVersion::V2).then_some(second.0),
            phandle: node.phandle(),
        })
    })
}

/// Discover the GIC in `fdt` and point the driver at it, leaving the
/// previous configuration in place when there is none.
#[must_use]
pub fn configure_from_fdt<'a>(fdt: &Fdt<'a>) -> Option<DiscoveredGic<'a>> {
    let found = find_gic(fdt)?;
    let distributor = usize::try_from(found.gicd_base).ok()?;
    match found.gicc_base {
        Some(gicc) => configure(distributor, usize::try_from(gicc).ok()?),
        None => configure_v3(distributor),
    }
    Some(found)
}

/// Visit the GIC node in `fdt` with its version, then each node beneath it
/// with none, stopping once past its subtree so the walk is safe with the
/// MMU off.
fn each_gic_node(
    fdt: &Fdt<'_>,
    mut visit: impl FnMut(&tairix_fdt::Node<'_>, &[tairix_fdt::BusLevel<'_>], usize, Option<GicVersion>),
) {
    let mut gic_depth = None;
    let _ = tairix_fdt::scan_translated(fdt, |node, levels, depth| match gic_depth {
        Some(top) if depth <= top => Some(()),
        Some(_) => {
            visit(node, levels, depth, None);
            None
        }
        None => {
            let (_, version) = gic_of(node)?;
            gic_depth = Some(depth);
            visit(node, levels, depth, Some(version));
            None
        }
    });
}

fn is_its(node: &tairix_fdt::Node<'_>) -> bool {
    node.property("compatible")
        .is_some_and(|names| names.iter_strings().any(|name| name == ITS_COMPATIBLE))
}

/// Hand `window` every register window of the GIC — its distributor, its
/// GICv2 CPU interface or GICv3 redistributor regions — and of each
/// interrupt translation service beneath it, so the boot can map them all
/// as Device memory.
pub fn for_each_window(fdt: &Fdt<'_>, mut window: impl FnMut(u64, u64)) {
    each_gic_node(fdt, |node, levels, depth, version| {
        let mut emit = |index| {
            if let Some((base, len)) = tairix_fdt::translated_reg(node, depth, levels, index) {
                window(base, len);
            }
        };
        match version {
            Some(GicVersion::V2) => (0..2).for_each(&mut emit),
            Some(GicVersion::V3) => {
                (0..=redistributor_region_count(node, depth, levels)).for_each(&mut emit);
            }
            None if is_its(node) => emit(0),
            None => {}
        }
    });
}

/// Hand `visit` each interrupt translation service beneath the GIC in `fdt`:
/// its phandle, where it has one, and its register frames' base and length.
pub fn for_each_its(fdt: &Fdt<'_>, mut visit: impl FnMut(Option<u32>, u64, u64)) {
    each_gic_node(fdt, |node, levels, depth, version| {
        if version.is_none() && is_its(node) {
            if let Some((base, len)) = tairix_fdt::translated_reg(node, depth, levels, 0) {
                visit(node.phandle(), base, len);
            }
        }
    });
}

/// Hand `region` each redistributor region a GICv3 in `fdt` describes, and
/// answer the stride between redistributors where the tree names one —
/// [`None`] when the tree holds no GICv3 or a region does not decode.
pub fn redistributor_regions(
    fdt: &Fdt<'_>,
    mut region: impl FnMut(RedistributorRegion),
) -> Option<Option<u64>> {
    tairix_fdt::scan_translated(fdt, |node, levels, depth| {
        let (_, version) = gic_of(node)?;
        if version != GicVersion::V3 {
            return Some(None);
        }
        let regions = redistributor_region_count(node, depth, levels);
        if regions == 0 {
            return Some(None);
        }
        for index in 1..=regions {
            let Some((base, len)) = tairix_fdt::translated_reg(node, depth, levels, index) else {
                return Some(None);
            };
            region(RedistributorRegion { base, len });
        }
        let stride = node
            .property("redistributor-stride")
            .and_then(|stride| stride.read_be_u64(0).ok());
        Some(Some(stride))
    })
    .flatten()
}

pub(crate) const GICD_CTLR: usize = 0x000;
/// `GICD_TYPER`: `ITLinesNumber` in bits `[4:0]` gives `32 * (n + 1)` lines.
pub(crate) const GICD_TYPER: usize = 0x004;
pub(crate) const GICD_TYPER_ITLINES_MASK: u32 = 0x1F;
/// Every interrupt of a word in Group 1.
const GROUP1_ALL: u32 = 0xFFFF_FFFF;
/// `GICD_IGROUPR<n>`: a clear bit is Group 0, signalled as FIQ.
pub(crate) const GICD_IGROUPR: usize = 0x080;
pub(crate) const GICD_ISENABLER: usize = 0x100;
pub(crate) const GICD_ICENABLER: usize = 0x180;
pub(crate) const GICD_ISPENDR: usize = 0x200;
pub(crate) const GICD_ISACTIVER: usize = 0x300;
pub(crate) const GICD_ICACTIVER: usize = 0x380;
/// `GICD_IPRIORITYR<n>`: a byte per interrupt, lower is more urgent.
pub(crate) const GICD_IPRIORITYR: usize = 0x400;
/// The priority every enabled interrupt takes. The debug watchdog's FIQ
/// sample sits below it so a masked FIQ never holds off the timer IRQ.
pub const MID_RANGE_PRIORITY: u8 = 0x80;
/// `GICD_ITARGETSR<n>`: a GICv2 SPI's CPU-interface mask, a byte each.
/// The first eight words are banked and read-only.
const GICD_ITARGETSR: usize = 0x800;
const GICD_SGIR: usize = 0xF00;
/// `GICD_ICFGR<n>`: two bits per interrupt, the upper set for edge.
pub(crate) const GICD_ICFGR: usize = 0xC00;

const GICC_CTLR: usize = 0x000;
const GICC_PMR: usize = 0x004;
const GICC_IAR: usize = 0x00C;
const GICC_EOIR: usize = 0x010;

/// `GICC_CTLR.EnableGrp0` in the single-Security-state view. On a
/// two-state GIC this and the bits below are Secure-only, so setting them
/// from the kernel does nothing and the watchdog's probe finds FIQ
/// undeliverable.
pub const GICC_CTLR_ENABLE_GRP0: u32 = 1 << 0;
/// `GICC_CTLR.FIQEn`: signal Group 0 as FIQ — for every Group 0 interrupt.
pub const GICC_CTLR_FIQEN: u32 = 1 << 3;
/// `GICC_CTLR.EnableGrp1` in the single-Security-state view.
pub const GICC_CTLR_ENABLE_GRP1: u32 = 1 << 1;
/// `GICC_CTLR.AckCtl`: `GICC_IAR` acknowledges Group 1 too, which would
/// otherwise read the reserved id 1022.
pub const GICC_CTLR_ACKCTL: u32 = 1 << 2;

/// The highest SPI either version addresses: `1020..=1023` are special.
pub const MAX_INTID: u32 = 1019;

/// A GICv2 `GICC_IAR`'s INTID field; bits `[12:10]` name an SGI's source.
pub const IAR_INTID_MASK: u32 = 0x3FF;

/// The INTID an acknowledgement of nothing reads; it takes no
/// end-of-interrupt.
pub const SPURIOUS_INTID: u32 = 1023;

/// The first SPI; below it each CPU's interrupts are its own.
pub const MIN_SPI_INTID: u32 = 32;

/// `GICD_SGIR` raising SGI `intid` on the CPU interfaces in `target_list`.
#[must_use]
pub const fn sgir_value(intid: u32, target_list: u8) -> u32 {
    ((target_list as u32) << 16) | (intid & 0xF)
}

const fn gicd_bit_word_offset(base: usize, intid: u32) -> usize {
    base + ((intid / 32) as usize) * 4
}

/// The `GICD_ICFGR` register holding `intid`'s field.
#[must_use]
pub const fn icfgr_offset(intid: u32) -> usize {
    GICD_ICFGR + (intid as usize / 16) * 4
}

/// The bit of `intid`'s `GICD_ICFGR` field that makes it edge-triggered.
#[must_use]
pub const fn icfgr_edge_bit(intid: u32) -> u32 {
    1 << (2 * (intid % 16) + 1)
}

/// The `GICD_ISENABLER` word holding `intid`.
#[must_use]
pub const fn isenabler_offset(intid: u32) -> usize {
    gicd_bit_word_offset(GICD_ISENABLER, intid)
}

/// `intid`'s bit in any one-bit-per-interrupt word.
#[must_use]
pub const fn isenabler_bit(intid: u32) -> u32 {
    1 << (intid % 32)
}

/// The `GICD_ITARGETSR` byte of `intid`.
#[must_use]
pub const fn itargetsr_offset(intid: u32) -> usize {
    GICD_ITARGETSR + intid as usize
}

/// The `GICD_ICENABLER` word holding `intid`.
#[must_use]
pub const fn icenabler_offset(intid: u32) -> usize {
    gicd_bit_word_offset(GICD_ICENABLER, intid)
}

/// The `GICD_IGROUPR` word holding `intid`.
#[must_use]
pub const fn igroupr_offset(intid: u32) -> usize {
    gicd_bit_word_offset(GICD_IGROUPR, intid)
}

/// The lowest SPI up to `max_intid` that is stuck and could still reach a
/// CPU — active in preference to pending-while-enabled — read through
/// `read`. A masked pending line is skipped: it cannot wedge a CPU. SGIs and
/// PPIs are banked per CPU, so an observer could only read its own.
#[must_use]
pub(crate) fn first_stuck_spi(
    max_intid: u32,
    read: impl Fn(usize) -> u32,
) -> Option<StuckInterrupt> {
    if let Some(intid) = first_matching_spi(GICD_ISACTIVER, max_intid, &read, |_| true) {
        return Some(StuckInterrupt {
            intid,
            active: true,
        });
    }
    let enabled = |id: u32| read(isenabler_offset(id)) & isenabler_bit(id) != 0;
    first_matching_spi(GICD_ISPENDR, max_intid, &read, enabled).map(|intid| StuckInterrupt {
        intid,
        active: false,
    })
}

/// The lowest SPI up to `max_intid` set in the bank at `base` that `accept`
/// admits.
fn first_matching_spi(
    base: usize,
    max_intid: u32,
    read: &impl Fn(usize) -> u32,
    accept: impl Fn(u32) -> bool,
) -> Option<u32> {
    let mut intid = MIN_SPI_INTID;
    while intid <= max_intid {
        let mut word = read(gicd_bit_word_offset(base, intid));
        while word != 0 {
            let candidate = intid + word.trailing_zeros();
            if candidate > max_intid {
                return None;
            }
            if accept(candidate) {
                return Some(candidate);
            }
            word &= word - 1;
        }
        intid += 32;
    }
    None
}

/// Volatile access to a GICv2 distributor and CPU interface.
pub trait GicMmio {
    /// Read the distributor register at `off`.
    fn gicd_read(&self, off: usize) -> u32;
    /// Write the distributor register at `off`.
    fn gicd_write(&self, off: usize, val: u32);
    /// Write the distributor byte register at `off`.
    fn gicd_write_byte(&self, off: usize, val: u8);
    /// Read the CPU-interface register at `off`.
    fn gicc_read(&self, off: usize) -> u32;
    /// Write the CPU-interface register at `off`.
    fn gicc_write(&self, off: usize, val: u32);
    /// Make this CPU's earlier stores visible to the inner-shareable domain
    /// before an SGI is raised, so a woken CPU never reads a run queue older
    /// than the IPI (`dsb ishst`).
    fn publish_barrier(&self);
}

/// The GICv2 register driver; policy is [`GicController`]'s.
pub struct Gicv2<M: GicMmio> {
    mmio: M,
}

impl<M: GicMmio> Gicv2<M> {
    /// Bind a driver to `mmio`.
    pub const fn new(mmio: M) -> Self {
        Self { mmio }
    }

    /// Forward pending interrupts to the CPU interfaces.
    pub fn init_distributor(&self) {
        self.mmio.gicd_write(GICD_CTLR, 1);
    }

    /// Signal interrupts to this CPU and open its priority mask.
    pub fn init_cpu_interface(&self) {
        self.mmio.gicc_write(GICC_PMR, 0xFF);
        self.mmio.gicc_write(GICC_CTLR, 1);
    }

    /// This CPU's interface mask, read from its banked `GICD_ITARGETSR0`.
    /// A uniprocessor GIC reads zero there and targets its one interface
    /// whatever a target list says.
    #[must_use]
    pub fn local_interface_mask(&self) -> u8 {
        match u8::try_from(self.mmio.gicd_read(GICD_ITARGETSR) & 0xFF) {
            Ok(0) | Err(_) => 1,
            Ok(mask) => mask,
        }
    }

    /// Give `intid` the mid-range priority and enable it.
    pub fn enable_intid(&self, intid: u32) {
        self.mmio
            .gicd_write_byte(GICD_IPRIORITYR + intid as usize, MID_RANGE_PRIORITY);
        self.mmio
            .gicd_write(isenabler_offset(intid), isenabler_bit(intid));
    }

    /// Disable `intid`.
    pub fn disable_intid(&self, intid: u32) {
        self.mmio
            .gicd_write(icenabler_offset(intid), isenabler_bit(intid));
    }

    /// Whether `intid` is enabled.
    #[must_use]
    pub fn is_enabled(&self, intid: u32) -> bool {
        self.mmio.gicd_read(isenabler_offset(intid)) & isenabler_bit(intid) != 0
    }

    /// Whether `intid` is edge-triggered.
    #[must_use]
    pub fn is_edge_triggered(&self, intid: u32) -> bool {
        self.mmio.gicd_read(icfgr_offset(intid)) & icfgr_edge_bit(intid) != 0
    }

    /// Make `intid` edge- or level-triggered. Its configuration register holds
    /// fifteen other interrupts' fields, so the caller serialises changes.
    pub fn set_edge_triggered(&self, intid: u32, edge: bool) {
        let offset = icfgr_offset(intid);
        let word = self.mmio.gicd_read(offset);
        let bit = icfgr_edge_bit(intid);
        self.mmio
            .gicd_write(offset, if edge { word | bit } else { word & !bit });
    }

    /// Set `intid`'s priority byte; a private interrupt's on the calling CPU.
    pub fn set_priority(&self, intid: u32, priority: u8) {
        self.mmio
            .gicd_write_byte(GICD_IPRIORITYR + intid as usize, priority);
    }

    /// Route SPI `intid` to the CPU interfaces in `cpu_targets`. A private
    /// interrupt's target byte is read-only, so it is left alone.
    pub fn route_spi(&self, intid: u32, cpu_targets: u8) {
        if intid >= MIN_SPI_INTID {
            self.mmio
                .gicd_write_byte(itargetsr_offset(intid), cpu_targets);
        }
    }

    /// Acknowledge the highest-priority pending interrupt, or [`None`] when
    /// nothing is. The token carries an SGI's source CPU, which its
    /// end-of-interrupt must write back.
    #[must_use]
    pub fn acknowledge(&self) -> Option<Acknowledged> {
        let iar = self.mmio.gicc_read(GICC_IAR);
        let intid = iar & IAR_INTID_MASK;
        (intid != SPURIOUS_INTID).then_some(Acknowledged { intid, token: iar })
    }

    /// End the interrupt whose acknowledgement token was `token`.
    pub fn end_of_interrupt(&self, token: u32) {
        self.mmio.gicc_write(GICC_EOIR, token);
    }

    /// The lowest SPI up to `max_intid` stuck active, or pending while
    /// enabled.
    #[must_use]
    pub fn stuck_spi(&self, max_intid: u32) -> Option<StuckInterrupt> {
        first_stuck_spi(max_intid, |off| self.mmio.gicd_read(off))
    }

    /// Raise SGI `intid` on the CPU interfaces in `target_list`, after this
    /// CPU's earlier stores are visible to them.
    pub fn send_sgi(&self, intid: u32, target_list: u8) {
        self.mmio.publish_barrier();
        self.mmio
            .gicd_write(GICD_SGIR, sgir_value(intid, target_list));
    }

    /// Put `intid` in Group 0, or back in Group 1; a private interrupt on the
    /// calling CPU.
    pub fn set_group(&self, intid: u32, group0: bool) {
        let off = igroupr_offset(intid);
        let word = self.mmio.gicd_read(off);
        let bit = isenabler_bit(intid);
        self.mmio
            .gicd_write(off, if group0 { word & !bit } else { word | bit });
    }

    fn igroupr_word_count(&self) -> usize {
        let itlines = self.mmio.gicd_read(GICD_TYPER) & GICD_TYPER_ITLINES_MASK;
        (itlines as usize) + 1
    }

    /// Deliver only `fiq_intid` as FIQ. `GICC_CTLR.FIQEn` signals every Group
    /// 0 interrupt as FIQ and this GIC resets them all to Group 0, so every
    /// other interrupt moves to Group 1 first, and `AckCtl` keeps those
    /// acknowledgeable through `GICC_IAR`. Group words past the first are
    /// distributor-wide; the first is banked, so every CPU calls this.
    pub fn route_selfsample_fiq(&self, fiq_intid: u32) {
        for word in 0..self.igroupr_word_count() {
            self.mmio.gicd_write(GICD_IGROUPR + word * 4, GROUP1_ALL);
        }
        self.set_group(fiq_intid, true);
        let cpu = self.mmio.gicc_read(GICC_CTLR)
            | GICC_CTLR_ENABLE_GRP0
            | GICC_CTLR_ENABLE_GRP1
            | GICC_CTLR_ACKCTL
            | GICC_CTLR_FIQEN;
        self.mmio.gicc_write(GICC_CTLR, cpu);
        // The distributor's group enables sit at the CPU interface's bits.
        let dist = self.mmio.gicd_read(GICD_CTLR) | GICC_CTLR_ENABLE_GRP0 | GICC_CTLR_ENABLE_GRP1;
        self.mmio.gicd_write(GICD_CTLR, dist);
    }

    /// `GICC_CTLR`.
    #[must_use]
    pub fn read_gicc_ctlr(&self) -> u32 {
        self.mmio.gicc_read(GICC_CTLR)
    }

    /// Write `GICC_CTLR`.
    pub fn write_gicc_ctlr(&self, val: u32) {
        self.mmio.gicc_write(GICC_CTLR, val);
    }

    /// `GICD_CTLR`.
    #[must_use]
    pub fn read_gicd_ctlr(&self) -> u32 {
        self.mmio.gicd_read(GICD_CTLR)
    }

    /// Write `GICD_CTLR`.
    pub fn write_gicd_ctlr(&self, val: u32) {
        self.mmio.gicd_write(GICD_CTLR, val);
    }
}

/// An acknowledged interrupt.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Acknowledged {
    /// Its INTID.
    pub intid: u32,
    token: u32,
}

impl Acknowledged {
    /// What its end-of-interrupt writes back.
    #[must_use]
    pub const fn token(self) -> u32 {
        self.token
    }
}

/// The per-interrupt operations a GIC driver offers the policy layer.
pub trait GicOps {
    /// Give `intid` the mid-range priority and enable it.
    fn enable(&self, intid: u32);
    /// Disable `intid`.
    fn disable(&self, intid: u32);
    /// Whether `intid` is enabled.
    fn is_enabled(&self, intid: u32) -> bool;
    /// Whether `intid` is edge-triggered.
    fn is_edge_triggered(&self, intid: u32) -> bool;
    /// Make `intid` edge- or level-triggered.
    fn set_edge_triggered(&self, intid: u32, edge: bool);
    /// Acknowledge the highest-priority pending interrupt.
    fn acknowledge(&self) -> Option<Acknowledged>;
    /// End the interrupt whose token was `token`.
    fn end_of_interrupt(&self, token: u32);
}

impl<M: GicMmio> GicOps for Gicv2<M> {
    fn enable(&self, intid: u32) {
        self.enable_intid(intid);
    }
    fn disable(&self, intid: u32) {
        self.disable_intid(intid);
    }
    fn is_enabled(&self, intid: u32) -> bool {
        Gicv2::is_enabled(self, intid)
    }
    fn is_edge_triggered(&self, intid: u32) -> bool {
        Gicv2::is_edge_triggered(self, intid)
    }
    fn set_edge_triggered(&self, intid: u32, edge: bool) {
        Gicv2::set_edge_triggered(self, intid, edge);
    }
    fn acknowledge(&self) -> Option<Acknowledged> {
        Gicv2::acknowledge(self)
    }
    fn end_of_interrupt(&self, token: u32) {
        Gicv2::end_of_interrupt(self, token);
    }
}

/// A GICv3 as the CPU owning the redistributor at `rd` sees it.
pub struct Gicv3Local<'g, M, C> {
    gic: &'g Gicv3<M, C>,
    rd: usize,
}

impl<'g, M, C> Gicv3Local<'g, M, C> {
    /// `gic` seen from the CPU whose redistributor is `rd`.
    pub const fn new(gic: &'g Gicv3<M, C>, rd: usize) -> Self {
        Self { gic, rd }
    }
}

impl<M: Gicv3Mmio, C: CpuInterface> GicOps for Gicv3Local<'_, M, C> {
    fn enable(&self, intid: u32) {
        self.gic.enable(self.rd, intid);
    }
    fn disable(&self, intid: u32) {
        self.gic.disable(self.rd, intid);
    }
    fn is_enabled(&self, intid: u32) -> bool {
        self.gic.is_enabled(self.rd, intid)
    }
    fn is_edge_triggered(&self, intid: u32) -> bool {
        self.gic.is_edge_triggered(self.rd, intid)
    }
    fn set_edge_triggered(&self, intid: u32, edge: bool) {
        self.gic.set_edge_triggered(self.rd, intid, edge);
    }
    fn acknowledge(&self) -> Option<Acknowledged> {
        self.gic.acknowledge().map(|intid| Acknowledged {
            intid,
            token: intid,
        })
    }
    fn end_of_interrupt(&self, token: u32) {
        self.gic.end_of_interrupt(token);
    }
}

/// The policy layer over a GIC driver: every line is checked against
/// `max_intid` before a register is touched.
pub struct GicController<O> {
    gic: O,
    max_intid: u32,
    /// Serialises changes to the configuration registers lines share.
    config: SpinLock<()>,
}

/// A trigger [`GicController::set_trigger`] cannot give a line.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct TriggerRefused;

impl<O: GicOps> GicController<O> {
    /// A controller over `gic` admitting lines up to `max_intid`, clamped to
    /// [`MAX_INTID`].
    #[must_use]
    pub const fn new(gic: O, max_intid: u32) -> Self {
        let max_intid = if max_intid > MAX_INTID {
            MAX_INTID
        } else {
            max_intid
        };
        Self {
            gic,
            max_intid,
            config: SpinLock::new(()),
        }
    }

    /// Make SPI `line` edge- or level-triggered before it is first unmasked.
    /// A line already so configured is left as it is; any other changes
    /// only while disabled, as changing an enabled one is unpredictable.
    ///
    /// # Errors
    ///
    /// [`TriggerRefused`] for a line past the controller, a private
    /// interrupt, an enabled line, or one whose configuration did not take —
    /// a Secure line ignores the write.
    pub fn set_trigger(&self, line: u32, edge: bool) -> Result<(), TriggerRefused> {
        if !self.in_range(line) {
            return Err(TriggerRefused);
        }
        let _changing = self.config.lock();
        if self.gic.is_edge_triggered(line) == edge {
            return Ok(());
        }
        if line < MIN_SPI_INTID || self.gic.is_enabled(line) {
            return Err(TriggerRefused);
        }
        self.gic.set_edge_triggered(line, edge);
        if self.gic.is_edge_triggered(line) != edge {
            return Err(TriggerRefused);
        }
        Ok(())
    }

    /// The highest line admitted.
    #[must_use]
    pub const fn max_intid(&self) -> u32 {
        self.max_intid
    }

    const fn in_range(&self, intid: u32) -> bool {
        intid <= self.max_intid
    }
}

impl<O: GicOps + Send + Sync> tairix_arch_api::IrqController for GicController<O> {
    /// Disable `line`, then fence so the mask is visible before a waiter
    /// observes the fire (`docs/src/security/irq.md`).
    fn mask(&self, line: u32) -> Result<(), tairix_arch_api::IrqControlError> {
        if !self.in_range(line) {
            return Err(tairix_arch_api::IrqControlError::OutOfRange);
        }
        self.gic.disable(line);
        core::sync::atomic::fence(Ordering::SeqCst);
        Ok(())
    }

    fn unmask(&self, line: u32) -> Result<(), tairix_arch_api::IrqControlError> {
        if !self.in_range(line) {
            return Err(tairix_arch_api::IrqControlError::OutOfRange);
        }
        self.gic.enable(line);
        Ok(())
    }
}

impl<O: GicOps + Send + Sync> tairix_arch_api::InterruptEntry for GicController<O> {
    fn claim(&self) -> Option<u32> {
        self.gic.acknowledge().map(Acknowledged::token)
    }

    fn complete(&self, token: u32) {
        self.gic.end_of_interrupt(token);
    }
}

/// Why the GIC could not be brought up or asked something of.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum GicError {
    /// `init` ran already.
    AlreadyInitialised,
    /// `init` has not run.
    NotInitialised,
    /// The CPU named has no slot, or has not brought its interface up.
    UnknownCpu,
    /// A GICv3 was given no redistributor regions.
    NoRedistributors,
    /// What was asked needs a GICv3.
    NotGicv3,
    /// The GICv3 refused.
    V3(Gicv3Error),
}

impl GicError {
    /// The stable name an audit record gives the refusal.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AlreadyInitialised => "gic_already_initialised",
            Self::NotInitialised => "gic_not_initialised",
            Self::UnknownCpu => "gic_unknown_cpu",
            Self::NoRedistributors => "gic_no_redistributors",
            Self::NotGicv3 => "gic_not_gicv3",
            Self::V3(Gicv3Error::NotGicv3) => "gicv3_wrong_revision",
            Self::V3(Gicv3Error::Unresponsive) => "gicv3_unresponsive",
            Self::V3(Gicv3Error::NoSystemRegisters) => "gicv3_no_system_registers",
            Self::V3(Gicv3Error::NoRedistributor) => "gicv3_no_redistributor",
            Self::V3(Gicv3Error::Unaddressable) => "gicv3_unaddressable",
            Self::V3(Gicv3Error::NoLpis) => "gicv3_no_lpis",
            Self::V3(Gicv3Error::LpisInUse) => "gicv3_lpis_in_use",
        }
    }
}

impl From<Gicv3Error> for GicError {
    fn from(err: Gicv3Error) -> Self {
        Self::V3(err)
    }
}

#[cfg(any(test, all(target_arch = "aarch64", target_os = "none")))]
/// How the GIC addresses one CPU — a GICv2 CPU-interface mask, or a GICv3
/// affinity and redistributor — recorded by that CPU as its interface comes
/// up, so nothing assumes a CPU's dense id is its interface number.
pub struct GicCpu {
    target: AtomicU64,
    redistributor: AtomicUsize,
}

#[cfg(any(test, all(target_arch = "aarch64", target_os = "none")))]
/// Set once a [`GicCpu`] is recorded; free in both a GICv2 mask and an
/// affinity.
const TARGET_RECORDED: u64 = 1 << 63;

#[cfg(any(test, all(target_arch = "aarch64", target_os = "none")))]
impl GicCpu {
    /// A slot no CPU has recorded.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            target: AtomicU64::new(0),
            redistributor: AtomicUsize::new(0),
        }
    }

    fn record(&self, target: u64, redistributor: usize) {
        self.redistributor.store(redistributor, Ordering::Relaxed);
        self.target
            .store(target | TARGET_RECORDED, Ordering::Release);
    }

    fn target(&self) -> Option<u64> {
        let target = self.target.load(Ordering::Acquire);
        (target & TARGET_RECORDED != 0).then_some(target & !TARGET_RECORDED)
    }

    fn redistributor(&self) -> Option<usize> {
        self.target()?;
        Some(self.redistributor.load(Ordering::Relaxed))
    }
}

#[cfg(any(test, all(target_arch = "aarch64", target_os = "none")))]
impl Default for GicCpu {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(any(test, all(target_arch = "aarch64", target_os = "none")))]
/// What `init` needs beyond the discovered bases: a slot for every CPU,
/// indexed by dense id, and a GICv3's redistributor regions.
pub struct GicTopology {
    cpus: &'static [GicCpu],
    redistributors: &'static [RedistributorRegion],
    redistributor_stride: Option<u64>,
}

#[cfg(any(test, all(target_arch = "aarch64", target_os = "none")))]
impl GicTopology {
    /// `cpus` and no redistributors: a GICv2's whole topology.
    #[must_use]
    pub const fn new(cpus: &'static [GicCpu]) -> Self {
        Self {
            cpus,
            redistributors: &[],
            redistributor_stride: None,
        }
    }

    /// Add a GICv3's redistributor regions and the stride between
    /// redistributors where the firmware names one.
    #[must_use]
    pub const fn with_redistributors(
        self,
        regions: &'static [RedistributorRegion],
        stride: Option<u64>,
    ) -> Self {
        Self {
            redistributors: regions,
            redistributor_stride: stride,
            ..self
        }
    }

    fn cpu(&self, cpu: CpuId) -> Result<&GicCpu, GicError> {
        usize::try_from(cpu)
            .ok()
            .and_then(|index| self.cpus.get(index))
            .ok_or(GicError::UnknownCpu)
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "none"))]
static TOPOLOGY: OnceCell<GicTopology> = OnceCell::new();

#[cfg(all(target_arch = "aarch64", target_os = "none"))]
fn topology() -> Result<&'static GicTopology, GicError> {
    TOPOLOGY
        .get()
        .ok()
        .flatten()
        .ok_or(GicError::NotInitialised)
}

#[cfg(all(target_arch = "aarch64", target_os = "none"))]
/// The recorded target of `cpu`.
fn target_of(cpu: CpuId) -> Result<u64, GicError> {
    topology()?.cpu(cpu)?.target().ok_or(GicError::UnknownCpu)
}

#[cfg(all(target_arch = "aarch64", target_os = "none"))]
/// The GICv2 interface mask a recorded target holds.
fn interface_mask(target: u64) -> u8 {
    u8::try_from(target).unwrap_or(0)
}

/// Bare-metal [`GicMmio`] over the discovered GICv2 windows.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
pub struct VolatileGicMmio;

#[cfg(all(target_arch = "aarch64", target_os = "none"))]
impl GicMmio for VolatileGicMmio {
    fn gicd_read(&self, off: usize) -> u32 {
        // SAFETY: `off` is a register in the discovered distributor window,
        // identity-mapped Device memory the kernel alone drives.
        unsafe { core::ptr::read_volatile((current().0 + off) as *const u32) }
    }
    fn gicd_write(&self, off: usize, val: u32) {
        // SAFETY: as `gicd_read`, a 32-bit store.
        unsafe { core::ptr::write_volatile((current().0 + off) as *mut u32, val) }
    }
    fn gicd_write_byte(&self, off: usize, val: u8) {
        // SAFETY: as `gicd_read`; the priority and target registers are
        // byte-accessible.
        unsafe { core::ptr::write_volatile((current().0 + off) as *mut u8, val) }
    }
    fn gicc_read(&self, off: usize) -> u32 {
        // SAFETY: `off` is a register in the discovered CPU-interface
        // window, identity-mapped Device memory the kernel alone drives.
        unsafe { core::ptr::read_volatile((current().1 + off) as *const u32) }
    }
    fn gicc_write(&self, off: usize, val: u32) {
        // SAFETY: as `gicc_read`, a 32-bit store.
        unsafe { core::ptr::write_volatile((current().1 + off) as *mut u32, val) }
    }
    fn publish_barrier(&self) {
        // SAFETY: `dsb ishst` only orders this CPU's earlier stores before
        // the `GICD_SGIR` write; it touches no memory itself.
        unsafe {
            core::arch::asm!("dsb ishst", options(nostack, preserves_flags));
        }
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "none"))]
const fn v2() -> Gicv2<VolatileGicMmio> {
    Gicv2::new(VolatileGicMmio)
}

#[cfg(all(target_arch = "aarch64", target_os = "none"))]
const fn v3() -> Gicv3<crate::gicv3::VolatileGicv3Mmio, crate::gicv3::SystemCpuInterface> {
    Gicv3::new(
        crate::gicv3::VolatileGicv3Mmio,
        crate::gicv3::SystemCpuInterface,
    )
}

/// The calling CPU's recorded redistributor.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
fn local_redistributor() -> Option<usize> {
    redistributor_of(crate::smp::current_cpu_index()).ok()
}

/// `cpu`'s recorded redistributor.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
fn redistributor_of(cpu: CpuId) -> Result<usize, GicError> {
    topology()?
        .cpu(cpu)?
        .redistributor()
        .ok_or(GicError::UnknownCpu)
}

/// The redistributor a GICv3 operation on `intid` names: the calling CPU's
/// for a private interrupt, and none an SPI reads.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
fn redistributor_for(intid: u32) -> Option<usize> {
    if intid < MIN_SPI_INTID {
        local_redistributor()
    } else {
        Some(0)
    }
}

/// The production [`GicOps`]: the discovered GIC, as the calling CPU sees
/// it. A private interrupt of a CPU that has not brought its GICv3
/// interface up has no redistributor to name, and is left alone.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
pub struct ActiveGic;

#[cfg(all(target_arch = "aarch64", target_os = "none"))]
impl ActiveGic {
    fn with<R>(intid: u32, quiet: R, op: impl FnOnce(&dyn GicOps) -> R) -> R {
        match version() {
            GicVersion::V2 => op(&v2()),
            GicVersion::V3 => match redistributor_for(intid) {
                Some(rd) => op(&Gicv3Local::new(&v3(), rd)),
                None => quiet,
            },
        }
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "none"))]
impl GicOps for ActiveGic {
    fn enable(&self, intid: u32) {
        Self::with(intid, (), |gic| gic.enable(intid));
    }
    fn disable(&self, intid: u32) {
        Self::with(intid, (), |gic| gic.disable(intid));
    }
    fn is_enabled(&self, intid: u32) -> bool {
        Self::with(intid, false, |gic| gic.is_enabled(intid))
    }
    fn is_edge_triggered(&self, intid: u32) -> bool {
        Self::with(intid, false, |gic| gic.is_edge_triggered(intid))
    }
    fn set_edge_triggered(&self, intid: u32, edge: bool) {
        Self::with(intid, (), |gic| gic.set_edge_triggered(intid, edge));
    }
    fn acknowledge(&self) -> Option<Acknowledged> {
        acknowledge()
    }
    fn end_of_interrupt(&self, token: u32) {
        match version() {
            GicVersion::V2 => v2().end_of_interrupt(token),
            GicVersion::V3 => v3().end_of_interrupt(token),
        }
    }
}

/// Record the calling CPU's GIC addressing in its slot.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
fn record_self(target: u64, redistributor: usize) -> Result<(), GicError> {
    topology()?
        .cpu(crate::smp::current_cpu_index())?
        .record(target, redistributor);
    Ok(())
}

/// Bring the calling CPU's GICv3 redistributor and interface up and record
/// it.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
fn bring_up_v3_cpu(topology: &GicTopology) -> Result<(), GicError> {
    let gic = v3();
    let here = gic.local_affinity();
    let rd = gic
        .find_redistributor(topology.redistributors, topology.redistributor_stride, here)
        .ok_or(Gicv3Error::NoRedistributor)?;
    gic.init_redistributor(rd)?;
    gic.init_cpu_interface()?;
    record_self(here.routing(), rd)
}

/// Bring the distributor and the boot CPU's interface up, every SPI routed
/// to the boot CPU, and record the boot CPU in its slot.
///
/// # Errors
///
/// [`GicError::AlreadyInitialised`] on a second call;
/// [`GicError::UnknownCpu`] when the boot CPU has no slot;
/// [`GicError::NoRedistributors`] for a GICv3 given none, or the GICv3's
/// refusal.
///
/// # Safety
///
/// Once, on the boot CPU, before any interrupt source is enabled, with the
/// discovered windows identity-mapped as Device memory.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
pub unsafe fn init(topology: GicTopology) -> Result<(), GicError> {
    TOPOLOGY
        .set(topology)
        .map_err(|_| GicError::AlreadyInitialised)?;
    let topology = self::topology()?;
    match version() {
        GicVersion::V2 => {
            let gic = v2();
            gic.init_cpu_interface();
            gic.init_distributor();
            record_self(u64::from(gic.local_interface_mask()), 0)
        }
        GicVersion::V3 => {
            if topology.redistributors.is_empty() {
                return Err(GicError::NoRedistributors);
            }
            let gic = v3();
            gic.init_distributor(gic.local_affinity())?;
            bring_up_v3_cpu(topology)
        }
    }
}

/// Bring a secondary CPU's interface up and record it.
///
/// # Errors
///
/// [`GicError::NotInitialised`] before `init`, [`GicError::UnknownCpu`]
/// for a CPU with no slot, or the GICv3's refusal.
///
/// # Safety
///
/// Once, on the secondary itself, after `init` ran on the boot CPU.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
pub unsafe fn init_secondary() -> Result<(), GicError> {
    let topology = topology()?;
    match version() {
        GicVersion::V2 => {
            let gic = v2();
            gic.init_cpu_interface();
            record_self(u64::from(gic.local_interface_mask()), 0)
        }
        GicVersion::V3 => bring_up_v3_cpu(topology),
    }
}

/// Whether the calling CPU's redistributor among `regions`, stepped
/// `stride` apart, can take physical LPIs: a GICv3 whose distributor
/// implements them, the redistributor found and its LPIs left off. Asked
/// before `init`, by a boot deciding whether devices may raise LPIs.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
#[must_use]
pub fn local_lpis_available(regions: &[RedistributorRegion], stride: Option<u64>) -> bool {
    if version() != GicVersion::V3 {
        return false;
    }
    let gic = v3();
    gic.lpi_id_bits().is_some()
        && gic
            .find_redistributor(regions, stride, gic.local_affinity())
            .is_some_and(|rd| gic.lpis_available(rd))
}

/// The INTID bits LPIs may use: the GICv3 distributor's, where it implements
/// LPIs.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
#[must_use]
pub fn lpi_id_bits() -> Option<u32> {
    (version() == GicVersion::V3)
        .then(|| v3().lpi_id_bits())
        .flatten()
}

/// Point `cpu`'s redistributor at `tables` and turn its LPIs on, answering
/// whether its table reads snoop the CPU's caches.
///
/// # Errors
///
/// [`GicError::NotGicv3`], [`GicError::NotInitialised`],
/// [`GicError::UnknownCpu`] for a CPU that has not brought its interface
/// up, or the redistributor's refusal.
///
/// # Safety
///
/// Once per redistributor, `tables` the kernel's for its life: the
/// redistributor reads and writes them from now on.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
pub unsafe fn enable_lpis(cpu: CpuId, tables: crate::gicv3::LpiTables) -> Result<bool, GicError> {
    if version() != GicVersion::V3 {
        return Err(GicError::NotGicv3);
    }
    let rd = redistributor_of(cpu)?;
    Ok(v3().enable_lpis(rd, tables)?)
}

/// `cpu`'s redistributor as an interrupt translation service's collection
/// names it: by physical address where the service takes addresses, else by
/// processor number.
///
/// # Errors
///
/// As [`enable_lpis`].
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
pub fn collection_target(cpu: CpuId, physical: bool) -> Result<u64, GicError> {
    if version() != GicVersion::V3 {
        return Err(GicError::NotGicv3);
    }
    Ok(v3().collection_target(redistributor_of(cpu)?, physical))
}

/// Give interrupt `intid` the mid-range priority and enable it; a private
/// interrupt on the calling CPU.
///
/// # Safety
///
/// After the calling CPU's `init` or `init_secondary`; enabling a line
/// lets it reach a CPU once its source is armed.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
pub unsafe fn enable_ppi(intid: u32) {
    ActiveGic.enable(intid);
}

/// Set `intid`'s priority; a private interrupt's on the calling CPU.
///
/// # Safety
///
/// As `enable_ppi`.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
pub unsafe fn set_ppi_priority(intid: u32, priority: u8) {
    match version() {
        GicVersion::V2 => v2().set_priority(intid, priority),
        GicVersion::V3 => {
            if let Some(rd) = redistributor_for(intid) {
                v3().set_priority(rd, intid, priority);
            }
        }
    }
}

/// Route SPI `intid` to `target`.
///
/// # Errors
///
/// [`GicError::UnknownCpu`] for a CPU that has not brought its interface
/// up, or [`GicError::NotInitialised`].
///
/// # Safety
///
/// After `init`.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
pub unsafe fn route_spi(intid: u32, target: CpuId) -> Result<(), GicError> {
    let to = target_of(target)?;
    match version() {
        GicVersion::V2 => v2().route_spi(intid, interface_mask(to)),
        GicVersion::V3 => v3().route_spi(intid, Affinity::of_mpidr(to)),
    }
    Ok(())
}

/// Acknowledge the highest-priority pending interrupt, or [`None`] when
/// nothing is pending.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
#[must_use]
pub fn acknowledge() -> Option<Acknowledged> {
    match version() {
        GicVersion::V2 => v2().acknowledge(),
        GicVersion::V3 => Gicv3Local::new(&v3(), 0).acknowledge(),
    }
}

/// End an interrupt `acknowledge` returned.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
pub fn end_of_interrupt(acknowledged: Acknowledged) {
    ActiveGic.end_of_interrupt(acknowledged.token());
}

/// Raise the reschedule SGI ([`crate::preempt::IPI_SGI`]) on `target`.
///
/// # Errors
///
/// [`GicError::UnknownCpu`] for a CPU that has not brought its interface
/// up, [`GicError::NotInitialised`], or the GICv3's refusal of an affinity
/// its SGIs cannot address.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
pub fn send_sgi(target: CpuId) -> Result<(), GicError> {
    let to = target_of(target)?;
    match version() {
        GicVersion::V2 => v2().send_sgi(crate::preempt::IPI_SGI, interface_mask(to)),
        GicVersion::V3 => v3().send_sgi(crate::preempt::IPI_SGI, Affinity::of_mpidr(to))?,
    }
    Ok(())
}

/// The lowest SPI stuck active, or pending while enabled.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
#[must_use]
pub fn stuck_spi() -> Option<StuckInterrupt> {
    match version() {
        GicVersion::V2 => v2().stuck_spi(MAX_INTID),
        GicVersion::V3 => v3().stuck_spi(MAX_INTID),
    }
}

/// Acknowledge the FIQ being taken: through `GICC_IAR` on a GICv2, whose
/// self-sample route sets `AckCtl`, and `ICC_IAR0_EL1` on a GICv3.
#[cfg(all(
    target_arch = "aarch64",
    target_os = "none",
    feature = "watchdog-diagnostics"
))]
#[must_use]
pub fn acknowledge_fiq() -> Option<Acknowledged> {
    match version() {
        GicVersion::V2 => v2().acknowledge(),
        GicVersion::V3 => v3().acknowledge_fiq().map(|intid| Acknowledged {
            intid,
            token: intid,
        }),
    }
}

/// End a FIQ `acknowledge_fiq` returned.
#[cfg(all(
    target_arch = "aarch64",
    target_os = "none",
    feature = "watchdog-diagnostics"
))]
pub fn end_of_fiq(acknowledged: Acknowledged) {
    match version() {
        GicVersion::V2 => v2().end_of_interrupt(acknowledged.token()),
        GicVersion::V3 => v3().end_of_fiq(acknowledged.token()),
    }
}

/// What `route_selfsample_fiq` changes, saved so a probe that found FIQ
/// undelivered can put it back.
#[cfg(all(
    target_arch = "aarch64",
    target_os = "none",
    feature = "watchdog-diagnostics"
))]
#[derive(Copy, Clone, Debug)]
pub enum SelfSampleRoute {
    /// A GICv2's CPU-interface and distributor control.
    V2 {
        /// `GICC_CTLR`.
        cpu: u32,
        /// `GICD_CTLR`.
        distributor: u32,
    },
    /// A GICv3's distributor control and Group 0 enable.
    V3(crate::gicv3::FiqRoute),
}

/// Whether this kernel may route an interrupt as FIQ: a GICv3 with two
/// Security states keeps Group 0 for the Secure world, whose firmware traps
/// this level's accesses to its controls.
///
/// # Safety
///
/// After the calling CPU's `init`.
#[cfg(all(
    target_arch = "aarch64",
    target_os = "none",
    feature = "watchdog-diagnostics"
))]
#[must_use]
pub unsafe fn selfsample_fiq_reachable() -> bool {
    match version() {
        GicVersion::V2 => true,
        GicVersion::V3 => v3().single_security_state(),
    }
}

/// The routing state `route_selfsample_fiq` would change.
///
/// # Safety
///
/// After the calling CPU's `init`.
#[cfg(all(
    target_arch = "aarch64",
    target_os = "none",
    feature = "watchdog-diagnostics"
))]
#[must_use]
pub unsafe fn selfsample_route() -> SelfSampleRoute {
    match version() {
        GicVersion::V2 => {
            let gic = v2();
            SelfSampleRoute::V2 {
                cpu: gic.read_gicc_ctlr(),
                distributor: gic.read_gicd_ctlr(),
            }
        }
        GicVersion::V3 => SelfSampleRoute::V3(v3().fiq_route()),
    }
}

/// Deliver the calling CPU's private interrupt `fiq_intid` alone as FIQ.
///
/// # Errors
///
/// [`GicError::UnknownCpu`] for a GICv3 CPU with no recorded
/// redistributor, or the GICv3's refusal.
///
/// # Safety
///
/// After the calling CPU's `init` or `init_secondary`.
#[cfg(all(
    target_arch = "aarch64",
    target_os = "none",
    feature = "watchdog-diagnostics"
))]
pub unsafe fn route_selfsample_fiq(fiq_intid: u32) -> Result<(), GicError> {
    match version() {
        GicVersion::V2 => v2().route_selfsample_fiq(fiq_intid),
        GicVersion::V3 => {
            let rd = local_redistributor().ok_or(GicError::UnknownCpu)?;
            v3().route_fiq(rd, fiq_intid)?;
        }
    }
    Ok(())
}

/// Put `fiq_intid` back in Group 1 and the routing state `saved` names back
/// in force.
///
/// # Errors
///
/// As `route_selfsample_fiq`.
///
/// # Safety
///
/// As `route_selfsample_fiq`.
#[cfg(all(
    target_arch = "aarch64",
    target_os = "none",
    feature = "watchdog-diagnostics"
))]
pub unsafe fn restore_selfsample(fiq_intid: u32, saved: SelfSampleRoute) -> Result<(), GicError> {
    match saved {
        SelfSampleRoute::V2 { cpu, distributor } => {
            let gic = v2();
            gic.set_group(fiq_intid, false);
            gic.write_gicc_ctlr(cpu);
            gic.write_gicd_ctlr(distributor);
        }
        SelfSampleRoute::V3(route) => {
            let rd = local_redistributor().ok_or(GicError::UnknownCpu)?;
            v3().restore_fiq(rd, fiq_intid, route)?;
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "gic_tests.rs"]
mod tests;
