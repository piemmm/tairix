//! The cue protocol (`cue-v1`, `plans/SOUND.md` §Default desktop sounds): a
//! program asks the cue authority for one desktop sound.
//!
//! A cue is an occurrence, not a state edge, so it is a request each
//! recipient witnesses rather than a notice: two devices arriving a second
//! apart are two sounds. The authority decides who may cue what from the
//! caller's kernel-attested origin, never from anything the request says;
//! the request names only the event and when it wants its answer.
//!
//! The session at the seat pushes its user's sound choices the same way
//! ([`CueRequest::Policy`]): a settings document naming no file, and each
//! chosen file beside it as a read-only `FD_GRANT` handle minted to the
//! authority's instance.
//!
//! Every decode fails closed: an unknown magic, version, operation, event or
//! answer, a dirty reserved byte, or a frame of any length but its own is
//! refused.

use crate::bounded_text::is_forbidden_character;
use crate::capability::CapabilityId;
use crate::le::{exact_len, put_u16, put_u32, put_u64, read_u16, read_u32, read_u64};
use crate::origin::{Origin, TrustDomain};
use crate::process::INIT_PID;
use crate::reply::{put_refusal, take_payload, STATUS_REPLY_LEN};
use crate::Errno;

/// Reserved well-known call-endpoint id of the cue authority (`"SD"`).
/// Binding it requires `CAP_IPC_BIND_PRIVILEGED`
/// ([`crate::ipc::is_reserved_endpoint`]): a squatter holding it would decide
/// which sounds every program makes.
pub const SOUND_CUE_ENDPOINT: u64 = 0x5344_1001;

/// The uid of the service account the cue authority runs as.
///
/// A system account compiled into the kernel's identity table and attested on
/// every IPC origin, so the audio service admits a cue's tenancy by it
/// ([`from_cue_authority`]) rather than by anything a request says.
pub const SOUNDD_UID: u32 = 21;

/// The uid of the login service's account, which owns a session's start and
/// end.
pub const LOGIN_UID: u32 = 13;

/// The uid of the device manager's account, the one principal that sees a
/// device both arrive and leave.
pub const DEVMGR_UID: u32 = 10;

/// Whether a request's kernel-attested `origin` is the cue authority.
#[must_use]
pub fn from_cue_authority(origin: &Origin) -> bool {
    origin.trust_domain() == TrustDomain::User && origin.uid() == SOUNDD_UID
}

/// Magic number identifying a cue request (`"CUE1"` little-endian).
pub const CUE_REQUEST_MAGIC: u32 = u32::from_le_bytes(*b"CUE1");

/// The `cue-v1` protocol version.
pub const CUE_VERSION_V1: u16 = 1;

/// Magic (4), version (2), operation (1), reserved (1).
const HEADER_LEN: usize = 8;

/// Operation discriminants.
mod op {
    pub const CUE: u8 = 1;
    pub const POLICY: u8 = 2;
}

/// The cue body: the event (1), the answer (1) and two reserved bytes.
const CUE_BODY_LEN: usize = 4;

/// Length of a [`CueRequest::Cue`] frame.
pub const CUE_REQUEST_LEN: usize = HEADER_LEN + CUE_BODY_LEN;

/// The events, which a policy carries one file slot for each of.
const EVENTS: usize = SoundEvent::ALL.len();

/// The policy's document length (2) and a reserved pair.
const POLICY_LEN_OFFSET: usize = HEADER_LEN + 8 * EVENTS;

/// Where a policy's document begins: after its file slots, its length and
/// the reserved pair.
const POLICY_DOCUMENT_OFFSET: usize = POLICY_LEN_OFFSET + 4;

/// The longest settings document a policy carries.
///
/// A validation bound, not a capacity: it holds the longest document a push
/// can carry — the master switch, the longest theme name at its longest
/// quoted spelling, the level, and every layered event's switch — with room
/// to spare (`lib/soundtheme`'s `the_longest_pushed_document_fits_the_policy`),
/// so a longer one is refused, never truncated.
pub const POLICY_DOCUMENT_MAX: usize = 2048;

/// The longest request the cue authority's rendezvous takes: a policy with
/// the longest document.
pub const CUE_REQUEST_MAX: usize = POLICY_DOCUMENT_OFFSET + POLICY_DOCUMENT_MAX;

/// One desktop sound a program may ask for.
///
/// Closed, so every consumer matching on it says what a new one means rather
/// than ignoring a name it does not know.
#[repr(u8)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum SoundEvent {
    /// The machine is ready for a user.
    Startup = 1,
    /// The machine is powering off.
    Shutdown = 2,
    /// A user signed in.
    Login = 3,
    /// A user signed out.
    Logout = 4,
    /// A device arrived.
    DeviceAttached = 5,
    /// A device left.
    DeviceDetached = 6,
    /// A notification.
    Notification = 7,
    /// A notification that is a message.
    NotificationMessage = 8,
    /// A notification that is a reminder.
    NotificationReminder = 9,
    /// Something failed.
    Error = 10,
    /// Something needs attention.
    Warning = 11,
    /// Something worth knowing.
    Info = 12,
    /// A terminal's bell.
    Bell = 13,
}

/// Who may cue an event.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum CueAuthority {
    /// Only the principal that owns the transition the sound announces, so
    /// no program can imitate the machine.
    Lifecycle,
    /// A principal whose session holds the room the sound plays in, as for
    /// any playback.
    Application,
}

impl SoundEvent {
    /// Every event, in discriminant order.
    pub const ALL: [Self; 13] = [
        Self::Startup,
        Self::Shutdown,
        Self::Login,
        Self::Logout,
        Self::DeviceAttached,
        Self::DeviceDetached,
        Self::Notification,
        Self::NotificationMessage,
        Self::NotificationReminder,
        Self::Error,
        Self::Warning,
        Self::Info,
        Self::Bell,
    ];

    /// Raw on-wire discriminant.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// The event's place in [`Self::ALL`], whose discriminants count up from
    /// one.
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize - 1
    }

    /// Inverse of [`Self::as_u8`].
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] for an undefined discriminant.
    pub const fn from_u8(raw: u8) -> Result<Self, Errno> {
        match raw {
            1 => Ok(Self::Startup),
            2 => Ok(Self::Shutdown),
            3 => Ok(Self::Login),
            4 => Ok(Self::Logout),
            5 => Ok(Self::DeviceAttached),
            6 => Ok(Self::DeviceDetached),
            7 => Ok(Self::Notification),
            8 => Ok(Self::NotificationMessage),
            9 => Ok(Self::NotificationReminder),
            10 => Ok(Self::Error),
            11 => Ok(Self::Warning),
            12 => Ok(Self::Info),
            13 => Ok(Self::Bell),
            _ => Err(Errno::OutOfRange),
        }
    }

    /// The event's stable name, as settings and listings spell it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Startup => "startup",
            Self::Shutdown => "shutdown",
            Self::Login => "login",
            Self::Logout => "logout",
            Self::DeviceAttached => "device_attached",
            Self::DeviceDetached => "device_detached",
            Self::Notification => "notification",
            Self::NotificationMessage => "notification_message",
            Self::NotificationReminder => "notification_reminder",
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Info => "info",
            Self::Bell => "bell",
        }
    }

    /// The event [`Self::name`] spells, if any.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|event| event.name() == name)
    }

    /// Whether `origin` is the principal that owns the transition this event
    /// announces, from its kernel-attested identity alone.
    ///
    /// Init, PID 1, which the kernel starts first and which never restarts,
    /// owns the machine's start; a holder of `CAP_SYSTEM_POWER` its end; the
    /// login service a session's; the device manager a device's. An
    /// application event announces no transition, so nothing owns it.
    #[must_use]
    pub fn owned_by(self, origin: &Origin) -> bool {
        if origin.trust_domain() != TrustDomain::User {
            return false;
        }
        match self {
            Self::Startup => origin.pid() == INIT_PID,
            Self::Shutdown => origin.capabilities().holds_cap(CapabilityId::SYSTEM_POWER),
            Self::Login | Self::Logout => origin.uid() == LOGIN_UID,
            Self::DeviceAttached | Self::DeviceDetached => origin.uid() == DEVMGR_UID,
            Self::Notification
            | Self::NotificationMessage
            | Self::NotificationReminder
            | Self::Error
            | Self::Warning
            | Self::Info
            | Self::Bell => false,
        }
    }

    /// Who may cue it.
    #[must_use]
    pub const fn authority(self) -> CueAuthority {
        match self {
            Self::Startup
            | Self::Shutdown
            | Self::Login
            | Self::Logout
            | Self::DeviceAttached
            | Self::DeviceDetached => CueAuthority::Lifecycle,
            Self::Notification
            | Self::NotificationMessage
            | Self::NotificationReminder
            | Self::Error
            | Self::Warning
            | Self::Info
            | Self::Bell => CueAuthority::Application,
        }
    }
}

/// When the cue authority answers a cue.
#[repr(u8)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Answer {
    /// Once the sound starts, or is decided against.
    Started = 0,
    /// Once the sound has played out, or its drain deadline has passed: for a
    /// principal about to take away the machine it plays on.
    PlayedOut = 1,
}

impl Answer {
    /// Inverse of the discriminant.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] for an undefined discriminant.
    pub const fn from_u8(raw: u8) -> Result<Self, Errno> {
        match raw {
            0 => Ok(Self::Started),
            1 => Ok(Self::PlayedOut),
            _ => Err(Errno::OutOfRange),
        }
    }
}

/// A request to the cue authority.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum CueRequest<'a> {
    /// Sound `event` once, in the caller's room.
    Cue {
        /// The occurrence.
        event: SoundEvent,
        /// When to be answered.
        answer: Answer,
    },
    /// Govern the seat's sounds by these choices while the caller's session
    /// holds it.
    Policy {
        /// The settings document, naming no file: at most
        /// [`POLICY_DOCUMENT_MAX`] bytes of UTF-8 with no control character
        /// but `'\n'`, possibly empty.
        document: &'a str,
        /// Each event's own file, in [`SoundEvent::ALL`] order: a read-only
        /// `FD_GRANT` handle minted to the authority's instance, or `0` for
        /// none. Slots may share a handle, since identical delegations are one
        /// handle; each is redeemed once.
        files: [u64; EVENTS],
    },
}

impl CueRequest<'_> {
    /// Write the request's frame to `out`, answering its length.
    ///
    /// # Errors
    ///
    /// [`Errno::LengthOutOfRange`] for an `out` shorter than the frame or a
    /// document past [`POLICY_DOCUMENT_MAX`], and [`Errno::OutOfRange`] for a
    /// document holding a control character: what the decode would refuse is
    /// never written.
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, Errno> {
        let len = match self {
            Self::Cue { .. } => CUE_REQUEST_LEN,
            Self::Policy { document, .. } => {
                validate_document(document)?;
                POLICY_DOCUMENT_OFFSET + document.len()
            }
        };
        let out = out.get_mut(..len).ok_or(Errno::LengthOutOfRange)?;
        out.fill(0);
        put_u32(out, 0, CUE_REQUEST_MAGIC);
        put_u16(out, 4, CUE_VERSION_V1);
        match self {
            Self::Cue { event, answer } => {
                out[6] = op::CUE;
                out[HEADER_LEN] = event.as_u8();
                out[HEADER_LEN + 1] = *answer as u8;
            }
            Self::Policy { document, files } => {
                out[6] = op::POLICY;
                for (at, handle) in files.iter().enumerate() {
                    put_u64(out, HEADER_LEN + 8 * at, *handle);
                }
                let len = u16::try_from(document.len()).map_err(|_| Errno::LengthOutOfRange)?;
                put_u16(out, POLICY_LEN_OFFSET, len);
                out[POLICY_DOCUMENT_OFFSET..].copy_from_slice(document.as_bytes());
            }
        }
        Ok(len)
    }
}

impl<'a> CueRequest<'a> {
    /// Decode a request frame.
    ///
    /// # Errors
    ///
    /// * [`Errno::BufferTooSmall`] — shorter than its operation's frame.
    /// * [`Errno::LengthOutOfRange`] — longer than its operation's frame, or
    ///   a document past [`POLICY_DOCUMENT_MAX`].
    /// * [`Errno::BadMagic`] — wrong magic or a dirty reserved byte.
    /// * [`Errno::AbiVersionUnsupported`] — not [`CUE_VERSION_V1`].
    /// * [`Errno::OutOfRange`] — an unknown operation, event or answer, or a
    ///   document that is not UTF-8 or holds a control character.
    pub fn from_bytes(bytes: &'a [u8]) -> Result<Self, Errno> {
        match operation(bytes)? {
            op::CUE => {
                exact_len(bytes, CUE_REQUEST_LEN)?;
                if bytes[HEADER_LEN + 2..].iter().any(|&byte| byte != 0) {
                    return Err(Errno::BadMagic);
                }
                Ok(Self::Cue {
                    event: SoundEvent::from_u8(bytes[HEADER_LEN])?,
                    answer: Answer::from_u8(bytes[HEADER_LEN + 1])?,
                })
            }
            op::POLICY => {
                if bytes.len() < POLICY_DOCUMENT_OFFSET {
                    return Err(Errno::BufferTooSmall);
                }
                if read_u16(bytes, POLICY_LEN_OFFSET + 2) != 0 {
                    return Err(Errno::BadMagic);
                }
                let len = usize::from(read_u16(bytes, POLICY_LEN_OFFSET));
                exact_len(bytes, POLICY_DOCUMENT_OFFSET + len)?;
                let document = core::str::from_utf8(&bytes[POLICY_DOCUMENT_OFFSET..])
                    .map_err(|_| Errno::OutOfRange)?;
                validate_document(document)?;
                Ok(Self::Policy {
                    document,
                    files: core::array::from_fn(|at| read_u64(bytes, HEADER_LEN + 8 * at)),
                })
            }
            _ => Err(Errno::OutOfRange),
        }
    }
}

/// The operation a request frame's header names.
fn operation(bytes: &[u8]) -> Result<u8, Errno> {
    if bytes.len() < HEADER_LEN {
        return Err(Errno::BufferTooSmall);
    }
    if read_u32(bytes, 0) != CUE_REQUEST_MAGIC {
        return Err(Errno::BadMagic);
    }
    if read_u16(bytes, 4) != CUE_VERSION_V1 {
        return Err(Errno::AbiVersionUnsupported);
    }
    if bytes[7] != 0 {
        return Err(Errno::BadMagic);
    }
    Ok(bytes[6])
}

/// Whether `request`'s header names [`CueRequest::Policy`], whatever the rest
/// of it holds: a request is answered in its operation's shape, refused or
/// not.
#[must_use]
pub fn names_policy(request: &[u8]) -> bool {
    operation(request) == Ok(op::POLICY)
}

/// Refuse a document a policy cannot carry.
fn validate_document(document: &str) -> Result<(), Errno> {
    if document.len() > POLICY_DOCUMENT_MAX {
        return Err(Errno::LengthOutOfRange);
    }
    if document.chars().any(|c| is_forbidden_character(c, true)) {
        return Err(Errno::OutOfRange);
    }
    Ok(())
}

/// Why a cue made no sound.
#[repr(u8)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Silence {
    /// The event, or every sound, is turned off.
    Disabled = 1,
    /// No theme has a sound for it.
    NoAsset = 2,
    /// Its sound could not be decoded.
    Undecodable = 3,
    /// The caller cued the same event moments ago.
    Coalesced = 4,
    /// As many cues as may play at once are playing.
    TooMany = 5,
    /// The room has no output.
    NoDevice = 6,
}

impl Silence {
    /// Raw on-wire discriminant.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// Inverse of [`Self::as_u8`].
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] for an undefined discriminant.
    pub const fn from_u8(raw: u8) -> Result<Self, Errno> {
        match raw {
            1 => Ok(Self::Disabled),
            2 => Ok(Self::NoAsset),
            3 => Ok(Self::Undecodable),
            4 => Ok(Self::Coalesced),
            5 => Ok(Self::TooMany),
            6 => Ok(Self::NoDevice),
            _ => Err(Errno::OutOfRange),
        }
    }
}

/// What became of a cue the authority admitted.
///
/// Silence is an answer, not a failure: a machine told to stay quiet stays
/// quiet, and the caller learns why.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum CueOutcome {
    /// The sound is playing, or has played out: whichever the request asked
    /// to be answered at.
    Played,
    /// The machine made no sound.
    Silent(Silence),
}

/// Outcome discriminants.
const PLAYED: u8 = 1;
const SILENT: u8 = 2;

/// Length of a cue reply: the status word, the outcome (1), its silence (1)
/// and two reserved bytes.
pub const CUE_REPLY_LEN: usize = STATUS_REPLY_LEN + 4;

/// Encode the answer to a cue.
#[must_use]
pub fn encode_cue_reply(result: Result<CueOutcome, Errno>) -> [u8; CUE_REPLY_LEN] {
    let mut out = [0u8; CUE_REPLY_LEN];
    match result {
        Ok(CueOutcome::Played) => out[STATUS_REPLY_LEN] = PLAYED,
        Ok(CueOutcome::Silent(silence)) => {
            out[STATUS_REPLY_LEN] = SILENT;
            out[STATUS_REPLY_LEN + 1] = silence.as_u8();
        }
        Err(err) => put_refusal(&mut out, err),
    }
    out
}

/// Decode the answer to a cue.
///
/// # Errors
///
/// * The authority's refusal: [`Errno::PermissionDenied`] for a lifecycle
///   event from anyone but its owner, [`Errno::SeatNotOwner`] for an
///   application event from outside the room, [`Errno::LimitExceeded`] past
///   the caller's cue rate.
/// * [`Errno::BufferTooSmall`] / [`Errno::LengthOutOfRange`] — not exactly
///   [`CUE_REPLY_LEN`] bytes.
/// * [`Errno::BadMagic`] — a dirty reserved byte, or a silence beside a
///   played cue.
/// * [`Errno::OutOfRange`] — an undefined outcome or silence.
pub fn decode_cue_reply(bytes: &[u8]) -> Result<CueOutcome, Errno> {
    exact_len(bytes, CUE_REPLY_LEN)?;
    let payload = take_payload(bytes, CUE_REPLY_LEN)?;
    if payload[2..].iter().any(|&byte| byte != 0) {
        return Err(Errno::BadMagic);
    }
    match payload[0] {
        PLAYED if payload[1] == 0 => Ok(CueOutcome::Played),
        PLAYED => Err(Errno::BadMagic),
        SILENT => Ok(CueOutcome::Silent(Silence::from_u8(payload[1])?)),
        _ => Err(Errno::OutOfRange),
    }
}

#[cfg(test)]
#[path = "sound_cue_tests.rs"]
mod tests;
