//! The `Run` entry-point binary of the `mv` tool — the program a shell
//! spawns to move (rename) files and directories.
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
//! the shared tree seam over the kernel-authorised `fs_*` calls (every
//! per-inode and mount check stays kernel-side; the kernel's dedicated
//! `CrossVolume` refusal of a cross-mount rename drives the copy-then-remove
//! fallback), `RtStaging`, which draws the runtime's staging names,
//! `RtPrompt`, which asks the `-i` confirmation on standard error and reads
//! the reply from standard input (consent only on a leading `y`/`Y`; an
//! unreadable reply is never consent), the shared `tairix_help::BundleHelp`,
//! which reads the tool's own bundle's `Help/` tree for the short-help
//! switches, and `Stdout`, which writes `-v` reports to the inherited
//! standard output. The tool binds only to its inherited descriptors, never a
//! console device, and holds no ambient authority.
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
    use alloc::string::String;

    use tairix_abi::Errno;
    use tairix_hash::{BuildSipHash13, HashSeed};
    use tairix_help::BundleHelp;
    use tairix_mv::{parse, run, MvError, Prompt, Seams, Staging};
    use tairix_rt::copy::WorkingDirectory;
    use tairix_rt::io::{self, write_stderr_line, Read, Stderr, Stdin, Stdout, Write};

    /// Longest interactive reply chunk read per syscall; the reply is only
    /// ever judged by its first byte, so the bound caps work, not meaning.
    const REPLY_MAX: usize = 64;

    /// The runtime's staging names, which every staged replacement on the
    /// system is spelled by.
    struct RtStaging;

    impl Staging for RtStaging {
        fn staging_name(&self, name: &str) -> Result<String, Errno> {
            tairix_rt::staging_name(name)
        }
    }

    /// The production [`Prompt`] over the inherited standard streams: the
    /// question goes to fd 2 (so it is seen even when fd 1 is redirected)
    /// and the reply is read from fd 0, the GNU shape. Only a reply whose
    /// first byte is `y`/`Y` consents; end-of-input or an unreadable stream
    /// is never consent.
    struct RtPrompt;

    impl Prompt for RtPrompt {
        fn confirm(&self, question: &str) -> Result<bool, Errno> {
            Stderr
                .write_all(format!("mv: {question} ").as_bytes())
                .map_err(io::Error::as_errno)?;
            let mut first: Option<u8> = None;
            let mut buf = [0u8; REPLY_MAX];
            loop {
                let n = Stdin.read(&mut buf).map_err(io::Error::as_errno)?;
                if n == 0 {
                    break;
                }
                for &byte in &buf[..n] {
                    if byte == b'\n' {
                        return Ok(matches!(first, Some(b'y' | b'Y')));
                    }
                    if first.is_none() {
                        first = Some(byte);
                    }
                }
            }
            Ok(matches!(first, Some(b'y' | b'Y')))
        }
    }

    /// Program entry point. `tairix-rt`'s `_start` calls it once the runtime
    /// is set up and routes its return value through the `exit` syscall.
    ///
    /// Exit codes: `0` on success, `1` on a filesystem, prompt, or output
    /// failure or a command line not understood, as GNU's `mv` ends.
    fn main() -> i32 {
        let Some(arguments) = tairix_help::arguments("mv") else {
            return tairix_help::MISUSE;
        };
        let command = match parse(&arguments) {
            Ok(command) => command,
            Err(err) => {
                tairix_help::report_misuse("mv", &err);
                return tairix_help::MISUSE;
            }
        };
        let locale = tairix_help::user_locale();
        // The tool's own bundle's `Help/` tree, read through the shared
        // syscall-backed source for the short-help switches.
        let seams = Seams {
            fs: &WorkingDirectory,
            staging: &RtStaging,
            prompt: &RtPrompt,
            help: &BundleHelp::new("mv"),
            out: &Stdout,
            diagnostics: &Stderr,
            // Without a key the record is predictable, never absent.
            keys: BuildSipHash13::with_seed(tairix_rt::hash_seed().unwrap_or(HashSeed::UNKEYED)),
        };
        match run(command, locale, &seams) {
            Ok(true) => 0,
            Ok(false) => 1,
            Err(err @ MvError::Usage) => {
                tairix_help::report_misuse("mv", &err);
                tairix_help::MISUSE
            }
            Err(err) => {
                write_stderr_line(format_args!("mv: {err}"));
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
