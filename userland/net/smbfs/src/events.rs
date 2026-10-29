//! Stable audit event ids the network-share service records its decisions
//! under (`plans/SYSLOG.md`), each naming the endpoint and the address it was
//! reached at, never a credential.

use tairix_log::EventId;

/// The service bound its endpoint and is serving.
pub const SERVICE_STARTED: EventId = EventId(35_000);

/// The service could not serve: the reason is the message.
pub const SERVICE_UNAVAILABLE: EventId = EventId(35_001);

/// A share was mounted: whose, the host, the share, the address and the
/// offer.
pub const MOUNTED: EventId = EventId(35_002);

/// A mount was withdrawn: whose, the host, the share, the offer, and why —
/// its owner asked, its login session ended, or its connection was lost.
pub const UNMOUNTED: EventId = EventId(35_003);

/// A request was refused: whose, the host it named, and why.
pub const REQUEST_REFUSED: EventId = EventId(35_004);

/// A connection reached a server other than the one a remembered sign-in is
/// for, and no credential was offered it.
pub const PIN_MISMATCH: EventId = EventId(35_005);

/// A server refused the credentials a connection signed in with.
pub const AUTHENTICATION_REJECTED: EventId = EventId(35_006);

/// A connection ended for a reason of its own: the server, the network, or
/// its worker.
pub const CONNECTION_ENDED: EventId = EventId(35_007);

/// A request could not be read, and was answered as unreadable.
pub const REQUEST_UNREADABLE: EventId = EventId(35_008);

/// The kernel would not attest the caller of a received request, which was
/// refused.
pub const ORIGIN_UNREADABLE: EventId = EventId(35_009);

/// The resolver was refused a thread of its own, so lookups stall the
/// service while they run.
pub const RESOLVER_UNTHREADED: EventId = EventId(35_010);

/// Bytes written to a share, their write already answered, were not taken
/// by its server: the next flush is answered with the error.
pub const WRITE_BACK_LOST: EventId = EventId(35_011);

/// The memory-pressure wake was refused, so nothing is cached and every
/// write is sent before it is answered.
pub const CACHE_UNWATCHED: EventId = EventId(35_012);

/// A connection mounted shares depend on dropped: its mounts wait for it to
/// be made again, within the grace window.
pub const CONNECTION_LOST: EventId = EventId(35_013);

/// A dropped connection was made again, and its mounts serve again.
pub const RECONNECTED: EventId = EventId(35_014);

/// A dropped connection was not made again within its grace window: its
/// mounts fail every call until it is.
pub const MOUNTS_LOST: EventId = EventId(35_015);

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [EventId; 16] = [
        SERVICE_STARTED,
        SERVICE_UNAVAILABLE,
        MOUNTED,
        UNMOUNTED,
        REQUEST_REFUSED,
        PIN_MISMATCH,
        AUTHENTICATION_REJECTED,
        CONNECTION_ENDED,
        REQUEST_UNREADABLE,
        ORIGIN_UNREADABLE,
        WRITE_BACK_LOST,
        CACHE_UNWATCHED,
        RESOLVER_UNTHREADED,
        CONNECTION_LOST,
        RECONNECTED,
        MOUNTS_LOST,
    ];

    #[test]
    fn every_id_is_the_service_s_own_and_distinct() {
        for (at, id) in ALL.iter().enumerate() {
            assert!(tairix_log::registry::SMBFS.contains(*id), "{id:?}");
            assert!(ALL[at + 1..].iter().all(|other| other != id), "{id:?}");
        }
    }
}
