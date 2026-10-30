//! The kernel's own translation root, and the image window every root
//! shares.
//!
//! The image window maps each section of the kernel image with only the
//! access its contents need, and nothing else of the gigabyte the image lies
//! in. The direct map's alias of the image is narrowed to match, so the code
//! and read-only data can be written through neither mapping. Both are built
//! from statics before the frame allocator exists, with a 2 MiB leaf wherever
//! one access covers a whole block.

use tairix_arch_api::frames::{PageTableFrames, TableFrame};

use super::{
    flags, is_kernel_half_slot, KernelAccess, ENTRIES_PER_TABLE, KERNEL_IMAGE_PML4_SLOT,
    KERNEL_TABLE_ENTRY, KERNEL_VMA_BASE, PAGES_PER_FINE_CHAIN, PAGE_SIZE,
};

#[cfg(all(target_arch = "x86_64", target_os = "none"))]
use core::sync::atomic::{AtomicU64, Ordering};

#[cfg(all(target_arch = "x86_64", target_os = "none"))]
use super::{
    active_root_phys, boot_physmap_entry, install_kernel_half, mapped_table, names_same_table,
    park_kernel_root, PageTablePool, ADDR_MASK, KERNEL_IMAGE_PML4, PARK_ROOT, PHYSMAP_PML4,
    PHYSMAP_PML4_FIRST_SLOT,
};

/// PDPT slot of the gigabyte the image lies in: virtual `KERNEL_VMA_BASE + p`
/// is physical `p`, for `p` below [`IMAGE_SPAN`].
const IMAGE_PDPT_SLOT: usize = ((KERNEL_VMA_BASE >> 30) & 0x1FF) as usize;

/// Physical bytes the image window maps: the one gigabyte its directory
/// covers, which is all the boot trampoline's own window covers too
/// (`linker.ld` refuses a larger image).
const IMAGE_SPAN: u64 = 1 << 30;

/// Bytes one 2 MiB leaf maps.
const BLOCK: u64 = 1 << 21;

const _: () = assert!(ENTRIES_PER_TABLE as u64 * BLOCK == IMAGE_SPAN);

/// Sections the image window maps.
const IMAGE_SECTIONS: usize = 4;

/// Page tables `runs` runs can need: a run shares a 2 MiB block with
/// anything else only at its two ends, and only such a block needs a table
/// of its own.
const fn split_blocks(runs: usize) -> usize {
    2 * runs
}

/// Tables the kernel root is built from: its PML4; the image window's PDPT,
/// its directory and the tables its sections split; and the direct map's
/// first span — a PDPT, the directory over the first GiB, and the tables
/// its read-only alias of the image splits.
const KERNEL_ROOT_TABLES: usize = 1 + 2 + split_blocks(IMAGE_SECTIONS) + 2 + split_blocks(1);

/// Tables the start-up root is built from: its PML4 and the chain to its one
/// identity page.
const STARTUP_ROOT_TABLES: usize = 1 + PAGES_PER_FINE_CHAIN;

/// Tables both roots are built from, which live as long as the kernel.
const ROOT_TABLES: usize = KERNEL_ROOT_TABLES + STARTUP_ROOT_TABLES;

/// A physical span `[start, end)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Span {
    start: u64,
    end: u64,
}

impl Span {
    const fn contains(self, at: u64) -> bool {
        self.start <= at && at < self.end
    }

    /// Whether an end of the span falls strictly inside `[base, base + len)`,
    /// so the span covers only part of it.
    fn splits(self, base: u64, len: u64) -> bool {
        let inside = |edge: u64| base < edge && edge < base + len;
        inside(self.start) || inside(self.end)
    }
}

/// Pages and the access the kernel has to them.
#[derive(Clone, Copy, Debug)]
struct Run {
    span: Span,
    access: KernelAccess,
}

/// The kernel image's sections, by physical span, as the linker lays them
/// out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ImageSections {
    /// The boot trampoline's data — its page tables, the boot stack and its
    /// guard, the boot CPU's descriptor tables — which the image window
    /// reaches by alias.
    boot_data: Span,
    /// The kernel's code.
    text: Span,
    /// Read-only data, the source bytes of the AP start-up payload among it.
    read_only: Span,
    /// Writable data and `.bss`.
    data: Span,
}

impl ImageSections {
    /// The sections, or [`None`] unless each is a non-empty run of whole
    /// pages, they follow one another in link order, and the last ends
    /// within [`IMAGE_SPAN`].
    fn new(boot_data: Span, text: Span, read_only: Span, data: Span) -> Option<Self> {
        let sections = Self {
            boot_data,
            text,
            read_only,
            data,
        };
        let spans = sections.spans();
        let page = PAGE_SIZE as u64;
        let well_formed = spans.iter().all(|span| {
            span.start < span.end
                && span.start.is_multiple_of(page)
                && span.end.is_multiple_of(page)
        }) && spans.windows(2).all(|pair| pair[0].end <= pair[1].start)
            && data.end <= IMAGE_SPAN;
        well_formed.then_some(sections)
    }

    /// The sections as `linker.ld` lays them out in this image.
    #[cfg(all(target_arch = "x86_64", target_os = "none"))]
    fn linked() -> Option<Self> {
        // Absolute symbols `linker.ld` defines as physical addresses; only
        // their values are read.
        extern "C" {
            static __image_boot_data_start: u8;
            static __image_boot_data_end: u8;
            static __image_text_start: u8;
            static __image_text_end: u8;
            static __image_rodata_start: u8;
            static __image_rodata_end: u8;
            static __image_data_start: u8;
            static __image_data_end: u8;
        }
        let span = |start: *const u8, end: *const u8| Span {
            start: start as u64,
            end: end as u64,
        };
        Self::new(
            span(
                core::ptr::addr_of!(__image_boot_data_start),
                core::ptr::addr_of!(__image_boot_data_end),
            ),
            span(
                core::ptr::addr_of!(__image_text_start),
                core::ptr::addr_of!(__image_text_end),
            ),
            span(
                core::ptr::addr_of!(__image_rodata_start),
                core::ptr::addr_of!(__image_rodata_end),
            ),
            span(
                core::ptr::addr_of!(__image_data_start),
                core::ptr::addr_of!(__image_data_end),
            ),
        )
    }

    const fn spans(&self) -> [Span; IMAGE_SECTIONS] {
        [self.boot_data, self.text, self.read_only, self.data]
    }

    /// How the image window maps each section.
    const fn window_runs(&self) -> [Run; IMAGE_SECTIONS] {
        [
            Run {
                span: self.boot_data,
                access: KernelAccess::Write,
            },
            Run {
                span: self.text,
                access: KernelAccess::Execute,
            },
            Run {
                span: self.read_only,
                access: KernelAccess::Read,
            },
            Run {
                span: self.data,
                access: KernelAccess::Write,
            },
        ]
    }

    /// What the direct map's alias of the image may grant: no more than
    /// reading the code and the read-only data.
    const fn alias_run(&self) -> Run {
        Run {
            span: Span {
                start: self.text.start,
                end: self.read_only.end,
            },
            access: KernelAccess::Read,
        }
    }
}

/// Fill `directory`, which maps physical `[0, IMAGE_SPAN)`, so each page has
/// the access of the run holding it, or `outside` where none does (nothing,
/// for [`None`]).
///
/// A block one access covers whole is a 2 MiB leaf; only a block a run ends
/// inside costs a page table from `frames`. [`None`] when `frames` runs out,
/// with the directory part-written.
fn map_first_gigabyte(
    directory: &mut [u64; ENTRIES_PER_TABLE],
    runs: &[Run],
    outside: Option<KernelAccess>,
    frames: &dyn PageTableFrames,
) -> Option<()> {
    let access_at = |at: u64| {
        runs.iter()
            .find(|run| run.span.contains(at))
            .map(|run| run.access)
            .or(outside)
    };
    for (index, entry) in directory.iter_mut().enumerate() {
        let block = index as u64 * BLOCK;
        *entry = if runs.iter().any(|run| run.span.splits(block, BLOCK)) {
            let TableFrame { phys, entries } = frames.alloc_table()?;
            for (page_index, leaf) in entries.iter_mut().enumerate() {
                let page = block + page_index as u64 * PAGE_SIZE as u64;
                *leaf = access_at(page).map_or(0, |access| page | access.leaf());
            }
            phys | KERNEL_TABLE_ENTRY
        } else {
            access_at(block).map_or(0, |access| block | access.leaf() | flags::HUGE)
        };
    }
    Some(())
}

/// Build the image window from `frames`: a PDPT whose one directory maps
/// each of `sections` as [`ImageSections::window_runs`] says and nothing
/// else. Returns the PML4 entry that reaches it.
fn build_image_window(sections: &ImageSections, frames: &dyn PageTableFrames) -> Option<u64> {
    let TableFrame {
        phys: pdpt_phys,
        entries: pdpt,
    } = frames.alloc_table()?;
    let TableFrame {
        phys: directory_phys,
        entries: directory,
    } = frames.alloc_table()?;
    map_first_gigabyte(directory, &sections.window_runs(), None, frames)?;
    pdpt[IMAGE_PDPT_SLOT] = directory_phys | KERNEL_TABLE_ENTRY;
    Some(pdpt_phys | KERNEL_TABLE_ENTRY)
}

/// Build the direct map's first span from `frames`: a copy of the boot
/// trampoline's `floor` PDPT whose directory over the first GiB is drawn
/// afresh, read-write and non-executable but for the image's code and
/// read-only data, which it maps read only. Returns the PML4 entry that
/// reaches it.
///
/// A copy, never an edit of the floor: narrowing a live 2 MiB leaf would
/// leave the TLB free to hold both page sizes for one address with different
/// rights, which the architecture leaves undefined. The kernel root carries
/// the copy from its first load, and the floor's other directories are
/// shared unchanged.
fn build_first_span(
    floor: &[u64; ENTRIES_PER_TABLE],
    sections: &ImageSections,
    frames: &dyn PageTableFrames,
) -> Option<u64> {
    let TableFrame {
        phys: pdpt_phys,
        entries: pdpt,
    } = frames.alloc_table()?;
    let TableFrame {
        phys: directory_phys,
        entries: directory,
    } = frames.alloc_table()?;
    map_first_gigabyte(
        directory,
        &[sections.alias_run()],
        Some(KernelAccess::Write),
        frames,
    )?;
    pdpt.copy_from_slice(floor);
    pdpt[0] = directory_phys | KERNEL_TABLE_ENTRY;
    Some(pdpt_phys | KERNEL_TABLE_ENTRY)
}

/// The index `address` selects in the table whose entries each map
/// `1 << shift` bytes.
const fn table_index(address: u64, shift: u32) -> usize {
    ((address >> shift) & 0x1FF) as usize
}

/// Build a start-up root from `frames`: the image window `image`, and `page`
/// mapped as itself, read and execute only — what an application processor
/// runs before it can load the kernel root. Returns the root's physical
/// address, or [`None`] for a `page` that is not a page of the user half or
/// when `frames` runs out.
fn build_startup_root(image: u64, page: u64, frames: &dyn PageTableFrames) -> Option<u64> {
    if !page.is_multiple_of(PAGE_SIZE as u64) || is_kernel_half_slot(table_index(page, 39)) {
        return None;
    }
    let TableFrame {
        phys: root,
        entries: pml4,
    } = frames.alloc_table()?;
    let TableFrame {
        phys: pdpt_phys,
        entries: pdpt,
    } = frames.alloc_table()?;
    let TableFrame {
        phys: directory_phys,
        entries: directory,
    } = frames.alloc_table()?;
    let TableFrame {
        phys: table_phys,
        entries: table,
    } = frames.alloc_table()?;
    table[table_index(page, 12)] = page | KernelAccess::Execute.leaf();
    directory[table_index(page, 21)] = table_phys | KERNEL_TABLE_ENTRY;
    pdpt[table_index(page, 30)] = directory_phys | KERNEL_TABLE_ENTRY;
    pml4[table_index(page, 39)] = pdpt_phys | KERNEL_TABLE_ENTRY;
    pml4[KERNEL_IMAGE_PML4_SLOT] = image;
    Some(root)
}

/// The tables of the kernel root and the start-up root.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
static ROOT_TABLE_POOL: PageTablePool<ROOT_TABLES> = PageTablePool::new();

/// The start-up root's physical address, or `0` before it is built.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
static STARTUP_ROOT: AtomicU64 = AtomicU64::new(0);

/// The start-up root an application processor loads first, or [`None`]
/// before the kernel root is built: the kernel's image window and the one
/// page of the AP trampoline, mapped as itself.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub(crate) fn startup_root() -> Option<u64> {
    match STARTUP_ROOT.load(Ordering::Acquire) {
        0 => None,
        root => Some(root),
    }
}

/// Build the kernel root, settle the protections every CPU runs the kernel
/// under ([`crate::protection`]), and move the boot CPU onto the root under
/// them.
///
/// The root carries the kernel half every root shares and nothing else: no
/// user half, and nothing physical mapped as itself. Building it also builds
/// the image window and the direct map's first span every later root
/// installs, and the start-up root an application processor first runs on.
/// Every table is fresh: nothing the trampoline's root runs on is edited, so
/// the one `CR3` load that moves the CPU across leaves no stale translation.
///
/// # Errors
///
/// Why the boot cannot go on: the image's sections are not ones its window
/// maps, the floor is not the trampoline's, the tables reserved for the root
/// ran out, or a root was built already.
///
/// # Safety
///
/// Once, on the boot CPU, with interrupts disabled and `RFLAGS.AC` clear, on
/// the boot trampoline's root, after the boot CPU's descriptor tables are
/// installed: the trampoline's identity window and the GDT inside it are left
/// behind here.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub(crate) unsafe fn enter_kernel_root() -> Result<(), &'static str> {
    const EXHAUSTED: &str = "the kernel root's page tables ran out";
    const FOREIGN_FLOOR: &str = "the direct map's floor is not the boot trampoline's";
    if PARK_ROOT.published().is_some() || PHYSMAP_PML4[0].load(Ordering::Acquire) != 0 {
        return Err("a translation root was built before the kernel root");
    }
    let sections = ImageSections::linked()
        .ok_or("the kernel image's sections are not ones its window maps")?;
    let image = build_image_window(&sections, &ROOT_TABLE_POOL).ok_or(EXHAUSTED)?;

    // The trampoline writes its floor's slot by a byte offset into its root;
    // if that ever disagreed with the slot constant, the copy would be of
    // something else. Check, do not assume.
    let floor = boot_physmap_entry();
    // SAFETY: the live root and the floor's PDPT are the trampoline's
    // tables, which nothing but this boot CPU reaches by pointer before the
    // kernel proper runs; both are only read.
    let trampoline_root = unsafe { mapped_table(active_root_phys()) }
        .ok_or("the boot trampoline's root is outside the direct map")?;
    if !names_same_table(trampoline_root[PHYSMAP_PML4_FIRST_SLOT], floor) {
        return Err(FOREIGN_FLOOR);
    }
    // SAFETY: as above.
    let floor = unsafe { mapped_table(floor & ADDR_MASK) }
        .ok_or("the direct map's floor is outside itself")?;
    let first_span = build_first_span(floor, &sections, &ROOT_TABLE_POOL).ok_or(EXHAUSTED)?;

    PHYSMAP_PML4[0].store(first_span, Ordering::Release);
    KERNEL_IMAGE_PML4.store(image, Ordering::Release);
    let TableFrame {
        phys: root,
        entries: pml4,
    } = ROOT_TABLE_POOL.alloc_table().ok_or(EXHAUSTED)?;
    install_kernel_half(pml4);
    let startup = build_startup_root(image, crate::smp::AP_TRAMPOLINE_PHYS, &ROOT_TABLE_POOL)
        .ok_or(EXHAUSTED)?;
    STARTUP_ROOT.store(startup, Ordering::Release);
    crate::protection::settle();
    PARK_ROOT.publish(root);
    // SAFETY: the caller's contract; the kernel root maps this code and the
    // boot stack through its image window as the trampoline's root does.
    if unsafe { join_kernel_root() } {
        Ok(())
    } else {
        Err("the kernel root was not published")
    }
}

/// Load the kernel root on the calling CPU and hold the CPU to the kernel's
/// protections ([`crate::protection`]). Returns `false`, changing nothing,
/// before the root is built or on a CPU that does not offer every protection
/// in force.
///
/// # Safety
///
/// CPL 0 with interrupts disabled and `RFLAGS.AC` clear, on a root that maps
/// the calling code and stack as the kernel root does — the boot trampoline's
/// root and the start-up root both do.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub(crate) unsafe fn join_kernel_root() -> bool {
    let Some(protections) = crate::protection::to_hold() else {
        return false;
    };
    if !park_kernel_root(true) {
        return false;
    }
    // SAFETY: the caller's contract, on the kernel root, which maps no user
    // page.
    unsafe { crate::protection::hold(protections) };
    true
}

#[cfg(test)]
#[path = "kernel_root_tests.rs"]
mod tests;
