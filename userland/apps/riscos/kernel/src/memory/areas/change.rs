//! `OS_ChangeDynamicArea`: moving memory between an area and the free pool,
//! the free pool and application space, and taking particular pages.
//!
//! A shrink may move less than asked; a grow moves all of it or nothing,
//! save that a grow naming its pages moves them in chunks of 63 and stops
//! at the first chunk refused. Every area's tables are reserved when it is
//! made, so mapping its pages never needs memory.

use alloc::vec::Vec;

use tairix_riscos_arm::mmu::{Frame, FrameKind, Table, PAGE_SIZE};

use super::super::guest::{Granted, Guest, PageEntry, Service};
use super::super::layout::APPLICATION;
use super::super::{Holder, Memory};
use super::{signature, Area, AreaError, AreaFlags, Failure, Handler, Id, Shape, FREE_POOL};

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
        if self.areas.resizing || guest.in_interrupt() {
            return refused(self, guest);
        }
        let Ok(area) = u32::try_from(number) else {
            return refused(self, guest);
        };
        let id = if area == FREE_POOL {
            None
        } else {
            let Ok(id) = self.areas.id(area) else {
                return refused(self, guest);
            };
            let Some(found) = self.areas.area(id) else {
                return refused(self, guest);
            };
            match (&found.shape, found.handler) {
                (Shape::Sparse { .. }, _) => return refused(self, guest),
                (Shape::Pmp(_), Some(handler)) => {
                    return self.resize_pool(id, handler, bytes, number, guest);
                }
                _ => Some(id),
            }
        };
        // Rounded up to a page, so a shrink shrinks by less; RISC OS's
        // overflow near the top turns into a shrink, and does here too.
        let rounded = (i64::from(bytes) + i64::from(PAGE_SIZE) - 1) & !(i64::from(PAGE_SIZE) - 1);
        let [a, b, c, d, ..] = rounded.to_le_bytes();
        let rounded = i32::from_le_bytes([a, b, c, d]);
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
        match (id, rounded > 0) {
            (None, true) => self.shrink_application(amount, guest),
            (None, false) => self.grow_application(amount, guest),
            (Some(id), false) => self.shrink_area(id, amount, true, guest),
            (Some(id), true) => self.grow_area(id, amount, true, guest),
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
        self.free_for(bytes / PAGE_SIZE, None, guest);
        let grown = self
            .slot_size(slot)
            .and_then(|size| size.checked_add(bytes))
            .ok_or(Failure::none(AreaError::NotAllMoved))?;
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

    /// Shrinks the shrinkable areas, most recently made first and `except`
    /// spared, until `pages` are free or none can give more.
    pub(super) fn free_for<G: Guest + ?Sized>(
        &mut self,
        pages: u32,
        except: Option<Id>,
        guest: &mut G,
    ) {
        let mut index = 0;
        while self.free_pages() < pages {
            let Some(&id) = self.areas.shrinkable.get(index) else {
                return;
            };
            index += 1;
            if Some(id) == except {
                continue;
            }
            let size = self.areas.area(id).map_or(0, Area::size);
            let short = (pages - self.free_pages()).saturating_mul(PAGE_SIZE);
            let _ = self.shrink_area(id, short.min(size), true, guest);
        }
    }

    /// Takes up to `bytes` from the top of an area; `counted` when the Task
    /// Manager is to hear of it.
    pub(super) fn shrink_area<G: Guest + ?Sized>(
        &mut self,
        id: Id,
        bytes: u32,
        counted: bool,
        guest: &mut G,
    ) -> Result<u32, Failure> {
        let area = self
            .areas
            .area(id)
            .ok_or(Failure::none(AreaError::BadDynamicArea))?;
        let (number, size, handler) = (area.number, area.size(), area.handler);
        let amount = bytes.min(size);
        let short = amount < bytes;
        let allowed = if amount == 0 {
            0
        } else {
            self.areas.resizing = true;
            let allowed = match handler {
                Some(handler) => guest
                    .pre_shrink(self, handler, amount, size)
                    .map(|allowed| allowed.min(amount) & !(PAGE_SIZE - 1)),
                None => Ok(amount),
            };
            let allowed = match allowed {
                Ok(allowed) => allowed,
                Err(error) => {
                    self.areas.resizing = false;
                    return Err(Failure::none(AreaError::refused(error)));
                }
            };
            if allowed > 0 {
                self.drop_area_pages(id, allowed / PAGE_SIZE);
            }
            self.areas.resizing = false;
            if allowed > 0 {
                if let Some(handler) = handler {
                    let size = self.areas.area(id).map_or(0, Area::size);
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
                area: signed(number),
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
        let Some(area) = self.areas.area_mut(id) else {
            return;
        };
        let (base, access, doubly) = (area.base, area.access(), area.doubly());
        let Shape::Pages(list) = &mut area.shape else {
            return;
        };
        let old = u32::try_from(list.len()).unwrap_or(0);
        let keep = old.saturating_sub(pages);
        let gone = list.split_off(keep as usize);
        let first = |count: u32, index: u32| base - count * PAGE_SIZE + index * PAGE_SIZE;
        for (index, &frame) in (keep..).zip(&gone) {
            let _ = self.mmu.unmap(Table::Global, base + index * PAGE_SIZE);
            if doubly {
                let _ = self.mmu.unmap(Table::Global, first(old, index));
            }
            self.pages.free(frame);
        }
        if doubly {
            let kept = match self.areas.area(id).map(|area| &area.shape) {
                Some(Shape::Pages(list)) => list.clone(),
                _ => Vec::new(),
            };
            // Moving up, each page lands where one above it was, so the
            // highest moves first.
            for (index, &frame) in kept.iter().enumerate().rev() {
                let index = u32::try_from(index).unwrap_or(0);
                let _ = self.mmu.unmap(Table::Global, first(old, index));
                let _ = self
                    .mmu
                    .map(Table::Global, first(keep, index), frame, access);
            }
        }
    }

    /// Adds `bytes` to an area, wholly or not at all but for a chunked
    /// grow's refused chunk.
    pub(super) fn grow_area<G: Guest + ?Sized>(
        &mut self,
        id: Id,
        bytes: u32,
        counted: bool,
        guest: &mut G,
    ) -> Result<u32, Failure> {
        let area = self
            .areas
            .area(id)
            .ok_or(Failure::none(AreaError::BadDynamicArea))?;
        let (number, size, max, flags) = (area.number, area.size(), area.max, area.flags);
        if bytes > max.saturating_sub(size) {
            guest.service(
                self,
                Service::MemoryMoved {
                    bytes: 0,
                    area: signed(number),
                },
            );
            return Err(Failure::none(AreaError::NotAllMoved));
        }
        let pages = bytes / PAGE_SIZE;
        self.free_for(pages, Some(id), guest);
        if self.free_pages() < pages {
            let application = self
                .current_slot()
                .and_then(|slot| self.slot_size(slot))
                .unwrap_or(0)
                / PAGE_SIZE;
            let short = pages - self.free_pages();
            if application >= short {
                self.shrink_application(short * PAGE_SIZE, guest)
                    .map_err(|failure| Failure::none(failure.error))?;
            }
        }
        if self.free_pages() < pages {
            return Err(Failure::none(AreaError::NotAllMoved));
        }
        let chunked = flags.has(AreaFlags::NEEDS_SPECIFIC_PAGES)
            || flags.has(AreaFlags::NEEDS_DMA)
            || flags.has(AreaFlags::PMP);
        let moved = if chunked {
            self.grow_in_chunks(id, pages, guest)
        } else {
            self.grow_at_once(id, pages, guest)
        };
        let (Ok(done) | Err(Failure { moved: done, .. })) = moved;
        if counted && done > 0 {
            self.areas.note(number, signature::RESIZED);
        }
        guest.service(
            self,
            Service::MemoryMoved {
                bytes: signed(done),
                area: signed(number),
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
        self.areas.resizing = true;
        if let Some(handler) = handler {
            if let Err(error) = guest.pre_grow(self, handler, &mut [], bytes, size) {
                self.areas.resizing = false;
                return Err(Failure::none(AreaError::refused(error)));
            }
        }
        let added = self
            .take_pages(id, size / PAGE_SIZE, pages)
            .and_then(|frames| self.add_area_pages(id, &frames));
        self.areas.resizing = false;
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
        let Some(area) = self.areas.area_mut(id) else {
            self.forget_pages(frames);
            return Err(AreaError::BadDynamicArea);
        };
        let (base, access, doubly) = (area.base, area.access(), area.doubly());
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
        let kept = if doubly { list.clone() } else { Vec::new() };
        list.extend_from_slice(frames);
        let first = |count: u32, index: u32| base - count * PAGE_SIZE + index * PAGE_SIZE;
        // Moving down, the highest page lands where the lowest was, so each
        // moves after the one below it has.
        for (index, &frame) in (0u32..).zip(&kept) {
            let _ = self.mmu.unmap(Table::Global, first(old, index));
            let _ = self
                .mmu
                .map(Table::Global, first(new, index), frame, access);
        }
        for (index, &frame) in (old..).zip(frames) {
            let _ = self
                .mmu
                .map(Table::Global, base + index * PAGE_SIZE, frame, access);
            if doubly {
                let _ = self
                    .mmu
                    .map(Table::Global, first(new, index), frame, access);
            }
            let _ = self.mmu.release(frame);
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
        self.areas.resizing = true;
        while left > 0 {
            let chunk = left.min(CHUNK);
            if let Err(error) = self.grow_chunk(id, chunk, guest) {
                self.areas.resizing = false;
                return Err(Failure { error, moved });
            }
            moved += chunk * PAGE_SIZE;
            left -= chunk;
        }
        self.areas.resizing = false;
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
        let mut granted = Granted { reserved: false };
        if flags.has(AreaFlags::NEEDS_DMA) {
            self.pick_pages(block)?;
        } else if let Some(handler) = handler {
            granted = guest
                .pre_grow(self, handler, block, bytes, size)
                .map_err(AreaError::refused)?;
        }
        let named = block.first().is_some_and(|entry| entry.page.is_some());
        if !named {
            self.pick_pages(block)?;
        }
        if let Err(error) = self.claim_pages(id, size / PAGE_SIZE, block, granted, named, guest) {
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

    /// Fills a page block with the lowest free pages no reservation keeps.
    fn pick_pages(&self, block: &mut [PageEntry]) -> Result<(), AreaError> {
        let mut from = Frame::new(0);
        for entry in block.iter_mut() {
            let page = loop {
                let page = self.mmu.next_free(from).ok_or(AreaError::NotAllMoved)?;
                from = Frame::new(page.number() + 1);
                if !self.pages.get(page.number()).reserved {
                    break page;
                }
            };
            entry.page = Some(page.number());
            entry.physical = page.number() * PAGE_SIZE;
        }
        Ok(())
    }

    /// Takes the pages a block names into an area, as its pages from
    /// `first` up, replacing any another holder has with free ones carrying
    /// their contents. Pages a handler named become unavailable to others.
    fn claim_pages<G: Guest + ?Sized>(
        &mut self,
        id: Id,
        first: u32,
        block: &[PageEntry],
        granted: Granted,
        named: bool,
        guest: &mut G,
    ) -> Result<(), AreaError> {
        let mut wanted = Vec::new();
        wanted
            .try_reserve(block.len())
            .map_err(|_| AreaError::NotAllMoved)?;
        for entry in block {
            let page = entry.page.ok_or(AreaError::CantGetPhysicalMemory)?;
            let record = self.pages.get(page);
            let kernel = record.holder == Some(Holder::Kernel);
            let ours = matches!(record.holder, Some(Holder::Area { area, .. }) if area == id);
            if page >= self.ceiling.pages()
                || record.unavailable
                || kernel
                || ours
                || (record.reserved && !granted.reserved)
                || wanted.contains(&page)
            {
                return Err(AreaError::CantGetPhysicalMemory);
            }
            wanted.push(page);
        }
        if named {
            guest.service(self, Service::PagesUnsafe { pages: block });
            guest.service(self, Service::ClaimFiq);
        }
        let mut replaced = Vec::new();
        let mut taken = Vec::new();
        let _ = replaced.try_reserve(block.len());
        let _ = taken.try_reserve(block.len());
        let mut outcome = Ok(());
        for (index, &page) in (first..).zip(&wanted) {
            let frame = Frame::new(page);
            let step = if self.mmu.is_free(frame) {
                self.mmu
                    .allocate_at(frame, FrameKind::Memory)
                    .map_err(|_| AreaError::CantGetPhysicalMemory)
            } else {
                self.replace_page(frame).map(|new| {
                    replaced.push(PageEntry {
                        page: Some(new.number()),
                        logical: 0,
                        physical: new.number() * PAGE_SIZE,
                    });
                })
            };
            if let Err(error) = step {
                outcome = Err(error);
                break;
            }
            if let Ok(record) = self.pages.get_mut(page) {
                record.holder = Some(Holder::Area { area: id, index });
                record.unavailable = named;
            }
            taken.push(frame);
        }
        if named {
            guest.service(self, Service::ReleaseFiq);
            guest.service(
                self,
                Service::PagesSafe {
                    old: block,
                    new: &replaced,
                },
            );
        }
        match outcome {
            Ok(()) => self.add_area_pages(id, &taken),
            Err(error) => {
                self.forget_pages(&taken);
                Err(error)
            }
        }
    }

    /// Moves `frame`'s holder onto a free frame carrying its contents,
    /// leaving `frame` held for the caller; returns the frame in its place.
    pub(super) fn replace_page(&mut self, frame: Frame) -> Result<Frame, AreaError> {
        let record = self.pages.get(frame.number());
        let holder = record.holder.ok_or(AreaError::CantGetPhysicalMemory)?;
        let places = self.places_of(holder);
        let new = self
            .mmu
            .allocate(FrameKind::Memory)
            .map_err(|_| AreaError::CantGetPhysicalMemory)?;
        if let Some(from) = self.mmu.words(frame).copied() {
            if let Some(to) = self.mmu.words_mut(new) {
                *to = from;
            }
        }
        let _ = self.mmu.hold(frame);
        for place in places.iter().flatten() {
            if let Some((_, access)) = self.mmu.lookup(place.table, place.address) {
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
        if let Ok(slot) = self.pages.get_mut(frame.number()) {
            slot.holder = None;
            slot.unavailable = false;
        }
        Ok(new)
    }

    fn is_pool(&self, id: Id) -> bool {
        self.areas
            .area(id)
            .is_some_and(|area| matches!(area.shape, Shape::Pmp(_)))
    }

    /// Where a page `holder` holds is mapped.
    fn places_of(&self, holder: Holder) -> [Option<Place>; 2] {
        let global = |address: Option<u32>| {
            address.map(|address| Place {
                table: Table::Global,
                address,
            })
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
                    Shape::Pages(_) => area.addresses(index).map(global),
                    Shape::Sparse { .. } => [global(Some(area.base + index * PAGE_SIZE)), None],
                    Shape::Pmp(pool) => [
                        global(
                            pool.window_of(index)
                                .map(|window| area.base + window * PAGE_SIZE),
                        ),
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
