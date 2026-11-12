//! `plans/PCEMU.md` PC21 QEMU vertical: boot the production `aarch64` `tairix-kernel`
//! pipeline against the root image the PC emulator's verticals plant the
//! `pcemucheck` fixture on, and have the scripted root shell run it. The
//! fixture checks the emulator's reference digest under this target's
//! compiler, then boots DOS in the machine's sandboxed worker with a compiler
//! sandbox's code installed into it and run natively
//! (`tests/integration/pcemu_program`).
//!
//! The run ends on the shell's exit after the fixture's, which the script
//! types only once the fixture has printed its pass; a failing fixture parks,
//! so the run times out with its reason on serial.

#![cfg_attr(itest_aarch64, no_std)]
#![cfg_attr(itest_aarch64, no_main)]
#![deny(missing_docs)]

#[cfg(itest_aarch64)]
mod kernel {
    use core::panic::PanicInfo;

    use tairix_arch_aarch64::{handle_panic_via_serial, qemu_exit, SerialSink, SERIAL_SINK};
    use tairix_itest_witness::FixtureExit;
    use tairix_kalloc::{Arena, FreeListAllocator, HEAP_BYTES};
    use tairix_kernel::aarch64::boot as boot_aarch64;
    use tairix_log::{Event, EventId, Sink};
    use tairix_test_pcemu::COMMAND;

    // The canonical QEMU `virt` device tree, dumped and embedded at build
    // time: QEMU passes no DTB pointer at an ELF `-kernel` entry.
    include!(concat!(env!("OUT_DIR"), "/dtb_fixture.rs"));

    /// `SyscallInvoked`, the audited record [`END`] reads.
    const SYSCALL_INVOKED_EVENT_ID: EventId = EventId(5000);

    /// The run's end: the shell's exit after the fixture's, whose machine
    /// worker and compiler run as the fixture too.
    static END: FixtureExit = FixtureExit::new(&[COMMAND], SYSCALL_INVOKED_EVENT_ID);

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

    /// A panic parks the CPU, so the run times out and fails loudly.
    #[panic_handler]
    fn tairix_pcemu_qemu_aarch64_panic(info: &PanicInfo<'_>) -> ! {
        handle_panic_via_serial(info)
    }

    /// The symbol the arch crate's boot trampoline calls: the production
    /// pipeline, over the embedded device tree.
    #[no_mangle]
    pub extern "C" fn kernel_main(_dtb: u64) -> ! {
        let dtb = DTB_BLOB.as_ptr() as u64;
        boot_aarch64::boot(
            dtb,
            &ALLOCATOR,
            &SERIAL_SINK,
            &AUDIT_SINK,
            // `SyscallInvoked` is `Debug`, below the default filter, and the
            // run's end reads it.
            tairix_log::Level::Debug,
            &tairix_kernel::hwtree_store::HW_TREE_SOURCE,
        )
    }
}

#[cfg(not(itest_aarch64))]
fn main() {}
