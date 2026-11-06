//! The twin-volume vertical (the *Twins* rules of
//! `docs/src/filesystem/drives.md`): boot the production aarch64
//! `tairix-kernel` pipeline against a root disk whose read-only `/System`
//! volume carries the signed xHCI, USB mass-storage, volume-manager and FAT32
//! drivers and the test-only `twins` fixture, with two USB sticks on QEMU's
//! xHCI controller each holding a copy of one labelled FAT32 image
//! (`tairix_test_fat32_image::build_labelled_image`).
//!
//! ## What this vertical asserts
//!
//! `devmgr` autoloads the host-controller driver against the controller's PCI
//! function, the mass-storage driver against each stick's interface, and the
//! volume manager against each unit they publish. The two volumes carry one
//! identity, so the kernel attaches them together: the first under the
//! identity and the label's name, the second under a key and a name of its
//! own. The scripted root shell runs `twins`, which waits on the mount table
//! for both, then checks one identity under two keys with `id::<identity>` the
//! first, a file's id distinct on each, a write through each landing on its
//! own, and each detached by its key, printing a marker as each step holds.
//!
//! A failing fixture states its reason and never exits, so the sink below
//! never arms and the run times out with the reason, and the last marker
//! before it, in the transcript.
//!
//! ## How it differs from a production kernel
//!
//! It reuses the production aarch64 boot pipeline unchanged and replaces only
//! the audit sink, in this test bin, so no QEMU-exit shortcut reaches a
//! production build.

#![cfg_attr(itest_aarch64, no_std)]
#![cfg_attr(itest_aarch64, no_main)]
#![deny(missing_docs)]

#[cfg(itest_aarch64)]
mod kernel {
    use core::panic::PanicInfo;

    use tairix_arch_aarch64::{handle_panic_via_serial, qemu_exit, SerialSink, SERIAL_SINK};
    use tairix_itest_witness::FixtureExit;
    use tairix_kalloc::{FreeListAllocator, Heap, HEAP_BYTES};
    use tairix_kernel::aarch64::boot as boot_aarch64;
    use tairix_kernel_syscall::AuditEvent;
    use tairix_log::{Event, Sink};
    use tairix_test_twins::COMMAND;

    // The canonical QEMU `virt` device tree, dumped and embedded at build
    // time (`build.rs`): QEMU passes no `x0` DTB pointer at an ELF `-kernel`
    // entry.
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

    /// The run's end: the fixture's exit, which only a pass lets it make,
    /// then the shell's.
    static END: FixtureExit = FixtureExit::new(&[COMMAND], AuditEvent::SyscallInvoked.id());

    /// Replays every event to the serial console and ends the run on the
    /// shell's exit after the fixture's.
    struct TwinsSink;

    impl Sink for TwinsSink {
        fn write_event(&self, event: &Event<'_>) {
            SerialSink::new().write_event(event);
            if END.passed(event) {
                qemu_exit::exit_success();
            }
        }
    }

    static AUDIT_SINK: TwinsSink = TwinsSink;

    /// Forward to the shared aarch64 panic bridge. A panic parks the CPU, so
    /// the run times out and the harness reports it.
    #[panic_handler]
    fn tairix_twins_qemu_aarch64_panic(info: &PanicInfo<'_>) -> ! {
        handle_panic_via_serial(info)
    }

    /// Boot entry point, called by the arch crate's `boot.s` trampoline.
    #[no_mangle]
    pub extern "C" fn kernel_main(_dtb: u64) -> ! {
        let dtb = DTB_BLOB.as_ptr() as u64;
        boot_aarch64::boot(
            dtb,
            &ALLOCATOR,
            &SERIAL_SINK,
            &AUDIT_SINK,
            // `SyscallInvoked` is `Debug`, below the default filter, and the
            // sink above reads it.
            tairix_log::Level::Debug,
            &tairix_kernel::hwtree_store::HW_TREE_SOURCE,
        )
    }
}

#[cfg(not(itest_aarch64))]
fn main() {}
