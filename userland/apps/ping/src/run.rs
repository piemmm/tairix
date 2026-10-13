//! The `Run` entry-point binary of the `ping` tool — the program a shell
//! spawns to measure reachability and round-trip time to a host.
//!
//! This is a **pure-Rust** program: TAIRiX is Rust-only, so it links the Rust
//! userland runtime `tairix-rt` — never the C ABI, which exists solely for
//! programs *not* written in Rust. `tairix-rt` provides `_start`, the
//! per-process stack canary, the panic handler, the `mem_map`-backed global
//! allocator, the standard-stream I/O, and the ICMP-echo socket wrappers;
//! `tairix_rt::entry!` names this program's `main`.
//!
//! `main` parses the argument vector, reads the `LANG` locale preference from
//! the inherited environment (the shell exports it; the tool invents no
//! second source), and runs the parsed command against the production seams:
//! the `RtPingIo` seam — name resolution through the shared `lib/resolver`
//! stub resolver, the echo socket over `tairix_rt::net`, and payload entropy
//! from `lib/rng`'s fast generator seeded by the kernel CSPRNG — the shared
//! `tairix_help::BundleHelp` for the short-help switches, and `Stdout`,
//! which writes the per-reply lines and statistics to the inherited standard
//! output and diagnostics to standard error. The tool binds only to its
//! inherited descriptors, never a console device, and holds no ambient
//! authority (the ICMP echo socket is capability-gated stack-side).
//!
//! The payload comes from `lib/rng`'s fast `ChaCha12` generator, keyed once
//! from the kernel CSPRNG: drawing every payload from the CSPRNG itself would
//! spend the reserve for nothing, and a private generator is forbidden.
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

    use tairix_abi::net_ipc::{ip_from_parts, NetAddrFamily};
    use tairix_abi::Errno;
    use tairix_help::BundleHelp;
    use tairix_ping::{parse, run, Command, EchoEvent, PingError, PingIo, USAGE};
    use tairix_resolver::TargetFailure;
    use tairix_rng::{FastRng, RandU64};
    use tairix_rt::io::{write_stderr_line, Stderr, Stdout};
    use tairix_rt::net::EchoSocket;

    /// The production [`PingIo`]: the monotonic clock and the shared echo
    /// socket, over the `tairix-rt` syscall wrappers.
    struct RtPingIo {
        /// The echo socket, opened by `connect` once the target resolved.
        socket: Option<EchoSocket>,
        /// The payload generator, seeded once from the kernel CSPRNG.
        rng: FastRng,
    }

    impl RtPingIo {
        /// Seed the payload generator. The socket itself is opened by
        /// [`PingIo::connect`](tairix_ping::PingIo::connect), once the target
        /// has resolved.
        fn open() -> Result<Self, PingError> {
            Ok(Self {
                socket: None,
                rng: seed_generator()?,
            })
        }

        fn socket(&self) -> Result<&EchoSocket, Errno> {
            self.socket.as_ref().ok_or(Errno::NotConnected)
        }
    }

    /// Key the payload generator from the kernel CSPRNG.
    ///
    /// A kernel RNG that cannot supply the key is reported, never worked
    /// around with a predictable stream — a compressible payload would
    /// silently invalidate the measurement the tool exists to make.
    fn seed_generator() -> Result<FastRng, PingError> {
        FastRng::keyed_by(tairix_rt::random_fill).map_err(PingError::Socket)
    }

    impl PingIo for RtPingIo {
        fn resolve(
            &mut self,
            host: &str,
            family: Option<NetAddrFamily>,
        ) -> Result<(NetAddrFamily, [u8; 16]), TargetFailure> {
            tairix_resolver::target_address(host, family).map(tairix_abi::net_ipc::address_parts)
        }

        fn reverse(&mut self, family: NetAddrFamily, addr: [u8; 16]) -> Option<String> {
            tairix_resolver::reverse_name(tairix_abi::net_ipc::ip_from_parts(family, addr))
        }

        fn connect(&mut self, family: NetAddrFamily, addr: [u8; 16]) -> Result<(), Errno> {
            self.socket = Some(EchoSocket::connect(ip_from_parts(family, addr))?);
            Ok(())
        }

        fn fill_payload(&mut self, out: &mut [u8]) {
            self.rng.fill_bytes(out);
        }

        fn now(&self) -> u64 {
            tairix_rt::clock_get()
        }

        fn send(&mut self, seq: u16, payload: &[u8]) -> Result<(), Errno> {
            self.socket()?.send(seq, None, payload)
        }

        fn await_network(&mut self, deadline_ns: u64) -> bool {
            self.socket()
                .is_ok_and(|socket| socket.await_network(deadline_ns))
        }

        fn recv(&mut self, deadline_ns: u64) -> Result<Option<EchoEvent>, Errno> {
            self.socket
                .as_mut()
                .ok_or(Errno::NotConnected)?
                .recv_until(deadline_ns, |echo| EchoEvent {
                    seq: echo.sequence,
                    outcome: echo.outcome,
                    family: echo.source.family,
                    addr: echo.source.addr,
                    hop_limit: echo.reply_hop_limit,
                    payload: echo.payload.to_vec(),
                })
        }

        fn sleep_until(&mut self, deadline_ns: u64) {
            tairix_rt::park_ns(deadline_ns.saturating_sub(tairix_rt::clock_get()));
        }
    }

    /// A stub [`PingIo`] for the help path, which touches no network. Its
    /// methods are never called (help returns before the ping loop), so they
    /// deny rather than act.
    struct NoNet;

    impl PingIo for NoNet {
        fn resolve(
            &mut self,
            _host: &str,
            _family: Option<NetAddrFamily>,
        ) -> Result<(NetAddrFamily, [u8; 16]), TargetFailure> {
            Err(TargetFailure::Unknown)
        }
        fn reverse(&mut self, _family: NetAddrFamily, _addr: [u8; 16]) -> Option<String> {
            None
        }
        fn connect(&mut self, _family: NetAddrFamily, _addr: [u8; 16]) -> Result<(), Errno> {
            Err(Errno::NotImplemented)
        }
        fn fill_payload(&mut self, _out: &mut [u8]) {}
        fn now(&self) -> u64 {
            0
        }
        fn send(&mut self, _seq: u16, _payload: &[u8]) -> Result<(), Errno> {
            Err(Errno::NotImplemented)
        }
        fn await_network(&mut self, _deadline_ns: u64) -> bool {
            false
        }
        fn recv(&mut self, _deadline_ns: u64) -> Result<Option<EchoEvent>, Errno> {
            Ok(None)
        }
        fn sleep_until(&mut self, _deadline_ns: u64) {}
    }

    /// Program entry point. Exit codes: `0` on success (at least one reply,
    /// or help), `1` when every request went unanswered, and `2` on a usage
    /// error or a setup/output failure.
    fn main() -> i32 {
        let Some(arguments) = tairix_rt::args() else {
            write_stderr_line(USAGE);
            return 2;
        };
        let command = match parse(&arguments) {
            Ok(command) => command,
            Err(err) => {
                write_stderr_line(&format!("ping: {err}"));
                write_stderr_line(USAGE);
                return 2;
            }
        };
        let locale = tairix_help::user_locale();
        let help = BundleHelp::new("ping");
        let is_help = matches!(command, Command::Help);
        let result = if is_help {
            run(command, locale, &mut NoNet, &help, &Stdout, &Stderr)
        } else {
            match RtPingIo::open() {
                Ok(mut io) => run(command, locale, &mut io, &help, &Stdout, &Stderr),
                Err(err) => Err(err),
            }
        };
        match result {
            Ok(summary) => i32::from(!is_help && !summary.any_received()),
            Err(err) => {
                write_stderr_line(&format!("ping: {err}"));
                2
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
