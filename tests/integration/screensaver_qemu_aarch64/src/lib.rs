//! The screensaver vertical's shared contract (`plans/SCREENSAVERS.md` SV10).
//!
//! The freestanding guest kernel (`src/main.rs`) and the host runner's
//! enrolment (`tools/xtask/src/commands/qemu_tests.rs`) both read these
//! definitions, so the savers the host plants and the ones the guest latches
//! cannot drift apart.
//!
//! The host plants two savers in the account's own store, both the system
//! Clock's program under a manifest of their own signed by a key of the
//! user's own: one asks for no feed, one for the machine feed only the
//! system's own savers are given. The desktop's record ids are generated
//! from the session's own definitions at build time.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

/// The identifier of the account's own saver that asks for no feed: the one
/// the script chooses and previews.
pub const GOOD_SAVER: &str = "org.example.saver.good";

/// Its bundle's name, `<name>.app` in the account's own store.
pub const GOOD_SAVER_NAME: &str = "Good";

/// What the chooser shows it as.
pub const GOOD_SAVER_TITLE: &str = "Good Clock";

/// The identifier of the account's own saver that asks for the machine feed:
/// the one the desktop refuses.
pub const ROGUE_SAVER: &str = "org.example.saver.rogue";

/// Its bundle's name.
pub const ROGUE_SAVER_NAME: &str = "Rogue";

/// What it calls itself.
pub const ROGUE_SAVER_TITLE: &str = "Rogue Clock";

include!(concat!(env!("OUT_DIR"), "/contract.rs"));

/// Whether `bundle` is the account's own saver bundle `name`.
#[must_use]
pub fn is_users_saver(bundle: &str, name: &str) -> bool {
    tairix_abi::is_user_saver_bundle(bundle)
        && bundle
            .rsplit_once('/')
            .and_then(|(_, leaf)| leaf.strip_suffix(tairix_abi::BUNDLE_SUFFIX))
            .is_some_and(|leaf| leaf == name)
}

#[cfg(test)]
mod tests {
    use super::{is_users_saver, GOOD_SAVER_NAME};

    /// Only a bundle in an account's own store, by its own name, matches.
    #[test]
    fn only_the_accounts_own_bundle_matches() {
        assert!(is_users_saver(
            "/Users/root/Library/Screensavers/Good.app",
            GOOD_SAVER_NAME
        ));
        assert!(!is_users_saver(
            "/System/Screensavers/Good.app",
            GOOD_SAVER_NAME
        ));
        assert!(!is_users_saver(
            "/Users/root/Library/Screensavers/Goodish.app",
            GOOD_SAVER_NAME
        ));
        assert!(!is_users_saver(
            "/Users/root/Library/Screensavers/Good",
            GOOD_SAVER_NAME
        ));
    }
}
