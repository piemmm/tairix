//! `NEGOTIATE` (MS-SMB2 §2.2.3, §2.2.4), its 3.1.1 contexts, validate-negotiate
//! (MS-SMB2 §2.2.31.4), and the client's choice of what to accept.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_util::flags;

use crate::file::NO_FILE;
use crate::header::{structure, HEADER_LEN};
use crate::ioctl::{IoctlRequest, IS_FSCTL};
use crate::wire::{buffer_at, from_utf16le, owned, utf16le, utf16le_len, Encoder, Guid, Reader};
use crate::{Refusal, SmbError};

/// A dialect this client speaks; nothing older is offered or accepted.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum Dialect {
    /// SMB 3.0.
    Smb300,
    /// SMB 3.0.2.
    Smb302,
    /// SMB 3.1.1.
    Smb311,
}

impl Dialect {
    /// Every dialect, oldest first: the order a request offers them in.
    pub const ALL: [Self; 3] = [Self::Smb300, Self::Smb302, Self::Smb311];

    /// The dialect's revision code.
    #[must_use]
    pub const fn code(self) -> u16 {
        match self {
            Self::Smb300 => 0x0300,
            Self::Smb302 => 0x0302,
            Self::Smb311 => 0x0311,
        }
    }

    /// The dialect with revision code `code`, if this client speaks it.
    #[must_use]
    pub const fn from_code(code: u16) -> Option<Self> {
        match code {
            0x0300 => Some(Self::Smb300),
            0x0302 => Some(Self::Smb302),
            0x0311 => Some(Self::Smb311),
            _ => None,
        }
    }
}

flags! {
    /// A negotiate `SecurityMode` (MS-SMB2 §2.2.3).
    pub struct SecurityMode(u16) {
        /// Signing is enabled.
        const SIGNING_ENABLED = 0x0001;
        /// Signing is required.
        const SIGNING_REQUIRED = 0x0002;
    }
}

flags! {
    /// Global capabilities (MS-SMB2 §2.2.3).
    pub struct Capabilities(u32) {
        /// Distributed File System.
        const DFS = 0x0000_0001;
        /// Leasing.
        const LEASING = 0x0000_0002;
        /// Multi-credit operations.
        const LARGE_MTU = 0x0000_0004;
        /// Multiple channels per session.
        const MULTI_CHANNEL = 0x0000_0008;
        /// Persistent handles.
        const PERSISTENT_HANDLES = 0x0000_0010;
        /// Directory leasing.
        const DIRECTORY_LEASING = 0x0000_0020;
        /// Encryption, as 3.0 and 3.0.2 announce it.
        const ENCRYPTION = 0x0000_0040;
        /// One-way notifications.
        const NOTIFICATIONS = 0x0000_0080;
    }
}

/// An encryption cipher (MS-SMB2 §2.2.3.1.2).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Cipher {
    /// AES-128-CCM.
    Aes128Ccm,
    /// AES-128-GCM.
    Aes128Gcm,
    /// AES-256-CCM.
    Aes256Ccm,
    /// AES-256-GCM.
    Aes256Gcm,
}

impl Cipher {
    /// What a 3.1.1 request offers, in preference order.
    pub const PREFERENCE: [Self; 4] = [
        Self::Aes256Gcm,
        Self::Aes128Gcm,
        Self::Aes256Ccm,
        Self::Aes128Ccm,
    ];

    /// The cipher's id.
    #[must_use]
    pub const fn code(self) -> u16 {
        match self {
            Self::Aes128Ccm => 1,
            Self::Aes128Gcm => 2,
            Self::Aes256Ccm => 3,
            Self::Aes256Gcm => 4,
        }
    }

    /// The cipher with id `code`.
    #[must_use]
    pub const fn from_code(code: u16) -> Option<Self> {
        match code {
            1 => Some(Self::Aes128Ccm),
            2 => Some(Self::Aes128Gcm),
            3 => Some(Self::Aes256Ccm),
            4 => Some(Self::Aes256Gcm),
            _ => None,
        }
    }

    /// Whether the cipher's keys are 256 bits.
    #[must_use]
    pub const fn is_256(self) -> bool {
        matches!(self, Self::Aes256Ccm | Self::Aes256Gcm)
    }
}

/// A signing algorithm (MS-SMB2 §2.2.3.1.7). HMAC-SHA256 belongs to the 2.x dialects
/// this client refuses, so it is never offered.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum SigningAlgorithm {
    /// AES-128-CMAC.
    AesCmac,
    /// AES-128-GMAC.
    AesGmac,
}

impl SigningAlgorithm {
    /// What a 3.1.1 request offers, in preference order.
    pub const PREFERENCE: [Self; 2] = [Self::AesGmac, Self::AesCmac];

    /// The algorithm's id.
    #[must_use]
    pub const fn code(self) -> u16 {
        match self {
            Self::AesCmac => 1,
            Self::AesGmac => 2,
        }
    }

    /// The algorithm with id `code`, if this client accepts it.
    #[must_use]
    pub const fn from_code(code: u16) -> Option<Self> {
        match code {
            1 => Some(Self::AesCmac),
            2 => Some(Self::AesGmac),
            _ => None,
        }
    }
}

/// The pre-auth integrity hash algorithm id for SHA-512, the only one.
pub const SHA512: u16 = 0x0001;

/// The salt length a pre-auth integrity context carries from this client.
pub const SALT_LEN: usize = 32;

/// A negotiate context type (MS-SMB2 §2.2.3.1).
mod context_type {
    pub const PREAUTH_INTEGRITY: u16 = 0x0001;
    pub const ENCRYPTION: u16 = 0x0002;
    pub const NETNAME: u16 = 0x0005;
    pub const SIGNING: u16 = 0x0008;
}

/// The most contexts a message may carry before it is refused.
const MAX_CONTEXTS: usize = 16;

/// The most ids one context's list may carry before it is refused.
const MAX_IDS: usize = 32;

/// A negotiate context, as either direction carries it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NegotiateContext {
    /// `PREAUTH_INTEGRITY_CAPABILITIES`: hash algorithm ids and a salt.
    PreauthIntegrity {
        /// The hash algorithm ids.
        hash_algorithms: Vec<u16>,
        /// The salt.
        salt: Vec<u8>,
    },
    /// `ENCRYPTION_CAPABILITIES`: cipher ids, offered or chosen.
    Encryption {
        /// The cipher ids, unknown ones kept as they came.
        ciphers: Vec<u16>,
    },
    /// `SIGNING_CAPABILITIES`: signing algorithm ids, offered or chosen.
    Signing {
        /// The algorithm ids, unknown ones kept as they came.
        algorithms: Vec<u16>,
    },
    /// `NETNAME_NEGOTIATE_CONTEXT_ID`: the server name the client dialled.
    NetName(String),
    /// A context this engine does not use, which a receiver ignores.
    Other {
        /// The context type.
        context_type: u16,
    },
}

impl NegotiateContext {
    fn encode(&self, enc: &mut Encoder) {
        let (kind, data_len) = match self {
            Self::PreauthIntegrity {
                hash_algorithms,
                salt,
            } => (
                context_type::PREAUTH_INTEGRITY,
                4 + 2 * hash_algorithms.len() + salt.len(),
            ),
            Self::Encryption { ciphers } => (context_type::ENCRYPTION, 2 + 2 * ciphers.len()),
            Self::Signing { algorithms } => (context_type::SIGNING, 2 + 2 * algorithms.len()),
            Self::NetName(name) => (context_type::NETNAME, utf16le_len(name)),
            Self::Other { .. } => {
                enc.fail(SmbError::Protocol);
                return;
            }
        };
        enc.u16(kind);
        let data_len = enc.u16_of(data_len);
        enc.u16(data_len);
        enc.u32(0);
        match self {
            Self::PreauthIntegrity {
                hash_algorithms,
                salt,
            } => {
                let count = enc.u16_of(hash_algorithms.len());
                enc.u16(count);
                let salt_len = enc.u16_of(salt.len());
                enc.u16(salt_len);
                for id in hash_algorithms {
                    enc.u16(*id);
                }
                enc.bytes(salt);
            }
            Self::Encryption { ciphers: ids } | Self::Signing { algorithms: ids } => {
                let count = enc.u16_of(ids.len());
                enc.u16(count);
                for id in ids {
                    enc.u16(*id);
                }
            }
            Self::NetName(name) => utf16le(name, enc),
            Self::Other { .. } => {}
        }
    }

    fn decode(kind: u16, data: &[u8]) -> Result<Self, SmbError> {
        let mut r = Reader::new(data);
        let ids = |r: &mut Reader<'_>, count: usize| -> Result<Vec<u16>, SmbError> {
            if count == 0 || count > MAX_IDS {
                return Err(SmbError::Malformed);
            }
            let mut ids = Vec::new();
            ids.try_reserve(count).map_err(|_| SmbError::OutOfMemory)?;
            for _ in 0..count {
                ids.push(r.u16()?);
            }
            Ok(ids)
        };
        Ok(match kind {
            context_type::PREAUTH_INTEGRITY => {
                let count = usize::from(r.u16()?);
                let salt_len = usize::from(r.u16()?);
                let hash_algorithms = ids(&mut r, count)?;
                let salt = owned(r.bytes(salt_len)?)?;
                Self::PreauthIntegrity {
                    hash_algorithms,
                    salt,
                }
            }
            context_type::ENCRYPTION => {
                let count = usize::from(r.u16()?);
                Self::Encryption {
                    ciphers: ids(&mut r, count)?,
                }
            }
            context_type::SIGNING => {
                let count = usize::from(r.u16()?);
                Self::Signing {
                    algorithms: ids(&mut r, count)?,
                }
            }
            context_type::NETNAME => return Ok(Self::NetName(from_utf16le(data)?)),
            context_type => return Ok(Self::Other { context_type }),
        })
    }
}

/// Append `contexts`, each eight-byte aligned from the header's start.
fn encode_contexts(contexts: &[NegotiateContext], enc: &mut Encoder) {
    for (index, context) in contexts.iter().enumerate() {
        if index > 0 {
            enc.align(8);
        }
        context.encode(enc);
    }
}

/// The `count` contexts at `offset` into `message`.
fn decode_contexts(
    message: &[u8],
    offset: usize,
    count: usize,
) -> Result<Vec<NegotiateContext>, SmbError> {
    if count > MAX_CONTEXTS {
        return Err(SmbError::TooLarge);
    }
    let mut contexts = Vec::new();
    contexts
        .try_reserve(count)
        .map_err(|_| SmbError::OutOfMemory)?;
    let mut at = offset;
    for index in 0..count {
        if index > 0 {
            at = at.next_multiple_of(8);
        }
        let mut r = Reader::at(message, at)?;
        let kind = r.u16()?;
        let data_len = usize::from(r.u16()?);
        r.skip(4)?;
        let data = r.bytes(data_len)?;
        contexts.push(NegotiateContext::decode(kind, data)?);
        at = r.position();
    }
    Ok(contexts)
}

/// A `NEGOTIATE` request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NegotiateRequest {
    /// The dialect revision codes offered, unknown ones kept as they came.
    pub dialects: Vec<u16>,
    /// The client's security mode.
    pub security_mode: SecurityMode,
    /// The client's capabilities.
    pub capabilities: Capabilities,
    /// The client's GUID.
    pub client_guid: Guid,
    /// The negotiate contexts, present when 3.1.1 is offered.
    pub contexts: Vec<NegotiateContext>,
}

impl NegotiateRequest {
    /// Append the request's body after its header.
    pub fn encode(&self, enc: &mut Encoder) {
        enc.u16(36);
        let count = enc.u16_of(self.dialects.len());
        enc.u16(count);
        enc.u16(self.security_mode.bits());
        enc.u16(0);
        enc.u32(self.capabilities.bits());
        enc.bytes(&self.client_guid);
        let context_offset_at = enc.position();
        enc.u32(0);
        let context_count = enc.u16_of(self.contexts.len());
        enc.u16(context_count);
        enc.u16(0);
        for dialect in &self.dialects {
            enc.u16(*dialect);
        }
        if !self.contexts.is_empty() {
            enc.align(8);
            let offset = enc.position();
            let offset = enc.u32_of(offset);
            enc.patch_u32(context_offset_at, offset);
            encode_contexts(&self.contexts, enc);
        }
    }

    /// The request in `message`, header included.
    ///
    /// # Errors
    ///
    /// [`SmbError::Malformed`] when it is not a well-formed request.
    pub fn decode(message: &[u8]) -> Result<Self, SmbError> {
        let mut r = structure(message, 36)?;
        let dialect_count = usize::from(r.u16()?);
        let security_mode = SecurityMode::from_bits(r.u16()?);
        r.skip(2)?;
        let capabilities = Capabilities::from_bits(r.u32()?);
        let client_guid = r.guid()?;
        let context_offset = usize::try_from(r.u32()?).map_err(|_| SmbError::Malformed)?;
        let context_count = usize::from(r.u16()?);
        r.skip(2)?;
        if dialect_count == 0 || dialect_count > MAX_IDS {
            return Err(SmbError::Malformed);
        }
        let mut dialects = Vec::new();
        dialects
            .try_reserve(dialect_count)
            .map_err(|_| SmbError::OutOfMemory)?;
        for _ in 0..dialect_count {
            dialects.push(r.u16()?);
        }
        let contexts = if dialects.contains(&Dialect::Smb311.code()) && context_count > 0 {
            decode_contexts(message, context_offset, context_count)?
        } else {
            Vec::new()
        };
        Ok(Self {
            dialects,
            security_mode,
            capabilities,
            client_guid,
            contexts,
        })
    }
}

/// A `NEGOTIATE` response.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NegotiateResponse {
    /// The server's security mode.
    pub security_mode: SecurityMode,
    /// The chosen dialect's revision code.
    pub dialect: u16,
    /// The server's GUID.
    pub server_guid: Guid,
    /// The server's capabilities.
    pub capabilities: Capabilities,
    /// The largest transaction buffer the server accepts.
    pub max_transact_size: u32,
    /// The largest read the server serves.
    pub max_read_size: u32,
    /// The largest write the server accepts.
    pub max_write_size: u32,
    /// The server's clock, as a FILETIME.
    pub system_time: u64,
    /// The server's start time, as a FILETIME, or zero.
    pub server_start_time: u64,
    /// The security buffer: the server's opening SPNEGO token.
    pub security_buffer: Vec<u8>,
    /// The negotiate contexts, present for 3.1.1.
    pub contexts: Vec<NegotiateContext>,
}

impl NegotiateResponse {
    /// Append the response's body after its header.
    pub fn encode(&self, enc: &mut Encoder) {
        enc.u16(65);
        enc.u16(self.security_mode.bits());
        enc.u16(self.dialect);
        let context_count = enc.u16_of(self.contexts.len());
        enc.u16(context_count);
        enc.bytes(&self.server_guid);
        enc.u32(self.capabilities.bits());
        enc.u32(self.max_transact_size);
        enc.u32(self.max_read_size);
        enc.u32(self.max_write_size);
        enc.u64(self.system_time);
        enc.u64(self.server_start_time);
        let security_offset = HEADER_LEN + 64;
        let security_offset = enc.u16_of(security_offset);
        enc.u16(security_offset);
        let security_len = enc.u16_of(self.security_buffer.len());
        enc.u16(security_len);
        let context_offset_at = enc.position();
        enc.u32(0);
        enc.bytes(&self.security_buffer);
        if !self.contexts.is_empty() {
            enc.align(8);
            let offset = enc.position();
            let offset = enc.u32_of(offset);
            enc.patch_u32(context_offset_at, offset);
            encode_contexts(&self.contexts, enc);
        }
    }

    /// The response in `message`, header included.
    ///
    /// # Errors
    ///
    /// [`SmbError::Malformed`] when it is not a well-formed response.
    pub fn decode(message: &[u8]) -> Result<Self, SmbError> {
        let mut r = structure(message, 65)?;
        let security_mode = SecurityMode::from_bits(r.u16()?);
        let dialect = r.u16()?;
        let context_count = usize::from(r.u16()?);
        let server_guid = r.guid()?;
        let capabilities = Capabilities::from_bits(r.u32()?);
        let max_transact_size = r.u32()?;
        let max_read_size = r.u32()?;
        let max_write_size = r.u32()?;
        let system_time = r.u64()?;
        let server_start_time = r.u64()?;
        let security_offset = usize::from(r.u16()?);
        let security_len = usize::from(r.u16()?);
        let context_offset = usize::try_from(r.u32()?).map_err(|_| SmbError::Malformed)?;
        let security_buffer = owned(buffer_at(message, security_offset, security_len)?)?;
        let contexts = if dialect == Dialect::Smb311.code() {
            decode_contexts(message, context_offset, context_count)?
        } else {
            Vec::new()
        };
        Ok(Self {
            security_mode,
            dialect,
            server_guid,
            capabilities,
            max_transact_size,
            max_read_size,
            max_write_size,
            system_time,
            server_start_time,
            security_buffer,
            contexts,
        })
    }
}

/// What this client offers in a `NEGOTIATE`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Offer {
    /// The oldest dialect accepted, `3.0` unless a caller raised it.
    pub floor: Dialect,
    /// The client's GUID.
    pub client_guid: Guid,
    /// The pre-auth integrity salt (random per connection).
    pub salt: [u8; SALT_LEN],
    /// The server name dialled, sent in the 3.1.1 net-name context.
    pub server_name: String,
}

/// What a negotiation settled on.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Negotiated {
    /// The dialect.
    pub dialect: Dialect,
    /// The cipher every message after session setup is encrypted with.
    pub cipher: Cipher,
    /// The algorithm a signed message is signed with.
    pub signing: SigningAlgorithm,
    /// The server's GUID: part of the endpoint pin.
    pub server_guid: Guid,
    /// The server's security mode.
    pub security_mode: SecurityMode,
    /// The server's capabilities.
    pub capabilities: Capabilities,
    /// The largest transaction buffer the server accepts.
    pub max_transact_size: u32,
    /// The largest read the server serves.
    pub max_read_size: u32,
    /// The largest write the server accepts.
    pub max_write_size: u32,
}

/// The capabilities this client announces.
pub const CLIENT_CAPABILITIES: Capabilities = Capabilities::DFS
    .with(Capabilities::LEASING)
    .with(Capabilities::LARGE_MTU)
    .with(Capabilities::DIRECTORY_LEASING)
    .with(Capabilities::ENCRYPTION);

impl Offer {
    /// The request this offer makes.
    #[must_use]
    pub fn request(&self) -> NegotiateRequest {
        let dialects: Vec<u16> = Dialect::ALL
            .into_iter()
            .filter(|&dialect| dialect >= self.floor)
            .map(Dialect::code)
            .collect();
        let contexts = if dialects.contains(&Dialect::Smb311.code()) {
            alloc::vec![
                NegotiateContext::PreauthIntegrity {
                    hash_algorithms: alloc::vec![SHA512],
                    salt: self.salt.to_vec(),
                },
                NegotiateContext::Encryption {
                    ciphers: Cipher::PREFERENCE.iter().map(|c| c.code()).collect(),
                },
                NegotiateContext::Signing {
                    algorithms: SigningAlgorithm::PREFERENCE
                        .iter()
                        .map(|a| a.code())
                        .collect(),
                },
                NegotiateContext::NetName(self.server_name.clone()),
            ]
        } else {
            Vec::new()
        };
        NegotiateRequest {
            dialects,
            security_mode: SecurityMode::SIGNING_ENABLED.with(SecurityMode::SIGNING_REQUIRED),
            capabilities: CLIENT_CAPABILITIES,
            client_guid: self.client_guid,
            contexts,
        }
    }

    /// Accept `response` or refuse it: a dialect within the floor, and for
    /// 3.1.1 exactly one SHA-512 pre-auth context and one offered cipher;
    /// for 3.0 and 3.0.2, the encryption capability.
    ///
    /// # Errors
    ///
    /// [`SmbError::Refused`] naming what fell short, or
    /// [`SmbError::Protocol`] for a choice this client never offered.
    pub fn accept(&self, response: &NegotiateResponse) -> Result<Negotiated, SmbError> {
        let dialect = Dialect::from_code(response.dialect)
            .filter(|&dialect| dialect >= self.floor)
            .ok_or(SmbError::Refused(Refusal::Dialect))?;
        let (cipher, signing) = match dialect {
            Dialect::Smb300 | Dialect::Smb302 => {
                if !response.capabilities.contains(Capabilities::ENCRYPTION) {
                    return Err(SmbError::Refused(Refusal::Encryption));
                }
                (Cipher::Aes128Ccm, SigningAlgorithm::AesCmac)
            }
            Dialect::Smb311 => choose_311(&response.contexts)?,
        };
        Ok(Negotiated {
            dialect,
            cipher,
            signing,
            server_guid: response.server_guid,
            security_mode: response.security_mode,
            capabilities: response.capabilities,
            max_transact_size: response.max_transact_size,
            max_read_size: response.max_read_size,
            max_write_size: response.max_write_size,
        })
    }
}

/// The cipher and signing algorithm a 3.1.1 response chose.
fn choose_311(contexts: &[NegotiateContext]) -> Result<(Cipher, SigningAlgorithm), SmbError> {
    let mut preauth = None;
    let mut cipher = None;
    let mut signing = None;
    for context in contexts {
        match context {
            NegotiateContext::PreauthIntegrity {
                hash_algorithms, ..
            } => {
                if preauth.replace(hash_algorithms.as_slice()).is_some() {
                    return Err(SmbError::Protocol);
                }
            }
            NegotiateContext::Encryption { ciphers } => {
                if cipher.replace(ciphers.as_slice()).is_some() {
                    return Err(SmbError::Protocol);
                }
            }
            NegotiateContext::Signing { algorithms } => {
                if signing.replace(algorithms.as_slice()).is_some() {
                    return Err(SmbError::Protocol);
                }
            }
            NegotiateContext::NetName(_) | NegotiateContext::Other { .. } => {}
        }
    }
    if preauth != Some(&[SHA512][..]) {
        return Err(SmbError::Refused(Refusal::Integrity));
    }
    // A cipher id of zero is the server declining to encrypt.
    let cipher = match cipher {
        Some([0]) | None => return Err(SmbError::Refused(Refusal::Encryption)),
        Some(&[id]) => Cipher::from_code(id).ok_or(SmbError::Protocol)?,
        Some(_) => return Err(SmbError::Protocol),
    };
    // A server that answers no signing context signs with AES-CMAC.
    let signing = match signing {
        None => SigningAlgorithm::AesCmac,
        Some(&[id]) => {
            SigningAlgorithm::from_code(id).ok_or(SmbError::Refused(Refusal::Signing))?
        }
        Some(_) => return Err(SmbError::Protocol),
    };
    Ok((cipher, signing))
}

/// The `FSCTL_VALIDATE_NEGOTIATE_INFO` control code.
pub const FSCTL_VALIDATE_NEGOTIATE_INFO: u32 = 0x0014_0204;

/// The `FSCTL_VALIDATE_NEGOTIATE_INFO` request restating `request`, taking
/// one answer back.
///
/// # Errors
///
/// As [`validate_negotiate_input`].
pub fn validate_negotiate_request(request: &NegotiateRequest) -> Result<IoctlRequest, SmbError> {
    Ok(IoctlRequest {
        ctl_code: FSCTL_VALIDATE_NEGOTIATE_INFO,
        file_id: NO_FILE,
        input: validate_negotiate_input(request)?,
        max_output_response: u32::try_from(VALIDATE_NEGOTIATE_OUTPUT_LEN)
            .map_err(|_| SmbError::TooLarge)?,
        flags: IS_FSCTL,
    })
}

/// A validate-negotiate request's input (MS-SMB2 §2.2.31.4): the negotiation as the
/// client sent it.
///
/// # Errors
///
/// [`SmbError::OutOfMemory`] or [`SmbError::TooLarge`] from encoding.
pub fn validate_negotiate_input(request: &NegotiateRequest) -> Result<Vec<u8>, SmbError> {
    let mut enc = Encoder::new();
    enc.u32(request.capabilities.bits());
    enc.bytes(&request.client_guid);
    enc.u16(request.security_mode.bits());
    let count = enc.u16_of(request.dialects.len());
    enc.u16(count);
    for dialect in &request.dialects {
        enc.u16(*dialect);
    }
    enc.finish()
}

/// The length of a validate-negotiate response's output.
pub const VALIDATE_NEGOTIATE_OUTPUT_LEN: usize = 24;

/// Check a validate-negotiate response's output against what was
/// negotiated, field by field.
///
/// # Errors
///
/// [`SmbError::Refused`] with [`Refusal::Downgrade`] on any disagreement,
/// [`SmbError::Malformed`] for an output that is not exactly one answer.
pub fn check_validate_negotiate(output: &[u8], negotiated: &Negotiated) -> Result<(), SmbError> {
    if output.len() != VALIDATE_NEGOTIATE_OUTPUT_LEN {
        return Err(SmbError::Malformed);
    }
    let mut r = Reader::new(output);
    let capabilities = r.u32()?;
    let guid = r.guid()?;
    let security_mode = r.u16()?;
    let dialect = r.u16()?;
    if capabilities == negotiated.capabilities.bits()
        && guid == negotiated.server_guid
        && security_mode == negotiated.security_mode.bits()
        && dialect == negotiated.dialect.code()
    {
        Ok(())
    } else {
        Err(SmbError::Refused(Refusal::Downgrade))
    }
}

/// A validate-negotiate response's output, as a server answers one.
#[must_use]
pub fn validate_negotiate_output(
    response: &NegotiateResponse,
) -> [u8; VALIDATE_NEGOTIATE_OUTPUT_LEN] {
    let mut out = [0u8; VALIDATE_NEGOTIATE_OUTPUT_LEN];
    out[..4].copy_from_slice(&response.capabilities.bits().to_le_bytes());
    out[4..20].copy_from_slice(&response.server_guid);
    out[20..22].copy_from_slice(&response.security_mode.bits().to_le_bytes());
    out[22..24].copy_from_slice(&response.dialect.to_le_bytes());
    out
}

#[cfg(test)]
#[path = "negotiate_tests.rs"]
mod tests;
