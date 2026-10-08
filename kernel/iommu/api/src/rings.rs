//! Ring memory for a unit that is itself a virtio device: what its queues
//! and requests live in.
//!
//! A unit's own DMA is not translated — nothing stands between it and RAM —
//! so a ring's device address is its physical address. Rings are never
//! freed: nothing proves a unit stopped reading them, as with every table a
//! unit was given.

use core::ptr::NonNull;

use tairix_abi::driver::dma::{DmaHost, DmaSlab, PoolId};
use tairix_abi::driver::virtio::VirtioHost;
use tairix_abi::driver::CompletionSignal;
use tairix_abi::DriverError;
use tairix_arch_api::PageTableFrames;

use crate::{Clock, IO_PAGE_SHIFT};

/// Physically contiguous, zeroed ring memory drawn from a frame source, and
/// the clock a unit's waits run on.
pub struct FrameRings<'f> {
    frames: &'f dyn PageTableFrames,
    clock: &'f dyn Clock,
    pool: PoolId,
}

impl<'f> FrameRings<'f> {
    /// Rings drawn from `frames`, waits measured on `clock`.
    #[must_use]
    pub fn new(frames: &'f dyn PageTableFrames, clock: &'f dyn Clock) -> Self {
        Self {
            frames,
            clock,
            pool: PoolId::fresh(),
        }
    }
}

impl DmaHost for FrameRings<'_> {
    fn alloc_dma_zeroed(&self, size: usize) -> Result<DmaSlab, DriverError> {
        if size == 0 {
            return Err(DriverError::BufferTooSmall);
        }
        let pages = size.div_ceil(1 << IO_PAGE_SHIFT);
        let order = pages
            .checked_next_power_of_two()
            .ok_or(DriverError::LengthOutOfRange)?
            .trailing_zeros();
        let phys = self
            .frames
            .alloc_block(order)
            .ok_or(DriverError::OutOfMemory)?;
        let Some(words) = self.frames.block_at(phys, order).and_then(NonNull::new) else {
            // Never handed to the unit, so it goes straight back.
            self.frames.free_block(phys, order);
            return Err(DriverError::DeviceFault);
        };
        // SAFETY: the block spans `2^order` frames, at least `size` bytes,
        // zeroed by its source; it was just handed out to this slab alone and
        // is never returned, so it stays valid and unaliased for the slab's
        // life. The unit masters it untranslated, at its physical address.
        Ok(unsafe { DmaSlab::from_leaked(phys, words.cast(), size, self.pool, 0) })
    }

    fn device_quiesced(&self) {}
}

impl VirtioHost for FrameRings<'_> {
    /// A unit is driven before any task can park, so its family spins on
    /// the clock instead; a wait asked for here makes none.
    fn notify_wait(&self, _queue_index: u16, _timeout_ns: u64) -> CompletionSignal {
        CompletionSignal::TimedOut
    }

    fn now_ns(&self) -> u64 {
        self.clock.now_ns()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hostmem::HostFrames;

    struct Stopped;

    impl Clock for Stopped {
        fn now_ns(&self) -> u64 {
            7
        }
    }

    #[test]
    fn a_ring_is_zeroed_contiguous_and_at_its_physical_address() {
        let frames = HostFrames::new(0x8000_0000);
        let rings = FrameRings::new(&frames, &Stopped);
        let mut slab = rings.alloc_dma_zeroed(5000).unwrap();
        assert_eq!(slab.as_bytes().len(), 5000);
        assert!(slab.as_bytes().iter().all(|&byte| byte == 0));
        assert_eq!(slab.device_addr() % 0x2000, 0, "two frames, aligned so");
        slab.as_bytes_mut()[4096] = 0xA5;
        let mut read = [0; 2];
        assert!(frames.read_bytes(slab.device_addr() + 4095, &mut read));
        assert_eq!(
            read,
            [0, 0xA5],
            "what the CPU writes is what the unit reads"
        );
        assert!(!frames.read_bytes(slab.device_addr() + 0x2000 - 1, &mut read));
        assert_eq!(rings.now_ns(), 7);
    }

    /// Hands out blocks whose CPU view it cannot give, counting what comes
    /// back.
    struct Unreachable<'f> {
        frames: &'f HostFrames,
        freed: core::sync::atomic::AtomicU32,
    }

    impl PageTableFrames for Unreachable<'_> {
        fn alloc_table(&self) -> Option<tairix_arch_api::TableFrame> {
            self.frames.alloc_table()
        }

        fn table_at(&self, phys: u64) -> Option<*mut [u64; tairix_arch_api::PAGE_TABLE_ENTRIES]> {
            self.frames.table_at(phys)
        }

        fn free_table(&self, phys: u64) {
            self.frames.free_table(phys);
        }

        fn alloc_block(&self, order: u32) -> Option<u64> {
            self.frames.alloc_block(order)
        }

        fn free_block(&self, phys: u64, order: u32) {
            self.freed
                .fetch_add(1, core::sync::atomic::Ordering::Relaxed);
            self.frames.free_block(phys, order);
        }
    }

    #[test]
    fn a_ring_whose_memory_cannot_be_seen_gives_it_back() {
        let frames = HostFrames::new(0x8000_0000);
        let unreachable = Unreachable {
            frames: &frames,
            freed: core::sync::atomic::AtomicU32::new(0),
        };
        let rings = FrameRings::new(&unreachable, &Stopped);
        assert!(matches!(
            rings.alloc_dma_zeroed(4096),
            Err(DriverError::DeviceFault)
        ));
        assert_eq!(unreachable.freed.into_inner(), 1);
    }

    #[test]
    fn an_empty_or_unobtainable_ring_is_refused() {
        let frames = HostFrames::new(0x8000_0000);
        let rings = FrameRings::new(&frames, &Stopped);
        assert!(matches!(
            rings.alloc_dma_zeroed(0),
            Err(DriverError::BufferTooSmall)
        ));
        frames.limit(1);
        assert!(matches!(
            rings.alloc_dma_zeroed(8192),
            Err(DriverError::OutOfMemory)
        ));
    }
}
