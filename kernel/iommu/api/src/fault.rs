//! Translation faults and the budget that contains a device raising them.

use tairix_collections::HashMap;
use tairix_hash::BuildFastHash;
use tairix_inline::ArrayVec;

use crate::IommuError;

/// Why a unit refused a device's access.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum FaultReason {
    /// The stream is blocked: no domain translates it.
    Blocked,
    /// Nothing is mapped at the address.
    Unmapped,
    /// The mapping does not allow the access.
    Denied,
    /// The device presented an address as already translated (PCIe ATS),
    /// which the unit refuses: no device is trusted to cache translations.
    Translated,
    /// The unit refused an interrupt request: from a source its entry does
    /// not admit, naming no entry, or in a format remapping blocks. The
    /// fault's `iova` is the entry it named.
    Interrupt,
    /// The unit found its own configuration for the stream malformed.
    Malformed,
    /// A reason code the family could not classify.
    Other(u16),
}

/// One refused access, as the unit recorded it.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Fault {
    /// The stream id the access carried.
    pub stream: u32,
    /// The page the access fell in; for [`FaultReason::Interrupt`], the
    /// remapping entry the request named, or the page of its message address
    /// from a unit that records no entry.
    pub iova: u64,
    /// Whether the access was a write.
    pub write: bool,
    /// Why it was refused.
    pub reason: FaultReason,
}

/// What to do with one fault under its unit's budget.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FaultVerdict {
    /// Record it.
    Record,
    /// Count it, but record nothing: its stream or its unit has used this
    /// window's share.
    Suppress,
    /// The stream crossed the storm threshold in this window: silence it,
    /// and record the storm where the unit's share has room — else count
    /// it, as a suppressed record is. Only the fault that crosses the
    /// threshold answers this.
    Storm {
        /// Whether the storm is recorded.
        recorded: bool,
    },
}

/// One charged fault's verdict.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Charge {
    /// What to do with the fault.
    pub verdict: FaultVerdict,
    /// Faults the unit suppressed since it last recorded one, to report
    /// beside this one; zero for a suppressed fault.
    pub suppressed: u64,
}

/// The most fault records any family lets a unit hold before they are
/// drained: each family sizes its queue to at most this, so a containment
/// bound set above it cannot be reached by records one drain finds waiting.
pub const FAULT_QUEUE_RECORDS: u32 = 512;

/// Faults a family takes per hold of its lock.
pub const FAULT_BATCH: usize = 32;

/// The faults a family takes under its lock in one go.
pub type FaultBatch = ArrayVec<Fault, FAULT_BATCH>;

/// Drain up to `records` faults: `take` fills a batch under the family's lock
/// and answers whether more remain, and each batch reaches `sink` with the
/// lock released, since what the sink does about a fault may be to call back
/// in. Answers whether more remain once `records` are taken.
pub fn drain_in_batches(
    records: usize,
    mut take: impl FnMut(&mut FaultBatch) -> bool,
    sink: &mut dyn FnMut(Fault),
) -> bool {
    for _ in 0..records.div_ceil(FAULT_BATCH) {
        let mut batch = FaultBatch::new();
        let more = take(&mut batch);
        for fault in batch {
            sink(fault);
        }
        if !more {
            return false;
        }
    }
    true
}

/// What one unit's faults may cost per window.
///
/// These are containment bounds, not capacities: whatever the machine, they
/// cap the log records, the drain work and the table room one unit's devices
/// can claim.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct FaultLimits {
    /// The accounting window.
    pub window_ns: u64,
    /// Faults one stream may have recorded per window.
    pub stream_records: u32,
    /// Faults the unit may have recorded per window, every stream together.
    pub unit_records: u32,
    /// Faults one stream may raise in a window before it is silenced.
    pub storm: u32,
    /// Drains of the unit per window; past it, the drain waits out the window.
    pub drains: u32,
    /// Streams charged apart per window. A stream past it is judged against
    /// the unit's share alone, and cannot storm.
    pub streams: usize,
}

/// One unit's fault accounting over fixed windows: which faults to record,
/// which stream to silence, and when the drain must wait.
pub struct FaultBudget {
    limits: FaultLimits,
    started_ns: u64,
    recorded: u32,
    drained: u32,
    suppressed: u64,
    /// Faults each stream raised this window.
    streams: HashMap<u32, u32, BuildFastHash>,
}

impl FaultBudget {
    /// A budget holding `limits` from a window starting at `now_ns`, with its
    /// stream table's room taken now so charging never allocates.
    ///
    /// # Errors
    ///
    /// [`IommuError::OutOfRange`] for limits that cannot work — a zero window,
    /// share or drain count, or a storm a stream reaches while still recorded
    /// — and [`IommuError::Exhausted`] when the table's room cannot be had.
    pub fn new(limits: FaultLimits, now_ns: u64) -> Result<Self, IommuError> {
        if limits.window_ns == 0
            || limits.stream_records == 0
            || limits.unit_records == 0
            || limits.drains == 0
            || limits.storm <= limits.stream_records
        {
            return Err(IommuError::OutOfRange);
        }
        let streams = HashMap::try_with_capacity_and_hasher(limits.streams, BuildFastHash::new())
            .map_err(|_| IommuError::Exhausted)?;
        Ok(Self {
            limits,
            started_ns: now_ns,
            recorded: 0,
            drained: 0,
            suppressed: 0,
            streams,
        })
    }

    /// Count one drain at `now_ns`, or name the instant the window ends when
    /// the window's drains are spent.
    ///
    /// # Errors
    ///
    /// The window's end, when the drain must wait for it.
    pub fn drain(&mut self, now_ns: u64) -> Result<(), u64> {
        self.roll(now_ns);
        if self.drained >= self.limits.drains {
            return Err(self.started_ns.saturating_add(self.limits.window_ns));
        }
        self.drained += 1;
        Ok(())
    }

    /// Charge one fault on `stream` at `now_ns`.
    pub fn charge(&mut self, stream: u32, now_ns: u64) -> Charge {
        self.roll(now_ns);
        let seen = self.seen(stream);
        let unit_share = self.recorded < self.limits.unit_records;
        if seen == Some(self.limits.storm) {
            // Containment is never budgeted; its record, like any, is.
            if unit_share {
                self.recorded += 1;
                return self.report(FaultVerdict::Storm { recorded: true });
            }
            self.suppressed = self.suppressed.saturating_add(1);
            return Charge {
                verdict: FaultVerdict::Storm { recorded: false },
                suppressed: 0,
            };
        }
        let stream_share = seen.is_none_or(|seen| seen <= self.limits.stream_records);
        if stream_share && unit_share {
            self.recorded += 1;
            return self.report(FaultVerdict::Record);
        }
        self.suppressed = self.suppressed.saturating_add(1);
        Charge {
            verdict: FaultVerdict::Suppress,
            suppressed: 0,
        }
    }

    /// Streams charged apart in this window.
    #[must_use]
    pub fn tracked(&self) -> usize {
        self.streams.len()
    }

    fn report(&mut self, verdict: FaultVerdict) -> Charge {
        Charge {
            verdict,
            suppressed: core::mem::take(&mut self.suppressed),
        }
    }

    fn seen(&mut self, stream: u32) -> Option<u32> {
        if let Some(seen) = self.streams.get_mut(&stream) {
            *seen = seen.saturating_add(1);
            return Some(*seen);
        }
        (self.streams.len() < self.limits.streams && self.streams.try_insert(stream, 1).is_ok())
            .then_some(1)
    }

    fn roll(&mut self, now_ns: u64) {
        if now_ns.saturating_sub(self.started_ns) >= self.limits.window_ns {
            self.started_ns = now_ns;
            self.recorded = 0;
            self.drained = 0;
            self.streams.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: FaultLimits = FaultLimits {
        window_ns: 1_000,
        stream_records: 2,
        unit_records: 3,
        storm: 5,
        drains: 2,
        streams: 4,
    };

    fn budget() -> FaultBudget {
        FaultBudget::new(LIMITS, 0).unwrap()
    }

    fn verdict(budget: &mut FaultBudget, stream: u32, now: u64) -> FaultVerdict {
        budget.charge(stream, now).verdict
    }

    #[test]
    fn a_stream_records_its_share_then_is_suppressed_then_storms_once() {
        let mut budget = budget();
        assert_eq!(verdict(&mut budget, 7, 0), FaultVerdict::Record);
        assert_eq!(verdict(&mut budget, 7, 1), FaultVerdict::Record);
        assert_eq!(verdict(&mut budget, 7, 2), FaultVerdict::Suppress);
        assert_eq!(verdict(&mut budget, 7, 3), FaultVerdict::Suppress);
        assert_eq!(
            budget.charge(7, 4),
            Charge {
                verdict: FaultVerdict::Storm { recorded: true },
                suppressed: 2
            },
            "the storm carries what went unrecorded"
        );
        assert_eq!(verdict(&mut budget, 7, 5), FaultVerdict::Suppress);
    }

    #[test]
    fn the_unit_s_share_bounds_many_streams_together() {
        let mut budget = budget();
        for stream in 0..3 {
            assert_eq!(verdict(&mut budget, stream, 0), FaultVerdict::Record);
        }
        assert_eq!(
            verdict(&mut budget, 3, 0),
            FaultVerdict::Suppress,
            "a fresh stream once the unit's share is spent"
        );
        assert_eq!(
            budget.charge(9, 1_000),
            Charge {
                verdict: FaultVerdict::Record,
                suppressed: 1
            },
            "a new window records again and says what the last one hid"
        );
    }

    #[test]
    fn a_stream_past_the_table_is_judged_on_the_unit_s_share_and_never_storms() {
        let limits = FaultLimits {
            unit_records: 100,
            streams: 1,
            ..LIMITS
        };
        let mut budget = FaultBudget::new(limits, 0).unwrap();
        assert_eq!(verdict(&mut budget, 1, 0), FaultVerdict::Record);
        assert_eq!(budget.tracked(), 1);
        for now in 0..10 {
            assert_eq!(verdict(&mut budget, 2, now), FaultVerdict::Record);
        }
        assert_eq!(budget.tracked(), 1, "the table holds its room, no more");
    }

    /// Every storm is contained, but its record takes the unit's share like
    /// any other: storms from many streams flood the log no more than their
    /// faults could.
    #[test]
    fn a_storm_past_the_unit_s_share_is_contained_but_not_recorded() {
        let limits = FaultLimits {
            unit_records: 1,
            ..LIMITS
        };
        let mut budget = FaultBudget::new(limits, 0).unwrap();
        assert_eq!(verdict(&mut budget, 9, 0), FaultVerdict::Record);
        for stream in 0..3 {
            for now in 0..4 {
                assert_eq!(verdict(&mut budget, stream, now), FaultVerdict::Suppress);
            }
            assert_eq!(
                verdict(&mut budget, stream, 4),
                FaultVerdict::Storm { recorded: false },
                "stream {stream}"
            );
        }
        assert_eq!(
            budget.charge(9, 1_000).suppressed,
            15,
            "the next record says what went unrecorded, storms included"
        );
    }

    /// What a stormed stream's device had queued is only counted, and the
    /// next window gives it a share again.
    #[test]
    fn a_stormed_stream_stays_counted_until_its_window_ends() {
        let mut budget = budget();
        for now in 0..5 {
            budget.charge(1, now);
        }
        for now in 5..20 {
            assert_eq!(budget.charge(1, now).verdict, FaultVerdict::Suppress);
        }
        assert_eq!(budget.charge(1, 1_000).verdict, FaultVerdict::Record);
        assert_eq!(budget.tracked(), 1, "the window ended every stream's count");
    }

    #[test]
    fn drains_past_the_window_s_count_wait_for_its_end() {
        let mut budget = budget();
        assert_eq!(budget.drain(10), Ok(()));
        assert_eq!(budget.drain(20), Ok(()));
        assert_eq!(budget.drain(30), Err(1_000));
        assert_eq!(budget.drain(1_000), Ok(()), "the next window drains again");
    }

    #[test]
    fn limits_that_cannot_work_are_refused() {
        for limits in [
            FaultLimits {
                window_ns: 0,
                ..LIMITS
            },
            FaultLimits {
                stream_records: 0,
                ..LIMITS
            },
            FaultLimits {
                unit_records: 0,
                ..LIMITS
            },
            FaultLimits {
                drains: 0,
                ..LIMITS
            },
            FaultLimits {
                storm: LIMITS.stream_records,
                ..LIMITS
            },
        ] {
            assert_eq!(
                FaultBudget::new(limits, 0).err(),
                Some(IommuError::OutOfRange)
            );
        }
    }
}
