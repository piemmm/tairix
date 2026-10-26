//! The channel between the front and its sandboxed decoder.
//!
//! Every frame is one fixed layout behind a one-byte tag, so either side's
//! reader is a bounds-checked field read and nothing more. The front parses no
//! DNS, SOAP or HTTP: an answer crosses as a
//! [`discovery_ipc`](tairix_abi::discovery_ipc) entry, already typed, and a
//! datagram to send or a metadata response to read crosses unread. Both
//! directions fail closed — a frame whose tag, length, or any field is not
//! exactly what an honest peer sends is refused whole.
//!
//! Encoders write into a buffer the caller holds, because the relay encodes
//! every datagram a segment carries and must not allocate per datagram.

use tairix_abi::discovery_ipc::{Entry, ENTRY_MAX, NAME_MAX};
use tairix_abi::net::SOCKET_MAX_DATAGRAM;
use tairix_abi::net_ipc::{
    address_parts, ip_from_parts, validate_if_name, NetAddrFamily, IF_NAME_LEN,
};
use tairix_abi::time::NANOS_PER_SEC;
use tairix_net::dns::RecordType;
use tairix_net::http::{HostName, MAX_HOST_NAME};
use tairix_net::mdns::{Destination, MAX_MESSAGE_LEN, MAX_QUESTIONS};
use tairix_net::rate::BucketLevel;
use tairix_net::wsd::{Endpoint, MetadataPath, MAX_ENDPOINT, MAX_METADATA, MAX_PATH};
use tairix_net::IpAddr;
use tairix_sandbox::wire::{Reader, WireError};

/// Bytes of the key the decoder's caches are indexed under.
pub const CACHE_KEY_LEN: usize = tairix_hash::HashSeed::LEN;

/// Bytes of the key the decoder's CSPRNG is seeded from.
pub const RNG_KEY_LEN: usize = tairix_rng::STREAM_KEY_LEN;

/// Datagrams one interface may send to the group in a burst: well above what
/// continuous querying for every question needs, and far below a flood. The
/// decoder spends the budget and the front holds it to it.
pub const GROUP_BURST: u32 = 32;

/// Datagrams one interface may send to the group per second after its burst.
pub const GROUP_RATE: u32 = 8;

/// The part of the group budget the decoder's scheduled sends leave alone,
/// so a name this host owns can be defended at once however many questions
/// are queued.
pub const GROUP_RESERVE: u32 = 4;

/// WS-Discovery datagrams one interface may send to its group in a burst: a
/// probe round's four sendings and room for resolves.
pub const WSD_BURST: u32 = 8;

/// WS-Discovery datagrams one interface may send per second after its burst.
pub const WSD_RATE: u32 = 2;

const TAG_CONFIGURE: u8 = 1;
const TAG_DATAGRAM: u8 = 2;
const TAG_TICK: u8 = 3;
const TAG_LINK: u8 = 4;
const TAG_ASK: u8 = 5;
const TAG_STOP: u8 = 6;
const TAG_REPLAY: u8 = 7;
const TAG_WSD_DATAGRAM: u8 = 8;
const TAG_FETCHED: u8 = 9;
const TAG_FETCH_FAILED: u8 = 10;

const TAG_DEADLINE: u8 = 1;
const TAG_ANSWER: u8 = 2;
const TAG_HELD: u8 = 3;
const TAG_REPLAYED: u8 = 4;
const TAG_TRANSMIT: u8 = 5;
const TAG_LINKED: u8 = 6;
const TAG_STOPPED: u8 = 7;
const TAG_WSD_TRANSMIT: u8 = 8;
const TAG_FETCH: u8 = 9;

const TO_GROUP: u8 = 0;
const TO_PEER: u8 = 1;

/// Length of a [`ToDecoder::Configure`] frame.
pub const CONFIGURE_LEN: usize = 1 + CACHE_KEY_LEN + RNG_KEY_LEN;

/// Length of a [`ToDecoder::Datagram`] frame before its payload: tag, time,
/// interface, address family, address, port, and whether it was sent to the
/// group.
pub const DATAGRAM_HEADER_LEN: usize = 1 + 8 + IF_NAME_LEN + 1 + 16 + 2 + 1;

/// Length of a [`ToDecoder::Tick`] frame.
pub const TICK_LEN: usize = 1 + 8;

/// Length of a [`ToDecoder::Link`] frame: tag, time, interface, edge, and the
/// multicast DNS and WS-Discovery budgets' tokens and instants.
pub const LINK_LEN: usize = 1 + 8 + IF_NAME_LEN + 1 + 2 * (8 + 8);

/// Length of a [`ToWsd::Datagram`] frame before its payload: tag, time,
/// interface, address family and address.
pub const WSD_DATAGRAM_HEADER_LEN: usize = 1 + 8 + IF_NAME_LEN + 1 + 16;

/// Length of a [`ToWsd::Fetched`] frame before its response, and of a whole
/// [`ToWsd::FetchFailed`]: tag, time, interface, fetch.
pub const FETCHED_HEADER_LEN: usize = 1 + 8 + IF_NAME_LEN + 4;

/// Length of a [`FromWsd::Transmit`] frame before its payload: tag,
/// interface, instant.
pub const WSD_TRANSMIT_HEADER_LEN: usize = 1 + IF_NAME_LEN + 8;

/// Length of a [`FromWsd::Fetch`] frame before its path, endpoint and host
/// name: tag, interface, fetch, family, address, port, and the three lengths.
pub const FETCH_HEADER_LEN: usize = 1 + IF_NAME_LEN + 4 + 1 + 16 + 2 + 3;

/// Length of a [`ToDecoder::Ask`] frame before its name: tag, time, question,
/// form, name length.
pub const ASK_HEADER_LEN: usize = 1 + 8 + 4 + 1 + 1;

/// Length of a [`ToDecoder::Stop`] frame.
pub const STOP_LEN: usize = 1 + 4;

/// Length of a [`ToDecoder::Replay`] frame.
pub const REPLAY_LEN: usize = 1 + 4 + 4;

/// Length of a [`FromDecoder::Deadline`] frame.
pub const DEADLINE_LEN: usize = 1 + 1 + 8;

/// Length of a [`FromDecoder::Replayed`] frame.
pub const REPLAYED_LEN: usize = 1 + 4;

/// Length of a [`FromDecoder::Linked`] frame.
pub const LINKED_LEN: usize = 1 + IF_NAME_LEN + 1;

/// Length of a [`FromDecoder::Stopped`] frame.
pub const STOPPED_LEN: usize = 1 + 4;

/// Length of a [`FromDecoder::Transmit`] frame before its payload: tag,
/// interface, instant, destination kind, family, address, port.
pub const TRANSMIT_HEADER_LEN: usize = 1 + IF_NAME_LEN + 8 + 1 + 1 + 16 + 2;

/// The longest frame the front sends: a metadata response at its bound, or a
/// datagram carrying the largest payload the stack delivers.
pub const MAX_TO_DECODER: usize =
    if FETCHED_HEADER_LEN + MAX_METADATA > DATAGRAM_HEADER_LEN + SOCKET_MAX_DATAGRAM {
        FETCHED_HEADER_LEN + MAX_METADATA
    } else {
        DATAGRAM_HEADER_LEN + SOCKET_MAX_DATAGRAM
    };

/// The longest frame the decoder sends: a datagram to transmit.
pub const MAX_FROM_DECODER: usize = TRANSMIT_HEADER_LEN + MAX_MESSAGE_LEN;

const _: () = assert!(ASK_HEADER_LEN + NAME_MAX <= MAX_TO_DECODER);
const _: () = assert!(1 + 4 + ENTRY_MAX <= MAX_FROM_DECODER);
const _: () =
    assert!(FETCH_HEADER_LEN + MAX_PATH + MAX_ENDPOINT + MAX_HOST_NAME <= MAX_FROM_DECODER);
// Each of a fetch's texts fits the byte its length is framed in.
const _: () = assert!(
    MAX_PATH <= u8::MAX as usize
        && MAX_ENDPOINT <= u8::MAX as usize
        && MAX_HOST_NAME <= u8::MAX as usize
);

/// What a question's answers are read as, which fixes the record type it asks
/// for and the [`Answer`](tairix_abi::discovery_ipc::Answer) each record
/// becomes.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Form {
    /// A browse: `PTR` records naming instances of the type asked about.
    Instance = 1,
    /// A resolve's `SRV`.
    Service = 2,
    /// A resolve's `TXT`.
    Text = 3,
    /// A host's `A`.
    AddressV4 = 4,
    /// A host's `AAAA`.
    AddressV6 = 5,
    /// A reverse lookup's `PTR`.
    Pointer = 6,
    /// The RFC 6763 §9 type enumeration: `PTR` records naming types.
    Type = 7,
    /// The computers announcing themselves over WS-Discovery, asked about
    /// the root name: the one question of its form.
    Computer = 8,
}

impl Form {
    /// The record type a multicast DNS question of this form asks for, or
    /// `None` for a form WS-Discovery answers.
    #[must_use]
    pub const fn record_type(self) -> Option<RecordType> {
        Some(match self {
            Self::Instance | Self::Pointer | Self::Type => RecordType::Ptr,
            Self::Service => RecordType::Srv,
            Self::Text => RecordType::Txt,
            Self::AddressV4 => RecordType::A,
            Self::AddressV6 => RecordType::Aaaa,
            Self::Computer => return None,
        })
    }

    const fn from_u8(value: u8) -> Option<Self> {
        Some(match value {
            1 => Self::Instance,
            2 => Self::Service,
            3 => Self::Text,
            4 => Self::AddressV4,
            5 => Self::AddressV6,
            6 => Self::Pointer,
            7 => Self::Type,
            8 => Self::Computer,
            _ => return None,
        })
    }
}

/// A frame the front sends its decoder.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToDecoder<'a> {
    /// The keys a fresh decoder works under. The first frame of every
    /// decoder, and never sent to it again.
    Configure {
        /// The key its caches are indexed under.
        cache_key: &'a [u8; CACHE_KEY_LEN],
        /// The key its CSPRNG is seeded from.
        rng_key: &'a [u8; RNG_KEY_LEN],
    },
    /// One datagram the stack delivered from a sender it found on-link.
    Datagram {
        /// The monotonic instant the front relayed it, in nanoseconds.
        now: u64,
        /// The logical interface it arrived on.
        interface: [u8; IF_NAME_LEN],
        /// The sender's address.
        source: IpAddr,
        /// The sender's port.
        port: u16,
        /// Whether it was sent to the group rather than to this host alone.
        to_group: bool,
        /// The datagram, unread.
        payload: &'a [u8],
    },
    /// The decoder's reported deadline has come.
    Tick {
        /// The monotonic instant, in nanoseconds.
        now: u64,
    },
    /// An interface came up, and is asked every question; or went down, and
    /// everything learned on it is forgotten.
    Link {
        /// The monotonic instant, in nanoseconds.
        now: u64,
        /// The interface.
        interface: [u8; IF_NAME_LEN],
        /// What became of it.
        edge: Edge,
    },
    /// Ask a question on every interface, now and as each comes up.
    Ask {
        /// The monotonic instant, in nanoseconds.
        now: u64,
        /// The front's id for it, below [`MAX_QUESTIONS`] and reused only
        /// after the decoder has said [`FromDecoder::Stopped`] for it.
        question: u32,
        /// How its answers are read.
        form: Form,
        /// The name asked about, in canonical wire form.
        name: &'a [u8],
    },
    /// Stop asking a question, and say so with [`FromDecoder::Stopped`].
    Stop {
        /// The question.
        question: u32,
    },
    /// Send every answer a question holds now, as [`FromDecoder::Held`] under
    /// `token`, then [`FromDecoder::Replayed`].
    Replay {
        /// The question.
        question: u32,
        /// The front's name for this replay.
        token: u32,
    },
    /// A frame for the decoder's WS-Discovery engines.
    Wsd(ToWsd<'a>),
}

impl<'a> ToDecoder<'a> {
    /// Encode into the front of `out`, returning the frame's length, or
    /// `None` when `out` cannot hold it or a field is longer than its bound.
    #[must_use]
    pub fn encode(&self, out: &mut [u8]) -> Option<usize> {
        match *self {
            Self::Configure { cache_key, rng_key } => {
                let frame = out.get_mut(..CONFIGURE_LEN)?;
                frame[0] = TAG_CONFIGURE;
                frame[1..=CACHE_KEY_LEN].copy_from_slice(cache_key);
                frame[1 + CACHE_KEY_LEN..].copy_from_slice(rng_key);
                Some(CONFIGURE_LEN)
            }
            Self::Datagram {
                now,
                interface,
                source,
                port,
                to_group,
                payload,
            } => {
                if payload.len() > SOCKET_MAX_DATAGRAM {
                    return None;
                }
                let len = DATAGRAM_HEADER_LEN + payload.len();
                let frame = out.get_mut(..len)?;
                stamp(frame, TAG_DATAGRAM, now, interface);
                put_address(&mut frame[25..42], source);
                frame[42..44].copy_from_slice(&port.to_le_bytes());
                frame[44] = u8::from(to_group);
                frame[DATAGRAM_HEADER_LEN..].copy_from_slice(payload);
                Some(len)
            }
            Self::Tick { now } => {
                let frame = out.get_mut(..TICK_LEN)?;
                frame[0] = TAG_TICK;
                frame[1..].copy_from_slice(&now.to_le_bytes());
                Some(TICK_LEN)
            }
            Self::Link {
                now,
                interface,
                edge,
            } => {
                let frame = out.get_mut(..LINK_LEN)?;
                frame.fill(0);
                stamp(frame, TAG_LINK, now, interface);
                if let Edge::Up(budgets) = edge {
                    if !budgets.are_levels() {
                        return None;
                    }
                    frame[25] = 1;
                    frame[26..34].copy_from_slice(&budgets.group.scaled.to_le_bytes());
                    frame[34..42].copy_from_slice(&budgets.group.at.to_le_bytes());
                    frame[42..50].copy_from_slice(&budgets.wsd.scaled.to_le_bytes());
                    frame[50..LINK_LEN].copy_from_slice(&budgets.wsd.at.to_le_bytes());
                }
                Some(LINK_LEN)
            }
            Self::Ask {
                now,
                question,
                form,
                name,
            } => {
                if name.is_empty() || name.len() > NAME_MAX || !is_question(question) {
                    return None;
                }
                let len = ASK_HEADER_LEN + name.len();
                let frame = out.get_mut(..len)?;
                frame[0] = TAG_ASK;
                frame[1..9].copy_from_slice(&now.to_le_bytes());
                frame[9..13].copy_from_slice(&question.to_le_bytes());
                frame[13] = form as u8;
                frame[14] = u8::try_from(name.len()).ok()?;
                frame[ASK_HEADER_LEN..].copy_from_slice(name);
                Some(len)
            }
            Self::Stop { question } => {
                if !is_question(question) {
                    return None;
                }
                let frame = out.get_mut(..STOP_LEN)?;
                frame[0] = TAG_STOP;
                frame[1..].copy_from_slice(&question.to_le_bytes());
                Some(STOP_LEN)
            }
            Self::Replay { question, token } => {
                if !is_question(question) {
                    return None;
                }
                let frame = out.get_mut(..REPLAY_LEN)?;
                frame[0] = TAG_REPLAY;
                frame[1..5].copy_from_slice(&question.to_le_bytes());
                frame[5..].copy_from_slice(&token.to_le_bytes());
                Some(REPLAY_LEN)
            }
            Self::Wsd(frame) => frame.encode(out),
        }
    }

    /// Decode one frame, refusing any an honest front cannot have sent.
    ///
    /// # Errors
    ///
    /// [`WireError`] for a short frame, an unknown tag, trailing bytes, an
    /// interface name the stack would never use, an unknown address family or
    /// form, an IPv4 address with a dirty tail, a flag other than `0` or `1`,
    /// a question id past [`MAX_QUESTIONS`], or a field longer than its bound.
    pub fn decode(frame: &'a [u8]) -> Result<Self, WireError> {
        let mut reader = Reader::new(frame);
        let decoded = match reader.u8()? {
            TAG_CONFIGURE => Self::Configure {
                cache_key: fixed(&mut reader)?,
                rng_key: fixed(&mut reader)?,
            },
            TAG_DATAGRAM => {
                let now = reader.u64()?;
                let interface = interface(&mut reader)?;
                let source = address(&mut reader)?;
                let port = u16::from_le_bytes(*fixed(&mut reader)?);
                let to_group = flag(reader.u8()?)?;
                let payload = reader.take(frame.len().saturating_sub(DATAGRAM_HEADER_LEN))?;
                if payload.len() > SOCKET_MAX_DATAGRAM {
                    return Err(WireError::Malformed);
                }
                Self::Datagram {
                    now,
                    interface,
                    source,
                    port,
                    to_group,
                    payload,
                }
            }
            TAG_TICK => Self::Tick { now: reader.u64()? },
            TAG_LINK => {
                let now = reader.u64()?;
                let interface = interface(&mut reader)?;
                let up = flag(reader.u8()?)?;
                let mut level = || -> Result<BucketLevel, WireError> {
                    Ok(BucketLevel {
                        scaled: reader.u64()?,
                        at: reader.u64()?,
                    })
                };
                let budgets = Budgets {
                    group: level()?,
                    wsd: level()?,
                };
                let edge = match up {
                    true if budgets.are_levels() => Edge::Up(budgets),
                    false if budgets == Budgets::NONE => Edge::Down,
                    _ => return Err(WireError::Malformed),
                };
                Self::Link {
                    now,
                    interface,
                    edge,
                }
            }
            TAG_ASK => {
                let now = reader.u64()?;
                let question = question(&mut reader)?;
                let form = Form::from_u8(reader.u8()?).ok_or(WireError::Malformed)?;
                let len = usize::from(reader.u8()?);
                if len == 0 {
                    return Err(WireError::Malformed);
                }
                Self::Ask {
                    now,
                    question,
                    form,
                    name: reader.take(len)?,
                }
            }
            TAG_STOP => Self::Stop {
                question: question(&mut reader)?,
            },
            TAG_REPLAY => Self::Replay {
                question: question(&mut reader)?,
                token: reader.u32()?,
            },
            tag @ (TAG_WSD_DATAGRAM | TAG_FETCHED | TAG_FETCH_FAILED) => {
                Self::Wsd(ToWsd::decode(tag, &mut reader, frame.len())?)
            }
            _ => return Err(WireError::Malformed),
        };
        if !reader.is_exhausted() {
            return Err(WireError::Malformed);
        }
        Ok(decoded)
    }
}

/// A frame the decoder sends its front.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FromDecoder<'a> {
    /// The earliest instant, in monotonic nanoseconds, at which any of the
    /// decoder's engines has work — or `None` while none has any.
    Deadline(Option<u64>),
    /// One answer to a question moved; the entry's `request` field is the
    /// question.
    Answer(Entry<'a>),
    /// One answer a question held when a replay was asked for.
    Held {
        /// The replay.
        token: u32,
        /// The answer, its `request` field the question.
        entry: Entry<'a>,
    },
    /// Every answer of the replay `token` has been sent.
    Replayed {
        /// The replay.
        token: u32,
    },
    /// A datagram to send.
    Transmit {
        /// The interface to send it from.
        interface: [u8; IF_NAME_LEN],
        /// The instant the decoder charged it to the interface's group
        /// budget, which the front charges it at too.
        at: u64,
        /// Where to.
        to: Destination,
        /// The datagram.
        payload: &'a [u8],
    },
    /// A [`ToDecoder::Link`] has been carried out: every frame before this
    /// one about `interface` described it as it was before the edge.
    Linked {
        /// The interface.
        interface: [u8; IF_NAME_LEN],
        /// Whether it is now up.
        up: bool,
    },
    /// A [`ToDecoder::Stop`] has been carried out: no frame after this one
    /// is about the question, whose id the front may give to another.
    Stopped {
        /// The question.
        question: u32,
    },
    /// A frame from the decoder's WS-Discovery engines.
    Wsd(FromWsd<'a>),
}

impl<'a> FromDecoder<'a> {
    /// Encode into the front of `out`, returning the frame's length, or
    /// `None` when `out` cannot hold it or a field is out of bounds.
    #[must_use]
    pub fn encode(&self, out: &mut [u8]) -> Option<usize> {
        match *self {
            Self::Deadline(at) => {
                let frame = out.get_mut(..DEADLINE_LEN)?;
                frame.fill(0);
                frame[0] = TAG_DEADLINE;
                if let Some(at) = at {
                    frame[1] = 1;
                    frame[2..].copy_from_slice(&at.to_le_bytes());
                }
                Some(DEADLINE_LEN)
            }
            Self::Answer(entry) => {
                let frame = out.get_mut(..1 + entry.wire_len())?;
                frame[0] = TAG_ANSWER;
                entry.encode(&mut frame[1..]).ok()?;
                Some(frame.len())
            }
            Self::Held { token, entry } => {
                let frame = out.get_mut(..5 + entry.wire_len())?;
                frame[0] = TAG_HELD;
                frame[1..5].copy_from_slice(&token.to_le_bytes());
                entry.encode(&mut frame[5..]).ok()?;
                Some(frame.len())
            }
            Self::Replayed { token } => {
                let frame = out.get_mut(..REPLAYED_LEN)?;
                frame[0] = TAG_REPLAYED;
                frame[1..].copy_from_slice(&token.to_le_bytes());
                Some(REPLAYED_LEN)
            }
            Self::Transmit {
                interface,
                at,
                to,
                payload,
            } => {
                if payload.is_empty() || payload.len() > MAX_MESSAGE_LEN {
                    return None;
                }
                let len = TRANSMIT_HEADER_LEN + payload.len();
                let frame = out.get_mut(..len)?;
                frame[..TRANSMIT_HEADER_LEN].fill(0);
                frame[0] = TAG_TRANSMIT;
                frame[1..=IF_NAME_LEN].copy_from_slice(&interface);
                frame[17..25].copy_from_slice(&at.to_le_bytes());
                if let Destination::Peer { addr, port } = to {
                    frame[25] = TO_PEER;
                    put_address(&mut frame[26..43], addr);
                    frame[43..TRANSMIT_HEADER_LEN].copy_from_slice(&port.to_le_bytes());
                } else {
                    frame[25] = TO_GROUP;
                }
                frame[TRANSMIT_HEADER_LEN..].copy_from_slice(payload);
                Some(len)
            }
            Self::Linked { interface, up } => {
                let frame = out.get_mut(..LINKED_LEN)?;
                frame[0] = TAG_LINKED;
                frame[1..=IF_NAME_LEN].copy_from_slice(&interface);
                frame[1 + IF_NAME_LEN] = u8::from(up);
                Some(LINKED_LEN)
            }
            Self::Stopped { question } => {
                if !is_question(question) {
                    return None;
                }
                let frame = out.get_mut(..STOPPED_LEN)?;
                frame[0] = TAG_STOPPED;
                frame[1..].copy_from_slice(&question.to_le_bytes());
                Some(STOPPED_LEN)
            }
            Self::Wsd(frame) => frame.encode(out),
        }
    }

    /// Decode one frame, refusing any an honest decoder cannot have sent.
    ///
    /// # Errors
    ///
    /// [`WireError`] for a short frame, an unknown tag, trailing bytes, a
    /// presence flag other than `0` or `1`, an absent deadline whose instant
    /// is not zero, an entry the discovery channel would refuse, an interface
    /// name the stack would never use, a group destination with an address, a
    /// datagram empty or past [`MAX_MESSAGE_LEN`], or a frame's own question
    /// id past [`MAX_QUESTIONS`]. The id an answer entry carries is judged by
    /// the question table, which holds the decoder to its word.
    pub fn decode(frame: &'a [u8]) -> Result<Self, WireError> {
        let mut reader = Reader::new(frame);
        let decoded = match reader.u8()? {
            TAG_DEADLINE => {
                let present = reader.u8()?;
                let at = reader.u64()?;
                match (present, at) {
                    (0, 0) => Self::Deadline(None),
                    (1, at) => Self::Deadline(Some(at)),
                    _ => return Err(WireError::Malformed),
                }
            }
            TAG_ANSWER => Self::Answer(entry(reader.take(frame.len() - 1)?)?),
            TAG_HELD => Self::Held {
                token: reader.u32()?,
                entry: entry(reader.take(frame.len().saturating_sub(5))?)?,
            },
            TAG_REPLAYED => Self::Replayed {
                token: reader.u32()?,
            },
            TAG_TRANSMIT => {
                let interface = interface(&mut reader)?;
                let at = reader.u64()?;
                let kind = reader.u8()?;
                let family = reader.u8()?;
                let octets = *fixed::<16>(&mut reader)?;
                let port = u16::from_le_bytes(*fixed(&mut reader)?);
                let to = match kind {
                    TO_GROUP if family == 0 && port == 0 && octets == [0; 16] => Destination::Group,
                    TO_PEER => Destination::Peer {
                        addr: address_from(family, octets)?,
                        port,
                    },
                    _ => return Err(WireError::Malformed),
                };
                let payload = reader.take(frame.len().saturating_sub(TRANSMIT_HEADER_LEN))?;
                if payload.is_empty() || payload.len() > MAX_MESSAGE_LEN {
                    return Err(WireError::Malformed);
                }
                Self::Transmit {
                    interface,
                    at,
                    to,
                    payload,
                }
            }
            TAG_LINKED => Self::Linked {
                interface: interface(&mut reader)?,
                up: flag(reader.u8()?)?,
            },
            TAG_STOPPED => Self::Stopped {
                question: question(&mut reader)?,
            },
            tag @ (TAG_WSD_TRANSMIT | TAG_FETCH) => {
                Self::Wsd(FromWsd::decode(tag, &mut reader, frame.len())?)
            }
            _ => return Err(WireError::Malformed),
        };
        if !reader.is_exhausted() {
            return Err(WireError::Malformed);
        }
        Ok(decoded)
    }
}

/// A frame the front sends the decoder's WS-Discovery engines.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToWsd<'a> {
    /// One WS-Discovery datagram the stack delivered from a sender it found
    /// on-link.
    Datagram {
        /// The monotonic instant the front relayed it, in nanoseconds.
        now: u64,
        /// The logical interface it arrived on.
        interface: [u8; IF_NAME_LEN],
        /// The sender's address.
        source: IpAddr,
        /// The datagram, unread.
        payload: &'a [u8],
    },
    /// The whole of what a [`FromWsd::Fetch`]'s target sent before it closed
    /// the connection, at most [`MAX_METADATA`] bytes, unread.
    Fetched {
        /// The monotonic instant, in nanoseconds.
        now: u64,
        /// The interface the fetch was made on.
        interface: [u8; IF_NAME_LEN],
        /// The decoder's id for the fetch.
        fetch: u32,
        /// The response.
        response: &'a [u8],
    },
    /// A [`FromWsd::Fetch`] could not complete: refused, unreachable, too
    /// slow, too large, or cut off.
    FetchFailed {
        /// The monotonic instant, in nanoseconds.
        now: u64,
        /// The interface the fetch was made on.
        interface: [u8; IF_NAME_LEN],
        /// The decoder's id for the fetch.
        fetch: u32,
    },
}

impl<'a> ToWsd<'a> {
    fn encode(&self, out: &mut [u8]) -> Option<usize> {
        match *self {
            Self::Datagram {
                now,
                interface,
                source,
                payload,
            } => {
                if payload.len() > SOCKET_MAX_DATAGRAM {
                    return None;
                }
                let frame = out.get_mut(..WSD_DATAGRAM_HEADER_LEN + payload.len())?;
                stamp(frame, TAG_WSD_DATAGRAM, now, interface);
                put_address(&mut frame[25..WSD_DATAGRAM_HEADER_LEN], source);
                frame[WSD_DATAGRAM_HEADER_LEN..].copy_from_slice(payload);
                Some(frame.len())
            }
            Self::Fetched {
                now,
                interface,
                fetch,
                response,
            } => {
                if response.len() > MAX_METADATA {
                    return None;
                }
                let frame = out.get_mut(..FETCHED_HEADER_LEN + response.len())?;
                stamp(frame, TAG_FETCHED, now, interface);
                frame[25..FETCHED_HEADER_LEN].copy_from_slice(&fetch.to_le_bytes());
                frame[FETCHED_HEADER_LEN..].copy_from_slice(response);
                Some(frame.len())
            }
            Self::FetchFailed {
                now,
                interface,
                fetch,
            } => {
                let frame = out.get_mut(..FETCHED_HEADER_LEN)?;
                stamp(frame, TAG_FETCH_FAILED, now, interface);
                frame[25..].copy_from_slice(&fetch.to_le_bytes());
                Some(FETCHED_HEADER_LEN)
            }
        }
    }

    /// Decode what follows `tag` in a frame of `len` bytes.
    fn decode(tag: u8, reader: &mut Reader<'a>, len: usize) -> Result<Self, WireError> {
        let now = reader.u64()?;
        let interface = interface(reader)?;
        match tag {
            TAG_WSD_DATAGRAM => {
                let source = address(reader)?;
                let payload = reader.take(len.saturating_sub(WSD_DATAGRAM_HEADER_LEN))?;
                if payload.len() > SOCKET_MAX_DATAGRAM {
                    return Err(WireError::Malformed);
                }
                Ok(Self::Datagram {
                    now,
                    interface,
                    source,
                    payload,
                })
            }
            TAG_FETCHED | TAG_FETCH_FAILED => {
                let fetch = reader.u32()?;
                let response = reader.take(len.saturating_sub(FETCHED_HEADER_LEN))?;
                match tag {
                    TAG_FETCHED if response.len() <= MAX_METADATA => Ok(Self::Fetched {
                        now,
                        interface,
                        fetch,
                        response,
                    }),
                    TAG_FETCH_FAILED if response.is_empty() => Ok(Self::FetchFailed {
                        now,
                        interface,
                        fetch,
                    }),
                    _ => Err(WireError::Malformed),
                }
            }
            _ => Err(WireError::Malformed),
        }
    }
}

/// A frame the decoder's WS-Discovery engines send the front.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FromWsd<'a> {
    /// A datagram to send to `interface`'s groups, built against its
    /// WS-Discovery budget as it stood at `at`.
    Transmit {
        /// The interface.
        interface: [u8; IF_NAME_LEN],
        /// The instant the budget was charged at, in nanoseconds.
        at: u64,
        /// The datagram.
        payload: &'a [u8],
    },
    /// Ask a target for its metadata, answered with [`ToWsd::Fetched`] or
    /// [`ToWsd::FetchFailed`].
    Fetch(FetchRequest<'a>),
}

/// A metadata fetch the decoder asks the front to make. The front builds the
/// request itself, so the decoder names only where and to whom.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FetchRequest<'a> {
    /// The interface the target was heard on.
    pub interface: [u8; IF_NAME_LEN],
    /// The decoder's id for the fetch.
    pub fetch: u32,
    /// The target's address: one it sent from.
    pub addr: IpAddr,
    /// The port its URL names, never `0`.
    pub port: u16,
    /// The path its URL names, as a [`MetadataPath`] spells it.
    pub path: &'a str,
    /// The endpoint the request is addressed to, as an [`Endpoint`] spells
    /// it.
    pub endpoint: &'a str,
    /// The name its URL gives its host, as a [`HostName`] spells it, which
    /// the request's `Host` field names; `None` when the URL names `addr`.
    pub name: Option<&'a str>,
}

impl FetchRequest<'_> {
    /// Whether an engine could have asked for it.
    fn is_well_formed(&self) -> bool {
        self.port != 0
            && MetadataPath::new(self.path).is_some()
            && Endpoint::new(self.endpoint).is_some()
            && self.name.is_none_or(|name| HostName::new(name).is_some())
    }
}

impl<'a> FromWsd<'a> {
    fn encode(&self, out: &mut [u8]) -> Option<usize> {
        match *self {
            Self::Transmit {
                interface,
                at,
                payload,
            } => {
                if payload.is_empty() || payload.len() > MAX_MESSAGE_LEN {
                    return None;
                }
                let frame = out.get_mut(..WSD_TRANSMIT_HEADER_LEN + payload.len())?;
                frame[0] = TAG_WSD_TRANSMIT;
                frame[1..=IF_NAME_LEN].copy_from_slice(&interface);
                frame[17..WSD_TRANSMIT_HEADER_LEN].copy_from_slice(&at.to_le_bytes());
                frame[WSD_TRANSMIT_HEADER_LEN..].copy_from_slice(payload);
                Some(frame.len())
            }
            Self::Fetch(request) => {
                if !request.is_well_formed() {
                    return None;
                }
                let (path, endpoint) = (request.path.as_bytes(), request.endpoint.as_bytes());
                let name = request.name.map_or(&[][..], str::as_bytes);
                let texts = path.len() + endpoint.len() + name.len();
                let frame = out.get_mut(..FETCH_HEADER_LEN + texts)?;
                frame[0] = TAG_FETCH;
                frame[1..=IF_NAME_LEN].copy_from_slice(&request.interface);
                frame[17..21].copy_from_slice(&request.fetch.to_le_bytes());
                put_address(&mut frame[21..38], request.addr);
                frame[38..40].copy_from_slice(&request.port.to_le_bytes());
                frame[40] = u8::try_from(path.len()).ok()?;
                frame[41] = u8::try_from(endpoint.len()).ok()?;
                frame[42] = u8::try_from(name.len()).ok()?;
                let (path_field, rest) = frame[FETCH_HEADER_LEN..].split_at_mut(path.len());
                let (endpoint_field, name_field) = rest.split_at_mut(endpoint.len());
                path_field.copy_from_slice(path);
                endpoint_field.copy_from_slice(endpoint);
                name_field.copy_from_slice(name);
                Some(frame.len())
            }
        }
    }

    /// Decode what follows `tag` in a frame of `len` bytes.
    fn decode(tag: u8, reader: &mut Reader<'a>, len: usize) -> Result<Self, WireError> {
        let interface = interface(reader)?;
        match tag {
            TAG_WSD_TRANSMIT => {
                let at = reader.u64()?;
                let payload = reader.take(len.saturating_sub(WSD_TRANSMIT_HEADER_LEN))?;
                if payload.is_empty() || payload.len() > MAX_MESSAGE_LEN {
                    return Err(WireError::Malformed);
                }
                Ok(Self::Transmit {
                    interface,
                    at,
                    payload,
                })
            }
            TAG_FETCH => {
                let fetch = reader.u32()?;
                let addr = address(reader)?;
                let port = u16::from_le_bytes(*fixed(reader)?);
                let path_len = usize::from(reader.u8()?);
                let endpoint_len = usize::from(reader.u8()?);
                let name_len = usize::from(reader.u8()?);
                let path = text(reader.take(path_len)?)?;
                let endpoint = text(reader.take(endpoint_len)?)?;
                let name = text(reader.take(name_len)?)?;
                let request = FetchRequest {
                    interface,
                    fetch,
                    addr,
                    port,
                    path,
                    endpoint,
                    name: (!name.is_empty()).then_some(name),
                };
                if !request.is_well_formed() {
                    return Err(WireError::Malformed);
                }
                Ok(Self::Fetch(request))
            }
            _ => Err(WireError::Malformed),
        }
    }
}

/// Write the tag, instant and interface a frame to the decoder opens with.
fn stamp(frame: &mut [u8], tag: u8, now: u64, interface: [u8; IF_NAME_LEN]) {
    frame[0] = tag;
    frame[1..9].copy_from_slice(&now.to_le_bytes());
    frame[9..9 + IF_NAME_LEN].copy_from_slice(&interface);
}

/// Write `addr` into a seventeen-byte field as the family byte and sixteen
/// octets [`address`] reads.
fn put_address(field: &mut [u8], addr: IpAddr) {
    let (family, octets) = address_parts(addr);
    field[0] = family.as_u8();
    field[1..].copy_from_slice(&octets);
}

/// Bytes the frame carries as text, refused when they are not UTF-8.
fn text(bytes: &[u8]) -> Result<&str, WireError> {
    core::str::from_utf8(bytes).map_err(|_| WireError::Malformed)
}

/// What became of an interface a [`ToDecoder::Link`] names.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Edge {
    /// It came up, with its send budgets as the front holds them, which the
    /// decoder spends from exactly as the front charges them.
    Up(Budgets),
    /// It went down.
    Down,
}

/// An interface's two send budgets: multicast DNS's and WS-Discovery's.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Budgets {
    /// The multicast DNS group budget.
    pub group: BucketLevel,
    /// The WS-Discovery group budget.
    pub wsd: BucketLevel,
}

impl Budgets {
    /// The zero levels a down edge carries.
    const NONE: Self = Self {
        group: BucketLevel { scaled: 0, at: 0 },
        wsd: BucketLevel { scaled: 0, at: 0 },
    };

    /// Whether each budget holds no more than its burst.
    fn are_levels(self) -> bool {
        let at_most = |level: BucketLevel, burst: u32| {
            level.scaled <= u64::from(burst) * u64::from(NANOS_PER_SEC)
        };
        at_most(self.group, GROUP_BURST) && at_most(self.wsd, WSD_BURST)
    }
}

/// Whether `id` can name a question: every id is a slot below
/// [`MAX_QUESTIONS`].
fn is_question(id: u32) -> bool {
    usize::try_from(id).is_ok_and(|id| id < MAX_QUESTIONS)
}

/// A question id.
fn question(reader: &mut Reader<'_>) -> Result<u32, WireError> {
    let id = reader.u32()?;
    if !is_question(id) {
        return Err(WireError::Malformed);
    }
    Ok(id)
}

/// Read `N` raw bytes as a fixed array.
fn fixed<'a, const N: usize>(reader: &mut Reader<'a>) -> Result<&'a [u8; N], WireError> {
    reader.take(N)?.try_into().map_err(|_| WireError::Truncated)
}

/// An interface name the stack would use.
fn interface(reader: &mut Reader<'_>) -> Result<[u8; IF_NAME_LEN], WireError> {
    let name = *fixed::<IF_NAME_LEN>(reader)?;
    validate_if_name(&name).map_err(|_| WireError::Malformed)?;
    Ok(name)
}

/// A family byte and sixteen octets, an IPv4 address's tail zero.
fn address(reader: &mut Reader<'_>) -> Result<IpAddr, WireError> {
    let family = reader.u8()?;
    address_from(family, *fixed::<16>(reader)?)
}

fn address_from(family: u8, octets: [u8; 16]) -> Result<IpAddr, WireError> {
    let family = NetAddrFamily::from_u8(family).map_err(|_| WireError::Malformed)?;
    if family == NetAddrFamily::V4 && octets[4..].iter().any(|&byte| byte != 0) {
        return Err(WireError::Malformed);
    }
    Ok(ip_from_parts(family, octets))
}

fn flag(byte: u8) -> Result<bool, WireError> {
    match byte {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(WireError::Malformed),
    }
}

/// An answer entry: an [`Entry::Answer`], never a flush or a loss, which only
/// the front writes.
fn entry(bytes: &[u8]) -> Result<Entry<'_>, WireError> {
    match Entry::decode(bytes) {
        Ok(entry @ Entry::Answer { .. }) => Ok(entry),
        _ => Err(WireError::Malformed),
    }
}

#[cfg(test)]
#[path = "wire_tests.rs"]
mod tests;
