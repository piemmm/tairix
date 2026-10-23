//! Releasing what an unmap cleared only once nothing can still reach it.
//!
//! Clearing a page-table entry does not end every route to its frame. Each
//! CPU the space is active on may still hold the old translation, and the
//! kernel's copy path translates through a snapshot of the space. A frame
//! freed while either still names it can be written after it has gone to its
//! next owner, so an unmap clears the entries, shuts both views — the TLBs
//! through [`SpaceTlb`], the snapshot through [`Retire`] — and only then
//! zeroes and frees the frames.

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{fence, AtomicBool, AtomicU64, Ordering};

use tairix_arch_api::{CpuId, CpuMask, CrossCpuTlbShootdown};

use crate::anon::{zero_frame, AnonError};
use crate::error::AllocError;
use crate::frame::{Frame, PAGE_SIZE};
use crate::phys::PhysMap;
use crate::vmm::{MapFlags, Page};

/// Monotonic allocator of the id every user address space is minted with:
/// never reused within a boot, so neither a CPU's tag for a space, nor the
/// [`Ramzip`](crate::ramzip) tier's entries, nor the audit log can confuse a
/// space with a later one. `u64` never wraps in a realistic boot.
static NEXT_SPACE_ID: AtomicU64 = AtomicU64::new(1);

/// A user address space's standing with the TLBs: an id no other space of the
/// boot bears, the generation its translations are at, and the CPUs running it
/// now.
///
/// The CPUs running it are the ones an invalidation must reach at once. A CPU
/// that left it may still hold its entries under the tag it gave the space,
/// which it never uses for the space again without first discarding them if
/// the generation moved past the one it last caught up with: so a CPU that
/// left need not be reached, only caught up when it returns.
pub struct TlbContext {
    id: u64,
    generation: AtomicU64,
    words: Box<[AtomicU64]>,
    /// A CPU past the set's capacity entered, so the set no longer names
    /// every holder and a shootdown must reach all CPUs. Never cleared.
    overflowed: AtomicBool,
}

impl TlbContext {
    /// A fresh space's context, with room for dense CPU ids below `cpus`,
    /// none of them running it.
    ///
    /// # Errors
    ///
    /// [`AllocError::OutOfMemory`] when the storage cannot be allocated.
    pub fn new(cpus: usize) -> Result<Self, AllocError> {
        let len = CpuMask::words_for(cpus);
        let mut words = Vec::new();
        words
            .try_reserve_exact(len)
            .map_err(|_| AllocError::OutOfMemory)?;
        words.resize_with(len, || AtomicU64::new(0));
        Ok(Self {
            id: NEXT_SPACE_ID.fetch_add(1, Ordering::Relaxed),
            generation: AtomicU64::new(0),
            words: words.into_boxed_slice(),
            overflowed: AtomicBool::new(false),
        })
    }

    /// The space's id, which no other space of the boot bears.
    #[must_use]
    pub const fn id(&self) -> u64 {
        self.id
    }

    /// The generation the space's translations are at, which every
    /// invalidation of them advances.
    ///
    /// Read after [`enter`](Self::enter)'s fence, so a CPU coming to run the
    /// space either sees an invalidation's advance or is reached by it.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    /// Advance the generation, before the CPUs that may cache the stale
    /// translations are read, answering the generation it advanced from.
    pub(crate) fn advance(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::SeqCst)
    }

    /// Record that `cpu` is about to load this space's root.
    ///
    /// Fenced before the load, so an unmap that clears an entry and then
    /// reads this set either sees `cpu` or `cpu` walks the cleared entry and
    /// reads the generation the unmap advanced.
    pub fn enter(&self, cpu: CpuId) {
        match self.slot(cpu) {
            Some((word, bit)) => {
                word.fetch_or(bit, Ordering::SeqCst);
            }
            None => self.overflowed.store(true, Ordering::SeqCst),
        }
        fence(Ordering::SeqCst);
    }

    /// Record that `cpu` has loaded another root and runs this space no more.
    pub fn leave(&self, cpu: CpuId) {
        if let Some((word, bit)) = self.slot(cpu) {
            word.fetch_and(!bit, Ordering::Release);
        }
    }

    /// Whether no CPU runs this space.
    #[must_use]
    pub fn is_idle(&self) -> bool {
        !self.overflowed.load(Ordering::Acquire) && self.mask().is_empty()
    }

    fn mask(&self) -> CpuMask<'_> {
        CpuMask::new(&self.words)
    }

    fn slot(&self, cpu: CpuId) -> Option<(&AtomicU64, u64)> {
        let (word, bit) = CpuMask::slot(cpu);
        Some((self.words.get(word)?, bit))
    }
}

/// How a space's cleared translations are discarded on the CPUs other than
/// the one that cleared them.
#[derive(Clone)]
pub struct SpaceTlb {
    context: Arc<TlbContext>,
    remote: Option<&'static (dyn CrossCpuTlbShootdown + Sync)>,
    credit: Option<OwnFlush>,
}

/// Told, on the CPU that cleared and discarded a space's entries itself, the
/// generation its invalidation advanced the space from.
pub type OwnFlush = fn(&TlbContext, u64);

impl SpaceTlb {
    /// A reach over `context`'s CPUs, invalidating through `remote`, which is
    /// [`None`] only on a port with no TLB.
    #[must_use]
    pub fn new(
        context: TlbContext,
        remote: Option<&'static (dyn CrossCpuTlbShootdown + Sync)>,
    ) -> Self {
        Self {
            context: Arc::new(context),
            remote,
            credit: None,
        }
    }

    /// This reach, telling `credit` of every invalidation on the CPU that made
    /// it, so a CPU that discarded the entries itself need not discard the
    /// space's tag when it next runs the space.
    #[must_use]
    pub fn crediting(self, credit: OwnFlush) -> Self {
        Self {
            credit: Some(credit),
            ..self
        }
    }

    /// The space's standing with the TLBs, which the dispatcher keeps current.
    #[must_use]
    pub fn context(&self) -> &Arc<TlbContext> {
        &self.context
    }

    /// Discard `pages` pages from `base` on every other CPU that may cache
    /// them, returning once none running the space can, and advancing the
    /// space's generation so every CPU that left it discards them before it
    /// runs the space again. The calling CPU has flushed its own.
    pub(crate) fn shoot_remote(&self, base: u64, pages: u64) {
        if pages == 0 {
            return;
        }
        let from = self.context.advance();
        if let Some(credit) = self.credit {
            credit(&self.context, from);
        }
        let Some(remote) = self.remote else {
            return;
        };
        // Orders the cleared entries and the advance before the read of who
        // may cache them.
        fence(Ordering::SeqCst);
        let pages = usize::try_from(pages).unwrap_or(usize::MAX);
        if self.context.overflowed.load(Ordering::Acquire) {
            remote.shootdown_range(base, pages);
        } else {
            remote.shootdown_user_range(self.context.mask(), base, pages);
        }
    }
}

/// A remote reach for host tests that hands each invalidation it is asked
/// for to its recorder as `(base, pages)`, `'static` as a port's handle is.
///
/// A user range reaches the recorder only when the mask names a CPU, as it
/// reaches a port's other CPUs only then.
#[cfg(any(test, feature = "host-tests"))]
pub struct RecordedRemote(pub fn(u64, usize));

#[cfg(any(test, feature = "host-tests"))]
impl CrossCpuTlbShootdown for RecordedRemote {
    fn shootdown_page(&self, vaddr: u64) {
        (self.0)(vaddr, 1);
    }

    fn shootdown_range(&self, start_vaddr: u64, page_count: usize) {
        (self.0)(start_vaddr, page_count);
    }

    fn shootdown_user_range(&self, cpus: CpuMask<'_>, start_vaddr: u64, page_count: usize) {
        if !cpus.is_empty() {
            (self.0)(start_vaddr, page_count);
        }
    }
}

/// A view of user pages that outlives their page-table entries.
pub trait Retire {
    /// `pages` pages from `base` lost their entries: return only once this
    /// view can no longer reach them.
    fn retire(&mut self, base: u64, pages: u64);

    /// [`Self::retire`] for each `(base, pages)` run, which a view behind a
    /// lock shuts under one hold.
    fn retire_runs(&mut self, runs: &mut dyn Iterator<Item = (u64, u64)>) {
        for (base, pages) in runs {
            self.retire(base, pages);
        }
    }

    /// `page`, retired, is mapped again to `frame` with `flags`: an unmap its
    /// caller undid before releasing the frame.
    fn restore(&mut self, page: Page, frame: Frame, flags: MapFlags);
}

/// Pages no view outside the page table ever saw: a mapping undone before
/// the call that made it returned.
pub struct Unpublished;

impl Retire for Unpublished {
    fn retire(&mut self, _base: u64, _pages: u64) {}

    fn restore(&mut self, _page: Page, _frame: Frame, _flags: MapFlags) {}
}

/// Frames held between one release step and the next.
///
/// A step pays one remote shootdown and one snapshot retirement for the
/// batch, so the batch is what amortises them; 64 frames is 1 KiB of stack.
const RETIRE_BATCH: usize = 64;

/// The frames an unmap cleared, held until [`SpaceTlb`] and [`Retire`] have
/// shut every view of them, then zeroed and released.
///
/// The pages held are ascending pages of one range the caller releases whole,
/// so a step shoots down the span from the lowest to the highest held — a page
/// in between that held no frame was never resident, and the range is going
/// anyway — while the snapshot retires each run of held pages alone, so a
/// sparse release costs it only its resident pages. A frame the direct map
/// cannot reach is kept rather than released unscrubbed. Dropping it runs the
/// last step, so an early return neither strands a frame nor releases one
/// unshut.
pub(crate) struct Retiring<'a> {
    tlb: Option<SpaceTlb>,
    retire: &'a mut dyn Retire,
    physmap: &'a dyn PhysMap,
    release: &'a mut dyn FnMut(Frame),
    held: [(u64, Frame); RETIRE_BATCH],
    count: usize,
    unscrubbed: bool,
}

impl<'a> Retiring<'a> {
    /// Hold frames for release through `release`, shutting `tlb`'s CPUs and
    /// `retire` first. A `tlb` of [`None`] reaches no other CPU: the space is
    /// active nowhere, or the port has no TLB.
    pub(crate) fn new(
        tlb: Option<SpaceTlb>,
        retire: &'a mut dyn Retire,
        physmap: &'a dyn PhysMap,
        release: &'a mut dyn FnMut(Frame),
    ) -> Self {
        Self {
            tlb,
            retire,
            physmap,
            release,
            held: [(0, Frame(0)); RETIRE_BATCH],
            count: 0,
            unscrubbed: false,
        }
    }

    /// Hold `frame`, whose entry at `va` has just been cleared and flushed on
    /// this CPU.
    pub(crate) fn hold(&mut self, va: u64, frame: Frame) {
        let full = self.count == RETIRE_BATCH;
        if full || self.count != 0 && va <= self.held[self.count - 1].0 {
            self.step();
        }
        self.held[self.count] = (va, frame);
        self.count += 1;
    }

    /// Shut every view of the held pages, then zero and release their frames.
    fn step(&mut self) {
        // Taken first, so no step can ever release a frame twice.
        let count = core::mem::take(&mut self.count);
        let Some((&(first, _), &(last, _))) =
            self.held[..count].first().zip(self.held[..count].last())
        else {
            return;
        };
        if let Some(tlb) = &self.tlb {
            tlb.shoot_remote(first, (last - first) / PAGE_SIZE as u64 + 1);
        }
        let page = PAGE_SIZE as u64;
        let mut runs = self.held[..count]
            .chunk_by(|a, b| b.0 == a.0 + page)
            .map(|run| (run[0].0, run.len() as u64));
        self.retire.retire_runs(&mut runs);
        for &(_, frame) in &self.held[..count] {
            if zero_frame(self.physmap, frame).is_ok() {
                (self.release)(frame);
            } else {
                self.unscrubbed = true;
            }
        }
    }

    /// Release everything still held.
    ///
    /// # Errors
    ///
    /// [`AnonError::PhysUnmapped`] when a frame could not be zeroed; it was
    /// kept rather than released.
    pub(crate) fn finish(mut self) -> Result<(), AnonError> {
        self.step();
        if self.unscrubbed {
            Err(AnonError::PhysUnmapped)
        } else {
            Ok(())
        }
    }
}

impl Drop for Retiring<'_> {
    fn drop(&mut self) {
        self.step();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    extern crate std;
    use std::cell::RefCell;
    use std::vec::Vec;

    use crate::frame::PhysAddr;
    use crate::phys::SimPhysMap;

    const SIM_BASE_FRAME: usize = 16;
    const SIM_FRAMES: usize = 8;

    fn sim() -> SimPhysMap {
        SimPhysMap::new(
            PhysAddr::new((SIM_BASE_FRAME * PAGE_SIZE) as u64),
            SIM_FRAMES * PAGE_SIZE,
        )
    }

    /// What a release step did, in order.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Event {
        Remote(u64, usize),
        Retired(u64, u64),
        Released(Frame),
    }

    #[derive(Default)]
    struct Log(RefCell<Vec<Event>>);

    impl Log {
        fn push(&self, event: Event) {
            self.0.borrow_mut().push(event);
        }
        fn take(&self) -> Vec<Event> {
            core::mem::take(&mut *self.0.borrow_mut())
        }
    }

    struct Recorder<'a>(&'a Log);

    impl Retire for Recorder<'_> {
        fn retire(&mut self, base: u64, pages: u64) {
            self.0.push(Event::Retired(base, pages));
        }

        fn restore(&mut self, _page: Page, _frame: Frame, _flags: MapFlags) {
            unreachable!("a batch never undoes an unmap");
        }
    }

    std::thread_local! {
        static REMOTE: RefCell<Vec<Event>> = const { RefCell::new(Vec::new()) };
    }

    /// The remote reach's log lives in a thread-local, because the reach is
    /// `'static` as the real handle is.
    fn record_remote(base: u64, pages: usize) {
        REMOTE.with(|log| log.borrow_mut().push(Event::Remote(base, pages)));
    }

    static REMOTE_LOG: RecordedRemote = RecordedRemote(record_remote);

    fn reach(active: &[CpuId]) -> SpaceTlb {
        let cpus = TlbContext::new(4).expect("a small set allocates");
        for &cpu in active {
            cpus.enter(cpu);
        }
        SpaceTlb::new(cpus, Some(&REMOTE_LOG))
    }

    fn remote_events() -> Vec<Event> {
        REMOTE.with(|log| core::mem::take(&mut *log.borrow_mut()))
    }

    fn page_va(n: u64) -> u64 {
        0x4000_0000 + n * PAGE_SIZE as u64
    }

    #[test]
    fn a_frame_is_released_only_after_both_views_are_shut() {
        let log = Log::default();
        let physmap = sim();
        let mut retire = Recorder(&log);
        let mut release = |frame| log.push(Event::Released(frame));
        let mut retiring = Retiring::new(Some(reach(&[1])), &mut retire, &physmap, &mut release);
        retiring.hold(page_va(0), Frame(SIM_BASE_FRAME));
        retiring.hold(page_va(2), Frame(SIM_BASE_FRAME + 1));
        assert!(log.take().is_empty(), "nothing is released while held");
        assert_eq!(retiring.finish(), Ok(()));
        assert_eq!(
            remote_events(),
            [Event::Remote(page_va(0), 3)],
            "one shootdown spans the batch"
        );
        assert_eq!(
            log.take(),
            [
                Event::Retired(page_va(0), 1),
                Event::Retired(page_va(2), 1),
                Event::Released(Frame(SIM_BASE_FRAME)),
                Event::Released(Frame(SIM_BASE_FRAME + 1)),
            ],
            "the snapshot retires each resident page, never the gap"
        );
    }

    #[test]
    fn a_released_frame_is_zeroed_first() {
        let log = Log::default();
        let physmap = sim();
        let frame = Frame(SIM_BASE_FRAME + 2);
        let ptr = physmap
            .translate(frame.start(), PAGE_SIZE)
            .expect("the frame is in the window");
        // SAFETY: the simulated window is the test's own buffer and the frame
        // lies inside it, as the translation just proved.
        unsafe { core::ptr::write_bytes(ptr.as_ptr(), 0xA5, PAGE_SIZE) };
        let mut retire = Recorder(&log);
        let mut release = |released: Frame| {
            // SAFETY: as above; the frame is unreachable from anywhere else.
            let page = unsafe { core::slice::from_raw_parts(ptr.as_ptr(), PAGE_SIZE) };
            assert!(page.iter().all(|&byte| byte == 0), "released dirty");
            log.push(Event::Released(released));
        };
        let mut retiring = Retiring::new(None, &mut retire, &physmap, &mut release);
        retiring.hold(page_va(0), frame);
        drop(retiring);
        assert_eq!(log.take().last(), Some(&Event::Released(frame)));
    }

    #[test]
    fn a_full_batch_or_a_page_out_of_order_releases_what_is_held() {
        let log = Log::default();
        let physmap = sim();
        let mut retire = Recorder(&log);
        let mut release = |_frame| {};
        let mut retiring = Retiring::new(None, &mut retire, &physmap, &mut release);
        for n in 0..RETIRE_BATCH as u64 {
            retiring.hold(page_va(n), Frame(SIM_BASE_FRAME));
        }
        assert!(log.take().is_empty(), "the batch is full but holds");
        retiring.hold(page_va(RETIRE_BATCH as u64), Frame(SIM_BASE_FRAME));
        assert_eq!(
            log.take(),
            [Event::Retired(page_va(0), RETIRE_BATCH as u64)]
        );
        retiring.hold(page_va(1), Frame(SIM_BASE_FRAME));
        assert_eq!(
            log.take(),
            [Event::Retired(page_va(RETIRE_BATCH as u64), 1)]
        );
        drop(retiring);
        assert_eq!(log.take(), [Event::Retired(page_va(1), 1)]);
    }

    #[test]
    fn a_frame_the_direct_map_cannot_reach_is_kept_and_reported() {
        let log = Log::default();
        let physmap = sim();
        let mut retire = Recorder(&log);
        let mut release = |frame| log.push(Event::Released(frame));
        let mut retiring = Retiring::new(None, &mut retire, &physmap, &mut release);
        retiring.hold(page_va(0), Frame(SIM_BASE_FRAME + SIM_FRAMES));
        assert_eq!(retiring.finish(), Err(AnonError::PhysUnmapped));
        assert_eq!(log.take(), [Event::Retired(page_va(0), 1)]);
    }

    #[test]
    fn a_space_active_only_here_reaches_no_other_cpu() {
        let log = Log::default();
        let physmap = sim();
        let mut retire = Recorder(&log);
        let mut release = |_frame| {};
        let mut retiring = Retiring::new(Some(reach(&[])), &mut retire, &physmap, &mut release);
        retiring.hold(page_va(0), Frame(SIM_BASE_FRAME));
        drop(retiring);
        assert!(remote_events().is_empty());
    }

    #[test]
    fn a_cpu_past_the_capacity_widens_every_shootdown_to_all_cpus() {
        let cpus = TlbContext::new(2).expect("a small set allocates");
        cpus.enter(7_000);
        assert!(!cpus.is_idle(), "an unrecorded holder is not idle");
        cpus.leave(7_000);
        assert!(!cpus.is_idle(), "the widening is permanent");
        let tlb = SpaceTlb::new(cpus, Some(&REMOTE_LOG));
        tlb.shoot_remote(page_va(3), 2);
        assert_eq!(remote_events(), [Event::Remote(page_va(3), 2)]);
    }

    #[test]
    fn a_cpu_is_active_from_enter_to_leave() {
        let cpus = TlbContext::new(130).expect("three words allocate");
        assert!(cpus.is_idle());
        cpus.enter(129);
        cpus.enter(0);
        assert_eq!(cpus.mask().iter().collect::<Vec<_>>(), [0, 129]);
        cpus.leave(129);
        cpus.leave(0);
        assert!(cpus.is_idle());
    }

    std::thread_local! {
        static CREDITED: RefCell<Vec<(u64, u64)>> = const { RefCell::new(Vec::new()) };
    }

    fn record_credit(context: &TlbContext, from: u64) {
        CREDITED.with(|credited| credited.borrow_mut().push((context.id(), from)));
    }

    /// Every invalidation tells the CPU that made it the generation it
    /// advanced the space from; an empty one tells it nothing.
    #[test]
    fn an_invalidation_credits_the_cpu_that_made_it_with_the_generation_it_left() {
        let tlb =
            SpaceTlb::new(TlbContext::new(1).expect("a context"), None).crediting(record_credit);
        let id = tlb.context().id();
        tlb.shoot_remote(page_va(0), 0);
        tlb.shoot_remote(page_va(0), 2);
        tlb.shoot_remote(page_va(4), 1);
        assert_eq!(CREDITED.with(RefCell::take), [(id, 0), (id, 1)]);
        assert_eq!(tlb.context().generation(), 2);
    }

    #[test]
    fn a_set_for_no_cpus_allocates_nothing_and_widens_on_any_entry() {
        let cpus = TlbContext::new(0).expect("an empty set needs no storage");
        assert!(cpus.is_idle());
        cpus.enter(0);
        assert!(!cpus.is_idle());
    }

    #[test]
    fn every_context_has_an_id_no_other_bears() {
        let ids: Vec<u64> = (0..64)
            .map(|_| TlbContext::new(1).expect("a context allocates").id())
            .collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len());
        assert!(ids.iter().all(|id| *id != 0), "no space is id nought");
    }

    /// An invalidation advances the generation whether or not a CPU runs the
    /// space, so one that left catches up when it returns; an empty one does
    /// not.
    #[test]
    fn a_shootdown_advances_the_generation_every_cpu_that_left_catches_up_on() {
        let tlb = reach(&[]);
        let before = tlb.context().generation();
        tlb.shoot_remote(page_va(0), 0);
        assert_eq!(
            tlb.context().generation(),
            before,
            "nothing was invalidated"
        );
        tlb.shoot_remote(page_va(0), 3);
        assert_eq!(tlb.context().generation(), before + 1);
        assert!(
            remote_events().is_empty(),
            "no CPU runs it, none is reached"
        );
        let unreached = SpaceTlb::new(TlbContext::new(1).expect("a context"), None);
        unreached.shoot_remote(page_va(0), 1);
        assert_eq!(
            unreached.context().generation(),
            1,
            "a port with no reach still advances"
        );
    }

    /// A CPU coming to run a space while an unmap of it shoots down: the
    /// unmap reaches the CPU, or the CPU reads the generation the unmap
    /// advanced and discards its tag on the way in, so no stale entry is
    /// kept. The store-buffering pairing between `enter` and `shoot_remote`,
    /// run on real threads because loom cannot build the kernel crate graph.
    #[test]
    fn a_cpu_entering_a_space_an_unmap_races_is_reached_or_catches_up() {
        static REACHED: AtomicU64 = AtomicU64::new(0);
        fn reached(_base: u64, _pages: usize) {
            REACHED.fetch_add(1, Ordering::SeqCst);
        }
        static RACED: RecordedRemote = RecordedRemote(reached);

        // The interpreter runs a round in the time the host runs thousands.
        const ROUNDS: u64 = if cfg!(miri) { 64 } else { 400_000 };
        let tlb = Arc::new(SpaceTlb::new(
            TlbContext::new(2).expect("a context allocates"),
            Some(&RACED),
        ));
        let round = Arc::new(AtomicU64::new(0));
        let read = Arc::new(AtomicU64::new(0));
        let done = Arc::new(AtomicU64::new(0));
        let switcher = {
            let (tlb, round, read, done) = (tlb.clone(), round.clone(), read.clone(), done.clone());
            std::thread::spawn(move || {
                for r in 1..=ROUNDS {
                    while round.load(Ordering::Acquire) != r {
                        core::hint::spin_loop();
                    }
                    tlb.context().enter(1);
                    read.store(tlb.context().generation(), Ordering::Release);
                    done.fetch_add(1, Ordering::AcqRel);
                }
            })
        };
        for r in 1..=ROUNDS {
            let generation = tlb.context().generation();
            let reaches = REACHED.load(Ordering::SeqCst);
            round.store(r, Ordering::Release);
            tlb.shoot_remote(page_va(0), 1);
            while done.load(Ordering::Acquire) != r {
                core::hint::spin_loop();
            }
            assert!(
                REACHED.load(Ordering::SeqCst) > reaches
                    || read.load(Ordering::Acquire) > generation,
                "round {r}: the unmap missed the CPU and the CPU missed the unmap"
            );
            tlb.context().leave(1);
        }
        switcher.join().expect("the switcher finished");
    }
}
