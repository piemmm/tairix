//! Failure surface for [`crate::IrqTable`] operations.
//!
//! The [`IrqError`] enum is the *internal* failure surface;
//! `kernel/core::syscalls::KernelSyscallHandlers` translates each
//! variant into the stable `tairix_abi::Errno` documented in
//! `docs/src/security/irq.md` (the failure-mode table). The
//! translation is intentionally one-to-one so the security audit
//! trail can correlate a syscall-handler-side rejection to the
//! exact kernel-side cause.

use tairix_abi::Errno;

/// Failure modes of [`crate::IrqTable::bind`] and
/// [`crate::IrqTable::fire`].
///
/// Mapped to ABI errnos at the syscall boundary; see
/// [`Self::to_errno`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum IrqError {
    /// `line` argument exceeded the table's configured
    /// `max_line`. The platform-specific upper bound comes from the
    /// architecture port (e.g. the maximum IO-APIC redirection
    /// entry index on x86_64). Maps to [`Errno::OutOfRange`].
    LineOutOfRange,
    /// The task already binds `line`: one binding per `(task, line)`,
    /// though several tasks may share a line. Maps to
    /// [`Errno::OutOfRange`] — the closest stable variant meaning
    /// "the operation was inapplicable to the current state".
    LineAlreadyBound,
    /// The binding could not be recorded for want of memory. Maps to
    /// [`Errno::OutOfMemory`].
    Exhausted,
    /// The controller-side mask write failed. The arch port
    /// reported the line was not programmable through its
    /// controller interface (e.g. an architecture without an
    /// implementation of the [`crate::IrqController`] trait).
    /// Maps to [`Errno::NotImplemented`].
    ArchUnsupported,
}

impl IrqError {
    /// Translate to the ABI errno the syscall handler returns.
    #[must_use]
    pub const fn to_errno(self) -> Errno {
        match self {
            Self::LineOutOfRange | Self::LineAlreadyBound => Errno::OutOfRange,
            Self::Exhausted => Errno::OutOfMemory,
            Self::ArchUnsupported => Errno::NotImplemented,
        }
    }
}

/// Failure modes of [`crate::IrqController::mask`].
///
/// Separate from [`IrqError`] so an architecture port without a
/// programmable interrupt controller can declare so explicitly
/// (the production wiring on aarch64 / riscv64 / wasm32 returns
/// [`Self::Unsupported`], surfaced at the syscall boundary as
/// `Errno::NotImplemented`). On x86_64 the IO-APIC implementation
/// returns [`Self::OutOfRange`] when the line exceeds
/// `max_redirection_entry`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum MaskError {
    /// The architecture has no programmable controller wired in
    /// this build. Always returned by the placeholder
    /// `IrqController` impls on aarch64 / riscv64 / wasm32.
    Unsupported,
    /// The line is outside the controller's addressable range.
    OutOfRange,
}

/// Failure modes of [`crate::IrqController::activate`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ActivationError {
    /// The line is outside the controller's addressable range.
    OutOfRange,
    /// The controller has no vector left to give the line.
    Exhausted,
    /// Nothing the line raises could reach a CPU: no remapping entry could
    /// be made for it, or the CPU it would reach cannot be named.
    Unroutable,
}

impl ActivationError {
    /// Translate to the ABI errno the syscall handler returns: an exhausted
    /// vector space reads as it does to `msi_alloc`.
    #[must_use]
    pub const fn to_errno(self) -> Errno {
        match self {
            Self::OutOfRange | Self::Exhausted => Errno::OutOfRange,
            Self::Unroutable => Errno::NotSupported,
        }
    }
}
