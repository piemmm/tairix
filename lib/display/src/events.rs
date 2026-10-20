//! The display service's event identifiers: what a log reader, or a test
//! harness reading the log, keys on.

use tairix_log::EventId;

/// Range start (inclusive) reserved for the display service's event
/// identifiers, per the `lib/log` convention of one 1 000-wide range per
/// subsystem. Once shipped the values are never re-used or re-numbered.
pub const DISPLAY_SERVICE_RANGE_START: u32 = tairix_log::registry::DISPLAY_SERVICE.start();

/// Range end (exclusive) reserved for display-service event identifiers.
pub const DISPLAY_SERVICE_RANGE_END: u32 = tairix_log::registry::DISPLAY_SERVICE.end();

/// One-shot: the first client frame the display took since this service
/// started, emitted only once a present was accepted — the witness that the
/// session → service → surface path is live end to end. Emitted after the
/// reply, so the present path pays nothing.
pub const FIRST_PRESENT: EventId = EventId(15_001);

/// The exact message [`FIRST_PRESENT`] is emitted with. A log consumer keys
/// on this rendered text, so it is defined once beside the id.
pub const FIRST_PRESENT_MESSAGE: &str = "first client frame presented to scan-out";

/// The display's output moved, to the mode its `width` and `height` fields
/// name, or to none when both are `0`.
pub const OUTPUT_MOVED: EventId = EventId(15_002);

/// The kernel refused an announcement of the display's output, so clients
/// learn of the move only from the refusals of their stale frames.
pub const OUTPUT_UNANNOUNCED: EventId = EventId(15_003);

/// The display could not read what woke its event sources; its `error`
/// field is the driver's refusal.
pub const EVENTS_FAILED: EventId = EventId(15_004);

/// A queued present never latched, so its presenter was released at the
/// deadline.
pub const LATCH_OVERDUE: EventId = EventId(15_005);

/// A latch could not be posted to its presenter's port; its `error` field is
/// the kernel's refusal.
pub const LATCH_UNDELIVERED: EventId = EventId(15_006);

const _: () = {
    let mut index = 0;
    let ids = [
        FIRST_PRESENT,
        OUTPUT_MOVED,
        OUTPUT_UNANNOUNCED,
        EVENTS_FAILED,
        LATCH_OVERDUE,
        LATCH_UNDELIVERED,
    ];
    while index < ids.len() {
        assert!(
            ids[index].0 >= DISPLAY_SERVICE_RANGE_START && ids[index].0 < DISPLAY_SERVICE_RANGE_END,
            "a display-service event lies in the display service's range"
        );
        index += 1;
    }
};
