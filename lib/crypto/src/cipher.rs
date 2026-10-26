//! Unauthenticated block-cipher modes: AES in counter mode, and AES-CBC with
//! ciphertext stealing.
//!
//! AES-CTR is the SSH transport's `aes{128,192,256}-ctr` (RFC 4344): the
//! whole 16-byte block is one big-endian counter, seeded from the IV the key
//! exchange derived and advanced across every packet of the connection.
//! Unlike the rest of this crate those wrappers are **stateful**, and that is
//! the point: a one-shot would restart the counter on each call, and CTR
//! reuses its keystream the instant a counter value repeats. One cipher
//! object per direction per connection, advanced by the bytes it processes,
//! is the only shape that cannot do that.
//!
//! AES-CTS is Kerberos's AES mode (RFC 3962 §6, RFC 8009 §5): CBC with the
//! last two blocks always swapped (NIST SP 800-38A addendum's CS3), from the
//! all-zero initial cipher state, and plain CBC for a message of exactly one
//! block. Kerberos never chains a cipher state between messages — every
//! plaintext starts with a random confounder instead — so these are one-shot.
//!
//! Neither mode provides integrity. Every caller must authenticate
//! separately — SSH with an [`crate::mac`] HMAC, Kerberos with HMAC-SHA1-96
//! or HMAC-SHA2 — and must verify the tag before acting on plaintext. Where
//! an AEAD will do, prefer [`crate::aead`].

use core::fmt;

use aes::cipher::{
    Block, BlockCipherDecrypt, BlockCipherEncrypt, InnerIvInit, Key, KeyInit, KeyIvInit,
    StreamCipher,
};
use aes::{Aes128, Aes192, Aes256};
use ctr::Ctr128BE;
use cts::{CbcCs3, Decrypt as _, Encrypt as _};

/// AES block size in bytes, and so the width of a CTR initial counter.
pub const AES_BLOCK_LEN: usize = 16;

/// An AES-CTR initial counter block as raw bytes.
pub type AesCtrIv = [u8; AES_BLOCK_LEN];

/// An AES-CTR run did not fit the counter.
///
/// The counter is the full 128-bit block, so exhausting it takes `2^132`
/// bytes under one key: unreachable for any real message. The error exists
/// because silently wrapping the counter would reuse keystream, which is a
/// total loss of confidentiality, so the path refuses rather than assuming.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct CipherError(());

impl fmt::Display for CipherError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("aes-ctr run exceeds the counter")
    }
}

/// Emit the key type, length constant, and stateful cipher for one AES-CTR
/// key size.
///
/// The three sizes differ only in the underlying AES key schedule, so the
/// wrapper is written once rather than three times.
macro_rules! aes_ctr_variant {
    (
        $block:ty,
        $spec:literal,
        $key_len:ident = $width:literal,
        $key_ty:ident,
        $cipher:ident
    ) => {
        #[doc = concat!("Length, in bytes, of an ", $spec, " key.")]
        pub const $key_len: usize = $width;

        #[doc = concat!("An ", $spec, " key as raw bytes.")]
        pub type $key_ty = [u8; $key_len];

        #[doc = concat!($spec, " in counter mode, advanced across calls.")]
        ///
        /// Holds the expanded key schedule and the live counter. The
        /// upstream cipher wipes both on drop, but the caller's copy of the
        /// key is the caller's to zero.
        pub struct $cipher {
            inner: Ctr128BE<$block>,
        }

        impl $cipher {
            #[doc = concat!("Start ", $spec, "-CTR under `key` from the initial")]
            /// counter block `iv`.
            ///
            /// `(key, iv)` must never repeat: the keystream depends on
            /// nothing else, so a repeat XORs two plaintexts together. SSH
            /// derives both per direction from the key exchange and rekeys
            /// before the counter could revisit a block.
            #[must_use]
            pub fn new(key: &$key_ty, iv: &AesCtrIv) -> Self {
                Self {
                    inner: Ctr128BE::<$block>::new(key.into(), iv.into()),
                }
            }

            /// XOR the next keystream bytes into `buffer`, advancing the
            /// counter by the bytes consumed.
            ///
            /// Encryption and decryption are the same operation. Successive
            /// calls continue one keystream, so a packet split across calls
            /// gives the same result as one call over the whole of it.
            ///
            /// # Errors
            ///
            /// Returns [`CipherError`] if the run would carry the counter
            /// past its last block rather than wrapping onto keystream this
            /// key has already emitted.
            pub fn apply(&mut self, buffer: &mut [u8]) -> Result<(), CipherError> {
                self.inner
                    .try_apply_keystream(buffer)
                    .map_err(|_| CipherError(()))
            }
        }

        impl fmt::Debug for $cipher {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                // Neither the key schedule nor the counter position is
                // printed: both narrow a keystream recovery.
                f.debug_struct(stringify!($cipher)).finish_non_exhaustive()
            }
        }
    };
}

aes_ctr_variant!(Aes128, "AES-128", AES128_KEY_LEN = 16, Aes128Key, Aes128Ctr);
aes_ctr_variant!(Aes192, "AES-192", AES192_KEY_LEN = 24, Aes192Key, Aes192Ctr);
aes_ctr_variant!(Aes256, "AES-256", AES256_KEY_LEN = 32, Aes256Key, Aes256Ctr);

/// An AES-CTS message was shorter than one AES block, which ciphertext
/// stealing cannot express.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct CtsLengthError(());

impl fmt::Display for CtsLengthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("aes-cts message shorter than one block")
    }
}

/// Encrypt `buffer` in place with AES-CTS under `key`.
fn cts_encrypt<C>(key: &Key<C>, buffer: &mut [u8]) -> Result<(), CtsLengthError>
where
    C: BlockCipherEncrypt + KeyInit,
{
    let cipher = C::new(key);
    match <&mut Block<C>>::try_from(&mut *buffer) {
        // One block is plain CBC from the zero state: the block cipher alone.
        Ok(block) => {
            cipher.encrypt_block(block);
            Ok(())
        }
        Err(_) => CbcCs3::inner_iv_init(cipher, &Block::<C>::default())
            .encrypt(buffer)
            .map_err(|_| CtsLengthError(())),
    }
}

/// Decrypt `buffer` in place with AES-CTS under `key`.
fn cts_decrypt<C>(key: &Key<C>, buffer: &mut [u8]) -> Result<(), CtsLengthError>
where
    C: BlockCipherDecrypt + KeyInit,
{
    let cipher = C::new(key);
    match <&mut Block<C>>::try_from(&mut *buffer) {
        Ok(block) => {
            cipher.decrypt_block(block);
            Ok(())
        }
        Err(_) => CbcCs3::inner_iv_init(cipher, &Block::<C>::default())
            .decrypt(buffer)
            .map_err(|_| CtsLengthError(())),
    }
}

/// Emit the one-shot AES-CTS encrypt and decrypt for one key size.
macro_rules! aes_cts_variant {
    ($block:ty, $spec:literal, $key_ty:ident, $encrypt:ident, $decrypt:ident) => {
        #[doc = concat!("Encrypt `buffer` in place with ", $spec, "-CTS under `key`,")]
        /// from the all-zero initial cipher state.
        ///
        /// # Errors
        ///
        /// [`CtsLengthError`] if `buffer` is shorter than one block.
        pub fn $encrypt(key: &$key_ty, buffer: &mut [u8]) -> Result<(), CtsLengthError> {
            cts_encrypt::<$block>(key.into(), buffer)
        }

        #[doc = concat!("Decrypt `buffer` in place with ", $spec, "-CTS under `key`,")]
        /// from the all-zero initial cipher state.
        ///
        /// The plaintext is unauthenticated until the caller's integrity
        /// check over it has passed.
        ///
        /// # Errors
        ///
        /// [`CtsLengthError`] if `buffer` is shorter than one block.
        pub fn $decrypt(key: &$key_ty, buffer: &mut [u8]) -> Result<(), CtsLengthError> {
            cts_decrypt::<$block>(key.into(), buffer)
        }
    };
}

aes_cts_variant!(
    Aes128,
    "AES-128",
    Aes128Key,
    aes128_cts_encrypt,
    aes128_cts_decrypt
);
aes_cts_variant!(
    Aes256,
    "AES-256",
    Aes256Key,
    aes256_cts_encrypt,
    aes256_cts_decrypt
);

#[cfg(test)]
#[path = "cipher_tests.rs"]
mod tests;
