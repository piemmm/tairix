//! `plans/TIMESYNC.md` TS-6 QEMU integration test: network time switched off
//! and back on while `timed` runs.
//!
//! The production aarch64 pipeline boots the `time-net-root` disk — the
//! net-only signed driver set, a planted static-addressing `network.conf`, and
//! on the encrypted root a planted `system.conf` naming the harness peer as the
//! one time server — with the NTP-server peer on the `dgram` netdev, exactly as
//! the TS-2 vertical does. The serial script then logs in and, at the shell,
//! asks the settings service for each change through `elevate`, the one way
//! the session's administrator holds `CAP_SETTINGS_WRITE`:
//!
//! 1. `configure time.network off`. Whether `timed` already follows the
//!    configuration or meets the root on its next ladder rung, it records
//!    network time switched off only after this write, and it arms the watch
//!    before it reads, so the watch is armed by the time that record lands;
//! 2. on that record, `configure time.network on`. Network time off ended the
//!    ladder, so the watch is the one path left that can carry this change:
//!    the service switches on, queries the peer as promptly as an unset clock
//!    would, and sets the clock.
//!
//! The guest exits on `timed`'s `CLOCK_SET` carrying the peer's instant
//! (`tairix_test_netstack_wire::applied_is_fixture_sample`), counted only once
//! the service has recorded network time off. A clock the service set earlier
//! from the configured server — before the first command reached it — does not
//! count, so a service that ignored the switch, or never saw the second change,
//! never earns the witness and fails on the deadline. The peer answers spoof
//! first, as in TS-2, so the value gate still rejects a believed spoof.

#![cfg_attr(itest_aarch64, no_std)]
#![cfg_attr(itest_aarch64, no_main)]
#![deny(missing_docs)]

// --- Freestanding test bin (`aarch64-unknown-none`) ----------------

#[cfg(itest_aarch64)]
mod kernel {
    use core::panic::PanicInfo;
    use core::sync::atomic::{AtomicBool, Ordering};

    use tairix_arch_aarch64::{handle_panic_via_serial, qemu_exit, SerialSink, SERIAL_SINK};
    use tairix_kalloc::{FreeListAllocator, Heap, HEAP_BYTES};
    use tairix_kernel::aarch64::boot as boot_aarch64;
    use tairix_log::{Event, FieldValue, Sink};
    use tairix_timesync::events::{CLOCK_SET, NETWORK_TIME_CHANGED};

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

    /// Replays every event through [`SERIAL_SINK`] and reports PASS once the
    /// clock is set from the peer after network time was switched off.
    struct SwitchWitness {
        /// Whether `timed` has recorded network time switched off. It
        /// publishes nothing but itself.
        off: AtomicBool,
    }

    impl SwitchWitness {
        /// The string field `key` of `event`.
        fn field<'a>(event: &'a Event<'_>, key: &str) -> Option<&'a str> {
            event
                .fields
                .iter()
                .find(|field| field.key == key)
                .and_then(|field| match field.value {
                    FieldValue::Str(text) => Some(text),
                    _ => None,
                })
        }
    }

    impl Sink for SwitchWitness {
        fn write_event(&self, event: &Event<'_>) {
            SerialSink::new().write_event(event);
            if event.id == NETWORK_TIME_CHANGED && Self::field(event, "network") == Some("off") {
                self.off.store(true, Ordering::Relaxed);
                return;
            }
            if event.id != CLOCK_SET || !self.off.load(Ordering::Relaxed) {
                return;
            }
            let applied = event.fields.iter().find(|f| f.key == "wall_secs");
            if let Some(FieldValue::SignedInt(secs)) = applied.map(|f| f.value) {
                if tairix_test_netstack_wire::applied_is_fixture_sample(secs) {
                    qemu_exit::exit_success();
                }
            }
        }
    }

    static WITNESS_SINK: SwitchWitness = SwitchWitness {
        off: AtomicBool::new(false),
    };

    /// Forward to the shared aarch64 panic bridge. A panic parks the CPU
    /// before the witness can fire, so the run times out and the harness
    /// reports `Outcome::Timeout` — the documented fail-loud behaviour.
    #[panic_handler]
    fn tairix_timed_switch_qemu_aarch64_panic(info: &PanicInfo<'_>) -> ! {
        handle_panic_via_serial(info)
    }

    /// Boot entry point — the symbol the arch crate's `boot.s` trampoline
    /// calls (via `tairix_arch_aarch64_main`).
    ///
    /// QEMU hands no DTB pointer (`_dtb == 0`), so the embedded `virt` blob's
    /// address is forwarded to the production boot pipeline. A service's own
    /// records reach the diagnostic sink, so the witness sits there and
    /// replays everything to the transcript; boot at the default `Info`
    /// filter, since both witness records are `Info` records.
    #[no_mangle]
    pub extern "C" fn kernel_main(_dtb: u64) -> ! {
        let dtb = DTB_BLOB.as_ptr() as u64;
        boot_aarch64::boot(
            dtb,
            &ALLOCATOR,
            &WITNESS_SINK,
            &SERIAL_SINK,
            tairix_log::Level::Info,
            &tairix_kernel::hwtree_store::HW_TREE_SOURCE,
        )
    }
}

// --- Host stub -----------------------------------------------------
#[cfg(not(itest_aarch64))]
fn main() {}
