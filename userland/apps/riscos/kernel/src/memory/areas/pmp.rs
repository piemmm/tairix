//! Physical memory pools (`OS_DynamicArea` 21 to 25): an area that owns a
//! list of physical pages and maps any of them anywhere in its window.
//!
//! A pool's pages are held whether mapped or not, so unmapping one never
//! frees it; releasing it from its slot does.

use alloc::vec::Vec;

use alloc::collections::TryReserveError;
use tairix_riscos_arm::mmu::{Access, Frame, FrameKind, Table, PAGE_SIZE};

use super::super::guest::{Guest, Service};
use super::super::{Holder, Memory};
use super::{AccessPrivilege, AreaError, AreaFlags, Id, Shape};

/// The page flags a logical op may set: the access privilege, the cache
/// bits and policy, and unavailability.
const LOGICAL_FLAGS: u32 = 0xF03F;
/// A page claimed for its owner alone.
const UNAVAILABLE: u32 = 1 << 15;
/// Permission to make a reserved page unavailable.
const RESERVED: u32 = 1 << 7;

/// A pool's pages: its page list and where in its window each is mapped.
pub(super) struct Pool {
    /// Each slot's page and its flags, if it has one.
    slots: Vec<Option<(Frame, u32)>>,
    /// Where each slot's page is mapped, by window page index.
    mapped_at: Vec<Option<u32>>,
    /// The page each window page shows, by slot.
    window: tairix_collections::OrderedMap<u32, u32>,
    /// The end of the window ever mapped.
    high_water: u32,
    filled: u32,
}

impl Pool {
    pub(super) fn new(capacity: u32, base: u32) -> Result<Self, TryReserveError> {
        let mut slots = Vec::new();
        slots.try_reserve_exact(capacity as usize)?;
        slots.resize(capacity as usize, None);
        let mut mapped_at = Vec::new();
        mapped_at.try_reserve_exact(capacity as usize)?;
        mapped_at.resize(capacity as usize, None);
        Ok(Self {
            slots,
            mapped_at,
            window: tairix_collections::OrderedMap::new(),
            high_water: base,
            filled: 0,
        })
    }

    /// The pages it owns.
    pub(super) const fn pages(&self) -> u32 {
        self.filled
    }

    /// The most its page list holds.
    pub(super) fn capacity(&self) -> u32 {
        u32::try_from(self.slots.len()).unwrap_or(u32::MAX)
    }

    /// The pages mapped in its window.
    pub(super) fn mapped(&self) -> usize {
        self.window.len()
    }

    /// Where slot `slot`'s page is mapped, by window page index.
    pub(super) fn window_of(&self, slot: u32) -> Option<u32> {
        *self.mapped_at.get(slot as usize)?
    }

    /// Puts `new` in slot `slot`, a replacement for its page.
    pub(super) fn replace(&mut self, slot: u32, new: Frame) {
        if let Some(Some((page, _))) = self.slots.get_mut(slot as usize) {
            *page = new;
        }
    }

    fn page(&self, slot: u32) -> Option<(Frame, u32)> {
        *self.slots.get(slot as usize)?
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
    /// The flags: unavailability asked, and permission over a reservation.
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
                claims += 1;
                peak = peak.max(claims);
            }
        }
        if self.free_pages() < peak {
            self.free_for(peak, Some(id), guest);
            if self.free_pages() < peak {
                let short = peak - self.free_pages();
                let _ = self.shrink_application(short * PAGE_SIZE, guest);
            }
            if self.free_pages() < peak {
                return Err((AreaError::NotAllMoved, 0));
            }
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
                area: i32::try_from(number).unwrap_or(-1),
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
        let permitted = op.flags & RESERVED != 0;
        let held = pool.page(op.slot);
        let keep = match (held, op.page) {
            (Some((page, _)), -2) => Some(page),
            (Some((page, _)), wanted) if i64::from(wanted) == i64::from(page.number()) => {
                Some(page)
            }
            _ => None,
        };
        if let Some(page) = keep {
            let record = self.pages.get(page.number());
            if unavailable && record.reserved && !permitted {
                return Err(AreaError::CantGetPhysicalMemory);
            }
            if let Ok(record) = self.pages.get_mut(page.number()) {
                record.unavailable = unavailable;
            }
            return Ok(());
        }
        if let Some((page, _)) = held {
            let pool = self.pool_mut(id).ok_or(AreaError::BadDynamicArea)?;
            if pool.mapped_at[op.slot as usize].is_some() {
                return Err(AreaError::NotAllMoved);
            }
            pool.slots[op.slot as usize] = None;
            pool.filled -= 1;
            self.pages.free(page);
            let _ = self.mmu.release(page);
        }
        let frame = match op.page {
            -1 => return Ok(()),
            -2 => {
                let mut from = Frame::new(0);
                loop {
                    let page = self.mmu.next_free(from).ok_or(AreaError::NotAllMoved)?;
                    from = Frame::new(page.number() + 1);
                    if !(unavailable && self.pages.get(page.number()).reserved) {
                        self.mmu
                            .allocate_at(page, FrameKind::Memory)
                            .map_err(|_| AreaError::NotAllMoved)?;
                        break page;
                    }
                }
            }
            page => {
                let page = Frame::new(u32::try_from(page).map_err(|_| AreaError::BadPageNumber)?);
                let record = self.pages.get(page.number());
                if record.unavailable || record.holder == Some(Holder::Kernel) {
                    return Err(AreaError::NotAllMoved);
                }
                if record.reserved && unavailable && !permitted {
                    return Err(AreaError::CantGetPhysicalMemory);
                }
                if self.mmu.is_free(page) {
                    self.mmu
                        .allocate_at(page, FrameKind::Memory)
                        .map_err(|_| AreaError::NotAllMoved)?;
                } else {
                    self.replace_page(page)?;
                }
                page
            }
        };
        if let Ok(record) = self.pages.get_mut(frame.number()) {
            record.holder = Some(Holder::Area {
                area: id,
                index: op.slot,
            });
            record.unavailable = unavailable;
        }
        let flags = area_flags | if unavailable { UNAVAILABLE } else { 0 };
        if let Some(pool) = self.pool_mut(id) {
            pool.slots[op.slot as usize] = Some((frame, flags));
            pool.filled += 1;
        }
        Ok(())
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
                pool.mapped_at[slot as usize] = None;
                let _ = self.mmu.unmap(Table::Global, address);
            }
            return Ok(());
        };
        let (frame, _) = pool.page(slot).ok_or(AreaError::BadPageNumber)?;
        let flags = op.flags & LOGICAL_FLAGS;
        let access: Access =
            AccessPrivilege::of(flags & AreaFlags::PRIVILEGE).ok_or(AreaError::BadParameters)?;
        let record = self.pages.get(frame.number());
        if flags & UNAVAILABLE != 0 && record.reserved && op.flags & RESERVED == 0 {
            return Err(AreaError::CantGetPhysicalMemory);
        }
        let pool = self.pool_mut(id).ok_or(AreaError::BadDynamicArea)?;
        let elsewhere = pool.mapped_at[slot as usize].filter(|&window| window != op.window);
        if let Some(window) = elsewhere {
            pool.window.remove(&window);
            let _ = self.mmu.unmap(Table::Global, base + window * PAGE_SIZE);
        }
        let pool = self.pool_mut(id).ok_or(AreaError::BadDynamicArea)?;
        if let Some(other) = occupant.filter(|&other| other != slot) {
            pool.mapped_at[other as usize] = None;
        }
        if occupant.is_some() {
            let _ = self.mmu.unmap(Table::Global, address);
        }
        let pool = self.pool_mut(id).ok_or(AreaError::BadDynamicArea)?;
        pool.window
            .try_insert(op.window, slot)
            .map_err(|_| AreaError::NotAllMoved)?;
        pool.mapped_at[slot as usize] = Some(op.window);
        pool.high_water = pool.high_water.max(address + PAGE_SIZE);
        if let Some(Some((_, page_flags))) = pool.slots.get_mut(slot as usize) {
            *page_flags = (*page_flags & !LOGICAL_FLAGS) | flags;
        }
        self.mmu
            .map(Table::Global, address, frame, access)
            .map_err(|_| AreaError::CantAllocateLevel2)?;
        if let Ok(record) = self.pages.get_mut(frame.number()) {
            record.unavailable = flags & UNAVAILABLE != 0;
        }
        Ok(())
    }

    /// `OS_DynamicArea` 23: lengthens a pool's page list, or trims its empty
    /// slots from the end; the slots changed.
    pub fn pool_resize(&mut self, number: u32, change: i32) -> Result<u32, AreaError> {
        let (id, pool) = self.pool(number)?;
        let capacity = pool.capacity();
        let ram = self.ceiling.pages();
        let pool = self.pool_mut(id).ok_or(AreaError::BadDynamicArea)?;
        if change >= 0 {
            let added = change.unsigned_abs().min(ram.saturating_sub(capacity));
            let len = (capacity + added) as usize;
            pool.slots
                .try_reserve(added as usize)
                .map_err(|_| AreaError::BadDynamicArea)?;
            pool.mapped_at
                .try_reserve(added as usize)
                .map_err(|_| AreaError::BadDynamicArea)?;
            pool.slots.resize(len, None);
            pool.mapped_at.resize(len, None);
            Ok(added)
        } else {
            let wanted = change.unsigned_abs().min(capacity);
            let empty = pool
                .slots
                .iter()
                .rev()
                .take(wanted as usize)
                .take_while(|slot| slot.is_none())
                .count();
            let len = pool.slots.len() - empty;
            pool.slots.truncate(len);
            pool.mapped_at.truncate(len);
            Ok(u32::try_from(empty).unwrap_or(0))
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
        let windows: Vec<u32> = pool.window.iter().map(|(&window, _)| window).collect();
        let pages: Vec<Frame> = pool
            .slots
            .iter()
            .flatten()
            .map(|&(frame, _)| frame)
            .collect();
        pool.window = tairix_collections::OrderedMap::new();
        pool.slots.iter_mut().for_each(|slot| *slot = None);
        pool.mapped_at.iter_mut().for_each(|slot| *slot = None);
        pool.filled = 0;
        for window in windows {
            let _ = self.mmu.unmap(Table::Global, base + window * PAGE_SIZE);
        }
        for page in pages {
            self.pages.free(page);
            let _ = self.mmu.release(page);
        }
        Ok(())
    }
}
