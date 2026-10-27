//! A scripted SMB 3 server for host tests: one connection, one account, one
//! disk share held in memory ([`Share`]), and whatever misbehaviour a test
//! asks of it. It verifies what a real server would — the NTLMv2 proof, the
//! MIC, the service name, the mechListMIC, the pre-auth hash through the keys
//! it derives — so a client that passes it has been checked by a second,
//! independent reading of the protocol. Test support, never in a TAIRiX
//! build.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_crypto::ct_eq;
use tairix_crypto::ntlm::{ntlm_checksum, ntlm_hmac, ntlm_signing_key, NtlmDirection, NtlmKey};
use tairix_der::{DerError, Reader as DerReader, Tag, Writer};

use crate::crypt::{
    KeyInputs, PreauthHash, SessionKeys, Transform, NONCE_SEED_LEN, TRANSFORM_HEADER_LEN,
    TRANSFORM_PROTOCOL_ID,
};
use crate::file::NO_FILE;
use crate::frame::{self, Framer};
use crate::header::{Address, Command, Flags, Header, HEADER_LEN, SIGNATURE_RANGE};
use crate::ioctl::{IoctlRequest, IoctlResponse};
use crate::negotiate::{
    validate_negotiate_input, validate_negotiate_output, Capabilities, Cipher, Dialect,
    NegotiateContext, NegotiateRequest, NegotiateResponse, SecurityMode, SigningAlgorithm,
    FSCTL_VALIDATE_NEGOTIATE_INFO, SHA512,
};
use crate::ntlm::{self, av, flag, NtlmCredential};
use crate::session::{SessionFlags, SessionSetupRequest, SessionSetupResponse};
use crate::simple::{encode_empty, ErrorResponse};
use crate::spnego::{self, NegState, Reply};
use crate::status::NtStatus;
use crate::tree::{ShareType, TreeConnectRequest, TreeConnectResponse};
use crate::wire::{from_utf16le, owned, utf16le, utf16le_len, Encoder, Guid, Reader};
use crate::{Role, SmbError};

mod pipe;
mod share;

use pipe::Pipes;
pub use share::Share;
use share::{Caller, Chain, Sent};

/// The server's GUID.
pub const SERVER_GUID: Guid = *b"tairix test peer";

/// The disk share the server serves.
pub const DISK_SHARE: &str = "share";

/// Kerberos 5's OIDs, as Microsoft's and the IETF's registrations spell them.
const MS_KRB5: &[u8] = &[0x2a, 0x86, 0x48, 0x82, 0xf7, 0x12, 0x01, 0x02, 0x02];
const KRB5: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x12, 0x01, 0x02, 0x02];

/// An OID no client is offered.
const OTHER_MECHANISM: &[u8] = &[0x2b, 0x06, 0x01, 0x04, 0x01, 0x82, 0x37, 0x02, 0x02, 0x1e];

/// `NTLMSSP_TARGET_TYPE_SERVER`, which a standalone server's challenge names.
const TARGET_TYPE_SERVER: u32 = 0x0002_0000;

/// `MsvAvNbComputerName` and `MsvAvNbDomainName`.
const AV_NB_COMPUTER: u16 = 0x0001;
const AV_NB_DOMAIN: u16 = 0x0002;

/// The share-wide access a tree connect grants: everything.
const MAXIMAL_ACCESS: u32 = 0x001f_01ff;

/// What a message about no tree is addressed by.
const NO_TREE: Address = Address::Sync { tree_id: 0 };

/// Where a `CHALLENGE_MESSAGE`'s payload starts.
const CHALLENGE_FIXED: usize = 56;

/// Which mechanisms the server's `NEGOTIATE` hints offer.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Hints {
    /// NTLMSSP, with Samba's placeholder hint.
    Ntlm,
    /// None at all: an empty security buffer.
    Empty,
    /// Kerberos alone.
    Kerberos,
}

/// The misbehaviour a test asks of the server.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Fault {
    /// None: the server keeps the protocol.
    None,
    /// The final `SESSION_SETUP` response is unsigned.
    UnsignedFinal,
    /// The final `SESSION_SETUP` response's signature is corrupt.
    ForgedFinal,
    /// The session is granted as a guest's.
    Guest,
    /// `SESSION_SETUP` succeeds at its first leg, before NTLM can finish.
    EarlySuccess,
    /// The first reply names a mechanism the client never offered.
    OtherMechanism,
    /// The server's mechListMIC is corrupt.
    ForgedMechListMic,
    /// The server sends no mechListMIC.
    NoMechListMic,
    /// The challenge withholds extended session security.
    WeakChallenge,
    /// The final response names another session.
    OtherSession,
    /// validate-negotiate answers with another dialect.
    Downgrade,
    /// validate-negotiate is answered `STATUS_NOT_SUPPORTED`.
    RefuseValidation,
    /// Requests on a tree answer `STATUS_NETWORK_SESSION_EXPIRED` until the
    /// session is re-authenticated.
    Expire,
    /// No response grants a credit.
    Starve,
    /// The first tree connect's response grants no credit.
    WithholdTreeCredit,
    /// `srvsvc` refuses the bind.
    RefuseBind,
    /// `NetrShareEnum` answers `ERROR_ACCESS_DENIED`.
    DenyShareEnum,
    /// The share listing names a share no tree connect could take.
    HostileShareName,
}

/// The server a test talks to.
#[derive(Clone, Debug)]
pub struct PeerConfig {
    /// The dialect it chooses; a client not offering it is refused.
    pub dialect: Dialect,
    /// The cipher a 3.1.1 server chooses.
    pub cipher: Cipher,
    /// The signing algorithm a 3.1.1 server chooses, or none, meaning
    /// AES-CMAC with no context.
    pub signing: Option<SigningAlgorithm>,
    /// Its name, which a tree path and the client's service name must name.
    pub host: &'static str,
    /// Its domain, and its one account's.
    pub domain: &'static str,
    /// The account.
    pub user: &'static str,
    /// The account's password.
    pub password: &'static str,
    /// What its `NEGOTIATE` hints offer.
    pub hints: Hints,
    /// The `MsvAvTimestamp` its challenge carries, or none.
    pub timestamp: Option<u64>,
    /// The credits each response grants.
    pub grant: u16,
    /// How many shares beyond its own it lists, to make a listing span
    /// fragments.
    pub extra_shares: u16,
    /// Its misbehaviour.
    pub fault: Fault,
}

impl PeerConfig {
    /// A server keeping the protocol and choosing `dialect`.
    #[must_use]
    pub const fn new(dialect: Dialect) -> Self {
        Self {
            dialect,
            cipher: Cipher::Aes256Gcm,
            signing: Some(SigningAlgorithm::AesGmac),
            host: "nas.example",
            domain: "WORKGROUP",
            user: "alice",
            password: "correct horse battery staple",
            hints: Hints::Ntlm,
            timestamp: Some(134_051_616_000_000_000),
            grant: 32,
            extra_shares: 0,
            fault: Fault::None,
        }
    }

    /// The cipher every message after setup is sealed with.
    const fn sealing(&self) -> Cipher {
        match self.dialect {
            Dialect::Smb300 | Dialect::Smb302 => Cipher::Aes128Ccm,
            Dialect::Smb311 => self.cipher,
        }
    }

    /// The algorithm the final `SESSION_SETUP` response is signed with.
    const fn signing_algorithm(&self) -> SigningAlgorithm {
        match (self.dialect, self.signing) {
            (Dialect::Smb311, Some(algorithm)) => algorithm,
            _ => SigningAlgorithm::AesCmac,
        }
    }
}

/// An NTLM exchange the server is part-way through.
#[derive(Debug)]
struct Setup {
    session_id: u64,
    renewal: bool,
    /// The session's pre-auth hash, for a first authentication on 3.1.1.
    preauth: PreauthHash,
    mech_types: Vec<u8>,
    negotiate: Vec<u8>,
    challenge: Vec<u8>,
    server_challenge: [u8; 8],
}

/// The scripted server.
pub struct Peer {
    config: PeerConfig,
    framer: Framer,
    out: Vec<u8>,
    negotiated: Option<(NegotiateRequest, NegotiateResponse)>,
    preauth: PreauthHash,
    challenges: u64,
    setup: Option<Setup>,
    session: Option<(u64, SessionKeys)>,
    previous_session_ids: Vec<u64>,
    renewed: bool,
    trees: Vec<(u32, ShareType)>,
    next_tree: u32,
    validations: usize,
    share: Share,
    pipes: Pipes,
}

impl core::fmt::Debug for Peer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Peer")
            .field("config", &self.config)
            .field("trees", &self.trees)
            .field("validations", &self.validations)
            .finish_non_exhaustive()
    }
}

impl Peer {
    /// A server waiting for a client's `NEGOTIATE`, its share empty.
    #[must_use]
    pub fn new(config: PeerConfig) -> Self {
        Self::with_share(config, Share::new())
    }

    /// A server waiting for a client's `NEGOTIATE` on a new connection to
    /// `share`.
    #[must_use]
    pub const fn with_share(config: PeerConfig, share: Share) -> Self {
        Self {
            config,
            framer: Framer::new(),
            out: Vec::new(),
            negotiated: None,
            preauth: PreauthHash::new(),
            challenges: 0,
            setup: None,
            session: None,
            previous_session_ids: Vec::new(),
            renewed: false,
            trees: Vec::new(),
            next_tree: 0,
            validations: 0,
            share,
            pipes: Pipes::new(),
        }
    }

    /// Drop the connection, and keep the share: what only this connection
    /// held closes, and the durable opens wait to be reclaimed.
    ///
    /// # Errors
    ///
    /// [`SmbError::OutOfMemory`].
    pub fn disconnect(mut self) -> Result<Share, SmbError> {
        self.share.detach()?;
        Ok(self.share)
    }

    /// The share it serves.
    #[must_use]
    pub const fn share(&self) -> &Share {
        &self.share
    }

    /// How many pipes are open on `IPC$`.
    #[must_use]
    pub fn open_pipes(&self) -> usize {
        self.pipes.open_count()
    }

    /// The session each `SESSION_SETUP` leg said it replaces, zero for
    /// none, in the order the legs arrived.
    #[must_use]
    pub fn previous_session_ids(&self) -> &[u64] {
        &self.previous_session_ids
    }

    /// The bytes it has answered with, taken.
    pub fn take_output(&mut self) -> Vec<u8> {
        core::mem::take(&mut self.out)
    }

    /// How many validate-negotiate requests have arrived.
    #[must_use]
    pub const fn validations(&self) -> usize {
        self.validations
    }

    /// The established session's id.
    #[must_use]
    pub fn session_id(&self) -> Option<u64> {
        self.session.as_ref().map(|(id, _)| *id)
    }

    /// Take `bytes` the client wrote and answer every request in them.
    ///
    /// # Errors
    ///
    /// Whatever a real server would drop the connection for: a frame that
    /// does not open, a request it cannot read, a protocol violation.
    pub fn receive(&mut self, bytes: &[u8]) -> Result<(), SmbError> {
        self.framer.push(bytes)?;
        while let Some(frame) = self.framer.next_frame()? {
            let mut chain = Chain::default();
            let (mut buf, at) = frame.into_parts();
            let sealed = buf.get(at..at + 4) == Some(&TRANSFORM_PROTOCOL_ID[..]);
            let start = if sealed {
                let transform = Transform::parse(&buf[at..])?;
                let (id, keys) = self.session.as_ref().ok_or(SmbError::Integrity)?;
                if transform.session_id != *id {
                    return Err(SmbError::Integrity);
                }
                keys.open(&mut buf[at..])?;
                at + TRANSFORM_HEADER_LEN
            } else {
                at
            };
            for range in frame::messages(&buf[start..])? {
                self.request(
                    &buf[start + range.start..start + range.end],
                    sealed,
                    &mut chain,
                )?;
            }
        }
        Ok(())
    }

    fn request(&mut self, message: &[u8], sealed: bool, chain: &mut Chain) -> Result<(), SmbError> {
        let header = Header::decode(message)?;
        if header.is_response() {
            return Err(SmbError::Protocol);
        }
        match header.command {
            Command::Negotiate => self.on_negotiate(&header, message),
            Command::SessionSetup => self.on_session_setup(&header, message, sealed),
            // A session that holds keys is served only sealed.
            _ if !sealed => Err(SmbError::Integrity),
            Command::TreeConnect => self.on_tree_connect(&header, message),
            Command::Ioctl => self.on_ioctl(&header, message, chain),
            Command::TreeDisconnect => {
                let Address::Sync { tree_id } = header.address else {
                    return Err(SmbError::Protocol);
                };
                self.trees.retain(|(tree, _)| *tree != tree_id);
                self.answer(&header, NtStatus::SUCCESS, tree_id, true, encode_empty)
            }
            Command::Logoff => {
                self.answer(&header, NtStatus::SUCCESS, 0, true, encode_empty)?;
                self.session = None;
                Ok(())
            }
            Command::Cancel | Command::OplockBreak => {
                self.on_share_request(&header, message, chain)
            }
            _ => self.on_tree_request(&header, message, chain),
        }
    }

    fn on_negotiate(&mut self, header: &Header, message: &[u8]) -> Result<(), SmbError> {
        if self.negotiated.is_some() {
            return Err(SmbError::Protocol);
        }
        let request = NegotiateRequest::decode(message)?;
        let dialect = self.config.dialect;
        if !request.dialects.contains(&dialect.code()) {
            return self.fail_request(header, NtStatus::NOT_SUPPORTED, 0, false);
        }
        let mut preauth = PreauthHash::new();
        preauth.update(message);
        let (capabilities, contexts) = match dialect {
            Dialect::Smb300 | Dialect::Smb302 => (
                Capabilities::LEASING
                    .with(Capabilities::LARGE_MTU)
                    .with(Capabilities::ENCRYPTION),
                Vec::new(),
            ),
            Dialect::Smb311 => {
                let mut contexts = alloc::vec![
                    NegotiateContext::PreauthIntegrity {
                        hash_algorithms: alloc::vec![SHA512],
                        salt: alloc::vec![0x5e; 32],
                    },
                    NegotiateContext::Encryption {
                        ciphers: alloc::vec![self.config.cipher.code()],
                    },
                ];
                if let Some(algorithm) = self.config.signing {
                    contexts.push(NegotiateContext::Signing {
                        algorithms: alloc::vec![algorithm.code()],
                    });
                }
                (
                    Capabilities::LEASING.with(Capabilities::LARGE_MTU),
                    contexts,
                )
            }
        };
        let response = NegotiateResponse {
            security_mode: SecurityMode::SIGNING_ENABLED,
            dialect: dialect.code(),
            server_guid: SERVER_GUID,
            capabilities,
            max_transact_size: 8 << 20,
            max_read_size: 8 << 20,
            max_write_size: 8 << 20,
            system_time: self.config.timestamp.unwrap_or(0),
            server_start_time: 0,
            security_buffer: hints(self.config.hints)?,
            contexts,
        };
        let answer = self.message(header, NtStatus::SUCCESS, 0, NO_TREE, |enc| {
            response.encode(enc);
        })?;
        if dialect == Dialect::Smb311 {
            preauth.update(&answer);
        }
        self.queue(answer, false)?;
        self.preauth = preauth;
        self.negotiated = Some((request, response));
        Ok(())
    }

    fn on_session_setup(
        &mut self,
        header: &Header,
        message: &[u8],
        sealed: bool,
    ) -> Result<(), SmbError> {
        let request = SessionSetupRequest::decode(message)?;
        self.previous_session_ids.push(request.previous_session_id);
        let renewal = match (sealed, &self.session) {
            (true, Some((id, _))) if header.session_id == *id => true,
            (false, None) => false,
            _ => return Err(SmbError::Protocol),
        };
        match self.setup.take() {
            None => self.first_leg(header, message, &request, renewal),
            Some(setup) if setup.session_id == header.session_id && setup.renewal == renewal => {
                self.second_leg(header, message, &request, setup)
            }
            Some(_) => Err(SmbError::Protocol),
        }
    }

    fn first_leg(
        &mut self,
        header: &Header,
        message: &[u8],
        request: &SessionSetupRequest,
        renewal: bool,
    ) -> Result<(), SmbError> {
        if !renewal && header.session_id != 0 {
            return Err(SmbError::Protocol);
        }
        let (mech_types, negotiate) = read_init(&request.security_buffer)?;
        let mut r = Reader::new(&negotiate);
        if r.array::<8>()? != ntlm::SIGNATURE || r.u32()? != ntlm::NEGOTIATE {
            return Err(SmbError::Malformed);
        }
        self.challenges += 1;
        let session_id = match &self.session {
            Some((id, _)) => *id,
            None => 0x0000_4400_0000_0000 | self.challenges,
        };
        if self.config.fault == Fault::EarlySuccess && !renewal {
            let token = neg_token_resp(Some(NegState::AcceptCompleted), None, None, None)?;
            return self.setup_answer(header, NtStatus::SUCCESS, session_id, token, false);
        }
        let mut preauth = self.preauth.clone();
        let use_preauth = !renewal && self.config.dialect == Dialect::Smb311;
        if use_preauth {
            preauth.update(message);
        }
        let mut server_challenge = [0u8; 8];
        server_challenge.copy_from_slice(&(0x5eed_0000_0000_0000 ^ self.challenges).to_le_bytes());
        let challenge = self.challenge_message(server_challenge)?;
        let mechanism = if self.config.fault == Fault::OtherMechanism {
            OTHER_MECHANISM
        } else {
            ntlm::OID
        };
        let response = SessionSetupResponse {
            session_flags: SessionFlags::default(),
            security_buffer: neg_token_resp(
                Some(NegState::AcceptIncomplete),
                Some(mechanism),
                Some(&challenge),
                None,
            )?,
        };
        let answer = self.message(
            header,
            NtStatus::MORE_PROCESSING_REQUIRED,
            session_id,
            NO_TREE,
            |enc| response.encode(enc),
        )?;
        if use_preauth {
            preauth.update(&answer);
        }
        self.queue(answer, renewal)?;
        self.setup = Some(Setup {
            session_id,
            renewal,
            preauth,
            mech_types,
            negotiate,
            challenge,
            server_challenge,
        });
        Ok(())
    }

    fn second_leg(
        &mut self,
        header: &Header,
        message: &[u8],
        request: &SessionSetupRequest,
        mut setup: Setup,
    ) -> Result<(), SmbError> {
        if !setup.renewal && self.config.dialect == Dialect::Smb311 {
            setup.preauth.update(message);
        }
        let reply = Reply::decode(&request.security_buffer)?;
        let (Some(authenticate), Some(mic)) = (reply.token, reply.mic) else {
            return Err(SmbError::Protocol);
        };
        let Ok(session_key) = self.verify(&setup, authenticate, mic) else {
            return self.fail_request(header, NtStatus::LOGON_FAILURE, 0, setup.renewal);
        };
        let server_signing = ntlm_signing_key(&session_key, NtlmDirection::ServerToClient);
        let mut server_mic = signature(&server_signing, &setup.mech_types);
        if self.config.fault == Fault::ForgedMechListMic {
            server_mic[5] ^= 0x01;
        }
        let mic = (self.config.fault != Fault::NoMechListMic).then_some(&server_mic[..]);
        let token = neg_token_resp(Some(NegState::AcceptCompleted), None, None, mic)?;
        if setup.renewal {
            self.renewed = true;
            return self.setup_answer(header, NtStatus::SUCCESS, setup.session_id, token, true);
        }
        let keys = SessionKeys::derive(
            &KeyInputs {
                dialect: self.config.dialect,
                cipher: self.config.sealing(),
                signing: self.config.signing_algorithm(),
                session_key: &session_key,
                preauth: &setup.preauth,
            },
            Role::Server,
            &[0x77; NONCE_SEED_LEN],
        )?;
        let guest = self.config.fault == Fault::Guest;
        let named = if self.config.fault == Fault::OtherSession {
            setup.session_id + 1
        } else {
            setup.session_id
        };
        let response = SessionSetupResponse {
            session_flags: if guest {
                SessionFlags::IS_GUEST
            } else {
                SessionFlags::default()
            },
            security_buffer: token,
        };
        let mut answer = self.message(header, NtStatus::SUCCESS, named, NO_TREE, |enc| {
            response.encode(enc);
        })?;
        if !guest && self.config.fault != Fault::UnsignedFinal {
            keys.sign(&mut answer)?;
        }
        if self.config.fault == Fault::ForgedFinal {
            answer[SIGNATURE_RANGE.start] ^= 0x01;
        }
        self.queue(answer, false)?;
        self.session = Some((setup.session_id, keys));
        Ok(())
    }

    /// Check an `AUTHENTICATE_MESSAGE` as a server does, and the session key
    /// it proves the client holds.
    fn verify(&self, setup: &Setup, authenticate: &[u8], mic: &[u8]) -> Result<NtlmKey, SmbError> {
        let message = read_authenticate(authenticate)?;
        if !message.user.eq_ignore_ascii_case(self.config.user)
            || !message.domain.eq_ignore_ascii_case(self.config.domain)
        {
            return Err(SmbError::Integrity);
        }
        let credential = NtlmCredential::new(&message.user, &message.domain, self.config.password)?;
        let key = credential.response_key();
        let (proof, blob) = message.nt.split_at_checked(16).ok_or(SmbError::Malformed)?;
        if blob.len() < 32 || blob[..2] != [1, 1] {
            return Err(SmbError::Malformed);
        }
        if !ct_eq(&ntlm_hmac(key, &[&setup.server_challenge, blob]), proof) {
            return Err(SmbError::Integrity);
        }
        let session_key = ntlm_hmac(key, &[proof]);
        self.check_target_info(&blob[28..blob.len() - 4])?;
        let expected = ntlm::message_integrity(
            &session_key,
            &setup.negotiate,
            &setup.challenge,
            authenticate,
        )?;
        if !ct_eq(&expected, &authenticate[ntlm::MIC]) {
            return Err(SmbError::Integrity);
        }
        let client_signing = ntlm_signing_key(&session_key, NtlmDirection::ClientToServer);
        if !ct_eq(&signature(&client_signing, &setup.mech_types), mic) {
            return Err(SmbError::Integrity);
        }
        Ok(session_key)
    }

    /// The client's answer to the target information: it announces its MIC,
    /// binds no channel, names this server's file service, and echoes the
    /// time.
    fn check_target_info(&self, target_info: &[u8]) -> Result<(), SmbError> {
        let pairs = ntlm::av_pairs(target_info)?;
        let value = |id: u16| {
            pairs
                .iter()
                .find(|(pair, _)| *pair == id)
                .map(|(_, value)| *value)
        };
        let flags = value(av::FLAGS)
            .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
            .map(u32::from_le_bytes);
        let mut spn = String::from("cifs/");
        spn.push_str(self.config.host);
        let named = value(av::TARGET_NAME).map(from_utf16le).transpose()?;
        let timestamp = self.config.timestamp.map(u64::to_le_bytes);
        if flags.is_none_or(|flags| flags & ntlm::AV_FLAG_MIC == 0)
            || value(av::CHANNEL_BINDINGS) != Some(&[0; 16][..])
            || named.is_none_or(|named| !named.eq_ignore_ascii_case(&spn))
            || value(av::TIMESTAMP) != timestamp.as_ref().map(|stamp| &stamp[..])
        {
            return Err(SmbError::Integrity);
        }
        Ok(())
    }

    fn challenge_message(&self, server_challenge: [u8; 8]) -> Result<Vec<u8>, SmbError> {
        let flags = if self.config.fault == Fault::WeakChallenge {
            CHALLENGE_FLAGS & !flag::EXTENDED_SESSION_SECURITY
        } else {
            CHALLENGE_FLAGS
        };
        let mut info = Encoder::new();
        let computer = self.config.host.split('.').next().unwrap_or_default();
        for (id, text) in [
            (AV_NB_DOMAIN, self.config.domain),
            (AV_NB_COMPUTER, computer),
        ] {
            info.u16(id);
            let len = info.u16_of(utf16le_len(text));
            info.u16(len);
            utf16le(text, &mut info);
        }
        if let Some(stamp) = self.config.timestamp {
            info.u16(av::TIMESTAMP);
            info.u16(8);
            info.u64(stamp);
        }
        info.u16(av::EOL);
        info.u16(0);
        challenge_message(flags, server_challenge, self.config.domain, &info.finish()?)
    }

    fn on_tree_connect(&mut self, header: &Header, message: &[u8]) -> Result<(), SmbError> {
        let request = TreeConnectRequest::decode(message)?;
        let share_type = request
            .path
            .strip_prefix(r"\\")
            .and_then(|rest| rest.split_once('\\'))
            .filter(|(host, _)| host.eq_ignore_ascii_case(self.config.host))
            .and_then(|(_, share)| match share {
                _ if share.eq_ignore_ascii_case(DISK_SHARE) => Some(ShareType::Disk),
                _ if share.eq_ignore_ascii_case("IPC$") => Some(ShareType::Pipe),
                _ => None,
            });
        let Some(share_type) = share_type else {
            return self.fail_request(header, NtStatus::BAD_NETWORK_NAME, 0, true);
        };
        self.next_tree += 1;
        let tree_id = self.next_tree;
        self.trees.push((tree_id, share_type));
        self.answer(header, NtStatus::SUCCESS, tree_id, true, |enc| {
            TreeConnectResponse {
                share_type,
                share_flags: 0,
                capabilities: 0,
                maximal_access: MAXIMAL_ACCESS,
            }
            .encode(enc);
        })
    }

    fn on_ioctl(
        &mut self,
        header: &Header,
        message: &[u8],
        chain: &mut Chain,
    ) -> Result<(), SmbError> {
        let Address::Sync { tree_id } = header.address else {
            return Err(SmbError::Protocol);
        };
        let request = IoctlRequest::decode(message)?;
        if request.ctl_code != FSCTL_VALIDATE_NEGOTIATE_INFO {
            return self.on_tree_request(header, message, chain);
        }
        self.validations += 1;
        let (sent, answered) = self.negotiated.as_ref().ok_or(SmbError::Protocol)?;
        // What the client says it sent must be what arrived.
        if request.input != validate_negotiate_input(sent)? || request.file_id != NO_FILE {
            return Err(SmbError::Integrity);
        }
        if self.config.fault == Fault::RefuseValidation {
            return self.fail_request(header, NtStatus::NOT_SUPPORTED, tree_id, true);
        }
        let mut output = validate_negotiate_output(answered);
        if self.config.fault == Fault::Downgrade {
            output[22] = 0x10;
            output[23] = 0x02;
        }
        let response = IoctlResponse {
            ctl_code: FSCTL_VALIDATE_NEGOTIATE_INFO,
            file_id: NO_FILE,
            output: owned(&output)?,
        };
        self.answer(header, NtStatus::SUCCESS, tree_id, true, |enc| {
            response.encode(enc);
        })
    }

    fn on_tree_request(
        &mut self,
        header: &Header,
        message: &[u8],
        chain: &mut Chain,
    ) -> Result<(), SmbError> {
        let Address::Sync { tree_id } = header.address else {
            return Err(SmbError::Protocol);
        };
        let Some(&(_, share_type)) = self.trees.iter().find(|(tree, _)| *tree == tree_id) else {
            return self.fail_request(header, NtStatus::NETWORK_NAME_DELETED, tree_id, true);
        };
        if self.config.fault == Fault::Expire && !self.renewed {
            return self.fail_request(header, NtStatus::NETWORK_SESSION_EXPIRED, tree_id, true);
        }
        match (header.command, share_type) {
            (Command::Echo, _) => {
                self.answer(header, NtStatus::SUCCESS, tree_id, true, encode_empty)
            }
            (_, ShareType::Disk) => self.on_share_request(header, message, chain),
            (_, ShareType::Pipe) => {
                let (status, body) = self.pipes.request(&self.config, header.command, message)?;
                self.answer(header, status, tree_id, true, |enc| {
                    enc.bytes(&body[HEADER_LEN..]);
                })
            }
            _ => self.fail_request(header, NtStatus::NOT_SUPPORTED, tree_id, true),
        }
    }

    /// Hand a request to the share, and send whatever it answers.
    fn on_share_request(
        &mut self,
        header: &Header,
        message: &[u8],
        chain: &mut Chain,
    ) -> Result<(), SmbError> {
        let client_guid = self
            .negotiated
            .as_ref()
            .map(|(request, _)| request.client_guid)
            .ok_or(SmbError::Protocol)?;
        let caller = Caller {
            user: self.config.user,
            client_guid,
        };
        self.share.request(caller, header, message, chain)?;
        let session_id = self.session.as_ref().map_or(0, |(id, _)| *id);
        for sent in self.share.take_sent() {
            let answer = match sent {
                Sent::Response {
                    request,
                    address,
                    status,
                    body,
                } => self.message(&request, status, session_id, address, |enc| {
                    enc.bytes(&body[HEADER_LEN..]);
                })?,
                Sent::Break(body) => {
                    let mut enc = Encoder::new();
                    Header {
                        credit_charge: 0,
                        status: NtStatus::SUCCESS,
                        command: Command::OplockBreak,
                        credits: 0,
                        flags: Flags::SERVER_TO_REDIR,
                        next_command: 0,
                        message_id: u64::MAX,
                        address: NO_TREE,
                        session_id: 0,
                        signature: [0; 16],
                    }
                    .encode(&mut enc);
                    enc.bytes(&body[HEADER_LEN..]);
                    enc.finish()?
                }
            };
            self.queue(answer, true)?;
        }
        Ok(())
    }

    /// A `SESSION_SETUP` response carrying `token`.
    fn setup_answer(
        &mut self,
        header: &Header,
        status: NtStatus,
        session_id: u64,
        token: Vec<u8>,
        sealed: bool,
    ) -> Result<(), SmbError> {
        let response = SessionSetupResponse {
            session_flags: SessionFlags::default(),
            security_buffer: token,
        };
        let answer = self.message(header, status, session_id, NO_TREE, |enc| {
            response.encode(enc);
        })?;
        self.queue(answer, sealed)
    }

    /// An error response with `status`.
    fn fail_request(
        &mut self,
        header: &Header,
        status: NtStatus,
        tree_id: u32,
        sealed: bool,
    ) -> Result<(), SmbError> {
        let session_id = self.session.as_ref().map_or(0, |(id, _)| *id);
        let answer = self.message(
            header,
            status,
            session_id,
            Address::Sync { tree_id },
            empty_error,
        )?;
        self.queue(answer, sealed)
    }

    /// A response on the established session's tree `tree_id`.
    fn answer(
        &mut self,
        header: &Header,
        status: NtStatus,
        tree_id: u32,
        sealed: bool,
        body: impl FnOnce(&mut Encoder),
    ) -> Result<(), SmbError> {
        let session_id = self.session.as_ref().map_or(0, |(id, _)| *id);
        let answer = self.message(header, status, session_id, Address::Sync { tree_id }, body)?;
        self.queue(answer, sealed)
    }

    fn message(
        &self,
        request: &Header,
        status: NtStatus,
        session_id: u64,
        address: Address,
        body: impl FnOnce(&mut Encoder),
    ) -> Result<Vec<u8>, SmbError> {
        let withheld = self.config.fault == Fault::WithholdTreeCredit
            && request.command == Command::TreeConnect
            && self.next_tree == 1;
        let credits = if self.config.fault == Fault::Starve || withheld {
            0
        } else {
            self.config.grant
        };
        let mut enc = Encoder::new();
        Header {
            credit_charge: request.credit_charge,
            status,
            command: request.command,
            credits,
            flags: Flags::SERVER_TO_REDIR,
            next_command: 0,
            message_id: request.message_id,
            address,
            session_id,
            signature: [0; 16],
        }
        .encode(&mut enc);
        body(&mut enc);
        enc.finish()
    }

    /// Frame `message`, sealed under the session's keys when `sealed`.
    fn queue(&mut self, message: Vec<u8>, sealed: bool) -> Result<(), SmbError> {
        let frame = if sealed {
            let (id, keys) = self.session.as_mut().ok_or(SmbError::Protocol)?;
            let mut frame = alloc::vec![0; TRANSFORM_HEADER_LEN];
            frame.extend_from_slice(&message);
            keys.seal(*id, &mut frame)?;
            frame
        } else {
            message
        };
        self.out.extend_from_slice(&frame::prefix(frame.len())?);
        self.out.extend_from_slice(&frame);
        Ok(())
    }
}

/// What `encode` writes behind a header-sized gap: a body whose offsets count
/// from its message's start, to follow whatever header it is sent behind.
pub(crate) fn behind_header(encode: impl FnOnce(&mut Encoder)) -> Result<Vec<u8>, SmbError> {
    let mut enc = Encoder::new();
    enc.zeros(HEADER_LEN);
    encode(&mut enc);
    enc.finish()
}

/// An error response's body with no error data.
fn empty_error(enc: &mut Encoder) {
    ErrorResponse {
        context_count: 0,
        data: Vec::new(),
    }
    .encode(enc);
}

/// What a standalone server's challenge agrees to: everything NTLMv2 needs.
pub(crate) const CHALLENGE_FLAGS: u32 = flag::UNICODE
    | flag::REQUEST_TARGET
    | flag::SIGN
    | flag::NTLM
    | flag::ALWAYS_SIGN
    | TARGET_TYPE_SERVER
    | flag::EXTENDED_SESSION_SECURITY
    | flag::TARGET_INFO
    | flag::VERSION
    | flag::KEY_128;

/// A `CHALLENGE_MESSAGE` agreeing to `flags`, naming `domain` and carrying
/// `target_info` as given.
pub(crate) fn challenge_message(
    flags: u32,
    server_challenge: [u8; 8],
    domain: &str,
    target_info: &[u8],
) -> Result<Vec<u8>, SmbError> {
    let name_len = utf16le_len(domain);
    let mut enc = Encoder::new();
    enc.bytes(&ntlm::SIGNATURE);
    enc.u32(ntlm::CHALLENGE);
    let len = enc.u16_of(name_len);
    enc.u16(len);
    enc.u16(len);
    let offset = enc.u32_of(CHALLENGE_FIXED);
    enc.u32(offset);
    enc.u32(flags);
    enc.bytes(&server_challenge);
    enc.zeros(8);
    let len = enc.u16_of(target_info.len());
    enc.u16(len);
    enc.u16(len);
    let offset = enc.u32_of(CHALLENGE_FIXED + name_len);
    enc.u32(offset);
    enc.bytes(&[10, 0, 0x4a, 0x61, 0, 0, 0, 0x0f]);
    utf16le(domain, &mut enc);
    enc.bytes(target_info);
    enc.finish()
}

/// The NTLMSSP signature over `data` at sequence zero.
fn signature(signing_key: &NtlmKey, data: &[u8]) -> [u8; ntlm::SIGNATURE_LEN] {
    let mut out = [0u8; ntlm::SIGNATURE_LEN];
    out[..4].copy_from_slice(&1u32.to_le_bytes());
    out[4..12].copy_from_slice(&ntlm_checksum(signing_key, 0, data));
    out
}

/// What the server reads of an `AUTHENTICATE_MESSAGE`.
struct Authenticate<'a> {
    nt: &'a [u8],
    domain: String,
    user: String,
}

fn read_authenticate(message: &[u8]) -> Result<Authenticate<'_>, SmbError> {
    let mut r = Reader::new(message);
    if r.array::<8>()? != ntlm::SIGNATURE || r.u32()? != ntlm::AUTHENTICATE {
        return Err(SmbError::Malformed);
    }
    let lm = ntlm::payload_field(message, &mut r)?;
    let nt = ntlm::payload_field(message, &mut r)?;
    let domain = from_utf16le(ntlm::payload_field(message, &mut r)?)?;
    let user = from_utf16le(ntlm::payload_field(message, &mut r)?)?;
    ntlm::payload_field(message, &mut r)?;
    let exchanged_key = ntlm::payload_field(message, &mut r)?;
    let flags = r.u32()?;
    let needed = flag::EXTENDED_SESSION_SECURITY | flag::UNICODE | flag::SIGN;
    if lm != [0; 24] || !exchanged_key.is_empty() || flags & needed != needed {
        return Err(SmbError::Malformed);
    }
    Ok(Authenticate { nt, domain, user })
}

/// The client's `negTokenInit`: its `MechTypeList` as encoded, and its
/// mechanism's token.
pub(crate) fn read_init(token: &[u8]) -> Result<(Vec<u8>, Vec<u8>), SmbError> {
    let mut r = DerReader::new(token);
    let (types, inner) = r.nested(Tag::application(0), |r| {
        if r.oid()? != spnego::OID {
            return Err(DerError::UnexpectedTag);
        }
        r.explicit(0, |r| {
            r.sequence(|r| {
                let types = r.explicit(0, DerReader::tlv)?;
                let inner = r.explicit(2, DerReader::octet_string)?;
                Ok((types.encoding, inner))
            })
        })
    })?;
    r.finish()?;
    Ok((owned(types)?, owned(inner)?))
}

/// An acceptor's `negTokenResp`.
fn neg_token_resp(
    state: Option<NegState>,
    mechanism: Option<&[u8]>,
    token: Option<&[u8]>,
    mic: Option<&[u8]>,
) -> Result<Vec<u8>, SmbError> {
    let mut w = Writer::new();
    w.explicit(1, |w| {
        w.sequence(|w| {
            if let Some(state) = state {
                w.explicit(0, |w| w.enumerated(state.code()));
            }
            if let Some(mechanism) = mechanism {
                w.explicit(1, |w| w.oid(mechanism));
            }
            if let Some(token) = token {
                w.explicit(2, |w| w.octet_string(token));
            }
            if let Some(mic) = mic {
                w.explicit(3, |w| w.octet_string(mic));
            }
        });
    });
    Ok(w.finish()?)
}

/// The `NEGOTIATE` hints: a `negTokenInit2` listing the mechanisms, with
/// Samba's placeholder hint name.
fn hints(hints: Hints) -> Result<Vec<u8>, SmbError> {
    let mechanisms: &[&[u8]] = match hints {
        Hints::Empty => return Ok(Vec::new()),
        Hints::Ntlm => &[ntlm::OID],
        Hints::Kerberos => &[MS_KRB5, KRB5],
    };
    let mut w = Writer::new();
    w.constructed(Tag::application(0), |w| {
        w.oid(spnego::OID);
        w.explicit(0, |w| {
            w.sequence(|w| {
                w.explicit(0, |w| {
                    w.sequence(|w| {
                        for mechanism in mechanisms {
                            w.oid(mechanism);
                        }
                    });
                });
                w.explicit(3, |w| {
                    w.sequence(|w| {
                        w.explicit(0, |w| {
                            w.general_string(b"not_defined_in_RFC4178@please_ignore");
                        });
                    });
                });
            });
        });
    });
    Ok(w.finish()?)
}
