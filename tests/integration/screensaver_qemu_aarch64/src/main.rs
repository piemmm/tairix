//! QEMU integration vertical: **a user's own screensaver chosen and shown,
//! and a user's saver asking for a privileged feed refused**
//! (`plans/SCREENSAVERS.md` SV10).
//!
//! # What this proves that no host test can
//!
//! The protocol, the host, the catalogue, the pane and the load gate are each
//! host-tested. What only a running machine shows is that they meet: that a
//! bundle put in the account's own store reaches the desktop's catalogue, the
//! Settings chooser and, previewed, the kernel's load gate as a self-signed
//! saver admitted into the drawing sandbox; that it draws through its region
//! and the desktop lays the frame on the screen; that a key takes it down; and
//! that a saver of the user's own asking for the machine feed is refused and
//! never run.
//!
//! So the guest boots the **production** aarch64 pipeline
//! (`boot_aarch64::boot`) against a planted encrypted root carrying the
//! complete app store and two savers in the account's own store, and the host
//! drives the desktop blind: unlock, log in, open Settings from the capsule's
//! menu, walk to Screensaver, choose `GOOD_SAVER`'s picture once the
//! catalogue has reached the chooser, press Preview, and type a key once the
//! saver's first frame is on the screen. One gate stands in for both sinks.
//!
//! # The PASS gate
//!
//! Four latches, each attributable to exactly one act:
//!
//! 1. **The privileged saver refused.** The desktop's refusal record naming
//!    `ROGUE_SAVER` for asking for a feed, from the scan at its bring-up.
//! 2. **The user's saver loaded.** An `APP_LOADED` naming `GOOD_SAVER_NAME`
//!    in the account's own store: the kernel verified its own signature.
//! 3. **Its frame shown.** The desktop's record of its first frame, a preview.
//! 4. **It was dismissed.** The desktop's record of it taken down, which only
//!    the typed key does here. That is the PASS.
//!
//! An `APP_LOADED` naming `ROGUE_SAVER_NAME`, or `GOOD_SAVER` ended for
//! misbehaving, fails the run at once. A panic parks the CPU, the guest falls
//! silent, and the runner reports a timeout: loud failure, never a false pass.

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
    use tairix_kalloc::{FreeListAllocator, Heap, HEAP_BYTES};
    use tairix_kernel::aarch64::boot as boot_aarch64;
    use tairix_log::{Event, FieldValue, Sink};
    use tairix_test_screensaver_qemu_aarch64::{
        is_users_saver, FEED_REFUSAL, GOOD_SAVER, GOOD_SAVER_NAME, ROGUE_SAVER, ROGUE_SAVER_NAME,
        SAVER_DISMISSED, SAVER_ENDED, SAVER_REFUSED, SAVER_SHOWN,
    };

    // The canonical QEMU `virt` device tree, dumped and embedded at build
    // time (`build.rs`). The boot pipeline discovers the board from it
    // because QEMU passes no `x0` DTB pointer at an ELF `-kernel` entry.
    include!(concat!(env!("OUT_DIR"), "/dtb_fixture.rs"));

    /// Static boot heap, mirroring the production aarch64 kernel binary's
    /// `.bss`-resident heap (zeroed by the boot trampoline).
    static HEAP: Heap = Heap::ZERO;

    /// Global allocator backed by [`HEAP`].
    ///
    /// SAFETY: the page-aligned `HEAP` static outlives the binary and the
    /// allocator is its only consumer.
    #[global_allocator]
    static ALLOCATOR: FreeListAllocator =
        unsafe { FreeListAllocator::new(HEAP.as_mut_ptr(), HEAP_BYTES) };

    /// Finisher code: the refused saver was loaded.
    const FAIL_ROGUE_LOADED: NonZeroU16 = fail_point!(1);

    /// Finisher code: the user's saver was ended for misbehaving, the reason
    /// on serial in the desktop's record.
    const FAIL_GOOD_ENDED: NonZeroU16 = fail_point!(2);

    /// Observer that replays the whole trail to serial and latches the PASS
    /// witnesses described in the module docs.
    struct SaverGate {
        /// The privileged saver was refused.
        refused: AtomicBool,
        /// The user's saver was loaded.
        loaded: AtomicBool,
        /// Its first frame reached the screen, after that.
        shown: AtomicBool,
    }

    impl SaverGate {
        /// A gate with no witness latched.
        const fn new() -> Self {
            Self {
                refused: AtomicBool::new(false),
                loaded: AtomicBool::new(false),
                shown: AtomicBool::new(false),
            }
        }

        /// Latch or fail on an `APP_LOADED` record, by the bundle it names.
        fn note_bundle_loaded(&self, event: &Event<'_>) {
            let Some(bundle) = field_str(event, "bundle") else {
                return;
            };
            if is_users_saver(bundle, ROGUE_SAVER_NAME) {
                qemu_exit::exit_failure(FAIL_ROGUE_LOADED);
            }
            if is_users_saver(bundle, GOOD_SAVER_NAME) {
                self.loaded.store(true, Ordering::Release);
            }
        }

        /// Act on one of the desktop's screensaver records about `saver`.
        fn note_saver(&self, id: u32, saver: &str, event: &Event<'_>) {
            match id {
                SAVER_REFUSED
                    if saver == ROGUE_SAVER && field_str(event, "reason") == Some(FEED_REFUSAL) =>
                {
                    self.refused.store(true, Ordering::Release);
                }
                SAVER_ENDED if saver == GOOD_SAVER => qemu_exit::exit_failure(FAIL_GOOD_ENDED),
                SAVER_SHOWN
                    if saver == GOOD_SAVER
                        && previewed(event)
                        && self.loaded.load(Ordering::Acquire) =>
                {
                    self.shown.store(true, Ordering::Release);
                }
                SAVER_DISMISSED
                    if saver == GOOD_SAVER
                        && self.shown.load(Ordering::Acquire)
                        && self.refused.load(Ordering::Acquire) =>
                {
                    qemu_exit::exit_success();
                }
                _ => {}
            }
        }
    }

    /// Whether the desktop recorded the saver as shown on request.
    fn previewed(event: &Event<'_>) -> bool {
        event
            .fields
            .iter()
            .any(|field| field.key == "preview" && field.value == FieldValue::Bool(true))
    }

    impl Sink for SaverGate {
        fn write_event(&self, event: &Event<'_>) {
            // Replay through the serial sink first, so the QEMU transcript
            // records the whole timeline and the host can gate its injection
            // on it.
            SerialSink::new().write_event(event);
            let id = event.id.0;
            if id == tairix_appload::events::APP_LOADED.0 {
                self.note_bundle_loaded(event);
            } else if let Some(saver) = field_str(event, "saver") {
                self.note_saver(id, saver, event);
            }
        }
    }

    /// The gate the boot pipeline is handed as both its sinks.
    static GATE: SaverGate = SaverGate::new();

    /// Forward to the shared aarch64 panic bridge. A panic before the PASS
    /// parks the CPU, the run times out, and the harness reports
    /// `Outcome::Timeout` — the documented fail-loud behaviour.
    #[panic_handler]
    fn tairix_screensaver_qemu_aarch64_panic(info: &PanicInfo<'_>) -> ! {
        handle_panic_via_serial(info)
    }

    /// Boot entry point — the symbol the arch crate's `boot.s` trampoline
    /// calls (via `tairix_arch_aarch64_main`).
    ///
    /// QEMU hands no DTB pointer (`_dtb == 0`), so the embedded `virt` blob's
    /// address is forwarded to the production boot pipeline with the gate in
    /// place of both sinks: the load gate's records are audit records and the
    /// desktop's are log records.
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
