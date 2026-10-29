//! The one bridge from a port's fatal entry points to
//! [`tairix_kernel_core`]'s single fatal-report path.
//!
//! Three things can end a TAIRiX kernel: a Rust `panic!`, a CPU exception
//! taken in kernel mode that the port's vector has no fix-up for, and a task's
//! kernel stack found corrupt by the dispatcher. All are non-recoverable and
//! all deserve the same post-mortem — the register snapshot, the bounded
//! backtrace, and the audit record `kernel/core`'s [`handle_panic`] /
//! [`fault_dump`] / [`stack_corruption_dump`] emit.
//!
//! Reaching that path needs four per-port values (the published arch
//! handle, the sink, the register/unwind handle, the console list) and the
//! port's own reports, for the window before boot has published an arch
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
use tairix_arch_api::fatal::{KernelFault, StackCorruption};
use tairix_arch_api::fault::{set_fault_handler, SetFaultHandlerError};
use tairix_kernel_core::{
    fault_dump, handle_panic, set_stack_corruption_report, stack_corruption_dump, ConsoleDevice,
    KernelArch, PanicContext, NO_CONSOLES,
};
use tairix_log::Sink;

/// The per-port values [`report_panic`], [`report_kernel_fault`] and
/// [`report_stack_corruption`] need.
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

    /// The port's own report of a corrupt kernel stack, for the same window.
    fn report_stack_corruption_before_init(corruption: &StackCorruption) -> !;
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

/// Corrupt-kernel-stack report for port `B`, installed once per boot through
/// [`tairix_kernel_core::set_stack_corruption_report`].
pub fn report_stack_corruption<B: FatalReport>(corruption: StackCorruption) -> ! {
    match B::arch() {
        None => B::report_stack_corruption_before_init(&corruption),
        Some(arch) => stack_corruption_dump(corruption, &context::<B>(arch)),
    }
}

/// Install port `B`'s fatal reports, each in its set-once slot: the
/// kernel-mode fault handler and the corrupt-stack report.
///
/// # Errors
///
/// [`SetFaultHandlerError::AlreadyInstalled`] when something published either
/// first. The other is installed all the same, and what was there keeps the
/// machine's fatal policy.
pub fn install_fatal_reports<B: FatalReport>() -> Result<(), SetFaultHandlerError> {
    let fault = set_fault_handler(report_kernel_fault::<B>);
    let stack = set_stack_corruption_report(report_stack_corruption::<B>);
    fault.and(stack)
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Mutex, OnceLock, PoisonError};

    use tairix_arch_api::backtrace::{Backtrace, BacktraceProfile, FrameLayout, RegisterSnapshot};
    use tairix_arch_api::fatal::{self, StackCheck, KERNEL_FAULT, KERNEL_STACK_CORRUPT};
    use tairix_arch_api::fault::{fault_handler, FaultHandlerFn};
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

    const CORRUPTION: StackCorruption = StackCorruption {
        check: StackCheck::Guard,
        task: 0x2a,
        task_sp: 0x8040_0f00,
        base: 0x8040_0000,
        top: 0x8040_4000,
    };

    /// Serialises the tests that drive the process-wide fatal latch.
    static LATCH: Mutex<()> = Mutex::new(());

    static ARCH: OnceLock<TestArch> = OnceLock::new();
    static SINK: OnceLock<TestSink> = OnceLock::new();
    static REENTRIES: AtomicUsize = AtomicUsize::new(0);
    static QUIET_ARCH: OnceLock<TestArch> = OnceLock::new();
    static QUIET_SINK: OnceLock<TestSink> = OnceLock::new();

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

        fn report_stack_corruption_before_init(_corruption: &StackCorruption) -> ! {
            unreachable!("the arch handle is published")
        }
    }

    /// A port whose console drains without incident.
    struct QuietPort;

    impl FatalReport for QuietPort {
        type Arch = TestArch;

        fn arch() -> Option<&'static TestArch> {
            Some(QUIET_ARCH.get_or_init(|| TestArch::with_cpus(1)))
        }

        fn audit_sink() -> &'static (dyn Sink + Sync) {
            QUIET_SINK.get_or_init(TestSink::new)
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

        fn report_stack_corruption_before_init(_corruption: &StackCorruption) -> ! {
            unreachable!("the arch handle is published")
        }
    }

    /// A port that has not published its arch handle yet, whose own report
    /// hands its cause back through the unwind.
    struct EarlyPort;

    impl FatalReport for EarlyPort {
        type Arch = TestArch;

        fn arch() -> Option<&'static TestArch> {
            None
        }

        fn audit_sink() -> &'static (dyn Sink + Sync) {
            unreachable!("an early report writes through the port")
        }

        fn backtrace() -> &'static dyn CpuStateCapture {
            unreachable!("an early report writes through the port")
        }

        fn report_panic_before_init(_info: &PanicInfo<'_>) -> ! {
            unreachable!("no test panics early")
        }

        fn report_fault_before_init(fault: &KernelFault) -> ! {
            std::panic::panic_any(*fault)
        }

        fn report_stack_corruption_before_init(corruption: &StackCorruption) -> ! {
            std::panic::panic_any(*corruption)
        }
    }

    fn foreign_fault_handler(_fault: KernelFault) -> ! {
        unreachable!("a host test never takes a fault")
    }

    /// Once boot has published the arch handle a corrupt stack gets the
    /// kernel's post-mortem, ending in its own record.
    #[test]
    fn a_corrupt_stack_ends_in_the_kernel_s_record() {
        let _latch = LATCH.lock().unwrap_or_else(PoisonError::into_inner);
        fatal::reset_for_tests();
        let result = catch_unwind(|| {
            report_stack_corruption::<QuietPort>(CORRUPTION);
        });
        assert!(result.is_err(), "the report halts");
        let events = QUIET_SINK
            .get()
            .map(TestSink::snapshot)
            .expect("a record was written");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, KERNEL_STACK_CORRUPT.id);
        assert_eq!(events[0].message, KERNEL_STACK_CORRUPT.message);
        fatal::reset_for_tests();
    }

    /// Before boot publishes the arch handle each cause goes to the port's
    /// own report, unchanged.
    #[test]
    fn an_early_cause_goes_to_the_port_s_own_report() {
        let stack = catch_unwind(|| {
            report_stack_corruption::<EarlyPort>(CORRUPTION);
        })
        .expect_err("the port's report never returns");
        assert_eq!(stack.downcast_ref::<StackCorruption>(), Some(&CORRUPTION));
        let fault = catch_unwind(|| {
            report_kernel_fault::<EarlyPort>(FAULT);
        })
        .expect_err("the port's report never returns");
        assert_eq!(fault.downcast_ref::<KernelFault>(), Some(&FAULT));
    }

    /// The one test of the process-wide report slots: one something else
    /// claimed first keeps its occupant, and the other is installed all the
    /// same.
    #[test]
    fn installing_the_reports_fills_both_slots_and_keeps_an_earlier_claim() {
        let foreign: FaultHandlerFn = foreign_fault_handler;
        assert_eq!(set_fault_handler(foreign), Ok(()));
        assert_eq!(
            install_fatal_reports::<QuietPort>(),
            Err(SetFaultHandlerError::AlreadyInstalled)
        );
        assert_eq!(
            fault_handler().map(|handler| handler as *const ()),
            Some(foreign as *const ())
        );
        assert_eq!(
            set_stack_corruption_report(report_stack_corruption::<QuietPort>),
            Err(SetFaultHandlerError::AlreadyInstalled),
            "the corrupt-stack report was installed"
        );
    }

    /// Every drain of the console runs behind the fatal latch, so a fault
    /// inside one is a nested entry, and one inside the nested entry's own
    /// drain the silent third — never a re-entry that drains again.
    #[test]
    fn a_fault_inside_the_console_drain_ends_in_the_nested_record() {
        let _latch = LATCH.lock().unwrap_or_else(PoisonError::into_inner);
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
