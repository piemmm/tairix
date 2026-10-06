//! The production riscv64 boot with an audit sink that reports PASS on the
//! first key an autoloaded input driver delivers.

use core::panic::PanicInfo;

use tairix_arch_riscv64::{handle_panic_via_serial, qemu_exit, SerialSink, SERIAL_SINK};
use tairix_kalloc::{FreeListAllocator, Heap, HEAP_BYTES};
use tairix_kernel::riscv64::boot as boot_riscv64;
use tairix_kernel_core::AuditEvent;
use tairix_log::{Event, FieldValue, Sink};

/// Static boot heap.
///
/// Placed in the linker's dedicated `.heap` (NOLOAD) section so the boot
/// trampoline does not zero its bytes (the bump allocator does not require
/// zeroed backing) and the boot pipeline excludes it from the usable
/// physical-memory map, exactly as the production riscv64 kernel binary's
/// heap does. `static mut` because the bump allocator hands out disjoint
/// slices via an atomic cursor; the storage is otherwise never aliased.
#[link_section = ".heap"]
static mut HEAP: Heap = Heap::ZERO;

/// Global allocator backed by [`HEAP`].
///
/// SAFETY: the page-aligned `HEAP` static outlives the binary and the
/// allocator is its only consumer.
#[global_allocator]
static ALLOCATOR: FreeListAllocator =
    unsafe { FreeListAllocator::new(core::ptr::addr_of!(HEAP) as *mut u8, HEAP_BYTES) };

/// Sink that replays every event through [`SERIAL_SINK`] and reports PASS
/// to QEMU the first time an `AuditEvent::InputDelivered` record with
/// `kind=key` appears — the autoloaded user-space virtio-input keyboard
/// driver delivering the typed key to the input-focus arbiter. An
/// unrecognised `kind` value flips nothing (fail closed — a malformed
/// witness can never satisfy PASS).
struct AutoloadInputSink;

impl Sink for AutoloadInputSink {
    fn write_event(&self, event: &Event<'_>) {
        // Replay through the serial sink so the QEMU transcript records the
        // full boot + unlock + autoload + input timeline (the harness also
        // gates its key injection on the `sc=irq_bind` line of this
        // replay — the autoloaded driver's arm step).
        SerialSink::new().write_event(event);
        if event.id.0 != AuditEvent::InputDelivered.id().0 {
            return;
        }
        for field in event.fields {
            if field.key == "kind" && matches!(field.value, FieldValue::Str("key")) {
                qemu_exit::exit_success();
            }
        }
    }
}

static AUDIT_SINK: AutoloadInputSink = AutoloadInputSink;

/// Forward to the shared riscv64 panic bridge. A panic before the PASS
/// finisher parks the hart, the run times out, and the harness reports
/// `Outcome::Timeout` — the documented fail-loud behaviour.
#[panic_handler]
fn tairix_autoload_input_qemu_riscv64_panic(info: &PanicInfo<'_>) -> ! {
    handle_panic_via_serial(info)
}

/// Boot entry point — the symbol the arch crate's `boot.s` trampoline
/// calls (via `tairix_arch_riscv64_main`).
///
/// Forwards the SBI hand-off values (`a0` = hartid, `a1` = DTB) to the
/// production boot pipeline with the audit-observer sink in place.
#[no_mangle]
pub extern "C" fn kernel_main(hartid: u64, dtb: u64) -> ! {
    // The autoloaded driver's arm step (`irq_bind`) is an audited syscall
    // whose `SyscallInvoked` record is `Debug`, below the default `Info`
    // filter; the harness waits for that record's `sc=irq_bind` serial
    // marker before injecting the key, so boot with the filter lowered.
    boot_riscv64::boot(
        hartid,
        dtb,
        &ALLOCATOR,
        &SERIAL_SINK,
        &AUDIT_SINK,
        tairix_log::Level::Debug,
    )
}
