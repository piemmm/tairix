//! The memory a family's tables live in, and the ordering that makes what the
//! CPU wrote there visible to the unit's walker.

use core::sync::atomic::{AtomicU64, Ordering};

use tairix_arch_api::{PageTableFrames, PAGE_TABLE_ENTRIES};

use crate::{IommuError, TableCoherence};

/// A table frame its [`TableMemory`] allocated: 4 KiB of
/// [`PAGE_TABLE_ENTRIES`] entries, named by its physical address.
///
/// Only [`TableMemory::alloc`] makes one, so a handle always names a table
/// of that memory's own, which is what makes reading and writing it through
/// [`TableMemory`] sound.
#[derive(Debug, Eq, PartialEq)]
pub struct Table {
    phys: u64,
}

impl Table {
    /// The physical address the unit's entries name the table by.
    #[must_use]
    pub const fn phys(&self) -> u64 {
        self.phys
    }
}

/// A block of physically contiguous table frames its [`TableMemory`]
/// allocated, for a structure the unit reads as one: an interrupt remapping
/// table, a device table, a queue.
///
/// Only [`TableMemory::alloc_block`] makes one, so a handle always names a
/// block of that memory's own.
#[derive(Debug, Eq, PartialEq)]
pub struct Block {
    phys: u64,
    order: u32,
}

impl Block {
    /// The physical address the unit is told the block is at.
    #[must_use]
    pub const fn phys(&self) -> u64 {
        self.phys
    }

    /// The block holds `2^order` frames.
    #[must_use]
    pub const fn order(&self) -> u32 {
        self.order
    }

    /// The 8-byte words it holds.
    #[must_use]
    pub const fn words(&self) -> usize {
        PAGE_TABLE_ENTRIES << self.order
    }
}

/// Where a family's tables come from, and how the unit's walker is made to see
/// what the CPU wrote to them.
#[derive(Copy, Clone)]
pub struct TableMemory<'f> {
    frames: &'f dyn PageTableFrames,
    coherence: Option<&'f dyn TableCoherence>,
}

impl<'f> TableMemory<'f> {
    /// Tables drawn from `frames`, written back through `coherence` for a
    /// walker that does not snoop the CPU's caches.
    #[must_use]
    pub const fn new(
        frames: &'f dyn PageTableFrames,
        coherence: Option<&'f dyn TableCoherence>,
    ) -> Self {
        Self { frames, coherence }
    }

    /// A zeroed table whose zeroes reach the walker before any later store
    /// can link it.
    ///
    /// # Errors
    ///
    /// [`IommuError::Exhausted`] when no frame can be had.
    pub fn alloc(&self) -> Result<Table, IommuError> {
        let phys = self.frames.alloc_table().ok_or(IommuError::Exhausted)?.phys;
        self.publish_at(phys, 0, PAGE_TABLE_ENTRIES);
        tairix_dma_barrier::dma_wmb();
        Ok(Table { phys })
    }

    /// Give `table` back to its frames, consuming the handle. The unit must
    /// walk it no more.
    // Taken by value on purpose: a freed table handle must not be reusable,
    // which is the ownership contract the by-value take enforces.
    #[allow(clippy::needless_pass_by_value)]
    pub fn free(&self, table: Table) {
        self.frames.free_table(table.phys);
    }

    /// Entry `index` of `table`.
    ///
    /// # Errors
    ///
    /// [`IommuError::Hardware`] for an index past the table, or a frame its
    /// source no longer reaches.
    pub fn read(&self, table: &Table, index: usize) -> Result<u64, IommuError> {
        // SAFETY: a `Table` names a table this memory allocated.
        unsafe { self.read_at(table.phys, index) }
    }

    /// Store `value` whole at entry `index` of `table`, so the walker never
    /// reads half of one. A walker that does not snoop sees it once
    /// [`Self::publish`] covers it.
    ///
    /// # Errors
    ///
    /// As [`Self::read`].
    pub fn write(&self, table: &Table, index: usize, value: u64) -> Result<(), IommuError> {
        // SAFETY: as `read`.
        unsafe { self.write_at(table.phys, index, value) }
    }

    /// Write entries `[first, first + count)` of `table` back to memory for a
    /// walker that does not snoop.
    pub fn publish(&self, table: &Table, first: usize, count: usize) {
        self.publish_at(table.phys, first, count);
    }

    /// A zeroed block of `2^order` contiguous frames whose zeroes reach the
    /// unit before any later store can link it.
    ///
    /// # Errors
    ///
    /// [`IommuError::Exhausted`] when no such block can be had.
    pub fn alloc_block(&self, order: u32) -> Result<Block, IommuError> {
        let phys = self
            .frames
            .alloc_block(order)
            .ok_or(IommuError::Exhausted)?;
        let block = Block { phys, order };
        self.publish_block(&block, 0, block.words());
        tairix_dma_barrier::dma_wmb();
        Ok(block)
    }

    /// Give `block` back to its frames, consuming the handle. The unit must
    /// read it no more.
    // Taken by value on purpose, as `free` is.
    #[allow(clippy::needless_pass_by_value)]
    pub fn free_block(&self, block: Block) {
        self.frames.free_block(block.phys, block.order);
    }

    /// Word `index` of `block`.
    ///
    /// # Errors
    ///
    /// [`IommuError::Hardware`] for an index past the block, or a block its
    /// source no longer reaches.
    pub fn read_block(&self, block: &Block, index: usize) -> Result<u64, IommuError> {
        let words = self.block_words(block, index)?;
        // SAFETY: a `Block` names a live block this memory allocated, which
        // no reference aliases, and `index` lies inside it.
        Ok(unsafe { word(words.add(index)) }.load(Ordering::Relaxed))
    }

    /// Store `value` whole at word `index` of `block`. A unit that does not
    /// snoop sees it once [`Self::publish_block`] covers it.
    ///
    /// # Errors
    ///
    /// As [`Self::read_block`].
    pub fn write_block(&self, block: &Block, index: usize, value: u64) -> Result<(), IommuError> {
        let words = self.block_words(block, index)?;
        // SAFETY: as `read_block`.
        unsafe { word(words.add(index)) }.store(value, Ordering::Relaxed);
        Ok(())
    }

    /// Write words `[first, first + count)` of `block` back to memory for a
    /// unit that does not snoop.
    pub fn publish_block(&self, block: &Block, first: usize, count: usize) {
        self.publish_at(block.phys, first, count);
    }

    fn block_words(&self, block: &Block, index: usize) -> Result<*mut u64, IommuError> {
        if index >= block.words() {
            return Err(IommuError::Hardware);
        }
        self.frames
            .block_at(block.phys, block.order)
            .ok_or(IommuError::Hardware)
    }

    /// Entry `index` of the table at `phys`.
    ///
    /// # Safety
    ///
    /// `phys` names a table this memory allocated and has not freed.
    pub(crate) unsafe fn read_at(&self, phys: u64, index: usize) -> Result<u64, IommuError> {
        let entries = self.entries(phys, index)?;
        // SAFETY: the caller's contract makes `entries` this memory's own view
        // of a live table, which no reference aliases, and `index` is inside it.
        Ok(unsafe { word(core::ptr::addr_of_mut!((*entries)[index])) }.load(Ordering::Relaxed))
    }

    /// Store `value` at entry `index` of the table at `phys`.
    ///
    /// # Safety
    ///
    /// As [`Self::read_at`].
    pub(crate) unsafe fn write_at(
        &self,
        phys: u64,
        index: usize,
        value: u64,
    ) -> Result<(), IommuError> {
        let entries = self.entries(phys, index)?;
        // SAFETY: as `read_at`.
        unsafe { word(core::ptr::addr_of_mut!((*entries)[index])) }.store(value, Ordering::Relaxed);
        Ok(())
    }

    pub(crate) fn publish_at(&self, phys: u64, first: usize, count: usize) {
        if let Some(coherence) = self.coherence {
            coherence.write_back(phys + (first as u64) * 8, count * 8);
        }
    }

    /// Give the table at `phys` back. The caller allocated it here and the
    /// unit walks it no more.
    pub(crate) fn free_at(&self, phys: u64) {
        self.frames.free_table(phys);
    }

    /// Give the block of `2^order` frames at `phys` back, as
    /// [`Self::free_at`] does a table.
    pub(crate) fn free_block_at(&self, phys: u64, order: u32) {
        self.frames.free_block(phys, order);
    }

    fn entries(
        &self,
        phys: u64,
        index: usize,
    ) -> Result<*mut [u64; PAGE_TABLE_ENTRIES], IommuError> {
        if index >= PAGE_TABLE_ENTRIES {
            return Err(IommuError::Hardware);
        }
        self.frames.table_at(phys).ok_or(IommuError::Hardware)
    }
}

/// The table word at `word`, reached atomically: a CPU access never races
/// another into a data race, and a walker never reads half of a store.
///
/// # Safety
///
/// `word` is aligned and valid for the lifetime the caller uses the result
/// for, and every CPU access to it is made through this.
unsafe fn word<'a>(word: *mut u64) -> &'a AtomicU64 {
    // SAFETY: the caller's contract is `from_ptr`'s.
    unsafe { AtomicU64::from_ptr(word) }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use crate::hostmem::HostFrames;

    /// Writers of one word on two CPUs race benignly: every access is
    /// atomic, so the word holds one of their values whole, never a tear.
    #[test]
    fn writers_of_one_word_on_two_cpus_race_benignly() {
        let frames = HostFrames::new(0x1_0000_0000);
        let memory = TableMemory::new(&frames, None);
        let block = memory.alloc_block(0).unwrap();
        let values = [0x1111_1111_1111_1111_u64, 0x2222_2222_2222_2222];
        std::thread::scope(|scope| {
            for value in values {
                let (memory, block) = (&memory, &block);
                scope.spawn(move || {
                    for _ in 0..16 {
                        memory.write_block(block, 3, value).unwrap();
                    }
                });
            }
        });
        assert!(values.contains(&memory.read_block(&block, 3).unwrap()));
    }

    #[test]
    fn a_block_is_zeroed_bounded_and_read_where_the_unit_reads_it() {
        let frames = HostFrames::new(0x1_0000_0000);
        let memory = TableMemory::new(&frames, None);
        let _ = memory.alloc().unwrap();
        let block = memory.alloc_block(1).unwrap();
        assert_eq!(
            block.phys() % (2 * crate::IO_PAGE_SIZE),
            0,
            "aligned to its size"
        );
        assert_eq!(block.words(), 2 * PAGE_TABLE_ENTRIES);
        assert_eq!(memory.read_block(&block, block.words() - 1), Ok(0));
        memory.write_block(&block, 600, 0xFEED).unwrap();
        assert_eq!(
            frames.word(block.phys() + 600 * 8),
            Some(0xFEED),
            "what the unit reads"
        );
        assert_eq!(
            memory.write_block(&block, block.words(), 1),
            Err(IommuError::Hardware),
            "a word past the block is refused"
        );
        memory.free_block(block);
        assert_eq!(frames.word(0x1_0000_2000), None, "freed");
    }

    #[test]
    fn a_block_the_frames_cannot_hold_is_exhaustion() {
        let frames = HostFrames::new(0x1_0000_0000);
        frames.limit(3);
        let memory = TableMemory::new(&frames, None);
        assert_eq!(memory.alloc_block(2).err(), Some(IommuError::Exhausted));
        assert!(memory.alloc_block(1).is_ok());
    }
}
