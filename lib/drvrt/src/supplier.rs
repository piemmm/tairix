//! A link supplier's view of the kernel about the call it is serving
//! (`plans/SUPPLIERS.md` SL1), behind a host-testable seam.
//!
//! A supplier believes a request its caller quotes only once the kernel
//! confirms the caller holds it, and releases what a caller held when the
//! caller ends. [`RtSupplier`] forwards each question to `tairix_rt`, so every
//! supplier asks it the one way.

use tairix_abi::hwtree::HwResource;
use tairix_abi::{Errno, ProcId};

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

/// A non-negative status as its value, a negative one as its `Errno`.
fn status(ret: i64) -> Result<u64, Errno> {
    u64::try_from(ret).map_err(|_| Errno::from_syscall(ret))
}

impl SupplierHost for RtSupplier {
    fn caller(&self, ticket: u64) -> Result<ProcId, Errno> {
        tairix_rt::peer_origin(self.endpoint, ticket).map(|origin| origin.proc_id())
    }

    fn caller_holds(&self, ticket: u64, record: &HwResource) -> Result<bool, Errno> {
        match status(tairix_rt::call_peer_holds(self.endpoint, ticket, record)) {
            Ok(_) => Ok(true),
            Err(Errno::PermissionDenied) => Ok(false),
            Err(reason) => Err(reason),
        }
    }

    fn reply(&mut self, ticket: u64, frame: &[u8]) -> Result<(), Errno> {
        status(tairix_rt::call_reply(self.endpoint, ticket, frame)).map(|_| ())
    }

    fn watch(&mut self, peer: ProcId) -> Result<(), Errno> {
        tairix_rt::peer_watch(peer)
    }

    fn unwatch(&mut self, peer: ProcId) {
        // A watch that already fired has nothing left to remove.
        let _ = tairix_rt::peer_unwatch(peer);
    }
}
