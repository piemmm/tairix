//! The buffers a presenter fills, which the HVS scans out where they lie.
//!
//! Each is carved in memory the HVS reaches and granted to the presenter, so
//! a present writes no pixel. A buffer the presenter frees while a list the
//! HVS reads still names it is kept until no list does: giving its memory
//! back tells the kernel the device has stopped reading it.

use tairix_abi::display_ipc::DISPLAY_MAX_BUFFERS;
use tairix_abi::driver::display::{AllocatedBuffer, BufferId};
use tairix_abi::DriverError;
use tairix_display::{BufferIds, BufferMemory, DeviceBuffer};
use tairix_inline::ArrayVec;

/// A zeroed buffer of `len` bytes from `memory`, and the bus address the HVS
/// reads it at, which must lie below 4 GiB: the HVS drives 32 address bits.
///
/// # Errors
///
/// The memory's refusal, or [`DriverError::OutOfRange`] for a buffer the HVS
/// cannot reach.
pub(crate) fn carve<A: BufferMemory>(
    memory: &mut A,
    len: usize,
) -> Result<(A::Region, u32), DriverError>
where
    A::Region: DeviceBuffer,
{
    let region = memory.create(len)?;
    let device = region.device_addr();
    let bus = device
        .checked_add(len as u64)
        .filter(|&end| end <= 1 << 32)
        .and_then(|_| u32::try_from(device).ok())
        .ok_or(DriverError::OutOfRange)?;
    Ok((region, bus))
}

/// One buffer and its layout.
pub(crate) struct Held<M> {
    pub id: BufferId,
    pub memory: M,
    pub bus: u32,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
}

impl<M> Held<M> {
    fn len(&self) -> usize {
        self.stride as usize * self.height as usize
    }
}

/// The buffers a presenter holds, and those it gave back that a list still
/// names.
pub(crate) struct Buffers<M> {
    held: ArrayVec<Held<M>, DISPLAY_MAX_BUFFERS>,
    doomed: ArrayVec<Held<M>, DISPLAY_MAX_BUFFERS>,
    bytes: usize,
    ids: BufferIds,
}

impl<M: DeviceBuffer> Buffers<M> {
    pub const fn new() -> Self {
        Self {
            held: ArrayVec::new(),
            doomed: ArrayVec::new(),
            bytes: 0,
            ids: BufferIds::new(),
        }
    }

    /// A `width` × `height` buffer laid out `stride` bytes a line, from
    /// `alloc`, within `budget` bytes for every buffer together.
    ///
    /// # Errors
    ///
    /// [`DriverError::NoSpace`] past the budget or the table, or the
    /// allocator's refusal.
    pub fn alloc<A: BufferMemory<Region = M>>(
        &mut self,
        alloc: &mut A,
        (width, height): (u32, u32),
        stride: u32,
        budget: usize,
    ) -> Result<AllocatedBuffer, DriverError> {
        let len = usize::try_from(u64::from(stride) * u64::from(height))
            .map_err(|_| DriverError::LengthOutOfRange)?;
        let bytes = self.bytes.checked_add(len).ok_or(DriverError::NoSpace)?;
        if self.held.len() + self.doomed.len() >= DISPLAY_MAX_BUFFERS || bytes > budget {
            return Err(DriverError::NoSpace);
        }
        let (held, doomed) = (&self.held, &self.doomed);
        let id = self
            .ids
            .mint(|id| held.iter().chain(doomed).any(|buffer| buffer.id == id))
            .ok_or(DriverError::NoSpace)?;
        let (memory, bus) = carve(alloc, len)?;
        let region = memory.id();
        self.held
            .try_push(Held {
                id,
                memory,
                bus,
                width,
                height,
                stride,
            })
            .map_err(|_| DriverError::NoSpace)?;
        self.bytes = bytes;
        Ok(AllocatedBuffer {
            id,
            region,
            stride_bytes: stride,
        })
    }

    pub fn get(&self, id: BufferId) -> Option<&Held<M>> {
        self.held.iter().find(|held| held.id == id)
    }

    /// Give buffer `id` back, kept while `named` says a list still names it.
    ///
    /// # Errors
    ///
    /// [`DriverError::NotFound`] for a buffer not held.
    pub fn free(
        &mut self,
        id: BufferId,
        named: impl Fn(BufferId) -> bool,
    ) -> Result<(), DriverError> {
        let at = self
            .held
            .iter()
            .position(|held| held.id == id)
            .ok_or(DriverError::NotFound)?;
        let Some(held) = self.held.swap_remove(at) else {
            return Err(DriverError::NotFound);
        };
        if named(id) {
            // The table has room: every doomed buffer left `held` first.
            let _ = self.doomed.try_push(held);
        } else {
            self.bytes = self.bytes.saturating_sub(held.len());
        }
        Ok(())
    }

    /// Give back every doomed buffer no list names any more.
    pub fn reap(&mut self, named: impl Fn(BufferId) -> bool) {
        let mut at = 0;
        while let Some(held) = self.doomed.get(at) {
            if named(held.id) {
                at += 1;
                continue;
            }
            if let Some(held) = self.doomed.swap_remove(at) {
                self.bytes = self.bytes.saturating_sub(held.len());
            }
        }
    }

    /// Whether a given-back buffer still waits on a list.
    pub fn any_doomed(&self) -> bool {
        !self.doomed.is_empty()
    }
}
