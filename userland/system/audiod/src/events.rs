//! Stable audit event ids the audio service records its security decisions
//! under (`plans/SOUND.md`, `plans/SYSLOG.md`).
//!
//! A refusal is as audit-worthy as a grant — "who tried" is the question an
//! incident asks — so the capture path records both. The recording indicator
//! the session draws is rendered from the same live state these records
//! describe, so what a user sees and what the log holds cannot disagree.

use tairix_log::EventId;

/// A driver's device channel was bound and its endpoints enumerated.
pub const DEVICE_BOUND: EventId = EventId(30_020);

/// A device channel could not be bound: its facts were unreadable, or it
/// presented no endpoint this service could serve.
pub const DEVICE_BIND_FAILED: EventId = EventId(30_021);

/// A device went away mid-stream. Its streams hold their positions and are
/// told; every other device is untouched.
pub const DEVICE_LOST: EventId = EventId(30_022);

/// The machine's default sink or source changed.
pub const DEFAULT_DEVICE_CHANGED: EventId = EventId(30_023);

/// A capture stream was opened, and by which principal.
pub const CAPTURE_OPENED: EventId = EventId(30_024);

/// A capture stream was refused, and to which principal. Recorded whether
/// the refusal was the missing capability, an unknown source, or a device
/// that could not be configured.
pub const CAPTURE_REFUSED: EventId = EventId(30_025);

/// A capture stream closed, so the live-capture set — and the indicator
/// drawn from it — shrank.
pub const CAPTURE_CLOSED: EventId = EventId(30_026);

/// The service claimed its `audio-v1` rendezvous and is serving.
pub const AUDIOD_READY: EventId = EventId(30_027);

/// A stream open the service refused, carrying what it refused with.
///
/// Capture refusals also raise [`CAPTURE_REFUSED`], which is the
/// security-relevant record; this one is the diagnosis, and covers playback
/// too — without it a machine whose sound does not work says nothing at all.
pub const STREAM_REFUSED: EventId = EventId(30_028);

/// The seat's lease moved, so the room the service mixes for did: which
/// room, and how many streams it now holds.
pub const ROOM_CHANGED: EventId = EventId(30_029);

/// A client broke its own ring's protocol — its positions were corrupt — so
/// that stream alone was stopped, and by whose stream.
pub const STREAM_FAULTED: EventId = EventId(30_030);

/// The seat's lease could not be read, so no stream plays, or the capture
/// count could not be published, so the recording indicator may lag.
pub const NOTICE_UNAVAILABLE: EventId = EventId(30_031);

/// A device control took effect: a default, a level or a mute, and whose it
/// was.
pub const CONTROL_CHANGED: EventId = EventId(30_032);

/// A device control was refused because the room is not the caller's, and
/// who tried.
pub const CONTROL_REFUSED: EventId = EventId(30_033);

/// The machine's baseline beneath every tenant's controls was adopted.
pub const BASELINE_ADOPTED: EventId = EventId(30_034);

/// A stream asked to play for the machine or another session, which only the
/// cue authority may, and only for a notification, was refused, and to whom.
pub const TENANCY_REFUSED: EventId = EventId(30_035);

/// An endpoint's facts changed under its streams — a sink replugged that
/// plays less — so its streams hold their positions as lost and the endpoint
/// is released for the next stream to configure afresh.
pub const ENDPOINT_CHANGED: EventId = EventId(30_036);
