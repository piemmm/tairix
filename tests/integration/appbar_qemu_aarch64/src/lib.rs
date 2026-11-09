//! The icon-bar vertical's shared interaction contract
//! (`plans/NEW-TASKBAR.md`).
//!
//! The freestanding guest kernel (`src/main.rs`) and the host runner's
//! enrolment (`tools/xtask/src/commands/qemu_tests.rs`) both read these
//! definitions, so the gesture the host injects and the witness the guest
//! latches can never drift apart.
//!
//! Every stage is sequenced on a **uniquely attributable** witness — the
//! exact bundle a load names, an announcement only the desktop makes — never
//! on a cumulative event count that an unrelated subsystem's cadence can shift
//! under it (`plans/OPEN-DEFECTS.md` D19/D20).
//!
//! # Who states what
//!
//! Only the session knows when a served window's pixels reached the display,
//! so `tairix_desktop_session::WINDOW_SHOWN_MESSAGE` is what says "there is
//! now something on screen worth reading" — a screendump taken on anything
//! earlier is a race against the frame. The host reads it off the serial
//! transcript, and the guest's gate off its diagnostic sink, where the kernel
//! delivers what userland emits; the load a bundle names is the kernel's own
//! audit record.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

/// Bare name of the application whose icon-bar slot the vertical drives —
/// the bundle is `<system application store>/<name>.app`, composed from the
/// shared `lib/abi` spellings on both sides rather than written out here.
///
/// The terminal, because it is the application whose declaration has rows
/// with a *visible consequence a machine can attest*: both its *New window*
/// row and its primary-click default action make it open another window in
/// the **same** process, which no other gesture in this vertical can
/// produce.
pub const BAR_APP_NAME: &str = "terminal";

/// Number of windows the application has opened once the vertical's whole
/// script has been delivered: the one it opens on launch, the one the chosen
/// *New window* menu row opens, and the one the slot's primary click opens
/// through the declared default action.
///
/// Each is a window the desktop records first shown, and in this world the
/// launched application is the only client that opens one — the desktop's own
/// surfaces (the bar, the launcher popup, the menu, the hover picker, the
/// desktop icons) are session-painted and never call the window channel. So
/// the count is attributable act by act, and reaching it is the guest's PASS.
pub const WINDOWS_OPENED: usize = 3;
