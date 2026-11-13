//! The emulator's diagnostic records and their stable identifiers.

use tairix_log::EventId;

/// The machine has run its first second of its own time since it was built,
/// its BASIC prompt on the screen.
pub const MACHINE_RUNNING: EventId = EventId(37_001);
/// [`MACHINE_RUNNING`]'s message.
pub const MACHINE_RUNNING_MESSAGE: &str = "beeb: the machine is running";
/// A worker that failed was replaced, the machine started again.
pub const WORKER_REPLACED: EventId = EventId(37_002);
/// [`WORKER_REPLACED`]'s message.
pub const WORKER_REPLACED_MESSAGE: &str = "beeb: the machine's worker was replaced";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_record_is_in_the_emulators_range() {
        for id in [MACHINE_RUNNING, WORKER_REPLACED] {
            assert!(tairix_log::registry::BEEB.contains(id), "{id:?}");
        }
    }
}
