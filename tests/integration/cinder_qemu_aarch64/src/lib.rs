//! The Cinder companion vertical's shared contract (`plans/CINDER.md` CD12).
//!
//! The freestanding guest kernel (`src/main.rs`) and the host runner's
//! enrolment (`tools/xtask/src/commands/qemu_tests.rs`) both read these
//! definitions, so what the host launches and what the guest latches cannot
//! drift apart.
//!
//! # Who states what
//!
//! - The **host** reads the serial transcript, so it gates each click and
//!   screendump on the desktop session's own announcements: the pen on
//!   screen, the companion's layer surface opened and on screen.
//! - The **guest** kernel's audit sink sees kernel audit records only, so it
//!   gates on the bundles the program library loads.
//!
//! Neither side infers the other's facts.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

/// Bare name of the companion's bundle in the system application store.
pub const COMPANION_APP_NAME: &str = "cinder";

/// Bare name of the application the host opens once the companion has been
/// photographed: its load is the guest's last witness. The terminal, the
/// icon-bar vertical's own subject, defined there once.
pub use tairix_test_appbar_qemu_aarch64::BAR_APP_NAME as THEN_APP_NAME;
