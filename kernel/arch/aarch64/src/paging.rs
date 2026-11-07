//! AArch64 stage-1 page-table primitives for the memory-isolation test.
//!
//! This module is the aarch64 analogue of `kernel/arch/{x86_64,riscv64}::paging`.
//! It implements the Arch HAL page-table surface
//! ([`tairix_arch_api::mmu::AddressSpace`] +
//! [`tairix_arch_api::tlb::TlbShootdown`]) `kernel/mem` drives: it
//! supplies the architectural mechanism the memory-isolation QEMU
//! vertical needs — two stage-1 translation hierarchies that disagree
//! about a single virtual address, so the MMU faults a process that
//! reaches for another's frame ("memory isolation is
//! enforced by hardware").
//!
//! # Translation scheme
//!
//! 4 KiB granule, three levels (start at L1) covering a 39-bit VA, in
//! **two** regimes the architecture keeps disjoint: `TCR_EL1.T0SZ = 25`
//! gives `TTBR0_EL1` the low `[0, 2^39)` for user space, and
//! `TCR_EL1.T1SZ = 25` gives `TTBR1_EL1` the top `2^39` bytes for the
//! kernel. VA = `L1 (9) | L2 (9) | L3 (9) | offset (12)` in each. An L1
//! block descriptor maps 1 GiB, an L2 block 2 MiB, an L3 page 4 KiB (ARM
//! ARM D5.3).
//!
//! The kernel regime is one global root (`KERNEL_L1`) carrying the direct
//! physical map, the remap window and, in its top slot, the kernel image
//! mapped by section (`kernel_root`), so a process root carries nothing of
//! the kernel and a switch between user spaces reprograms `TTBR0_EL1` alone.
//!
//! Descriptor low bits (ARM ARM D5.3.1): a *table* or *page* descriptor
//! is `0b11`, a *block* descriptor is `0b01`. The lower attributes carry
//! the `MAIR_EL1` attribute index, access permission, shareability, and
//! the access flag.
//!
//! The bit-twiddling that encodes an output address into a descriptor,
//! extracts the per-level table index from a VA, and assembles the
//! attribute words is pure arithmetic and is host-unit-tested below; the
//! `&mut`-recovering table walk and the `TTBR0_EL1`/`SCTLR_EL1` write are
//! gated to the freestanding aarch64 target.

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use tairix_arch_api::frames::{
    active_frames, descriptors, pool_slot_of, reclaim_hierarchy, Leaf, PageTableFrames, TableFrame,
    LINK_ORDERING, WALK_ORDERING,
};
use tairix_arch_api::gigapages::GigapageMask;
use tairix_arch_api::mmu::{
    AccessTracking, AddressSpace as MmuAddressSpace, KernelWindow, MapError, PageFlags,
};
use tairix_arch_api::tags::{AddressTag, AddressTags, ParkRoot};
use tairix_arch_api::tlb::TlbShootdown;

#[cfg(any(test, all(target_arch = "aarch64", target_os = "none")))]
mod kernel_root;

#[cfg(all(target_arch = "aarch64", target_os = "none"))]
pub(crate) use kernel_root::enter_kernel_root;

/// Size of a single page (and of a page-table page): the one system granule.
pub use tairix_abi::PAGE_SIZE;

/// Number of 64-bit entries in a stage-1 page-table page.
pub const ENTRIES_PER_TABLE: usize = 512;

/// Number of paging levels (L1 → L2 → L3).
pub const LEVELS: usize = 3;

/// Descriptor low-bit encodings and lower-attribute fields (ARM ARM
/// D5.3.1 / D5.3.3).
pub mod attrs {
    /// Valid bit (bit 0). Set on every live descriptor.
    pub const VALID: u64 = 1 << 0;
    /// Bit 1: set for a table (at L1/L2) or page (at L3) descriptor,
    /// clear for a block descriptor. With [`VALID`] this gives `0b11`
    /// for table/page and `0b01` for block.
    pub const TABLE_OR_PAGE: u64 = 1 << 1;
    /// Access flag (bit 10). Set eagerly so a platform without hardware
    /// AF management does not fault on first touch.
    pub const AF: u64 = 1 << 10;
    /// Not global (bit 11): the entry belongs to the ASID its walk ran under,
    /// so a root loaded under another ASID never sees it.
    pub const NOT_GLOBAL: u64 = 1 << 11;
    /// Inner-shareable (bits `[9:8] = 0b11`).
    pub const SH_INNER: u64 = 0b11 << 8;
    /// Access permission `0b00` (bits `[7:6]`): read/write at EL1, no EL0
    /// access. The kernel-only mapping the isolation test uses.
    pub const AP_RW_EL1: u64 = 0b00 << 6;
    /// Access permission `0b10` (bits `[7:6]`): read-only at EL1, no EL0
    /// access. A kernel code page.
    pub const AP_RO_EL1: u64 = 0b10 << 6;
    /// Access permission `0b01` (bits `[7:6]`): read/write at EL1 **and**
    /// EL0. Used for an EL0 data mapping (e.g. a user stack).
    pub const AP_RW_EL0: u64 = 0b01 << 6;
    /// Access permission `0b11` (bits `[7:6]`): read-only at EL1 **and**
    /// EL0. Used for an EL0 code mapping (executable but not writable).
    pub const AP_RO_EL0: u64 = 0b11 << 6;
    /// Privileged execute-never (bit 53).
    pub const PXN: u64 = 1 << 53;
    /// Unprivileged execute-never (bit 54).
    pub const UXN: u64 = 1 << 54;
    /// Software-defined leaf bit distinguishing write-combining Normal-NC
    /// framebuffer mappings from bidirectional coherent-DMA mappings.
    pub const SW_WRITE_COMBINE: u64 = 1 << 55;
    /// Software-defined leaf bit marking a DMA buffer, whatever its memory
    /// type ([`PageFlags::DMA`](tairix_arch_api::mmu::PageFlags::DMA)).
    pub const SW_DMA: u64 = 1 << 56;

    /// `MAIR_EL1` attribute index for Normal write-back memory (index 0).
    pub const ATTR_IDX_NORMAL: u64 = 0 << 2;
    /// `MAIR_EL1` attribute index for Device-nGnRE memory (index 1).
    pub const ATTR_IDX_DEVICE: u64 = 1 << 2;
    /// `MAIR_EL1` attribute index for Normal **Non-Cacheable** memory
    /// (index 2). The memory type for a buffer shared with a DMA master
    /// that does not snoop the CPU caches (the BCM2711 PCIe root complex):
    /// the CPU bypasses its caches, so a descriptor it writes is visible to
    /// the device, and an event the device writes is visible to the CPU,
    /// with no explicit cache maintenance. Unlike Device-nGnRE it permits
    /// ordinary (including unaligned) loads/stores, so the xHCI ring and
    /// context structures the driver reads/writes behave normally.
    pub const ATTR_IDX_NORMAL_NC: u64 = 2 << 2;
    /// `MAIR_EL1` attribute index for Device-nGnRnE memory (index 3): a PCI
    /// host's configuration region, whose writes are non-posted, so none
    /// may be acknowledged before the function has it.
    pub const ATTR_IDX_DEVICE_NONPOSTED: u64 = 3 << 2;
}

/// `MAIR_EL1` value pairing attribute index 0 = Normal write-back
/// read/write-allocate, index 1 = Device-nGnRE, index 2 = Normal
/// Non-Cacheable (outer + inner non-cacheable, `0x44`), and index 3 =
/// Device-nGnRnE (`0x00`) (ARM ARM D13.2.95).
pub const MAIR_VALUE: u64 = u64::from_le_bytes([0xFF, 0x04, 0x44, 0x00, 0, 0, 0, 0]);

/// Virtual-address bits each translation regime covers: `2^39` for
/// `TTBR0_EL1` (user) and the same for `TTBR1_EL1` (kernel).
const VA_BITS: u32 = 39;

/// First address of the kernel (`TTBR1_EL1`) regime — the base of the top
/// `2^39` bytes of the 64-bit space, which `TCR_EL1.T1SZ` selects.
///
/// The architecture, not a slot convention, is what keeps this disjoint
/// from every `TTBR0_EL1` address: the two regimes are separate walks with
/// separate roots, so no user mapping can name a kernel address and the
/// port is ready for a `TTBR0`-only unmap of the kernel (KPTI).
pub const KERNEL_VA_BASE: u64 = !0u64 << VA_BITS;

/// L1 slot of the kernel regime the image occupies: the top one.
const IMAGE_SLOT: usize = ENTRIES_PER_TABLE - 1;

/// First address of the gigabyte the image is linked in. The linker script
/// puts the image at its load address's offset within it, so the image's
/// virtual and physical addresses agree in every block.
pub const KERNEL_IMAGE_GIGABYTE: u64 = KERNEL_VA_BASE + ((IMAGE_SLOT as u64) << 30);

/// Bytes of the boot stack's guard, which the kernel root maps read-only.
pub const BOOT_STACK_GUARD_BYTES: u64 = PAGE_SIZE as u64;

// The image's layout and this module's map of it share these definitions, and
// the linker's own offset between where the image is linked and loaded is
// kept where `image_phys` reads it.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
core::arch::global_asm!(
    ".globl kernel_image_gigabyte",
    ".set kernel_image_gigabyte, {gigabyte}",
    ".globl kernel_image_block",
    ".set kernel_image_block, {block}",
    ".globl boot_stack_guard_bytes",
    ".set boot_stack_guard_bytes, {guard}",
    ".pushsection .rodata.kimage_voffset, \"a\"",
    ".balign 8",
    ".globl tairix_aarch64_kimage_voffset",
    ".type tairix_aarch64_kimage_voffset, %object",
    ".size tairix_aarch64_kimage_voffset, 8",
    "tairix_aarch64_kimage_voffset:",
    ".quad __kimage_voffset",
    ".popsection",
    gigabyte = const KERNEL_IMAGE_GIGABYTE,
    block = const tairix_arch_api::imagemap::BLOCK,
    guard = const BOOT_STACK_GUARD_BYTES,
);

/// The firmware tree as the kernel reaches it: read-only at `virt`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FirmwareTree {
    /// Where the kernel reads the tree.
    pub virt: u64,
    /// Where the firmware put it, which the frame allocator must not hand
    /// out.
    pub phys: u64,
    /// Its bytes, as its header states them.
    pub len: u64,
}

static TREE_VIRT: AtomicU64 = AtomicU64::new(0);
static TREE_PHYS: AtomicU64 = AtomicU64::new(0);
/// Published last; zero until a tree is mapped.
static TREE_LEN: AtomicU64 = AtomicU64::new(0);

/// The firmware tree the boot mapped, or [`None`] when the firmware handed
/// over none.
#[must_use]
pub fn firmware_tree() -> Option<FirmwareTree> {
    let len = TREE_LEN.load(Ordering::Acquire);
    (len != 0).then(|| FirmwareTree {
        virt: TREE_VIRT.load(Ordering::Relaxed),
        phys: TREE_PHYS.load(Ordering::Relaxed),
        len,
    })
}

/// Record `tree` as the firmware's, once, before any other CPU runs.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
fn publish_firmware_tree(tree: FirmwareTree) {
    TREE_VIRT.store(tree.virt, Ordering::Relaxed);
    TREE_PHYS.store(tree.phys, Ordering::Relaxed);
    TREE_LEN.store(tree.len, Ordering::Release);
}

/// The physical address of `virt`, an address in the kernel image, which is
/// loaded where its linker script says: `__kimage_voffset` below its link
/// address. Host builds load no image, so there an address is its own.
#[must_use]
pub fn image_phys(virt: u64) -> u64 {
    virt.wrapping_sub(kimage_voffset())
}

#[cfg(all(target_arch = "aarch64", target_os = "none"))]
fn kimage_voffset() -> u64 {
    extern "C" {
        static tairix_aarch64_kimage_voffset: u64;
    }
    // SAFETY: a word in `.rodata` the linker fills and nothing writes.
    unsafe { core::ptr::addr_of!(tairix_aarch64_kimage_voffset).read() }
}

#[cfg(not(all(target_arch = "aarch64", target_os = "none")))]
const fn kimage_voffset() -> u64 {
    0
}

/// `TCR_EL1` value: both regimes 39-bit, 4 KiB granule, inner/outer
/// write-back cacheable and inner-shareable walks, `TTBR0_EL1` owning the
/// ASID (`A1 = 0`), and a 40-bit (1 TiB) output address size.
pub const TCR_VALUE: u64 = {
    let t0sz: u64 = (64 - VA_BITS) as u64;
    let irgn0: u64 = 0b01 << 8;
    let orgn0: u64 = 0b01 << 10;
    let sh0: u64 = 0b11 << 12;
    let tg0: u64 = 0b00 << 14; // 4 KiB granule for TTBR0
    let t1sz: u64 = ((64 - VA_BITS) as u64) << 16;
    let irgn1: u64 = 0b01 << 24;
    let orgn1: u64 = 0b01 << 26;
    let sh1: u64 = 0b11 << 28;
    let tg1: u64 = 0b10 << 30; // 4 KiB granule for TTBR1 (a distinct encoding)
    let ips: u64 = 0b010 << 32; // 40-bit (1 TiB) physical address size
    t0sz | irgn0 | orgn0 | sh0 | tg0 | t1sz | irgn1 | orgn1 | sh1 | tg1 | ips
};

/// The `SCTLR_EL1` bits that are RES1 on ARMv8.0-A (ARM ARM D13.2.118):
/// bits 29, 28, 23, 22, 20, and 11. Every other bit — including the
/// booby-traps `EE`/`E0E` (data big-endian), `WXN` (writable implies
/// execute-never), `A`/`SA`/`SA0` (alignment checking) — is left clear.
/// Bit 23 is [`SCTLR_SPAN`] on a CPU with PAN.
pub const SCTLR_RES1: u64 = (1 << 29) | (1 << 28) | (1 << 23) | (1 << 22) | (1 << 20) | (1 << 11);

/// `SCTLR_EL1.SPAN`: set, an exception taken to EL1 leaves `PSTATE.PAN` as it
/// was; clear, it sets PAN. RES1 where the CPU lacks PAN, so only a CPU the
/// kernel holds to PAN clears it ([`crate::protection::sctlr_mmu_on`]).
pub const SCTLR_SPAN: u64 = 1 << 23;

/// The known MMU-off `SCTLR_EL1` (= [`SCTLR_RES1`], `0x30D0_0800`) the
/// entry trampolines establish before the first EL1 data access.
///
/// `SCTLR_EL1` is architecturally **UNKNOWN** when EL1 is first entered
/// on real silicon — behind the firmware's EL2 hand-off and behind a
/// PSCI `CPU_ON` alike (QEMU resets it to a benign value, which is why
/// only hardware ever saw the difference). An UNKNOWN `EE` makes every
/// data access byte-swapped; an UNKNOWN `WXN` makes the writable kernel
/// mapping execute-never the instant translation is enabled — a silent
/// pre-vectors hang on the Pi 4. The trampolines (`boot.s`, `smp.s`)
/// therefore write this value, built from immediates, before their first
/// data access, so EL1 always starts from known ground rather than trusting
/// the reset state.
pub const SCTLR_MMU_OFF: u64 = SCTLR_RES1;

/// The full MMU-on `SCTLR_EL1` value the trampolines turn translation on
/// with: [`SCTLR_RES1`] plus `M` (stage-1 translation), `C` (data cache),
/// and `I` (instruction cache). A CPU held to PAN then writes it with
/// [`SCTLR_SPAN`] clear (`hold_translation_controls`).
///
/// Written as a whole — never OR-ed into the live register — so no
/// UNKNOWN reset bit survives into translated execution (see
/// [`SCTLR_MMU_OFF`]). `C` is required, not an optimisation: the
/// LDXR/STXR exclusives the allocator and scheduler rely on are only
/// guaranteed on cacheable Normal memory (a non-cacheable exclusive
/// needs a global monitor the BCM2711 does not provide), and the
/// framebuffer path already cleans its writes to the point of coherency
/// (`crate::video`).
pub const SCTLR_MMU_ON: u64 = SCTLR_RES1 | (1 << 0) | (1 << 2) | (1 << 12);

/// Physical-address mask of a descriptor's output-address field
/// (bits `[47:12]`).
const ADDR_MASK: u64 = 0x0000_FFFF_FFFF_F000;

/// `true` iff a descriptor is a *block* leaf — valid, with bit 1 clear.
#[must_use]
pub const fn is_block(desc: u64) -> bool {
    (desc & attrs::VALID) != 0 && (desc & attrs::TABLE_OR_PAGE) == 0
}

/// Encode an output physical address plus lower attributes into a block
/// or page descriptor. `paddr` must be page/block-aligned.
#[must_use]
pub const fn descriptor(paddr: u64, lower_attrs: u64) -> u64 {
    (paddr & ADDR_MASK) | lower_attrs
}

/// Encode a next-level table pointer into a table descriptor (`0b11`).
#[must_use]
pub const fn table_descriptor(paddr: u64) -> u64 {
    (paddr & ADDR_MASK) | attrs::VALID | attrs::TABLE_OR_PAGE
}

/// Recover the output physical address a descriptor points at.
#[must_use]
pub const fn phys_from_descriptor(desc: u64) -> u64 {
    desc & ADDR_MASK
}

/// Extract the 9-bit table index for paging `level` (1 = top, 3 = leaf)
/// from a virtual address.
#[must_use]
pub const fn table_index(vaddr: u64, level: usize) -> usize {
    ((vaddr >> table_shift(level)) & 0x1FF) as usize
}

/// The log2 of the bytes one entry of a `level` table maps: L1 indexes
/// bits `[38:30]`, L2 `[29:21]`, L3 `[20:12]`.
const fn table_shift(level: usize) -> usize {
    12 + 9 * (LEVELS - level)
}

/// What the kernel may do with Normal memory it maps for itself: never both
/// write and execute it, and nothing at EL0.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KernelAccess {
    /// Read and execute: the kernel's code.
    Execute,
    /// Read only.
    Read,
    /// Read and write.
    Write,
}

impl KernelAccess {
    /// The lower attributes granting exactly this access to a page, or with
    /// `block` a block.
    #[must_use]
    pub const fn leaf(self, block: bool) -> u64 {
        let rights = match self {
            Self::Execute => attrs::AP_RO_EL1,
            Self::Read => attrs::AP_RO_EL1 | attrs::PXN,
            Self::Write => attrs::AP_RW_EL1 | attrs::PXN,
        };
        let shape = if block { 0 } else { attrs::TABLE_OR_PAGE };
        attrs::VALID
            | shape
            | attrs::AF
            | attrs::SH_INNER
            | attrs::ATTR_IDX_NORMAL
            | attrs::UXN
            | rights
    }
}

/// The trampoline's early map of the image: Normal blocks, writable *and*
/// executable, which the kernel root replaces before anything but the port
/// entry has run.
#[cfg(any(test, all(target_arch = "aarch64", target_os = "none")))]
pub(crate) const EARLY_IMAGE_BLOCK: u64 = attrs::VALID
    | attrs::AF
    | attrs::SH_INNER
    | attrs::AP_RW_EL1
    | attrs::ATTR_IDX_NORMAL
    | attrs::UXN;

/// The identity map's one leaf, the page translation is turned on from:
/// tagged with the boot ASID, so parking `TTBR0_EL1` discards it.
#[cfg(any(test, all(target_arch = "aarch64", target_os = "none")))]
pub(crate) const IDMAP_LEAF: u64 = KernelAccess::Execute.leaf(false) | attrs::NOT_GLOBAL;

/// Lower attributes for an **EL0-executable** Normal-memory page leaf:
/// read-only at EL1 and EL0 (`AP_RO_EL0`), privileged-execute-never
/// (`PXN`, so EL1 cannot run user code) but *unprivileged*-executable
/// (`UXN` clear). The output is a page descriptor (`TABLE_OR_PAGE`); EL0
/// code is always mapped at 4 KiB granularity.
#[must_use]
pub const fn el0_code_leaf_attrs() -> u64 {
    attrs::VALID
        | attrs::TABLE_OR_PAGE
        | attrs::AF
        | attrs::SH_INNER
        | attrs::AP_RO_EL0
        | attrs::ATTR_IDX_NORMAL
        | attrs::PXN
}

/// Lower attributes for an **EL0 read-only, non-executable** Normal-memory
/// page leaf: read-only at EL1 and EL0 (`AP_RO_EL0`) and execute-never at
/// both ELs (`PXN | UXN`). Used for a read-only EL0 data page — an `rxe`
/// `ReadOnly` segment (`.rodata`) or the kernel-written process startup
/// block — where [`el0_code_leaf_attrs`] would wrongly leave the page
/// EL0-executable. The output is a page descriptor (`TABLE_OR_PAGE`).
#[must_use]
pub const fn el0_rodata_leaf_attrs() -> u64 {
    attrs::VALID
        | attrs::TABLE_OR_PAGE
        | attrs::AF
        | attrs::SH_INNER
        | attrs::AP_RO_EL0
        | attrs::ATTR_IDX_NORMAL
        | attrs::PXN
        | attrs::UXN
}

/// Lower attributes for an **EL0-writable** Normal-memory page leaf:
/// read/write at EL1 and EL0 (`AP_RW_EL0`), execute-never at both ELs
/// (`PXN | UXN`). Used for an EL0 data page such as a user stack. The
/// output is a page descriptor (`TABLE_OR_PAGE`).
#[must_use]
pub const fn el0_data_leaf_attrs() -> u64 {
    attrs::VALID
        | attrs::TABLE_OR_PAGE
        | attrs::AF
        | attrs::SH_INNER
        | attrs::AP_RW_EL0
        | attrs::ATTR_IDX_NORMAL
        | attrs::PXN
        | attrs::UXN
}

/// Lower attributes for an **EL0-accessible** Normal **Non-Cacheable**
/// page leaf: read/write at EL1 and EL0 (`AP_RW_EL0`), execute-never at
/// both ELs (`PXN | UXN`), Normal Non-Cacheable memory type (MAIR index
/// 2). Used for a DMA buffer a **user-space driver** shares with a
/// non-I/O-coherent device (the [`PageFlags::DMA_COHERENT`] leaf, the
/// coherent-DMA analogue of [`el0_data_leaf_attrs`]): the buffer is
/// coherent with the device without per-access cache maintenance, and the
/// driver still accesses it with ordinary loads/stores (Device memory
/// would forbid the unaligned ring accesses). The output is a page
/// descriptor (`TABLE_OR_PAGE`); DMA buffers are mapped at 4 KiB
/// granularity.
#[must_use]
pub const fn el0_dma_coherent_leaf_attrs() -> u64 {
    non_cacheable_leaf(attrs::AP_RW_EL0)
}

/// [`el0_dma_coherent_leaf_attrs`] for a page only the kernel reaches:
/// read/write at EL1 alone (`AP_RW_EL1`).
#[must_use]
pub const fn dma_coherent_leaf_attrs() -> u64 {
    non_cacheable_leaf(attrs::AP_RW_EL1)
}

/// A Normal Non-Cacheable, execute-never page leaf under the access `ap`
/// grants.
const fn non_cacheable_leaf(ap: u64) -> u64 {
    attrs::VALID
        | attrs::TABLE_OR_PAGE
        | attrs::AF
        | attrs::SH_INNER
        | ap
        | attrs::ATTR_IDX_NORMAL_NC
        | attrs::PXN
        | attrs::UXN
}

/// Lower attributes for a kernel Device-memory leaf: read/write at EL1,
/// executable nowhere, the Device-nGnRE memory type (MAIR index 1).
#[must_use]
pub const fn device_leaf_attrs(block: bool) -> u64 {
    kernel_device_leaf(block, attrs::ATTR_IDX_DEVICE)
}

/// [`device_leaf_attrs`] for a PCI host's configuration region: the
/// Device-nGnRnE memory type.
#[must_use]
pub const fn config_leaf_attrs(block: bool) -> u64 {
    kernel_device_leaf(block, attrs::ATTR_IDX_DEVICE_NONPOSTED)
}

const fn kernel_device_leaf(block: bool, attr_idx: u64) -> u64 {
    let base = attrs::VALID | attrs::AF | attrs::AP_RW_EL1 | attr_idx | attrs::PXN | attrs::UXN;
    if block {
        base
    } else {
        base | attrs::TABLE_OR_PAGE
    }
}

/// Lower attributes for an **EL0-accessible** Device-memory page leaf:
/// read/write at EL1 and EL0 (`AP_RW_EL0`), execute-never at both ELs
/// (`PXN | UXN`), Device-nGnRE memory type (MAIR index 1). Used for a
/// device MMIO window a **user-space driver** maps into its own address
/// space through the `mmio_map` syscall (`plans/PI.md` P10 chunk 5d-0):
/// the kernel-only [`device_leaf_attrs`] leaves the page `AP_RW_EL1`, so an
/// EL0 driver reading its own mapped register would take a permission fault
/// — this is the EL0 counterpart, the Device-memory analogue of
/// [`el0_data_leaf_attrs`]. The output is a page descriptor
/// (`TABLE_OR_PAGE`); device windows are always mapped at 4 KiB granularity.
#[must_use]
pub const fn el0_device_leaf_attrs() -> u64 {
    attrs::VALID
        | attrs::TABLE_OR_PAGE
        | attrs::AF
        | attrs::AP_RW_EL0
        | attrs::ATTR_IDX_DEVICE
        | attrs::PXN
        | attrs::UXN
}

use tairix_arch_api::gigapages::word_holds;
pub use tairix_arch_api::gigapages::{
    from_extents as gigapage_mask_from_extents, MASK_WORDS as GIGAPAGE_MASK_WORDS,
};

const _: () = assert!(
    tairix_arch_api::gigapages::MASK_WORDS * 64 == ENTRIES_PER_TABLE,
    "one gigapage bit per L1 slot"
);

// --- The kernel (`TTBR1_EL1`) regime -------------------------------

/// The one kernel L1 root: the direct physical map in its low slots, the
/// remap window above it, and the kernel image in the top slot.
///
/// A `.bss` static, so its tables live inside the kernel image rather than in
/// allocator-backed frames — which is what lets the Supervisor's destructive
/// whole-RAM sweep keep running under the translation it is testing. The boot
/// CPU loads it from the port entry and each secondary from `smp.s`, which
/// names it; it is never replaced.
#[export_name = "tairix_aarch64_kernel_root"]
static KERNEL_L1: KernelRoot = KernelRoot(UnsafeCell::new(Table::new()));

/// Interior-mutable wrapper over the kernel L1 root's storage.
struct KernelRoot(UnsafeCell<Table>);

// SAFETY: every software access goes through the atomic view
// [`kernel_root_entries`] gives; the rest is the MMU's own table walk.
unsafe impl Sync for KernelRoot {}

/// The kernel root's entries.
fn kernel_root_entries() -> &'static [AtomicU64; ENTRIES_PER_TABLE] {
    // SAFETY: the static's aligned storage, live for the image's lifetime,
    // which nothing reaches through a `&mut`.
    unsafe { descriptors(KERNEL_L1.0.get().cast()) }
}

/// Physical address of the kernel L1 root — the value `TTBR1_EL1` carries.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
fn kernel_root_phys() -> u64 {
    image_phys(KERNEL_L1.0.get() as u64)
}

/// A root with no entry. `TTBR0_EL1` holds it whenever no process runs, so
/// a kernel access to a low address — a null dereference among them —
/// faults; `TTBR1_EL1` holds it for the instant the kernel root replaces the
/// trampoline's map.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
static EMPTY_ROOT: Table = Table::new();

/// Physical address of [`EMPTY_ROOT`].
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
fn empty_root_phys() -> u64 {
    image_phys(core::ptr::addr_of!(EMPTY_ROOT) as u64)
}

/// Claim a one-time publication into the kernel root, reporting whether this
/// caller won it.
///
/// The claim, not a load-then-store check over the published words, is what
/// makes a publication unique: two callers could both pass the check. The
/// host tests are threaded, so the claim is load-bearing there.
fn claim_kernel_root_write(flag: &AtomicBool) -> bool {
    flag.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
}

/// Claim on the kernel remap window's slot range.
static WINDOW_CLAIM: AtomicBool = AtomicBool::new(false);

/// Set once the window's descriptors *and* the kernel root's window slots
/// are visible to the table walker.
///
/// Separate from [`WINDOW_CLAIM`] because the two answer different
/// questions: the claim excludes a second writer, this reports completion.
/// The descriptors are published *before* the kernel root is filled — the
/// fill reads them — so a reader taking a live descriptor as "ready" could map
/// into the window before the walker had the root slot.
static WINDOW_PUBLISHED: AtomicBool = AtomicBool::new(false);

/// L1 slots the kernel remap window claims, just below the image's.
///
/// Sized from the port's VA layout rather than from a byte figure: the
/// regime spans [`ENTRIES_PER_TABLE`] gigapages, and the window takes an
/// eighth of them (64 GiB). Address space is free until something is backed
/// into it, so the only cost of a generous window is one shared L2 table per
/// slot; what the size bounds is the kernel heap, and on any machine this
/// port runs on installed RAM binds long before 64 GiB of kernel heap does.
const KERNEL_WINDOW_SLOTS: usize = ENTRIES_PER_TABLE / 8;

/// First L1 slot of the kernel remap window.
const KERNEL_WINDOW_FIRST_SLOT: usize = IMAGE_SLOT - KERNEL_WINDOW_SLOTS;

/// Pages the kernel remap window spans.
const KERNEL_WINDOW_PAGES: usize = KERNEL_WINDOW_SLOTS * ENTRIES_PER_TABLE * ENTRIES_PER_TABLE;

/// First L1 slot of the kernel regime the direct physical map claims — its
/// very first.
const PHYSMAP_FIRST_SLOT: usize = 0;

/// L1 slots the direct physical map spans: everything from its first slot
/// up to the remap window. Derived, so moving either boundary cannot leave
/// the two overlapping.
const PHYSMAP_SLOTS: usize = KERNEL_WINDOW_FIRST_SLOT - PHYSMAP_FIRST_SLOT;

/// Base virtual address of the direct physical map: a covered physical `p`
/// is reachable at `PHYSMAP_VMA_BASE + p` on every CPU.
pub const PHYSMAP_VMA_BASE: u64 = KERNEL_VA_BASE + ((PHYSMAP_FIRST_SLOT as u64) << 30);

/// Widest direct physical map the claimed slots can express, in gigabytes.
///
/// An L1 leaf *is* a 1 GiB block, so one slot is one gigabyte. Memory above
/// this is unreachable and every consumer of it fails closed; it is the
/// architectural ceiling of a 39-bit regime, not a board constant.
pub const MAX_PHYSMAP_GIB: usize = PHYSMAP_SLOTS;

/// One past the highest physical address the direct map can reach.
pub const PHYSMAP_REACH: u64 = (MAX_PHYSMAP_GIB as u64) << 30;

/// What a gigabyte of the direct map holds, and so how the kernel maps it.
/// Each gigabyte is typed once, so no physical address is ever reached under
/// two memory types — a Normal alias of a register invites the core's
/// speculative reads into it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Holding {
    /// RAM: Normal write-back, read/write, never executed.
    Ram,
    /// Device registers: Device-nGnRE.
    Registers,
    /// A PCI host's configuration region: Device-nGnRnE, whose writes are
    /// non-posted, so none is acknowledged before the function has it.
    Config,
}

impl Holding {
    /// The block leaf typing a gigabyte as `self`.
    const fn leaf(self) -> u64 {
        match self {
            Self::Ram => KernelAccess::Write.leaf(true),
            Self::Registers => device_leaf_attrs(true),
            Self::Config => config_leaf_attrs(true),
        }
    }

    /// What the direct-map entry `entry` holds, or `None` for an invalid
    /// one. Only the image's own gigabyte is a table: RAM, split around the
    /// read-only alias of the image.
    const fn of(entry: u64) -> Option<Self> {
        if entry & attrs::VALID == 0 {
            return None;
        }
        if !is_block(entry) {
            return Some(Self::Ram);
        }
        match entry & (0b111 << 2) {
            attrs::ATTR_IDX_DEVICE => Some(Self::Registers),
            attrs::ATTR_IDX_DEVICE_NONPOSTED => Some(Self::Config),
            _ => Some(Self::Ram),
        }
    }
}

/// Type each gigabyte `mask` names, below [`MAX_PHYSMAP_GIB`], as `holding`
/// in the direct map, and answer which of them hold it now. A gigabyte the
/// map already types otherwise keeps its type: none is ever retyped.
///
/// So that RAM never shares a gigabyte with registers, the boot types the
/// registers first and then only the RAM gigabytes left to it.
#[must_use]
pub fn reveal(mask: &GigapageMask, holding: Holding) -> GigapageMask {
    let root = kernel_root_entries();
    let mut held = [0u64; GIGAPAGE_MASK_WORDS];
    for gigapage in (0..MAX_PHYSMAP_GIB).filter(|&index| word_holds(mask[index / 64], index)) {
        let typed = descriptor((gigapage as u64) << 30, holding.leaf());
        let now = match root[PHYSMAP_FIRST_SLOT + gigapage].compare_exchange(
            0,
            typed,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => typed,
            Err(existing) => existing,
        };
        if Holding::of(now) == Some(holding) {
            held[gigapage / 64] |= 1 << (gigapage % 64);
        }
    }
    // An invalid entry is cached nowhere, so making one valid owes the
    // walker ordering, never an invalidation.
    publish_table_update();
    held
}

/// What the direct map holds at gigabyte `index`, or `None` where it maps
/// nothing.
#[must_use]
pub fn holding(index: usize) -> Option<Holding> {
    (index < MAX_PHYSMAP_GIB)
        .then(|| {
            Holding::of(kernel_root_entries()[PHYSMAP_FIRST_SLOT + index].load(Ordering::Acquire))
        })
        .flatten()
}

/// Whether every gigabyte `[phys, phys + len)` touches holds what `wanted`
/// accepts. Fails closed on an empty, wrapping or over-wide range.
fn every_gigabyte(phys: u64, len: u64, wanted: impl Fn(Holding) -> bool) -> bool {
    let Some(last) = len.checked_sub(1).and_then(|tail| phys.checked_add(tail)) else {
        return false;
    };
    let (first, last) = ((phys >> 30) as usize, (last >> 30) as usize);
    last < MAX_PHYSMAP_GIB && (first..=last).all(|index| holding(index).is_some_and(&wanted))
}

/// Gigabytes of RAM the direct map covers.
#[must_use]
pub fn physmap_gigapages() -> usize {
    (0..MAX_PHYSMAP_GIB)
        .filter(|&index| holding(index) == Some(Holding::Ram))
        .count()
}

/// The direct-map virtual address of physical `phys`.
///
/// `const`, so a fixed address names its direct-map spelling without a
/// run-time load. It resolves only for a `phys` the map covers;
/// [`physmap_covers`] (RAM) or [`registers_virt`] (registers) is the check a
/// caller with a discovered address makes first.
#[must_use]
pub const fn physmap_virt(phys: u64) -> u64 {
    PHYSMAP_VMA_BASE.wrapping_add(phys)
}

/// Where the kernel reaches the register at physical `phys`: its direct-map
/// spelling, valid once the boot has typed its gigabyte as registers
/// ([`registers_virt`]).
#[must_use]
// The port is 64-bit: an address is the same width as a `usize` and a `u64`.
#[allow(clippy::cast_possible_truncation)]
pub const fn mmio(phys: usize) -> usize {
    physmap_virt(phys as u64) as usize
}

/// `true` when the direct map covers every byte of `[phys, phys + len)` as
/// RAM.
#[must_use]
pub fn physmap_covers(phys: u64, len: u64) -> bool {
    every_gigabyte(phys, len, |holding| holding == Holding::Ram)
}

/// The direct-map address of the registers at `[phys, phys + len)`, or
/// `None` unless every byte lies in a gigabyte typed as registers or a
/// configuration region: no other kernel mapping of a register is uncached.
#[must_use]
pub fn registers_virt(phys: u64, len: u64) -> Option<u64> {
    every_gigabyte(phys, len, |holding| holding != Holding::Ram).then(|| physmap_virt(phys))
}

/// The window's shared L1 table descriptors, one per claimed slot, or `0`
/// before [`reserve_kernel_window`] runs.
///
/// The kernel root holds the live copy; these are what
/// [`AddressSpace::new_kernel_window`] installs into the throwaway root the
/// remap layer edits the window's shared sub-hierarchy through, so a leaf
/// added under one of the L2 tables they point at is immediately visible
/// under the live regime.
static KERNEL_WINDOW_L1: [AtomicU64; KERNEL_WINDOW_SLOTS] =
    [const { AtomicU64::new(0) }; KERNEL_WINDOW_SLOTS];

/// Base virtual address of the kernel remap window.
#[must_use]
pub const fn kernel_window_base() -> u64 {
    KERNEL_VA_BASE + ((KERNEL_WINDOW_FIRST_SLOT as u64) << 30)
}

/// A window whose extent is not representable is refused at run time, which
/// would silently leave the kernel heap on its bootstrap region; and a map
/// that ran into the window would shadow the heap's own tables. Fail the
/// build instead.
const _: () = {
    assert!(
        KernelWindow::is_representable(kernel_window_base(), KERNEL_WINDOW_PAGES),
        "the kernel remap window must be a representable extent"
    );
    assert!(
        PHYSMAP_FIRST_SLOT < KERNEL_WINDOW_FIRST_SLOT,
        "the direct physical map must start below the kernel remap window"
    );
    assert!(
        KERNEL_WINDOW_FIRST_SLOT + KERNEL_WINDOW_SLOTS == IMAGE_SLOT,
        "the kernel remap window must end where the image's slot begins"
    );
};

/// Reserve the kernel remap window: draw one shared L2 table per claimed
/// L1 slot, publish the descriptors, and install them in the kernel root so
/// the running CPUs see the window immediately.
///
/// Called once, from the boot path, after the frame allocator exists (the
/// tables come from it, not from the fixed boot pool). A second call
/// returns the same window without drawing anything.
///
/// Returns `None`, having changed nothing, when the frame source cannot
/// supply the shared tables. Unlike the `TTBR0` layout this replaced, no
/// discovered Device or RAM gigapage can claim a window slot: the window
/// lives in a regime no board resource is mapped into.
pub fn reserve_kernel_window(frames: &'static dyn PageTableFrames) -> Option<KernelWindow> {
    // SAFETY: the window's L1 slots are this port's own — the compile-time
    // assertion above pins its extent and keeps the direct map below it,
    // and the publication below installs one shared sub-hierarchy in every
    // root this port builds, so the run is reserved and resolves
    // identically under each.
    let window = unsafe { KernelWindow::at_address(kernel_window_base(), KERNEL_WINDOW_PAGES) }?;
    if WINDOW_PUBLISHED.load(Ordering::Acquire) {
        return Some(window);
    }
    if !claim_kernel_root_write(&WINDOW_CLAIM) {
        // Another caller holds the reservation but has not finished
        // publishing it; handing back a window whose shared tables are not
        // yet visible would let the remap layer draw private ones beside
        // them. Fail closed.
        return None;
    }

    for (offset, slot) in KERNEL_WINDOW_L1.iter().enumerate() {
        let Some(TableFrame { phys, entries: _ }) = frames.alloc_table() else {
            // Undo the partial reservation, and release the claim, so a
            // retry starts clean.
            for undone in KERNEL_WINDOW_L1.iter().take(offset) {
                frames.free_table(phys_from_descriptor(undone.swap(0, Ordering::AcqRel)));
            }
            WINDOW_CLAIM.store(false, Ordering::Release);
            return None;
        };
        slot.store(table_descriptor(phys), Ordering::Release);
    }
    let root = kernel_root_entries();
    for (offset, slot) in KERNEL_WINDOW_L1.iter().enumerate() {
        root[KERNEL_WINDOW_FIRST_SLOT + offset]
            .store(slot.load(Ordering::Acquire), Ordering::Release);
    }
    publish_table_update();
    WINDOW_PUBLISHED.store(true, Ordering::Release);
    Some(window)
}

/// Copy the published window descriptors into the window slots of `root`, a
/// table no walk reaches yet.
fn install_kernel_window_slots(root: &mut [u64; ENTRIES_PER_TABLE]) {
    for offset in 0..KERNEL_WINDOW_SLOTS {
        let descriptor = KERNEL_WINDOW_L1[offset].load(Ordering::Acquire);
        if descriptor != 0 {
            root[KERNEL_WINDOW_FIRST_SLOT + offset] = descriptor;
        }
    }
}

/// Which translation regime an [`AddressSpace`]'s root serves.
///
/// A regime is a property of the root, not of an address: the L1 index of
/// a kernel-window address and of a user address 447 GiB up are the same
/// nine bits, so a walk given the wrong root would silently install a leaf
/// in the wrong regime. Every mapping operation checks the address against
/// the root's regime first and refuses a mismatch.
#[derive(Copy, Clone, Eq, PartialEq)]
enum Regime {
    /// A `TTBR0_EL1` root: the low `[0, 2^39)` user regime.
    User,
    /// A handle onto the kernel regime's shared remap-window hierarchy.
    KernelWindow,
}

impl Regime {
    /// `true` when `vaddr` belongs to this regime.
    const fn holds(self, vaddr: u64) -> bool {
        match self {
            Self::User => vaddr < (1 << VA_BITS),
            Self::KernelWindow => {
                vaddr >= kernel_window_base()
                    && vaddr - kernel_window_base()
                        < (KERNEL_WINDOW_PAGES as u64) * PAGE_SIZE as u64
            }
        }
    }

    /// The scope bit of a leaf mapped in this regime: a user root's leaves
    /// belong to the ASID it runs under, the kernel window's to every ASID.
    const fn leaf_scope(self) -> u64 {
        match self {
            Self::User => attrs::NOT_GLOBAL,
            Self::KernelWindow => 0,
        }
    }
}

/// Publish a translation-table store to the MMU's table walker before
/// the next access depends on it: `dsb ishst` orders the store for the
/// walker, `isb` discards any fetch-ahead made under the old tables.
///
/// For a store that makes valid an entry no TLB can hold — an invalid entry,
/// or one whose Access flag faults — which therefore needs no invalidation:
/// the kernel root's set-once updates, ordering a child table's contents
/// ahead of the descriptor that publishes them, and the Access-flag fix-up.
/// It is no substitute for the TLB maintenance a *withdrawn* translation
/// needs. Host builds walk no hardware tables, so this is a no-op there.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
fn publish_table_update() {
    // SAFETY: barrier-only instruction sequence — no memory or register
    // operands, no state observed or mutated beyond ordering.
    unsafe {
        core::arch::asm!("dsb ishst", "isb", options(nostack, preserves_flags));
    }
}

#[cfg(not(all(target_arch = "aarch64", target_os = "none")))]
fn publish_table_update() {}

/// Clean+invalidate every data-cache line of `[base, base + len)` to the
/// point of coherency (`dc civac`, line size decoded from the live
/// `CTR_EL0` by [`dcache_line_bytes`]), then `dsb sy`.
///
/// This is the bridge between the CPU's cacheable writes and an observer
/// that reads the same bytes non-cacheably — a secondary core polling its
/// release gate before its MMU is on, or a device that does not snoop: without
/// the sweep, a dirty (or stale) line over the range would shadow — or later
/// overwrite — the DRAM bytes the non-cacheable observer works with on real
/// silicon (cache-less QEMU cannot show it).
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
pub fn clean_invalidate_range_to_poc(base: u64, len: u64) {
    sweep_to_poc(base, len, |addr| {
        // SAFETY: `dc civac` performs cache maintenance only — it never
        // changes memory contents — so it is sound for any address; the
        // caller names a range it owns.
        unsafe {
            core::arch::asm!("dc civac, {addr}", addr = in(reg) addr,
                options(nostack, preserves_flags));
        }
    });
}

/// Clean every data-cache line of `[base, base + len)` to the point of
/// coherency (`dc cvac`), then `dsb sy`: what a non-snooping reader of bytes
/// the CPU goes on caching needs — a DMA scan-out, an interrupt
/// translation service's tables.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
pub fn clean_range_to_poc(base: u64, len: u64) {
    sweep_to_poc(base, len, |addr| {
        // SAFETY: `dc cvac` writes a line back and changes no memory
        // contents, so it is sound for any address; the caller names a
        // range it owns.
        unsafe {
            core::arch::asm!("dc cvac, {addr}", addr = in(reg) addr,
                options(nostack, preserves_flags));
        }
    });
}

/// Run `maintain` on every data-cache line of `[base, base + len)`, line
/// size decoded from the live `CTR_EL0`, then complete it with `dsb sy`.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
fn sweep_to_poc(base: u64, len: u64, maintain: impl Fn(u64)) {
    if len == 0 {
        return;
    }
    let ctr: u64;
    // SAFETY: `CTR_EL0` is an unprivileged read-only identification
    // register; reading it has no side effects.
    unsafe {
        core::arch::asm!("mrs {ctr}, CTR_EL0", ctr = out(reg) ctr,
            options(nomem, nostack, preserves_flags));
    }
    let line = dcache_line_bytes(ctr) as u64;
    // From the line-aligned base, so the first partial line is covered too.
    let mut addr = base & !(line - 1);
    let end = base.saturating_add(len);
    while addr < end {
        maintain(addr);
        addr += line;
    }
    // SAFETY: barrier-only instruction — completes the maintenance in
    // the full-system domain before the non-cacheable observer reads.
    unsafe {
        core::arch::asm!("dsb sy", options(nostack, preserves_flags));
    }
}

/// Host stand-in for [`clean_invalidate_range_to_poc`]: the host has no
/// data cache to maintain, so the sweep is vacuous. Under `cfg(test)` it
/// records the requested range so the host unit tests can assert that a
/// producer of bytes read non-cacheably swept them to the point of
/// coherency (the recorder is a per-thread log, so parallel tests never
/// contend).
#[cfg(not(all(target_arch = "aarch64", target_os = "none")))]
pub fn clean_invalidate_range_to_poc(base: u64, len: u64) {
    #[cfg(test)]
    record_poc_sweep(base, len);
    #[cfg(not(test))]
    let _ = (base, len);
}

/// Host stand-in for [`clean_range_to_poc`], recording as
/// [`clean_invalidate_range_to_poc`]'s does.
#[cfg(not(all(target_arch = "aarch64", target_os = "none")))]
pub fn clean_range_to_poc(base: u64, len: u64) {
    #[cfg(test)]
    record_poc_sweep(base, len);
    #[cfg(not(test))]
    let _ = (base, len);
}

#[cfg(test)]
std::thread_local! {
    static POC_SWEEPS: std::cell::RefCell<std::vec::Vec<(u64, u64)>> =
        const { std::cell::RefCell::new(std::vec::Vec::new()) };
}

/// Record a `(base, len)` point-of-coherency sweep for the current test
/// thread (see the host [`clean_invalidate_range_to_poc`] stand-in).
#[cfg(test)]
fn record_poc_sweep(base: u64, len: u64) {
    POC_SWEEPS.with(|log| log.borrow_mut().push((base, len)));
}

/// Drain and return the point-of-coherency sweeps recorded on this test
/// thread since the last drain.
#[cfg(test)]
pub(crate) fn take_recorded_poc_sweeps() -> std::vec::Vec<(u64, u64)> {
    POC_SWEEPS.with(|log| core::mem::take(&mut *log.borrow_mut()))
}

/// Smallest data-cache line size in bytes encoded by a `CTR_EL0` value:
/// `DminLine` (bits `[19:16]`) is the log2 of that line's length in
/// *words*, so the byte length is `4 << DminLine` (ARM ARM D13.2.34 —
/// 64 bytes on the Cortex-A72's `0x8444_C004`). Pure so the decode is
/// host-unit-tested; the freestanding cache-maintenance sweep
/// ([`clean_invalidate_range_to_poc`]) feeds it the live register.
#[must_use]
pub const fn dcache_line_bytes(ctr_el0: u64) -> usize {
    4 << ((ctr_el0 >> 16) & 0xF)
}

/// Smallest instruction-cache line size in bytes encoded by a `CTR_EL0`
/// value: `IminLine` (bits `[3:0]`) is the log2 of that line's length in
/// *words*, so the byte length is `4 << IminLine` (ARM ARM D13.2.34).
/// Pure so the decode is host-unit-tested; the freestanding
/// instruction-cache sync ([`crate::kernel_arch::sync_instruction_cache_range`])
/// feeds it the live register. Separate from [`dcache_line_bytes`] because
/// the I- and D-cache minimum line sizes can differ, and `ic ivau` /
/// `dc cvau` must each step by their own line.
#[must_use]
pub const fn icache_line_bytes(ctr_el0: u64) -> usize {
    4 << (ctr_el0 & 0xF)
}

/// One page-table page: 512 × u64, naturally aligned.
#[repr(C, align(4096))]
struct Table([u64; ENTRIES_PER_TABLE]);

impl Table {
    const fn new() -> Self {
        Self([0; ENTRIES_PER_TABLE])
    }
}

/// Default pool capacity: what the memory-isolation test and the small
/// bootstrap spaces need — two [`AddressSpace`]s, each a root plus a
/// 3-level walk for the extra 4 KiB mapping, with spares. A consumer with a
/// different, *derived* demand instantiates [`PageTablePool`] with its own
/// capacity instead of changing this default for everyone.
const POOL_SIZE: usize = 16;

/// A statically-allocated pool of zero-initialised page-table pages.
///
/// Allocation is monotonic — frames are never freed — which matches the
/// set-up → run → exit lifecycle of the isolation test. A real allocator
/// lives in `kernel/mem` and is wired in by a later stage.
pub struct PageTablePool<const CAPACITY: usize = POOL_SIZE> {
    storage: [UnsafeCell<Table>; CAPACITY],
    used: AtomicUsize,
}

// SAFETY: the pool exposes `&self` allocation but every allocated frame is
// handed out exactly once, by a monotonic `fetch_add`, so distinct
// allocations never alias.
unsafe impl<const CAPACITY: usize> Sync for PageTablePool<CAPACITY> {}

impl<const CAPACITY: usize> Default for PageTablePool<CAPACITY> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const CAPACITY: usize> PageTablePool<CAPACITY> {
    /// Construct an empty pool. `const`, so the pool lives in `.bss`.
    #[must_use]
    pub const fn new() -> Self {
        // The array initialiser needs a `const`, and copying it per slot is
        // the point: each element must be its own independent table.
        #[allow(clippy::declare_interior_mutable_const)]
        const ZERO: UnsafeCell<Table> = UnsafeCell::new(Table::new());
        // Built in a `const fn`, so the pool lands in `.bss` rather than on a
        // stack frame.
        #[allow(clippy::large_stack_arrays)]
        let storage = [ZERO; CAPACITY];
        Self {
            storage,
            used: AtomicUsize::new(0),
        }
    }

    /// Allocate a fresh, zero-initialised table page.
    ///
    /// Returns `None` when the pool is exhausted, which callers fail closed
    /// on as a deterministic OOM, never a panic.
    pub fn alloc(&self) -> Option<&'static mut [u64; ENTRIES_PER_TABLE]> {
        let idx = self.used.fetch_add(1, Ordering::SeqCst);
        if idx >= CAPACITY {
            // Park the counter at the cap so a pathological number of
            // post-exhaustion calls cannot wrap it.
            self.used.store(CAPACITY, Ordering::SeqCst);
            return None;
        }
        // SAFETY: the monotonic counter hands this index to this call alone,
        // so the returned `&'static mut` never aliases another.
        let cell = &self.storage[idx];
        let table_ref: &'static mut Table = unsafe { &mut *cell.get() };
        Some(&mut table_ref.0)
    }
}

impl<const CAPACITY: usize> PageTableFrames for PageTablePool<CAPACITY> {
    fn alloc_table(&self) -> Option<TableFrame> {
        let entries = self.alloc()?;
        let phys = image_phys(entries.as_ptr() as u64);
        Some(TableFrame { phys, entries })
    }

    fn table_at(&self, phys: u64) -> Option<*mut [u64; ENTRIES_PER_TABLE]> {
        // Recovered from the slot the pool handed `phys` out of, so the
        // pointer keeps its storage's provenance; a `phys` from anywhere
        // else names no slot and the walk asking for it fails closed.
        let index = pool_slot_of(image_phys(self.storage.as_ptr() as u64), CAPACITY, phys)?;
        Some(self.storage[index].get().cast())
    }

    fn free_table(&self, phys: u64) {
        // The boot pool is a bump allocator over permanent kernel-image
        // `.bss`: its storage is never reclaimable RAM and the boot space
        // built over it is never torn down, so a returned frame is retired
        // without reuse. Per-process spaces draw from the allocator-backed
        // `kernel/mem` source, whose `free_table` genuinely recycles.
        let _ = phys;
    }
}

/// A stage-1 address space built on a freshly-allocated L1 root table: a
/// process's `TTBR0_EL1` regime, which carries nothing of the kernel's, or a
/// handle onto the kernel remap window.
pub struct AddressSpace {
    root_phys: u64,
    /// The frame source the page-table walk allocates intermediate
    /// tables from, retained so the [`tairix_arch_api::mmu::AddressSpace`]
    /// HAL impl can install mappings without the caller re-supplying it.
    /// The static [`PageTablePool`] is the boot/bootstrap source; a real
    /// per-process space is built over the `kernel/mem` frame-allocator
    /// source (`plans/WIRING.md` W5b-3).
    frames: &'static dyn PageTableFrames,
    /// Which regime this root serves. Every mapping operation refuses an
    /// address the regime does not hold, so a kernel-window address can
    /// never be walked into a user root's identically-indexed slot.
    regime: Regime,
}

impl AddressSpace {
    /// Build an empty process root: the kernel regime lives in `TTBR1_EL1`,
    /// which every CPU carries, so a process root holds only what the
    /// process maps.
    ///
    /// # Errors
    ///
    /// Returns `None` if the frame source cannot supply the root table.
    pub fn new_user(frames: &'static dyn PageTableFrames) -> Option<Self> {
        let TableFrame {
            phys: root_phys,
            entries: _,
        } = frames.alloc_table()?;
        Some(Self {
            root_phys,
            frames,
            regime: Regime::User,
        })
    }

    /// Build a root that maps **only** the kernel remap window — the handle
    /// the kernel-heap remap layer edits the window's shared sub-hierarchy
    /// through.
    ///
    /// The root is never activated: the window's L1 descriptors point at
    /// tables the live kernel root shares, so a leaf installed through this
    /// space is immediately visible under the `TTBR1_EL1` regime. Keeping
    /// it separate means the remap layer draws its intermediate tables from
    /// the frame allocator rather than from the fixed boot pool, and its
    /// regime refuses every address outside the window.
    ///
    /// # Errors
    ///
    /// Returns `None` if the frame source cannot supply the root table.
    pub fn new_kernel_window(frames: &'static dyn PageTableFrames) -> Option<Self> {
        let TableFrame {
            phys: root_phys,
            entries: root,
        } = frames.alloc_table()?;
        install_kernel_window_slots(root);
        Some(Self {
            root_phys,
            frames,
            regime: Regime::KernelWindow,
        })
    }

    /// The L1 root table's descriptors, recovered through the frame source
    /// that drew it, or [`None`] when the source cannot reach it (fail
    /// closed).
    ///
    /// The space retains only `root_phys`, never a reference: another CPU
    /// walks the root while it is active ([`set_accessed_flag_in_active`]).
    fn root_table(&self) -> Option<&[AtomicU64; ENTRIES_PER_TABLE]> {
        // SAFETY: `root_phys` names this space's L1 table, drawn from
        // `self.frames`, and it lives as long as the space.
        Some(unsafe { descriptors(self.frames.table_at(self.root_phys)?) })
    }

    /// The valid leaf covering `vaddr` in this hierarchy, or [`None`].
    fn leaf(&self, vaddr: u64) -> Option<Leaf<'_>> {
        // SAFETY: as `root_table`.
        unsafe { leaf_in(self.frames, self.root_phys, vaddr) }
    }

    /// The 4 KiB page leaf covering `vaddr`, or [`None`] where none does: a
    /// block on the way is never shattered by a per-page operation.
    fn page_leaf(&self, vaddr: u64) -> Option<Leaf<'_>> {
        self.leaf(vaddr)
            .filter(|leaf| leaf.shift == table_shift(LEVELS))
    }

    /// Map `paddr` at `vaddr` with 4 KiB granularity using the supplied
    /// page-leaf `leaf_attrs` (e.g. [`el0_code_leaf_attrs`] /
    /// [`el0_data_leaf_attrs`] for an EL0 user mapping): the port's one
    /// 4 KiB walk.
    ///
    /// `vaddr` and `paddr` must be page-aligned. Returns `None` for a leaf
    /// both writable and executable, on page-table-pool exhaustion, or if
    /// the walk meets an existing block it would have to shatter.
    pub fn map_4k_with_attrs(
        &mut self,
        frames: &'static dyn PageTableFrames,
        vaddr: u64,
        paddr: u64,
        leaf_attrs: u64,
    ) -> Option<()> {
        if (vaddr & (PAGE_SIZE as u64 - 1)) != 0
            || (paddr & (PAGE_SIZE as u64 - 1)) != 0
            || !self.regime.holds(vaddr)
            || writable_and_executable(leaf_attrs)
        {
            return None;
        }
        let l2 = ensure_child(self.root_table()?, table_index(vaddr, 1), frames)?;
        let l3 = ensure_child(l2, table_index(vaddr, 2), frames)?;
        let slot = &l3[table_index(vaddr, 3)];
        if (slot.load(Ordering::Relaxed) & attrs::VALID) != 0 {
            return None;
        }
        slot.store(descriptor(paddr, leaf_attrs), Ordering::Relaxed);
        Some(())
    }

    /// Physical address of the L1 root table (the value programmed into
    /// `TTBR0_EL1`). Exposed so tests can observe it.
    #[must_use]
    pub fn root_phys(&self) -> u64 {
        self.root_phys
    }

    /// Translate the architecture-neutral [`PageFlags`] into a stage-1
    /// page-leaf attribute word (one neutral
    /// vocabulary, decoded once at the HAL boundary). W^X is the default: an executable user page is mapped read-only
    /// ([`el0_code_leaf_attrs`]); a writable user page is execute-never
    /// ([`el0_data_leaf_attrs`]); a read-only user page is execute-never
    /// ([`el0_rodata_leaf_attrs`]). A kernel page is read/write and executes
    /// nowhere, or, asked to execute, read-only and executable at EL1 alone
    /// ([`KernelAccess`]); a kernel Device page uses
    /// [`device_leaf_attrs`], and an **EL0** Device page (a user-space
    /// driver's `mmio_map` window, `DEVICE | USER`) uses the EL0-accessible
    /// [`el0_device_leaf_attrs`] — otherwise the driver would take a
    /// permission fault reading its own mapped register (`plans/PI.md` P10
    /// chunk 5d-0).
    fn leaf_attrs_for(flags: PageFlags) -> u64 {
        let mark = if flags.contains(PageFlags::DMA) {
            attrs::SW_DMA
        } else {
            0
        };
        Self::memory_attrs_for(flags) | mark
    }

    /// [`Self::leaf_attrs_for`]'s attributes but for the software DMA mark.
    fn memory_attrs_for(flags: PageFlags) -> u64 {
        if flags.contains(PageFlags::DEVICE) {
            if flags.contains(PageFlags::USER) {
                el0_device_leaf_attrs()
            } else {
                device_leaf_attrs(false)
            }
        } else if flags.contains(PageFlags::WRITE_COMBINE)
            || flags.contains(PageFlags::DMA_COHERENT)
        {
            // Normal Non-Cacheable: a write-combined surface, or a buffer
            // shared with a non-I/O-coherent DMA master (the BCM2711 PCIe
            // root complex), which then sees the CPU's writes without cache
            // maintenance while ring loads and stores still work (Device
            // memory would forbid them). EL0 reaches it only if asked.
            let leaf = if flags.contains(PageFlags::USER) {
                el0_dma_coherent_leaf_attrs()
            } else {
                dma_coherent_leaf_attrs()
            };
            if flags.contains(PageFlags::WRITE_COMBINE) {
                leaf | attrs::SW_WRITE_COMBINE
            } else {
                leaf
            }
        } else if flags.contains(PageFlags::USER) {
            if flags.contains(PageFlags::EXEC) {
                el0_code_leaf_attrs()
            } else if flags.contains(PageFlags::WRITE) {
                el0_data_leaf_attrs()
            } else {
                el0_rodata_leaf_attrs()
            }
        } else if flags.contains(PageFlags::EXEC) {
            KernelAccess::Execute.leaf(false)
        } else {
            KernelAccess::Write.leaf(false)
        }
    }
}

// Invalidate the calling CPU's whole local data cache, every level to the
// point of coherency, by set/way (`dc isw`), then `dsb sy; isb`: what a
// secondary does before turning translation on, its caches in an UNKNOWN
// state (`smp.s`). Invalidate, never clean, or a stale line would be written
// over the tables. A leaf routine using only x0–x11 and no stack, called with
// the MMU off.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
core::arch::global_asm!(
    ".section .text",
    ".globl _invalidate_local_dcache_to_poc",
    "_invalidate_local_dcache_to_poc:",
    "  mrs   x0, clidr_el1",
    "  and   x3, x0, #0x7000000", // LoC in CLIDR[26:24]
    "  lsr   x3, x3, #23",        // x3 = LoC << 1 (loop bound in 2*level units)
    "  cbz   x3, 5f",             // no cache levels to the PoC → done
    "  mov   x10, #0",            // x10 = 2 * current cache level
    "1:",
    "  add   x2, x10, x10, lsr #1", // x2 = 3*level = bit offset of this level's Ctype
    "  lsr   x1, x0, x2",
    "  and   x1, x1, #7", // x1 = cache type at this level
    "  cmp   x1, #2",
    "  b.lt  4f",              // <2: no data/unified cache here → skip
    "  msr   csselr_el1, x10", // select data/unified cache at this level (InD=0)
    "  isb",
    "  mrs   x1, ccsidr_el1",
    "  and   x2, x1, #7",
    "  add   x2, x2, #4", // x2 = log2(line bytes)
    "  mov   x4, #0x3ff",
    "  and   x4, x4, x1, lsr #3", // x4 = associativity - 1 (max way)
    "  clz   w5, w4",             // w5 = bit position for the way field
    "  mov   x7, #0x7fff",
    "  and   x7, x7, x1, lsr #13", // x7 = number of sets - 1 (max set)
    "2:",
    "  mov   x9, x4", // x9 = way iterator
    "3:",
    "  lsl   x6, x9, x5",
    "  orr   x11, x10, x6", // set/way operand: level | way
    "  lsl   x6, x7, x2",
    "  orr   x11, x11, x6", // | set
    "  dc    isw, x11",     // invalidate this set/way (never clean)
    "  subs  x9, x9, #1",
    "  b.ge  3b",
    "  subs  x7, x7, #1",
    "  b.ge  2b",
    "4:",
    "  add   x10, x10, #2", // next cache level (2*level units)
    "  cmp   x3, x10",
    "  b.gt  1b",
    "5:",
    "  dsb   sy",
    "  isb",
    "  ret",
);

impl MmuAddressSpace for AddressSpace {
    fn map_page(&mut self, vaddr: u64, paddr: u64, flags: PageFlags) -> Result<(), MapError> {
        if (vaddr & (PAGE_SIZE as u64 - 1)) != 0 || (paddr & (PAGE_SIZE as u64 - 1)) != 0 {
            return Err(MapError::Misaligned);
        }
        if flags.is_write_exec() || !self.regime.holds(vaddr) {
            return Err(MapError::InvalidFlags);
        }
        if self.leaf(vaddr).is_some() {
            return Err(MapError::AlreadyMapped);
        }
        let frames = self.frames;
        let attrs = Self::leaf_attrs_for(flags) | self.regime.leaf_scope();
        // Alignment and prior-mapping are already ruled out, so the only
        // remaining failure from the walk is frame-source exhaustion.
        self.map_4k_with_attrs(frames, vaddr, paddr, attrs)
            .ok_or(MapError::PoolExhausted)
    }

    fn translate(&self, vaddr: u64) -> Option<(u64, PageFlags)> {
        if !self.regime.holds(vaddr) {
            return None;
        }
        let leaf = self.leaf(vaddr)?;
        Some((
            resolved_page(phys_from_descriptor(leaf.seen), vaddr, leaf.shift),
            page_flags_from_leaf(leaf.seen),
        ))
    }

    fn unmap(&mut self, vaddr: u64) -> Result<u64, MapError> {
        if (vaddr & (PAGE_SIZE as u64 - 1)) != 0 {
            return Err(MapError::Misaligned);
        }
        if !self.regime.holds(vaddr) {
            return Err(MapError::NotMapped);
        }
        let leaf = self.page_leaf(vaddr).ok_or(MapError::NotMapped)?;
        Ok(phys_from_descriptor(leaf.slot.swap(0, Ordering::Relaxed)))
    }

    fn root_phys(&self) -> u64 {
        self.root_phys
    }

    fn access_tracking(&self) -> AccessTracking {
        // The per-page referenced bit the cold-page scanner
        // (`kernel/mem::coldscan`) needs is the Access Flag (AF, bit 10).
        // aarch64 manages AF in software: the port leaves `TCR_EL1.HA`
        // clear, so every part, the Pi 5's A76 with HAFDBS included, raises
        // an Access-Flag fault when a valid leaf whose AF is clear is
        // accessed. `test_and_clear_accessed`
        // clears AF (and invalidates the leaf's TLB entry); the next access
        // then takes that fault, which the synchronous-exception path
        // (`crate::exceptions`) resolves by setting AF back through
        // [`set_accessed_flag_in_active`] and retrying — so a probe reading
        // AF still clear proves the page went untouched. The whole clock
        // round-trip is proven by the `accessed-bit-qemu-aarch64` vertical,
        // on the baseline model, which lacks HAFDBS.
        AccessTracking::Supported
    }

    fn test_and_clear_accessed(&mut self, vaddr: u64) -> Result<bool, MapError> {
        if (vaddr & (PAGE_SIZE as u64 - 1)) != 0 {
            return Err(MapError::Misaligned);
        }
        if !self.regime.holds(vaddr) {
            return Err(MapError::NotMapped);
        }
        // The tier tracks only 4 KiB anonymous leaves, never a coarse block.
        let leaf = self.page_leaf(vaddr).ok_or(MapError::NotMapped)?;
        let was_accessed = (leaf.seen & attrs::AF) != 0;
        if was_accessed {
            // Clear the Access Flag so the next access raises an
            // Access-Flag fault the exception path re-sets; a later probe
            // reading it still clear proves the page went untouched in
            // between (the clock scan). Invalidate the stale TLB entry so
            // the cleared flag is observed on the next translation rather
            // than served from a cached descriptor that still carries AF.
            leaf.slot.fetch_and(!attrs::AF, Ordering::Relaxed);
            invalidate_page_inner_shareable(vaddr);
        }
        Ok(was_accessed)
    }

    unsafe fn activate(&self) {
        // SAFETY: the kernel lives in `TTBR1_EL1`, which loading a process
        // root under the boot ASID leaves untouched.
        unsafe {
            activate_user_root(
                self.root_phys,
                AddressTag {
                    tag: 0,
                    flush: true,
                },
            );
        }
    }

    unsafe fn reclaim_table_frames(&mut self) {
        // A kernel-window handle owns nothing reclaimable: every table it
        // can reach is the live regime's shared sub-hierarchy, so walking
        // it would free the kernel heap's own page tables. The window is
        // permanent; its handle retires without freeing (fail closed).
        if self.regime == Regime::KernelWindow {
            return;
        }
        // Defence in depth: the dispatcher parks a CPU off a user root at
        // every task suspend, so a dead space's root is never the active
        // translation here — but freeing the walked-from root of a live
        // regime would be catastrophic, so verify and re-park first. With
        // no park root published the frames are retired unreclaimed
        // rather than dismantling the active translation (fail closed).
        if active_root_phys() == self.root_phys && !park_kernel_root(true) {
            return;
        }
        let frames = self.frames;
        // A stage-1 hierarchy rooted at L1: an L1/L2 entry that is valid
        // and not a block is a table pointer; L3 (depth 2) entries are
        // page leaves and are never descended into.
        let child_of = |entry: u64, depth: usize| -> Option<u64> {
            (depth < 2 && (entry & attrs::VALID) != 0 && !is_block(entry))
                .then(|| phys_from_descriptor(entry))
        };
        // SAFETY: every phys `child_of` yields was written by
        // `ensure_child` from a `TableFrame` of `self.frames`, so it names
        // a live table this hierarchy owns and the source can reach; the
        // guards above uphold the not-active contract the caller asserts,
        // and `self` is borrowed mutably so no other reference walks the
        // tables.
        unsafe {
            reclaim_hierarchy(self.root_phys, frames, &child_of);
        }
    }
}

impl TlbShootdown for AddressSpace {
    fn flush_page(&mut self, vaddr: u64) {
        invalidate_page_inner_shareable(vaddr);
    }

    fn flush_range(&mut self, _start_vaddr: u64, page_count: usize) {
        if page_count != 0 {
            invalidate_all_inner_shareable();
        }
    }

    fn publish_mappings(&mut self, _start_vaddr: u64, page_count: usize) {
        // A leaf that was invalid cannot be cached, so installing one owes
        // the walker ordering, not invalidation — and the range flush above
        // is a whole-domain broadcast, which would turn every mapping
        // installation into a system-wide TLB wipe.
        if page_count != 0 {
            publish_table_update();
        }
    }
}

/// The `TLBI VAAE1IS` register operand for the page holding `vaddr`:
/// `VA[55:12]` in bits `[43:0]`, and nothing else.
///
/// The mask is not cosmetic. Above the VA field sit `TTL` (bits `[47:44]`,
/// a translation-level hint) and `ASID` (bits `[63:48]`, RES0 for the
/// all-ASID variant). A bare `vaddr >> 12` leaves `VA[63:56]` sitting in
/// both: harmless for a low address, where those bits are zero, but a
/// kernel-regime address carries all-ones there and would encode
/// `TTL = 0b1111` — a 64 KiB-granule level-3 hint that entitles the
/// implementation to leave this port's 4 KiB entry in the TLB. A stale
/// translation surviving an unmap is a use-after-free of the frame behind
/// it, and an emulator that ignores `TTL` cannot show it.
///
/// Compiled on the host for its unit test; the `tlbi` that consumes it is
/// freestanding-only.
#[cfg(any(all(target_arch = "aarch64", target_os = "none"), test))]
const fn tlbi_page_operand(vaddr: u64) -> u64 {
    const VA_FIELD_BITS: u32 = 44;
    (vaddr >> 12) & ((1u64 << VA_FIELD_BITS) - 1)
}

/// Invalidate, on every PE in the inner-shareable domain, the stage-1
/// EL1&0 TLB entries for the 4 KiB page containing `vaddr` (all ASIDs).
///
/// This is the single instruction sequence shared by both the *local*
/// per-page flush ([`TlbShootdown::flush_page`]) and the *cross-CPU*
/// shootdown ([`tairix_arch_api::CrossCpuTlbShootdown::shootdown_page`] on
/// [`crate::kernel_arch::Aarch64Arch`]): `tlbi vaae1is` is the
/// inner-shareable *broadcast* variant, so the "local" and "cross-CPU"
/// shootdowns are literally the same operation on aarch64 — there is one
/// implementation, not two.
pub(crate) fn invalidate_page_inner_shareable(vaddr: u64) {
    invalidate_range_inner_shareable(vaddr, 1);
}

/// Invalidate, on every PE in the inner-shareable domain, the stage-1
/// EL1&0 TLB entries for `pages` consecutive 4 KiB pages from `start_vaddr`
/// (all ASIDs), paying one barrier pair for the whole range.
///
/// The single-page flush is this with `pages == 1`, so there is one
/// invalidation sequence on this port rather than two. A bounded range —
/// the kernel-heap teardown batch — costs one `tlbi` per page and one
/// synchronisation, which beats both a per-page barrier pair and the
/// whole-domain [`invalidate_all_inner_shareable`] sledgehammer; a range
/// past [`RANGE_INVALIDATION_CEILING`] is that sledgehammer instead.
pub(crate) fn invalidate_range_inner_shareable(start_vaddr: u64, pages: usize) {
    if pages == 0 {
        return;
    }
    if invalidates_whole_domain(pages) {
        invalidate_all_inner_shareable();
        return;
    }
    #[cfg(all(target_arch = "aarch64", target_os = "none"))]
    {
        // SAFETY: `tlbi vaae1is` invalidates the inner-shareable TLB
        // entries for the page named by its operand (VA[55:12], all
        // ASIDs); the leading `dsb ishst` orders the table stores before
        // the invalidations and the trailing `dsb ish` + `isb` make them
        // visible before the next translation. The sequence touches no
        // memory and only discards cached translations. No Rust spelling
        // exists.
        unsafe {
            core::arch::asm!("dsb ishst", options(nostack, preserves_flags));
            let mut vaddr = start_vaddr;
            for _ in 0..pages {
                core::arch::asm!(
                    "tlbi vaae1is, {page}",
                    page = in(reg) tlbi_page_operand(vaddr),
                    options(nostack, preserves_flags),
                );
                vaddr = vaddr.wrapping_add(PAGE_SIZE as u64);
            }
            core::arch::asm!("dsb ish", "isb", options(nostack, preserves_flags));
        }
    }
    #[cfg(not(all(target_arch = "aarch64", target_os = "none")))]
    {
        // The host has no TLB to invalidate; a flush is vacuous.
        let _ = start_vaddr;
    }
}

/// Past this many pages one whole-domain invalidation is taken to be cheaper
/// than a `tlbi` each, every one a broadcast each PE must answer; any ceiling
/// is correct, only the cost moves. It also bounds a
/// sparse unmap's span, which would otherwise cost a broadcast per page of
/// its gaps.
const RANGE_INVALIDATION_CEILING: usize = 512;

/// Whether invalidating `pages` pages should invalidate the whole domain
/// instead.
const fn invalidates_whole_domain(pages: usize) -> bool {
    pages > RANGE_INVALIDATION_CEILING
}

/// Invalidate every stage-1 EL1&0 translation on every PE in the
/// inner-shareable domain.
///
/// A large newly-installed range uses this broad operation once instead of
/// issuing a barrier-bracketed `tlbi` for every 4 KiB leaf. Over-invalidation
/// is safe, while the single broadcast keeps the range visible to every PE
/// before execution continues.
pub(crate) fn invalidate_all_inner_shareable() {
    #[cfg(all(target_arch = "aarch64", target_os = "none"))]
    {
        // SAFETY: `tlbi vmalle1is` invalidates all stage-1 EL1&0
        // translations in the inner-shareable domain. The barriers order
        // prior page-table writes before invalidation and subsequent
        // translations after completion. It only discards cached state.
        unsafe {
            core::arch::asm!(
                "dsb ishst",
                "tlbi vmalle1is",
                "dsb ish",
                "isb",
                options(nostack, preserves_flags),
            );
        }
    }
}

/// The root [`park_kernel_root`] parks on: `EMPTY_ROOT`, once the kernel
/// root is entered.
static PARK_ROOT: ParkRoot = ParkRoot::new();

/// The root [`park_kernel_root`] parks on, or `None` before the kernel root
/// is entered.
#[must_use]
pub fn park_root() -> Option<u64> {
    PARK_ROOT.published()
}

/// Park the calling CPU's low translation regime on the empty root under
/// ASID `0`, discarding that ASID's entries first when `flush`, so no user
/// space's root remains active after its task suspends or exits — and, on a
/// CPU fresh from its trampoline, nor the identity map's page. Returns
/// `false`, changing nothing, before the kernel root is entered (fail
/// closed).
///
/// The dispatcher calls this after every switch-back from a user task;
/// address-space teardown and the machine takeover call it, flushing,
/// before dismantling a root that is somehow still active.
pub fn park_kernel_root(flush: bool) -> bool {
    // SAFETY: the published root maps nothing, and the kernel runs from
    // `TTBR1_EL1` — `activate_user_root`'s contract (inert on the host, where
    // the root is never published anyway).
    unsafe { PARK_ROOT.park(flush, activate_user_root) }
}

/// Where `TTBR0_EL1` holds the ASID (`TCR_EL1.A1` clear).
const TTBR_ASID_SHIFT: u32 = 48;

/// The ASID field of `TTBR0_EL1`.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
const TTBR_ASID_MASK: u64 = !(u64::MAX >> (64 - TTBR_ASID_SHIFT));

/// The Common-not-Private bit of `TTBR0_EL1`.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
const TTBR_CNP: u64 = 1;

/// The `TTBR0_EL1` value loading `root_phys` under ASID `tag`.
#[must_use]
pub const fn ttbr0(root_phys: u64, tag: u16) -> u64 {
    root_phys | ((tag as u64) << TTBR_ASID_SHIFT)
}

/// The port's address-space tags: an ASID in `TTBR0_EL1` beside each root,
/// and a non-global bit on each of a root's own leaves.
pub struct Aarch64Tags;

/// The aarch64 port's tags, which the kernel loads user roots and parks by.
pub static TAGS: Aarch64Tags = Aarch64Tags;

impl AddressTags for Aarch64Tags {
    /// Every implementation has at least 8-bit ASIDs, which `TCR_EL1.AS`
    /// clear selects; `0` is the park root's.
    fn user_tags(&self) -> u16 {
        u16::from(u8::MAX)
    }

    fn root_register(&self, root_phys: u64, tag: u16) -> u64 {
        ttbr0(root_phys, tag)
    }

    /// A user space is invalidated by `tlbi vaae1is` or `tlbi vmalle1is`:
    /// every ASID, broadcast to every PE in the inner-shareable domain.
    fn invalidation_reaches_every_tag(&self) -> bool {
        true
    }

    unsafe fn flush_current_tag(&self) {
        // SAFETY: the caller runs in kernel mode on the dispatcher's context.
        unsafe { flush_current_asid() };
    }

    unsafe fn park(&self, flush: bool) -> bool {
        park_kernel_root(flush)
    }
}

/// Discard every non-global entry of the ASID `TTBR0_EL1` is loaded under,
/// on this PE alone.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
unsafe fn flush_current_asid() {
    // SAFETY: `tlbi aside1` takes the ASID in bits [63:48], exactly where
    // `TTBR0_EL1` holds it; it invalidates only this PE's entries of that
    // ASID, and the `dsb nsh`/`isb` pair completes it before the next access.
    // No memory is touched and no Rust spelling exists for these.
    unsafe {
        core::arch::asm!(
            "mrs {ttbr}, TTBR0_EL1",
            "and {ttbr}, {ttbr}, {asid}",
            "tlbi aside1, {ttbr}",
            "dsb nsh",
            "isb",
            ttbr = out(reg) _,
            asid = in(reg) TTBR_ASID_MASK,
            options(nostack, preserves_flags),
        );
    }
}

/// Host substitute: there are no TLBs to discard.
#[cfg(not(all(target_arch = "aarch64", target_os = "none")))]
unsafe fn flush_current_asid() {}

/// The physical root of the calling CPU's active low translation regime
/// (`TTBR0_EL1`'s base address), or `0` on the host, which has no
/// translation registers.
fn active_root_phys() -> u64 {
    #[cfg(all(target_arch = "aarch64", target_os = "none"))]
    {
        let ttbr: u64;
        // SAFETY: reading `TTBR0_EL1` observes the active root without
        // side effects; no Rust spelling exists for the system register.
        unsafe {
            core::arch::asm!("mrs {v}, TTBR0_EL1", v = out(reg) ttbr, options(nostack, preserves_flags, nomem));
        }
        // The table base, without the ASID or the CnP bit ([0]).
        ttbr & !(TTBR_ASID_MASK | TTBR_CNP)
    }
    #[cfg(not(all(target_arch = "aarch64", target_os = "none")))]
    {
        0
    }
}

/// Set the Access Flag on the leaf of the *active* stage-1 translation
/// regime (`TTBR0_EL1`) covering `vaddr`, resolving an Access-Flag fault
/// the software-managed referenced bit raised.
///
/// This is the exception-path counterpart of
/// [`AddressSpace::test_and_clear_accessed`]: after the cold-page scanner
/// clears a leaf's AF (bit 10), the next access to that page takes an
/// Access-Flag fault, since the port leaves `TCR_EL1.HA` clear and no part
/// sets it in its walk. The synchronous-exception handler
/// (`crate::exceptions`) calls this with `FAR_EL1` to set AF back on the
/// faulting leaf, so the retried instruction succeeds and a later probe sees
/// the page was touched.
///
/// It walks the live tables through the frame source that drew them and
/// answers whether the access is to be retried: a **valid** leaf covers
/// `vaddr`, AF now set on it, whether this CPU set it or another CPU's
/// fix-up set it first. Only an address no valid leaf covers takes the
/// ordinary fault path (fail closed: this never fabricates a mapping). Any
/// other cause of the fault is reported on the retry as its own, the Access
/// flag being checked ahead of permissions. It allocates nothing and is
/// sound in exception context.
///
/// It takes no lock, so `unmap` or the scanner may be changing the same
/// leaf on another CPU. AF is set only by an exchange from the descriptor
/// the walk read: a leaf unmapped, remapped or re-permissioned meanwhile is
/// never written back, which would hand the retried access a frame its
/// space no longer owns.
///
/// Only a `TTBR0_EL1` address is resolved: the referenced-bit clock tracks
/// anonymous user leaves, and every kernel-regime leaf is mapped with AF
/// already set, so a kernel address here is not this mechanism's fault. It
/// would otherwise index the *user* root with the kernel regime's nine
/// bits and fix up an unrelated leaf.
#[must_use]
pub fn set_accessed_flag_in_active(vaddr: u64) -> bool {
    if !Regime::User.holds(vaddr) {
        return false;
    }
    let root_phys = active_root_phys();
    if root_phys == 0 {
        return false;
    }
    let Some(frames) = active_frames() else {
        return false;
    };
    // SAFETY: `root_phys` is the L1 table of a space this port built, so the
    // published production source reaches it and every table below it, and
    // its space outlives the task this CPU is running in it.
    unsafe { set_accessed_flag_in_root(frames, root_phys, vaddr) }
}

/// Walk the stage-1 hierarchy rooted at `root_phys` and set the Access
/// Flag on the valid leaf covering `vaddr`, as [`set_accessed_flag_in_active`]
/// does for the live root. Returns whether the access is to be retried: a
/// valid leaf covers `vaddr`.
///
/// # Safety
///
/// As [`leaf_in`].
#[must_use]
unsafe fn set_accessed_flag_in_root(
    frames: &dyn PageTableFrames,
    root_phys: u64,
    vaddr: u64,
) -> bool {
    // SAFETY: the function contract, forwarded.
    let Some(leaf) = (unsafe { leaf_in(frames, root_phys, vaddr) }) else {
        return false;
    };
    if leaf.set_while(attrs::AF, retries_access).is_none() {
        return false;
    }
    // No TLB caches an entry that faults on its Access flag, so setting the
    // flag owes the walkers ordering, not an invalidation.
    publish_table_update();
    true
}

/// Whether an Access-flag fault on the leaf holding `desc` is retried: while
/// it is valid, whether this CPU sets the flag or another CPU's fix-up set
/// it first.
const fn retries_access(desc: u64) -> bool {
    (desc & attrs::VALID) != 0
}

/// The valid leaf, block or page, covering `vaddr` in the hierarchy rooted
/// at `root_phys`, or [`None`] where no valid leaf does.
///
/// # Safety
///
/// `root_phys` must be an L1 table drawn from `frames`, whose descendant
/// tables `frames` can therefore reach, and the hierarchy must stay
/// allocated for `'a`.
unsafe fn leaf_in<'a>(
    frames: &dyn PageTableFrames,
    root_phys: u64,
    vaddr: u64,
) -> Option<Leaf<'a>> {
    let mut table = frames.table_at(root_phys)?;
    for level in 1..=LEVELS {
        // SAFETY: `table` is the root, or the table a valid table descriptor
        // above names, of a hierarchy the contract keeps allocated for `'a`.
        let slot = &unsafe { descriptors(table) }[table_index(vaddr, level)];
        let seen = slot.load(WALK_ORDERING);
        if (seen & attrs::VALID) == 0 {
            return None;
        }
        if level == LEVELS || is_block(seen) {
            return Some(Leaf {
                slot,
                seen,
                shift: table_shift(level),
            });
        }
        table = frames.table_at(phys_from_descriptor(seen))?;
    }
    None
}

/// Reactivate `root_phys` as the active stage-1 EL1&0 translation root under
/// `tag`'s ASID (reprogram `TTBR0_EL1`) on a CPU whose MMU is already
/// enabled, discarding that ASID's entries first when the tag says so.
///
/// This is the `SP2b` user-kthread `pre_resume` primitive (`plans/SPAWN.md`
/// SP2): immediately before the kernel `eret`s back into a user task's
/// EL0, that task's own page-table root must be installed so its
/// translations — and only its — are in force, keeping sibling processes
/// hardware-isolated. A root's own leaves are not global, so the entries of
/// every other ASID stay in the TLB unseen, and the space they belong to
/// finds them again when it next runs.
///
/// Only the low (`TTBR0_EL1`) regime changes between user spaces: the
/// kernel runs from `TTBR1_EL1`, which every CPU loaded once.
///
/// # Safety
///
/// Translation is on, the caller runs from the kernel regime, and
/// `root_phys` is an L1 table of this port's that stays allocated while it
/// is loaded.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
pub unsafe fn activate_user_root(root_phys: u64, tag: AddressTag) {
    let ttbr = ttbr0(root_phys, tag.tag);
    // SAFETY: writing `TTBR0_EL1` swaps the low translation regime and its
    // ASID in one write; the `isb` makes it in force before the next access.
    // The caller's contract guarantees the new root covers the running kernel
    // context.
    unsafe {
        core::arch::asm!(
            "msr TTBR0_EL1, {ttbr}",
            "isb",
            ttbr = in(reg) ttbr,
            options(nostack, preserves_flags),
        );
    }
    if tag.flush {
        // Only once the ASID is in force: an earlier regime under the same
        // ASID could otherwise refill it before the switch, and what fills it
        // now comes from this root.
        // SAFETY: the dispatcher's context in kernel mode, the root just
        // loaded under the ASID discarded.
        unsafe { flush_current_asid() };
    }
}

/// Host substitute: reprogramming `TTBR0_EL1` is meaningful only on the
/// bare-metal aarch64 target. Never linked into a kernel image and never
/// reached on the host (the QEMU verticals exercise the real switch).
///
/// # Safety
///
/// Carries the same contract as the bare-metal definition above (MMU
/// enabled; `root_phys` maps the running kernel context), so the two
/// `cfg` arms present one `unsafe` API. The host body is inert.
#[cfg(not(all(target_arch = "aarch64", target_os = "none")))]
pub unsafe fn activate_user_root(root_phys: u64, tag: AddressTag) {
    let _ = (root_phys, tag);
}

/// Translate `addr` as an EL1 stage-1 access under the **active** regime and
/// return the raw `PAR_EL1` result (`bit 0`: `0` = translated, `1` = fault).
///
/// `AT` only *translates*; it never accesses the memory and cannot fault, so
/// this is safe to run from a fault handler or an interrupt over an address
/// that may be unmapped, misaligned, or wild. `PAR_EL1` is saved and restored
/// so a translation the interrupted context had in flight is never clobbered.
///
/// The port's EL1 `AT` probe: the fatal report's faulting-address probe is
/// this, and the watchdog's stack-link check ([`el1_may_read`]) starts with
/// it, so their fail-closed reading of `PAR_EL1` cannot diverge.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
#[must_use]
pub fn translate_el1(addr: u64, write: bool) -> u64 {
    let par: u64;
    // SAFETY: as the rustdoc above states — `AT S1E1R`/`AT S1E1W` record the
    // outcome of translating `addr` in `PAR_EL1` without accessing it, so
    // neither can fault. The `ISB` is the context-synchronization event the
    // architecture requires before the result is guaranteed visible to the
    // following `MRS`. `PAR_EL1` is snapshotted first and restored last.
    unsafe {
        if write {
            core::arch::asm!(
                "mrs {saved}, par_el1",
                "at s1e1w, {addr}",
                "isb",
                "mrs {par}, par_el1",
                "msr par_el1, {saved}",
                saved = out(reg) _,
                addr = in(reg) addr,
                par = out(reg) par,
                options(nostack, preserves_flags),
            );
        } else {
            core::arch::asm!(
                "mrs {saved}, par_el1",
                "at s1e1r, {addr}",
                "isb",
                "mrs {par}, par_el1",
                "msr par_el1, {saved}",
                saved = out(reg) _,
                addr = in(reg) addr,
                par = out(reg) par,
                options(nostack, preserves_flags),
            );
        }
    }
    par
}

/// The physical address the kernel's `virt` translates to under the active
/// regimes, or `None` where nothing maps it for an EL1 read.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
#[must_use]
pub fn kernel_phys(virt: u64) -> Option<u64> {
    let par = translate_el1(virt, false);
    (!par_faulted(par)).then(|| par_phys(par, virt))
}

/// Read the raw translation descriptors the active root holds for `addr`,
/// root-downward, into `out`, returning how many were read.
///
/// For a fatal report only. A translation fault says an entry is absent; it
/// does not say whether the hierarchy that holds it is *intact*. A
/// well-formed table descriptor pointing at a plausible table, with one
/// invalid leaf, is a mapping that was never made or was removed; a
/// descriptor that is arbitrary data means the table page itself has been
/// clobbered or reused, which is a different and worse defect.
///
/// A table is a *physical* address, so it is read through the direct
/// physical map. A table the map does not cover ends the walk. Every table page is additionally proved translatable with
/// the non-faulting probe before it is read, so a clobbered or unmapped
/// table ends the walk instead of faulting inside the fault handler. The
/// walk also stops at an invalid entry or a leaf, neither of which names a
/// further table.
///
/// `root` is the active *low* root; a kernel-regime `addr` is translated by
/// the global kernel root instead, because the two regimes index their
/// roots with the same nine bits and walking the wrong one would report an
/// unrelated mapping's descriptors as this address's.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
pub fn table_path(root: u64, addr: u64, out: &mut [u64]) -> usize {
    let mut table = if Regime::User.holds(addr) {
        root & ADDR_MASK
    } else {
        kernel_root_phys()
    };
    let mut read = 0usize;
    for level in 1..=3usize {
        if read >= out.len() {
            break;
        }
        if !physmap_covers(table, PAGE_SIZE as u64) {
            break;
        }
        let slot = physmap_virt(table) + (table_index(addr, level) as u64) * 8;
        if par_faulted(translate_el1(slot, false)) {
            break;
        }
        // SAFETY: the map covers `table` and the probe proved `slot`
        // translates for an EL1 read, and `table_index` masks to the
        // 512-entry table, so `slot` addresses one in-range word of a live
        // table page. Volatile so the read is not reordered or elided on
        // the report path.
        let entry = unsafe { core::ptr::read_volatile(slot as *const u64) };
        out[read] = entry;
        read += 1;
        if (entry & attrs::VALID) == 0 || is_block(entry) {
            break;
        }
        table = entry & ADDR_MASK;
    }
    read
}

/// The physical address a successful `PAR_EL1` translation result names: the
/// `PA` field combined with the offset within the page. Pure, so the decode
/// is host-tested independently of the probe that produces it.
#[must_use]
pub const fn par_phys(par: u64, addr: u64) -> u64 {
    (par & 0x0000_FFFF_FFFF_F000) | (addr & 0xFFF)
}

/// `true` when a `PAR_EL1` translation result reports a fault (its `F` bit).
/// Pure, and the one reading of that bit the port shares.
#[must_use]
pub const fn par_faulted(par: u64) -> bool {
    (par & 1) != 0
}

/// Whether an EL1 load from `addr` would succeed: it translates for EL1 and,
/// while PAN is in force, EL0 cannot reach it. `AT S1E1R` ignores PAN, so a
/// page EL0 may access would otherwise answer readable and then fault.
///
/// Translates only, like [`translate_el1`], so it cannot fault.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
#[must_use]
pub fn el1_may_read(addr: u64) -> bool {
    if par_faulted(translate_el1(addr, false)) {
        return false;
    }
    if !crate::protection::pan_in_force() {
        return true;
    }
    let par: u64;
    // SAFETY: as `translate_el1`'s: `AT S1E0R` records the outcome of
    // translating `addr` for EL0 in `PAR_EL1` without accessing it, and the
    // interrupted context's `PAR_EL1` is restored.
    unsafe {
        core::arch::asm!(
            "mrs {saved}, par_el1",
            "at s1e0r, {addr}",
            "isb",
            "mrs {par}, par_el1",
            "msr par_el1, {saved}",
            saved = out(reg) _,
            addr = in(reg) addr,
            par = out(reg) par,
            options(nostack, preserves_flags),
        );
    }
    par_faulted(par)
}

/// The child table entry `idx` of `parent` names, linking a fresh one where
/// the entry is invalid, or [`None`] for a block there (never shattered) or
/// an exhausted source.
fn ensure_child<'a>(
    parent: &'a [AtomicU64; ENTRIES_PER_TABLE],
    idx: usize,
    frames: &dyn PageTableFrames,
) -> Option<&'a [AtomicU64; ENTRIES_PER_TABLE]> {
    let entry = parent[idx].load(WALK_ORDERING);
    if (entry & attrs::VALID) != 0 {
        if is_block(entry) {
            return None;
        }
        let table = frames.table_at(phys_from_descriptor(entry))?;
        // SAFETY: every valid table descriptor was linked below with an
        // output address drawn from `frames`, so the source's view of it is
        // a table of this hierarchy, allocated as long as `parent` is.
        Some(unsafe { descriptors(table) })
    } else {
        let TableFrame { phys, entries } = frames.alloc_table()?;
        parent[idx].store(table_descriptor(phys), LINK_ORDERING);
        // SAFETY: as above; `entries` is not used again.
        Some(unsafe { descriptors(entries) })
    }
}

/// Whether the leaf `desc` lets some level both write and execute its page:
/// EL1 writes every writable page, EL0 those it may reach.
const fn writable_and_executable(desc: u64) -> bool {
    let ap = desc & (0b11 << 6);
    let writable = ap == attrs::AP_RW_EL1 || ap == attrs::AP_RW_EL0;
    let el0_reaches = ap == attrs::AP_RW_EL0 || ap == attrs::AP_RO_EL0;
    let executable = desc & attrs::PXN == 0 || (el0_reaches && desc & attrs::UXN == 0);
    writable && executable
}

/// Write `SCTLR_EL1` whole: translation and caches on, with `SPAN` as the
/// protections the calling CPU is held to say ([`crate::protection`]). The
/// trampolines turn translation on before the protections are settled.
///
/// # Safety
///
/// Translation is on, and the calling CPU holds what it settled.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
pub(crate) unsafe fn hold_translation_controls() {
    // SAFETY: the value differs from the live one in `SPAN` at most, which
    // only decides whether an exception taken to EL1 sets PAN.
    unsafe {
        core::arch::asm!(
            "msr sctlr_el1, {sctlr}",
            "isb",
            sctlr = in(reg) crate::protection::sctlr_mmu_on(crate::protection::pan_in_force()),
            options(nostack, preserves_flags),
        );
    }
}

/// Decode a stage-1 leaf (block or page) descriptor's attributes back
/// into the neutral [`PageFlags`] (the inverse of
/// [`AddressSpace::leaf_attrs_for`]). A valid leaf is always readable;
/// the AP field decides writability and EL0 reachability, the
/// execute-never bit for the leaf's privilege level decides
/// executability, and the `MAIR` attribute index decides Device.
fn page_flags_from_leaf(desc: u64) -> PageFlags {
    let mut out = PageFlags::READ;
    let ap = desc & (0b11 << 6);
    let user = ap == attrs::AP_RW_EL0 || ap == attrs::AP_RO_EL0;
    if ap == attrs::AP_RW_EL1 || ap == attrs::AP_RW_EL0 {
        out = out | PageFlags::WRITE;
    }
    if user {
        out = out | PageFlags::USER;
        if desc & attrs::UXN == 0 {
            out = out | PageFlags::EXEC;
        }
    } else if desc & attrs::PXN == 0 {
        out = out | PageFlags::EXEC;
    }
    // The `MAIR` attribute index (bits [4:2]) selects the memory type:
    // indices 1 and 3 = Device, index 2 = Normal Non-Cacheable (a coherent
    // DMA buffer), index 0 = cacheable Normal (no attribute bit).
    let attr_idx = desc & (0b111 << 2);
    if attr_idx == attrs::ATTR_IDX_DEVICE || attr_idx == attrs::ATTR_IDX_DEVICE_NONPOSTED {
        out = out | PageFlags::DEVICE;
    } else if attr_idx == attrs::ATTR_IDX_NORMAL_NC {
        if desc & attrs::SW_WRITE_COMBINE != 0 {
            out = out | PageFlags::WRITE_COMBINE;
        } else {
            out = out | PageFlags::DMA_COHERENT;
        }
    }
    if desc & attrs::SW_DMA != 0 {
        out = out | PageFlags::DMA;
    }
    out
}

/// 4 KiB-aligned physical address `vaddr` resolves to under a leaf whose
/// region starts at `leaf_base` and spans `1 << region_shift` bytes
/// (30 = L1 block, 21 = L2 block, 12 = L3 page). The page offset is
/// dropped so the result is page-aligned (the HAL `translate` contract
/// reports the 4 KiB page base).
fn resolved_page(leaf_base: u64, vaddr: u64, region_shift: usize) -> u64 {
    let region_mask = (1u64 << region_shift) - 1;
    (leaf_base + (vaddr & region_mask)) & !((PAGE_SIZE as u64) - 1)
}

#[cfg(test)]
#[path = "paging_tests.rs"]
mod tests;
