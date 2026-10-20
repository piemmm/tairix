//! The memory a display engine allocates its presenter's buffers in: shared
//! regions the display service grants, and, for an engine whose device reads
//! them where they lie, the address the device reads them at.

use tairix_abi::display_ipc::{DISPLAY_MAX_BUFFERS, DISPLAY_MAX_FRAMES};
use tairix_abi::driver::display::{BufferId, DisplayMode};
use tairix_abi::DriverError;

use crate::scanout::scanout_len;

/// Most bytes one presenter's buffers may hold together on a display that
/// composites no layers: the most output frames a presenter rotates through,
/// which leaves its pointer images room beside them. `None` for a mode with
/// no frame.
#[must_use]
pub fn frames_budget(mode: &DisplayMode) -> Option<usize> {
    scanout_len(mode)?.checked_mul(DISPLAY_MAX_FRAMES as usize)
}

/// The ids a driver gives its presenter's buffers: each fresh, and never one a
/// buffer it still holds has, however long it runs.
#[derive(Debug, Default)]
pub struct BufferIds {
    last: u32,
}

impl BufferIds {
    /// None given yet.
    #[must_use]
    pub const fn new() -> Self {
        Self { last: 0 }
    }

    /// The next id `held` says no buffer has; `None` only when every id is
    /// held, which no table of [`DISPLAY_MAX_BUFFERS`] makes so.
    pub fn mint(&mut self, held: impl Fn(BufferId) -> bool) -> Option<BufferId> {
        // At most a table's worth are held, so one of a table's worth and one
        // more successive ids is free.
        for _ in 0..=DISPLAY_MAX_BUFFERS {
            self.last = self.last.checked_add(1).unwrap_or(1);
            let id = BufferId::new(self.last)?;
            if !held(id) {
                return Some(id);
            }
        }
        None
    }
}

/// Copy `rows` rows of `row_bytes` bytes each from `source`, whose rows lie
/// `source_stride` bytes apart, into `target`, whose rows lie `target_stride`
/// bytes apart.
///
/// # Errors
///
/// [`DriverError::BufferTooSmall`] where either side ends short of a row; the
/// rows before it are copied.
pub fn copy_rows(
    source: &[u8],
    source_stride: usize,
    target: &mut [u8],
    target_stride: usize,
    row_bytes: usize,
    rows: usize,
) -> Result<(), DriverError> {
    for line in 0..rows {
        let (from, to) = (line * source_stride, line * target_stride);
        let (Some(dst), Some(src)) = (
            target.get_mut(to..to + row_bytes),
            source.get(from..from + row_bytes),
        ) else {
            return Err(DriverError::BufferTooSmall);
        };
        dst.copy_from_slice(src);
    }
    Ok(())
}

/// The shared memory a display engine allocates its presenter's buffers in.
pub trait BufferMemory {
    /// A region, released when dropped.
    type Region: SharedBuffer;

    /// Create a zeroed region of at least `len` bytes.
    ///
    /// # Errors
    ///
    /// [`DriverError::OutOfMemory`] when none can be had, or the memory's
    /// own refusal: none its device reaches.
    fn create(&mut self, len: usize) -> Result<Self::Region, DriverError>;
}

/// A shared region a display engine allocated.
pub trait SharedBuffer {
    /// The kernel's id for the region, which the display service grants.
    fn id(&self) -> u64;

    /// Read the region's bytes, as the engine maps them.
    fn with_bytes<R>(&self, read: impl FnOnce(&[u8]) -> R) -> R;
}

/// A shared region a display device reads where it lies.
///
/// Dropping it tells the kernel the device has stopped reading it, so an
/// engine drops one only once its device can no longer reach it.
pub trait DeviceBuffer: SharedBuffer {
    /// Where the device reads the first byte: its I/O virtual address behind
    /// a translation unit, else its bus address.
    fn device_addr(&self) -> u64;

    /// Write the region's bytes, as the engine maps them.
    fn with_bytes_mut<R>(&mut self, write: impl FnOnce(&mut [u8]) -> R) -> R;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(raw: u32) -> BufferId {
        BufferId::new(raw).expect("a non-zero id")
    }

    #[test]
    fn ids_count_up_from_one() {
        let mut ids = BufferIds::new();
        assert_eq!(ids.mint(|_| false), Some(id(1)));
        assert_eq!(ids.mint(|_| false), Some(id(2)));
    }

    /// The count wraps past zero, and an id a buffer still holds from the
    /// last time round is never given out again.
    #[test]
    fn a_wrapped_count_skips_every_id_still_held() {
        let mut ids = BufferIds { last: u32::MAX - 1 };
        assert_eq!(ids.mint(|_| false), Some(id(u32::MAX)));
        let held = [id(1), id(2)];
        assert_eq!(ids.mint(|candidate| held.contains(&candidate)), Some(id(3)));
    }

    #[test]
    fn a_full_table_s_worth_of_held_ids_leaves_one_free() {
        let mut ids = BufferIds::new();
        let held = |candidate: BufferId| candidate.get() as usize <= DISPLAY_MAX_BUFFERS;
        let minted = ids.mint(held).expect("one free");
        assert_eq!(minted.get() as usize, DISPLAY_MAX_BUFFERS + 1);
        assert_eq!(BufferIds::new().mint(|_| true), None, "every id held");
    }
}
