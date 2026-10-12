//! Stable audit event ids the cue authority records its decisions under
//! (`plans/SYSLOG.md`).

use tairix_log::EventId;
use tairix_soundtheme::SoundEvent;

/// The service bound its rendezvous and is serving.
pub const SOUNDD_READY: EventId = EventId(22_000);

/// A lifecycle sound was asked for by a principal that does not own the
/// transition it announces — an attempt to imitate the machine — and who.
pub const CUE_REFUSED: EventId = EventId(22_001);

/// A principal went past its cue rate, or found no record free: recorded
/// once a flood, and who.
pub const CUE_LIMITED: EventId = EventId(22_002);

/// A theme's sound could not be read or decoded, so its cues are silent.
pub const CUE_UNDECODABLE: EventId = EventId(22_003);

/// Neither the active theme nor the shipped one holds a sound: an install
/// missing part of its theme.
pub const CUE_NO_ASSET: EventId = EventId(22_004);

/// The machine's sound document held a setting it refuses, which stays at
/// its default, or could not be read, so the last one read stays in force.
pub const SETTINGS_REFUSED: EventId = EventId(22_005);

/// A sound did not play out by its deadline and was stopped.
pub const CUE_CUT: EventId = EventId(22_006);

/// The service could not start serving: the reason is the message.
pub const SERVICE_UNAVAILABLE: EventId = EventId(22_007);

/// A machine's sound — a lifecycle event's, played for the machine — played
/// out: the record of the machine having announced a transition aloud.
pub const MACHINE_CUE_PLAYED: EventId = EventId(22_008);

/// The session at the seat handed over its user's sound choices, which now
/// govern the seat's sounds.
pub const POLICY_ADOPTED: EventId = EventId(22_009);

/// A push of sound choices was refused: from anyone but the anchor of the
/// login session at the seat, a document naming a file or a setting no user's
/// choices reach, or a handle that would not redeem.
pub const POLICY_REFUSED: EventId = EventId(22_010);

/// A session's sound choices stopped governing the seat's sounds: it left the
/// seat, or they no longer read over the machine's document.
pub const POLICY_DROPPED: EventId = EventId(22_011);

/// The message [`MACHINE_CUE_PLAYED`] is recorded with for `event`'s sound,
/// naming it, so a reader of the log need not parse the record's fields.
#[must_use]
pub const fn played_out_message(event: SoundEvent) -> &'static str {
    match event {
        SoundEvent::Startup => "soundd: the startup sound played out",
        SoundEvent::Shutdown => "soundd: the shutdown sound played out",
        SoundEvent::Login => "soundd: the login sound played out",
        SoundEvent::Logout => "soundd: the logout sound played out",
        SoundEvent::DeviceAttached => "soundd: a device's arrival sound played out",
        SoundEvent::DeviceDetached => "soundd: a device's departure sound played out",
        SoundEvent::Notification
        | SoundEvent::NotificationMessage
        | SoundEvent::NotificationReminder
        | SoundEvent::Error
        | SoundEvent::Warning
        | SoundEvent::Info
        | SoundEvent::Bell => "soundd: a machine sound played out",
    }
}
