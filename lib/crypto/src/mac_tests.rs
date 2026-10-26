//! Unit tests for the HMAC and Poly1305 wrappers.
//!
//! The HMAC known answers come from an implementation independent of the one
//! under test (`CPython`'s `hmac` over OpenSSL). The SHA-2 instantiations are
//! additionally cross-checked in-tree against the textbook RFC 2104
//! construction built from this crate's own SHA wrappers, so a wiring error
//! that swapped a digest, mis-padded a key, or fed the key as the message
//! fails twice over. HMAC-SHA1 has only the independent known answer,
//! because the bare SHA-1 digest is deliberately not exposed to build a
//! textbook check from.
//!
//! The Poly1305 known answer is RFC 8439 §2.5.2 verbatim. HMAC-SHA384's is
//! RFC 4231 test case 1, AES-CMAC's are RFC 4493 §4's, and AES-GMAC's is the
//! NIST GCM vector with an empty plaintext (`gcmEncryptExtIV128`, count 0),
//! each cross-checked with `cryptography` over OpenSSL.

use super::{
    aes128_cmac, aes128_cmac_verify, aes128_gmac, aes128_gmac_verify, hmac_sha1, hmac_sha1_verify,
    hmac_sha256, hmac_sha256_parts, hmac_sha256_verify, hmac_sha384, hmac_sha384_verify,
    hmac_sha512, hmac_sha512_parts, hmac_sha512_verify, poly1305, poly1305_verify, HmacSha1Key,
    HmacSha256Key, HmacSha384Key, HmacSha512Key, Poly1305Key, HMAC_SHA1_TAG_LEN,
    HMAC_SHA256_TAG_LEN, HMAC_SHA512_TAG_LEN, POLY1305_TAG_LEN,
};
use crate::test_hex::hex;

use crate::hash::{sha256, sha512};

/// The message every HMAC known answer below is taken over.
const MESSAGE: &[u8] = b"the quick brown fox jumps over the lazy dog";

/// A deterministic 32-byte key for the HMAC-SHA-256 known answer.
const SHA256_KEY: [u8; 32] = [
    0x0b, 0x30, 0x55, 0x7a, 0x9f, 0xc4, 0xe9, 0x0e, 0x33, 0x58, 0x7d, 0xa2, 0xc7, 0xec, 0x11, 0x36,
    0x5b, 0x80, 0xa5, 0xca, 0xef, 0x14, 0x39, 0x5e, 0x83, 0xa8, 0xcd, 0xf2, 0x17, 0x3c, 0x61, 0x86,
];

/// `HMAC-SHA-256(SHA256_KEY, MESSAGE)`, computed by `CPython`'s `hmac`.
const SHA256_TAG: [u8; HMAC_SHA256_TAG_LEN] = [
    0x92, 0x2c, 0x32, 0xcc, 0x9f, 0xd3, 0x29, 0x99, 0xa0, 0x79, 0xb9, 0x1c, 0x4d, 0x58, 0x0d, 0xc2,
    0x45, 0x6a, 0x78, 0x3c, 0x30, 0x44, 0xae, 0xa6, 0xd1, 0xca, 0x9e, 0xf7, 0xab, 0xb6, 0x01, 0xcb,
];

/// A deterministic 64-byte key for the HMAC-SHA-512 known answer.
const SHA512_KEY: [u8; 64] = [
    0x0b, 0x30, 0x55, 0x7a, 0x9f, 0xc4, 0xe9, 0x0e, 0x33, 0x58, 0x7d, 0xa2, 0xc7, 0xec, 0x11, 0x36,
    0x5b, 0x80, 0xa5, 0xca, 0xef, 0x14, 0x39, 0x5e, 0x83, 0xa8, 0xcd, 0xf2, 0x17, 0x3c, 0x61, 0x86,
    0xab, 0xd0, 0xf5, 0x1a, 0x3f, 0x64, 0x89, 0xae, 0xd3, 0xf8, 0x1d, 0x42, 0x67, 0x8c, 0xb1, 0xd6,
    0xfb, 0x20, 0x45, 0x6a, 0x8f, 0xb4, 0xd9, 0xfe, 0x23, 0x48, 0x6d, 0x92, 0xb7, 0xdc, 0x01, 0x26,
];

/// `HMAC-SHA-512(SHA512_KEY, MESSAGE)`, computed by `CPython`'s `hmac`.
const SHA512_TAG: [u8; HMAC_SHA512_TAG_LEN] = [
    0x34, 0x96, 0x25, 0x7d, 0x10, 0x08, 0x9c, 0x39, 0x70, 0x28, 0x76, 0x25, 0xc1, 0x42, 0x4b, 0xb1,
    0x11, 0x89, 0x0c, 0xac, 0xd8, 0x12, 0x2a, 0x9a, 0x1d, 0x6a, 0x0f, 0xcf, 0x50, 0xff, 0x43, 0x6e,
    0xbb, 0x6c, 0x43, 0x78, 0x9d, 0x46, 0x64, 0xe9, 0x95, 0x04, 0x59, 0xf6, 0xe7, 0x89, 0x4b, 0xfd,
    0xd6, 0xd8, 0x5a, 0xa6, 0xf4, 0xc9, 0xe7, 0x41, 0xc8, 0x2d, 0xb5, 0x12, 0xee, 0x15, 0x8e, 0xe5,
];

/// A deterministic 20-byte key for the HMAC-SHA-1 known answer.
const SHA1_KEY: [u8; 20] = [
    0x0b, 0x30, 0x55, 0x7a, 0x9f, 0xc4, 0xe9, 0x0e, 0x33, 0x58, 0x7d, 0xa2, 0xc7, 0xec, 0x11, 0x36,
    0x5b, 0x80, 0xa5, 0xca,
];

/// `HMAC-SHA-1(SHA1_KEY, MESSAGE)`, computed by `CPython`'s `hmac`.
const SHA1_TAG: [u8; HMAC_SHA1_TAG_LEN] = [
    0x7a, 0x94, 0x5c, 0x99, 0xfc, 0x0b, 0xc7, 0xc5, 0x6f, 0x1d, 0xf8, 0x9a, 0x8d, 0xc5, 0x6c, 0x04,
    0x5a, 0x36, 0x0c, 0x53,
];

/// RFC 8439 §2.5.2: the Poly1305 one-time key.
const POLY1305_RFC8439_KEY: Poly1305Key = [
    0x85, 0xd6, 0xbe, 0x78, 0x57, 0x55, 0x6d, 0x33, 0x7f, 0x44, 0x52, 0xfe, 0x42, 0xd5, 0x06, 0xa8,
    0x01, 0x03, 0x80, 0x8a, 0xfb, 0x0d, 0xb2, 0xfd, 0x4a, 0xbf, 0xf6, 0xaf, 0x41, 0x49, 0xf5, 0x1b,
];

/// RFC 8439 §2.5.2: the message the tag below is taken over.
const POLY1305_RFC8439_MESSAGE: &[u8] = b"Cryptographic Forum Research Group";

/// RFC 8439 §2.5.2: the expected tag.
const POLY1305_RFC8439_TAG: [u8; POLY1305_TAG_LEN] = [
    0xa8, 0x06, 0x1d, 0xc1, 0x30, 0x51, 0x36, 0xc6, 0xc2, 0x2b, 0x8b, 0xaf, 0x0c, 0x01, 0x27, 0xa9,
];

/// The textbook HMAC construction (RFC 2104) for a key shorter than the
/// digest's block, computed from a hash wrapper rather than from the HMAC
/// wrapper under test. `BLOCK` is the digest's block size and `OUT` its
/// output width; both buffers are fixed-size arrays, so this needs no
/// allocator.
fn textbook<const BLOCK: usize, const OUT: usize>(
    hash: impl Fn(&[u8]) -> [u8; OUT],
    key: &[u8],
    message: &[u8],
    scratch: &mut [u8],
) -> [u8; OUT] {
    assert!(
        key.len() <= BLOCK,
        "this helper covers the short-key case only"
    );
    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for (i, k) in key.iter().enumerate() {
        ipad[i] ^= k;
        opad[i] ^= k;
    }

    let inner_len = BLOCK + message.len();
    scratch[..BLOCK].copy_from_slice(&ipad);
    scratch[BLOCK..inner_len].copy_from_slice(message);
    let inner = hash(&scratch[..inner_len]);

    scratch[..BLOCK].copy_from_slice(&opad);
    scratch[BLOCK..BLOCK + OUT].copy_from_slice(&inner);
    hash(&scratch[..BLOCK + OUT])
}

#[test]
fn hmac_matches_the_independent_known_answers() {
    assert_eq!(hmac_sha256(&SHA256_KEY, MESSAGE), SHA256_TAG);
    assert_eq!(hmac_sha512(&SHA512_KEY, MESSAGE), SHA512_TAG);
    assert_eq!(hmac_sha1(&SHA1_KEY, MESSAGE), SHA1_TAG);
}

#[test]
fn hmac_sha2_matches_the_textbook_construction() {
    let mut scratch = [0u8; 256];
    assert_eq!(
        textbook::<64, 32>(sha256, &SHA256_KEY, MESSAGE, &mut scratch),
        SHA256_TAG
    );
    assert_eq!(
        textbook::<128, 64>(sha512, &SHA512_KEY, MESSAGE, &mut scratch),
        SHA512_TAG
    );
}

#[test]
fn poly1305_matches_the_rfc8439_vector() {
    let tag = poly1305(&POLY1305_RFC8439_KEY, POLY1305_RFC8439_MESSAGE);
    assert_eq!(tag, POLY1305_RFC8439_TAG);
    assert!(poly1305_verify(
        &POLY1305_RFC8439_KEY,
        POLY1305_RFC8439_MESSAGE,
        &tag
    ));
}

#[test]
fn poly1305_rejects_a_tampered_message_key_or_tag() {
    let tag = POLY1305_RFC8439_TAG;

    let mut bad_tag = tag;
    bad_tag[0] ^= 0x01;
    assert!(!poly1305_verify(
        &POLY1305_RFC8439_KEY,
        POLY1305_RFC8439_MESSAGE,
        &bad_tag
    ));
    assert!(!poly1305_verify(
        &POLY1305_RFC8439_KEY,
        b"Cryptographic Forum Research Grouq",
        &tag
    ));

    let mut other_key = POLY1305_RFC8439_KEY;
    other_key[31] ^= 0x01;
    assert!(!poly1305_verify(&other_key, POLY1305_RFC8439_MESSAGE, &tag));
}

/// Poly1305 pads a trailing partial block, so an exact multiple of the
/// 16-byte block and a ragged length must both authenticate and must not
/// collide — the padding is what separates them.
#[test]
fn poly1305_spans_the_block_boundary() {
    let key = POLY1305_RFC8439_KEY;
    let aligned = [0x5au8; 32];
    let ragged = [0x5au8; 33];
    assert!(poly1305_verify(&key, &aligned, &poly1305(&key, &aligned)));
    assert!(poly1305_verify(&key, &ragged, &poly1305(&key, &ragged)));
    assert_ne!(poly1305(&key, &aligned), poly1305(&key, &ragged));
    assert_ne!(poly1305(&key, &[]), poly1305(&key, &aligned));
}

/// `*_parts` must equal the one-shot over the joined parts for any split, so
/// the DRBG's `V ‖ byte ‖ data` form and SSH's `seq ‖ packet` form are
/// faithful.
#[test]
fn parts_equal_the_concatenated_single_shot() {
    for split in [0usize, 1, 7, MESSAGE.len() - 1, MESSAGE.len()] {
        let (a, b) = MESSAGE.split_at(split);
        assert_eq!(
            hmac_sha256_parts(&SHA256_KEY, &[a, b]),
            SHA256_TAG,
            "sha256 split at {split}"
        );
        assert_eq!(
            hmac_sha512_parts(&SHA512_KEY, &[a, b]),
            SHA512_TAG,
            "sha512 split at {split}"
        );
    }
    assert_eq!(
        hmac_sha256_parts(&SHA256_KEY, &[&[], MESSAGE, &[]]),
        SHA256_TAG
    );
}

#[test]
fn verify_accepts_a_genuine_tag_and_rejects_tampering() {
    assert!(hmac_sha256_verify(&SHA256_KEY, MESSAGE, &SHA256_TAG));
    assert!(hmac_sha512_verify(&SHA512_KEY, MESSAGE, &SHA512_TAG));
    assert!(hmac_sha1_verify(&SHA1_KEY, MESSAGE, &SHA1_TAG));

    let mut bad = SHA256_TAG;
    bad[0] ^= 0x01;
    assert!(!hmac_sha256_verify(&SHA256_KEY, MESSAGE, &bad));
    assert!(!hmac_sha256_verify(
        &SHA256_KEY,
        b"another message",
        &SHA256_TAG
    ));

    let mut other_key: HmacSha256Key = SHA256_KEY;
    other_key[0] ^= 0x01;
    assert!(!hmac_sha256_verify(&other_key, MESSAGE, &SHA256_TAG));
}

#[test]
fn tags_are_the_declared_widths() {
    assert_eq!(HMAC_SHA256_TAG_LEN, 32);
    assert_eq!(HMAC_SHA512_TAG_LEN, 64);
    assert_eq!(HMAC_SHA1_TAG_LEN, 20);
    assert_eq!(POLY1305_TAG_LEN, 16);
}

/// The three instantiations are genuinely different digests, so a macro
/// expansion that wired two of them to the same core would fail here.
#[test]
fn the_instantiations_do_not_share_a_core() {
    let short: HmacSha1Key = [0x11; 20];
    let mid: HmacSha256Key = [0x11; 32];
    let long: HmacSha512Key = [0x11; 64];
    assert_ne!(
        hmac_sha256(&mid, MESSAGE)[..],
        hmac_sha512(&long, MESSAGE)[..32]
    );
    assert_ne!(
        hmac_sha1(&short, MESSAGE)[..],
        hmac_sha256(&mid, MESSAGE)[..20]
    );
}

/// Every hash state wipes itself on drop, and so does an HMAC's keyed
/// midstate, which lives in the two hash cores it holds. The upstream
/// `zeroize` features are what make them do so; this stops compiling if one
/// is dropped.
#[test]
fn hash_and_mac_states_wipe_themselves_on_drop() {
    fn wipes<T: sha2::digest::zeroize::ZeroizeOnDrop>() {}
    wipes::<sha2::Sha256>();
    wipes::<sha2::Sha512>();
    wipes::<sha1::Sha1>();
    wipes::<sha2::block_api::Sha256VarCore>();
    wipes::<sha2::block_api::Sha512VarCore>();
}

#[test]
fn hmac_sha384_matches_rfc_4231() {
    // HMAC zero-extends a short key to the block size, so the RFC's 20-byte
    // key is the fixed-width key with zeros after it.
    let mut key: HmacSha384Key = [0; 48];
    key[..20].fill(0x0b);
    let tag = hmac_sha384(&key, b"Hi There");
    assert_eq!(
        tag,
        hex::<48>(
            "afd03944d84895626b0825f4ab46907f15f9dadbe4101ec682aa034c7cebc59c\
             faea9ea9076ede7f4af152e8b2fa9cb6"
        )
    );
    assert!(hmac_sha384_verify(&key, b"Hi There", &tag));
    assert!(!hmac_sha384_verify(&key, b"Hi there", &tag));
}

/// RFC 4493 §4's key, shared by its four examples.
const CMAC_KEY: [u8; 16] = [
    0x2b, 0x7e, 0x15, 0x16, 0x28, 0xae, 0xd2, 0xa6, 0xab, 0xf7, 0x15, 0x88, 0x09, 0xcf, 0x4f, 0x3c,
];

/// RFC 4493 §4's message; the examples take its first 0, 16, 40 and 64 bytes.
fn cmac_message() -> [u8; 64] {
    hex(
        "6bc1bee22e409f96e93d7e117393172aae2d8a571e03ac9c9eb76fac45af8e51\
         30c81c46a35ce411e5fbc1191a0a52eff69f2445df4f9b17ad2b417be66c3710",
    )
}

#[test]
fn aes128_cmac_matches_rfc_4493() {
    let message = cmac_message();
    for (len, expected) in [
        (0, "bb1d6929e95937287fa37d129b756746"),
        (16, "070a16b46b4d4144f79bdd9dd04a287c"),
        (40, "dfa66747de9ae63030ca32611497c827"),
        (64, "51f0bebf7e3b9d92fc49741779363cfe"),
    ] {
        assert_eq!(
            aes128_cmac(&CMAC_KEY, &message[..len]),
            hex(expected),
            "{len} bytes"
        );
    }
}

#[test]
fn aes128_cmac_verification_refuses_any_other_value() {
    let message = cmac_message();
    let tag = aes128_cmac(&CMAC_KEY, &message);
    assert!(aes128_cmac_verify(&CMAC_KEY, &message, &tag));
    let mut forged = tag;
    forged[0] ^= 0x80;
    assert!(!aes128_cmac_verify(&CMAC_KEY, &message, &forged));
    assert!(!aes128_cmac_verify(&CMAC_KEY, &message[..63], &tag));
}

#[test]
fn aes128_gmac_matches_the_nist_empty_plaintext_vector() {
    let key = hex("77be63708971c4e240d1cb79e8d77feb");
    let nonce = hex("e0e00f19fed7ba0136a797f3");
    let data = hex::<16>("7a43ec1d9c0a5a78a0b16533a6213cab");
    let tag = aes128_gmac(&key, &nonce, &data).expect("within the limit");
    assert_eq!(tag, hex("209fcc8d3675ed938e9c7166709dd946"));
    assert!(aes128_gmac_verify(&key, &nonce, &data, &tag));
}

#[test]
fn aes128_gmac_binds_the_nonce_and_the_data() {
    let key = [0x42; 16];
    let nonce = [7; 12];
    let tag = aes128_gmac(&key, &nonce, b"an SMB 3.1.1 message").expect("within the limit");
    let mut other_nonce = nonce;
    other_nonce[11] ^= 1;
    assert!(!aes128_gmac_verify(
        &key,
        &other_nonce,
        b"an SMB 3.1.1 message",
        &tag
    ));
    assert!(!aes128_gmac_verify(
        &key,
        &nonce,
        b"an SMB 3.1.1 messagf",
        &tag
    ));
}
