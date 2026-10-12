//! `i2c-v1`: the I²C transaction seam (`plans/I2C.md` I1, I2).
//!
//! I²C carries no enumeration protocol: a controller cannot ask the bus what
//! is on it, and probing every address can be destructive, so discovery names
//! each target in the platform's tree. A controller's driver serves one
//! endpoint per controller node ([`I2C_CONTROLLER_ENDPOINTS`]) under the
//! node's `I2c` [`LinkDuty`](crate::hwlink::LinkDuty), and a consumer calls it
//! quoting the [`LinkRequest`] discovery gave it:
//!
//! - a **target link** names one address and reaches that target alone — the
//!   link a node on the bus holds to its parent;
//! - a **bus link** names none and reaches every address of a bus wired to
//!   one consumer, as an HDMI port's DDC bus is.
//!
//! A consumer opens its channel and is granted a buffer the controller made
//! for it; a transaction names messages by offset and length into that
//! buffer, and runs as one: START, a repeated START before each further
//! message, STOP.

use crate::hwlink::{LinkRequest, LinkRole, RegionGrant};
use crate::hwtree::{HwResource, NodeEndpointBlock};
use crate::le::{put_u16, put_u32, read_u16, read_u32};
use crate::reply::{decode_status_reply, encode_status_reply, STATUS_REPLY_LEN};
use crate::{DriverError, Errno};

/// The endpoints I²C controllers serve, indexed by the controller's node id.
/// Binding one takes the node's `I2c` [`LinkDuty`](crate::hwlink::LinkDuty).
pub const I2C_CONTROLLER_ENDPOINTS: NodeEndpointBlock = NodeEndpointBlock::tagged(*b"IC");

/// Magic opening every request frame (`"I2CR"`).
pub const I2C_REQUEST_MAGIC: u32 = u32::from_le_bytes(*b"I2CR");

/// The `i2c-v1` protocol version.
pub const I2C_VERSION_V1: u16 = 1;

/// The most messages one transaction carries: a defence, not a capacity.
pub const I2C_MAX_MESSAGES: usize = 8;

/// The most bytes one message moves: one HID report descriptor in a single
/// read, as HID over I²C requires.
pub const I2C_MAX_MESSAGE_LEN: usize = 4096;

/// Bytes of the buffer a channel is granted: a longest message and the writes
/// before it.
pub const I2C_BUFFER_LEN: usize = 2 * I2C_MAX_MESSAGE_LEN;

/// The speed a bus runs at when its node declares none: standard mode.
pub const STANDARD_MODE_HZ: u32 = 100_000;

/// A request's header: magic, version, operation and a reserved byte.
const HEADER_LEN: usize = 8;

/// A reply's header: the status word and a reserved word.
const REPLY_HEADER_LEN: usize = 8;

/// A message's wire form.
const MESSAGE_LEN: usize = 8;

/// A transfer's body before its messages: the link and the message count.
const TRANSFER_HEAD_LEN: usize = HwResource::WIRE_LEN + 4;

/// The longest request frame.
pub const I2C_MAX_REQUEST: usize = HEADER_LEN + TRANSFER_HEAD_LEN + I2C_MAX_MESSAGES * MESSAGE_LEN;

/// The longest reply frame.
pub const I2C_MAX_REPLY: usize = REPLY_HEADER_LEN + OPEN_REPLY_LEN;

/// An open reply's body: the bus's facts and the buffer's grant.
const OPEN_REPLY_LEN: usize = 8 + RegionGrant::WIRE_LEN;

/// A transfer reply's body: how the transaction ended.
const TRANSFER_REPLY_LEN: usize = 4;

/// Lowest address a device may answer to: everything below is reserved by the
/// I²C specification (general call, START byte, CBUS, Hs-mode master codes).
const FIRST_DEVICE_ADDRESS: u8 = 0x08;

/// Highest address a device may answer to: `1111 0xx` is the 10-bit
/// addressing escape and `1111 1xx` is the device-ID prefix.
const LAST_DEVICE_ADDRESS: u8 = 0x77;

/// A validated 7-bit I²C device address.
///
/// Construction refuses every address the I²C-bus specification reserves
/// (NXP UM10204 §3.1.12), so a transfer can never be aimed at the general
/// call, a Hs-mode master code, or the 10-bit escape. 10-bit addressing is out
/// of scope: no part this class reaches uses it, and admitting the escape as an
/// ordinary address would mis-address the bus.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct I2cAddress(u8);

impl I2cAddress {
    /// Validate `raw` as a 7-bit device address.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] for a reserved address or one wider than seven
    /// bits.
    pub const fn new(raw: u8) -> Result<Self, Errno> {
        if raw < FIRST_DEVICE_ADDRESS || raw > LAST_DEVICE_ADDRESS {
            return Err(Errno::OutOfRange);
        }
        Ok(Self(raw))
    }

    /// The address a target link's one selector cell names.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] if the cell is not a usable 7-bit address.
    pub fn from_cell(cell: u32) -> Result<Self, Errno> {
        Self::new(u8::try_from(cell).map_err(|_| Errno::OutOfRange)?)
    }

    /// The validated 7-bit address.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

/// What a link reaches on its bus.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum I2cReach {
    /// One target.
    Target(I2cAddress),
    /// Every address of a bus wired to one consumer.
    Bus,
}

impl I2cReach {
    /// What `link`'s selector names.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] for a link of another role, a reserved address,
    /// or a selector of more than one cell.
    pub fn of(link: &LinkRequest) -> Result<Self, Errno> {
        if link.role() != LinkRole::I2c {
            return Err(Errno::OutOfRange);
        }
        match *link.selector() {
            [] => Ok(Self::Bus),
            [cell] => I2cAddress::from_cell(cell).map(Self::Target),
            _ => Err(Errno::OutOfRange),
        }
    }

    /// Whether this reach includes `address`.
    #[must_use]
    pub fn reaches(self, address: I2cAddress) -> bool {
        match self {
            Self::Target(target) => target == address,
            Self::Bus => true,
        }
    }
}

/// Which way a message moves its bytes.
#[repr(u8)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Direction {
    /// From the buffer to the target.
    Write = 0,
    /// From the target into the buffer.
    Read = 1,
}

/// One message of a transaction: an address, a direction, and the bytes of
/// the channel's buffer it moves.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct I2cMessage {
    /// The target it addresses.
    pub address: I2cAddress,
    /// Which way it moves its bytes.
    pub direction: Direction,
    /// Where in the buffer its bytes start.
    pub offset: u16,
    /// How many bytes it moves.
    pub len: u16,
}

impl I2cMessage {
    /// The span of the buffer the message moves.
    #[must_use]
    pub const fn span(&self) -> core::ops::Range<usize> {
        self.offset as usize..self.offset as usize + self.len as usize
    }

    fn encode(self, out: &mut [u8; MESSAGE_LEN]) {
        out[0] = self.address.get();
        out[1] = self.direction as u8;
        put_u16(out, 4, self.offset);
        put_u16(out, 6, self.len);
    }

    fn decode(bytes: [u8; MESSAGE_LEN]) -> Result<Self, Errno> {
        if bytes[2] != 0 || bytes[3] != 0 {
            return Err(Errno::BadMagic);
        }
        let direction = match bytes[1] {
            0 => Direction::Write,
            1 => Direction::Read,
            _ => return Err(Errno::BadMagic),
        };
        Ok(Self {
            address: I2cAddress::new(bytes[0])?,
            direction,
            offset: read_u16(&bytes, 4),
            len: read_u16(&bytes, 6),
        })
    }
}

/// A validated transaction: one to [`I2C_MAX_MESSAGES`] messages, each within
/// [`I2C_MAX_MESSAGE_LEN`] and the channel's buffer, and no read empty.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct I2cTransaction {
    messages: [I2cMessage; I2C_MAX_MESSAGES],
    count: u8,
}

impl I2cTransaction {
    /// The transaction of `messages`, in order.
    ///
    /// A write may be empty, which addresses its target and moves nothing; a
    /// read may not, since no controller can end a read before its first
    /// byte.
    ///
    /// # Errors
    ///
    /// [`Errno::LengthOutOfRange`] for no message, more than
    /// [`I2C_MAX_MESSAGES`], an empty read, or a message longer than
    /// [`I2C_MAX_MESSAGE_LEN`] or reaching past [`I2C_BUFFER_LEN`].
    pub fn new(messages: &[I2cMessage]) -> Result<Self, Errno> {
        if messages.is_empty() || messages.len() > I2C_MAX_MESSAGES {
            return Err(Errno::LengthOutOfRange);
        }
        let mut validated = [messages[0]; I2C_MAX_MESSAGES];
        for (slot, message) in validated.iter_mut().zip(messages) {
            let len = usize::from(message.len);
            if len > I2C_MAX_MESSAGE_LEN
                || message.span().end > I2C_BUFFER_LEN
                || (len == 0 && message.direction == Direction::Read)
            {
                return Err(Errno::LengthOutOfRange);
            }
            *slot = *message;
        }
        Ok(Self {
            messages: validated,
            count: u8::try_from(messages.len()).map_err(|_| Errno::LengthOutOfRange)?,
        })
    }

    /// The transaction an [`I2cPort`] runs to write `write_len` bytes to
    /// `address` and read `read_len` back: the write at the buffer's start and
    /// the read after it, either alone when the other is empty, and an empty
    /// write when both are.
    ///
    /// # Errors
    ///
    /// [`Errno::LengthOutOfRange`] for a part [`new`](Self::new) refuses.
    pub fn write_then_read(
        address: I2cAddress,
        write_len: usize,
        read_len: usize,
    ) -> Result<Self, Errno> {
        let len = |bytes: usize| u16::try_from(bytes).map_err(|_| Errno::LengthOutOfRange);
        let (write, read) = (len(write_len)?, len(read_len)?);
        let message = |direction, offset, len| I2cMessage {
            address,
            direction,
            offset,
            len,
        };
        match (write, read) {
            (_, 0) => Self::new(&[message(Direction::Write, 0, write)]),
            (0, _) => Self::new(&[message(Direction::Read, 0, read)]),
            _ => Self::new(&[
                message(Direction::Write, 0, write),
                message(Direction::Read, write, read),
            ]),
        }
    }

    /// The messages, in order.
    #[must_use]
    pub fn messages(&self) -> &[I2cMessage] {
        &self.messages[..usize::from(self.count)]
    }
}

/// The repeated STARTs a bus can issue.
#[repr(u8)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum RepeatedStart {
    /// None: a transaction is one message.
    None = 0,
    /// After a write that moved bytes, before a further write or a read that
    /// ends the transaction: a controller that chains the next message onto
    /// the last byte of the write before it.
    BeforeFinalRead = 1,
    /// Before any message.
    Any = 2,
}

impl RepeatedStart {
    const fn from_u8(byte: u8) -> Option<Self> {
        match byte {
            0 => Some(Self::None),
            1 => Some(Self::BeforeFinalRead),
            2 => Some(Self::Any),
            _ => None,
        }
    }

    /// Whether a bus issuing these can run `transaction` as one.
    #[must_use]
    pub fn admits(self, transaction: &I2cTransaction) -> bool {
        let messages = transaction.messages();
        match self {
            Self::None => messages.len() == 1,
            Self::BeforeFinalRead => messages
                .iter()
                .rev()
                .skip(1)
                .all(|message| message.direction == Direction::Write && message.len != 0),
            Self::Any => true,
        }
    }
}

/// What a bus states about itself before any request.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct BusFacts {
    /// The most it runs at, in Hz.
    pub speed_hz: u32,
    /// The repeated STARTs it can issue.
    pub repeated_start: RepeatedStart,
}

/// A channel as its client holds it: the bus's facts and the grant of the
/// buffer its transactions move their bytes through.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct I2cChannel {
    /// What the bus states about itself.
    pub facts: BusFacts,
    /// The buffer the controller made for the channel.
    pub buffer: RegionGrant,
}

/// How a transaction that reached the bus failed, naming the message it ended
/// in.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum I2cFault {
    /// The message went unacknowledged: its address, or for a controller
    /// that tells them apart, a data byte.
    NoAck {
        /// The message's index.
        message: u8,
    },
    /// Another master won the bus.
    ArbitrationLost {
        /// The message's index.
        message: u8,
    },
    /// The bus faulted: a line held, or a target stretching the clock past
    /// the controller's limit.
    BusFault {
        /// The message's index.
        message: u8,
    },
    /// The transaction outlasted the bus's own deadline; the controller was
    /// reset.
    TimedOut {
        /// The message's index.
        message: u8,
    },
}

impl I2cFault {
    const NO_ACK: u8 = 1;
    const ARBITRATION_LOST: u8 = 2;
    const BUS_FAULT: u8 = 3;
    const TIMED_OUT: u8 = 4;

    const fn code(self) -> (u8, u8) {
        match self {
            Self::NoAck { message } => (Self::NO_ACK, message),
            Self::ArbitrationLost { message } => (Self::ARBITRATION_LOST, message),
            Self::BusFault { message } => (Self::BUS_FAULT, message),
            Self::TimedOut { message } => (Self::TIMED_OUT, message),
        }
    }

    const fn from_code(code: u8, message: u8) -> Option<Self> {
        match code {
            Self::NO_ACK => Some(Self::NoAck { message }),
            Self::ARBITRATION_LOST => Some(Self::ArbitrationLost { message }),
            Self::BUS_FAULT => Some(Self::BusFault { message }),
            Self::TIMED_OUT => Some(Self::TimedOut { message }),
            _ => None,
        }
    }

    /// The driver error a register-level port reports it as: a target that
    /// does not answer its first message is not found; one that answered and
    /// then refused, and any other fault, is the device's.
    #[must_use]
    pub const fn as_driver_error(self) -> DriverError {
        match self {
            Self::NoAck { message: 0 } => DriverError::NotFound,
            Self::NoAck { .. }
            | Self::ArbitrationLost { .. }
            | Self::BusFault { .. }
            | Self::TimedOut { .. } => DriverError::DeviceFault,
        }
    }
}

/// The operation a request carries.
#[repr(u8)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum I2cOp {
    /// Open the channel the link names.
    Open = 1,
    /// Run a transaction over it.
    Transfer = 2,
    /// Close it.
    Close = 3,
}

impl I2cOp {
    const fn from_u8(byte: u8) -> Option<Self> {
        match byte {
            1 => Some(Self::Open),
            2 => Some(Self::Transfer),
            3 => Some(Self::Close),
            _ => None,
        }
    }
}

/// A decoded request.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum I2cRequest {
    /// Open the channel `link` names.
    Open(LinkRequest),
    /// Run `transaction` over the channel `link` opened.
    Transfer {
        /// The channel's link.
        link: LinkRequest,
        /// The transaction.
        transaction: I2cTransaction,
    },
    /// Close the channel `link` opened.
    Close(LinkRequest),
}

impl I2cRequest {
    /// The operation the request carries.
    #[must_use]
    pub const fn op(&self) -> I2cOp {
        match self {
            Self::Open(_) => I2cOp::Open,
            Self::Transfer { .. } => I2cOp::Transfer,
            Self::Close(_) => I2cOp::Close,
        }
    }

    /// The link the request names.
    #[must_use]
    pub const fn link(&self) -> &LinkRequest {
        match self {
            Self::Open(link) | Self::Close(link) | Self::Transfer { link, .. } => link,
        }
    }

    /// Encode the request into `out`, answering its length.
    ///
    /// # Errors
    ///
    /// [`Errno::BufferTooSmall`] if `out` cannot hold it.
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, Errno> {
        let messages = match self {
            Self::Transfer { transaction, .. } => transaction.messages(),
            Self::Open(_) | Self::Close(_) => &[],
        };
        let len = match self {
            Self::Transfer { .. } => HEADER_LEN + TRANSFER_HEAD_LEN + messages.len() * MESSAGE_LEN,
            Self::Open(_) | Self::Close(_) => HEADER_LEN + HwResource::WIRE_LEN,
        };
        let frame = out.get_mut(..len).ok_or(Errno::BufferTooSmall)?;
        frame.fill(0);
        put_u32(frame, 0, I2C_REQUEST_MAGIC);
        put_u16(frame, 4, I2C_VERSION_V1);
        frame[6] = self.op() as u8;
        frame[HEADER_LEN..HEADER_LEN + HwResource::WIRE_LEN]
            .copy_from_slice(&HwResource::request(self.link()).to_le_bytes());
        if let Self::Transfer { transaction, .. } = self {
            put_u32(
                frame,
                HEADER_LEN + HwResource::WIRE_LEN,
                u32::from(transaction.count),
            );
            let (slots, _) = frame[HEADER_LEN + TRANSFER_HEAD_LEN..].as_chunks_mut::<MESSAGE_LEN>();
            for (slot, message) in slots.iter_mut().zip(messages) {
                message.encode(slot);
            }
        }
        Ok(len)
    }

    /// Decode a request frame.
    ///
    /// # Errors
    ///
    /// * [`Errno::LengthOutOfRange`] — not exactly its operation's length, or
    ///   a transaction [`I2cTransaction::new`] refuses.
    /// * [`Errno::BadMagic`] — a wrong magic, a dirty reserved byte, or a
    ///   quoted record that is not a canonical I²C link.
    /// * [`Errno::AbiVersionUnsupported`] — not [`I2C_VERSION_V1`].
    /// * [`Errno::OutOfRange`] — an unknown operation or a reserved address.
    pub fn decode(bytes: &[u8]) -> Result<Self, Errno> {
        let header = bytes.get(..HEADER_LEN).ok_or(Errno::LengthOutOfRange)?;
        if read_u32(header, 0) != I2C_REQUEST_MAGIC || header[7] != 0 {
            return Err(Errno::BadMagic);
        }
        if read_u16(header, 4) != I2C_VERSION_V1 {
            return Err(Errno::AbiVersionUnsupported);
        }
        let op = I2cOp::from_u8(header[6]).ok_or(Errno::OutOfRange)?;
        let record = bytes
            .get(HEADER_LEN..HEADER_LEN + HwResource::WIRE_LEN)
            .ok_or(Errno::LengthOutOfRange)?;
        let link = HwResource::from_bytes(record)
            .and_then(|record| record.link_request())
            .map_err(|_| Errno::BadMagic)?;
        if link.role() != LinkRole::I2c {
            return Err(Errno::BadMagic);
        }
        match op {
            I2cOp::Open | I2cOp::Close => {
                if bytes.len() != HEADER_LEN + HwResource::WIRE_LEN {
                    return Err(Errno::LengthOutOfRange);
                }
                Ok(if op == I2cOp::Open {
                    Self::Open(link)
                } else {
                    Self::Close(link)
                })
            }
            I2cOp::Transfer => {
                let count = bytes
                    .get(HEADER_LEN + HwResource::WIRE_LEN..HEADER_LEN + TRANSFER_HEAD_LEN)
                    .map(|count| read_u32(count, 0))
                    .ok_or(Errno::LengthOutOfRange)?;
                let count = usize::try_from(count)
                    .ok()
                    .filter(|&count| (1..=I2C_MAX_MESSAGES).contains(&count))
                    .ok_or(Errno::LengthOutOfRange)?;
                if bytes.len() != HEADER_LEN + TRANSFER_HEAD_LEN + count * MESSAGE_LEN {
                    return Err(Errno::LengthOutOfRange);
                }
                let mut messages = [I2cMessage {
                    address: I2cAddress(FIRST_DEVICE_ADDRESS),
                    direction: Direction::Write,
                    offset: 0,
                    len: 0,
                }; I2C_MAX_MESSAGES];
                let (wires, _) = bytes[HEADER_LEN + TRANSFER_HEAD_LEN..].as_chunks::<MESSAGE_LEN>();
                for (slot, wire) in messages.iter_mut().zip(wires) {
                    *slot = I2cMessage::decode(*wire)?;
                }
                Ok(Self::Transfer {
                    link,
                    transaction: I2cTransaction::new(&messages[..count])?,
                })
            }
        }
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

/// A success frame with a body of `body_len` zeroed bytes, and its length.
fn success_frame(out: &mut [u8], body_len: usize) -> Result<(&mut [u8], usize), Errno> {
    let len = REPLY_HEADER_LEN + body_len;
    let frame = out.get_mut(..len).ok_or(Errno::BufferTooSmall)?;
    frame.fill(0);
    frame[..STATUS_REPLY_LEN].copy_from_slice(&encode_status_reply(Ok(())));
    Ok((&mut frame[REPLY_HEADER_LEN..], len))
}

/// The body of a success reply of `body_len` bytes, or the refusal it carries.
fn success_body(bytes: &[u8], body_len: usize) -> Result<&[u8], Errno> {
    decode_status_reply(bytes)?;
    if bytes.len() != REPLY_HEADER_LEN + body_len || read_u32(bytes, STATUS_REPLY_LEN) != 0 {
        return Err(Errno::BadMagic);
    }
    Ok(&bytes[REPLY_HEADER_LEN..])
}

/// Frame the reply to [`I2cOp::Open`].
///
/// # Errors
///
/// [`Errno::BufferTooSmall`] if `out` cannot hold it.
pub fn encode_open_reply(out: &mut [u8], channel: &I2cChannel) -> Result<usize, Errno> {
    let (body, len) = success_frame(out, OPEN_REPLY_LEN)?;
    put_u32(body, 0, channel.facts.speed_hz);
    body[4] = channel.facts.repeated_start as u8;
    body[8..].copy_from_slice(&channel.buffer.to_le_bytes());
    Ok(len)
}

/// Decode the reply to [`I2cOp::Open`].
///
/// # Errors
///
/// The refusal the reply carries, [`Errno::OutOfRange`] for a bus stating no
/// speed or an unusable grant, or [`Errno::BadMagic`] for a malformed frame.
pub fn decode_open_reply(bytes: &[u8]) -> Result<I2cChannel, Errno> {
    let body = success_body(bytes, OPEN_REPLY_LEN)?;
    if body[5..8].iter().any(|&byte| byte != 0) {
        return Err(Errno::BadMagic);
    }
    let repeated_start = RepeatedStart::from_u8(body[4]).ok_or(Errno::BadMagic)?;
    let speed_hz = read_u32(body, 0);
    if speed_hz == 0 {
        return Err(Errno::OutOfRange);
    }
    Ok(I2cChannel {
        facts: BusFacts {
            speed_hz,
            repeated_start,
        },
        buffer: RegionGrant::from_bytes(&body[8..])?,
    })
}

/// Frame the reply to [`I2cOp::Transfer`]: completed, or the fault that
/// ended it.
///
/// # Errors
///
/// [`Errno::BufferTooSmall`] if `out` cannot hold it.
pub fn encode_transfer_reply(
    out: &mut [u8],
    outcome: Result<(), I2cFault>,
) -> Result<usize, Errno> {
    let (body, len) = success_frame(out, TRANSFER_REPLY_LEN)?;
    if let Err(fault) = outcome {
        let (code, message) = fault.code();
        body[0] = code;
        body[1] = message;
    }
    Ok(len)
}

/// Decode the reply to [`I2cOp::Transfer`]: the refusal, or how the
/// transaction ended.
///
/// # Errors
///
/// The refusal the reply carries, or [`Errno::BadMagic`] for a malformed
/// frame.
pub fn decode_transfer_reply(bytes: &[u8]) -> Result<Result<(), I2cFault>, Errno> {
    let body = success_body(bytes, TRANSFER_REPLY_LEN)?;
    if body[2] != 0 || body[3] != 0 {
        return Err(Errno::BadMagic);
    }
    match (body[0], body[1]) {
        (0, 0) => Ok(Ok(())),
        (code, message) => I2cFault::from_code(code, message)
            .filter(|_| usize::from(message) < I2C_MAX_MESSAGES)
            .map(Err)
            .ok_or(Errno::BadMagic),
    }
}

/// Frame the reply to [`I2cOp::Close`].
///
/// # Errors
///
/// [`Errno::BufferTooSmall`] if `out` cannot hold it.
pub fn encode_close_reply(out: &mut [u8]) -> Result<usize, Errno> {
    success_frame(out, 0).map(|(_, len)| len)
}

/// Decode the reply to [`I2cOp::Close`].
///
/// # Errors
///
/// The refusal the reply carries, or [`Errno::BadMagic`] for a malformed
/// frame.
pub fn decode_close_reply(bytes: &[u8]) -> Result<(), Errno> {
    success_body(bytes, 0).map(|_| ())
}

/// The transfer path to **one** addressed part: a write, then a read, in one
/// transaction.
///
/// That is what every register-addressed part needs: the write carries the
/// register pointer and the read takes the block back. There is no address
/// argument: a port is a target channel, which reaches exactly one part, so a
/// chip driver has nowhere to name a neighbour.
pub trait I2cPort {
    /// Run one write-then-read transaction against this part.
    ///
    /// An empty `write` skips the write and an empty `read` skips the read;
    /// both empty addresses the part and stops, which is how a caller checks
    /// that it answers at all. On `Ok` the whole of `read` has been filled.
    ///
    /// # Errors
    ///
    /// * [`DriverError::LengthOutOfRange`] if either part exceeds
    ///   [`I2C_MAX_MESSAGE_LEN`], or both the channel's buffer.
    /// * [`DriverError::NotFound`] if the part did not acknowledge its
    ///   address.
    /// * [`DriverError::DeviceFault`] for any other bus fault.
    /// * [`DriverError::PermissionDenied`] if the caller may not reach the
    ///   controller.
    fn transfer(&mut self, write: &[u8], read: &mut [u8]) -> Result<(), DriverError>;
}

/// A borrowed port is a port, so a caller can keep the part and lend the
/// driver it.
impl<T: I2cPort + ?Sized> I2cPort for &mut T {
    fn transfer(&mut self, write: &[u8], read: &mut [u8]) -> Result<(), DriverError> {
        (**self).transfer(write, read)
    }
}

#[cfg(test)]
#[path = "i2c_tests.rs"]
mod tests;
