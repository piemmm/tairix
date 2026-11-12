//! QEMU integration vertical: **the desktop's screen capture**
//! (`plans/CAPTURE.md` CP11).
//!
//! # What this proves that no host test can
//!
//! The chord match, the controller, the composition, the keep rule and the
//! sweep are each host-tested against a fake file seam. What only a running
//! machine shows is that they meet the real ones: that the chord pressed at
//! the seat keyboard reaches the session as a physical key and is taken before
//! any window sees it, that the compositor's tap hands over a still, and that
//! the session's file worker writes it through the kernel VFS and names it in
//! the account's Desktop — and that a scratch file a session ended mid-capture
//! left in the account's pictures is named at the next start, through the same
//! runtime seam.
//!
//! So the guest boots the **production** aarch64 pipeline
//! (`boot_aarch64::boot`) against the autoload disk with that leftover planted,
//! and the host unlocks, logs in, dumps the revealed desktop and then presses
//! `Meta+Shift+3`. Once the still is kept it opens the capture bar, moves to
//! *Record Entire Screen* and starts it, and once it has started presses the
//! still's chord again, which stops it. One gate stands in for both sinks.
//! After the PASS the host reads the still back off the disk and holds it to
//! the dump, every pixel off the bar the same, and decodes the recording's
//! last frame and holds it to the dump as closely as the codec keeps it.
//!
//! # The PASS gate
//!
//! 1. **The leftover kept.** The desktop's kept record naming a leftover,
//!    from the sweep its file worker runs first.
//! 2. **The still taken.** The desktop's record of a still of the screen
//!    composed.
//! 3. **The still kept.** Its kept record naming a still.
//! 4. **The recording ended**, with nothing lost.
//! 5. **The recording kept.** Its kept record naming a recording, which
//!    PASSes.
//!
//! Any refused capture fails the run at once, its reason on serial in the
//! desktop's own record, as does a recording that ends having lost anything.
//! The sweep is the first job of the one queue the still is kept through, so
//! a still kept before any leftover means the leftover was never kept, and
//! fails the run too.

#![cfg_attr(itest_aarch64, no_std)]
#![cfg_attr(itest_aarch64, no_main)]
#![deny(missing_docs)]

// --- Freestanding test bin (`aarch64-unknown-none`) ----------------

#[cfg(itest_aarch64)]
mod kernel {
    use core::num::NonZeroU16;
    use core::panic::PanicInfo;
    use core::sync::atomic::{AtomicBool, Ordering};

    use tairix_arch_aarch64::{handle_panic_via_serial, qemu_exit, SerialSink};
    use tairix_itest_finisher::fail_point;
    use tairix_itest_witness::field_str;
    use tairix_kalloc::{Arena, FreeListAllocator, HEAP_BYTES};
    use tairix_kernel::aarch64::boot as boot_aarch64;
    use tairix_log::{Event, Sink};
    use tairix_test_capture_qemu_aarch64::{
        CAPTURE_KEPT, CAPTURE_REFUSED, CAPTURE_TAKEN, KEPT_LEFTOVER, KEPT_RECORDING, KEPT_STILL,
        NOTHING_LOST, RECORDING_ENDED, TARGET_SCREEN,
    };

    // The canonical QEMU `virt` device tree, dumped and embedded at build
    // time (`build.rs`). The boot pipeline discovers the board from it
    // because QEMU passes no `x0` DTB pointer at an ELF `-kernel` entry.
    include!(concat!(env!("OUT_DIR"), "/dtb_fixture.rs"));

    /// Static boot heap, mirroring the production aarch64 kernel binary's
    /// `.bss`-resident heap (zeroed by the boot trampoline).
    static HEAP: Arena<HEAP_BYTES> = Arena::ZERO;

    /// Global allocator backed by [`HEAP`].
    ///
    /// SAFETY: the page-aligned `HEAP` static outlives the binary and the
    /// allocator is its only consumer.
    #[global_allocator]
    static ALLOCATOR: FreeListAllocator =
        unsafe { FreeListAllocator::new(HEAP.as_mut_ptr(), HEAP_BYTES) };

    /// Finisher code: a capture was refused, its reason on serial in the
    /// desktop's record.
    const FAIL_REFUSED: NonZeroU16 = fail_point!(1);

    /// Finisher code: the still was kept with no leftover kept before it.
    const FAIL_LEFTOVER_UNKEPT: NonZeroU16 = fail_point!(2);

    /// Finisher code: the recording ended having lost something, what on
    /// serial in the desktop's record.
    const FAIL_RECORDING_LOST: NonZeroU16 = fail_point!(3);

    /// Observer that replays the whole trail to serial and latches the PASS
    /// witnesses described in the module docs.
    struct CaptureGate {
        /// The planted leftover was kept under its name.
        leftover: AtomicBool,
        /// A still of the screen was composed.
        taken: AtomicBool,
        /// The still was kept.
        still: AtomicBool,
        /// A recording ended with nothing lost.
        recorded: AtomicBool,
    }

    impl CaptureGate {
        /// A gate with no witness latched.
        const fn new() -> Self {
            Self {
                leftover: AtomicBool::new(false),
                taken: AtomicBool::new(false),
                still: AtomicBool::new(false),
                recorded: AtomicBool::new(false),
            }
        }

        /// Act on one of the desktop's kept records.
        fn note_kept(&self, event: &Event<'_>) {
            match field_str(event, "how") {
                Some(KEPT_LEFTOVER) => self.leftover.store(true, Ordering::Release),
                Some(KEPT_STILL) if self.taken.load(Ordering::Acquire) => {
                    if !self.leftover.load(Ordering::Acquire) {
                        qemu_exit::exit_failure(FAIL_LEFTOVER_UNKEPT);
                    }
                    self.still.store(true, Ordering::Release);
                }
                Some(KEPT_RECORDING) if self.recorded.load(Ordering::Acquire) => {
                    qemu_exit::exit_success();
                }
                _ => {}
            }
        }

        /// Act on the desktop's record of a recording ended.
        fn note_ended(&self, event: &Event<'_>) {
            if field_str(event, "lost") != Some(NOTHING_LOST) {
                qemu_exit::exit_failure(FAIL_RECORDING_LOST);
            }
            if self.still.load(Ordering::Acquire) {
                self.recorded.store(true, Ordering::Release);
            }
        }
    }

    impl Sink for CaptureGate {
        fn write_event(&self, event: &Event<'_>) {
            // Replay through the serial sink first, so the QEMU transcript
            // records the whole timeline and the host can gate its injection
            // on it.
            SerialSink::new().write_event(event);
            match event.id.0 {
                CAPTURE_REFUSED => qemu_exit::exit_failure(FAIL_REFUSED),
                CAPTURE_TAKEN if field_str(event, "target") == Some(TARGET_SCREEN) => {
                    self.taken.store(true, Ordering::Release);
                }
                CAPTURE_KEPT => self.note_kept(event),
                RECORDING_ENDED => self.note_ended(event),
                _ => {}
            }
        }
    }

    /// The gate the boot pipeline is handed as both its sinks.
    static GATE: CaptureGate = CaptureGate::new();

    /// Forward to the shared aarch64 panic bridge. A panic before the PASS
    /// parks the CPU, the run times out, and the harness reports
    /// `Outcome::Timeout` — the documented fail-loud behaviour.
    #[panic_handler]
    fn tairix_capture_qemu_aarch64_panic(info: &PanicInfo<'_>) -> ! {
        handle_panic_via_serial(info)
    }

    /// Boot entry point — the symbol the arch crate's `boot.s` trampoline
    /// calls (via `tairix_arch_aarch64_main`).
    ///
    /// QEMU hands no DTB pointer (`_dtb == 0`), so the embedded `virt` blob's
    /// address is forwarded to the production boot pipeline with the gate in
    /// place of both sinks: the desktop's records are log records.
    #[no_mangle]
    pub extern "C" fn kernel_main(_dtb: u64) -> ! {
        let dtb = DTB_BLOB.as_ptr() as u64;
        boot_aarch64::boot(
            dtb,
            &ALLOCATOR,
            &GATE,
            &GATE,
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
