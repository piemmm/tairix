//! `clock-v1`: the clock seam (`plans/SOUND.md` SND8, `plans/SUPPLIERS.md`
//! SL1's `Clock` role).
//!
//! A clock controller's registers set every clock on the chip, the cores' and
//! the memory's among them, so only the controller's driver maps them. It
//! serves one endpoint per controller node ([`CLOCK_CONTROLLER_ENDPOINTS`]),
//! bound under the node's [`LinkDuty`](crate::hwlink::LinkDuty), and a
//! consumer calls it naming the [`LinkRequest`] discovery gave it for its
//! clock.

use crate::hwlink::{LinkRequest, LinkRole};
use crate::hwtree::{HwResource, NodeEndpointBlock};
use crate::le::{put_u16, put_u32, put_u64, read_u16, read_u32, read_u64};
use crate::reply::{decode_status_reply, encode_status_reply, STATUS_REPLY_LEN};
use crate::Errno;

/// The endpoints clock controllers serve, indexed by the controller's node id.
/// Binding one takes the node's clock [`LinkDuty`](crate::hwlink::LinkDuty).
pub const CLOCK_CONTROLLER_ENDPOINTS: NodeEndpointBlock = NodeEndpointBlock::tagged(*b"CK");

/// Magic opening every request frame (`"CLKR"`).
pub const CLOCK_REQUEST_MAGIC: u32 = u32::from_le_bytes(*b"CLKR");

/// The `clock-v1` protocol version.
pub const CLOCK_VERSION_V1: u16 = 1;

/// A request's header: magic, version, operation and a reserved byte.
const HEADER_LEN: usize = 8;

/// A reply's header: the status word and a reserved word.
const REPLY_HEADER_LEN: usize = 8;

/// The operation a request carries.
#[repr(u8)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ClockOp {
    /// Report the clock's state.
    Describe = 1,
    /// Run the clock as near a rate as the controller can make it.
    Run = 2,
    /// Stop the clock and give it up.
    Release = 3,
}

impl ClockOp {
    const fn from_u8(byte: u8) -> Option<Self> {
        match byte {
            1 => Some(Self::Describe),
            2 => Some(Self::Run),
            3 => Some(Self::Release),
            _ => None,
        }
    }

    const fn request_body_len(self) -> usize {
        match self {
            Self::Describe | Self::Release => HwResource::WIRE_LEN,
            Self::Run => HwResource::WIRE_LEN + 8,
        }
    }

    const fn reply_body_len(self) -> usize {
        match self {
            Self::Describe => 16,
            Self::Run => 8,
            Self::Release => 0,
        }
    }
}

/// Largest request frame: a [`ClockRequest::Run`].
pub const CLOCK_MAX_REQUEST: usize = HEADER_LEN + HwResource::WIRE_LEN + 8;

/// Largest reply frame: a [`ClockOp::Describe`] report.
pub const CLOCK_MAX_REPLY: usize = REPLY_HEADER_LEN + 16;

/// A request to a clock controller's endpoint, naming the consumer's clock
/// link, which the controller believes only once the kernel confirms the
/// caller holds it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ClockRequest {
    /// Report the clock's state.
    Describe(LinkRequest),
    /// Run the clock as near `hz` as the controller can make it, which a
    /// clock another consumer holds can only run at the rate it already has.
    Run {
        /// The consumer's clock link.
        link: LinkRequest,
        /// The rate asked for, in Hz.
        hz: u64,
    },
    /// Stop the clock and give it up, so another consumer may set it.
    Release(LinkRequest),
}

/// A clock's state, as [`ClockOp::Describe`] reports it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ClockState {
    /// The rate it runs at, in Hz; `0` while it is stopped.
    pub hz: u64,
    /// Another consumer holds it, so it runs only at the rate it has.
    pub held_elsewhere: bool,
}

/// [`ClockState`]'s one flag.
const HELD_ELSEWHERE: u32 = 1;

impl ClockRequest {
    /// The operation the request carries.
    #[must_use]
    pub const fn op(&self) -> ClockOp {
        match self {
            Self::Describe(_) => ClockOp::Describe,
            Self::Run { .. } => ClockOp::Run,
            Self::Release(_) => ClockOp::Release,
        }
    }

    /// The clock link the request names.
    #[must_use]
    pub const fn link(&self) -> &LinkRequest {
        match self {
            Self::Describe(link) | Self::Release(link) | Self::Run { link, .. } => link,
        }
    }

    /// Encode the request into `out`, answering its length.
    ///
    /// # Errors
    ///
    /// [`Errno::BufferTooSmall`] if `out` cannot hold it.
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, Errno> {
        let op = self.op();
        let len = HEADER_LEN + op.request_body_len();
        let frame = out.get_mut(..len).ok_or(Errno::BufferTooSmall)?;
        frame.fill(0);
        put_u32(frame, 0, CLOCK_REQUEST_MAGIC);
        put_u16(frame, 4, CLOCK_VERSION_V1);
        frame[6] = op as u8;
        frame[HEADER_LEN..HEADER_LEN + HwResource::WIRE_LEN]
            .copy_from_slice(&HwResource::request(self.link()).to_le_bytes());
        if let Self::Run { hz, .. } = self {
            put_u64(frame, HEADER_LEN + HwResource::WIRE_LEN, *hz);
        }
        Ok(len)
    }

    /// Decode a request frame.
    ///
    /// # Errors
    ///
    /// * [`Errno::LengthOutOfRange`] — not exactly its operation's length.
    /// * [`Errno::BadMagic`] — a wrong magic, a dirty reserved byte, or a
    ///   quoted record that is not a canonical clock link.
    /// * [`Errno::AbiVersionUnsupported`] — not [`CLOCK_VERSION_V1`].
    /// * [`Errno::OutOfRange`] — an unknown operation.
    pub fn decode(bytes: &[u8]) -> Result<Self, Errno> {
        let header = bytes.get(..HEADER_LEN).ok_or(Errno::LengthOutOfRange)?;
        if read_u32(header, 0) != CLOCK_REQUEST_MAGIC || header[7] != 0 {
            return Err(Errno::BadMagic);
        }
        if read_u16(header, 4) != CLOCK_VERSION_V1 {
            return Err(Errno::AbiVersionUnsupported);
        }
        let op = ClockOp::from_u8(header[6]).ok_or(Errno::OutOfRange)?;
        if bytes.len() != HEADER_LEN + op.request_body_len() {
            return Err(Errno::LengthOutOfRange);
        }
        let record = HwResource::from_bytes(&bytes[HEADER_LEN..HEADER_LEN + HwResource::WIRE_LEN])
            .map_err(|_| Errno::BadMagic)?;
        let link = record.link_request().map_err(|_| Errno::BadMagic)?;
        if link.role() != LinkRole::Clock {
            return Err(Errno::BadMagic);
        }
        Ok(match op {
            ClockOp::Describe => Self::Describe(link),
            ClockOp::Release => Self::Release(link),
            ClockOp::Run => Self::Run {
                link,
                hz: read_u64(bytes, HEADER_LEN + HwResource::WIRE_LEN),
            },
        })
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

/// A success frame for `op`, its body zeroed, and its length.
fn success_frame(out: &mut [u8], op: ClockOp) -> Result<(&mut [u8], usize), Errno> {
    let len = REPLY_HEADER_LEN + op.reply_body_len();
    let frame = out.get_mut(..len).ok_or(Errno::BufferTooSmall)?;
    frame.fill(0);
    frame[..STATUS_REPLY_LEN].copy_from_slice(&encode_status_reply(Ok(())));
    Ok((&mut frame[REPLY_HEADER_LEN..], len))
}

/// The body of a success reply to `op`, or the refusal it carries.
fn success_body(bytes: &[u8], op: ClockOp) -> Result<&[u8], Errno> {
    decode_status_reply(bytes)?;
    if bytes.len() != REPLY_HEADER_LEN + op.reply_body_len()
        || read_u32(bytes, STATUS_REPLY_LEN) != 0
    {
        return Err(Errno::BadMagic);
    }
    Ok(&bytes[REPLY_HEADER_LEN..])
}

/// Frame the reply to [`ClockOp::Describe`].
///
/// # Errors
///
/// [`Errno::BufferTooSmall`] if `out` cannot hold it.
pub fn encode_describe_reply(out: &mut [u8], state: ClockState) -> Result<usize, Errno> {
    let (body, len) = success_frame(out, ClockOp::Describe)?;
    put_u64(body, 0, state.hz);
    put_u32(
        body,
        8,
        if state.held_elsewhere {
            HELD_ELSEWHERE
        } else {
            0
        },
    );
    Ok(len)
}

/// Decode the reply to [`ClockOp::Describe`].
///
/// # Errors
///
/// The refusal the reply carries, or [`Errno::BadMagic`] for a frame of the
/// wrong length or a set bit no field defines.
pub fn decode_describe_reply(bytes: &[u8]) -> Result<ClockState, Errno> {
    let body = success_body(bytes, ClockOp::Describe)?;
    let flags = read_u32(body, 8);
    if flags & !HELD_ELSEWHERE != 0 || read_u32(body, 12) != 0 {
        return Err(Errno::BadMagic);
    }
    Ok(ClockState {
        hz: read_u64(body, 0),
        held_elsewhere: flags & HELD_ELSEWHERE != 0,
    })
}

/// Frame the reply to [`ClockOp::Run`]: the rate the clock now runs at.
///
/// # Errors
///
/// [`Errno::BufferTooSmall`] if `out` cannot hold it.
pub fn encode_run_reply(out: &mut [u8], hz: u64) -> Result<usize, Errno> {
    let (body, len) = success_frame(out, ClockOp::Run)?;
    put_u64(body, 0, hz);
    Ok(len)
}

/// Decode the reply to [`ClockOp::Run`].
///
/// # Errors
///
/// The refusal the reply carries, or [`Errno::BadMagic`] for a frame of the
/// wrong length.
pub fn decode_run_reply(bytes: &[u8]) -> Result<u64, Errno> {
    success_body(bytes, ClockOp::Run).map(|body| read_u64(body, 0))
}

/// Frame the reply to [`ClockOp::Release`].
///
/// # Errors
///
/// [`Errno::BufferTooSmall`] if `out` cannot hold it.
pub fn encode_release_reply(out: &mut [u8]) -> Result<usize, Errno> {
    success_frame(out, ClockOp::Release).map(|(_, len)| len)
}

/// Decode the reply to [`ClockOp::Release`].
///
/// # Errors
///
/// The refusal the reply carries, or [`Errno::BadMagic`] for a frame of the
/// wrong length.
pub fn decode_release_reply(bytes: &[u8]) -> Result<(), Errno> {
    success_body(bytes, ClockOp::Release).map(|_| ())
}

#[cfg(test)]
#[path = "clock_tests.rs"]
mod tests;
