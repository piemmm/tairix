//! QEMU vertical: **3D work against QEMU's Venus GPU**
//! (`plans/GPU.md` GP5.2, GP5.3).
//!
//! The production boot discovers QEMU's OpenGL virtio GPU, with Venus, blob
//! resources and a host-visible region, as a virtio-PCI function on a `q35`
//! board, and grants the region to its node; `devmgr` autoloads the Venus
//! check (`tests/integration/venus_check_program`) in the display driver's
//! place. The check brings the device up as that driver does, makes a Venus
//! context and two host blobs, maps them into the region and into itself,
//! reads back through a blob's second mapping what its first wrote, submits a
//! fenced stream, and lets it all go. It then makes the driver's Venus
//! backend a GPU as the driver does and holds it to the software device with
//! `gpucheck`'s work. The host renders through Mesa's software Vulkan driver,
//! so the run needs no GPU.
//!
//! PASS is the check's own verdict; its failure ends the run at once with the
//! step that failed in the transcript.

#![cfg_attr(itest_x86_64, no_std)]
#![cfg_attr(itest_x86_64, no_main)]
#![deny(missing_docs)]

#[cfg(itest_x86_64)]
mod kernel {
    use core::panic::PanicInfo;

    use tairix_arch_x86_64::qemu_exit;
    use tairix_kernel::kalloc::{Arena, HEAP_BYTES};
    use tairix_kernel::{boot, handle_panic_via_kernel_core, FreeListAllocator, SerialSink};
    use tairix_log::{Event, Sink};
    use tairix_test_venus_check::{FAILED, PASSED};

    /// Sink that replays every event to the serial line and ends the run on
    /// the Venus check's verdict.
    struct VerdictSink;

    impl Sink for VerdictSink {
        fn write_event(&self, event: &Event<'_>) {
            SerialSink::new().write_event(event);
            if event.id == PASSED {
                qemu_exit::exit_success();
            }
            if event.id == FAILED {
                qemu_exit::exit_failure();
            }
        }
    }

    static AUDIT_SINK: VerdictSink = VerdictSink;

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
}

#[cfg(not(itest_x86_64))]
fn main() {}
