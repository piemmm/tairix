//! The generic radix I/O page-table engine.
//!
//! Every table-walking family (VT-d, AMD-Vi, the Arm SMMU, the RISC-V IOMMU) walks
//! a radix tree of 4 KiB tables of 512 entries; they differ only in how an
//! entry is encoded, at which levels a leaf may sit, and whether the root
//! resolves more bits in a larger table (RISC-V's second stage). The walk, the
//! choice of leaf size, the allocation of tables, their release once
//! emptied, and the ordering that makes an entry visible to the walker are
//! written once here, over a [`PteFormat`].

use alloc::vec::Vec;

use tairix_arch_api::PAGE_TABLE_ENTRIES;
use tairix_collections::HashMap;
use tairix_hash::BuildFastHash;

use crate::domain::FrameRun;
use crate::memory::{Block, Table, TableMemory};
use crate::{Access, IommuError, Reach, IO_PAGE_SHIFT, IO_PAGE_SIZE};

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

    /// `log2` of the pages the root spans, each further bit of it a bit more
    /// of IOVA the root resolves.
    fn root_order(&self) -> u32 {
        0
    }
}

/// The widest root any family walks, as an order of tables: an `SMMUv3`
/// stage 2 walk's sixteen concatenated tables, 64 KiB.
pub const MAX_ROOT_ORDER: u32 = 4;

/// One leaf of a tree: a translation of a naturally aligned span.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Leaf {
    /// The IOVA it starts at.
    pub iova: u64,
    /// The bytes it spans.
    pub len: u64,
    /// The physical address it starts at.
    pub phys: u64,
    /// What it lets a device do.
    pub access: Access,
}

/// Bits of IOVA a tree of `levels` levels translates.
#[must_use]
pub const fn reach_bits(levels: u32) -> u32 {
    IO_PAGE_SHIFT + LEVEL_BITS * levels
}

/// Whether the inclusive spans `[first, end]` and `[iova, last]` meet.
const fn touches(first: u64, end: u64, iova: u64, last: u64) -> bool {
    first <= last && iova <= end
}

/// Bytes one entry at `level` spans.
const fn span(level: u32) -> u64 {
    IO_PAGE_SIZE << (LEVEL_BITS * level)
}

// Masked to the table's `entries` before the cast, so nothing is truncated.
#[allow(clippy::cast_possible_truncation)]
fn index_of(iova: u64, level: u32, entries: usize) -> usize {
    ((iova >> (IO_PAGE_SHIFT + LEVEL_BITS * level)) & (entries as u64 - 1)) as usize
}

/// The root: one table, or a block of them the format's root spans.
enum Root {
    Table(Table),
    Block(Block),
}

impl Root {
    const fn phys(&self) -> u64 {
        match self {
            Self::Table(table) => table.phys(),
            Self::Block(block) => block.phys(),
        }
    }
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
    root: Root,
    /// `log2` of the pages the root spans.
    root_order: u32,
    /// Bits of IOVA the unit translates, which the tree may reach past.
    input_bits: u32,
    /// The exclusive physical address the unit's entries can name up to,
    /// past which a format's entry would silently name another page.
    output_limit: u64,
    memory: TableMemory<'f>,
    /// Live entries in each table below the root, so an unmap knows a table
    /// it emptied without reading it back. Keyed by the table's own address.
    occupancy: HashMap<u64, u16, BuildFastHash>,
    /// Tables an unmap emptied, freed once a sync covers them.
    retired: Vec<Retired>,
}

/// A table an unmap emptied: its frame, the first and last IOVA its entry
/// spanned, and the batch handed to the unit to confirm it gone.
struct Retired {
    phys: u64,
    first: u64,
    last: u64,
    confirmed_by: Option<u64>,
}

impl<'f, F: PteFormat> IoPageTable<'f, F> {
    /// An empty tree of `levels` levels drawing its tables from `memory`,
    /// mapping only within `reach`.
    ///
    /// # Errors
    ///
    /// [`IommuError::OutOfRange`] for a depth outside `1..=`[`MAX_LEVELS`], a
    /// root wider than 64 KiB, a format without 4 KiB leaves, or a reach
    /// translating more IOVA than the tree resolves, and
    /// [`IommuError::Exhausted`] when no root can be had.
    pub fn new(
        format: F,
        levels: u32,
        memory: TableMemory<'f>,
        reach: Reach,
    ) -> Result<Self, IommuError> {
        let order = format.root_order();
        if !(1..=MAX_LEVELS).contains(&levels)
            || order > MAX_ROOT_ORDER
            || !format.leaf_allowed(0)
            || reach.input_bits > reach_bits(levels) + order
        {
            return Err(IommuError::OutOfRange);
        }
        let root = if order == 0 {
            Root::Table(memory.alloc()?)
        } else {
            Root::Block(memory.alloc_block(order)?)
        };
        Ok(Self {
            format,
            levels,
            root,
            root_order: order,
            input_bits: reach.input_bits,
            output_limit: reach.output_limit(),
            memory,
            occupancy: HashMap::with_hasher(BuildFastHash::new()),
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
        self.input_bits
    }

    /// Map `[iova, iova + len)` onto `[phys, phys + len)` with the largest
    /// leaves both alignments allow. A failure unmaps whatever this call
    /// installed before returning, retiring any table it emptied.
    ///
    /// # Errors
    ///
    /// [`IommuError::OutOfRange`] for a misaligned or empty range, or one past
    /// the unit's reach at either end, [`IommuError::AlreadyMapped`] where
    /// something is mapped, and [`IommuError::Exhausted`] when a table cannot
    /// be had.
    pub fn map(
        &mut self,
        iova: u64,
        phys: u64,
        len: u64,
        access: Access,
    ) -> Result<(), IommuError> {
        self.check_range(iova, len)?;
        let end = phys.checked_add(len).ok_or(IommuError::OutOfRange)?;
        if !phys.is_multiple_of(IO_PAGE_SIZE) || end > self.output_limit {
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

    /// [`Self::map`] each of `runs` back to back from `iova`, answering the
    /// bytes they span; a refusal takes back every run installed before it.
    ///
    /// # Errors
    ///
    /// As [`Self::map`], or [`IommuError::Unconfirmed`] where what was
    /// installed could not be taken back.
    pub fn map_runs(
        &mut self,
        iova: u64,
        runs: &[FrameRun],
        access: Access,
    ) -> Result<u64, IommuError> {
        let mut mapped = 0;
        for run in runs {
            if let Err(err) = self.map(iova + mapped, run.phys, run.bytes(), access) {
                return Err(self.take_back(iova, mapped, err));
            }
            mapped += run.bytes();
        }
        Ok(mapped)
    }

    /// Take back the `mapped` bytes from `iova` a refused operation had
    /// installed and answer `err`, or [`IommuError::Unconfirmed`] where they
    /// could not all be removed.
    pub fn take_back(&mut self, iova: u64, mapped: u64, err: IommuError) -> IommuError {
        if mapped == 0 || self.unmap(iova, mapped).is_ok() {
            err
        } else {
            IommuError::Unconfirmed
        }
    }

    /// Remove `[iova, iova + len)`, which must be exactly leaves earlier maps
    /// installed. A table left empty is unlinked and retired: the walker may
    /// still hold it cached, so it is freed only once the unit confirms a
    /// sync reaching it, tagged with the batch confirming it
    /// ([`Self::tag_retired`]) and freed once that batch is done
    /// ([`Self::release_tagged`]).
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

    /// Free every table an unmap retired, for tables no unit walks or one
    /// whose domain the unit confirmed gone whole.
    pub fn release_retired(&mut self) {
        for retired in self.retired.drain(..) {
            self.memory.free_at(retired.phys);
        }
    }

    /// Tag every retired table no batch confirms yet — or, with a `range`,
    /// only those whose entry spanned any of `[iova, iova + len)`, which an
    /// invalidation of that range, walk caches included, reaches — as
    /// confirmed by `batch`. An unmap retires only tables its own range
    /// emptied, so its range's sync covers them.
    pub fn tag_retired(&mut self, range: Option<(u64, u64)>, batch: u64) {
        for retired in &mut self.retired {
            let reached = range.is_none_or(|(iova, len)| retired.touches(iova, len));
            if reached && retired.confirmed_by.is_none() {
                retired.confirmed_by = Some(batch);
            }
        }
    }

    /// Free the tables `batch` confirmed gone, which the unit has done.
    pub fn release_tagged(&mut self, batch: u64) {
        let memory = &self.memory;
        self.retired.retain(|retired| {
            let confirmed = retired.confirmed_by == Some(batch);
            if confirmed {
                memory.free_at(retired.phys);
            }
            !confirmed
        });
    }

    /// Hand the tables `batch` was to confirm, which failed, back to the next
    /// sync.
    pub fn untag(&mut self, batch: u64) {
        for retired in &mut self.retired {
            if retired.confirmed_by == Some(batch) {
                retired.confirmed_by = None;
            }
        }
    }

    /// Whether any table awaits release.
    #[must_use]
    pub fn has_retired(&self) -> bool {
        !self.retired.is_empty()
    }

    /// Whether a table awaiting release had its entry span any of
    /// `[iova, iova + len)`: an unmap of it changed an entry above a leaf.
    #[must_use]
    pub fn has_retired_touching(&self, iova: u64, len: u64) -> bool {
        self.retired
            .iter()
            .any(|retired| retired.touches(iova, len))
    }

    /// The physical address `iova` translates to and the access its leaf
    /// grants, or [`None`] where nothing is mapped.
    #[must_use]
    pub fn translate(&self, iova: u64) -> Option<(u64, Access)> {
        self.leaf_at(iova)
            .map(|leaf| (leaf.phys + (iova - leaf.iova), leaf.access))
    }

    /// The leaf `iova` falls in: its IOVA, the bytes it spans, the physical
    /// address it starts at and its access; [`None`] where nothing is mapped.
    #[must_use]
    pub fn leaf_at(&self, iova: u64) -> Option<Leaf> {
        if iova.checked_shr(self.input_bits()).unwrap_or(0) != 0 {
            return None;
        }
        let mut table = self.root();
        let mut level = self.levels - 1;
        loop {
            match self
                .format
                .decode(self.read(table, self.index(iova, level)).ok()?, level)
            {
                Pte::Leaf(phys, access) => {
                    let len = span(level);
                    return Some(Leaf {
                        iova: iova & !(len - 1),
                        len,
                        phys,
                        access,
                    });
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
        let end = iova.checked_add(len).ok_or(IommuError::OutOfRange)?;
        // A 64-bit reach is bounded by the address space itself.
        let past_reach = 1u64
            .checked_shl(self.input_bits())
            .is_some_and(|reach| end > reach);
        if len == 0
            || !iova.is_multiple_of(IO_PAGE_SIZE)
            || !len.is_multiple_of(IO_PAGE_SIZE)
            || past_reach
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
                    // A walk cache may still reach a retired table through
                    // this entry, so it takes no leaf until that is released.
                    && self.retired_at(iova, level).is_none()
            })
            .unwrap_or(0)
    }

    /// Where in the retired list the table an unmap took from the entry at
    /// `level` holding `iova` is.
    fn retired_at(&self, iova: u64, level: u32) -> Option<usize> {
        let size = span(level);
        let first = iova & !(size - 1);
        let last = first + (size - 1);
        self.retired
            .iter()
            .position(|retired| retired.first == first && retired.last == last)
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
            let index = self.index(iova, level);
            let entry = self.read(table, index)?;
            if level == target {
                if self.format.decode(entry, level) != Pte::Absent {
                    return Err(IommuError::AlreadyMapped);
                }
                return self.fill(table, index, self.format.leaf(phys, level, access));
            }
            table = match self.format.decode(entry, level) {
                Pte::Table(child) => child,
                Pte::Leaf(..) => return Err(IommuError::AlreadyMapped),
                Pte::Absent => {
                    self.occupancy
                        .try_reserve(1)
                        .map_err(|_| IommuError::Exhausted)?;
                    // The table an unmap retired from this entry is relinked, so
                    // a walk cache still holding the entry reaches the leaf
                    // this map lays rather than a table that maps nothing.
                    let child = if let Some(at) = self.retired_at(iova, level) {
                        let child = self.retired[at].phys;
                        self.fill(table, index, self.format.table(child, level))?;
                        self.retired.swap_remove(at);
                        child
                    } else {
                        let child = self.memory.alloc()?.phys();
                        if let Err(err) = self.fill(table, index, self.format.table(child, level)) {
                            self.memory.free_at(child);
                            return Err(err);
                        }
                        child
                    };
                    // Room was reserved above.
                    let _ = self.occupancy.try_insert(child, 0);
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
        let first = self.index(start, level);
        let last = self.index(end - 1, level);
        for index in first..=last {
            let entry_base = base + (index as u64) * size;
            // Inclusive, so an entry ending at the top of the address space
            // is spelled.
            let entry_last = entry_base + (size - 1);
            let entry = self.read(table, index)?;
            match self.format.decode(entry, level) {
                Pte::Absent => return Err(IommuError::NotMapped),
                Pte::Leaf(..) => {
                    if entry_base < start || entry_last >= end {
                        return Err(IommuError::Split);
                    }
                    self.clear(table, index)?;
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
                        if entry_last < end {
                            entry_last + 1
                        } else {
                            end
                        },
                    );
                    // An empty table that cannot be recorded for release stays
                    // linked: it maps nothing.
                    if self.occupancy.get(&child) == Some(&0) && self.retired.try_reserve(1).is_ok()
                    {
                        self.clear(table, index)?;
                        self.occupancy.remove(&child);
                        self.retired.push(Retired {
                            phys: child,
                            first: entry_base,
                            last: entry_last,
                            confirmed_by: None,
                        });
                    }
                    result?;
                }
            }
        }
        Ok(())
    }

    /// Make an absent entry `entry`.
    fn fill(&mut self, table: u64, index: usize, entry: u64) -> Result<(), IommuError> {
        self.write(table, index, entry)?;
        if let Some(live) = self.occupancy.get_mut(&table) {
            *live += 1;
        }
        Ok(())
    }

    /// Make a present entry absent.
    fn clear(&mut self, table: u64, index: usize) -> Result<(), IommuError> {
        self.write(table, index, 0)?;
        if let Some(live) = self.occupancy.get_mut(&table) {
            *live = live.saturating_sub(1);
        }
        Ok(())
    }

    /// The entry `iova` falls in of a table at `level`: the root's if the
    /// level is the root's.
    fn index(&self, iova: u64, level: u32) -> usize {
        let entries = if level == self.levels - 1 {
            self.root_entries()
        } else {
            PAGE_TABLE_ENTRIES
        };
        index_of(iova, level, entries)
    }

    const fn root_entries(&self) -> usize {
        PAGE_TABLE_ENTRIES << self.root_order
    }

    /// `table` is one this tree names, so it came from `self.memory`.
    fn read(&self, table: u64, index: usize) -> Result<u64, IommuError> {
        if let Root::Block(root) = &self.root {
            if root.phys() == table {
                return self.memory.read_block(root, index);
            }
        }
        // SAFETY: the tree only names tables its memory allocated and unlinks
        // each before freeing it, and its owner serialises its writers.
        unsafe { self.memory.read_at(table, index) }
    }

    /// Store `entry` whole, so the walker never reads half of one, then make
    /// it visible to a walker that does not snoop.
    fn write(&self, table: u64, index: usize, entry: u64) -> Result<(), IommuError> {
        if let Root::Block(root) = &self.root {
            if root.phys() == table {
                self.memory.write_block(root, index, entry)?;
                self.memory.publish_block(root, index, 1);
                return Ok(());
            }
        }
        // SAFETY: as `read`.
        unsafe { self.memory.write_at(table, index, entry) }?;
        self.memory.publish_at(table, index, 1);
        Ok(())
    }

    /// Free every table below the entries `[0, entries)` of `table` at
    /// `level`.
    fn free_below(&self, table: u64, level: u32, entries: usize) {
        if level == 0 {
            return;
        }
        for index in 0..entries {
            if let Ok(Pte::Table(child)) = self
                .read(table, index)
                .map(|entry| self.format.decode(entry, level))
            {
                self.free_below(child, level - 1, PAGE_TABLE_ENTRIES);
                self.memory.free_at(child);
            }
        }
    }
}

impl Retired {
    fn touches(&self, iova: u64, len: u64) -> bool {
        touches(
            self.first,
            self.last,
            iova,
            iova.saturating_add(len.max(1) - 1),
        )
    }
}

impl<F: PteFormat> Drop for IoPageTable<'_, F> {
    fn drop(&mut self) {
        self.release_retired();
        self.free_below(self.root(), self.levels - 1, self.root_entries());
        match &self.root {
            Root::Table(table) => self.memory.free_at(table.phys()),
            Root::Block(block) => self.memory.free_block_at(block.phys(), block.order()),
        }
    }
}

#[cfg(test)]
#[path = "pagetable_tests.rs"]
mod tests;
