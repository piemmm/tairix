//! The concurrent-pickers vertical's shared contract (`plans/APPWIN.md` AW5).
//!
//! The freestanding guest kernel (`src/main.rs`) and the host runner's
//! enrolment (`tools/xtask/src/commands/qemu_tests.rs`) both read these
//! definitions, so the gestures the host injects and the witnesses the guest
//! latches can never drift apart. Every principal and syscall is the
//! picker-delegation vertical's own (`tairix-test-filepick-qemu-aarch64`),
//! read rather than restated.
//!
//! # What the vertical proves that no host test can
//!
//! The session's table of picks is host-tested: each pick named by an id never
//! reused, bounded per window, application and session, cascaded, and
//! concluded on its own. What only a running machine can show is that two
//! pickers of one application are up together on the real desktop, each
//! holding only the window that asked, and that each concludes into that
//! window as a delegation the session mints and the viewer redeems.
//!
//! # Who states what
//!
//! The host gates each gesture on the session's own witnesses: its first
//! choice on the last picker's `PICKER_SHOWN`, so every picker was on screen
//! before any concluded. The guest's sink gates on the kernel's audit records:
//! [`PICKS`] chains of [`PICK_CHAIN`], the viewer's redeem made by one
//! kernel-attested task every time. It prints each step it latches
//! ([`PICK_STEP_MARKERS`]), so a run that does not pass names the missing hop.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use tairix_abi::window_ipc::PICK_MAX_OPEN;

/// Bare name of the application the library launches: the picture and
/// document viewer, which holds no filesystem capability of its own, so a file
/// reaching it is proof of the delegation.
pub const PICK_APP_NAME: &str = tairix_test_filepick_qemu_aarch64::PICK_APP_NAME;

/// Process name (`comm`) the kernel attests for the desktop session, which
/// mints each delegation.
pub const SESSION_COMM: &str = tairix_test_filepick_qemu_aarch64::SESSION_COMM;

/// Process name (`comm`) the kernel attests for the viewer, which redeems
/// each delegation.
pub const VIEWER_COMM: &str = tairix_test_filepick_qemu_aarch64::RECIPIENT_COMM;

/// Name of the syscall a delegation is minted with, as the syscall audit
/// field renders it.
pub const GRANT_SYSCALL: &str = tairix_test_filepick_qemu_aarch64::GRANT_SYSCALL;

/// Name of the syscall a delegation is redeemed with, as the syscall audit
/// field renders it.
pub const REDEEM_SYSCALL: &str = tairix_test_filepick_qemu_aarch64::REDEEM_SYSCALL;

/// Pickers the script holds up at once, one for each of the viewer's windows,
/// and so the delegations the gate wants.
///
/// Two, because two is the least that is concurrent: the later picker stands
/// above the earlier, and each must conclude on its own, into its own window.
pub const PICKS: u32 = 2;

// Each pick is one the viewer may have open with the others, or the later
// ones would be refused rather than shown.
const _: () = assert!(PICKS as usize <= PICK_MAX_OPEN);

/// One pick's conclusion as the audit trail shows it, in order: the session
/// mints a delegation, then the viewer redeems it. Each pair is `(comm, sc)` as
/// the kernel-attested audit fields render them.
pub const PICK_CHAIN: [(&str, &str); 2] =
    [(SESSION_COMM, GRANT_SYSCALL), (VIEWER_COMM, REDEEM_SYSCALL)];

/// Index in [`PICK_CHAIN`] of the viewer's redeem: the step whose attested
/// task names the instance every pick concludes into.
pub const VIEWER_REDEEM_STEP: usize = PICK_CHAIN.len() - 1;

/// What the guest prints as each step of [`PICK_CHAIN`] latches, in the same
/// order; a passing run prints the set [`PICKS`] times.
///
/// Sized from [`PICK_CHAIN`], so a step added without a witness to report it
/// does not compile.
pub const PICK_STEP_MARKERS: [&str; PICK_CHAIN.len()] = [
    "FILEPICKS desktop delegated a chosen file",
    "FILEPICKS viewer redeemed a chosen file",
];

/// What the guest prints when a redeem comes from a task other than the viewer
/// instance this run watches: the pick reached a second viewer rather than the
/// one both pickers were asked from, so it is not counted.
pub const FOREIGN_VIEWER_MARKER: &str = "FILEPICKS a foreign viewer redeemed; pick refused";

#[cfg(test)]
mod tests {
    use super::{
        GRANT_SYSCALL, PICK_APP_NAME, PICK_CHAIN, REDEEM_SYSCALL, SESSION_COMM, VIEWER_COMM,
        VIEWER_REDEEM_STEP,
    };

    /// A pick crosses from the session to a distinct principal, the viewer the
    /// library launches, and ends at that principal's redeem. Collapsed
    /// principals would latch both steps while proving no hand-off at all.
    #[test]
    fn a_pick_crosses_from_the_session_to_the_viewer_the_library_launches() {
        assert_ne!(SESSION_COMM, VIEWER_COMM);
        assert_eq!(VIEWER_COMM, PICK_APP_NAME);
        assert_eq!(PICK_CHAIN[0], (SESSION_COMM, GRANT_SYSCALL));
        assert_eq!(
            PICK_CHAIN[VIEWER_REDEEM_STEP],
            (VIEWER_COMM, REDEEM_SYSCALL)
        );
    }
}
