//! Shared host-test identity material.
//!
//! An [`AccountState::Active`](tairix_users::AccountState) record must be
//! login-shaped — home, shell, and a real stored password — so a fixture
//! modelling a human account cannot skip the password.

use tairix_users::StoredPassword;

/// The password [`shared_password`] verifies.
pub(crate) const SHARED_SECRET: &[u8] = b"correct horse";

/// [`SHARED_SECRET`]'s stored record under a `0x42` salt at the minimum
/// accepted cost.
///
/// Carried rather than derived: a derivation costs minutes under the UB
/// oracle, and a fixture needs the record, not the work of making it.
const SHARED_RECORD: &str = "pbkdf2-sha256$1000$42424242424242424242424242424242$\
                             ffcd5f876e3c20b48baefb6a808b7f5cc4bb3e2fd0325981a6d09c9fc6e01739";

/// The stored password every host-test human account carries.
pub(crate) fn shared_password() -> StoredPassword {
    StoredPassword::decode(SHARED_RECORD).expect("the fixture record decodes")
}
