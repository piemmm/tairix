//! The `TAIRiXBridge.app` bundle's `Run`.
//!
//! Started by the user it is the front end: each session's window, each
//! context's screen, the settings, and the relays the sessions' workers ask
//! for. Re-entered through `SPAWN_SELF` in the session-worker role it is one
//! session, in a canonical sandbox holding nothing but its two pipes and the
//! spawn share its screens are drawn into (`plans/RISCOS-EMULATOR.md` §15).
//!
//! Nothing that waits runs on the loop that owes the windows their frames:
//! the file worker serves every session's HostFS and starts each session's
//! worker, the font worker carries glyph requests to the font service and
//! works out the desktop font, and the settings writer commits the settings.
//! The loop asks, and collects what lands.
//!
//! On the host it is an inert stub.

#![cfg_attr(freestanding, no_std)]
#![cfg_attr(freestanding, no_main)]
#![deny(missing_docs)]

#[cfg(freestanding)]
extern crate alloc;

#[cfg(freestanding)]
#[path = "run/files.rs"]
mod files;
#[cfg(freestanding)]
#[path = "run/fonts.rs"]
mod fonts;
#[cfg(freestanding)]
#[path = "run/front.rs"]
mod front;
#[cfg(freestanding)]
#[path = "run/role.rs"]
mod role;
#[cfg(freestanding)]
#[path = "run/screen.rs"]
mod screen;
#[cfg(freestanding)]
#[path = "run/session.rs"]
mod session;

#[cfg(freestanding)]
tairix_rt::entry!(main);

#[cfg(freestanding)]
fn main() -> i32 {
    if tairix_sandbox::rt::session_worker_role() {
        return role::serve();
    }
    if tairix_rt::arg(1).is_some_and(|arg| matches!(arg, b"-h" | b"--help" | b"-?")) {
        return tairix_help::print_own_short_help(front::APP_NAME, None);
    }
    front::run()
}

#[cfg(not(freestanding))]
fn main() {}
