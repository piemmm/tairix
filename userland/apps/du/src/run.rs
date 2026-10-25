//! The `Run` entry-point binary of the `du` tool — the program a shell
//! spawns to estimate file space usage.
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
//! command against the production seams: `RtWalk`, which stats paths and
//! reads directories through the kernel-authorised `fs_*` syscalls (every
//! per-inode and mount check stays kernel-side), the shared
//! `tairix_help::BundleHelp`, which reads the tool's own bundle's `Help/`
//! tree for the short-help switches, and `Stdout`/`Stderr`, which write
//! the usage rows to the inherited standard output and the diagnostics to
//! standard error. The tool binds only to its inherited descriptors, never
//! a console device, and holds no ambient authority.
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
    use alloc::vec::Vec;

    use tairix_abi::fs::DirEntries;
    use tairix_abi::Errno;
    use tairix_du::{parse, run, Entry, FinalLink, Metadata, Walk};
    use tairix_hash::{BuildSipHash13, HashSeed};
    use tairix_help::BundleHelp;
    use tairix_rt::io::{write_stderr_line, Stderr, Stdout};
    use tairix_rt::File;

    /// The production [`Walk`]: the kernel-authorised `fs_*` view of the
    /// filesystem. It adds no authority — every path resolution, per-inode
    /// permission, and mount-flag check happens kernel-side under the
    /// caller's attested identity, and a refusal surfaces as the exact
    /// [`Errno`] the kernel chose.
    struct RtWalk;

    impl Walk for RtWalk {
        fn stat(&self, path: &str, link: FinalLink) -> Result<Metadata, Errno> {
            let file =
                File::open(path.as_bytes(), link.resolve_only()).map_err(Errno::from_syscall)?;
            let stat = file.stat().map_err(Errno::from_syscall)?;
            Ok(Metadata {
                kind: stat.kind,
                size: stat.size,
                allocated: stat.allocated,
                id: stat.id,
                nlink: stat.nlink,
            })
        }

        fn read_dir(&self, path: &str) -> Result<Vec<Entry>, Errno> {
            let buf = tairix_rt::read_dir_all(path.as_bytes()).map_err(Errno::from_syscall)?;
            let mut entries = Vec::new();
            for entry in DirEntries::new(&buf) {
                let entry = entry?;
                // The ABI contract makes every entry name UTF-8; a name
                // that is not is a corrupt or hostile stream, refused whole
                // rather than silently dropped from the walk — `OutOfRange`,
                // the same errno the entry decoder itself uses for a field
                // outside its permitted domain.
                let name = core::str::from_utf8(entry.name).map_err(|_| Errno::OutOfRange)?;
                entries.push(Entry {
                    name: String::from(name),
                    meta: Metadata {
                        kind: entry.kind,
                        size: entry.size,
                        allocated: entry.allocated,
                        id: entry.id,
                        nlink: entry.nlink,
                    },
                });
            }
            Ok(entries)
        }
    }

    /// Program entry point. `tairix-rt`'s `_start` calls it once the runtime
    /// is set up and routes its return value through the `exit` syscall.
    ///
    /// Exit codes: `0` on success, `1` when a path was diagnosed, the
    /// output failed, or the command line was not understood.
    fn main() -> i32 {
        let Some(arguments) = tairix_help::arguments("du") else {
            return tairix_help::MISUSE;
        };
        let command = match parse(&arguments) {
            Ok(command) => command,
            Err(err) => {
                tairix_help::report_misuse("du", &err);
                return tairix_help::MISUSE;
            }
        };
        let locale = tairix_help::user_locale();
        // The tool's own bundle's `Help/` tree, read through the shared
        // syscall-backed source for the short-help switches.
        match run(
            command,
            locale,
            &RtWalk,
            &BundleHelp::new("du"),
            &Stdout,
            &Stderr,
            BuildSipHash13::with_seed(tairix_rt::hash_seed().unwrap_or(HashSeed::UNKEYED)),
        ) {
            Ok(true) => 0,
            Ok(false) => 1,
            Err(err) => {
                write_stderr_line(format_args!("du: {err}"));
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
