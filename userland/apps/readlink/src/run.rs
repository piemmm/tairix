//! The `Run` entry-point binary of the `readlink` tool — the program a
//! shell spawns to print a symbolic link's target.
//!
//! This is a **pure-Rust** program: TAIRiX is Rust-only, so it links the Rust
//! userland runtime `tairix-rt` — never the C ABI, which exists solely for
//! programs *not* written in Rust. `tairix-rt` provides `_start`, the
//! per-process stack canary, the panic handler, the `mem_map`-backed global
//! allocator, and the syscall wrappers; `tairix_rt::entry!` names this
//! program's `main`.
//!
//! `main` collects the inherited argument vector, parses it, and reads each
//! operand's stored target through `fs_readlink` — or, under `-f`/`-e`/`-m`,
//! its canonical path through `fs_realpath` — under the caller's attested
//! identity. Without a canonicalisation switch the final component is never
//! followed, so the answer is the link's own target rather than a resolution
//! of it; with one, the kernel's own resolution answers. The reserved
//! `-?`/`--help` switches render the tool's own Help document through the
//! shared engine. The tool binds only to its inherited descriptors, never a
//! console device.
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

    use alloc::string::String;

    use tairix_abi::{Errno, RealpathMode};
    use tairix_help::BundleHelp;
    use tairix_readlink::{parse, run, Filesystem};
    use tairix_rt::io::{write_stderr_line, Stderr, Stdout};

    /// The production [`Filesystem`] over `fs_readlink` and `fs_realpath`.
    struct RtFilesystem;

    impl Filesystem for RtFilesystem {
        fn read_link(&self, path: &str) -> Result<String, Errno> {
            tairix_rt::readlink(path)
        }

        fn realpath(&self, path: &str, mode: RealpathMode) -> Result<String, Errno> {
            tairix_rt::realpath(path, mode)
        }
    }

    /// Program entry point. `tairix-rt`'s `_start` calls it once the runtime
    /// is set up and routes its return value through the `exit` syscall.
    ///
    /// Exit codes: `0` when every target was printed, `1` when a read was
    /// refused, the output failed or the command line was not understood,
    /// as GNU's `readlink` ends.
    fn main() -> i32 {
        let Some(arguments) = tairix_help::arguments("readlink") else {
            return tairix_help::MISUSE;
        };
        let command = match parse(&arguments) {
            Ok(command) => command,
            Err(err) => {
                tairix_help::report_misuse("readlink", &err);
                return tairix_help::MISUSE;
            }
        };
        let locale = tairix_help::user_locale();
        // The tool's own bundle's `Help/` tree, read through the shared
        // syscall-backed source for the short-help switches.
        match run(
            command,
            locale,
            &RtFilesystem,
            &BundleHelp::new("readlink"),
            &Stdout,
            &Stderr,
        ) {
            Ok(true) => 0,
            Ok(false) => 1,
            Err(err) => {
                write_stderr_line(format_args!("readlink: {err}"));
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
