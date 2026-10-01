//! The generic radix I/O page-table engine.
//!
//! Every table-walking family (VT-d, AMD-Vi, the Arm SMMU, the RISC-V IOMMU) walks
//! a radix tree of 4 KiB tables of 512 entries; they differ only in how an
//! entry is encoded and at which levels a leaf may sit. The walk, the
//! choice of leaf size, the allocation of tables, their release once
//! emptied, and the ordering that makes an entry visible to the walker are
//! written once here, over a [`PteFormat`].

use alloc::vec::Vec;

use tairix_arch_api::PAGE_TABLE_ENTRIES;

use crate::memory::{Table, TableMemory};
use crate::{Access, IommuError, IO_PAGE_SHIFT, IO_PAGE_SIZE};

/// Bits of IOVA one level resolves.
const LEVEL_BITS: u32 = PAGE_TABLE_ENTRIES.trailing_zeros();

/// The deepest tree any family walks: AMD-Vi's six levels.
pub const MAX_LEVELS: u32 = 6;

/// The deepest level a leaf may sit at: 1 GiB, the largest leaf any family
/// defines at a 4 KiB granule.
const MAX_LEAF_LEVEL: u32 = 2;

/// What one table entry holds.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Pte {
    /// Nothing: the walker faults here.
    Absent,
    /// A pointer to the table one level down.
    Table(u64),
    /// A translation of the entry's whole span onto the physical address.
    Leaf(u64, Access),
}

/// How one unit encodes its table entries.
pub trait PteFormat: Send + Sync {
    /// Whether a leaf may sit at `level`: level 0 maps one 4 KiB page, level
    /// 1 two MiB, level 2 one GiB. Level 0 always allows one.
    fn leaf_allowed(&self, level: u32) -> bool;

    /// An entry at `level` pointing at the table at `phys` one level down.
    fn table(&self, phys: u64, level: u32) -> u64;

    /// A leaf at `level` translating its span onto `phys` with `access`.
    fn leaf(&self, phys: u64, level: u32, access: Access) -> u64;

    /// What `entry`, read at `level`, holds.
    fn decode(&self, entry: u64, level: u32) -> Pte;
}

/// Bytes one entry at `level` spans.
const fn span(level: u32) -> u64 {
    IO_PAGE_SIZE << (LEVEL_BITS * level)
}

// Masked to the table's entries before the cast, so nothing is truncated.
#[allow(clippy::cast_possible_truncation)]
fn index_of(iova: u64, level: u32) -> usize {
    ((iova >> (IO_PAGE_SHIFT + LEVEL_BITS * level)) & (PAGE_TABLE_ENTRIES as u64 - 1)) as usize
}

/// One domain's table tree.
///
/// The tree is the family's: it decides when the unit may no longer walk it
/// (every stream detached and a sync confirmed), and only then drops it —
/// dropping frees every table.
///
/// Every table it names — its root, and each child [`Self::map`] linked — was
/// allocated from its memory and is unlinked before it is freed.
pub struct IoPageTable<'f, F: PteFormat> {
    format: F,
    levels: u32,
    root: Table,
    memory: TableMemory<'f>,
    /// Tables an unmap emptied, freed at the next [`Self::release_retired`].
    retired: Vec<u64>,
}

impl<'f, F: PteFormat> IoPageTable<'f, F> {
    /// An empty tree of `levels` levels drawing its tables from `memory`.
    ///
    /// # Errors
    ///
    /// [`IommuError::OutOfRange`] for a depth outside `1..=`[`MAX_LEVELS`] or a
    /// format without 4 KiB leaves, and [`IommuError::Exhausted`] when no
    /// root table can be had.
    pub fn new(format: F, levels: u32, memory: TableMemory<'f>) -> Result<Self, IommuError> {
        if !(1..=MAX_LEVELS).contains(&levels) || !format.leaf_allowed(0) {
            return Err(IommuError::OutOfRange);
        }
        Ok(Self {
            format,
            levels,
            root: memory.alloc()?,
            memory,
            retired: Vec::new(),
        })
    }

    /// Physical address of the root table.
    #[must_use]
    pub const fn root(&self) -> u64 {
        self.root.phys()
    }

    /// Levels the tree walks.
    #[must_use]
    pub const fn levels(&self) -> u32 {
        self.levels
    }

    /// Bits of IOVA the tree translates.
    #[must_use]
    pub const fn input_bits(&self) -> u32 {
        IO_PAGE_SHIFT + LEVEL_BITS * self.levels
    }

    /// Map `[iova, iova + len)` onto `[phys, phys + len)` with the largest
    /// leaves both alignments allow. A failure unmaps whatever this call
    /// installed before returning, retiring any table it emptied.
    ///
    /// # Errors
    ///
    /// [`IommuError::OutOfRange`] for a misaligned or empty range or one past
    /// the tree's reach, [`IommuError::AlreadyMapped`] where something is
    /// mapped, and [`IommuError::Exhausted`] when a table cannot be had.
    pub fn map(
        &mut self,
        iova: u64,
        phys: u64,
        len: u64,
        access: Access,
    ) -> Result<(), IommuError> {
        self.check_range(iova, len)?;
        if !phys.is_multiple_of(IO_PAGE_SIZE) || phys.checked_add(len).is_none() {
            return Err(IommuError::OutOfRange);
        }
        let mut done = 0;
        while done < len {
            let level = self.leaf_level(iova + done, phys + done, len - done);
            if let Err(err) = self.install(iova + done, phys + done, level, access) {
                if done != 0 {
                    // Only this call's own leaves are removed, so the undo
                    // cannot fail.
                    let _ = self.unmap(iova, done);
                }
                return Err(err);
            }
            done += span(level);
        }
        tairix_dma_barrier::dma_wmb();
        Ok(())
    }

    /// Remove `[iova, iova + len)`, which must be exactly leaves earlier maps
    /// installed. A table left empty is unlinked and retired: the walker may
    /// still hold it cached, so it is freed only by
    /// [`Self::release_retired`] after the unit confirms a sync.
    ///
    /// # Errors
    ///
    /// [`IommuError::OutOfRange`] for a misaligned or out-of-reach range,
    /// [`IommuError::NotMapped`] for a hole, and [`IommuError::Split`] where
    /// a leaf reaches outside the range. Leaves before the one refused stay
    /// removed; the caller treats the domain as unconfirmed.
    pub fn unmap(&mut self, iova: u64, len: u64) -> Result<(), IommuError> {
        self.check_range(iova, len)?;
        let result = self.unmap_level(self.root(), self.levels - 1, 0, iova, iova + len);
        tairix_dma_barrier::dma_wmb();
        result
    }

    /// Free every table an unmap retired. Call only once the unit has
    /// confirmed that no cached walk of them survives.
    pub fn release_retired(&mut self) {
        for phys in self.retired.drain(..) {
            self.memory.free_at(phys);
        }
    }

    /// Whether any table awaits release.
    #[must_use]
    pub fn has_retired(&self) -> bool {
        !self.retired.is_empty()
    }

    /// The physical address `iova` translates to and the access its leaf
    /// grants, or [`None`] where nothing is mapped.
    #[must_use]
    pub fn translate(&self, iova: u64) -> Option<(u64, Access)> {
        if iova.checked_shr(self.input_bits()).unwrap_or(0) != 0 {
            return None;
        }
        let mut table = self.root();
        let mut level = self.levels - 1;
        loop {
            match self
                .format
                .decode(self.read(table, index_of(iova, level)).ok()?, level)
            {
                Pte::Leaf(phys, access) => {
                    return Some((phys + (iova & (span(level) - 1)), access))
                }
                Pte::Table(child) if level > 0 => {
                    table = child;
                    level -= 1;
                }
                Pte::Absent | Pte::Table(_) => return None,
            }
        }
    }

    fn check_range(&self, iova: u64, len: u64) -> Result<(), IommuError> {
        let reach = 1u64 << self.input_bits().min(63);
        let end = iova.checked_add(len).ok_or(IommuError::OutOfRange)?;
        if len == 0
            || !iova.is_multiple_of(IO_PAGE_SIZE)
            || !len.is_multiple_of(IO_PAGE_SIZE)
            || end > reach
        {
            return Err(IommuError::OutOfRange);
        }
        Ok(())
    }

    /// The deepest level whose leaf starts at `iova`/`phys` and fits in `left`.
    fn leaf_level(&self, iova: u64, phys: u64, left: u64) -> u32 {
        (1..=MAX_LEAF_LEVEL.min(self.levels - 1))
            .rev()
            .find(|&level| {
                let size = span(level);
                self.format.leaf_allowed(level)
                    && iova.is_multiple_of(size)
                    && phys.is_multiple_of(size)
                    && left >= size
            })
            .unwrap_or(0)
    }

    fn install(
        &mut self,
        iova: u64,
        phys: u64,
        target: u32,
        access: Access,
    ) -> Result<(), IommuError> {
        let mut table = self.root();
        let mut level = self.levels - 1;
        loop {
            let index = index_of(iova, level);
            let entry = self.read(table, index)?;
            if level == target {
                if self.format.decode(entry, level) != Pte::Absent {
                    return Err(IommuError::AlreadyMapped);
                }
                self.write(table, index, self.format.leaf(phys, level, access))?;
                return Ok(());
            }
            table = match self.format.decode(entry, level) {
                Pte::Table(child) => child,
                Pte::Leaf(..) => return Err(IommuError::AlreadyMapped),
                Pte::Absent => {
                    let child = self.memory.alloc()?.phys();
                    self.write(table, index, self.format.table(child, level))?;
                    child
                }
            };
            level -= 1;
        }
    }

    fn unmap_level(
        &mut self,
        table: u64,
        level: u32,
        base: u64,
        start: u64,
        end: u64,
    ) -> Result<(), IommuError> {
        let size = span(level);
        let first = index_of(start, level);
        let last = index_of(end - 1, level);
        for index in first..=last {
            let entry_base = base + (index as u64) * size;
            let entry = self.read(table, index)?;
            match self.format.decode(entry, level) {
                Pte::Absent => return Err(IommuError::NotMapped),
                Pte::Leaf(..) => {
                    if entry_base < start || entry_base + size > end {
                        return Err(IommuError::Split);
                    }
                    self.write(table, index, 0)?;
                }
                Pte::Table(child) => {
                    if level == 0 {
                        return Err(IommuError::Hardware);
                    }
                    let result = self.unmap_level(
                        child,
                        level - 1,
                        entry_base,
                        start.max(entry_base),
                        end.min(entry_base + size),
                    );
                    // An empty table that cannot be recorded for release stays
                    // linked: it maps nothing.
                    if self.is_empty(child) && self.retired.try_reserve(1).is_ok() {
                        self.write(table, index, 0)?;
                        self.retired.push(child);
                    }
                    result?;
                }
            }
        }
        Ok(())
    }

    fn is_empty(&self, table: u64) -> bool {
        (0..PAGE_TABLE_ENTRIES).all(|index| self.read(table, index) == Ok(0))
    }

    /// `table` is one this tree names, so it came from `self.memory`.
    fn read(&self, table: u64, index: usize) -> Result<u64, IommuError> {
        // SAFETY: the tree only names tables its memory allocated and unlinks
        // each before freeing it, and its owner serialises its writers.
        unsafe { self.memory.read_at(table, index) }
    }

    /// Store `entry` whole, so the walker never reads half of one, then make
    /// it visible to a walker that does not snoop.
    fn write(&self, table: u64, index: usize, entry: u64) -> Result<(), IommuError> {
        // SAFETY: as `read`.
        unsafe { self.memory.write_at(table, index, entry) }?;
        self.memory.publish_at(table, index, 1);
        Ok(())
    }

    fn free_tree(&self, table: u64, level: u32) {
        if level > 0 {
            for index in 0..PAGE_TABLE_ENTRIES {
                if let Ok(Pte::Table(child)) = self
                    .read(table, index)
                    .map(|entry| self.format.decode(entry, level))
                {
                    self.free_tree(child, level - 1);
                }
            }
        }
        self.memory.free_at(table);
    }
}

impl<F: PteFormat> Drop for IoPageTable<'_, F> {
    fn drop(&mut self) {
        self.release_retired();
        self.free_tree(self.root(), self.levels - 1);
    }
}

#[cfg(test)]
#[path = "pagetable_tests.rs"]
mod tests;
