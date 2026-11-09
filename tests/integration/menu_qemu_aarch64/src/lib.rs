//! The desktop-menu vertical's shared interaction contract
//! (`plans/NEW-MENUS.md` D17).
//!
//! The freestanding guest kernel (`src/main.rs`) and the host runner's
//! enrolment (`tools/xtask/src/commands/qemu_tests.rs`) both read these
//! definitions, so the gesture the host injects and the witness the guest
//! latches can never drift apart.
//!
//! Every stage is sequenced on a **uniquely attributable** witness — the exact
//! bundle a load names, an announcement only the desktop makes — never on a
//! cumulative event count an unrelated subsystem's cadence can shift under it
//! (`plans/OPEN-DEFECTS.md` D19/D20).
//!
//! # Who states what
//!
//! Only the session knows when a served window's pixels reached the display,
//! and only the session knows when a menu chain's plates did — an application
//! learns that its open was *accepted* and never that a plate was drawn. So
//! `tairix_desktop_session::WINDOW_SHOWN_MESSAGE` is what says "the window is
//! there to right-click" and `MENU_SHOWN_MESSAGE` is what says "the plate is
//! there to photograph and click": the host reads them off the serial
//! transcript, and the guest's gate off its diagnostic sink, where the kernel
//! delivers what userland emits. The load a bundle names is the kernel's own
//! audit record.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

/// Bare name of the application whose window menu the vertical opens — the
/// bundle is `<system application store>/<name>.app`, composed from the shared
/// `lib/abi` spellings on both sides rather than written out here.
///
/// The terminal, because it is the migrated surface: it keeps no menu shell of
/// its own, so a plate on its window is the desktop's service and nothing else,
/// and its *Settings…* row opens a further surface — a consequence a machine
/// can attest.
///
/// It **is** the icon-bar vertical's own subject rather than a second spelling
/// of it: the host launches through the one shared bar-launch reconstruction,
/// so the bundle this PASS gate waits for and the bundle that reconstruction
/// launches are equal by definition and are defined once.
pub use tairix_test_appbar_qemu_aarch64::BAR_APP_NAME as MENU_APP_NAME;
