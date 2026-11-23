//! A virtio device's shared-memory region (virtio 1.2 §2.10), held a part at
//! a time.
//!
//! A region may be larger than the driver's device window, so the driver holds
//! the part it needs, maps it while it uses it, and hands it back. A part's
//! pages are free for another only once nothing maps them: a mapped part is
//! never released.

use tairix_abi::driver::{MemoryMapper, MemoryWindow, RegionCaching};
use tairix_abi::hwtree::HwResource;
use tairix_abi::{DriverError, PAGE_SIZE};
use tairix_collections::RangeMap;

use crate::VirtioError;

/// One of a device's shared-memory regions and the parts held of it.
pub struct SharedMemory {
    region: HwResource,
    id: u8,
    pages: u64,
    /// The page runs held, the gaps between them the region's free space.
    held: RangeMap<u64, ()>,
}

/// A run of whole pages of a [`SharedMemory`] region, with its mapping into
/// the driver while it has one.
#[derive(Debug)]
pub struct Part {
    /// Where its region lies, which tells it from a run of another region.
    region: u64,
    first: u64,
    pages: u64,
    window: Option<(MemoryWindow, RegionCaching)>,
}

impl Part {
    /// Where in its region the part begins.
    #[must_use]
    pub fn offset(&self) -> u64 {
        self.first * PAGE_SIZE as u64
    }

    /// Bytes the part holds: whole pages.
    #[must_use]
    pub fn size(&self) -> u64 {
        self.pages * PAGE_SIZE as u64
    }

    /// The part's window while it is mapped.
    #[must_use]
    pub fn window(&self) -> Option<&MemoryWindow> {
        self.window.as_ref().map(|(window, _)| window)
    }
}

impl SharedMemory {
    /// The parts of `region`, a granted
    /// [`MemoryRegion`](tairix_abi::hwtree::HwResourceKind::MemoryRegion).
    ///
    /// # Errors
    ///
    /// [`VirtioError::DeviceFault`] for a resource that is no memory region,
    /// or one not a whole number of pages, which the kernel never grants.
    pub fn new(region: HwResource) -> Result<Self, VirtioError> {
        let id = region.memory_region_id().ok_or(VirtioError::DeviceFault)?;
        let page = PAGE_SIZE as u64;
        if !region.length().is_multiple_of(page) {
            return Err(VirtioError::DeviceFault);
        }
        Ok(Self {
            region,
            id,
            pages: region.length() / page,
            held: RangeMap::new(),
        })
    }

    /// The region's id among its device's, which says what the device uses it
    /// for.
    #[must_use]
    pub fn id(&self) -> u8 {
        self.id
    }

    /// Bytes the region holds.
    #[must_use]
    pub fn size(&self) -> u64 {
        self.region.length()
    }

    /// Hold the lowest run of free pages that holds `len` bytes.
    ///
    /// # Errors
    ///
    /// * [`VirtioError::Host`] — [`DriverError::LengthOutOfRange`] for no
    ///   bytes, [`DriverError::NoSpace`] where no free run holds them.
    /// * [`VirtioError::OutOfMemory`] — the run's record cannot be had.
    pub fn reserve(&mut self, len: u64) -> Result<Part, VirtioError> {
        let pages = len.div_ceil(PAGE_SIZE as u64);
        if pages == 0 {
            return Err(VirtioError::Host(DriverError::LengthOutOfRange));
        }
        let run = self
            .held
            .place(0..self.pages, pages, ())
            .map_err(|_| VirtioError::OutOfMemory)?
            .ok_or(VirtioError::Host(DriverError::NoSpace))?;
        Ok(Part {
            region: self.region.base(),
            first: run.start,
            pages,
            window: None,
        })
    }

    /// Map `part` into the driver through `mapper` with the `caching` its
    /// device states, returning its window; a part already mapped so answers
    /// the window it has.
    ///
    /// # Errors
    ///
    /// [`VirtioError::Host`] — [`DriverError::NotFound`] for a part this
    /// region does not hold, [`DriverError::Busy`] for one mapped with
    /// another caching, else the mapper's refusal.
    pub fn map<'p>(
        &self,
        part: &'p mut Part,
        mapper: &dyn MemoryMapper,
        caching: RegionCaching,
    ) -> Result<&'p MemoryWindow, VirtioError> {
        self.check(part)?;
        let window = match part.window.take() {
            Some((window, held)) if held == caching => window,
            Some(other) => {
                part.window = Some(other);
                return Err(VirtioError::Host(DriverError::Busy));
            }
            None => {
                let len = usize::try_from(part.size())
                    .map_err(|_| VirtioError::Host(DriverError::LengthOutOfRange))?;
                mapper
                    .map_memory(&self.region, part.offset(), len, caching)
                    .map_err(VirtioError::Host)?
            }
        };
        let (window, _) = part.window.insert((window, caching));
        Ok(window)
    }

    /// Unmap `part` from the driver; a part not mapped is left as it is.
    ///
    /// # Errors
    ///
    /// [`VirtioError::Host`] — [`DriverError::NotFound`] for a part this
    /// region does not hold, else the mapper's refusal, which leaves the part
    /// mapped.
    pub fn unmap(&self, part: &mut Part, mapper: &dyn MemoryMapper) -> Result<(), VirtioError> {
        self.check(part)?;
        let Some((window, caching)) = part.window.take() else {
            return Ok(());
        };
        mapper.unmap_memory(window).map_err(|(window, why)| {
            part.window = Some((window, caching));
            VirtioError::Host(why)
        })
    }

    /// Give `part` back for another to hold.
    ///
    /// # Errors
    ///
    /// The part itself, refused, while it is still mapped or where this region
    /// does not hold it.
    pub fn release(&mut self, part: Part) -> Result<(), Part> {
        if part.window.is_some() || self.check(&part).is_err() {
            return Err(part);
        }
        self.held.remove(part.first);
        Ok(())
    }

    /// Whether `part` is the run this region holds at its first page.
    fn check(&self, part: &Part) -> Result<(), VirtioError> {
        match self.held.get(part.first) {
            Some((run, ()))
                if part.region == self.region.base() && run.end - run.start == part.pages =>
            {
                Ok(())
            }
            _ => Err(VirtioError::Host(DriverError::NotFound)),
        }
    }
}

#[cfg(test)]
#[path = "shared_memory_tests.rs"]
mod tests;
