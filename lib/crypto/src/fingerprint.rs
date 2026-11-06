//! Identity fingerprints.
//!
//! A fingerprint is the leading [`FINGERPRINT_LEN`] bytes of a SHA-256 over a
//! domain tag and the fields that follow it: a construction over the audited
//! [`crate::hash`] wrapper, not a primitive of its own. It is compared as
//! proof — a medium's held writes are replayed only onto a device whose
//! fingerprint is the one recorded — so two things sharing one must be
//! infeasible to find, not merely unlikely.

use crate::hash::Sha256Stream;

/// Bytes of a fingerprint: the leading half of a SHA-256 digest.
pub const FINGERPRINT_LEN: usize = 16;

/// A fingerprint being taken: a domain, then fields.
///
/// The domain names the kind of thing fingerprinted, so things of different
/// kinds never share a fingerprint; it is fed as its bytes, so no domain may
/// be a prefix of another. Fields are fed as their bytes too, and framing them
/// is the caller's: a fixed-width field as itself and a variable-length one
/// after its length, so no two field lists feed the same bytes.
pub struct Fingerprint {
    digest: Sha256Stream,
}

impl Fingerprint {
    /// Begin the fingerprint of a thing of the kind `domain` names.
    #[must_use]
    pub fn new(domain: &[u8]) -> Self {
        let mut digest = Sha256Stream::new();
        digest.update(domain);
        Self { digest }
    }

    /// Feed the next field.
    pub fn field(&mut self, bytes: &[u8]) {
        self.digest.update(bytes);
    }

    /// The fingerprint of the domain and every field fed.
    #[must_use]
    pub fn finish(self) -> [u8; FINGERPRINT_LEN] {
        let digest = self.digest.finalize();
        let mut fingerprint = [0u8; FINGERPRINT_LEN];
        fingerprint.copy_from_slice(&digest[..FINGERPRINT_LEN]);
        fingerprint
    }
}

#[cfg(test)]
#[path = "fingerprint_tests.rs"]
mod tests;
