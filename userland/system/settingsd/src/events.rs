//! Stable audit event ids the settings service records its decisions under
//! (`plans/SYSLOG.md`).

use tairix_log::EventId;

/// The service bound its rendezvous and is serving.
pub const SETTINGSD_READY: EventId = EventId(34_000);

/// The service could not serve: the reason is the message.
pub const SERVICE_UNAVAILABLE: EventId = EventId(34_001);

/// A document was changed, by whom, and how many settings.
pub const CHANGE_APPLIED: EventId = EventId(34_002);

/// A change was refused, whose, and why; nothing was written.
pub const CHANGE_REFUSED: EventId = EventId(34_003);

/// A document was read, and by whom: the network configuration names the
/// machine's hardware and its addresses.
pub const READ_ANSWERED: EventId = EventId(34_004);

/// A read was refused, whose, and why.
pub const READ_REFUSED: EventId = EventId(34_005);

/// A request was refused before it named anything: malformed, or from a
/// caller not holding `CAP_SETTINGS_WRITE`.
pub const REQUEST_REFUSED: EventId = EventId(34_006);

/// The kernel would not attest the caller of a received request, which was
/// refused.
pub const ORIGIN_UNREADABLE: EventId = EventId(34_007);

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [EventId; 8] = [
        SETTINGSD_READY,
        SERVICE_UNAVAILABLE,
        CHANGE_APPLIED,
        CHANGE_REFUSED,
        READ_ANSWERED,
        READ_REFUSED,
        REQUEST_REFUSED,
        ORIGIN_UNREADABLE,
    ];

    #[test]
    fn every_id_is_the_service_s_own_and_distinct() {
        for (at, id) in ALL.iter().enumerate() {
            assert!(tairix_log::registry::SETTINGSD.contains(*id), "{id:?}");
            assert!(ALL[at + 1..].iter().all(|other| other != id), "{id:?}");
        }
    }
}
