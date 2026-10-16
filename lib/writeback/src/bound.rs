//! The RAM-derived ceiling on what one volume's open transaction may pin.

use alloc::sync::Arc;

use tairix_abi::DriverError;
use tairix_reclaim::{CacheBudget, GrowthAllowance, PinnedAccounting, PressureBand, PressureGauge};

use crate::RUN_BYTES;

/// The smallest ceiling worth having: one coalesced device transfer.
///
/// A floor, not a capacity: below one transfer the drain can never form a
/// full run, so the set would pin memory without buying the commands it
/// exists to buy. A transaction completes regardless, because a write path
/// always stores at least one record before the bound can cut it short.
pub const WRITEBACK_FLOOR_BYTES: usize = RUN_BYTES;

/// The byte ceiling, pressure gauge, and pinned ledger a volume's dirty set
/// is held to.
///
/// A staged block exists nowhere else, so it can only be written out, never
/// dropped; the set is bounded by forcing a commit, not by eviction, and is
/// deliberately kept out of the reclaim gate whose contract is droppability.
/// The ceiling is the **machine's**: every mounted volume takes a share, so a
/// machine's volumes never pin a multiple of what it has.
///
/// A handle with no bound is not memory-governed; the host installs one on
/// every writable mount, so only host tools and unit tests run without.
pub struct WritebackBound {
    budget: CacheBudget,
    gauge: &'static dyn PressureGauge,
    pinned: Arc<PinnedAccounting>,
}

impl WritebackBound {
    /// The bound a volume gets from `budget` (derived from discovered RAM),
    /// the system pressure `gauge`, and the `pinned` ledger the host
    /// publishes its footprint through.
    ///
    /// # Errors
    ///
    /// [`DriverError::NoSpace`] when the machine's whole derived ceiling
    /// cannot hold [`WRITEBACK_FLOOR_BYTES`]: the mount is refused rather
    /// than accepted and left to commit after almost every record.
    pub fn new(
        budget: CacheBudget,
        gauge: &'static dyn PressureGauge,
        pinned: Arc<PinnedAccounting>,
    ) -> Result<Self, DriverError> {
        if budget.hard() < WRITEBACK_FLOOR_BYTES {
            return Err(DriverError::NoSpace);
        }
        Ok(Self {
            budget,
            gauge,
            pinned,
        })
    }

    /// One gauge reading, folded once, for the operation about to run while
    /// the volume holds `held_bytes`.
    ///
    /// Read per operation, not per staged block: in the kernel the reading
    /// takes the global frame-allocator lock.
    #[must_use]
    pub fn reading(&self, held_bytes: usize) -> WritebackReading {
        let allowance = self.gauge.growth_allowance();
        WritebackReading {
            band: allowance.band(),
            ceiling: self.ceiling_bytes(allowance, held_bytes),
        }
    }

    /// This volume's share of the banded budget, capped by what the volumes
    /// already holding leave and by the machine-wide reserve, and never below
    /// the floor: a volume must be able to complete a transaction.
    fn ceiling_bytes(&self, allowance: GrowthAllowance, held_bytes: usize) -> usize {
        let banded = self.budget.hard() >> allowance.band().depth();
        let share = banded / self.pinned.drawing_pools();
        let left = banded.saturating_sub(self.pinned.other_bytes());
        let reserved = held_bytes.saturating_add(allowance.remaining_bytes());
        share.min(left).min(reserved).max(WRITEBACK_FLOOR_BYTES)
    }

    /// Publish what the volume's transaction pins, so the ledger row and the
    /// share the other volumes decide against carry it.
    pub fn publish(&self, bytes: usize, entries: u64) {
        self.pinned.set(bytes, entries);
    }

    /// Note one pass that wrote the set out and returned its bytes.
    pub fn note_released(&self) {
        self.pinned.note_released();
    }

    /// Note one admission the bound cut short.
    pub fn note_refusal(&self) {
        self.pinned.note_refusal();
    }
}

impl Drop for WritebackBound {
    /// The ledger row outlives the mount, so the footprint is zeroed rather
    /// than left claiming an unmounted volume's bytes against the others.
    fn drop(&mut self) {
        self.publish(0, 0);
    }
}

/// What one gauge reading told the operation about to run.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct WritebackReading {
    /// The band the reading folded to, which sets the dirty-age window.
    pub band: PressureBand,
    /// Bytes the dirty set may hold before the transaction is published.
    pub ceiling: usize,
}

#[cfg(test)]
#[path = "bound_tests.rs"]
mod tests;
