//! The bus-agnostic USB transport IPC protocol (`plans/USB.md` §1.3, U2;
//! `plans/SOUND.md` SND6 for the isochronous half).
//!
//! The modular USB stack splits the host-controller driver (HCD) from the
//! per-interface class drivers: the HCD owns one controller (its registers,
//! DMA rings, and root-hub ports) and serves a **transport call endpoint** per
//! USB interface it emits into the hardware tree; a class driver binds that
//! emitted interface node and submits requests over the endpoint, touching no
//! controller register and no other interface's buffer.
//!
//! A request is one of:
//!
//! * a **URB** ([`UrbRequest`]) — one queued control, interrupt or bulk
//!   transfer over the node's shared buffer, answered with its completion;
//! * an **interface operation** — selecting an interface's alternate setting,
//!   which only the HCD may do because it reprograms the controller's
//!   endpoint contexts and reserves bus bandwidth, or claiming a sibling
//!   interface the device publishes no node for (a streaming interface whose
//!   function the bound interface governs, as USB Audio and Video arrange
//!   them);
//! * an **isochronous stream** operation. An isochronous endpoint moves a
//!   fixed budget every service interval whether or not anyone asked, so it is
//!   not a URB at all: the class driver starts a stream over a region of
//!   [`IsoLayout`] slots the HCD creates, queues each slot as it fills (OUT)
//!   or empties (IN) it, and is told each slot's completion — its bus frame,
//!   when it finished, and which service intervals it missed — on a port
//!   derived from its own attested pid ([`iso_notify_endpoint_for`]).
//!
//! Every frame decodes only from its canonical encoding at its exact length,
//! so a request can be re-encoded to the very bytes that carried it.

use core::num::NonZeroU32;

use crate::le::{put_i32, put_u16, put_u32, put_u64, read_i32, read_u16, read_u32, read_u64};
use crate::reply::STATUS_REPLY_LEN;
use crate::{Errno, ProcId, PROC_ID_LEN};

/// Highest USB endpoint *number* an interface can address (USB 2.0 §9.6.6:
/// `bEndpointAddress` carries a 4-bit endpoint number, so `1..=15` are
/// device endpoints and `0` is the shared control endpoint). A validation
/// bound on an untrusted field, not a scalable capacity.
pub const MAX_ENDPOINT: u8 = 15;

/// Direction of a URB's data stage, from the host's point of view.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum UsbDirection {
    /// Host → device (an OUT transfer; the data buffer is read by the HCD).
    Out = 0,
    /// Device → host (an IN transfer; the data buffer is written by the HCD).
    In = 1,
}

impl UsbDirection {
    /// The wire byte for this direction.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// Recover a direction from its wire byte.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] if `value` is neither [`Self::Out`] nor
    /// [`Self::In`] (fail closed on a malformed field).
    pub const fn from_u8(value: u8) -> Result<Self, Errno> {
        match value {
            0 => Ok(Self::Out),
            1 => Ok(Self::In),
            _ => Err(Errno::OutOfRange),
        }
    }

    /// The direction a `bEndpointAddress` names (bit 7).
    #[must_use]
    pub const fn of_address(address: u8) -> Self {
        if address & ENDPOINT_ADDRESS_IN != 0 {
            Self::In
        } else {
            Self::Out
        }
    }
}

/// `bEndpointAddress` bit 7: the endpoint moves data device → host.
pub const ENDPOINT_ADDRESS_IN: u8 = 0x80;

/// Whether `address` is a well-formed device-endpoint `bEndpointAddress`:
/// a number `1..=`[`MAX_ENDPOINT`], a direction bit, and nothing else.
#[must_use]
pub const fn is_device_endpoint_address(address: u8) -> bool {
    let number = address & 0x0F;
    number != 0 && address & 0x70 == 0
}

/// USB transfer type of a URB (USB 2.0 §9.6.6 `bmAttributes` transfer-type
/// field). Isochronous transfers are a stream ([`IsoStartParams`]), never a
/// URB, so a URB naming one is refused.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum UsbTransferType {
    /// A control transfer on endpoint 0 (SETUP + optional data + status).
    Control = 0,
    /// An interrupt transfer on a device endpoint (e.g. an HID report).
    Interrupt = 1,
    /// A bulk transfer on a device endpoint.
    Bulk = 2,
}

impl UsbTransferType {
    /// The wire byte for this transfer type.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// Recover a transfer type from its wire byte.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] if `value` is not a known transfer type (fail
    /// closed on any future value).
    pub const fn from_u8(value: u8) -> Result<Self, Errno> {
        match value {
            0 => Ok(Self::Control),
            1 => Ok(Self::Interrupt),
            2 => Ok(Self::Bulk),
            _ => Err(Errno::OutOfRange),
        }
    }
}

/// A USB request block: one queued transfer over the node's shared buffer.
///
/// The transfer's payload lives in the shared-memory buffer the node carries;
/// the request carries only the transfer's shape. The HCD validates every
/// field against the interface before it touches a ring (`plans/USB.md`
/// §1.3) and fails closed.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct UrbRequest {
    /// The endpoint *number* within the interface (`0` is the control
    /// endpoint; `1..=`[`MAX_ENDPOINT`] are device endpoints). The direction
    /// is carried explicitly in [`Self::direction`] rather than folded into a
    /// `bEndpointAddress` bit, so there is one source of truth for it.
    pub endpoint: u8,
    /// The transfer type.
    pub transfer_type: UsbTransferType,
    /// The data-stage direction.
    pub direction: UsbDirection,
    /// Number of bytes to transfer, never larger than the shared buffer (the
    /// HCD re-checks this against its mapping).
    pub length: u32,
    /// The 8-byte SETUP packet, meaningful only for a
    /// [`UsbTransferType::Control`] transfer (zero-filled otherwise).
    pub setup: [u8; 8],
}

/// The parameters of an isochronous stream ([`UsbRequest::IsoStart`]).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct IsoStartParams {
    /// The isochronous endpoint's `bEndpointAddress`, in the alternate
    /// setting its interface currently has selected.
    pub endpoint: u8,
    /// The stream region's slot geometry.
    pub layout: IsoLayout,
}

/// One request on a transport endpoint.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum UsbRequest {
    /// Run one transfer.
    Transfer(UrbRequest),
    /// Select `alternate` on `interface`, which must be the node's own or one
    /// it claimed. Every stream on the interface's current setting must have
    /// been stopped.
    SetInterface {
        /// `bInterfaceNumber`.
        interface: u8,
        /// `bAlternateSetting`.
        alternate: u8,
    },
    /// Govern `interface` of the same device, which no node of its own
    /// serves.
    ClaimInterface {
        /// `bInterfaceNumber`.
        interface: u8,
    },
    /// Start an isochronous stream.
    IsoStart(IsoStartParams),
    /// Hand slot `slot` of the stream on `endpoint` to the controller: filled
    /// with data (OUT) or emptied for the next reception (IN).
    IsoQueue {
        /// The stream's `bEndpointAddress`.
        endpoint: u8,
        /// The slot index, below the stream's [`IsoLayout::slots`].
        slot: u16,
    },
    /// Stop the stream on `endpoint` and release its region.
    IsoStop {
        /// The stream's `bEndpointAddress`.
        endpoint: u8,
    },
}

/// Operation bytes, each request's first byte.
mod op {
    pub const TRANSFER: u8 = 1;
    pub const SET_INTERFACE: u8 = 2;
    pub const CLAIM_INTERFACE: u8 = 3;
    pub const ISO_START: u8 = 4;
    pub const ISO_QUEUE: u8 = 5;
    pub const ISO_STOP: u8 = 6;
}

/// Encoded length of a [`UsbRequest::Transfer`]: `op(1) || endpoint(1) ||
/// transfer_type(1) || direction(1) || length(4) || setup(8)`.
pub const URB_REQUEST_LEN: usize = 16;

/// Encoded length of a [`UsbRequest::SetInterface`].
const SET_INTERFACE_LEN: usize = 3;
/// Encoded length of a [`UsbRequest::ClaimInterface`].
const CLAIM_INTERFACE_LEN: usize = 2;
/// Encoded length of a [`UsbRequest::IsoStart`]: `op(1) || endpoint(1) ||
/// slots(2) || packets(2) || pad(2) || packet_bytes(4)`.
const ISO_START_LEN: usize = 12;
/// Encoded length of a [`UsbRequest::IsoQueue`].
const ISO_QUEUE_LEN: usize = 4;
/// Encoded length of a [`UsbRequest::IsoStop`].
const ISO_STOP_LEN: usize = 2;

/// The longest request frame, and so the transport endpoint's request bound.
pub const USB_REQUEST_MAX_LEN: usize = URB_REQUEST_LEN;

const _: () = assert!(
    URB_REQUEST_LEN >= ISO_START_LEN
        && URB_REQUEST_LEN >= SET_INTERFACE_LEN
        && URB_REQUEST_LEN >= ISO_QUEUE_LEN
);

impl UsbRequest {
    /// Encode `self` into `buf`, returning the number of bytes written.
    ///
    /// # Errors
    ///
    /// [`Errno::BufferTooSmall`] if `buf` cannot hold the frame.
    pub fn encode(&self, buf: &mut [u8]) -> Result<usize, Errno> {
        let len = self.encoded_len();
        let out = buf.get_mut(..len).ok_or(Errno::BufferTooSmall)?;
        out.fill(0);
        match *self {
            Self::Transfer(urb) => {
                out[0] = op::TRANSFER;
                out[1] = urb.endpoint;
                out[2] = urb.transfer_type.as_u8();
                out[3] = urb.direction.as_u8();
                put_u32(out, 4, urb.length);
                out[8..16].copy_from_slice(&urb.setup);
            }
            Self::SetInterface {
                interface,
                alternate,
            } => {
                out[0] = op::SET_INTERFACE;
                out[1] = interface;
                out[2] = alternate;
            }
            Self::ClaimInterface { interface } => {
                out[0] = op::CLAIM_INTERFACE;
                out[1] = interface;
            }
            Self::IsoStart(params) => {
                out[0] = op::ISO_START;
                out[1] = params.endpoint;
                put_u16(out, 2, params.layout.slots);
                put_u16(out, 4, params.layout.packets);
                put_u32(out, 8, params.layout.packet_bytes);
            }
            Self::IsoQueue { endpoint, slot } => {
                out[0] = op::ISO_QUEUE;
                out[1] = endpoint;
                put_u16(out, 2, slot);
            }
            Self::IsoStop { endpoint } => {
                out[0] = op::ISO_STOP;
                out[1] = endpoint;
            }
        }
        Ok(len)
    }

    /// The frame length [`Self::encode`] writes.
    #[must_use]
    pub const fn encoded_len(&self) -> usize {
        match self {
            Self::Transfer(_) => URB_REQUEST_LEN,
            Self::SetInterface { .. } => SET_INTERFACE_LEN,
            Self::ClaimInterface { .. } => CLAIM_INTERFACE_LEN,
            Self::IsoStart(_) => ISO_START_LEN,
            Self::IsoQueue { .. } => ISO_QUEUE_LEN,
            Self::IsoStop { .. } => ISO_STOP_LEN,
        }
    }

    /// Decode a request, validating every field its encoding fixes.
    ///
    /// This rejects a malformed *encoding*; the HCD performs the further
    /// *semantic* checks that need the live interface (the endpoint belongs
    /// to it, the length fits the buffer) before it acts.
    ///
    /// # Errors
    ///
    /// * [`Errno::LengthOutOfRange`] if `bytes` is not exactly its op's frame
    ///   length.
    /// * [`Errno::OutOfRange`] for an unknown op, transfer type or direction,
    ///   an endpoint that is not a device endpoint's, a non-zero pad byte, or
    ///   an [`IsoLayout`] outside its bounds.
    pub fn decode(bytes: &[u8]) -> Result<Self, Errno> {
        let op = *bytes.first().ok_or(Errno::LengthOutOfRange)?;
        let expect = match op {
            op::TRANSFER => URB_REQUEST_LEN,
            op::SET_INTERFACE => SET_INTERFACE_LEN,
            op::CLAIM_INTERFACE => CLAIM_INTERFACE_LEN,
            op::ISO_START => ISO_START_LEN,
            op::ISO_QUEUE => ISO_QUEUE_LEN,
            op::ISO_STOP => ISO_STOP_LEN,
            _ => return Err(Errno::OutOfRange),
        };
        if bytes.len() != expect {
            return Err(Errno::LengthOutOfRange);
        }
        let stream_endpoint = |address: u8| {
            if is_device_endpoint_address(address) {
                Ok(address)
            } else {
                Err(Errno::OutOfRange)
            }
        };
        Ok(match op {
            op::TRANSFER => {
                let endpoint = bytes[1];
                if endpoint > MAX_ENDPOINT {
                    return Err(Errno::OutOfRange);
                }
                let mut setup = [0u8; 8];
                setup.copy_from_slice(&bytes[8..16]);
                Self::Transfer(UrbRequest {
                    endpoint,
                    transfer_type: UsbTransferType::from_u8(bytes[2])?,
                    direction: UsbDirection::from_u8(bytes[3])?,
                    length: read_u32(bytes, 4),
                    setup,
                })
            }
            op::SET_INTERFACE => Self::SetInterface {
                interface: bytes[1],
                alternate: bytes[2],
            },
            op::CLAIM_INTERFACE => Self::ClaimInterface {
                interface: bytes[1],
            },
            op::ISO_START => {
                if bytes[6..8] != [0, 0] {
                    return Err(Errno::OutOfRange);
                }
                Self::IsoStart(IsoStartParams {
                    endpoint: stream_endpoint(bytes[1])?,
                    layout: IsoLayout::new(
                        read_u16(bytes, 2),
                        read_u16(bytes, 4),
                        read_u32(bytes, 8),
                    )?,
                })
            }
            op::ISO_QUEUE => {
                let slot = read_u16(bytes, 2);
                if slot >= ISO_MAX_SLOTS {
                    return Err(Errno::OutOfRange);
                }
                Self::IsoQueue {
                    endpoint: stream_endpoint(bytes[1])?,
                    slot,
                }
            }
            _ => Self::IsoStop {
                endpoint: stream_endpoint(bytes[1])?,
            },
        })
    }
}

/// Fixed prefix of every completion frame: a status word (`0` on success,
/// else the negated [`Errno`] discriminant), the shared status frame.
const COMPLETION_STATUS_LEN: usize = STATUS_REPLY_LEN;

/// Encoded length of a URB completion: the status word followed by the
/// `u32` byte count actually transferred.
pub const URB_COMPLETION_LEN: usize = COMPLETION_STATUS_LEN + 4;

/// Encode a successful URB completion carrying `transferred` bytes into
/// `buf`.
///
/// # Errors
///
/// [`Errno::BufferTooSmall`] if `buf` cannot hold [`URB_COMPLETION_LEN`]
/// bytes.
pub fn encode_completion(buf: &mut [u8], transferred: u32) -> Result<usize, Errno> {
    if buf.len() < URB_COMPLETION_LEN {
        return Err(Errno::BufferTooSmall);
    }
    put_i32(buf, 0, 0);
    put_u32(buf, COMPLETION_STATUS_LEN, transferred);
    Ok(URB_COMPLETION_LEN)
}

/// Encode a fail-closed error completion (status only) into `buf`.
///
/// Used both for a hard transfer failure and for the benign
/// [`Errno::WouldBlock`] a non-blocking interrupt-IN poll returns when no
/// report has arrived yet — the caller distinguishes them by the decoded
/// [`Errno`].
///
/// # Errors
///
/// [`Errno::BufferTooSmall`] if `buf` is shorter than the status word.
pub fn encode_error_completion(buf: &mut [u8], err: Errno) -> Result<usize, Errno> {
    if buf.len() < COMPLETION_STATUS_LEN {
        return Err(Errno::BufferTooSmall);
    }
    // A negative status carries `-errno`; `Errno` discriminants are positive.
    put_i32(buf, 0, -err.as_i32());
    Ok(COMPLETION_STATUS_LEN)
}

/// Decode a URB completion: the bytes transferred on success, else the
/// carried [`Errno`].
///
/// # Errors
///
/// The carried [`Errno`] for an error frame (e.g. [`Errno::WouldBlock`] for a
/// not-yet-arrived interrupt-IN report, or a hard transfer fault), or
/// [`Errno::BadMagic`] if a success frame is truncated or the status word is
/// neither `0` nor a known negated discriminant (wire corruption — fail
/// closed), or [`Errno::BufferTooSmall`] if `reply` is shorter than the
/// status word.
pub fn decode_completion(reply: &[u8]) -> Result<u32, Errno> {
    if reply.len() < COMPLETION_STATUS_LEN {
        return Err(Errno::BufferTooSmall);
    }
    match read_i32(reply, 0) {
        0 => {
            if reply.len() < URB_COMPLETION_LEN {
                return Err(Errno::BadMagic);
            }
            Ok(read_u32(reply, COMPLETION_STATUS_LEN))
        }
        negative => Err(Errno::try_from_status(negative).unwrap_or(Errno::BadMagic)),
    }
}

/// Fewest slots a stream may have: one being moved while the other is queued.
pub const ISO_MIN_SLOTS: u16 = 2;

/// Most slots a stream may have — a validation bound on an untrusted field.
pub const ISO_MAX_SLOTS: u16 = 32;

/// Most service intervals one slot may span — a validation bound.
pub const ISO_MAX_PACKETS: u16 = 64;

/// Most service intervals a stream may have queued at once, across all its
/// slots: what one page of transfer ring holds at two TRBs an interval, so a
/// layout these bounds admit is one a host controller can schedule whole.
pub const ISO_MAX_INTERVALS: u32 = 127;

/// Largest payload one service interval may move: a `SuperSpeed`
/// isochronous endpoint's 16 bursts of 3 × 1024-byte packets (USB 3.2
/// §9.6.7). A validation bound fixed by the bus, never widened.
pub const ISO_MAX_PACKET_BYTES: u32 = 48 * 1024;

/// Largest stream region: a containment bound on the memory one stream may
/// make the HCD map and carve, not a capacity.
pub const ISO_MAX_REGION_BYTES: usize = 4 << 20;

/// Bytes of a slot header before its packet records: the packet count and a
/// reserved word.
const ISO_SLOT_HEADER_FIXED: usize = 8;

/// Bytes of one packet record: its length and its [`IsoPacketStatus`].
const ISO_PACKET_RECORD_LEN: usize = 8;

/// Alignment of every slot header and data area.
const ISO_SLOT_ALIGN: usize = 64;

/// The geometry of an isochronous stream's shared region.
///
/// The region is `slots` slots back to back. Each slot covers `packets`
/// consecutive service intervals and is a header — a reserved word, then one
/// record per interval: the bytes it moves and how it went — followed by
/// `packets` data areas of `packet_bytes` each. For OUT the class driver
/// writes a slot's records and data before it queues the slot; for IN the HCD
/// writes them before it reports the slot done.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct IsoLayout {
    /// Slots in the region.
    pub slots: u16,
    /// Service intervals per slot.
    pub packets: u16,
    /// Bytes reserved for each interval's payload.
    pub packet_bytes: u32,
}

impl IsoLayout {
    /// A layout, held to every bound.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] for a slot count outside
    /// [`ISO_MIN_SLOTS`]`..=`[`ISO_MAX_SLOTS`], a packet count outside
    /// `1..=`[`ISO_MAX_PACKETS`], more than [`ISO_MAX_INTERVALS`] intervals
    /// across the slots, a packet size outside `1..=`[`ISO_MAX_PACKET_BYTES`],
    /// or a region past [`ISO_MAX_REGION_BYTES`].
    pub const fn new(slots: u16, packets: u16, packet_bytes: u32) -> Result<Self, Errno> {
        if slots < ISO_MIN_SLOTS
            || slots > ISO_MAX_SLOTS
            || packets == 0
            || packets > ISO_MAX_PACKETS
            || slots as u32 * packets as u32 > ISO_MAX_INTERVALS
            || packet_bytes == 0
            || packet_bytes > ISO_MAX_PACKET_BYTES
        {
            return Err(Errno::OutOfRange);
        }
        let layout = Self {
            slots,
            packets,
            packet_bytes,
        };
        if layout.region_len() > ISO_MAX_REGION_BYTES {
            return Err(Errno::OutOfRange);
        }
        Ok(layout)
    }

    /// Bytes of a slot's header.
    #[must_use]
    pub const fn header_len(self) -> usize {
        (ISO_SLOT_HEADER_FIXED + self.packets as usize * ISO_PACKET_RECORD_LEN)
            .next_multiple_of(ISO_SLOT_ALIGN)
    }

    /// Bytes from one slot to the next.
    #[must_use]
    pub const fn slot_stride(self) -> usize {
        (self.header_len() + self.packets as usize * self.packet_bytes as usize)
            .next_multiple_of(ISO_SLOT_ALIGN)
    }

    /// Bytes of the whole region.
    #[must_use]
    pub const fn region_len(self) -> usize {
        self.slots as usize * self.slot_stride()
    }

    /// Notifications a stream of this layout can have outstanding: one per
    /// queued slot, and its halt. A notify port bound with this much room
    /// never refuses one; the HCD ends a stream whose notification is
    /// refused.
    #[must_use]
    pub const fn notify_capacity(self) -> usize {
        self.slots as usize + 1
    }

    /// Offset of `packet`'s record in `slot`.
    #[must_use]
    pub const fn record_offset(self, slot: u16, packet: u16) -> usize {
        slot as usize * self.slot_stride()
            + ISO_SLOT_HEADER_FIXED
            + packet as usize * ISO_PACKET_RECORD_LEN
    }

    /// Offset of `packet`'s data in `slot`.
    #[must_use]
    pub const fn data_offset(self, slot: u16, packet: u16) -> usize {
        slot as usize * self.slot_stride()
            + self.header_len()
            + packet as usize * self.packet_bytes as usize
    }

    /// Read `packet`'s record in `slot` out of `region`.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] for a slot or packet past the layout, or a
    /// status word no [`IsoPacketStatus`] carries; [`Errno::BufferTooSmall`]
    /// for a region shorter than the layout.
    pub fn record(self, region: &[u8], slot: u16, packet: u16) -> Result<IsoPacket, Errno> {
        if slot >= self.slots || packet >= self.packets {
            return Err(Errno::OutOfRange);
        }
        let at = self.record_offset(slot, packet);
        let record = region
            .get(at..at + ISO_PACKET_RECORD_LEN)
            .ok_or(Errno::BufferTooSmall)?;
        Ok(IsoPacket {
            length: read_u32(record, 0),
            status: IsoPacketStatus::from_u32(read_u32(record, 4))?,
        })
    }

    /// Write `packet`'s record in `slot` into `region`.
    ///
    /// # Errors
    ///
    /// As [`Self::record`].
    pub fn set_record(
        self,
        region: &mut [u8],
        slot: u16,
        packet: u16,
        record: IsoPacket,
    ) -> Result<(), Errno> {
        if slot >= self.slots || packet >= self.packets {
            return Err(Errno::OutOfRange);
        }
        let at = self.record_offset(slot, packet);
        let bytes = region
            .get_mut(at..at + ISO_PACKET_RECORD_LEN)
            .ok_or(Errno::BufferTooSmall)?;
        put_u32(bytes, 0, record.length);
        put_u32(bytes, 4, record.status.as_u32());
        Ok(())
    }

    /// `packet`'s data area in `slot` within `region`.
    ///
    /// # Errors
    ///
    /// As [`Self::record`].
    pub fn data(self, region: &[u8], slot: u16, packet: u16) -> Result<&[u8], Errno> {
        let range = self.data_range(slot, packet)?;
        region.get(range).ok_or(Errno::BufferTooSmall)
    }

    /// `packet`'s data area in `slot` within `region`, writable.
    ///
    /// # Errors
    ///
    /// As [`Self::record`].
    pub fn data_mut(self, region: &mut [u8], slot: u16, packet: u16) -> Result<&mut [u8], Errno> {
        let range = self.data_range(slot, packet)?;
        region.get_mut(range).ok_or(Errno::BufferTooSmall)
    }

    fn data_range(self, slot: u16, packet: u16) -> Result<core::ops::Range<usize>, Errno> {
        if slot >= self.slots || packet >= self.packets {
            return Err(Errno::OutOfRange);
        }
        let at = self.data_offset(slot, packet);
        Ok(at..at + self.packet_bytes as usize)
    }
}

/// How one service interval of a stream went.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum IsoPacketStatus {
    /// The interval moved its record's length.
    Moved = 0,
    /// The interval passed without the transfer running: the controller
    /// missed it, or the slot was queued too late for it. Nothing moved, and
    /// the gap is the interval's whole budget.
    Missed = 1,
    /// The transfer ran and failed (a transaction error, babble, or an
    /// overrun); for IN its bytes are not to be believed.
    Failed = 2,
}

impl IsoPacketStatus {
    /// The wire word.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self as u32
    }

    /// Recover a status from its wire word.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] for a word no status carries.
    pub const fn from_u32(value: u32) -> Result<Self, Errno> {
        match value {
            0 => Ok(Self::Moved),
            1 => Ok(Self::Missed),
            2 => Ok(Self::Failed),
            _ => Err(Errno::OutOfRange),
        }
    }
}

/// One service interval's record in a slot header.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct IsoPacket {
    /// Bytes the interval moves (OUT, as queued) or moved (IN, as received).
    pub length: u32,
    /// How it went; for OUT the class driver writes [`IsoPacketStatus::Moved`].
    pub status: IsoPacketStatus,
}

/// The bus speed a device runs at, which fixes the length of its intervals
/// and the format of its explicit feedback (USB 2.0 §5.12.4.2).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum UsbSpeed {
    /// 12 Mb/s, 1 ms frames.
    Full = 1,
    /// 1.5 Mb/s, 1 ms frames.
    Low = 2,
    /// 480 Mb/s, 125 µs microframes.
    High = 3,
    /// 5 Gb/s and up, 125 µs bus intervals.
    Super = 4,
}

impl UsbSpeed {
    /// The wire byte, the xHCI default protocol speed ID.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// Recover a speed from its wire byte.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] for a byte no speed carries.
    pub const fn from_u8(value: u8) -> Result<Self, Errno> {
        match value {
            1 => Ok(Self::Full),
            2 => Ok(Self::Low),
            3 => Ok(Self::High),
            4 => Ok(Self::Super),
            _ => Err(Errno::OutOfRange),
        }
    }

    /// Whether the bus counts 125 µs microframes rather than 1 ms frames.
    #[must_use]
    pub const fn has_microframes(self) -> bool {
        matches!(self, Self::High | Self::Super)
    }
}

/// What a started stream was granted ([`UsbRequest::IsoStart`]'s reply).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct IsoGrant {
    /// The region's grant, which the class driver maps with `shm_map_from`
    /// naming [`Self::grantor`].
    pub region_grant: u64,
    /// The HCD's process instance, which delegated the grant: the kernel maps
    /// it only for the grantor that delegated it, and every [`IsoNotify`] the
    /// stream sends carries it as its attested origin.
    pub grantor: ProcId,
    /// The port the stream's [`IsoNotify`]s are sent to — the class driver's
    /// own [`iso_notify_endpoint_for`].
    pub notify: u64,
    /// Microframes (125 µs) between two of the endpoint's service intervals.
    pub interval_microframes: u32,
    /// The device's bus speed.
    pub speed: UsbSpeed,
    /// The stream's number, which every [`IsoNotify`] about it carries, so a
    /// notice a stream since stopped left behind is never read as one about
    /// its successor on the same endpoint.
    pub stream: NonZeroU32,
}

/// Encoded length of an [`IsoGrant`] reply: the status word, a pad word,
/// then `region_grant(8) || grantor(16) || notify(8) ||
/// interval_microframes(4) || speed(1) || reserved(3) || stream(4) ||
/// reserved(4)`.
pub const ISO_GRANT_REPLY_LEN: usize = 56;

/// Byte offsets of an [`IsoGrant`] reply's fields.
mod grant_reply {
    pub const REGION_GRANT: usize = 8;
    pub const GRANTOR: usize = 16;
    pub const NOTIFY: usize = 32;
    pub const INTERVAL: usize = 40;
    pub const SPEED: usize = 44;
    pub const RESERVED: core::ops::Range<usize> = 45..48;
    pub const STREAM: usize = 48;
    pub const TAIL: core::ops::Range<usize> = 52..56;
}

/// The longest reply a transport endpoint sends.
pub const USB_REPLY_MAX_LEN: usize = ISO_GRANT_REPLY_LEN;

impl IsoGrant {
    /// Encode a successful [`UsbRequest::IsoStart`] reply.
    #[must_use]
    pub fn encode(&self) -> [u8; ISO_GRANT_REPLY_LEN] {
        let mut out = [0u8; ISO_GRANT_REPLY_LEN];
        put_u64(&mut out, grant_reply::REGION_GRANT, self.region_grant);
        out[grant_reply::GRANTOR..grant_reply::GRANTOR + PROC_ID_LEN]
            .copy_from_slice(&self.grantor.to_le_bytes());
        put_u64(&mut out, grant_reply::NOTIFY, self.notify);
        put_u32(&mut out, grant_reply::INTERVAL, self.interval_microframes);
        out[grant_reply::SPEED] = self.speed.as_u8();
        put_u32(&mut out, grant_reply::STREAM, self.stream.get());
        out
    }

    /// Decode a [`UsbRequest::IsoStart`] reply.
    ///
    /// # Errors
    ///
    /// The carried [`Errno`] for a refusal (a corrupt status word reads as
    /// [`Errno::OutOfRange`], as every status frame's does);
    /// [`Errno::BadMagic`] for a success frame of the wrong length, a
    /// non-zero reserved field, the reserved grant `0`, the kernel's own
    /// instance as grantor, an interval of zero, an unknown speed, or a
    /// stream numbered `0`; [`Errno::BufferTooSmall`] for a frame shorter
    /// than its status word.
    pub fn decode(reply: &[u8]) -> Result<Self, Errno> {
        crate::reply::decode_status_reply(reply)?;
        if reply.len() != ISO_GRANT_REPLY_LEN
            || read_u32(reply, 4) != 0
            || reply[grant_reply::RESERVED] != [0, 0, 0]
            || reply[grant_reply::TAIL] != [0, 0, 0, 0]
        {
            return Err(Errno::BadMagic);
        }
        let region_grant = read_u64(reply, grant_reply::REGION_GRANT);
        let grantor =
            ProcId::from_bytes(&reply[grant_reply::GRANTOR..grant_reply::GRANTOR + PROC_ID_LEN])
                .map_err(|_| Errno::BadMagic)?;
        let interval_microframes = read_u32(reply, grant_reply::INTERVAL);
        if region_grant == 0 || grantor.is_kernel() || interval_microframes == 0 {
            return Err(Errno::BadMagic);
        }
        Ok(Self {
            region_grant,
            grantor,
            notify: read_u64(reply, grant_reply::NOTIFY),
            interval_microframes,
            speed: UsbSpeed::from_u8(reply[grant_reply::SPEED]).map_err(|_| Errno::BadMagic)?,
            stream: NonZeroU32::new(read_u32(reply, grant_reply::STREAM)).ok_or(Errno::BadMagic)?,
        })
    }
}

/// High tag of a class driver's isochronous notify-port id.
const ISO_NOTIFY_ENDPOINT_TAG: u64 = 0x5553_0000_0000_0000;

/// The notify port a class driver binds for its stream on `endpoint`
/// (a `bEndpointAddress`), from its own kernel-attested `pid`.
///
/// The HCD derives the same id from the caller's attested origin, so a class
/// driver cannot name another process's port and make the HCD a relay for
/// wakes it never asked for. The id is unreserved, so binding it needs no
/// privilege; `pid` takes the 40 bits [`crate::PID_MAX`] spans and the
/// endpoint address the low byte, so the fields tile the word and no pid
/// reaches the tag.
#[must_use]
pub const fn iso_notify_endpoint_for(pid: u64, endpoint: u8) -> u64 {
    ISO_NOTIFY_ENDPOINT_TAG | ((pid & crate::PID_MAX) << 8) | endpoint as u64
}

/// Magic opening every [`IsoNotify`]: `"USBN"`.
const ISO_NOTIFY_MAGIC: [u8; 4] = *b"USBN";

/// Encoded length of an [`IsoNotify`].
pub const ISO_NOTIFY_LEN: usize = 40;

/// What the HCD tells a stream's class driver.
///
/// A receiver admits only the stream's [`IsoGrant::grantor`] to its port, and
/// believes a notice only when its attested origin is that grantor and it
/// names the stream's [`IsoGrant::stream`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum IsoNotify {
    /// A queued slot finished; its records say how each interval went.
    SlotDone {
        /// The stream's `bEndpointAddress`.
        endpoint: u8,
        /// The stream's number.
        stream: NonZeroU32,
        /// The slot.
        slot: u16,
        /// Service intervals that passed carrying nothing between the slot
        /// before this one and this one: a gap the class driver accounts for.
        skipped: u32,
        /// The bus microframe of the slot's first service interval, on the
        /// HCD's extended (never wrapping) count.
        microframe: u64,
        /// Monotonic nanoseconds at which the HCD saw the slot finish.
        completed_at: u64,
    },
    /// The stream ended on its own — its device went or its endpoint
    /// faulted — and holds no slot any longer.
    Halted {
        /// The stream's `bEndpointAddress`.
        endpoint: u8,
        /// The stream's number.
        stream: NonZeroU32,
        /// Why.
        reason: Errno,
    },
}

impl IsoNotify {
    /// Encode the notification.
    #[must_use]
    pub fn encode(&self) -> [u8; ISO_NOTIFY_LEN] {
        let mut out = [0u8; ISO_NOTIFY_LEN];
        out[..4].copy_from_slice(&ISO_NOTIFY_MAGIC);
        match *self {
            Self::SlotDone {
                endpoint,
                stream,
                slot,
                skipped,
                microframe,
                completed_at,
            } => {
                out[4] = 1;
                out[5] = endpoint;
                put_u16(&mut out, 6, slot);
                put_u32(&mut out, 8, skipped);
                put_u32(&mut out, 12, stream.get());
                put_u64(&mut out, 16, microframe);
                put_u64(&mut out, 24, completed_at);
            }
            Self::Halted {
                endpoint,
                stream,
                reason,
            } => {
                out[4] = 2;
                out[5] = endpoint;
                put_i32(&mut out, 8, reason.as_i32());
                put_u32(&mut out, 12, stream.get());
            }
        }
        out
    }

    /// Decode a notification.
    ///
    /// # Errors
    ///
    /// [`Errno::BadMagic`] for a frame of the wrong length or magic, an
    /// unknown kind, a non-zero reserved byte, an endpoint that is not a
    /// device endpoint's, a stream numbered `0`, a slot past
    /// [`ISO_MAX_SLOTS`], or a halt reason no [`Errno`] carries.
    pub fn decode(bytes: &[u8]) -> Result<Self, Errno> {
        if bytes.len() != ISO_NOTIFY_LEN || bytes[..4] != ISO_NOTIFY_MAGIC {
            return Err(Errno::BadMagic);
        }
        let endpoint = bytes[5];
        if !is_device_endpoint_address(endpoint) {
            return Err(Errno::BadMagic);
        }
        let stream = NonZeroU32::new(read_u32(bytes, 12)).ok_or(Errno::BadMagic)?;
        match bytes[4] {
            1 => {
                let slot = read_u16(bytes, 6);
                if slot >= ISO_MAX_SLOTS || read_u64(bytes, 32) != 0 {
                    return Err(Errno::BadMagic);
                }
                Ok(Self::SlotDone {
                    endpoint,
                    stream,
                    slot,
                    skipped: read_u32(bytes, 8),
                    microframe: read_u64(bytes, 16),
                    completed_at: read_u64(bytes, 24),
                })
            }
            2 => {
                if bytes[6..8] != [0, 0] || bytes[16..].iter().any(|&b| b != 0) {
                    return Err(Errno::BadMagic);
                }
                let reason = Errno::from_i32(read_i32(bytes, 8)).ok_or(Errno::BadMagic)?;
                Ok(Self::Halted {
                    endpoint,
                    stream,
                    reason,
                })
            }
            _ => Err(Errno::BadMagic),
        }
    }

    /// The endpoint the notice is about.
    #[must_use]
    pub const fn endpoint(&self) -> u8 {
        match *self {
            Self::SlotDone { endpoint, .. } | Self::Halted { endpoint, .. } => endpoint,
        }
    }

    /// The number of the stream the notice is about.
    #[must_use]
    pub const fn stream(&self) -> NonZeroU32 {
        match *self {
            Self::SlotDone { stream, .. } | Self::Halted { stream, .. } => stream,
        }
    }
}

#[cfg(test)]
#[path = "usb_urb_tests.rs"]
mod tests;
