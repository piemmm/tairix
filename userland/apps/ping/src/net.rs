//! The network + clock seam through which `ping` sends echo requests and
//! receives replies.
//!
//! The engine ([`crate::client`]) is pure and host-testable: it never names
//! a syscall. All contact with the outside world — name resolution, the
//! monotonic clock, the ICMP echo socket, the payload entropy, and the
//! wait/park between sends — goes through this object-safe [`PingIo`] trait.
//! The production implementation (`src/run.rs`) drives it with the shared
//! `lib/resolver` stub resolver, the `tairix-rt` ICMP-echo socket wrappers,
//! `lib/rng`'s fast generator, and the clock/wait-set syscalls; host tests
//! drive it with an in-memory fake.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::net::EchoOutcome;
use tairix_abi::net_ipc::NetAddrFamily;
use tairix_abi::Errno;
use tairix_resolver::TargetFailure;

/// What became of one echo request — a reply, or an ICMP error a router or
/// the host reported about it — owned so the engine can judge it after the
/// borrow of the receive buffer ends.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EchoEvent {
    /// The request's sequence number.
    pub seq: u16,
    /// What became of it.
    pub outcome: EchoOutcome,
    /// The source family.
    pub family: NetAddrFamily,
    /// The host that answered or the router that reported, its address bytes
    /// (IPv4 uses the first four).
    pub addr: [u8; 16],
    /// The echoed payload; empty for an error.
    pub payload: Vec<u8>,
}

/// Name resolution, the clock, the echo socket, the payload entropy, and the
/// wait/park the ping engine drives.
pub trait PingIo {
    /// Resolve the target operand `host` to an address, restricted to
    /// `family` when the command line forced one. An address literal
    /// resolves without a query.
    ///
    /// # Errors
    ///
    /// The [`TargetFailure`] the caller reports naming the host it was given.
    fn resolve(
        &mut self,
        host: &str,
        family: Option<NetAddrFamily>,
    ) -> Result<(NetAddrFamily, [u8; 16]), TargetFailure>;

    /// The name `addr` reverse-resolves to (a `PTR` lookup), or [`None`]
    /// when it has no record or the lookup did not conclude.
    ///
    /// Called for the target and for each router that reports an error, and
    /// only when the command line did not ask for numeric output, so `-n`
    /// issues no DNS query at all.
    fn reverse(&mut self, family: NetAddrFamily, addr: [u8; 16]) -> Option<String>;

    /// Open the echo socket and connect it to the resolved peer, so the
    /// stack filters replies to that peer and assigns the ICMP identifier.
    ///
    /// Called once, after [`Self::resolve`] and before the first
    /// [`Self::send`].
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the open or connect raised — [`Errno::PermissionDenied`]
    /// without `CAP_NET`/`CAP_NET_RAW`.
    fn connect(&mut self, family: NetAddrFamily, addr: [u8; 16]) -> Result<(), Errno>;

    /// Fill `out` with the bytes of the next request's payload.
    ///
    /// The default payload is high-entropy random data, drawn fresh for each
    /// request so a link that compresses or de-duplicates traffic cannot
    /// report a capacity it does not have.
    fn fill_payload(&mut self, out: &mut [u8]);

    /// The current monotonic time in nanoseconds.
    fn now(&self) -> u64;

    /// Send one echo request bearing `seq` and `payload` to the connected
    /// peer.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the send raised (e.g. [`Errno::NetworkUnreachable`]
    /// while no interface can carry it yet).
    fn send(&mut self, seq: u16, payload: &[u8]) -> Result<(), Errno>;

    /// Park until the machine's network interfaces next change — one comes
    /// up, or gains a usable address — or the absolute `deadline_ns` passes,
    /// answering whether they changed.
    fn await_network(&mut self, deadline_ns: u64) -> bool;

    /// Wait for what became of a request, giving the CPU up until it arrives
    /// or the absolute `deadline_ns` passes (never a busy spin). Returns
    /// [`None`] when the deadline passed with nothing.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] a fatal receive error raised.
    fn recv(&mut self, deadline_ns: u64) -> Result<Option<EchoEvent>, Errno>;

    /// Park until the absolute `deadline_ns` (the inter-request spacing),
    /// giving the CPU up rather than spinning.
    fn sleep_until(&mut self, deadline_ns: u64);
}
