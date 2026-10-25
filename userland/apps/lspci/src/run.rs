//! The `Run` entry-point binary of the `lspci` tool — the program a shell
//! spawns to list the discovered PCI/PCIe devices.
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
//! exports it; the tool invents no second source), loads the bundle's own
//! `Resources/pci.ids.bin` lookup table through the kernel-authorised `fs_*`
//! syscalls (a missing or corrupt table degrades the listing to numeric ids
//! with the reason on standard error — never a withheld inventory, never a
//! fabricated name), and runs the parsed command against the production
//! seams: the shared `tairix_procinfo::IpcTransport` for the `sysinfo-v1`
//! `HARDWARE_TREE` query, the shared `tairix_help::BundleHelp` for the
//! short-help switches, and `Stdout`/`Stderr`, which write the listing
//! to the inherited standard output (with the naming advisory on fd 3,
//! best-effort) and the diagnostics to standard error. The tool binds only
//! to its inherited descriptors, never a console device, and holds no
//! ambient authority.
//!
//! On the host it is an inert stub so `cargo build --workspace`, clippy, and
//! fmt still cover the file.

#![cfg_attr(all(freestanding, feature = "program"), no_std)]
#![cfg_attr(all(freestanding, feature = "program"), no_main)]
#![deny(missing_docs)]

// --- Pure-Rust program --------------------------------------------------
#[cfg(all(freestanding, feature = "program"))]
mod program {

    use tairix_devids::{DbKind, DevIds};
    use tairix_help::BundleHelp;
    use tairix_lspci::{parse, run};
    use tairix_procinfo::IpcTransport;
    use tairix_rt::io::{write_stderr_line, Stdout};

    /// The command word this bundle is named by.
    const OWN_WORD: &str = "lspci";

    /// Program entry point. `tairix-rt`'s `_start` calls it once the runtime
    /// is set up and routes its return value through the `exit` syscall.
    ///
    /// Exit codes: `0` on success, `1` when the query or output failed
    /// (including a refused `CAP_SYSINFO_HW` — the tool's whole purpose, so
    /// the reason lands on standard error and nothing is fabricated) or the
    /// command line was not understood.
    fn main() -> i32 {
        let Some(arguments) = tairix_help::arguments(OWN_WORD) else {
            return tairix_help::MISUSE;
        };
        let command = match parse(&arguments) {
            Ok(command) => command,
            Err(err) => {
                tairix_help::report_misuse(OWN_WORD, &err);
                return tairix_help::MISUSE;
            }
        };
        let locale = tairix_help::user_locale();
        // The bundle's own lookup table. A load or validation failure is a
        // lost naming aid, not a lost inventory: the reason goes to standard
        // error and the listing renders numeric ids.
        let table_bytes = match tairix_devids::bundled::load(DbKind::Pci, OWN_WORD) {
            Ok(bytes) => Some(bytes),
            Err(why) => {
                write_stderr_line(format_args!(
                    "lspci: device names unavailable ({why}); listing numeric ids"
                ));
                None
            }
        };
        let database = table_bytes.as_deref().and_then(|bytes| {
            match DevIds::parse(DbKind::Pci, bytes) {
                Ok(db) => Some(db),
                Err(err) => {
                    write_stderr_line(format_args!(
                        "lspci: device names unavailable (invalid table: {err}); listing numeric ids"
                    ));
                    None
                }
            }
        });
        match run(
            command,
            locale,
            &IpcTransport,
            database.as_ref(),
            &BundleHelp::new(OWN_WORD),
            &Stdout,
        ) {
            Ok(()) => 0,
            Err(err) => {
                write_stderr_line(format_args!("lspci: {err}"));
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
