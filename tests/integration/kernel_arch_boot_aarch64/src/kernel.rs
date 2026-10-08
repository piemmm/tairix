//! The production-boot vertical's run, shared by the GICv2 and GICv3
//! binaries.

use core::num::NonZeroU16;
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use tairix_arch_aarch64::{handle_panic_via_serial, qemu_exit, SerialSink, SERIAL_SINK};
use tairix_itest_finisher::fail_point;
use tairix_kalloc::{FreeListAllocator, Heap, HEAP_BYTES};
use tairix_kernel::aarch64::boot as boot_aarch64;
use tairix_log::{Event, EventId, Sink};
use tairix_test_kheap_growth as kheap_growth;

// The board's device tree, embedded at build time: the boot pipeline
// discovers the board from it because QEMU passes no `x0` DTB pointer at an
// ELF `-kernel` entry.
use crate::tree::DTB_BLOB;

/// Static boot heap.
///
/// The boot heap, in `.bss`, which the layout places below `__kernel_end` so
/// the boot memory map never hands a heap frame to the allocator.
static HEAP: Heap = Heap::ZERO;

/// Global allocator backed by [`HEAP`].
///
/// SAFETY: the page-aligned `HEAP` static outlives the binary and
/// the allocator is its only consumer.
#[global_allocator]
static ALLOCATOR: FreeListAllocator =
    unsafe { FreeListAllocator::new(HEAP.as_mut_ptr(), HEAP_BYTES) };

/// `EventId` emitted by `kernel_core::kernel_main` once every init
/// phase completed. Pinned by the `event_ids_are_unique` test in
/// `kernel/core/src/audit.rs`.
const BOOT_COMPLETED_EVENT_ID: EventId = EventId(4004);

/// `EventId` of `AuditEvent::SecondaryCpuStartFailed` — a refused
/// PSCI `CPU_ON` on this fully-emulable board is a regression, not a
/// degrade to tolerate. Pinned by `event_ids_are_unique`.
const SECONDARY_START_FAILED_EVENT_ID: EventId = EventId(4071);

/// `EventId` of `AuditEvent::SecondaryCpuOnline` — each started
/// secondary core's own attestation that it reached the kernel
/// dispatch loop. Pinned by `event_ids_are_unique`.
const SECONDARY_ONLINE_EVENT_ID: EventId = EventId(4072);

/// Secondary cores the `-smp 4` run must bring online (the embedded
/// tree's `/cpus` minus the boot core). Matches the harness `cpus`
/// and the `build.rs` DTB dump — all three name the same topology.
const EXPECTED_SECONDARIES: u32 = 3;
/// Failure finisher codes, distinct per failure site.
const FAIL_VIDEO_INACTIVE: NonZeroU16 = fail_point!(1);
const FAIL_SECONDARY_START: NonZeroU16 = fail_point!(2);
const FAIL_KHEAP_GROWTH: NonZeroU16 = fail_point!(3);
/// Failure finisher code for a boot that left the fatal-fault slot
/// empty, so a kernel-mode exception would park the CPU mutely.
const FAIL_NO_FAULT_HANDLER: NonZeroU16 = fail_point!(4);

/// Set once `BootCompleted` was observed (with the video console
/// active); the PASS finisher additionally requires every secondary
/// online.
static BOOT_COMPLETED: AtomicBool = AtomicBool::new(false);

/// Count of `SecondaryCpuOnline` attestations observed.
static SECONDARIES_ONLINE: AtomicU32 = AtomicU32::new(0);

/// Sink that replays every event through [`SERIAL_SINK`] and reports
/// PASS to QEMU once `BootCompleted` **and** all
/// [`EXPECTED_SECONDARIES`] `SecondaryCpuOnline` attestations have
/// been observed — but only if the ramfb framebuffer boot console
/// came up.
///
/// The harness attaches `-device ramfb`, so the production pre-MMU
/// video bring-up must have discovered the `virt` tree's `fw_cfg`
/// node, programmed the scan-out, and switched the console to the
/// screen (`video::is_active`). A boot that completed with the
/// console still on the UART is a display regression reported as
/// FAIL, not a pass with a dark screen. A `SecondaryCpuStartFailed`
/// is an immediate FAIL: on the emulated `virt` board with a
/// discovered PSCI conduit every `CPU_ON` must be accepted.
///
/// `write_event` runs concurrently once the secondaries are live
/// (each core emits through this same sink), so the completion
/// bookkeeping is plain atomics and the PASS condition is checked on
/// both the boot-completed and the online edges — whichever lands
/// last fires the finisher exactly once (the semihosting exit ends
/// the whole machine).
struct BootCompletedExitSink;

impl BootCompletedExitSink {
    /// Fire the PASS finisher iff boot completed and every
    /// secondary attested.
    fn exit_if_complete() {
        if BOOT_COMPLETED.load(Ordering::SeqCst)
            && SECONDARIES_ONLINE.load(Ordering::SeqCst) >= EXPECTED_SECONDARIES
        {
            qemu_exit::exit_success();
        }
    }
}

impl Sink for BootCompletedExitSink {
    fn write_event(&self, event: &Event<'_>) {
        // Replay through the serial sink so the QEMU transcript
        // records the full boot timeline.
        SerialSink::new().write_event(event);
        if event.id == BOOT_COMPLETED_EVENT_ID {
            if !tairix_arch_aarch64::video::is_active() {
                qemu_exit::exit_failure(FAIL_VIDEO_INACTIVE);
            }
            // Boot proved the remap window exists; this proves the heap
            // can grow a region into it and dereference every page.
            if kheap_growth::verify(&ALLOCATOR, &SERIAL_SINK).is_err() {
                qemu_exit::exit_failure(FAIL_KHEAP_GROWTH);
            }
            // A booted kernel reports a fault with its post-mortem; an
            // empty slot leaves only the port's bare report, with no
            // registers and no backtrace.
            if tairix_arch_api::fault::fault_handler().is_none() {
                qemu_exit::exit_failure(FAIL_NO_FAULT_HANDLER);
            }
            BOOT_COMPLETED.store(true, Ordering::SeqCst);
            Self::exit_if_complete();
        } else if event.id == SECONDARY_ONLINE_EVENT_ID {
            SECONDARIES_ONLINE.fetch_add(1, Ordering::SeqCst);
            Self::exit_if_complete();
        } else if event.id == SECONDARY_START_FAILED_EVENT_ID {
            qemu_exit::exit_failure(FAIL_SECONDARY_START);
        }
    }
}

static AUDIT_SINK: BootCompletedExitSink = BootCompletedExitSink;

/// Forward to the shared aarch64 panic bridge. A panic before
/// `BootCompleted` parks the CPU, the run times out, and the harness
/// reports `Outcome::Timeout` — the documented fail-loud behaviour.
#[panic_handler]
fn tairix_kernel_arch_boot_aarch64_panic(info: &PanicInfo<'_>) -> ! {
    handle_panic_via_serial(info)
}

/// Boot entry point — the symbol the arch crate's `boot.s`
/// trampoline calls (via `tairix_arch_aarch64_main`).
///
/// QEMU hands no DTB pointer (`_dtb == 0`), so the embedded `virt`
/// blob's address is forwarded to the production boot pipeline with
/// the audit-observer sink in place.
#[no_mangle]
pub extern "C" fn kernel_main(_dtb: u64) -> ! {
    let dtb = DTB_BLOB.as_ptr() as u64;
    boot_aarch64::boot(
        dtb,
        &ALLOCATOR,
        &SERIAL_SINK,
        &AUDIT_SINK,
        tairix_log::Level::Info,
        &tairix_kernel::hwtree_store::HW_TREE_SOURCE,
    )
}
