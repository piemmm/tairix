//! `irqline-v1`: an interrupt line through a controller other than the
//! platform's root one (`plans/SUPPLIERS.md` SL4's `InterruptLine` role,
//! `plans/GPIO.md` G1).
//!
//! A second-level interrupt controller — an L2 aggregator, a GPIO block —
//! collects lines the root controller never sees, so only its driver maps it.
//! It serves one endpoint per controller node ([`INTERRUPT_LINE_ENDPOINTS`]),
//! bound under the node's [`LinkDuty`](crate::hwlink::LinkDuty), and a
//! consumer calls it naming the [`LinkRequest`] discovery gave it for its
//! line. A `Wait` is answered when the line fires; the line stays masked from
//! then until the consumer waits again, so a level line is serviced before it
//! can fire again, and an edge that arrives meanwhile answers the next `Wait`
//! at once.

use crate::hwlink::{LinkRequest, LinkRole};
use crate::hwtree::{HwResource, NodeEndpointBlock};
use crate::le::{put_u16, put_u32, read_u16, read_u32};
use crate::reply::{decode_status_reply, encode_status_reply, STATUS_REPLY_LEN};
use crate::Errno;

/// The endpoints second-level interrupt controllers serve, indexed by the
/// controller's node id. Binding one takes the node's interrupt-line
/// [`LinkDuty`](crate::hwlink::LinkDuty).
pub const INTERRUPT_LINE_ENDPOINTS: NodeEndpointBlock = NodeEndpointBlock::tagged(*b"IL");

/// Magic opening every request frame (`"ILNR"`).
pub const IRQLINE_REQUEST_MAGIC: u32 = u32::from_le_bytes(*b"ILNR");

/// The `irqline-v1` protocol version.
pub const IRQLINE_VERSION_V1: u16 = 1;

/// A request's header: magic, version, operation and a reserved byte.
const HEADER_LEN: usize = 8;

/// The operation a request carries.
#[repr(u8)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum IrqLineOp {
    /// Unmask the line and answer when it fires.
    Wait = 1,
}

impl IrqLineOp {
    const fn from_u8(byte: u8) -> Option<Self> {
        match byte {
            1 => Some(Self::Wait),
            _ => None,
        }
    }
}

/// Every request frame's length: the header and the quoted link.
pub const IRQLINE_MAX_REQUEST: usize = HEADER_LEN + HwResource::WIRE_LEN;

/// Every reply frame's length: the status word alone.
pub const IRQLINE_MAX_REPLY: usize = STATUS_REPLY_LEN;

/// A request to a controller's endpoint, naming the consumer's line, which
/// the controller believes only once the kernel confirms the caller holds it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum IrqLineRequest {
    /// Unmask the line and answer when it fires.
    Wait(LinkRequest),
}

impl IrqLineRequest {
    /// The operation the request carries.
    #[must_use]
    pub const fn op(&self) -> IrqLineOp {
        match self {
            Self::Wait(_) => IrqLineOp::Wait,
        }
    }

    /// The line the request names.
    #[must_use]
    pub const fn link(&self) -> &LinkRequest {
        match self {
            Self::Wait(link) => link,
        }
    }

    /// Encode the request into `out`, answering its length.
    ///
    /// # Errors
    ///
    /// [`Errno::BufferTooSmall`] if `out` cannot hold it.
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, Errno> {
        let frame = out
            .get_mut(..IRQLINE_MAX_REQUEST)
            .ok_or(Errno::BufferTooSmall)?;
        frame.fill(0);
        put_u32(frame, 0, IRQLINE_REQUEST_MAGIC);
        put_u16(frame, 4, IRQLINE_VERSION_V1);
        frame[6] = self.op() as u8;
        frame[HEADER_LEN..].copy_from_slice(&HwResource::request(self.link()).to_le_bytes());
        Ok(IRQLINE_MAX_REQUEST)
    }

    /// Decode a request frame.
    ///
    /// # Errors
    ///
    /// * [`Errno::LengthOutOfRange`] — not exactly a request's length.
    /// * [`Errno::BadMagic`] — a wrong magic, a dirty reserved byte, or a
    ///   quoted record that is not a canonical interrupt-line link.
    /// * [`Errno::AbiVersionUnsupported`] — not [`IRQLINE_VERSION_V1`].
    /// * [`Errno::OutOfRange`] — an unknown operation.
    pub fn decode(bytes: &[u8]) -> Result<Self, Errno> {
        let header = bytes.get(..HEADER_LEN).ok_or(Errno::LengthOutOfRange)?;
        if read_u32(header, 0) != IRQLINE_REQUEST_MAGIC || header[7] != 0 {
            return Err(Errno::BadMagic);
        }
        if read_u16(header, 4) != IRQLINE_VERSION_V1 {
            return Err(Errno::AbiVersionUnsupported);
        }
        let op = IrqLineOp::from_u8(header[6]).ok_or(Errno::OutOfRange)?;
        if bytes.len() != IRQLINE_MAX_REQUEST {
            return Err(Errno::LengthOutOfRange);
        }
        let record = HwResource::from_bytes(&bytes[HEADER_LEN..]).map_err(|_| Errno::BadMagic)?;
        let link = record.link_request().map_err(|_| Errno::BadMagic)?;
        if link.role() != LinkRole::InterruptLine {
            return Err(Errno::BadMagic);
        }
        Ok(match op {
            IrqLineOp::Wait => Self::Wait(link),
        })
    }
}

/// Frame the answer to a request: its refusal, or for a [`IrqLineOp::Wait`]
/// that the line fired.
///
/// # Errors
///
/// [`Errno::BufferTooSmall`] if `out` cannot hold the status word.
pub fn encode_reply(out: &mut [u8], outcome: Result<(), Errno>) -> Result<usize, Errno> {
    let frame = out
        .get_mut(..STATUS_REPLY_LEN)
        .ok_or(Errno::BufferTooSmall)?;
    frame.copy_from_slice(&encode_status_reply(outcome));
    Ok(STATUS_REPLY_LEN)
}

/// Decode the answer to a request.
///
/// # Errors
///
/// The refusal the reply carries, or [`Errno::BadMagic`] for a frame of the
/// wrong length.
pub fn decode_reply(bytes: &[u8]) -> Result<(), Errno> {
    if bytes.len() != STATUS_REPLY_LEN {
        return Err(Errno::BadMagic);
    }
    decode_status_reply(bytes)
}

#[cfg(test)]
#[path = "irqline_tests.rs"]
mod tests;
