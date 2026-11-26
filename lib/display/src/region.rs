//! The map-once seam over the kernel's `shm_map` of a region another task
//! granted: how a window server reads the frames an application owns.

use alloc::rc::Rc;

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
    type Region: FrameRegion + 'static;

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

/// One frame of a mapped region, held for as long as anything still reads it:
/// the region stays mapped until its last holder lets go.
#[derive(Clone)]
pub struct SharedFrame {
    region: Rc<dyn FrameRegion>,
    start: usize,
    len: usize,
}

impl SharedFrame {
    /// The `len` bytes of `region` from `start`.
    ///
    /// # Errors
    ///
    /// [`Errno::DeviceFault`] for a span the region does not hold.
    pub fn new(region: Rc<dyn FrameRegion>, start: usize, len: usize) -> Result<Self, Errno> {
        let end = start.checked_add(len).ok_or(Errno::DeviceFault)?;
        if end > region.bytes().len() {
            return Err(Errno::DeviceFault);
        }
        Ok(Self { region, start, len })
    }

    /// The frame's bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        // A region's length is fixed when it is mapped, so the span checked on
        // construction is always there.
        self.region
            .bytes()
            .get(self.start..self.start.saturating_add(self.len))
            .unwrap_or_default()
    }
}

impl core::fmt::Debug for SharedFrame {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SharedFrame")
            .field("start", &self.start)
            .field("len", &self.len)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use alloc::rc::Rc;
    use alloc::vec::Vec;

    use super::{FrameRegion, SharedFrame};
    use tairix_abi::Errno;

    struct Bytes(Vec<u8>);

    impl FrameRegion for Bytes {
        fn bytes(&self) -> &[u8] {
            &self.0
        }
    }

    fn region() -> Rc<dyn FrameRegion> {
        Rc::new(Bytes((0..16).collect()))
    }

    #[test]
    fn a_frame_is_its_span_of_the_region_and_keeps_the_region_alive() {
        let region = region();
        let frame = SharedFrame::new(Rc::clone(&region), 4, 8).expect("inside");
        assert_eq!(frame.bytes(), &[4, 5, 6, 7, 8, 9, 10, 11]);
        drop(region);
        assert_eq!(
            frame.bytes(),
            &[4, 5, 6, 7, 8, 9, 10, 11],
            "held by the frame alone"
        );
    }

    #[test]
    fn a_span_past_the_region_is_refused() {
        for (start, len) in [(9, 8), (0, 17), (usize::MAX, 2)] {
            assert_eq!(
                SharedFrame::new(region(), start, len).err(),
                Some(Errno::DeviceFault),
                "{start} {len}"
            );
        }
    }
}
