//! The `Run` entry-point binary of the `df` tool — the program a shell
//! spawns to report filesystem space usage.
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
//! command against the production seams: the shared
//! `tairix_procinfo::IpcTransport` for the `sysinfo-v1` list queries,
//! `RtProbe`, which confirms a `file` operand through the kernel-authorised
//! `fs_*` syscalls (every per-inode and mount check stays kernel-side), the
//! shared `tairix_help::BundleHelp` for the short-help switches, and
//! `Stdout`/`Stderr`, which write the table to the inherited standard
//! output (with the omission advisory on fd 3, best-effort) and the
//! diagnostics to standard error. The tool binds only to its inherited
//! descriptors, never a console device, and holds no ambient authority.
//!
//! On the host it is an inert stub so `cargo build --workspace`, clippy, and
//! fmt still cover the file.

#![cfg_attr(all(freestanding, feature = "program"), no_std)]
#![cfg_attr(all(freestanding, feature = "program"), no_main)]
#![deny(missing_docs)]

// --- Pure-Rust program --------------------------------------------------
#[cfg(all(freestanding, feature = "program"))]
mod program {

    use tairix_abi::fs::OpenFlags;
    use tairix_abi::Errno;
    use tairix_df::{parse, run, PathProbe};
    use tairix_help::BundleHelp;
    use tairix_procinfo::IpcTransport;
    use tairix_rt::io::{write_stderr_line, Stderr, Stdout};
    use tairix_rt::File;

    /// The production [`PathProbe`]: a resolve-only open through the
    /// kernel-authorised `fs_*` syscalls. It adds no authority — every
    /// path resolution, per-inode permission, and mount-flag check
    /// happens kernel-side under the caller's attested identity, and a
    /// refusal surfaces as the exact [`Errno`] the kernel chose.
    struct RtProbe;

    impl PathProbe for RtProbe {
        fn probe(&self, path: &str) -> Result<(), Errno> {
            // No read authority is requested; the handle is closed on
            // drop, and only the node's existence is learned.
            File::open(path.as_bytes(), OpenFlags::empty())
                .map(|_| ())
                .map_err(Errno::from_syscall)
        }
    }

    /// Program entry point. `tairix-rt`'s `_start` calls it once the runtime
    /// is set up and routes its return value through the `exit` syscall.
    ///
    /// Exit codes: `0` on success, `1` when an operand was diagnosed, the
    /// filters left nothing, the query/output failed, or the command line
    /// was not understood.
    fn main() -> i32 {
        let Some(arguments) = tairix_help::arguments("df") else {
            return tairix_help::MISUSE;
        };
        let command = match parse(&arguments) {
            Ok(command) => command,
            Err(err) => {
                tairix_help::report_misuse("df", &err);
                return tairix_help::MISUSE;
            }
        };
        let locale = tairix_help::user_locale();
        // The tool's own bundle's `Help/` tree, read through the shared
        // syscall-backed source for the short-help switches.
        match run(
            command,
            locale,
            &IpcTransport,
            &RtProbe,
            &BundleHelp::new("df"),
            &Stdout,
            &Stderr,
        ) {
            Ok(true) => 0,
            Ok(false) => 1,
            Err(err) => {
                write_stderr_line(format_args!("df: {err}"));
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
