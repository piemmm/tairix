//! QEMU integration test: an image written to a larger medium grows its root
//! to fill it.
//!
//! The production aarch64 pipeline boots over the encrypted-root whole-disk
//! image planted on a medium four times its size. At the writable root mount
//! the kernel extends the root partition — the image's last — to the medium's
//! end, then grows the volume into it, reporting each step as `ROOT_GROWN`.
//! The partition grows first, so the volume's record witnesses both: PASS on
//! it, FAIL on `ROOT_GROWTH_REFUSED`.

#![cfg_attr(itest_aarch64, no_std)]
#![cfg_attr(itest_aarch64, no_main)]
#![deny(missing_docs)]

#[cfg(itest_aarch64)]
mod kernel {
    use core::panic::PanicInfo;

    use tairix_arch_aarch64::{handle_panic_via_serial, qemu_exit, SerialSink, SERIAL_SINK};
    use tairix_kalloc::{FreeListAllocator, Heap, HEAP_BYTES};
    use tairix_kernel::aarch64::boot as boot_aarch64;
    use tairix_kernel::unlock_orchestrate::{ROOT_GROWN, ROOT_GROWTH_REFUSED};
    use tairix_log::{Event, FieldValue, Sink};

    // QEMU passes no DTB pointer at an ELF `-kernel` entry, so the `virt`
    // tree is embedded at build time.
    include!(concat!(env!("OUT_DIR"), "/dtb_fixture.rs"));

    /// The global allocator's arena.
    static HEAP: Heap = Heap::ZERO;

    /// Global allocator backed by [`HEAP`].
    ///
    /// SAFETY: the page-aligned `HEAP` static outlives the binary and the
    /// allocator is its only consumer.
    #[global_allocator]
    static ALLOCATOR: FreeListAllocator =
        unsafe { FreeListAllocator::new(HEAP.as_mut_ptr(), HEAP_BYTES) };

    /// Replays every event to the serial console and ends the run on the
    /// root's growth record.
    struct GrowthSink;

    impl Sink for GrowthSink {
        fn write_event(&self, event: &Event<'_>) {
            SerialSink::new().write_event(event);
            if event.id == ROOT_GROWTH_REFUSED {
                qemu_exit::exit_failure(core::num::NonZeroU16::MIN);
            }
            let volume = event
                .fields
                .iter()
                .any(|field| field.key == "what" && field.value == FieldValue::Str("volume"));
            if event.id == ROOT_GROWN && volume {
                qemu_exit::exit_success();
            }
        }
    }

    static AUDIT_SINK: GrowthSink = GrowthSink;

    /// A panic parks the CPU and the run times out: the harness reports it.
    #[panic_handler]
    fn tairix_root_grow_qemu_aarch64_panic(info: &PanicInfo<'_>) -> ! {
        handle_panic_via_serial(info)
    }

    /// Boot entry point, called by the arch crate's trampoline.
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
}

#[cfg(not(itest_aarch64))]
fn main() {}
