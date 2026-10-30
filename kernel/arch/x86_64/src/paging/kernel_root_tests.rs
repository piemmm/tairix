use tairix_arch_api::frames::{PageTableFrames, TableFrame};
use tairix_arch_api::mmu::{self, PageFlags};

use super::*;
use crate::paging::{
    AddressSpace, PageTablePool, ADDR_MASK, KERNEL_IMAGE_PML4_SLOT, PHYSMAP_PML4_FIRST_SLOT,
    PHYSMAP_VMA_BASE,
};

const MIB: u64 = 1 << 20;
const PAGE: u64 = PAGE_SIZE as u64;

fn span(start: u64, end: u64) -> Span {
    Span { start, end }
}

/// The layout a production kernel links to: the boot data just above 1 MiB
/// and every section boundary inside a 2 MiB block.
fn linked_like() -> ImageSections {
    ImageSections::new(
        span(0x10_2000, 0x12_2000),
        span(0x12_2000, 0x5A_1000),
        span(0x5A_1000, 0x114_6000),
        span(0x114_6000, 0x21E_6000),
    )
    .expect("a layout the window maps")
}

/// A root reaching `entry` from PML4 slot `slot`, drawn from `frames`.
fn root_with(slot: usize, entry: u64, frames: &'static dyn PageTableFrames) -> AddressSpace {
    let TableFrame { phys, entries } = frames.alloc_table().expect("a root");
    entries[slot] = entry;
    AddressSpace {
        pml4_phys: phys,
        frames,
    }
}

/// What `space` maps `virt` as, checking it maps it to `phys`.
fn rights_at(space: &AddressSpace, virt: u64, phys: u64) -> Option<PageFlags> {
    let (to, rights) = mmu::AddressSpace::translate(space, virt)?;
    assert_eq!(to, phys, "{virt:#x} maps the wrong frame");
    Some(rights)
}

const READ_EXECUTE: PageFlags = PageFlags::READ.union(PageFlags::EXEC);
const READ_WRITE: PageFlags = PageFlags::READ.union(PageFlags::WRITE);

#[test]
fn a_span_splits_only_a_block_one_of_its_ends_falls_inside() {
    let block = span(2 * MIB, 4 * MIB);
    assert!(!block.splits(2 * MIB, 2 * MIB), "a span that is the block");
    assert!(
        !span(MIB, 8 * MIB).splits(2 * MIB, 2 * MIB),
        "one that covers it"
    );
    assert!(
        !span(4 * MIB, 6 * MIB).splits(2 * MIB, 2 * MIB),
        "one that starts after it"
    );
    assert!(span(3 * MIB, 6 * MIB).splits(2 * MIB, 2 * MIB));
    assert!(span(MIB, 3 * MIB).splits(2 * MIB, 2 * MIB));
    assert!(block.contains(2 * MIB) && !block.contains(4 * MIB));
}

#[test]
fn sections_the_window_cannot_map_are_refused() {
    let ok = linked_like();
    let [boot, text, read_only, data] = ok.spans();
    assert!(
        ImageSections::new(span(boot.start + 1, boot.end), text, read_only, data).is_none(),
        "a span not of whole pages"
    );
    assert!(
        ImageSections::new(boot, span(text.start, text.start), read_only, data).is_none(),
        "an empty section"
    );
    assert!(
        ImageSections::new(
            boot,
            span(text.start, read_only.start + PAGE),
            read_only,
            data
        )
        .is_none(),
        "sections that overlap"
    );
    assert!(
        ImageSections::new(text, boot, read_only, data).is_none(),
        "sections out of link order"
    );
    assert!(
        ImageSections::new(boot, text, read_only, span(data.start, IMAGE_SPAN + PAGE)).is_none(),
        "an image past the window's gigabyte"
    );
}

#[test]
fn the_image_window_maps_each_section_with_only_its_access() {
    static POOL: PageTablePool<ROOT_TABLES> = PageTablePool::new();
    let image = build_image_window(&linked_like(), &POOL).expect("the window");
    let root = root_with(KERNEL_IMAGE_PML4_SLOT, image, &POOL);
    let at = |phys: u64| rights_at(&root, KERNEL_VMA_BASE + phys, phys);

    assert_eq!(at(0), None, "low memory");
    assert_eq!(
        at(MIB),
        None,
        "the boot code, which runs only from its identity"
    );
    assert_eq!(at(0x10_2000), Some(READ_WRITE), "the boot data");
    assert_eq!(at(0x12_2000), Some(READ_EXECUTE), "the first page of text");
    assert_eq!(at(0x5A_0000), Some(READ_EXECUTE), "the last page of text");
    assert_eq!(at(0x5A_1000), Some(PageFlags::READ), "read-only data");
    assert_eq!(at(0x114_5000), Some(PageFlags::READ), "its last page");
    assert_eq!(at(0x114_6000), Some(READ_WRITE), "writable data");
    assert_eq!(at(0x21E_5000), Some(READ_WRITE), "the last page of .bss");
    assert_eq!(at(0x21E_6000), None, "the RAM past the image");
    assert_eq!(at(IMAGE_SPAN - PAGE), None, "the end of the gigabyte");
}

#[test]
fn a_block_one_section_fills_is_one_large_leaf_and_only_a_split_block_a_table() {
    static POOL: PageTablePool<ROOT_TABLES> = PageTablePool::new();
    let image = build_image_window(&linked_like(), &POOL).expect("the window");
    let pdpt = POOL.table_at(image & ADDR_MASK).expect("the window's PDPT");
    // SAFETY: tables of the test's own pool, only read.
    let directory = POOL
        .table_at(unsafe { &*pdpt }[IMAGE_PDPT_SLOT] & ADDR_MASK)
        .expect("its directory");
    // SAFETY: as above.
    let directory = unsafe { &*directory };
    let tables = directory
        .iter()
        .filter(|entry| **entry & flags::PRESENT != 0 && **entry & flags::HUGE == 0)
        .count();
    assert_eq!(tables, 4, "blocks 0, 2, 8 and 16 hold a section boundary");
    assert_eq!(
        directory[1],
        (2 * MIB) | KernelAccess::Execute.leaf() | flags::HUGE,
        "a block inside the text"
    );
    assert_eq!(directory[100], 0, "a block past the image");
}

#[test]
fn nothing_the_image_window_maps_is_both_writable_and_executable() {
    static POOL: PageTablePool<ROOT_TABLES> = PageTablePool::new();
    let image = build_image_window(&linked_like(), &POOL).expect("the window");
    let pdpt = POOL.table_at(image & ADDR_MASK).expect("the window's PDPT");
    // SAFETY: tables of the test's own pool, only read.
    let directory = POOL
        .table_at(unsafe { &*pdpt }[IMAGE_PDPT_SLOT] & ADDR_MASK)
        .expect("its directory");
    let writable_and_executable =
        |leaf: u64| leaf & flags::WRITABLE != 0 && leaf & flags::NO_EXECUTE == 0;
    // SAFETY: as above.
    for entry in unsafe { &*directory } {
        if *entry & flags::PRESENT == 0 {
            continue;
        }
        if *entry & flags::HUGE != 0 {
            assert!(!writable_and_executable(*entry), "{entry:#x}");
            continue;
        }
        let table = POOL
            .table_at(*entry & ADDR_MASK)
            .expect("a table of the pool");
        // SAFETY: as above.
        for leaf in unsafe { &*table } {
            assert!(*leaf & flags::USER == 0, "{leaf:#x} reaches ring 3");
            assert!(!writable_and_executable(*leaf), "{leaf:#x}");
        }
    }
}

#[test]
fn the_direct_map_reads_the_code_and_read_only_data_and_writes_the_rest() {
    static POOL: PageTablePool<ROOT_TABLES> = PageTablePool::new();
    let mut floor = [0u64; ENTRIES_PER_TABLE];
    for (gib, entry) in floor.iter_mut().enumerate().take(4) {
        *entry = (0x0010_7000 + (gib as u64) * PAGE) | KERNEL_TABLE_ENTRY;
    }
    let first_span = build_first_span(&floor, &linked_like(), &POOL).expect("the first span");
    let pdpt = POOL
        .table_at(first_span & ADDR_MASK)
        .expect("the span's PDPT");
    // SAFETY: a table of the test's own pool, only read.
    let pdpt = unsafe { &*pdpt };
    assert_eq!(
        pdpt[1..],
        floor[1..],
        "the floor's other directories are shared"
    );
    assert_ne!(pdpt[0], floor[0], "the first directory is drawn afresh");

    let root = root_with(PHYSMAP_PML4_FIRST_SLOT, first_span, &POOL);
    let at = |phys: u64| rights_at(&root, PHYSMAP_VMA_BASE + phys, phys);
    assert_eq!(at(0), Some(READ_WRITE), "low memory");
    assert_eq!(at(0x10_2000), Some(READ_WRITE), "the boot data");
    assert_eq!(at(0x12_2000), Some(PageFlags::READ), "the text");
    assert_eq!(at(0x5A_1000), Some(PageFlags::READ), "the read-only data");
    assert_eq!(at(0x114_6000), Some(READ_WRITE), "writable data");
    assert_eq!(
        at(IMAGE_SPAN - PAGE),
        Some(READ_WRITE),
        "the end of the gigabyte"
    );
}

#[test]
fn the_start_up_root_maps_only_the_trampoline_page_as_itself() {
    static POOL: PageTablePool<ROOT_TABLES> = PageTablePool::new();
    let image = 0x0040_0000 | KERNEL_TABLE_ENTRY;
    let page = crate::smp::AP_TRAMPOLINE_PHYS;
    let root = build_startup_root(image, page, &POOL).expect("the start-up root");
    let space = AddressSpace {
        pml4_phys: root,
        frames: &POOL,
    };
    assert_eq!(rights_at(&space, page, page), Some(READ_EXECUTE));
    for neighbour in [0, page - PAGE, page + PAGE, 2 * MIB] {
        assert_eq!(
            rights_at(&space, neighbour, neighbour),
            None,
            "{neighbour:#x}"
        );
    }
    let pml4 = POOL.table_at(root).expect("the pool's own root");
    // SAFETY: a table of the test's own pool, only read.
    let pml4 = unsafe { &*pml4 };
    assert_eq!(
        pml4[KERNEL_IMAGE_PML4_SLOT], image,
        "the kernel's image window"
    );
    assert_eq!(
        pml4.iter().filter(|entry| **entry != 0).count(),
        2,
        "nothing else: no direct map, no remap window"
    );
}

#[test]
fn a_start_up_page_that_is_not_a_user_half_page_is_refused() {
    static POOL: PageTablePool<ROOT_TABLES> = PageTablePool::new();
    assert!(build_startup_root(0, crate::smp::AP_TRAMPOLINE_PHYS + 1, &POOL).is_none());
    assert!(build_startup_root(0, 1 << 47, &POOL).is_none());
}

/// Every section and the alias run with both ends in a block of their own:
/// the most tables the roots can need, which must be exactly what is
/// reserved for them.
#[test]
fn the_tables_reserved_for_the_roots_cover_their_worst_layout() {
    static POOL: PageTablePool<ROOT_TABLES> = PageTablePool::new();
    let off = PAGE;
    let worst = ImageSections::new(
        span(MIB + off, 3 * MIB + off),
        span(5 * MIB + off, 7 * MIB + off),
        span(9 * MIB + off, 11 * MIB + off),
        span(13 * MIB + off, 15 * MIB + off),
    )
    .expect("a layout the window maps");
    let image = build_image_window(&worst, &POOL).expect("the window");
    let floor = [0u64; ENTRIES_PER_TABLE];
    build_first_span(&floor, &worst, &POOL).expect("the first span");
    POOL.alloc_table().expect("the kernel root's PML4");
    build_startup_root(image, crate::smp::AP_TRAMPOLINE_PHYS, &POOL).expect("the start-up root");
    assert!(
        POOL.alloc_table().is_none(),
        "the reserve holds no table the worst layout leaves unused"
    );
}
