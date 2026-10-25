//! The `Run` entry-point binary of the `chmod` tool — the program a shell
//! spawns to change file mode bits.
//!
//! This is a **pure-Rust** program: TAIRiX is Rust-only, so it links the Rust
//! userland runtime `tairix-rt` — never the C ABI, which exists solely for
//! programs *not* written in Rust. `tairix-rt` provides `_start`, the
//! per-process stack canary, the panic handler, the `mem_map`-backed global
//! allocator, and the syscall wrappers; `tairix_rt::entry!` names this
//! program's `main`.
//!
//! `main` collects the inherited argument vector, reads the `LANG` locale
//! preference from the inherited environment (plans/APPS.md §5 — the shell
//! exports it; the tool invents no second source), and runs the parsed
//! command against the production seams: `tairix_rt::copy::WorkingDirectory`,
//! the shared tree seam over the kernel-authorised `fs_*` calls, through
//! whose descriptors every mode is read and set (the owner-only rule and
//! every per-inode and mount check stay kernel-side), the process's umask
//! read once, the shared `tairix_help::BundleHelp`, which reads the tool's
//! own bundle's `Help/` tree for the short-help switches, and `Stdout`, which
//! writes the `-v`/`-c` reports to the inherited standard output. The tool
//! binds only to its inherited descriptors, never a console device, and
//! holds no ambient authority.
//!
//! On the host it is an inert stub so `cargo build --workspace`, clippy, and
//! fmt still cover the file.

#![cfg_attr(all(freestanding, feature = "program"), no_std)]
#![cfg_attr(all(freestanding, feature = "program"), no_main)]
#![deny(missing_docs)]

// --- Pure-Rust program --------------------------------------------------
#[cfg(all(freestanding, feature = "program"))]
mod program {
    use tairix_chmod::{parse, run};
    use tairix_help::BundleHelp;
    use tairix_rt::copy::WorkingDirectory;
    use tairix_rt::io::{write_stderr_line, Stderr, Stdout};

    /// Program entry point. `tairix-rt`'s `_start` calls it once the runtime
    /// is set up and routes its return value through the `exit` syscall.
    ///
    /// Exit codes: `0` on success, `1` on a filesystem or output failure
    /// (silent for `-f`'s suppressed diagnostics — the silence is the very
    /// thing the user asked for; the status still reports the failure) or a
    /// command line not understood, an unparsable mode included, as GNU's
    /// `chmod` ends.
    fn main() -> i32 {
        let Some(arguments) = tairix_help::arguments("chmod") else {
            return tairix_help::MISUSE;
        };
        let command = match parse(&arguments) {
            Ok(command) => command,
            Err(err) => {
                tairix_help::report_misuse("chmod", &err);
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
            &BundleHelp::new("chmod"),
            &Stdout,
            &Stderr,
        ) {
            Ok(true) => 0,
            Ok(false) => 1,
            Err(err) => {
                write_stderr_line(format_args!("chmod: {err}"));
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
