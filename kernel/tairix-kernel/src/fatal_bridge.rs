//! The one bridge from a port's fatal entry points to
//! [`tairix_kernel_core`]'s single fatal-report path.
//!
//! Two things can end a TAIRiX kernel: a Rust `panic!`, and a CPU
//! exception taken in kernel mode that the port's vector has no fix-up
//! for. Both are non-recoverable and both deserve the same post-mortem —
//! the register snapshot, the bounded backtrace, and the audit record
//! `kernel/core`'s [`handle_panic`] / [`fault_dump`] emit.
//!
//! Reaching that path needs four per-port values (the published arch
//! handle, the sink, the register/unwind handle, the console list) and the
//! port's own two reports, for the window before boot has published an arch
//! handle. Everything else — which sink, which handle, how the context is
//! assembled — is one decision for the whole kernel and lives here, not
//! copied into each port's bridge. A port supplies the values by implementing
//! [`FatalReport`]; a fourth port implements the trait rather than growing a
//! fifth copy of the policy.
//!
//! # Why the bin crate
//!
//! An architecture crate may not depend on `kernel/core` (a port names only
//! the Arch HAL and `lib/*`), and `kernel/core` cannot own a
//! `#[panic_handler]` (a host-test build already has `std`'s). The bin
//! crate is the one layer that links both, so the bridge lives here beside
//! the per-port `panic_ctx` modules that implement the trait.

use core::panic::PanicInfo;

use tairix_arch_api::backtrace::CpuStateCapture;
use tairix_arch_api::fatal::KernelFault;
use tairix_kernel_core::{
    fault_dump, handle_panic, ConsoleDevice, KernelArch, PanicContext, NO_CONSOLES,
};
use tairix_log::Sink;

/// The per-port values [`report_panic`] and [`report_kernel_fault`] need.
///
/// Implemented once per port, on a private unit type beside that port's
/// published arch pointer.
pub trait FatalReport {
    /// The port's `KernelArch` handle type.
    type Arch: KernelArch + 'static;

    /// The arch handle boot published, or [`None`] in the window before it
    /// did (a fault or panic from inside `boot` itself, or from the
    /// allocator on heap exhaustion).
    fn arch() -> Option<&'static Self::Arch>;

    /// Sink the fatal record is written to.
    ///
    /// This is the port's own serial sink, not the audit sink a boot handed
    /// to `kernel_core`: a fatal report must not depend on anything that
    /// could itself be the reason the kernel is dying. Where it queues, the
    /// report drains it through `KernelArch::flush_console_blocking`.
    fn audit_sink() -> &'static (dyn Sink + Sync);

    /// The port's post-mortem register-capture and unwind handle.
    fn backtrace() -> &'static dyn CpuStateCapture;

    /// The installed console list, so the report takes the display surface
    /// back from a graphical session before it writes. A port that wires no
    /// console has none to reclaim.
    #[must_use]
    fn consoles() -> &'static [ConsoleDevice] {
        &NO_CONSOLES
    }

    /// The port's own report of a panic, for the null-handle window above:
    /// the shared record shape and fatal latch, without the post-mortem the
    /// kernel cannot yet give. Parks the CPU — never a silent reset.
    fn report_panic_before_init(info: &PanicInfo<'_>) -> !;

    /// The port's own report of a kernel fault, for the same window.
    fn report_fault_before_init(fault: &KernelFault) -> !;
}

/// Assemble the port's fatal-report context.
fn context<B: FatalReport>(arch: &B::Arch) -> PanicContext<'_, B::Arch> {
    PanicContext::new(arch, B::audit_sink())
        .with_backtrace(B::backtrace())
        .with_consoles(B::consoles())
}

/// `#[panic_handler]` body for every binary of port `B`.
pub fn report_panic<B: FatalReport>(info: &PanicInfo<'_>) -> ! {
    match B::arch() {
        None => B::report_panic_before_init(info),
        Some(arch) => handle_panic(info, &context::<B>(arch)),
    }
}

/// Fatal kernel-fault handler for port `B`, installed once per boot through
/// [`tairix_arch_api::fault::set_fault_handler`].
///
/// Never returns: the kernel has no fix-up for the exception, so resuming
/// would re-trap forever.
pub fn report_kernel_fault<B: FatalReport>(fault: KernelFault) -> ! {
    match B::arch() {
        None => B::report_fault_before_init(&fault),
        Some(arch) => fault_dump(fault, &context::<B>(arch)),
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::OnceLock;

    use tairix_arch_api::backtrace::{Backtrace, BacktraceProfile, FrameLayout, RegisterSnapshot};
    use tairix_arch_api::fatal::{self, KERNEL_FAULT};
    use tairix_arch_api::KernelStackRegion;
    use tairix_kernel_core::test_arch::TestArch;
    use tairix_kernel_core::test_sink::TestSink;

    use super::*;

    const FAULT: KernelFault = KernelFault {
        syndrome: Some(0x9600_0045),
        address: Some(0x1000),
        pc: 0x4008_0000,
        sp: None,
    };

    static ARCH: OnceLock<TestArch> = OnceLock::new();
    static SINK: OnceLock<TestSink> = OnceLock::new();
    static REENTRIES: AtomicUsize = AtomicUsize::new(0);

    /// A console drain that faults, as one the failure itself corrupted
    /// would: the fault re-enters the fatal path from inside the drain.
    fn faulting_drain() {
        // Bounded so an ordering that re-enters fails the test rather than
        // overrunning its stack.
        if REENTRIES.fetch_add(1, Ordering::SeqCst) < 3 {
            report_kernel_fault::<FaultingConsole>(FAULT);
        }
    }

    struct NoCapture;

    impl CpuStateCapture for NoCapture {
        fn profile(&self) -> BacktraceProfile {
            BacktraceProfile {
                register_capture: Backtrace::Unsupported("host bridge test"),
                frame_unwind: Backtrace::Unsupported("host bridge test"),
            }
        }
        fn capture(&self) -> RegisterSnapshot {
            RegisterSnapshot::new(0, 0, 0)
        }
        fn frame_layout(&self) -> Option<FrameLayout> {
            None
        }
        fn boot_stack(&self) -> Option<KernelStackRegion> {
            None
        }
    }

    struct FaultingConsole;

    impl FatalReport for FaultingConsole {
        type Arch = TestArch;

        fn arch() -> Option<&'static TestArch> {
            Some(ARCH.get_or_init(|| {
                let arch = TestArch::with_cpus(1);
                arch.set_console_flush_hook(faulting_drain);
                arch
            }))
        }

        fn audit_sink() -> &'static (dyn Sink + Sync) {
            SINK.get_or_init(TestSink::new)
        }

        fn backtrace() -> &'static dyn CpuStateCapture {
            &NoCapture
        }

        fn report_panic_before_init(_info: &PanicInfo<'_>) -> ! {
            unreachable!("the arch handle is published")
        }

        fn report_fault_before_init(_fault: &KernelFault) -> ! {
            unreachable!("the arch handle is published")
        }
    }

    /// Every drain of the console runs behind the fatal latch, so a fault
    /// inside one is a nested entry, and one inside the nested entry's own
    /// drain the silent third — never a re-entry that drains again.
    #[test]
    fn a_fault_inside_the_console_drain_ends_in_the_nested_record() {
        fatal::reset_for_tests();
        let result = catch_unwind(AssertUnwindSafe(|| {
            report_kernel_fault::<FaultingConsole>(FAULT)
        }));
        assert!(result.is_err(), "the report halts");
        assert_eq!(
            REENTRIES.load(Ordering::SeqCst),
            2,
            "the first entry's drain and the nested entry's, then silence"
        );
        let events = SINK
            .get()
            .map(TestSink::snapshot)
            .expect("a record was written");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].message, KERNEL_FAULT.nested);
        assert_eq!(ARCH.get().map(TestArch::halt_count), Some(1));
        fatal::reset_for_tests();
    }
}
