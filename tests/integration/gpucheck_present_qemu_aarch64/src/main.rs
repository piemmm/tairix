//! QEMU integration vertical: **`gpucheck --present` shown full screen on a
//! virtio GPU, promoted, every frame shown** (`plans/GPU.md` GP6.5).
//!
//! # What this proves that no host test can
//!
//! The scene, the swapchain, the frame's fate and the compositor's promotion
//! each have their own suites. What only a running machine can show is that a
//! window a device draws reaches the screen through the one display path: the
//! command launched by name from a shell opens a window, asks for full screen,
//! fills its frames through the swapchain, is promoted on a display that
//! draws its own frames — its presents laid straight into the display's frame
//! — holds exactly the scene the host draws, and is told every frame it
//! presented was shown.
//!
//! So the guest boots the **production** aarch64 pipeline
//! (`boot_aarch64::boot`) against a planted encrypted root carrying the signed
//! input and virtio-GPU display driver bundles and the complete app store,
//! and the host drives the desktop blind through the QEMU monitor: unlock, log
//! in, start the desktop, open a terminal from the program library, type
//! [`COMMAND_LINE`], photograph the screen once the session says the window is
//! on screen full screen and how, and press `Escape`. Only the sinks are
//! swapped, for one gate latching the PASS witnesses below.
//!
//! # The PASS gate
//!
//! Three latches, in order, each attributable to exactly one act:
//!
//! 1. **The check launched.** An `APP_LOADED` record naming the command's
//!    bundle in the system command store — the shell resolving the typed
//!    word.
//! 2. **Its window opened.** The desktop's record of a window first shown
//!    after that load. The check opens one window, and the terminal's was
//!    shown before the check was loaded.
//! 3. **Every frame was shown.** An `APP_LOADED` record naming
//!    [`THEN_COMMAND`]'s bundle after that window: the typed line joins the two
//!    with `&&`, so the shell loads it only once the check exited with status
//!    zero, which it does only when every frame it presented was reported
//!    shown.
//!
//! # Why the guest cannot exit early
//!
//! The check presents until `Escape`, and the runner holds that key until the
//! screendump it keyed on the same witness is on disk, so the last latch
//! cannot land before the dump.
//!
//! A panic before all three latches parks the CPU, the guest falls silent,
//! and the runner reports a timeout — loud failure, never a false pass.

#![cfg_attr(itest_aarch64, no_std)]
#![cfg_attr(itest_aarch64, no_main)]
#![deny(missing_docs)]

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
    use tairix_test_gpucheck_present_qemu_aarch64::{CHECK_COMMAND, THEN_COMMAND};

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

    /// The gate both sinks are handed: it latches the three PASS witnesses
    /// described in the module docs and replays the whole trail to serial.
    struct PresentGate {
        /// Armed when the check's bundle loads, then the windows first shown:
        /// the check's.
        windows: ShownWindows<1>,
        /// The witness command was loaded after that window.
        every_frame_shown: AtomicBool,
    }

    impl PresentGate {
        /// A gate with no witness latched.
        const fn new() -> Self {
            Self {
                windows: ShownWindows::new(),
                every_frame_shown: AtomicBool::new(false),
            }
        }

        /// Latch the launch witness, then the clean-exit one, from the bundle
        /// an `APP_LOADED` record names. The second is honoured only once the
        /// check's window exists, so an earlier `true` cannot pass for it.
        fn note_bundle_loaded(&self, event: &Event<'_>) {
            let Some(bundle) = field_str(event, "bundle") else {
                return;
            };
            let store = tairix_abi::SYSTEM_COMMAND_STORE;
            if names_bundle(bundle, store, CHECK_COMMAND) {
                self.windows.arm();
            } else if self.windows.count() >= 1 && names_bundle(bundle, store, THEN_COMMAND) {
                self.every_frame_shown.store(true, Ordering::Release);
            }
        }
    }

    impl Sink for PresentGate {
        fn write_event(&self, event: &Event<'_>) {
            // Latched before the record reaches serial, so the line the host
            // acts on cannot precede what it attests; replayed so the QEMU
            // transcript records the whole boot → unlock → desktop → check
            // timeline the host gates its injection on.
            if event.id.0 == tairix_appload::events::APP_LOADED.0 {
                self.note_bundle_loaded(event);
            } else if let Some(window) = window_shown(event) {
                self.windows.note(window);
            }
            SerialSink::new().write_event(event);
            if self.every_frame_shown.load(Ordering::Acquire) {
                qemu_exit::exit_success();
            }
        }
    }

    /// The gate the boot pipeline is handed as both its sinks.
    static GATE: PresentGate = PresentGate::new();

    /// Forward to the shared aarch64 panic bridge. A panic before the PASS
    /// finisher parks the CPU, the run times out, and the harness reports
    /// `Outcome::Timeout` — the documented fail-loud behaviour.
    #[panic_handler]
    fn tairix_gpucheck_present_qemu_aarch64_panic(info: &PanicInfo<'_>) -> ! {
        handle_panic_via_serial(info)
    }

    /// Boot entry point, the symbol the arch crate's `boot.s` trampoline calls:
    /// the production boot pipeline over the embedded `virt` tree, with the
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

#[cfg(not(itest_aarch64))]
fn main() {}
