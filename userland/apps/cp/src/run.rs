//! The `Run` entry-point binary of the `cp` tool — the program a shell
//! spawns to copy files and directories.
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
//! command against the production seams: `RtFileSystem`, the shared tree seam
//! over the kernel-authorised `fs_*` calls (`tairix_rt::copy::WorkingDirectory`)
//! with what `cp` needs beyond it — an operand opened through its final link,
//! a resolved spelling, a link put in a name's place in one step — every
//! per-inode and mount check staying kernel-side; `RtPrompt`, which asks the
//! `-i` confirmation on standard error and reads the reply from standard
//! input (consent only on a leading `y`/`Y`; an unreadable reply is never
//! consent); the shared `tairix_help::BundleHelp`, which reads the tool's own
//! bundle's `Help/` tree for the short-help switches; and `Stdout`, which
//! writes `-v` reports to the inherited standard output. The tool binds only
//! to its inherited descriptors, never a console device, and holds no ambient
//! authority.
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

    use tairix_abi::{Errno, LinkFlags, OpenFlags, RealpathMode};
    use tairix_cp::{parse, run, CpError, FileSystem, Prompt, Roots, Seams};
    use tairix_hash::{BuildSipHash13, HashSeed};
    use tairix_help::BundleHelp;
    use tairix_rt::copy::WorkingDirectory;
    use tairix_rt::io::{self, write_stderr_line, Read, Stderr, Stdin, Stdout, Write};
    use tairix_rt::{Dir, File};

    /// Longest interactive reply chunk read per syscall; the reply is only
    /// ever judged by its first byte, so the bound caps work, not meaning.
    const REPLY_MAX: usize = 64;

    /// The production [`FileSystem`]: the working directory's shared tree
    /// seam, and an operand reached through its final link beside it. It
    /// adds no authority — every resolution, per-inode permission and
    /// mount-flag check happens kernel-side under the caller's attested
    /// identity, and a refusal surfaces as the exact [`Errno`] the kernel
    /// chose.
    struct RtFileSystem;

    impl Roots for RtFileSystem {
        type Dir = Dir;

        fn open_dir(&self, path: &str, follow: bool) -> Result<Dir, Errno> {
            WorkingDirectory.open_dir(path, follow)
        }

        fn parent(&self, path: &str) -> Result<(Dir, String), Errno> {
            WorkingDirectory.parent(path)
        }

        fn open_node(&self, path: &str, follow: bool) -> Result<File, Errno> {
            WorkingDirectory.open_node(path, follow)
        }
    }

    impl FileSystem for RtFileSystem {
        fn open_source(&self, path: &str) -> Result<File, Errno> {
            File::open(path.as_bytes(), OpenFlags::READ).map_err(Errno::from_syscall)
        }

        fn truncate_existing(&self, path: &str) -> Result<File, Errno> {
            File::open(path.as_bytes(), OpenFlags::WRITE.union(OpenFlags::TRUNCATE))
                .map_err(Errno::from_syscall)
        }

        fn canonical(&self, path: &str) -> Result<String, Errno> {
            tairix_rt::realpath(path, RealpathMode::Final)
        }

        fn symlink(&self, target: &str, link: &str, replace: bool) -> Result<(), Errno> {
            if replace {
                return tairix_rt::replace_with_link(link, tairix_rt::NewLink::Symbolic(target));
            }
            match tairix_rt::fs_symlink(target.as_bytes(), link.as_bytes()) {
                0 => Ok(()),
                ret => Err(Errno::from_syscall(ret)),
            }
        }

        fn link(&self, existing: &str, new: &str, replace: bool) -> Result<(), Errno> {
            // The node that gains a name is the one spelled — POSIX
            // `link()` — so a link at `existing` cannot redirect the new
            // name at what it points to.
            if replace {
                let link = tairix_rt::NewLink::Hard {
                    existing,
                    follow: false,
                };
                return tairix_rt::replace_with_link(new, link);
            }
            match tairix_rt::fs_link(existing.as_bytes(), new.as_bytes(), LinkFlags::empty()) {
                0 => Ok(()),
                ret => Err(Errno::from_syscall(ret)),
            }
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
                .write_all(format!("cp: {question} ").as_bytes())
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
    /// failure or a command line not understood, as GNU's `cp` ends.
    fn main() -> i32 {
        let Some(arguments) = tairix_help::arguments("cp") else {
            return tairix_help::MISUSE;
        };
        let command = match parse(&arguments) {
            Ok(command) => command,
            Err(err) => {
                tairix_help::report_misuse("cp", &err);
                return tairix_help::MISUSE;
            }
        };
        let locale = tairix_help::user_locale();
        // The tool's own bundle's `Help/` tree, read through the shared
        // syscall-backed source for the short-help switches.
        let seams = Seams {
            fs: &RtFileSystem,
            prompt: &RtPrompt,
            help: &BundleHelp::new("cp"),
            out: &Stdout,
            diagnostics: &Stderr,
            // Without a key the records are predictable, never absent.
            keys: BuildSipHash13::with_seed(tairix_rt::hash_seed().unwrap_or(HashSeed::UNKEYED)),
        };
        match run(command, locale, &seams) {
            Ok(true) => 0,
            Ok(false) => 1,
            Err(err @ CpError::Usage) => {
                tairix_help::report_misuse("cp", &err);
                tairix_help::MISUSE
            }
            Err(err) => {
                write_stderr_line(format_args!("cp: {err}"));
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
