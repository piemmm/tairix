//! QEMU integration vertical: **let Cinder out and see him on the desktop**
//! (`plans/CINDER.md` CD12, the regression test `OPEN-DEFECTS` D1807 owes).
//!
//! # What this proves that no host test can
//!
//! Every piece of the companion's logic is host-tested; what only a running
//! machine shows is that the pieces are wired to each other. D1807 was a loop
//! that never collected the answers its own presents earned, so its wait set
//! stayed ready, its frame deadline never fired, and a companion let out was
//! never drawn — invisible to every host test.
//!
//! So the guest boots the **production** aarch64 pipeline
//! (`boot_aarch64::boot`) against a planted encrypted root carrying the signed
//! input and display driver bundles and the complete app store, and the host
//! drives the desktop blind through the QEMU monitor: unlock, log in, start the
//! desktop, open Cinder from the program library, photograph his pen, press
//! *Let Cinder out*, photograph him on the desktop, and then open the terminal
//! from the library. Only the audit sink is swapped, for the PASS witnesses
//! below.
//!
//! # The PASS gate
//!
//! Two latches, in order, each attributable to exactly one act:
//!
//! 1. **Cinder launched.** An `APP_LOADED` record naming the companion's
//!    bundle in the system application store.
//! 2. **The host saw him.** An `APP_LOADED` record naming the terminal's
//!    bundle after that: the host opens the terminal only once the companion's
//!    surface is on screen and photographed, and nothing else in the run
//!    starts one.
//!
//! # Why the guest cannot pass early
//!
//! The runner sends no pointer step while a screendump it has asked for is
//! still being taken, and the terminal's launch is the script's last step, so
//! the second latch cannot land before the last dump is on disk. A companion
//! that never reaches the screen never earns the dump that step waits on.
//!
//! A panic before both latches parks the CPU, the guest falls silent, and the
//! runner reports a timeout — loud failure, never a false pass.

#![cfg_attr(itest_aarch64, no_std)]
#![cfg_attr(itest_aarch64, no_main)]
#![deny(missing_docs)]

// --- Freestanding test bin (`aarch64-unknown-none`) ----------------

#[cfg(itest_aarch64)]
mod kernel {
    use core::panic::PanicInfo;
    use core::sync::atomic::{AtomicBool, Ordering};

    use tairix_arch_aarch64::{handle_panic_via_serial, qemu_exit, SerialSink, SERIAL_SINK};
    use tairix_itest_witness::{field_str, names_bundle};
    use tairix_kalloc::{Arena, FreeListAllocator, HEAP_BYTES};
    use tairix_kernel::aarch64::boot as boot_aarch64;
    use tairix_log::{Event, Sink};
    use tairix_test_cinder_qemu_aarch64::{COMPANION_APP_NAME, THEN_APP_NAME};

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

    /// Audit observer that replays the whole trail to serial and latches the
    /// two PASS witnesses described in the module docs.
    struct CompanionSink {
        /// The companion's bundle was loaded.
        launched: AtomicBool,
    }

    impl CompanionSink {
        /// Latch the launch from an `APP_LOADED` record, and pass on the
        /// terminal's load once it has been latched.
        fn note_bundle_loaded(&self, event: &Event<'_>) {
            let Some(bundle) = field_str(event, "bundle") else {
                return;
            };
            let store = tairix_abi::SYSTEM_APPLICATION_STORE;
            if names_bundle(bundle, store, COMPANION_APP_NAME) {
                self.launched.store(true, Ordering::Release);
            } else if self.launched.load(Ordering::Acquire)
                && names_bundle(bundle, store, THEN_APP_NAME)
            {
                qemu_exit::exit_success();
            }
        }
    }

    impl Sink for CompanionSink {
        fn write_event(&self, event: &Event<'_>) {
            // Replay through the serial sink first, so the QEMU transcript
            // records the whole timeline and the host can gate on it.
            SerialSink::new().write_event(event);
            if event.id.0 == tairix_appload::events::APP_LOADED.0 {
                self.note_bundle_loaded(event);
            }
        }
    }

    /// The audit observer the boot pipeline is handed.
    static AUDIT_SINK: CompanionSink = CompanionSink {
        launched: AtomicBool::new(false),
    };

    /// Forward to the shared aarch64 panic bridge. A panic before the PASS
    /// finisher parks the CPU, the run times out, and the harness reports
    /// `Outcome::Timeout` — the documented fail-loud behaviour.
    #[panic_handler]
    fn tairix_cinder_qemu_aarch64_panic(info: &PanicInfo<'_>) -> ! {
        handle_panic_via_serial(info)
    }

    /// Boot entry point — the symbol the arch crate's `boot.s` trampoline
    /// calls (via `tairix_arch_aarch64_main`).
    ///
    /// QEMU hands no DTB pointer (`_dtb == 0`), so the embedded `virt` blob's
    /// address is forwarded to the production boot pipeline with the audit
    /// observer in place.
    #[no_mangle]
    pub extern "C" fn kernel_main(_dtb: u64) -> ! {
        let dtb = DTB_BLOB.as_ptr() as u64;
        boot_aarch64::boot(
            dtb,
            &ALLOCATOR,
            &SERIAL_SINK,
            &AUDIT_SINK,
            // The host waits for `SyscallInvoked`'s (`EventId(5000)`, at
            // `Debug`) `sc=irq_bind` marker before typing the unlock
            // passphrase, so boot with the filter lowered.
            tairix_log::Level::Debug,
            &tairix_kernel::hwtree_store::HW_TREE_SOURCE,
        )
    }
}

// --- Host stub -----------------------------------------------------
#[cfg(not(itest_aarch64))]
fn main() {}
