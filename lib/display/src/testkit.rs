//! Buffers in ordinary heap memory, for a harness that has no shared memory
//! to hand a display engine: each region it creates stays fillable by the
//! harness, as a presenter fills a granted one through its own mapping.

use alloc::rc::{Rc, Weak};
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};

use tairix_abi::driver::display::{DamageRect, Display};
use tairix_abi::DriverError;

use crate::memory::{BufferMemory, DeviceBuffer, SharedBuffer};

/// The regions a [`HeapMemory`] created and its engine still holds, by id.
type Regions = Rc<RefCell<Vec<(u64, Weak<RefCell<Vec<u8>>>)>>>;

/// Heap regions an engine allocates its presenter's buffers in.
#[derive(Clone, Default)]
pub struct HeapMemory {
    regions: Regions,
    next: Rc<Cell<u64>>,
    refuse: bool,
}

impl HeapMemory {
    /// A memory that refuses every region, as a machine with none to give
    /// does.
    #[must_use]
    pub fn refusing() -> Self {
        Self {
            refuse: true,
            ..Self::default()
        }
    }

    /// Write `bytes` at the start of region `id`, as its presenter draws.
    ///
    /// # Errors
    ///
    /// [`DriverError::NotFound`] for a region this memory did not create, and
    /// [`DriverError::BufferTooSmall`] for more bytes than it holds.
    pub fn fill(&self, id: u64, bytes: &[u8]) -> Result<(), DriverError> {
        let region = self
            .regions
            .borrow()
            .iter()
            .find(|(held, _)| *held == id)
            .and_then(|(_, region)| region.upgrade())
            .ok_or(DriverError::NotFound)?;
        let mut held = region.borrow_mut();
        held.get_mut(..bytes.len())
            .ok_or(DriverError::BufferTooSmall)?
            .copy_from_slice(bytes);
        Ok(())
    }

    /// Show `frame`, laid out as `display`'s output, through one buffer of
    /// that size: allocated, drawn, presented whole and given back.
    ///
    /// # Errors
    ///
    /// The engine's refusal at any step, or [`DriverError::Stale`] where it
    /// has no output.
    pub fn present_whole(
        &self,
        display: &mut dyn Display,
        frame: &[u8],
    ) -> Result<(), DriverError> {
        let mode = display.output().ok_or(DriverError::Stale)?;
        let buffer = display.alloc_buffer(mode.width_px, mode.height_px)?;
        let shown = self
            .fill(buffer.region, frame)
            .and_then(|()| display.present(buffer.id, &[DamageRect::full(&mode)]));
        let freed = display.free_buffer(buffer.id);
        shown.and(freed)
    }
}

/// One region of a [`HeapMemory`].
pub struct HeapRegion {
    id: u64,
    bytes: Rc<RefCell<Vec<u8>>>,
}

/// The tag every address a heap region claims a device reads it at carries,
/// so no command naming one can be mistaken for a CPU address.
const DEVICE_TAG: u64 = 1 << 56;

/// What a region is zeroed from a page at a time: one copy per page, where
/// `resize` writes each byte, which an interpreter such as Miri pays for byte
/// by byte.
const ZEROES: [u8; 4096] = [0; 4096];

impl HeapMemory {
    /// Where a device would read region `id`, as [`DeviceBuffer::device_addr`]
    /// answers for it.
    #[must_use]
    pub const fn device_addr_of(id: u64) -> u64 {
        DEVICE_TAG | (id << 32)
    }
}

impl DeviceBuffer for HeapRegion {
    fn device_addr(&self) -> u64 {
        HeapMemory::device_addr_of(self.id)
    }

    fn with_bytes_mut<R>(&mut self, write: impl FnOnce(&mut [u8]) -> R) -> R {
        write(&mut self.bytes.borrow_mut())
    }
}

impl SharedBuffer for HeapRegion {
    fn id(&self) -> u64 {
        self.id
    }

    fn with_bytes<R>(&self, read: impl FnOnce(&[u8]) -> R) -> R {
        read(&self.bytes.borrow())
    }
}

impl BufferMemory for HeapMemory {
    type Region = HeapRegion;

    fn create(&mut self, len: usize) -> Result<HeapRegion, DriverError> {
        if self.refuse {
            return Err(DriverError::OutOfMemory);
        }
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(len)
            .map_err(|_| DriverError::OutOfMemory)?;
        while bytes.len() < len {
            let page = (len - bytes.len()).min(ZEROES.len());
            bytes.extend_from_slice(&ZEROES[..page]);
        }
        let id = self.next.get() + 1;
        self.next.set(id);
        let bytes = Rc::new(RefCell::new(bytes));
        let mut regions = self.regions.borrow_mut();
        regions.retain(|(_, region)| region.strong_count() > 0);
        regions.push((id, Rc::downgrade(&bytes)));
        Ok(HeapRegion { id, bytes })
    }
}
