//! NTLMv2 (MS-NLMP), the initiator: the three NTLMSSP messages, the NTLMv2
//! response bound to the server's target information and to the service the
//! client means to reach, the MIC over the whole exchange, the session key,
//! and the message signature SPNEGO's mechListMIC is made with.
//!
//! Only extended session security without key exchange is spoken: with
//! NTLMv2 an exchanged key adds RC4 and no security, since it is recoverable
//! from the same NT hash either way. A server that will not agree to that is
//! refused, never accommodated.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::netshare_ipc::ACCOUNT_NAME_MAX;
use tairix_abi::time::Time64;
use tairix_crypto::ct_eq;
use tairix_crypto::ntlm::{
    ntlm_checksum, ntlm_checksum_verify, ntlm_hmac, ntlm_signing_key, ntowfv2, NtlmDirection,
    NtlmKey,
};
use zeroize::Zeroize;

use crate::wire::{buffer_at, filetime_floor, owned, utf16le, utf16le_len, Encoder, Reader};
use crate::{Refusal, SmbError};

/// NTLMSSP's mechanism OID, 1.3.6.1.4.1.311.2.2.10, as DER contents.
pub const OID: &[u8] = &[0x2b, 0x06, 0x01, 0x04, 0x01, 0x82, 0x37, 0x02, 0x02, 0x0a];

/// The length of an NTLMSSP message signature: version, checksum, sequence.
pub const SIGNATURE_LEN: usize = 16;

pub(crate) const SIGNATURE: [u8; 8] = *b"NTLMSSP\0";

pub(crate) const NEGOTIATE: u32 = 1;
pub(crate) const CHALLENGE: u32 = 2;
pub(crate) const AUTHENTICATE: u32 = 3;

/// `NegotiateFlags` (MS-NLMP §2.2.2.5).
pub(crate) mod flag {
    pub const UNICODE: u32 = 0x0000_0001;
    pub const REQUEST_TARGET: u32 = 0x0000_0004;
    pub const SIGN: u32 = 0x0000_0010;
    pub const NTLM: u32 = 0x0000_0200;
    pub const ALWAYS_SIGN: u32 = 0x0000_8000;
    pub const EXTENDED_SESSION_SECURITY: u32 = 0x0008_0000;
    pub const TARGET_INFO: u32 = 0x0080_0000;
    pub const VERSION: u32 = 0x0200_0000;
    pub const KEY_128: u32 = 0x2000_0000;
}

/// What the `NEGOTIATE_MESSAGE` asks for.
const NEGOTIATE_FLAGS: u32 = flag::UNICODE
    | flag::REQUEST_TARGET
    | flag::SIGN
    | flag::NTLM
    | flag::ALWAYS_SIGN
    | flag::EXTENDED_SESSION_SECURITY
    | flag::VERSION
    | flag::KEY_128;

/// What the `AUTHENTICATE_MESSAGE` echoes of the server's agreement: what was
/// asked for, and the target information only a server announces.
const ACCEPTED: u32 = NEGOTIATE_FLAGS | flag::TARGET_INFO;

/// What a server must agree to: NTLMv2 needs its target information, and its
/// signatures extended session security.
const REQUIRED: u32 = flag::UNICODE
    | flag::SIGN
    | flag::NTLM
    | flag::EXTENDED_SESSION_SECURITY
    | flag::TARGET_INFO
    | flag::KEY_128;

/// The VERSION a message carries: product version zero, for the field is a
/// debugging aid and names nothing, and NTLM revision 15, the only one.
const VERSION: [u8; 8] = [0, 0, 0, 0, 0, 0, 0, 0x0f];

/// `AV_PAIR` ids (MS-NLMP §2.2.2.1).
pub(crate) mod av {
    pub const EOL: u16 = 0x0000;
    pub const FLAGS: u16 = 0x0006;
    pub const TIMESTAMP: u16 = 0x0007;
    pub const TARGET_NAME: u16 = 0x0009;
    pub const CHANNEL_BINDINGS: u16 = 0x000a;
}

/// `MsvAvFlags`: the AUTHENTICATE message carries a MIC.
pub(crate) const AV_FLAG_MIC: u32 = 0x0000_0002;

/// The most target information a server may send, and the most UTF-16 a
/// name may take: bounds on a stranger's bytes and on what a sixteen-bit
/// field carries.
const MAX_TARGET_INFO: usize = 4096;
const MAX_NAME_BYTES: usize = 512;

// A UTF-8 byte is at most two bytes of UTF-16, so every account name a
// network-share request carries fits.
const _: () = assert!(2 * ACCOUNT_NAME_MAX <= MAX_NAME_BYTES);

/// Where an AUTHENTICATE message's MIC lies, and where its payload starts.
pub(crate) const MIC: core::ops::Range<usize> = 72..88;
const AUTHENTICATE_FIXED: usize = 88;

/// What NTLMv2 authenticates with: the account, and the key its password
/// derives (NTOWFv2), held in place of the password and wiped on drop.
pub struct NtlmCredential {
    user: String,
    domain: String,
    response_key: NtlmKey,
}

impl core::fmt::Debug for NtlmCredential {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("NtlmCredential")
            .field("user", &self.user)
            .field("domain", &self.domain)
            .finish_non_exhaustive()
    }
}

impl Drop for NtlmCredential {
    fn drop(&mut self) {
        self.response_key.zeroize();
    }
}

impl NtlmCredential {
    /// The credential for `user` in `domain` — empty for a server's own
    /// accounts — with `password`, which is not kept.
    ///
    /// # Errors
    ///
    /// [`SmbError::Refused`] with [`Refusal::Guest`] for an empty user, which
    /// is an anonymous logon; [`SmbError::TooLarge`] for a name past the
    /// bound; [`SmbError::OutOfMemory`].
    pub fn new(user: &str, domain: &str, password: &str) -> Result<Self, SmbError> {
        if user.is_empty() {
            return Err(SmbError::Refused(Refusal::Guest));
        }
        let user = bounded_name(user)?;
        let domain = bounded_name(domain)?;
        let mut identity = Encoder::new();
        for c in user.chars().map(upper_simple) {
            let mut units = [0u16; 2];
            for unit in c.encode_utf16(&mut units) {
                identity.u16(*unit);
            }
        }
        utf16le(&domain, &mut identity);
        let identity = identity.finish()?;
        let mut secret = utf16le_exact(password)?;
        let response_key = ntowfv2(&secret, &identity);
        secret.zeroize();
        Ok(Self {
            user,
            domain,
            response_key,
        })
    }

    /// The credential for `user` in `domain` whose password derived
    /// `response_key`: what a process that never sees the password signs in
    /// with.
    ///
    /// # Errors
    ///
    /// As [`Self::new`].
    pub fn keyed(user: &str, domain: &str, response_key: &NtlmKey) -> Result<Self, SmbError> {
        if user.is_empty() {
            return Err(SmbError::Refused(Refusal::Guest));
        }
        Ok(Self {
            user: bounded_name(user)?,
            domain: bounded_name(domain)?,
            response_key: *response_key,
        })
    }

    /// The account's user name.
    #[must_use]
    pub fn user(&self) -> &str {
        &self.user
    }

    /// The account's domain.
    #[must_use]
    pub fn domain(&self) -> &str {
        &self.domain
    }

    /// The NTOWFv2 key: what [`Self::keyed`] is handed, and what a server
    /// verifies a response with.
    #[must_use]
    pub const fn response_key(&self) -> &NtlmKey {
        &self.response_key
    }

    /// Whether `other` is the same account with the same password, the keys
    /// compared in constant time.
    #[must_use]
    pub fn matches(&self, other: &Self) -> bool {
        let same_key = ct_eq(&self.response_key, &other.response_key);
        same_key && self.user == other.user && self.domain == other.domain
    }
}

/// `text` as UTF-16LE in a buffer reserved whole before it is written, so a
/// secret is never left behind in a buffer outgrown on the way.
fn utf16le_exact(text: &str) -> Result<Vec<u8>, SmbError> {
    let mut out = Vec::new();
    out.try_reserve_exact(utf16le_len(text))
        .map_err(|_| SmbError::OutOfMemory)?;
    for unit in text.encode_utf16() {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    Ok(out)
}

/// `name`, held, once its UTF-16 fits the bound.
fn bounded_name(name: &str) -> Result<String, SmbError> {
    if utf16le_len(name) > MAX_NAME_BYTES {
        return Err(SmbError::TooLarge);
    }
    let mut out = String::new();
    out.try_reserve(name.len())
        .map_err(|_| SmbError::OutOfMemory)?;
    out.push_str(name);
    Ok(out)
}

/// `c` upper-cased as NTOWFv2's `Uppercase` is by Windows and Samba: Unicode's
/// simple mapping over UTF-16 units, so a character outside the Basic
/// Multilingual Plane, or one with no one-character capital, stays as it is.
fn upper_simple(c: char) -> char {
    if u32::from(c) > 0xffff {
        return c;
    }
    let mut upper = c.to_uppercase();
    match (upper.next(), upper.next()) {
        (Some(single), None) => single,
        _ => iota_subscript_capital(c).unwrap_or(c),
    }
}

/// The simple capital of a Greek small letter with iota subscript, whose
/// full uppercase is two letters: in the UCD these are the only characters
/// whose simple mapping a one-character full mapping does not give.
fn iota_subscript_capital(c: char) -> Option<char> {
    let code = u32::from(c);
    let capital = match code {
        0x1f80..=0x1f87 | 0x1f90..=0x1f97 | 0x1fa0..=0x1fa7 => code + 8,
        0x1fb3 | 0x1fc3 | 0x1ff3 => code + 9,
        _ => return None,
    };
    char::from_u32(capital)
}

/// What the host's random and clock contribute to one exchange.
#[derive(Copy, Clone, Debug)]
pub struct NtlmInputs {
    /// Eight bytes from the host's CSPRNG.
    pub client_challenge: [u8; 8],
    /// The host's time, used only when the server sends none.
    pub now: Time64,
}

/// A server's `CHALLENGE_MESSAGE`, checked.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Challenge<'a> {
    /// The flags the server agreed to.
    pub flags: u32,
    /// The server's challenge.
    pub server_challenge: [u8; 8],
    /// The target information's `AV_PAIR`s, `MsvAvEOL` excluded.
    pub pairs: Vec<(u16, &'a [u8])>,
    /// The server's `MsvAvTimestamp`, when it sent one.
    pub timestamp: Option<[u8; 8]>,
    /// The server's `MsvAvFlags`, when it sent them.
    pub av_flags: Option<u32>,
}

impl<'a> Challenge<'a> {
    /// The `CHALLENGE_MESSAGE` in `message`.
    ///
    /// # Errors
    ///
    /// [`SmbError::Malformed`] for a message that is not a well-formed
    /// challenge, [`SmbError::TooLarge`] for target information past the
    /// bound.
    pub fn decode(message: &'a [u8]) -> Result<Self, SmbError> {
        let mut r = Reader::new(message);
        if r.array::<8>()? != SIGNATURE || r.u32()? != CHALLENGE {
            return Err(SmbError::Malformed);
        }
        r.skip(8)?;
        let flags = r.u32()?;
        let server_challenge = r.array()?;
        r.skip(8)?;
        let target_info = payload_field(message, &mut r)?;
        if target_info.len() > MAX_TARGET_INFO {
            return Err(SmbError::TooLarge);
        }
        let pairs = av_pairs(target_info)?;
        let single = |id: u16| -> Result<Option<&[u8]>, SmbError> {
            let mut found = pairs.iter().filter(|(pair, _)| *pair == id);
            match (found.next(), found.next()) {
                (Some((_, value)), None) => Ok(Some(value)),
                (None, _) => Ok(None),
                _ => Err(SmbError::Malformed),
            }
        };
        let timestamp = match single(av::TIMESTAMP)? {
            Some(value) => Some(value.try_into().map_err(|_| SmbError::Malformed)?),
            None => None,
        };
        let av_flags = match single(av::FLAGS)? {
            Some(value) => Some(u32::from_le_bytes(
                value.try_into().map_err(|_| SmbError::Malformed)?,
            )),
            None => None,
        };
        Ok(Self {
            flags,
            server_challenge,
            pairs,
            timestamp,
            av_flags,
        })
    }
}

/// The `(len, max len, offset)` field next in `r`, as the bytes it names.
pub(crate) fn payload_field<'a>(
    message: &'a [u8],
    r: &mut Reader<'_>,
) -> Result<&'a [u8], SmbError> {
    let len = usize::from(r.u16()?);
    r.skip(2)?;
    let offset = usize::try_from(r.u32()?).map_err(|_| SmbError::Malformed)?;
    buffer_at(message, offset, len)
}

/// The `AV_PAIR`s of `target_info`, which `MsvAvEOL` must end.
pub(crate) fn av_pairs(target_info: &[u8]) -> Result<Vec<(u16, &[u8])>, SmbError> {
    let mut pairs = Vec::new();
    let mut r = Reader::new(target_info);
    loop {
        let id = r.u16()?;
        let len = usize::from(r.u16()?);
        let value = r.bytes(len)?;
        if id == av::EOL {
            if len != 0 || r.remaining() != 0 {
                return Err(SmbError::Malformed);
            }
            return Ok(pairs);
        }
        pairs.try_reserve(1).map_err(|_| SmbError::OutOfMemory)?;
        pairs.push((id, value));
    }
}

/// The established exchange: the session key and each direction's signing
/// key and sequence, wiped on drop.
struct Keys {
    session: NtlmKey,
    client_signing: NtlmKey,
    server_signing: NtlmKey,
    client_sequence: u32,
    server_sequence: u32,
}

impl Drop for Keys {
    fn drop(&mut self) {
        self.session.zeroize();
        self.client_signing.zeroize();
        self.server_signing.zeroize();
    }
}

/// One NTLMv2 exchange, client side.
pub struct NtlmInitiator {
    credential: NtlmCredential,
    target_name: String,
    inputs: NtlmInputs,
    negotiate: Vec<u8>,
    answered: bool,
    keys: Option<Keys>,
}

impl core::fmt::Debug for NtlmInitiator {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("NtlmInitiator")
            .field("credential", &self.credential)
            .field("target_name", &self.target_name)
            .field("established", &self.keys.is_some())
            .finish_non_exhaustive()
    }
}

impl NtlmInitiator {
    /// An exchange authenticating `credential` to the service
    /// `target_name` — `cifs/<host>` for a file server — whose name the
    /// response binds, so it cannot be relayed to another.
    ///
    /// # Errors
    ///
    /// [`SmbError::TooLarge`] for a target name past the bound,
    /// [`SmbError::OutOfMemory`].
    pub fn new(
        credential: NtlmCredential,
        target_name: &str,
        inputs: NtlmInputs,
    ) -> Result<Self, SmbError> {
        Ok(Self {
            credential,
            target_name: bounded_name(target_name)?,
            inputs,
            negotiate: Vec::new(),
            answered: false,
            keys: None,
        })
    }

    /// The `NEGOTIATE_MESSAGE` that opens the exchange.
    ///
    /// # Errors
    ///
    /// [`SmbError::Protocol`] once it has been made; [`SmbError::OutOfMemory`].
    pub fn negotiate(&mut self) -> Result<Vec<u8>, SmbError> {
        if !self.negotiate.is_empty() {
            return Err(SmbError::Protocol);
        }
        let mut enc = Encoder::new();
        enc.bytes(&SIGNATURE);
        enc.u32(NEGOTIATE);
        enc.u32(NEGOTIATE_FLAGS);
        enc.zeros(16);
        enc.bytes(&VERSION);
        self.negotiate = enc.finish()?;
        owned(&self.negotiate)
    }

    /// Answer the server's `CHALLENGE_MESSAGE` with the
    /// `AUTHENTICATE_MESSAGE`, which completes the exchange. The
    /// password-derived key is wiped once the response is made.
    ///
    /// # Errors
    ///
    /// [`SmbError::Refused`] with [`Refusal::Ntlm`] when the server will not
    /// agree to NTLMv2's flags; [`SmbError::Malformed`] for a malformed
    /// challenge; [`SmbError::Protocol`] out of turn;
    /// [`SmbError::TimeOutOfRange`] when the server sent no time and the
    /// host's cannot be carried.
    pub fn authenticate(&mut self, challenge_message: &[u8]) -> Result<Vec<u8>, SmbError> {
        if self.negotiate.is_empty() || self.answered {
            return Err(SmbError::Protocol);
        }
        let challenge = Challenge::decode(challenge_message)?;
        if challenge.flags & REQUIRED != REQUIRED {
            return Err(SmbError::Refused(Refusal::Ntlm));
        }
        let time = match challenge.timestamp {
            Some(stamp) => stamp,
            None => filetime_floor(self.inputs.now)?.to_le_bytes(),
        };
        let target_info = self.target_info(&challenge)?;
        self.answered = true;
        let response = ntlmv2_response(
            &self.credential.response_key,
            challenge.server_challenge,
            time,
            self.inputs.client_challenge,
            &target_info,
        );
        self.credential.response_key.zeroize();
        let (response, mut session) = response?;
        let message = self.authenticate_message(challenge.flags & ACCEPTED, &response);
        let keys = Keys {
            session,
            client_signing: ntlm_signing_key(&session, NtlmDirection::ClientToServer),
            server_signing: ntlm_signing_key(&session, NtlmDirection::ServerToClient),
            client_sequence: 0,
            server_sequence: 0,
        };
        session.zeroize();
        let mut message = message?;
        let mic = message_integrity(&keys.session, &self.negotiate, challenge_message, &message)?;
        message[MIC].copy_from_slice(&mic);
        self.keys = Some(keys);
        Ok(message)
    }

    /// The server's target information as the client answers it: the
    /// server's pairs, then `MsvAvFlags` announcing the MIC, null channel
    /// bindings, and the service the client means to reach.
    fn target_info(&self, challenge: &Challenge<'_>) -> Result<Vec<u8>, SmbError> {
        let mut enc = Encoder::new();
        for &(id, value) in &challenge.pairs {
            if matches!(id, av::FLAGS | av::TARGET_NAME | av::CHANNEL_BINDINGS) {
                continue;
            }
            enc.u16(id);
            let len = enc.u16_of(value.len());
            enc.u16(len);
            enc.bytes(value);
        }
        enc.u16(av::FLAGS);
        enc.u16(4);
        enc.u32(challenge.av_flags.unwrap_or(0) | AV_FLAG_MIC);
        enc.u16(av::CHANNEL_BINDINGS);
        enc.u16(16);
        enc.zeros(16);
        enc.u16(av::TARGET_NAME);
        let len = enc.u16_of(utf16le_len(&self.target_name));
        enc.u16(len);
        utf16le(&self.target_name, &mut enc);
        enc.u16(av::EOL);
        enc.u16(0);
        enc.finish()
    }

    /// The `AUTHENTICATE_MESSAGE`, its MIC still zero.
    fn authenticate_message(&self, flags: u32, nt_response: &[u8]) -> Result<Vec<u8>, SmbError> {
        let mut payload = Encoder::new();
        utf16le(&self.credential.domain, &mut payload);
        let user = payload.position();
        utf16le(&self.credential.user, &mut payload);
        let lm = payload.position();
        payload.zeros(24);
        let nt = payload.position();
        payload.bytes(nt_response);
        let end = payload.position();
        let payload = payload.finish()?;

        let mut enc = Encoder::new();
        enc.bytes(&SIGNATURE);
        enc.u32(AUTHENTICATE);
        let field = |enc: &mut Encoder, from: usize, to: usize| {
            let len = enc.u16_of(to - from);
            enc.u16(len);
            enc.u16(len);
            let offset = enc.u32_of(AUTHENTICATE_FIXED + from);
            enc.u32(offset);
        };
        // The LMv2 response is zeros: NTLMv2's own response carries the time.
        field(&mut enc, lm, nt);
        field(&mut enc, nt, end);
        field(&mut enc, 0, user);
        field(&mut enc, user, lm);
        // No workstation name, and no exchanged key.
        field(&mut enc, lm, lm);
        field(&mut enc, end, end);
        enc.u32(flags);
        if flags & flag::VERSION == 0 {
            enc.zeros(VERSION.len());
        } else {
            enc.bytes(&VERSION);
        }
        enc.zeros(MIC.len());
        enc.bytes(&payload);
        enc.finish()
    }

    /// The session key the exchange established, which SMB keys from.
    #[must_use]
    pub fn session_key(&self) -> Option<&NtlmKey> {
        self.keys.as_ref().map(|keys| &keys.session)
    }

    /// Sign `message` as `GSS_GetMIC` does, at the next client sequence
    /// number.
    ///
    /// # Errors
    ///
    /// [`SmbError::Protocol`] before the exchange completes or once the
    /// sequence is spent.
    pub fn sign(&mut self, message: &[u8]) -> Result<[u8; SIGNATURE_LEN], SmbError> {
        let keys = self.keys.as_mut().ok_or(SmbError::Protocol)?;
        let sequence = keys.client_sequence;
        keys.client_sequence = sequence.checked_add(1).ok_or(SmbError::Protocol)?;
        let checksum = ntlm_checksum(&keys.client_signing, sequence, message);
        let mut signature = [0u8; SIGNATURE_LEN];
        signature[..4].copy_from_slice(&1u32.to_le_bytes());
        signature[4..12].copy_from_slice(&checksum);
        signature[12..].copy_from_slice(&sequence.to_le_bytes());
        Ok(signature)
    }

    /// Check the server's `GSS_GetMIC` signature over `message`, at the next
    /// server sequence number.
    ///
    /// # Errors
    ///
    /// [`SmbError::Integrity`] when it does not verify, [`SmbError::Protocol`]
    /// before the exchange completes.
    pub fn verify(&mut self, message: &[u8], signature: &[u8]) -> Result<(), SmbError> {
        let keys = self.keys.as_mut().ok_or(SmbError::Protocol)?;
        let sequence = keys.server_sequence;
        let Ok(signature) = <&[u8; SIGNATURE_LEN]>::try_from(signature) else {
            return Err(SmbError::Integrity);
        };
        let mut checksum = [0u8; 8];
        checksum.copy_from_slice(&signature[4..12]);
        if signature[..4] != 1u32.to_le_bytes()
            || signature[12..] != sequence.to_le_bytes()
            || !ntlm_checksum_verify(&keys.server_signing, sequence, message, &checksum)
        {
            return Err(SmbError::Integrity);
        }
        keys.server_sequence = sequence.checked_add(1).ok_or(SmbError::Protocol)?;
        Ok(())
    }
}

/// The MIC (MS-NLMP §3.1.5.1.2): HMAC-MD5 under the exported session key over
/// the three messages, the `AUTHENTICATE_MESSAGE`'s own MIC field read as
/// zeros.
pub(crate) fn message_integrity(
    session_key: &NtlmKey,
    negotiate: &[u8],
    challenge: &[u8],
    authenticate: &[u8],
) -> Result<NtlmKey, SmbError> {
    let head = authenticate.get(..MIC.start).ok_or(SmbError::Malformed)?;
    let tail = authenticate.get(MIC.end..).ok_or(SmbError::Malformed)?;
    Ok(ntlm_hmac(
        session_key,
        &[negotiate, challenge, head, &[0; MIC.end - MIC.start], tail],
    ))
}

/// The NTLMv2 response (MS-NLMP §3.3.2) and the session base key it yields:
/// `NTProofStr ‖ temp`, where `temp` carries the time, the client challenge
/// and `target_info`, and the proof is keyed by the response key over the
/// server's challenge and `temp`.
fn ntlmv2_response(
    response_key: &NtlmKey,
    server_challenge: [u8; 8],
    time: [u8; 8],
    client_challenge: [u8; 8],
    target_info: &[u8],
) -> Result<(Vec<u8>, NtlmKey), SmbError> {
    let mut response = Encoder::new();
    response.zeros(16);
    response.bytes(&[1, 1]);
    response.zeros(6);
    response.bytes(&time);
    response.bytes(&client_challenge);
    response.zeros(4);
    response.bytes(target_info);
    response.zeros(4);
    let mut response = response.finish()?;
    let proof = ntlm_hmac(response_key, &[&server_challenge, &response[16..]]);
    response[..16].copy_from_slice(&proof);
    Ok((response, ntlm_hmac(response_key, &[&proof])))
}

#[cfg(test)]
#[path = "ntlm_tests.rs"]
mod tests;
