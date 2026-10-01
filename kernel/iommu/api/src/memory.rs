//! The memory a family's tables live in, and the ordering that makes what the
//! CPU wrote there visible to the unit's walker.

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

    /// Entry `index` of the table at `phys`.
    ///
    /// # Safety
    ///
    /// `phys` names a table this memory allocated and has not freed.
    pub(crate) unsafe fn read_at(&self, phys: u64, index: usize) -> Result<u64, IommuError> {
        let entries = self.entries(phys, index)?;
        // SAFETY: the caller's contract makes `entries` this memory's own view
        // of a live table, which no reference aliases, and `index` is inside it.
        Ok(unsafe { core::ptr::addr_of!((*entries)[index]).read_volatile() })
    }

    /// Store `value` at entry `index` of the table at `phys`.
    ///
    /// # Safety
    ///
    /// As [`Self::read_at`], and the caller serialises every writer of it.
    pub(crate) unsafe fn write_at(
        &self,
        phys: u64,
        index: usize,
        value: u64,
    ) -> Result<(), IommuError> {
        let entries = self.entries(phys, index)?;
        // SAFETY: as `read_at`; the caller serialises the table's writers.
        unsafe { core::ptr::addr_of_mut!((*entries)[index]).write_volatile(value) };
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
