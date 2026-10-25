//! The `Run` entry-point binary of the `mkdir` tool — the program a shell
//! spawns to make directories.
//!
//! This is a **pure-Rust** program: TAIRiX is Rust-only, so it links the Rust
//! userland runtime `tairix-rt` — never the C ABI, which exists solely for
//! programs *not* written in Rust. `tairix-rt` provides `_start`, the
//! per-process stack canary, the panic handler, the `mem_map`-backed global
//! allocator, and the syscall wrappers; `tairix_rt::entry!` names this
//! program's `main`.
//!
//! `main` collects the inherited argument vector, parses it, and makes each
//! operand through the shared tree seam (`tairix_rt::copy`), under the
//! caller's attested identity: each directory made is held, its mode read
//! back and set through it where `-m` or `-p` asks for bits the umask took
//! away, and a `-p` ancestor's child made in it. The
//! reserved `-h`/`-?`/`--help` short-help switches render the
//! tool's own Help document through the shared engine. The tool binds only
//! to its inherited descriptors, never a console device.
//!
//! On the host it is an inert stub so `cargo build --workspace`, clippy, and
//! fmt still cover the file.

#![cfg_attr(all(freestanding, feature = "program"), no_std)]
#![cfg_attr(all(freestanding, feature = "program"), no_main)]
#![deny(missing_docs)]

// --- Pure-Rust program --------------------------------------------------
#[cfg(all(freestanding, feature = "program"))]
mod program {

    use tairix_help::BundleHelp;
    use tairix_mkdir::{parse, run, MkdirError};
    use tairix_rt::copy::WorkingDirectory;
    use tairix_rt::io::{write_stderr_line, Stderr, Stdout};

    /// Program entry point. `tairix-rt`'s `_start` calls it once the runtime
    /// is set up and routes its return value through the `exit` syscall.
    ///
    /// Exit codes: `0` on success, `1` on a filesystem or output failure, a
    /// mode not understood, or a command line not understood, as GNU's
    /// `mkdir` ends.
    fn main() -> i32 {
        let Some(arguments) = tairix_help::arguments("mkdir") else {
            return tairix_help::MISUSE;
        };
        let command = match parse(&arguments) {
            Ok(command) => command,
            // GNU's `mkdir` names a bad mode alone, without pointing at
            // `--help`.
            Err(err @ MkdirError::BadMode(_)) => {
                write_stderr_line(format_args!("mkdir: {err}"));
                return 1;
            }
            Err(err) => {
                tairix_help::report_misuse("mkdir", &err);
                return tairix_help::MISUSE;
            }
        };
        let locale = tairix_help::user_locale();
        // The tool's own bundle's `Help/` tree, read through the shared
        // syscall-backed source for the short-help switches.
        match run(
            command,
            locale,
            &WorkingDirectory,
            tairix_rt::umask_get(),
            &BundleHelp::new("mkdir"),
            &Stdout,
            &Stderr,
        ) {
            Ok(true) => 0,
            Ok(false) => 1,
            Err(err) => {
                write_stderr_line(format_args!("mkdir: {err}"));
                1
            }
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
