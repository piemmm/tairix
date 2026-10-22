//! The `Run` entry-point binary of the `telnet` tool — the program a shell
//! spawns to reach a remote host.
//!
//! This is a **pure-Rust** program: TAIRiX is Rust-only, so it links the Rust
//! userland runtime `tairix-rt` — never the C ABI, which exists solely for
//! programs *not* written in Rust. `tairix-rt` provides `_start`, the
//! per-process stack canary, the panic handler, the `mem_map`-backed global
//! allocator, the standard-stream I/O, and the threads the relay needs;
//! `tairix_rt::entry!` names this program's `main`.
//!
//! # Why two threads, and why neither of them polls
//!
//! Telnet must carry the keyboard and the connection at the same time. The
//! stack delivers a socket's stream events to an async **port**, which joins a
//! wait-set; a console-backed standard input cannot join one, so the keyboard
//! is read by the runtime's key relay (`tairix_rt::keys::KeyRelay`), a thread
//! forwarding it to a private port the same wait-set watches. The main thread
//! parks on both and sees one ordered event stream. Neither thread spins, and
//! no timer is armed.
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
    use alloc::vec::Vec;

    use tairix_abi::net::{
        ShutdownHow, SocketAddr, SocketId, SocketStreamEvent, StreamCloseReason,
    };
    use tairix_abi::net_ipc::NetAddrFamily;
    use tairix_abi::notice::NoticeTopic;
    use tairix_abi::waitset::WaitSourceKind;
    use tairix_abi::{Errno, InputMode, TerminalSize};
    use tairix_help::BundleHelp;
    use tairix_net::IpAddr;
    use tairix_rt::io::{write_stderr_line, Stderr, Stdout};
    use tairix_rt::keys::{KeyRelay, Keys};
    use tairix_rt::{PrivatePort, WaitSet};
    use tairix_telnet::net::{
        await_handshake, connect_when_reachable, CloseReason, Endpoint, Handshake, IoEvent,
        TelnetIo,
    };
    use tairix_telnet::{parse, run, Command, FALLBACK_TERM, USAGE};
    use tairix_util::quote::quote;

    /// Wait-set token for the stack's delivery port.
    const DELIVER_TOKEN: u64 = 1;
    /// Wait-set token for the keyboard port.
    const KEYBOARD_TOKEN: u64 = 2;
    /// Wait-set token for the network-interfaces notice.
    const NETWORK_TOKEN: u64 = 3;

    /// Mailbox depth for each port. Generous headroom so a burst of server
    /// output or fast typing queues rather than back-pressuring its producer;
    /// the connection's own receive window bounds the true in-flight volume.
    const PORT_CAPACITY: usize = 64;

    /// How long `connect` waits for an interface to come up at boot, when the
    /// NIC driver may not be bound to the stack yet and it answers
    /// `NetworkUnreachable`.
    const CONNECT_PATIENCE_NANOS: u64 = 5 * NANOS_PER_SEC;

    /// Bytes offered per `stream_send` call, so one marshalled request stays
    /// modest however much the operator pasted.
    const SEND_CHUNK: usize = 4096;

    /// How long the handshake may take before it is declared dead: generous
    /// for an emulated link, bounded so a wedged stack fails loud with a
    /// reason instead of hanging the terminal.
    const HANDSHAKE_TIMEOUT_NANOS: u64 = 60 * NANOS_PER_SEC;

    /// Nanoseconds in a second.
    const NANOS_PER_SEC: u64 = tairix_abi::time::NANOS_PER_SEC as u64;

    /// The conventional fallback grid for a console whose true size the kernel
    /// cannot attest — the terminal-library policy, applied here rather than
    /// fabricated by the kernel.
    const FALLBACK_ROWS: u16 = 24;
    /// Columns of the fallback grid.
    const FALLBACK_COLS: u16 = 80;

    /// The production [`TelnetIo`]: the stream socket, the two ports, the
    /// wait-sets they are registered on, and the terminal controls.
    struct RtTelnetIo {
        /// The wait-set the relay parks on: the delivery port and the keyboard.
        set: WaitSet,
        /// The delivery port alone, for the handshake and for output waiting
        /// on room, neither of which may wake for a keystroke it cannot take.
        stream: WaitSet,
        /// The network-interfaces notice, for a connect waiting on the network.
        network: WaitSet,
        /// The process-private port the stack delivers stream events to.
        deliver: PrivatePort,
        /// The keyboard, read on a thread of its own.
        keys: KeyRelay,
        /// The connected socket, when there is one.
        socket: Option<SocketId>,
        /// A local address to bind before connecting (`-b`).
        bind: Option<SocketAddr>,
        /// Decode buffer for one delivery message.
        buf: Vec<u8>,
    }

    impl RtTelnetIo {
        /// Bind the delivery port, create the wait-set, and register it and
        /// the keyboard's on it.
        fn open(bind: Option<SocketAddr>, keys: KeyRelay) -> Result<Self, Errno> {
            let deliver = PrivatePort::bind(SocketStreamEvent::MAX_WIRE_LEN, PORT_CAPACITY)?;
            let set = WaitSet::create()?;
            set.add(WaitSourceKind::Port, deliver.id(), DELIVER_TOKEN)?;
            set.add(WaitSourceKind::Port, keys.port(), KEYBOARD_TOKEN)?;
            let stream = WaitSet::create()?;
            stream.add(WaitSourceKind::Port, deliver.id(), DELIVER_TOKEN)?;
            let network = WaitSet::create()?;
            network.add(
                WaitSourceKind::SystemNotice,
                u64::from(NoticeTopic::Network.as_u32()),
                NETWORK_TOKEN,
            )?;
            Ok(Self {
                set,
                stream,
                network,
                deliver,
                keys,
                socket: None,
                bind,
                buf: alloc::vec![0u8; SocketStreamEvent::MAX_WIRE_LEN],
            })
        }

        /// Park until one of the two ports has a message, giving the CPU up.
        fn park(&self, keyboard: bool) {
            let set = if keyboard { &self.set } else { &self.stream };
            let _ = set.wait_until(u64::MAX);
        }

        /// Drain one keyboard message, or [`None`] if the mailbox was empty.
        fn drain_keyboard(&mut self) -> Option<Result<IoEvent, Errno>> {
            Some(Ok(match self.keys.take()? {
                Keys::Typed(typed) => IoEvent::Keyboard(typed.to_vec()),
                Keys::Ended => IoEvent::KeyboardClosed,
            }))
        }

        /// Drain one stream event, or [`None`] if the mailbox was empty or the
        /// message was not one this session may act on.
        fn drain_network(&mut self) -> Option<Result<IoEvent, Errno>> {
            // Only the stack's own events come back: the receive discards a
            // forged sender unread.
            let event = tairix_rt::net::stream_recv(self.deliver.id(), &mut self.buf).ok()?;
            let socket = self.socket?;
            match event {
                SocketStreamEvent::Data { socket: s, payload } if s == socket => {
                    Some(Ok(IoEvent::Network(payload.to_vec())))
                }
                SocketStreamEvent::Room { socket: s } if s == socket => Some(Ok(IoEvent::Room)),
                SocketStreamEvent::Closed { socket: s, reason } if s == socket => {
                    self.socket = None;
                    Some(Ok(IoEvent::Closed(match reason {
                        StreamCloseReason::PeerClosed => CloseReason::PeerClosed,
                        _ => CloseReason::Reset,
                    })))
                }
                // `Connected` is consumed by `connect`; an event for a socket
                // this session no longer holds, and an `Accepted` a client can
                // never own, are dropped.
                _ => None,
            }
        }

        /// Await the `Connected` event for `socket`, or the `Closed` that says
        /// the handshake failed.
        ///
        /// Bounded: a stack that answers neither is a wedged stack, and this
        /// must fail loud with a reason rather than hang the terminal.
        fn await_connected(&mut self, socket: SocketId) -> Result<(), Errno> {
            let deadline = tairix_rt::clock_get().saturating_add(HANDSHAKE_TIMEOUT_NANOS);
            let (deliver, buf, stream) = (self.deliver.id(), &mut self.buf, &self.stream);
            await_handshake(
                || match tairix_rt::net::stream_recv(deliver, buf) {
                    Ok(SocketStreamEvent::Connected { socket: s }) if s == socket => {
                        Some(Handshake::Connected)
                    }
                    Ok(SocketStreamEvent::Closed { socket: s, .. }) if s == socket => {
                        Some(Handshake::Refused)
                    }
                    Ok(_) => Some(Handshake::Unrelated),
                    Err(_) => None,
                },
                || stream.wait_until(deadline).map(|woke| woke.is_some()),
            )
        }
    }

    impl TelnetIo for RtTelnetIo {
        fn resolve(
            &mut self,
            host: &str,
            port: u16,
            family: Option<NetAddrFamily>,
        ) -> Option<Endpoint> {
            // The literal-first, family-preference policy is shared, so
            // `telnet` and `ping` cannot disagree about what a host operand
            // means. A literal needs no query, which is what makes
            // `telnet <address>` work with no resolver configured.
            let address = tairix_resolver::host_address(host, family)?;
            Some(endpoint_for(address, port))
        }

        fn connect(&mut self, endpoint: Endpoint) -> Result<(), Errno> {
            let socket = tairix_rt::net::stream_socket(endpoint.family, self.deliver.id())?;
            if let Some(local) = self.bind {
                if local.family != endpoint.family {
                    let _ = tairix_rt::net::close(socket);
                    return Err(Errno::AddressUnavailable);
                }
                if let Err(errno) = tairix_rt::net::bind(socket, local) {
                    let _ = tairix_rt::net::close(socket);
                    return Err(errno);
                }
            }
            let peer = SocketAddr {
                family: endpoint.family,
                addr: endpoint.addr,
                port: endpoint.port,
            };
            // The stack has no egress until a NIC is bound to it, which at boot
            // can be after this program starts: each change to the interfaces
            // is a reason to try again.
            let patience = tairix_rt::clock_get().saturating_add(CONNECT_PATIENCE_NANOS);
            let network = &self.network;
            if let Err(errno) = connect_when_reachable(
                || tairix_rt::net::connect(socket, peer),
                || matches!(network.wait_until(patience), Ok(Some(_))),
            ) {
                let _ = tairix_rt::net::close(socket);
                return Err(errno);
            }
            self.socket = Some(socket);
            if let Err(errno) = self.await_connected(socket) {
                let _ = tairix_rt::net::close(socket);
                self.socket = None;
                return Err(errno);
            }
            Ok(())
        }

        fn connected(&self) -> bool {
            self.socket.is_some()
        }

        fn next_event(&mut self, keyboard: bool) -> Result<IoEvent, Errno> {
            loop {
                // Both mailboxes are drained before parking again, so an event
                // that arrived while the last one was being handled is never
                // left waiting behind a park.
                if let Some(event) = self.drain_network() {
                    return event;
                }
                if keyboard {
                    if let Some(event) = self.drain_keyboard() {
                        return event;
                    }
                }
                self.park(keyboard);
            }
        }

        fn send(&mut self, bytes: &[u8]) -> Result<usize, Errno> {
            let socket = self.socket.ok_or(Errno::NotConnected)?;
            let mut taken = 0usize;
            for chunk in bytes.chunks(SEND_CHUNK) {
                let accepted = tairix_rt::net::stream_send(socket, chunk)? as usize;
                taken += accepted;
                if accepted < chunk.len() {
                    break;
                }
            }
            Ok(taken)
        }

        fn terminal_size(&mut self) -> Option<TerminalSize> {
            tairix_rt::terminal_size(tairix_abi::STDOUT)
                .ok()
                .or_else(|| {
                    // A byte-stream console's true geometry is a property of the
                    // terminal at the far end, which the kernel cannot attest; the
                    // conventional fallback is the client's policy to apply.
                    TerminalSize::new(FALLBACK_ROWS, FALLBACK_COLS).ok()
                })
        }

        fn set_input_mode(&mut self, mode: InputMode) {
            let _ = tairix_rt::set_input_mode(mode);
        }

        fn suspend(&mut self) -> Result<(), Errno> {
            tairix_rt::stop_self()
        }

        fn shutdown_write(&mut self) -> Result<(), Errno> {
            let socket = self.socket.ok_or(Errno::NotConnected)?;
            tairix_rt::net::shutdown(socket, ShutdownHow::Write)
        }

        fn close(&mut self) -> Result<(), Errno> {
            match self.socket.take() {
                Some(socket) => tairix_rt::net::close(socket),
                None => Ok(()),
            }
        }
    }

    /// The socket endpoint for a resolved address.
    fn endpoint_for(address: IpAddr, port: u16) -> Endpoint {
        let (family, addr) = tairix_abi::net_ipc::address_parts(address);
        Endpoint { family, addr, port }
    }

    /// Parse a `-b` local-bind address. The port is always ephemeral: binding a
    /// *privileged* local port would need a capability a client has no business
    /// holding, and no server matches on a client's source port.
    fn bind_address(text: &str) -> Option<SocketAddr> {
        let address: IpAddr = text.parse().ok()?;
        let endpoint = endpoint_for(address, 0);
        Some(SocketAddr {
            family: endpoint.family,
            addr: endpoint.addr,
            port: 0,
        })
    }

    /// Program entry point. Exit codes: `0` for a session that ran, `1` for a
    /// failure that defeated it (an unresolvable host, a refused socket, a
    /// terminal that could not be taken), and `2` for a usage error.
    fn main() -> i32 {
        let Some(arguments) = tairix_rt::args() else {
            write_stderr_line(USAGE);
            return 2;
        };
        let command = match parse(&arguments) {
            Ok(command) => command,
            Err(err) => {
                write_stderr_line(&format!("telnet: {err}"));
                write_stderr_line(USAGE);
                return 2;
            }
        };
        let locale = tairix_help::user_locale();
        // The terminal type the session exports, reported over TERMINAL-TYPE;
        // a session that exported none is told so rather than claiming to be a
        // terminal this console may not implement.
        let term = tairix_rt::env_var(b"TERM")
            .and_then(|raw| core::str::from_utf8(raw).ok())
            .unwrap_or(FALLBACK_TERM);
        let help = BundleHelp::new("telnet");

        if matches!(command, Command::Help) {
            // The help path opens no socket, binds no port and starts no
            // thread: it is a plain write to standard output.
            return match run(command, locale, term, &mut NoIo, &help, &Stdout, &Stderr) {
                Ok(()) => 0,
                Err(err) => {
                    write_stderr_line(&format!("telnet: {err}"));
                    2
                }
            };
        }

        // `-a` asks for the login name without naming it, so it comes from the
        // session's own `USER`. Nothing else is ever disclosed by an
        // invocation.
        let command = match command {
            Command::Session { mut config, target } => {
                if config.auto_login && config.user.is_none() {
                    config.user = tairix_rt::env_var(b"USER")
                        .and_then(|raw| core::str::from_utf8(raw).ok())
                        .map(alloc::string::ToString::to_string);
                }
                Command::Session { config, target }
            }
            help_command @ Command::Help => help_command,
        };

        let bind = match &command {
            Command::Session { config, .. } => match config.bind.as_deref() {
                Some(text) => {
                    let Some(address) = bind_address(text) else {
                        write_stderr_line(&format!(
                            "telnet: {text} is not a local address",
                            text = quote(text)
                        ));
                        return 2;
                    };
                    Some(address)
                }
                None => None,
            },
            Command::Help => None,
        };

        // The raw discipline is taken *before* the keyboard's reader exists: a
        // keystroke it consumed under the cooked one would be echoed by the
        // console and held to the end of a line. `run` takes it again, which is
        // idempotent, and is what restores the cooked default on every exit
        // path.
        let _ = tairix_rt::set_input_mode(InputMode::Raw);
        let keys = match KeyRelay::start() {
            Ok(keys) => keys,
            Err(errno) => {
                write_stderr_line(&format!("telnet: cannot start the input reader: {errno}"));
                return 1;
            }
        };
        let mut io = match RtTelnetIo::open(bind, keys) {
            Ok(io) => io,
            Err(errno) => {
                write_stderr_line(&format!("telnet: cannot set up the session: {errno}"));
                return 1;
            }
        };

        match run(command, locale, term, &mut io, &help, &Stdout, &Stderr) {
            Ok(()) => 0,
            Err(err) => {
                write_stderr_line(&format!("telnet: {err}"));
                1
            }
        }
    }

    /// The seam for the help path, which reaches nothing. Every method denies
    /// rather than acting, and none is called: `run` returns after writing the
    /// help.
    struct NoIo;

    impl TelnetIo for NoIo {
        fn resolve(
            &mut self,
            _host: &str,
            _port: u16,
            _family: Option<NetAddrFamily>,
        ) -> Option<Endpoint> {
            None
        }

        fn connect(&mut self, _endpoint: Endpoint) -> Result<(), Errno> {
            Err(Errno::NotConnected)
        }

        fn connected(&self) -> bool {
            false
        }

        fn next_event(&mut self, _keyboard: bool) -> Result<IoEvent, Errno> {
            Err(Errno::NotConnected)
        }

        fn send(&mut self, _bytes: &[u8]) -> Result<usize, Errno> {
            Err(Errno::NotConnected)
        }

        fn terminal_size(&mut self) -> Option<TerminalSize> {
            None
        }

        fn set_input_mode(&mut self, _mode: InputMode) {}

        fn suspend(&mut self) -> Result<(), Errno> {
            Err(Errno::NotImplemented)
        }

        fn shutdown_write(&mut self) -> Result<(), Errno> {
            Err(Errno::NotConnected)
        }

        fn close(&mut self) -> Result<(), Errno> {
            Ok(())
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
