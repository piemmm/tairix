//! Physical memory pools (`OS_DynamicArea` 21 to 25): an area that owns a
//! list of physical pages and maps any of them anywhere in its window.
//!
//! A pool's pages are held whether mapped or not, so unmapping one never
//! frees it; releasing it from its slot does. A page list costs nothing for
//! its empty slots: only those holding a page are kept.

use tairix_collections::OrderedMap;
use tairix_riscos_arm::mmu::{Access, Frame, FrameKind, Table, PAGE_SIZE};

use super::super::guest::{Guest, Service};
use super::super::map::UNAVAILABLE;
use super::super::{Holder, Memory, Record, RESERVED};
use super::change::named;
use super::{access_privilege, AreaError, AreaFlags, Id, Shape};

/// The page flags a logical op may set: the access privilege, the cache
/// bits and policy, and unavailability.
const LOGICAL_FLAGS: u32 = 0xF03F;

/// A pool's pages: its page list and where in its window each is mapped.
pub(super) struct Pool {
    /// Each filled slot's page and its flags.
    slots: OrderedMap<u32, (Frame, u32)>,
    /// Where each mapped slot's page is, by window page index.
    mapped_at: OrderedMap<u32, u32>,
    /// The slot each mapped window page shows.
    window: OrderedMap<u32, u32>,
    /// The slots its page list has, filled or not.
    capacity: u32,
}

impl Pool {
    pub(super) const fn new(capacity: u32) -> Self {
        Self {
            slots: OrderedMap::new(),
            mapped_at: OrderedMap::new(),
            window: OrderedMap::new(),
            capacity,
        }
    }

    /// The pages it owns.
    pub(super) fn pages(&self) -> u32 {
        u32::try_from(self.slots.len()).unwrap_or(u32::MAX)
    }

    /// The most its page list holds.
    pub(super) const fn capacity(&self) -> u32 {
        self.capacity
    }

    /// The pages mapped in its window.
    pub(super) fn mapped(&self) -> usize {
        self.window.len()
    }

    /// Where slot `slot`'s page is mapped, by window page index.
    pub(super) fn window_of(&self, slot: u32) -> Option<u32> {
        self.mapped_at.get(&slot).copied()
    }

    /// Puts `new` in slot `slot`, a replacement for its page.
    pub(super) fn replace(&mut self, slot: u32, new: Frame) {
        if let Some((page, _)) = self.slots.get_mut(&slot) {
            *page = new;
        }
    }

    fn page(&self, slot: u32) -> Option<(Frame, u32)> {
        self.slots.get(&slot).copied()
    }
}

/// One entry of `OS_DynamicArea` 21's list.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PhysicalOp {
    /// The page list's slot.
    pub slot: u32,
    /// The page: -1 releases the slot's, -2 lets the kernel choose, any
    /// other takes that page.
    pub page: i32,
    /// The flags: unavailability asked.
    pub flags: u32,
}

/// One entry of `OS_DynamicArea` 22's list.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LogicalOp {
    /// The window's page index.
    pub window: u32,
    /// The slot whose page to map there, or none to unmap it.
    pub slot: Option<u32>,
    /// The page's flags there.
    pub flags: u32,
}

/// One entry of `OS_DynamicArea` 25's list: one of slot, page and window
/// index given, the others found.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct PageQuery {
    /// The page list's slot.
    pub slot: Option<u32>,
    /// The physical page.
    pub page: Option<u32>,
    /// The window's page index.
    pub window: Option<u32>,
    /// The page's flags.
    pub flags: u32,
}

impl Memory {
    fn pool(&self, number: u32) -> Result<(Id, &Pool), AreaError> {
        let id = self.areas.id(number)?;
        match self.areas.area(id).map(|area| &area.shape) {
            Some(Shape::Pmp(pool)) => Ok((id, pool)),
            _ => Err(AreaError::BadDynamicArea),
        }
    }

    fn pool_mut(&mut self, id: Id) -> Option<&mut Pool> {
        match &mut self.areas.area_mut(id)?.shape {
            Shape::Pmp(pool) => Some(pool),
            _ => None,
        }
    }

    /// `OS_DynamicArea` 21: claims and releases a pool's physical pages,
    /// entry by entry. On failure, the index of the entry that failed.
    pub fn pool_physical<G: Guest + ?Sized>(
        &mut self,
        number: u32,
        ops: &[PhysicalOp],
        guest: &mut G,
    ) -> Result<(), (AreaError, usize)> {
        let (id, pool) = self.pool(number).map_err(|error| (error, 0))?;
        if self.areas.resizing {
            return Err((AreaError::NotAllMoved, 0));
        }
        if ops.is_empty() {
            return Ok(());
        }
        let (capacity, before) = (pool.capacity(), pool.pages());
        let key = self.areas.key(id).ok_or((AreaError::BadDynamicArea, 0))?;
        let mut claims = 0u32;
        let mut peak = 0u32;
        for (index, op) in ops.iter().enumerate() {
            let specific = u32::try_from(op.page).ok();
            if op.slot >= capacity || specific.is_some_and(|page| page >= self.ceiling.pages()) {
                return Err((AreaError::BadPageNumber, index));
            }
            if op.page == -1 {
                claims = claims.saturating_sub(1);
            } else {
                claims = claims.saturating_add(1);
                peak = peak.max(claims);
            }
        }
        // A claim of a slot already holding a page gives that page back
        // first, so no list needs more new pages than there are empty slots.
        let peak = peak.min(capacity.saturating_sub(before));
        if self.free_pages() < peak && self.make_room(peak, Some(key), guest).is_err() {
            return Err((AreaError::NotAllMoved, 0));
        }
        // Guest code ran making room: the pool may have gone.
        if self.areas.area_at(key).is_none() {
            return Err((AreaError::BadDynamicArea, 0));
        }
        let mut outcome = Ok(());
        for (index, op) in ops.iter().enumerate() {
            if let Err(error) = self.physical_op(id, *op) {
                outcome = Err((error, index));
                break;
            }
        }
        let after = self.pool_pages(id);
        let moved =
            i32::try_from(after.abs_diff(before).saturating_mul(PAGE_SIZE)).unwrap_or(i32::MAX);
        guest.service(
            self,
            Service::MemoryMoved {
                bytes: moved,
                area: named(number),
            },
        );
        outcome
    }

    fn physical_op(&mut self, id: Id, op: PhysicalOp) -> Result<(), AreaError> {
        let area_flags = self
            .areas
            .area(id)
            .map_or(0, |area| area.flags.0 & AreaFlags::PAGE_BITS);
        let pool = self.pool_mut(id).ok_or(AreaError::BadDynamicArea)?;
        if op.slot >= pool.capacity() {
            return Err(AreaError::BadPageNumber);
        }
        let unavailable = op.flags & UNAVAILABLE != 0;
        // Locking a reserved page says it knows the page is reserved.
        let lock_refused =
            |record: Record| unavailable && record.reserved && op.flags & RESERVED == 0;
        let held = pool.page(op.slot);
        let keep = match (held, op.page) {
            (Some((page, _)), -2) => Some(page),
            (Some((page, _)), wanted) if i64::from(wanted) == i64::from(page.number()) => {
                Some(page)
            }
            _ => None,
        };
        if let Some(page) = keep {
            if lock_refused(self.pages.get(page.number())) {
                return Err(AreaError::PageReserved);
            }
            if let Ok(record) = self.pages.get_mut(page.number()) {
                record.unavailable = unavailable;
            }
            return Ok(());
        }
        let pool = self.pool_mut(id).ok_or(AreaError::BadDynamicArea)?;
        if held.is_some() && pool.mapped_at.get(&op.slot).is_some() {
            return Err(AreaError::NotAllMoved);
        }
        // Whatever can refuse the claim is asked before the slot's old page
        // goes, so a refusal changes nothing.
        let wanted = match op.page {
            -1 => None,
            -2 => Some(self.free_unreserved().ok_or(AreaError::NotAllMoved)?),
            page => {
                let page = u32::try_from(page).map_err(|_| AreaError::BadPageNumber)?;
                let record = self.pages.get(page);
                if lock_refused(record) {
                    return Err(AreaError::PageReserved);
                }
                if record.unavailable || record.holder == Some(Holder::Kernel) {
                    return Err(AreaError::NotAllMoved);
                }
                Some(Frame::new(page))
            }
        };
        if let Some(frame) = wanted {
            self.pages
                .get_mut(frame.number())
                .map_err(|_| AreaError::NotAllMoved)?;
            self.pool_mut(id)
                .ok_or(AreaError::BadDynamicArea)?
                .slots
                .try_reserve(1)
                .map_err(|_| AreaError::NotAllMoved)?;
        }
        if let Some((page, _)) = held {
            if let Some(pool) = self.pool_mut(id) {
                pool.slots.remove(&op.slot);
            }
            self.pages.free(page);
            let _ = self.mmu.release(page);
        }
        let Some(frame) = wanted else {
            return Ok(());
        };
        // Only the host's own memory can fail these now; the old page, freed,
        // is there for a replacement.
        if self.mmu.is_free(frame) {
            self.mmu
                .allocate_at(frame, FrameKind::Memory)
                .map_err(|_| AreaError::NotAllMoved)?;
        } else {
            self.replace_page(frame)?;
        }
        if let Ok(record) = self.pages.get_mut(frame.number()) {
            record.holder = Some(Holder::Area {
                area: id,
                index: op.slot,
            });
            record.unavailable = unavailable;
        }
        let flags = area_flags | if unavailable { UNAVAILABLE } else { 0 };
        if let Some(pool) = self.pool_mut(id) {
            // Room was reserved for the entry above.
            let _ = pool.slots.try_insert(op.slot, (frame, flags));
        }
        Ok(())
    }

    /// The lowest free page no one reserved, for a claim that lets the
    /// kernel choose.
    fn free_unreserved(&self) -> Option<Frame> {
        let mut from = Frame::new(0);
        loop {
            let frame = self.mmu.next_free(from)?;
            if !self.pages.get(frame.number()).reserved {
                return Some(frame);
            }
            from = Frame::new(frame.number().checked_add(1)?);
        }
    }

    /// `OS_DynamicArea` 22: maps and unmaps a pool's pages in its window,
    /// entry by entry. On failure, the index of the entry that failed.
    pub fn pool_logical(
        &mut self,
        number: u32,
        ops: &[LogicalOp],
    ) -> Result<(), (AreaError, usize)> {
        let (id, _) = self.pool(number).map_err(|error| (error, 0))?;
        if self.areas.resizing {
            return Err((AreaError::NotAllMoved, 0));
        }
        for (index, op) in ops.iter().enumerate() {
            self.logical_op(id, *op).map_err(|error| (error, index))?;
        }
        Ok(())
    }

    fn logical_op(&mut self, id: Id, op: LogicalOp) -> Result<(), AreaError> {
        let area = self.areas.area(id).ok_or(AreaError::BadDynamicArea)?;
        let (base, max) = (area.base, area.max);
        if op.window >= max / PAGE_SIZE {
            return Err(AreaError::BadPageNumber);
        }
        let address = base + op.window * PAGE_SIZE;
        let pool = self.pool_mut(id).ok_or(AreaError::BadDynamicArea)?;
        let occupant = pool.window.get(&op.window).copied();
        let Some(slot) = op.slot else {
            if let Some(slot) = occupant {
                pool.window.remove(&op.window);
                pool.mapped_at.remove(&slot);
                let _ = self.mmu.unmap(Table::Global, address);
            }
            return Ok(());
        };
        let (frame, _) = pool.page(slot).ok_or(AreaError::BadPageNumber)?;
        let flags = op.flags & LOGICAL_FLAGS;
        let access: Access =
            access_privilege(flags & AreaFlags::PRIVILEGE).ok_or(AreaError::BadParameters)?;
        // Room for both entries first, so nothing below can be half done.
        pool.window
            .try_reserve(1)
            .and_then(|()| pool.mapped_at.try_reserve(1))
            .map_err(|_| AreaError::NotAllMoved)?;
        let elsewhere = pool
            .mapped_at
            .get(&slot)
            .copied()
            .filter(|&window| window != op.window);
        if let Some(window) = elsewhere {
            pool.window.remove(&window);
            let _ = self.mmu.unmap(Table::Global, base + window * PAGE_SIZE);
        }
        let pool = self.pool_mut(id).ok_or(AreaError::BadDynamicArea)?;
        if let Some(other) = occupant.filter(|&other| other != slot) {
            pool.mapped_at.remove(&other);
        }
        if occupant.is_some() {
            let _ = self.mmu.unmap(Table::Global, address);
        }
        let mapped = self.mmu.map(Table::Global, address, frame, access);
        let pool = self.pool_mut(id).ok_or(AreaError::BadDynamicArea)?;
        if mapped.is_err() {
            // Unmapped from where it was, and mapped nowhere.
            pool.window.remove(&op.window);
            pool.mapped_at.remove(&slot);
            return Err(AreaError::CantAllocateLevel2);
        }
        let _ = pool.window.try_insert(op.window, slot);
        let _ = pool.mapped_at.try_insert(slot, op.window);
        if let Some((_, page_flags)) = pool.slots.get_mut(&slot) {
            *page_flags = (*page_flags & !LOGICAL_FLAGS) | flags;
        }
        if let Ok(record) = self.pages.get_mut(frame.number()) {
            record.unavailable = flags & UNAVAILABLE != 0;
        }
        Ok(())
    }

    /// `OS_DynamicArea` 23: lengthens a pool's page list, or trims its empty
    /// slots from the end; the slots changed.
    pub fn pool_resize(&mut self, number: u32, change: i32) -> Result<u32, AreaError> {
        let (id, _) = self.pool(number)?;
        let ram = self.ceiling.pages();
        let pool = self.pool_mut(id).ok_or(AreaError::BadDynamicArea)?;
        let capacity = pool.capacity;
        if change >= 0 {
            let added = change.unsigned_abs().min(ram.saturating_sub(capacity));
            pool.capacity += added;
            Ok(added)
        } else {
            let wanted = change.unsigned_abs().min(capacity);
            let filled_to = pool.slots.last_key_value().map_or(0, |(&slot, _)| slot + 1);
            let trimmed = wanted.min(capacity - filled_to.min(capacity));
            pool.capacity -= trimmed;
            Ok(trimmed)
        }
    }

    /// `OS_DynamicArea` 25: fills in each query's page, slot, window index
    /// and flags from the one it gives. On failure, the index that failed;
    /// the queries before it are answered.
    pub fn pool_query(
        &self,
        number: u32,
        queries: &mut [PageQuery],
    ) -> Result<(), (AreaError, usize)> {
        let (id, pool) = self.pool(number).map_err(|error| (error, 0))?;
        for (index, query) in queries.iter_mut().enumerate() {
            let slot = match (query.slot, query.page, query.window) {
                (Some(slot), _, _) => {
                    if slot >= pool.capacity() {
                        return Err((AreaError::BadPageNumber, index));
                    }
                    Some(slot)
                }
                (None, Some(page), _) => {
                    if page >= self.ceiling.pages() {
                        return Err((AreaError::BadPageNumber, index));
                    }
                    match self.pages.get(page).holder {
                        Some(Holder::Area { area, index }) if area == id => Some(index),
                        _ => None,
                    }
                }
                (None, None, Some(window)) => pool.window.get(&window).copied(),
                (None, None, None) => None,
            };
            let found = slot.and_then(|slot| pool.page(slot).map(|page| (slot, page)));
            if let Some((slot, (frame, flags))) = found {
                query.slot = Some(slot);
                query.page = Some(frame.number());
                query.window = pool.window_of(slot);
                let unavailable = if self.pages.get(frame.number()).unavailable {
                    UNAVAILABLE
                } else {
                    0
                };
                query.flags = (flags & LOGICAL_FLAGS & !UNAVAILABLE) | unavailable;
            } else {
                if query.slot.is_some() || query.page.is_some() {
                    query.window = None;
                }
                query.flags = 0;
            }
        }
        Ok(())
    }

    /// Unmaps and releases every page of a pool, as removing it does.
    pub(super) fn empty_pool(&mut self, id: Id) -> Result<(), AreaError> {
        let base = self.areas.area(id).map_or(0, |area| area.base);
        let Some(pool) = self.pool_mut(id) else {
            return Err(AreaError::BadDynamicArea);
        };
        let window = core::mem::take(&mut pool.window);
        let slots = core::mem::take(&mut pool.slots);
        pool.mapped_at.clear();
        for (&page, _) in &window {
            let _ = self.mmu.unmap(Table::Global, base + page * PAGE_SIZE);
        }
        for (_, &(frame, _)) in &slots {
            self.pages.free(frame);
            let _ = self.mmu.release(frame);
        }
        Ok(())
    }
}
