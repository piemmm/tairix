//! The `Run` entry-point binary of the `configure` tool — the program a
//! shell spawns to read and set the boot-time system-configuration store.
//!
//! This is a **pure-Rust** program: TAIRiX is Rust-only, so it links the Rust
//! userland runtime `tairix-rt` — never the C ABI, which exists solely for
//! programs *not* written in Rust. `tairix-rt` provides `_start`, the
//! per-process stack canary, the panic handler, the `mem_map`-backed global
//! allocator, and the syscall wrappers; `tairix_rt::entry!` names this
//! program's `main`.
//!
//! `main` collects the inherited argument vector, reads the `LANG` locale
//! preference from the inherited environment (plans/APPS.md §5), parses the
//! arguments with the pure [`tairix_configure`] grammar, and runs the
//! resulting command against the production seams: the machine
//! configuration's two layers, read through the secured VFS under the
//! caller's attested identity; the settings service, which reads the network
//! configuration and makes every change, admitting only a caller holding
//! `CAP_SETTINGS_WRITE`; the network stack's capability-gated admin endpoint
//! a live change is pushed over; the shared `tairix_help::BundleHelp` for the
//! short-help switches; and the inherited standard output (fd 1). The tool
//! adds no authority, and binds only to its inherited descriptors, never a
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
    use alloc::vec::Vec;

    use tairix_abi::driver_store::SystemConfigFile;
    use tairix_abi::net_ipc::{
        NetBondConfigMsg, NetInterfaceConfigMsg, NetstackRequest, NetworkSettings,
        NETSTACK_ENDPOINT,
    };
    use tairix_abi::reply::{decode_status_reply, STATUS_REPLY_LEN};
    use tairix_abi::settings_ipc::{
        decode_reply, encode_read, encode_set, set_request_len, READ_REQUEST_LEN,
        SETTINGS_ENDPOINT, SETTINGS_REPLY_MAX,
    };
    use tairix_abi::Errno;
    use tairix_configure::{parse, run, ConfigureError, Documents, NetPolicy};
    use tairix_help::BundleHelp;
    use tairix_rt::io::{write_stderr_line, Stderr, Stdout};

    /// The production [`Documents`]: the machine configuration's layers
    /// through the VFS, and the settings service for the rest.
    struct Service;

    /// A zeroed buffer of `len` bytes, or the refusal when the heap cannot
    /// hold it.
    fn buffer(len: usize) -> Result<Vec<u8>, Errno> {
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(len)
            .map_err(|_| Errno::OutOfMemory)?;
        bytes.resize(len, 0);
        Ok(bytes)
    }

    /// Ask the settings service `request`, answering the document it returns.
    fn ask(request: &[u8]) -> Result<String, Errno> {
        let mut reply = buffer(SETTINGS_REPLY_MAX)?;
        let len = tairix_rt::ipc_call(SETTINGS_ENDPOINT, request, &mut reply)
            .map_err(Errno::from_syscall)?;
        decode_reply(&reply[..len]).map(String::from)
    }

    impl Documents for Service {
        fn read(&self, which: SystemConfigFile) -> Result<String, Errno> {
            match which {
                SystemConfigFile::System => which
                    .read_effective(|path| {
                        tairix_rt::read_document(path.as_bytes(), tairix_sysconfig::MAX_CONFIG_LEN)
                    })
                    .map(Option::unwrap_or_default),
                // Not open: it names the machine's hardware and addresses.
                SystemConfigFile::Network => {
                    let mut request = [0u8; READ_REQUEST_LEN];
                    let len = encode_read(which, &mut request)?;
                    ask(&request[..len])
                }
            }
        }

        fn change(
            &self,
            which: SystemConfigFile,
            settings: &[(&str, &str)],
        ) -> Result<String, Errno> {
            let mut request = buffer(set_request_len(settings))?;
            let len = encode_set(which, settings, &mut request)?;
            ask(&request[..len])
        }
    }

    /// The production [`NetPolicy`]: one `ipc_call` to the network stack's
    /// reserved admin endpoint. The kernel gates it on this process's
    /// `CAP_NET_ADMIN`, so the seam adds no authority — an ordinary user's
    /// `configure` is refused there, exactly as its change is refused by the
    /// settings service's gate.
    struct StackPolicy;

    impl NetPolicy for StackPolicy {
        fn machine_ram_bytes(&self) -> u64 {
            // Zero when the broker cannot answer, which derives the
            // smallest machine's capacity rather than none.
            tairix_procinfo::memory_total_bytes(&tairix_procinfo::IpcTransport).unwrap_or(0)
        }
        fn apply(&self, settings: NetworkSettings) -> Result<(), Errno> {
            admin_call(&NetstackRequest::ApplyNetworkSettings(settings).to_le_bytes())
        }

        fn apply_interface(&self, config: &NetInterfaceConfigMsg) -> Result<(), Errno> {
            admin_call(&config.to_le_bytes())
        }

        fn apply_bond(&self, config: &NetBondConfigMsg) -> Result<(), Errno> {
            admin_call(&config.to_le_bytes())
        }
    }

    /// One framed admin request to the network stack, decoding its status
    /// reply. Each admin message is self-identifying on the wire, so the
    /// three deliveries differ only in what they frame.
    fn admin_call(request: &[u8]) -> Result<(), Errno> {
        let mut reply = [0u8; STATUS_REPLY_LEN];
        let len = tairix_rt::ipc_call(NETSTACK_ENDPOINT, request, &mut reply)
            .map_err(Errno::from_syscall)?;
        decode_status_reply(&reply[..len])
    }

    /// Program entry point. `tairix-rt`'s `_start` calls it once the runtime
    /// is set up and routes its return value through the `exit` syscall.
    ///
    /// Exit codes: `0` on success (a listing, a shown value, a completed
    /// set, or the short help), `1` otherwise: the settings could not be
    /// read or changed (a permission denial states its reason on the
    /// diagnostic stream), the output could not be delivered, or the command
    /// line was not understood or asked for what the registries refuse.
    fn main() -> i32 {
        let Some(arguments) = tairix_help::arguments("configure") else {
            return tairix_help::MISUSE;
        };
        let command = match parse(&arguments) {
            Ok(command) => command,
            Err(err) => {
                tairix_help::report_misuse("configure", &err);
                return tairix_help::MISUSE;
            }
        };
        let locale = tairix_help::user_locale();
        // The tool's own bundle's `Help/` tree, read through the shared
        // syscall-backed source for the short-help switches.
        match run(
            command,
            locale,
            &Service,
            &StackPolicy,
            &BundleHelp::new("configure"),
            &Stdout,
            &Stderr,
        ) {
            Ok(()) => 0,
            Err(err @ (ConfigureError::Usage | ConfigureError::UnknownKey)) => {
                tairix_help::report_misuse("configure", &err);
                tairix_help::MISUSE
            }
            Err(err) => {
                write_stderr_line(format_args!("configure: {err}"));
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
