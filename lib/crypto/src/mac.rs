//! Keyed message authentication: HMAC and Poly1305.
//!
//! HMAC-SHA256 (RFC 2104, FIPS 198-1) is the keyed authenticator `ARXFS`
//! seals every metadata block with (`docs/src/filesystem/arxfs-spec.md` §5,
//! §8): the tag covers a block's identity, owner, generation, expected
//! address, and payload, so a stale, misdirected, wrong-type, torn, or
//! bit-rotted metadata block fails the check and is repaired from its
//! redundant copy rather than trusted.
//!
//! HMAC-SHA512 is the SSH transport's `hmac-sha2-512` (RFC 6668), and
//! HMAC-SHA384 is Kerberos's `aes256-cts-hmac-sha384-192` (RFC 8009).
//! HMAC-SHA1 exists for two fixed foreign formats: OpenSSH's hashed
//! `known_hosts` index (`|1|salt|HMAC-SHA1(salt, host)`), an obfuscation
//! over a hostname, and the RFC 3962 Kerberos enctypes' integrity check and
//! string-to-key. HMAC's security rests on the digest's pseudo-randomness,
//! not its collision resistance, so SHA-1's collision weakness bears on
//! neither. The bare SHA-1 digest is not exposed at all.
//!
//! AES-CMAC (RFC 4493) and AES-GMAC (GCM over an empty plaintext, NIST SP
//! 800-38D) are how SMB 3 signs a message: 3.0 and 3.0.2 with CMAC, 3.1.1
//! with GMAC where it was negotiated.
//!
//! Poly1305 (RFC 8439 §2.5) is the one-time authenticator
//! `chacha20-poly1305@openssh.com` keys from a `ChaCha20` block. The packaged
//! [`crate::aead`] cannot serve that construction: OpenSSH authenticates a
//! length field that is encrypted under a *second*, independent key, so the
//! MAC and the cipher are composed by the protocol rather than by the AEAD.
//!
//! As with the rest of `lib/crypto`, the wrappers expose a *narrower* API
//! than the upstream crates: callers hand in a fixed-size key and receive a
//! fixed-size tag, never the upstream `Mac`/`KeyInit` traits or the
//! `GenericArray` types. Every verification goes through [`crate::ct_eq`] so
//! a caller cannot accidentally reintroduce a timing-leaking `==` over a
//! secret tag.

use aes::Aes128;
use aes_gcm::aead::AeadInOut;
use aes_gcm::{Aes128Gcm, Nonce as GcmNonce};
use cmac::Cmac;
use hmac::{Hmac, KeyInit as HmacKeyInit, Mac};
use poly1305::Poly1305;
use sha1::Sha1;
use sha2::{Sha256, Sha384, Sha512};

use crate::constant_time::ct_eq;

/// Emit the key/tag types, one-shot, multi-part, and verifying entry points
/// for one HMAC instantiation.
///
/// The five items differ between instantiations only in the upstream digest
/// and the output width, so they are written once rather than three times.
macro_rules! hmac_variant {
    (
        $digest:ty,
        $spec:literal,
        $key_len:ident,
        $tag_len:ident,
        $width:literal,
        $key_ty:ident,
        $tag_ty:ident,
        $one_shot:ident,
        $parts:ident,
        $verify:ident
    ) => {
        #[doc = concat!("Length, in bytes, of an ", $spec, " key. It matches the")]
        /// digest width, so the key is neither zero-extended past the point
        /// where extra bytes stop contributing nor hashed down by HMAC's
        /// over-long-key rule.
        pub const $key_len: usize = $width;

        #[doc = concat!("Length, in bytes, of an ", $spec, " tag.")]
        pub const $tag_len: usize = $width;

        #[doc = concat!("An ", $spec, " key as raw bytes.")]
        pub type $key_ty = [u8; $key_len];

        #[doc = concat!("An ", $spec, " tag as raw bytes.")]
        pub type $tag_ty = [u8; $tag_len];

        #[doc = concat!("Compute the ", $spec, " tag of `data` under `key`.")]
        #[must_use]
        pub fn $one_shot(key: &$key_ty, data: &[u8]) -> $tag_ty {
            $parts(key, &[data])
        }

        #[doc = concat!("Compute the ", $spec, " tag of the concatenation of")]
        /// `parts` under `key`.
        ///
        /// Equivalent to the one-shot over `parts.concat()`, but feeds each
        /// part to the underlying streaming HMAC in turn so the caller never
        /// has to allocate or stack-copy a contiguous buffer. This is what
        /// lets `tairix-rng`'s HMAC-DRBG compute `HMAC(K, V ‖ byte ‖ data)`
        /// (NIST SP 800-90A) over its working state with no allocator — the
        /// kernel allocator must not be on the entropy path — and no
        /// arbitrary fixed-size scratch bound.
        #[must_use]
        pub fn $parts(key: &$key_ty, parts: &[&[u8]]) -> $tag_ty {
            // SAFETY-INVARIANT: HMAC accepts a key of any length, so
            // construction from a fixed-size array can never return
            // `InvalidLength`. The `expect` documents an invariant that is
            // unreachable in practice.
            let mut mac = <Hmac<$digest> as HmacKeyInit>::new_from_slice(key)
                .expect("HMAC accepts any key length");
            for part in parts {
                mac.update(part);
            }
            let out = mac.finalize().into_bytes();
            let mut tag = [0u8; $tag_len];
            tag.copy_from_slice(out.as_slice());
            tag
        }

        #[doc = concat!("Verify that `tag` is the ", $spec, " of `data` under")]
        /// `key`, in constant time with respect to the tag contents.
        ///
        /// The comparison goes through [`crate::ct_eq`], so it does not leak
        /// through timing how many leading tag bytes matched.
        #[must_use]
        pub fn $verify(key: &$key_ty, data: &[u8], tag: &$tag_ty) -> bool {
            ct_eq(&$one_shot(key, data), tag)
        }
    };
}

hmac_variant!(
    Sha256,
    "HMAC-SHA256",
    HMAC_SHA256_KEY_LEN,
    HMAC_SHA256_TAG_LEN,
    32,
    HmacSha256Key,
    HmacSha256Tag,
    hmac_sha256,
    hmac_sha256_parts,
    hmac_sha256_verify
);

hmac_variant!(
    Sha512,
    "HMAC-SHA512",
    HMAC_SHA512_KEY_LEN,
    HMAC_SHA512_TAG_LEN,
    64,
    HmacSha512Key,
    HmacSha512Tag,
    hmac_sha512,
    hmac_sha512_parts,
    hmac_sha512_verify
);

hmac_variant!(
    Sha384,
    "HMAC-SHA384",
    HMAC_SHA384_KEY_LEN,
    HMAC_SHA384_TAG_LEN,
    48,
    HmacSha384Key,
    HmacSha384Tag,
    hmac_sha384,
    hmac_sha384_parts,
    hmac_sha384_verify
);

hmac_variant!(
    Sha1,
    "HMAC-SHA1",
    HMAC_SHA1_KEY_LEN,
    HMAC_SHA1_TAG_LEN,
    20,
    HmacSha1Key,
    HmacSha1Tag,
    hmac_sha1,
    hmac_sha1_parts,
    hmac_sha1_verify
);

/// Length, in bytes, of a Poly1305 one-time key.
pub const POLY1305_KEY_LEN: usize = 32;

/// Length, in bytes, of a Poly1305 tag.
pub const POLY1305_TAG_LEN: usize = 16;

/// A Poly1305 one-time key as raw bytes.
///
/// "One-time" is not advice. Poly1305 is a Wegman-Carter authenticator:
/// authenticating two different messages under the same key lets an
/// adversary solve for the key and forge arbitrarily. Every caller must
/// derive a fresh key per message — `chacha20-poly1305@openssh.com` takes
/// one `ChaCha20` block under the packet's own sequence number.
pub type Poly1305Key = [u8; POLY1305_KEY_LEN];

/// A Poly1305 tag as raw bytes.
pub type Poly1305Tag = [u8; POLY1305_TAG_LEN];

/// Compute the Poly1305 tag of `data` under the one-time `key`
/// (RFC 8439 §2.5.1).
///
/// There is no multi-part form. SSH authenticates a packet's encrypted
/// length field and ciphertext, which are contiguous in the buffer the
/// packet was framed into or received from the socket, so a second entry
/// point would buy a caller nothing and would hand-roll block buffering
/// around an authenticator that already does it.
#[must_use]
pub fn poly1305(key: &Poly1305Key, data: &[u8]) -> Poly1305Tag {
    Poly1305::new(key.into()).compute_unpadded(data).into()
}

/// Verify that `tag` is the Poly1305 of `data` under the one-time `key`, in
/// constant time with respect to the tag contents.
#[must_use]
pub fn poly1305_verify(key: &Poly1305Key, data: &[u8], tag: &Poly1305Tag) -> bool {
    ct_eq(&poly1305(key, data), tag)
}

/// Length, in bytes, of an AES-128-CMAC or AES-128-GMAC key.
pub const AES128_MAC_KEY_LEN: usize = 16;

/// Length, in bytes, of an AES-CMAC or AES-GMAC tag: one AES block.
pub const AES_MAC_TAG_LEN: usize = 16;

/// Length, in bytes, of an AES-GMAC nonce, GCM's 96-bit nonce.
pub const AES_GMAC_NONCE_LEN: usize = 12;

/// An AES-128-CMAC or AES-128-GMAC key as raw bytes.
pub type Aes128MacKey = [u8; AES128_MAC_KEY_LEN];

/// An AES-CMAC or AES-GMAC tag as raw bytes.
pub type AesMacTag = [u8; AES_MAC_TAG_LEN];

/// An AES-GMAC nonce as raw bytes.
pub type AesGmacNonce = [u8; AES_GMAC_NONCE_LEN];

/// Compute the AES-128-CMAC (RFC 4493) of `data` under `key`.
#[must_use]
pub fn aes128_cmac(key: &Aes128MacKey, data: &[u8]) -> AesMacTag {
    let mut mac = <Cmac<Aes128> as cmac::KeyInit>::new(key.into());
    mac.update(data);
    mac.finalize().into_bytes().into()
}

/// Verify that `tag` is the AES-128-CMAC of `data` under `key`, in constant
/// time with respect to the tag contents.
#[must_use]
pub fn aes128_cmac_verify(key: &Aes128MacKey, data: &[u8], tag: &AesMacTag) -> bool {
    ct_eq(&aes128_cmac(key, data), tag)
}

/// Compute the AES-128-GMAC of `data` under `key` and `nonce`: GCM's tag over
/// an empty plaintext with `data` as the associated data.
///
/// GMAC inherits GCM's nonce rule: one `(key, nonce)` pair authenticates one
/// message, ever. SMB 3.1.1 builds the nonce from the message id, which a
/// connection never reuses; this function generates none.
///
/// `None` when `data` is longer than GCM authenticates under one nonce
/// (about 64 GiB).
#[must_use]
pub fn aes128_gmac(key: &Aes128MacKey, nonce: &AesGmacNonce, data: &[u8]) -> Option<AesMacTag> {
    let cipher = <Aes128Gcm as aes_gcm::KeyInit>::new(key.into());
    let mut empty = [0u8; 0];
    cipher
        .encrypt_inout_detached(&GcmNonce::from(*nonce), data, empty.as_mut_slice().into())
        .ok()
        .map(Into::into)
}

/// Verify that `tag` is the AES-128-GMAC of `data` under `key` and `nonce`,
/// in constant time with respect to the tag contents. Input GMAC cannot
/// authenticate never verifies.
#[must_use]
pub fn aes128_gmac_verify(
    key: &Aes128MacKey,
    nonce: &AesGmacNonce,
    data: &[u8],
    tag: &AesMacTag,
) -> bool {
    aes128_gmac(key, nonce, data).is_some_and(|computed| ct_eq(&computed, tag))
}

#[cfg(test)]
#[path = "mac_tests.rs"]
mod tests;
