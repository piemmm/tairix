//! A link supplier's view of the kernel about the call it is serving
//! (`plans/SUPPLIERS.md` SL1), behind a host-testable seam.
//!
//! A supplier believes a request its caller quotes only once the kernel
//! confirms the caller holds it, and releases what a caller held when the
//! caller ends. [`RtSupplier`] forwards each question to `tairix_rt`, so every
//! supplier asks it the one way.

use tairix_abi::hwtree::HwResource;
use tairix_abi::{DriverError, Errno, ProcId};

/// What a link supplier asks the kernel about the call it is serving.
pub trait SupplierHost {
    /// The instance whose call `ticket` is in service.
    ///
    /// # Errors
    ///
    /// The kernel's refusal, for a call no longer in service.
    fn caller(&self, ticket: u64) -> Result<ProcId, Errno>;

    /// Whether that caller holds a grant covering `record`.
    ///
    /// # Errors
    ///
    /// The kernel's refusal, other than the answer "no".
    fn caller_holds(&self, ticket: u64, record: &HwResource) -> Result<bool, Errno>;

    /// Answer the call `ticket` with `frame`.
    ///
    /// # Errors
    ///
    /// The kernel's refusal; a caller that has ended cannot be answered.
    fn reply(&mut self, ticket: u64, frame: &[u8]) -> Result<(), Errno>;

    /// Be told when `peer` ends. Idempotent.
    ///
    /// # Errors
    ///
    /// [`Errno::NotFound`] when it already has.
    fn watch(&mut self, peer: ProcId) -> Result<(), Errno>;

    /// Stop being told when `peer` ends.
    fn unwatch(&mut self, peer: ProcId);
}

/// Where a supplier's decisions of kind `R` go: the system log in a driver,
/// a list a test reads back.
pub trait Recorder<R> {
    /// Record `record`.
    fn record(&mut self, record: R);
}

/// The errno a link protocol's reply carries for a refusal from the hardware
/// a supplier drives: what the hardware cannot do is not supported, and what
/// another holder has is busy.
#[must_use]
pub const fn refusal(err: DriverError) -> Errno {
    match err {
        DriverError::Unsupported => Errno::NotSupported,
        DriverError::Busy => Errno::Busy,
        other => other.as_errno(),
    }
}

/// A link supplier, as the loop serving its endpoint drives it.
pub trait Supplier {
    /// Serve the call `ticket` carrying `frame`, answering it now or once
    /// what it waits on happens.
    fn serve(&mut self, ticket: u64, frame: &[u8]);

    /// Drop everything `peer` held; it has ended.
    fn peer_exited(&mut self, peer: ProcId);

    /// Service the device's event source at `index` of those the loop waits
    /// on: an interrupt line, or the answer to a call the supplier posted.
    ///
    /// # Errors
    ///
    /// The fault that ends the service.
    fn woke(&mut self, _index: usize) -> Result<(), DriverError> {
        Ok(())
    }

    /// When, in microseconds on the monotonic clock
    /// [`Delay::now_us`](tairix_abi::Delay::now_us) reads, the supplier is
    /// next due [`Supplier::expired`]; [`None`] while it awaits nothing.
    fn deadline_us(&self) -> Option<u64> {
        None
    }

    /// Its deadline came. It moves the deadline on or clears it: one left
    /// passed comes again at once.
    ///
    /// # Errors
    ///
    /// The fault that ends the service.
    fn expired(&mut self) -> Result<(), DriverError> {
        Ok(())
    }
}

/// The kernel, as the supplier serving one endpoint sees it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct RtSupplier {
    endpoint: u64,
}

impl RtSupplier {
    /// The supplier serving `endpoint`.
    #[must_use]
    pub const fn new(endpoint: u64) -> Self {
        Self { endpoint }
    }

    /// The endpoint it serves.
    #[must_use]
    pub const fn endpoint(&self) -> u64 {
        self.endpoint
    }
}

impl SupplierHost for RtSupplier {
    fn caller(&self, ticket: u64) -> Result<ProcId, Errno> {
        tairix_rt::peer_origin(self.endpoint, ticket).map(|origin| origin.proc_id())
    }

    fn caller_holds(&self, ticket: u64, record: &HwResource) -> Result<bool, Errno> {
        match Errno::syscall_result(tairix_rt::call_peer_holds(self.endpoint, ticket, record)) {
            Ok(_) => Ok(true),
            Err(Errno::PermissionDenied) => Ok(false),
            Err(reason) => Err(reason),
        }
    }

    fn reply(&mut self, ticket: u64, frame: &[u8]) -> Result<(), Errno> {
        Errno::syscall_result(tairix_rt::call_reply(self.endpoint, ticket, frame)).map(|_| ())
    }

    fn watch(&mut self, peer: ProcId) -> Result<(), Errno> {
        tairix_rt::peer_watch(peer)
    }

    fn unwatch(&mut self, peer: ProcId) {
        // A watch that already fired has nothing left to remove.
        let _ = tairix_rt::peer_unwatch(peer);
    }
}

/// What a supplier's deadline asks of the loop serving it.
#[cfg(any(test, target_os = "none"))]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Timer {
    /// It has come.
    Due,
    /// Wait this many nanoseconds at most.
    Wait(u64),
}

/// What `deadline_us` asks of the loop at `now_us`.
#[cfg(any(test, target_os = "none"))]
pub(crate) fn timer(deadline_us: Option<u64>, now_us: u64) -> Timer {
    match deadline_us {
        Some(at) if at <= now_us => Timer::Due,
        Some(at) => Timer::Wait((at - now_us).saturating_mul(1_000)),
        None => Timer::Wait(tairix_abi::waitset::WAITSET_TIMEOUT_NONE),
    }
}

#[cfg(test)]
mod timer_tests {
    use super::{timer, Timer};
    use tairix_abi::waitset::WAITSET_TIMEOUT_NONE;

    #[test]
    fn a_deadline_comes_at_its_instant_and_is_waited_for_until_then() {
        assert_eq!(timer(None, 5), Timer::Wait(WAITSET_TIMEOUT_NONE));
        assert_eq!(timer(Some(5), 5), Timer::Due);
        assert_eq!(timer(Some(4), 5), Timer::Due, "a deadline passed");
        assert_eq!(timer(Some(1_005), 5), Timer::Wait(1_000_000));
        assert_eq!(timer(Some(u64::MAX), 0), Timer::Wait(u64::MAX));
    }
}
