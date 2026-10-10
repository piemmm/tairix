//! `audio-v1`: the client stream ABI — the one surface a program plays or
//! records sound through (`plans/SOUND.md`).
//!
//! There is exactly one transport and no bypass. A program enumerates the
//! sinks and sources it may see, opens a stream, is told the latency it was
//! granted, shares a PCM ring, and writes frames at exact positions. There is
//! no exclusive mode, no raw device node, and no second client API — the
//! single path is low-latency enough that nothing wants to go around it.
//!
//! # What this protocol deliberately does not have
//!
//! **No period or buffer size.** Those are the device's ring geometry, and
//! every system that leaks them into its client API makes every program
//! re-derive latency from numbers it should never have seen. A client states a
//! latency *target* and is told the latency it was *granted*, in frames and in
//! [`Duration64`], so it never needs to know a sample rate to reason about
//! time.
//!
//! **No mix format.** A stream runs at the rate, in the encoding and with the
//! channel layout it asked for, and the one mixer converts to whatever the
//! device runs — through the system's one resampler — so no program carries
//! a converter of its own. A single stream at unity gain whose rate and format
//! the device already runs reaches the hardware unaltered. Bit-exactness is a
//! property of the one path, not a mode beside it.
//!
//! # Positions, not offsets
//!
//! Every position is a monotone [`Frames`] count, so `Start` at a frame,
//! gapless playback, and A/V sync are exact arithmetic. The clock is
//! *exported*: [`ClockReport`] hands back the device's own (position, time)
//! pair and its measured rate, so a program that must line sound up with
//! anything else is doing arithmetic rather than guessing.
//!
//! # Authority
//!
//! Playback needs no capability: the authorisation is that the caller's
//! session holds the seat lease on the sink, checked at open against the
//! kernel-attested caller. Opening a *source* additionally demands the capture
//! capability, and every live capture stream is machine state the session
//! draws an indicator from — a recording program cannot suppress it
//! (`plans/SOUND.md`). A stream id is a service-issued token, and the service
//! checks it against the kernel-attested caller, so a guessed id cannot reach
//! another principal's stream.
//!
//! A device's controls — which sink or source is the default, an endpoint's
//! level and mute — belong to the room it serves, as its sound does: the
//! service admits a change from a caller whose session holds the room, from
//! anyone while the room is unclaimed, and from nobody while it is withheld.
//! Persistent configuration names an endpoint by its [`AudioLocation`], never
//! by the id it carries for one boot.
//!
//! # Fail closed
//!
//! Every decode is total: an unknown magic, version, operation or role, a
//! dirty reserved field, an out-of-range rate or latency, or a notify port
//! naming a reserved rendezvous refuses with one typed [`Errno`].

use crate::appinfo::BundleId;
use crate::driver::audio::{
    ring_bounds, AudioName, ChannelMap, Frames, GainRange, JackState, Rate, RateSupport,
    SampleFormat, SampleFormats, StreamDirection, AUDIO_NAME_MAX, CHANNEL_MAP_WIRE_LEN,
    GAIN_RANGE_WIRE_LEN, MAX_DEVICE_ENDPOINTS, RATE_SUPPORT_WIRE_LEN,
};
use crate::le::{put_i32, put_u16, put_u32, put_u64, read_i32, read_u16, read_u32, read_u64};
use crate::time::{Duration64, Time64};
use crate::Errno;

/// Reserved well-known call-endpoint id of the audio service (`"AU"`
/// hex-spelled prefix, the convention every service rendezvous follows).
/// Binding it requires `CAP_IPC_BIND_PRIVILEGED`
/// ([`crate::ipc::is_reserved_endpoint`]): a squatter claiming the rendezvous
/// first would receive every program's samples and learn their shared-memory
/// grants.
pub const AUDIO_ENDPOINT: u64 = 0x4155_1001;

/// Magic number identifying an audio-service request (`"AUD1"`).
pub const AUDIO_REQUEST_MAGIC: u32 = u32::from_le_bytes(*b"AUD1");

/// Magic number identifying an audio-service notification (`"AUDN"`).
pub const AUDIO_NOTIFY_MAGIC: u32 = u32::from_le_bytes(*b"AUDN");

/// The `audio-v1` protocol version.
pub const AUDIO_VERSION_V1: u16 = 1;

/// High tag of a client-owned stream notify-port id (see
/// [`notify_endpoint_for`]).
const AUDIO_CLIENT_NOTIFY_TAG: u64 = 0x4155_0000_0000_0000;

/// Streams one process may hold open at once.
///
/// The notify-port id packs the stream's slot into a byte beside the whole of
/// the pid, so the packing itself bounds it. A fixed containment bound, not a
/// capacity: what actually bounds a principal's streams is the resource limit
/// the service admits against, and a process wanting two hundred and
/// fifty-six simultaneous audio streams is not a use, it is an attack.
pub const MAX_CLIENT_STREAM_SLOTS: u64 = 256;

/// The notify-mailbox endpoint id a client binds for one stream, and the
/// service `ipc_send`s that stream's [`AudioNotify`] to.
///
/// The **service** derives it from the caller's kernel-attested pid and hands
/// it back in the [`StreamGrant`]; the client never names it in a request. A
/// client that could name its own notify port could name somebody *else's*
/// mailbox instead and use the audio service as a proxy to spam it, and no
/// check the service could make would tell the two apart. Deriving it from an
/// identity the caller cannot forge removes the question.
///
/// The id is deliberately **not** reserved, so the client `port_bind`s it
/// without a privileged bind, and the mailbox is owner-only to receive, so a
/// bystander cannot steal the wakes. `slot` occupies the low byte and `pid`
/// the next 40 bits — the whole of [`crate::PID_MAX`] — so the three fields
/// tile the word exactly and no pid can reach the tag.
#[must_use]
pub const fn notify_endpoint_for(pid: u64, slot: u64) -> u64 {
    AUDIO_CLIENT_NOTIFY_TAG | ((pid & crate::PID_MAX) << 8) | (slot % MAX_CLIENT_STREAM_SLOTS)
}

/// What a stream is *for*, which is what lets routing be a policy rather than
/// a per-application configuration file.
///
/// The one input a program gives the router beyond its format: whether the
/// sound is media, a conversation, a notification, or accessibility output.
/// Routing, ducking, and what survives a seat switch are decided from this by
/// one policy function, never from a list of process names.
#[repr(u8)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum StreamRole {
    /// Music, video, games — anything a user chose to play.
    Media = 0,
    /// A live conversation, which ducks media and survives where media does
    /// not.
    Communication = 1,
    /// A short alert. A notification from a session that does not hold the
    /// seat is dropped rather than queued: one that arrives ten minutes late
    /// is noise.
    Notification = 2,
    /// A screen reader or other assistive output, which outranks media.
    Accessibility = 3,
}

impl StreamRole {
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
            0 => Ok(Self::Media),
            1 => Ok(Self::Communication),
            2 => Ok(Self::Notification),
            3 => Ok(Self::Accessibility),
            _ => Err(Errno::OutOfRange),
        }
    }

    /// The role's stable name, as a listing spells it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Media => "media",
            Self::Communication => "communication",
            Self::Notification => "notification",
            Self::Accessibility => "accessibility",
        }
    }
}

/// Where a stream stands.
#[repr(u8)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum StreamState {
    /// Opened, not yet started.
    Idle = 0,
    /// Frames are moving.
    Running = 1,
    /// Stopped at a frame boundary, holding its position.
    Paused = 2,
    /// Playing out what is queued, then stopping.
    Draining = 3,
    /// The session does not hold the sink's seat lease, so the stream is
    /// paused at a frame boundary and *told so*. A departing user's music does
    /// not play into the arriving user's room, and it does not silently vanish
    /// either: on switch-back it resumes from the exact frame.
    SeatInactive = 4,
    /// The device went away. The position is intact and the reason is stated;
    /// other devices are untouched.
    DeviceLost = 5,
    /// The stream's own ring broke the protocol — its positions were corrupt —
    /// so the service stopped reading it. Nothing else on the device is
    /// touched; the stream moves no frame again and its owner closes it.
    Faulted = 6,
}

impl StreamState {
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
            0 => Ok(Self::Idle),
            1 => Ok(Self::Running),
            2 => Ok(Self::Paused),
            3 => Ok(Self::Draining),
            4 => Ok(Self::SeatInactive),
            5 => Ok(Self::DeviceLost),
            6 => Ok(Self::Faulted),
            _ => Err(Errno::OutOfRange),
        }
    }

    /// The state's stable name, as a listing spells it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Draining => "draining",
            Self::SeatInactive => "seat-inactive",
            Self::DeviceLost => "device-lost",
            Self::Faulted => "faulted",
        }
    }
}

/// Fixed request header: magic (4) + version (2) + op (1) + reserved (1).
const HEADER_LEN: usize = 8;

/// Operation discriminants (the request's seventh byte).
mod op {
    pub const ENUMERATE: u8 = 1;
    pub const OPEN: u8 = 2;
    pub const ATTACH: u8 = 3;
    pub const START: u8 = 4;
    pub const STOP: u8 = 5;
    pub const DRAIN: u8 = 6;
    pub const FLUSH: u8 = 7;
    pub const CLOCK: u8 = 8;
    pub const GAIN: u8 = 9;
    pub const MUTE: u8 = 10;
    pub const STATE: u8 = 11;
    pub const CLOSE: u8 = 12;
    pub const BIND_DRIVER: u8 = 13;
    pub const SET_DEFAULT: u8 = 14;
    pub const SET_LEVEL: u8 = 15;
    pub const SET_MUTE: u8 = 16;
    pub const LIST_STREAMS: u8 = 17;
    pub const BASELINE: u8 = 18;
    pub const UNBIND_DRIVER: u8 = 19;
}

/// Byte offsets within the [`AudioRequest::Enumerate`] body.
mod enumerate {
    pub const DIRECTION: usize = 0;
    pub const RESERVED: usize = 1;
    pub const AFTER: usize = 4;
    pub const LEN: usize = 8;
}

/// Byte offsets within the [`AudioRequest::Open`] body.
mod open {
    use super::CHANNEL_MAP_WIRE_LEN;

    pub const DEVICE: usize = 0;
    pub const DIRECTION: usize = 4;
    pub const FORMAT: usize = 5;
    pub const ROLE: usize = 6;
    pub const RESERVED0: usize = 7;
    pub const RATE: usize = 8;
    pub const LATENCY: usize = 12;
    pub const CHANNEL_MAP: usize = 16;
    pub const RESERVED1: usize = CHANNEL_MAP + CHANNEL_MAP_WIRE_LEN;
    pub const LEN: usize = RESERVED1 + 3;
}

/// Byte offsets within the [`AudioRequest::Attach`] body.
mod attach {
    pub const STREAM: usize = 0;
    pub const GRANT: usize = 8;
    pub const LEN: usize = 16;
}

/// Byte offsets within the transport-control (`Start` / `Stop`) body.
mod transport {
    pub const STREAM: usize = 0;
    pub const AT: usize = 8;
    pub const LEN: usize = 16;
}

/// Byte offsets within the [`AudioRequest::Gain`] / [`AudioRequest::Mute`]
/// body.
mod level {
    pub const STREAM: usize = 0;
    pub const MILLIBEL: usize = 8;
    pub const MUTED: usize = 8;
    pub const RESERVED: usize = 12;
    pub const LEN: usize = 16;
}

/// Byte offsets within the [`AudioRequest::BindDriver`] body.
mod bind {
    pub const ENDPOINT: usize = 0;
    pub const LOCATION: usize = 8;
    pub const LEN: usize = 16;
}

/// Byte offsets within a device-control body: `SetDefault`, whose value bytes
/// are reserved, `SetLevel` and `SetMute`.
mod control {
    pub const DEVICE: usize = 0;
    pub const VALUE: usize = 4;
    pub const LEN: usize = 8;
}

/// Byte offsets within the [`AudioRequest::Baseline`] body.
mod baseline {
    use super::LOCATION_WIRE_LEN;

    pub const OUTPUT: usize = 0;
    pub const INPUT: usize = LOCATION_WIRE_LEN;
    pub const LEVEL: usize = 2 * LOCATION_WIRE_LEN;
    pub const RESERVED: usize = LEVEL + 4;
    pub const LEN: usize = RESERVED + 4;
}

/// Wire length of a body naming nothing but one 64-bit value: a stream, a
/// driver's device-channel endpoint, or where a listing continues.
const STREAM_BODY_LEN: usize = 8;

/// Largest audio-service request frame: the header plus the widest body. A
/// fixed validation bound sizing the buffer both sides pin for the endpoint.
pub const AUDIO_MAX_REQUEST: usize = HEADER_LEN + largest_body();

const fn largest_body() -> usize {
    let mut largest = STREAM_BODY_LEN;
    if enumerate::LEN > largest {
        largest = enumerate::LEN;
    }
    if open::LEN > largest {
        largest = open::LEN;
    }
    if attach::LEN > largest {
        largest = attach::LEN;
    }
    if transport::LEN > largest {
        largest = transport::LEN;
    }
    if level::LEN > largest {
        largest = level::LEN;
    }
    if bind::LEN > largest {
        largest = bind::LEN;
    }
    if control::LEN > largest {
        largest = control::LEN;
    }
    if baseline::LEN > largest {
        largest = baseline::LEN;
    }
    largest
}

/// A level in hundredths of a decibel, unity at most: a stream's own, or a
/// sink's or source's.
///
/// A tenant's samples are clamped at full scale before they reach a shared
/// mix, so no level may raise them past it, and an endpoint's never takes its
/// device past the device's own 0 dB point. Any attenuation is a level, down
/// to the one that rounds every sample to silence.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct AudioGain(i32);

impl AudioGain {
    /// The level that changes nothing, and therefore the bit-exact one.
    pub const UNITY: Self = Self(0);

    /// The longest spelling [`Display`](core::fmt::Display) writes:
    /// `-21474836.48dB`.
    pub const TEXT_MAX: usize = 14;

    /// The level `millibel` hundredths of a decibel name.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] above unity.
    pub const fn new(millibel: i32) -> Result<Self, Errno> {
        if millibel > Self::UNITY.0 {
            return Err(Errno::OutOfRange);
        }
        Ok(Self(millibel))
    }

    /// The level in hundredths of a decibel.
    #[must_use]
    pub const fn millibel(self) -> i32 {
        self.0
    }

    /// Parse the one spelling [`Display`](core::fmt::Display) writes:
    /// decibels with the `dB` suffix, to at most two decimals, with no
    /// leading zeros, no trailing decimal zeros, no `+`, and no `-0dB`.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] for any other text, or a level above unity.
    pub fn parse(text: &str) -> Result<Self, Errno> {
        let number = text.strip_suffix("dB").ok_or(Errno::OutOfRange)?;
        let (negative, magnitude) = match number.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, number),
        };
        let (whole, fraction) = magnitude.split_once('.').unwrap_or((magnitude, ""));
        let digits = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
        let canonical_whole = digits(whole) && (whole.len() == 1 || !whole.starts_with('0'));
        let canonical_fraction = !magnitude.contains('.')
            || (digits(fraction) && fraction.len() <= 2 && !fraction.ends_with('0'));
        if !canonical_whole || !canonical_fraction {
            return Err(Errno::OutOfRange);
        }
        // Wider than the level, so the quietest one's magnitude fits before
        // its sign does.
        let whole: i64 = whole.parse().map_err(|_| Errno::OutOfRange)?;
        let hundredths: i64 = match fraction.len() {
            0 => 0,
            1 => fraction.parse::<i64>().map_err(|_| Errno::OutOfRange)? * 10,
            _ => fraction.parse::<i64>().map_err(|_| Errno::OutOfRange)?,
        };
        let magnitude = whole
            .checked_mul(100)
            .and_then(|value| value.checked_add(hundredths))
            .ok_or(Errno::OutOfRange)?;
        if magnitude == 0 && negative {
            return Err(Errno::OutOfRange);
        }
        let millibel = i32::try_from(if negative { -magnitude } else { magnitude })
            .map_err(|_| Errno::OutOfRange)?;
        Self::new(millibel)
    }
}

/// How many decimal digits `value` is spelled in.
const fn decimal_digits(value: u16) -> usize {
    let mut digits = 1;
    let mut rest = value / 10;
    while rest > 0 {
        digits += 1;
        rest /= 10;
    }
    digits
}

impl core::fmt::Display for AudioGain {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let magnitude = self.0.unsigned_abs();
        let sign = if self.0 < 0 { "-" } else { "" };
        let (whole, hundredths) = (magnitude / 100, magnitude % 100);
        match hundredths {
            0 => write!(f, "{sign}{whole}dB"),
            tenths if tenths % 10 == 0 => write!(f, "{sign}{whole}.{}dB", tenths / 10),
            _ => write!(f, "{sign}{whole}.{hundredths:02}dB"),
        }
    }
}

/// Where a sink or source sits in the machine: its device's place in the
/// hardware tree, and its index on that device.
///
/// A device id names an endpoint for one boot; a location names it across
/// boots, so persistent configuration keys on this. The device manager
/// derives the device half from the hardware tree when it hands the channel
/// over, so the same hardware in the same place answers the same location.
/// It is spelled `<device>.<index>`: sixteen lowercase hex digits, then the
/// index in decimal.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct AudioLocation {
    device: u64,
    endpoint: u16,
}

/// Wire length of an optional [`AudioLocation`]: the device half (zero for
/// none), the index, and reserved bytes.
const LOCATION_WIRE_LEN: usize = 16;

impl AudioLocation {
    /// The `endpoint`th sink or source of the device at `device`.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] for a zero device half, which names no place, or
    /// an index past [`MAX_DEVICE_ENDPOINTS`].
    pub const fn new(device: u64, endpoint: u16) -> Result<Self, Errno> {
        if device == 0 || endpoint >= MAX_DEVICE_ENDPOINTS {
            return Err(Errno::OutOfRange);
        }
        Ok(Self { device, endpoint })
    }

    /// The device's place in the hardware tree.
    #[must_use]
    pub const fn device(self) -> u64 {
        self.device
    }

    /// The longest spelling [`Display`](core::fmt::Display) writes: sixteen
    /// hex digits, a dot, and the highest endpoint index.
    pub const TEXT_MAX: usize = 16 + 1 + decimal_digits(MAX_DEVICE_ENDPOINTS - 1);

    /// The endpoint's index on its device.
    #[must_use]
    pub const fn endpoint(self) -> u16 {
        self.endpoint
    }

    /// Parse the canonical spelling and nothing else, so a location has one
    /// written form and configuration cannot hold two that mean the same.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] for any other text, or one [`new`](Self::new)
    /// refuses.
    pub fn parse(text: &str) -> Result<Self, Errno> {
        let (device, endpoint) = text.split_once('.').ok_or(Errno::OutOfRange)?;
        let canonical_hex = device.len() == 16
            && device
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        let canonical_index = !endpoint.is_empty()
            && endpoint.bytes().all(|b| b.is_ascii_digit())
            && (endpoint == "0" || !endpoint.starts_with('0'));
        if !canonical_hex || !canonical_index {
            return Err(Errno::OutOfRange);
        }
        let device = u64::from_str_radix(device, 16).map_err(|_| Errno::OutOfRange)?;
        let endpoint = endpoint.parse::<u16>().map_err(|_| Errno::OutOfRange)?;
        Self::new(device, endpoint)
    }

    fn put(location: Option<Self>, out: &mut [u8]) {
        if let Some(location) = location {
            put_u64(out, 0, location.device);
            put_u16(out, 8, location.endpoint);
        }
    }

    fn read(bytes: &[u8]) -> Result<Option<Self>, Errno> {
        if bytes[10..LOCATION_WIRE_LEN].iter().any(|b| *b != 0) {
            return Err(Errno::BadMagic);
        }
        match (read_u64(bytes, 0), read_u16(bytes, 8)) {
            (0, 0) => Ok(None),
            (device, endpoint) => Self::new(device, endpoint).map(Some),
        }
    }
}

impl core::fmt::Display for AudioLocation {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:016x}.{}", self.device, self.endpoint)
    }
}

/// The machine's baseline beneath every tenant's controls: what an endpoint
/// starts at, and which sink and source the machine prefers as defaults.
///
/// The administrator's, from `system.conf`; the device manager carries it to
/// the service.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct AudioBaseline {
    /// The sink preferred as the default, or [`None`] for the first bound.
    pub output: Option<AudioLocation>,
    /// The source preferred as the default, or [`None`] for the first bound.
    pub input: Option<AudioLocation>,
    /// The level every endpoint starts at.
    pub level: AudioGain,
}

impl AudioBaseline {
    /// No preference, every endpoint at unity: the machine nobody configured.
    pub const DEFAULT: Self = Self {
        output: None,
        input: None,
        level: AudioGain::UNITY,
    };
}

/// One audio-service operation.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum AudioRequest {
    /// Describe the sink or source the caller may see with the least id
    /// above `after`, or refuse with [`Errno::NotFound`] when there is none.
    /// A walk is keyed by id rather than by position, so a device that comes
    /// or goes during it cannot make the walk skip one that stayed.
    Enumerate {
        /// Sinks or sources.
        direction: StreamDirection,
        /// The last id the walk was answered, or zero to start it.
        after: u32,
    },
    /// Open a stream, and be told what was actually granted.
    Open(OpenParams),
    /// Adopt the caller's shared PCM region for the stream. The region is
    /// exactly [`StreamGrant`]'s geometry; the handle is the endpoint-directed
    /// `shm_grant` the caller minted.
    Attach {
        /// The stream the region carries frames for.
        stream_id: u64,
        /// The `shm_grant` handle minted to the service's serving task.
        region_grant: u64,
    },
    /// Begin moving frames at an exact position. A stop already scheduled
    /// ahead of it stays in force, so a client can name a segment's end
    /// before the device ever moves.
    Start {
        /// The stream to start.
        stream_id: u64,
        /// The position its first frame belongs at.
        at: Frames,
    },
    /// Stop at an exact position, holding it so a resume is exact: at once
    /// for a position already reached, otherwise when the stream reaches it.
    Stop {
        /// The stream to stop.
        stream_id: u64,
        /// The position to stop at.
        at: Frames,
    },
    /// Play out everything queued, then stop.
    Drain {
        /// The stream to drain.
        stream_id: u64,
    },
    /// Discard everything queued. The position advances over the discarded
    /// frames, so the stream's arithmetic still describes where what follows
    /// belongs.
    Flush {
        /// The stream to flush.
        stream_id: u64,
    },
    /// Read the device clock this stream is in the domain of.
    Clock {
        /// The stream whose clock is wanted.
        stream_id: u64,
    },
    /// Set this stream's level.
    Gain {
        /// The stream to set.
        stream_id: u64,
        /// Its level.
        gain: AudioGain,
    },
    /// Mute or unmute this stream, independently of its gain.
    Mute {
        /// The stream to set.
        stream_id: u64,
        /// Whether it is muted.
        muted: bool,
    },
    /// Read the stream's state, the position it changed at, and its glitch
    /// tallies.
    State {
        /// The stream to report on.
        stream_id: u64,
    },
    /// Close the stream and release its region.
    Close {
        /// The stream to close.
        stream_id: u64,
    },
    /// Adopt a driver's `audiochan-v1` device channel as a sound device.
    ///
    /// Not a client operation: the device manager issues it when a driver
    /// publishes its channel node, and the service admits it only from a
    /// caller the kernel attests holds `CAP_DRV_LOAD` — the authority to put
    /// a driver on the machine, which is exactly the authority to tell the
    /// mixer about one. No new capability is minted for it, and no ordinary
    /// program can reach it.
    BindDriver {
        /// The reserved device-channel endpoint the driver claimed and
        /// published as a hardware-tree resource.
        endpoint_id: u64,
        /// The device's place in the hardware tree: the device half of each
        /// of its endpoints' [`AudioLocation`]s. Never zero.
        location: u64,
    },
    /// Retire a driver's device channel: every stream on it is told the
    /// device is lost, and the device is forgotten once none rides it.
    ///
    /// The device manager's, when the channel's node leaves the hardware
    /// tree, admitted on the authority [`BindDriver`](Self::BindDriver) is.
    UnbindDriver {
        /// The device-channel endpoint the driver published.
        endpoint_id: u64,
    },
    /// Make a sink or source the default for its direction, for the room's
    /// tenant.
    SetDefault {
        /// The endpoint to make the default.
        device_id: u32,
    },
    /// Set a sink's or source's own level, for the room's tenant.
    SetLevel {
        /// The endpoint to set.
        device_id: u32,
        /// Its level.
        level: AudioGain,
    },
    /// Mute or unmute a sink or source, independently of its level, for the
    /// room's tenant.
    SetMute {
        /// The endpoint to set.
        device_id: u32,
        /// Whether it is muted.
        muted: bool,
    },
    /// Describe the first open stream whose id is above `after`, or refuse
    /// with [`Errno::NotFound`] past the last.
    ///
    /// Not a client operation: answered only to a caller the kernel attests
    /// holds `CAP_SYSINFO_INTROSPECT` — the System Information service, which
    /// decides what each of its own callers may see. Paging by id rather than
    /// by position means a stream opening or closing between two calls
    /// neither repeats nor hides another.
    ListStreams {
        /// The id the listing continues after; zero starts it.
        after: u64,
    },
    /// The machine's baseline beneath every tenant's controls, from the device
    /// manager, admitted on the authority [`BindDriver`](Self::BindDriver) is.
    Baseline(AudioBaseline),
}

/// What a client asks for when it opens a stream.
///
/// There is no channel count: the channel map carries it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct OpenParams {
    /// The device to open on, as enumerated — or **zero for this machine's
    /// default** in the direction asked for. Enumerated identities start at
    /// one, so zero can never name a real device; a client with no
    /// preference spends no round trip enumerating and cannot race the
    /// default changing under it. With no default adopted the open is
    /// refused rather than resolved to an arbitrary device.
    pub device_id: u32,
    /// Playback or capture. Capture demands the capture capability.
    pub direction: StreamDirection,
    /// The sample encoding the client will write or read.
    pub format: SampleFormat,
    /// The rate the client's material is at.
    pub rate: Rate,
    /// The client's channel layout.
    pub channel_map: ChannelMap,
    /// What the sound is for.
    pub role: StreamRole,
    /// The latency the client would like, in frames at its own rate. The
    /// service answers the latency it granted rather than failing.
    pub latency_target_frames: u32,
}

impl AudioRequest {
    /// Largest encoded request frame.
    pub const MAX_WIRE_LEN: usize = AUDIO_MAX_REQUEST;

    /// The operation's wire discriminant byte.
    const fn op_byte(&self) -> u8 {
        match self {
            Self::Enumerate { .. } => op::ENUMERATE,
            Self::Open(_) => op::OPEN,
            Self::Attach { .. } => op::ATTACH,
            Self::Start { .. } => op::START,
            Self::Stop { .. } => op::STOP,
            Self::Drain { .. } => op::DRAIN,
            Self::Flush { .. } => op::FLUSH,
            Self::Clock { .. } => op::CLOCK,
            Self::Gain { .. } => op::GAIN,
            Self::Mute { .. } => op::MUTE,
            Self::State { .. } => op::STATE,
            Self::Close { .. } => op::CLOSE,
            Self::BindDriver { .. } => op::BIND_DRIVER,
            Self::UnbindDriver { .. } => op::UNBIND_DRIVER,
            Self::SetDefault { .. } => op::SET_DEFAULT,
            Self::SetLevel { .. } => op::SET_LEVEL,
            Self::SetMute { .. } => op::SET_MUTE,
            Self::ListStreams { .. } => op::LIST_STREAMS,
            Self::Baseline(_) => op::BASELINE,
        }
    }

    /// Encoded length of this operation's frame.
    const fn wire_len(&self) -> usize {
        HEADER_LEN
            + match self {
                Self::Enumerate { .. } => enumerate::LEN,
                Self::Open(_) => open::LEN,
                Self::Attach { .. } => attach::LEN,
                Self::Start { .. } | Self::Stop { .. } => transport::LEN,
                Self::Gain { .. } | Self::Mute { .. } => level::LEN,
                Self::BindDriver { .. } => bind::LEN,
                Self::SetDefault { .. } | Self::SetLevel { .. } | Self::SetMute { .. } => {
                    control::LEN
                }
                Self::Baseline(_) => baseline::LEN,
                Self::Drain { .. }
                | Self::Flush { .. }
                | Self::Clock { .. }
                | Self::State { .. }
                | Self::Close { .. }
                | Self::UnbindDriver { .. }
                | Self::ListStreams { .. } => STREAM_BODY_LEN,
            }
    }

    /// Encode `self` into `out`, returning the number of bytes written.
    ///
    /// # Errors
    ///
    /// [`Errno::BufferTooSmall`] if `out` cannot hold the encoded frame.
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, Errno> {
        let len = self.wire_len();
        let Some(frame) = out.get_mut(..len) else {
            return Err(Errno::BufferTooSmall);
        };
        frame.fill(0);
        put_u32(frame, 0, AUDIO_REQUEST_MAGIC);
        put_u16(frame, 4, AUDIO_VERSION_V1);
        frame[6] = self.op_byte();
        let body = &mut frame[HEADER_LEN..];
        match self {
            Self::Enumerate { direction, after } => {
                body[enumerate::DIRECTION] = direction.as_u8();
                put_u32(body, enumerate::AFTER, *after);
            }
            Self::Open(params) => encode_open(body, params),
            Self::Attach {
                stream_id,
                region_grant,
            } => {
                put_u64(body, attach::STREAM, *stream_id);
                put_u64(body, attach::GRANT, *region_grant);
            }
            Self::Start { stream_id, at } | Self::Stop { stream_id, at } => {
                put_u64(body, transport::STREAM, *stream_id);
                put_u64(body, transport::AT, at.get());
            }
            Self::Gain { stream_id, gain } => {
                put_u64(body, level::STREAM, *stream_id);
                put_i32(body, level::MILLIBEL, gain.millibel());
            }
            Self::Mute { stream_id, muted } => {
                put_u64(body, level::STREAM, *stream_id);
                body[level::MUTED] = u8::from(*muted);
            }
            Self::Drain { stream_id }
            | Self::Flush { stream_id }
            | Self::Clock { stream_id }
            | Self::State { stream_id }
            | Self::Close { stream_id } => put_u64(body, 0, *stream_id),
            Self::BindDriver {
                endpoint_id,
                location,
            } => {
                put_u64(body, bind::ENDPOINT, *endpoint_id);
                put_u64(body, bind::LOCATION, *location);
            }
            Self::UnbindDriver { endpoint_id } => put_u64(body, 0, *endpoint_id),
            Self::ListStreams { after } => put_u64(body, 0, *after),
            Self::SetDefault { device_id } => put_u32(body, control::DEVICE, *device_id),
            Self::SetLevel { device_id, level } => {
                put_u32(body, control::DEVICE, *device_id);
                put_i32(body, control::VALUE, level.millibel());
            }
            Self::SetMute { device_id, muted } => {
                put_u32(body, control::DEVICE, *device_id);
                body[control::VALUE] = u8::from(*muted);
            }
            Self::Baseline(baseline) => {
                AudioLocation::put(baseline.output, &mut body[baseline::OUTPUT..]);
                AudioLocation::put(baseline.input, &mut body[baseline::INPUT..]);
                put_i32(body, baseline::LEVEL, baseline.level.millibel());
            }
        }
        Ok(len)
    }

    /// Decode a request frame, fail-closed.
    ///
    /// # Errors
    ///
    /// * [`Errno::BufferTooSmall`] — shorter than the operation requires.
    /// * [`Errno::BadMagic`] — wrong magic or a dirty reserved field.
    /// * [`Errno::AbiVersionUnsupported`] — not [`AUDIO_VERSION_V1`].
    /// * [`Errno::OutOfRange`] — an unknown operation byte, a zero stream id,
    ///   or an out-of-range embedded value.
    pub fn decode(bytes: &[u8]) -> Result<Self, Errno> {
        if bytes.len() < HEADER_LEN {
            return Err(Errno::BufferTooSmall);
        }
        if read_u32(bytes, 0) != AUDIO_REQUEST_MAGIC {
            return Err(Errno::BadMagic);
        }
        if read_u16(bytes, 4) != AUDIO_VERSION_V1 {
            return Err(Errno::AbiVersionUnsupported);
        }
        if bytes[7] != 0 {
            return Err(Errno::BadMagic);
        }
        let op = bytes[6];
        let body = bytes
            .get(HEADER_LEN..HEADER_LEN + body_len(op)?)
            .ok_or(Errno::BufferTooSmall)?;
        match op {
            op::ENUMERATE => decode_enumerate(body),
            op::OPEN => Ok(Self::Open(decode_open(body)?)),
            op::ATTACH => Ok(Self::Attach {
                stream_id: checked_stream(read_u64(body, attach::STREAM))?,
                region_grant: read_u64(body, attach::GRANT),
            }),
            op::START | op::STOP => {
                let stream_id = checked_stream(read_u64(body, transport::STREAM))?;
                let at = Frames::new(read_u64(body, transport::AT));
                if op == op::START {
                    Ok(Self::Start { stream_id, at })
                } else {
                    Ok(Self::Stop { stream_id, at })
                }
            }
            op::GAIN => decode_gain(body),
            op::MUTE => decode_mute(body),
            op::BIND_DRIVER => decode_bind(body),
            op::UNBIND_DRIVER => Ok(Self::UnbindDriver {
                endpoint_id: checked_endpoint(read_u64(body, 0))?,
            }),
            op::LIST_STREAMS => Ok(Self::ListStreams {
                after: read_u64(body, 0),
            }),
            op::SET_DEFAULT | op::SET_LEVEL | op::SET_MUTE => decode_control(op, body),
            op::BASELINE => decode_baseline(body),
            _ => decode_stream_only(op, body),
        }
    }
}

/// Body length of the operation `op` names.
const fn body_len(op: u8) -> Result<usize, Errno> {
    match op {
        op::ENUMERATE => Ok(enumerate::LEN),
        op::OPEN => Ok(open::LEN),
        op::ATTACH => Ok(attach::LEN),
        op::START | op::STOP => Ok(transport::LEN),
        op::GAIN | op::MUTE => Ok(level::LEN),
        op::BIND_DRIVER => Ok(bind::LEN),
        op::SET_DEFAULT | op::SET_LEVEL | op::SET_MUTE => Ok(control::LEN),
        op::BASELINE => Ok(baseline::LEN),
        op::DRAIN
        | op::FLUSH
        | op::CLOCK
        | op::STATE
        | op::CLOSE
        | op::UNBIND_DRIVER
        | op::LIST_STREAMS => Ok(STREAM_BODY_LEN),
        _ => Err(Errno::OutOfRange),
    }
}

/// A stream id, refused when it is the zero no stream is ever issued.
///
/// Zero is what an uninitialised or truncated frame carries, so reserving it
/// turns a whole class of confused requests into a refusal rather than an
/// operation on whichever stream happened to be first.
const fn checked_stream(stream_id: u64) -> Result<u64, Errno> {
    if stream_id == 0 {
        return Err(Errno::OutOfRange);
    }
    Ok(stream_id)
}

/// A device-channel endpoint id, refused when it is the zero no endpoint is
/// ever given.
const fn checked_endpoint(endpoint_id: u64) -> Result<u64, Errno> {
    if endpoint_id == 0 {
        return Err(Errno::OutOfRange);
    }
    Ok(endpoint_id)
}

fn decode_bind(body: &[u8]) -> Result<AudioRequest, Errno> {
    let location = read_u64(body, bind::LOCATION);
    if location == 0 {
        return Err(Errno::OutOfRange);
    }
    Ok(AudioRequest::BindDriver {
        endpoint_id: checked_endpoint(read_u64(body, bind::ENDPOINT))?,
        location,
    })
}

/// Decode a device control. Zero names "the default" when a stream is
/// opened, never an endpoint, so it is refused here.
fn decode_control(op: u8, body: &[u8]) -> Result<AudioRequest, Errno> {
    let device_id = read_u32(body, control::DEVICE);
    if device_id == 0 {
        return Err(Errno::OutOfRange);
    }
    let value = &body[control::VALUE..control::LEN];
    match op {
        op::SET_DEFAULT => {
            if value.iter().any(|b| *b != 0) {
                return Err(Errno::BadMagic);
            }
            Ok(AudioRequest::SetDefault { device_id })
        }
        op::SET_LEVEL => Ok(AudioRequest::SetLevel {
            device_id,
            level: AudioGain::new(read_i32(body, control::VALUE))?,
        }),
        _ => {
            if value[1..].iter().any(|b| *b != 0) {
                return Err(Errno::BadMagic);
            }
            let muted = match value[0] {
                0 => false,
                1 => true,
                _ => return Err(Errno::OutOfRange),
            };
            Ok(AudioRequest::SetMute { device_id, muted })
        }
    }
}

fn decode_baseline(body: &[u8]) -> Result<AudioRequest, Errno> {
    if read_u32(body, baseline::RESERVED) != 0 {
        return Err(Errno::BadMagic);
    }
    Ok(AudioRequest::Baseline(AudioBaseline {
        output: AudioLocation::read(&body[baseline::OUTPUT..baseline::INPUT])?,
        input: AudioLocation::read(&body[baseline::INPUT..baseline::LEVEL])?,
        level: AudioGain::new(read_i32(body, baseline::LEVEL))?,
    }))
}

fn decode_enumerate(body: &[u8]) -> Result<AudioRequest, Errno> {
    if body[enumerate::RESERVED..enumerate::AFTER]
        .iter()
        .any(|&byte| byte != 0)
    {
        return Err(Errno::BadMagic);
    }
    Ok(AudioRequest::Enumerate {
        direction: StreamDirection::from_u8(body[enumerate::DIRECTION])?,
        after: read_u32(body, enumerate::AFTER),
    })
}

fn encode_open(body: &mut [u8], params: &OpenParams) {
    put_u32(body, open::DEVICE, params.device_id);
    body[open::DIRECTION] = params.direction.as_u8();
    body[open::FORMAT] = params.format.as_u8();
    body[open::ROLE] = params.role.as_u8();
    put_u32(body, open::RATE, params.rate.hz());
    put_u32(body, open::LATENCY, params.latency_target_frames);
    body[open::CHANNEL_MAP..open::CHANNEL_MAP + CHANNEL_MAP_WIRE_LEN]
        .copy_from_slice(&params.channel_map.to_wire());
}

fn decode_open(body: &[u8]) -> Result<OpenParams, Errno> {
    if body[open::RESERVED0] != 0 || body[open::RESERVED1..].iter().any(|b| *b != 0) {
        return Err(Errno::BadMagic);
    }
    let latency_target_frames = read_u32(body, open::LATENCY);
    // A target of zero asks for no buffering at all and one past the ring
    // bound asks for memory no grant may pin; both are refused rather than
    // silently clamped, so a client learns its request was nonsense.
    if latency_target_frames == 0 || latency_target_frames > ring_bounds::MAX_FRAMES {
        return Err(Errno::OutOfRange);
    }
    Ok(OpenParams {
        device_id: read_u32(body, open::DEVICE),
        direction: StreamDirection::from_u8(body[open::DIRECTION])?,
        format: SampleFormat::from_u8(body[open::FORMAT])?,
        rate: Rate::new(read_u32(body, open::RATE))?,
        channel_map: ChannelMap::from_wire(&body[open::CHANNEL_MAP..])?,
        role: StreamRole::from_u8(body[open::ROLE])?,
        latency_target_frames,
    })
}

fn decode_gain(body: &[u8]) -> Result<AudioRequest, Errno> {
    if read_u32(body, level::RESERVED) != 0 {
        return Err(Errno::BadMagic);
    }
    Ok(AudioRequest::Gain {
        stream_id: checked_stream(read_u64(body, level::STREAM))?,
        gain: AudioGain::new(read_i32(body, level::MILLIBEL))?,
    })
}

fn decode_mute(body: &[u8]) -> Result<AudioRequest, Errno> {
    if body[level::MUTED + 1..].iter().any(|b| *b != 0) {
        return Err(Errno::BadMagic);
    }
    let muted = match body[level::MUTED] {
        0 => false,
        1 => true,
        _ => return Err(Errno::OutOfRange),
    };
    Ok(AudioRequest::Mute {
        stream_id: checked_stream(read_u64(body, level::STREAM))?,
        muted,
    })
}

fn decode_stream_only(op: u8, body: &[u8]) -> Result<AudioRequest, Errno> {
    let stream_id = checked_stream(read_u64(body, 0))?;
    match op {
        op::DRAIN => Ok(AudioRequest::Drain { stream_id }),
        op::FLUSH => Ok(AudioRequest::Flush { stream_id }),
        op::CLOCK => Ok(AudioRequest::Clock { stream_id }),
        op::STATE => Ok(AudioRequest::State { stream_id }),
        op::CLOSE => Ok(AudioRequest::Close { stream_id }),
        _ => Err(Errno::OutOfRange),
    }
}

/// Byte offsets within an [`AudioDeviceDescriptor`] payload.
mod descriptor {
    use super::{
        AUDIO_NAME_MAX, CHANNEL_MAP_WIRE_LEN, GAIN_RANGE_WIRE_LEN, LOCATION_WIRE_LEN,
        RATE_SUPPORT_WIRE_LEN,
    };

    pub const DEVICE: usize = 0;
    pub const DIRECTION: usize = 4;
    pub const JACK: usize = 5;
    pub const DEFAULT: usize = 6;
    pub const RESERVED0: usize = 7;
    pub const FORMATS: usize = 8;
    pub const RESERVED1: usize = 10;
    pub const CHANNEL_MAP: usize = 12;
    pub const RESERVED2: usize = CHANNEL_MAP + CHANNEL_MAP_WIRE_LEN;
    pub const RATES: usize = RESERVED2 + 3;
    pub const GAIN: usize = RATES + RATE_SUPPORT_WIRE_LEN;
    pub const NAME_LEN: usize = GAIN + GAIN_RANGE_WIRE_LEN;
    pub const NAME: usize = NAME_LEN + 1;
    pub const LOCATION: usize = NAME + AUDIO_NAME_MAX;
    pub const LEVEL: usize = LOCATION + LOCATION_WIRE_LEN;
    pub const FLAGS: usize = LEVEL + 4;
    pub const RESERVED3: usize = FLAGS + 1;
    pub const CLOCK: usize = FLAGS + 4;
    pub const LOST: usize = CLOCK + 4;
    pub const LEN: usize = LOST + 8;

    /// The endpoint is muted.
    pub const MUTED: u8 = 1;
    /// The caller may change its controls.
    pub const CONTROLLABLE: u8 = 2;
    /// The caller's login session is the room's tenant; never without
    /// `CONTROLLABLE`.
    pub const TENANT: u8 = 4;
    /// The level is the tenant's own, not the machine's baseline.
    pub const OWN_LEVEL: u8 = 8;
    /// Every flag a descriptor may carry.
    pub const ALL: u8 = MUTED | CONTROLLABLE | TENANT | OWN_LEVEL;
}

/// What the caller may do with a device's controls, as the room the device
/// serves decides.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ControlAccess {
    /// The room is another session's, or withheld: the controls are shown,
    /// not the caller's to change.
    Shown,
    /// The room is unclaimed, so anybody may change them.
    Shared,
    /// The caller's own login session holds the room, so the controls shown
    /// are its own.
    Own,
}

impl ControlAccess {
    /// Whether the caller may change the controls.
    #[must_use]
    pub const fn may_change(self) -> bool {
        !matches!(self, Self::Shown)
    }
}

/// Whether a device is its direction's default, and why.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DefaultChoice {
    /// Another device is.
    No,
    /// It is, by the machine's preference or for want of any.
    Inherited,
    /// It is, because the room's tenant prefers it.
    Preferred,
}

impl DefaultChoice {
    /// Whether the device is its direction's default.
    #[must_use]
    pub const fn is_default(self) -> bool {
        !matches!(self, Self::No)
    }

    const fn as_u8(self) -> u8 {
        match self {
            Self::No => 0,
            Self::Inherited => 1,
            Self::Preferred => 2,
        }
    }

    const fn from_u8(byte: u8) -> Result<Self, Errno> {
        match byte {
            0 => Ok(Self::No),
            1 => Ok(Self::Inherited),
            2 => Ok(Self::Preferred),
            _ => Err(Errno::OutOfRange),
        }
    }
}

/// One sink or source, as a client sees it.
///
/// The `audio:` resource reference is derived rather than carried:
/// `audio:sink/<id>` for a playback device and `audio:source/<id>` for a
/// capture one, so the scheme and the descriptor cannot disagree.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct AudioDeviceDescriptor {
    /// The service-assigned identity a stream is opened against.
    pub device_id: u32,
    /// Whether it is a sink or a source.
    pub direction: StreamDirection,
    /// Whether anything is plugged into its connector.
    pub jack: JackState,
    /// Whether it is its direction's default for the room's tenant, and why.
    pub default: DefaultChoice,
    /// Sample encodings it accepts without conversion.
    pub formats: SampleFormats,
    /// Its channel layout.
    pub channel_map: ChannelMap,
    /// Rates it can be clocked at.
    pub rates: RateSupport,
    /// Its hardware gain control, where it has one. A user interface shows one
    /// number, so the service reports which part of the volume is the
    /// hardware's.
    pub gain: Option<GainRange>,
    /// What to call it.
    pub name: AudioName,
    /// Where it is, across boots.
    pub location: AudioLocation,
    /// Its own level, as the room's tenant has it.
    pub level: AudioGain,
    /// Whether `level` is the room's tenant's own setting rather than the
    /// machine's baseline, which a setting that remembers it must tell apart.
    pub own_level: bool,
    /// Whether it is muted.
    pub muted: bool,
    /// What the caller may do with its controls now.
    pub access: ControlAccess,
    /// The rate its device clock is measured at, in thousandths of a hertz,
    /// or zero while it is not clocking.
    pub clock_millihertz: u32,
    /// Frames the device reported losing since it was bound.
    pub lost_frames: u64,
}

/// Wire length of one [`AudioDeviceDescriptor`]: the `Enumerate` reply's
/// payload, and one record of the System Information API's device listing.
pub const AUDIO_DEVICE_RECORD_LEN: usize = descriptor::LEN;

/// Wire length of the `Enumerate` reply: a status word then the descriptor
/// (zeroed on refusal).
pub const AUDIO_ENUMERATE_REPLY_LEN: usize = 4 + AUDIO_DEVICE_RECORD_LEN;

impl AudioDeviceDescriptor {
    /// Encode little-endian.
    #[must_use]
    pub fn to_le_bytes(&self) -> [u8; AUDIO_DEVICE_RECORD_LEN] {
        let mut body = [0u8; AUDIO_DEVICE_RECORD_LEN];
        put_u32(&mut body, descriptor::DEVICE, self.device_id);
        body[descriptor::DIRECTION] = self.direction.as_u8();
        body[descriptor::JACK] = self.jack.as_u8();
        body[descriptor::DEFAULT] = self.default.as_u8();
        put_u16(&mut body, descriptor::FORMATS, self.formats.bits());
        body[descriptor::CHANNEL_MAP..descriptor::CHANNEL_MAP + CHANNEL_MAP_WIRE_LEN]
            .copy_from_slice(&self.channel_map.to_wire());
        body[descriptor::RATES..descriptor::RATES + RATE_SUPPORT_WIRE_LEN]
            .copy_from_slice(&self.rates.to_wire());
        body[descriptor::GAIN..descriptor::GAIN + GAIN_RANGE_WIRE_LEN]
            .copy_from_slice(&GainRange::to_wire(self.gain));
        body[descriptor::NAME_LEN] = self.name.len_byte();
        body[descriptor::NAME..descriptor::LOCATION].copy_from_slice(self.name.raw_bytes());
        AudioLocation::put(Some(self.location), &mut body[descriptor::LOCATION..]);
        put_i32(&mut body, descriptor::LEVEL, self.level.millibel());
        let mut flags = 0;
        if self.muted {
            flags |= descriptor::MUTED;
        }
        flags |= match self.access {
            ControlAccess::Shown => 0,
            ControlAccess::Shared => descriptor::CONTROLLABLE,
            ControlAccess::Own => descriptor::CONTROLLABLE | descriptor::TENANT,
        };
        if self.own_level {
            flags |= descriptor::OWN_LEVEL;
        }
        body[descriptor::FLAGS] = flags;
        put_u32(&mut body, descriptor::CLOCK, self.clock_millihertz);
        put_u64(&mut body, descriptor::LOST, self.lost_frames);
        body
    }

    /// Decode, fail-closed.
    ///
    /// # Errors
    ///
    /// [`Errno::BufferTooSmall`] for a short record, [`Errno::BadMagic`] for
    /// a dirty reserved field or an undefined flag, or whatever the embedded
    /// values' own decoders refuse.
    pub fn from_le_bytes(body: &[u8]) -> Result<Self, Errno> {
        let body = body
            .get(..AUDIO_DEVICE_RECORD_LEN)
            .ok_or(Errno::BufferTooSmall)?;
        if body[descriptor::RESERVED0] != 0
            || read_u16(body, descriptor::RESERVED1) != 0
            || body[descriptor::RESERVED2..descriptor::RATES]
                .iter()
                .any(|b| *b != 0)
            || body[descriptor::RESERVED3..descriptor::CLOCK]
                .iter()
                .any(|b| *b != 0)
            || body[descriptor::FLAGS] & !descriptor::ALL != 0
        {
            return Err(Errno::BadMagic);
        }
        let default = DefaultChoice::from_u8(body[descriptor::DEFAULT])?;
        let formats = SampleFormats::from_bits(read_u16(body, descriptor::FORMATS))?;
        if formats.is_empty() {
            return Err(Errno::OutOfRange);
        }
        let mut name = [0u8; AUDIO_NAME_MAX];
        name.copy_from_slice(&body[descriptor::NAME..descriptor::LOCATION]);
        let flags = body[descriptor::FLAGS];
        let flag = |bit: u8| flags & bit != 0;
        let access = match (flag(descriptor::CONTROLLABLE), flag(descriptor::TENANT)) {
            (false, false) => ControlAccess::Shown,
            (true, false) => ControlAccess::Shared,
            (true, true) => ControlAccess::Own,
            (false, true) => return Err(Errno::OutOfRange),
        };
        Ok(Self {
            device_id: read_u32(body, descriptor::DEVICE),
            direction: StreamDirection::from_u8(body[descriptor::DIRECTION])?,
            jack: JackState::from_u8(body[descriptor::JACK])?,
            default,
            formats,
            channel_map: ChannelMap::from_wire(&body[descriptor::CHANNEL_MAP..])?,
            rates: RateSupport::from_wire(&body[descriptor::RATES..])?,
            gain: GainRange::from_wire(&body[descriptor::GAIN..])?,
            name: AudioName::from_wire(body[descriptor::NAME_LEN], &name)?,
            location: AudioLocation::read(&body[descriptor::LOCATION..descriptor::LEVEL])?
                .ok_or(Errno::OutOfRange)?,
            level: AudioGain::new(read_i32(body, descriptor::LEVEL))?,
            own_level: flag(descriptor::OWN_LEVEL),
            muted: flag(descriptor::MUTED),
            access,
            clock_millihertz: read_u32(body, descriptor::CLOCK),
            lost_frames: read_u64(body, descriptor::LOST),
        })
    }
}

/// Encode the service's reply to [`AudioRequest::Enumerate`].
#[must_use]
pub fn encode_enumerate_reply(
    result: Result<AudioDeviceDescriptor, Errno>,
) -> [u8; AUDIO_ENUMERATE_REPLY_LEN] {
    let mut out = [0u8; AUDIO_ENUMERATE_REPLY_LEN];
    match result {
        Ok(device) => out[4..].copy_from_slice(&device.to_le_bytes()),
        Err(err) => crate::reply::put_refusal(&mut out, err),
    }
    out
}

/// Decode an `Enumerate` reply, fail-closed.
///
/// # Errors
///
/// [`Errno::BufferTooSmall`] for a short frame, [`Errno::NotFound`] once the
/// list is exhausted or any other refusal the service returned, or whatever
/// [`AudioDeviceDescriptor::from_le_bytes`] refuses.
pub fn decode_enumerate_reply(bytes: &[u8]) -> Result<AudioDeviceDescriptor, Errno> {
    AudioDeviceDescriptor::from_le_bytes(crate::reply::take_payload(
        bytes,
        AUDIO_ENUMERATE_REPLY_LEN,
    )?)
}

/// Byte offsets within a [`StreamDescriptor`] payload.
mod stream_descriptor {
    use crate::appinfo::BUNDLE_ID_MAX;

    pub const STREAM: usize = 0;
    pub const OWNER_PID: usize = 8;
    pub const POSITION: usize = 16;
    pub const XRUN_FRAMES: usize = 24;
    pub const DEVICE: usize = 32;
    pub const OWNER_UID: usize = 36;
    pub const XRUNS: usize = 40;
    pub const DIRECTION: usize = 44;
    pub const ROLE: usize = 45;
    pub const STATE: usize = 46;
    pub const APP_LEN: usize = 47;
    pub const APP: usize = 48;
    pub const LEN: usize = APP + BUNDLE_ID_MAX;
}

/// One open stream, as the System Information service is told of it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct StreamDescriptor {
    /// The service-issued stream id.
    pub stream_id: u64,
    /// The sink or source it rides.
    pub device_id: u32,
    /// Playback or capture.
    pub direction: StreamDirection,
    /// What its sound is for.
    pub role: StreamRole,
    /// What its owner is told it is doing; a stream the room holds reports
    /// [`StreamState::SeatInactive`].
    pub state: StreamState,
    /// The position it has reached.
    pub position: Frames,
    /// Distinct under- or over-runs since it was opened.
    pub xruns: u32,
    /// Frames lost across them.
    pub xrun_frames: u64,
    /// The user it runs as.
    pub owner_uid: u32,
    /// The process that opened it.
    pub owner_pid: u64,
    /// The application it belongs to, where the kernel attests one.
    pub owner_app: Option<BundleId>,
}

/// Wire length of one [`StreamDescriptor`]: the `ListStreams` reply's
/// payload, and one record of the System Information API's stream listings.
pub const AUDIO_STREAM_RECORD_LEN: usize = stream_descriptor::LEN;

/// Wire length of the `ListStreams` reply.
pub const AUDIO_STREAMS_REPLY_LEN: usize = 4 + AUDIO_STREAM_RECORD_LEN;

impl StreamDescriptor {
    /// Encode little-endian.
    #[must_use]
    pub fn to_le_bytes(&self) -> [u8; AUDIO_STREAM_RECORD_LEN] {
        let mut body = [0u8; AUDIO_STREAM_RECORD_LEN];
        put_u64(&mut body, stream_descriptor::STREAM, self.stream_id);
        put_u64(&mut body, stream_descriptor::OWNER_PID, self.owner_pid);
        put_u64(&mut body, stream_descriptor::POSITION, self.position.get());
        put_u64(&mut body, stream_descriptor::XRUN_FRAMES, self.xrun_frames);
        put_u32(&mut body, stream_descriptor::DEVICE, self.device_id);
        put_u32(&mut body, stream_descriptor::OWNER_UID, self.owner_uid);
        put_u32(&mut body, stream_descriptor::XRUNS, self.xruns);
        body[stream_descriptor::DIRECTION] = self.direction.as_u8();
        body[stream_descriptor::ROLE] = self.role.as_u8();
        body[stream_descriptor::STATE] = self.state.as_u8();
        if let Some(app) = self.owner_app {
            let id = app.as_str().as_bytes();
            body[stream_descriptor::APP_LEN] = u8::try_from(id.len()).unwrap_or(0);
            body[stream_descriptor::APP..stream_descriptor::APP + id.len()].copy_from_slice(id);
        }
        body
    }

    /// Decode, fail-closed.
    ///
    /// # Errors
    ///
    /// * [`Errno::BufferTooSmall`] — shorter than [`AUDIO_STREAM_RECORD_LEN`].
    /// * [`Errno::BadMagic`] — bytes past the application's name.
    /// * [`Errno::OutOfRange`] — an undefined direction, role or state, a
    ///   zero stream id, or a name that is not a bundle identifier.
    pub fn from_le_bytes(body: &[u8]) -> Result<Self, Errno> {
        let body = body
            .get(..AUDIO_STREAM_RECORD_LEN)
            .ok_or(Errno::BufferTooSmall)?;
        let app_len = usize::from(body[stream_descriptor::APP_LEN]);
        let app = body
            .get(stream_descriptor::APP..stream_descriptor::APP + app_len)
            .ok_or(Errno::OutOfRange)?;
        if body[stream_descriptor::APP + app_len..]
            .iter()
            .any(|b| *b != 0)
        {
            return Err(Errno::BadMagic);
        }
        let owner_app = if app.is_empty() {
            None
        } else {
            let id = core::str::from_utf8(app).map_err(|_| Errno::OutOfRange)?;
            crate::validate_bundle_id(id)?;
            Some(BundleId::new(id)?)
        };
        Ok(Self {
            stream_id: checked_stream(read_u64(body, stream_descriptor::STREAM))?,
            device_id: read_u32(body, stream_descriptor::DEVICE),
            direction: StreamDirection::from_u8(body[stream_descriptor::DIRECTION])?,
            role: StreamRole::from_u8(body[stream_descriptor::ROLE])?,
            state: StreamState::from_u8(body[stream_descriptor::STATE])?,
            position: Frames::new(read_u64(body, stream_descriptor::POSITION)),
            xruns: read_u32(body, stream_descriptor::XRUNS),
            xrun_frames: read_u64(body, stream_descriptor::XRUN_FRAMES),
            owner_uid: read_u32(body, stream_descriptor::OWNER_UID),
            owner_pid: read_u64(body, stream_descriptor::OWNER_PID),
            owner_app,
        })
    }
}

/// Encode the service's reply to [`AudioRequest::ListStreams`].
#[must_use]
pub fn encode_streams_reply(
    result: Result<StreamDescriptor, Errno>,
) -> [u8; AUDIO_STREAMS_REPLY_LEN] {
    let mut out = [0u8; AUDIO_STREAMS_REPLY_LEN];
    match result {
        Ok(stream) => out[4..].copy_from_slice(&stream.to_le_bytes()),
        Err(err) => crate::reply::put_refusal(&mut out, err),
    }
    out
}

/// Decode a `ListStreams` reply, fail-closed.
///
/// # Errors
///
/// [`Errno::BufferTooSmall`] for a short frame, the decoded [`Errno`] when the
/// service refused ([`Errno::NotFound`] past the last stream), or whatever
/// [`StreamDescriptor::from_le_bytes`] refuses.
pub fn decode_streams_reply(bytes: &[u8]) -> Result<StreamDescriptor, Errno> {
    StreamDescriptor::from_le_bytes(crate::reply::take_payload(bytes, AUDIO_STREAMS_REPLY_LEN)?)
}

/// Byte offsets within a [`StreamGrant`] payload.
mod stream_grant {
    use super::{Duration64, CHANNEL_MAP_WIRE_LEN};

    pub const STREAM: usize = 0;
    pub const NOTIFY: usize = 8;
    pub const RATE: usize = 16;
    pub const FORMAT: usize = 20;
    pub const RESERVED0: usize = 21;
    pub const RING_FRAMES: usize = 24;
    pub const LATENCY_FRAMES: usize = 28;
    pub const CLOCK_DOMAIN: usize = 32;
    pub const RESERVED1: usize = 36;
    pub const LATENCY: usize = 40;
    pub const CHANNEL_MAP: usize = LATENCY + Duration64::WIRE_LEN;
    pub const RESERVED2: usize = CHANNEL_MAP + CHANNEL_MAP_WIRE_LEN;
    pub const LEN: usize = RESERVED2 + 3;
}

/// What the service actually granted, which is not always what was asked for.
///
/// The client compares it with its request and owns whatever conversion the
/// difference implies. Nothing is silently resampled on its behalf, which is
/// what makes the bit-exact path a property rather than a mode.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct StreamGrant {
    /// The stream's identity for every later request.
    pub stream_id: u64,
    /// The mailbox the service wakes this stream on, derived from the caller's
    /// kernel-attested pid ([`notify_endpoint_for`]) — the client binds it, it
    /// does not choose it.
    pub notify_endpoint: u64,
    /// The rate the stream actually runs at.
    pub rate: Rate,
    /// The sample encoding it actually runs in.
    pub format: SampleFormat,
    /// The channel layout it actually carries.
    pub channel_map: ChannelMap,
    /// Frames the shared ring holds; the region is exactly this geometry.
    pub ring_frames: u32,
    /// The latency granted, in frames at [`Self::rate`].
    pub granted_latency_frames: u32,
    /// The same latency as a span, so a client never needs a sample rate to
    /// reason about time.
    pub granted_latency: Duration64,
    /// The device clock this stream belongs to. Two streams sharing a domain
    /// share a clock exactly; moving between domains is a re-open with the
    /// position carried across, never a hidden resampler.
    pub clock_domain: u32,
}

/// Wire length of the `Open` reply.
pub const AUDIO_OPEN_REPLY_LEN: usize = 4 + stream_grant::LEN;

/// Encode the service's reply to [`AudioRequest::Open`].
#[must_use]
pub fn encode_open_reply(result: Result<StreamGrant, Errno>) -> [u8; AUDIO_OPEN_REPLY_LEN] {
    let mut out = [0u8; AUDIO_OPEN_REPLY_LEN];
    match result {
        Ok(granted) => {
            let body = &mut out[4..];
            put_u64(body, stream_grant::STREAM, granted.stream_id);
            put_u64(body, stream_grant::NOTIFY, granted.notify_endpoint);
            put_u32(body, stream_grant::RATE, granted.rate.hz());
            body[stream_grant::FORMAT] = granted.format.as_u8();
            put_u32(body, stream_grant::RING_FRAMES, granted.ring_frames);
            put_u32(
                body,
                stream_grant::LATENCY_FRAMES,
                granted.granted_latency_frames,
            );
            put_u32(body, stream_grant::CLOCK_DOMAIN, granted.clock_domain);
            body[stream_grant::LATENCY..stream_grant::LATENCY + Duration64::WIRE_LEN]
                .copy_from_slice(&granted.granted_latency.to_le_bytes());
            body[stream_grant::CHANNEL_MAP..stream_grant::CHANNEL_MAP + CHANNEL_MAP_WIRE_LEN]
                .copy_from_slice(&granted.channel_map.to_wire());
        }
        Err(err) => crate::reply::put_refusal(&mut out, err),
    }
    out
}

/// Decode an `Open` reply, fail-closed.
///
/// # Errors
///
/// * [`Errno::BufferTooSmall`] — shorter than [`AUDIO_OPEN_REPLY_LEN`].
/// * The decoded [`Errno`] — the service refused the open.
/// * [`Errno::BadMagic`] — a dirty reserved field.
/// * [`Errno::OutOfRange`] — a zero stream id, a notify port naming a reserved
///   rendezvous, a ring the index arithmetic could not serve, or a granted
///   latency the ring could not hold.
pub fn decode_open_reply(bytes: &[u8]) -> Result<StreamGrant, Errno> {
    let body = crate::reply::take_payload(bytes, AUDIO_OPEN_REPLY_LEN)?;
    if body[stream_grant::RESERVED0..stream_grant::RING_FRAMES]
        .iter()
        .any(|b| *b != 0)
        || read_u32(body, stream_grant::RESERVED1) != 0
        || body[stream_grant::RESERVED2..].iter().any(|b| *b != 0)
    {
        return Err(Errno::BadMagic);
    }
    let notify_endpoint = read_u64(body, stream_grant::NOTIFY);
    // A grant naming a reserved rendezvous would have the client bind a
    // system service's id — refused here rather than discovered at bind time.
    if crate::ipc::is_reserved_endpoint(notify_endpoint) {
        return Err(Errno::OutOfRange);
    }
    let ring_frames = read_u32(body, stream_grant::RING_FRAMES);
    let granted_latency_frames = read_u32(body, stream_grant::LATENCY_FRAMES);
    if !(ring_bounds::MIN_FRAMES..=ring_bounds::MAX_FRAMES).contains(&ring_frames)
        || !ring_frames.is_power_of_two()
        || granted_latency_frames == 0
        || granted_latency_frames > ring_frames
    {
        return Err(Errno::OutOfRange);
    }
    Ok(StreamGrant {
        stream_id: checked_stream(read_u64(body, stream_grant::STREAM))?,
        notify_endpoint,
        rate: Rate::new(read_u32(body, stream_grant::RATE))?,
        format: SampleFormat::from_u8(body[stream_grant::FORMAT])?,
        channel_map: ChannelMap::from_wire(&body[stream_grant::CHANNEL_MAP..])?,
        ring_frames,
        granted_latency_frames,
        granted_latency: Duration64::from_bytes(&body[stream_grant::LATENCY..])?,
        clock_domain: read_u32(body, stream_grant::CLOCK_DOMAIN),
    })
}

/// Byte offsets within a [`ClockReport`] payload.
mod clock {
    use crate::time::Time64;

    pub const POSITION: usize = 0;
    pub const RATE_MILLIHERTZ: usize = 8;
    pub const RESERVED: usize = 12;
    pub const SAMPLED_AT: usize = 16;
    pub const LEN: usize = SAMPLED_AT + Time64::WIRE_LEN;
}

/// The exported device clock: where the device is, when that was true, and how
/// fast it is *actually* going.
///
/// The measured rate is what makes cross-device drift a number rather than a
/// mystery: a device whose crystal says 48 000 and whose reality says 47 998.6
/// reports the second.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ClockReport {
    /// The device's frame position when it was sampled.
    pub position: Frames,
    /// The measured rate, in thousandths of a hertz.
    pub rate_millihertz: u32,
    /// When [`Self::position`] was sampled.
    pub sampled_at: Time64,
}

/// Wire length of the `Clock` reply.
pub const AUDIO_CLOCK_REPLY_LEN: usize = 4 + clock::LEN;

/// Encode the service's reply to [`AudioRequest::Clock`].
#[must_use]
pub fn encode_clock_reply(result: Result<ClockReport, Errno>) -> [u8; AUDIO_CLOCK_REPLY_LEN] {
    let mut out = [0u8; AUDIO_CLOCK_REPLY_LEN];
    match result {
        Ok(report) => {
            let body = &mut out[4..];
            put_u64(body, clock::POSITION, report.position.get());
            put_u32(body, clock::RATE_MILLIHERTZ, report.rate_millihertz);
            body[clock::SAMPLED_AT..clock::SAMPLED_AT + Time64::WIRE_LEN]
                .copy_from_slice(&report.sampled_at.to_le_bytes());
        }
        Err(err) => crate::reply::put_refusal(&mut out, err),
    }
    out
}

/// Decode a `Clock` reply, fail-closed.
///
/// # Errors
///
/// * [`Errno::BufferTooSmall`] — shorter than [`AUDIO_CLOCK_REPLY_LEN`].
/// * The decoded [`Errno`] — the service refused.
/// * [`Errno::BadMagic`] — a dirty reserved field.
/// * [`Errno::OutOfRange`] — a measured rate outside what any converter runs
///   at, which would poison every fit built on it.
/// * [`Errno::TimestampOutOfRange`] — a non-canonical sample time.
pub fn decode_clock_reply(bytes: &[u8]) -> Result<ClockReport, Errno> {
    let body = crate::reply::take_payload(bytes, AUDIO_CLOCK_REPLY_LEN)?;
    if read_u32(body, clock::RESERVED) != 0 {
        return Err(Errno::BadMagic);
    }
    let rate_millihertz = read_u32(body, clock::RATE_MILLIHERTZ);
    if !(Rate::MIN_HZ * 1_000..=Rate::MAX_HZ * 1_000).contains(&rate_millihertz) {
        return Err(Errno::OutOfRange);
    }
    Ok(ClockReport {
        position: Frames::new(read_u64(body, clock::POSITION)),
        rate_millihertz,
        sampled_at: Time64::from_bytes(&body[clock::SAMPLED_AT..])?,
    })
}

/// Byte offsets within a [`StreamReport`] payload.
mod state {
    pub const STATE: usize = 0;
    pub const RESERVED: usize = 1;
    pub const XRUNS: usize = 4;
    pub const CHANGED_AT: usize = 8;
    pub const XRUN_FRAMES: usize = 16;
    pub const LEN: usize = 24;
}

/// Where a stream stands, and what it has lost.
///
/// Both tallies are reported because neither implies the other: the frame
/// count says how much audio was missed and the event count says how often the
/// user heard a glitch.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct StreamReport {
    /// The stream's state.
    pub state: StreamState,
    /// The position it entered that state at.
    pub changed_at: Frames,
    /// Distinct under- or over-runs since the stream was opened.
    pub xruns: u32,
    /// Frames lost across all of them.
    pub xrun_frames: u64,
}

/// Wire length of the `State` reply.
pub const AUDIO_STATE_REPLY_LEN: usize = 4 + state::LEN;

/// Encode the service's reply to [`AudioRequest::State`].
#[must_use]
pub fn encode_state_reply(result: Result<StreamReport, Errno>) -> [u8; AUDIO_STATE_REPLY_LEN] {
    let mut out = [0u8; AUDIO_STATE_REPLY_LEN];
    match result {
        Ok(report) => {
            let body = &mut out[4..];
            body[state::STATE] = report.state.as_u8();
            put_u32(body, state::XRUNS, report.xruns);
            put_u64(body, state::CHANGED_AT, report.changed_at.get());
            put_u64(body, state::XRUN_FRAMES, report.xrun_frames);
        }
        Err(err) => crate::reply::put_refusal(&mut out, err),
    }
    out
}

/// Decode a `State` reply, fail-closed.
///
/// # Errors
///
/// * [`Errno::BufferTooSmall`] — shorter than [`AUDIO_STATE_REPLY_LEN`].
/// * The decoded [`Errno`] — the service refused.
/// * [`Errno::BadMagic`] — a dirty reserved field.
/// * [`Errno::OutOfRange`] — an undefined state.
pub fn decode_state_reply(bytes: &[u8]) -> Result<StreamReport, Errno> {
    let body = crate::reply::take_payload(bytes, AUDIO_STATE_REPLY_LEN)?;
    if body[state::RESERVED..state::XRUNS].iter().any(|b| *b != 0) {
        return Err(Errno::BadMagic);
    }
    Ok(StreamReport {
        state: StreamState::from_u8(body[state::STATE])?,
        changed_at: Frames::new(read_u64(body, state::CHANGED_AT)),
        xruns: read_u32(body, state::XRUNS),
        xrun_frames: read_u64(body, state::XRUN_FRAMES),
    })
}

/// Largest reply any audio-service request produces.
///
/// Computed rather than naming whichever reply is biggest today: widening one
/// payload must not silently leave every buffer in the contract short.
pub const AUDIO_MAX_REPLY: usize = largest_reply();

const fn largest_reply() -> usize {
    let mut largest = AUDIO_ENUMERATE_REPLY_LEN;
    if AUDIO_OPEN_REPLY_LEN > largest {
        largest = AUDIO_OPEN_REPLY_LEN;
    }
    if AUDIO_CLOCK_REPLY_LEN > largest {
        largest = AUDIO_CLOCK_REPLY_LEN;
    }
    if AUDIO_STATE_REPLY_LEN > largest {
        largest = AUDIO_STATE_REPLY_LEN;
    }
    if AUDIO_STREAMS_REPLY_LEN > largest {
        largest = AUDIO_STREAMS_REPLY_LEN;
    }
    if crate::reply::STATUS_REPLY_LEN > largest {
        largest = crate::reply::STATUS_REPLY_LEN;
    }
    largest
}

const _: () = assert!(AUDIO_MAX_REPLY >= AUDIO_ENUMERATE_REPLY_LEN);
const _: () = assert!(AUDIO_MAX_REPLY >= AUDIO_OPEN_REPLY_LEN);
const _: () = assert!(AUDIO_MAX_REPLY >= AUDIO_CLOCK_REPLY_LEN);
const _: () = assert!(AUDIO_MAX_REPLY >= AUDIO_STATE_REPLY_LEN);
const _: () = assert!(AUDIO_MAX_REPLY >= AUDIO_STREAMS_REPLY_LEN);
const _: () = assert!(AUDIO_MAX_REPLY >= crate::reply::STATUS_REPLY_LEN);
const _: () = assert!(AUDIO_MAX_REQUEST >= HEADER_LEN + open::LEN);

/// Byte offsets within an [`AudioNotify`] frame.
mod notify {
    pub const KIND: usize = 6;
    pub const STATE: usize = 7;
    pub const STREAM: usize = 8;
    pub const POSITION: usize = 16;
    pub const LOST_FRAMES: usize = 24;
    pub const LEN: usize = 32;
}

/// Wire length of an [`AudioNotify`] frame.
pub const AUDIO_NOTIFY_LEN: usize = notify::LEN;

/// Wire byte for a space-available notification.
const NOTIFY_SPACE: u8 = 1;
/// Wire byte for a state-change notification.
const NOTIFY_STATE: u8 = 2;
/// Wire byte for an under/over-run notification.
const NOTIFY_XRUN: u8 = 3;

/// The service → client wake, sent to the stream's
/// [`StreamGrant::notify_endpoint`].
///
/// A client parks on this port in its wait set and never polls its ring.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum AudioNotify {
    /// The ring has room for more frames (playback), or frames have arrived in
    /// it (capture). `position` is the consumer position the service has
    /// reached.
    SpaceAvailable {
        /// The stream whose ring moved.
        stream_id: u64,
        /// The service's position in that ring.
        position: Frames,
    },
    /// The stream changed state, and here is the exact frame it changed at —
    /// so a seat switch resumes from the frame it paused on rather than near
    /// it.
    StateChanged {
        /// The stream that changed.
        stream_id: u64,
        /// Its new state.
        state: StreamState,
        /// The position it changed at.
        at: Frames,
    },
    /// Frames were lost. The position never lies, so a client resynchronises
    /// exactly rather than drifting.
    Xrun {
        /// The stream that lost frames.
        stream_id: u64,
        /// The position the loss started at.
        at: Frames,
        /// How many frames were lost.
        lost_frames: u64,
    },
}

impl AudioNotify {
    /// Encoded length of the notify frame.
    pub const WIRE_LEN: usize = AUDIO_NOTIFY_LEN;

    /// Encode the notify frame.
    #[must_use]
    pub fn encode(&self) -> [u8; AUDIO_NOTIFY_LEN] {
        let mut out = [0u8; AUDIO_NOTIFY_LEN];
        put_u32(&mut out, 0, AUDIO_NOTIFY_MAGIC);
        put_u16(&mut out, 4, AUDIO_VERSION_V1);
        match self {
            Self::SpaceAvailable {
                stream_id,
                position,
            } => {
                out[notify::KIND] = NOTIFY_SPACE;
                put_u64(&mut out, notify::STREAM, *stream_id);
                put_u64(&mut out, notify::POSITION, position.get());
            }
            Self::StateChanged {
                stream_id,
                state,
                at,
            } => {
                out[notify::KIND] = NOTIFY_STATE;
                out[notify::STATE] = state.as_u8();
                put_u64(&mut out, notify::STREAM, *stream_id);
                put_u64(&mut out, notify::POSITION, at.get());
            }
            Self::Xrun {
                stream_id,
                at,
                lost_frames,
            } => {
                out[notify::KIND] = NOTIFY_XRUN;
                put_u64(&mut out, notify::STREAM, *stream_id);
                put_u64(&mut out, notify::POSITION, at.get());
                put_u64(&mut out, notify::LOST_FRAMES, *lost_frames);
            }
        }
        out
    }

    /// Decode a notify frame, fail-closed.
    ///
    /// A field the notification's own kind does not define must be zero: a
    /// populated one would be a value the decoder never looked at.
    ///
    /// # Errors
    ///
    /// * [`Errno::BufferTooSmall`] — shorter than [`AUDIO_NOTIFY_LEN`].
    /// * [`Errno::BadMagic`] — wrong magic or a field the kind does not
    ///   define.
    /// * [`Errno::AbiVersionUnsupported`] — not [`AUDIO_VERSION_V1`].
    /// * [`Errno::OutOfRange`] — an unknown kind byte, a zero stream id, or an
    ///   undefined state.
    pub fn decode(bytes: &[u8]) -> Result<Self, Errno> {
        let Some(bytes) = bytes.get(..AUDIO_NOTIFY_LEN) else {
            return Err(Errno::BufferTooSmall);
        };
        if read_u32(bytes, 0) != AUDIO_NOTIFY_MAGIC {
            return Err(Errno::BadMagic);
        }
        if read_u16(bytes, 4) != AUDIO_VERSION_V1 {
            return Err(Errno::AbiVersionUnsupported);
        }
        let stream_id = checked_stream(read_u64(bytes, notify::STREAM))?;
        let position = Frames::new(read_u64(bytes, notify::POSITION));
        let lost_frames = read_u64(bytes, notify::LOST_FRAMES);
        match bytes[notify::KIND] {
            NOTIFY_SPACE => {
                if bytes[notify::STATE] != 0 || lost_frames != 0 {
                    return Err(Errno::BadMagic);
                }
                Ok(Self::SpaceAvailable {
                    stream_id,
                    position,
                })
            }
            NOTIFY_STATE => {
                if lost_frames != 0 {
                    return Err(Errno::BadMagic);
                }
                Ok(Self::StateChanged {
                    stream_id,
                    state: StreamState::from_u8(bytes[notify::STATE])?,
                    at: position,
                })
            }
            NOTIFY_XRUN => {
                if bytes[notify::STATE] != 0 {
                    return Err(Errno::BadMagic);
                }
                Ok(Self::Xrun {
                    stream_id,
                    at: position,
                    lost_frames,
                })
            }
            _ => Err(Errno::OutOfRange),
        }
    }
}

#[cfg(test)]
#[path = "audio_tests.rs"]
mod tests;
