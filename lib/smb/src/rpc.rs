//! DCE/RPC over an SMB named pipe (C706 §12, MS-RPCE §2.1.1.2), and the one
//! call this client makes on it: listing a server's shares with `srvsvc`'s
//! `NetrShareEnum` at level 1 (MS-SRVS §3.1.4.8).
//!
//! [`ShareListing`] is the whole exchange as a sequence of requests on the
//! `IPC$` tree — open the pipe, bind, call, read the rest of the answer,
//! close — and checks each answer before asking the next: the call is bound
//! with NDR and answered under the ids it was made with, no fragment is
//! larger than this client said it receives, and the answer is bounded. A
//! listing that fails still closes the pipe it opened.
//!
//! Share names and remarks are a stranger's text: a name is kept only when a
//! tree connect could name it ([`is_share_name`]), and both are for a
//! display-safety policy to show, never to be shown raw.

use alloc::string::String;
use alloc::vec::Vec;

use crate::file::create::{
    Access, CreateOptions, CreateRequest, CreateResponse, Disposition, ShareAccess,
};
use crate::file::io::{
    CloseRequest, CloseResponse, ReadRequest, ReadResponse, WriteRequest, WriteResponse,
};
use crate::file::name::SharePath;
use crate::file::{FileAttributes, FileId};
use crate::header::{Command, Header};
use crate::ioctl::{IoctlRequest, IoctlResponse, IS_FSCTL};
use crate::status::NtStatus;
use crate::tree::is_share_name;
use crate::wire::{from_utf16le, utf16le, Encoder, Reader};
use crate::SmbError;

/// The pipe `srvsvc` listens on.
pub const PIPE: &str = "srvsvc";

/// The control that writes a request to a pipe and reads its answer in one
/// round trip (MS-FSCC §2.3.49).
pub const FSCTL_PIPE_TRANSCEIVE: u32 = 0x0011_c017;

/// The largest fragment this client sends or receives: what Windows uses
/// over a pipe.
pub const MAX_FRAGMENT: usize = 4280;

/// The most shares one listing holds.
pub const MAX_SHARES: usize = 256;

/// The most stub data one answer may carry, across all its fragments.
pub const MAX_STUB: usize = 256 << 10;

/// The `NetrShareEnum` operation's number.
pub const SHARE_ENUM: u16 = 15;

/// The share type's flag for a special share: `IPC$`, `ADMIN$`, a drive's.
pub const SPECIAL: u32 = 0x8000_0000;

/// The share type's flag for a share not kept across restarts.
pub const TEMPORARY: u32 = 0x4000_0000;

/// Little-endian integers, ASCII characters, IEEE floats.
const DREP: [u8; 4] = [0x10, 0, 0, 0];

const FIRST_FRAG: u8 = 0x01;
const LAST_FRAG: u8 = 0x02;
const OBJECT_UUID: u8 = 0x80;

/// The type of each PDU this client speaks or reads.
mod ptype {
    pub const REQUEST: u8 = 0;
    pub const RESPONSE: u8 = 2;
    pub const FAULT: u8 = 3;
    pub const BIND: u8 = 11;
    pub const BIND_ACK: u8 = 12;
    pub const BIND_NAK: u8 = 13;
}

/// A bind result accepting the context.
pub const ACCEPTANCE: u16 = 0;

/// The share type's bits naming its kind.
const KIND_MASK: u32 = 0xff;

/// The ids this client binds and calls under.
const BIND_CALL: u32 = 1;
const SHARE_ENUM_CALL: u32 = 2;
const CONTEXT: u16 = 0;

/// A UUID's 16 bytes as DCE puts them on the wire: its first three fields
/// little-endian, the rest as written.
#[must_use]
pub const fn uuid(time_low: u32, time_mid: u16, time_high: u16, rest: [u8; 8]) -> [u8; 16] {
    let low = time_low.to_le_bytes();
    let mid = time_mid.to_le_bytes();
    let high = time_high.to_le_bytes();
    [
        low[0], low[1], low[2], low[3], mid[0], mid[1], high[0], high[1], rest[0], rest[1],
        rest[2], rest[3], rest[4], rest[5], rest[6], rest[7],
    ]
}

/// An interface or transfer syntax: its UUID, wire order, and its version,
/// the major number in the low half.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Syntax {
    /// The UUID.
    pub uuid: [u8; 16],
    /// The version.
    pub version: u32,
}

impl Syntax {
    fn encode(&self, enc: &mut Encoder) {
        enc.bytes(&self.uuid);
        enc.u32(self.version);
    }

    fn read(r: &mut Reader<'_>) -> Result<Self, SmbError> {
        Ok(Self {
            uuid: r.array()?,
            version: r.u32()?,
        })
    }
}

/// `srvsvc`, version 3.0 (MS-SRVS §1.9).
pub const SRVSVC: Syntax = Syntax {
    uuid: uuid(
        0x4b32_4fc8,
        0x1670,
        0x01d3,
        [0x12, 0x78, 0x5a, 0x47, 0xbf, 0x6e, 0xe1, 0x88],
    ),
    version: 3,
};

/// NDR 2.0 (C706 §14).
pub const NDR: Syntax = Syntax {
    uuid: uuid(
        0x8a88_5d04,
        0x1ceb,
        0x11c9,
        [0x9f, 0xe8, 0x08, 0x00, 0x2b, 0x10, 0x48, 0x60],
    ),
    version: 2,
};

/// A bind offering one presentation context (C706 §12.6.4.3).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Bind {
    /// The call id.
    pub call_id: u32,
    /// The largest fragment the client sends.
    pub max_xmit_frag: u16,
    /// The largest fragment the client receives.
    pub max_recv_frag: u16,
    /// The context's id.
    pub context: u16,
    /// The interface bound to.
    pub abstract_syntax: Syntax,
    /// The one transfer syntax offered.
    pub transfer_syntax: Syntax,
}

/// A bind's acknowledgment, answering its one context (C706 §12.6.4.4).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct BindAck<'a> {
    /// The call id.
    pub call_id: u32,
    /// The largest fragment the server sends.
    pub max_xmit_frag: u16,
    /// The largest fragment the server receives.
    pub max_recv_frag: u16,
    /// The association the binding joined.
    pub assoc_group: u32,
    /// The server's name for the endpoint, as it spells it.
    pub secondary_address: &'a [u8],
    /// The context's result: [`ACCEPTANCE`] or a rejection.
    pub result: u16,
    /// Why it was rejected.
    pub reason: u16,
    /// The transfer syntax accepted.
    pub transfer_syntax: Syntax,
}

/// A request fragment (C706 §12.6.4.9).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct RequestFragment<'a> {
    /// The call id.
    pub call_id: u32,
    /// Whether this is the call's first fragment.
    pub first: bool,
    /// Whether this is the call's last fragment.
    pub last: bool,
    /// The context the call is made in.
    pub context: u16,
    /// The operation.
    pub opnum: u16,
    /// The fragment's stub data.
    pub stub: &'a [u8],
}

/// A response fragment (C706 §12.6.4.10).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ResponseFragment<'a> {
    /// The call id.
    pub call_id: u32,
    /// Whether this is the answer's first fragment.
    pub first: bool,
    /// Whether this is the answer's last fragment.
    pub last: bool,
    /// The context the call was made in.
    pub context: u16,
    /// The fragment's stub data.
    pub stub: &'a [u8],
}

/// A connection-oriented PDU this client sends or reads. None carries
/// authentication: the SMB session beneath already seals every byte.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Pdu<'a> {
    /// A bind.
    Bind(Bind),
    /// Its acknowledgment.
    BindAck(BindAck<'a>),
    /// Its rejection.
    BindNak {
        /// The call id.
        call_id: u32,
        /// Why it was rejected.
        reason: u16,
    },
    /// A request fragment.
    Request(RequestFragment<'a>),
    /// A response fragment.
    Response(ResponseFragment<'a>),
    /// A call's failure.
    Fault {
        /// The call id.
        call_id: u32,
        /// The failure's status.
        status: u32,
    },
}

/// Append the common header, and the place its fragment length is patched.
fn pdu_header(enc: &mut Encoder, ptype: u8, flags: u8, call_id: u32) -> usize {
    let start = enc.position();
    enc.u8(5);
    enc.u8(0);
    enc.u8(ptype);
    enc.u8(flags);
    enc.bytes(&DREP);
    enc.u16(0);
    enc.u16(0);
    enc.u32(call_id);
    start
}

fn patch_length(enc: &mut Encoder, start: usize) {
    let len = enc.u16_of(enc.position().saturating_sub(start));
    enc.patch_u16(start + 8, len);
}

const fn fragment_flags(first: bool, last: bool) -> u8 {
    (if first { FIRST_FRAG } else { 0 }) | (if last { LAST_FRAG } else { 0 })
}

impl<'a> Pdu<'a> {
    /// Append the PDU.
    pub fn encode(&self, enc: &mut Encoder) {
        let start = match self {
            Self::Bind(bind) => {
                let start = pdu_header(enc, ptype::BIND, FIRST_FRAG | LAST_FRAG, bind.call_id);
                enc.u16(bind.max_xmit_frag);
                enc.u16(bind.max_recv_frag);
                enc.u32(0);
                enc.u8(1);
                enc.zeros(3);
                enc.u16(bind.context);
                enc.u8(1);
                enc.u8(0);
                bind.abstract_syntax.encode(enc);
                bind.transfer_syntax.encode(enc);
                start
            }
            Self::BindAck(ack) => {
                let start = pdu_header(enc, ptype::BIND_ACK, FIRST_FRAG | LAST_FRAG, ack.call_id);
                enc.u16(ack.max_xmit_frag);
                enc.u16(ack.max_recv_frag);
                enc.u32(ack.assoc_group);
                let len = enc.u16_of(ack.secondary_address.len());
                enc.u16(len);
                enc.bytes(ack.secondary_address);
                let at = enc.position() - start;
                enc.zeros(at.next_multiple_of(4) - at);
                enc.u8(1);
                enc.zeros(3);
                enc.u16(ack.result);
                enc.u16(ack.reason);
                ack.transfer_syntax.encode(enc);
                start
            }
            Self::BindNak { call_id, reason } => {
                let start = pdu_header(enc, ptype::BIND_NAK, FIRST_FRAG | LAST_FRAG, *call_id);
                enc.u16(*reason);
                enc.u8(1);
                enc.u8(5);
                enc.u8(0);
                start
            }
            Self::Request(request) => {
                let flags = fragment_flags(request.first, request.last);
                let start = pdu_header(enc, ptype::REQUEST, flags, request.call_id);
                let hint = enc.u32_of(request.stub.len());
                enc.u32(hint);
                enc.u16(request.context);
                enc.u16(request.opnum);
                enc.bytes(request.stub);
                start
            }
            Self::Response(response) => {
                let flags = fragment_flags(response.first, response.last);
                let start = pdu_header(enc, ptype::RESPONSE, flags, response.call_id);
                let hint = enc.u32_of(response.stub.len());
                enc.u32(hint);
                enc.u16(response.context);
                enc.u8(0);
                enc.u8(0);
                enc.bytes(response.stub);
                start
            }
            Self::Fault { call_id, status } => {
                let start = pdu_header(enc, ptype::FAULT, FIRST_FRAG | LAST_FRAG, *call_id);
                enc.u32(0);
                enc.u16(0);
                enc.u8(0);
                enc.u8(0);
                enc.u32(*status);
                enc.u32(0);
                start
            }
        };
        patch_length(enc, start);
    }

    /// The one PDU `pdu` holds, whole.
    ///
    /// # Errors
    ///
    /// [`SmbError::Malformed`] for anything but one well-formed PDU of a type
    /// this client speaks, in its data representation;
    /// [`SmbError::Protocol`] for one carrying authentication, which this
    /// client never binds with.
    pub fn decode(pdu: &'a [u8]) -> Result<Self, SmbError> {
        let mut r = Reader::new(pdu);
        if r.u8()? != 5 || r.u8()? != 0 {
            return Err(SmbError::Malformed);
        }
        let kind = r.u8()?;
        let flags = r.u8()?;
        if r.array::<4>()? != DREP {
            return Err(SmbError::Malformed);
        }
        let frag_length = usize::from(r.u16()?);
        let auth_length = r.u16()?;
        let call_id = r.u32()?;
        if frag_length != pdu.len() {
            return Err(SmbError::Malformed);
        }
        if auth_length != 0 {
            return Err(SmbError::Protocol);
        }
        let (first, last) = (flags & FIRST_FRAG != 0, flags & LAST_FRAG != 0);
        match kind {
            ptype::BIND => read_bind(&mut r, call_id),
            ptype::BIND_ACK => read_bind_ack(&mut r, pdu.len(), call_id),
            ptype::BIND_NAK => {
                let reason = r.u16()?;
                let versions = usize::from(r.u8()?);
                r.skip(versions * 2)?;
                Ok(Self::BindNak { call_id, reason })
            }
            ptype::REQUEST if flags & OBJECT_UUID == 0 => {
                r.skip(4)?;
                let context = r.u16()?;
                let opnum = r.u16()?;
                Ok(Self::Request(RequestFragment {
                    call_id,
                    first,
                    last,
                    context,
                    opnum,
                    stub: r.bytes(r.remaining())?,
                }))
            }
            ptype::RESPONSE => {
                r.skip(4)?;
                let context = r.u16()?;
                r.skip(2)?;
                Ok(Self::Response(ResponseFragment {
                    call_id,
                    first,
                    last,
                    context,
                    stub: r.bytes(r.remaining())?,
                }))
            }
            ptype::FAULT => {
                r.skip(8)?;
                Ok(Self::Fault {
                    call_id,
                    status: r.u32()?,
                })
            }
            _ => Err(SmbError::Malformed),
        }
    }
}

/// A bind's body: one context, one transfer syntax, nothing after.
fn read_bind<'a>(r: &mut Reader<'a>, call_id: u32) -> Result<Pdu<'a>, SmbError> {
    let max_xmit_frag = r.u16()?;
    let max_recv_frag = r.u16()?;
    r.skip(4)?;
    let contexts = r.u8()?;
    r.skip(3)?;
    let context = r.u16()?;
    let transfers = r.u8()?;
    r.skip(1)?;
    if contexts != 1 || transfers != 1 {
        return Err(SmbError::Malformed);
    }
    let bind = Bind {
        call_id,
        max_xmit_frag,
        max_recv_frag,
        context,
        abstract_syntax: Syntax::read(r)?,
        transfer_syntax: Syntax::read(r)?,
    };
    end(r)?;
    Ok(Pdu::Bind(bind))
}

/// A bind acknowledgment's body, `len` bytes of PDU in all: its secondary
/// address padded to four bytes from the PDU's start, then one result.
fn read_bind_ack<'a>(r: &mut Reader<'a>, len: usize, call_id: u32) -> Result<Pdu<'a>, SmbError> {
    let max_xmit_frag = r.u16()?;
    let max_recv_frag = r.u16()?;
    let assoc_group = r.u32()?;
    let address_len = usize::from(r.u16()?);
    let secondary_address = r.bytes(address_len)?;
    let at = len - r.remaining();
    r.skip(at.next_multiple_of(4) - at)?;
    if r.u8()? != 1 {
        return Err(SmbError::Malformed);
    }
    r.skip(3)?;
    let ack = BindAck {
        call_id,
        max_xmit_frag,
        max_recv_frag,
        assoc_group,
        secondary_address,
        result: r.u16()?,
        reason: r.u16()?,
        transfer_syntax: Syntax::read(r)?,
    };
    end(r)?;
    Ok(Pdu::BindAck(ack))
}

fn end(r: &Reader<'_>) -> Result<(), SmbError> {
    if r.remaining() == 0 {
        Ok(())
    } else {
        Err(SmbError::Malformed)
    }
}

/// Reads NDR (C706 §14) from a stub, each field aligned from the stub's
/// start.
struct Ndr<'a> {
    stub: &'a [u8],
    r: Reader<'a>,
}

impl<'a> Ndr<'a> {
    const fn new(stub: &'a [u8]) -> Self {
        Self {
            stub,
            r: Reader::new(stub),
        }
    }

    fn u32(&mut self) -> Result<u32, SmbError> {
        let at = self.stub.len() - self.r.remaining();
        self.r.skip(at.next_multiple_of(4) - at)?;
        self.r.u32()
    }

    fn count(&mut self) -> Result<usize, SmbError> {
        usize::try_from(self.u32()?).map_err(|_| SmbError::Malformed)
    }

    /// Whether the next pointer is set.
    fn pointer(&mut self) -> Result<bool, SmbError> {
        Ok(self.u32()? != 0)
    }

    /// A conformant varying `[string]`: its UTF-16LE units, the terminator
    /// they must end with left off.
    fn string(&mut self) -> Result<&'a [u8], SmbError> {
        let max_count = self.count()?;
        let offset = self.u32()?;
        let actual = self.count()?;
        if offset != 0 || actual == 0 || actual > max_count {
            return Err(SmbError::Malformed);
        }
        let units = self
            .r
            .bytes(actual.checked_mul(2).ok_or(SmbError::Malformed)?)?;
        let (text, terminator) = units.split_at(units.len() - 2);
        if terminator != [0, 0] {
            return Err(SmbError::Malformed);
        }
        Ok(text)
    }

    fn finish(&self) -> Result<(), SmbError> {
        end(&self.r)
    }
}

/// Append `text` as a conformant varying `[string]`, terminator included.
fn write_string(enc: &mut Encoder, text: &str) {
    enc.align(4);
    let count = enc.u32_of(text.encode_utf16().count() + 1);
    enc.u32(count);
    enc.u32(0);
    enc.u32(count);
    utf16le(text, enc);
    enc.u16(0);
}

/// A non-null pointer's referent id: any value names one, and these are the
/// ones Windows uses.
const fn referent(index: u32) -> u32 {
    0x0002_0000 + 4 * index
}

/// Write the `NetrShareEnum` request's stub, for every share at level 1, the
/// server named as `server` is: `\\` and the name dialled.
pub fn write_share_enum_request(enc: &mut Encoder, server: &str) {
    enc.u32(referent(0));
    write_string(enc, server);
    enc.align(4);
    enc.u32(1);
    enc.u32(1);
    enc.u32(referent(1));
    enc.u32(0);
    enc.u32(0);
    enc.u32(u32::MAX);
    enc.u32(0);
}

/// The server a `NetrShareEnum` request's stub names, once it asks for every
/// share at level 1 and nothing more.
///
/// # Errors
///
/// [`SmbError::Malformed`] for anything else.
pub fn read_share_enum_request(stub: &[u8]) -> Result<String, SmbError> {
    let mut ndr = Ndr::new(stub);
    let server = if ndr.pointer()? {
        from_utf16le(ndr.string()?)?
    } else {
        String::new()
    };
    let shape = [
        ndr.u32()?,
        ndr.u32()?,
        u32::from(ndr.pointer()?),
        ndr.u32()?,
        u32::from(ndr.pointer()?),
    ];
    let preferred = ndr.u32()?;
    let resume = ndr.pointer()?;
    if shape != [1, 1, 1, 0, 0] || preferred != u32::MAX || resume {
        return Err(SmbError::Malformed);
    }
    ndr.finish()?;
    Ok(server)
}

/// What kind of resource a share is (MS-SRVS §2.2.2.4).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum ShareKind {
    /// Files.
    Disk,
    /// A print queue.
    PrintQueue,
    /// A communication device.
    Device,
    /// Interprocess communication: `IPC$`.
    Ipc,
    /// A kind this client does not name.
    Other(u8),
}

impl ShareKind {
    /// The kind's number in a share type.
    #[must_use]
    pub fn code(self) -> u32 {
        match self {
            Self::Disk => 0,
            Self::PrintQueue => 1,
            Self::Device => 2,
            Self::Ipc => 3,
            Self::Other(code) => u32::from(code),
        }
    }

    fn of(share_type: u32) -> Self {
        match share_type & KIND_MASK {
            0 => Self::Disk,
            1 => Self::PrintQueue,
            2 => Self::Device,
            3 => Self::Ipc,
            code => Self::Other(u8::try_from(code).unwrap_or(u8::MAX)),
        }
    }
}

/// One share a server listed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Share {
    /// Its name, one a tree connect can take.
    pub name: String,
    /// What it is.
    pub kind: ShareKind,
    /// Whether it is a special share: `IPC$`, `ADMIN$`, a drive's.
    pub special: bool,
    /// Whether it lasts only until the server restarts.
    pub temporary: bool,
    /// The server's description of it. A unit that does not decode is
    /// U+FFFD: this is display text, and a bad unit costs it no share.
    pub remark: String,
}

/// A server's shares.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ShareList {
    /// The shares a tree connect can name, in the server's order.
    pub shares: Vec<Share>,
    /// How many it listed under a name no tree connect could take.
    pub omitted: usize,
}

/// One share as a server lists it: what [`read_share_enum`] reads back.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ShareRecord<'a> {
    /// Its name.
    pub name: &'a str,
    /// Its type: a [`ShareKind`]'s code, and [`SPECIAL`] or [`TEMPORARY`].
    pub share_type: u32,
    /// Its remark.
    pub remark: &'a str,
}

/// Write a `NetrShareEnum` response's stub: `records` and success, or, when
/// `status` is not zero, that failure and no shares.
pub fn write_share_enum(enc: &mut Encoder, records: &[ShareRecord<'_>], status: u32) {
    enc.u32(1);
    enc.u32(1);
    if status == 0 {
        let count = enc.u32_of(records.len());
        enc.u32(referent(0));
        enc.u32(count);
        enc.u32(if records.is_empty() { 0 } else { referent(1) });
        if !records.is_empty() {
            enc.u32(count);
            for (index, record) in (0u32..).zip(records) {
                enc.u32(referent(2 + 2 * index));
                enc.u32(record.share_type);
                enc.u32(referent(3 + 2 * index));
            }
            for record in records {
                write_string(enc, record.name);
                write_string(enc, record.remark);
            }
        }
        enc.align(4);
        enc.u32(count);
    } else {
        enc.u32(0);
        enc.u32(0);
    }
    enc.u32(0);
    enc.u32(status);
}

/// The shares a `NetrShareEnum` response's stub lists.
///
/// # Errors
///
/// [`SmbError::Rpc`] with the call's status when it failed;
/// [`SmbError::TooLarge`] past [`MAX_SHARES`]; [`SmbError::Malformed`] for
/// a stub that is not a level-1 answer; [`SmbError::OutOfMemory`].
pub fn read_share_enum(stub: &[u8]) -> Result<ShareList, SmbError> {
    let mut ndr = Ndr::new(stub);
    if ndr.u32()? != 1 || ndr.u32()? != 1 {
        return Err(SmbError::Malformed);
    }
    let container = ndr.pointer()?;
    let (count, array) = if container {
        (ndr.count()?, ndr.pointer()?)
    } else {
        (0, false)
    };
    if count > MAX_SHARES {
        return Err(SmbError::TooLarge);
    }
    if (count > 0) != array {
        return Err(SmbError::Malformed);
    }
    let mut records = Vec::new();
    records
        .try_reserve_exact(count)
        .map_err(|_| SmbError::OutOfMemory)?;
    if array {
        if ndr.count()? != count {
            return Err(SmbError::Malformed);
        }
        for _ in 0..count {
            records.push((ndr.pointer()?, ndr.u32()?, ndr.pointer()?));
        }
    }
    let mut list = ShareList::default();
    list.shares
        .try_reserve_exact(count)
        .map_err(|_| SmbError::OutOfMemory)?;
    for &(named, share_type, remarked) in &records {
        let name = if named { Some(ndr.string()?) } else { None };
        let remark = if remarked { Some(ndr.string()?) } else { None };
        match name.map(from_utf16le).transpose() {
            Ok(Some(name)) if is_share_name(&name) => list.shares.push(Share {
                name,
                kind: ShareKind::of(share_type),
                special: share_type & SPECIAL != 0,
                temporary: share_type & TEMPORARY != 0,
                remark: remark.map_or(Ok(String::new()), lossy)?,
            }),
            Err(SmbError::OutOfMemory) => return Err(SmbError::OutOfMemory),
            _ => list.omitted += 1,
        }
    }
    ndr.u32()?;
    if ndr.pointer()? {
        ndr.u32()?;
    }
    let status = ndr.u32()?;
    ndr.finish()?;
    if status != 0 {
        return Err(SmbError::Rpc(status));
    }
    if !container {
        return Err(SmbError::Malformed);
    }
    Ok(list)
}

/// UTF-16LE `units` as text, each unit that does not decode a U+FFFD.
fn lossy(units: &[u8]) -> Result<String, SmbError> {
    let (units, _) = units.as_chunks::<2>();
    let mut text = String::new();
    text.try_reserve(units.len() * 3)
        .map_err(|_| SmbError::OutOfMemory)?;
    text.extend(
        char::decode_utf16(units.iter().map(|unit| u16::from_le_bytes(*unit)))
            .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER)),
    );
    Ok(text)
}

/// Where a listing stands: the request it sends next.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Step {
    Open,
    Bind,
    AwaitBind,
    Call,
    AwaitAnswer,
    Close,
    Over,
}

/// Listing a server's shares through `srvsvc` on its `IPC$` tree.
///
/// Send the request [`ShareListing::command`] names, with the body
/// [`ShareListing::encode`] writes, and hand its response to
/// [`ShareListing::answer`]; when there is no request left,
/// [`ShareListing::finish`] says what was found. The answer to one request
/// is awaited before the next is sent.
#[derive(Debug)]
pub struct ShareListing {
    step: Step,
    open: CreateRequest,
    bind: Vec<u8>,
    call: Vec<u8>,
    pipe: Option<FileId>,
    /// The call, once the pipe it goes on is known.
    transceive: Option<IoctlRequest>,
    pdu: Vec<u8>,
    /// Whether the answer's first fragment has arrived.
    answering: bool,
    stub: Vec<u8>,
    list: Option<ShareList>,
    failed: Option<SmbError>,
}

impl ShareListing {
    /// A listing of the shares of the server dialled as `server_name`.
    ///
    /// # Errors
    ///
    /// [`SmbError::TooLarge`] for a name the call cannot carry in one
    /// fragment; [`SmbError::OutOfMemory`].
    pub fn new(server_name: &str) -> Result<Self, SmbError> {
        let mut server = String::new();
        server
            .try_reserve(2 + server_name.len())
            .map_err(|_| SmbError::OutOfMemory)?;
        server.push_str(r"\\");
        server.push_str(server_name);
        let mut stub = Encoder::new();
        write_share_enum_request(&mut stub, &server);
        let stub = stub.finish()?;
        let mut call = Encoder::new();
        Pdu::Request(RequestFragment {
            call_id: SHARE_ENUM_CALL,
            first: true,
            last: true,
            context: CONTEXT,
            opnum: SHARE_ENUM,
            stub: &stub,
        })
        .encode(&mut call);
        let call = call.finish()?;
        if call.len() > MAX_FRAGMENT {
            return Err(SmbError::TooLarge);
        }
        let frag = u16::try_from(MAX_FRAGMENT).map_err(|_| SmbError::TooLarge)?;
        let mut bind = Encoder::new();
        Pdu::Bind(Bind {
            call_id: BIND_CALL,
            max_xmit_frag: frag,
            max_recv_frag: frag,
            context: CONTEXT,
            abstract_syntax: SRVSVC,
            transfer_syntax: NDR,
        })
        .encode(&mut bind);
        Ok(Self {
            step: Step::Open,
            open: CreateRequest {
                path: SharePath::root().join(PIPE)?,
                access: Access::READ_DATA
                    .with(Access::WRITE_DATA)
                    .with(Access::READ_ATTRIBUTES),
                attributes: FileAttributes::NONE,
                share: ShareAccess::READ.with(ShareAccess::WRITE),
                disposition: Disposition::Open,
                options: CreateOptions::NON_DIRECTORY_FILE,
                lease: None,
                durable: None,
                maximal_access: false,
                disk_id: false,
            },
            bind: bind.finish()?,
            call,
            pipe: None,
            transceive: None,
            pdu: Vec::new(),
            answering: false,
            stub: Vec::new(),
            list: None,
            failed: None,
        })
    }

    /// The command of the request to send next, or `None` once the listing is
    /// over.
    #[must_use]
    pub const fn command(&self) -> Option<Command> {
        match self.step {
            Step::Open => Some(Command::Create),
            Step::Bind => Some(Command::Write),
            Step::AwaitBind | Step::AwaitAnswer => Some(Command::Read),
            Step::Call => Some(Command::Ioctl),
            Step::Close => Some(Command::Close),
            Step::Over => None,
        }
    }

    /// Append the next request's body after its header.
    pub fn encode(&self, enc: &mut Encoder) {
        let pipe = self.pipe.unwrap_or_default();
        let fragment = u32::try_from(MAX_FRAGMENT).unwrap_or(u32::MAX);
        match self.step {
            Step::Open => self.open.encode(enc),
            Step::Bind => WriteRequest {
                offset: 0,
                file_id: pipe,
                write_through: false,
                data: &self.bind,
            }
            .encode(enc),
            Step::AwaitBind | Step::AwaitAnswer => ReadRequest::new(pipe, 0, fragment).encode(enc),
            Step::Call => match &self.transceive {
                Some(request) => request.encode(enc),
                None => enc.fail(SmbError::Protocol),
            },
            Step::Close => CloseRequest {
                post_query: false,
                file_id: pipe,
            }
            .encode(enc),
            Step::Over => enc.fail(SmbError::Protocol),
        }
    }

    /// Take the response to the request last sent.
    pub fn answer(&mut self, message: &[u8]) {
        if let Err(err) = self.take(message) {
            self.failed.get_or_insert(err);
            self.step = if self.pipe.is_some() && self.step != Step::Close {
                Step::Close
            } else {
                Step::Over
            };
        }
    }

    /// The shares found, once no request is left.
    ///
    /// # Errors
    ///
    /// The listing's first failure: an answer's status, [`SmbError::Rpc`]
    /// for a refused bind or a failed call, or a malformed or protocol-
    /// breaking answer; [`SmbError::Protocol`] while requests are left.
    pub fn finish(self) -> Result<ShareList, SmbError> {
        if let Some(err) = self.failed {
            return Err(err);
        }
        match (self.step, self.list) {
            (Step::Over, Some(list)) => Ok(list),
            _ => Err(SmbError::Protocol),
        }
    }

    fn take(&mut self, message: &[u8]) -> Result<(), SmbError> {
        let status = Header::decode(message)?.status;
        match self.step {
            Step::Open => {
                succeeded(status)?;
                self.pipe = Some(CreateResponse::decode(message)?.file_id);
                self.step = Step::Bind;
            }
            Step::Bind => {
                succeeded(status)?;
                let written = WriteResponse::decode(message)?.count;
                if usize::try_from(written).ok() != Some(self.bind.len()) {
                    return Err(SmbError::Protocol);
                }
                self.step = Step::AwaitBind;
            }
            Step::AwaitBind => {
                let more = partial(status)?;
                if let Some(pdu) = self.gather(ReadResponse::decode(message)?.data, more)? {
                    self.on_bind_answer(&pdu)?;
                }
            }
            Step::Call => {
                let more = partial(status)?;
                let response = IoctlResponse::decode(message)?;
                if response.ctl_code != FSCTL_PIPE_TRANSCEIVE || Some(response.file_id) != self.pipe
                {
                    return Err(SmbError::Protocol);
                }
                self.step = Step::AwaitAnswer;
                if let Some(pdu) = self.gather(&response.output, more)? {
                    self.on_fragment(&pdu)?;
                }
            }
            Step::AwaitAnswer => {
                let more = partial(status)?;
                if let Some(pdu) = self.gather(ReadResponse::decode(message)?.data, more)? {
                    self.on_fragment(&pdu)?;
                }
            }
            Step::Close => {
                succeeded(status)?;
                CloseResponse::decode(message)?;
                self.step = Step::Over;
            }
            Step::Over => return Err(SmbError::Protocol),
        }
        Ok(())
    }

    /// Add `data` to the PDU being read; the PDU once it is whole, which it is
    /// when the pipe has no more of it.
    fn gather(&mut self, data: &[u8], more: bool) -> Result<Option<Vec<u8>>, SmbError> {
        if self.pdu.len() + data.len() > MAX_FRAGMENT {
            return Err(SmbError::TooLarge);
        }
        self.pdu
            .try_reserve(data.len())
            .map_err(|_| SmbError::OutOfMemory)?;
        self.pdu.extend_from_slice(data);
        Ok((!more).then(|| core::mem::take(&mut self.pdu)))
    }

    fn on_bind_answer(&mut self, pdu: &[u8]) -> Result<(), SmbError> {
        match Pdu::decode(pdu)? {
            Pdu::BindAck(ack) if ack.call_id == BIND_CALL => {
                if ack.result != ACCEPTANCE {
                    return Err(SmbError::Rpc(u32::from(ack.reason)));
                }
                if ack.transfer_syntax != NDR || usize::from(ack.max_recv_frag) < self.call.len() {
                    return Err(SmbError::Protocol);
                }
                self.transceive = Some(IoctlRequest {
                    ctl_code: FSCTL_PIPE_TRANSCEIVE,
                    file_id: self.pipe.ok_or(SmbError::Protocol)?,
                    input: core::mem::take(&mut self.call),
                    max_output_response: u32::try_from(MAX_FRAGMENT)
                        .map_err(|_| SmbError::TooLarge)?,
                    flags: IS_FSCTL,
                });
                self.step = Step::Call;
                Ok(())
            }
            Pdu::BindNak { call_id, reason } if call_id == BIND_CALL => {
                Err(SmbError::Rpc(u32::from(reason)))
            }
            _ => Err(SmbError::Protocol),
        }
    }

    fn on_fragment(&mut self, pdu: &[u8]) -> Result<(), SmbError> {
        let fragment = match Pdu::decode(pdu)? {
            Pdu::Response(fragment)
                if fragment.call_id == SHARE_ENUM_CALL && fragment.context == CONTEXT =>
            {
                fragment
            }
            Pdu::Fault { call_id, status } if call_id == SHARE_ENUM_CALL => {
                return Err(SmbError::Rpc(status));
            }
            _ => return Err(SmbError::Protocol),
        };
        // The first fragment opens the answer, and no other does.
        if fragment.first == self.answering {
            return Err(SmbError::Protocol);
        }
        self.answering = true;
        if self.stub.len() + fragment.stub.len() > MAX_STUB {
            return Err(SmbError::TooLarge);
        }
        self.stub
            .try_reserve(fragment.stub.len())
            .map_err(|_| SmbError::OutOfMemory)?;
        self.stub.extend_from_slice(fragment.stub);
        if fragment.last {
            self.list = Some(read_share_enum(&self.stub)?);
            self.stub = Vec::new();
            self.step = Step::Close;
        }
        Ok(())
    }
}

/// Whether `status` is success.
fn succeeded(status: NtStatus) -> Result<(), SmbError> {
    if status == NtStatus::SUCCESS {
        Ok(())
    } else {
        Err(SmbError::Status(status))
    }
}

/// Whether a pipe read answered with `status` left more of its message.
fn partial(status: NtStatus) -> Result<bool, SmbError> {
    match status {
        NtStatus::SUCCESS => Ok(false),
        NtStatus::BUFFER_OVERFLOW => Ok(true),
        status => Err(SmbError::Status(status)),
    }
}

#[cfg(test)]
#[path = "rpc_tests.rs"]
mod tests;
