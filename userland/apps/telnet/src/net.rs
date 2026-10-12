//! The seam through which the session touches the outside world.
//!
//! The engine in [`crate::client`] is single-threaded and pure: it asks for the
//! next event and reacts. That is possible even though telnet must relay both
//! directions at once because the *seam* owns the multiplexing — the production
//! implementation (`src/run.rs`) parks one wait-set over both the stack's
//! socket-delivery port and the port its keyboard-reader thread posts to, so
//! neither side is ever polled and the engine sees one ordered event stream.
//!
//! Host tests drive the same engine with a scripted in-memory [`TelnetIo`], so
//! the tested code and the shipped code are the same code.

use alloc::vec::Vec;

use tairix_abi::net_ipc::NetAddrFamily;
use tairix_abi::{Errno, InputMode, TerminalSize};

/// A resolved remote endpoint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Endpoint {
    /// The address family the connection will use.
    pub family: NetAddrFamily,
    /// The address, IPv4 in the first four octets.
    pub addr: [u8; 16],
    /// The TCP port.
    pub port: u16,
}

/// Why a connection ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CloseReason {
    /// The peer closed its side of the connection orderly.
    PeerClosed,
    /// The connection was reset or otherwise aborted.
    Reset,
    /// The local side closed it (the `close`/`quit` commands).
    Local,
}

/// One event the session reacts to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IoEvent {
    /// Bytes arrived from the network, verbatim and un-interpreted.
    Network(Vec<u8>),
    /// Bytes arrived from the local terminal, verbatim.
    Keyboard(Vec<u8>),
    /// The local terminal reached end of input; nothing more will be typed.
    KeyboardClosed,
    /// The connection ended.
    Closed(CloseReason),
}

/// The connection, resolver and terminal the session drives.
pub trait TelnetIo {
    /// Resolve `host` to an endpoint on `port`, restricted to `family` when the
    /// command line named one. A literal address resolves without a query.
    ///
    /// Returns [`None`] when the host has no address of the wanted family — the
    /// caller reports it naming the host, so no error type is needed here.
    fn resolve(&mut self, host: &str, port: u16, family: Option<NetAddrFamily>)
        -> Option<Endpoint>;

    /// Open a connection to `endpoint`, replacing any current one.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the open or connect raised — [`Errno::PermissionDenied`]
    /// without `CAP_NET`, [`Errno::NetworkUnreachable`] with no route.
    fn connect(&mut self, endpoint: Endpoint) -> Result<(), Errno>;

    /// Whether a connection is currently open.
    fn connected(&self) -> bool;

    /// Block until the next event, giving the CPU up meanwhile (never a spin).
    ///
    /// # Errors
    ///
    /// Any [`Errno`] a fatal wait or receive raised. A transient empty mailbox
    /// is not an error — the implementation parks and retries.
    fn next_event(&mut self) -> Result<IoEvent, Errno>;

    /// Transmit `bytes` on the connection, retrying a momentarily-full send
    /// buffer until every byte is accepted.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the send raised — [`Errno::NotConnected`] once the peer
    /// has gone, or a transport failure.
    fn send(&mut self, bytes: &[u8]) -> Result<(), Errno>;

    /// The terminal's current character grid, or [`None`] when the console
    /// cannot attest one (a serial line, whose far-end terminal the kernel
    /// cannot know).
    fn terminal_size(&mut self) -> Option<TerminalSize>;

    /// Select the local read line discipline. The session takes
    /// [`InputMode::Raw`] for the relay and restores [`InputMode::Cooked`]
    /// before it ends, so the next program on this console sees the
    /// interactive default.
    fn set_input_mode(&mut self, mode: InputMode);

    /// Stop this process until it is continued (the `z` command).
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the request raised; an environment with no job control
    /// refuses and the session continues.
    fn suspend(&mut self) -> Result<(), Errno>;

    /// Send the connection's FIN, keeping it readable (the `send eof` path for
    /// a server that expects a half-close).
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the request raised.
    fn shutdown_write(&mut self) -> Result<(), Errno>;

    /// Release the connection.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the close raised.
    fn close(&mut self) -> Result<(), Errno>;
}

/// What one stream event tells a connect awaiting its handshake.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Handshake {
    /// The handshake completed.
    Connected,
    /// The connection closed before it completed.
    Refused,
    /// The event concerns something else.
    Unrelated,
}

/// Await a handshake: take events with `next` until one settles it, parking
/// with `park` whenever none is waiting. `park` answers whether anything
/// arrived before its deadline, so only the deadline ends the wait, however
/// often it wakes.
///
/// # Errors
///
/// [`Errno::NotConnected`] once the connection is refused,
/// [`Errno::TimedOut`] once `park` reports its deadline passed, or the park's
/// own failure.
pub fn await_handshake(
    mut next: impl FnMut() -> Option<Handshake>,
    mut park: impl FnMut() -> Result<bool, Errno>,
) -> Result<(), Errno> {
    loop {
        match next() {
            Some(Handshake::Connected) => return Ok(()),
            Some(Handshake::Refused) => return Err(Errno::NotConnected),
            Some(Handshake::Unrelated) => {}
            None => {
                if !park()? {
                    return Err(Errno::TimedOut);
                }
            }
        }
    }
}

/// Connect with `connect`, trying again each time `network_changed` reports
/// the machine's interfaces changed while the network is unreachable; it
/// answers `false` once its patience runs out.
///
/// # Errors
///
/// The connect's refusal — [`Errno::NetworkUnreachable`] once no change came
/// in time.
pub fn connect_when_reachable(
    mut connect: impl FnMut() -> Result<(), Errno>,
    mut network_changed: impl FnMut() -> bool,
) -> Result<(), Errno> {
    loop {
        match connect() {
            Err(Errno::NetworkUnreachable) if network_changed() => {}
            done => return done,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_deadline_ends_a_handshake_wait_however_often_it_wakes() {
        let wakes = core::cell::Cell::new(0u32);
        let settled = await_handshake(
            || (wakes.get() == 10_000).then_some(Handshake::Connected),
            || {
                wakes.set(wakes.get() + 1);
                Ok(true)
            },
        );
        assert_eq!(settled, Ok(()));
        assert_eq!(await_handshake(|| None, || Ok(false)), Err(Errno::TimedOut));
    }

    #[test]
    fn a_handshake_is_settled_by_its_own_events_only() {
        let mut events = [Handshake::Unrelated, Handshake::Refused].into_iter();
        assert_eq!(
            await_handshake(|| events.next(), || Ok(true)),
            Err(Errno::NotConnected)
        );
        assert_eq!(
            await_handshake(|| None, || Err(Errno::NotFound)),
            Err(Errno::NotFound),
            "a failed park ends the wait"
        );
    }

    #[test]
    fn a_connect_is_tried_again_only_when_the_network_changes() {
        let mut attempts = 0u32;
        let mut changes = 0u32;
        let connected = connect_when_reachable(
            || {
                attempts += 1;
                if attempts < 3 {
                    Err(Errno::NetworkUnreachable)
                } else {
                    Ok(())
                }
            },
            || {
                changes += 1;
                true
            },
        );
        assert_eq!(connected, Ok(()));
        assert_eq!((attempts, changes), (3, 2));
        assert_eq!(
            connect_when_reachable(|| Err(Errno::NetworkUnreachable), || false),
            Err(Errno::NetworkUnreachable),
            "no change in time ends it"
        );
        let mut waited = false;
        assert_eq!(
            connect_when_reachable(
                || Err(Errno::AddressUnavailable),
                || {
                    waited = true;
                    true
                }
            ),
            Err(Errno::AddressUnavailable)
        );
        assert!(!waited, "any other refusal is the answer at once");
    }
}
