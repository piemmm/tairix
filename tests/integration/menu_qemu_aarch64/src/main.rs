//! QEMU integration vertical: **open an application's window menu, see the
//! desktop's plate on screen, and choose a row** (`plans/NEW-MENUS.md` D17).
//!
//! # What this proves that no host test can
//!
//! Every piece of menu *logic* is already covered by host unit tests: the
//! bounded wire model and its codec, the chain's plates, placement, grab,
//! traversal, dismissal and lifetime, the terminal's row model and its
//! id↔command inverse. What only a real machine can show is that those pieces
//! are **wired to each other and to the running application** — that a
//! secondary press on a client reaches the application as a pointer event,
//! that the `OpenMenu` it sends is served and accepted, that the session
//! actually *draws* a plate where the chain says it is, that a click on a row
//! is routed into the chain rather than to whatever is behind it, and that the
//! one `MenuClosed` answer reaches the application and runs the row's command.
//!
//! Before this vertical nothing on the system had ever put a plate on a screen.
//!
//! So the guest boots the **production** aarch64 pipeline
//! (`boot_aarch64::boot`) against a planted encrypted root carrying the signed
//! input and display driver bundles, and the host drives the desktop blind
//! through the QEMU monitor: unlock, log in, start the desktop, launch the
//! terminal from the program library, right-click its client, photograph the
//! plate, and click the *Settings…* row. Only the sinks are swapped, for one
//! gate latching the PASS witnesses below.
//!
//! # The PASS gate
//!
//! Three latches, in order, each attributable to exactly one act:
//!
//! 1. **The application launched.** An `APP_LOADED` record naming the bundle
//!    [`MENU_APP_NAME`] spells — the library row's own launch.
//! 2. **A menu came up.** The desktop's own record that a chain's plates
//!    reached the display, after that launch.
//! 3. **A row's command reached the application.** The desktop's record of a
//!    window first shown *after* that one — the settings sheet the chosen
//!    *Settings…* row opens. The application is the only client in this world
//!    that opens a window (the desktop's own surfaces, the chain's plates
//!    included, are session-painted and never call the window channel), and it
//!    opens that sheet on nothing but the `MenuOutcome::Chosen` naming that
//!    row. So the third latch attests the whole round trip: the press reached
//!    the client, the open was accepted, a chain came up, the click hit the row
//!    the chain had drawn, and the one answer the desktop owes was delivered
//!    back.
//!
//! Ordering is what makes each latch attributable: the terminal's own window
//! was shown before the menu, so only a window first shown after it counts.
//! The desktop's records are log records, which the kernel delivers to the
//! diagnostic sink, so the gate is both sinks.
//!
//! # Why the guest cannot exit early
//!
//! The sheet is opened by the script's final click, and the runner sends no
//! pointer step until every screendump it has already asked for has been taken
//! and parsed. So the sheet that completes the PASS cannot open until the
//! plate's dump is safely on disk, and the guest can never exit out from under
//! the evidence.
//!
//! A panic before all three latches parks the CPU, the guest falls silent, and
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
    use tairix_itest_desktop_witness::{menu_shown, window_shown, ShownWindows};
    use tairix_itest_witness::{field_str, names_bundle};
    use tairix_kalloc::{Arena, FreeListAllocator, HEAP_BYTES};
    use tairix_kernel::aarch64::boot as boot_aarch64;
    use tairix_log::{Event, Sink};
    use tairix_test_menu_qemu_aarch64::MENU_APP_NAME;

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

    /// The gate both sinks are handed: it latches the three PASS witnesses
    /// described in the module docs and replays the whole trail to serial.
    struct MenuGate {
        /// The application's bundle was loaded.
        app_launched: AtomicBool,
        /// The desktop drew a menu's plates after that.
        menu_shown: AtomicBool,
        /// The windows first shown once the menu was: the sheet its row opens.
        sheets: ShownWindows<1>,
    }

    impl MenuGate {
        /// A gate with no witness latched.
        const fn new() -> Self {
            Self {
                app_launched: AtomicBool::new(false),
                menu_shown: AtomicBool::new(false),
                sheets: ShownWindows::new(),
            }
        }

        /// Latch what `event` attests.
        fn note(&self, event: &Event<'_>) {
            if event.id.0 == tairix_appload::events::APP_LOADED.0 {
                let launched = field_str(event, "bundle").is_some_and(|bundle| {
                    names_bundle(bundle, tairix_abi::SYSTEM_APPLICATION_STORE, MENU_APP_NAME)
                });
                if launched {
                    self.app_launched.store(true, Ordering::Release);
                }
            } else if let Some(window) = window_shown(event) {
                self.sheets.note(window);
            } else if menu_shown(event) && self.app_launched.load(Ordering::Acquire) {
                self.menu_shown.store(true, Ordering::Release);
                self.sheets.arm();
            }
        }

        /// Whether every witness is in.
        fn passed(&self) -> bool {
            self.app_launched.load(Ordering::Acquire)
                && self.menu_shown.load(Ordering::Acquire)
                && self.sheets.count() >= 1
        }
    }

    impl Sink for MenuGate {
        fn write_event(&self, event: &Event<'_>) {
            // Latched before the record reaches serial, so the line the host
            // acts on cannot precede what it attests; replayed so the QEMU
            // transcript records the full boot → unlock → desktop → menu
            // timeline the host gates its injection on.
            self.note(event);
            SerialSink::new().write_event(event);
            if self.passed() {
                qemu_exit::exit_success();
            }
        }
    }

    /// The gate the boot pipeline is handed as both its sinks.
    static GATE: MenuGate = MenuGate::new();

    /// Forward to the shared aarch64 panic bridge. A panic before the PASS
    /// finisher parks the CPU, the run times out, and the harness reports
    /// `Outcome::Timeout` — the documented fail-loud behaviour.
    #[panic_handler]
    fn tairix_menu_qemu_aarch64_panic(info: &PanicInfo<'_>) -> ! {
        handle_panic_via_serial(info)
    }

    /// Boot entry point — the symbol the arch crate's `boot.s` trampoline
    /// calls (via `tairix_arch_aarch64_main`).
    ///
    /// QEMU hands no DTB pointer (`_dtb == 0`), so the embedded `virt` blob's
    /// address is forwarded to the production boot pipeline with the gate in
    /// place of both sinks.
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
