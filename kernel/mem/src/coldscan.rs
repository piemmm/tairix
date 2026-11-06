//! Cold-page identification for the compressed-memory tier
//! (`plans/SWAPSWAPSWAP.md` section 5: "not recently accessed by the
//! page-replacement policy").
//!
//! A **second-chance (clock)** scan (Corbató, 1968) over a task's candidate
//! pages, driven by the per-page referenced bit the Arch HAL exposes
//! ([`AddressSpace::test_and_clear_accessed`]). A [`ColdPageScanner`] keeps one
//! clock hand per address space, and each [`Sweep`] moves it on from where the
//! last one stopped, reading and clearing the bit of every page it visits: a
//! page whose bit was set was touched since the hand last passed and gets a
//! second chance; a page whose bit was clear went untouched for a whole
//! revolution and is cold. Clearing the bit also discards the page's TLB entry,
//! so the next real access sets it again, in hardware or through a port's
//! access-flag fault.
//!
//! A sweep visits at most its budget of pages and none twice. The cost of a
//! reclaim is therefore bounded by what it asks for, never by the size of the
//! space, and a working set that is entirely hot costs one bounded step of the
//! hand per sweep rather than a walk of every page.
//!
//! # Fail closed
//!
//! On a port whose [`AccessTracking`](tairix_arch_api::mmu::AccessTracking) is
//! not `Supported` there is no referenced bit, so a sweep is refused with
//! [`ColdScanError::Unsupported`] and nothing is classified cold: a false-cold
//! verdict would evict a hot page and cause the thrash the tier exists to
//! avoid.

use core::ops::RangeInclusive;

use tairix_inline::ArrayVec;

use crate::anon::page_at;
use crate::vmm::{AddressSpace, Page, PageTable};

/// Why a [`ColdPageScanner::sweep`] cannot run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ColdScanError {
    /// The address space's backend exposes no per-page referenced bit
    /// (a non-`Supported`
    /// [`AccessTracking`](tairix_arch_api::mmu::AccessTracking)), so no
    /// page can be shown to be cold. Fail closed: the caller reclaims
    /// nothing rather than guess.
    Unsupported,
}

/// A second-chance (clock) cold-page scanner for one address space.
///
/// The [`Self::hand`] it carries is that space's clock position, so each
/// sweep resumes where the previous one stopped and every page is offered its
/// second chance in turn.
#[derive(Debug, Default, Clone, Copy)]
pub struct ColdPageScanner {
    /// The page number the next sweep starts from.
    hand: u64,
}

impl ColdPageScanner {
    /// A fresh scanner with its clock hand at the start of the space.
    #[must_use]
    pub const fn new() -> Self {
        Self { hand: 0 }
    }

    /// The current clock-hand page number (diagnostic / test observer).
    #[must_use]
    pub const fn hand(&self) -> u64 {
        self.hand
    }

    /// Begin a sweep over the live pages of `space` inside `window`, starting
    /// at the clock hand and visiting at most `budget` pages.
    ///
    /// A hand outside `window` starts the sweep at the window's first page.
    ///
    /// # Errors
    ///
    /// [`ColdScanError::Unsupported`] if the backend exposes no referenced
    /// bit: nothing can be classified cold (fail closed).
    pub fn sweep<P: PageTable>(
        &mut self,
        space: &AddressSpace<P>,
        window: RangeInclusive<Page>,
        budget: usize,
    ) -> Result<Sweep<'_>, ColdScanError> {
        if !space.access_tracking().is_supported() {
            return Err(ColdScanError::Unsupported);
        }
        let (first, last) = (window.start().number(), window.end().number());
        let origin = if (first..=last).contains(&self.hand) {
            self.hand
        } else {
            first
        };
        self.hand = origin;
        Ok(Sweep {
            hand: &mut self.hand,
            first,
            last,
            origin,
            wrapped: false,
            done: first > last,
            budget,
            visited: 0,
        })
    }
}

/// One bounded pass of a [`ColdPageScanner`]'s clock hand, begun by
/// [`ColdPageScanner::sweep`].
///
/// It runs from the hand to the end of its window, then from the window's
/// start back to where it began, and moves the hand as it goes. A caller may
/// take its cold pages over several [`Self::collect`] calls, acting on each
/// batch in between, and still no page is visited twice: a second visit would
/// find the bit the first one cleared and call a hot page cold.
#[derive(Debug)]
pub struct Sweep<'a> {
    hand: &'a mut u64,
    first: u64,
    last: u64,
    origin: u64,
    wrapped: bool,
    done: bool,
    budget: usize,
    visited: usize,
}

impl Sweep<'_> {
    /// Visit live pages from the hand, adding each cold one `eligible` admits
    /// to `cold`, until `cold` holds `limit` pages or is full, the budget is
    /// spent, or the sweep has come round to where it began.
    ///
    /// An ineligible page counts against the budget but keeps its referenced
    /// bit, so the bound on the work holds whatever the window contains and a
    /// page the caller may not reclaim is never aged. A page whose probe fails
    /// (raced out by an unmap) is not cold.
    pub fn collect<P: PageTable, const N: usize>(
        &mut self,
        space: &mut AddressSpace<P>,
        eligible: impl Fn(Page) -> bool,
        limit: usize,
        cold: &mut ArrayVec<Page, N>,
    ) {
        while !self.done && cold.len() < limit && !cold.is_full() && self.visited < self.budget {
            let end = if self.wrapped {
                self.origin.checked_sub(1)
            } else {
                Some(self.last)
            };
            let Some(page) = end.and_then(|end| next_live(space, *self.hand, end)) else {
                self.turn();
                continue;
            };
            self.visited += 1;
            *self.hand = page.number().saturating_add(1);
            let is_cold =
                eligible(page) && matches!(space.test_and_clear_accessed(page), Ok(false));
            if is_cold && cold.try_push(page).is_err() {
                // The loop guard leaves room; were it ever wrong, the page is
                // offered again rather than lost.
                self.visited -= 1;
                *self.hand = page.number();
                return;
            }
        }
    }

    /// How many live pages this sweep has visited.
    #[must_use]
    pub const fn visited(&self) -> usize {
        self.visited
    }

    /// Past the end of the window, carry on from its start; past the page the
    /// sweep began at, stop with the hand back there.
    fn turn(&mut self) {
        if self.wrapped || self.origin == self.first {
            self.done = true;
            *self.hand = self.origin;
        } else {
            self.wrapped = true;
            *self.hand = self.first;
        }
    }
}

/// The lowest live page of `space` numbered within `from..=last`.
fn next_live<P: PageTable>(space: &AddressSpace<P>, from: u64, last: u64) -> Option<Page> {
    let from = page_at(0, from).ok()?;
    let last = page_at(0, last).ok()?;
    space.next_live(from, last)
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;
    use crate::frame::{Frame, PhysAddr, PAGE_SIZE};
    use crate::vmm::{HostPageTable, MapFlags, VirtAddr};
    use alloc::vec::Vec;
    use tairix_arch_api::mmu::{AddressSpace as HalAddressSpace, MapError, PageFlags};
    use tairix_arch_api::tlb::TlbShootdown;

    /// User RW flags, the shape of an anonymous page.
    const RW_USER: MapFlags = MapFlags::READ.union(MapFlags::WRITE).union(MapFlags::USER);

    /// Room for every cold page a test collects.
    type Cold = ArrayVec<Page, 64>;

    fn page(n: u64) -> Page {
        Page::from_addr(VirtAddr::new(n * PAGE_SIZE as u64)).expect("aligned")
    }

    fn frame(n: u64) -> Frame {
        Frame::containing(PhysAddr::new(0x10_0000 + n * PAGE_SIZE as u64))
    }

    /// A space with `count` mapped anonymous pages at page numbers
    /// `1..=count`.
    fn space_with(count: u64) -> AddressSpace<HostPageTable> {
        let mut space = AddressSpace::new(HostPageTable::new());
        for n in 1..=count {
            space.map(page(n), frame(n), RW_USER).expect("map");
        }
        space
    }

    fn touch(space: &mut AddressSpace<HostPageTable>, pages: impl IntoIterator<Item = u64>) {
        for n in pages {
            space
                .table_mut_for_test()
                .mark_accessed(page(n).start().as_u64());
        }
    }

    fn numbers(cold: &Cold) -> Vec<u64> {
        cold.iter().map(|page| page.number()).collect()
    }

    /// One sweep over `window` collecting up to `limit` cold pages; returns
    /// them and how many pages the sweep visited.
    fn sweep_once(
        scanner: &mut ColdPageScanner,
        space: &mut AddressSpace<HostPageTable>,
        window: RangeInclusive<u64>,
        budget: usize,
        limit: usize,
    ) -> (Vec<u64>, usize) {
        let mut sweep = scanner
            .sweep(space, page(*window.start())..=page(*window.end()), budget)
            .expect("supported");
        let mut cold = Cold::new();
        sweep.collect(space, |_| true, limit, &mut cold);
        (numbers(&cold), sweep.visited())
    }

    #[test]
    fn untouched_pages_are_cold_up_to_the_limit() {
        let mut space = space_with(6);
        let mut scanner = ColdPageScanner::new();
        let (cold, visited) = sweep_once(&mut scanner, &mut space, 1..=6, 64, 4);
        assert_eq!(cold, [1, 2, 3, 4]);
        assert_eq!(visited, 4);
    }

    #[test]
    fn a_referenced_page_gets_a_second_chance_and_is_not_cold() {
        let mut space = space_with(4);
        touch(&mut space, [2, 3]);
        let mut scanner = ColdPageScanner::new();
        let (cold, visited) = sweep_once(&mut scanner, &mut space, 1..=4, 64, 8);
        assert_eq!(cold, [1, 4]);
        assert_eq!(visited, 4);
    }

    #[test]
    fn the_second_chance_clears_the_bit_so_a_still_idle_page_is_cold_next_revolution() {
        let mut space = space_with(3);
        touch(&mut space, [2]);
        let mut scanner = ColdPageScanner::new();
        let (first, _) = sweep_once(&mut scanner, &mut space, 1..=3, 64, 8);
        assert_eq!(first, [1, 3]);
        let (second, _) = sweep_once(&mut scanner, &mut space, 1..=3, 64, 8);
        assert_eq!(second, [1, 2, 3]);
    }

    #[test]
    fn the_clock_hand_keeps_its_place_across_sweeps() {
        let mut space = space_with(5);
        let mut scanner = ColdPageScanner::new();
        let (first, _) = sweep_once(&mut scanner, &mut space, 1..=5, 64, 2);
        assert_eq!(first, [1, 2]);
        assert_eq!(scanner.hand(), 3);
        let (second, _) = sweep_once(&mut scanner, &mut space, 1..=5, 64, 2);
        assert_eq!(second, [3, 4]);
    }

    /// The defect this bound closes: a reclaim over a space whose pages are
    /// all hot walked every one of them on each triggering fault.
    #[test]
    fn a_sweep_visits_at_most_its_budget_when_every_page_is_hot() {
        let mut space = space_with(1000);
        touch(&mut space, 1..=1000);
        let mut scanner = ColdPageScanner::new();
        let (cold, visited) = sweep_once(&mut scanner, &mut space, 1..=1000, 16, 8);
        assert!(cold.is_empty());
        assert_eq!(visited, 16);
        assert_eq!(scanner.hand(), 17);
        assert_eq!(
            space.test_and_clear_accessed(page(17)),
            Ok(true),
            "not aged"
        );
        let (_, visited) = sweep_once(&mut scanner, &mut space, 1..=1000, 16, 8);
        assert_eq!(visited, 16);
        assert_eq!(scanner.hand(), 33, "the next sweep moved on from there");
    }

    /// The defect this walk closes: reclaim built heap lists of the space's
    /// pages, infallibly, on the very path memory pressure drives. It now
    /// allocates nothing, so it completes under a heap refusing every request.
    #[test]
    fn a_sweep_allocates_nothing_and_completes_on_a_refusing_heap() {
        let mut space = space_with(200);
        touch(&mut space, 1..=100);
        let mut scanner = ColdPageScanner::new();
        let ((cold, visited), metering) = tairix_fuzzseed::meter::metered(|| {
            tairix_fuzzseed::meter::refusing_above(0, || {
                let mut sweep = scanner
                    .sweep(&space, page(1)..=page(200), 256)
                    .expect("supported");
                let mut cold = Cold::new();
                sweep.collect(&mut space, |_| true, 8, &mut cold);
                (cold, sweep.visited())
            })
        });
        assert_eq!(numbers(&cold), [101, 102, 103, 104, 105, 106, 107, 108]);
        assert_eq!(visited, 108);
        assert_eq!(metering.allocations, 0, "{metering:?}");
    }

    #[test]
    fn a_sweep_never_visits_a_page_twice() {
        let mut space = space_with(3);
        touch(&mut space, 1..=3);
        let mut scanner = ColdPageScanner::new();
        let mut sweep = scanner
            .sweep(&space, page(1)..=page(3), 64)
            .expect("supported");
        let mut cold = Cold::new();
        sweep.collect(&mut space, |_| true, 8, &mut cold);
        sweep.collect(&mut space, |_| true, 8, &mut cold);
        assert!(
            cold.is_empty(),
            "a page whose bit this sweep cleared is not cold"
        );
        assert_eq!(sweep.visited(), 3);
        assert_eq!(
            scanner.hand(),
            1,
            "a whole revolution leaves the hand where it began"
        );
    }

    #[test]
    fn a_sweep_wraps_from_the_hand_and_stops_where_it_began() {
        let mut space = space_with(5);
        let mut scanner = ColdPageScanner::new();
        sweep_once(&mut scanner, &mut space, 1..=5, 64, 3);
        assert_eq!(scanner.hand(), 4);
        let (cold, visited) = sweep_once(&mut scanner, &mut space, 1..=5, 64, 16);
        assert_eq!(cold, [4, 5, 1, 2, 3]);
        assert_eq!(visited, 5);
        assert_eq!(scanner.hand(), 4);
    }

    #[test]
    fn collecting_in_batches_continues_one_sweep() {
        let mut space = space_with(6);
        let mut scanner = ColdPageScanner::new();
        let mut sweep = scanner
            .sweep(&space, page(1)..=page(6), 64)
            .expect("supported");
        let mut first = Cold::new();
        sweep.collect(&mut space, |_| true, 4, &mut first);
        let mut second = Cold::new();
        sweep.collect(&mut space, |_| true, 4, &mut second);
        let mut third = Cold::new();
        sweep.collect(&mut space, |_| true, 4, &mut third);
        assert_eq!(numbers(&first), [1, 2, 3, 4]);
        assert_eq!(numbers(&second), [5, 6]);
        assert!(third.is_empty());
        assert_eq!(sweep.visited(), 6);
    }

    #[test]
    fn a_full_buffer_ends_the_collection_without_losing_a_page() {
        let mut space = space_with(4);
        let mut scanner = ColdPageScanner::new();
        let mut sweep = scanner
            .sweep(&space, page(1)..=page(4), 64)
            .expect("supported");
        let mut cold = ArrayVec::<Page, 2>::new();
        sweep.collect(&mut space, |_| true, 8, &mut cold);
        assert_eq!(cold.as_slice(), [page(1), page(2)]);
        assert_eq!(scanner.hand(), 3);
    }

    #[test]
    fn an_ineligible_page_counts_against_the_budget_and_keeps_its_bit() {
        let mut space = space_with(4);
        touch(&mut space, 1..=4);
        let mut scanner = ColdPageScanner::new();
        let mut sweep = scanner
            .sweep(&space, page(1)..=page(4), 4)
            .expect("supported");
        let mut cold = Cold::new();
        sweep.collect(&mut space, |page| page.number() % 2 == 0, 8, &mut cold);
        assert_eq!(sweep.visited(), 4);
        assert_eq!(space.test_and_clear_accessed(page(1)), Ok(true));
        assert_eq!(space.test_and_clear_accessed(page(3)), Ok(true));
        assert_eq!(space.test_and_clear_accessed(page(2)), Ok(false), "aged");
    }

    #[test]
    fn pages_outside_the_window_are_never_visited() {
        let mut space = space_with(6);
        touch(&mut space, [1, 6]);
        let mut scanner = ColdPageScanner::new();
        let (cold, visited) = sweep_once(&mut scanner, &mut space, 3..=4, 64, 8);
        assert_eq!(cold, [3, 4]);
        assert_eq!(visited, 2);
        assert_eq!(space.test_and_clear_accessed(page(1)), Ok(true));
        assert_eq!(space.test_and_clear_accessed(page(6)), Ok(true));
    }

    #[test]
    fn a_hand_outside_the_window_starts_at_its_first_page() {
        let mut space = space_with(8);
        let mut scanner = ColdPageScanner::new();
        sweep_once(&mut scanner, &mut space, 1..=8, 64, 7);
        assert_eq!(scanner.hand(), 8);
        let (cold, _) = sweep_once(&mut scanner, &mut space, 2..=4, 64, 1);
        assert_eq!(cold, [2]);
    }

    #[test]
    fn a_zero_budget_or_limit_collects_nothing() {
        let mut space = space_with(3);
        let mut scanner = ColdPageScanner::new();
        assert_eq!(
            sweep_once(&mut scanner, &mut space, 1..=3, 0, 8),
            (Vec::new(), 0)
        );
        assert_eq!(
            sweep_once(&mut scanner, &mut space, 1..=3, 8, 0),
            (Vec::new(), 0)
        );
    }

    #[test]
    fn a_window_with_no_live_page_is_no_cold_pages() {
        let mut space = space_with(2);
        let mut scanner = ColdPageScanner::new();
        let (cold, visited) = sweep_once(&mut scanner, &mut space, 10..=20, 64, 8);
        assert!(cold.is_empty());
        assert_eq!(visited, 0);
    }

    /// A backend that honestly reports no referenced bit: the scanner must
    /// fail closed rather than treat every page as cold.
    #[derive(Default)]
    struct NoTrackTable {
        entries: alloc::collections::BTreeMap<u64, (u64, PageFlags)>,
    }

    impl HalAddressSpace for NoTrackTable {
        fn map_page(&mut self, vaddr: u64, paddr: u64, flags: PageFlags) -> Result<(), MapError> {
            self.entries.insert(vaddr, (paddr, flags));
            Ok(())
        }
        fn translate(&self, vaddr: u64) -> Option<(u64, PageFlags)> {
            self.entries.get(&vaddr).copied()
        }
        fn unmap(&mut self, vaddr: u64) -> Result<u64, MapError> {
            self.entries
                .remove(&vaddr)
                .map(|(p, _)| p)
                .ok_or(MapError::NotMapped)
        }
        fn root_phys(&self) -> u64 {
            PAGE_SIZE as u64
        }
        // Uses the default `access_tracking` (Unsupported) and the default
        // fail-closed `test_and_clear_accessed`.
        unsafe fn activate(&self) {}
    }

    impl TlbShootdown for NoTrackTable {
        fn flush_page(&mut self, _vaddr: u64) {}
        fn flush_range(&mut self, _start_vaddr: u64, _page_count: usize) {}
    }

    #[test]
    fn a_backend_without_a_referenced_bit_fails_closed() {
        let mut space = AddressSpace::new(NoTrackTable::default());
        space.map(page(1), frame(1), RW_USER).expect("map");
        let mut scanner = ColdPageScanner::new();
        assert_eq!(
            scanner.sweep(&space, page(1)..=page(1), 4).err(),
            Some(ColdScanError::Unsupported)
        );
    }
}
