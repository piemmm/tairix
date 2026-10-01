//! The `Run` entry-point binary of the `dirname` tool — the program a
//! shell spawns to strip the last component from names.
//!
//! This is a **pure-Rust** program: TAIRiX is Rust-only, so it links the Rust
//! userland runtime `tairix-rt` — never the C ABI, which exists solely for
//! programs *not* written in Rust. `tairix-rt` provides `_start`, the
//! per-process stack canary, the panic handler, the `mem_map`-backed global
//! allocator, and the syscall wrappers; `tairix_rt::entry!` names this
//! program's `main`.
//!
//! `main` collects the inherited argument vector, performs the purely
//! lexical directory-name surgery ([`tairix_dirname`] — no operand path is
//! resolved or touched on disk), and writes the results to the inherited
//! standard output (fd 1). The reserved `-h`/`-?`/`--help` short-help
//! switches render the tool's own Help document through the shared engine.
//! The tool binds only to its inherited descriptors, never a console
//! device.
//!
//! On the host it is an inert stub so `cargo build --workspace`, clippy, and
//! fmt still cover the file.

#![cfg_attr(all(freestanding, feature = "program"), no_std)]
#![cfg_attr(all(freestanding, feature = "program"), no_main)]
#![deny(missing_docs)]

// --- Pure-Rust program --------------------------------------------------
#[cfg(all(freestanding, feature = "program"))]
mod program {
    extern crate alloc;

    use alloc::format;

    use tairix_dirname::{output, parse, Command, USAGE};
    use tairix_rt::io::{write_stderr_line, Stdout, Write};

    /// Program entry point. `tairix-rt`'s `_start` calls it once the runtime
    /// is set up and routes its return value through the `exit` syscall.
    ///
    /// Exit codes: `0` when the results (or short help) were written, `1`
    /// when the output could not be delivered, `2` on a usage error.
    fn main() -> i32 {
        // A malformed (non-UTF-8) argument vector is a usage error, reported
        // rather than guessed at.
        let Some(arguments) = tairix_rt::args() else {
            write_stderr_line(USAGE);
            return 2;
        };
        let (names, zero) = match parse(&arguments) {
            Ok(Command::Emit { names, zero }) => (names, zero),
            Ok(Command::Help) => return tairix_help::print_own_short_help("dirname", Some(USAGE)),
            Err(err) => {
                write_stderr_line(&format!("dirname: {err}"));
                write_stderr_line(USAGE);
                return 2;
            }
        };
        match Stdout.write_all(&output(&names, zero)) {
            Ok(()) => 0,
            Err(_) => 1,
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
