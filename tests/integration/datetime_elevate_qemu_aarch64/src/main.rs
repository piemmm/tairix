//! QEMU integration vertical: **open the taskbar clock's *Set Date & Time…*
//! row, authenticate through the elevation broker, and witness the Date &
//! Time window on screen**.
//!
//! # What this proves that no host test can
//!
//! Every piece of elevation *logic* — the authorisation prompt, the broker's
//! listing of who may allow it, its re-authentication, the Launch reply — is
//! already covered by host unit tests. What only a real machine can show is that those pieces are
//! **wired to each other and to the running application**: that the
//! session's `SetDateTime` outcome opens the prompt, that the broker starts
//! `/System/Components/datetime.app/Run` as the authenticated account,
//! and that the application creates a window the desktop session serves.
//!
//! So the guest boots the **production** aarch64 pipeline
//! (`boot_aarch64::boot`) against a planted encrypted root, and the host
//! drives the desktop blind through the QEMU monitor: unlock, log in, start
//! the desktop, right-click the clock, choose *Set Date & Time…*, and type
//! the signed-in fixture account's password into the prompt, which asks for
//! nothing else because that account may set the clock. Only the sinks are
//! swapped, for one gate latching the PASS witnesses below.
//!
//! # The PASS gate
//!
//! Two latches, each attributable to exactly one act:
//!
//! 1. **The application launched.** An `APP_LOADED` record naming the
//!    Date & Time bundle.
//! 2. **Its window was opened.** The desktop's record of [`WINDOWS_OPENED`]
//!    windows first shown after that launch; the autostarted file manager's
//!    were shown before it. The desktop's records are log records, which the
//!    kernel delivers to the diagnostic sink, so one gate is both sinks.
//!
//! # Why the guest cannot exit early
//!
//! The window is opened by the elevated program after the password is typed,
//! and the runner sends no keystroke until the prompt announces that its
//! question is drawn. So the window that completes the PASS cannot open
//! until the prompt is up and the password is offered.
//!
//! A panic before both latches parks the CPU, the guest falls silent, and
//! the runner reports a timeout — loud failure, never a false pass.

#![cfg_attr(itest_aarch64, no_std)]
#![cfg_attr(itest_aarch64, no_main)]
#![deny(missing_docs)]

// --- Freestanding test bin (`aarch64-unknown-none`) ----------------

#[cfg(itest_aarch64)]
mod kernel {
    use core::panic::PanicInfo;
    use core::sync::atomic::{AtomicBool, Ordering};

    use tairix_arch_aarch64::{handle_panic_via_serial, qemu_exit, SerialSink};
    use tairix_itest_desktop_witness::{window_shown, ShownWindows};
    use tairix_itest_witness::{field_str, names_bundle};
    use tairix_kalloc::{Arena, FreeListAllocator, HEAP_BYTES};
    use tairix_kernel::aarch64::boot as boot_aarch64;
    use tairix_log::{Event, Sink};
    use tairix_test_datetime_elevate_qemu_aarch64::{DATETIME_APP_NAME, WINDOWS_OPENED};

    // The canonical QEMU `virt` device tree, dumped and embedded at build
    // time (`build.rs`). The boot pipeline discovers the board from it
    // because QEMU passes no `x0` DTB pointer at an ELF `-kernel` entry.
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

    /// The gate both sinks are handed: it latches the two PASS witnesses
    /// described in the module docs and replays the whole trail to serial.
    struct DateTimeElevateGate {
        /// The elevated application's bundle was loaded.
        app_launched: AtomicBool,
        /// The windows first shown since.
        windows: ShownWindows<WINDOWS_OPENED>,
    }

    impl DateTimeElevateGate {
        /// A gate with no witness latched.
        const fn new() -> Self {
            Self {
                app_launched: AtomicBool::new(false),
                windows: ShownWindows::new(),
            }
        }

        /// Latch what `event` attests.
        fn note(&self, event: &Event<'_>) {
            if event.id.0 == tairix_appload::events::APP_LOADED.0 {
                let launched = field_str(event, "bundle").is_some_and(|bundle| {
                    names_bundle(
                        bundle,
                        tairix_abi::SYSTEM_COMPONENT_STORE,
                        DATETIME_APP_NAME,
                    )
                });
                if launched {
                    self.app_launched.store(true, Ordering::Release);
                    self.windows.arm();
                }
            } else if let Some(window) = window_shown(event) {
                self.windows.note(window);
            }
        }

        /// Whether every witness is in.
        fn passed(&self) -> bool {
            self.app_launched.load(Ordering::Acquire) && self.windows.count() >= WINDOWS_OPENED
        }
    }

    impl Sink for DateTimeElevateGate {
        fn write_event(&self, event: &Event<'_>) {
            // Latched before the record reaches serial, so the line the host
            // acts on cannot precede what it attests; replayed so the QEMU
            // transcript records the full boot → unlock → desktop → elevate
            // timeline the host gates its injection on.
            self.note(event);
            SerialSink::new().write_event(event);
            if self.passed() {
                qemu_exit::exit_success();
            }
        }
    }

    /// The gate the boot pipeline is handed as both its sinks.
    static GATE: DateTimeElevateGate = DateTimeElevateGate::new();

    /// Forward to the shared aarch64 panic bridge. A panic before the PASS
    /// finisher parks the CPU, the run times out, and the harness reports
    /// `Outcome::Timeout` — the documented fail-loud behaviour.
    #[panic_handler]
    fn tairix_datetime_elevate_qemu_aarch64_panic(info: &PanicInfo<'_>) -> ! {
        handle_panic_via_serial(info)
    }

    /// Boot entry point — the symbol the arch crate's `boot.s` trampoline
    /// calls (via `tairix_arch_aarch64_main`).
    ///
    /// QEMU hands no DTB pointer (`_dtb == 0`), so the embedded `virt`
    /// blob's address is forwarded to the production boot pipeline with the
    /// gate in place of both sinks.
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
