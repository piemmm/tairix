//! BCM2711 PCIe root-complex MSI controller (Raspberry Pi 4).
//!
//! The root complex captures an endpoint's message write at a doorbell of its
//! own and demultiplexes up to 32 vectors onto one shared GIC SPI, the second
//! `interrupts` entry of its `brcm,bcm2711-pcie` node. When that SPI fires the
//! kernel reads which vectors are pending, services each and clears its
//! status, or the level-sensitive SPI re-asserts for ever.
//!
//! [`BrcmMsiController`] is this controller as the Arch HAL
//! [`MsiMultiplexer`] slice, and the binary reaches it only through
//! `crate::msimux`, so it never names the part. The register sequencing is
//! host-tested through the [`MsiMmio`] seam; the freestanding
//! `VolatileMsiMmio` reaches the discovered register base, never a board
//! constant. The user-space `drivers/bus/pcie_brcm` releases the block from
//! reset, trains the link and enumerates the endpoint over disjoint registers.
//!
//! # Hardware constants
//!
//! The MSI address (bit 0 its enable), data-config and status, clear and
//! mask-clear offsets, the data pattern and the above-4 GiB doorbell are the
//! ones FreeBSD's ISC-licensed `bcm2838_pci.c` programs; the below-4 GiB
//! doorbell and the mask-set offset await metal verification (QEMU models no
//! Pi PCIe).

use core::num::NonZeroU32;
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
use core::sync::atomic::AtomicUsize;
use core::sync::atomic::{fence, AtomicU32, Ordering};

use tairix_abi::MsiMessage;
use tairix_arch_api::{IrqControlError, IrqController, MsiMultiplexer, MsiMuxError, MsiMuxShape};
use tairix_sync::{Once, OnceCell};

/// Number of distinct MSI message vectors the BCM2711 root-complex MSI
/// controller demultiplexes onto its one shared GIC SPI. A hardware fact
/// (the width of the `INTR2` status/mask registers), not a tunable
/// capacity.
pub const NUM_MSI_VECTORS: u32 = 32;

/// Root-complex register base in effect before discovery runs (`0`, an
/// invalid base): the BCM2711 PCIe register block has no fail-safe default
/// — there is no second board to fall back to — so an access before
/// [`configure`] resolves the real base from the device tree targets the
/// null page and faults closed rather than poking a fabricated address.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
const NO_BASE: usize = 0;

/// Currently-selected root-complex register base, resolved from the
/// discovered `brcm,bcm2711-pcie` node at boot ([`configure`]). The MSI
/// registers below are accessed at fixed offsets from it.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
static RC_BASE: AtomicUsize = AtomicUsize::new(NO_BASE);

/// Low half of the currently selected MSI doorbell target. The target depends
/// on the discovered inbound PCIe aperture size, so boot configures it beside
/// the register base before the first MSI allocation.
static MSI_TARGET_LO: AtomicU32 = AtomicU32::new(msi_target_halves(MSI_TARGET_ADDR_LT_4GB).0);

/// High half of the currently selected MSI doorbell target; see
/// [`MSI_TARGET_LO`].
static MSI_TARGET_HI: AtomicU32 = AtomicU32::new(msi_target_halves(MSI_TARGET_ADDR_LT_4GB).1);

/// Point the MSI controller at the discovered root-complex register base and
/// select the doorbell target matching the discovered inbound aperture.
///
/// Called once, by [`configure_from_fdt`], before discovery publishes the
/// controller. `Release`/`Acquire` pairs the stores with [`current`] and
/// [`current_msi_target`]'s loads so the freestanding MMIO path observes the
/// resolved base and target.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
fn configure(rc_base: usize, inbound_size: u64) {
    let target = msi_target_for_inbound_size(inbound_size);
    let (target_lo, target_hi) = msi_target_halves(target);
    MSI_TARGET_LO.store(target_lo, Ordering::Release);
    MSI_TARGET_HI.store(target_hi, Ordering::Release);
    RC_BASE.store(rc_base, Ordering::Release);
}

/// The root-complex register base currently in effect, or `0` before
/// [`configure`] has run.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
#[must_use]
fn current() -> usize {
    RC_BASE.load(Ordering::Acquire)
}

/// The MSI doorbell target selected from the discovered inbound aperture.
#[must_use]
pub fn current_msi_target() -> u64 {
    (u64::from(MSI_TARGET_HI.load(Ordering::Acquire)) << 32)
        | u64::from(MSI_TARGET_LO.load(Ordering::Acquire))
}

// --- Register offsets (from the RC register base) -------------------------
//
// The MSI doorbell BAR config and the per-vector INTR2 interrupt block.

/// The low 32 bits of the doorbell target
/// address an endpoint's MSI write must hit, OR'd with the enable bit
/// ([`MSI_BAR_CONFIG_LO_ENABLE`]).
pub const MSI_BAR_CONFIG_LO: usize = 0x4044;

/// The high 32 bits of the doorbell target
/// address.
pub const MSI_BAR_CONFIG_HI: usize = 0x4048;

/// The expected MSI data pattern; the
/// low 16 bits ([`MSI_DATA_MAGIC`]) are the base the per-vector data word is
/// OR'd into.
pub const MSI_DATA_CONFIG: usize = 0x404c;

/// Enable bit of [`MSI_BAR_CONFIG_LO`]: set so the controller captures MSI
/// writes to the doorbell address.
pub const MSI_BAR_CONFIG_LO_ENABLE: u32 = 1 << 0;

/// [`MSI_DATA_CONFIG`] value for the 32-vector configuration: the
/// controller matches MSI writes
/// whose data is [`MSI_DATA_MAGIC`] OR the 5-bit vector index.
pub const MSI_DATA_CONFIG_VAL_32: u32 = 0xffe0_6540;

/// Low-16-bit magic of the MSI data word ([`MSI_DATA_CONFIG_VAL_32`] `&
/// 0xffff`): an endpoint's MSI Data register is programmed with this OR
/// the vector index, so the controller routes the write to that vector.
pub const MSI_DATA_MAGIC: u32 = 0x6540;

/// Threshold at which the BCM2711 root complex uses the high MSI target.
pub const MSI_TARGET_GT_4GB_THRESHOLD: u64 = 0x1_0000_0000;

/// Doorbell target used when the inbound aperture is smaller than 4 GiB.
/// It is an RC-internal capture address, not
/// RAM.
pub const MSI_TARGET_ADDR_LT_4GB: u64 = 0x0000_0000_FFFF_FFFC;

/// Doorbell target used when the inbound aperture is at least 4 GiB.
/// Pi 4 firmware commonly exposes an 8 GiB
/// inbound aperture, so using the below-4GiB target there leaves endpoint MSI
/// writes visible to xHCI but invisible to the root-complex INTR2 status.
pub const MSI_TARGET_ADDR_GT_4GB: u64 = 0x0000_000F_FFFF_FFFC;

/// Select the BCM2711 MSI doorbell target for the discovered inbound aperture.
#[must_use]
pub const fn msi_target_for_inbound_size(inbound_size: u64) -> u64 {
    if inbound_size >= MSI_TARGET_GT_4GB_THRESHOLD {
        MSI_TARGET_ADDR_GT_4GB
    } else {
        MSI_TARGET_ADDR_LT_4GB
    }
}

const fn msi_target_halves(target: u64) -> (u32, u32) {
    let bytes = target.to_le_bytes();
    (
        u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
    )
}

/// Base of the per-vector `INTR2` interrupt block within the RC register
/// window: a standard brcmstb level-2 interrupt
/// controller (status / clear / mask-set / mask-clear), one bit per vector.
pub const INTR2_BASE: usize = 0x4500;

/// `INTR2` interrupt-status register (offset from [`INTR2_BASE`]): bit `v`
/// set means MSI vector `v` is pending.
pub const INTR2_STATUS: usize = 0x00;

/// `INTR2` interrupt-clear register: write bit `v` (write-1-to-clear) to
/// clear vector `v`'s pending status after servicing it.
pub const INTR2_CLR: usize = 0x08;

/// `INTR2` mask-set register: write bit `v` to **mask** (disable) vector
/// `v` — the mask-before-wake step before a waiter observes the wake.
pub const INTR2_MASK_SET: usize = 0x10;

/// `INTR2` mask-clear register: write bit `v` to **unmask** (enable) vector
/// `v` — the re-arm after a driver has serviced a completion.
pub const INTR2_MASK_CLR: usize = 0x14;

/// How much of the discovered register window the controller reaches: up to
/// and including its last vector register.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
const REGISTERS_LEN: u64 = (INTR2_BASE + INTR2_MASK_CLR) as u64 + 4;

/// All-ones word: mask or clear every vector at once during init.
const ALL_VECTORS: u32 = 0xFFFF_FFFF;

/// The message a function's MSI capability is programmed with to raise
/// `vector`: the doorbell selected from the discovered inbound aperture, and
/// [`MSI_DATA_MAGIC`] OR the vector, the pattern [`MSI_DATA_CONFIG_VAL_32`]
/// matches.
#[must_use]
pub fn msi_message(vector: u32) -> MsiMessage {
    msi_message_for_target(vector, current_msi_target())
}

/// [`msi_message`] for an explicitly selected doorbell target.
#[must_use]
pub const fn msi_message_for_target(vector: u32, target: u64) -> MsiMessage {
    MsiMessage {
        address: target,
        data: MSI_DATA_MAGIC | (vector & (NUM_MSI_VECTORS - 1)),
    }
}

/// `true` iff `vector` is a valid MSI vector index (`0..NUM_MSI_VECTORS`).
#[must_use]
pub const fn vector_in_range(vector: u32) -> bool {
    vector < NUM_MSI_VECTORS
}

/// MMIO access to the root-complex MSI registers.
///
/// The seam the pure controller logic is written against: the freestanding
/// `VolatileMsiMmio` performs real reads/writes at the discovered base,
/// while host tests drive an in-memory model, so the register sequencing is
/// unit-tested without hardware.
pub trait MsiMmio {
    /// Read the 32-bit register at byte `offset` from the RC base.
    fn read32(&self, offset: usize) -> u32;
    /// Write `value` to the 32-bit register at byte `offset` from the RC
    /// base.
    fn write32(&self, offset: usize, value: u32);
}

/// The BCM2711 root-complex MSI controller over an [`MsiMmio`] backend.
pub struct BrcmMsi<M: MsiMmio> {
    mmio: M,
}

impl<M: MsiMmio> BrcmMsi<M> {
    /// Wrap an [`MsiMmio`] backend.
    #[must_use]
    pub const fn new(mmio: M) -> Self {
        Self { mmio }
    }

    /// Open the block: unmask and clear every vector, then program the
    /// doorbell target (enabled) and the data pattern. Every vector is open
    /// from here, so a message raised before its driver waits fires, is
    /// masked by that fire, and is acknowledged. Only once the root complex
    /// is out of reset.
    pub fn init(&self) {
        self.init_with_target(current_msi_target());
    }

    /// Initialise the controller with an explicitly selected MSI doorbell
    /// target. Host tests exercise both hardware aperture shapes through this
    /// entry point; production uses [`init`](Self::init), which reads the
    /// target selected by `configure`.
    pub fn init_with_target(&self, target: u64) {
        let (target_lo, target_hi) = msi_target_halves(target);
        // Clear any stale pending status a prior owner (firmware) left
        // before enabling the doorbell.
        self.mmio.write32(INTR2_BASE + INTR2_MASK_CLR, ALL_VECTORS);
        self.mmio.write32(INTR2_BASE + INTR2_CLR, ALL_VECTORS);
        // Program the doorbell target address (low half carries the enable
        // bit) and the data-config pattern the per-vector data matches.
        self.mmio
            .write32(MSI_BAR_CONFIG_LO, target_lo | MSI_BAR_CONFIG_LO_ENABLE);
        self.mmio.write32(MSI_BAR_CONFIG_HI, target_hi);
        self.mmio.write32(MSI_DATA_CONFIG, MSI_DATA_CONFIG_VAL_32);
    }

    /// Mask (disable delivery of) MSI `vector`. A no-op for an
    /// out-of-range vector (fail closed — never touch a foreign bit).
    pub fn mask(&self, vector: u32) {
        if vector_in_range(vector) {
            self.mmio.write32(INTR2_BASE + INTR2_MASK_SET, 1 << vector);
        }
    }

    /// Unmask (re-arm) MSI `vector` for the next interrupt. A no-op for an
    /// out-of-range vector.
    pub fn unmask(&self, vector: u32) {
        if vector_in_range(vector) {
            self.mmio.write32(INTR2_BASE + INTR2_MASK_CLR, 1 << vector);
        }
    }

    /// Clear MSI `vector`'s pending status after it has been serviced, so
    /// the level-sensitive shared GIC SPI deasserts. A no-op for an
    /// out-of-range vector.
    pub fn clear(&self, vector: u32) {
        if vector_in_range(vector) {
            self.mmio.write32(INTR2_BASE + INTR2_CLR, 1 << vector);
        }
    }

    /// Read the pending-vector bitmap (`INTR2 STATUS`): bit `v` set means
    /// MSI vector `v` fired and is awaiting service.
    #[must_use]
    pub fn pending(&self) -> u32 {
        self.mmio.read32(INTR2_BASE + INTR2_STATUS)
    }
}

/// Iterate the vector indices set in a pending-status bitmap, lowest first.
///
/// A pure function so the fan-out order is unit-tested without hardware.
pub fn pending_vectors(status: u32) -> impl Iterator<Item = u32> {
    (0..NUM_MSI_VECTORS).filter(move |v| status & (1 << v) != 0)
}

/// [`BrcmMsi`] as the Arch HAL [`MsiMultiplexer`]: discovery records the
/// parent INTID, and [`MsiMultiplexer::enable`] opens the block once.
///
/// It reaches the block only once discovery recorded the parent, after the
/// register base was published, and the vector registers only once the block
/// is enabled: before that it captures nothing, so there is nothing to mask,
/// unmask or drain.
pub struct BrcmMsiController<M: MsiMmio> {
    msi: BrcmMsi<M>,
    parent: OnceCell<u32>,
    enabled: Once<()>,
}

impl<M: MsiMmio> BrcmMsiController<M> {
    /// A controller over `mmio`, undiscovered until its parent is recorded.
    #[must_use]
    pub const fn new(mmio: M) -> Self {
        Self {
            msi: BrcmMsi::new(mmio),
            parent: OnceCell::new(),
            enabled: Once::new(),
        }
    }

    fn parent_intid(&self) -> Option<u32> {
        self.parent.get().ok().flatten().copied()
    }

    fn is_enabled(&self) -> bool {
        matches!(self.enabled.get(), Ok(Some(())))
    }

    fn admit(&self, vector: u32) -> Result<(), IrqControlError> {
        if self.parent_intid().is_some() && vector_in_range(vector) {
            Ok(())
        } else {
            Err(IrqControlError::OutOfRange)
        }
    }
}

impl<M: MsiMmio + Send + Sync> IrqController for BrcmMsiController<M> {
    fn mask(&self, vector: u32) -> Result<(), IrqControlError> {
        self.admit(vector)?;
        if self.is_enabled() {
            self.msi.mask(vector);
            // The masked state is visible before a waiter observes the fire.
            fence(Ordering::SeqCst);
        }
        Ok(())
    }

    fn unmask(&self, vector: u32) -> Result<(), IrqControlError> {
        self.admit(vector)?;
        if self.is_enabled() {
            self.msi.unmask(vector);
        }
        Ok(())
    }
}

impl<M: MsiMmio + Send + Sync> MsiMultiplexer for BrcmMsiController<M> {
    fn shape(&self) -> Option<MsiMuxShape> {
        Some(MsiMuxShape {
            vectors: NonZeroU32::new(NUM_MSI_VECTORS)?,
            parent_line: self.parent_intid()?,
        })
    }

    fn enable(&self) -> Result<(), MsiMuxError> {
        if self.parent_intid().is_none() {
            return Err(MsiMuxError::NotSupported);
        }
        if self.is_enabled() {
            return Ok(());
        }
        match self.enabled.call_once_infallible(|| self.msi.init()) {
            Ok(&()) => Ok(()),
            Err(_) => Err(MsiMuxError::NotSupported),
        }
    }

    fn message(&self, vector: u32) -> Result<MsiMessage, MsiMuxError> {
        if self.parent_intid().is_none() {
            return Err(MsiMuxError::NotSupported);
        }
        if !vector_in_range(vector) {
            return Err(MsiMuxError::OutOfRange);
        }
        Ok(msi_message(vector))
    }

    fn drain(&self, fire: &mut dyn FnMut(u32)) {
        if !self.is_enabled() {
            return;
        }
        for vector in pending_vectors(self.msi.pending()) {
            fire(vector);
            self.msi.clear(vector);
        }
    }
}

/// Bare-metal [`MsiMmio`] over the **discovered** root-complex register
/// base ([`current`]).
///
/// A zero-sized handle, like [`crate::gic::VolatileGicMmio`]: each access
/// reads the base [`configure`] resolved from the device tree, so the
/// driver always targets the discovered window and the handle is
/// const-constructible (it can live in a `static`). Compiled only for the
/// freestanding aarch64 target; host builds use the test model.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
pub struct VolatileMsiMmio;

#[cfg(all(target_arch = "aarch64", target_os = "none"))]
impl MsiMmio for VolatileMsiMmio {
    fn read32(&self, offset: usize) -> u32 {
        // SAFETY: `offset` is one of this module's register offsets, all
        // within the first `REGISTERS_LEN` bytes of the discovered window,
        // which `configure_from_fdt` checked the window spans and the
        // identity map types Device; `current()` is that window's base,
        // published before the controller reaches the block.
        unsafe { core::ptr::read_volatile((current() + offset) as *const u32) }
    }
    fn write32(&self, offset: usize, value: u32) {
        // SAFETY: as `read32`, but a 32-bit store.
        unsafe { core::ptr::write_volatile((current() + offset) as *mut u32, value) }
    }
}

/// The one root complex discovery finds.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
static CONTROLLER: BrcmMsiController<VolatileMsiMmio> = BrcmMsiController::new(VolatileMsiMmio);

/// Point [`CONTROLLER`] at the `brcm,bcm2711-pcie` node `fdt` describes and
/// record the INTID its MSI controller raises; [`None`] for a tree with no
/// such node, or whose register window does not hold the controller's
/// registers in Device-typed identity gigapages. Once, with the MMU on: the
/// base and the parent are published through atomics.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
pub fn configure_from_fdt(fdt: &crate::fdt::Fdt<'_>) -> Option<&'static dyn MsiMultiplexer> {
    let pcie = crate::platform::pcie_bringup(fdt)?;
    let parent = crate::platform::pcie_msi_intid(fdt)?;
    if pcie.regs_len < REGISTERS_LEN
        || !crate::paging::identity_device_covers(pcie.regs_phys, REGISTERS_LEN)
    {
        return None;
    }
    configure(usize::try_from(pcie.regs_phys).ok()?, pcie.inbound_size);
    CONTROLLER.parent.set(parent).ok()?;
    Some(&CONTROLLER)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::vec::Vec;
    use tairix_arch_api::msimux::conformance;

    const STATUS: usize = INTR2_BASE + INTR2_STATUS;
    const CLEAR: usize = INTR2_BASE + INTR2_CLR;
    const MASK_SET: usize = INTR2_BASE + INTR2_MASK_SET;
    const MASK_CLEAR: usize = INTR2_BASE + INTR2_MASK_CLR;
    /// Where a drain's `fire` is logged among the writes.
    const FIRED: usize = usize::MAX;
    const PARENT: u32 = 148;

    /// The block over atomics: status latches what `raise` sets whatever the
    /// mask and clears where ones are written, the mask sets and clears
    /// through its two registers, and every write is logged in order.
    #[derive(Default)]
    struct Block {
        status: AtomicU32,
        mask: AtomicU32,
        writes: Mutex<Vec<(usize, u32)>>,
    }

    impl Block {
        fn raise(&self, vector: u32) {
            self.status.fetch_or(1 << vector, Ordering::SeqCst);
        }

        fn log(&self, offset: usize, value: u32) {
            self.writes.lock().unwrap().push((offset, value));
        }

        fn writes(&self) -> Vec<(usize, u32)> {
            self.writes.lock().unwrap().clone()
        }

        fn wrote(&self, offset: usize, value: u32) -> bool {
            self.writes().contains(&(offset, value))
        }
    }

    impl MsiMmio for Block {
        fn read32(&self, offset: usize) -> u32 {
            match offset {
                STATUS => self.status.load(Ordering::SeqCst),
                _ => 0,
            }
        }

        fn write32(&self, offset: usize, value: u32) {
            self.log(offset, value);
            match offset {
                CLEAR => {
                    self.status.fetch_and(!value, Ordering::SeqCst);
                }
                MASK_SET => {
                    self.mask.fetch_or(value, Ordering::SeqCst);
                }
                MASK_CLEAR => {
                    self.mask.fetch_and(!value, Ordering::SeqCst);
                }
                _ => {}
            }
        }
    }

    /// A controller discovery found.
    fn discovered() -> BrcmMsiController<Block> {
        let controller = BrcmMsiController::new(Block::default());
        assert!(controller.parent.set(PARENT).is_ok());
        controller
    }

    #[test]
    fn passes_msi_multiplexer_conformance() {
        let controller = discovered();
        conformance::run_discovered(&controller, &mut |vector| {
            controller.msi.mmio.raise(vector);
        });
    }

    #[test]
    fn undiscovered_multiplexer_fails_closed() {
        let controller = BrcmMsiController::new(Block::default());
        conformance::run_undiscovered(&controller);
        assert!(controller.msi.mmio.writes().is_empty());
    }

    #[test]
    fn drain_acknowledges_each_vector_after_firing_it() {
        let controller = discovered();
        assert_eq!(controller.enable(), Ok(()));
        let enabled = controller.msi.mmio.writes().len();
        controller.msi.mmio.raise(5);
        controller.msi.mmio.raise(2);
        controller.drain(&mut |vector| controller.msi.mmio.log(FIRED, vector));
        assert_eq!(
            controller.msi.mmio.writes()[enabled..],
            [(FIRED, 2), (CLEAR, 1 << 2), (FIRED, 5), (CLEAR, 1 << 5)]
        );
        assert_eq!(controller.msi.pending(), 0);
    }

    #[test]
    fn enable_opens_the_block_once() {
        let controller = discovered();
        let block = &controller.msi.mmio;
        assert_eq!(controller.enable(), Ok(()));
        assert!(block.wrote(MSI_DATA_CONFIG, MSI_DATA_CONFIG_VAL_32));
        let enabled = block.writes().len();
        assert_eq!(controller.enable(), Ok(()));
        assert_eq!(
            block.writes().len(),
            enabled,
            "a second enable touches nothing"
        );
    }

    #[test]
    fn nothing_is_touched_before_enable() {
        let controller = discovered();
        controller.msi.mmio.raise(3);
        assert_eq!(controller.mask(3), Ok(()));
        assert_eq!(controller.unmask(3), Ok(()));
        let mut fired = 0;
        controller.drain(&mut |_| fired += 1);
        assert_eq!(fired, 0);
        assert!(controller.msi.mmio.writes().is_empty());
    }

    #[test]
    fn msi_message_encodes_target_and_vector() {
        // The data is the magic OR the vector index, matching the programmed
        // data-config pattern; the address comes from the discovered aperture.
        for (vector, data) in [
            (0, MSI_DATA_MAGIC),
            (1, MSI_DATA_MAGIC | 1),
            (31, MSI_DATA_MAGIC | 31),
        ] {
            assert_eq!(
                msi_message_for_target(vector, MSI_TARGET_ADDR_GT_4GB),
                MsiMessage {
                    address: MSI_TARGET_ADDR_GT_4GB,
                    data
                }
            );
        }
    }

    #[test]
    fn msi_target_follows_discovered_inbound_aperture() {
        assert_eq!(msi_target_for_inbound_size(0), MSI_TARGET_ADDR_LT_4GB);
        assert_eq!(
            msi_target_for_inbound_size(MSI_TARGET_GT_4GB_THRESHOLD - 1),
            MSI_TARGET_ADDR_LT_4GB
        );
        assert_eq!(
            msi_target_for_inbound_size(MSI_TARGET_GT_4GB_THRESHOLD),
            MSI_TARGET_ADDR_GT_4GB
        );
        assert_eq!(
            msi_target_for_inbound_size(0x2_0000_0000),
            MSI_TARGET_ADDR_GT_4GB
        );
    }

    #[test]
    fn init_unmasks_clears_then_enables_the_below_4g_doorbell() {
        let msi = BrcmMsi::new(Block::default());
        msi.init_with_target(MSI_TARGET_ADDR_LT_4GB);
        let m = &msi.mmio;
        // Every supported vector unmasked and cleared.
        assert!(m.wrote(MASK_CLEAR, ALL_VECTORS));
        assert!(m.wrote(CLEAR, ALL_VECTORS));
        // Doorbell target programmed with the enable bit, and the data
        // pattern set.
        assert!(m.wrote(MSI_BAR_CONFIG_LO, 0xFFFF_FFFC | MSI_BAR_CONFIG_LO_ENABLE));
        assert!(m.wrote(MSI_BAR_CONFIG_HI, 0));
        assert!(m.wrote(MSI_DATA_CONFIG, MSI_DATA_CONFIG_VAL_32));
    }

    #[test]
    fn init_programs_the_high_doorbell_for_large_inbound_apertures() {
        let msi = BrcmMsi::new(Block::default());
        msi.init_with_target(MSI_TARGET_ADDR_GT_4GB);
        let m = &msi.mmio;
        assert!(m.wrote(MSI_BAR_CONFIG_LO, 0xFFFF_FFFC | MSI_BAR_CONFIG_LO_ENABLE));
        assert!(m.wrote(MSI_BAR_CONFIG_HI, 0xF));
        assert!(m.wrote(MSI_DATA_CONFIG, MSI_DATA_CONFIG_VAL_32));
    }

    #[test]
    fn mask_unmask_clear_touch_only_the_addressed_vector() {
        let msi = BrcmMsi::new(Block::default());
        msi.mask(5);
        msi.unmask(5);
        msi.clear(5);
        let m = &msi.mmio;
        assert!(m.wrote(MASK_SET, 1 << 5));
        assert!(m.wrote(MASK_CLEAR, 1 << 5));
        assert!(m.wrote(CLEAR, 1 << 5));
    }

    #[test]
    fn out_of_range_vector_writes_nothing_fail_closed() {
        let msi = BrcmMsi::new(Block::default());
        msi.mask(NUM_MSI_VECTORS);
        msi.unmask(NUM_MSI_VECTORS + 10);
        msi.clear(u32::MAX);
        assert!(msi.mmio.writes().is_empty());
    }

    #[test]
    fn pending_reads_the_status_register() {
        let msi = BrcmMsi::new(Block::default());
        msi.mmio.status.store(0b1010, Ordering::SeqCst);
        assert_eq!(msi.pending(), 0b1010);
    }

    #[test]
    fn pending_vectors_yields_set_bits_lowest_first() {
        let got: Vec<u32> = pending_vectors(0b1001_0010).collect();
        assert_eq!(got, [1, 4, 7]);
    }

    #[test]
    fn pending_vectors_empty_status_yields_nothing() {
        assert_eq!(pending_vectors(0).count(), 0);
    }
}
