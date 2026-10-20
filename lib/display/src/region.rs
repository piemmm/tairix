//! The map-once seam over the kernel's `shm_map` of a region another task
//! granted: how a window server reads the frames an application owns.

use tairix_abi::{Errno, ProcId};

/// A mapped shared region the holder reads frames from.
pub trait FrameRegion {
    /// The mapped bytes. The length is fixed at map time.
    fn bytes(&self) -> &[u8];
}

/// The map-once seam over the kernel's `shm_map` of a granted handle.
///
/// Dropping the returned region is the implementation's cue to unmap.
pub trait ShmMapper {
    /// The region type a successful map yields.
    type Region: FrameRegion;

    /// Map the region `grantor` delegated as `handle`, requiring at least
    /// `min_len` bytes.
    ///
    /// # Errors
    ///
    /// * [`Errno::NotFound`] — the handle names no grant `grantor`
    ///   delegated this task.
    /// * [`Errno::LengthOutOfRange`] — the region is smaller than
    ///   `min_len`.
    fn map(&mut self, grantor: ProcId, handle: u64, min_len: usize) -> Result<Self::Region, Errno>;
}
