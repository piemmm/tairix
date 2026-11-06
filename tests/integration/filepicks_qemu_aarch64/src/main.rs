//! QEMU integration vertical: **two trusted pickers of one application up at
//! once, each concluding on its own** (`plans/APPWIN.md` AW5).
//!
//! # What this proves that no host test can
//!
//! The session's table of picks is host-tested. What only a running machine
//! can show is that two pickers are up together on the real desktop, each
//! holding only the window that asked, and that each concludes into that
//! window as a delegation the session mints and the viewer redeems.
//!
//! So the guest boots the **production** aarch64 pipeline
//! (`boot_aarch64::boot`) against a planted encrypted root, and the host drives
//! the desktop blind through the QEMU monitor: unlock, log in, start the
//! desktop, launch `view` from the program library, where it is resident and
//! opens nothing, and launch it from there twice more. Each later launch
//! reaches the running viewer, which opens a window and asks for a document,
//! so two pickers stand cascaded. Only the audit sink is swapped, for the PASS
//! witnesses below.
//!
//! # The PASS gate
//!
//! [`PICKS`](tairix_test_filepicks_qemu_aarch64::PICKS) chains of
//! [`PICK_CHAIN`](tairix_test_filepicks_qemu_aarch64::PICK_CHAIN): the session
//! mints, the viewer redeems, each in order and attributed by the kernel to the
//! principal that made it, every redeem made by one attested task, the
//! instance both pickers were asked from. A redeem from any other task restarts
//! the chain instead of completing it (fail closed). Each latched step prints
//! its own marker, so a run that does not pass names the missing hop.
//!
//! # Why the guest cannot exit early
//!
//! The host chooses in a picker only once the session has announced the last
//! one on screen, so both were up before either concluded, and makes the second
//! choice only once the window the first opened its document in is on screen.
//! A panic before the gate parks the CPU, the guest falls silent, and the
//! runner reports a timeout: loud failure, never a false pass.

#![cfg_attr(itest_aarch64, no_std)]
#![cfg_attr(itest_aarch64, no_main)]
#![deny(missing_docs)]

#[cfg(itest_aarch64)]
mod kernel {
    use core::panic::PanicInfo;

    use tairix_arch_aarch64::{handle_panic_via_serial, qemu_exit, SerialSink, SERIAL_SINK};
    use tairix_itest_witness::{marker, ChainNote, SyscallChain};
    use tairix_kalloc::{FreeListAllocator, Heap, HEAP_BYTES};
    use tairix_kernel::aarch64::boot as boot_aarch64;
    use tairix_log::{Event, Sink};
    use tairix_test_filepicks_qemu_aarch64::{
        FOREIGN_VIEWER_MARKER, PICKS, PICK_CHAIN, PICK_STEP_MARKERS, VIEWER_REDEEM_STEP,
    };

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

    /// The picks the gate watches: [`PICK_CHAIN`] in order, the viewer's
    /// redeem made by one kernel-attested task every time.
    static CONCLUDED: SyscallChain = SyscallChain::new(&PICK_CHAIN, VIEWER_REDEEM_STEP);

    /// Audit observer that replays the whole trail to serial and latches the
    /// picks described in the module docs.
    struct PicksSink;

    impl Sink for PicksSink {
        fn write_event(&self, event: &Event<'_>) {
            // Replay through the serial sink first, so the transcript records
            // the whole timeline the host gates its gestures on.
            SerialSink::new().write_event(event);
            if event.id.0 != tairix_kernel_syscall::AuditEvent::SyscallInvoked.id().0 {
                return;
            }
            let said = match CONCLUDED.note(event) {
                ChainNote::Latched { step, .. } => PICK_STEP_MARKERS.get(step).copied(),
                ChainNote::Foreign => Some(FOREIGN_VIEWER_MARKER),
                ChainNote::Unrelated => None,
            };
            if let Some(said) = said {
                SerialSink::new().write_event(&marker(said));
            }
            if CONCLUDED.rounds() >= PICKS {
                qemu_exit::exit_success();
            }
        }
    }

    /// The audit observer the boot pipeline is handed.
    static AUDIT_SINK: PicksSink = PicksSink;

    /// Forward to the shared aarch64 panic bridge. A panic before the PASS
    /// finisher parks the CPU, the run times out, and the harness reports it.
    #[panic_handler]
    fn tairix_filepicks_qemu_aarch64_panic(info: &PanicInfo<'_>) -> ! {
        handle_panic_via_serial(info)
    }

    /// Boot entry point, called by the arch crate's `boot.s` trampoline.
    ///
    /// QEMU hands no DTB pointer, so the embedded `virt` blob's address is
    /// forwarded to the production boot pipeline with the audit observer in
    /// place.
    #[no_mangle]
    pub extern "C" fn kernel_main(_dtb: u64) -> ! {
        let dtb = DTB_BLOB.as_ptr() as u64;
        boot_aarch64::boot(
            dtb,
            &ALLOCATOR,
            &SERIAL_SINK,
            &AUDIT_SINK,
            // `SyscallInvoked` is `Debug`, below the default `Info` filter,
            // and it carries every PASS witness; the host also waits for its
            // `sc=irq_bind` marker before typing the unlock passphrase.
            tairix_log::Level::Debug,
            &tairix_kernel::hwtree_store::HW_TREE_SOURCE,
        )
    }
}

#[cfg(not(itest_aarch64))]
fn main() {}
