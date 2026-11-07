//! The kernel root's image half: the image in the top gigabyte of the kernel
//! regime, each section mapped with only the access it needs; the direct
//! map's alias of the image's gigabyte, narrowed so the code and read-only
//! data can be written through neither mapping; and a read-only window onto
//! the firmware's device tree.
//!
//! Built from statics by the boot CPU's port entry while the trampoline's
//! early map still runs it, then loaded in that map's place
//! ([`enter_kernel_root`]): a root switch, so no live entry ever changes and
//! no translation changes its page size under a TLB.

#[cfg(all(target_arch = "aarch64", target_os = "none"))]
use core::sync::atomic::Ordering;

use tairix_arch_api::frames::{PageTableFrames, TableFrame};
use tairix_arch_api::imagemap::{
    in_page_order, map_gigabyte, split_blocks, ImageLeaves, Run, Span, BLOCK, GIGABYTE,
};

use super::{descriptor, table_descriptor, KernelAccess, ENTRIES_PER_TABLE, PAGE_SIZE};

#[cfg(all(target_arch = "aarch64", target_os = "none"))]
use super::{
    empty_root_phys, image_phys, kernel_root_entries, kernel_root_phys, park_kernel_root,
    publish_firmware_tree, reveal, FirmwareTree, Holding, PageTablePool, GIGAPAGE_MASK_WORDS,
    IMAGE_SLOT, KERNEL_IMAGE_GIGABYTE, MAX_PHYSMAP_GIB, PARK_ROOT, PHYSMAP_FIRST_SLOT,
};

/// Sections the image window maps: the code, the read-only data, the data,
/// the boot stack's guard, the boot stack, the trampoline's tables and the
/// boot heap.
const IMAGE_SECTIONS: usize = 7;

/// Runs the direct map's alias of the image narrows: the code with the
/// read-only data, and the trampoline's tables.
const ALIAS_RUNS: usize = 2;

/// Blocks of the image's gigabyte the firmware tree's window spans: a tree is
/// at most [`MAX_TREE_BYTES`], wherever in a page it starts.
const TREE_WINDOW_BLOCKS: usize = 2;

/// First block of the image's gigabyte the tree's window takes: the last two
/// below the top of the regime, whose exclusive end is not representable.
const TREE_WINDOW_FIRST_BLOCK: usize = ENTRIES_PER_TABLE - 1 - TREE_WINDOW_BLOCKS;

// The linker refuses an image reaching the tree's window.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
core::arch::global_asm!(
    ".globl kernel_image_reach",
    ".set kernel_image_reach, {reach}",
    reach = const TREE_WINDOW_FIRST_BLOCK as u64 * BLOCK,
);

/// The most bytes a firmware tree may span: the Linux aarch64 boot
/// protocol's bound, which the window is sized to.
const MAX_TREE_BYTES: u64 = 2 << 20;

/// The fewest: a tree's header.
const MIN_TREE_BYTES: u64 = 40;

const _: () = assert!(
    MAX_TREE_BYTES + PAGE_SIZE as u64 <= TREE_WINDOW_BLOCKS as u64 * BLOCK,
    "the window holds the largest tree at any page offset"
);

/// Tables the image half is built from: the image's L2 and the tables its
/// sections split, the tree window's page tables, and the alias's L2 and the
/// tables its runs split.
const KERNEL_ROOT_TABLES: usize =
    1 + split_blocks(IMAGE_SECTIONS) + TREE_WINDOW_BLOCKS + 1 + split_blocks(ALIAS_RUNS);

/// How this port spells a kernel leaf and a table entry.
struct KernelLeaves;

impl ImageLeaves for KernelLeaves {
    type Access = KernelAccess;

    fn leaf(&self, addr: u64, access: KernelAccess, block: bool) -> u64 {
        descriptor(addr, access.leaf(block))
    }

    fn table(&self, phys: u64) -> u64 {
        table_descriptor(phys)
    }
}

/// The kernel image's sections, by physical span, as the linker lays them
/// out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ImageSections {
    /// The code, in whole blocks.
    text: Span,
    /// Read-only data.
    read_only: Span,
    /// Writable data and `.bss` below the boot stack's guard.
    data: Span,
    /// The boot stack's guard, mapped read-only so an overrun faults on its
    /// first byte.
    guard: Span,
    /// The boot stack and the rest of `.bss`.
    stack: Span,
    /// The trampoline's tables, the identity root among them, which nothing
    /// writes once translation is on.
    tables: Span,
    /// The boot heap; empty in an image that keeps none apart.
    heap: Span,
}

impl ImageSections {
    /// The sections, or [`None`] unless each is a run of whole pages,
    /// non-empty but for the heap, they follow one another in link order,
    /// the code fills whole blocks, and the image ends in its first physical
    /// gigabyte below the tree's window.
    fn new(
        text: Span,
        read_only: Span,
        data: Span,
        guard: Span,
        stack: Span,
        tables: Span,
        heap: Span,
    ) -> Option<Self> {
        let sections = Self {
            text,
            read_only,
            data,
            guard,
            stack,
            tables,
            heap,
        };
        let spans = sections.spans();
        let gigabyte = sections.gigabyte();
        let well_formed = in_page_order(&spans)
            && spans[..IMAGE_SECTIONS - 1]
                .iter()
                .all(|span| span.start < span.end)
            && text.start.is_multiple_of(BLOCK)
            && text.end.is_multiple_of(BLOCK)
            && heap
                .end
                .checked_sub(gigabyte)
                .is_some_and(|reach| reach <= TREE_WINDOW_FIRST_BLOCK as u64 * BLOCK);
        well_formed.then_some(sections)
    }

    /// The sections as the linker script lays them out in this image.
    #[cfg(all(target_arch = "aarch64", target_os = "none"))]
    fn linked() -> Option<Self> {
        // Linker-script symbols, whose addresses are their link addresses.
        extern "C" {
            static __text_start: u8;
            static __text_end: u8;
            static __rodata_start: u8;
            static __rodata_end: u8;
            static __data_start: u8;
            static __boot_stack_guard_bottom: u8;
            static __boot_stack_bottom: u8;
            static __bss_end: u8;
            static __early_tables_start: u8;
            static __early_tables_end: u8;
            static __heap_start: u8;
            static __kernel_end: u8;
        }
        let span = |start: *const u8, end: *const u8| Span {
            start: image_phys(start as u64),
            end: image_phys(end as u64),
        };
        Self::new(
            span(
                core::ptr::addr_of!(__text_start),
                core::ptr::addr_of!(__text_end),
            ),
            span(
                core::ptr::addr_of!(__rodata_start),
                core::ptr::addr_of!(__rodata_end),
            ),
            span(
                core::ptr::addr_of!(__data_start),
                core::ptr::addr_of!(__boot_stack_guard_bottom),
            ),
            span(
                core::ptr::addr_of!(__boot_stack_guard_bottom),
                core::ptr::addr_of!(__boot_stack_bottom),
            ),
            span(
                core::ptr::addr_of!(__boot_stack_bottom),
                core::ptr::addr_of!(__bss_end),
            ),
            span(
                core::ptr::addr_of!(__early_tables_start),
                core::ptr::addr_of!(__early_tables_end),
            ),
            span(
                core::ptr::addr_of!(__heap_start),
                core::ptr::addr_of!(__kernel_end),
            ),
        )
    }

    const fn spans(&self) -> [Span; IMAGE_SECTIONS] {
        [
            self.text,
            self.read_only,
            self.data,
            self.guard,
            self.stack,
            self.tables,
            self.heap,
        ]
    }

    /// The physical gigabyte the image lies in.
    const fn gigabyte(&self) -> u64 {
        self.text.start & !(GIGABYTE - 1)
    }

    /// How the image window maps each section.
    const fn window_runs(&self) -> [Run<KernelAccess>; IMAGE_SECTIONS] {
        const fn run(span: Span, access: KernelAccess) -> Run<KernelAccess> {
            Run { span, access }
        }
        [
            run(self.text, KernelAccess::Execute),
            run(self.read_only, KernelAccess::Read),
            run(self.data, KernelAccess::Write),
            run(self.guard, KernelAccess::Read),
            run(self.stack, KernelAccess::Write),
            run(self.tables, KernelAccess::Read),
            run(self.heap, KernelAccess::Write),
        ]
    }

    /// What the direct map's alias of the image may grant beyond the rest
    /// of its RAM: no more than reading the code, the read-only data and the
    /// trampoline's tables.
    const fn alias_runs(&self) -> [Run<KernelAccess>; ALIAS_RUNS] {
        [
            Run {
                span: Span {
                    start: self.text.start,
                    end: self.read_only.end,
                },
                access: KernelAccess::Read,
            },
            Run {
                span: self.tables,
                access: KernelAccess::Read,
            },
        ]
    }
}

/// A firmware tree: its physical address and length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TreeSpan {
    phys: u64,
    len: u64,
}

impl TreeSpan {
    /// The tree the trampoline read the header of, or [`None`] for no tree or
    /// a length no tree has.
    fn new(phys: u64, len: u64) -> Option<Self> {
        let fits = (MIN_TREE_BYTES..=MAX_TREE_BYTES).contains(&len)
            && phys != 0
            && phys.checked_add(len).is_some();
        fits.then_some(Self { phys, len })
    }

    /// Where the window puts the tree's first byte.
    const fn window_address(self, image_gigabyte: u64) -> u64 {
        image_gigabyte
            + TREE_WINDOW_FIRST_BLOCK as u64 * BLOCK
            + (self.phys & (PAGE_SIZE as u64 - 1))
    }
}

/// Build the image's L2 table from `frames`: each of `sections` mapped as
/// [`ImageSections::window_runs`] says, `tree` read-only in its window, and
/// nothing else. Returns the kernel-root entry that reaches it.
fn build_image_window(
    sections: &ImageSections,
    tree: Option<TreeSpan>,
    frames: &dyn PageTableFrames,
) -> Option<u64> {
    let TableFrame {
        phys: l2_phys,
        entries: l2,
    } = frames.alloc_table()?;
    map_gigabyte(
        l2,
        sections.gigabyte(),
        &sections.window_runs(),
        None,
        &KernelLeaves,
        frames,
    )?;
    if let Some(tree) = tree {
        let page = PAGE_SIZE as u64;
        let first = tree.phys & !(page - 1);
        let pages = usize::try_from((tree.phys + tree.len).div_ceil(page) - first / page).ok()?;
        for (block, run) in (0..pages).step_by(ENTRIES_PER_TABLE).enumerate() {
            let TableFrame { phys, entries } = frames.alloc_table()?;
            for (at, leaf) in (run..pages).zip(entries.iter_mut()) {
                *leaf = descriptor(first + at as u64 * page, KernelAccess::Read.leaf(false));
            }
            l2[TREE_WINDOW_FIRST_BLOCK + block] = table_descriptor(phys);
        }
    }
    Some(table_descriptor(l2_phys))
}

/// Build the direct map's alias of the image's gigabyte from `frames`: RAM
/// read/write and executable nowhere, but for [`ImageSections::alias_runs`],
/// read only. Returns the kernel-root entry that reaches it.
fn build_alias(sections: &ImageSections, frames: &dyn PageTableFrames) -> Option<u64> {
    let TableFrame { phys, entries } = frames.alloc_table()?;
    map_gigabyte(
        entries,
        sections.gigabyte(),
        &sections.alias_runs(),
        Some(KernelAccess::Write),
        &KernelLeaves,
        frames,
    )?;
    Some(table_descriptor(phys))
}

#[cfg(all(target_arch = "aarch64", target_os = "none"))]
extern "C" {
    /// `boot.s`'s identity-page routine loading a kernel root by way of an
    /// empty one.
    fn tairix_aarch64_replace_ttbr1(root: u64, empty: u64);
}

/// The tables of the image half, which live as long as the kernel.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
static KERNEL_ROOT_POOL: PageTablePool<KERNEL_ROOT_TABLES> = PageTablePool::new();

/// The gigabytes a boot handed no tree types as registers: those of the
/// register bases the port starts from before any tree is read, QEMU
/// `virt`'s. A board that hands over a tree types its own from it.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
const TREELESS_REGISTERS: [u64; GIGAPAGE_MASK_WORDS] = {
    let defaults = [
        crate::console::DEFAULT_CONSOLE_BASE,
        crate::gic::DEFAULT_GICD_BASE,
        crate::gic::DEFAULT_GICC_BASE,
    ];
    let mut mask = [0; GIGAPAGE_MASK_WORDS];
    let mut at = 0;
    while at < defaults.len() {
        let gigabyte = defaults[at] >> 30;
        mask[gigabyte / 64] |= 1 << (gigabyte % 64);
        at += 1;
    }
    mask
};

/// Build the kernel root's image half, load the kernel root in place of the
/// trampoline's early map, park the low regime on the empty root, and map
/// the firmware tree the trampoline found at `tree_phys`, `tree_len` bytes
/// long. Returns where the kernel reads the tree, or `0` for none.
///
/// The image's gigabyte of the direct map is typed RAM first, so no register
/// a board reports there can retype it.
///
/// # Errors
///
/// Why the boot cannot go on: the image's sections are not ones the window
/// maps, the tables reserved for the root ran out, or the image lies beyond
/// the direct map.
///
/// # Safety
///
/// Once, on the boot CPU, from the port entry: interrupts masked, on the
/// trampoline's early map and its identity root.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
pub(crate) unsafe fn enter_kernel_root(tree_phys: u64, tree_len: u64) -> Result<u64, &'static str> {
    const EXHAUSTED: &str = "the kernel root's page tables ran out";
    if PARK_ROOT.published().is_some() {
        return Err("the kernel root was entered already");
    }
    let sections = ImageSections::linked()
        .ok_or("the kernel image's sections are not ones its window maps")?;
    let gigapage = (sections.gigabyte() >> 30) as usize;
    if gigapage >= MAX_PHYSMAP_GIB {
        return Err("the kernel image lies beyond the direct map");
    }
    let tree = TreeSpan::new(tree_phys, tree_len);
    let image = build_image_window(&sections, tree, &KERNEL_ROOT_POOL).ok_or(EXHAUSTED)?;
    let alias = build_alias(&sections, &KERNEL_ROOT_POOL).ok_or(EXHAUSTED)?;
    let root = kernel_root_entries();
    root[IMAGE_SLOT].store(image, Ordering::Release);
    root[PHYSMAP_FIRST_SLOT + gigapage].store(alias, Ordering::Release);

    // The routine runs from the identity page, the one place translation of
    // the kernel regime can be absent from.
    let replace = image_phys(tairix_aarch64_replace_ttbr1 as *const () as u64);
    // SAFETY: the routine is the identity page's own, which the trampoline's
    // identity root maps at its physical address; it touches no memory and
    // returns into the new root, which maps this code and the boot stack at
    // the addresses the early map did.
    unsafe {
        core::arch::asm!(
            "blr {replace}",
            replace = in(reg) replace,
            in("x0") kernel_root_phys(),
            in("x1") empty_root_phys(),
            clobber_abi("C"),
            options(nostack),
        );
    }
    PARK_ROOT.publish(empty_root_phys());
    if !park_kernel_root(true) {
        return Err("the empty root was not published");
    }

    let Some(tree) = tree else {
        let _ = reveal(&TREELESS_REGISTERS, Holding::Registers);
        return Ok(0);
    };
    let virt = tree.window_address(KERNEL_IMAGE_GIGABYTE);
    publish_firmware_tree(FirmwareTree {
        virt,
        phys: tree.phys,
        len: tree.len,
    });
    Ok(virt)
}

#[cfg(test)]
#[path = "kernel_root_tests.rs"]
mod tests;
