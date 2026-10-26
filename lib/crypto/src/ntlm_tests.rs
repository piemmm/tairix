//! Unit tests for the NTLMv2 constructions.
//!
//! The NTLMv2 answers are MS-NLMP §4.2.4's worked example (user `User`,
//! domain `Domain`, password `Password`, server challenge
//! `0123456789abcdef`, client challenge `aa…aa`, time zero). The HMAC-MD5
//! answers are RFC 2202's, and the signing key is §4.2.4's for the exported
//! session key `55…55`, cross-checked with `CPython`'s `hashlib`.

use super::{
    ntlm_checksum, ntlm_checksum_verify, ntlm_hmac, ntlm_hmac_verify, ntlm_signing_key, ntowfv2,
    NtlmDirection,
};
use crate::test_hex::hex;

/// `text` as UTF-16LE, the encoding NTLM hashes every string in.
fn utf16le<const N: usize>(text: &str) -> [u8; N] {
    let mut out = [0u8; N];
    let mut units = text.encode_utf16();
    for pair in out.as_chunks_mut::<2>().0 {
        *pair = units.next().expect("long enough").to_le_bytes();
    }
    assert!(units.next().is_none(), "exactly long enough");
    out
}

const SERVER_CHALLENGE: [u8; 8] = [0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef];

const CLIENT_CHALLENGE: [u8; 8] = [0xaa; 8];

fn response_key() -> [u8; 16] {
    ntowfv2(&utf16le::<16>("Password"), &utf16le::<20>("USERDomain"))
}

/// The NTLMv2 `temp` blob of the worked example: the versions, time zero,
/// the client challenge and the server's target info (`Domain`, `Server`).
fn temp() -> [u8; 68] {
    hex("0101000000000000\
         0000000000000000\
         aaaaaaaaaaaaaaaa\
         00000000\
         02000c0044006f006d00610069006e00\
         01000c00530065007200760065007200\
         00000000\
         00000000")
}

#[test]
fn ntowfv2_matches_the_worked_example() {
    assert_eq!(response_key(), hex("0c868a403bfd7a93a3001ef22ef02e3f"));
}

#[test]
fn the_ntlmv2_proof_and_session_base_key_match_the_worked_example() {
    let key = response_key();
    let proof = ntlm_hmac(&key, &[&SERVER_CHALLENGE, &temp()]);
    assert_eq!(proof, hex("68cd0ab851e51c96aabc927bebef6a1c"));
    assert_eq!(
        ntlm_hmac(&key, &[&proof]),
        hex("8de40ccadbc14a82f15cb0ad0de95ca3")
    );
}

#[test]
fn the_lmv2_response_matches_the_worked_example() {
    assert_eq!(
        ntlm_hmac(&response_key(), &[&SERVER_CHALLENGE, &CLIENT_CHALLENGE]),
        hex("86c35097ac9cec102554764a57cccc19")
    );
}

#[test]
fn hmac_md5_matches_rfc_2202() {
    assert_eq!(
        ntlm_hmac(&[0x0b; 16], &[b"Hi There"]),
        hex("9294727a3638bb1c13f48ef8158bfc9d")
    );
    assert_eq!(
        ntlm_hmac(&[0xaa; 16], &[&[0xdd; 50]]),
        hex("56be34521d144c88dbb8c733f0e8b3f6")
    );
}

#[test]
fn hmac_md5_parts_are_their_concatenation() {
    assert_eq!(
        ntlm_hmac(&[0xaa; 16], &[&[0xdd; 20], &[0xdd; 30]]),
        ntlm_hmac(&[0xaa; 16], &[&[0xdd; 50]])
    );
}

#[test]
fn hmac_md5_verification_refuses_any_other_value() {
    let tag = ntlm_hmac(&[0x0b; 16], &[b"Hi There"]);
    assert!(ntlm_hmac_verify(&[0x0b; 16], &[b"Hi There"], &tag));
    let mut forged = tag;
    forged[15] ^= 1;
    assert!(!ntlm_hmac_verify(&[0x0b; 16], &[b"Hi There"], &forged));
    assert!(!ntlm_hmac_verify(&[0x0b; 16], &[b"Hi there"], &tag));
}

#[test]
fn the_signing_key_matches_the_worked_example_and_depends_on_direction() {
    let exported = [0x55; 16];
    let client = ntlm_signing_key(&exported, NtlmDirection::ClientToServer);
    assert_eq!(client, hex("4788dc861b4782f35d43fd98fe1a2d39"));
    assert_ne!(
        ntlm_signing_key(&exported, NtlmDirection::ServerToClient),
        client
    );
}

#[test]
fn the_checksum_is_the_hmac_of_the_sequence_and_message_truncated() {
    let key = ntlm_signing_key(&[0x55; 16], NtlmDirection::ClientToServer);
    let message = b"Plaintext";
    let full = ntlm_hmac(&key, &[&7u32.to_le_bytes(), message]);
    let checksum = ntlm_checksum(&key, 7, message);
    assert_eq!(checksum, full[..8]);
    assert!(ntlm_checksum_verify(&key, 7, message, &checksum));
    assert!(!ntlm_checksum_verify(&key, 8, message, &checksum));
    assert!(!ntlm_checksum_verify(&key, 7, b"Plaintexu", &checksum));
}
