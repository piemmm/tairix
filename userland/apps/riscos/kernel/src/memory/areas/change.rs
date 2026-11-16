//! `OS_ChangeDynamicArea`: moving memory between an area and the free pool,
//! the free pool and application space, and taking particular pages.
//!
//! A shrink may move less than asked; a grow moves all of it or nothing,
//! save that a grow naming its pages moves them in chunks of 63 and stops
//! at the first chunk refused. Every area's tables are reserved when it is
//! made, so mapping its pages never needs memory.

use alloc::vec::Vec;

use tairix_riscos_arm::mmu::{Frame, FrameKind, Table, PAGE_SIZE};

use super::super::guest::{Guest, PageEntry, Service};
use super::super::layout::APPLICATION;
use super::super::{Holder, Memory};
use super::{
    lower_copy, signature, Area, AreaError, AreaFlags, Failure, Handler, Id, Key, Shape, FREE_POOL,
};

/// The most pages one grow handler call names: RISC OS's page block.
pub(super) const CHUNK: u32 = 63;

/// Where a page lives, for a replacement to move it from.
#[derive(Clone, Copy)]
struct Place {
    table: Table,
    address: u32,
}

/// `bytes` as a service call's signed count.
fn signed(bytes: u32) -> i32 {
    i32::try_from(bytes).unwrap_or(i32::MAX)
}

/// Area `number` as a service call names it; past the numbers a caller can
/// name, as the most it can.
pub(super) fn named(number: u32) -> i32 {
    signed(number)
}

/// The pages a block names, and those another holder has.
struct Wanted {
    /// Each page, with where in `moving` it is when another holder has it.
    pages: Vec<(Frame, Option<usize>)>,
    /// The pages another holder has, where it has them: the ones to move.
    moving: Vec<PageEntry>,
}

impl Memory {
    /// `OS_ChangeDynamicArea`: moves `bytes` into area `number`, or out of it
    /// when negative, returning the bytes moved. The free pool's change is
    /// application space's opposite one.
    pub fn change_area<G: Guest + ?Sized>(
        &mut self,
        number: i32,
        bytes: i32,
        guest: &mut G,
    ) -> Result<u32, Failure> {
        // Service handlers are not run from an interrupt.
        if guest.in_interrupt() {
            return Err(Failure::none(AreaError::NotAllMoved));
        }
        let refused = |memory: &mut Self, guest: &mut G| {
            guest.service(
                memory,
                Service::MemoryMoved {
                    bytes: 0,
                    area: number,
                },
            );
            Err(Failure::none(AreaError::NotAllMoved))
        };
        if self.areas.resizing {
            return refused(self, guest);
        }
        let Ok(area) = u32::try_from(number) else {
            return refused(self, guest);
        };
        let key = if area == FREE_POOL {
            None
        } else {
            let Some(key) = self.areas.id(area).ok().and_then(|id| self.areas.key(id)) else {
                return refused(self, guest);
            };
            let Some(found) = self.areas.area_at(key) else {
                return refused(self, guest);
            };
            match (&found.shape, found.handler) {
                (Shape::Sparse { .. }, _) => return refused(self, guest),
                (Shape::Pmp(_), Some(handler)) => {
                    return self.resize_pool(key.id, handler, bytes, number, guest);
                }
                _ => Some(key),
            }
        };
        // Rounded up to a page, so a shrink shrinks by less; RISC OS's
        // overflow near the top turns into a shrink, and does here too.
        let page = (PAGE_SIZE - 1).cast_signed();
        let rounded = bytes.wrapping_add(page) & !page;
        if rounded == 0 {
            guest.service(
                self,
                Service::MemoryMoved {
                    bytes: 0,
                    area: number,
                },
            );
            return Ok(0);
        }
        let amount = rounded.unsigned_abs();
        match (key, rounded > 0) {
            (None, true) => self.shrink_application(amount, guest),
            (None, false) => self.grow_application(amount, guest),
            (Some(key), false) => self.shrink_area(key, amount, true, guest),
            (Some(key), true) => self.grow_area(key, amount, true, guest),
        }
    }

    /// A pool's change, which its handler makes.
    fn resize_pool<G: Guest + ?Sized>(
        &mut self,
        id: Id,
        handler: Handler,
        bytes: i32,
        named: i32,
        guest: &mut G,
    ) -> Result<u32, Failure> {
        let before = self.pool_pages(id);
        let answer = guest.resize_pmp(self, handler, bytes >> 12, named);
        let moved = self
            .pool_pages(id)
            .abs_diff(before)
            .saturating_mul(PAGE_SIZE);
        answer.map(|()| moved).map_err(|error| Failure {
            error: AreaError::refused(error),
            moved,
        })
    }

    pub(super) fn pool_pages(&self, id: Id) -> u32 {
        match self.areas.area(id).map(|area| &area.shape) {
            Some(Shape::Pmp(pool)) => pool.pages(),
            _ => 0,
        }
    }

    /// Refuses a change to application space the current active object or
    /// a module objects to.
    fn check_application<G: Guest + ?Sized>(
        &mut self,
        change: i32,
        guest: &mut G,
    ) -> Result<(), AreaError> {
        let size = self
            .current_slot()
            .and_then(|slot| self.slot_size(slot))
            .unwrap_or(0);
        let active = guest.active_object();
        if active <= APPLICATION + size {
            if !guest.moving_memory(self, change.unsigned_abs()) {
                return Err(AreaError::Application);
            }
        } else if guest.service(
            self,
            Service::Memory {
                change,
                active_object: active,
            },
        ) {
            return Err(AreaError::ApplicationInUse);
        }
        Ok(())
    }

    /// The free pool grown by `bytes` of application space's top.
    pub(super) fn shrink_application<G: Guest + ?Sized>(
        &mut self,
        bytes: u32,
        guest: &mut G,
    ) -> Result<u32, Failure> {
        self.check_application(-signed(bytes), guest)
            .map_err(Failure::none)?;
        let slot = self
            .current_slot()
            .ok_or(Failure::none(AreaError::NotAllMoved))?;
        let moved = bytes.min(self.slot_size(slot).unwrap_or(0));
        self.shrink_slot(slot, moved / PAGE_SIZE);
        guest.service(
            self,
            Service::MemoryMoved {
                bytes: -signed(moved),
                area: -1,
            },
        );
        if moved < bytes {
            Err(Failure {
                error: AreaError::NotAllMoved,
                moved,
            })
        } else {
            Ok(moved)
        }
    }

    /// Application space grown by `bytes` from the free pool, wholly or not.
    fn grow_application<G: Guest + ?Sized>(
        &mut self,
        bytes: u32,
        guest: &mut G,
    ) -> Result<u32, Failure> {
        self.check_application(signed(bytes), guest)
            .map_err(Failure::none)?;
        let slot = self
            .current_slot()
            .ok_or(Failure::none(AreaError::NotAllMoved))?;
        let grown = self
            .slot_size(slot)
            .and_then(|size| size.checked_add(bytes))
            .ok_or(Failure::none(AreaError::NotAllMoved))?;
        // A slot that may not reach the size takes nothing from the
        // shrinkable areas.
        if !self.slot_may_reach(slot, grown) {
            return Err(Failure::none(AreaError::NotAllMoved));
        }
        self.free_for(bytes / PAGE_SIZE, None, guest);
        self.set_slot_size(slot, grown)
            .map_err(|_| Failure::none(AreaError::NotAllMoved))?;
        guest.service(
            self,
            Service::MemoryMoved {
                bytes: signed(bytes),
                area: -1,
            },
        );
        Ok(bytes)
    }

    /// Shrinks the shrinkable areas as they stand, most recently made first
    /// and `except` spared, until `pages` are free or none can give more.
    fn free_for<G: Guest + ?Sized>(&mut self, pages: u32, except: Option<Key>, guest: &mut G) {
        for key in self.areas.shrinkable_now() {
            if self.free_pages() >= pages {
                return;
            }
            if Some(key) == except {
                continue;
            }
            let Some(size) = self.areas.area_at(key).map(Area::size) else {
                continue;
            };
            let short = (pages - self.free_pages()).saturating_mul(PAGE_SIZE);
            let _ = self.shrink_area(key, short.min(size), true, guest);
        }
    }

    /// Frees `pages` for an area other than `except`: the shrinkable areas
    /// first, then application space's top where it holds the rest.
    pub(super) fn make_room<G: Guest + ?Sized>(
        &mut self,
        pages: u32,
        except: Option<Key>,
        guest: &mut G,
    ) -> Result<(), AreaError> {
        self.free_for(pages, except, guest);
        if self.free_pages() < pages {
            let application = self
                .current_slot()
                .and_then(|slot| self.slot_size(slot))
                .unwrap_or(0)
                / PAGE_SIZE;
            let short = pages - self.free_pages();
            // Application space that cannot cover the rest is not shrunk
            // for nothing.
            if application < short {
                return Err(AreaError::NotAllMoved);
            }
            self.shrink_application(short.saturating_mul(PAGE_SIZE), guest)
                .map_err(|failure| failure.error)?;
        }
        if self.free_pages() < pages {
            return Err(AreaError::NotAllMoved);
        }
        Ok(())
    }

    /// Takes up to `bytes` from the top of an area; `counted` when the Task
    /// Manager is to hear of it.
    pub(super) fn shrink_area<G: Guest + ?Sized>(
        &mut self,
        key: Key,
        bytes: u32,
        counted: bool,
        guest: &mut G,
    ) -> Result<u32, Failure> {
        let area = self
            .areas
            .area_at(key)
            .ok_or(Failure::none(AreaError::BadDynamicArea))?;
        // Only an area of its own pages gives them back by shrinking.
        if !matches!(area.shape, Shape::Pages(_)) {
            return Err(Failure::none(AreaError::NotAllMoved));
        }
        let (number, size, handler) = (area.number, area.size(), area.handler);
        let amount = bytes.min(size);
        let short = amount < bytes;
        let allowed = if amount == 0 {
            0
        } else {
            let was = core::mem::replace(&mut self.areas.resizing, true);
            let allowed = match handler {
                Some(handler) => guest
                    .pre_shrink(self, handler, amount, size)
                    .map(|allowed| allowed.min(amount) & !(PAGE_SIZE - 1)),
                None => Ok(amount),
            };
            let allowed = match allowed {
                Ok(allowed) => allowed,
                Err(error) => {
                    self.areas.resizing = was;
                    return Err(Failure::none(AreaError::refused(error)));
                }
            };
            if allowed > 0 {
                self.drop_area_pages(key.id, allowed / PAGE_SIZE);
            }
            self.areas.resizing = was;
            if allowed > 0 {
                if let Some(handler) = handler {
                    let size = self.areas.area_at(key).map_or(0, Area::size);
                    guest.post_shrink(self, handler, allowed, size);
                }
                if counted {
                    self.areas.note(number, signature::RESIZED);
                }
            }
            allowed
        };
        guest.service(
            self,
            Service::MemoryMoved {
                bytes: -signed(allowed),
                area: named(number),
            },
        );
        if short || allowed < amount {
            Err(Failure {
                error: AreaError::NotAllMoved,
                moved: allowed,
            })
        } else {
            Ok(allowed)
        }
    }

    /// Unmaps the top `pages` of a contiguous or doubly mapped area, giving
    /// them back to the free pool; a doubly mapped area's first copy then
    /// moves up by what left.
    fn drop_area_pages(&mut self, id: Id, pages: u32) {
        let Self {
            areas,
            mmu,
            pages: records,
            ..
        } = self;
        let Some(area) = areas.area_mut(id) else {
            return;
        };
        let (base, access, doubly) = (area.base, area.access, area.doubly());
        let Shape::Pages(list) = &mut area.shape else {
            return;
        };
        let old = u32::try_from(list.len()).unwrap_or(0);
        let keep = old.saturating_sub(pages);
        for (index, &frame) in (keep..).zip(&list[keep as usize..]) {
            let _ = mmu.unmap(Table::Global, base + index * PAGE_SIZE);
            if doubly {
                let _ = mmu.unmap(Table::Global, lower_copy(base, old, index));
            }
            records.free(frame);
        }
        list.truncate(keep as usize);
        if doubly {
            // Moving up, each page lands where one above it was, so the
            // highest moves first; the area's reservation is its own, so
            // the place left is free.
            for (index, &frame) in list.iter().enumerate().rev() {
                let index = u32::try_from(index).unwrap_or(0);
                let _ = mmu.unmap(Table::Global, lower_copy(base, old, index));
                let _ = mmu.map(Table::Global, lower_copy(base, keep, index), frame, access);
            }
        }
    }

    /// Adds `bytes` to an area, wholly or not at all but for a chunked
    /// grow's refused chunk.
    pub(super) fn grow_area<G: Guest + ?Sized>(
        &mut self,
        key: Key,
        bytes: u32,
        counted: bool,
        guest: &mut G,
    ) -> Result<u32, Failure> {
        let unmoved = |memory: &mut Self, number: u32, guest: &mut G, error: AreaError| {
            guest.service(
                memory,
                Service::MemoryMoved {
                    bytes: 0,
                    area: named(number),
                },
            );
            Err(Failure::none(error))
        };
        let area = self
            .areas
            .area_at(key)
            .ok_or(Failure::none(AreaError::BadDynamicArea))?;
        let (number, size, max) = (area.number, area.size(), area.max);
        if bytes > max.saturating_sub(size) {
            return unmoved(self, number, guest, AreaError::NotAllMoved);
        }
        let pages = bytes / PAGE_SIZE;
        self.make_room(pages, Some(key), guest)
            .map_err(Failure::none)?;
        // Guest code ran making room: the area may have gone, or grown.
        let Some(area) = self.areas.area_at(key) else {
            return unmoved(self, number, guest, AreaError::BadDynamicArea);
        };
        let (size, max, flags) = (area.size(), area.max, area.flags);
        if bytes > max.saturating_sub(size) {
            return unmoved(self, number, guest, AreaError::NotAllMoved);
        }
        let chunked = flags.has(AreaFlags::NEEDS_SPECIFIC_PAGES) || flags.has(AreaFlags::NEEDS_DMA);
        let moved = if chunked {
            self.grow_in_chunks(key.id, pages, guest)
        } else {
            self.grow_at_once(key.id, pages, guest)
        };
        let (Ok(done) | Err(Failure { moved: done, .. })) = moved;
        if counted && done > 0 {
            self.areas.note(number, signature::RESIZED);
        }
        guest.service(
            self,
            Service::MemoryMoved {
                bytes: signed(done),
                area: named(number),
            },
        );
        moved
    }

    /// A grow whose handler, if any, is asked once for all of it.
    fn grow_at_once<G: Guest + ?Sized>(
        &mut self,
        id: Id,
        pages: u32,
        guest: &mut G,
    ) -> Result<u32, Failure> {
        let bytes = pages * PAGE_SIZE;
        let area = self
            .areas
            .area(id)
            .ok_or(Failure::none(AreaError::BadDynamicArea))?;
        let (size, handler) = (area.size(), area.handler);
        let was = core::mem::replace(&mut self.areas.resizing, true);
        if let Some(handler) = handler {
            if let Err(error) = guest.pre_grow(self, handler, &mut [], bytes, size) {
                self.areas.resizing = was;
                return Err(Failure::none(AreaError::refused(error)));
            }
        }
        let added = self
            .take_pages(id, size / PAGE_SIZE, pages)
            .and_then(|frames| self.add_area_pages(id, &frames));
        self.areas.resizing = was;
        added.map_err(Failure::none)?;
        if let Some(handler) = handler {
            let size = self.areas.area(id).map_or(0, Area::size);
            guest.post_grow(self, handler, &[], bytes, size);
        }
        Ok(bytes)
    }

    /// Takes `count` free pages for an area's pages from `first` up, held.
    fn take_pages(&mut self, id: Id, first: u32, count: u32) -> Result<Vec<Frame>, AreaError> {
        let mut frames = Vec::new();
        frames
            .try_reserve(count as usize)
            .map_err(|_| AreaError::NotAllMoved)?;
        for index in first..first + count {
            let Ok(frame) = self.take_page(Holder::Area { area: id, index }) else {
                self.forget_pages(&frames);
                return Err(AreaError::NotAllMoved);
            };
            frames.push(frame);
        }
        Ok(frames)
    }

    /// Lets go of held pages that were never added, freeing them.
    pub(super) fn forget_pages(&mut self, frames: &[Frame]) {
        for &frame in frames {
            self.pages.free(frame);
            let _ = self.mmu.release(frame);
        }
    }

    /// Maps `frames`, held, onto the top of an area and lets go of them;
    /// on failure they are freed and the area is as it was.
    fn add_area_pages(&mut self, id: Id, frames: &[Frame]) -> Result<(), AreaError> {
        let Self { areas, mmu, .. } = self;
        let Some(area) = areas.area_mut(id) else {
            self.forget_pages(frames);
            return Err(AreaError::BadDynamicArea);
        };
        let (base, access, doubly) = (area.base, area.access, area.doubly());
        let Shape::Pages(list) = &mut area.shape else {
            self.forget_pages(frames);
            return Err(AreaError::NotAllMoved);
        };
        if list.try_reserve(frames.len()).is_err() {
            self.forget_pages(frames);
            return Err(AreaError::NotAllMoved);
        }
        let old = u32::try_from(list.len()).unwrap_or(0);
        let new = old + u32::try_from(frames.len()).unwrap_or(0);
        // Moving down, the highest page lands where the lowest was, so each
        // of the first copies moves after the one below it has; the new
        // pages' first copies go where the old ones were.
        if doubly {
            for (index, &frame) in (0u32..).zip(list.iter()) {
                let _ = mmu.unmap(Table::Global, lower_copy(base, old, index));
                let _ = mmu.map(Table::Global, lower_copy(base, new, index), frame, access);
            }
        }
        let mut mapped = 0;
        let mut failed = false;
        for (index, &frame) in (old..).zip(frames) {
            if mmu
                .map(Table::Global, base + index * PAGE_SIZE, frame, access)
                .is_err()
            {
                failed = true;
                break;
            }
            if doubly
                && mmu
                    .map(Table::Global, lower_copy(base, new, index), frame, access)
                    .is_err()
            {
                let _ = mmu.unmap(Table::Global, base + index * PAGE_SIZE);
                failed = true;
                break;
            }
            mapped += 1;
        }
        if failed {
            for (index, _) in (old..).zip(&frames[..mapped]) {
                let _ = mmu.unmap(Table::Global, base + index * PAGE_SIZE);
                if doubly {
                    let _ = mmu.unmap(Table::Global, lower_copy(base, new, index));
                }
            }
            if doubly {
                // Back up, the highest first, to where they were.
                for (index, &frame) in list.iter().enumerate().rev() {
                    let index = u32::try_from(index).unwrap_or(0);
                    let _ = mmu.unmap(Table::Global, lower_copy(base, new, index));
                    let _ = mmu.map(Table::Global, lower_copy(base, old, index), frame, access);
                }
            }
            self.forget_pages(frames);
            return Err(AreaError::NotAllMoved);
        }
        list.extend_from_slice(frames);
        for &frame in frames {
            let _ = mmu.release(frame);
        }
        Ok(())
    }

    /// A grow of an area that may name its pages: a chunk of at most 63 at
    /// a time, each asked of the handler, whose answer is taken whole.
    fn grow_in_chunks<G: Guest + ?Sized>(
        &mut self,
        id: Id,
        pages: u32,
        guest: &mut G,
    ) -> Result<u32, Failure> {
        let mut moved = 0;
        let mut left = pages;
        let was = core::mem::replace(&mut self.areas.resizing, true);
        while left > 0 {
            let chunk = left.min(CHUNK);
            if let Err(error) = self.grow_chunk(id, chunk, guest) {
                self.areas.resizing = was;
                return Err(Failure { error, moved });
            }
            moved += chunk * PAGE_SIZE;
            left -= chunk;
        }
        self.areas.resizing = was;
        Ok(moved)
    }

    fn grow_chunk<G: Guest + ?Sized>(
        &mut self,
        id: Id,
        count: u32,
        guest: &mut G,
    ) -> Result<(), AreaError> {
        let area = self.areas.area(id).ok_or(AreaError::BadDynamicArea)?;
        let (size, flags, handler, base) = (area.size(), area.flags, area.handler, area.base);
        let bytes = count * PAGE_SIZE;
        let mut block = [PageEntry {
            page: None,
            logical: 0,
            physical: 0,
        }; CHUNK as usize];
        let block = &mut block[..count as usize];
        for (index, entry) in (size / PAGE_SIZE..).zip(block.iter_mut()) {
            entry.logical = base + index * PAGE_SIZE;
        }
        // A DMA area's pages are the kernel's to choose; its handler sees
        // them, and may still refuse.
        let dma = flags.has(AreaFlags::NEEDS_DMA);
        if dma {
            self.pick_pages(block)?;
        }
        if let Some(handler) = handler {
            guest
                .pre_grow(self, handler, block, bytes, size)
                .map_err(AreaError::refused)?;
        }
        let named = !dma && block.first().is_some_and(|entry| entry.page.is_some());
        if !dma && !named {
            self.pick_pages(block)?;
        }
        if let Err(error) = self.claim_pages(id, size / PAGE_SIZE, block, named, guest) {
            if let Some(handler) = handler {
                guest.post_grow(self, handler, &[], 0, size);
            }
            return Err(error);
        }
        if let Some(handler) = handler {
            let size = self.areas.area(id).map_or(0, Area::size);
            guest.post_grow(self, handler, block, bytes, size);
        }
        Ok(())
    }

    /// Fills a page block with the lowest free pages.
    fn pick_pages(&self, block: &mut [PageEntry]) -> Result<(), AreaError> {
        let mut from = Frame::new(0);
        for entry in block.iter_mut() {
            let page = self.mmu.next_free(from).ok_or(AreaError::NotAllMoved)?;
            from = Frame::new(page.number() + 1);
            entry.page = Some(page.number());
            entry.physical = page.number() * PAGE_SIZE;
        }
        Ok(())
    }

    /// The pages a block names for area `id`. A page the area may not take,
    /// or named twice, refuses the whole block.
    fn wanted_pages(&self, id: Id, block: &[PageEntry]) -> Result<Wanted, AreaError> {
        let mut wanted: Vec<(Frame, Option<usize>)> = Vec::new();
        wanted
            .try_reserve(block.len())
            .map_err(|_| AreaError::NotAllMoved)?;
        let mut moving = Vec::new();
        moving
            .try_reserve(block.len())
            .map_err(|_| AreaError::NotAllMoved)?;
        for entry in block {
            let page = entry.page.ok_or(AreaError::CantGetPhysicalMemory)?;
            let frame = Frame::new(page);
            let record = self.pages.get(page);
            let kernel = record.holder == Some(Holder::Kernel);
            let ours = matches!(record.holder, Some(Holder::Area { area, .. }) if area == id);
            let unheld_in_use = record.holder.is_none() && !self.mmu.is_free(frame);
            if page >= self.ceiling.pages()
                || record.unavailable
                || kernel
                || ours
                || unheld_in_use
                || wanted.iter().any(|&(other, _)| other == frame)
            {
                return Err(AreaError::CantGetPhysicalMemory);
            }
            let mut held = None;
            if let Some(holder) = record.holder {
                held = Some(moving.len());
                moving.push(PageEntry {
                    page: Some(page),
                    logical: self.places_of(holder)[0].map_or(0, |place| place.address),
                    physical: page * PAGE_SIZE,
                });
            }
            wanted.push((frame, held));
        }
        Ok(Wanted {
            pages: wanted,
            moving,
        })
    }

    /// Takes the pages a block names into an area, as its pages from
    /// `first` up, replacing any another holder has with free ones carrying
    /// their contents. Pages a handler named become unavailable to others.
    fn claim_pages<G: Guest + ?Sized>(
        &mut self,
        id: Id,
        first: u32,
        block: &[PageEntry],
        named: bool,
        guest: &mut G,
    ) -> Result<(), AreaError> {
        let Wanted {
            pages: wanted,
            moving,
        } = self.wanted_pages(id, block)?;
        // Every page's record is made before any moves.
        if let Some(highest) = wanted.iter().map(|&(frame, _)| frame.number()).max() {
            self.pages
                .get_mut(highest)
                .map_err(|_| AreaError::NotAllMoved)?;
        }
        let announced = named && !moving.is_empty();
        if announced {
            guest.service(self, Service::PagesUnsafe { pages: &moving });
            guest.service(self, Service::ClaimFiq);
        }
        // Each page moved, and the page that took its place, in step.
        let mut moved = Vec::new();
        let mut replaced = Vec::new();
        let mut claimed = Vec::new();
        let reserved = moved
            .try_reserve(moving.len())
            .and_then(|()| replaced.try_reserve(moving.len()))
            .and_then(|()| claimed.try_reserve(block.len()));
        let mut outcome = reserved.map_err(|_| AreaError::NotAllMoved);
        // The free pages first, so no replacement is drawn from the block.
        // Guest code ran since they were looked at, so each is taken only
        // as it was then: a free page still free, a held one still held.
        let free = (first..)
            .zip(&wanted)
            .filter(|(_, (_, held))| held.is_none());
        let held = (first..)
            .zip(&wanted)
            .filter(|(_, (_, held))| held.is_some());
        for (index, &(frame, held)) in free.chain(held) {
            if outcome.is_err() {
                break;
            }
            let step = match held.and_then(|at| moving.get(at).copied()) {
                None => self
                    .mmu
                    .allocate_at(frame, FrameKind::Memory)
                    .map_err(|_| AreaError::CantGetPhysicalMemory),
                Some(before) => self.replace_page(frame).map(|new| {
                    moved.push(before);
                    replaced.push(PageEntry {
                        page: Some(new.number()),
                        logical: before.logical,
                        physical: new.number() * PAGE_SIZE,
                    });
                }),
            };
            if let Err(error) = step {
                outcome = Err(error);
                break;
            }
            // Its record was made above.
            if let Ok(record) = self.pages.get_mut(frame.number()) {
                record.holder = Some(Holder::Area { area: id, index });
                record.unavailable = named;
            }
            claimed.push(frame);
        }
        if announced {
            guest.service(self, Service::ReleaseFiq);
            guest.service(
                self,
                Service::PagesSafe {
                    old: &moved,
                    new: &replaced,
                },
            );
        }
        if let Err(error) = outcome {
            self.forget_pages(&claimed);
            return Err(error);
        }
        // The area takes them in the block's order; `claimed` has room.
        claimed.clear();
        claimed.extend(wanted.iter().map(|&(frame, _)| frame));
        self.add_area_pages(id, &claimed)
    }

    /// Moves `frame`'s holder onto a free frame carrying its contents and
    /// its watches, leaving `frame` held for the caller; returns the frame
    /// in its place.
    pub(super) fn replace_page(&mut self, frame: Frame) -> Result<Frame, AreaError> {
        let record = self.pages.get(frame.number());
        let holder = record.holder.ok_or(AreaError::CantGetPhysicalMemory)?;
        let places = self.places_of(holder);
        let new = self
            .mmu
            .allocate(FrameKind::Memory)
            .map_err(|_| AreaError::CantGetPhysicalMemory)?;
        // The new page's record is made before anything moves.
        let carried = self.pages.get_mut(new.number()).is_ok()
            && self
                .mmu
                .copy_frame(frame, new)
                .and_then(|()| self.mmu.move_watches(frame, new))
                .is_ok();
        if !carried {
            let _ = self.mmu.release(new);
            return Err(AreaError::CantGetPhysicalMemory);
        }
        let _ = self.mmu.hold(frame);
        for place in places.iter().flatten() {
            if let Some((_, access)) = self.mmu.lookup(place.table, place.address) {
                // Mapping where a page was just unmapped cannot fail.
                let _ = self.mmu.unmap(place.table, place.address);
                let _ = self.mmu.map(place.table, place.address, new, access);
            }
        }
        match holder {
            Holder::Slot { slot, index } => self.slots.replace(slot, index, new),
            Holder::Area { area, index } => self.replace_in_area(area, index, new),
            Holder::Kernel => {}
        }
        if let Ok(slot) = self.pages.get_mut(new.number()) {
            *slot = record;
        }
        let pool = matches!(holder, Holder::Area { area, .. } if self.is_pool(area));
        if !pool {
            // A pool keeps its pages held; anything else's are its mappings'.
            let _ = self.mmu.release(new);
        }
        self.pages.free(frame);
        Ok(new)
    }

    fn is_pool(&self, id: Id) -> bool {
        self.areas
            .area(id)
            .is_some_and(|area| matches!(area.shape, Shape::Pmp(_)))
    }

    /// Where a page `holder` holds is mapped.
    fn places_of(&self, holder: Holder) -> [Option<Place>; 2] {
        let global = |address: u32| Place {
            table: Table::Global,
            address,
        };
        match holder {
            Holder::Slot { slot, index } => [
                self.slots.page(slot, index).map(|(space, _)| Place {
                    table: Table::Space(space),
                    address: APPLICATION + index * PAGE_SIZE,
                }),
                None,
            ],
            Holder::Area { area, index } => match self.areas.area(area) {
                Some(area) => match &area.shape {
                    Shape::Pages(_) => {
                        let (upper, lower) = area.addresses(index);
                        [Some(global(upper)), lower.map(global)]
                    }
                    Shape::Sparse { .. } => [Some(global(area.base + index * PAGE_SIZE)), None],
                    Shape::Pmp(pool) => [
                        pool.window_of(index)
                            .map(|window| global(area.base + window * PAGE_SIZE)),
                        None,
                    ],
                },
                None => [None, None],
            },
            Holder::Kernel => [None, None],
        }
    }

    fn replace_in_area(&mut self, id: Id, index: u32, new: Frame) {
        let Some(area) = self.areas.area_mut(id) else {
            return;
        };
        match &mut area.shape {
            Shape::Pages(list) => {
                if let Some(page) = list.get_mut(index as usize) {
                    *page = new;
                }
            }
            Shape::Sparse { pages, .. } => {
                if let Some(page) = pages.get_mut(&index) {
                    *page = new;
                }
            }
            Shape::Pmp(pool) => pool.replace(index, new),
        }
    }
}
