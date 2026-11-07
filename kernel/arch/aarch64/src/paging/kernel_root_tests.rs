use std::vec::Vec;

use super::*;
use crate::paging::{
    attrs, is_block, phys_from_descriptor, writable_and_executable, PageTablePool,
};

const MIB: u64 = 1 << 20;
const PAGE: u64 = PAGE_SIZE as u64;
const G: u64 = GIGABYTE;

fn span(start: u64, end: u64) -> Span {
    Span { start, end }
}

/// A `virt`-like layout: the text in whole blocks 2 MiB into gigabyte 1, and
/// every other boundary inside a block.
fn linked_like() -> ImageSections {
    ImageSections::new(
        span(G + 2 * MIB, G + 8 * MIB),
        span(G + 8 * MIB, G + 9 * MIB + 0x4000),
        span(G + 9 * MIB + 0x4000, G + 10 * MIB + 0x7000),
        span(G + 10 * MIB + 0x7000, G + 10 * MIB + 0x8000),
        span(G + 10 * MIB + 0x8000, G + 11 * MIB),
        span(G + 11 * MIB, G + 11 * MIB + 0x5000),
        span(G + 11 * MIB + 0x5000, G + 20 * MIB),
    )
    .expect("a layout the window maps")
}

/// The valid leaf the hierarchy under the kernel-root entry `entry` maps the
/// block `index`, page `page` with.
fn leaf_at<const N: usize>(
    pool: &PageTablePool<N>,
    entry: u64,
    index: usize,
    page: usize,
) -> Option<u64> {
    let l2 = pool.table_at(phys_from_descriptor(entry))?;
    // SAFETY: the test's own pool, only read.
    let block = unsafe { &*l2 }[index];
    if block & attrs::VALID == 0 {
        return None;
    }
    if is_block(block) {
        return Some(block);
    }
    let l3 = pool.table_at(phys_from_descriptor(block))?;
    // SAFETY: as above.
    let leaf = unsafe { &*l3 }[page];
    (leaf & attrs::VALID != 0).then_some(leaf)
}

/// The leaf covering physical `addr` of the gigabyte the entry maps.
fn leaf_for<const N: usize>(pool: &PageTablePool<N>, entry: u64, addr: u64) -> Option<u64> {
    let within = addr % G;
    leaf_at(
        pool,
        entry,
        (within / BLOCK) as usize,
        ((within % BLOCK) / PAGE) as usize,
    )
}

/// The access a kernel leaf grants.
fn access(leaf: u64) -> KernelAccess {
    let ap = leaf & (0b11 << 6);
    match (ap == attrs::AP_RW_EL1, leaf & attrs::PXN != 0) {
        (false, false) => KernelAccess::Execute,
        (false, true) => KernelAccess::Read,
        (true, true) => KernelAccess::Write,
        (true, false) => panic!("{leaf:#x} is writable and executable"),
    }
}

/// Every valid leaf under the entry, with whether it is a block.
fn leaves<const N: usize>(pool: &PageTablePool<N>, entry: u64) -> Vec<(u64, bool)> {
    let mut out = Vec::new();
    for index in 0..ENTRIES_PER_TABLE {
        let Some(first) = leaf_at(pool, entry, index, 0) else {
            continue;
        };
        if is_block(first) {
            out.push((first, true));
            continue;
        }
        out.extend(
            (0..ENTRIES_PER_TABLE)
                .filter_map(|page| leaf_at(pool, entry, index, page).map(|leaf| (leaf, false))),
        );
    }
    out
}

#[test]
fn the_image_window_maps_each_section_with_only_its_access() {
    static POOL: PageTablePool<KERNEL_ROOT_TABLES> = PageTablePool::new();
    let sections = linked_like();
    let entry = build_image_window(&sections, None, &POOL).expect("the window");
    let at = |addr: u64| leaf_for(&POOL, entry, addr).map(access);
    for (addr, expect) in [
        (G + 2 * MIB, KernelAccess::Execute),
        (G + 8 * MIB - PAGE, KernelAccess::Execute),
        (G + 8 * MIB, KernelAccess::Read),
        (G + 9 * MIB + 0x3000, KernelAccess::Read),
        (G + 9 * MIB + 0x4000, KernelAccess::Write),
        (G + 10 * MIB + 0x7000, KernelAccess::Read),
        (G + 10 * MIB + 0x8000, KernelAccess::Write),
        (G + 11 * MIB, KernelAccess::Read),
        (G + 11 * MIB + 0x5000, KernelAccess::Write),
        (G + 20 * MIB - PAGE, KernelAccess::Write),
    ] {
        assert_eq!(at(addr), Some(expect), "{addr:#x}");
        let leaf = leaf_for(&POOL, entry, addr).expect("mapped");
        let size = if is_block(leaf) { BLOCK } else { PAGE };
        assert_eq!(phys_from_descriptor(leaf), addr & !(size - 1), "{addr:#x}");
    }
    assert_eq!(at(G), None, "below the image");
    assert_eq!(at(G + 2 * MIB - PAGE), None, "the page under the text");
    assert_eq!(at(G + 20 * MIB), None, "past the image");
}

#[test]
fn the_text_is_mapped_with_block_leaves_only() {
    static POOL: PageTablePool<KERNEL_ROOT_TABLES> = PageTablePool::new();
    let entry = build_image_window(&linked_like(), None, &POOL).expect("the window");
    let executable: Vec<(u64, bool)> = leaves(&POOL, entry)
        .into_iter()
        .filter(|&(leaf, _)| access(leaf) == KernelAccess::Execute)
        .collect();
    assert_eq!(executable.len(), 3, "6 MiB of text, three blocks");
    assert!(executable.iter().all(|&(_, block)| block));
}

#[test]
fn nothing_the_window_or_the_alias_maps_is_both_writable_and_executable() {
    static POOL: PageTablePool<KERNEL_ROOT_TABLES> = PageTablePool::new();
    let sections = linked_like();
    let window = build_image_window(&sections, None, &POOL).expect("the window");
    let alias = build_alias(&sections, &POOL).expect("the alias");
    for (leaf, _) in leaves(&POOL, window)
        .into_iter()
        .chain(leaves(&POOL, alias))
    {
        assert!(!writable_and_executable(leaf), "{leaf:#x}");
        assert_ne!(leaf & attrs::UXN, 0, "{leaf:#x} is EL0-executable");
    }
}

#[test]
fn the_direct_map_reads_the_code_its_data_and_the_tables_and_writes_the_rest() {
    static POOL: PageTablePool<KERNEL_ROOT_TABLES> = PageTablePool::new();
    let entry = build_alias(&linked_like(), &POOL).expect("the alias");
    let at = |addr: u64| leaf_for(&POOL, entry, addr).map(access);
    for (addr, expect) in [
        (G, KernelAccess::Write),
        (G + 2 * MIB, KernelAccess::Read),
        (G + 9 * MIB + 0x3000, KernelAccess::Read),
        (G + 9 * MIB + 0x4000, KernelAccess::Write),
        (G + 10 * MIB + 0x7000, KernelAccess::Write),
        (G + 11 * MIB, KernelAccess::Read),
        (G + 11 * MIB + 0x5000, KernelAccess::Write),
        (2 * G - PAGE, KernelAccess::Write),
    ] {
        assert_eq!(at(addr), Some(expect), "{addr:#x}");
    }
    assert_eq!(
        POOL.used.load(core::sync::atomic::Ordering::SeqCst),
        1 + 2,
        "the gigabyte's table and the two blocks a run ends inside"
    );
}

#[test]
fn the_tree_is_mapped_read_only_in_its_window_and_nothing_beside_it() {
    static POOL: PageTablePool<KERNEL_ROOT_TABLES> = PageTablePool::new();
    let tree = TreeSpan::new(G + 0x100, 0x2_0005).expect("a tree");
    let entry = build_image_window(&linked_like(), Some(tree), &POOL).expect("the window");
    let window = tree.window_address(0);
    assert_eq!(window, TREE_WINDOW_FIRST_BLOCK as u64 * BLOCK + 0x100);
    let pages = 0x21;
    for page in 0..pages {
        let leaf = leaf_at(&POOL, entry, TREE_WINDOW_FIRST_BLOCK, page).expect("a tree page");
        assert_eq!(access(leaf), KernelAccess::Read);
        assert_eq!(phys_from_descriptor(leaf), G + page as u64 * PAGE);
    }
    assert_eq!(leaf_at(&POOL, entry, TREE_WINDOW_FIRST_BLOCK, pages), None);
    assert_eq!(leaf_at(&POOL, entry, TREE_WINDOW_FIRST_BLOCK + 1, 0), None);
    assert_eq!(leaf_at(&POOL, entry, ENTRIES_PER_TABLE - 1, 0), None);
}

#[test]
fn the_largest_tree_at_the_last_offset_fits_its_window() {
    static POOL: PageTablePool<KERNEL_ROOT_TABLES> = PageTablePool::new();
    let tree = TreeSpan::new(G + PAGE - 8, MAX_TREE_BYTES).expect("the largest tree");
    let entry = build_image_window(&linked_like(), Some(tree), &POOL).expect("the window");
    let last = leaf_at(&POOL, entry, TREE_WINDOW_FIRST_BLOCK + 1, 0).expect("its 513th page");
    assert_eq!(phys_from_descriptor(last), G + 512 * PAGE);
    assert_eq!(leaf_at(&POOL, entry, TREE_WINDOW_FIRST_BLOCK + 1, 1), None);
}

#[test]
fn a_tree_of_no_tree_length_is_not_mapped() {
    assert_eq!(TreeSpan::new(G, 0), None, "no tree");
    assert_eq!(
        TreeSpan::new(G, MIN_TREE_BYTES - 1),
        None,
        "shorter than a header"
    );
    assert_eq!(TreeSpan::new(G, MAX_TREE_BYTES + 1), None, "past the bound");
    assert_eq!(TreeSpan::new(0, MIN_TREE_BYTES), None, "at no address");
    assert_eq!(
        TreeSpan::new(u64::MAX - 8, MIN_TREE_BYTES),
        None,
        "wrapping"
    );
    assert!(TreeSpan::new(G, MAX_TREE_BYTES).is_some());
}

#[test]
fn sections_the_window_cannot_map_are_refused() {
    let good = linked_like();
    let with = |change: &dyn Fn(&mut [Span; IMAGE_SECTIONS])| {
        let mut spans = good.spans();
        change(&mut spans);
        let [text, read_only, data, guard, stack, tables, heap] = spans;
        ImageSections::new(text, read_only, data, guard, stack, tables, heap)
    };
    assert!(with(&|_| {}).is_some());
    assert!(
        with(&|spans| spans[0].start += PAGE).is_none(),
        "text off a block"
    );
    assert!(
        with(&|spans| spans[2].end += 8).is_none(),
        "a span off a page"
    );
    assert!(
        with(&|spans| spans[3].end = spans[3].start).is_none(),
        "an empty guard"
    );
    assert!(
        with(&|spans| spans[1].end = spans[2].start + PAGE).is_none(),
        "out of order"
    );
    assert!(
        with(&|spans| spans[6].start = spans[6].end).is_some(),
        "an image with no heap apart"
    );
    assert!(
        with(&|spans| spans[6].end = G + TREE_WINDOW_FIRST_BLOCK as u64 * BLOCK + PAGE).is_none(),
        "an image reaching the tree's window"
    );
}

/// Every boundary inside a block, and the largest tree: the most tables the
/// image half can take.
#[test]
fn the_tables_reserved_for_the_image_half_cover_its_worst_layout() {
    static POOL: PageTablePool<KERNEL_ROOT_TABLES> = PageTablePool::new();
    let k = PAGE;
    let worst = ImageSections::new(
        span(G + 2 * MIB, G + 4 * MIB),
        span(G + 4 * MIB, G + 6 * MIB + k),
        span(G + 6 * MIB + k, G + 8 * MIB + k),
        span(G + 8 * MIB + k, G + 10 * MIB + k),
        span(G + 10 * MIB + k, G + 12 * MIB + k),
        span(G + 12 * MIB + k, G + 14 * MIB + k),
        span(G + 14 * MIB + k, G + 16 * MIB + k),
    )
    .expect("a layout the window maps");
    let tree = TreeSpan::new(G + PAGE - 8, MAX_TREE_BYTES);
    build_image_window(&worst, tree, &POOL).expect("the window from the reserve");
    build_alias(&worst, &POOL).expect("the alias from the reserve");
}
