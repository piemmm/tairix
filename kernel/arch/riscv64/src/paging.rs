//! Sv39 page-table primitives for the riscv64 port.
//!
//! This module is the riscv64 analogue of `kernel/arch/x86_64::paging`.
//! It implements the Arch HAL page-table surface
//! ([`tairix_arch_api::mmu::AddressSpace`] +
//! [`tairix_arch_api::tlb::TlbShootdown`]) `kernel/mem` drives, and it
//! supplies the inherent [`AddressSpace::new_identity_gigapages`] /
//! `AddressSpace::switch` the production boot pipeline
//! (`tairix_kernel::riscv64::boot`, `plans/PI.md` RV-P2) uses to enable
//! the Sv39 identity MMU. The same primitives back the memory-isolation
//! QEMU vertical's two Sv39 hierarchies that disagree about a single
//! virtual address, so the MMU faults a process that reaches for
//! another's frame ("memory isolation is enforced by
//! hardware").
//!
//! # Sv39
//!
//! Sv39 (RISC-V privileged spec §4.4.1) is a three-level, 39-bit
//! virtual-address scheme: VA = `VPN[2] (9) | VPN[1] (9) | VPN[0] (9) |
//! offset (12)`. Each page-table entry packs the next-level PPN
//! (physical page number) into bits `[53:10]` along with the
//! permission/valid bits in `[9:0]`. The `satp` CSR selects Sv39 with
//! mode `8` in its top four bits and carries the root table's PPN.
//!
//! The bit-twiddling that encodes a PPN into a PTE, extracts the
//! per-level VPN index from a VA, and assembles the `satp` value is
//! pure arithmetic and is host-unit-tested below; the `&mut`-recovering
//! table walk and the `satp` write are gated to the freestanding
//! riscv64 target.

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicU64, AtomicU8, AtomicUsize, Ordering};

use tairix_arch_api::frames::{
    active_frames, descriptors, pool_slot_of, reclaim_hierarchy, Leaf, PageTableFrames, TableFrame,
    LINK_ORDERING, WALK_ORDERING,
};
use tairix_arch_api::gigapages;
use tairix_arch_api::mmu::{
    AccessTracking, AddressSpace as MmuAddressSpace, KernelWindow, MapError, PageFlags,
};
use tairix_arch_api::tags::{AddressTag, AddressTags, ParkRoot};
use tairix_arch_api::tlb::TlbShootdown;

/// Size of a single page (and of a page-table page): the one system granule.
pub use tairix_abi::PAGE_SIZE;

/// Number of 64-bit entries in an Sv39 page-table page.
pub const ENTRIES_PER_TABLE: usize = 512;

/// Number of paging levels in Sv39.
pub const SV39_LEVELS: usize = 3;

/// `satp` MODE field value selecting Sv39 (privileged spec table 4.3).
pub const SATP_MODE_SV39: u64 = 8;

/// Bit position of the `satp` MODE field on RV64.
pub const SATP_MODE_SHIFT: u64 = 60;

/// Bit position and width of the `satp` ASID field on RV64.
const SATP_ASID_SHIFT: u64 = 44;
const SATP_ASID_BITS: u32 = 16;

/// A physical page number's 44 bits, as `satp` and a page-table entry both
/// hold it.
pub(crate) const PPN_MASK: u64 = (1 << SATP_ASID_SHIFT) - 1;

/// Page-table entry permission/valid bits (privileged spec §4.4.1).
pub mod flags {
    /// Entry is valid.
    pub const VALID: u64 = 1 << 0;
    /// Readable.
    pub const READ: u64 = 1 << 1;
    /// Writable.
    pub const WRITE: u64 = 1 << 2;
    /// Executable.
    pub const EXEC: u64 = 1 << 3;
    /// Accessible from user mode.
    pub const USER: u64 = 1 << 4;
    /// Accessed (set this eagerly so platforms without HW A/D updates
    /// do not fault on first touch).
    pub const ACCESSED: u64 = 1 << 6;
    /// Dirty (set eagerly alongside [`WRITE`], same rationale).
    pub const DIRTY: u64 = 1 << 7;
    /// One of the two bits a leaf leaves to software (RSW), marking a DMA
    /// buffer ([`PageFlags::DMA`](tairix_arch_api::mmu::PageFlags::DMA)).
    pub const SW_DMA: u64 = 1 << 8;
}

/// `true` iff a PTE is a *leaf* — valid and carrying at least one of
/// R/W/X. A valid entry with R=W=X=0 is a pointer to the next level.
#[must_use]
pub const fn pte_is_leaf(pte: u64) -> bool {
    (pte & flags::VALID) != 0 && (pte & (flags::READ | flags::WRITE | flags::EXEC)) != 0
}

/// Encode a physical address into the PPN field of a PTE.
///
/// Sv39 stores `paddr >> 12` in PTE bits `[53:10]`. The low 12 bits of
/// `paddr` (the page offset) are dropped — callers pass page-aligned
/// addresses.
#[must_use]
pub const fn pte_from_phys(paddr: u64, flags: u64) -> u64 {
    ((paddr >> 12) << 10) | flags
}

/// Recover the physical address a PTE points at (its PPN shifted back
/// into place). Inverse of [`pte_from_phys`] modulo the flag bits.
#[must_use]
pub const fn phys_from_pte(pte: u64) -> u64 {
    ((pte >> 10) & PPN_MASK) << 12
}

/// Extract the 9-bit VPN index for paging `level` (0 = leaf, 2 = root)
/// from a virtual address.
#[must_use]
pub const fn vpn_index(vaddr: u64, level: usize) -> usize {
    ((vaddr >> level_shift(level)) & 0x1FF) as usize
}

/// The log2 of the bytes one entry of a `level` table maps.
const fn level_shift(level: usize) -> usize {
    12 + 9 * level
}

/// Assemble the `satp` value selecting Sv39 with `root_phys` as the
/// root table under ASID `asid`.
#[must_use]
pub const fn satp_sv39(root_phys: u64, asid: u16) -> u64 {
    (SATP_MODE_SV39 << SATP_MODE_SHIFT) | ((asid as u64) << SATP_ASID_SHIFT) | (root_phys >> 12)
}

/// How many ASID bits a hart implements, from the `satp` it read back after
/// one wrote every ASID bit: the field is WARL, and keeps only those.
#[must_use]
pub fn asid_bits(readback: u64) -> u8 {
    let field = (readback >> SATP_ASID_SHIFT) & ((1 << SATP_ASID_BITS) - 1);
    u8::try_from(field.count_ones()).unwrap_or(0)
}

/// The fewest ASID bits any hart implements, each noting its own when it
/// first turns translation on. Every hart tags by the narrowest, so none is
/// handed an ASID its `satp` would truncate onto another.
struct AsidWidth(AtomicU8);

impl AsidWidth {
    /// Above every width a hart can report, so the first probe's minimum is
    /// its own: no hart tags until one ran.
    const UNPROBED: u8 = u8::MAX;

    const fn new() -> Self {
        Self(AtomicU8::new(Self::UNPROBED))
    }

    /// Fold in the `satp` a hart read back after writing every ASID bit.
    #[cfg(any(test, all(target_arch = "riscv64", target_os = "none")))]
    fn note(&self, readback: u64) {
        self.0.fetch_min(asid_bits(readback), Ordering::AcqRel);
    }

    /// A hart that had no root to probe with shows no width, and no width it
    /// never showed can be trusted: no hart tags.
    #[cfg(any(test, all(target_arch = "riscv64", target_os = "none")))]
    fn note_unprobed(&self) {
        self.0.fetch_min(0, Ordering::AcqRel);
    }

    /// Every ASID but `0`, the park root's, that the narrowest hart keeps;
    /// none before any hart probed, or when one keeps no ASID bit.
    fn user_tags(&self) -> u16 {
        match self.0.load(Ordering::Acquire) {
            Self::UNPROBED | 0 => 0,
            bits => u16::try_from((1u32 << bits.min(16)) - 1).unwrap_or(u16::MAX),
        }
    }
}

static ASID_WIDTH: AsidWidth = AsidWidth::new();

/// Load `root_phys` under ASID `0` on the calling hart, noting first how many
/// ASID bits the hart keeps: `satp` is written with every ASID bit set and
/// read back, the field being WARL, then written under ASID `0`, and every
/// entry is then discarded so neither the probe nor any earlier regime
/// survives. Each hart does this as it first turns translation on, so no
/// hart is handed an ASID its own `satp` would truncate.
///
/// # Safety
///
/// `root_phys` must map the running `pc` and `sp` as the regime in force
/// does.
#[cfg(all(target_arch = "riscv64", target_os = "none"))]
pub(crate) unsafe fn load_probing_asids(root_phys: u64) {
    let satp = satp_sv39(root_phys, 0);
    let probe = satp_sv39(root_phys, u16::MAX);
    let readback: u64;
    // SAFETY: both writes load the same root, which the caller asserts maps
    // `pc` and `sp`; `sfence.vma x0, x0` then discards every entry.
    unsafe {
        core::arch::asm!(
            "csrw satp, {probe}",
            "csrr {readback}, satp",
            "csrw satp, {satp}",
            "sfence.vma",
            probe = in(reg) probe,
            readback = out(reg) readback,
            satp = in(reg) satp,
            options(nostack, preserves_flags),
        );
    }
    ASID_WIDTH.note(readback);
}

/// One page-table page: 512 × u64, naturally aligned.
#[repr(C, align(4096))]
struct Table([u64; ENTRIES_PER_TABLE]);

impl Table {
    const fn new() -> Self {
        Self([0; ENTRIES_PER_TABLE])
    }
}

/// Maximum number of page-table pages the memory-isolation test needs:
/// two [`AddressSpace`]s, each a 3-level walk for the gigapage identity
/// map plus one extra 4 KiB mapping, with spares.
const POOL_SIZE: usize = 16;

/// A statically-allocated pool of zero-initialised page-table pages.
///
/// Allocation is monotonic — frames are never freed — which matches the
/// set-up → run → exit lifecycle of the isolation test. A real
/// allocator lives in `kernel/mem` and is wired in by a later stage.
pub struct PageTablePool {
    storage: [UnsafeCell<Table>; POOL_SIZE],
    used: AtomicUsize,
}

// SAFETY: the pool exposes `&self` allocation but every allocated frame
// is handed out exactly once (monotonic `AtomicUsize`), so distinct
// allocations never alias.
unsafe impl Sync for PageTablePool {}

impl Default for PageTablePool {
    fn default() -> Self {
        Self::new()
    }
}

impl PageTablePool {
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
        let storage = [ZERO; POOL_SIZE];
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
        if idx >= POOL_SIZE {
            self.used.store(POOL_SIZE, Ordering::SeqCst);
            return None;
        }
        // SAFETY: monotonic allocator + atomic fetch_add means this index
        // is owned by *this* call uniquely; the returned `&'static mut`
        // never aliases another.
        let cell = &self.storage[idx];
        let table_ref: &'static mut Table = unsafe { &mut *cell.get() };
        Some(&mut table_ref.0)
    }
}

impl PageTableFrames for PageTablePool {
    fn alloc_table(&self) -> Option<TableFrame> {
        let entries = self.alloc()?;
        // Sv39 runs identity-mapped for the kernel's own memory, so the
        // table's virtual address is its physical address (`plans/WIRING.md` W5b-3 — the bootstrap frame source).
        let phys = phys_of(entries.as_ptr() as u64);
        Some(TableFrame { phys, entries })
    }

    fn table_at(&self, phys: u64) -> Option<*mut [u64; ENTRIES_PER_TABLE]> {
        // Recovered from the slot the pool handed `phys` out of, so the
        // pointer keeps its storage's provenance; a `phys` from anywhere
        // else names no slot and the walk asking for it fails closed.
        let index = pool_slot_of(phys_of(self.storage.as_ptr() as u64), POOL_SIZE, phys)?;
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

/// An Sv39 address space built on a freshly-allocated root table.
///
/// The constructor identity-maps the low `gigabytes` GiB of physical
/// memory with 1 GiB leaf entries (R|W|X) so the kernel's own
/// code/stack/data and the `virt` board's MMIO remain reachable
/// whichever [`AddressSpace`] is active. [`Self::map_4k`] adds the
/// finer-grained mappings the memory-isolation test diverges on.
pub struct AddressSpace {
    root_phys: u64,
    /// The frame source the page-table walk allocates intermediate
    /// tables from, retained so the [`tairix_arch_api::mmu::AddressSpace`]
    /// HAL impl can install mappings without the caller re-supplying it.
    /// The static [`PageTablePool`] is the boot/bootstrap source; a real
    /// per-process space is built over the `kernel/mem` frame-allocator
    /// source (`plans/WIRING.md` W5b-3).
    frames: &'static dyn PageTableFrames,
}

impl AddressSpace {
    /// Build a new address space identity-mapping `[0, gigabytes GiB)`
    /// with 1 GiB leaf pages.
    ///
    /// `gigabytes` must be `1..=`[`IDENTITY_GIGAPAGES`] — the canonical
    /// lower half, the only range where a root slot's virtual address *is*
    /// the physical address it maps. A wider extent is refused rather than
    /// installing an upper-half leaf that is identity in neither direction.
    /// On the QEMU `virt` board four gigapages cover the MMIO window and the
    /// 2 GiB RAM base at `0x8000_0000`.
    ///
    /// # Errors
    ///
    /// Returns `None` if `gigabytes` is out of range or the page-table
    /// pool is exhausted.
    pub fn new_identity_gigapages(
        frames: &'static dyn PageTableFrames,
        gigabytes: usize,
    ) -> Option<Self> {
        if gigabytes == 0 || gigabytes > IDENTITY_GIGAPAGES {
            return None;
        }
        let TableFrame {
            phys: root_phys,
            entries: root,
        } = frames.alloc_table()?;
        let leaf = flags::VALID
            | flags::READ
            | flags::WRITE
            | flags::EXEC
            | flags::ACCESSED
            | flags::DIRTY;
        for (i, slot) in root.iter_mut().take(gigabytes).enumerate() {
            let paddr = (i as u64) << 30;
            *slot = pte_from_phys(paddr, leaf);
        }
        // Every root reaches the kernel remap window and the direct
        // physical map, so a kernel address in either resolves whichever
        // root is active. Done here rather than at each call site so no
        // future space can be built without them.
        install_kernel_window_slots(root);
        install_physmap_slots(root);
        Some(Self { root_phys, frames })
    }

    /// Build a root that maps **only** the kernel remap window — the handle
    /// the kernel-heap remap layer edits the window's shared sub-hierarchy
    /// through.
    ///
    /// The root is never activated: because the window's root entries point
    /// at tables every other root shares, a leaf installed through this
    /// space is immediately visible under all of them. Keeping it separate
    /// means the remap layer draws its intermediate tables from the frame
    /// allocator rather than from the fixed boot pool, and installs no leaf
    /// outside the window: the only other slots the root carries are the
    /// direct physical map's gigapage leaves, which a walk refuses to
    /// shatter.
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
        install_physmap_slots(root);
        Some(Self { root_phys, frames })
    }

    /// The Sv39 root table's entries, recovered through the frame source
    /// that drew it, or [`None`] when the source cannot reach it (fail
    /// closed).
    ///
    /// The space retains only `root_phys`, never a reference: another hart
    /// walks the root while it is active ([`retry_granted_fault`]).
    fn root_table(&self) -> Option<&[AtomicU64; ENTRIES_PER_TABLE]> {
        // SAFETY: `root_phys` names this space's root table, drawn from
        // `self.frames`, and it lives as long as the space.
        Some(unsafe { descriptors(self.frames.table_at(self.root_phys)?) })
    }

    /// The valid leaf covering `vaddr` in this hierarchy, or [`None`].
    fn leaf(&self, vaddr: u64) -> Option<Leaf<'_>> {
        // SAFETY: as `root_table`.
        unsafe { leaf_in(self.frames, self.root_phys, vaddr) }
    }

    /// The 4 KiB leaf covering `vaddr`, or [`None`] where none does: a
    /// gigapage or megapage on the way is never shattered by a per-page
    /// operation.
    fn page_leaf(&self, vaddr: u64) -> Option<Leaf<'_>> {
        self.leaf(vaddr).filter(|leaf| leaf.shift == level_shift(0))
    }

    /// Map `paddr` at `vaddr` with 4 KiB granularity.
    ///
    /// `vaddr` and `paddr` must be page-aligned. Returns `None` on
    /// page-table-pool exhaustion, if the walk meets an existing leaf
    /// (gigapage / megapage) it would have to shatter — the isolation
    /// test maps outside the identity-mapped gigapages so that path is
    /// not exercised — or for a user leaf in a kernel root slot.
    pub fn map_4k(
        &mut self,
        frames: &'static dyn PageTableFrames,
        vaddr: u64,
        paddr: u64,
        flags: u64,
    ) -> Option<()> {
        if (vaddr & (PAGE_SIZE as u64 - 1)) != 0 || (paddr & (PAGE_SIZE as u64 - 1)) != 0 {
            return None;
        }
        let i2 = vpn_index(vaddr, 2);

        // A user leaf in a kernel slot would hand U-mode the direct
        // physical map or the kernel heap's remap window, both shared by
        // every root. The window allocators already bound every user
        // address below the half, so this is the fail-closed floor under
        // them rather than the only check.
        if flags & flags::USER != 0 && is_kernel_slot(i2) {
            return None;
        }

        let l1 = ensure_child(self.root_table()?, i2, frames)?;
        let l0 = ensure_child(l1, vpn_index(vaddr, 1), frames)?;
        let slot = &l0[vpn_index(vaddr, 0)];
        if pte_is_leaf(slot.load(Ordering::Relaxed)) {
            return None;
        }
        slot.store(
            pte_from_phys(paddr, flags | flags::VALID | flags::ACCESSED | flags::DIRTY),
            Ordering::Relaxed,
        );
        Some(())
    }

    /// Map the 1 GiB region at `paddr` to `vaddr` with a single root-level
    /// gigapage leaf.
    ///
    /// `vaddr` and `paddr` must be 1 GiB-aligned. This installs one leaf
    /// directly in the root table (no child tables), so it costs no pool
    /// frames — the cheap way to alias a whole gigabyte of physical memory at
    /// a high virtual address with different permissions (e.g. the `USER`
    /// bit) than the identity map carries. Returns `None` on a misaligned
    /// address or if the target root slot is already occupied (a leaf or a
    /// table pointer) — it refuses to overwrite an existing mapping rather
    /// than silently clobber it.
    ///
    /// **TEST-ONLY SCAFFOLDING.** This exists solely to let the (in-progress)
    /// crt0 QEMU round-trip vertical alias the kernel's RAM at a high `BIAS`
    /// with the `USER` bit so it can `sret` into U-mode. It is gated to test
    /// builds and the `test-harness` feature so it is **never** compiled into
    /// a production kernel image; remove the gate (and this note) only when a
    /// real U-mode loader in `kernel/mem` makes it a supported primitive.
    #[cfg(any(test, feature = "test-harness"))]
    pub fn map_gigapage(&mut self, vaddr: u64, paddr: u64, flags: u64) -> Option<()> {
        const GIB: u64 = 1 << 30;
        if (vaddr & (GIB - 1)) != 0 || (paddr & (GIB - 1)) != 0 {
            return None;
        }
        let i2 = vpn_index(vaddr, 2);
        // A kernel slot is not identity address space; aliasing into one
        // would clobber the direct physical map or the shared remap
        // hierarchy every root points at.
        let slot = &self.root_table()?[i2];
        if (slot.load(Ordering::Relaxed) & flags::VALID) != 0 || is_kernel_slot(i2) {
            return None;
        }
        slot.store(
            pte_from_phys(paddr, flags | flags::VALID | flags::ACCESSED | flags::DIRTY),
            Ordering::Relaxed,
        );
        Some(())
    }

    /// Switch the active page table to this address space (write `satp`
    /// and flush the TLB).
    ///
    /// # Safety
    ///
    /// The caller must guarantee that this address space also maps the
    /// currently-executing `pc` and the current stack — otherwise the
    /// hart faults on the next fetch/access. [`Self::new_identity_gigapages`]
    /// upholds that by identity-mapping the kernel's gigapages.
    #[cfg(all(target_arch = "riscv64", target_os = "none"))]
    pub unsafe fn switch(&self) {
        // The first fully-configured space activated on the metal is the
        // permanent boot space: publish its root, set-once, as the park
        // root teardown and the dispatcher's suspend path re-install so a
        // dead user root is never left active (see [`park_kernel_root`]).
        PARK_ROOT.publish(self.root_phys);
        // SAFETY: the caller asserts this space maps `pc` and `sp`.
        unsafe { load_probing_asids(self.root_phys) };
    }

    /// Physical address of the root table (the PPN that goes into
    /// `satp`). Exposed so tests can observe it.
    #[must_use]
    pub fn root_phys(&self) -> u64 {
        self.root_phys
    }
}

/// Translate the architecture-neutral [`PageFlags`] into the Sv39
/// permission bits (one neutral vocabulary, decoded
/// once at the HAL boundary). The `VALID`/`ACCESSED`/`DIRTY` bits are
/// added by [`AddressSpace::map_4k`]; riscv64 has no page-table Device
/// attribute (memory type is PMA-driven), so [`PageFlags::DEVICE`] only
/// affects the absent caching attribute and maps to the same R/W/X here.
fn sv39_flags(flags: PageFlags) -> u64 {
    let mut bits = 0;
    if flags.contains(PageFlags::READ) {
        bits |= flags::READ;
    }
    if flags.contains(PageFlags::WRITE) {
        bits |= flags::WRITE;
    }
    if flags.contains(PageFlags::EXEC) {
        bits |= flags::EXEC;
    }
    if flags.contains(PageFlags::USER) {
        bits |= flags::USER;
    }
    if flags.contains(PageFlags::DMA) {
        bits |= flags::SW_DMA;
    }
    bits
}

/// Decode an Sv39 leaf PTE's permission bits back into the neutral
/// [`PageFlags`] (the inverse of [`sv39_flags`]). riscv64 has no
/// page-table Device attribute, so [`PageFlags::DEVICE`] is not
/// recoverable from a leaf and is never reported.
fn page_flags_from_sv39(pte: u64) -> PageFlags {
    let mut out = PageFlags::empty();
    if pte & flags::READ != 0 {
        out = out | PageFlags::READ;
    }
    if pte & flags::WRITE != 0 {
        out = out | PageFlags::WRITE;
    }
    if pte & flags::EXEC != 0 {
        out = out | PageFlags::EXEC;
    }
    if pte & flags::USER != 0 {
        out = out | PageFlags::USER;
    }
    if pte & flags::SW_DMA != 0 {
        out = out | PageFlags::DMA;
    }
    out
}

/// 4 KiB-aligned physical address `vaddr` resolves to under a leaf whose
/// region starts at `leaf_base` and spans `1 << region_shift` bytes
/// (30 = gigapage, 21 = megapage, 12 = 4 KiB). The page offset is
/// dropped so the result is always page-aligned (the HAL `translate`
/// contract reports the 4 KiB page base).
fn resolved_page(leaf_base: u64, vaddr: u64, region_shift: usize) -> u64 {
    let region_mask = (1u64 << region_shift) - 1;
    (leaf_base + (vaddr & region_mask)) & !((PAGE_SIZE as u64) - 1)
}

impl MmuAddressSpace for AddressSpace {
    fn map_page(&mut self, vaddr: u64, paddr: u64, flags: PageFlags) -> Result<(), MapError> {
        if (vaddr & (PAGE_SIZE as u64 - 1)) != 0 || (paddr & (PAGE_SIZE as u64 - 1)) != 0 {
            return Err(MapError::Misaligned);
        }
        // A non-canonical address names no page: mapping it would install
        // its canonical alias's leaf.
        if flags.is_write_exec() || !is_canonical(vaddr) {
            return Err(MapError::InvalidFlags);
        }
        // Sv39 states no memory type, so neither can be honoured: memory a
        // device that does not snoop shares would be cached.
        if flags.contains(PageFlags::WRITE_COMBINE) || flags.contains(PageFlags::DMA_COHERENT) {
            return Err(MapError::Unsupported);
        }
        // Checked ahead of the leaf walk so the refusal names the reason the
        // address is unusable rather than whatever the kernel's own shared
        // windows happen to have mapped there.
        if flags.contains(PageFlags::USER) && is_kernel_slot(vpn_index(vaddr, 2)) {
            return Err(MapError::InvalidFlags);
        }
        if self.leaf(vaddr).is_some() {
            return Err(MapError::AlreadyMapped);
        }
        let frames = self.frames;
        // Alignment, prior mapping, and the kernel-slot floor are already
        // ruled out, so the only remaining failure from the walk is
        // frame-source exhaustion.
        self.map_4k(frames, vaddr, paddr, sv39_flags(flags))
            .ok_or(MapError::PoolExhausted)
    }

    fn translate(&self, vaddr: u64) -> Option<(u64, PageFlags)> {
        let leaf = self.leaf(vaddr)?;
        Some((
            resolved_page(phys_from_pte(leaf.seen), vaddr, leaf.shift),
            page_flags_from_sv39(leaf.seen),
        ))
    }

    fn unmap(&mut self, vaddr: u64) -> Result<u64, MapError> {
        if (vaddr & (PAGE_SIZE as u64 - 1)) != 0 {
            return Err(MapError::Misaligned);
        }
        let leaf = self.page_leaf(vaddr).ok_or(MapError::NotMapped)?;
        Ok(phys_from_pte(leaf.slot.swap(0, Ordering::Relaxed)))
    }

    fn root_phys(&self) -> u64 {
        self.root_phys
    }

    fn access_tracking(&self) -> AccessTracking {
        // The per-page referenced bit the cold-page scanner
        // (`kernel/mem::coldscan`) needs is the Accessed bit (A, PTE bit
        // 6). RISC-V leaves A/D update *implementation-defined*: a chip may
        // update them in hardware during the walk (the Svadu behaviour QEMU
        // `virt` exposes by default) or raise a page fault when A/D must be
        // set, leaving software to set them (the Svade behaviour).
        // `test_and_clear_accessed` clears A (and invalidates the leaf's
        // TLB entry); on a Svadu part the next access re-sets A in the walk,
        // on a Svade part it raises a load/store/instruction page fault the
        // trap path resolves through [`retry_granted_fault`] by
        // setting A back and retrying. Either way a probe reading A still
        // clear proves the page went untouched, so the facility is honestly
        // Supported. The software Svade path is proven on emulated `svade`
        // hardware by the `accessed-bit-qemu-riscv64` vertical.
        AccessTracking::Supported
    }

    fn test_and_clear_accessed(&mut self, vaddr: u64) -> Result<bool, MapError> {
        if (vaddr & (PAGE_SIZE as u64 - 1)) != 0 {
            return Err(MapError::Misaligned);
        }
        // The tier tracks only 4 KiB anonymous leaves, never a gigapage or
        // megapage.
        let leaf = self.page_leaf(vaddr).ok_or(MapError::NotMapped)?;
        let was_accessed = (leaf.seen & flags::ACCESSED) != 0;
        if was_accessed {
            // Clear the Accessed bit so the next access re-sets it (in the
            // walk on Svadu, or via the Svade page-fault path); a later
            // probe reading it still clear proves the page went untouched.
            // Invalidate the stale TLB entry so the cleared bit is observed
            // on the next translation rather than served from a cached PTE.
            // An atomic clear, so a Dirty bit the walk sets meanwhile stays.
            leaf.slot.fetch_and(!flags::ACCESSED, Ordering::Relaxed);
            invalidate_page_local(vaddr);
        }
        Ok(was_accessed)
    }

    unsafe fn activate(&self) {
        #[cfg(all(target_arch = "riscv64", target_os = "none"))]
        {
            // SAFETY: forwards to the gated `satp` activation primitive;
            // the caller upholds the `MmuAddressSpace::activate` contract
            // (this space maps the current `pc`/`sp`/MMIO), which is
            // exactly `AddressSpace::switch`'s contract.
            unsafe { self.switch() };
        }
        #[cfg(not(all(target_arch = "riscv64", target_os = "none")))]
        {
            unreachable!("Sv39 activation is only meaningful on the riscv64 bare-metal target")
        }
    }

    unsafe fn reclaim_table_frames(&mut self) {
        // Defence in depth: the dispatcher parks a hart off a user root at
        // every task suspend, so a dead space's root is never the active
        // translation here — but freeing the walked-from root of a live
        // regime would be catastrophic, so verify and re-park first. With
        // no park root published the frames are retired unreclaimed
        // rather than dismantling the active translation (fail closed).
        if active_root_phys() == self.root_phys && !park_kernel_root(true) {
            return;
        }
        // The kernel's own root slots — the direct physical map and the
        // remap window — describe state *every* root shares, not state this
        // hierarchy owns, and the walk below cannot tell the two apart: it
        // would free the live kernel heap's page tables. Drop them from this
        // root first; both are permanent and are reached through every other
        // root unchanged.
        let Some(root_table) = self.root_table() else {
            return;
        };
        for slot in root_table.iter().skip(PHYSMAP_FIRST_SLOT) {
            slot.store(0, Ordering::Relaxed);
        }
        let frames = self.frames;
        // An Sv39 hierarchy rooted at level 2: a valid PTE with R=W=X=0 is
        // a pointer to the next level; level-0 (depth 2) entries are page
        // leaves and are never descended into.
        let child_of = |entry: u64, depth: usize| -> Option<u64> {
            (depth < 2 && (entry & flags::VALID) != 0 && !pte_is_leaf(entry))
                .then(|| phys_from_pte(entry))
        };
        // SAFETY: every phys `child_of` yields was written by
        // `ensure_child` from a `TableFrame` of `self.frames`, so it names
        // a live table this hierarchy owns and the source can reach; the
        // guard above upholds the not-active contract the caller asserts,
        // and `self` is borrowed mutably so no other reference walks the
        // tables.
        unsafe {
            reclaim_hierarchy(self.root_phys, frames, &child_of);
        }
    }
}

impl TlbShootdown for AddressSpace {
    fn flush_page(&mut self, vaddr: u64) {
        invalidate_page_local(vaddr);
    }

    fn flush_range(&mut self, _start_vaddr: u64, page_count: usize) {
        if page_count != 0 {
            invalidate_all_local();
        }
    }

    fn publish_mappings(&mut self, _start_vaddr: u64, page_count: usize) {
        // Sv39 permits an implementation to cache invalid entries, so
        // making a leaf valid genuinely needs the fence — this port cannot
        // publish an installation with a bare barrier the way aarch64 can.
        // One whole-hart fence covers the range, but it reaches only *this*
        // hart: a space active on several (the kernel remap window, the
        // boot root) owes the others one too, which the port declares
        // through `CrossCpuTlbShootdown::publish_needs_remote` so the
        // consumer follows this with an SBI RFENCE. An address space has no
        // way to know which harts share it, so the reach cannot be decided
        // here.
        if page_count != 0 {
            invalidate_all_local();
        }
    }
}

/// Invalidate the *calling* hart's cached Sv39 translation for the 4 KiB
/// page containing `vaddr`.
///
/// This is the single instruction sequence shared by both the local
/// per-page flush ([`TlbShootdown::flush_page`]) and the local half of
/// the cross-CPU shootdown
/// ([`tairix_arch_api::CrossCpuTlbShootdown::shootdown_page`] on
/// [`crate::kernel_arch::RiscvArch`]) — one implementation, not two. Unlike aarch64 there is no broadcast variant: the
/// cross-CPU path reaches *other* harts through the SBI RFENCE firmware
/// call (`crate::sbi::remote_sfence_vma`).
pub(crate) fn invalidate_page_local(vaddr: u64) {
    invalidate_range_local(vaddr, 1);
}

/// Root-table slots the kernel remap window claims, at the very top of the
/// Sv39 range.
///
/// Sized from the port's VA layout rather than from a byte figure: Sv39's
/// root table holds [`ENTRIES_PER_TABLE`] gigapages, and the window takes
/// the top eighth of them (64 GiB). Address space is free until something
/// is backed into it, so the only cost of a generous window is one shared
/// intermediate table per slot; what the size bounds is the kernel heap,
/// and on any machine this port runs on installed RAM binds long before
/// 64 GiB of kernel heap does.
///
/// The direct physical map ([`PHYSMAP_SLOTS`]) is sized to stop below the
/// window, so the two never overlap.
pub const KERNEL_WINDOW_SLOTS: usize = ENTRIES_PER_TABLE / 8;

/// First root-table slot of the kernel remap window.
///
/// The window stops one gigapage short of the top of the address space: an
/// extent whose exclusive top is not representable is refused outright
/// (which keeps every consumer free of wrap arithmetic), and the last
/// gigapage of Sv39 is worth less than that simplicity.
const KERNEL_WINDOW_FIRST_SLOT: usize = ENTRIES_PER_TABLE - 1 - KERNEL_WINDOW_SLOTS;

/// Pages the kernel remap window spans.
const KERNEL_WINDOW_PAGES: usize = KERNEL_WINDOW_SLOTS * ENTRIES_PER_TABLE * ENTRIES_PER_TABLE;

/// Widest identity map an Sv39 root can honestly carry, in gigapages: the
/// whole canonical lower half, and no more.
///
/// Sv39 sign-extends from bit 38, so only a lower-half root slot names a
/// virtual address *equal* to the physical address it maps. A leaf in a
/// slot above that names an upper-half address, which is identity in
/// neither direction — so the extent stops exactly where the kernel's own
/// upper-half windows begin. RAM above it is reached through the direct
/// physical map, not by widening this.
pub const IDENTITY_GIGAPAGES: usize = PHYSMAP_FIRST_SLOT;

/// First root-table slot the direct physical map claims — the first slot of
/// Sv39's canonical upper half.
///
/// The port's user virtual region is exactly the lower half
/// (`USER_VA_TOP == 1 << 38`), so no user address can name this slot or any
/// above it. The map runs from here up to the kernel remap window: the
/// kernel half and the user half share no slot, which is what lets a
/// process root carry the map without carrying a mapping of RAM in the half
/// user code addresses.
pub const PHYSMAP_FIRST_SLOT: usize = ENTRIES_PER_TABLE / 2;

/// Root-table slots the direct physical map spans — everything from its
/// first slot up to the kernel remap window. Derived, so moving either
/// boundary cannot leave the two overlapping.
pub const PHYSMAP_SLOTS: usize = KERNEL_WINDOW_FIRST_SLOT - PHYSMAP_FIRST_SLOT;

/// Base virtual address of the direct physical map: physical `p` is
/// reachable at `PHYSMAP_VMA_BASE + p` under every root.
pub const PHYSMAP_VMA_BASE: u64 = upper_half_slot_base(PHYSMAP_FIRST_SLOT);

/// Widest direct physical map the claimed slots can express, in gigabytes.
///
/// A root-level Sv39 leaf *is* a 1 GiB page, so one slot is one gigabyte
/// and the map costs no page tables at all — unlike a four-level port,
/// which needs one intermediate table per span.
pub const MAX_PHYSMAP_GIB: usize = PHYSMAP_SLOTS;

/// The map must stay in the upper half (or its base would need no sign
/// extension and [`upper_half_slot_base`] would be the wrong spelling) and
/// start below the kernel remap window (or its slot count would be a
/// negative span). Pinned so moving either boundary fails the build rather
/// than producing an address nothing maps.
const _: () = assert!(
    PHYSMAP_FIRST_SLOT >= ENTRIES_PER_TABLE / 2 && PHYSMAP_FIRST_SLOT < KERNEL_WINDOW_FIRST_SLOT,
    "the direct physical map must sit in the upper half, below the kernel remap window"
);

/// The map's shared root-table entries — one 1 GiB leaf per covered
/// gigabyte, or `0` for a slot the published extent does not reach.
///
/// Every root this port builds installs them, so a root's whole share of
/// the map is its own root entries: a root-level leaf is already a
/// gigapage, so there is nothing beneath them to draw or share.
static PHYSMAP_ROOT: [AtomicU64; PHYSMAP_SLOTS] = [const { AtomicU64::new(0) }; PHYSMAP_SLOTS];

/// Gigabytes of physical memory the live direct map covers. Zero until the
/// boot path sizes it from the discovered memory map, so a consumer on a
/// build with no boot path reaches nothing and fails closed.
static PHYSMAP_GIGAPAGES: AtomicUsize = AtomicUsize::new(0);

/// Gigapages holding device registers the kernel drives, named by the boot
/// path before the map is published: each becomes a leaf of the map whether
/// or not RAM reaches it, so a register window above the identity window
/// every root carries is reached at [`physmap_virt`] in any root. Sv39
/// carries no memory type, so the platform's attributes decide how they are
/// reached, as the identity window's.
pub static KERNEL_DEVICES: gigapages::KernelDevices =
    gigapages::KernelDevices::new(MAX_PHYSMAP_GIB);

/// Gigabytes of physical memory the live direct physical map covers.
#[must_use]
pub fn physmap_gigapages() -> usize {
    PHYSMAP_GIGAPAGES.load(Ordering::Acquire)
}

/// Exclusive top of the live direct physical map, in bytes — the highest
/// physical address the kernel can reach by pointer.
#[must_use]
pub fn physmap_bytes() -> u64 {
    (physmap_gigapages() as u64) << 30
}

/// The direct-map virtual address of physical `phys`.
///
/// `const`, so a fixed address names its direct-map spelling without a
/// run-time load. The address resolves only for a `phys` below
/// [`physmap_bytes`]; a caller with a discovered address checks that first.
#[must_use]
pub const fn physmap_virt(phys: u64) -> u64 {
    PHYSMAP_VMA_BASE.wrapping_add(phys)
}

/// The map's root entry for the gigabyte at `gib`: a 1 GiB leaf, readable
/// and writable but never executable or user-accessible. The kernel
/// executes from its identity window, so nothing is ever fetched through
/// the map.
const fn physmap_leaf(gib: usize) -> u64 {
    pte_from_phys(
        (gib as u64) << 30,
        flags::VALID | flags::READ | flags::WRITE | flags::ACCESSED | flags::DIRTY,
    )
}

/// Record `gib` gigabytes as the direct map's extent and fill the shared
/// root entries every later root installs, with a leaf for each configured
/// device gigapage beside them, set-once.
///
/// Split out from [`install_boot_physmap`] because this half is pure
/// bookkeeping: it is what the host tests drive to observe that every root
/// constructor installs the published map, with no live `satp` to patch.
///
/// Returns `false`, having published nothing, for an extent of zero or one
/// wider than the claimed slots can express, or for a second call.
fn publish_physmap(gib: usize) -> bool {
    if gib == 0 || gib > MAX_PHYSMAP_GIB {
        return false;
    }
    if PHYSMAP_GIGAPAGES
        .compare_exchange(0, gib, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return false;
    }
    let mut devices = [0u64; gigapages::MASK_WORDS];
    for (gigabyte, slot) in PHYSMAP_ROOT.iter().enumerate() {
        let device = KERNEL_DEVICES.named(gigabyte);
        if device {
            devices[gigabyte / 64] |= 1 << (gigabyte % 64);
        }
        if gigabyte < gib || device {
            slot.store(physmap_leaf(gigabyte), Ordering::Release);
        }
    }
    KERNEL_DEVICES.publish(devices);
    true
}

/// Size the direct physical map to `[0, gib GiB)`, publish it as the map
/// every later root installs, and patch it into the live root so the
/// running hart reaches a frame by pointer immediately.
///
/// Called once from the boot path, on the boot hart after the MMU is on and
/// before anything reaches a frame by pointer. The live root is reached
/// through `frames` — the source that drew it — so this dereferences no
/// physical address of its own, and the map itself needs no page tables:
/// each covered gigabyte is one root-level leaf.
///
/// Returns `false`, having changed nothing, for a `gib` of zero or wider
/// than [`MAX_PHYSMAP_GIB`], for a second call, or when `frames` cannot
/// reach the live root; the caller then fails the boot rather than running
/// on RAM it cannot address.
#[must_use]
pub fn install_boot_physmap(frames: &dyn PageTableFrames, gib: usize) -> bool {
    // Reached before publishing, so a root the source cannot vouch for
    // leaves the map unpublished rather than claimed-but-absent.
    let Some(table) = frames.table_at(active_root_phys()) else {
        return false;
    };
    if !publish_physmap(gib) {
        return false;
    }
    // SAFETY: `active_root_phys` names this hart's live root table and
    // `frames` is the source that drew it, so its view is dereferenceable.
    // The boot root's constructing `AddressSpace` handle is dropped before
    // the boot path reaches here and no other hart is started yet, so this
    // `&mut` is unique; the only entries written are the map's own slots,
    // which the identity fill never reaches and no other writer touches.
    let root = unsafe { &mut *table };
    install_physmap_slots(root);
    publish_table_update();
    true
}

/// Copy the published direct-map leaves into `root`'s claimed slots.
///
/// Every root constructor calls this, so no space can be built without the
/// map. An invalid-to-valid leaf needs no invalidation, only the fence the
/// callers issue.
fn install_physmap_slots(root: &mut [u64; ENTRIES_PER_TABLE]) {
    for (offset, slot) in PHYSMAP_ROOT.iter().enumerate() {
        let entry = slot.load(Ordering::Acquire);
        if entry != 0 {
            root[PHYSMAP_FIRST_SLOT + offset] = entry;
        }
    }
}

/// `true` when root-table slot `index` is the kernel's — the direct
/// physical map, or the remap window above it.
///
/// The port's user region stops exactly at the first of them, so this is
/// also "not addressable by a user program".
const fn is_kernel_slot(index: usize) -> bool {
    index >= PHYSMAP_FIRST_SLOT
}

/// The window's shared root-table entries, one per claimed slot, or `0`
/// before [`reserve_kernel_window`] runs.
///
/// Every root this port builds installs these, so a leaf added under one of
/// the shared intermediate tables they point at resolves identically
/// whichever root is active — the property that lets kernel code reach a
/// remapped kernel address while a user task's root is loaded.
static KERNEL_WINDOW_ROOT: [AtomicU64; KERNEL_WINDOW_SLOTS] =
    [const { AtomicU64::new(0) }; KERNEL_WINDOW_SLOTS];

/// The window is placed in the upper half of the Sv39 range, so its base
/// must carry the sign extension below. Pinned so a change to
/// [`KERNEL_WINDOW_SLOTS`] cannot silently move the window into the lower
/// half and leave the spelling wrong.
const _: () = assert!(
    KERNEL_WINDOW_FIRST_SLOT >= ENTRIES_PER_TABLE / 2,
    "the kernel remap window must stay in the upper half of the Sv39 range"
);

/// Canonical virtual address of upper-half root slot `slot`.
///
/// Sv39 addresses are sign-extended from bit 38, so a root slot in the
/// upper half of the table names an *upper-half* virtual address: bits
/// 63:39 must all be set. Spelling a base as the bare `slot << 30` would be
/// non-canonical and fault on every access, so both upper-half windows —
/// the remap window and the direct physical map — derive theirs here.
const fn upper_half_slot_base(slot: usize) -> u64 {
    (u64::MAX << 39) | ((slot as u64) << 30)
}

/// Base virtual address of the kernel remap window.
#[must_use]
pub const fn kernel_window_base() -> u64 {
    upper_half_slot_base(KERNEL_WINDOW_FIRST_SLOT)
}

/// A window whose extent is not representable is refused at run time, which
/// would silently leave the kernel heap on its bootstrap region. Fail the
/// build instead.
const _: () = assert!(
    KernelWindow::is_representable(kernel_window_base(), KERNEL_WINDOW_PAGES),
    "the kernel remap window must be a representable extent"
);

/// Reserve the kernel remap window: draw one shared intermediate table per
/// claimed root slot, publish the entries every root installs, and patch
/// them into the live root so the running harts see the window immediately.
///
/// Called once, from the boot path, after the frame allocator exists (the
/// tables come from it, not from the fixed boot pool). A second call
/// returns the same window without drawing anything. Returns `None`,
/// having changed nothing, when the frame source cannot supply the shared
/// tables (fail closed — the kernel heap then stays on its bootstrap
/// region).
pub fn reserve_kernel_window(frames: &'static dyn PageTableFrames) -> Option<KernelWindow> {
    // SAFETY: the window's root slots are this port's own — the
    // compile-time assertion above pins its extent, and the publication
    // below installs one shared intermediate table per claimed slot in
    // every root this port builds, so the run is reserved and resolves
    // identically under each.
    let window = unsafe { KernelWindow::at_address(kernel_window_base(), KERNEL_WINDOW_PAGES) }?;
    if KERNEL_WINDOW_ROOT[0].load(Ordering::Acquire) != 0 {
        return Some(window);
    }
    for (offset, slot) in KERNEL_WINDOW_ROOT.iter().enumerate() {
        let Some(TableFrame { phys, entries: _ }) = frames.alloc_table() else {
            // Undo the partial reservation so a retry starts clean.
            for undone in KERNEL_WINDOW_ROOT.iter().take(offset) {
                frames.free_table(phys_from_pte(undone.swap(0, Ordering::AcqRel)));
            }
            return None;
        };
        // Non-leaf (table pointer): valid set, R/W/X clear.
        slot.store(pte_from_phys(phys, flags::VALID), Ordering::Release);
    }
    install_kernel_window(frames, active_root_phys());
    Some(window)
}

/// Install the published window entries into the root table at
/// `root_phys`, reaching it through `frames`. Does nothing when no window
/// is reserved, when `root_phys` is zero (what the host build and an
/// unpaged caller report), or when `frames` cannot reach that root.
fn install_kernel_window(frames: &'static dyn PageTableFrames, root_phys: u64) {
    if root_phys == 0 {
        return;
    }
    let Some(table) = frames.table_at(root_phys) else {
        return;
    };
    // SAFETY: `root_phys` names this port's own live root table and the
    // production source's direct map covers it, so its view is
    // dereferenceable; the only entries written are the window's own
    // slots, which no other writer touches.
    let root = unsafe { &mut *table };
    install_kernel_window_slots(root);
    publish_table_update();
}

/// Copy the published window entries into `root`'s top slots.
///
/// Every root constructor calls this, so a space built before *or* after
/// the reservation ends up with the window (the boot root is patched in
/// place by [`reserve_kernel_window`]). An invalid-to-valid non-leaf entry
/// needs no TLB maintenance, only the fence the callers issue.
fn install_kernel_window_slots(root: &mut [u64; ENTRIES_PER_TABLE]) {
    for offset in 0..KERNEL_WINDOW_SLOTS {
        let entry = KERNEL_WINDOW_ROOT[offset].load(Ordering::Acquire);
        if entry != 0 {
            root[KERNEL_WINDOW_FIRST_SLOT + offset] = entry;
        }
    }
}

/// Publish a page-table store to the MMU's walker before the next access
/// depends on it. An invalid-to-valid entry needs no invalidation, only
/// ordering; `sfence.vma` provides it. Host builds walk no hardware
/// tables, so this is a no-op there.
fn publish_table_update() {
    #[cfg(all(target_arch = "riscv64", target_os = "none"))]
    {
        // SAFETY: `sfence.vma` orders prior page-table stores against
        // subsequent implicit walks; it touches no memory and only
        // discards cached translation state.
        unsafe {
            core::arch::asm!("sfence.vma", options(nostack, preserves_flags));
        }
    }
}

/// Invalidate the calling hart's cached translations for `pages`
/// consecutive 4 KiB pages from `start_vaddr`.
///
/// The single-page flush is this with `pages == 1`, so there is one local
/// invalidation sequence on this port rather than two.
pub(crate) fn invalidate_range_local(start_vaddr: u64, pages: usize) {
    if fences_whole_space(pages) {
        invalidate_all_local();
        return;
    }
    #[cfg(all(target_arch = "riscv64", target_os = "none"))]
    {
        let mut vaddr = start_vaddr;
        for _ in 0..pages {
            // SAFETY: `sfence.vma {addr}, zero` is the documented Sv39
            // single-page TLB invalidation; it touches no memory and only
            // discards the cached translation for `vaddr`. No Rust
            // spelling exists.
            unsafe {
                core::arch::asm!(
                    "sfence.vma {addr}, zero",
                    addr = in(reg) vaddr,
                    options(nostack, preserves_flags),
                );
            }
            vaddr = vaddr.wrapping_add(PAGE_SIZE as u64);
        }
    }
    #[cfg(not(all(target_arch = "riscv64", target_os = "none")))]
    {
        // The host has no TLB to invalidate; a flush is vacuous.
        let _ = (start_vaddr, pages);
    }
}

/// Past this many pages one whole-space `sfence.vma` is taken to be cheaper
/// than one a page; any ceiling is correct, only the cost moves. It
/// also bounds a sparse unmap's span, which would otherwise cost a fence per
/// page of its gaps, on this hart and, through RFENCE, on the others.
const RANGE_FENCE_CEILING: usize = 64;

/// Whether fencing `pages` pages should fence the whole address space
/// instead, locally and remotely alike.
pub(crate) const fn fences_whole_space(pages: usize) -> bool {
    pages > RANGE_FENCE_CEILING
}

/// Invalidate every cached translation on the calling hart.
///
/// A multi-page transactional map uses one all-address `sfence.vma` instead
/// of one instruction per leaf. The scheduler never runs one task's address
/// space on two harts concurrently, so this has the same reach as the local
/// per-page operation while avoiding linear fence cost.
fn invalidate_all_local() {
    #[cfg(all(target_arch = "riscv64", target_os = "none"))]
    {
        // SAFETY: `sfence.vma zero, zero` invalidates all cached address
        // translations on the calling hart. It touches no memory and only
        // discards cached translation state.
        unsafe {
            core::arch::asm!("sfence.vma zero, zero", options(nostack, preserves_flags));
        }
    }
}

/// The boot space's root, which [`park_kernel_root`] parks on.
static PARK_ROOT: ParkRoot = ParkRoot::new();

/// Bring a secondary hart's translation in line with the boot hart's before
/// anything can hand it a tag: join the boot space's root, noting the ASID
/// bits the hart keeps, or, where the boot hart runs untranslated and has
/// published none, stay untranslated with no hart tagging.
///
/// # Safety
///
/// The hart runs on the kernel image and a stack inside it, which the boot
/// root identity-maps.
#[cfg(all(target_arch = "riscv64", target_os = "none"))]
pub(crate) unsafe fn join_boot_translation() {
    match PARK_ROOT.published() {
        // SAFETY: the caller's contract: the boot root maps the running `pc`
        // and `sp`.
        Some(root) => unsafe { load_probing_asids(root) },
        None => ASID_WIDTH.note_unprobed(),
    }
}

/// Park the calling hart's translation regime on the published boot
/// kernel root under ASID `0`, discarding that ASID's entries first when
/// `flush`, so no user space's root remains active after its task suspends
/// or exits. Returns `false`, changing nothing, when no park root has been
/// published yet (fail closed).
///
/// The dispatcher calls this after every switch-back from a user task;
/// address-space teardown and the machine takeover call it, flushing,
/// before dismantling a root that is somehow still active.
pub fn park_kernel_root(flush: bool) -> bool {
    // SAFETY: the published root is the boot space's, which identity-maps
    // the kernel window and the board MMIO for the image's lifetime —
    // exactly `activate_user_root`'s contract (inert on the host, where
    // the root is never published anyway).
    unsafe { PARK_ROOT.park(flush, activate_user_root) }
}

/// The port's address-space tags: the ASID field of `satp`, as wide as the
/// narrowest hart implements.
pub struct Riscv64Tags;

/// The riscv64 port's tags, which the kernel loads user roots and parks by.
pub static TAGS: Riscv64Tags = Riscv64Tags;

impl AddressTags for Riscv64Tags {
    fn user_tags(&self) -> u16 {
        ASID_WIDTH.user_tags()
    }

    fn root_register(&self, root_phys: u64, tag: u16) -> u64 {
        satp_sv39(root_phys, tag)
    }

    /// A user space's remote fences reach only the harts running it now.
    fn invalidation_reaches_every_tag(&self) -> bool {
        false
    }

    unsafe fn flush_current_tag(&self) {
        // SAFETY: the caller runs in S-mode on the dispatcher's context.
        unsafe { flush_current_asid() };
    }

    unsafe fn park(&self, flush: bool) -> bool {
        park_kernel_root(flush)
    }
}

/// Discard every entry of the ASID `satp` is loaded under, on this hart.
#[cfg(all(target_arch = "riscv64", target_os = "none"))]
unsafe fn flush_current_asid() {
    // SAFETY: `sfence.vma x0, rs2` with `rs2` holding the ASID read from
    // `satp` discards only that ASID's non-global entries on this hart. No
    // memory is touched and no Rust spelling exists for either.
    unsafe {
        core::arch::asm!(
            "csrr {asid}, satp",
            "srli {asid}, {asid}, {shift}",
            "and {asid}, {asid}, {mask}",
            "sfence.vma zero, {asid}",
            asid = out(reg) _,
            shift = const SATP_ASID_SHIFT,
            mask = in(reg) (1u64 << SATP_ASID_BITS) - 1,
            options(nostack, preserves_flags),
        );
    }
}

/// Host substitute: there are no TLBs to discard.
#[cfg(not(all(target_arch = "riscv64", target_os = "none")))]
unsafe fn flush_current_asid() {}

/// The physical root of the calling hart's active Sv39 translation
/// regime (`satp`'s PPN shifted back to an address), or `0` on the host,
/// which has no translation registers.
fn active_root_phys() -> u64 {
    #[cfg(all(target_arch = "riscv64", target_os = "none"))]
    {
        let satp: u64;
        // SAFETY: reading `satp` observes the active root without side
        // effects; no Rust spelling exists for the CSR.
        unsafe {
            core::arch::asm!("csrr {v}, satp", v = out(reg) satp, options(nostack, preserves_flags, nomem));
        }
        (satp & PPN_MASK) << 12
    }
    #[cfg(not(all(target_arch = "riscv64", target_os = "none")))]
    {
        0
    }
}

/// What a faulting access did, and so the leaf-permission bit it requires:
/// [`retry_granted_fault`] retries only on a leaf that genuinely grants it,
/// never masking a real permission fault, which raises the same cause.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum AccessKind {
    /// A load page fault (`scause` 13): the leaf must be readable.
    Load,
    /// A store/AMO page fault (`scause` 15): the leaf must be writable,
    /// and the Dirty bit is set alongside Accessed.
    Store,
    /// An instruction page fault (`scause` 12): the leaf must be
    /// executable.
    Instruction,
}

impl AccessKind {
    /// The leaf permission bit the access requires.
    const fn required_perm(self) -> u64 {
        match self {
            Self::Load => flags::READ,
            Self::Store => flags::WRITE,
            Self::Instruction => flags::EXEC,
        }
    }

    /// Whether resolving the fault also sets the Dirty bit (a store).
    const fn sets_dirty(self) -> bool {
        matches!(self, Self::Store)
    }
}

/// How a faulting access reached its leaf: what it did, and the mode it ran
/// in.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct FaultingAccess {
    /// What the access did.
    pub kind: AccessKind,
    /// It ran in U-mode.
    pub from_user: bool,
}

impl FaultingAccess {
    /// Whether `leaf`, a valid leaf, grants this access as the hart's own walk
    /// would: its permission bit, and its user bit against the mode. The
    /// kernel runs with `sstatus.SUM` clear, so no user page grants S-mode.
    const fn granted_by(self, leaf: u64) -> bool {
        (leaf & self.kind.required_perm()) != 0 && ((leaf & flags::USER) != 0) == self.from_user
    }
}

/// Retry an access that faulted on the leaf of the *active* Sv39 regime
/// (`satp`) covering `vaddr` when that leaf grants it, answering whether it
/// will be retried.
///
/// A granting leaf means the fault was not the access's own: either its
/// Accessed (or, for a store, Dirty) bit was clear on a part without hardware
/// A/D update — the bit the cold-page scanner clears through
/// [`AddressSpace::test_and_clear_accessed`] — or this hart used a
/// translation it cached before the leaf was made valid, which Sv39 lets an
/// implementation keep. The bits are set, this hart's entry for the page is
/// discarded, and the access retried, so a page another hart mapped reaches
/// this one with no fence to every hart that ever ran the space. A
/// non-canonical address, an absent leaf, or one that does not grant the
/// access is the access's own fault and touches nothing. It allocates nothing
/// and is sound in trap context.
///
/// It takes no lock, so `unmap` or the scanner may be changing the same leaf
/// on another hart; the bits are set only by an exchange from the leaf the
/// walk read ([`Leaf::set_while`]), never written back over a change.
#[must_use]
pub fn retry_granted_fault(vaddr: u64, access: FaultingAccess) -> bool {
    let root_phys = active_root_phys();
    if root_phys == 0 {
        return false;
    }
    let Some(frames) = active_frames() else {
        return false;
    };
    // SAFETY: `root_phys` is the root table of a space this port built, so
    // the published production source reaches it and every table below it,
    // and its space outlives the task this hart is running in it.
    unsafe { retry_granted_in_root(frames, root_phys, vaddr, access) }
}

/// Walk the Sv39 hierarchy rooted at `root_phys` for the leaf covering
/// `vaddr` and retry `access` there, as [`retry_granted_fault`] does for the
/// live root.
///
/// # Safety
///
/// As [`leaf_in`].
#[must_use]
unsafe fn retry_granted_in_root(
    frames: &dyn PageTableFrames,
    root_phys: u64,
    vaddr: u64,
    access: FaultingAccess,
) -> bool {
    // SAFETY: the function contract, forwarded.
    let Some(leaf) = (unsafe { leaf_in(frames, root_phys, vaddr) }) else {
        return false;
    };
    let bits = if access.kind.sets_dirty() {
        flags::ACCESSED | flags::DIRTY
    } else {
        flags::ACCESSED
    };
    if leaf.set_while(bits, |pte| access.granted_by(pte)).is_none() {
        return false;
    }
    invalidate_page_local(vaddr);
    true
}

/// The valid leaf covering `vaddr` in the Sv39 hierarchy rooted at
/// `root_phys`, or [`None`] for a non-canonical address or one no valid leaf
/// covers.
///
/// # Safety
///
/// `root_phys` must be a Sv39 root table drawn from `frames`, whose
/// descendant tables `frames` can therefore reach, and the hierarchy must
/// stay allocated for `'a`.
unsafe fn leaf_in<'a>(
    frames: &dyn PageTableFrames,
    root_phys: u64,
    vaddr: u64,
) -> Option<Leaf<'a>> {
    if !is_canonical(vaddr) {
        return None;
    }
    let mut table = frames.table_at(root_phys)?;
    for level in (0..SV39_LEVELS).rev() {
        // SAFETY: `table` is the root, or the table a valid non-leaf entry
        // above names, of a hierarchy the contract keeps allocated for `'a`.
        let slot = &unsafe { descriptors(table) }[vpn_index(vaddr, level)];
        let seen = slot.load(WALK_ORDERING);
        if (seen & flags::VALID) == 0 {
            return None;
        }
        if pte_is_leaf(seen) {
            return Some(Leaf {
                slot,
                seen,
                shift: level_shift(level),
            });
        }
        if level == 0 {
            return None;
        }
        table = frames.table_at(phys_from_pte(seen))?;
    }
    None
}

/// Whether Sv39 translates `vaddr` at all: it faults an address whose bits
/// 63..=38 are not all one value before it walks, which a walk by index
/// would otherwise resolve to its canonical alias.
const fn is_canonical(vaddr: u64) -> bool {
    let high = vaddr >> 38;
    high == 0 || high == u64::MAX >> 38
}

/// Reactivate `root_phys` as the active Sv39 translation root under `tag`'s
/// ASID (write `satp`) on a hart whose paging is already on, discarding that
/// ASID's entries when the tag says so.
///
/// This is the RV-X1 user-kthread `pre_resume` primitive (`plans/PI.md`
/// §X), the riscv64 sibling of the `aarch64`/`x86_64` `activate_user_root`:
/// immediately before the kernel `sret`s back into a user task's U-mode,
/// that task's own page-table root must be installed so its translations —
/// and only its — are in force, keeping sibling processes hardware-isolated.
/// Every other ASID's entries stay in the TLB unseen, for its space to find
/// again when it next runs here.
///
/// # Safety
///
/// Paging must already be enabled, and the root table at `root_phys` must
/// map the currently-executing kernel `pc`, `sp`, and the MMIO the code
/// touches identically to the outgoing root — every TAIRiX user space
/// identity-maps the low kernel window, so this holds for any task root,
/// but a `root_phys` that does not faults the hart on its next access.
#[cfg(all(target_arch = "riscv64", target_os = "none"))]
pub unsafe fn activate_user_root(root_phys: u64, tag: AddressTag) {
    let satp = satp_sv39(root_phys, tag.tag);
    // SAFETY: writing `satp` swaps the Sv39 translation root and its ASID;
    // the caller's contract guarantees the new root covers the running
    // kernel context. No memory is touched and no Rust spelling exists for
    // `satp`.
    unsafe {
        core::arch::asm!(
            "csrw satp, {satp}",
            satp = in(reg) satp,
            options(nostack, preserves_flags),
        );
    }
    if tag.flush {
        // SAFETY: as above; the root is loaded, so this discards the ASID's
        // entries before any of them is used for the space.
        unsafe { flush_current_asid() };
    }
}

/// Host substitute: reprogramming `satp` is meaningful only on the
/// bare-metal riscv64 target. Never linked into a kernel image and never
/// reached on the host (the QEMU verticals exercise the real switch).
///
/// # Safety
///
/// Carries the same contract as the bare-metal definition above (paging
/// enabled; `root_phys` maps the running kernel context), so the two `cfg`
/// arms present one `unsafe` API. The host body is inert.
#[cfg(not(all(target_arch = "riscv64", target_os = "none")))]
pub unsafe fn activate_user_root(root_phys: u64, tag: AddressTag) {
    let _ = (root_phys, tag);
}

/// The child table entry `idx` of `parent` names, linking a fresh one where
/// the entry is invalid, or [`None`] for a leaf there (never shattered) or
/// an exhausted source.
fn ensure_child<'a>(
    parent: &'a [AtomicU64; ENTRIES_PER_TABLE],
    idx: usize,
    frames: &dyn PageTableFrames,
) -> Option<&'a [AtomicU64; ENTRIES_PER_TABLE]> {
    let entry = parent[idx].load(WALK_ORDERING);
    if (entry & flags::VALID) != 0 {
        if pte_is_leaf(entry) {
            return None;
        }
        let table = frames.table_at(phys_from_pte(entry))?;
        // SAFETY: every valid non-leaf entry was linked below with a PPN
        // drawn from `frames`, so the source's view of it is a table of this
        // hierarchy, allocated as long as `parent` is.
        Some(unsafe { descriptors(table) })
    } else {
        let TableFrame { phys, entries } = frames.alloc_table()?;
        // A table pointer: valid, R/W/X clear.
        parent[idx].store(pte_from_phys(phys, flags::VALID), LINK_ORDERING);
        // SAFETY: as above; `entries` is not used again.
        Some(unsafe { descriptors(entries) })
    }
}

/// Physical address of the kernel-owned virtual address `virt`.
///
/// Identity-mapped: virtual == physical for everything the kernel owns,
/// because the boot trampoline runs with `satp = 0` (bare) and the
/// gigapage identity map preserves it.
const fn phys_of(virt: u64) -> u64 {
    virt
}

#[cfg(test)]
#[path = "paging_tests.rs"]
mod tests;
