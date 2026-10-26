//! SMB 3's cryptography (MS-SMB2 §3.1.4): the pre-auth integrity hash, key
//! derivation, signing, and the encryption transform. Every primitive is
//! `lib/crypto`'s.

use alloc::vec::Vec;
use core::ops::{Range, RangeFrom};

use tairix_crypto::{
    aes128_cmac, aes128_gmac, kdf_counter_sha256, Aes128Ccm, Aes128Gcm, Aes256Ccm, Aes256Gcm,
    Sha512Stream, AES_CCM_NONCE_LEN, AES_GCM_NONCE_LEN,
};
use zeroize::Zeroize;

use crate::header::{Command, Flags, Header, FLAGS_RANGE, SIGNATURE_RANGE};
use crate::negotiate::{Cipher, Dialect, SigningAlgorithm};
use crate::wire::Reader;
use crate::{Role, SmbError};

/// The pre-auth integrity hash (3.1.1): SHA-512 chained over every message
/// of a negotiation and a session's setup, binding the keys to exactly the
/// bytes exchanged.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreauthHash([u8; 64]);

impl PreauthHash {
    /// The hash before any message: all zeros.
    #[must_use]
    pub const fn new() -> Self {
        Self([0; 64])
    }

    /// Chain `message`: `H = SHA-512(H ‖ message)`.
    pub fn update(&mut self, message: &[u8]) {
        let mut stream = Sha512Stream::new();
        stream.update(&self.0);
        stream.update(message);
        self.0 = stream.finalize();
    }

    /// The current value.
    #[must_use]
    pub const fn value(&self) -> &[u8; 64] {
        &self.0
    }
}

impl Default for PreauthHash {
    fn default() -> Self {
        Self::new()
    }
}

/// One direction's sealing key, expanded once.
enum Seal {
    Ccm128(Aes128Ccm),
    Ccm256(Aes256Ccm),
    Gcm128(Aes128Gcm),
    Gcm256(Aes256Gcm),
}

impl Seal {
    fn new(cipher: Cipher, key: &[u8; 32]) -> Self {
        let short = |key: &[u8; 32]| -> [u8; 16] {
            let mut out = [0u8; 16];
            out.copy_from_slice(&key[..16]);
            out
        };
        match cipher {
            Cipher::Aes128Ccm => {
                let mut k = short(key);
                let seal = Self::Ccm128(Aes128Ccm::new(&k));
                k.zeroize();
                seal
            }
            Cipher::Aes128Gcm => {
                let mut k = short(key);
                let seal = Self::Gcm128(Aes128Gcm::new(&k));
                k.zeroize();
                seal
            }
            Cipher::Aes256Ccm => Self::Ccm256(Aes256Ccm::new(key)),
            Cipher::Aes256Gcm => Self::Gcm256(Aes256Gcm::new(key)),
        }
    }

    fn seal(&self, nonce: &[u8; 16], aad: &[u8], buf: &mut [u8]) -> Result<[u8; 16], SmbError> {
        let result = match self {
            Self::Ccm128(c) => c.seal(&ccm_nonce(nonce), aad, buf),
            Self::Ccm256(c) => c.seal(&ccm_nonce(nonce), aad, buf),
            Self::Gcm128(c) => c.seal(&gcm_nonce(nonce), aad, buf),
            Self::Gcm256(c) => c.seal(&gcm_nonce(nonce), aad, buf),
        };
        result.map_err(|_| SmbError::TooLarge)
    }

    fn open(&self, nonce: &[u8; 16], aad: &[u8], buf: &mut [u8], tag: &[u8; 16]) -> bool {
        match self {
            Self::Ccm128(c) => c.open(&ccm_nonce(nonce), aad, buf, tag).is_ok(),
            Self::Ccm256(c) => c.open(&ccm_nonce(nonce), aad, buf, tag).is_ok(),
            Self::Gcm128(c) => c.open(&gcm_nonce(nonce), aad, buf, tag).is_ok(),
            Self::Gcm256(c) => c.open(&gcm_nonce(nonce), aad, buf, tag).is_ok(),
        }
    }

    const fn nonce_len(&self) -> usize {
        match self {
            Self::Ccm128(_) | Self::Ccm256(_) => AES_CCM_NONCE_LEN,
            Self::Gcm128(_) | Self::Gcm256(_) => AES_GCM_NONCE_LEN,
        }
    }
}

fn ccm_nonce(nonce: &[u8; 16]) -> [u8; AES_CCM_NONCE_LEN] {
    let mut out = [0u8; AES_CCM_NONCE_LEN];
    out.copy_from_slice(&nonce[..AES_CCM_NONCE_LEN]);
    out
}

fn gcm_nonce(nonce: &[u8; 16]) -> [u8; AES_GCM_NONCE_LEN] {
    let mut out = [0u8; AES_GCM_NONCE_LEN];
    out.copy_from_slice(&nonce[..AES_GCM_NONCE_LEN]);
    out
}

/// The label and context each key is derived under (MS-SMB2 §3.2.5.3.1): signing,
/// client-to-server, server-to-client, then application. 3.1.1 binds every
/// one to the pre-auth integrity hash.
fn labels(dialect: Dialect, preauth: &[u8; 64]) -> [(&'static [u8], &[u8]); 4] {
    match dialect {
        Dialect::Smb300 | Dialect::Smb302 => [
            (b"SMB2AESCMAC\0", b"SmbSign\0"),
            (b"SMB2AESCCM\0", b"ServerIn \0"),
            (b"SMB2AESCCM\0", b"ServerOut\0"),
            (b"SMB2APP\0", b"SmbRpc\0"),
        ],
        Dialect::Smb311 => [
            (b"SMBSigningKey\0", preauth),
            (b"SMBC2SCipherKey\0", preauth),
            (b"SMBS2CCipherKey\0", preauth),
            (b"SMBAppKey\0", preauth),
        ],
    }
}

/// The bytes of a sealing nonce that count: its first eight.
const COUNTER_LEN: usize = 8;

/// How many random bytes seed one direction's sealing nonces: where the
/// counter starts, then the bytes that follow it in every nonce — CCM's
/// shorter nonce keeps three of them.
pub const NONCE_SEED_LEN: usize = AES_GCM_NONCE_LEN;

const _: () = assert!(AES_CCM_NONCE_LEN > COUNTER_LEN && AES_CCM_NONCE_LEN <= NONCE_SEED_LEN);

/// The nonces one direction seals under. The counter never repeats within
/// the sequence; the random start and the random bytes after it keep two
/// sequences apart should one key ever be derived twice.
struct Nonces {
    template: [u8; 16],
    start: u64,
    used: u64,
}

impl Nonces {
    fn new(seed: &[u8; NONCE_SEED_LEN], nonce_len: usize) -> Self {
        let mut template = [0u8; 16];
        template[COUNTER_LEN..nonce_len].copy_from_slice(&seed[COUNTER_LEN..nonce_len]);
        let mut start = [0u8; COUNTER_LEN];
        start.copy_from_slice(&seed[..COUNTER_LEN]);
        Self {
            template,
            start: u64::from_le_bytes(start),
            used: 0,
        }
    }

    /// The next nonce, as the transform header's sixteen-byte field. A
    /// sequence that would repeat ends instead.
    fn next(&mut self) -> Result<[u8; 16], SmbError> {
        let index = self.used;
        self.used = index.checked_add(1).ok_or(SmbError::TooLarge)?;
        let mut nonce = self.template;
        nonce[..COUNTER_LEN].copy_from_slice(&self.start.wrapping_add(index).to_le_bytes());
        Ok(nonce)
    }
}

/// The keys one session derives from its session key (MS-SMB2 §3.2.5.3.1), for one
/// role: what it signs and seals with, what it opens with, and the nonce
/// sequence it seals under.
pub struct SessionKeys {
    signing: [u8; 16],
    signing_algorithm: SigningAlgorithm,
    application: [u8; 16],
    outbound: Seal,
    inbound: Seal,
    nonces: Nonces,
}

impl core::fmt::Debug for SessionKeys {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // No key is ever printed.
        f.debug_struct("SessionKeys").finish_non_exhaustive()
    }
}

impl Drop for SessionKeys {
    fn drop(&mut self) {
        self.signing.zeroize();
        self.application.zeroize();
    }
}

/// What a session's keys are derived from.
pub struct KeyInputs<'a> {
    /// The dialect negotiated.
    pub dialect: Dialect,
    /// The cipher negotiated.
    pub cipher: Cipher,
    /// The signing algorithm negotiated.
    pub signing: SigningAlgorithm,
    /// The whole key the authentication produced.
    pub session_key: &'a [u8],
    /// The session's pre-auth integrity hash, for 3.1.1.
    pub preauth: &'a PreauthHash,
}

/// A session's four keys as bytes, wiped on drop.
struct Derived {
    signing: [u8; 16],
    application: [u8; 16],
    /// Client to server; only the first sixteen bytes for a 128-bit cipher.
    c2s: [u8; 32],
    /// Server to client, likewise.
    s2c: [u8; 32],
}

impl Drop for Derived {
    fn drop(&mut self) {
        self.signing.zeroize();
        self.application.zeroize();
        self.c2s.zeroize();
        self.s2c.zeroize();
    }
}

/// Derive a session's four keys (MS-SMB2 §3.2.5.3.1). The session key is its first
/// sixteen bytes, zero-padded; an AES-256 cipher on 3.1.1 keys from the
/// whole of it.
fn derive_keys(inputs: &KeyInputs<'_>) -> Result<Derived, SmbError> {
    if inputs.session_key.is_empty() {
        return Err(SmbError::Protocol);
    }
    let mut short = [0u8; 16];
    let take = inputs.session_key.len().min(16);
    short[..take].copy_from_slice(&inputs.session_key[..take]);
    let wide = inputs.dialect == Dialect::Smb311 && inputs.cipher.is_256();
    let cipher_len = if wide { 32 } else { 16 };

    let [sign, c2s, s2c, app] = labels(inputs.dialect, inputs.preauth.value());
    let mut keys = Derived {
        signing: [0; 16],
        application: [0; 16],
        c2s: [0; 32],
        s2c: [0; 32],
    };
    let cipher_key: &[u8] = if wide { inputs.session_key } else { &short };
    let derived = kdf_counter_sha256(&short, sign.0, sign.1, &mut keys.signing)
        .and_then(|()| kdf_counter_sha256(&short, app.0, app.1, &mut keys.application))
        .and_then(|()| kdf_counter_sha256(cipher_key, c2s.0, c2s.1, &mut keys.c2s[..cipher_len]))
        .and_then(|()| kdf_counter_sha256(cipher_key, s2c.0, s2c.1, &mut keys.s2c[..cipher_len]));
    short.zeroize();
    derived.map_err(|_| SmbError::Protocol)?;
    Ok(keys)
}

impl SessionKeys {
    /// Derive a session's keys for `role`, sealing under nonces seeded from
    /// `nonce_seed`, which must be fresh CSPRNG output.
    ///
    /// # Errors
    ///
    /// [`SmbError::Protocol`] for an empty session key, which no completed
    /// authentication produces.
    pub fn derive(
        inputs: &KeyInputs<'_>,
        role: Role,
        nonce_seed: &[u8; NONCE_SEED_LEN],
    ) -> Result<Self, SmbError> {
        let keys = derive_keys(inputs)?;
        let (out_key, in_key) = match role {
            Role::Client => (&keys.c2s, &keys.s2c),
            Role::Server => (&keys.s2c, &keys.c2s),
        };
        let outbound = Seal::new(inputs.cipher, out_key);
        let nonces = Nonces::new(nonce_seed, outbound.nonce_len());
        Ok(Self {
            signing: keys.signing,
            signing_algorithm: inputs.signing,
            application: keys.application,
            outbound,
            inbound: Seal::new(inputs.cipher, in_key),
            nonces,
        })
    }

    /// The application key (`SMB2APP` / `SMBAppKey`), which DCE/RPC over the
    /// session's pipes may key from.
    #[must_use]
    pub const fn application_key(&self) -> &[u8; 16] {
        &self.application
    }

    /// Sign `message` in place: set [`Flags::SIGNED`] and write the
    /// signature over the message with the signature field zeroed.
    ///
    /// # Errors
    ///
    /// [`SmbError::Malformed`] when `message` has no header.
    pub fn sign(&self, message: &mut [u8]) -> Result<(), SmbError> {
        let flags = message.get_mut(FLAGS_RANGE).ok_or(SmbError::Malformed)?;
        let wire =
            u32::from_le_bytes([flags[0], flags[1], flags[2], flags[3]]) | Flags::SIGNED.bits();
        flags.copy_from_slice(&wire.to_le_bytes());
        let header = Header::decode(message)?;
        message[SIGNATURE_RANGE].fill(0);
        let tag = self.signature(&header, message)?;
        message[SIGNATURE_RANGE].copy_from_slice(&tag);
        Ok(())
    }

    /// Check `message`'s signature. A message whose [`Flags::SIGNED`] is
    /// clear does not verify.
    ///
    /// # Errors
    ///
    /// [`SmbError::Integrity`] when it does not verify,
    /// [`SmbError::OutOfMemory`] when the scratch copy cannot be made.
    pub fn verify(&self, message: &[u8]) -> Result<(), SmbError> {
        let header = Header::decode(message)?;
        if !header.flags.contains(Flags::SIGNED) {
            return Err(SmbError::Integrity);
        }
        let mut scratch = Vec::new();
        scratch
            .try_reserve(message.len())
            .map_err(|_| SmbError::OutOfMemory)?;
        scratch.extend_from_slice(message);
        scratch[SIGNATURE_RANGE].fill(0);
        let expected = self.signature(&header, &scratch)?;
        if tairix_crypto::ct_eq(&expected, &header.signature) {
            Ok(())
        } else {
            Err(SmbError::Integrity)
        }
    }

    fn signature(&self, header: &Header, zeroed: &[u8]) -> Result<[u8; 16], SmbError> {
        match self.signing_algorithm {
            SigningAlgorithm::AesCmac => Ok(aes128_cmac(&self.signing, zeroed)),
            SigningAlgorithm::AesGmac => {
                // MS-SMB2 §3.1.4.1: the message id, then the response and
                // cancel bits.
                let mut nonce = [0u8; 12];
                nonce[..8].copy_from_slice(&header.message_id.to_le_bytes());
                let role = u32::from(header.flags.contains(Flags::SERVER_TO_REDIR));
                let cancel = u32::from(header.command == Command::Cancel) << 1;
                nonce[8..].copy_from_slice(&(role | cancel).to_le_bytes());
                aes128_gmac(&self.signing, &nonce, zeroed).ok_or(SmbError::TooLarge)
            }
        }
    }

    /// Seal the message or compound in `frame[TRANSFORM_HEADER_LEN..]` in
    /// place for `session_id`, writing the transform header in front of it
    /// under the next nonce of these keys' sequence.
    ///
    /// # Errors
    ///
    /// [`SmbError::Malformed`] when `frame` holds no message after the
    /// header's room, [`SmbError::TooLarge`] past what the transform or the
    /// cipher carries.
    pub fn seal(&mut self, session_id: u64, frame: &mut [u8]) -> Result<(), SmbError> {
        let size = frame
            .len()
            .checked_sub(TRANSFORM_HEADER_LEN)
            .filter(|&size| size > 0)
            .ok_or(SmbError::Malformed)?;
        let size = u32::try_from(size).map_err(|_| SmbError::TooLarge)?;
        let nonce = self.nonces.next()?;
        let (header, body) = frame.split_at_mut(TRANSFORM_HEADER_LEN);
        header[..4].copy_from_slice(&TRANSFORM_PROTOCOL_ID);
        header[TRANSFORM_NONCE].copy_from_slice(&nonce);
        header[TRANSFORM_SIZE].copy_from_slice(&size.to_le_bytes());
        header[TRANSFORM_RESERVED].fill(0);
        header[TRANSFORM_FLAGS].copy_from_slice(&TRANSFORM_ENCRYPTED.to_le_bytes());
        header[TRANSFORM_SESSION].copy_from_slice(&session_id.to_le_bytes());
        let tag = self.outbound.seal(&nonce, &header[TRANSFORM_AAD], body)?;
        header[TRANSFORM_TAG].copy_from_slice(&tag);
        Ok(())
    }

    /// Open a transform frame in place, returning the plaintext it carried:
    /// `frame[TRANSFORM_HEADER_LEN..]`.
    ///
    /// # Errors
    ///
    /// [`SmbError::Integrity`] when the tag does not verify,
    /// [`SmbError::Malformed`] for a frame that is not a transform.
    pub fn open<'f>(&self, frame: &'f mut [u8]) -> Result<&'f [u8], SmbError> {
        let transform = Transform::parse(frame)?;
        let (header, body) = frame.split_at_mut(TRANSFORM_HEADER_LEN);
        if self.inbound.open(
            &transform.nonce,
            &header[TRANSFORM_AAD],
            body,
            &transform.tag,
        ) {
            Ok(body)
        } else {
            Err(SmbError::Integrity)
        }
    }
}

/// The four bytes a transform frame begins with.
pub const TRANSFORM_PROTOCOL_ID: [u8; 4] = [0xfd, b'S', b'M', b'B'];

/// A transform header's length.
pub const TRANSFORM_HEADER_LEN: usize = 52;

const TRANSFORM_TAG: Range<usize> = 4..20;
const TRANSFORM_NONCE: Range<usize> = 20..36;
const TRANSFORM_SIZE: Range<usize> = 36..40;
const TRANSFORM_RESERVED: Range<usize> = 40..42;
const TRANSFORM_FLAGS: Range<usize> = 42..44;
const TRANSFORM_SESSION: Range<usize> = 44..52;

/// What the tag authenticates besides the message: the header from the nonce
/// on.
const TRANSFORM_AAD: RangeFrom<usize> = TRANSFORM_NONCE.start..;

/// The transform header's `Flags`: encrypted (3.1.1's flag, and 3.0's
/// AES-128-CCM algorithm id, which have the same value).
const TRANSFORM_ENCRYPTED: u16 = 0x0001;

/// A transform header's fields.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Transform {
    /// The AEAD tag.
    pub tag: [u8; 16],
    /// The nonce field.
    pub nonce: [u8; 16],
    /// The session whose keys sealed the frame.
    pub session_id: u64,
}

impl Transform {
    /// The transform header at the start of `frame`, checked against the
    /// length that follows it.
    ///
    /// # Errors
    ///
    /// [`SmbError::Malformed`] for a frame that is not a well-formed
    /// transform.
    pub fn parse(frame: &[u8]) -> Result<Self, SmbError> {
        let header = frame
            .get(..TRANSFORM_HEADER_LEN)
            .ok_or(SmbError::Malformed)?;
        let field = |range: Range<usize>| Reader::new(&header[range]);
        if header[..4] != TRANSFORM_PROTOCOL_ID
            || field(TRANSFORM_FLAGS).u16()? != TRANSFORM_ENCRYPTED
        {
            return Err(SmbError::Malformed);
        }
        let size =
            usize::try_from(field(TRANSFORM_SIZE).u32()?).map_err(|_| SmbError::Malformed)?;
        if size == 0 || frame.len() - TRANSFORM_HEADER_LEN != size {
            return Err(SmbError::Malformed);
        }
        Ok(Self {
            tag: field(TRANSFORM_TAG).array()?,
            nonce: field(TRANSFORM_NONCE).array()?,
            session_id: field(TRANSFORM_SESSION).u64()?,
        })
    }
}

#[cfg(test)]
#[path = "crypt_tests.rs"]
mod tests;
