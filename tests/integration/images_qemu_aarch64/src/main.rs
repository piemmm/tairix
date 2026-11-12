//! `plans/DISK-IMAGES.md` DI7 and `plans/ARCHIVES.md` AR11 vertical: boot
//! the production aarch64 `tairix-kernel` pipeline against a root disk whose
//! read-only `/System` volume carries the signed AmigaDOS, ADFS, ISO 9660 and
//! archive drivers and the test-only `images` fixture, and whose root volume
//! holds an Acorn ADFS floppy, an AmigaDOS floppy, the RISC OS disc and an
//! archive of each format in the user's home.
//!
//! ## What this vertical asserts
//!
//! The scripted root shell runs `images`, which copies each image and
//! archive into the home, mounts it with the real `mount` — the kernel
//! recognising which is which from its contents, publishing its volume node
//! under the kernel's image-volumes node, and `devmgr` loading the format's
//! driver against it — reads what it holds, writes on the floppies and is
//! refused on the disc and on an image the user may only read, ejects each
//! with the real `unmount`, and mounts the floppies again to read back what
//! it wrote. Each archive's `ReadMe` is read with its RISC OS filetype; a
//! file is written and typed in every archive but the RAR, which refuses it,
//! and each written archive, committed as it is ejected, is mounted again to
//! read both files and their types back.
//!
//! A failing fixture states its reason and never exits, so the sink below
//! never arms and the run times out with the reason in the transcript.
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
    use tairix_kalloc::{Arena, FreeListAllocator, HEAP_BYTES};
    use tairix_kernel::aarch64::boot as boot_aarch64;
    use tairix_log::{Event, EventId, Sink};
    use tairix_test_images::COMMAND;

    // The canonical QEMU `virt` device tree, dumped and embedded at build
    // time (`build.rs`): QEMU passes no `x0` DTB pointer at an ELF `-kernel`
    // entry.
    include!(concat!(env!("OUT_DIR"), "/dtb_fixture.rs"));

    /// The global allocator's arena.
    static HEAP: Arena<HEAP_BYTES> = Arena::ZERO;

    /// Global allocator backed by [`HEAP`].
    ///
    /// SAFETY: the page-aligned `HEAP` static outlives the binary and the
    /// allocator is its only consumer.
    #[global_allocator]
    static ALLOCATOR: FreeListAllocator =
        unsafe { FreeListAllocator::new(HEAP.as_mut_ptr(), HEAP_BYTES) };

    /// `SyscallInvoked`, the audited record the sink watches for exits.
    const SYSCALL_INVOKED_EVENT_ID: EventId = EventId(5000);

    /// The run's end: the fixture's exit, which only a pass lets it make,
    /// then the shell's.
    static END: FixtureExit = FixtureExit::new(&[COMMAND], SYSCALL_INVOKED_EVENT_ID);

    /// Replays every event to the serial console and ends the run on the
    /// shell's exit after the fixture's.
    struct ImagesSink;

    impl Sink for ImagesSink {
        fn write_event(&self, event: &Event<'_>) {
            SerialSink::new().write_event(event);
            if END.passed(event) {
                qemu_exit::exit_success();
            }
        }
    }

    static AUDIT_SINK: ImagesSink = ImagesSink;

    /// Forward to the shared aarch64 panic bridge. A panic parks the CPU, so
    /// the run times out and the harness reports it.
    #[panic_handler]
    fn tairix_images_qemu_aarch64_panic(info: &PanicInfo<'_>) -> ! {
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
