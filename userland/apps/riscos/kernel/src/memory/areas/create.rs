//! Making, removing and renumbering areas (`OS_DynamicArea` 0, 1 and 4).

use core::ops::Range;

use tairix_collections::OrderedMap;
use tairix_riscos_arm::mmu::{Table, PAGE_SIZE};

use super::super::guest::{Guest, Service};
use super::super::layout::HIGH_ZERO_PAGE;
use super::super::{Bits, Memory};
use super::{
    name_key, signature, AccessPrivilege, Area, AreaError, AreaFlags, Handler, Id, Shape, Title,
    FREE_POOL, QUICK, SYSTEM, TITLE,
};

/// The id a reservation carries when it is the kernel's own part of the
/// layout rather than an area's.
pub(super) const KERNEL: Id = Id::MAX;

/// What `OS_DynamicArea` 0 asks for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Create<'a> {
    /// The number wanted, or none to be given one.
    pub number: Option<u32>,
    /// The initial size.
    pub size: u32,
    /// The base wanted, or none to be given one.
    pub base: Option<u32>,
    /// The flags.
    pub flags: u32,
    /// The maximum, or none for as large as the clamps allow.
    pub max: Option<u32>,
    /// The handler's code, if it has one.
    pub handler: Option<u32>,
    /// The handler's workspace, or none for the area's base.
    pub workspace: Option<u32>,
    /// The title, or none for the number in hex.
    pub title: Option<&'a [u8]>,
    /// A pool's page list length.
    pub pool: u32,
    /// The addressing of the program making it.
    pub caller: Bits,
}

/// What `OS_DynamicArea` 0 made.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Created {
    /// Its number.
    pub number: u32,
    /// Its base, as RISC OS reports it.
    pub base: u32,
    /// Its maximum.
    pub max: u32,
}

/// The title RISC OS gives an area made with none: its number in hex.
fn hex_title(number: u32) -> Title {
    let mut title = Title::new();
    for shift in (0..8).rev() {
        let digit = b"0123456789ABCDEF"[((number >> (shift * 4)) & 0xF) as usize];
        let _ = title.try_push(digit);
    }
    title
}

/// A reservation's addresses as the MMU takes them.
fn addresses(range: &Range<u64>) -> Range<u32> {
    u32::try_from(range.start).unwrap_or(u32::MAX)..u32::try_from(range.end).unwrap_or(u32::MAX)
}

impl Memory {
    /// Reserves the kernel's own parts of the layout, so no area is placed
    /// over them.
    pub(in super::super) fn reserve_layout(&mut self) -> Result<(), AreaError> {
        let layout = self.layout;
        let stacks = layout.stacks.all();
        let span = |range: &Range<u32>| u64::from(range.start)..u64::from(range.end);
        let low = stacks.iter().map(|stack| stack.start).min().unwrap_or(0);
        let high = stacks.iter().map(|stack| stack.end).max().unwrap_or(0);
        let parts = [
            span(&layout.rma),
            span(&layout.system_heap),
            span(&(low..high)),
            span(&layout.host),
            u64::from(HIGH_ZERO_PAGE)..1 << 32,
        ];
        for part in parts {
            self.areas
                .reserved
                .insert(part, KERNEL)
                .map_err(|_| AreaError::CantAllocateArea)?;
        }
        Ok(())
    }

    /// The number a new area is given: the quick numbers first, then the
    /// next unused past the last one probed for.
    fn next_number(&mut self) -> Result<u32, AreaError> {
        if let Some(number) = self.areas.quick.pop() {
            return Ok(number);
        }
        let mut number = self.areas.treacle.saturating_add(1).max(QUICK.end);
        while self.areas.numbers.get(&number).is_some() {
            number = number.checked_add(1).ok_or(AreaError::CantAllocateArea)?;
        }
        Ok(number)
    }

    /// The most an area of `request` may reach: the clamps, then the
    /// guest's RAM, rounded up to a page; at least its initial size.
    fn area_max(&self, request: &Create<'_>) -> u32 {
        let clamps = self.areas.clamps;
        let asked = request.max.unwrap_or(u32::MAX);
        let max = if AreaFlags(request.flags).has(AreaFlags::SPARSE) {
            asked.min(clamps.sparse).max(request.size)
        } else {
            let clamp = if request.max.is_none() {
                clamps.unbounded
            } else {
                clamps.bounded
            };
            let ram = u32::try_from(self.ceiling.bytes()).unwrap_or(u32::MAX);
            asked.min(clamp).max(request.size).min(ram)
        };
        // Past the last page's start it wraps to nothing, as RISC OS's does.
        max.checked_next_multiple_of(PAGE_SIZE).unwrap_or(0)
    }

    /// The windows an area of a `caller`'s may be placed in: upward from the
    /// application-space limit, and for a 26-bit caller the 26-bit window
    /// above every slot.
    fn placements(&self, caller: Bits) -> [Option<Range<u32>>; 2] {
        let areas = &self.layout.areas;
        let window = match (caller, &self.layout.window) {
            (Bits::Bit26, Some(window)) => {
                Some(window.start.max(self.slots.highest_top())..window.end)
            }
            _ => None,
        };
        [
            Some(areas.start.max(self.application_limit())..areas.end),
            window,
        ]
    }

    /// Where an area reserving `span` bytes goes when its caller names no
    /// base: a 26-bit caller's into the window from its top, any other's up
    /// from the application-space limit. Returns the reservation's start.
    fn place(&self, span: u64, caller: Bits) -> Result<u64, AreaError> {
        let [areas, window] = self.placements(caller);
        let span_of = |range: Range<u32>| u64::from(range.start)..u64::from(range.end);
        let found = match window {
            Some(window) => self.areas.reserved.last_free(span_of(window), span),
            None => areas.and_then(|areas| self.areas.reserved.first_free(span_of(areas), span)),
        };
        found.ok_or(AreaError::CantAllocateArea)
    }

    /// Refuses a reservation its caller named that leaves the windows it
    /// could be placed in, or meets another.
    fn may_reserve(&self, range: &Range<u64>, caller: Bits) -> Result<(), AreaError> {
        let inside = self.placements(caller).into_iter().flatten().any(|window| {
            u64::from(window.start) <= range.start && range.end <= u64::from(window.end)
        });
        if !inside
            || self
                .areas
                .reserved
                .overlapping(range.clone())
                .next()
                .is_some()
        {
            return Err(AreaError::OverlappingAreas);
        }
        Ok(())
    }

    /// Refuses a number a caller may not make an area with.
    fn check_number(&self, number: u32) -> Result<(), AreaError> {
        if number <= SYSTEM || QUICK.contains(&number) || self.areas.numbers.get(&number).is_some()
        {
            return Err(AreaError::AreaAlreadyExists);
        }
        if i32::try_from(number).is_err() {
            return Err(AreaError::BadDynamicAreaOptions);
        }
        Ok(())
    }

    /// Refuses flags RISC OS 5 refuses, and an access privilege it lacks.
    fn check_flags(request: &Create<'_>) -> Result<(), AreaError> {
        let flags = AreaFlags(request.flags);
        let privilege =
            AccessPrivilege::of(flags.privilege()).ok_or(AreaError::BadDynamicAreaOptions)?;
        let exclusive = AreaFlags::DOUBLY_MAPPED
            | AreaFlags::NEEDS_SPECIFIC_PAGES
            | AreaFlags::SPARSE
            | AreaFlags::NEEDS_DMA;
        let bad_pool = flags.has(AreaFlags::PMP)
            && (flags.0 & exclusive != AreaFlags::NEEDS_SPECIFIC_PAGES
                || request.size != 0
                || request.handler.is_none());
        // RISC OS 5 maps a cacheable area twice only where neither copy may
        // be run.
        let bad_double = flags.has(AreaFlags::DOUBLY_MAPPED)
            && !flags.has(AreaFlags::NOT_CACHEABLE)
            && privilege.execute;
        let bad_abort = flags.has(AreaFlags::ABORTABLE) && request.handler.is_none();
        if bad_pool || bad_double || bad_abort {
            return Err(AreaError::BadDynamicAreaOptions);
        }
        Ok(())
    }

    /// The reservation of an area of `max` bytes: the caller's base, or one
    /// placed for it; none for an area of no logical size.
    fn reservation(&self, request: &Create<'_>, max: u32) -> Result<Range<u64>, AreaError> {
        if max == 0 {
            return Ok(0..0);
        }
        let doubly = AreaFlags(request.flags).has(AreaFlags::DOUBLY_MAPPED);
        let span = u64::from(max) * if doubly { 2 } else { 1 };
        let start = match request.base {
            Some(base) => {
                if !base.is_multiple_of(PAGE_SIZE) {
                    return Err(AreaError::AreaNotOnPageBoundary);
                }
                let start = if doubly {
                    base.checked_sub(max).ok_or(AreaError::OverlappingAreas)?
                } else {
                    base
                };
                let range = u64::from(start)..u64::from(start) + span;
                if range.end > 1 << 32 {
                    return Err(AreaError::OverlappingAreas);
                }
                self.may_reserve(&range, request.caller)?;
                u64::from(start)
            }
            None => self.place(span, request.caller)?,
        };
        Ok(start..start + span)
    }

    /// `OS_DynamicArea` 0: makes an area and grows it to its initial size,
    /// its handler called as for any grow.
    pub fn create_area<G: Guest + ?Sized>(
        &mut self,
        request: &Create<'_>,
        guest: &mut G,
    ) -> Result<Created, AreaError> {
        if let Some(number) = request.number {
            self.check_number(number)?;
        }
        Self::check_flags(request)?;
        // Its initial grow is a resize, which no other may interleave with.
        if request.size != 0 && self.areas.resizing {
            return Err(AreaError::NotAllMoved);
        }
        let max = self.area_max(request);
        let reserved = self.reservation(request, max)?;
        let mut flags = AreaFlags(request.flags);
        let doubly = flags.has(AreaFlags::DOUBLY_MAPPED);
        let base = u32::try_from(if doubly {
            reserved.start + u64::from(max)
        } else {
            reserved.start
        })
        .map_err(|_| AreaError::OverlappingAreas)?;
        let sparse = flags.has(AreaFlags::SPARSE);
        let mut handler = request.handler.map(|code| Handler {
            code,
            workspace: request.workspace.unwrap_or(base),
        });
        if sparse {
            flags.0 &= !(AreaFlags::DOUBLY_MAPPED
                | AreaFlags::NEEDS_SPECIFIC_PAGES
                | AreaFlags::SHRINKABLE);
            flags.0 |= AreaFlags::NOT_USER_DRAGGABLE;
            if !flags.has(AreaFlags::ABORTABLE) {
                handler = None;
            }
        }
        let shape = if flags.has(AreaFlags::PMP) {
            Shape::Pmp(
                super::pmp::Pool::new(request.pool.min(self.ceiling.pages()), base)
                    .map_err(|_| AreaError::CantAllocateArea)?,
            )
        } else {
            Shape::Pages(alloc::vec::Vec::new())
        };
        if !reserved.is_empty() {
            self.mmu
                .reserve(Table::Global, addresses(&reserved))
                .map_err(|_| AreaError::CantAllocateLevel2)?;
        }
        let id = match self.link(request, max, base, flags, handler, shape, &reserved) {
            Ok(id) => id,
            Err(error) => {
                if !reserved.is_empty() {
                    self.mmu.unreserve(Table::Global, addresses(&reserved));
                }
                return Err(error);
            }
        };
        let number = self.areas.area(id).map_or(0, |area| area.number);
        if request.size != 0 {
            if let Err(failure) =
                self.grow_area(id, request.size.next_multiple_of(PAGE_SIZE), false, guest)
            {
                // The handler that refused is not asked again.
                self.discard_pages(id);
                self.unlink(id);
                return Err(failure.error);
            }
        }
        if sparse && self.make_sparse(id).is_err() {
            self.discard_pages(id);
            self.unlink(id);
            return Err(AreaError::CantAllocateArea);
        }
        if flags.has(AreaFlags::SHRINKABLE)
            && !sparse
            && self.areas.shrinkable.try_reserve(1).is_ok()
        {
            self.areas.shrinkable.insert(0, id);
        }
        guest.service(self, Service::DynamicAreaCreate { area: number });
        self.areas.note(number, signature::CREATED);
        Ok(Created { number, base, max })
    }

    /// Enters a new area in every table, numbered and titled.
    #[expect(
        clippy::too_many_arguments,
        reason = "one per field an area is made with"
    )]
    fn link(
        &mut self,
        request: &Create<'_>,
        max: u32,
        base: u32,
        flags: AreaFlags,
        handler: Option<Handler>,
        shape: Shape,
        reserved: &Range<u64>,
    ) -> Result<Id, AreaError> {
        let areas = &mut self.areas;
        areas
            .entries
            .try_reserve(1)
            .map_err(|_| AreaError::CantAllocateArea)?;
        areas
            .free_ids
            .try_reserve(1)
            .map_err(|_| AreaError::CantAllocateArea)?;
        areas
            .quick
            .try_reserve(1)
            .map_err(|_| AreaError::CantAllocateArea)?;
        let number = match request.number {
            Some(number) => number,
            None => self.next_number()?,
        };
        let areas = &mut self.areas;
        let title = match request.title {
            Some(title) => {
                let mut kept = Title::new();
                for &byte in title.iter().take(TITLE) {
                    let _ = kept.try_push(byte);
                }
                kept
            }
            None => hex_title(number),
        };
        areas.sequence += 1;
        let sequence = areas.sequence;
        let key = name_key(title.as_slice(), sequence);
        let id = match areas.free_ids.pop() {
            Some(id) => id,
            None => Id::try_from(areas.entries.len()).map_err(|_| AreaError::CantAllocateArea)?,
        };
        let entered = areas
            .numbers
            .try_insert(number, id)
            .map_err(|_| AreaError::CantAllocateArea)
            .and_then(|_| {
                areas
                    .names
                    .try_insert(key, id)
                    .map_err(|_| AreaError::CantAllocateArea)
            })
            .and_then(|_| {
                if reserved.is_empty() {
                    Ok(())
                } else {
                    areas
                        .reserved
                        .insert(reserved.clone(), id)
                        .map_err(|_| AreaError::OverlappingAreas)
                }
            });
        if let Err(error) = entered {
            areas.numbers.remove(&number);
            areas.names.remove(&key);
            areas.free_ids.push(id);
            if QUICK.contains(&number) {
                areas.quick.push(number);
            }
            return Err(error);
        }
        let area = Area {
            number,
            title,
            base,
            max,
            flags,
            handler,
            sequence,
            shape,
        };
        match areas.entries.get_mut(id as usize) {
            Some(slot) => *slot = Some(area),
            None => areas.entries.push(Some(area)),
        }
        if number >= QUICK.end {
            areas.treacle = areas.treacle.saturating_add(1);
        }
        Ok(id)
    }

    /// Takes an area out of every table, its number and its tables free.
    fn unlink(&mut self, id: Id) {
        let Some(area) = self
            .areas
            .entries
            .get_mut(id as usize)
            .and_then(Option::take)
        else {
            return;
        };
        let reserved = area.reserved();
        if !reserved.is_empty() {
            self.mmu.unreserve(Table::Global, addresses(&reserved));
            if self
                .areas
                .reserved
                .get(reserved.start)
                .is_some_and(|(_, &owner)| owner == id)
            {
                self.areas.reserved.remove(reserved.start);
            }
        }
        self.areas.numbers.remove(&area.number);
        self.areas
            .names
            .remove(&name_key(area.title.as_slice(), area.sequence));
        self.areas.shrinkable.retain(|&other| other != id);
        if self.areas.free_ids.try_reserve(1).is_ok() {
            self.areas.free_ids.push(id);
        }
        if QUICK.contains(&area.number) && self.areas.quick.try_reserve(1).is_ok() {
            self.areas.quick.push(area.number);
        }
    }

    /// Unmaps and frees every page of a contiguous or doubly mapped area,
    /// asking no handler.
    fn discard_pages(&mut self, id: Id) {
        let Some(area) = self.areas.area_mut(id) else {
            return;
        };
        let (base, doubly) = (area.base, area.doubly());
        let Shape::Pages(list) = &mut area.shape else {
            return;
        };
        let pages = core::mem::take(list);
        let count = u32::try_from(pages.len()).unwrap_or(0);
        for (index, &frame) in (0u32..).zip(&pages) {
            let _ = self.mmu.unmap(Table::Global, base + index * PAGE_SIZE);
            if doubly {
                let _ = self
                    .mmu
                    .unmap(Table::Global, base - count * PAGE_SIZE + index * PAGE_SIZE);
            }
            self.pages.free(frame);
        }
    }

    /// Turns a newly grown area's pages into a sparse area's, mapped where
    /// they are; refused, changing nothing, if the host has no memory for
    /// its map.
    fn make_sparse(&mut self, id: Id) -> Result<(), AreaError> {
        let Some(area) = self.areas.area_mut(id) else {
            return Err(AreaError::BadDynamicArea);
        };
        let Shape::Pages(list) = &area.shape else {
            return Err(AreaError::BadDynamicArea);
        };
        let mut pages = OrderedMap::new();
        for (index, &frame) in (0u32..).zip(list.iter()) {
            pages
                .try_insert(index, frame)
                .map_err(|_| AreaError::CantAllocateArea)?;
        }
        let high_water = area.base + area.size();
        area.shape = Shape::Sparse { pages, high_water };
        Ok(())
    }

    /// `OS_DynamicArea` 1: removes an area, giving its memory back.
    pub fn remove_area<G: Guest + ?Sized>(
        &mut self,
        number: u32,
        guest: &mut G,
    ) -> Result<(), AreaError> {
        if number <= SYSTEM {
            return Err(AreaError::BadDynamicArea);
        }
        if self.areas.resizing {
            return Err(AreaError::NotAllMoved);
        }
        let id = self.areas.id(number)?;
        let area = self.areas.area(id).ok_or(AreaError::BadDynamicArea)?;
        let size = area.size();
        match &area.shape {
            Shape::Sparse { .. } => self.release_sparse(id),
            Shape::Pmp(_) => self.empty_pool(id)?,
            Shape::Pages(_) => {
                if let Err(failure) = self.shrink_area(id, size, false, guest) {
                    let restore = size.saturating_sub(self.areas.area(id).map_or(0, Area::size));
                    let _ = self.grow_area(id, restore, false, guest);
                    return Err(failure.error);
                }
                // Its PostShrink handler may have grown it again: it stays,
                // rather than its pages going with it.
                if self.areas.area(id).is_some_and(|area| area.size() != 0) {
                    return Err(AreaError::NotAllMoved);
                }
            }
        }
        self.unlink(id);
        guest.service(self, Service::DynamicAreaRemove { area: number });
        self.areas.note(number, signature::REMOVED);
        Ok(())
    }

    /// `OS_DynamicArea` 4: renumbers an area.
    pub fn renumber_area<G: Guest + ?Sized>(
        &mut self,
        old: u32,
        new: u32,
        guest: &mut G,
    ) -> Result<(), AreaError> {
        let refused = |number: u32| {
            number <= SYSTEM || QUICK.contains(&number) || i32::try_from(number).is_err()
        };
        if refused(old) || refused(new) || old == FREE_POOL {
            return Err(AreaError::IllegalRenumber);
        }
        let id = self.areas.id(old)?;
        if self.areas.numbers.get(&new).is_some() {
            return Err(AreaError::AreaAlreadyExists);
        }
        self.areas
            .numbers
            .try_insert(new, id)
            .map_err(|_| AreaError::CantAllocateArea)?;
        self.areas.numbers.remove(&old);
        if let Some(area) = self.areas.area_mut(id) {
            area.number = new;
        }
        guest.service(self, Service::DynamicAreaRenumber { old, new });
        self.areas.note(old, signature::RENUMBERED);
        Ok(())
    }
}
