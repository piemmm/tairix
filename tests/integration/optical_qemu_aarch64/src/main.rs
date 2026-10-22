//! `plans/DEVICES.md` USB optical vertical: boot the production aarch64
//! `tairix-kernel` pipeline against a root disk whose read-only `/System`
//! volume carries the signed xHCI, USB mass-storage and volume-manager
//! drivers and the test-only `optical` fixture, with a USB optical drive on
//! QEMU's xHCI controller holding a RISC OS disc
//! (`tairix_test_iso9660_image::riscos_disc`).
//!
//! ## What this vertical asserts
//!
//! `devmgr` autoloads the host-controller driver against the controller's PCI
//! function, the mass-storage driver against the drive's interface, and the
//! volume manager against the unit it publishes. The drive answers as a
//! multimedia unit, so the disc is served as an optical disc, recognised as
//! ISO 9660 and attached under `/Storage`. The scripted root shell runs
//! `optical`, which waits for that volume, copies it onto ARXFS with the real
//! `cp`, and checks every file's bytes and RISC OS filetype on the disc and in
//! the copy — the filetype travelling from the disc's `ARCHIMEDES` record,
//! through the ISO 9660 driver's `acorn.filetype` attribute and the copy
//! engine, into ARXFS's attribute store. The run then changes the disc in the
//! drive (`tairix_test_iso9660_image::second_disc`): the first disc's volume
//! must go and the second's come, read back through the unit's fresh
//! publication.
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
    use core::sync::atomic::{AtomicBool, Ordering};

    use tairix_arch_aarch64::{handle_panic_via_serial, qemu_exit, SerialSink, SERIAL_SINK};
    use tairix_itest_witness::field_str;
    use tairix_kalloc::{FreeListAllocator, Heap, HEAP_BYTES};
    use tairix_kernel::aarch64::boot as boot_aarch64;
    use tairix_log::{Event, EventId, Sink};
    use tairix_test_optical::COMMAND;

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

    /// `SyscallInvoked`, the audited record the sink watches for exits.
    const SYSCALL_INVOKED_EVENT_ID: EventId = EventId(5000);

    /// The login shell, whose `exit` the script types only once the fixture
    /// has reported its pass: no other process's exit, however it lands, can
    /// end the run.
    const SHELL: &str = "elsh";

    /// Whether the fixture has exited, which only a pass lets it do.
    static FIXTURE_EXITED: AtomicBool = AtomicBool::new(false);

    /// Replays every event to the serial console and ends the run on the
    /// shell's exit after the fixture's.
    struct OpticalSink;

    impl Sink for OpticalSink {
        fn write_event(&self, event: &Event<'_>) {
            SerialSink::new().write_event(event);
            if event.id != SYSCALL_INVOKED_EVENT_ID || field_str(event, "sc") != Some("exit") {
                return;
            }
            match field_str(event, "comm") {
                Some(COMMAND) => FIXTURE_EXITED.store(true, Ordering::Release),
                Some(SHELL) if FIXTURE_EXITED.load(Ordering::Acquire) => qemu_exit::exit_success(),
                _ => {}
            }
        }
    }

    static AUDIT_SINK: OpticalSink = OpticalSink;

    /// Forward to the shared aarch64 panic bridge. A panic parks the CPU, so
    /// the run times out and the harness reports it.
    #[panic_handler]
    fn tairix_optical_qemu_aarch64_panic(info: &PanicInfo<'_>) -> ! {
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
