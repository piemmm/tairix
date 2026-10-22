//! The write-back core the block-backed filesystem drivers share
//! (`plans/ARXFS-WRITEBACK.md`).
//!
//! A filesystem that batches its commits holds each transaction's changed
//! blocks in RAM and publishes them behind one barrier. That needs four
//! pieces, defined once here so no driver carries its own copy:
//!
//! * [`DirtySet`] — the transaction's blocks keyed by address, with a
//!   per-operation savepoint so a failed operation undoes alone, drained in
//!   ascending coalesced runs.
//! * [`WritebackBound`] — the RAM-derived, pressure-governed, machine-shared
//!   ceiling on what a volume's transaction may pin.
//! * [`CommitScheduler`] — the dirty-age window by command cost, reported to
//!   the host's write-back timer so a quiet volume is still published.
//! * [`RunWindow`] — the fallible staging buffer one coalesced device
//!   transfer of at most [`RUN_BYTES`] moves through.
//!
//! The crate performs no I/O: the owning driver does, so every piece stays
//! host-testable.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

use alloc::vec::Vec;

use zeroize::Zeroize;

mod bound;
mod dirty;
mod schedule;

pub use bound::{WritebackBound, WritebackReading, WRITEBACK_FLOOR_BYTES};
pub use dirty::{DirtySet, WritePhase};
#[cfg(any(test, feature = "test-util"))]
pub use schedule::TestVolumeHost;
pub use schedule::{writeback_window_ns, CommitScheduler};

/// Bytes one coalesced device request covers, in either direction.
///
/// A storage controller moves this much in one DMA transfer, so both
/// directions gather contiguous blocks up to it rather than paying a command
/// and a completion wait per block. It also bounds the staging each transfer
/// allocates, so many volumes on a small machine stay bounded.
pub const RUN_BYTES: usize = 64 * 1024;

/// A bounded staging window for one coalesced device run.
///
/// Reserved fallibly, so a machine too short of memory to hold it does
/// slower per-block I/O rather than failing the operation, and wiped on drop
/// because it carries volume contents. The default window holds nothing until
/// [`Self::reserve`] grows it.
#[derive(Default)]
pub struct RunWindow {
    bytes: Vec<u8>,
}

impl RunWindow {
    /// A window of `span` bytes, or an absent one when `span` is zero or the
    /// reservation is refused.
    #[must_use]
    pub fn new(span: usize) -> Self {
        // A refused window falls back to the per-block form, where
        // `vec![0; span]` would abort.
        Self {
            bytes: tairix_util::fallible::filled(span, 0).unwrap_or_default(),
        }
    }

    /// Whether the window is absent and the caller must fall back.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// The first `span` bytes of the window, grown to hold them, or `None`
    /// when the reservation is refused. Growing wipes what the window held
    /// rather than carrying it into the new buffer.
    pub fn reserve(&mut self, span: usize) -> Option<&mut [u8]> {
        if self.bytes.len() < span {
            let grown = tairix_util::fallible::filled(span, 0)?;
            self.bytes.zeroize();
            self.bytes = grown;
        }
        Some(&mut self.bytes[..span])
    }

    /// The staging bytes, or `None` when there is no window.
    pub fn buf(&mut self) -> Option<&mut [u8]> {
        if self.bytes.is_empty() {
            None
        } else {
            Some(&mut self.bytes)
        }
    }
}

impl Drop for RunWindow {
    fn drop(&mut self) {
        self.bytes.zeroize();
    }
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
