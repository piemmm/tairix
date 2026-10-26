//! NTLMv2's constructions (MS-NLMP §3.3.2, §3.4.4, §3.4.5) — the only place
//! TAIRiX computes MD4 or MD5.
//!
//! NTLM is a foreign protocol TAIRiX cannot choose: a workgroup SMB server
//! authenticates no other way. Its digests are broken — MD4 outright, MD5 for
//! collisions — so neither is offered as a digest. This module exposes the
//! protocol's constructions alone, each over the input the protocol fixes, so
//! nothing outside NTLM can reach for them. HMAC-MD5 is offered as NTLM's
//! keyed function: as a MAC and PRF it is weakened rather than broken, which
//! is what NTLMv2's security rests on.
//!
//! The constructions are the extended-session-security forms without key
//! exchange, the only ones TAIRiX negotiates: no checksum is RC4-sealed and no
//! session key is RC4-wrapped.

use hmac::{Hmac, KeyInit as _, Mac};
use md4::{Digest as _, Md4};
use md5::Md5;
use zeroize::Zeroize;

use crate::constant_time::ct_eq;

/// Length, in bytes, of every NTLM key, proof and HMAC-MD5 output.
pub const NTLM_KEY_LEN: usize = 16;

/// Length, in bytes, of an NTLMSSP message signature's checksum.
pub const NTLM_CHECKSUM_LEN: usize = 8;

/// An NTLM key, proof or HMAC-MD5 output as raw bytes.
pub type NtlmKey = [u8; NTLM_KEY_LEN];

/// An NTLMSSP message signature's checksum as raw bytes.
pub type NtlmChecksum = [u8; NTLM_CHECKSUM_LEN];

/// HMAC-MD5 under `key` over the concatenation of `parts`.
#[must_use]
pub fn ntlm_hmac(key: &NtlmKey, parts: &[&[u8]]) -> NtlmKey {
    hmac_md5(key, parts)
}

/// Verify that `expected` is [`ntlm_hmac`] of `parts` under `key`, in
/// constant time with respect to its contents.
#[must_use]
pub fn ntlm_hmac_verify(key: &NtlmKey, parts: &[&[u8]], expected: &NtlmKey) -> bool {
    ct_eq(&hmac_md5(key, parts), expected)
}

fn hmac_md5(key: &[u8], parts: &[&[u8]]) -> NtlmKey {
    // SAFETY-INVARIANT: HMAC accepts a key of any length, so construction
    // from a slice can never return `InvalidLength`.
    let mut mac = Hmac::<Md5>::new_from_slice(key).expect("HMAC accepts any key length");
    for part in parts {
        mac.update(part);
    }
    mac.finalize().into_bytes().into()
}

/// NTOWFv2: `HMAC-MD5(MD4(password), identity)`, the NTLMv2 response key.
///
/// `password_utf16le` is the password as UTF-16LE. `identity_utf16le` is
/// `UNICODE(Uppercase(user) ‖ domain)`, whose case mapping and encoding are
/// the protocol layer's. The NT hash between the two steps is wiped before
/// returning: it is a password equivalent.
#[must_use]
pub fn ntowfv2(password_utf16le: &[u8], identity_utf16le: &[u8]) -> NtlmKey {
    let mut nt_hash: NtlmKey = Md4::digest(password_utf16le).into();
    let key = hmac_md5(&nt_hash, &[identity_utf16le]);
    nt_hash.zeroize();
    key
}

/// Which way an NTLMSSP signature travels.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum NtlmDirection {
    /// Signed by the client, verified by the server.
    ClientToServer,
    /// Signed by the server, verified by the client.
    ServerToClient,
}

impl NtlmDirection {
    const fn magic(self) -> &'static [u8] {
        match self {
            Self::ClientToServer => b"session key to client-to-server signing key magic constant\0",
            Self::ServerToClient => b"session key to server-to-client signing key magic constant\0",
        }
    }
}

/// SIGNKEY (MS-NLMP §3.4.5.2): `MD5(exported session key ‖ magic)` for
/// `direction`.
#[must_use]
pub fn ntlm_signing_key(exported_session_key: &NtlmKey, direction: NtlmDirection) -> NtlmKey {
    let mut digest = Md5::new();
    digest.update(exported_session_key);
    digest.update(direction.magic());
    digest.finalize().into()
}

/// The checksum of an NTLMSSP message signature (MS-NLMP §3.4.4.2):
/// `HMAC-MD5(signing key, sequence ‖ message)[..8]`.
#[must_use]
pub fn ntlm_checksum(signing_key: &NtlmKey, sequence: u32, message: &[u8]) -> NtlmChecksum {
    let mut full = hmac_md5(signing_key, &[&sequence.to_le_bytes(), message]);
    let mut checksum = [0u8; NTLM_CHECKSUM_LEN];
    checksum.copy_from_slice(&full[..NTLM_CHECKSUM_LEN]);
    full.zeroize();
    checksum
}

/// Verify that `expected` is [`ntlm_checksum`] of `message` at `sequence`,
/// in constant time with respect to its contents.
#[must_use]
pub fn ntlm_checksum_verify(
    signing_key: &NtlmKey,
    sequence: u32,
    message: &[u8],
    expected: &NtlmChecksum,
) -> bool {
    ct_eq(&ntlm_checksum(signing_key, sequence, message), expected)
}

#[cfg(test)]
#[path = "ntlm_tests.rs"]
mod tests;
