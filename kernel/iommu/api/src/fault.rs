//! Translation faults and the budget that keeps a device from flooding the
//! log with them.

use tairix_collections::HashMap;
use tairix_hash::BuildFastHash;

/// Why a unit refused a device's access.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum FaultReason {
    /// The stream is blocked: no domain translates it.
    Blocked,
    /// Nothing is mapped at the address.
    Unmapped,
    /// The mapping does not allow the access.
    Denied,
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
    /// The page the access fell in.
    pub iova: u64,
    /// Whether the access was a write.
    pub write: bool,
    /// Why it was refused.
    pub reason: FaultReason,
}

/// What to do with one fault under its stream's budget.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FaultVerdict {
    /// Record it.
    Record,
    /// Count it, but record nothing: the stream has used its window's share.
    Suppress,
    /// The stream crossed the storm threshold in this window: block it. Only
    /// the fault that crosses it answers this.
    Storm,
}

/// Per-stream fault accounting over fixed windows.
///
/// The thresholds are containment bounds, not capacities: a stream records at
/// most `record` faults per window, and one that raises `storm` in a window
/// is misbehaving badly enough to lose its DMA.
pub struct FaultBudget {
    window_ns: u64,
    record: u32,
    storm: u32,
    streams: HashMap<u32, StreamWindow, BuildFastHash>,
}

#[derive(Copy, Clone)]
struct StreamWindow {
    started_ns: u64,
    seen: u32,
    suppressed: u32,
}

impl FaultBudget {
    /// A budget recording `record` faults per stream per `window_ns`, and
    /// calling a storm at `storm` in one window.
    #[must_use]
    pub fn new(window_ns: u64, record: u32, storm: u32) -> Self {
        Self {
            window_ns,
            record,
            storm,
            streams: HashMap::with_hasher(BuildFastHash::new()),
        }
    }

    /// Charge one fault on `stream` at `now_ns`. A stream whose window has
    /// run out starts a new one. The count of faults the ending window
    /// suppressed is returned beside the verdict so the caller can say how
    /// many went unrecorded.
    pub fn charge(&mut self, stream: u32, now_ns: u64) -> (FaultVerdict, u32) {
        let fresh = StreamWindow {
            started_ns: now_ns,
            seen: 0,
            suppressed: 0,
        };
        // A stream the table cannot hold is judged on this one fault alone.
        if !self.streams.contains_key(&stream) && self.streams.try_insert(stream, fresh).is_err() {
            return (FaultVerdict::Record, 0);
        }
        let Some(entry) = self.streams.get_mut(&stream) else {
            return (FaultVerdict::Record, 0);
        };
        let mut carried = 0;
        if now_ns.saturating_sub(entry.started_ns) >= self.window_ns {
            carried = entry.suppressed;
            *entry = fresh;
        }
        entry.seen = entry.seen.saturating_add(1);
        let verdict = if entry.seen == self.storm {
            FaultVerdict::Storm
        } else if entry.seen <= self.record {
            FaultVerdict::Record
        } else {
            entry.suppressed = entry.suppressed.saturating_add(1);
            FaultVerdict::Suppress
        };
        (verdict, carried)
    }

    /// Forget `stream`: it was blocked, or its domain ended.
    pub fn forget(&mut self, stream: u32) {
        self.streams.remove(&stream);
    }

    /// Streams with a live window.
    #[must_use]
    pub fn tracked(&self) -> usize {
        self.streams.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stream_records_its_share_then_is_suppressed_then_storms_once() {
        let mut budget = FaultBudget::new(1_000, 2, 5);
        assert_eq!(budget.charge(7, 0), (FaultVerdict::Record, 0));
        assert_eq!(budget.charge(7, 1), (FaultVerdict::Record, 0));
        assert_eq!(budget.charge(7, 2), (FaultVerdict::Suppress, 0));
        assert_eq!(budget.charge(7, 3), (FaultVerdict::Suppress, 0));
        assert_eq!(budget.charge(7, 4), (FaultVerdict::Storm, 0));
        assert_eq!(budget.charge(7, 5), (FaultVerdict::Suppress, 0));
    }

    #[test]
    fn a_new_window_starts_over_and_reports_what_the_last_one_suppressed() {
        let mut budget = FaultBudget::new(1_000, 1, 100);
        budget.charge(3, 0);
        budget.charge(3, 10);
        budget.charge(3, 20);
        assert_eq!(budget.charge(3, 1_000), (FaultVerdict::Record, 2));
        assert_eq!(budget.charge(3, 1_001), (FaultVerdict::Suppress, 0));
    }

    #[test]
    fn streams_are_charged_apart_and_forgotten_on_request() {
        let mut budget = FaultBudget::new(1_000, 1, 3);
        budget.charge(1, 0);
        assert_eq!(budget.charge(2, 0), (FaultVerdict::Record, 0));
        assert_eq!(budget.tracked(), 2);
        budget.forget(1);
        assert_eq!(budget.tracked(), 1);
        assert_eq!(budget.charge(1, 1), (FaultVerdict::Record, 0));
    }
}
