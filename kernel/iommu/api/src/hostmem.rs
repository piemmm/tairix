//! Host memory for the engine's tests and the family crates' register
//! models: a table-frame arena at synthetic physical addresses, and a model
//! of whatever else a unit reads from memory.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;

use tairix_arch_api::{PageTableFrames, TableFrame, PAGE_TABLE_ENTRIES};
use tairix_sync::SpinLock;

use crate::IO_PAGE_SIZE;

type Table = [u64; PAGE_TABLE_ENTRIES];

/// A table-frame source over host memory. Frames get distinct synthetic
/// physical addresses above `base`, and are really freed, so a model that
/// reads a freed table sees it gone.
pub struct HostFrames {
    state: SpinLock<State>,
}

struct State {
    base: u64,
    next: u64,
    live: BTreeMap<u64, *mut Table>,
    /// Blocks handed out, by base: their order and their words.
    blocks: BTreeMap<u64, (u32, *mut [u64])>,
    /// Frames still allowed to be handed out, or [`None`] for no limit.
    budget: Option<usize>,
}

// SAFETY: the raw table pointers are owned boxes the arena alone frees, and
// every access to the map goes through the spinlock.
unsafe impl Send for State {}

impl HostFrames {
    /// An arena handing out frames from physical address `base` up.
    #[must_use]
    pub const fn new(base: u64) -> Self {
        Self {
            state: SpinLock::new(State {
                base,
                next: 0,
                live: BTreeMap::new(),
                blocks: BTreeMap::new(),
                budget: None,
            }),
        }
    }

    /// Let at most `frames` more frames be handed out.
    pub fn limit(&self, frames: usize) {
        self.state.lock().budget = Some(frames);
    }

    /// Frames handed out and not yet freed.
    #[must_use]
    pub fn live(&self) -> usize {
        self.state.lock().live.len()
    }

    /// Blocks handed out and not yet freed.
    #[must_use]
    pub fn live_blocks(&self) -> usize {
        self.state.lock().blocks.len()
    }

    /// Entry `index` of the table at `phys`, as a unit walking memory reads
    /// it, or [`None`] when no live table is there.
    #[must_use]
    pub fn entry(&self, phys: u64, index: usize) -> Option<u64> {
        let state = self.state.lock();
        let table = *state.live.get(&phys)?;
        // SAFETY: the table is live (the arena frees it only under this lock)
        // and `index` is checked against its length.
        (index < PAGE_TABLE_ENTRIES).then(|| unsafe { (*table)[index] })
    }

    /// Store `value` into entry `index` of the table at `phys`, as a unit
    /// writing memory would; `false` when no live table is there.
    pub fn store(&self, phys: u64, index: usize, value: u64) -> bool {
        let state = self.state.lock();
        let Some(&table) = state.live.get(&phys) else {
            return false;
        };
        if index >= PAGE_TABLE_ENTRIES {
            return false;
        }
        // SAFETY: as `entry`; the arena's lock serialises this store against
        // every other access through the arena.
        unsafe { (*table)[index] = value };
        true
    }
}

impl HostFrames {
    /// The 8-byte word at physical `address` in any live table or block, as
    /// a unit reading memory does, or [`None`] where nothing lives there.
    #[must_use]
    pub fn word(&self, address: u64) -> Option<u64> {
        let state = self.state.lock();
        let (words, index) = Self::locate(&state, address)?;
        // SAFETY: the table or block is live (the arena frees it only under
        // this lock) and `index` lies inside it.
        Some(unsafe { *words.add(index) })
    }

    /// Store `value` at physical `address` in a live table or block, as a
    /// unit writing memory does; `false` where nothing lives there.
    pub fn store_word(&self, address: u64, value: u64) -> bool {
        let state = self.state.lock();
        let Some((words, index)) = Self::locate(&state, address) else {
            return false;
        };
        // SAFETY: as `word`; the arena's lock serialises the store.
        unsafe { *words.add(index) = value };
        true
    }

    fn locate(state: &State, address: u64) -> Option<(*mut u64, usize)> {
        if !address.is_multiple_of(8) {
            return None;
        }
        let page = address & !(IO_PAGE_SIZE - 1);
        if let Some(&table) = state.live.get(&page) {
            return Some((
                table.cast::<u64>(),
                usize::try_from((address - page) / 8).ok()?,
            ));
        }
        let (&base, &(_, words)) = state.blocks.range(..=address).next_back()?;
        let index = usize::try_from((address - base) / 8).ok()?;
        (index < words.len()).then_some((words.cast::<u64>(), index))
    }
}

impl Drop for HostFrames {
    fn drop(&mut self) {
        let state = self.state.get_mut();
        for (_, table) in core::mem::take(&mut state.live) {
            // SAFETY: every pointer in the map came from `Box::into_raw` and
            // is freed exactly once, here or in `free_table`.
            drop(unsafe { Box::from_raw(table) });
        }
        for (_, (_, words)) in core::mem::take(&mut state.blocks) {
            // SAFETY: as above, for `alloc_block`'s boxes.
            drop(unsafe { Box::from_raw(words) });
        }
    }
}

impl PageTableFrames for HostFrames {
    fn alloc_table(&self) -> Option<TableFrame> {
        let mut state = self.state.lock();
        if let Some(budget) = state.budget.as_mut() {
            *budget = budget.checked_sub(1)?;
        }
        let phys = state.base + state.next * IO_PAGE_SIZE;
        state.next += 1;
        let table = Box::into_raw(Box::new([0u64; PAGE_TABLE_ENTRIES]));
        state.live.insert(phys, table);
        // SAFETY: the box is live until `free_table(phys)`, which a caller may
        // issue only after its last use of this frame.
        let entries = unsafe { &mut *table };
        Some(TableFrame { phys, entries })
    }

    fn table_at(&self, phys: u64) -> Option<*mut Table> {
        self.state.lock().live.get(&phys).copied()
    }

    fn free_table(&self, phys: u64) {
        if let Some(table) = self.state.lock().live.remove(&phys) {
            // SAFETY: the pointer came from `Box::into_raw` and has just left
            // the map, so it is freed exactly once.
            drop(unsafe { Box::from_raw(table) });
        }
    }

    fn alloc_block(&self, order: u32) -> Option<u64> {
        let frames = 1u64.checked_shl(order)?;
        let words = usize::try_from(frames)
            .ok()?
            .checked_mul(PAGE_TABLE_ENTRIES)?;
        let mut state = self.state.lock();
        if let Some(budget) = state.budget.as_mut() {
            *budget = budget.checked_sub(usize::try_from(frames).ok()?)?;
        }
        // Aligned to its own size, as a buddy block is.
        let next = state.next.next_multiple_of(frames);
        let phys = state.base + next * IO_PAGE_SIZE;
        state.next = next + frames;
        let block = Box::into_raw(alloc::vec![0u64; words].into_boxed_slice());
        state.blocks.insert(phys, (order, block));
        Some(phys)
    }

    fn block_at(&self, phys: u64, order: u32) -> Option<*mut u64> {
        let state = self.state.lock();
        let &(held, words) = state.blocks.get(&phys)?;
        (held == order).then_some(words.cast::<u64>())
    }

    fn free_block(&self, phys: u64, order: u32) {
        let mut state = self.state.lock();
        if state
            .blocks
            .get(&phys)
            .is_some_and(|&(held, _)| held == order)
        {
            if let Some((_, words)) = state.blocks.remove(&phys) {
                // SAFETY: the pointer came from `Box::into_raw` and has just
                // left the map, so it is freed exactly once.
                drop(unsafe { Box::from_raw(words) });
            }
        }
    }
}
