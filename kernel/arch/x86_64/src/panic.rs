//! The port's own fatal reports, for the boot binaries that link no kernel
//! core and for a failure before one installed its handlers.
//!
//! Rust forbids library-defined `#[panic_handler]`s, so each binary declares
//! its own one-liner that forwards to [`handle_panic_via_serial`]. Every
//! report is the shared [`tairix_arch_api::fatal`] shape, written to COM1 —
//! brought up if nothing has yet, never re-initialised under a line in use —
//! and every one parks the CPU: never a silent reset, and never through QEMU's
//! debug-exit port. Their closing record is what ends a QEMU run at once
//! rather than on its inactivity budget.
//!
//! The *production* kernel routes a panic and a kernel fault through
//! `tairix_kernel_core`'s post-mortem (a register snapshot and a bounded
//! backtrace) via the bin-crate bridge; these are the paths below it.

use core::fmt;
use core::panic::{Location, PanicInfo};

use tairix_arch_api::fatal::{self, KernelFault, Processor, Reporter};
use tairix_arch_api::CpuStateCapture as _;

use crate::backtrace::Backtracer;
use crate::serial::fatal_com1;

const REPORTER: Reporter = Reporter { port: "x86_64" };

/// Shared `#[panic_handler]` body for the x86_64 boot binaries: report the
/// panic on COM1 and park the CPU.
pub fn handle_panic_via_serial(info: &PanicInfo<'_>) -> ! {
    report_panic(info, info.location())
}

/// Refuse to go on, saying why: an invariant the boot relies on has broken
/// where nothing above the port is installed to report it.
#[track_caller]
pub fn refuse(reason: &str) -> ! {
    report_panic(&reason, Some(Location::caller()))
}

fn report_panic(message: &dyn fmt::Display, location: Option<&Location<'_>>) -> ! {
    let entry = fatal::enter();
    REPORTER.panic(
        &mut fatal_com1(),
        entry,
        processor(),
        message,
        location,
        || Backtracer.boot_stack_verdict(Some(Backtracer.capture().sp)),
    );
    crate::reset::park_cpu()
}

/// Report a fatal exception no fault handler claimed and park the CPU.
///
/// The boot-stack guard is judged from the stack pointer the CPU pushed, not
/// this report's own: a vector delivered on an IST stack runs on a stack of
/// its own, which says nothing about where the interrupted code's was.
pub fn report_unclaimed_fault(fault: &KernelFault) -> ! {
    let entry = fatal::enter();
    REPORTER.fault(
        &mut fatal_com1(),
        entry,
        processor(),
        fault,
        format_args!("{}", crate::fault::Decoded(fault)),
        || Backtracer.boot_stack_verdict(fault.sp),
    );
    crate::reset::park_cpu()
}

/// The running CPU's dense id where the kernel has mapped it, else its
/// initial APIC id under its own key — read through `CPUID` rather than the
/// local APIC, which a report taken before the APIC is mapped would fault on.
fn processor() -> Processor {
    let apic = core::arch::x86_64::__cpuid(1).ebx >> 24;
    match crate::preempt::cpu_id_for_lapic(apic) {
        u32::MAX => Processor::Hardware {
            key: "apic_id",
            id: u64::from(apic),
        },
        cpu => Processor::Cpu(cpu),
    }
}
