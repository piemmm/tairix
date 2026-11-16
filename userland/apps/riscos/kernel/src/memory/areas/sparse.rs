//! Sparse areas (`OS_DynamicArea` 9 and 10): pages mapped where their owner
//! claims them, each run of pages moved to or from the free pool as one.

use alloc::vec::Vec;

use tairix_collections::OrderedMap;
use tairix_riscos_arm::mmu::{Frame, Table, PAGE_SIZE};

use super::super::guest::{Guest, Service};
use super::super::{Holder, Memory};
use super::change::named;
use super::{signature, AreaError, Id, Key, Shape};

/// The first page from `from` below `end` the map holds, or `end`.
fn first_held(pages: &OrderedMap<u32, Frame>, from: u32, end: u32) -> u32 {
    pages.range(from..end).next().map_or(end, |(&page, _)| page)
}

/// The first page from `from` below `end` the map does not hold, or `end`.
fn first_free(pages: &OrderedMap<u32, Frame>, from: u32, end: u32) -> u32 {
    let mut at = from;
    for (&page, _) in pages.range(from..end) {
        if page != at {
            break;
        }
        at += 1;
    }
    at
}

impl Memory {
    /// A sparse area's id and the page range `[base, base + size)` names
    /// within it: widened to whole pages for a claim, narrowed for a release.
    fn sparse_range(
        &self,
        number: u32,
        base: u32,
        size: u32,
        claim: bool,
    ) -> Result<(Id, u32, u32), AreaError> {
        if self.areas.resizing {
            return Err(AreaError::NotAllMoved);
        }
        let id = self
            .areas
            .id(number)
            .map_err(|_| AreaError::InvalidSparse)?;
        let area = self.areas.area(id).ok_or(AreaError::InvalidSparse)?;
        if !matches!(area.shape, Shape::Sparse { .. }) {
            return Err(AreaError::InvalidSparse);
        }
        let end = u64::from(base) + u64::from(size);
        let page = u64::from(PAGE_SIZE);
        let (start, end) = if claim {
            (u64::from(base) / page * page, end.div_ceil(page) * page)
        } else {
            (u64::from(base).div_ceil(page) * page, end / page * page)
        };
        let limit = u64::from(area.base) + u64::from(area.max);
        if start < u64::from(area.base) || end > limit {
            return Err(AreaError::InvalidSparse);
        }
        let first = u32::try_from((start - u64::from(area.base)) / page).unwrap_or(0);
        let last = u32::try_from(end.saturating_sub(u64::from(area.base)) / page).unwrap_or(0);
        Ok((id, first, last.max(first)))
    }

    /// The map of a sparse area's pages.
    fn sparse_pages(&self, id: Id) -> Option<&OrderedMap<u32, Frame>> {
        match &self.areas.area(id)?.shape {
            Shape::Sparse { pages, .. } => Some(pages),
            _ => None,
        }
    }

    /// `OS_DynamicArea` 9: maps every unmapped page of `[base, base + size)`
    /// in a sparse area, run by run, stopping at the first that fails.
    pub fn sparse_claim<G: Guest + ?Sized>(
        &mut self,
        number: u32,
        base: u32,
        size: u32,
        guest: &mut G,
    ) -> Result<(), AreaError> {
        let (id, first, end) = self.sparse_range(number, base, size, true)?;
        let key = self.areas.key(id).ok_or(AreaError::InvalidSparse)?;
        let area_base = self.areas.area(id).map_or(0, |area| area.base);
        if let Some(Shape::Sparse { high_water, .. }) =
            self.areas.area_mut(id).map(|area| &mut area.shape)
        {
            *high_water = (*high_water).max(area_base + end * PAGE_SIZE);
        }
        let mut outcome = Ok(());
        let mut index = first;
        while index < end {
            let Some(pages) = self.sparse_pages(id) else {
                outcome = Err(AreaError::InvalidSparse);
                break;
            };
            let start = first_free(pages, index, end);
            if start == end {
                break;
            }
            let run = first_held(pages, start, end) - start;
            let claimed = match self.claim_run(key, start, run, guest) {
                Ok(claimed) => claimed,
                Err(error) => {
                    outcome = Err(error);
                    break;
                }
            };
            if claimed > 0 {
                guest.service(
                    self,
                    Service::MemoryMoved {
                        bytes: i32::try_from(claimed * PAGE_SIZE).unwrap_or(i32::MAX),
                        area: named(number),
                    },
                );
            }
            index = start + claimed.max(1);
        }
        self.finish_sparse(number, guest);
        outcome
    }

    /// Maps up to `run` free pages from page `index` of a sparse area,
    /// drawing on the shrinkable areas and application space when the free
    /// pool is short; the pages claimed, all of the run still unmapped once
    /// room is made, or none.
    fn claim_run<G: Guest + ?Sized>(
        &mut self,
        key: Key,
        index: u32,
        run: u32,
        guest: &mut G,
    ) -> Result<u32, AreaError> {
        if self.free_pages() < run && self.make_room(run, Some(key), guest).is_err() {
            return Err(AreaError::NotAllMoved);
        }
        // Guest code ran making room: the area may have gone, or claimed
        // some of these pages itself.
        let area = self.areas.area_at(key).ok_or(AreaError::InvalidSparse)?;
        let (base, access) = (area.base, area.access);
        let Shape::Sparse { pages, .. } = &area.shape else {
            return Err(AreaError::InvalidSparse);
        };
        let run = first_held(pages, index, index + run) - index;
        if self.free_pages() < run {
            return Err(AreaError::NotAllMoved);
        }
        let mut taken = Vec::new();
        taken
            .try_reserve(run as usize)
            .map_err(|_| AreaError::NotAllMoved)?;
        if let Some(Shape::Sparse { pages, .. }) =
            self.areas.area_at_mut(key).map(|area| &mut area.shape)
        {
            pages
                .try_reserve(run as usize)
                .map_err(|_| AreaError::NotAllMoved)?;
        }
        for page in index..index + run {
            let Ok(frame) = self.take_page(Holder::Area {
                area: key.id,
                index: page,
            }) else {
                self.forget_pages(&taken);
                return Err(AreaError::NotAllMoved);
            };
            taken.push(frame);
        }
        for (page, &frame) in (index..).zip(&taken) {
            if self
                .mmu
                .map(Table::Global, base + page * PAGE_SIZE, frame, access)
                .is_err()
            {
                for undone in index..page {
                    let _ = self.mmu.unmap(Table::Global, base + undone * PAGE_SIZE);
                }
                self.forget_pages(&taken);
                return Err(AreaError::NotAllMoved);
            }
        }
        if let Some(Shape::Sparse { pages, .. }) =
            self.areas.area_at_mut(key).map(|area| &mut area.shape)
        {
            for (page, &frame) in (index..).zip(&taken) {
                // Room was reserved for every entry above.
                let _ = pages.try_insert(page, frame);
            }
        }
        for &frame in &taken {
            let _ = self.mmu.release(frame);
        }
        Ok(run)
    }

    /// `OS_DynamicArea` 10: frees every mapped page of `[base, base + size)`
    /// in a sparse area, up to its high-water mark.
    pub fn sparse_release<G: Guest + ?Sized>(
        &mut self,
        number: u32,
        base: u32,
        size: u32,
        guest: &mut G,
    ) -> Result<(), AreaError> {
        let (id, first, end) = self.sparse_range(number, base, size, false)?;
        let area = self.areas.area(id).ok_or(AreaError::InvalidSparse)?;
        let area_base = area.base;
        let water = match &area.shape {
            Shape::Sparse { high_water, .. } => (high_water - area_base) / PAGE_SIZE,
            _ => 0,
        };
        let end = end.min(water);
        let mut index = first;
        while index < end {
            let Some(pages) = self.sparse_pages(id) else {
                break;
            };
            let start = first_held(pages, index, end);
            if start == end {
                break;
            }
            let stop = first_free(pages, start, end);
            let Self {
                areas,
                mmu,
                pages: records,
                ..
            } = self;
            if let Some(Shape::Sparse { pages, .. }) =
                areas.area_mut(id).map(|area| &mut area.shape)
            {
                for page in start..stop {
                    if let Some(frame) = pages.remove(&page) {
                        let _ = mmu.unmap(Table::Global, area_base + page * PAGE_SIZE);
                        records.free(frame);
                    }
                }
            }
            let bytes = i32::try_from((stop - start) * PAGE_SIZE).unwrap_or(i32::MAX);
            guest.service(
                self,
                Service::MemoryMoved {
                    bytes: -bytes,
                    area: named(number),
                },
            );
            index = stop;
        }
        self.finish_sparse(number, guest);
        Ok(())
    }

    /// Ends a sparse claim or release as RISC OS does: one more service
    /// with nothing moved, and the change noted.
    fn finish_sparse<G: Guest + ?Sized>(&mut self, number: u32, guest: &mut G) {
        guest.service(
            self,
            Service::MemoryMoved {
                bytes: 0,
                area: named(number),
            },
        );
        self.areas.note(number, signature::RESIZED);
    }

    /// Frees every page of a sparse area, as removing it does.
    pub(super) fn release_sparse(&mut self, id: Id) {
        let Some(area) = self.areas.area_mut(id) else {
            return;
        };
        let base = area.base;
        let Shape::Sparse { pages, .. } = &mut area.shape else {
            return;
        };
        let held = core::mem::take(pages);
        for (&index, &frame) in &held {
            let _ = self.mmu.unmap(Table::Global, base + index * PAGE_SIZE);
            self.pages.free(frame);
        }
    }
}
