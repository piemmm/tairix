//! `plans/GPU.md` GP8 / MG3 QEMU vertical: **untrusted kernels on a GPU
//! taking Venus work**.
//!
//! The production `x86_64` pipeline boots on a `q35` board whose virtio GPU
//! takes Venus work; its driver autoloads and serves the GPU on its node's
//! channel, the board having no DMA translation unit. The scripted root
//! shell runs the `shadercheck` fixture (`tests/integration/shadercheck_program`),
//! which hands each kernel the disk plants to `gpucheck --shader` as any
//! program the account starts: an honest kernel, in WGSL and compiled, must
//! agree with the software device; one reaching past every memory must read
//! only zeros and nothing of its neighbour's; one that never ends must be
//! lost while the GPU serves on; and one the front end and one the validator
//! refuse must be refused by name before any device is handed them. The host
//! renders through Mesa's software Vulkan driver, so the run needs no GPU.
//!
//! The run ends on the shell's exit after the fixture's, which the script
//! types only once the fixture has logged that every kernel was answered as
//! it must be; a kernel answered otherwise is logged with what it answered,
//! and the run times out with it on serial.

#![cfg_attr(itest_x86_64, no_std)]
#![cfg_attr(itest_x86_64, no_main)]
#![deny(missing_docs)]

#[cfg(itest_x86_64)]
mod kernel {
    use core::panic::PanicInfo;

    use tairix_arch_x86_64::qemu_exit;
    use tairix_itest_witness::FixtureExit;
    use tairix_kernel::kalloc::{Arena, HEAP_BYTES};
    use tairix_kernel::{
        boot, handle_panic_via_kernel_core, FreeListAllocator, SerialSink, SERIAL_SINK,
    };
    use tairix_log::{Event, Sink};
    use tairix_test_shadercheck::COMMAND;

    /// The run's end: the shell's exit after the fixture's.
    static END: FixtureExit = FixtureExit::new(&[COMMAND]);

    /// Replays every event to the serial console and ends the run at [`END`].
    struct EndSink;

    impl Sink for EndSink {
        fn write_event(&self, event: &Event<'_>) {
            SerialSink::new().write_event(event);
            if END.passed(event) {
                qemu_exit::exit_success();
            }
        }
    }

    static AUDIT_SINK: EndSink = EndSink;

    /// The global allocator's arena.
    static HEAP: Arena<HEAP_BYTES> = Arena::ZERO;

    /// Global allocator backed by [`HEAP`].
    ///
    /// SAFETY: the page-aligned `HEAP` static outlives the binary and the
    /// allocator is its only consumer.
    #[global_allocator]
    static ALLOCATOR: FreeListAllocator =
        unsafe { FreeListAllocator::new(HEAP.as_mut_ptr(), HEAP_BYTES) };

    /// A panic halts the guest, so the run times out and fails loudly.
    #[panic_handler]
    fn tairix_gpucheck_shader_qemu_x86_64_panic(info: &PanicInfo<'_>) -> ! {
        handle_panic_via_kernel_core(info)
    }

    /// The symbol the arch crate's boot trampoline calls: the production
    /// pipeline.
    #[no_mangle]
    pub extern "C" fn kernel_main(multiboot_info: u64) -> ! {
        boot(
            multiboot_info,
            &ALLOCATOR,
            &SERIAL_SINK,
            &AUDIT_SINK,
            // `SyscallInvoked` is `Debug`, below the default filter, and the
            // run's end reads it.
            tairix_log::Level::Debug,
        )
    }
}

#[cfg(not(itest_x86_64))]
fn main() {}
