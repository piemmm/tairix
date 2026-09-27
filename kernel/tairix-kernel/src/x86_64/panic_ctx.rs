//! x86_64 half of the fatal-report bridge: the port's four values plus
//! its two entry points ([`handle_panic_via_kernel_core`] for a `panic!`,
//! [`install_kernel_fault_handler`] for a fatal supervisor exception).
//!
//! # Why this lives in the library half
//!
//! Each binary (`src/main.rs` and the QEMU integration test bins) must
//! declare its own `#[panic_handler]` — Rust forbids library-defined panic
//! handlers — so the body they all call lives here. The shared policy is
//! [`crate::fatal_bridge`]; this module is only the x86_64 values.
//!
//! # How [`BinArch`] is reached
//!
//! The report needs a `&BinArch` (for `current_cpu` / `halt`). The arch
//! handle does not exist until [`crate::x86_64::boot::boot`] has parsed the
//! ACPI MADT, so it cannot live in a plain `static`; `boot` publishes
//! `Arc::as_ptr(&arc)` into [`PANIC_ARCH_PTR`] before any code that could
//! panic runs. Before that publish the port's own report stands in (fail
//! closed, never a silent reset).

use core::panic::PanicInfo;
use core::sync::atomic::{AtomicPtr, Ordering};

use tairix_arch_api::backtrace::CpuStateCapture;
use tairix_arch_api::fatal::KernelFault;
use tairix_arch_api::fault::{set_fault_handler, SetFaultHandlerError};
use tairix_arch_x86_64::backtrace::Backtracer;
use tairix_arch_x86_64::serial::SERIAL_SINK;
use tairix_log::Sink;

use crate::fatal_bridge::{report_kernel_fault, report_panic, FatalReport};
use crate::x86_64::arch_wrapper::BinArch;

/// The x86_64 post-mortem CPU-state handle the bridge attaches so the
/// report carries registers + a backtrace. Zero-sized and stateless.
static BACKTRACER: Backtracer = Backtracer::new();

/// `AtomicPtr<BinArch>` published by [`crate::x86_64::boot::boot`] once the
/// architecture handle is constructed.
///
/// The pointer is `Arc::as_ptr(&arc)`, which is stable for the lifetime of
/// the `Arc`; the `Arc` itself lives for the lifetime of the running kernel
/// (it is held by `BootInfo` and re-cloned into `kernel_core`'s internal
/// `KernelState`).
pub static PANIC_ARCH_PTR: AtomicPtr<BinArch> = AtomicPtr::new(core::ptr::null_mut());

/// Publish `arch` into [`PANIC_ARCH_PTR`].
///
/// Called by [`crate::x86_64::boot::boot`] immediately after constructing
/// the `Arc<BinArch>` (and before installing the syscall dispatch callback
/// / arming the LAPIC timer).
///
/// # Safety
///
/// `arch_ptr` must remain valid for the lifetime of the kernel image.
/// The standard caller is `Arc::as_ptr(&arc) as *mut BinArch`, where
/// `arc` is kept alive by `BootInfo`'s `arch` field.
pub unsafe fn publish_arch(arch_ptr: *const BinArch) {
    // `Release` so a report that follows on another CPU observes the
    // initialised pointer; the matching `Acquire` load lives in
    // `X86Fatal::arch`.
    PANIC_ARCH_PTR.store(arch_ptr.cast_mut(), Ordering::Release);
}

/// This port's [`FatalReport`] values.
struct X86Fatal;

impl FatalReport for X86Fatal {
    type Arch = BinArch;

    fn arch() -> Option<&'static BinArch> {
        let raw = PANIC_ARCH_PTR.load(Ordering::Acquire);
        if raw.is_null() {
            return None;
        }
        // SAFETY: `publish_arch` only ever stores the result of
        // `Arc::as_ptr(&arc)`, where `arc: Arc<BinArch>` is held by
        // `kernel_core::BootInfo` for the lifetime of the running kernel;
        // the pointee therefore outlives every report that can observe a
        // non-null `PANIC_ARCH_PTR`.
        Some(unsafe { &*raw })
    }

    fn audit_sink() -> &'static (dyn Sink + Sync) {
        &SERIAL_SINK
    }

    fn backtrace() -> &'static dyn CpuStateCapture {
        &BACKTRACER
    }

    fn report_panic_before_init(info: &PanicInfo<'_>) -> ! {
        tairix_arch_x86_64::panic::handle_panic_via_serial(info)
    }

    fn report_fault_before_init(fault: &KernelFault) -> ! {
        tairix_arch_x86_64::panic::report_unclaimed_fault(fault)
    }
}

/// Shared `#[panic_handler]` body used by every bin crate.
pub fn handle_panic_via_kernel_core(info: &PanicInfo<'_>) -> ! {
    report_panic::<X86Fatal>(info)
}

/// Install the production fatal-fault handler on this port.
///
/// Called at `boot` entry, before the IDT can deliver anything, so every
/// supervisor exception gets the kernel's post-mortem rather than the port's
/// own report, which carries no register snapshot or backtrace.
///
/// # Errors
///
/// [`SetFaultHandlerError::AlreadyInstalled`] when something published a
/// handler first. The slot is set-once by design, so the existing handler
/// keeps the machine's fatal policy; only a QEMU vertical that deliberately
/// observes its own faults installs ahead of boot.
pub fn install_kernel_fault_handler() -> Result<(), SetFaultHandlerError> {
    set_fault_handler(report_kernel_fault::<X86Fatal>)
}
