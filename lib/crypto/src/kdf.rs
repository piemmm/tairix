//! Key derivation.
//!
//! TAIRiX derives subkeys with HMAC-SHA256 used as a pseudo-random function,
//! the single-block case of HKDF-Expand (RFC 5869): a 256-bit secret keys the
//! MAC and a caller-chosen, domain-separating `context` string is the message.
//! The 256-bit output is itself a 256-bit key, so no expansion past one block
//! is ever required and the construction stays a thin wrapper over the audited
//! [`crate::mac`] primitive rather than a hand-rolled KDF.
//!
//! This is what `ARXFS` uses to grow its per-volume key hierarchy
//! (`docs/src/filesystem/arxfs-spec.md` §7): one master key derives the
//! metadata-authentication, filename, and content keys, each under a distinct
//! `context`, so a derived key never collides with another use of the master.
//!
//! For **password** material TAIRiX uses PBKDF2-HMAC-SHA256 (RFC 8018 §5.2):
//! a deliberately slow, salted derivation that makes offline guessing of a
//! stolen `/System/Security/Users` record expensive ([`pbkdf2_sha256`]). It
//! is a standard *construction* over the same audited HMAC primitive — the
//! same shape as `tairix-rng`'s HMAC-DRBG — never a hand-rolled primitive. Verification goes through [`crate::ct_eq`]
//! ([`pbkdf2_sha256_verify`]) so a stored hash comparison cannot leak through
//! timing. PBKDF2-HMAC-SHA1 ([`pbkdf2_sha1`]) is the same construction for one
//! foreign format: the RFC 3962 Kerberos string-to-key.
//!
//! The NIST SP 800-108 KDF in counter mode ([`kdf_counter_sha256`],
//! [`kdf_counter_sha384`]) is how SMB 3 derives its signing and encryption
//! keys from a session key (MS-SMB2 §3.1.4.2) and how the RFC 8009 Kerberos
//! enctypes derive theirs.

use core::fmt;
use core::num::NonZeroU32;

use hmac::digest::OutputSizeUser;
use hmac::{EagerHash, Hmac, KeyInit as _, Mac};
use sha1::Sha1;
use sha2::{Sha256, Sha384};
use zeroize::Zeroize;

use crate::constant_time::ct_eq;
use crate::mac::{hmac_sha256, HmacSha256Key};

/// Length, in bytes, of a derived key. Matches both [`crate::mac::HMAC_SHA256_KEY_LEN`]
/// and [`crate::aead::AEAD_KEY_LEN`], so a derived key drops straight into
/// either primitive without truncation or expansion.
pub const DERIVED_KEY_LEN: usize = 32;

/// A 256-bit derived key as raw bytes.
pub type DerivedKey = [u8; DERIVED_KEY_LEN];

/// Derive a 256-bit subkey from a 256-bit `secret` and a domain-separating
/// `context`.
///
/// Computes `HMAC-SHA256(secret, context)` — the single-block HKDF-Expand
/// case (RFC 5869) — through the audited [`crate::mac`] wrapper. Distinct
/// `context` values yield independent keys from the same `secret`, so callers
/// must give each derived key its own stable, unique context label.
///
/// The output is uniformly random under the PRF assumption on HMAC-SHA256 and
/// reveals nothing about `secret`.
#[must_use]
pub fn derive_key(secret: &HmacSha256Key, context: &[u8]) -> DerivedKey {
    hmac_sha256(secret, context)
}

/// Length, in bytes, of a PBKDF2-derived password hash: one SHA-256 block,
/// so the derivation is the single-block PBKDF2 case (`T_1` only).
pub const PASSWORD_HASH_LEN: usize = 32;

/// A PBKDF2-HMAC-SHA256 password hash as raw bytes.
pub type PasswordHash = [u8; PASSWORD_HASH_LEN];

/// A derivation was asked for an output length it does not define: empty,
/// or more blocks than its 32-bit block counter can number.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct KdfLengthError(());

impl fmt::Display for KdfLengthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("key derivation output length out of range")
    }
}

/// The keyed PRF both derivations below iterate: HMAC over `D` under `key`.
fn prf<D: EagerHash>(key: &[u8]) -> Hmac<D> {
    // SAFETY-INVARIANT: HMAC accepts a key of any length, so construction
    // from an arbitrary slice can never return `InvalidLength`.
    Hmac::<D>::new_from_slice(key).expect("HMAC accepts any key length")
}

/// Fill `chunk` with PBKDF2 block `index` (RFC 8018 §5.2), truncated:
/// `T_i = U_1 ⊕ … ⊕ U_c` with `U_1 = HMAC(password, salt ‖ INT(i))` and
/// `U_j = HMAC(password, U_{j-1})`, `key` being HMAC keyed by the password.
fn pbkdf2_block<D: EagerHash>(
    key: &Hmac<D>,
    salt: &[u8],
    index: u32,
    iterations: NonZeroU32,
    chunk: &mut [u8],
) {
    let mut mac = key.clone();
    mac.update(salt);
    mac.update(&index.to_be_bytes());
    let mut u = mac.finalize().into_bytes();
    let mut block = u.clone();
    for _ in 1..iterations.get() {
        let mut mac = key.clone();
        mac.update(&u);
        u.zeroize();
        u = mac.finalize().into_bytes();
        for (acc, byte) in block.iter_mut().zip(u.iter()) {
            *acc ^= byte;
        }
    }
    chunk.copy_from_slice(&block[..chunk.len()]);
    u.zeroize();
    block.zeroize();
}

/// Fill `out` with PBKDF2 over HMAC-`D`: `T_1 ‖ T_2 ‖ …` truncated to `out`.
fn pbkdf2<D: EagerHash>(
    password: &[u8],
    salt: &[u8],
    iterations: NonZeroU32,
    out: &mut [u8],
) -> Result<(), KdfLengthError> {
    let width = <Hmac<D> as OutputSizeUser>::output_size();
    if out.is_empty() || u32::try_from(out.len().div_ceil(width)).is_err() {
        return Err(KdfLengthError(()));
    }
    let key = prf::<D>(password);
    for (index, chunk) in (1u32..).zip(out.chunks_mut(width)) {
        pbkdf2_block(&key, salt, index, iterations, chunk);
    }
    Ok(())
}

/// Derive a [`PasswordHash`] from `password` and `salt` with `iterations`
/// rounds of PBKDF2-HMAC-SHA256 (RFC 8018 §5.2).
///
/// The output length equals the HMAC output, so exactly one PBKDF2 block is
/// computed. `iterations` is [`NonZeroU32`] because zero rounds is not a
/// defined PBKDF2 input; the type, not a runtime check, rules it out. Callers
/// choose the cost; the users-database format pins its own accepted range.
#[must_use]
pub fn pbkdf2_sha256(password: &[u8], salt: &[u8], iterations: NonZeroU32) -> PasswordHash {
    let mut out = [0u8; PASSWORD_HASH_LEN];
    pbkdf2_block(&prf::<Sha256>(password), salt, 1, iterations, &mut out);
    out
}

/// Fill `out` with PBKDF2-HMAC-SHA1 of `password` under `salt` and
/// `iterations` (RFC 8018 §5.2), the RFC 3962 Kerberos string-to-key's
/// first step.
///
/// # Errors
///
/// [`KdfLengthError`] if `out` is empty or longer than PBKDF2 defines.
pub fn pbkdf2_sha1(
    password: &[u8],
    salt: &[u8],
    iterations: NonZeroU32,
    out: &mut [u8],
) -> Result<(), KdfLengthError> {
    pbkdf2::<Sha1>(password, salt, iterations, out)
}

/// Fill `out` with the NIST SP 800-108 counter-mode KDF over HMAC-`D`:
/// block `i` is `HMAC(key, [i]₃₂ ‖ label ‖ 0x00 ‖ context ‖ [L]₃₂)`, where
/// `L` is `out`'s length in bits.
fn kdf_counter<D: EagerHash>(
    key: &[u8],
    label: &[u8],
    context: &[u8],
    out: &mut [u8],
) -> Result<(), KdfLengthError> {
    let bits = out
        .len()
        .checked_mul(8)
        .and_then(|bits| u32::try_from(bits).ok())
        .filter(|&bits| bits > 0)
        .ok_or(KdfLengthError(()))?;
    let key = prf::<D>(key);
    let width = <Hmac<D> as OutputSizeUser>::output_size();
    for (index, chunk) in (1u32..).zip(out.chunks_mut(width)) {
        let mut mac = key.clone();
        mac.update(&index.to_be_bytes());
        mac.update(label);
        mac.update(&[0]);
        mac.update(context);
        mac.update(&bits.to_be_bytes());
        let mut block = mac.finalize().into_bytes();
        chunk.copy_from_slice(&block[..chunk.len()]);
        block.zeroize();
    }
    Ok(())
}

/// Fill `out` with the SP 800-108 counter-mode KDF over HMAC-SHA256 —
/// MS-SMB2 §3.1.4.2's key derivation and RFC 8009's KDF-HMAC-SHA2 for
/// `aes128-cts-hmac-sha256-128`.
///
/// # Errors
///
/// [`KdfLengthError`] if `out` is empty or its length in bits overflows
/// the 32-bit `L` field.
pub fn kdf_counter_sha256(
    key: &[u8],
    label: &[u8],
    context: &[u8],
    out: &mut [u8],
) -> Result<(), KdfLengthError> {
    kdf_counter::<Sha256>(key, label, context, out)
}

/// Fill `out` with the SP 800-108 counter-mode KDF over HMAC-SHA384 —
/// RFC 8009's KDF-HMAC-SHA2 for `aes256-cts-hmac-sha384-192`.
///
/// # Errors
///
/// As [`kdf_counter_sha256`].
pub fn kdf_counter_sha384(
    key: &[u8],
    label: &[u8],
    context: &[u8],
    out: &mut [u8],
) -> Result<(), KdfLengthError> {
    kdf_counter::<Sha384>(key, label, context, out)
}

/// Verify that `expected` is the PBKDF2-HMAC-SHA256 hash of `password` under
/// `salt` and `iterations`, in constant time with respect to the hash
/// contents.
///
/// The comparison goes through [`crate::ct_eq`], so it does not leak through
/// timing how many leading hash bytes matched.
#[must_use]
pub fn pbkdf2_sha256_verify(
    password: &[u8],
    salt: &[u8],
    iterations: NonZeroU32,
    expected: &PasswordHash,
) -> bool {
    ct_eq(&pbkdf2_sha256(password, salt, iterations), expected)
}

/// A `bcrypt-pbkdf` derivation was refused.
///
/// Opaque and single-variant, as elsewhere in this crate: a zero round
/// count, an empty passphrase or salt, an output length outside what the
/// construction defines, or a scratch buffer too small are all a malformed
/// key file or a caller error rather than something to branch on.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct BcryptPbkdfError(());

impl fmt::Display for BcryptPbkdfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("bcrypt-pbkdf derivation refused")
    }
}

/// Block size, in bytes, the `bcrypt-pbkdf` stride is counted in.
const BCRYPT_PBKDF_BLOCK_LEN: usize = 32;

/// Longest output the construction defines.
pub const BCRYPT_PBKDF_MAX_OUTPUT_LEN: usize = BCRYPT_PBKDF_BLOCK_LEN * BCRYPT_PBKDF_BLOCK_LEN;

/// Scratch bytes a `bcrypt_pbkdf` of `output_len` bytes needs: the output
/// length rounded up to the block.
#[must_use]
pub const fn bcrypt_pbkdf_scratch_len(output_len: usize) -> usize {
    output_len.div_ceil(BCRYPT_PBKDF_BLOCK_LEN) * BCRYPT_PBKDF_BLOCK_LEN
}

/// Derive `output.len()` bytes from `passphrase` and `salt` with `rounds`
/// of OpenBSD's `bcrypt_pbkdf`, using `scratch` as working space.
///
/// This is a *foreign* format's KDF, present for one reason: the OpenSSH v1
/// private-key container wraps a passphrase-encrypted key with it, so
/// reading a user's existing `id_ed25519` requires it exactly as specified.
/// It is not TAIRiX's password KDF — [`pbkdf2_sha256`] is, and the users
/// database uses that. The construction is deliberately memory-hard
/// relative to plain PBKDF2: each round runs a Blowfish key schedule, which
/// is what makes a stolen key file expensive to attack offline.
///
/// `scratch` is the caller's rather than a buffer allocated here, for the
/// same reason the keystream in [`crate::stream`] writes straight into the
/// caller's destinations: it ends the call holding derived key material,
/// and the holder is the only party that can wipe it. It must be at least
/// [`bcrypt_pbkdf_scratch_len`] bytes.
///
/// # Errors
///
/// Returns [`BcryptPbkdfError`] if `rounds` is zero, either input is empty,
/// `output` is empty or longer than [`BCRYPT_PBKDF_MAX_OUTPUT_LEN`], or
/// `scratch` is too small. Each is refused rather than silently derived
/// from a truncated input.
pub fn bcrypt_pbkdf(
    passphrase: &[u8],
    salt: &[u8],
    rounds: u32,
    output: &mut [u8],
    scratch: &mut [u8],
) -> Result<(), BcryptPbkdfError> {
    bcrypt_pbkdf::bcrypt_pbkdf_with_memory(passphrase, salt, rounds, output, scratch)
        .map_err(|_| BcryptPbkdfError(()))
}

#[cfg(test)]
#[path = "kdf_tests.rs"]
mod tests;
