//! Unit tests for the identity fingerprint, against the audited SHA-256 it is
//! taken from.

use super::{Fingerprint, FINGERPRINT_LEN};
use crate::hash::sha256;

/// The leading [`FINGERPRINT_LEN`] bytes of the SHA-256 of `message`.
fn leading(message: &[u8]) -> [u8; FINGERPRINT_LEN] {
    let mut out = [0u8; FINGERPRINT_LEN];
    out.copy_from_slice(&sha256(message)[..FINGERPRINT_LEN]);
    out
}

#[test]
fn a_fingerprint_is_the_leading_bytes_of_sha256_over_its_domain_and_fields() {
    let mut fingerprint = Fingerprint::new(b"a");
    fingerprint.field(b"b");
    fingerprint.field(b"c");
    assert_eq!(fingerprint.finish(), leading(b"abc"));
    assert_eq!(
        Fingerprint::new(b"abc").finish(),
        leading(b"abc"),
        "a domain no field follows is fingerprinted alone"
    );
}

#[test]
fn fields_spanning_several_blocks_fingerprint_their_whole_message() {
    let mut message = [0u8; 200];
    for (i, byte) in message.iter_mut().enumerate() {
        *byte = u8::try_from(i).expect("a 200-byte message is indexed in a byte");
    }
    let (domain, fields) = message.split_at(19);
    let mut fingerprint = Fingerprint::new(domain);
    for field in fields.chunks(7) {
        fingerprint.field(field);
    }
    assert_eq!(fingerprint.finish(), leading(&message));
}

#[test]
fn one_field_list_fingerprints_apart_under_distinct_domains() {
    let mut first = Fingerprint::new(b"tairix.test.first");
    first.field(&[0x5A; 32]);
    let mut second = Fingerprint::new(b"tairix.test.second");
    second.field(&[0x5A; 32]);
    assert_ne!(first.finish(), second.finish());
}
