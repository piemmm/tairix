//! The guest kernel every Venus vertical boots: the production x86_64
//! pipeline, observed by a sink that ends the run on what became of the GPU,
//! judged against what the board the binary boots must do with it: serve its
//! channel (`crate::GPU_SERVED`), or refuse it to a node no unit translates
//! on a platform declaring one, the kernel auditing the refusal and the check
//! reporting the GPU unserved once the device's 3D work has run.

use core::panic::PanicInfo;
use core::sync::atomic::{AtomicBool, Ordering};

use tairix_arch_x86_64::qemu_exit;
use tairix_kernel::kalloc::{Arena, HEAP_BYTES};
use tairix_kernel::{boot, handle_panic_via_kernel_core, FreeListAllocator, SerialSink};
use tairix_kernel_core::iommu::DmaPath;
use tairix_kernel_core::AuditEvent;
use tairix_log::{Event, FieldValue, Sink};
use tairix_test_venus_check::{FAILED, PASSED, UNSERVED};

/// Replays every event to the serial line and ends the run on the check's
/// verdict.
struct VerdictSink {
    refusal_audited: AtomicBool,
}

impl Sink for VerdictSink {
    fn write_event(&self, event: &Event<'_>) {
        SerialSink::new().write_event(event);
        if event.id == AuditEvent::DeviceChannelUnconfined.id() && untranslated(event) {
            self.refusal_audited.store(true, Ordering::Release);
        }
        let met = if crate::GPU_SERVED {
            event.id == PASSED
        } else {
            event.id == UNSERVED && self.refusal_audited.load(Ordering::Acquire)
        };
        if met {
            qemu_exit::exit_success();
        }
        if [PASSED, FAILED, UNSERVED].contains(&event.id) {
            qemu_exit::exit_failure();
        }
    }
}

/// Whether a refusal names a node whose DMA bypasses every unit.
fn untranslated(event: &Event<'_>) -> bool {
    event.fields.iter().any(|field| {
        field.key == "dma"
            && matches!(field.value, FieldValue::Str(path) if path == DmaPath::Untranslated.name())
    })
}

static AUDIT_SINK: VerdictSink = VerdictSink {
    refusal_audited: AtomicBool::new(false),
};

/// The global allocator's arena.
static HEAP: Arena<HEAP_BYTES> = Arena::ZERO;

/// Global allocator backed by [`HEAP`].
///
/// SAFETY: the page-aligned `HEAP` static outlives the binary and the
/// allocator is its only consumer.
#[global_allocator]
static ALLOCATOR: FreeListAllocator =
    unsafe { FreeListAllocator::new(HEAP.as_mut_ptr(), HEAP_BYTES) };

/// Forward to the shared panic bridge. A panic halts the guest and the run
/// times out: loud failure, never a false pass.
#[panic_handler]
fn tairix_virtio_gpu_venus_qemu_x86_64_panic(info: &PanicInfo<'_>) -> ! {
    handle_panic_via_kernel_core(info)
}

/// The symbol the arch crate's boot trampoline calls, forwarding to the
/// production boot pipeline with the witness observing both streams.
#[no_mangle]
pub extern "C" fn kernel_main(multiboot_info: u64) -> ! {
    boot(
        multiboot_info,
        &ALLOCATOR,
        // The check's verdict reaches the diagnostic stream.
        &AUDIT_SINK,
        &AUDIT_SINK,
        tairix_log::Level::Info,
    )
}
