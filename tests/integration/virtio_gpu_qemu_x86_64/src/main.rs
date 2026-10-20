//! QEMU vertical: **an x86_64 machine whose only display is a virtio GPU**
//! (`plans/FIX-DISPLAY-ACCELERATION.md` C).
//!
//! The production boot discovers the GPU as a virtio-PCI function on a `q35`
//! board, marks it the console's host and makes the framebuffer console
//! console 0; `devmgr` autoloads the virtio-gpu driver, which hosts the
//! console on the device; the unlock passphrase is typed at the virtual
//! keyboard; and login, finding a display, runs the graphical login screen,
//! whose frame the driver presents. The runner's screendump of the GPU's
//! screen holds the console.
//!
//! The PASS witnesses are the pair the shared
//! `tairix_itest_witness::GreeterFrame` reads: the greeter's bundle loaded,
//! then the first frame the display took, reported by the process the kernel
//! names as serving the display endpoint.

#![cfg_attr(itest_x86_64, no_std)]
#![cfg_attr(itest_x86_64, no_main)]
#![deny(missing_docs)]

#[cfg(itest_x86_64)]
mod kernel {
    use core::panic::PanicInfo;

    use tairix_arch_x86_64::qemu_exit;
    use tairix_itest_witness::GreeterFrame;
    use tairix_kernel::kalloc::{Heap, HEAP_BYTES};
    use tairix_kernel::{boot, handle_panic_via_kernel_core, FreeListAllocator, SerialSink};
    use tairix_log::{Event, Sink};

    /// Sink that replays every event, audit and diagnostic, to the serial
    /// line and ends the run once the login screen's frame reached the
    /// display.
    struct GreeterSink(GreeterFrame);

    impl Sink for GreeterSink {
        fn write_event(&self, event: &Event<'_>) {
            SerialSink::new().write_event(event);
            if self.0.observe(
                event,
                tairix_itest_witness::GreeterRecords {
                    app_loaded: tairix_appload::events::APP_LOADED,
                    endpoint_created: tairix_kernel_ipc::AuditEvent::CallEndpointCreated.id(),
                    first_present: tairix_display::events::FIRST_PRESENT,
                },
            ) {
                qemu_exit::exit_success();
            }
        }
    }

    static AUDIT_SINK: GreeterSink = GreeterSink(GreeterFrame::new());

    /// The global allocator's arena.
    static HEAP: Heap = Heap::ZERO;

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
    fn tairix_virtio_gpu_qemu_x86_64_panic(info: &PanicInfo<'_>) -> ! {
        handle_panic_via_kernel_core(info)
    }

    /// The symbol the arch crate's boot trampoline calls, forwarding to the
    /// production boot pipeline with the witness observing both streams.
    #[no_mangle]
    pub extern "C" fn kernel_main(multiboot_info: u64) -> ! {
        boot(
            multiboot_info,
            &ALLOCATOR,
            // The display service's record reaches the diagnostic stream.
            &AUDIT_SINK,
            &AUDIT_SINK,
            // The runner types the passphrase on the `sc=irq_bind` markers of
            // `SyscallInvoked`, a `Debug` record.
            tairix_log::Level::Debug,
        )
    }
}

#[cfg(not(itest_x86_64))]
fn main() {}
