//! `encoder-v1`: a display engine's encoder (`plans/SOUND.md` `SND19l`, the
//! `Encoder` link role).
//!
//! An encoder — an HDMI transmitter — is a device of its own, driven by its
//! own driver, while the scan-out is the display engine's: the engine calls its
//! encoder's endpoint ([`ENCODER_ENDPOINTS`]), naming the [`LinkRequest`]
//! discovery gave it, to learn the connector and to take the encoder through
//! the phases of a mode set, which it interleaves with its own timing
//! generator's.
//!
//! The connector is the encoder's to know: whether a sink is connected and the
//! mode it runs for it, its own choice within the sink's and its own limits.
//! A posted [`EncoderRequest::Wait`] is answered when the connector moves.
//! The [`Phase`] order is the server's to hold, from the phase the hardware
//! was found in.

use crate::driver::display::Timing;
use crate::hwlink::{LinkRequest, LinkRole};
use crate::hwtree::{HwResource, NodeEndpointBlock};
use crate::le::{put_u16, put_u32, put_u64, read_u16, read_u32, read_u64};
use crate::reply::{decode_status_reply, encode_status_reply, STATUS_REPLY_LEN};
use crate::{DriverError, Errno};

/// The endpoints encoders serve, indexed by the encoder's node id. Binding one
/// takes the node's encoder [`LinkDuty`](crate::hwlink::LinkDuty).
pub const ENCODER_ENDPOINTS: NodeEndpointBlock = NodeEndpointBlock::tagged(*b"EN");

/// Magic opening every request frame (`"ENCR"`).
pub const ENCODER_REQUEST_MAGIC: u32 = u32::from_le_bytes(*b"ENCR");

/// The `encoder-v1` protocol version.
pub const ENCODER_VERSION_V1: u16 = 1;

/// A request's header: magic, version, operation and a reserved byte.
const HEADER_LEN: usize = 8;

/// A reply's header: the status word and a reserved word.
const REPLY_HEADER_LEN: usize = 8;

/// A request's fixed part: the header and the quoted link.
const LINKED_LEN: usize = HEADER_LEN + HwResource::WIRE_LEN;

/// A state reply's body: the count of moves (8), the flags (1), the phase (1),
/// a reserved pair (2), the link's generation (4) and the mode (24).
const STATE_LEN: usize = 16 + Timing::WIRE_LEN;

/// Largest request frame: a configure.
pub const ENCODER_MAX_REQUEST: usize = LINKED_LEN + Timing::WIRE_LEN;

/// Largest reply frame: a state report.
pub const ENCODER_MAX_REPLY: usize = REPLY_HEADER_LEN + STATE_LEN;

/// The operation a request carries.
#[repr(u8)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum EncoderOp {
    /// Report the connector and the phase.
    Describe = 1,
    /// Answer when the connector moves.
    Wait = 2,
    /// Set the encoder's clocks, PHY and timing up for a mode, before the
    /// engine configures its timing generator.
    Configure = 3,
    /// Set the colour conversion and the FIFO up, once the timing generator
    /// is configured and before its video starts.
    Prepare = 4,
    /// Start the output — video, the link's mode, `InfoFrames` — once the
    /// timing generator's video has started.
    Enable = 5,
    /// Stop the output, once the timing generator's video has stopped.
    Disable = 6,
    /// Power the PHY and clocks down, once the timing generator is reset.
    PowerDown = 7,
    /// Bound the connector's mode by what the engine's pipe carries.
    Limit = 8,
}

impl EncoderOp {
    const fn from_u8(byte: u8) -> Option<Self> {
        Some(match byte {
            1 => Self::Describe,
            2 => Self::Wait,
            3 => Self::Configure,
            4 => Self::Prepare,
            5 => Self::Enable,
            6 => Self::Disable,
            7 => Self::PowerDown,
            8 => Self::Limit,
            _ => return None,
        })
    }

    const fn request_len(self) -> usize {
        match self {
            Self::Wait | Self::Limit => LINKED_LEN + 8,
            Self::Configure => ENCODER_MAX_REQUEST,
            Self::Describe | Self::Prepare | Self::Enable | Self::Disable | Self::PowerDown => {
                LINKED_LEN
            }
        }
    }
}

/// Where an encoder's output is in a mode set.
#[repr(u8)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Phase {
    /// Powered down.
    Off = 0,
    /// Set up for a mode, its output not yet prepared.
    Configured = 1,
    /// Ready for the timing generator's video.
    Prepared = 2,
    /// Sending.
    Enabled = 3,
    /// Stopped, still powered.
    Disabled = 4,
}

impl Phase {
    const fn from_u8(byte: u8) -> Option<Self> {
        Some(match byte {
            0 => Self::Off,
            1 => Self::Configured,
            2 => Self::Prepared,
            3 => Self::Enabled,
            4 => Self::Disabled,
            _ => return None,
        })
    }

    /// The phase `op` takes the encoder to from `self`, or [`None`] when it
    /// is out of order there. A power-down also abandons a mode set part-way,
    /// and one with nothing powered is a step that changes nothing.
    #[must_use]
    pub const fn after(self, op: EncoderOp) -> Option<Self> {
        match (op, self) {
            (EncoderOp::Configure, Self::Off) => Some(Self::Configured),
            (EncoderOp::Prepare, Self::Configured) => Some(Self::Prepared),
            (EncoderOp::Enable, Self::Prepared) => Some(Self::Enabled),
            (EncoderOp::Disable, Self::Enabled) => Some(Self::Disabled),
            (
                EncoderOp::PowerDown,
                Self::Off | Self::Configured | Self::Prepared | Self::Disabled,
            ) => Some(Self::Off),
            (EncoderOp::Describe | EncoderOp::Wait | EncoderOp::Limit, phase) => Some(phase),
            _ => None,
        }
    }
}

/// The connector an encoder drives: whether a sink is connected, and the mode
/// it runs for that sink — [`None`] with no sink, or with one it can drive
/// none of the modes of.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Connector {
    /// A sink is connected.
    pub connected: bool,
    /// The mode the encoder runs for it.
    pub mode: Option<Timing>,
    /// Counts the times a sink lost what its running link set it up with —
    /// the scrambling an HDMI 2.0 sink forgets when it is replugged faster
    /// than its hotplug is read. An output brought up under an older count is
    /// brought up again, as DRM's link status asks of a compositor.
    pub link: u32,
}

/// What a display engine's pipe carries, which the connector's mode must fit:
/// the pixel clock its timing generator takes, and the rate its compositor
/// fills a line's active pixels at.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct PipeLimits {
    /// The fastest pixel clock, in kHz.
    pub max_clock_khz: u32,
    /// The fastest [`Timing::active_rate_khz`].
    pub max_active_khz: u32,
}

impl PipeLimits {
    /// Whether the pipe carries `timing`.
    #[must_use]
    pub fn carries(&self, timing: &Timing) -> bool {
        timing.clock_khz <= self.max_clock_khz && timing.active_rate_khz() <= self.max_active_khz
    }

    fn to_le_bytes(self) -> [u8; 8] {
        let mut out = [0u8; 8];
        put_u32(&mut out, 0, self.max_clock_khz);
        put_u32(&mut out, 4, self.max_active_khz);
        out
    }

    /// A pipe that carries nothing is no pipe.
    fn from_bytes(bytes: &[u8]) -> Result<Self, Errno> {
        let limits = Self {
            max_clock_khz: read_u32(bytes, 0),
            max_active_khz: read_u32(bytes, 4),
        };
        if limits.max_clock_khz == 0 || limits.max_active_khz == 0 {
            return Err(Errno::OutOfRange);
        }
        Ok(limits)
    }
}

/// An encoder as [`EncoderOp::Describe`] and [`EncoderOp::Wait`] report it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct EncoderState {
    /// How many times the connector has moved since the server started.
    pub moves: u64,
    /// The connector.
    pub connector: Connector,
    /// Where the output is in a mode set.
    pub phase: Phase,
}

/// [`EncoderState`]'s flag: a sink is connected.
const CONNECTED: u8 = 1 << 0;
/// [`EncoderState`]'s flag: the mode field carries a mode.
const HAS_MODE: u8 = 1 << 1;

/// The device an encoder's driver drives.
///
/// The server calls it only in the order [`Phase::after`] admits, and only
/// [`Self::limit`], [`Self::service_event`] and [`Self::expired`] may move the
/// connector, each answering whether it did. A step the device refuses leaves
/// the hardware in the phase it was in.
pub trait Encoder {
    /// The connector now.
    fn connector(&self) -> Connector;

    /// The phase the hardware is in, read once when the server starts: what
    /// the firmware, or an earlier driver, left running.
    fn phase(&self) -> Phase;

    /// Bound the connector's mode by what the engine's pipe carries,
    /// answering whether the connector moved: a mode it ran that the pipe
    /// does not carry gives way to the best one it does.
    ///
    /// # Errors
    ///
    /// The fault that leaves the encoder unable to choose a mode.
    fn limit(&mut self, limits: &PipeLimits) -> Result<bool, DriverError>;

    /// Collect what woke the encoder's event source at `index` — a sink
    /// arriving or leaving — answering whether the connector moved.
    ///
    /// A sink the encoder cannot read is a connector with no mode, not an
    /// error; every source that fired is quiet again on return.
    ///
    /// # Errors
    ///
    /// The fault that leaves the encoder unable to watch its connector at
    /// all, which ends the service.
    fn service_event(&mut self, index: usize) -> Result<bool, DriverError>;

    /// When, in microseconds on the monotonic clock
    /// [`Delay::now_us`](crate::Delay::now_us) reads, the encoder is next due
    /// [`Encoder::expired`]: a check its hardware raises no interrupt for.
    /// [`None`] while it awaits none.
    fn deadline_us(&self) -> Option<u64> {
        None
    }

    /// Its deadline came: it moves the deadline on or clears it, answering
    /// whether the connector moved.
    ///
    /// # Errors
    ///
    /// The fault that ends the service.
    fn expired(&mut self) -> Result<bool, DriverError> {
        Ok(false)
    }

    /// Set the clocks, PHY and timing up for `timing`, the connector's mode.
    ///
    /// # Errors
    ///
    /// The device's refusal; the encoder is left powered down.
    fn configure(&mut self, timing: &Timing) -> Result<(), DriverError>;

    /// Set the colour conversion and the FIFO up.
    ///
    /// # Errors
    ///
    /// The device's refusal.
    fn prepare(&mut self) -> Result<(), DriverError>;

    /// Start the output.
    ///
    /// # Errors
    ///
    /// The device's refusal.
    fn enable(&mut self) -> Result<(), DriverError>;

    /// Stop the output.
    ///
    /// # Errors
    ///
    /// The device's refusal.
    fn disable(&mut self) -> Result<(), DriverError>;

    /// Power the PHY and the clocks down.
    ///
    /// # Errors
    ///
    /// The device's refusal.
    fn power_down(&mut self) -> Result<(), DriverError>;
}

/// A request to an encoder's endpoint, naming the caller's encoder link,
/// which the encoder believes only once the kernel confirms the caller holds
/// it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum EncoderRequest {
    /// Report the connector and the phase.
    Describe(LinkRequest),
    /// Answer when the count of the connector's moves is other than `seen`.
    Wait {
        /// The caller's encoder link.
        link: LinkRequest,
        /// The count the caller last read.
        seen: u64,
    },
    /// Set up for `timing`, which must be the connector's mode.
    Configure {
        /// The caller's encoder link.
        link: LinkRequest,
        /// The mode.
        timing: Timing,
    },
    /// Prepare the output.
    Prepare(LinkRequest),
    /// Start the output.
    Enable(LinkRequest),
    /// Stop the output.
    Disable(LinkRequest),
    /// Power down.
    PowerDown(LinkRequest),
    /// Bound the connector's mode by what the caller's pipe carries.
    Limit {
        /// The caller's encoder link.
        link: LinkRequest,
        /// What the pipe carries.
        limits: PipeLimits,
    },
}

impl EncoderRequest {
    /// The operation the request carries.
    #[must_use]
    pub const fn op(&self) -> EncoderOp {
        match self {
            Self::Describe(_) => EncoderOp::Describe,
            Self::Wait { .. } => EncoderOp::Wait,
            Self::Configure { .. } => EncoderOp::Configure,
            Self::Prepare(_) => EncoderOp::Prepare,
            Self::Enable(_) => EncoderOp::Enable,
            Self::Disable(_) => EncoderOp::Disable,
            Self::PowerDown(_) => EncoderOp::PowerDown,
            Self::Limit { .. } => EncoderOp::Limit,
        }
    }

    /// The encoder link the request names.
    #[must_use]
    pub const fn link(&self) -> &LinkRequest {
        match self {
            Self::Describe(link)
            | Self::Prepare(link)
            | Self::Enable(link)
            | Self::Disable(link)
            | Self::PowerDown(link)
            | Self::Wait { link, .. }
            | Self::Configure { link, .. }
            | Self::Limit { link, .. } => link,
        }
    }

    /// Encode the request into `out`, answering its length.
    ///
    /// # Errors
    ///
    /// [`Errno::BufferTooSmall`] if `out` cannot hold it.
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, Errno> {
        let op = self.op();
        let len = op.request_len();
        let frame = out.get_mut(..len).ok_or(Errno::BufferTooSmall)?;
        frame.fill(0);
        put_u32(frame, 0, ENCODER_REQUEST_MAGIC);
        put_u16(frame, 4, ENCODER_VERSION_V1);
        frame[6] = op as u8;
        frame[HEADER_LEN..LINKED_LEN]
            .copy_from_slice(&HwResource::request(self.link()).to_le_bytes());
        match self {
            Self::Wait { seen, .. } => put_u64(frame, LINKED_LEN, *seen),
            Self::Configure { timing, .. } => {
                frame[LINKED_LEN..].copy_from_slice(&timing.to_le_bytes());
            }
            Self::Limit { limits, .. } => {
                frame[LINKED_LEN..].copy_from_slice(&limits.to_le_bytes());
            }
            Self::Describe(_)
            | Self::Prepare(_)
            | Self::Enable(_)
            | Self::Disable(_)
            | Self::PowerDown(_) => {}
        }
        Ok(len)
    }

    /// Decode a request frame.
    ///
    /// # Errors
    ///
    /// * [`Errno::LengthOutOfRange`] — not exactly its operation's length, or
    ///   a timing nothing could clock out.
    /// * [`Errno::BadMagic`] — a wrong magic, a dirty reserved byte or flag,
    ///   or a quoted record that is not a canonical encoder link.
    /// * [`Errno::AbiVersionUnsupported`] — not [`ENCODER_VERSION_V1`].
    /// * [`Errno::OutOfRange`] — an unknown operation, or a pipe that
    ///   carries nothing.
    pub fn decode(bytes: &[u8]) -> Result<Self, Errno> {
        let header = bytes.get(..HEADER_LEN).ok_or(Errno::LengthOutOfRange)?;
        if read_u32(header, 0) != ENCODER_REQUEST_MAGIC || header[7] != 0 {
            return Err(Errno::BadMagic);
        }
        if read_u16(header, 4) != ENCODER_VERSION_V1 {
            return Err(Errno::AbiVersionUnsupported);
        }
        let op = EncoderOp::from_u8(header[6]).ok_or(Errno::OutOfRange)?;
        if bytes.len() != op.request_len() {
            return Err(Errno::LengthOutOfRange);
        }
        let record =
            HwResource::from_bytes(&bytes[HEADER_LEN..LINKED_LEN]).map_err(|_| Errno::BadMagic)?;
        let link = record.link_request().map_err(|_| Errno::BadMagic)?;
        if link.role() != LinkRole::Encoder {
            return Err(Errno::BadMagic);
        }
        let argument = &bytes[LINKED_LEN..];
        Ok(match op {
            EncoderOp::Describe => Self::Describe(link),
            EncoderOp::Wait => Self::Wait {
                link,
                seen: read_u64(argument, 0),
            },
            EncoderOp::Configure => Self::Configure {
                link,
                timing: Timing::from_bytes(argument)?,
            },
            EncoderOp::Prepare => Self::Prepare(link),
            EncoderOp::Enable => Self::Enable(link),
            EncoderOp::Disable => Self::Disable(link),
            EncoderOp::PowerDown => Self::PowerDown(link),
            EncoderOp::Limit => Self::Limit {
                link,
                limits: PipeLimits::from_bytes(argument)?,
            },
        })
    }
}

/// The reason an encoder driver's refusal travels as, keeping apart a mode
/// the encoder cannot set, an encoder another engine holds, and a connector
/// that moved. A device's own [`DriverError::NotFound`] travels as a fault,
/// since the transport's says the encoder is gone.
#[must_use]
pub const fn refusal_reason(err: DriverError) -> Errno {
    match err {
        DriverError::Unsupported => Errno::NotSupported,
        DriverError::Busy => Errno::Busy,
        DriverError::NotFound => Errno::DeviceFault,
        other => other.as_errno(),
    }
}

/// The refusal a reply's reason stands for, as [`refusal_reason`] sent it.
/// [`Errno::NotFound`] is the transport's alone — the encoder is gone — and
/// any other reason it never sends is the encoder failing.
#[must_use]
pub const fn refusal(reason: Errno) -> DriverError {
    match reason {
        Errno::NotSupported => DriverError::Unsupported,
        Errno::Busy => DriverError::Busy,
        Errno::Stale => DriverError::Stale,
        Errno::OutOfRange => DriverError::OutOfRange,
        Errno::NotFound => DriverError::NotFound,
        other => DriverError::from_errno(other),
    }
}

/// Frame a refusal of a request.
///
/// # Errors
///
/// [`Errno::BufferTooSmall`] if `out` cannot hold the status word.
pub fn encode_error_reply(out: &mut [u8], err: Errno) -> Result<usize, Errno> {
    let frame = out
        .get_mut(..STATUS_REPLY_LEN)
        .ok_or(Errno::BufferTooSmall)?;
    frame.copy_from_slice(&encode_status_reply(Err(err)));
    Ok(STATUS_REPLY_LEN)
}

/// A success frame with a `body_len`-byte body, zeroed, and its length.
fn success_frame(out: &mut [u8], body_len: usize) -> Result<(&mut [u8], usize), Errno> {
    let len = REPLY_HEADER_LEN + body_len;
    let frame = out.get_mut(..len).ok_or(Errno::BufferTooSmall)?;
    frame.fill(0);
    frame[..STATUS_REPLY_LEN].copy_from_slice(&encode_status_reply(Ok(())));
    Ok((&mut frame[REPLY_HEADER_LEN..], len))
}

/// The `body_len`-byte body of a success reply, or the refusal it carries.
fn success_body(bytes: &[u8], body_len: usize) -> Result<&[u8], Errno> {
    decode_status_reply(bytes)?;
    if bytes.len() != REPLY_HEADER_LEN + body_len || read_u32(bytes, STATUS_REPLY_LEN) != 0 {
        return Err(Errno::BadMagic);
    }
    Ok(&bytes[REPLY_HEADER_LEN..])
}

/// Frame the reply to [`EncoderOp::Describe`] and [`EncoderOp::Wait`].
///
/// # Errors
///
/// [`Errno::BufferTooSmall`] if `out` cannot hold it.
pub fn encode_state_reply(out: &mut [u8], state: &EncoderState) -> Result<usize, Errno> {
    let (body, len) = success_frame(out, STATE_LEN)?;
    put_u64(body, 0, state.moves);
    let connector = state.connector;
    body[8] = (u8::from(connector.connected) * CONNECTED)
        | (u8::from(connector.mode.is_some()) * HAS_MODE);
    body[9] = state.phase as u8;
    put_u32(body, 12, connector.link);
    if let Some(mode) = connector.mode {
        body[16..].copy_from_slice(&mode.to_le_bytes());
    }
    Ok(len)
}

/// Decode the reply to [`EncoderOp::Describe`] and [`EncoderOp::Wait`].
///
/// # Errors
///
/// The refusal the reply carries, or [`Errno::BadMagic`] for a frame of the
/// wrong length, a dirty reserved run, an unknown flag or phase, a mode with
/// no sink connected, or a mode field that contradicts its flag; a mode
/// nothing could clock out is [`Errno::LengthOutOfRange`].
pub fn decode_state_reply(bytes: &[u8]) -> Result<EncoderState, Errno> {
    let body = success_body(bytes, STATE_LEN)?;
    let flags = body[8];
    if flags & !(CONNECTED | HAS_MODE) != 0
        || (flags & HAS_MODE != 0 && flags & CONNECTED == 0)
        || body[10..12].iter().any(|&b| b != 0)
    {
        return Err(Errno::BadMagic);
    }
    let phase = Phase::from_u8(body[9]).ok_or(Errno::BadMagic)?;
    let mode = if flags & HAS_MODE == 0 {
        if body[16..].iter().any(|&b| b != 0) {
            return Err(Errno::BadMagic);
        }
        None
    } else {
        Some(Timing::from_bytes(&body[16..])?)
    };
    Ok(EncoderState {
        moves: read_u64(body, 0),
        connector: Connector {
            connected: flags & CONNECTED != 0,
            mode,
            link: read_u32(body, 12),
        },
        phase,
    })
}

/// Frame the bare acknowledgement of a phase.
///
/// # Errors
///
/// [`Errno::BufferTooSmall`] if `out` cannot hold it.
pub fn encode_done_reply(out: &mut [u8]) -> Result<usize, Errno> {
    success_frame(out, 0).map(|(_, len)| len)
}

/// Decode the bare acknowledgement of a phase.
///
/// # Errors
///
/// The refusal the reply carries, or [`Errno::BadMagic`] for a frame of the
/// wrong length.
pub fn decode_done_reply(bytes: &[u8]) -> Result<(), Errno> {
    success_body(bytes, 0).map(|_| ())
}

#[cfg(test)]
#[path = "encoder_tests.rs"]
mod tests;
