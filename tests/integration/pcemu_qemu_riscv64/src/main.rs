//! `plans/PCEMU.md` PC21 QEMU vertical: boot the production `riscv64` `tairix-kernel`
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

#![cfg_attr(itest_riscv64, no_std)]
#![cfg_attr(itest_riscv64, no_main)]
#![deny(missing_docs)]

#[cfg(itest_riscv64)]
mod kernel {
    use core::panic::PanicInfo;

    use tairix_arch_riscv64::{handle_panic_via_serial, qemu_exit, SerialSink, SERIAL_SINK};
    use tairix_itest_witness::FixtureExit;
    use tairix_kalloc::{Arena, FreeListAllocator, HEAP_BYTES};
    use tairix_kernel::riscv64::boot as boot_riscv64;
    use tairix_log::{Event, EventId, Sink};
    use tairix_test_pcemu::COMMAND;

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

    /// The global allocator's arena, in the NOLOAD `.heap` section so the boot
    /// trampoline does not spend time zeroing it.
    #[link_section = ".heap"]
    static HEAP: Arena<HEAP_BYTES> = Arena::ZERO;

    /// Global allocator backed by [`HEAP`].
    ///
    /// SAFETY: the page-aligned `HEAP` static outlives the binary and the
    /// allocator is its only consumer.
    #[global_allocator]
    static ALLOCATOR: FreeListAllocator =
        unsafe { FreeListAllocator::new(HEAP.as_mut_ptr(), HEAP_BYTES) };

    /// A panic parks the hart, so the run times out and fails loudly.
    #[panic_handler]
    fn tairix_pcemu_qemu_riscv64_panic(info: &PanicInfo<'_>) -> ! {
        handle_panic_via_serial(info)
    }

    /// The symbol the arch crate's boot trampoline calls: the production
    /// pipeline, over the SBI hand-off.
    #[no_mangle]
    pub extern "C" fn kernel_main(hartid: u64, dtb: u64) -> ! {
        boot_riscv64::boot(
            hartid,
            dtb,
            &ALLOCATOR,
            &SERIAL_SINK,
            &AUDIT_SINK,
            // `SyscallInvoked` is `Debug`, below the default filter, and the
            // run's end reads it.
            tairix_log::Level::Debug,
        )
    }
}

#[cfg(not(itest_riscv64))]
fn main() {}
