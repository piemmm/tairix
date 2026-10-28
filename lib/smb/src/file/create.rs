//! `CREATE` (MS-SMB2 §2.2.13, §2.2.14) and the create contexts this client
//! asks with: a lease, a durable handle or its reconnection, the maximal
//! access, and the on-disk id.
//!
//! A context list is read whole and fails closed: each context inside the
//! list's span, eight-byte aligned, named once. A context this client never
//! asks for is passed over unread; whether what came back answers what was
//! asked is the caller's to judge.

use tairix_inline::ArrayVec;
use tairix_util::flags;

use crate::file::lease::Lease;
use crate::file::name::SharePath;
use crate::file::{exactly, FileAttributes, FileId, FileTimes};
use crate::header::{structure, HEADER_LEN};
use crate::status::NtStatus;
use crate::wire::{buffer_at, Encoder, Guid, Reader};
use crate::SmbError;

flags! {
    /// The access an open asks for (MS-SMB2 §2.2.13.1). A directory's names
    /// for the low bits share the file's values.
    pub struct Access(u32) {
        /// Read data, or list a directory.
        const READ_DATA = 0x0000_0001;
        /// Write data, or add a file to a directory.
        const WRITE_DATA = 0x0000_0002;
        /// Append data, or add a subdirectory.
        const APPEND_DATA = 0x0000_0004;
        /// Read extended attributes.
        const READ_EA = 0x0000_0008;
        /// Write extended attributes.
        const WRITE_EA = 0x0000_0010;
        /// Execute, or traverse a directory.
        const EXECUTE = 0x0000_0020;
        /// Delete a directory's entries.
        const DELETE_CHILD = 0x0000_0040;
        /// Read attributes.
        const READ_ATTRIBUTES = 0x0000_0080;
        /// Write attributes.
        const WRITE_ATTRIBUTES = 0x0000_0100;
        /// Delete or rename.
        const DELETE = 0x0001_0000;
        /// Read the security descriptor.
        const READ_CONTROL = 0x0002_0000;
        /// Synchronise, which servers ignore.
        const SYNCHRONIZE = 0x0010_0000;
        /// Whatever the most the server grants is.
        const MAXIMUM_ALLOWED = 0x0200_0000;
    }
}

flags! {
    /// What other opens may do while this one stands.
    pub struct ShareAccess(u32) {
        /// Read.
        const READ = 0x0000_0001;
        /// Write.
        const WRITE = 0x0000_0002;
        /// Delete or rename.
        const DELETE = 0x0000_0004;
    }
}

flags! {
    /// How an open is made (MS-SMB2 §2.2.13).
    pub struct CreateOptions(u32) {
        /// The object must be a directory.
        const DIRECTORY_FILE = 0x0000_0001;
        /// Writes reach storage before they complete.
        const WRITE_THROUGH = 0x0000_0002;
        /// Access will be sequential.
        const SEQUENTIAL_ONLY = 0x0000_0004;
        /// The object must not be a directory.
        const NON_DIRECTORY_FILE = 0x0000_0040;
        /// Access will be random.
        const RANDOM_ACCESS = 0x0000_0800;
        /// Delete the object when its last open closes.
        const DELETE_ON_CLOSE = 0x0000_1000;
        /// Open a reparse point itself, not what it points at.
        const OPEN_REPARSE_POINT = 0x0020_0000;
    }
}

/// What a create does when the object exists, and when it does not.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Disposition {
    /// Replace an existing object; else create.
    Supersede,
    /// Open an existing object; else fail.
    Open,
    /// Fail if it exists; else create.
    Create,
    /// Open an existing object; else create.
    OpenIf,
    /// Truncate an existing object; else fail.
    Overwrite,
    /// Truncate an existing object; else create.
    OverwriteIf,
}

impl Disposition {
    const fn code(self) -> u32 {
        match self {
            Self::Supersede => 0,
            Self::Open => 1,
            Self::Create => 2,
            Self::OpenIf => 3,
            Self::Overwrite => 4,
            Self::OverwriteIf => 5,
        }
    }

    const fn from_code(code: u32) -> Option<Self> {
        match code {
            0 => Some(Self::Supersede),
            1 => Some(Self::Open),
            2 => Some(Self::Create),
            3 => Some(Self::OpenIf),
            4 => Some(Self::Overwrite),
            5 => Some(Self::OverwriteIf),
            _ => None,
        }
    }
}

/// What a create did.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum CreateAction {
    /// Replaced an existing object.
    Superseded,
    /// Opened an existing object.
    Opened,
    /// Created the object.
    Created,
    /// Truncated an existing object.
    Overwritten,
}

impl CreateAction {
    const fn code(self) -> u32 {
        match self {
            Self::Superseded => 0,
            Self::Opened => 1,
            Self::Created => 2,
            Self::Overwritten => 3,
        }
    }

    const fn from_code(code: u32) -> Option<Self> {
        match code {
            0 => Some(Self::Superseded),
            1 => Some(Self::Opened),
            2 => Some(Self::Created),
            3 => Some(Self::Overwritten),
            _ => None,
        }
    }
}

/// An oplock level (MS-SMB2 §2.2.13). This client asks for a lease or none.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum OplockLevel {
    /// None.
    None,
    /// Level II.
    Two,
    /// Exclusive.
    Exclusive,
    /// Batch.
    Batch,
    /// A lease, carried in its create context.
    Lease,
}

impl OplockLevel {
    const fn code(self) -> u8 {
        match self {
            Self::None => 0x00,
            Self::Two => 0x01,
            Self::Exclusive => 0x08,
            Self::Batch => 0x09,
            Self::Lease => 0xff,
        }
    }

    const fn from_code(code: u8) -> Option<Self> {
        match code {
            0x00 => Some(Self::None),
            0x01 => Some(Self::Two),
            0x08 => Some(Self::Exclusive),
            0x09 => Some(Self::Batch),
            0xff => Some(Self::Lease),
            _ => None,
        }
    }
}

/// Durability asked of a create.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Durable {
    /// Make the open survive a dropped connection for `timeout_ms`, zero
    /// asking the server's default.
    Request {
        /// How long the server holds the open for a reconnection.
        timeout_ms: u32,
        /// The create's own id, by which a replay of it is recognised.
        create_guid: Guid,
    },
    /// Reclaim the durable open `file_id` that the create `create_guid`
    /// made before the connection dropped.
    Reconnect {
        /// The open's id.
        file_id: FileId,
        /// The create that made it.
        create_guid: Guid,
    },
}

/// A `CREATE` request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateRequest {
    /// The object, relative to the share's root.
    pub path: SharePath,
    /// The access asked for.
    pub access: Access,
    /// The attributes a new object is made with.
    pub attributes: FileAttributes,
    /// What other opens may do meanwhile.
    pub share: ShareAccess,
    /// What to do whether or not the object exists.
    pub disposition: Disposition,
    /// How to open it.
    pub options: CreateOptions,
    /// The lease asked for, if any.
    pub lease: Option<Lease>,
    /// Durability asked for, if any.
    pub durable: Option<Durable>,
    /// Whether to ask the most access the session may have.
    pub maximal_access: bool,
    /// Whether to ask the object's on-disk id.
    pub disk_id: bool,
}

/// Create context names (MS-SMB2 §2.2.13.2).
mod name {
    pub const LEASE: [u8; 4] = *b"RqLs";
    pub const DURABLE: [u8; 4] = *b"DH2Q";
    pub const RECONNECT: [u8; 4] = *b"DH2C";
    pub const MAXIMAL_ACCESS: [u8; 4] = *b"MxAc";
    pub const DISK_ID: [u8; 4] = *b"QFid";
}

/// The fixed part of a `CREATE` request's body, which its name follows.
const REQUEST_FIXED: usize = 56;

/// The most contexts one list may hold before it is refused.
const MAX_CONTEXTS: usize = 16;

/// The impersonation level every create carries: impersonation.
const IMPERSONATION: u32 = 2;

/// Where a context's four-byte name sits within it, and where its data
/// starts, eight-byte aligned after the name.
const CONTEXT_NAME_AT: usize = 16;
const CONTEXT_DATA_AT: usize = 24;

/// The data lengths of the contexts this client reads.
const DURABLE_REQUEST_LEN: usize = 32;
const RECONNECT_LEN: usize = 36;
const DURABLE_RESPONSE_LEN: usize = 8;
const MAXIMAL_ACCESS_LEN: usize = 8;
const DISK_ID_LEN: usize = 32;

/// Writes one create context's data.
type ContextData<'a> = &'a dyn Fn(&mut Encoder);

/// Append `contexts` as a context list eight-byte aligned from the header's
/// start, and patch the offset and length at `fields` to name it.
fn write_contexts(enc: &mut Encoder, fields: usize, contexts: &[([u8; 4], ContextData<'_>)]) {
    if contexts.is_empty() {
        return;
    }
    enc.align(8);
    let list = enc.position();
    for (index, (name, data)) in contexts.iter().enumerate() {
        let start = enc.position();
        enc.u32(0);
        let name_at = enc.u16_of(CONTEXT_NAME_AT);
        enc.u16(name_at);
        enc.u16(4);
        enc.u16(0);
        enc.u16(0);
        enc.u32(0);
        enc.bytes(name);
        enc.zeros(CONTEXT_DATA_AT - CONTEXT_NAME_AT - name.len());
        data(enc);
        // A latched failure stops the position, so the data can read short.
        let len = enc.position().saturating_sub(start + CONTEXT_DATA_AT);
        if len > 0 {
            let data_at = enc.u16_of(CONTEXT_DATA_AT);
            enc.patch_u16(start + 10, data_at);
            let len = enc.u32_of(len);
            enc.patch_u32(start + 12, len);
        }
        if index + 1 < contexts.len() {
            enc.align(8);
            let next = enc.u32_of(enc.position() - start);
            enc.patch_u32(start, next);
        }
    }
    let offset = enc.u32_of(list);
    enc.patch_u32(fields, offset);
    let len = enc.u32_of(enc.position() - list);
    enc.patch_u32(fields + 4, len);
}

/// One context of a list, as read.
struct Context<'a> {
    name: &'a [u8],
    data: &'a [u8],
}

/// The contexts of the list `len` bytes long at `offset` into `message`.
fn read_contexts(
    message: &[u8],
    offset: usize,
    len: usize,
) -> Result<ArrayVec<Context<'_>, MAX_CONTEXTS>, SmbError> {
    let list = buffer_at(message, offset, len)?;
    let mut contexts = ArrayVec::new();
    let mut at = 0;
    while at < list.len() {
        let mut r = Reader::at(list, at)?;
        let next = usize::try_from(r.u32()?).map_err(|_| SmbError::Malformed)?;
        let name_offset = usize::from(r.u16()?);
        let name_len = usize::from(r.u16()?);
        r.skip(2)?;
        let data_offset = usize::from(r.u16()?);
        let data_len = usize::try_from(r.u32()?).map_err(|_| SmbError::Malformed)?;
        let end = match next {
            0 => list.len(),
            _ if !next.is_multiple_of(8) => return Err(SmbError::Malformed),
            _ => at
                .checked_add(next)
                .filter(|&end| end < list.len())
                .ok_or(SmbError::Malformed)?,
        };
        let this = &list[at..end];
        let name = buffer_at(this, name_offset, name_len)?;
        let data = buffer_at(this, data_offset, data_len)?;
        if name.is_empty() || contexts.iter().any(|seen: &Context<'_>| seen.name == name) {
            return Err(SmbError::Malformed);
        }
        contexts
            .try_push(Context { name, data })
            .map_err(|_| SmbError::TooLarge)?;
        if next == 0 {
            break;
        }
        at = end;
    }
    Ok(contexts)
}

impl CreateRequest {
    /// Append the request's body after its header.
    pub fn encode(&self, enc: &mut Encoder) {
        enc.u16(57);
        enc.u8(0);
        let oplock = if self.lease.is_some() {
            OplockLevel::Lease
        } else {
            OplockLevel::None
        };
        enc.u8(oplock.code());
        enc.u32(IMPERSONATION);
        enc.zeros(16);
        enc.u32(self.access.bits());
        enc.u32(self.attributes.bits());
        enc.u32(self.share.bits());
        enc.u32(self.disposition.code());
        enc.u32(self.options.bits());
        let name_offset = enc.u16_of(HEADER_LEN + REQUEST_FIXED);
        enc.u16(name_offset);
        let name_len = enc.u16_of(self.path.utf16_len());
        enc.u16(name_len);
        let fields = enc.position();
        enc.u32(0);
        enc.u32(0);
        self.path.encode(enc);

        let lease = self
            .lease
            .map(|lease| move |enc: &mut Encoder| lease.encode(enc));
        let durable = self.durable.map(|durable| {
            let name = match durable {
                Durable::Request { .. } => name::DURABLE,
                Durable::Reconnect { .. } => name::RECONNECT,
            };
            (name, move |enc: &mut Encoder| match durable {
                Durable::Request {
                    timeout_ms,
                    create_guid,
                } => {
                    enc.u32(timeout_ms);
                    enc.u32(0);
                    enc.u64(0);
                    enc.bytes(&create_guid);
                }
                Durable::Reconnect {
                    file_id,
                    create_guid,
                } => {
                    enc.bytes(&file_id);
                    enc.bytes(&create_guid);
                    enc.u32(0);
                }
            })
        });
        let empty = |_: &mut Encoder| {};
        let mut contexts: [([u8; 4], ContextData<'_>); 4] = [([0; 4], &empty); 4];
        let mut count = 0;
        if let Some(lease) = &lease {
            contexts[count] = (name::LEASE, lease);
            count += 1;
        }
        if let Some((name, data)) = &durable {
            contexts[count] = (*name, data);
            count += 1;
        }
        if self.maximal_access {
            contexts[count] = (name::MAXIMAL_ACCESS, &empty);
            count += 1;
        }
        if self.disk_id {
            contexts[count] = (name::DISK_ID, &empty);
            count += 1;
        }
        if count == 0 && self.path.is_root() {
            // The buffer is never empty, whatever it holds.
            enc.u8(0);
        }
        write_contexts(enc, fields, &contexts[..count]);
    }

    /// The request in `message`, header included.
    ///
    /// # Errors
    ///
    /// [`SmbError::Malformed`] when it is not a well-formed request,
    /// [`SmbError::Name`] for a path either side refuses.
    pub fn decode(message: &[u8]) -> Result<Self, SmbError> {
        let mut r = structure(message, 57)?;
        r.skip(1)?;
        let oplock = OplockLevel::from_code(r.u8()?).ok_or(SmbError::Malformed)?;
        r.skip(20)?;
        let access = Access::from_bits(r.u32()?);
        let attributes = FileAttributes::from_bits(r.u32()?);
        let share = ShareAccess::from_bits(r.u32()?);
        let disposition = Disposition::from_code(r.u32()?).ok_or(SmbError::Malformed)?;
        let options = CreateOptions::from_bits(r.u32()?);
        let name_offset = usize::from(r.u16()?);
        let name_len = usize::from(r.u16()?);
        let contexts_offset = usize::try_from(r.u32()?).map_err(|_| SmbError::Malformed)?;
        let contexts_len = usize::try_from(r.u32()?).map_err(|_| SmbError::Malformed)?;
        let mut request = Self {
            path: SharePath::decode(buffer_at(message, name_offset, name_len)?)?,
            access,
            attributes,
            share,
            disposition,
            options,
            lease: None,
            durable: None,
            maximal_access: false,
            disk_id: false,
        };
        for context in read_contexts(message, contexts_offset, contexts_len)? {
            let durable = match <[u8; 4]>::try_from(context.name) {
                Ok(name::LEASE) => {
                    request.lease = Some(Lease::decode(context.data)?);
                    None
                }
                Ok(name::DURABLE) => {
                    let mut r = exactly(context.data, DURABLE_REQUEST_LEN)?;
                    let timeout_ms = r.u32()?;
                    r.skip(12)?;
                    Some(Durable::Request {
                        timeout_ms,
                        create_guid: r.guid()?,
                    })
                }
                Ok(name::RECONNECT) => {
                    let mut r = exactly(context.data, RECONNECT_LEN)?;
                    Some(Durable::Reconnect {
                        file_id: r.array()?,
                        create_guid: r.guid()?,
                    })
                }
                Ok(name::MAXIMAL_ACCESS) => {
                    request.maximal_access = true;
                    None
                }
                Ok(name::DISK_ID) => {
                    request.disk_id = true;
                    None
                }
                _ => None,
            };
            if let Some(durable) = durable {
                if request.durable.replace(durable).is_some() {
                    return Err(SmbError::Malformed);
                }
            }
        }
        if (oplock == OplockLevel::Lease) != request.lease.is_some() {
            return Err(SmbError::Malformed);
        }
        Ok(request)
    }
}

/// The maximal access a create was answered with.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct MaximalAccess {
    /// Whether the query succeeded: `access` means nothing otherwise.
    pub status: NtStatus,
    /// The most access the session may have on the object.
    pub access: Access,
}

/// An object's on-disk identity.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct DiskId {
    /// The object's file id within its volume.
    pub file: u64,
    /// The volume's id.
    pub volume: u64,
}

/// A `CREATE` response.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateResponse {
    /// The oplock granted.
    pub oplock: OplockLevel,
    /// Whether the object reached is a reparse point the create did not ask
    /// to open as one.
    pub reparse_point: bool,
    /// What the create did.
    pub action: CreateAction,
    /// The object's times.
    pub times: FileTimes,
    /// The bytes allocated to it.
    pub allocation_size: u64,
    /// Its size.
    pub end_of_file: u64,
    /// Its attributes.
    pub attributes: FileAttributes,
    /// The open's id.
    pub file_id: FileId,
    /// The lease granted, if any.
    pub lease: Option<Lease>,
    /// How long the server holds the open after a disconnect, when it made
    /// the open durable.
    pub durable_timeout_ms: Option<u32>,
    /// The maximal access, if asked.
    pub maximal_access: Option<MaximalAccess>,
    /// The on-disk id, if asked.
    pub disk_id: Option<DiskId>,
}

impl CreateResponse {
    /// Append the response's body after its header.
    pub fn encode(&self, enc: &mut Encoder) {
        enc.u16(89);
        enc.u8(self.oplock.code());
        enc.u8(u8::from(self.reparse_point));
        enc.u32(self.action.code());
        self.times.write(enc);
        enc.u64(self.allocation_size);
        enc.u64(self.end_of_file);
        enc.u32(self.attributes.bits());
        enc.u32(0);
        enc.bytes(&self.file_id);
        let fields = enc.position();
        enc.u32(0);
        enc.u32(0);

        let lease = self
            .lease
            .map(|lease| move |enc: &mut Encoder| lease.encode(enc));
        let durable = self.durable_timeout_ms.map(|timeout| {
            move |enc: &mut Encoder| {
                enc.u32(timeout);
                enc.u32(0);
            }
        });
        let maximal = self.maximal_access.map(|maximal| {
            move |enc: &mut Encoder| {
                enc.u32(maximal.status.code());
                enc.u32(maximal.access.bits());
            }
        });
        let disk = self.disk_id.map(|disk| {
            move |enc: &mut Encoder| {
                enc.u64(disk.file);
                enc.u64(disk.volume);
                enc.zeros(16);
            }
        });
        let empty = |_: &mut Encoder| {};
        let mut contexts: [([u8; 4], ContextData<'_>); 4] = [([0; 4], &empty); 4];
        let mut count = 0;
        if let Some(data) = &lease {
            contexts[count] = (name::LEASE, data);
            count += 1;
        }
        if let Some(data) = &durable {
            contexts[count] = (name::DURABLE, data);
            count += 1;
        }
        if let Some(data) = &maximal {
            contexts[count] = (name::MAXIMAL_ACCESS, data);
            count += 1;
        }
        if let Some(data) = &disk {
            contexts[count] = (name::DISK_ID, data);
            count += 1;
        }
        if count == 0 {
            // The buffer is never empty, whatever it holds.
            enc.u8(0);
        }
        write_contexts(enc, fields, &contexts[..count]);
    }

    /// The response in `message`, header included.
    ///
    /// # Errors
    ///
    /// [`SmbError::Malformed`] when it is not a well-formed response,
    /// [`SmbError::TooLarge`] for a context list past the bound.
    pub fn decode(message: &[u8]) -> Result<Self, SmbError> {
        let mut r = structure(message, 89)?;
        let oplock = OplockLevel::from_code(r.u8()?).ok_or(SmbError::Malformed)?;
        let reparse_point = r.u8()? & 0x01 != 0;
        let action = CreateAction::from_code(r.u32()?).ok_or(SmbError::Malformed)?;
        let times = FileTimes::read(&mut r)?;
        let allocation_size = r.u64()?;
        let end_of_file = r.u64()?;
        let attributes = FileAttributes::from_bits(r.u32()?);
        r.skip(4)?;
        let file_id = r.array()?;
        let contexts_offset = usize::try_from(r.u32()?).map_err(|_| SmbError::Malformed)?;
        let contexts_len = usize::try_from(r.u32()?).map_err(|_| SmbError::Malformed)?;
        let mut response = Self {
            oplock,
            reparse_point,
            action,
            times,
            allocation_size,
            end_of_file,
            attributes,
            file_id,
            lease: None,
            durable_timeout_ms: None,
            maximal_access: None,
            disk_id: None,
        };
        for context in read_contexts(message, contexts_offset, contexts_len)? {
            match <[u8; 4]>::try_from(context.name) {
                Ok(name::LEASE) => response.lease = Some(Lease::decode(context.data)?),
                Ok(name::DURABLE) => {
                    let mut r = exactly(context.data, DURABLE_RESPONSE_LEN)?;
                    response.durable_timeout_ms = Some(r.u32()?);
                }
                Ok(name::MAXIMAL_ACCESS) => {
                    let mut r = exactly(context.data, MAXIMAL_ACCESS_LEN)?;
                    response.maximal_access = Some(MaximalAccess {
                        status: NtStatus::from_code(r.u32()?),
                        access: Access::from_bits(r.u32()?),
                    });
                }
                Ok(name::DISK_ID) => {
                    let mut r = exactly(context.data, DISK_ID_LEN)?;
                    response.disk_id = Some(DiskId {
                        file: r.u64()?,
                        volume: r.u64()?,
                    });
                }
                _ => {}
            }
        }
        if (oplock == OplockLevel::Lease) != response.lease.is_some() {
            return Err(SmbError::Malformed);
        }
        Ok(response)
    }
}

#[cfg(test)]
#[path = "create_tests.rs"]
mod tests;
