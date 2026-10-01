//! The `Run` entry-point binary of the `true` tool — the program a shell
//! spawns to do nothing, successfully.
//!
//! This is a **pure-Rust** program: TAIRiX is Rust-only, so it links the Rust
//! userland runtime `tairix-rt` — never the C ABI, which exists solely for
//! programs *not* written in Rust. `tairix-rt` provides `_start`, the
//! per-process stack canary, the panic handler, the `mem_map`-backed global
//! allocator, and the syscall wrappers; `tairix_rt::entry!` names this
//! program's `main`.
//!
//! `main` collects the inherited argument vector and exits `0`, ignoring
//! every argument exactly as GNU `true` does. The one exception is a first
//! argument of `-h`/`-?`/`--help` (the reserved short-help switches), which
//! renders the tool's own Help document through the shared engine. The tool
//! binds only to its inherited descriptors, never a console device.
//!
//! On the host it is an inert stub so `cargo build --workspace`, clippy, and
//! fmt still cover the file.

#![cfg_attr(all(freestanding, feature = "program"), no_std)]
#![cfg_attr(all(freestanding, feature = "program"), no_main)]
#![deny(missing_docs)]

// --- Pure-Rust program --------------------------------------------------
#[cfg(all(freestanding, feature = "program"))]
mod program {
    use tairix_true::{parse, Command, USAGE};

    /// Program entry point. `tairix-rt`'s `_start` calls it once the runtime
    /// is set up and routes its return value through the `exit` syscall.
    ///
    /// Exit codes: `0` always (the tool's whole purpose), except `1` when a
    /// requested short help could not be written.
    fn main() -> i32 {
        // A malformed (non-UTF-8) argument vector is ignored like any other
        // argument: GNU `true` has no failure mode, so neither does this one.
        let Some(arguments) = tairix_rt::args() else {
            return 0;
        };
        match parse(&arguments) {
            Command::Succeed => 0,
            Command::Help => tairix_help::print_own_short_help("true", Some(USAGE)),
        }
    }

    tairix_rt::entry!(main);
}

// --- Host stub ----------------------------------------------------------
//
// On the host (`cargo build --workspace`, clippy, fmt) the program's real
// entry — the freestanding `tairix-rt` `_start` path — is not compiled, so
// this inert `main` keeps the crate building under the host tooling. It
// performs no I/O.
#[cfg(not(all(freestanding, feature = "program")))]
fn main() {}
