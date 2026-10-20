//! QEMU integration vertical: **an unconfigured machine that can draw boots
//! to the graphical login screen** (`plans/NEW-DESKTOP-LOGIN.md` G7.1).
//!
//! # What this proves that no host test can
//!
//! Which login a round runs is decided by pure, host-tested policy
//! (`tairix_login::session`). What only a real machine can show is that the
//! policy is fed the truth: that the display driver has autoloaded and bound
//! its service, that the encrypted root is mounted by the time the round
//! begins, and that the settings store on that root answers "I hold no
//! configuration" rather than "I am not here". Get any of those wrong and a
//! machine that could show a login screen sits at a text prompt instead —
//! which is exactly the defect this vertical was written for: login probed
//! for the store's *directory*, a directory `configure` only creates on its
//! first write, so every never-configured installation read as an
//! unmountable volume and pinned itself to the text prompt.
//!
//! # The disk is the experiment
//!
//! The backing volume is `FsDisk::GreeterRootDisk`, which carries the same
//! signed input and display driver bundles as the autoload vertical but the
//! **standard** application store — no planted `os.loginType`. That is the
//! state a fresh installation boots in, and the host script types nothing
//! but the unlock passphrase: no account, no `desktop` command, nothing that
//! could ask for a graphical session. So the login screen can only be here
//! because login chose it.
//!
//! # The PASS witnesses
//!
//! Both rest on what the kernel attests. A userland record reaches the
//! diagnostic sink only — the audit sink stays kernel-only, so user space can
//! neither forge nor truncate an entry — and the kernel stamps it with its
//! emitter, so a userland record counts only from the process the audit trail
//! says it must come from. The greeter's own "the login screen is up" cannot
//! be the gate, however plainly it reads on the serial transcript.
//!
//! 1. **Login chose the graphical round.** An `APP_LOADED` record naming the
//!    greeter's bundle in the system service store: the kernel verified and
//!    loaded it. Nothing else on this disk launches that bundle, and a text
//!    round never asks for it, so the record is login's decision itself.
//!    `PROCESS_SPAWNED` would not do — it carries an entry address and can
//!    attribute no bundle.
//! 2. **A frame reached the display.** Once (1) has latched, the display
//!    service's `FIRST_PRESENT`, which it records only once its display took
//!    a client's frame, from the process the kernel's `CallEndpointCreated`
//!    names as binding the reserved display endpoint. A present the display
//!    refused never produces it. The greeter is the only graphical client
//!    alive at that point — no desktop, no app, and the script types nothing
//!    after the passphrase — so the frame is the login screen's own.
//!
//! Together they say the machine decided on the graphical login *and*
//! reached the point of drawing it. A text round, a greeter that cannot
//! paint, or a panic all leave a latch open: the guest never exits, and the
//! runner reports a timeout — loud failure, never a false pass.

#![cfg_attr(itest_aarch64, no_std)]
#![cfg_attr(itest_aarch64, no_main)]
#![deny(missing_docs)]

// --- Freestanding test bin (`aarch64-unknown-none`) ----------------

#[cfg(itest_aarch64)]
mod kernel {
    use core::panic::PanicInfo;

    use tairix_arch_aarch64::{handle_panic_via_serial, qemu_exit, SerialSink};
    use tairix_itest_witness::GreeterFrame;
    use tairix_kalloc::{FreeListAllocator, Heap, HEAP_BYTES};
    use tairix_kernel::aarch64::boot as boot_aarch64;
    use tairix_log::{Event, Sink};

    // The canonical QEMU `virt` device tree, dumped and embedded at build
    // time (`build.rs`). The boot pipeline discovers the board from it
    // because QEMU passes no `x0` DTB pointer at an ELF `-kernel` entry.
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

    /// Observer of both the audit and the diagnostic stream, which replays
    /// each to serial and latches the two PASS witnesses described in the
    /// module docs.
    struct GreeterDefaultSink(GreeterFrame);

    impl Sink for GreeterDefaultSink {
        fn write_event(&self, event: &Event<'_>) {
            // Replay through the serial sink first, so the QEMU transcript
            // records the full boot → autoload → unlock → login → greeter
            // timeline and the host can gate its injection on it.
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

    /// The observer the boot pipeline is handed for both streams.
    static AUDIT_SINK: GreeterDefaultSink = GreeterDefaultSink(GreeterFrame::new());

    /// Forward to the shared aarch64 panic bridge. A panic before the PASS
    /// finisher parks the CPU, the run times out, and the harness reports
    /// `Outcome::Timeout` — the documented fail-loud behaviour.
    #[panic_handler]
    fn tairix_greeter_default_qemu_aarch64_panic(info: &PanicInfo<'_>) -> ! {
        handle_panic_via_serial(info)
    }

    /// Boot entry point — the symbol the arch crate's `boot.s` trampoline
    /// calls (via `tairix_arch_aarch64_main`).
    ///
    /// QEMU hands no DTB pointer (`_dtb == 0`), so the embedded `virt`
    /// blob's address is forwarded to the production boot pipeline with the
    /// audit observer in place.
    #[no_mangle]
    pub extern "C" fn kernel_main(_dtb: u64) -> ! {
        let dtb = DTB_BLOB.as_ptr() as u64;
        boot_aarch64::boot(
            dtb,
            &ALLOCATOR,
            // The display service's record reaches the diagnostic stream.
            &AUDIT_SINK,
            &AUDIT_SINK,
            // `SyscallInvoked` (`EventId(5000)`) is `Debug`, below the
            // default `Info` filter; the host waits for that record's
            // `sc=irq_bind` marker before typing the unlock passphrase, so
            // boot with the filter lowered.
            tairix_log::Level::Debug,
            &tairix_kernel::hwtree_store::HW_TREE_SOURCE,
        )
    }
}

// --- Host stub -----------------------------------------------------
#[cfg(not(itest_aarch64))]
fn main() {}
