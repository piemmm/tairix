//! The port's own fatal reports, for the boot binaries that link no kernel
//! core and for a failure before one installed its handlers.
//!
//! Rust forbids library-defined `#[panic_handler]`s, so each binary declares
//! its own one-liner that forwards to [`handle_panic_via_serial`]. Every
//! report is the shared [`tairix_arch_api::fatal`] shape, written through the
//! synchronous SBI console, and every one parks the hart: never a silent
//! reset. Their closing record is what ends a QEMU run at once rather than on
//! its inactivity budget.
//!
//! The *production* kernel routes a panic and a kernel fault through
//! `tairix_kernel_core`'s post-mortem (a register snapshot and a bounded
//! backtrace) via the bin-crate bridge; these are the paths below it.

use core::fmt;
use core::panic::{Location, PanicInfo};

use tairix_arch_api::fatal::{self, KernelFault, Processor, Reporter};
use tairix_arch_api::CpuStateCapture as _;

use crate::backtrace::Backtracer;
use crate::kernel_arch::halt_current_hart;
use crate::serial::SbiWriter;

const REPORTER: Reporter = Reporter { port: "riscv64" };

/// Shared `#[panic_handler]` body for the riscv64 boot binaries: report the
/// panic on the SBI console and park the hart.
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
        &mut SbiWriter,
        entry,
        processor(),
        message,
        location,
        || {
            let bt = Backtracer::new();
            bt.boot_stack_verdict(Some(bt.capture().sp))
        },
    );
    halt_current_hart()
}

/// Report a fatal trap no fault handler claimed and park the hart.
pub fn report_unclaimed_fault(fault: &KernelFault) -> ! {
    let entry = fatal::enter();
    REPORTER.fault(
        &mut SbiWriter,
        entry,
        processor(),
        fault,
        format_args!("{}", crate::fault::Decoded(fault)),
        || Backtracer::new().boot_stack_verdict(fault.sp),
    );
    halt_current_hart()
}

/// The running hart, by the SBI id `tp` holds: the map to a dense CPU id
/// lives in the kernel's arch handle, which a report from below it cannot
/// reach.
fn processor() -> Processor {
    Processor::Hardware {
        key: "hart",
        id: u64::from(crate::smp::current_hartid()),
    }
}
