//! The dynamic-area SWIs (`plans/RISCOS-EMULATOR.md` §3): `OS_DynamicArea`,
//! `OS_ChangeDynamicArea` and `OS_ReadDynamicArea`, their registers
//! marshalled onto the address space's manager; and the handler calls,
//! services and upcalls the manager makes, run as guest code.
//!
//! An area's title lives in the system heap while the area does, where the
//! pointer the info reasons hand out stays good.

use alloc::vec::Vec;

use tairix_collections::OrderedMap;
use tairix_riscos_arm::mmu::PAGE_SIZE;
use tairix_riscos_arm::psr;

use crate::error::Error;
use crate::host::Routine;
use crate::kheap::KernelHeap;
use crate::memory::handler_reason as reason;
use crate::memory::{
    Allowed, AreaError, Clamps, Create, Failure, Guest, GuestError, Handler, Located, LogicalOp,
    Memory, PageEntry, PageQuery, PhysicalOp, Service, FREE_POOL,
};
use crate::os::{len32, Entry, Fail, Os, Regs, Target, Unwind, World};

/// `UpCall_MovingMemory`.
const MOVING_MEMORY: u32 = 257;
/// What a `PreGrow` handler answers in R0 to take pages `OS_Memory` 23
/// reserved.
const RESV: u32 = u32::from_le_bytes(*b"RESV");
/// A handler page block's entry.
const ENTRY: u32 = 12;
/// A service's 64-bit page block entry.
const ENTRY_64: u32 = 20;

/// "No such dynamic area handler call".
pub(crate) const UNKNOWN_AREA_HANDLER: Error = Error {
    number: 0x1C9,
    token: "UnknownAreaHandlerReason",
};

/// The error an area failure gives.
fn area_error(error: AreaError) -> Option<Error> {
    error
        .number_and_token()
        .map(|(number, token)| Error { number, token })
}

/// Areas' titles in guest memory, by area number.
pub(crate) struct Titles {
    blocks: OrderedMap<u32, u32>,
}

impl Titles {
    pub(crate) const fn new() -> Self {
        Self {
            blocks: OrderedMap::new(),
        }
    }
}

/// Clamps a byte count to what the info reasons report.
fn clamped(bytes: u64) -> u32 {
    u32::try_from(bytes.min(0x7FFF_F000)).unwrap_or(0x7FFF_F000)
}

impl Os<'_> {
    /// Makes one of the kernel's own areas, its number among those no
    /// program may ask for, and grows it to its initial size, its handler
    /// called as for any grow.
    ///
    /// # Errors
    ///
    /// The error `OS_DynamicArea` 0 gives for the same refusal.
    pub fn create_kernel_area(
        &mut self,
        request: &crate::memory::Create<'_>,
    ) -> Result<crate::memory::Created, Fail> {
        match self.memory.create_kernel_area(request, self.world) {
            Ok(created) => Ok(created),
            Err(error) => Err(self.fail_area(error)),
        }
    }

    /// Dynamic area `number`'s base and size now, the area named by its plain
    /// number: none of `OS_ReadDynamicArea`'s other spellings of one.
    ///
    /// # Errors
    ///
    /// As `OS_ReadDynamicArea` refuses an area that does not exist.
    pub fn area_extent(&mut self, number: u32) -> Result<(u32, u32), Fail> {
        let read = self
            .memory
            .read_area(number.cast_signed())
            .map_err(|error| self.fail_area(error))?;
        Ok((read.start, clamped(read.size)))
    }

    /// Watches area `number`'s pages for the display: see
    /// [`crate::memory::Memory::watch_area_for_display`].
    ///
    /// # Errors
    ///
    /// The error `OS_DynamicArea` gives for the refusal.
    pub fn watch_area_for_display(&mut self, number: u32) -> Result<(), Fail> {
        match self.memory.watch_area_for_display(number) {
            Ok(()) => Ok(()),
            Err(error) => Err(self.fail_area(error)),
        }
    }

    /// Hands `each` the offset of every granule of area `number` stored
    /// into since last asked.
    pub fn take_display_damage(&mut self, number: u32, each: impl FnMut(u32)) {
        self.memory.take_display_damage(number, each);
    }

    /// The error `error` gives: the kernel's, or the handler's own block.
    pub(crate) fn fail_area(&mut self, error: AreaError) -> Fail {
        match (area_error(error), error) {
            (_, AreaError::Guest(GuestError(block))) => Fail::Error(block),
            (Some(error), _) => self.error(error),
            (None, _) => self.error(UNKNOWN_AREA_HANDLER),
        }
    }

    /// An unwind a handler started while the manager was changing: the
    /// manager has backed out, and control goes where the handler sent it.
    fn unwinding(&mut self) -> Result<(), Fail> {
        match self.world.unwinding.take() {
            Some(unwind) => Err(Fail::Unwind(unwind)),
            None => Ok(()),
        }
    }

    /// `area`'s title in guest memory: made from the manager's if it has
    /// none there yet.
    fn title_block(&mut self, area: u32, title: &[u8]) -> Result<u32, Fail> {
        if let Some(&block) = self.world.titles.blocks.get(&area) {
            return Ok(block);
        }
        let block = self.system_node(len32(title.len()) + 1)?;
        self.write_terminated(block, title, 0, title.len() + 1)?;
        if self.world.titles.blocks.try_insert(area, block).is_err() {
            self.free_system_node(block);
            return Err(self.no_memory());
        }
        Ok(block)
    }

    /// Forgets `area`'s title.
    fn drop_title(&mut self, area: u32) {
        if let Some(block) = self.world.titles.blocks.remove(&area) {
            self.free_system_node(block);
        }
    }

    /// `OS_ChangeDynamicArea`: `area` (application space for the free pool
    /// shrinking) changed by `bytes`; the bytes moved.
    pub(crate) fn change_area_by(&mut self, area: u32, bytes: i32) -> Result<u32, Fail> {
        let number = i32::try_from(area).unwrap_or(i32::MAX);
        let changed = self.memory.change_area(number, bytes, self.world);
        self.unwinding()?;
        let moved = changed.map_err(|failure: Failure| self.fail_area(failure.error))?;
        if area == FREE_POOL {
            let top = self.application_top();
            self.world.env.set_application_top(top);
        }
        Ok(moved)
    }

    /// `OS_ChangeDynamicArea`'s SWI: R0 the area, R1 the change; R1 the bytes
    /// moved, also on failure.
    pub(crate) fn change_dynamic_area(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let number = regs.r[0].cast_signed();
        let changed = self
            .memory
            .change_area(number, regs.r[1].cast_signed(), self.world);
        self.unwinding()?;
        if regs.r[0] == FREE_POOL {
            let top = self.application_top();
            self.world.env.set_application_top(top);
        }
        match changed {
            Ok(moved) => {
                regs.r[1] = moved;
                Ok(())
            }
            Err(failure) => {
                regs.r[1] = failure.moved;
                Err(self.fail_area(failure.error))
            }
        }
    }

    /// `OS_ReadDynamicArea`.
    pub(crate) fn read_dynamic_area(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let asked = regs.r[0];
        if asked == u32::MAX {
            regs.r[0] = crate::memory::layout::APPLICATION;
            regs.r[1] = self.application_top() - crate::memory::layout::APPLICATION;
            regs.r[2] = self.memory.application_limit() - crate::memory::layout::APPLICATION;
            return Ok(());
        }
        let (number, with_max) = if asked < 0x100 {
            (asked & 0x7F, asked & 0x80 != 0)
        } else {
            (asked, true)
        };
        let read = self
            .memory
            .read_area(number.cast_signed())
            .map_err(|error| self.fail_area(error))?;
        regs.r[0] = read.start;
        regs.r[1] = clamped(read.size);
        if with_max {
            regs.r[2] = clamped(read.max);
        }
        Ok(())
    }

    /// `OS_DynamicArea`.
    #[expect(clippy::too_many_lines, reason = "one arm a reason")]
    pub(crate) fn dynamic_area(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        match regs.r[0] {
            0 => self.area_create(regs),
            1 => self.area_remove(regs),
            2 => self.area_info(regs),
            3 => {
                let after = (regs.r[1] != u32::MAX).then_some(regs.r[1]);
                let next = self
                    .memory
                    .enumerate_areas(after)
                    .map_err(|e| self.fail_area(e))?;
                regs.r[1] = next.unwrap_or(u32::MAX);
                Ok(())
            }
            4 => {
                let renumbered = self.memory.renumber_area(regs.r[1], regs.r[2], self.world);
                self.unwinding()?;
                renumbered.map_err(|e| self.fail_area(e))?;
                if let Some(block) = self.world.titles.blocks.remove(&regs.r[1]) {
                    if self
                        .world
                        .titles
                        .blocks
                        .try_insert(regs.r[2], block)
                        .is_err()
                    {
                        self.free_system_node(block);
                    }
                }
                Ok(())
            }
            5 | 27 => {
                let excluding = (regs.r[1] != u32::MAX).then_some(regs.r[1]);
                let pages = self.memory.free_area_pages(excluding, self.world);
                self.unwinding()?;
                let pages = pages.map_err(|e| self.fail_area(e))?;
                regs.r[2] = if regs.r[0] == 27 {
                    pages
                } else {
                    clamped(u64::from(pages) * u64::from(PAGE_SIZE))
                };
                Ok(())
            }
            6 => {
                let (area, signature) = self.memory.take_area_changes();
                regs.r[1] = area.unwrap_or(u32::MAX);
                regs.r[2] = signature;
                Ok(())
            }
            7 | 28 => self.area_enumerate_info(regs),
            8 => {
                let old = self.memory.set_clamps(Clamps {
                    unbounded: regs.r[1],
                    bounded: regs.r[2],
                    sparse: regs.r[3],
                });
                regs.r[1] = old.unbounded;
                regs.r[2] = old.bounded;
                regs.r[3] = old.sparse;
                Ok(())
            }
            9 | 10 => {
                let (area, base, size) = (regs.r[1], regs.r[2], regs.r[3]);
                let done = if regs.r[0] == 9 {
                    self.memory.sparse_claim(area, base, size, self.world)
                } else {
                    self.memory.sparse_release(area, base, size, self.world)
                };
                self.unwinding()?;
                done.map_err(|e| self.fail_area(e))
            }
            20 => {
                let located = self
                    .memory
                    .locate(regs.r[1])
                    .map_err(|e| self.fail_area(e))?;
                (regs.r[0], regs.r[1]) = match located {
                    Located::Region(region) => (1, region),
                    Located::Area(area) => (0, area),
                };
                Ok(())
            }
            21 => self.area_physical(regs),
            22 => self.area_logical(regs),
            23 => match self.memory.pool_resize(regs.r[1], regs.r[2].cast_signed()) {
                Ok(change) => {
                    regs.r[2] = change;
                    Ok(())
                }
                Err(error) => {
                    regs.r[2] = 0;
                    Err(self.fail_area(error))
                }
            },
            24 => self.area_pool_info(regs),
            25 => self.area_pages(regs),
            26 => {
                let old = self
                    .memory
                    .set_application_limit(regs.r[1])
                    .map_err(|e| self.fail_area(e))?;
                regs.r[1] = old;
                Ok(())
            }
            _ => Err(self.error(crate::kheap::UNKNOWN_REASON)),
        }
    }

    /// Reason 0.
    fn area_create(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let given = |r: u32| (r != u32::MAX).then_some(r);
        let title = if regs.r[8] == 0 {
            None
        } else {
            Some(
                self.read_until(regs.r[8], crate::memory::TITLE, |b| b == 0)?
                    .0,
            )
        };
        let request = Create {
            number: given(regs.r[1]),
            size: regs.r[2],
            base: given(regs.r[3]),
            flags: regs.r[4],
            max: given(regs.r[5]),
            handler: (regs.r[6] != 0).then_some(regs.r[6]),
            workspace: given(regs.r[7]),
            title: title.as_deref(),
            pool: regs.r[9],
            caller: self.caller_bits(),
        };
        let created = self.memory.create_area(&request, self.world);
        self.unwinding()?;
        let created = created.map_err(|e| self.fail_area(e))?;
        regs.r[1] = created.number;
        regs.r[3] = created.base;
        regs.r[5] = created.max;
        regs.r[9] = regs.r[9].min(self.memory.ceiling_pages());
        Ok(())
    }

    /// Reason 1.
    fn area_remove(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let removed = self.memory.remove_area(regs.r[1], self.world);
        self.unwinding()?;
        removed.map_err(|e| self.fail_area(e))?;
        self.drop_title(regs.r[1]);
        Ok(())
    }

    /// Reason 2.
    fn area_info(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let info = match self.memory.area_info(regs.r[1]) {
            Ok(info) => info,
            Err(error) => return Err(self.fail_area(error)),
        };
        let (number, size, base, flags, max, handler) = (
            info.number,
            info.size,
            info.base,
            info.flags,
            info.max,
            info.handler,
        );
        let pool = info.pool;
        let title: Option<Vec<u8>> = tairix_util::fallible::copied(info.title);
        let title = title.ok_or_else(|| self.no_memory())?;
        let title = self.title_block(number, &title)?;
        let (size, max) = match pool {
            Some((pages, most)) => (
                clamped(u64::from(pages) * u64::from(PAGE_SIZE)),
                clamped(u64::from(most) * u64::from(PAGE_SIZE)),
            ),
            None => (size, max),
        };
        regs.r[2] = size;
        regs.r[3] = base;
        regs.r[4] = flags;
        regs.r[5] = max;
        regs.r[6] = handler.map_or(0, |h| h.code);
        regs.r[7] = handler.map_or(0, |h| h.workspace);
        regs.r[8] = title;
        Ok(())
    }

    /// Reasons 7 and 28.
    fn area_enumerate_info(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let pages = regs.r[0] == 28;
        let after = (regs.r[1] != u32::MAX).then_some(regs.r[1]);
        let next = match self.memory.enumerate_area_info(after) {
            Ok(next) => next,
            Err(error) => return Err(self.fail_area(error)),
        };
        let Some(info) = next else {
            regs.r[1] = u32::MAX;
            return Ok(());
        };
        let (number, size, base, flags, max, pool) = (
            info.number,
            info.size,
            info.base,
            info.flags,
            info.max,
            info.pool,
        );
        let title: Option<Vec<u8>> = tairix_util::fallible::copied(info.title);
        let title = title.ok_or_else(|| self.no_memory())?;
        let title = self.title_block(number, &title)?;
        let (size, max) = match (pool, pages) {
            (Some((p, m)), true) => (p, m),
            (Some((p, m)), false) => (
                clamped(u64::from(p) * u64::from(PAGE_SIZE)),
                clamped(u64::from(m) * u64::from(PAGE_SIZE)),
            ),
            (None, true) => (size / PAGE_SIZE, max / PAGE_SIZE),
            (None, false) => (size, max),
        };
        regs.r[1] = number;
        regs.r[2] = size;
        regs.r[3] = base;
        regs.r[4] = flags;
        regs.r[5] = max;
        regs.r[6] = title;
        Ok(())
    }

    /// Reason 24.
    fn area_pool_info(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let info = match self.memory.area_info(regs.r[1]) {
            Ok(info) => info,
            Err(error) => return Err(self.fail_area(error)),
        };
        let (number, size, base, flags, max, pool) = (
            info.number,
            info.size,
            info.base,
            info.flags,
            info.max,
            info.pool,
        );
        let title: Option<Vec<u8>> = tairix_util::fallible::copied(info.title);
        let title = title.ok_or_else(|| self.no_memory())?;
        let title = self.title_block(number, &title)?;
        let (pages, most) = pool.unwrap_or((size / PAGE_SIZE, max / PAGE_SIZE));
        regs.r[2] = size;
        regs.r[3] = base;
        regs.r[4] = flags;
        regs.r[5] = max;
        regs.r[6] = pages;
        regs.r[7] = most;
        regs.r[8] = title;
        Ok(())
    }

    /// Reads `count` entries of `N` words each from `at`, each as `entry`
    /// makes it.
    fn read_entries<const N: usize, T>(
        &mut self,
        at: u32,
        count: u32,
        entry: impl Fn([u32; N]) -> T,
    ) -> Result<Vec<T>, Fail> {
        let most = self.memory.ceiling_pages().saturating_mul(4).max(4096);
        if count.checked_mul(len32(N)).is_none_or(|words| words > most) {
            return Err(self.error(crate::error::BUFFER_OVERFLOW));
        }
        let mut out = Vec::new();
        if out.try_reserve_exact(count as usize).is_err() {
            return Err(self.no_memory());
        }
        let mut cursor = at;
        for _ in 0..count {
            let mut fields = [0; N];
            for field in &mut fields {
                *field = self.read_word(cursor)?;
                cursor = cursor.wrapping_add(4);
            }
            out.push(entry(fields));
        }
        Ok(out)
    }

    /// Reason 21.
    fn area_physical(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let (area, list, count) = (regs.r[1], regs.r[2], regs.r[3]);
        let ops = self.read_entries(list, count, |[slot, page, flags]| PhysicalOp {
            slot,
            page: page.cast_signed(),
            flags,
        })?;
        let done = self.memory.pool_physical(area, &ops, self.world);
        self.unwinding()?;
        match done {
            Ok(()) => {
                regs.r[2] = list.wrapping_add(count.wrapping_mul(ENTRY));
                regs.r[3] = 0;
                Ok(())
            }
            Err((error, index)) => {
                let index = len32(index);
                regs.r[2] = list.wrapping_add(index.wrapping_mul(ENTRY));
                regs.r[3] = count - index;
                Err(self.fail_area(error))
            }
        }
    }

    /// Reason 22.
    fn area_logical(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let (area, list, count) = (regs.r[1], regs.r[2], regs.r[3]);
        let ops = self.read_entries(list, count, |[window, slot, flags]| LogicalOp {
            window,
            slot: (slot != u32::MAX).then_some(slot),
            flags,
        })?;
        match self.memory.pool_logical(area, &ops) {
            Ok(()) => {
                regs.r[2] = list.wrapping_add(count.wrapping_mul(ENTRY));
                regs.r[3] = 0;
                Ok(())
            }
            Err((error, index)) => {
                let index = len32(index);
                regs.r[2] = list.wrapping_add(index.wrapping_mul(ENTRY));
                regs.r[3] = count - index;
                Err(self.fail_area(error))
            }
        }
    }

    /// Reason 25.
    fn area_pages(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let (area, list, count) = (regs.r[1], regs.r[2], regs.r[3]);
        let given = |w: u32| (w != u32::MAX).then_some(w);
        let mut queries =
            self.read_entries(list, count, |[slot, page, window, flags]| PageQuery {
                slot: given(slot),
                page: given(page),
                window: given(window),
                flags,
            })?;
        let done = self.memory.pool_query(area, &mut queries);
        let answered = match done {
            Ok(()) => queries.len(),
            Err((_, index)) => index,
        };
        for (i, query) in (0u32..).zip(&queries[..answered]) {
            let at = list.wrapping_add(i.wrapping_mul(16));
            self.write_word(at, query.slot.unwrap_or(u32::MAX))?;
            self.write_word(at.wrapping_add(4), query.page.unwrap_or(u32::MAX))?;
            self.write_word(at.wrapping_add(8), query.window.unwrap_or(u32::MAX))?;
            self.write_word(at.wrapping_add(12), query.flags)?;
        }
        done.map_err(|(error, _)| self.fail_area(error))
    }

    /// Calls a guest area handler for `reason` with `regs`, its page block,
    /// if it has one, below the supervisor stack so the handler's own stack
    /// cannot meet it. Its registers back, and whether it set V.
    fn area_handler(
        &mut self,
        handler: Handler,
        mut regs: Regs,
        block: Option<&mut [PageEntry]>,
    ) -> Result<(Regs, bool), Unwind> {
        let saved_sp = self.supervisor_stack();
        let entries = block.as_ref().map_or(0, |b| len32(b.len()));
        let room = (entries * ENTRY + 7) & !7;
        let block_at = saved_sp.wrapping_sub(room);
        if let Some(block) = &block {
            for (i, entry) in (0..).zip(block.iter()) {
                let at = block_at + i * ENTRY;
                self.write_entry(at, entry)?;
            }
            regs.r[1] = block_at;
        }
        regs.r[12] = handler.workspace;
        let target = Target {
            address: handler.code,
            bits: handler.bits,
        };
        self.set_supervisor_stack(block_at);
        let called = self.call(target, &mut regs, Entry::Supervisor);
        self.set_supervisor_stack(saved_sp);
        called?;
        if let Some(block) = block {
            for (i, entry) in (0..).zip(block.iter_mut()) {
                let page = self.peek_word(block_at + i * ENTRY).unwrap_or(u32::MAX);
                entry.page = (page != u32::MAX).then_some(page);
            }
        }
        let failed = regs.flag(psr::V);
        Ok((regs, failed))
    }

    /// Writes a handler page block entry at `at`.
    fn write_entry(&mut self, at: u32, entry: &PageEntry) -> Result<(), Unwind> {
        let words = [
            entry.page.unwrap_or(u32::MAX),
            entry.logical,
            entry.physical,
        ];
        for (i, &word) in (0..).zip(&words) {
            self.poke_word(at + 4 * i, word)
                .ok_or(Unwind::Ended(crate::os::Ended::Fault))?;
        }
        Ok(())
    }

    /// The kernel heap a handler is the kernel's own for, if it is.
    fn heap_handler(&self, handler: Handler) -> Option<KernelHeap> {
        if handler.code != self.world.routine_address(Routine::HeapArea) {
            return None;
        }
        match handler.workspace {
            0 => Some(KernelHeap::System),
            1 => Some(KernelHeap::Rma),
            _ => None,
        }
    }

    /// Writes `pages` as a service's page block below `sp`, moving `sp`
    /// down past it: the 64-bit layout when `wide`. The block's address.
    fn stack_block(
        &mut self,
        sp: &mut u32,
        pages: &[PageEntry],
        wide: bool,
    ) -> Result<u32, Unwind> {
        let size = len32(pages.len()) * if wide { ENTRY_64 } else { ENTRY };
        *sp = sp.wrapping_sub((size + 7) & !7);
        for (i, entry) in (0..).zip(pages) {
            if wide {
                let words = [
                    entry.page.unwrap_or(u32::MAX),
                    entry.logical,
                    0,
                    entry.physical,
                    0,
                ];
                for (j, word) in (0..).zip(words) {
                    self.poke_word(*sp + i * ENTRY_64 + 4 * j, word)
                        .ok_or(Unwind::Ended(crate::os::Ended::Fault))?;
                }
            } else {
                self.write_entry(*sp + i * ENTRY, entry)?;
            }
        }
        Ok(*sp)
    }

    /// Offers `regs`' service with the supervisor stack at `sp`, below the
    /// page blocks it names, and back where it was after.
    fn offer_below(&mut self, regs: &mut Regs, sp: u32, saved: u32) -> Result<(), Unwind> {
        self.set_supervisor_stack(sp);
        let offered = self.service(regs);
        self.set_supervisor_stack(saved);
        offered
    }

    /// The service a change of the address space issues: claimed or not.
    /// The 64-bit page services go with their 32-bit forms, the 64-bit
    /// first before a change and last after it.
    fn memory_service(&mut self, service: Service<'_>) -> Result<bool, Unwind> {
        let saved = self.supervisor_stack();
        let mut sp = saved;
        let mut regs = Regs::default();
        match service {
            Service::Memory {
                change,
                active_object,
            } => {
                regs.r[..3].copy_from_slice(&[change.cast_unsigned(), 0x11, active_object]);
            }
            Service::MemoryMoved { bytes, area } => {
                regs.r[..3].copy_from_slice(&[bytes.cast_unsigned(), 0x4E, area.cast_unsigned()]);
            }
            Service::DynamicAreaCreate { area } => regs.r[1..3].copy_from_slice(&[0x90, area]),
            Service::DynamicAreaRemove { area } => regs.r[1..3].copy_from_slice(&[0x91, area]),
            Service::DynamicAreaRenumber { old, new } => {
                regs.r[1..4].copy_from_slice(&[0x92, old, new]);
            }
            Service::PagesUnsafe { pages } => {
                let count = len32(pages.len());
                let wide = self.stack_block(&mut sp, pages, true)?;
                let narrow = self.stack_block(&mut sp, pages, false)?;
                let mut first = Regs::default();
                first.r[1..4].copy_from_slice(&[0xE3, wide, count]);
                self.offer_below(&mut first, sp, saved)?;
                regs.r[1..4].copy_from_slice(&[0x8E, narrow, count]);
            }
            Service::PagesSafe { old, new } => {
                let count = len32(old.len());
                let old_narrow = self.stack_block(&mut sp, old, false)?;
                let new_narrow = self.stack_block(&mut sp, new, false)?;
                let old_wide = self.stack_block(&mut sp, old, true)?;
                let new_wide = self.stack_block(&mut sp, new, true)?;
                let mut first = Regs::default();
                first.r[1..5].copy_from_slice(&[0x8F, count, old_narrow, new_narrow]);
                self.offer_below(&mut first, sp, saved)?;
                regs.r[1..5].copy_from_slice(&[0xE4, count, old_wide, new_wide]);
            }
            Service::ClaimFiq => regs.r[1] = 0x0C,
            Service::ReleaseFiq => regs.r[1] = 0x0B,
        }
        self.offer_below(&mut regs, sp, saved)?;
        Ok(regs.r[1] == 0)
    }
}

/// A seam call's unwind, kept for after the manager returns: the manager is
/// told the call refused, and makes no more.
fn kept(world: &mut World, unwind: Unwind) {
    world.unwinding.get_or_insert(unwind);
}

impl Os<'_> {
    /// A shrink handler's refusal: its message, which the PRM has `PreShrink`
    /// return where `PreGrow` returns a block, as the general refusal's text;
    /// that refusal itself for none.
    fn shrink_refusal(&mut self, message: u32) -> Option<GuestError> {
        if message == 0 {
            return None;
        }
        let (text, _) = self
            .read_until(message, crate::error::TEXT, |b| b == 0)
            .ok()?;
        let number = AreaError::NotAllMoved
            .number_and_token()
            .map_or(0, |(number, _)| number);
        match self.raw_error(number, &text) {
            Fail::Error(block) => Some(GuestError(block)),
            Fail::Unwind(unwind) => {
                kept(self.world, unwind);
                None
            }
        }
    }
}

impl Guest for World {
    fn pre_grow(
        &mut self,
        memory: &mut Memory,
        handler: Handler,
        block: &mut [PageEntry],
        bytes: u32,
        size: u32,
    ) -> Result<Allowed, Option<GuestError>> {
        if self.unwinding.is_some() {
            return Err(None);
        }
        let mut os = Os {
            memory,
            world: self,
        };
        if os.heap_handler(handler).is_some() {
            return Ok(Allowed::default());
        }
        let pages = bytes / PAGE_SIZE;
        let regs = Regs::new([
            reason::PRE_GROW,
            0,
            pages,
            bytes,
            size,
            PAGE_SIZE,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
        ]);
        let block = (!block.is_empty()).then_some(block);
        match os.area_handler(handler, regs, block) {
            Ok((regs, false)) => Ok(Allowed {
                reserved: regs.r[0] == RESV,
            }),
            Ok((regs, true)) => Err((regs.r[0] != 0).then_some(GuestError(regs.r[0]))),
            Err(unwind) => {
                kept(os.world, unwind);
                Err(None)
            }
        }
    }

    fn post_grow(
        &mut self,
        memory: &mut Memory,
        handler: Handler,
        block: &[PageEntry],
        bytes: u32,
        size: u32,
    ) {
        if self.unwinding.is_some() {
            return;
        }
        let mut os = Os {
            memory,
            world: self,
        };
        if let Some(heap) = os.heap_handler(handler) {
            os.heap_area_resized(heap, size);
            return;
        }
        let mut copy: Vec<PageEntry> = Vec::new();
        if !block.is_empty() && copy.try_reserve_exact(block.len()).is_ok() {
            copy.extend_from_slice(block);
        }
        let regs = Regs::new([
            reason::POST_GROW,
            0,
            bytes / PAGE_SIZE,
            bytes,
            size,
            PAGE_SIZE,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
        ]);
        let block = (!copy.is_empty()).then_some(copy.as_mut_slice());
        if let Err(unwind) = os.area_handler(handler, regs, block) {
            kept(os.world, unwind);
        }
    }

    fn pre_shrink(
        &mut self,
        memory: &mut Memory,
        handler: Handler,
        bytes: u32,
        size: u32,
    ) -> Result<u32, Option<GuestError>> {
        if self.unwinding.is_some() {
            return Err(None);
        }
        let mut os = Os {
            memory,
            world: self,
        };
        if let Some(heap) = os.heap_handler(handler) {
            return Ok(os.heap_area_shrink(heap, bytes));
        }
        let regs = Regs::new([
            reason::PRE_SHRINK,
            0,
            0,
            bytes,
            size,
            PAGE_SIZE,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
        ]);
        match os.area_handler(handler, regs, None) {
            Ok((regs, false)) => Ok(regs.r[3]),
            Ok((regs, true)) => Err(os.shrink_refusal(regs.r[0])),
            Err(unwind) => {
                kept(os.world, unwind);
                Err(None)
            }
        }
    }

    fn post_shrink(&mut self, memory: &mut Memory, handler: Handler, bytes: u32, size: u32) {
        if self.unwinding.is_some() {
            return;
        }
        let mut os = Os {
            memory,
            world: self,
        };
        if let Some(heap) = os.heap_handler(handler) {
            os.heap_area_resized(heap, size);
            return;
        }
        let regs = Regs::new([
            reason::POST_SHRINK,
            0,
            0,
            bytes,
            size,
            PAGE_SIZE,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
        ]);
        if let Err(unwind) = os.area_handler(handler, regs, None) {
            kept(os.world, unwind);
        }
    }

    fn test_shrink(&mut self, memory: &mut Memory, handler: Handler, size: u32) -> u32 {
        if self.unwinding.is_some() {
            return 0;
        }
        let mut os = Os {
            memory,
            world: self,
        };
        if let Some(heap) = os.heap_handler(handler) {
            return os.heap_area_spare(heap);
        }
        let regs = Regs::new([
            reason::TEST_SHRINK,
            0,
            0,
            0,
            size,
            PAGE_SIZE,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
        ]);
        match os.area_handler(handler, regs, None) {
            Ok((regs, false)) => regs.r[3],
            Ok((_, true)) => 0,
            Err(unwind) => {
                kept(os.world, unwind);
                0
            }
        }
    }

    fn resize_pmp(
        &mut self,
        memory: &mut Memory,
        handler: Handler,
        pages: i32,
        area: i32,
    ) -> Result<(), Option<GuestError>> {
        if self.unwinding.is_some() {
            return Err(None);
        }
        let mut os = Os {
            memory,
            world: self,
        };
        let regs = Regs::new([
            reason::RESIZE_PMP,
            pages.cast_unsigned(),
            area.cast_unsigned(),
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
        ]);
        match os.area_handler(handler, regs, None) {
            Ok((_, false)) => Ok(()),
            Ok((regs, true)) => Err((regs.r[0] != 0).then_some(GuestError(regs.r[0]))),
            Err(unwind) => {
                kept(os.world, unwind);
                Err(None)
            }
        }
    }

    fn service(&mut self, memory: &mut Memory, service: Service<'_>) -> bool {
        if self.unwinding.is_some() {
            return false;
        }
        let mut os = Os {
            memory,
            world: self,
        };
        match os.memory_service(service) {
            Ok(claimed) => claimed,
            Err(unwind) => {
                kept(os.world, unwind);
                false
            }
        }
    }

    fn moving_memory(&mut self, memory: &mut Memory, bytes: u32) -> bool {
        if self.unwinding.is_some() {
            return false;
        }
        let mut os = Os {
            memory,
            world: self,
        };
        let mut regs = Regs::new([MOVING_MEMORY, bytes, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        match os.call_vector(crate::vectors::UPCALL, &mut regs, Entry::SupervisorIrq) {
            Ok(()) => regs.r[0] == 0,
            Err(unwind) => {
                kept(os.world, unwind);
                false
            }
        }
    }

    fn active_object(&self) -> u32 {
        self.env.active_object()
    }

    fn in_interrupt(&self) -> bool {
        self.in_interrupt
    }
}

impl Os<'_> {
    /// The heap areas' handler entered from guest code at its host routine,
    /// as any area handler is: R0 the reason, R12 which heap.
    pub(crate) fn heap_area_entered(&mut self) -> Result<(), Unwind> {
        let cpu = self.world.processor.cpu();
        let reason = cpu.reg(tairix_riscos_arm::reg::Reg::field(0));
        let bytes = cpu.reg(tairix_riscos_arm::reg::Reg::field(3));
        let size = cpu.reg(tairix_riscos_arm::reg::Reg::field(4));
        let heap = match cpu.reg(tairix_riscos_arm::reg::Reg::field(12)) {
            0 => KernelHeap::System,
            _ => KernelHeap::Rma,
        };
        let (result, failed) = match reason {
            reason::PRE_GROW => (Some(0), false),
            reason::POST_GROW | reason::POST_SHRINK => {
                self.heap_area_resized(heap, size);
                (None, false)
            }
            reason::PRE_SHRINK => (Some(self.heap_area_shrink(heap, bytes)), false),
            reason::TEST_SHRINK => (Some(self.heap_area_spare(heap)), false),
            _ => match self.error(UNKNOWN_AREA_HANDLER) {
                Fail::Error(block) => (Some(block), true),
                Fail::Unwind(unwind) => return Err(unwind),
            },
        };
        let cpu = self.world.processor.cpu();
        if let Some(value) = result {
            let register = if failed || reason == reason::PRE_GROW {
                0
            } else {
                3
            };
            cpu.set_reg(tairix_riscos_arm::reg::Reg::field(register), value);
        }
        let flags = if failed {
            cpu.cpsr() | psr::V
        } else {
            cpu.cpsr() & !psr::V
        };
        cpu.set_nzcv(flags);
        self.return_to_lr();
        Ok(())
    }
}

#[cfg(test)]
mod tests;
