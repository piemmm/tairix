//! The client connection core: framing, credits and message ids, the
//! encryption transform on the way out, and on the way in its check and the
//! matching of every response to the request it answers.
//!
//! Once a session holds keys, everything sent on it is sealed, and a response
//! to a sealed request is accepted only sealed under the same session's keys:
//! an attacker on the path can neither read the session nor answer for the
//! server. The one exception is the unsolicited oplock or lease break, which a
//! server sends under no session's keys. A break only ever asks the client to
//! cache less, so honouring a forged one costs performance, while ignoring a
//! genuine one would serve stale data — the safe direction is to honour it.
//!
//! A frame is encoded once, into the buffer it is sent from, and sealed in
//! place; a received frame is opened in place, and a frame holding one
//! message is handed over in the buffer it arrived in.
//!
//! Any malformed, unexpected or failing message ends the connection: every
//! later call answers the first failure.

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::ops::{Deref, Range};

use tairix_collections::HashMap;
use tairix_hash::BuildFastHash;

use crate::crypt::{SessionKeys, Transform, TRANSFORM_HEADER_LEN, TRANSFORM_PROTOCOL_ID};
use crate::frame::{self, Frame, Framer, PREFIX_LEN};
use crate::header::{Address, Command, Flags, Header, NEXT_COMMAND_RANGE, PROTOCOL_ID};
use crate::simple::encode_empty;
use crate::status::NtStatus;
use crate::wire::{owned, Encoder};
use crate::SmbError;

/// The credit window this client asks a server to keep open.
const TARGET_CREDITS: u32 = 64;

/// The most credits a client holds, whatever a server grants: a bound on how
/// far ahead the message-id window can be pushed.
pub const MAX_CREDITS: u32 = 8192;

/// The message id an unsolicited oplock or lease break carries, which no
/// request may take.
const UNSOLICITED: u64 = u64::MAX;

/// Writes a request's body into the frame it is sent in, the encoder just
/// past the request's header and counting positions from the header's start.
pub type Body<'a> = &'a dyn Fn(&mut Encoder);

/// A request, as [`Connection::send`] takes it.
#[derive(Copy, Clone)]
pub struct Outgoing<'a> {
    /// The command.
    pub command: Command,
    /// The session, zero before one exists.
    pub session_id: u64,
    /// The tree, zero before one is connected.
    pub tree_id: u32,
    /// The body that follows the header.
    pub body: Body<'a>,
    /// How many credits the request consumes: one per 64 KiB of payload,
    /// at least one, zero for `NEGOTIATE` alone.
    pub credit_charge: u16,
    /// Flags beyond those the connection decides, such as
    /// [`Flags::RELATED_OPERATIONS`] within a compound.
    pub flags: Flags,
}

impl core::fmt::Debug for Outgoing<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Outgoing")
            .field("command", &self.command)
            .field("session_id", &self.session_id)
            .field("tree_id", &self.tree_id)
            .field("credit_charge", &self.credit_charge)
            .field("flags", &self.flags)
            .finish_non_exhaustive()
    }
}

/// The id a request's response is matched by: its message id.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Ticket(u64);

impl Ticket {
    /// The message id.
    #[must_use]
    pub const fn message_id(self) -> u64 {
        self.0
    }
}

/// What one [`Connection::send`] queued.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Sent {
    /// Each request's ticket, in order.
    pub tickets: Vec<Ticket>,
    /// The frame as it went in the clear — what the pre-auth integrity hash
    /// covers — or empty for a sealed frame.
    pub clear: Vec<u8>,
}

/// A message as it arrived, header included.
#[derive(Clone, Debug)]
pub struct Message {
    buf: Vec<u8>,
    range: Range<usize>,
}

impl Message {
    fn copied(bytes: &[u8]) -> Result<Self, SmbError> {
        let buf = owned(bytes)?;
        let range = 0..buf.len();
        Ok(Self { buf, range })
    }
}

impl Deref for Message {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.buf[self.range.clone()]
    }
}

impl PartialEq for Message {
    fn eq(&self, other: &Self) -> bool {
        **self == **other
    }
}

impl Eq for Message {}

/// What arrived from the server.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Received {
    /// The final response to a request.
    Response {
        /// The request it answers.
        ticket: Ticket,
        /// The response's header, as the connection checked it.
        header: Header,
        /// The response.
        message: Message,
        /// Whether it arrived sealed.
        sealed: bool,
    },
    /// An unsolicited oplock or lease break.
    Break {
        /// The break.
        message: Message,
    },
}

#[derive(Debug)]
struct Pending {
    command: Command,
    session_id: u64,
    tree_id: u32,
    sealed: bool,
    /// The id an interim response moved the request to.
    async_id: Option<u64>,
}

/// What one received message turned out to be.
enum Arrival {
    Response(Ticket, Header),
    Break,
    Interim,
}

/// A client's view of one connection.
pub struct Connection {
    framer: Framer,
    out: Vec<u8>,
    credits: u32,
    next_id: u64,
    pending: HashMap<u64, Pending, BuildFastHash>,
    /// One session per principal sharing the connection: a handful at most.
    sessions: Vec<(u64, SessionKeys)>,
    received: VecDeque<Received>,
    failed: Option<SmbError>,
}

impl core::fmt::Debug for Connection {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Connection")
            .field("credits", &self.credits)
            .field("next_id", &self.next_id)
            .field("pending", &self.pending.len())
            .field("failed", &self.failed)
            .finish_non_exhaustive()
    }
}

impl Default for Connection {
    fn default() -> Self {
        Self::new()
    }
}

impl Connection {
    /// A connection holding the one credit a `NEGOTIATE` is sent with.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            framer: Framer::new(),
            out: Vec::new(),
            credits: 1,
            next_id: 0,
            pending: HashMap::with_hasher(BuildFastHash::new()),
            sessions: Vec::new(),
            received: VecDeque::new(),
            failed: None,
        }
    }

    /// Credits granted and not yet spent.
    #[must_use]
    pub const fn credits(&self) -> u32 {
        self.credits
    }

    /// Requests sent and not yet answered.
    #[must_use]
    pub const fn outstanding(&self) -> usize {
        self.pending.len()
    }

    /// The failure that ended the connection, if one has.
    #[must_use]
    pub const fn failure(&self) -> Option<SmbError> {
        self.failed
    }

    /// Give `session_id` its keys: from now on everything sent on it is
    /// sealed and everything it answers must arrive sealed.
    ///
    /// # Errors
    ///
    /// [`SmbError::Protocol`] for session zero or a session that already
    /// holds keys — a session's keys are derived once, and replacing them
    /// would restart a nonce sequence under the same key.
    pub fn install_keys(&mut self, session_id: u64, keys: SessionKeys) -> Result<(), SmbError> {
        if session_id == 0 || self.keys(session_id).is_some() {
            return Err(SmbError::Protocol);
        }
        self.sessions
            .try_reserve(1)
            .map_err(|_| SmbError::OutOfMemory)?;
        self.sessions.push((session_id, keys));
        Ok(())
    }

    /// Forget `session_id` and its keys, once nothing is outstanding on it: a
    /// sealed answer arriving afterwards can no longer be opened, and ends the
    /// connection.
    pub fn remove_session(&mut self, session_id: u64) {
        self.sessions.retain(|(id, _)| *id != session_id);
    }

    fn keys(&self, session_id: u64) -> Option<&SessionKeys> {
        self.sessions
            .iter()
            .find(|(id, _)| *id == session_id)
            .map(|(_, keys)| keys)
    }

    fn keys_mut(&mut self, session_id: u64) -> Option<&mut SessionKeys> {
        self.sessions
            .iter_mut()
            .find(|(id, _)| *id == session_id)
            .map(|(_, keys)| keys)
    }

    fn guard(&self) -> Result<(), SmbError> {
        self.failed.map_or(Ok(()), Err)
    }

    fn fail(&mut self, err: SmbError) -> SmbError {
        if self.failed.is_none() {
            self.failed = Some(err);
        }
        err
    }

    /// Send `requests` in one frame — a compound when there are several.
    ///
    /// # Errors
    ///
    /// [`SmbError::NoCredit`] when the credits granted do not cover them,
    /// [`SmbError::Protocol`] for no request or a compound spanning
    /// sessions, or the failure that ended the connection.
    pub fn send(&mut self, requests: &[Outgoing<'_>]) -> Result<Sent, SmbError> {
        self.guard()?;
        let (first, rest) = requests.split_first().ok_or(SmbError::Protocol)?;
        let session_id = first.session_id;
        if rest.iter().any(|r| r.session_id != session_id) || requests.len() > frame::MAX_COMPOUND {
            return Err(SmbError::Protocol);
        }
        let charge: u32 = requests
            .iter()
            .map(|r| u32::from(r.credit_charge.max(1)))
            .sum();
        if charge > self.credits {
            return Err(SmbError::NoCredit);
        }
        let sealed = session_id != 0 && self.keys(session_id).is_some();
        let mut tickets = Vec::new();
        tickets
            .try_reserve(requests.len())
            .map_err(|_| SmbError::OutOfMemory)?;
        self.pending
            .try_reserve(requests.len())
            .map_err(|_| SmbError::OutOfMemory)?;

        let ask = u16::try_from(TARGET_CREDITS.saturating_sub(self.credits - charge).max(1))
            .unwrap_or(u16::MAX);
        let mut enc = framing(sealed);
        let mut id = self.next_id;
        for (index, request) in requests.iter().enumerate() {
            enc.start_message();
            Header {
                credit_charge: request.credit_charge,
                status: NtStatus::SUCCESS,
                command: request.command,
                credits: ask,
                flags: request.flags,
                next_command: 0,
                message_id: id,
                address: Address::Sync {
                    tree_id: request.tree_id,
                },
                session_id,
                signature: [0; 16],
            }
            .encode(&mut enc);
            (request.body)(&mut enc);
            if index + 1 < requests.len() {
                enc.align(8);
                let next = enc.u32_of(enc.position());
                enc.patch_u32(NEXT_COMMAND_RANGE.start, next);
            }
            tickets.push(Ticket(id));
            // A request taking the id a break carries, the last one, overflows
            // here and is never sent.
            id = id
                .checked_add(u64::from(request.credit_charge.max(1)))
                .ok_or(SmbError::TooLarge)?;
        }
        let frame = enc.finish()?;
        let clear = if sealed {
            Vec::new()
        } else {
            owned(&frame[PREFIX_LEN..])?
        };

        for (request, ticket) in requests.iter().zip(&tickets) {
            let pending = Pending {
                command: request.command,
                session_id,
                tree_id: request.tree_id,
                sealed,
                async_id: None,
            };
            if self.pending.try_insert(ticket.0, pending).is_err() {
                self.forget(&tickets);
                return Err(SmbError::OutOfMemory);
            }
        }
        if let Err(err) = self.queue(frame, session_id, sealed) {
            self.forget(&tickets);
            return Err(err);
        }
        self.credits -= charge;
        self.next_id = id;
        Ok(Sent { tickets, clear })
    }

    fn forget(&mut self, tickets: &[Ticket]) {
        for ticket in tickets {
            self.pending.remove(&ticket.0);
        }
    }

    /// Ask the server to cancel the request `ticket` names. The request
    /// still completes — normally with `STATUS_CANCELLED` — and a `CANCEL`
    /// consumes no credit.
    ///
    /// # Errors
    ///
    /// [`SmbError::Protocol`] when nothing is outstanding under `ticket`, or
    /// the failure that ended the connection.
    pub fn cancel(&mut self, ticket: Ticket) -> Result<(), SmbError> {
        self.guard()?;
        let pending = self.pending.get(&ticket.0).ok_or(SmbError::Protocol)?;
        let (session_id, sealed) = (pending.session_id, pending.sealed);
        let address = pending.async_id.map_or(
            Address::Sync {
                tree_id: pending.tree_id,
            },
            |async_id| Address::Async { async_id },
        );
        let mut enc = framing(sealed);
        enc.start_message();
        Header {
            credit_charge: 0,
            status: NtStatus::SUCCESS,
            command: Command::Cancel,
            credits: 0,
            flags: Flags::NONE,
            next_command: 0,
            message_id: ticket.0,
            address,
            session_id,
            signature: [0; 16],
        }
        .encode(&mut enc);
        encode_empty(&mut enc);
        self.queue(enc.finish()?, session_id, sealed)
    }

    /// Prefix `frame` — the room [`framing`] left, then the plaintext —
    /// sealing it for `session_id` first when `sealed`, and queue it for the
    /// socket.
    fn queue(&mut self, mut frame: Vec<u8>, session_id: u64, sealed: bool) -> Result<(), SmbError> {
        let len = frame
            .len()
            .checked_sub(PREFIX_LEN)
            .ok_or(SmbError::Malformed)?;
        let prefix = frame::prefix(len)?;
        if sealed {
            self.keys_mut(session_id)
                .ok_or(SmbError::Protocol)?
                .seal(session_id, &mut frame[PREFIX_LEN..])?;
        }
        frame[..PREFIX_LEN].copy_from_slice(&prefix);
        if self.out.is_empty() {
            self.out = frame;
        } else {
            self.out
                .try_reserve(frame.len())
                .map_err(|_| SmbError::OutOfMemory)?;
            self.out.extend_from_slice(&frame);
        }
        Ok(())
    }

    /// The bytes waiting to be written to the socket, taken.
    pub fn take_output(&mut self) -> Vec<u8> {
        core::mem::take(&mut self.out)
    }

    /// The next thing that arrived, if any.
    pub fn next_received(&mut self) -> Option<Received> {
        self.received.pop_front()
    }

    /// Take `bytes` the socket read and process every whole frame in them.
    ///
    /// # Errors
    ///
    /// The failure that ends the connection: a malformed frame or message,
    /// one that breaks the protocol, or one that fails its integrity check.
    pub fn receive(&mut self, bytes: &[u8]) -> Result<(), SmbError> {
        self.guard()?;
        self.framer.push(bytes).map_err(|err| self.fail(err))?;
        loop {
            let frame = match self.framer.next_frame() {
                Ok(Some(frame)) => frame,
                Ok(None) => return Ok(()),
                Err(err) => return Err(self.fail(err)),
            };
            self.frame(frame).map_err(|err| self.fail(err))?;
        }
    }

    fn frame(&mut self, frame: Frame) -> Result<(), SmbError> {
        let (mut buf, at) = frame.into_parts();
        let (start, sealed_by) = match buf.get(at..at + 4) {
            Some(id) if id == TRANSFORM_PROTOCOL_ID => {
                let session_id = Transform::parse(&buf[at..])?.session_id;
                self.keys(session_id)
                    .ok_or(SmbError::Integrity)?
                    .open(&mut buf[at..])?;
                (at + TRANSFORM_HEADER_LEN, Some(session_id))
            }
            Some(id) if id == PROTOCOL_ID => (at, None),
            // A compressed frame, never negotiated, or no protocol at all.
            _ => return Err(SmbError::Protocol),
        };
        let ranges = frame::messages(&buf[start..])?;
        let whole = ranges.len() == 1;
        for range in ranges {
            let range = start + range.start..start + range.end;
            let arrival = self.accept(&buf[range.clone()], sealed_by)?;
            let message = || {
                if whole {
                    Ok(Message {
                        buf: core::mem::take(&mut buf),
                        range,
                    })
                } else {
                    Message::copied(&buf[range])
                }
            };
            match arrival {
                Arrival::Interim => {}
                Arrival::Break => {
                    let message = message()?;
                    self.push(Received::Break { message })?;
                }
                Arrival::Response(ticket, header) => {
                    let message = message()?;
                    self.push(Received::Response {
                        ticket,
                        header,
                        message,
                        sealed: sealed_by.is_some(),
                    })?;
                }
            }
        }
        Ok(())
    }

    /// Check one received message against what is outstanding, settle its
    /// credits, and say what it is.
    fn accept(&mut self, message: &[u8], sealed_by: Option<u64>) -> Result<Arrival, SmbError> {
        let header = Header::decode(message)?;
        if !header.is_response() {
            return Err(SmbError::Protocol);
        }
        if header.message_id == UNSOLICITED {
            return if header.command == Command::OplockBreak {
                Ok(Arrival::Break)
            } else {
                Err(SmbError::Protocol)
            };
        }
        let pending = self
            .pending
            .get_mut(&header.message_id)
            .ok_or(SmbError::Protocol)?;
        if header.command != pending.command {
            return Err(SmbError::Protocol);
        }
        if pending.sealed {
            if sealed_by != Some(pending.session_id) {
                return Err(SmbError::Integrity);
            }
            if header.session_id != pending.session_id {
                return Err(SmbError::Protocol);
            }
        }
        if header.status == NtStatus::PENDING {
            // An interim response, at most one: the final one follows under
            // the id it names, which a cancel then addresses.
            match (header.address, pending.async_id) {
                (Address::Async { async_id }, None) => pending.async_id = Some(async_id),
                _ => return Err(SmbError::Protocol),
            }
        } else if let Some(async_id) = pending.async_id {
            if header.address != (Address::Async { async_id }) {
                return Err(SmbError::Protocol);
            }
        }
        self.credits = self
            .credits
            .saturating_add(u32::from(header.credits))
            .min(MAX_CREDITS);
        if header.status == NtStatus::PENDING {
            return Ok(Arrival::Interim);
        }
        self.pending.remove(&header.message_id);
        // With nothing outstanding no later response can grant a credit, so
        // a server that leaves none has wedged the connection (MS-SMB2
        // §3.3.1.2 forbids it).
        if self.credits == 0 && self.pending.is_empty() {
            return Err(SmbError::Protocol);
        }
        Ok(Arrival::Response(Ticket(header.message_id), header))
    }

    fn push(&mut self, received: Received) -> Result<(), SmbError> {
        self.received
            .try_reserve(1)
            .map_err(|_| SmbError::OutOfMemory)?;
        self.received.push_back(received);
        Ok(())
    }
}

/// An encoder holding room for what goes in front of a frame's plaintext:
/// its prefix, and the transform header when it is sealed.
fn framing(sealed: bool) -> Encoder {
    let mut enc = Encoder::new();
    enc.zeros(PREFIX_LEN);
    if sealed {
        enc.zeros(TRANSFORM_HEADER_LEN);
    }
    enc
}

#[cfg(test)]
#[path = "connection_tests.rs"]
mod tests;
