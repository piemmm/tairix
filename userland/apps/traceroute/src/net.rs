//! The network and clock seam `traceroute` probes through.
//!
//! The engine never names a syscall: name resolution, the clock, and the echo
//! socket all go through [`TraceIo`]. The `Run` binary implements it over
//! `lib/resolver` and `tairix_rt::net`; host tests implement it in memory.

use alloc::string::String;
use core::net::IpAddr;
use core::num::NonZeroU8;

use tairix_abi::net::EchoOutcome;
use tairix_abi::net_ipc::NetAddrFamily;
use tairix_abi::Errno;
use tairix_resolver::TargetFailure;

/// What the echo socket reported about one probe.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Answer {
    /// The probe's sequence number.
    pub sequence: u16,
    /// The router that reported, or the destination that answered.
    pub from: IpAddr,
    /// What became of the probe.
    pub outcome: EchoOutcome,
}

/// Name resolution, the clock, and the echo socket the trace drives.
pub trait TraceIo {
    /// Resolve the target operand, restricted to `family` when one was forced.
    ///
    /// # Errors
    ///
    /// The [`TargetFailure`] reported after the host as typed.
    fn resolve(
        &mut self,
        host: &str,
        family: Option<NetAddrFamily>,
    ) -> Result<IpAddr, TargetFailure>;

    /// The name `addr` reverse-resolves to, or [`None`].
    fn reverse(&mut self, addr: IpAddr) -> Option<String>;

    /// Open the echo socket and connect it to `target`, so only answers about
    /// this trace's probes reach it.
    ///
    /// # Errors
    ///
    /// The open or connect refusal — [`Errno::PermissionDenied`] without
    /// `CAP_NET`/`CAP_NET_RAW`.
    fn connect(&mut self, target: IpAddr) -> Result<(), Errno>;

    /// The current monotonic time in nanoseconds.
    fn now(&self) -> u64;

    /// Send probe `sequence` carrying `hop_limit` and `payload`.
    ///
    /// # Errors
    ///
    /// The send's refusal ([`Errno::NetworkUnreachable`] while no interface
    /// can carry it yet).
    fn send(&mut self, sequence: u16, hop_limit: NonZeroU8, payload: &[u8]) -> Result<(), Errno>;

    /// Park until the network's interfaces next change or `deadline_ns`
    /// passes, answering whether they changed.
    fn await_network(&mut self, deadline_ns: u64) -> bool;

    /// Wait for the next answer until `deadline_ns`, never busy-spinning;
    /// [`None`] once the deadline passed.
    ///
    /// # Errors
    ///
    /// A fatal receive failure.
    fn recv(&mut self, deadline_ns: u64) -> Result<Option<Answer>, Errno>;
}
