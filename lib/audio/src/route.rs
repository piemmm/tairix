//! The routing policy: a pure function from (role, whose room the device
//! serves, which sink was asked for) to what happens to a stream.
//!
//! This is where "policy, not a per-application configuration file" is cashed
//! in. A program says what its sound is *for* and nothing else; which sink it
//! lands on, whether it survives a user switch, and whether it ducks are all
//! decided here, from state, by one function with no I/O and no process
//! names in it. That is what makes the behaviour testable over the whole
//! cross-product rather than discoverable by experiment.
//!
//! # The seat owns the sound exactly as it owns the screen
//!
//! A seat's speakers and microphones serve the room its display lease
//! describes. A stream whose login session holds that lease is mixed; any
//! other is **held at a frame boundary and told so**, keeping its position so
//! a switch back resumes on the frame it stopped on. A departing user's music
//! does not play into the arriving user's room, their recorder does not hear
//! it, and neither silently vanishes. While the seat changes hands, or is held
//! by a presenter no login encloses — the login screen — the room is
//! nobody's.
//!
//! A notification is the exception, and its role is what says so: one that
//! arrives ten minutes late is noise, so it is dropped rather than queued.
//!
//! # An unclaimed room is anybody's
//!
//! A seat no presenter holds — a server playing an alert, a machine at its
//! text console — leaves its room unclaimed, and a stream there simply plays.
//! A graphical session claims the seat, which is what stops a remote login
//! making noise in somebody's room.

use tairix_abi::audio::StreamRole;
use tairix_abi::driver::audio::StreamDirection;
use tairix_abi::seat::{DisplayLease, ReleaseSurface};
use tairix_abi::{Errno, ProcId};

use crate::volume::DUCK_MILLIBEL;

/// Whose room a seat's devices serve.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Room {
    /// No presenter holds the seat, so the room is anybody's.
    Unclaimed,
    /// A process this login session encloses holds the seat.
    Session(ProcId),
    /// The seat is changing hands, or is held by a presenter no login
    /// encloses, so the room is nobody's.
    Withheld,
}

impl From<DisplayLease> for Room {
    fn from(lease: DisplayLease) -> Self {
        if lease.live_generation().is_some() {
            return lease.session().map_or(Self::Withheld, Self::Session);
        }
        match lease.released_to() {
            Some(ReleaseSurface::Handover) => Self::Withheld,
            _ => Self::Unclaimed,
        }
    }
}

/// What a room does with a stream.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Admission {
    /// Move its frames.
    Mix,
    /// Hold it at its frame position, told why.
    Hold,
    /// Hold it, and discard what it queued: a notification from outside the
    /// room, whose lateness would make it noise.
    Drop,
}

/// What `room` does with a `direction` stream of `role` whose owner lies
/// within `session` ([`None`] for a principal no login encloses).
#[must_use]
pub fn admit(
    direction: StreamDirection,
    role: StreamRole,
    session: Option<ProcId>,
    room: Room,
) -> Admission {
    match room {
        Room::Unclaimed => Admission::Mix,
        Room::Session(holder) if session == Some(holder) => Admission::Mix,
        // Captured audio is the owner's to keep, late or not.
        _ if direction == StreamDirection::Playback && role == StreamRole::Notification => {
            Admission::Drop
        }
        _ => Admission::Hold,
    }
}

/// One sink, as the policy sees it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct SinkState {
    /// The service-assigned identity a stream is opened against.
    pub device_id: u32,
    /// Whether the machine's configured default for playback.
    pub is_default: bool,
    /// Whose room the sink plays into.
    pub room: Room,
}

/// One stream, as the policy sees it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct StreamRequest {
    /// What the sound is for.
    pub role: StreamRole,
    /// The sink the client named, or [`None`] for the machine default.
    pub requested_device: Option<u32>,
    /// The login session the stream's owner lies within, or [`None`] for a
    /// principal no login encloses — a daemon.
    pub session: Option<ProcId>,
}

/// What the policy decided.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Routing {
    /// Mix the stream into this sink.
    Play {
        /// The sink it lands on.
        device_id: u32,
    },
    /// Hold the stream at its frame position and tell it why: the sink's room
    /// is not its session's.
    Pause {
        /// The sink it would land on when its session is next active.
        device_id: u32,
    },
    /// Discard the stream's frames. Only a notification from an inactive
    /// session, whose lateness would make it noise.
    Drop,
    /// Refuse the stream outright.
    Refuse(
        /// Why.
        Errno,
    ),
}

/// Decide what happens to `request` given the sinks the machine has.
///
/// Fails closed: a named sink that does not exist, and a machine with no
/// configured default, are refusals rather than a sink picked arbitrarily.
#[must_use]
pub fn route(request: &StreamRequest, sinks: &[SinkState]) -> Routing {
    let Some(sink) = choose(request.requested_device, sinks) else {
        return Routing::Refuse(match request.requested_device {
            // A named device that is not there is a different answer from a
            // machine that has not been told which sink to use.
            Some(_) => Errno::NotFound,
            None => Errno::DeviceOffline,
        });
    };
    let device_id = sink.device_id;
    match admit(
        StreamDirection::Playback,
        request.role,
        request.session,
        sink.room,
    ) {
        Admission::Mix => Routing::Play { device_id },
        Admission::Hold => Routing::Pause { device_id },
        Admission::Drop => Routing::Drop,
    }
}

/// The sink a request lands on: the one it named, else the configured
/// default.
fn choose(requested: Option<u32>, sinks: &[SinkState]) -> Option<&SinkState> {
    match requested {
        Some(device_id) => sinks.iter().find(|sink| sink.device_id == device_id),
        None => sinks.iter().find(|sink| sink.is_default),
    }
}

/// The roles live on one sink: all the ducking rule reads.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Roles(u8);

impl Roles {
    /// Count `role` among them.
    pub fn insert(&mut self, role: StreamRole) {
        self.0 |= Self::bit(role);
    }

    /// Whether `role` is among them.
    #[must_use]
    pub const fn contains(self, role: StreamRole) -> bool {
        self.0 & Self::bit(role) != 0
    }

    const fn bit(role: StreamRole) -> u8 {
        match role {
            StreamRole::Media => 1,
            StreamRole::Communication => 2,
            StreamRole::Notification => 4,
            StreamRole::Accessibility => 8,
        }
    }
}

impl FromIterator<StreamRole> for Roles {
    fn from_iter<I: IntoIterator<Item = StreamRole>>(roles: I) -> Self {
        let mut set = Self::default();
        for role in roles {
            set.insert(role);
        }
        set
    }
}

/// The attenuation `role` takes while `live` are the roles on the same sink.
///
/// Media steps aside for speech — a conversation or assistive output — and
/// nothing else ducks: a notification is short enough that attenuating the
/// music under it would be more noticeable than the notification, and two
/// conversations on one sink are the user's own doing.
#[must_use]
pub const fn duck_millibel(role: StreamRole, live: Roles) -> i32 {
    let speech =
        live.contains(StreamRole::Communication) || live.contains(StreamRole::Accessibility);
    if matches!(role, StreamRole::Media) && speech {
        DUCK_MILLIBEL
    } else {
        0
    }
}

#[cfg(test)]
#[path = "route_tests.rs"]
mod tests;
