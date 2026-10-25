//! The `Run` entry-point binary of the `unlink` tool — the program a shell
//! spawns to remove one name.
//!
//! This is a **pure-Rust** program: TAIRiX is Rust-only, so it links the Rust
//! userland runtime `tairix-rt` — never the C ABI, which exists solely for
//! programs *not* written in Rust. `tairix-rt` provides `_start`, the
//! per-process stack canary, the panic handler, the `mem_map`-backed global
//! allocator, and the syscall wrappers; `tairix_rt::entry!` names this
//! program's `main`.
//!
//! `main` collects the inherited argument vector, parses it, and removes the
//! one operand through `fs_unlink` under the caller's attested identity. The
//! empty flag word asks for a **non-directory** removal, so the kernel
//! refuses a directory in the same locked walk that would have removed the
//! entry — this program never races a stat against a removal, and never
//! follows a link to decide what to remove. The reserved `-?`/`--help`
//! switches render the tool's own Help document through the shared engine.
//! The tool binds only to its inherited descriptors, never a console device.
//!
//! On the host it is an inert stub so `cargo build --workspace`, clippy, and
//! fmt still cover the file.

#![cfg_attr(all(freestanding, feature = "program"), no_std)]
#![cfg_attr(all(freestanding, feature = "program"), no_main)]
#![deny(missing_docs)]

// --- Pure-Rust program --------------------------------------------------
#[cfg(all(freestanding, feature = "program"))]
mod program {

    use tairix_abi::{Errno, UnlinkFlags};
    use tairix_help::BundleHelp;
    use tairix_rt::io::{write_stderr_line, Stdout};
    use tairix_unlink::{parse, run, Filesystem};

    /// The production [`Filesystem`] over `fs_unlink`.
    ///
    /// The empty flag word is the name-removal posture: a directory is
    /// refused by the kernel, and the final component is never followed, so
    /// a symbolic link is removed itself rather than its target.
    struct RtFilesystem;

    impl Filesystem for RtFilesystem {
        fn unlink(&self, path: &str) -> Result<(), Errno> {
            let ret = tairix_rt::fs_unlink(path.as_bytes(), UnlinkFlags::empty());
            if ret != 0 {
                return Err(Errno::from_syscall(ret));
            }
            Ok(())
        }
    }

    /// Program entry point. `tairix-rt`'s `_start` calls it once the runtime
    /// is set up and routes its return value through the `exit` syscall.
    ///
    /// Exit codes: `0` on success, `1` on a filesystem or output failure or
    /// a command line not understood, as GNU's `unlink` ends.
    fn main() -> i32 {
        let Some(arguments) = tairix_help::arguments("unlink") else {
            return tairix_help::MISUSE;
        };
        let command = match parse(&arguments) {
            Ok(command) => command,
            Err(err) => {
                tairix_help::report_misuse("unlink", &err);
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
            &BundleHelp::new("unlink"),
            &Stdout,
        ) {
            Ok(()) => 0,
            Err(err) => {
                write_stderr_line(format_args!("unlink: {err}"));
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
