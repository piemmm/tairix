//! Sparse areas (`OS_DynamicArea` 9 and 10): pages mapped where their owner
//! claims them, each run of pages moved to or from the free pool as one.

use alloc::vec::Vec;

use tairix_riscos_arm::mmu::{Frame, Table, PAGE_SIZE};

use super::super::guest::{Guest, Service};
use super::super::{Holder, Memory};
use super::{signature, AreaError, Id, Shape};

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
        let area_base = self.areas.area(id).map_or(0, |area| area.base);
        if let Some(Shape::Sparse { high_water, .. }) =
            self.areas.area_mut(id).map(|area| &mut area.shape)
        {
            *high_water = (*high_water).max(area_base + end * PAGE_SIZE);
        }
        let mut outcome = Ok(());
        let mut index = first;
        while index < end {
            if self.sparse_page(id, index).is_some() {
                index += 1;
                continue;
            }
            let run = (index..end)
                .take_while(|&page| self.sparse_page(id, page).is_none())
                .count();
            let run = u32::try_from(run).unwrap_or(0);
            if let Err(error) = self.claim_run(id, index, run, guest) {
                outcome = Err(error);
                break;
            }
            let bytes = i32::try_from(run * PAGE_SIZE).unwrap_or(i32::MAX);
            guest.service(
                self,
                Service::MemoryMoved {
                    bytes,
                    area: i32::try_from(number).unwrap_or(-1),
                },
            );
            index += run;
        }
        self.finish_sparse(number, guest);
        outcome
    }

    fn sparse_page(&self, id: Id, index: u32) -> Option<Frame> {
        match &self.areas.area(id)?.shape {
            Shape::Sparse { pages, .. } => pages.get(&index).copied(),
            _ => None,
        }
    }

    /// Maps `run` free pages from page `index` of a sparse area, drawing on
    /// the shrinkable areas and application space when the free pool is
    /// short; all of the run or none.
    fn claim_run<G: Guest + ?Sized>(
        &mut self,
        id: Id,
        index: u32,
        run: u32,
        guest: &mut G,
    ) -> Result<(), AreaError> {
        self.free_for(run, Some(id), guest);
        if self.free_pages() < run {
            let short = run - self.free_pages();
            let _ = self.shrink_application(short * PAGE_SIZE, guest);
        }
        if self.free_pages() < run {
            return Err(AreaError::NotAllMoved);
        }
        let area = self.areas.area(id).ok_or(AreaError::BadDynamicArea)?;
        let (base, access) = (area.base, area.access());
        let mut taken = Vec::new();
        taken
            .try_reserve(run as usize)
            .map_err(|_| AreaError::NotAllMoved)?;
        for page in index..index + run {
            let Ok(frame) = self.take_page(Holder::Area {
                area: id,
                index: page,
            }) else {
                self.forget_pages(&taken);
                return Err(AreaError::NotAllMoved);
            };
            taken.push(frame);
        }
        for (page, &frame) in (index..).zip(&taken) {
            let _ = self
                .mmu
                .map(Table::Global, base + page * PAGE_SIZE, frame, access);
            let _ = self.mmu.release(frame);
            if let Some(Shape::Sparse { pages, .. }) =
                self.areas.area_mut(id).map(|area| &mut area.shape)
            {
                let _ = pages.try_insert(page, frame);
            }
        }
        Ok(())
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
            if self.sparse_page(id, index).is_none() {
                index += 1;
                continue;
            }
            let run = (index..end)
                .take_while(|&page| self.sparse_page(id, page).is_some())
                .count();
            let run = u32::try_from(run).unwrap_or(0);
            for page in index..index + run {
                let frame = match self.areas.area_mut(id).map(|area| &mut area.shape) {
                    Some(Shape::Sparse { pages, .. }) => pages.remove(&page),
                    _ => None,
                };
                if let Some(frame) = frame {
                    let _ = self.mmu.unmap(Table::Global, area_base + page * PAGE_SIZE);
                    self.pages.free(frame);
                }
            }
            let bytes = i32::try_from(run * PAGE_SIZE).unwrap_or(i32::MAX);
            guest.service(
                self,
                Service::MemoryMoved {
                    bytes: -bytes,
                    area: i32::try_from(number).unwrap_or(-1),
                },
            );
            index += run;
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
                area: i32::try_from(number).unwrap_or(-1),
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
        let held: Vec<(u32, Frame)> = pages
            .iter()
            .map(|(&index, &frame)| (index, frame))
            .collect();
        *pages = tairix_collections::OrderedMap::new();
        for (index, frame) in held {
            let _ = self.mmu.unmap(Table::Global, base + index * PAGE_SIZE);
            self.pages.free(frame);
        }
    }
}
