//! Stable [`tairix_log::EventId`] constants `netmap` emits when it probes.
//!
//! Active reachability probing is an administrative action (an ICMP-echo sweep
//! and a TCP well-known-port check), so each campaign's start, finish and
//! refusal are audited. The app owns the `33000..34000` range; the identifiers
//! are frozen and pinned by the test below.

use tairix_log::EventId;

/// Range start (inclusive) reserved for `netmap` event identifiers.
pub const NETMAP_RANGE_START: u32 = tairix_log::registry::NETMAP.start();

/// Range end (exclusive) reserved for `netmap` event identifiers.
pub const NETMAP_RANGE_END: u32 = tairix_log::registry::NETMAP.end();

/// A probe campaign began; carries the posture and the target count.
pub const PROBE_STARTED: EventId = EventId(33_001);

/// A probe campaign finished; carries the count of hosts found reachable.
pub const PROBE_FINISHED: EventId = EventId(33_002);

/// The message [`PROBE_FINISHED`] carries. Shared so the QEMU vertical can gate
/// on the live system's own audit line rather than a copy of the text.
pub const PROBE_FINISHED_MESSAGE: &str = "netmap: reachability probe finished";

/// A campaign could not probe: a socket or capability refusal; carries why.
pub const PROBE_REFUSED: EventId = EventId(33_003);

#[cfg(test)]
#[path = "events_tests.rs"]
mod tests;
