//! Toolchain-binary checksumming.
//!
//! Every external binary the wrapper invokes is hashed with the audited
//! SHA-256 from `lib/crypto` (never hand-roll crypto) so
//! the exact bytes that were run are recorded for the audit trail. When a caller (or the environment) supplies an
//! expected digest, the wrapper *verifies* it and fails closed on a mismatch; otherwise it records the computed digest for logging.

use tairix_crypto::{sha256, Sha256Digest, SHA256_OUTPUT_LEN};
use tairix_util::hex;

/// Compute the SHA-256 digest of `bytes`.
#[must_use]
pub fn digest(bytes: &[u8]) -> Sha256Digest {
    sha256(bytes)
}

/// Lowercase hex encoding of a 32-byte digest.
#[must_use]
pub fn to_hex(digest: &Sha256Digest) -> String {
    digest
        .iter()
        .flat_map(|&byte| hex::digits(byte))
        .map(char::from)
        .collect()
}

/// Parse a 64-character lowercase/uppercase hex string into a digest.
///
/// Returns `None` for any string that is not exactly 64 hex digits, so an
/// out-of-range or malformed pin fails closed rather than matching by
/// accident.
#[must_use]
pub fn parse_hex(text: &str) -> Option<Sha256Digest> {
    let mut out = [0u8; SHA256_OUTPUT_LEN];
    hex::decode_into(text.trim(), &mut out).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips() {
        let d = digest(b"");
        // SHA-256("") — FIPS 180-4 §A.1.
        assert_eq!(
            to_hex(&d),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(parse_hex(&to_hex(&d)), Some(d));
    }

    #[test]
    fn parse_rejects_wrong_length_and_non_hex() {
        assert_eq!(parse_hex("deadbeef"), None);
        assert_eq!(parse_hex(&"z".repeat(64)), None);
        assert_eq!(parse_hex(&"0".repeat(63)), None);
    }

    #[test]
    fn parse_is_case_insensitive_and_trims() {
        let upper = "E3B0C44298FC1C149AFBF4C8996FB92427AE41E4649B934CA495991B7852B855";
        assert_eq!(parse_hex(&format!("  {upper}  ")), Some(digest(b"")));
    }
}
