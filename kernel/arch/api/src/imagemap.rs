//! A gigabyte of the kernel image mapped by runs: each page with the access
//! of the run that holds it, in a block leaf wherever one access covers the
//! whole block.
//!
//! The policy is the same on every port that maps its image by section; only
//! the descriptor spelling differs, which a port supplies as [`ImageLeaves`].

use crate::frames::{PageTableFrames, TableFrame, PAGE_TABLE_ENTRIES};

/// Bytes one page maps.
const PAGE: u64 = 1 << 12;

/// Bytes one block leaf, an entry of the table [`map_gigabyte`] fills, maps.
pub const BLOCK: u64 = 1 << 21;

/// Bytes the table [`map_gigabyte`] fills maps.
pub const GIGABYTE: u64 = 1 << 30;

const _: () = assert!(PAGE_TABLE_ENTRIES as u64 * BLOCK == GIGABYTE);

/// A physical span `[start, end)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    /// The first byte.
    pub start: u64,
    /// One past the last byte.
    pub end: u64,
}

impl Span {
    /// Whether `at` is one of the span's bytes.
    #[must_use]
    pub const fn contains(self, at: u64) -> bool {
        self.start <= at && at < self.end
    }

    /// Whether an end of the span falls strictly inside `[base, base + len)`,
    /// so the span covers only part of it. An empty span covers nothing, so
    /// splits nothing.
    #[must_use]
    pub fn splits(self, base: u64, len: u64) -> bool {
        let inside = |edge: u64| base < edge && edge < base + len;
        self.start < self.end && (inside(self.start) || inside(self.end))
    }
}

/// Pages and the access the kernel has to them.
#[derive(Clone, Copy, Debug)]
pub struct Run<A> {
    /// The pages.
    pub span: Span,
    /// What the kernel may do with them.
    pub access: A,
}

/// Page tables `runs` runs can need under [`map_gigabyte`]: a run shares a
/// block with anything else only at its two ends, and only such a block
/// needs a table of its own.
#[must_use]
pub const fn split_blocks(runs: usize) -> usize {
    2 * runs
}

/// How a port spells the entries [`map_gigabyte`] writes.
pub trait ImageLeaves {
    /// What the kernel may do with a page.
    type Access: Copy;

    /// A leaf mapping the page, or with `block` the block, at `addr` with
    /// `access`.
    fn leaf(&self, addr: u64, access: Self::Access, block: bool) -> u64;

    /// An entry pointing at the page table at `phys`.
    fn table(&self, phys: u64) -> u64;
}

/// Fill `table`, which maps the physical gigabyte at `base`, so each page has
/// the access of the run holding it, or `outside` where none does (nothing,
/// for [`None`]).
///
/// A block one access covers whole is one block leaf; only a block a run ends
/// inside costs a page table from `frames`. [`None`] when `base` is not a
/// gigabyte's or `frames` runs out, with the table part-written.
pub fn map_gigabyte<L: ImageLeaves>(
    table: &mut [u64; PAGE_TABLE_ENTRIES],
    base: u64,
    runs: &[Run<L::Access>],
    outside: Option<L::Access>,
    leaves: &L,
    frames: &dyn PageTableFrames,
) -> Option<()> {
    if !base.is_multiple_of(GIGABYTE) {
        return None;
    }
    let access_at = |at: u64| {
        runs.iter()
            .find(|run| run.span.contains(at))
            .map(|run| run.access)
            .or(outside)
    };
    for (index, entry) in table.iter_mut().enumerate() {
        let block = base + index as u64 * BLOCK;
        *entry = if runs.iter().any(|run| run.span.splits(block, BLOCK)) {
            let TableFrame { phys, entries } = frames.alloc_table()?;
            for (page_index, leaf) in entries.iter_mut().enumerate() {
                let page = block + page_index as u64 * PAGE;
                *leaf = access_at(page).map_or(0, |access| leaves.leaf(page, access, false));
            }
            leaves.table(phys)
        } else {
            access_at(block).map_or(0, |access| leaves.leaf(block, access, true))
        };
    }
    Some(())
}

#[cfg(test)]
#[path = "imagemap_tests.rs"]
mod tests;
