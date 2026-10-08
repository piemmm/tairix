//! Host stand-ins for the kernel's frame source, for the tests of drivers
//! whose hardware reads tables from memory.

use std::alloc::{alloc_zeroed, dealloc, Layout};
use std::sync::Mutex;
use std::vec::Vec;

use tairix_arch_api::{PageTableFrames, TableFrame, PAGE_TABLE_ENTRIES};

/// Zeroed, size-aligned blocks from the host heap; a block's address is its
/// "physical" one.
#[derive(Default)]
pub(crate) struct HostBlocks(Mutex<Vec<(usize, u32, *mut u8)>>);

// SAFETY: every pointer is a live heap block only the mutex hands out, and
// the tests touch a block's memory through one owner at a time.
unsafe impl Sync for HostBlocks {}
// SAFETY: as above.
unsafe impl Send for HostBlocks {}

fn layout(order: u32) -> Layout {
    let bytes = 4096usize << order;
    Layout::from_size_align(bytes, bytes).unwrap()
}

impl HostBlocks {
    /// Blocks handed out and not yet freed.
    pub(crate) fn live(&self) -> usize {
        self.0.lock().unwrap().len()
    }

    /// The word at `phys`, which a live block holds.
    pub(crate) fn read(&self, phys: u64) -> u64 {
        let blocks = self.0.lock().unwrap();
        let address = usize::try_from(phys).unwrap();
        let &(base, _, ptr) = blocks
            .iter()
            .find(|&&(base, order, _)| (base..base + (4096 << order)).contains(&address))
            .unwrap_or_else(|| panic!("the service read {phys:#x}, which no table holds"));
        // SAFETY: `address` lies in the live block at `base`; a word the
        // service reads need not be aligned in the host's allocation.
        unsafe { ptr.add(address - base).cast::<u64>().read_unaligned() }
    }
}

impl PageTableFrames for HostBlocks {
    fn alloc_table(&self) -> Option<TableFrame> {
        let phys = self.alloc_block(0)?;
        let ptr = self.block_at(phys, 0)?;
        // SAFETY: a fresh, zeroed, frame-aligned block of 512 words that only
        // this frame names.
        let entries = unsafe { &mut *ptr.cast::<[u64; PAGE_TABLE_ENTRIES]>() };
        Some(TableFrame { phys, entries })
    }
    fn table_at(&self, phys: u64) -> Option<*mut [u64; PAGE_TABLE_ENTRIES]> {
        self.block_at(phys, 0).map(<*mut u64>::cast)
    }
    fn free_table(&self, phys: u64) {
        self.free_block(phys, 0);
    }
    fn alloc_block(&self, order: u32) -> Option<u64> {
        // SAFETY: a non-zero size, power-of-two alignment layout.
        let ptr = unsafe { alloc_zeroed(layout(order)) };
        if ptr.is_null() {
            return None;
        }
        self.0.lock().unwrap().push((ptr as usize, order, ptr));
        Some(ptr as u64)
    }
    fn block_at(&self, phys: u64, order: u32) -> Option<*mut u64> {
        let blocks = self.0.lock().unwrap();
        blocks
            .iter()
            .find(|&&(base, size, _)| base as u64 == phys && size == order)
            .map(|&(_, _, ptr)| ptr.cast())
    }
    fn free_block(&self, phys: u64, order: u32) {
        let mut blocks = self.0.lock().unwrap();
        let index = blocks
            .iter()
            .position(|&(base, size, _)| base as u64 == phys && size == order)
            .expect("only a block handed out is freed");
        let (_, _, ptr) = blocks.swap_remove(index);
        // SAFETY: `ptr` came from `alloc_zeroed` with this layout and is
        // freed once, now no table names it.
        unsafe { dealloc(ptr, layout(order)) };
    }
}

impl Drop for HostBlocks {
    fn drop(&mut self) {
        for &(_, order, ptr) in self.0.get_mut().unwrap().iter() {
            // SAFETY: as `free_block`, at the end of the test that drew it.
            unsafe { dealloc(ptr, layout(order)) };
        }
    }
}
