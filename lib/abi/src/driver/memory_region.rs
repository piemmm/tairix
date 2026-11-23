//! A device's memory region, mapped a part at a time (`abi-v1`).
//!
//! A [`MemoryRegion`](crate::hwtree::HwResourceKind::MemoryRegion) grant is
//! memory, not registers: the kernel maps a part of it with the
//! [`RegionCaching`] the device states for that part, and the driver copies
//! bytes through the [`MemoryWindow`] it is handed. A region may be larger
//! than the driver's device window, so a part is mapped while it is needed
//! and handed back.

use core::ptr::NonNull;

use super::mmio::WindowError;
use super::DriverError;
use crate::hwtree::HwResource;
use crate::Errno;

/// How the CPU caches a mapped part of a memory region.
///
/// The device states it per part, as the caching its own copy of the part is
/// kept coherent under: a cached alias of memory the host maps uncached loses
/// writes on hardware that does not snoop across the two.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum RegionCaching {
    /// Cached, written back.
    WriteBack = 1,
    /// Uncached, with writes combined.
    WriteCombine = 2,
    /// Uncached.
    Uncached = 3,
}

impl RegionCaching {
    /// The `mmio_map` caching argument for `caching`: `0` maps a window as
    /// its grant names, which every grant but a memory region's does.
    #[must_use]
    pub const fn arg(caching: Option<Self>) -> u64 {
        match caching {
            Some(caching) => caching as u64,
            None => 0,
        }
    }

    /// Decode the `mmio_map` caching argument.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] for any value [`Self::arg`] does not produce.
    pub const fn from_arg(raw: u64) -> Result<Option<Self>, Errno> {
        match raw {
            0 => Ok(None),
            1 => Ok(Some(Self::WriteBack)),
            2 => Ok(Some(Self::WriteCombine)),
            3 => Ok(Some(Self::Uncached)),
            _ => Err(Errno::OutOfRange),
        }
    }
}

/// A part of a device's memory region, mapped into the driver.
///
/// # Invariants (established by [`MemoryWindow::from_mapping`])
///
/// * `base` points at a readable, writable mapping of exactly `len` bytes,
///   valid until the window is handed back to the mapper that made it.
/// * `offset` is where in its region the part begins.
#[derive(Debug)]
pub struct MemoryWindow {
    base: NonNull<u8>,
    len: usize,
    offset: u64,
}

// SAFETY: the mapping belongs to the process, not to the thread that asked for
// it, so the window may move between threads. It is not `Sync`: two threads
// copying into one part at once would race on its bytes.
unsafe impl Send for MemoryWindow {}

impl MemoryWindow {
    /// A window over a mapping the kernel installed.
    ///
    /// # Safety
    ///
    /// `base` must point at a readable, writable mapping of exactly `len`
    /// bytes that stays valid until the window is handed back to its mapper,
    /// and no other live reference may alias those bytes.
    #[must_use]
    pub unsafe fn from_mapping(offset: u64, base: NonNull<u8>, len: usize) -> Self {
        Self { base, len, offset }
    }

    /// Bytes the part holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the part holds no bytes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Where in its region the part begins.
    #[must_use]
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// The address the part is mapped at, which names it when it is handed
    /// back.
    #[must_use]
    pub fn mapped_at(&self) -> usize {
        self.base.as_ptr().addr()
    }

    /// The first of `len` bytes at `at`, all inside the part.
    fn span(&self, at: usize, len: usize) -> Result<*mut u8, WindowError> {
        let end = at.checked_add(len).ok_or(WindowError::OutOfBounds)?;
        if end > self.len {
            return Err(WindowError::OutOfBounds);
        }
        // SAFETY: `at <= end <= len`, so the offset stays inside the one
        // mapping the construction invariant names.
        Ok(unsafe { self.base.as_ptr().add(at) })
    }

    /// Copy the part's bytes at `at` into `into`.
    ///
    /// A byte the device writes during the copy may be read as either value;
    /// when a byte is settled is the region's protocol to say.
    ///
    /// # Errors
    ///
    /// [`WindowError::OutOfBounds`] for a span past the part.
    pub fn read(&self, at: usize, into: &mut [u8]) -> Result<(), WindowError> {
        let from = self.span(at, into.len())?;
        // SAFETY: `span` proved the source lies inside the mapping, and `into`
        // is a caller buffer no mapping of the region can alias.
        unsafe { core::ptr::copy_nonoverlapping(from, into.as_mut_ptr(), into.len()) };
        Ok(())
    }

    /// Copy `from` into the part at `at`.
    ///
    /// # Errors
    ///
    /// [`WindowError::OutOfBounds`] for a span past the part.
    pub fn write(&self, at: usize, from: &[u8]) -> Result<(), WindowError> {
        let into = self.span(at, from.len())?;
        // SAFETY: as in `read`, with the roles exchanged.
        unsafe { core::ptr::copy_nonoverlapping(from.as_ptr(), into, from.len()) };
        Ok(())
    }
}

/// What maps the parts of a driver's granted memory regions into it.
pub trait MemoryMapper {
    /// Map `[offset, offset + len)` of `region`, a granted
    /// [`MemoryRegion`](crate::hwtree::HwResourceKind::MemoryRegion), with
    /// the `caching` the device states for it.
    ///
    /// # Errors
    ///
    /// * [`DriverError::NotFound`] — `region` is not one of the driver's
    ///   grants.
    /// * [`DriverError::OutOfRange`] — the part runs past the region.
    /// * [`DriverError::LengthOutOfRange`] — an empty part.
    /// * [`DriverError::OutOfMemory`] — the driver's device window has no
    ///   room for it.
    /// * [`DriverError::PermissionDenied`] — the driver holds no
    ///   [`CapabilityId::MMIO_MAP`](crate::CapabilityId::MMIO_MAP).
    fn map_memory(
        &self,
        region: &HwResource,
        offset: u64,
        len: usize,
        caching: RegionCaching,
    ) -> Result<MemoryWindow, DriverError>;

    /// Hand back the part `window` maps; once this returns, no CPU of the
    /// process reaches the part through it.
    ///
    /// # Errors
    ///
    /// The window and the reason, where the part could not be unmapped: it is
    /// still mapped, so its bytes may not be reused.
    fn unmap_memory(&self, window: MemoryWindow) -> Result<(), (MemoryWindow, DriverError)>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[repr(align(8))]
    struct Aligned([u8; 64]);

    fn window_over(buf: &mut Aligned, offset: u64) -> MemoryWindow {
        let base = NonNull::new(buf.0.as_mut_ptr()).expect("non-null");
        // SAFETY: the buffer outlives the window in every test, which reaches
        // it through the window alone.
        unsafe { MemoryWindow::from_mapping(offset, base, buf.0.len()) }
    }

    #[test]
    fn a_window_copies_its_bytes_both_ways_at_any_offset() {
        let mut buf = Aligned([0; 64]);
        let window = window_over(&mut buf, 0x3000);
        assert_eq!((window.len(), window.offset()), (64, 0x3000));
        assert!(!window.is_empty());
        window.write(13, b"region").expect("in bounds");
        let mut back = [0u8; 6];
        window.read(13, &mut back).expect("in bounds");
        assert_eq!(&back, b"region");
        window.write(58, &[7; 6]).expect("reaches the last byte");
        window.read(0, &mut []).expect("an empty copy at the start");
        window.read(64, &mut []).expect("an empty copy at the end");
    }

    #[test]
    fn the_caching_argument_round_trips_and_refuses_every_other_value() {
        for caching in [
            None,
            Some(RegionCaching::WriteBack),
            Some(RegionCaching::WriteCombine),
            Some(RegionCaching::Uncached),
        ] {
            assert_eq!(
                RegionCaching::from_arg(RegionCaching::arg(caching)),
                Ok(caching)
            );
        }
        for raw in [4, 0x100, u64::from(u32::MAX) + 1, u64::MAX] {
            assert_eq!(RegionCaching::from_arg(raw), Err(Errno::OutOfRange));
        }
    }

    #[test]
    fn a_span_past_the_window_copies_nothing() {
        let mut buf = Aligned([0; 64]);
        let window = window_over(&mut buf, 0);
        assert_eq!(window.write(59, &[1; 6]), Err(WindowError::OutOfBounds));
        assert_eq!(
            window.read(usize::MAX, &mut [0; 2]),
            Err(WindowError::OutOfBounds)
        );
        assert_eq!(window.read(65, &mut []), Err(WindowError::OutOfBounds));
        let mut all = [1u8; 64];
        window.read(0, &mut all).expect("in bounds");
        assert_eq!(all, [0; 64], "the refused write changed nothing");
    }
}
