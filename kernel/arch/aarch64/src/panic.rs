//! The port's own fatal reports, for the boot binaries that link no kernel
//! core and for a failure before one installed its handlers.
//!
//! Rust forbids library-defined `#[panic_handler]`s, so each binary declares
//! its own one-liner that forwards to [`handle_panic_via_serial`]. Every
//! report is the shared [`tairix_arch_api::fatal`] shape, written on the
//! console directly — never through the queued sink, whose lock the dying
//! code may hold — and every one parks the CPU: never a silent reset. Their
//! closing record is what ends a QEMU run at once rather than on its
//! inactivity budget.
//!
//! The *production* kernel routes a panic and a kernel fault through
//! `tairix_kernel_core`'s post-mortem (a register snapshot and a bounded
//! backtrace) via the bin-crate bridge; these are the paths below it.

use core::fmt;
use core::panic::{Location, PanicInfo};

use tairix_arch_api::fatal::{self, Entry, KernelFault, Processor, Reporter};
use tairix_arch_api::CpuStateCapture as _;

use crate::backtrace::Backtracer;
use crate::kernel_arch::halt_current_cpu;
use crate::serial::ConsoleWriter;

const REPORTER: Reporter = Reporter { port: "aarch64" };

/// Shared `#[panic_handler]` body for the aarch64 boot binaries: report the
/// panic on the console and park the CPU.
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
    let entry = enter();
    REPORTER.panic(
        &mut ConsoleWriter,
        entry,
        processor(),
        message,
        location,
        || {
            let bt = Backtracer::new();
            bt.boot_stack_verdict(Some(bt.capture().sp))
        },
    );
    halt_current_cpu()
}

/// Report a fatal exception no fault handler claimed and park the CPU.
pub fn report_unclaimed_fault(fault: &KernelFault) -> ! {
    let entry = enter();
    REPORTER.fault(
        &mut ConsoleWriter,
        entry,
        processor(),
        fault,
        format_args!("{}", crate::fault::Decoded(fault)),
        || Backtracer::new().boot_stack_verdict(fault.sp),
    );
    halt_current_cpu()
}

/// Enter the fatal-report path, and on the first entry push the buffered
/// lead-up context to the wire ahead of the report.
///
/// With stage-1 translation off every access is Device-nGnRnE, where an
/// atomic read-modify-write may never complete, so the latch is entered with
/// plain accesses and the queued console, which a lock guards, is left alone.
fn enter() -> Entry {
    if !crate::paging::translation_enabled() {
        return fatal::enter_without_atomics();
    }
    let entry = fatal::enter();
    if entry == Entry::Report {
        crate::serial::flush_serial_blocking();
    }
    entry
}

/// The running CPU's dense id: the boot CPU's is seeded at entry, and each
/// secondary publishes its own before it takes an exception.
fn processor() -> Processor {
    Processor::Cpu(crate::smp::current_cpu_index())
}
