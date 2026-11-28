//! TAIRiX Bridge's diagnostic records and their stable identifiers.

use tairix_log::EventId;

/// A session started: its kind and its guest's RAM.
pub const SESSION_STARTED: EventId = EventId(39_001);
/// A session ended, and why.
pub const SESSION_ENDED: EventId = EventId(39_002);
/// A session's worker was contained for what it sent.
pub const WORKER_CONTAINED: EventId = EventId(39_003);
/// A launch was placed in the shared session or an isolated one.
pub const LAUNCH_PLACED: EventId = EventId(39_004);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_record_is_in_the_bridge_s_range() {
        for id in [
            SESSION_STARTED,
            SESSION_ENDED,
            WORKER_CONTAINED,
            LAUNCH_PLACED,
        ] {
            assert!(tairix_log::registry::RISCOS.contains(id), "{id:?}");
        }
    }
}
