//! The `Run` entry-point binary of the `traceroute` tool.
//!
//! A pure-Rust program over `tairix-rt`: `main` parses the argument vector,
//! reads the `LANG` locale preference the shell exported, and runs the parsed
//! command against `RtTraceIo` — the shared stub resolver for the target and
//! the hop names, and `tairix_rt`'s echo socket for the probes — writing each
//! line to the inherited standard output and a failure to standard error. It
//! binds only its inherited descriptors and holds no ambient authority: the
//! echo socket is capability-gated stack-side.
//!
//! On the host it is an inert stub so `cargo build --workspace`, clippy, and
//! fmt still cover the file.

#![cfg_attr(all(freestanding, feature = "program"), no_std)]
#![cfg_attr(all(freestanding, feature = "program"), no_main)]
#![deny(missing_docs)]

#[cfg(all(freestanding, feature = "program"))]
mod program {
    extern crate alloc;

    use alloc::string::String;
    use core::net::IpAddr;
    use core::num::NonZeroU8;

    use tairix_abi::net_ipc::{ip_from_parts, NetAddrFamily};
    use tairix_abi::Errno;
    use tairix_help::BundleHelp;
    use tairix_resolver::{RtDnsTransport, TargetFailure};
    use tairix_rt::io::{write_stderr_line, Stdout};
    use tairix_rt::net::EchoSocket;
    use tairix_traceroute::{parse, run, Answer, TraceIo};

    /// The production [`TraceIo`]: one resolver transport for the target and
    /// every hop name, and the echo socket once the target resolved.
    struct RtTraceIo {
        names: Option<RtDnsTransport>,
        socket: Option<EchoSocket>,
    }

    impl TraceIo for RtTraceIo {
        fn resolve(
            &mut self,
            host: &str,
            family: Option<NetAddrFamily>,
        ) -> Result<IpAddr, TargetFailure> {
            tairix_resolver::target_address(host, family)
        }

        fn reverse(&mut self, addr: IpAddr) -> Option<String> {
            if self.names.is_none() {
                self.names = RtDnsTransport::open().ok();
            }
            self.names.as_mut()?.reverse_name(addr)
        }

        fn connect(&mut self, target: IpAddr) -> Result<(), Errno> {
            self.socket = Some(EchoSocket::connect(target)?);
            Ok(())
        }

        fn now(&self) -> u64 {
            tairix_rt::clock_get()
        }

        fn send(
            &mut self,
            sequence: u16,
            hop_limit: NonZeroU8,
            payload: &[u8],
        ) -> Result<(), Errno> {
            self.socket.as_ref().ok_or(Errno::NotConnected)?.send(
                sequence,
                Some(hop_limit),
                payload,
            )
        }

        fn await_network(&mut self, deadline_ns: u64) -> bool {
            self.socket
                .as_ref()
                .is_some_and(|socket| socket.await_network(deadline_ns))
        }

        fn recv(&mut self, deadline_ns: u64) -> Result<Option<Answer>, Errno> {
            self.socket
                .as_mut()
                .ok_or(Errno::NotConnected)?
                .recv_until(deadline_ns, |echo| Answer {
                    sequence: echo.sequence,
                    from: ip_from_parts(echo.source.family, echo.source.addr),
                    outcome: echo.outcome,
                })
        }
    }

    /// Program entry point. Exit codes: `0` once the trace ran (whether or not
    /// it reached the host), `1` when the command line was not understood,
    /// and `2` on a failure that ended the run.
    fn main() -> i32 {
        let Some(arguments) = tairix_help::arguments("traceroute") else {
            return tairix_help::MISUSE;
        };
        let command = match parse(&arguments) {
            Ok(command) => command,
            Err(err) => {
                tairix_help::report_misuse("traceroute", &err);
                return tairix_help::MISUSE;
            }
        };
        let locale = tairix_help::user_locale();
        let help = BundleHelp::new("traceroute");
        let mut io = RtTraceIo {
            names: None,
            socket: None,
        };
        match run(command, locale, &mut io, &help, &Stdout) {
            Ok(()) => 0,
            Err(err) => {
                write_stderr_line(format_args!("traceroute: {err}"));
                2
            }
        }
    }

    tairix_rt::entry!(main);
}

#[cfg(not(all(freestanding, feature = "program")))]
fn main() {}
