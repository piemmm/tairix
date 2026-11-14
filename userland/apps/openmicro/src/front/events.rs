//! The emulator's diagnostic records and their stable identifiers.

use tairix_log::EventId;

/// The machine has run its first second of its own time since it was built,
/// its BASIC prompt on the screen.
pub const MACHINE_RUNNING: EventId = EventId(37_001);
/// [`MACHINE_RUNNING`]'s message.
pub const MACHINE_RUNNING_MESSAGE: &str = "OpenMicro: the machine is running";
/// A worker that failed was replaced, the machine started again.
pub const WORKER_REPLACED: EventId = EventId(37_002);
/// [`WORKER_REPLACED`]'s message.
pub const WORKER_REPLACED_MESSAGE: &str = "OpenMicro: the machine's worker was replaced";
/// The library was read and its window shows every machine.
pub const LIBRARY_READ: EventId = EventId(37_003);
/// [`LIBRARY_READ`]'s message.
pub const LIBRARY_READ_MESSAGE: &str = "OpenMicro: the machine library is read";
/// A machine's suspended state is on the store and its window gone.
pub const MACHINE_SUSPENDED: EventId = EventId(37_004);
/// [`MACHINE_SUSPENDED`]'s message.
pub const MACHINE_SUSPENDED_MESSAGE: &str = "OpenMicro: a machine is suspended";
/// A resumed machine's suspended state is restored.
pub const MACHINE_RESUMED: EventId = EventId(37_005);
/// [`MACHINE_RESUMED`]'s message.
pub const MACHINE_RESUMED_MESSAGE: &str = "OpenMicro: a machine resumed";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_record_is_in_the_emulators_range() {
        for id in [
            MACHINE_RUNNING,
            WORKER_REPLACED,
            LIBRARY_READ,
            MACHINE_SUSPENDED,
            MACHINE_RESUMED,
        ] {
            assert!(tairix_log::registry::OPENMICRO.contains(id), "{id:?}");
        }
    }
}
