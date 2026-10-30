//! x86_64 page tables: the kernel's own root, the roots processes run on,
//! and the walk that edits them.
//!
//! Every root shares one kernel half: the kernel image's window (PML4 slot
//! 511), mapped section by section with only the access each needs; the
//! direct physical map (slots 256–509), which nothing executes from; and the
//! kernel heap's remap window (slot 510). A process root adds its own user
//! half and nothing else.
//!
//! The kernel's own root has no user half and maps nothing physical as
//! itself: the boot CPU moves onto it before the kernel proper runs
//! (`enter_kernel_root`), and an application processor joins it before its
//! entry does anything else (`join_kernel_root`). The one identity mapping
//! left is the page an application processor starts executing at, in a root
//! only a starting processor loads.
//!
//! The walk ([`tairix_arch_api::mmu::AddressSpace`]) recovers each level
//! through the frame source that drew it, so it runs on the host too; only a
//! `CR3` load needs the metal.

use core::cell::UnsafeCell;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicU64, AtomicU8, AtomicUsize, Ordering};

use tairix_arch_api::frames::{pool_slot_of, PageTableFrames, TableFrame};

#[cfg(all(target_arch = "x86_64", target_os = "none"))]
extern "C" {
    /// The boot trampoline's PDPT for the direct physical map's floor
    /// (`boot.s` SAFETY-INVARIANT 10), linked 1:1 in low memory.
    static boot_pdpt_physmap: u8;
}
use tairix_arch_api::mmu::{AddressSpace as MmuAddressSpace, KernelWindow, MapError, PageFlags};
use tairix_arch_api::tags::{AddressTag, AddressTags, ParkRoot};
use tairix_arch_api::tlb::TlbShootdown;

/// Size of a single page (and of a page-table page): the one system granule.
pub use tairix_abi::PAGE_SIZE;

/// Number of 64-bit entries in a page-table page (PML4 / PDPT / PD / PT).
pub const ENTRIES_PER_TABLE: usize = 512;

/// Gigabytes of the direct physical map the boot trampoline lays before the
/// firmware memory map is read (`boot.s` SAFETY-INVARIANT 10, whose
/// `boot_pds_physmap` holds a page directory for each).
///
/// What the boot reads before it knows the machine lies below it: the
/// loader's hand-off, the firmware tables, and the architectural register
/// blocks.
pub const BOOT_PHYSMAP_GIB: usize = 4;

/// The first physical address past the boot trampoline's direct-map floor.
pub const BOOT_PHYSMAP_END: u64 = (BOOT_PHYSMAP_GIB as u64) << 30;

/// Sign-extend a PML4 slot's base to its canonical higher-half virtual
/// address: bit 47 of any slot at or above 256 is set, so bits 63:48
/// are ones.
const fn canonical_slot_base(slot: usize) -> u64 {
    0xFFFF_0000_0000_0000 | ((slot as u64) << 39)
}

/// First PML4 slot the direct physical map claims.
///
/// The port's user virtual region runs to `1 << 47`, which is exactly this
/// slot's base, and slots 510 (the kernel remap window) and 511 (the
/// higher-half kernel image) are taken. So the map starts at the first
/// slot above user space and runs to the remap window: the kernel half
/// and the user half share no slot, which is what lets a process root
/// carry the map without carrying a mapping of RAM in the half user code
/// addresses.
pub const PHYSMAP_PML4_FIRST_SLOT: usize = 256;

/// PML4 slots the direct physical map spans — everything from its first
/// slot up to the kernel remap window. Derived, so moving either boundary
/// cannot leave the two overlapping.
pub const PHYSMAP_PML4_SLOTS: usize = KERNEL_WINDOW_PML4_SLOT - PHYSMAP_PML4_FIRST_SLOT;

/// Base virtual address of the direct physical map: physical `p` is
/// reachable at `PHYSMAP_VMA_BASE + p` under every root.
pub const PHYSMAP_VMA_BASE: u64 = canonical_slot_base(PHYSMAP_PML4_FIRST_SLOT);

/// Widest direct physical map the claimed slots can express, in gigabytes
/// (512 GiB per slot).
pub const MAX_PHYSMAP_GIB: usize = PHYSMAP_PML4_SLOTS * ENTRIES_PER_TABLE;

/// The map must start below the kernel remap window (or its slot count
/// would be a negative span) and stay in the higher half (or its base
/// would need no sign extension and the spelling above would be wrong).
/// Pinned so moving either boundary fails the build rather than producing
/// an address nothing maps.
const _: () = assert!(
    PHYSMAP_PML4_FIRST_SLOT < KERNEL_WINDOW_PML4_SLOT
        && PHYSMAP_PML4_FIRST_SLOT >= ENTRIES_PER_TABLE / 2,
    "the direct physical map must sit in the higher half, below the kernel remap window"
);

/// The map's shared PML4 entries, one per claimed slot, or `0` for a slot
/// the reservation did not need.
///
/// Every root this port builds installs them, so the tables beneath them
/// are shared rather than redrawn per process: a root's share of the map
/// is its own PML4 entries and not one page.
static PHYSMAP_PML4: [AtomicU64; PHYSMAP_PML4_SLOTS] =
    [const { AtomicU64::new(0) }; PHYSMAP_PML4_SLOTS];

/// Gigabytes the direct map covers before it is widened: the floor the
/// boot trampoline lays down (`boot.s` SAFETY-INVARIANT 10). The host has
/// no trampoline and so no map at all.
const PHYSMAP_FLOOR_GIB: usize = if cfg!(all(target_arch = "x86_64", target_os = "none")) {
    BOOT_PHYSMAP_GIB
} else {
    0
};

/// Gigabytes of physical memory the live direct map covers.
static PHYSMAP_GIGAPAGES: AtomicUsize = AtomicUsize::new(PHYSMAP_FLOOR_GIB);

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
/// `const`, so a fixed MMIO register block (the LAPIC, an IO-APIC) names
/// its direct-map address without a run-time load on the interrupt path.
/// The address resolves only for a `phys` below [`physmap_bytes`]; a
/// caller with a discovered address checks that first.
#[must_use]
pub const fn physmap_virt(phys: u64) -> u64 {
    PHYSMAP_VMA_BASE.wrapping_add(phys)
}

/// Whether the live direct map holds all of physical `[phys, phys + len)`.
fn physmap_holds(phys: u64, len: u64) -> bool {
    phys.checked_add(len)
        .is_some_and(|end| end <= physmap_bytes())
}

/// The direct-map address of physical `[phys, phys + len)`, or [`None`] for
/// an empty span or one the live map does not wholly hold.
#[must_use]
pub fn physmap_span(phys: u64, len: u64) -> Option<NonNull<u8>> {
    if len == 0 || !physmap_holds(phys, len) {
        return None;
    }
    let virt = usize::try_from(physmap_virt(phys)).ok()?;
    NonNull::new(core::ptr::with_exposed_provenance_mut(virt))
}

/// The bytes at physical `[phys, phys + len)`, read through the direct map,
/// or [`None`] where [`physmap_span`] refuses the span.
///
/// # Safety
///
/// Nothing may write those bytes while the slice is in use: they are the
/// loader's hand-off or firmware tables, never memory the kernel allocates.
#[must_use]
pub unsafe fn physmap_slice(phys: u64, len: usize) -> Option<&'static [u8]> {
    let base = physmap_span(phys, u64::try_from(len).ok()?)?;
    // SAFETY: `physmap_span` placed the whole span inside the live direct
    // map, which is never torn down, and the caller vouches that nothing
    // writes it.
    Some(unsafe { core::slice::from_raw_parts(base.as_ptr().cast_const(), len) })
}

/// Write the cache lines holding physical `[phys, phys + len)` back to
/// memory, `line` bytes at a time, for a DMA walker that does not snoop the
/// caches: stores before the call reach memory before any after it. `false`,
/// writing nothing back, for a range outside the direct map or a `line` that
/// is not a power of two.
#[must_use]
pub fn write_back(phys: u64, len: usize, line: u64) -> bool {
    let Ok(len) = u64::try_from(len) else {
        return false;
    };
    if !physmap_holds(phys, len) || !line.is_power_of_two() {
        return false;
    }
    #[cfg(all(target_arch = "x86_64", target_os = "none"))]
    {
        let stop = physmap_virt(phys + len);
        let mut addr = physmap_virt(phys & !(line - 1));
        // SAFETY: `mfence` orders memory accesses only. The first keeps the
        // flushes below behind the stores that wrote the lines, the second
        // keeps whatever the caller does next behind the flushes.
        unsafe { core::arch::asm!("mfence", options(nostack, preserves_flags)) };
        while addr < stop {
            // SAFETY: `addr` is a direct-map address below `physmap_virt(end)`,
            // which the check above placed inside the live direct map;
            // `clflush` writes the line back and invalidates it, changing no
            // byte of memory.
            unsafe {
                core::arch::asm!("clflush [{}]", in(reg) addr, options(nostack, preserves_flags));
            }
            addr += line;
        }
        // SAFETY: as the fence above.
        unsafe { core::arch::asm!("mfence", options(nostack, preserves_flags)) };
    }
    true
}

/// `true` when the part maps 1 GiB pages at PDPT level (CPUID
/// `0x8000_0001` `EDX[26]`, AMD64 APM Vol. 3 / Intel SDM Vol. 2A).
///
/// With gigapages the direct map's widening draws no page directories at
/// all. The host build reports `false` (no CPU to ask), which only selects
/// the 2 MiB path the host never walks.
#[must_use]
pub fn gigapages_supported() -> bool {
    #[cfg(all(target_arch = "x86_64", target_os = "none"))]
    {
        // Leaf `0x8000_0000` reports the highest extended leaf, so the
        // feature leaf is read only once the part admits to having it.
        if core::arch::x86_64::__cpuid(0x8000_0000).eax < 0x8000_0001 {
            return false;
        }
        core::arch::x86_64::__cpuid(0x8000_0001).edx & (1 << 26) != 0
    }
    #[cfg(not(all(target_arch = "x86_64", target_os = "none")))]
    {
        false
    }
}

/// Page-table pages `install_boot_physmap` must draw to widen the map to
/// `gib` gigabytes.
///
/// The first span's PDPT and the leaves below [`BOOT_PHYSMAP_GIB`] exist
/// before it runs, so what is left is one PDPT per *further* 512 GiB
/// span, plus — only on a part without 1 GiB pages, where a PDPT cannot
/// hold the leaves itself — one page directory per gigabyte above the
/// floor. A `gib` at or below the floor needs nothing.
#[must_use]
pub fn physmap_table_frames(gib: usize) -> usize {
    // A map at or below the floor draws nothing: it has no span beyond the
    // trampoline's own and no gigabyte above the leaves it already holds.
    let further_spans = gib.div_ceil(ENTRIES_PER_TABLE).saturating_sub(1);
    let directories = if gigapages_supported() {
        0
    } else {
        gib.saturating_sub(PHYSMAP_FLOOR_GIB)
    };
    further_spans + directories
}

/// Base virtual address of the -2 GiB higher-half kernel window.
///
/// A kernel symbol linked at `KERNEL_VMA_BASE + p` is loaded at physical
/// `p` (`kernel/arch/x86_64/linker.ld`; `boot.s` SAFETY-INVARIANT 9). Used
/// to turn a higher-half kernel virtual address back into the physical
/// address the MMU needs in a page-table entry or CR3. Must equal the
/// `KERNEL_VMA_BASE` in `linker.ld` and the literal in `boot.s`.
pub const KERNEL_VMA_BASE: u64 = 0xFFFF_FFFF_8000_0000;

/// Physical-address field of a page-table entry (bits 51:12).
///
/// Masking an entry with this recovers the child table's — or the mapped
/// page's — physical address without the flag and attribute bits.
const ADDR_MASK: u64 = 0x000F_FFFF_FFFF_F000;

/// One past the highest physical address an entry can name.
pub const PHYSICAL_ADDRESS_LIMIT: u64 = (ADDR_MASK | 0xFFF) + 1;

/// Page-table entry flags actually used here.
pub mod flags {
    /// Entry is present.
    pub const PRESENT: u64 = 1 << 0;
    /// Writable.
    pub const WRITABLE: u64 = 1 << 1;
    /// User-accessible (CPL 3 may reach the page). Must be set on the
    /// leaf **and** on every intermediate entry on the walk, otherwise
    /// the CPU denies the ring-3 access (Intel SDM Vol 3A §4.6).
    pub const USER: u64 = 1 << 2;
    /// Page-level write-through/PAT index bit.
    pub const WRITE_THROUGH: u64 = 1 << 3;
    /// Page-level cache-disable/PAT index bit.
    pub const CACHE_DISABLE: u64 = 1 << 4;
    /// Accessed: the CPU sets this on the leaf (and every intermediate
    /// entry it walks) the first time the page is read, written, or
    /// fetched, and never clears it itself (Intel SDM Vol 3A §4.8). This
    /// is the hardware referenced bit the page-replacement clock scan
    /// reads and clears to tell a genuinely cold page from a hot one
    /// before the compressed-memory tier reclaims it.
    pub const ACCESSED: u64 = 1 << 5;
    /// Page Size (1 for huge pages at PD or PDPT level).
    pub const HUGE: u64 = 1 << 7;
    /// No-Execute (bit 63): an instruction fetch from the page faults.
    /// Both trampolines set `IA32_EFER.NXE` before paging is on, so every
    /// CPU honours it from its first instruction.
    pub const NO_EXECUTE: u64 = 1 << 63;
    /// One of the bits a leaf leaves to software, marking a DMA buffer
    /// ([`PageFlags::DMA`](tairix_arch_api::mmu::PageFlags::DMA)).
    pub const SW_DMA: u64 = 1 << 9;
}

#[cfg(any(test, all(target_arch = "x86_64", target_os = "none")))]
mod kernel_root;
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub(crate) use kernel_root::{enter_kernel_root, join_kernel_root, startup_root};

/// What the kernel may do with a page it maps for itself: never more than
/// one of writing and executing, and nothing for ring 3.
#[cfg(any(test, all(target_arch = "x86_64", target_os = "none")))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KernelAccess {
    /// Read and execute: the kernel's text.
    Execute,
    /// Read only.
    Read,
    /// Read and write.
    Write,
}

#[cfg(any(test, all(target_arch = "x86_64", target_os = "none")))]
impl KernelAccess {
    /// The leaf bits granting exactly this access, for a 4 KiB leaf or,
    /// with [`flags::HUGE`] added, a larger one.
    const fn leaf(self) -> u64 {
        match self {
            Self::Execute => flags::PRESENT,
            Self::Read => flags::PRESENT | flags::NO_EXECUTE,
            Self::Write => flags::PRESENT | flags::WRITABLE | flags::NO_EXECUTE,
        }
    }
}

/// Bits of an entry pointing at a kernel table below it: the leaves decide
/// the access, so the entry restricts nothing itself.
#[cfg(any(test, all(target_arch = "x86_64", target_os = "none")))]
const KERNEL_TABLE_ENTRY: u64 = flags::PRESENT | flags::WRITABLE;

/// Leaf permissions, memory type and software marks one 4 KiB mapping walk
/// applies.
///
/// Grouped into a named value so the walk is steered by labelled fields
/// rather than a row of positional booleans at each call site.
#[derive(Clone, Copy)]
struct LeafPolicy {
    writable: bool,
    user: bool,
    no_execute: bool,
    attrs: u64,
}

impl LeafPolicy {
    /// The page-table entry flag word this policy maps to.
    fn pte_flags(self) -> u64 {
        let mut bits = flags::PRESENT;
        if self.writable {
            bits |= flags::WRITABLE;
        }
        if self.user {
            bits |= flags::USER;
        }
        if self.no_execute {
            bits |= flags::NO_EXECUTE;
        }
        bits | self.attrs
    }
}

/// One page-table page: 512 × u64, naturally aligned.
#[repr(C, align(4096))]
struct Table([u64; ENTRIES_PER_TABLE]);

impl Table {
    const fn new() -> Self {
        Self([0; ENTRIES_PER_TABLE])
    }
}

/// Pages one fine-grained mapping chain costs (PDPT + PD + PT).
const PAGES_PER_FINE_CHAIN: usize = 3;

/// Page-table pages a live root costs: its PML4 and one fine-grained chain.
/// The kernel half it carries is shared, not drawn per root.
const PAGES_PER_LIVE_ROOT: usize = 1 + PAGES_PER_FINE_CHAIN;

/// Pages a [`PageTablePool`] hands out unless its type names another count:
/// the two roots a bring-up path or a fixture builds at once, plus a further
/// fine-grained chain each. A pool that runs dry makes `alloc_table` answer
/// `None`, so the constructor fails closed rather than returning a root with
/// holes in it.
const POOL_TABLES: usize = 2 * (PAGES_PER_LIVE_ROOT + PAGES_PER_FINE_CHAIN);

/// A statically-allocated pool of `N` zero-initialised page-table pages.
///
/// Allocation is monotonic: tables drawn from kernel-image storage are
/// never reclaimable RAM, so a returned one is retired without reuse.
pub struct PageTablePool<const N: usize = POOL_TABLES> {
    storage: [UnsafeCell<Table>; N],
    used: AtomicUsize,
}

// SAFETY: the pool exposes `&self` allocation but every allocated frame
// is handed out exactly once (monotonic `AtomicUsize`), so distinct
// allocations never alias. Callers receive `&'static mut [u64; 512]`s
// whose lifetimes are disjoint by construction.
unsafe impl<const N: usize> Sync for PageTablePool<N> {}

impl<const N: usize> Default for PageTablePool<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> PageTablePool<N> {
    /// Construct an empty pool. `const`, so the pool lives in `.bss`.
    #[must_use]
    pub const fn new() -> Self {
        // Each element must be its own cell, so the initialiser is a
        // `const` copied per slot rather than a value shared between them.
        #[allow(clippy::declare_interior_mutable_const)]
        const ZERO: UnsafeCell<Table> = UnsafeCell::new(Table::new());
        // Evaluated into the `static` each pool is, never a stack frame,
        // whatever the `large_stack_arrays` heuristic assumes.
        #[allow(clippy::large_stack_arrays)]
        let storage = [ZERO; N];
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
        if idx >= N {
            // Roll back so subsequent allocations also fail closed rather
            // than overflowing `usize`.
            self.used.store(N, Ordering::SeqCst);
            return None;
        }
        // SAFETY: monotonic allocator + atomic fetch_add means this index
        // is owned by *this* call uniquely. We cast the `UnsafeCell`
        // pointer to a `&'static mut` once and never alias it.
        let cell = &self.storage[idx];
        let raw = cell.get();
        let table_ref: &'static mut Table = unsafe { &mut *raw };
        Some(&mut table_ref.0)
    }
}

impl<const N: usize> PageTableFrames for PageTablePool<N> {
    fn alloc_table(&self) -> Option<TableFrame> {
        let entries = self.alloc()?;
        // The static pool is kernel-image storage, so its physical address
        // is its image-window address less the window's base.
        let phys = phys_of(entries.as_ptr() as u64);
        Some(TableFrame { phys, entries })
    }

    fn table_at(&self, phys: u64) -> Option<*mut [u64; ENTRIES_PER_TABLE]> {
        // Recovered from the slot the pool handed `phys` out of, so the
        // pointer keeps its storage's provenance; a `phys` from anywhere
        // else names no slot and the walk asking for it fails closed.
        let index = pool_slot_of(phys_of(self.storage.as_ptr() as u64), N, phys)?;
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

/// An address space built on a freshly-allocated PML4.
///
/// A root carries the kernel half every root shares — the kernel image's
/// window, the remap window, the direct physical map — and whatever user
/// mappings are added to it; none maps anything physical as itself.
pub struct AddressSpace {
    pml4_phys: u64,
    /// The frame source the page-table walk allocates intermediate
    /// tables from, retained so the [`tairix_arch_api::mmu::AddressSpace`]
    /// HAL impl can install mappings without the caller re-supplying it.
    /// The static [`PageTablePool`] is the boot/bootstrap source; a real
    /// per-process space is built over the `kernel/mem` frame-allocator
    /// source (`plans/WIRING.md` W5b-3).
    frames: &'static dyn PageTableFrames,
}

impl AddressSpace {
    /// Build a root for a **process**: the shared kernel half and an empty
    /// user half.
    ///
    /// The kernel half's tables are shared rather than drawn per root, so a
    /// process pays its own PML4 for the whole kernel — image, heap window
    /// and all of RAM — and not one page more.
    ///
    /// # Errors
    ///
    /// Returns `None` if the frame source is exhausted.
    pub fn new_process_root(frames: &'static dyn PageTableFrames) -> Option<Self> {
        let TableFrame {
            phys: pml4_phys,
            entries: pml4,
        } = frames.alloc_table()?;
        install_kernel_half(pml4);
        Some(Self { pml4_phys, frames })
    }

    /// Build a root that maps **only** the kernel remap window — the handle
    /// the kernel-heap remap layer edits the window's shared sub-hierarchy
    /// through.
    ///
    /// The root is never loaded into `CR3`: because the window's PML4 entry
    /// points at a PDPT every other root shares, a leaf installed through
    /// this space is immediately visible under all of them. Keeping it
    /// separate means the remap layer draws its intermediate tables from the
    /// frame allocator rather than from the fixed boot pool, and cannot
    /// reach any address outside the window.
    ///
    /// # Errors
    ///
    /// Returns `None` if the frame source cannot supply the root table.
    pub fn new_kernel_window(frames: &'static dyn PageTableFrames) -> Option<Self> {
        let TableFrame {
            phys: pml4_phys,
            entries: pml4,
        } = frames.alloc_table()?;
        install_kernel_window_slot(pml4);
        Some(Self { pml4_phys, frames })
    }

    /// The PML4, recovered through the frame source that drew it, or
    /// [`None`] when the source cannot reach it (fail closed).
    ///
    /// The space retains only `pml4_phys`, so the root is reached exactly
    /// as every other level of the walk is and no long-lived `&mut` to it
    /// can alias a second walk of the same table.
    fn root_table(&self) -> Option<*mut [u64; ENTRIES_PER_TABLE]> {
        self.frames.table_at(self.pml4_phys)
    }

    /// `true` if `vaddr` already resolves to a live leaf (4 KiB page or
    /// 2 MiB huge page) in this hierarchy.
    ///
    /// A read-only four-level walk used by the
    /// [`tairix_arch_api::mmu::AddressSpace`] HAL impl to report
    /// [`tairix_arch_api::mmu::MapError::AlreadyMapped`] rather than
    /// silently clobber an existing mapping (`map_4k_inner` overwrites a
    /// PT leaf without checking, so the HAL layer must guard it here).
    /// Each level is recovered through the frame source that drew it,
    /// exactly as [`ensure_child`] does, so an entry the source cannot
    /// reach reads as "no leaf here".
    fn leaf_present(&self, vaddr: u64) -> bool {
        let i4 = ((vaddr >> 39) & 0x1FF) as usize;
        let i3 = ((vaddr >> 30) & 0x1FF) as usize;
        let i2 = ((vaddr >> 21) & 0x1FF) as usize;
        let i1 = ((vaddr >> 12) & 0x1FF) as usize;
        let Some(root) = self.root_table() else {
            return false;
        };
        // SAFETY: `pml4_phys` names this space's live PML4, drawn from
        // `self.frames`; `&self` keeps the read shared.
        let e4 = unsafe { &*root }[i4];
        if e4 & flags::PRESENT == 0 {
            return false;
        }
        let Some(pdpt) = self.frames.table_at(e4 & ADDR_MASK) else {
            return false;
        };
        // SAFETY: a present entry holds a table address `ensure_child`
        // drew from this source, so its view of it is a live table of this
        // hierarchy.
        let e3 = unsafe { &*pdpt }[i3];
        if e3 & flags::PRESENT == 0 {
            return false;
        }
        if e3 & flags::HUGE != 0 {
            return true;
        }
        let Some(pd) = self.frames.table_at(e3 & ADDR_MASK) else {
            return false;
        };
        // SAFETY: as above — a present non-huge PDPT entry's address is a
        // live PD of this hierarchy.
        let e2 = unsafe { &*pd }[i2];
        if e2 & flags::PRESENT == 0 {
            return false;
        }
        if e2 & flags::HUGE != 0 {
            return true;
        }
        let Some(pt) = self.frames.table_at(e2 & ADDR_MASK) else {
            return false;
        };
        // SAFETY: as above — a present non-huge PD entry's address is a
        // live PT of this hierarchy.
        (unsafe { &*pt })[i1] & flags::PRESENT != 0
    }

    /// Map `paddr` at `vaddr` with a 4 KiB page granularity.
    ///
    /// `vaddr` and `paddr` must be 4 KiB-aligned. Returns `None` on
    /// page-table-pool exhaustion or if the walk meets an existing huge
    /// page.
    pub fn map_4k(
        &mut self,
        frames: &'static dyn PageTableFrames,
        vaddr: u64,
        paddr: u64,
        writable: bool,
    ) -> Option<()> {
        self.map_4k_inner(
            frames,
            vaddr,
            paddr,
            LeafPolicy {
                writable,
                user: false,
                no_execute: false,
                attrs: 0,
            },
        )
    }

    /// Map `paddr` at `vaddr` (4 KiB granularity) **user-accessible** with
    /// explicit W^X leaf permissions: `writable` selects [`flags::WRITABLE`]
    /// and `executable` selects whether the page is instruction-fetchable.
    /// A non-executable leaf gets the [`flags::NO_EXECUTE`] bit, so a
    /// writable data page is mapped non-executable (`RW`) and a read-only
    /// data page non-executable (`R`) — the W^X contract a
    /// process image's `RW`/`R` segments and its stack need (a code segment
    /// is mapped `executable = true`, `writable = false`, i.e. `RX`).
    ///
    /// `vaddr` and `paddr` must be 4 KiB-aligned. Returns `None` on
    /// page-table-pool exhaustion or if the walk hits an existing huge page.
    pub fn map_4k_user_wx(
        &mut self,
        frames: &'static dyn PageTableFrames,
        vaddr: u64,
        paddr: u64,
        writable: bool,
        executable: bool,
    ) -> Option<()> {
        self.map_4k_inner(
            frames,
            vaddr,
            paddr,
            LeafPolicy {
                writable,
                user: true,
                no_execute: !executable,
                attrs: 0,
            },
        )
    }

    /// Shared 4 KiB mapping walk for [`Self::map_4k`] and
    /// [`Self::map_4k_user_wx`] (one definition).
    ///
    /// When `leaf.user` is set, [`flags::USER`] is OR-ed into the leaf and
    /// into each intermediate entry on the walk; a kernel mapping leaves
    /// every level without the bit, so ring 3 cannot reach it.
    fn map_4k_inner(
        &mut self,
        frames: &'static dyn PageTableFrames,
        vaddr: u64,
        paddr: u64,
        leaf: LeafPolicy,
    ) -> Option<()> {
        assert_eq!(vaddr & 0xFFF, 0, "vaddr must be page-aligned");
        assert_eq!(paddr & 0xFFF, 0, "paddr must be page-aligned");

        let flags_ = leaf.pte_flags();

        let i4 = ((vaddr >> 39) & 0x1FF) as usize;
        let i3 = ((vaddr >> 30) & 0x1FF) as usize;
        let i2 = ((vaddr >> 21) & 0x1FF) as usize;
        let i1 = ((vaddr >> 12) & 0x1FF) as usize;

        // A user leaf in a kernel-half slot would hand ring 3 the direct
        // physical map, the kernel heap's remap window, or the kernel
        // image. The window allocators already bound every user address
        // below the half, so this is the fail-closed floor under them
        // rather than the only check.
        if leaf.user && is_kernel_half_slot(i4) {
            return None;
        }

        // SAFETY: `pml4_phys` names this space's live PML4, drawn from
        // `self.frames`; `&mut self` makes the exclusive borrow sound.
        let pml4 = unsafe { &mut *self.root_table()? };
        let pdpt = ensure_child(pml4, i4, frames)?;
        if leaf.user {
            pml4[i4] |= flags::USER;
        }
        let pd = ensure_child(pdpt, i3, frames)?;
        if leaf.user {
            pdpt[i3] |= flags::USER;
        }

        // A huge leaf here is a live mapping the walk must not shatter.
        if (pd[i2] & flags::HUGE) != 0 {
            return None;
        }
        let pt = ensure_child(pd, i2, frames)?;
        if leaf.user {
            pd[i2] |= flags::USER;
        }
        pt[i1] = paddr | flags_;
        Some(())
    }

    /// Switch the active page table to this address space.
    ///
    /// # Safety
    ///
    /// Caller must guarantee that the new PML4 also maps the currently
    /// executing instruction's `rip` and the current stack — otherwise
    /// the CPU will fault on the very next memory access. Every root
    /// carries the kernel half, so that holds for code and stacks the kernel
    /// runs on.
    #[cfg(all(target_arch = "x86_64", target_os = "none"))]
    pub unsafe fn switch(&self) {
        // SAFETY: caller asserts the new mappings cover RIP and RSP; see
        // the `# Safety` paragraph above. `mov cr3, _` is otherwise a
        // pure architectural state change.
        unsafe {
            core::arch::asm!(
                "mov cr3, {p}",
                p = in(reg) self.pml4_phys,
                options(nostack, preserves_flags),
            );
        }
    }

    /// Physical address of this PML4 (i.e. the value that would go into
    /// CR3). Exposed so tests can observe it for assertions.
    #[must_use]
    pub fn pml4_phys(&self) -> u64 {
        self.pml4_phys
    }
}

/// PML4 slot the kernel remap window claims: the highest one below the
/// kernel image's. One PML4 slot is 512 GiB of address space, which costs
/// nothing until something is backed into it and needs one shared PDPT.
const KERNEL_WINDOW_PML4_SLOT: usize = KERNEL_IMAGE_PML4_SLOT - 1;

/// PML4 slot of the -2 GiB window the kernel image is linked in.
const KERNEL_IMAGE_PML4_SLOT: usize = ((KERNEL_VMA_BASE >> 39) & 0x1FF) as usize;

const _: () = assert!(
    KERNEL_IMAGE_PML4_SLOT == ENTRIES_PER_TABLE - 1 && KERNEL_VMA_BASE.trailing_zeros() >= 30,
    "the image window is the top PML4 slot, and its base a whole gigabyte"
);

/// Pages the kernel remap window spans: one PML4 slot is 512 entries at
/// each of the three levels below it.
const KERNEL_WINDOW_PAGES: usize = ENTRIES_PER_TABLE * ENTRIES_PER_TABLE * ENTRIES_PER_TABLE;

/// The window's shared PML4 entry, or `0` before
/// [`reserve_kernel_window`] runs.
///
/// Every root this port builds installs it, so a leaf added under the
/// shared PDPT it points at resolves identically whichever root is active —
/// the property that lets kernel code reach a remapped kernel address while
/// a user task's root is loaded.
static KERNEL_WINDOW_PML4: AtomicU64 = AtomicU64::new(0);

/// Base virtual address of the kernel remap window (canonical: PML4 slot
/// 510 sign-extends to the higher half).
#[must_use]
pub const fn kernel_window_base() -> u64 {
    // Bit 47 of the slot's base is set, so bits 63:48 sign-extend to ones.
    0xFFFF_0000_0000_0000 | ((KERNEL_WINDOW_PML4_SLOT as u64) << 39)
}

/// A window whose extent is not representable is refused at run time, which
/// would silently leave the kernel heap on its bootstrap region. Fail the
/// build instead.
const _: () = assert!(
    KernelWindow::is_representable(kernel_window_base(), KERNEL_WINDOW_PAGES),
    "the kernel remap window must be a representable extent"
);

/// Widen the direct physical map to `[0, gib GiB)` out of the carved
/// `tables` run and publish it as the map every later root installs
/// ([`physmap_gigapages`]).
///
/// The boot trampoline lays the map's floor down before it knows how much
/// RAM is installed (`boot.s` SAFETY-INVARIANT 10), covering
/// `[0, BOOT_PHYSMAP_GIB GiB)`, and the kernel root carries a copy of its
/// PDPT as the map's first span at [`PHYSMAP_PML4_FIRST_SLOT`]. That is
/// enough for the architectural MMIO frames and the firmware tables; this is
/// what extends it over the discovered RAM, so a frame the allocator draws
/// from the top of a multi-terabyte pool is reachable by pointer like any
/// other. Every leaf is non-executable, as the floor's are.
///
/// `tables` is the physical base of [`physmap_table_frames`] contiguous
/// page-aligned frames — the further spans' PDPTs first, then the page
/// directories a part without 1 GiB pages needs. They are written through
/// the floor, so the run must lie inside it.
///
/// Returns `false`, having changed nothing, for a `gib` that is not a
/// widening ([`BOOT_PHYSMAP_GIB`] or less), exceeds [`MAX_PHYSMAP_GIB`],
/// comes with a `tables` run that is misaligned or reaches outside the
/// floor, arrives after the map has already been widened, or meets a live
/// root that does not carry the published first span — the caller then
/// fails the boot rather than running on a map it did not install.
///
/// # Safety
///
/// * Paging is enabled, the caller runs on the boot CPU before any
///   secondary is brought up, and no other CPU walks the live PML4.
/// * `tables` names [`physmap_table_frames`] page-aligned frames that no
///   other owner holds (the boot path reserves them out of the memory map
///   before the frame allocator is built).
///
/// The widening only adds translations — in the first span above the
/// floor, and in PML4 slots left empty — so no live translation changes
/// meaning; `CR3` is reloaded at the end so the CPU picks the new entries
/// up.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
#[must_use]
pub unsafe fn install_boot_physmap(gib: usize, tables: u64) -> bool {
    if gib <= BOOT_PHYSMAP_GIB || gib > MAX_PHYSMAP_GIB {
        return false;
    }
    // One widening per boot: a second would rewrite the trampoline's table
    // under roots already built from the published map.
    if physmap_gigapages() != PHYSMAP_FLOOR_GIB {
        return false;
    }
    let frames = physmap_table_frames(gib);
    let Some(bytes) = (frames as u64).checked_mul(PAGE_SIZE as u64) else {
        return false;
    };
    let inside_floor = tables & (PAGE_SIZE as u64 - 1) == 0
        && tables
            .checked_add(bytes)
            .is_some_and(|end| end <= BOOT_PHYSMAP_END);
    if !inside_floor {
        return false;
    }
    // SAFETY: `CR3` names the live PML4, a kernel-image table the floor
    // holds. The only entries written are the map's own slots, which are
    // empty above its first span.
    let Some(pml4) = (unsafe { mapped_table(active_root_phys()) }) else {
        return false;
    };
    // The first span the kernel root published; a live root without it is
    // not one this widening may extend.
    let floor = PHYSMAP_PML4[0].load(Ordering::Acquire);
    if floor == 0 || !names_same_table(pml4[PHYSMAP_PML4_FIRST_SLOT], floor) {
        return false;
    }

    // The carved run is laid out as the further spans' PDPTs followed by the
    // page directories, so a page is found by index alone.
    let spans = gib.div_ceil(ENTRIES_PER_TABLE);
    let huge = gigapages_supported();
    let directories = tables + ((spans - 1) as u64) * PAGE_SIZE as u64;
    let leaf = KernelAccess::Write.leaf() | flags::HUGE;

    // The entry for one gigabyte of the map: a 1 GiB leaf where the part
    // has them, else a page directory of 2 MiB leaves drawn from the run.
    // One definition, shared by the trampoline's span and the further ones.
    let entry_for = |slot_gib: usize| -> Option<u64> {
        let base = (slot_gib as u64) << 30;
        if huge {
            return Some(base | leaf);
        }
        let pd_phys = directories + ((slot_gib - PHYSMAP_FLOOR_GIB) as u64) * PAGE_SIZE as u64;
        // These frames were carved from the firmware memory map before the
        // frame allocator exists, so no `PageTableFrames` source drew them
        // and the floor is the only view of them there is.
        //
        // SAFETY: the caller pins the run as unowned page-aligned frames,
        // checked above to lie inside the floor, so the page aliases nothing
        // live.
        let pd = unsafe { mapped_table(pd_phys) }?;
        for (block, entry) in pd.iter_mut().enumerate() {
            *entry = (base + ((block as u64) << 21)) | leaf;
        }
        Some(pd_phys | KERNEL_TABLE_ENTRY)
    };

    let mut entries = [0u64; PHYSMAP_PML4_SLOTS];
    entries[0] = floor;
    for (span, entry) in entries.iter_mut().enumerate().take(spans) {
        let pdpt_phys = if span == 0 {
            floor & ADDR_MASK
        } else {
            let phys = tables + ((span - 1) as u64) * PAGE_SIZE as u64;
            *entry = phys | KERNEL_TABLE_ENTRY;
            phys
        };
        // SAFETY: span 0 is the kernel root's own table and the rest are
        // pages of the caller's carved run; both lie inside the floor and
        // alias nothing live.
        let Some(pdpt) = (unsafe { mapped_table(pdpt_phys) }) else {
            return false;
        };
        let first_gib = span * ENTRIES_PER_TABLE;
        // The first span already maps the floor; leave those entries
        // exactly as they are, since live translations use them.
        let from = if span == 0 { PHYSMAP_FLOOR_GIB } else { 0 };
        for (slot, pte) in pdpt.iter_mut().enumerate().skip(from) {
            let slot_gib = first_gib + slot;
            if slot_gib >= gib {
                break;
            }
            let Some(entry) = entry_for(slot_gib) else {
                return false;
            };
            *pte = entry;
        }
    }

    if !publish_physmap(&entries, gib) {
        return false;
    }
    install_physmap_slots(pml4);

    invalidate_all_local();
    true
}

/// Record `entries` as the direct map's shared PML4 entries and `gib` as
/// its extent, set-once.
///
/// Split out from `install_boot_physmap` because this half is pure
/// bookkeeping: it is what the host tests drive to observe that every root
/// constructor installs the published slots, with no `CR3` to patch.
///
/// Returns `false`, having published nothing, for an extent that does not
/// match the entries it arrives with, or for a second call.
#[cfg(any(all(target_arch = "x86_64", target_os = "none"), test))]
fn publish_physmap(entries: &[u64; PHYSMAP_PML4_SLOTS], gib: usize) -> bool {
    // A publication must add gigabytes above the floor the trampoline
    // already covers, and stay inside the slots the map claims.
    if gib.saturating_sub(PHYSMAP_FLOOR_GIB) == 0 || gib > MAX_PHYSMAP_GIB {
        return false;
    }
    // An entry per 512 GiB span and no more: a short run would leave a hole
    // the map claims to cover, a long one a slot nothing backs.
    let spans = gib.div_ceil(ENTRIES_PER_TABLE);
    if entries.iter().take(spans).any(|entry| *entry == 0)
        || entries.iter().skip(spans).any(|entry| *entry != 0)
    {
        return false;
    }
    if PHYSMAP_GIGAPAGES
        .compare_exchange(PHYSMAP_FLOOR_GIB, gib, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return false;
    }
    for (slot, entry) in PHYSMAP_PML4.iter().zip(entries) {
        slot.store(*entry, Ordering::Release);
    }
    true
}

/// Copy the published direct-map entries into `pml4`'s claimed slots.
///
/// Every root constructor calls this, so no space can be built without the
/// map: the kernel root's first span, and the widening's further spans (it
/// patches the live root itself). A not-present-to-present entry needs no
/// TLB maintenance: the CPU never caches an absent translation.
fn install_physmap_slots(pml4: &mut [u64; ENTRIES_PER_TABLE]) {
    for (offset, slot) in PHYSMAP_PML4.iter().enumerate() {
        let entry = slot.load(Ordering::Acquire);
        if entry != 0 {
            pml4[PHYSMAP_PML4_FIRST_SLOT + offset] = entry;
        }
    }
}

/// The direct map's floor entry as the boot trampoline built it: its own
/// PDPT, whose address is a link-time constant because `.boot.bss` is linked
/// 1:1 in low memory (`linker.ld`), so it *is* the physical address the MMU
/// needs.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
fn boot_physmap_entry() -> u64 {
    // A link-time constant, never dereferenced here.
    let phys = core::ptr::addr_of!(boot_pdpt_physmap) as u64;
    phys | KERNEL_TABLE_ENTRY
}

/// `true` if PML4 slot `index` belongs to the kernel half — the direct
/// physical map, the kernel remap window, or the higher-half kernel image.
///
/// The port's user virtual region stops exactly at the first of them, so
/// this is also "not addressable by a user program".
const fn is_kernel_half_slot(index: usize) -> bool {
    index >= PHYSMAP_PML4_FIRST_SLOT
}

/// Discard the calling CPU's cached translations under the PCID loaded —
/// every one, with PCIDs off — by reloading `CR3` with the value it already
/// holds.
///
/// The port sets no `GLOBAL` leaf, so the reload leaves nothing of that PCID
/// in the TLB or the paging-structure caches; the root is unchanged and still
/// maps the executing code, stack, and per-CPU data.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub(crate) fn invalidate_all_local() {
    // SAFETY: `mov cr3` with the value already loaded is a pure TLB flush.
    // It changes no translation and touches no memory.
    unsafe {
        core::arch::asm!(
            "mov {t}, cr3",
            "mov cr3, {t}",
            t = out(reg) _,
            options(nostack, preserves_flags),
        );
    }
}

/// Reserve the kernel remap window: draw the shared PDPT, publish the PML4
/// entry every root installs, and patch it into the live root so the
/// running CPUs see the window immediately.
///
/// Called once, from the boot path, after the frame allocator exists (the
/// table comes from it, not from the fixed boot pool). A second call
/// returns the same window without drawing anything. Returns `None`,
/// having changed nothing, when the frame source cannot supply the shared
/// table (fail closed — the kernel heap then stays on its bootstrap
/// region).
pub fn reserve_kernel_window(frames: &'static dyn PageTableFrames) -> Option<KernelWindow> {
    // SAFETY: the window's PML4 slot is this port's own — the compile-time
    // assertion above pins its extent, and `install_kernel_window` points
    // that slot of every root this port builds at one shared sub-hierarchy,
    // so the run is reserved and resolves identically under each.
    let window = unsafe { KernelWindow::at_address(kernel_window_base(), KERNEL_WINDOW_PAGES) }?;
    if KERNEL_WINDOW_PML4.load(Ordering::Acquire) != 0 {
        return Some(window);
    }
    let TableFrame { phys, entries: _ } = frames.alloc_table()?;
    KERNEL_WINDOW_PML4.store(phys | flags::PRESENT | flags::WRITABLE, Ordering::Release);
    install_kernel_window(frames, active_root_phys());
    Some(window)
}

/// Install the published window entry into the PML4 at `root_phys`,
/// reaching it through `frames`. Does nothing when no window is reserved,
/// when `root_phys` is zero (what the host build reports), or when
/// `frames` cannot reach that root.
fn install_kernel_window(frames: &'static dyn PageTableFrames, root_phys: u64) {
    if root_phys == 0 {
        return;
    }
    let Some(table) = frames.table_at(root_phys) else {
        return;
    };
    // SAFETY: a non-zero `CR3` base names the live PML4 and the production
    // source's direct map covers it, so its view is dereferenceable. The
    // only entry written is the window's own slot, which no other writer
    // touches.
    let root = unsafe { &mut *table };
    install_kernel_window_slot(root);
}

/// Copy the published window entry into `pml4`'s slot.
///
/// Every root constructor calls this, so a space built before *or* after
/// the reservation ends up with the window (the boot root is patched in
/// place by [`reserve_kernel_window`]). A not-present-to-present entry
/// needs no TLB maintenance: the CPU never caches an absent translation.
fn install_kernel_window_slot(pml4: &mut [u64; ENTRIES_PER_TABLE]) {
    let entry = KERNEL_WINDOW_PML4.load(Ordering::Acquire);
    if entry != 0 {
        pml4[KERNEL_WINDOW_PML4_SLOT] = entry;
    }
}

/// The image window's shared PML4 entry, or `0` before the kernel root is
/// built.
static KERNEL_IMAGE_PML4: AtomicU64 = AtomicU64::new(0);

/// Install the kernel half every root shares into `pml4`: the image window,
/// the remap window once reserved, and the direct physical map.
fn install_kernel_half(pml4: &mut [u64; ENTRIES_PER_TABLE]) {
    let image = KERNEL_IMAGE_PML4.load(Ordering::Acquire);
    if image != 0 {
        pml4[KERNEL_IMAGE_PML4_SLOT] = image;
    }
    install_kernel_window_slot(pml4);
    install_physmap_slots(pml4);
}

/// Whether `entry` points at the table that `table`, an entry this port
/// wrote, points at.
///
/// Compared by what the entry names rather than bit for bit: the CPU sets
/// the accessed bit in every entry it walks, so a live entry stops equalling
/// the value written into it the first time its span is touched.
#[cfg(any(test, all(target_arch = "x86_64", target_os = "none")))]
const fn names_same_table(entry: u64, table: u64) -> bool {
    entry & flags::PRESENT != 0
        && entry & flags::HUGE == 0
        && entry & ADDR_MASK == table & ADDR_MASK
}

/// The page table at physical `phys`, reached through the direct map, or
/// [`None`] for an address that is not a page the live map holds. No table
/// is ever at physical `0`.
///
/// # Safety
///
/// `phys` names a page table no other reference reaches while the returned
/// one is in use.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
unsafe fn mapped_table(phys: u64) -> Option<&'static mut [u64; ENTRIES_PER_TABLE]> {
    if phys == 0 || !phys.is_multiple_of(PAGE_SIZE as u64) {
        return None;
    }
    physmap_span(phys, PAGE_SIZE as u64)?;
    // SAFETY: the direct map holds the whole page, never torn down, and the
    // caller vouches that nothing else reaches the table.
    Some(unsafe { &mut *(physmap_virt(phys) as *mut [u64; ENTRIES_PER_TABLE]) })
}

/// The kernel root, which every CPU runs kernel threads and the dispatcher
/// on and [`park_kernel_root`] parks on.
static PARK_ROOT: ParkRoot = ParkRoot::new();

/// Park the calling CPU's translation regime on the kernel root under PCID
/// `0`, discarding that PCID's entries first when `flush`, so no user
/// space's root remains active after its task suspends or exits. Returns
/// `false`, changing nothing, before the kernel root is built (fail closed).
///
/// The dispatcher calls this after every switch-back from a user task;
/// address-space teardown calls it, flushing, before dismantling a root
/// that is somehow still active.
pub fn park_kernel_root(flush: bool) -> bool {
    // SAFETY: the kernel root carries the kernel half every root shares,
    // which holds everything kernel code runs on — exactly
    // `activate_user_root`'s contract (inert on the host, where no root is
    // ever published).
    unsafe { PARK_ROOT.park(flush, activate_user_root) }
}

/// The PCID field of `CR3`, below the table base, and the bit of a `CR3`
/// write that keeps the loaded PCID's entries instead of discarding them.
pub(crate) const CR3_PCID: u64 = 0xFFF;
const CR3_KEEP: u64 = 1 << 63;

/// Whether every CPU brought up has PCIDs on: [`PCIDS_UNKNOWN`] before the
/// first, which decides; any later CPU without them turns them off for all,
/// before a user task can run.
static PCIDS: AtomicU8 = AtomicU8::new(PCIDS_UNKNOWN);
const PCIDS_UNKNOWN: u8 = 0;
const PCIDS_ON: u8 = 1;
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
const PCIDS_OFF: u8 = 2;

/// Advanced before every invalidation of the kernel's shared mappings, which
/// `invlpg` and a `CR3` reload carry out only under the PCID in use.
static KERNEL_EPOCH: AtomicU64 = AtomicU64::new(0);

/// Advance [`KERNEL_EPOCH`], before a kernel-range shootdown reaches any CPU.
#[cfg(all(feature = "sched-arch", target_arch = "x86_64", target_os = "none"))]
pub(crate) fn advance_kernel_epoch() {
    KERNEL_EPOCH.fetch_add(1, Ordering::SeqCst);
}

/// The `CR3` value loading `root_phys` under PCID `tag`, keeping the PCID's
/// entries when `keep`.
#[must_use]
pub const fn cr3(root_phys: u64, tag: u16, keep: bool) -> u64 {
    let keep = if keep { CR3_KEEP } else { 0 };
    root_phys | (tag as u64 & CR3_PCID) | keep
}

/// Turn PCIDs on for the calling CPU when it has them, and record whether
/// every CPU does.
///
/// # Safety
///
/// CPL 0 on the CPU being brought up, interrupts disabled, its root loaded
/// under PCID `0`, as setting `CR4.PCIDE` requires.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub(crate) unsafe fn enable_pcids() {
    /// `CR4.PCIDE`: `CR3` carries a process-context identifier.
    const CR4_PCIDE: u64 = 1 << 17;
    /// CPUID leaf 1 `ECX` bit 17: the CPU has process-context identifiers.
    const LEAF1_ECX_PCID: u32 = 1 << 17;
    let has = core::arch::x86_64::__cpuid(1).ecx & LEAF1_ECX_PCID != 0;
    if has {
        // SAFETY: CPUID advertises PCIDs and `CR3`'s PCID field is `0`, so
        // setting `CR4.PCIDE` changes no translation and cannot fault.
        unsafe {
            core::arch::asm!(
                "mov {t}, cr4",
                "or {t}, {bit}",
                "mov cr4, {t}",
                t = out(reg) _,
                bit = in(reg) CR4_PCIDE,
                options(nostack),
            );
        }
        let _ =
            PCIDS.compare_exchange(PCIDS_UNKNOWN, PCIDS_ON, Ordering::AcqRel, Ordering::Acquire);
    } else {
        PCIDS.store(PCIDS_OFF, Ordering::Release);
    }
}

/// Whether `CR3` writes carry a PCID on every CPU.
fn pcids_on() -> bool {
    PCIDS.load(Ordering::Acquire) == PCIDS_ON
}

/// The port's address-space tags: a PCID in `CR3` beside each root, on a CPU
/// that has them.
pub struct X86_64Tags;

/// The x86_64 port's tags, which the kernel loads user roots and parks by.
pub static TAGS: X86_64Tags = X86_64Tags;

impl AddressTags for X86_64Tags {
    /// Every PCID but `0`, the park root's, once every CPU has them on.
    fn user_tags(&self) -> u16 {
        if pcids_on() {
            u16::try_from(CR3_PCID).unwrap_or(0)
        } else {
            0
        }
    }

    fn root_register(&self, root_phys: u64, tag: u16) -> u64 {
        cr3(root_phys, tag, false)
    }

    /// `invlpg` and a `CR3` reload discard only the loaded PCID's entries,
    /// and a user space's shootdown reaches only the CPUs running it.
    fn invalidation_reaches_every_tag(&self) -> bool {
        false
    }

    fn kernel_epoch(&self) -> u64 {
        KERNEL_EPOCH.load(Ordering::SeqCst)
    }

    unsafe fn flush_current_tag(&self) {
        #[cfg(all(target_arch = "x86_64", target_os = "none"))]
        invalidate_all_local();
    }

    unsafe fn park(&self, flush: bool) -> bool {
        park_kernel_root(flush)
    }
}

/// The physical root of the calling CPU's active translation regime
/// (`CR3`'s table base, PCID/flag bits masked off), or `0` on the host,
/// which runs no translation regime of its own.
fn active_root_phys() -> u64 {
    #[cfg(all(target_arch = "x86_64", target_os = "none"))]
    {
        let cr3: u64;
        // SAFETY: reading `CR3` observes the active root without side
        // effects; no Rust spelling exists for the control register.
        unsafe {
            core::arch::asm!("mov {v}, cr3", v = out(reg) cr3, options(nostack, preserves_flags, nomem));
        }
        cr3 & !CR3_PCID
    }
    #[cfg(not(all(target_arch = "x86_64", target_os = "none")))]
    {
        0
    }
}

/// Reactivate `root_phys` as the active top-level translation root under
/// `tag`'s PCID (load `CR3`) on a CPU whose paging is already enabled,
/// discarding that PCID's entries as it loads when the tag says so.
///
/// This is the X1 user-kthread `pre_resume` primitive (`plans/PI.md` §X),
/// the x86_64 sibling of the aarch64 `activate_user_root`: immediately
/// before the kernel returns into a user task's ring 3, that task's own
/// PML4 must be installed so its translations — and only its — are in
/// force, keeping sibling processes hardware-isolated. With PCIDs on, every
/// other PCID's entries stay in the TLB unseen; without them every load
/// discards the non-global entries (Intel SDM Vol 3A §4.10.4).
///
/// # Safety
///
/// Paging must already be enabled, and the PML4 at `root_phys` must map the
/// currently-executing kernel `rip`, `rsp`, and the data the code touches
/// (the per-CPU `swapgs` TLS, the dispatcher's stack) identically to the
/// outgoing root — every root this port builds carries the shared kernel
/// half those lie in, but a `root_phys` that does not faults the CPU on its
/// next access.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub unsafe fn activate_user_root(root_phys: u64, tag: AddressTag) {
    let value = if pcids_on() {
        cr3(root_phys, tag.tag, !tag.flush)
    } else {
        root_phys
    };
    // SAFETY: `mov cr3, _` swaps the active translation root, discarding the
    // loaded PCID's entries unless the keep bit is set, which only a CPU with
    // PCIDs on is given; it touches no memory and no Rust spelling exists for
    // `CR3`. The caller's contract guarantees the new root covers the running
    // kernel context (see the `# Safety` paragraph above).
    unsafe {
        core::arch::asm!(
            "mov cr3, {value}",
            value = in(reg) value,
            options(nostack, preserves_flags),
        );
    }
}

/// Host substitute: reloading `CR3` is meaningful only on the bare-metal
/// x86_64 target. Never linked into a kernel image and never reached on the
/// host (the QEMU verticals exercise the real reload).
///
/// # Safety
///
/// Carries the same contract as the bare-metal definition above (paging
/// enabled; `root_phys` maps the running kernel context), so the two `cfg`
/// arms present one `unsafe` API. The host body is inert.
#[cfg(not(all(target_arch = "x86_64", target_os = "none")))]
pub unsafe fn activate_user_root(root_phys: u64, tag: AddressTag) {
    let _ = (root_phys, tag);
}

impl MmuAddressSpace for AddressSpace {
    fn map_page(&mut self, vaddr: u64, paddr: u64, flags: PageFlags) -> Result<(), MapError> {
        if (vaddr & (PAGE_SIZE as u64 - 1)) != 0 || (paddr & (PAGE_SIZE as u64 - 1)) != 0 {
            return Err(MapError::Misaligned);
        }
        if flags.is_write_exec() {
            return Err(MapError::InvalidFlags);
        }
        // Every x86 DMA master snoops; RAM mapped uncached beside the direct
        // map's write-back alias would be a mismatched memory type.
        if flags.contains(PageFlags::WRITE_COMBINE) || flags.contains(PageFlags::DMA_COHERENT) {
            return Err(MapError::Unsupported);
        }
        if self.leaf_present(vaddr) {
            return Err(MapError::AlreadyMapped);
        }
        let frames = self.frames;
        let writable = flags.contains(PageFlags::WRITE);
        let user = flags.contains(PageFlags::USER);
        let executable = flags.contains(PageFlags::EXEC);
        let mut attrs = 0;
        if flags.contains(PageFlags::DEVICE) {
            attrs |= flags::CACHE_DISABLE | flags::WRITE_THROUGH;
        }
        if flags.contains(PageFlags::DMA) {
            attrs |= flags::SW_DMA;
        }
        let result = self.map_4k_inner(
            frames,
            vaddr,
            paddr,
            LeafPolicy {
                writable,
                user,
                no_execute: !executable,
                attrs,
            },
        );
        // Alignment and prior-mapping are ruled out, so the only remaining
        // failure is frame-source exhaustion.
        result.ok_or(MapError::PoolExhausted)
    }

    fn translate(&self, vaddr: u64) -> Option<(u64, PageFlags)> {
        let i4 = ((vaddr >> 39) & 0x1FF) as usize;
        let i3 = ((vaddr >> 30) & 0x1FF) as usize;
        let i2 = ((vaddr >> 21) & 0x1FF) as usize;
        let i1 = ((vaddr >> 12) & 0x1FF) as usize;
        // SAFETY: `pml4_phys` names this space's live PML4, drawn from
        // `self.frames`; `&self` keeps the read shared.
        let e4 = unsafe { &*self.root_table()? }[i4];
        if e4 & flags::PRESENT == 0 {
            return None;
        }
        // SAFETY: a present entry holds a table address `ensure_child`
        // drew from this source (the same round-trip `leaf_present` relies
        // on), so its view of it is a live table of this hierarchy.
        let e3 = unsafe { &*self.frames.table_at(e4 & ADDR_MASK)? }[i3];
        if e3 & flags::PRESENT == 0 {
            return None;
        }
        if e3 & flags::HUGE != 0 {
            return Some((
                resolved_page(e3 & ADDR_MASK, vaddr, 30),
                page_flags_from_pte(e3),
            ));
        }
        // SAFETY: as above — a present non-huge PDPT entry's address is a
        // live PD of this hierarchy.
        let e2 = unsafe { &*self.frames.table_at(e3 & ADDR_MASK)? }[i2];
        if e2 & flags::PRESENT == 0 {
            return None;
        }
        if e2 & flags::HUGE != 0 {
            return Some((
                resolved_page(e2 & ADDR_MASK, vaddr, 21),
                page_flags_from_pte(e2),
            ));
        }
        // SAFETY: as above — a present non-huge PD entry's address is a
        // live PT of this hierarchy.
        let e1 = unsafe { &*self.frames.table_at(e2 & ADDR_MASK)? }[i1];
        if e1 & flags::PRESENT == 0 {
            return None;
        }
        Some((e1 & ADDR_MASK, page_flags_from_pte(e1)))
    }

    fn unmap(&mut self, vaddr: u64) -> Result<u64, MapError> {
        if (vaddr & (PAGE_SIZE as u64 - 1)) != 0 {
            return Err(MapError::Misaligned);
        }
        let i4 = ((vaddr >> 39) & 0x1FF) as usize;
        let i3 = ((vaddr >> 30) & 0x1FF) as usize;
        let i2 = ((vaddr >> 21) & 0x1FF) as usize;
        let i1 = ((vaddr >> 12) & 0x1FF) as usize;
        // Navigate to the 4 KiB PT leaf without allocating. A missing
        // level or a huge-page leaf means there is no 4 KiB leaf to tear
        // down here — fail closed (per-page unmap never shatters a huge
        // page).
        let frames = self.frames;
        let root = self.root_table().ok_or(MapError::NotMapped)?;
        // SAFETY: `pml4_phys` names this space's live PML4, drawn from
        // `frames`; `&mut self` makes the exclusive borrow sound.
        let e4 = unsafe { &*root }[i4];
        if e4 & flags::PRESENT == 0 {
            return Err(MapError::NotMapped);
        }
        let pdpt = frames.table_at(e4 & ADDR_MASK).ok_or(MapError::NotMapped)?;
        // SAFETY: present entry → a live PDPT of this hierarchy (see
        // `translate`).
        let e3 = unsafe { &*pdpt }[i3];
        if e3 & flags::PRESENT == 0 || e3 & flags::HUGE != 0 {
            return Err(MapError::NotMapped);
        }
        let pd = frames.table_at(e3 & ADDR_MASK).ok_or(MapError::NotMapped)?;
        // SAFETY: present non-huge PDPT entry → a live PD of this
        // hierarchy.
        let e2 = unsafe { &*pd }[i2];
        if e2 & flags::PRESENT == 0 || e2 & flags::HUGE != 0 {
            return Err(MapError::NotMapped);
        }
        let pt_table = frames.table_at(e2 & ADDR_MASK).ok_or(MapError::NotMapped)?;
        // SAFETY: present non-huge PD entry → a live PT of this hierarchy,
        // and `&mut self` makes the exclusive borrow of the leaf sound.
        let pt = unsafe { &mut *pt_table };
        let e1 = pt[i1];
        if e1 & flags::PRESENT == 0 {
            return Err(MapError::NotMapped);
        }
        pt[i1] = 0;
        Ok(e1 & ADDR_MASK)
    }

    fn root_phys(&self) -> u64 {
        self.pml4_phys
    }

    fn access_tracking(&self) -> tairix_arch_api::mmu::AccessTracking {
        // x86_64 has an unconditional hardware referenced bit: the CPU
        // sets the leaf PTE's Accessed bit (bit 5) on the first access and
        // never clears it itself (Intel SDM Vol 3A §4.8), so a clock scan
        // can read and clear it with no software fault path — unlike the
        // aarch64 / riscv64 ports, whose access flag needs a software
        // access-flag-fault handler on parts that do not update it in the
        // page walk. Supported on every x86_64 CPU TAIRiX targets.
        tairix_arch_api::mmu::AccessTracking::Supported
    }

    fn test_and_clear_accessed(&mut self, vaddr: u64) -> Result<bool, MapError> {
        if (vaddr & (PAGE_SIZE as u64 - 1)) != 0 {
            return Err(MapError::Misaligned);
        }
        let i4 = ((vaddr >> 39) & 0x1FF) as usize;
        let i3 = ((vaddr >> 30) & 0x1FF) as usize;
        let i2 = ((vaddr >> 21) & 0x1FF) as usize;
        let i1 = ((vaddr >> 12) & 0x1FF) as usize;
        // Navigate to the 4 KiB PT leaf without allocating, exactly as
        // `unmap` does. A missing level or a huge-page leaf means there is
        // no 4 KiB leaf whose referenced bit this reports — fail closed
        // with `NotMapped` (the tier tracks only 4 KiB anonymous leaves,
        // never a huge block).
        let frames = self.frames;
        let root = self.root_table().ok_or(MapError::NotMapped)?;
        // SAFETY: `pml4_phys` names this space's live PML4, drawn from
        // `frames`; `&mut self` makes the exclusive borrow sound.
        let e4 = unsafe { &*root }[i4];
        if e4 & flags::PRESENT == 0 {
            return Err(MapError::NotMapped);
        }
        let pdpt = frames.table_at(e4 & ADDR_MASK).ok_or(MapError::NotMapped)?;
        // SAFETY: present entry → a live PDPT of this hierarchy (see
        // `translate`).
        let e3 = unsafe { &*pdpt }[i3];
        if e3 & flags::PRESENT == 0 || e3 & flags::HUGE != 0 {
            return Err(MapError::NotMapped);
        }
        let pd = frames.table_at(e3 & ADDR_MASK).ok_or(MapError::NotMapped)?;
        // SAFETY: present non-huge PDPT entry → a live PD of this
        // hierarchy.
        let e2 = unsafe { &*pd }[i2];
        if e2 & flags::PRESENT == 0 || e2 & flags::HUGE != 0 {
            return Err(MapError::NotMapped);
        }
        let pt_table = frames.table_at(e2 & ADDR_MASK).ok_or(MapError::NotMapped)?;
        // SAFETY: present non-huge PD entry → a live PT of this hierarchy,
        // and `&mut self` makes the exclusive borrow of the leaf sound.
        let pt = unsafe { &mut *pt_table };
        let e1 = pt[i1];
        if e1 & flags::PRESENT == 0 {
            return Err(MapError::NotMapped);
        }
        let was_accessed = e1 & flags::ACCESSED != 0;
        if was_accessed {
            // Clear the Accessed bit so the CPU re-sets it on the next
            // touch; a later probe reading it still clear proves the page
            // went untouched in between (the clock scan).
            pt[i1] = e1 & !flags::ACCESSED;
            // The stale TLB entry may still permit an access without a
            // page-walk (and so without re-setting Accessed), so the
            // cleared bit only becomes observable once the TLB entry is
            // invalidated: flush this page on the current CPU.
            self.flush_page(vaddr);
        }
        Ok(was_accessed)
    }

    unsafe fn reclaim_table_frames(&mut self) {
        // Defence in depth: the dispatcher parks a CPU off a user root at
        // every task suspend, so a dead space's root is never the active
        // translation here — but freeing the walked-from root of a live
        // regime would be catastrophic, so verify and re-park first. With
        // no park root published the frames are retired unreclaimed rather
        // than dismantling the active translation (fail closed). Only the
        // bare-metal target has a `CR3` to compare: a host space is no
        // CPU's active translation.
        #[cfg(all(target_arch = "x86_64", target_os = "none"))]
        if active_root_phys() == self.pml4_phys && !park_kernel_root(true) {
            return;
        }
        let Some(root) = self.root_table() else {
            return;
        };
        // Every kernel-half entry points at tables *every* root shares — the
        // image window, the remap window, the direct map — and the walk
        // below cannot tell them from tables this hierarchy owns: it would
        // free the kernel's own page tables. Drop them from this root first;
        // all are permanent and reached through every other root unchanged.
        //
        // SAFETY: `pml4_phys` names this space's live PML4, drawn from
        // `self.frames`; `&mut self` makes the exclusive borrow sound, and
        // the borrow ends before the reclaim walk below re-reads the root.
        unsafe {
            for slot in (*root).iter_mut().skip(PHYSMAP_PML4_FIRST_SLOT) {
                *slot = 0;
            }
        }
        let frames = self.frames;
        // A four-level hierarchy rooted at the PML4: a present PML4 entry
        // always points at a PDPT; a present PDPT/PD entry without `HUGE`
        // points at the next table; PT (depth 3) entries are page leaves
        // and are never descended into.
        let child_of = |entry: u64, depth: usize| -> Option<u64> {
            (depth < 3
                && (entry & flags::PRESENT) != 0
                && (depth == 0 || (entry & flags::HUGE) == 0))
                .then_some(entry & ADDR_MASK)
        };
        // SAFETY: with the kernel half dropped, every phys `child_of` yields
        // was written by `ensure_child` from a `TableFrame` of
        // `self.frames`, so it names a live table this hierarchy owns and
        // the source can reach; the guard above upholds the not-active
        // contract the caller asserts, and `self` is borrowed mutably so no
        // other reference walks the tables.
        unsafe {
            tairix_arch_api::frames::reclaim_hierarchy(self.pml4_phys, frames, &child_of);
        }
    }

    unsafe fn activate(&self) {
        #[cfg(all(target_arch = "x86_64", target_os = "none"))]
        {
            // SAFETY: forwards to the gated `CR3` load primitive; the
            // caller upholds the `MmuAddressSpace::activate` contract (this
            // space maps the current `rip`/`rsp`), which is exactly
            // `AddressSpace::switch`'s contract.
            unsafe { self.switch() };
        }
        #[cfg(not(all(target_arch = "x86_64", target_os = "none")))]
        {
            unreachable!("CR3 activation is only meaningful on the x86_64 bare-metal target")
        }
    }
}

impl TlbShootdown for AddressSpace {
    fn flush_page(&mut self, vaddr: u64) {
        #[cfg(all(target_arch = "x86_64", target_os = "none"))]
        {
            // SAFETY: `invlpg` invalidates the calling CPU's TLB entry for
            // the page containing the operand address; it touches no
            // memory and only discards a cached translation. No Rust
            // spelling exists.
            unsafe {
                core::arch::asm!(
                    "invlpg [{addr}]",
                    addr = in(reg) vaddr,
                    options(nostack, preserves_flags),
                );
            }
        }
        #[cfg(not(all(target_arch = "x86_64", target_os = "none")))]
        {
            // The host has no TLB to invalidate; a flush is vacuous.
            let _ = vaddr;
        }
    }

    fn publish_mappings(&mut self, start_vaddr: u64, page_count: usize) {
        // Nothing is owed. A not-present paging-structure entry is never
        // cached (Intel SDM Vol 3A, "Caching Translation Information"), so
        // installing a leaf leaves no stale translation to discard, and the
        // store is already ordered ahead of the walk that reads it. The
        // default's per-page `invlpg` sweep would be pure waste.
        let _ = (start_vaddr, page_count);
    }
}

/// Decode an x86_64 leaf PTE's permission bits back into the neutral
/// [`PageFlags`]. Present implies readable; `WRITABLE`/`USER` map
/// directly; executability is the inverse of the `NO_EXECUTE` bit.
fn page_flags_from_pte(pte: u64) -> PageFlags {
    let mut out = PageFlags::READ;
    if pte & flags::WRITABLE != 0 {
        out = out | PageFlags::WRITE;
    }
    if pte & flags::USER != 0 {
        out = out | PageFlags::USER;
    }
    if pte & (flags::CACHE_DISABLE | flags::WRITE_THROUGH)
        == (flags::CACHE_DISABLE | flags::WRITE_THROUGH)
    {
        out = out | PageFlags::DEVICE;
    }
    if pte & flags::NO_EXECUTE == 0 {
        out = out | PageFlags::EXEC;
    }
    if pte & flags::SW_DMA != 0 {
        out = out | PageFlags::DMA;
    }
    out
}

/// 4 KiB-aligned physical address `vaddr` resolves to under a leaf whose
/// region starts at `leaf_base` and spans `1 << region_shift` bytes
/// (30 = 1 GiB PDPT leaf, 21 = 2 MiB PD leaf, 12 = 4 KiB PT leaf).
fn resolved_page(leaf_base: u64, vaddr: u64, region_shift: u32) -> u64 {
    let region_mask = (1u64 << region_shift) - 1;
    (leaf_base + (vaddr & region_mask)) & !((PAGE_SIZE as u64) - 1)
}

// `&mut [u64; 512]` in, `&'static mut [u64; 512]` out: the returned
// reference does not borrow from `parent` (it points at a freshly
// alloc'd table from `frames`, or at a sibling table recovered from the
// same source). `mut_from_ref` / `mut_from_immut` clippy lint
// flags this shape because the function does not return a borrow of
// `parent`'s lifetime — which is exactly the documented contract.
#[allow(clippy::mut_from_ref)]
fn ensure_child(
    parent: &mut [u64; ENTRIES_PER_TABLE],
    idx: usize,
    frames: &'static dyn PageTableFrames,
) -> Option<&'static mut [u64; ENTRIES_PER_TABLE]> {
    let entry = parent[idx];
    if entry & flags::PRESENT != 0 {
        // A present *huge* leaf is a data page, not a table: dereferencing
        // its address as one would let a mapping walk scribble page-table
        // entries over mapped memory. Refuse, so the caller fails closed.
        if entry & flags::HUGE != 0 {
            return None;
        }
        let table = frames.table_at(entry & ADDR_MASK)?;
        // SAFETY: every entry that has PRESENT set was inserted below (or
        // by a root constructor) with a physical address drawn from
        // `frames`, so the source's view of it is a live table of this
        // hierarchy; the walk holds the hierarchy exclusively, so the
        // `&mut` does not alias.
        let child: &'static mut [u64; ENTRIES_PER_TABLE] = unsafe { &mut *table };
        Some(child)
    } else {
        let TableFrame { phys, entries } = frames.alloc_table()?;
        parent[idx] = phys | flags::PRESENT | flags::WRITABLE;
        Some(entries)
    }
}

/// Physical address of the kernel static at virtual address `virt`.
///
/// The page-table pool is a higher-half kernel static (linked at
/// `KERNEL_VMA_BASE + phys`; see `linker.ld` / `boot.s`
/// SAFETY-INVARIANT 9), so its virtual address converts back to the
/// physical address the MMU needs in a page-table entry or CR3 by
/// subtracting the window base. Wrapping, so the host build of
/// [`PageTablePool`] — whose statics live nowhere near the higher half —
/// yields a `phys` its own `table_at` still inverts rather than
/// panicking on an overflow the target can never see.
const fn phys_of(virt: u64) -> u64 {
    virt.wrapping_sub(KERNEL_VMA_BASE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::sync::atomic::AtomicU64;
    use tairix_arch_api::mmu::{self, PageFlags};

    /// A user-half address no root maps until a test does, so the walk
    /// draws fresh tables for it.
    const FINE_VA: u64 = 480u64 << 30;
    const FINE_PA: u64 = 0x4123_4000;

    #[test]
    fn write_back_refuses_what_the_direct_map_cannot_reach() {
        assert!(!write_back(0, 0x1000, 48), "a line size no cache has");
        assert!(!write_back(u64::MAX - 8, 0x1000, 64), "a range that wraps");
        assert!(
            !write_back(physmap_bytes(), 1, 64),
            "the host carries no direct map at all"
        );
    }

    /// A span is reached only whole and only inside the live map. The extent
    /// is process-global, so the reachable case is checked against whatever
    /// it is when the test runs.
    #[test]
    fn a_direct_map_span_is_refused_unless_the_map_holds_it_whole() {
        assert!(physmap_span(0x1000, 0).is_none(), "an empty span");
        assert!(
            !physmap_holds(u64::MAX - 0xFFF, 0x2000),
            "a span that wraps"
        );
        let top = physmap_bytes();
        assert!(!physmap_holds(top, 1), "past the map's extent");
        if let Some(last) = top.checked_sub(1) {
            assert!(!physmap_holds(last, 2), "straddling its end");
            assert!(physmap_holds(last, 1), "its last byte");
        }
    }

    #[test]
    fn page_constants_are_canonical() {
        assert_eq!(PAGE_SIZE, 4096);
        assert_eq!(ENTRIES_PER_TABLE, 512);
        // Intel SDM Vol 3A §4.5 paging-structure flag bit positions.
        assert_eq!(flags::PRESENT, 1 << 0);
        assert_eq!(flags::WRITABLE, 1 << 1);
        assert_eq!(flags::USER, 1 << 2);
        assert_eq!(flags::HUGE, 1 << 7);
        assert_eq!(flags::NO_EXECUTE, 1 << 63);
    }

    /// The pool's `phys_of`/`table_at` pair is the port's whole
    /// physical↔virtual relationship, so the shared suite runs over the
    /// real pool here rather than only on the metal.
    #[test]
    fn passes_frames_conformance() {
        static POOL: PageTablePool = PageTablePool::new();
        tairix_arch_api::frames::conformance::run_all(&POOL, POOL_TABLES);
    }

    #[test]
    fn passes_mmu_conformance() {
        static POOL: PageTablePool = PageTablePool::new();
        let mut space = AddressSpace::new_process_root(&POOL).expect("a root");
        mmu::conformance::run_all(&mut space, FINE_VA, FINE_PA);
    }

    /// The PCID sits in `CR3`'s low twelve bits under the table base, and the
    /// keep bit rides only on a write that asks to keep the PCID's entries.
    #[test]
    fn cr3_carries_the_pcid_under_the_table_base_and_keeps_only_when_asked() {
        assert_eq!(cr3(0x4123_4000, 0, false), 0x4123_4000);
        assert_eq!(cr3(0x4123_4000, 0x7FF, false), 0x4123_47FF);
        assert_eq!(cr3(0x4123_4000, 9, true), 0x4123_4009 | (1 << 63));
        assert_eq!(cr3(0x4123_4000, u16::MAX, false) & !CR3_PCID, 0x4123_4000);
        let widest = u16::try_from(CR3_PCID).expect("twelve bits");
        tairix_arch_api::tags_conformance::run_all(&TAGS, widest, 0x4123_4000, 0x4124_4000);
        assert!(!TAGS.invalidation_reaches_every_tag());
        assert_eq!(TAGS.user_tags(), 0, "no CPU turned PCIDs on on the host");
    }

    /// A parent entry whose address the frame source never handed out is
    /// what a clobbered or hostile table looks like. Every walk must read
    /// it as "nothing mapped here" rather than dereference the address the
    /// integer happens to name.
    #[test]
    fn an_entry_the_source_cannot_reach_fails_the_walk_closed() {
        static POOL: PageTablePool = PageTablePool::new();
        let mut space = AddressSpace::new_process_root(&POOL).expect("a root");
        mmu::AddressSpace::map_page(&mut space, FINE_VA, FINE_PA, PageFlags::READ)
            .expect("map the probe page");
        // A page-aligned table the pool never handed out, holding a
        // present huge leaf at the index the walk would read next.
        // Recovering a table by dereferencing its address — what the walk
        // did before it asked the frame source — would read this and
        // answer with a mapping; asking the source refuses the address
        // outright.
        let mut foreign = Table::new();
        foreign.0[((FINE_VA >> 21) & 0x1FF) as usize] =
            FINE_PA | flags::PRESENT | flags::WRITABLE | flags::HUGE;
        let foreign_phys = phys_of(foreign.0.as_ptr() as u64);

        // Overwrite the PDPT entry covering `FINE_VA` to point at it,
        // present and non-huge so the walk would follow it.
        let root = POOL
            .table_at(space.root_phys())
            .expect("the pool's own root");
        // SAFETY: this space's live PML4 from the process-static pool,
        // exclusively owned here; the PDPT it names is the same pool's.
        let pdpt_phys = unsafe { (*root)[((FINE_VA >> 39) & 0x1FF) as usize] } & ADDR_MASK;
        let pdpt = POOL.table_at(pdpt_phys).expect("the pool's own PDPT");
        // SAFETY: as above.
        unsafe {
            (*pdpt)[((FINE_VA >> 30) & 0x1FF) as usize] =
                foreign_phys | flags::PRESENT | flags::WRITABLE;
        }

        assert_eq!(mmu::AddressSpace::translate(&space, FINE_VA), None);
        assert_eq!(
            mmu::AddressSpace::unmap(&mut space, FINE_VA),
            Err(MapError::NotMapped)
        );
        assert_eq!(
            mmu::AddressSpace::test_and_clear_accessed(&mut space, FINE_VA),
            Err(MapError::NotMapped)
        );
        // And a fresh map over the unreachable branch is refused rather
        // than walked into: `leaf_present` reads it as absent, then
        // `ensure_child` refuses the entry it cannot recover.
        assert_eq!(
            mmu::AddressSpace::map_page(&mut space, FINE_VA, FINE_PA, PageFlags::READ),
            Err(MapError::PoolExhausted)
        );
    }

    /// A recording [`PageTableFrames`] double: a bump pool over `'static`
    /// slots plus a log of every `free_table` return, so teardown can be
    /// asserted to hand back exactly the frames the hierarchy drew.
    struct RecordingFrames {
        storage: [UnsafeCell<Table>; Self::CAPACITY],
        used: AtomicUsize,
        freed: [AtomicU64; Self::CAPACITY],
        freed_len: AtomicUsize,
    }

    // SAFETY: each slot is handed out exactly once via the monotonic
    // `used` counter, so the `&'static mut` views never alias; the freed
    // log is plain atomics.
    unsafe impl Sync for RecordingFrames {}

    impl RecordingFrames {
        const CAPACITY: usize = 16;

        const fn new() -> Self {
            // The array initialisers need a `const`, and copying it per
            // slot is the point: each element is its own cell.
            #[allow(clippy::declare_interior_mutable_const)]
            const ZERO: UnsafeCell<Table> = UnsafeCell::new(Table::new());
            #[allow(clippy::declare_interior_mutable_const)]
            const FREED: AtomicU64 = AtomicU64::new(0);
            // `const`, so the pool lands in the `static` it initialises
            // rather than a runtime stack frame, despite the
            // `large_stack_arrays` heuristic.
            #[allow(clippy::large_stack_arrays)]
            Self {
                storage: [ZERO; Self::CAPACITY],
                used: AtomicUsize::new(0),
                freed: [FREED; Self::CAPACITY],
                freed_len: AtomicUsize::new(0),
            }
        }

        fn freed_phys(&self) -> impl Iterator<Item = u64> + '_ {
            self.freed
                .iter()
                .take(self.freed_len.load(Ordering::SeqCst))
                .map(|slot| slot.load(Ordering::SeqCst))
        }
    }

    impl PageTableFrames for RecordingFrames {
        fn alloc_table(&self) -> Option<TableFrame> {
            let idx = self.used.fetch_add(1, Ordering::SeqCst);
            if idx >= Self::CAPACITY {
                self.used.store(Self::CAPACITY, Ordering::SeqCst);
                return None;
            }
            // SAFETY: the monotonic index makes this slot exclusively ours.
            let table: &'static mut Table = unsafe { &mut *self.storage[idx].get() };
            let entries = &mut table.0;
            let phys = phys_of(entries.as_ptr() as u64);
            Some(TableFrame { phys, entries })
        }

        fn table_at(&self, phys: u64) -> Option<*mut [u64; ENTRIES_PER_TABLE]> {
            let base = phys_of(self.storage.as_ptr() as u64);
            let index = pool_slot_of(base, Self::CAPACITY, phys)?;
            Some(self.storage[index].get().cast())
        }

        fn free_table(&self, phys: u64) {
            let slot = self.freed_len.fetch_add(1, Ordering::SeqCst);
            assert!(slot < Self::CAPACITY, "more frees than the pool can hold");
            self.freed[slot].store(phys, Ordering::SeqCst);
        }
    }

    /// Teardown hands back exactly the tables the four-level hierarchy
    /// drew — the root last, each once — and never a leaf frame.
    #[test]
    fn reclaim_table_frames_returns_every_drawn_table_exactly_once() {
        static POOL: RecordingFrames = RecordingFrames::new();
        let mut space = AddressSpace::new_process_root(&POOL).expect("a root");
        let root_phys = space.root_phys();
        let drawn = POOL.used.load(Ordering::SeqCst);
        // Two pages a gigabyte apart, so the walk draws an independent
        // PD/PT pair for each, both under the one PDPT slot 0 draws: a PML4
        // slot spans 512 GiB.
        mmu::AddressSpace::map_page(&mut space, FINE_VA, FINE_PA, PageFlags::READ).expect("map A");
        mmu::AddressSpace::map_page(
            &mut space,
            FINE_VA + (1u64 << 30),
            FINE_PA + PAGE_SIZE as u64,
            PageFlags::READ,
        )
        .expect("map B");
        let total = POOL.used.load(Ordering::SeqCst);
        assert_eq!(total, drawn + 5, "a PDPT, then a PD/PT pair per page");

        // SAFETY: a host space is no CPU's active translation, and no
        // other reference into its tables is live.
        unsafe { mmu::AddressSpace::reclaim_table_frames(&mut space) };

        let mut count = 0usize;
        for phys in POOL.freed_phys() {
            assert!(
                POOL.freed_phys().take(count).all(|seen| seen != phys),
                "no table is freed twice"
            );
            count += 1;
        }
        assert_eq!(count, total, "every drawn table was returned");
        assert_eq!(
            POOL.freed_phys().last(),
            Some(root_phys),
            "the root is freed last"
        );
        assert!(
            POOL.freed_phys().all(|phys| phys != FINE_PA),
            "a leaf frame is never freed"
        );
    }

    #[test]
    fn the_direct_map_claims_the_slots_between_user_space_and_the_remap_window() {
        // 1 << 47 is the port's user-VA ceiling, and the map starts at the
        // slot it lands on: the two share no slot, so a process root can
        // carry the whole of RAM without carrying it in the half a user
        // program addresses.
        assert_eq!((1u64 << 47) >> 39, PHYSMAP_PML4_FIRST_SLOT as u64);
        assert_eq!(PHYSMAP_VMA_BASE, 0xFFFF_8000_0000_0000);
        assert_eq!(PHYSMAP_PML4_SLOTS, 254);
        // 254 slots at 512 GiB is 127 TiB, so the reach is bounded by the
        // architecture rather than by where user space begins — which is
        // the whole point of moving the map out of the low half.
        assert_eq!(MAX_PHYSMAP_GIB, 254 * 512);
        assert!(is_kernel_half_slot(PHYSMAP_PML4_FIRST_SLOT));
        assert!(!is_kernel_half_slot(PHYSMAP_PML4_FIRST_SLOT - 1));
        assert!(is_kernel_half_slot(KERNEL_WINDOW_PML4_SLOT));
    }

    /// The widening draws only what the boot trampoline does not already
    /// supply: its span's PDPT and the leaves below the floor are free.
    #[test]
    fn physmap_table_frames_counts_what_the_trampoline_does_not_supply() {
        // The host build reports no 1 GiB pages, so each gigabyte above the
        // floor also costs its own page directory.
        assert!(!gigapages_supported());
        let floor = PHYSMAP_FLOOR_GIB;
        assert_eq!(physmap_table_frames(floor), 0);
        assert_eq!(physmap_table_frames(floor + 1), 1);
        assert_eq!(
            physmap_table_frames(ENTRIES_PER_TABLE),
            ENTRIES_PER_TABLE - floor,
            "one span, a directory per gigabyte above the floor"
        );
        assert_eq!(
            physmap_table_frames(ENTRIES_PER_TABLE + 1),
            1 + (ENTRIES_PER_TABLE + 1 - floor),
            "a second span costs its own PDPT"
        );
    }

    /// A process root carries the shared kernel half and an empty user half,
    /// so no address of it maps anything physical as itself, NULL included.
    #[test]
    fn a_process_root_maps_nothing_physical_as_itself() {
        static POOL: PageTablePool = PageTablePool::new();
        let process = AddressSpace::new_process_root(&POOL).expect("a process root");
        for probe in [
            0,
            crate::smp::AP_TRAMPOLINE_PHYS,
            0x0020_0000,
            BOOT_PHYSMAP_END - PAGE_SIZE as u64,
        ] {
            assert!(
                mmu::AddressSpace::translate(&process, probe).is_none(),
                "{probe:#x} is mapped as itself"
            );
        }
        let root = POOL
            .table_at(process.root_phys())
            .expect("the pool's own root");
        // SAFETY: this space's PML4 from the test's own pool, only read.
        let root = unsafe { &*root };
        assert!(
            root.iter()
                .take(PHYSMAP_PML4_FIRST_SLOT)
                .all(|entry| *entry == 0),
            "the user half starts empty"
        );
    }

    /// The CPU sets the accessed bit in every entry it walks, so a live entry
    /// is matched by the table it names, never bit for bit.
    #[test]
    fn the_floor_check_ignores_the_accessed_bit() {
        let floor = 0x0010_5000 | KERNEL_TABLE_ENTRY;
        assert!(names_same_table(floor, floor));
        assert!(names_same_table(floor | flags::ACCESSED, floor));
        assert!(!names_same_table(floor & !flags::PRESENT, floor));
        assert!(
            !names_same_table(floor | flags::HUGE, floor),
            "a leaf names no table"
        );
        assert!(!names_same_table(floor + PAGE_SIZE as u64, floor));
    }

    /// Every x86 DMA master snoops, so memory a non-snooping one would share
    /// is never asked for, and mapping it uncached beside the direct map's
    /// write-back alias is refused rather than done.
    #[test]
    fn memory_for_a_master_that_does_not_snoop_is_refused() {
        use tairix_arch_api::mmu::{self, PageFlags};
        static POOL: PageTablePool = PageTablePool::new();
        let mut space = AddressSpace::new_process_root(&POOL).expect("a process root");
        let shared = PageFlags::READ | PageFlags::WRITE | PageFlags::USER;
        assert_eq!(
            mmu::AddressSpace::map_page(
                &mut space,
                FINE_VA,
                FINE_PA,
                shared | PageFlags::DMA_COHERENT
            ),
            Err(MapError::Unsupported)
        );
        assert_eq!(
            mmu::AddressSpace::map_page(&mut space, FINE_VA, FINE_PA, shared),
            Ok(())
        );
    }

    /// A DMA buffer's mark rides a software bit the walk ignores and decodes
    /// back; a leaf without it carries none.
    #[test]
    fn the_dma_mark_round_trips_through_a_leaf() {
        static POOL: PageTablePool = PageTablePool::new();
        let mut space = AddressSpace::new_process_root(&POOL).expect("a process root");
        let shared = PageFlags::READ | PageFlags::WRITE | PageFlags::USER;
        mmu::AddressSpace::map_page(&mut space, FINE_VA, FINE_PA, shared | PageFlags::DMA)
            .expect("maps");
        let (_, flags) = mmu::AddressSpace::translate(&space, FINE_VA).expect("mapped");
        assert!(flags.contains(shared | PageFlags::DMA));
        let plain = FINE_VA + PAGE_SIZE as u64;
        mmu::AddressSpace::map_page(&mut space, plain, FINE_PA, shared).expect("maps");
        let (_, flags) = mmu::AddressSpace::translate(&space, plain).expect("mapped");
        assert!(!flags.contains(PageFlags::DMA));
    }

    /// A user leaf in a kernel-half slot would hand ring 3 the direct map,
    /// the kernel heap's remap window, or the kernel image. The walk
    /// refuses it whatever the caller computed.
    #[test]
    fn a_user_mapping_is_refused_in_the_kernel_half() {
        static POOL: PageTablePool = PageTablePool::new();
        let mut space = AddressSpace::new_process_root(&POOL).expect("a process root");
        for slot in [
            PHYSMAP_PML4_FIRST_SLOT,
            PHYSMAP_PML4_FIRST_SLOT + PHYSMAP_PML4_SLOTS - 1,
            KERNEL_WINDOW_PML4_SLOT,
        ] {
            assert!(
                space
                    .map_4k_user_wx(&POOL, canonical_slot_base(slot), FINE_PA, true, false)
                    .is_none(),
                "slot {slot} is the kernel's"
            );
        }
        // Not a blanket refusal: the user half still maps.
        assert!(space
            .map_4k_user_wx(&POOL, FINE_VA, FINE_PA, true, false)
            .is_some());
    }

    #[test]
    fn publish_physmap_refuses_an_extent_its_entries_do_not_cover() {
        let none = [0u64; PHYSMAP_PML4_SLOTS];
        assert!(!publish_physmap(&none, 0), "an empty map covers nothing");
        assert!(
            !publish_physmap(&none, MAX_PHYSMAP_GIB + 1),
            "wider than the claimed slots can express"
        );
        assert!(
            !publish_physmap(&none, 1),
            "a span with no entry would leave a hole the map claims to cover"
        );
        let mut extra = [0u64; PHYSMAP_PML4_SLOTS];
        extra[0] = flags::PRESENT | flags::WRITABLE;
        extra[1] = flags::PRESENT | flags::WRITABLE;
        assert!(
            !publish_physmap(&extra, 1),
            "a spare entry would leave a slot nothing backs"
        );
    }

    /// The one test that drives the set-once publications, because they are
    /// process-global: a second caller is refused by design. Every other test
    /// here is insensitive to them — a root's kernel-half slots name tables
    /// this pool owns and the reclaim walk drops them before it descends, so
    /// neither the pool accounting nor a walk of the user half changes.
    #[test]
    fn every_root_installs_the_published_kernel_half() {
        static POOL: PageTablePool = PageTablePool::new();
        const GIB: usize = 4;

        // One shared PDPT, as the boot carve builds it: 1 GiB leaves
        // covering `[0, GIB GiB)`, reached from the map's first slot.
        let TableFrame {
            phys: pdpt_phys,
            entries: pdpt,
        } = POOL.alloc_table().expect("a PDPT");
        for (gib, slot) in pdpt.iter_mut().take(GIB).enumerate() {
            *slot = ((gib as u64) << 30) | KernelAccess::Write.leaf() | flags::HUGE;
        }
        let mut entries = [0u64; PHYSMAP_PML4_SLOTS];
        entries[0] = pdpt_phys | KERNEL_TABLE_ENTRY;
        assert!(publish_physmap(&entries, GIB), "the first publication");
        assert_eq!(physmap_gigapages(), GIB);
        assert_eq!(physmap_bytes(), (GIB as u64) << 30);
        assert!(
            !publish_physmap(&entries, GIB),
            "the map is installed once per boot"
        );
        let TableFrame { phys: image, .. } = POOL.alloc_table().expect("an image PDPT");
        KERNEL_IMAGE_PML4.store(image | KERNEL_TABLE_ENTRY, Ordering::Release);

        let probe = 0x1234_5000u64;
        let space = AddressSpace::new_process_root(&POOL).expect("a process root");
        let (phys, rights) =
            mmu::AddressSpace::translate(&space, physmap_virt(probe)).expect("in the direct map");
        assert_eq!(
            phys, probe,
            "every root reaches a frame through the direct map"
        );
        assert!(
            !rights.contains(PageFlags::EXEC),
            "nothing runs from the direct map"
        );
        // Past the published extent nothing is mapped, so a frame the map
        // does not cover faults rather than reading a neighbour's.
        assert!(
            mmu::AddressSpace::translate(&space, physmap_virt(physmap_bytes())).is_none(),
            "the map stops at its published extent"
        );
        let root = POOL
            .table_at(space.root_phys())
            .expect("the pool's own root");
        // SAFETY: this space's PML4 from the test's own pool, only read.
        let root = unsafe { &*root };
        assert_eq!(root[KERNEL_IMAGE_PML4_SLOT], image | KERNEL_TABLE_ENTRY);
        assert_eq!(root[PHYSMAP_PML4_FIRST_SLOT], entries[0]);
    }
}
