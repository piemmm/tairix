//! QEMU integration vertical: **touch, end to end** (`plans/POINTING.md`
//! PO9).
//!
//! The guest boots the production aarch64 pipeline (`boot_aarch64::boot`)
//! against the planted encrypted root, with a virtio multitouch touchscreen
//! beside the keyboard and no mouse at all. The host unlocks the root, logs in
//! and starts the desktop, then taps the program library's button and the
//! terminal's row in the popup it opens, over QEMU's control monitor. Only the
//! audit sink is swapped, for the PASS witnesses.
//!
//! # The PASS gate
//!
//! 1. **A touch frame reached the seat**: the kernel's one-shot
//!    `InputDelivered` witness naming a touch delivery. QEMU's contacts
//!    crossed the virtio device, the autoloaded driver decoded the slot
//!    protocol into a frame, and `touch_inject` admitted it.
//! 2. **The taps launched the terminal**: an `APP_LOADED` record naming its
//!    bundle. Only the popup's row launches it, only the button opens the
//!    popup, and the touchscreen is the one pointing device — so the launch
//!    says the session read each tap as a primary press where it landed, and
//!    the bar acted on both.
//!
//! A panic before both latches parks the CPU, the guest falls silent, and the
//! runner reports a timeout: a loud failure, never a false pass.

#![cfg_attr(itest_aarch64, no_std)]
#![cfg_attr(itest_aarch64, no_main)]
#![deny(missing_docs)]

#[cfg(itest_aarch64)]
mod kernel {
    use core::panic::PanicInfo;
    use core::sync::atomic::{AtomicBool, Ordering};

    use tairix_arch_aarch64::{handle_panic_via_serial, qemu_exit, SerialSink, SERIAL_SINK};
    use tairix_itest_witness::{field_str, names_bundle};
    use tairix_kalloc::{FreeListAllocator, Heap, HEAP_BYTES};
    use tairix_kernel::aarch64::boot as boot_aarch64;
    use tairix_kernel_core::seat::DeliveredInputKind;
    use tairix_kernel_core::AuditEvent;
    use tairix_log::{Event, Sink};
    use tairix_test_appbar_qemu_aarch64::BAR_APP_NAME;

    // QEMU passes no DTB pointer at an ELF `-kernel` entry, so the board is
    // discovered from the `virt` tree `build.rs` embeds.
    include!(concat!(env!("OUT_DIR"), "/dtb_fixture.rs"));

    /// Static boot heap, as the production aarch64 kernel binary's is.
    static HEAP: Heap = Heap::ZERO;

    /// Global allocator backed by [`HEAP`].
    ///
    /// SAFETY: the page-aligned `HEAP` static outlives the binary and the
    /// allocator is its only consumer.
    #[global_allocator]
    static ALLOCATOR: FreeListAllocator =
        unsafe { FreeListAllocator::new(HEAP.as_mut_ptr(), HEAP_BYTES) };

    /// Audit observer that replays the trail to serial and latches the two
    /// PASS witnesses described in the module docs.
    struct TouchSink {
        /// A touch frame was delivered to the seat.
        touched: AtomicBool,
        /// The terminal's bundle was loaded.
        launched: AtomicBool,
    }

    impl TouchSink {
        const fn new() -> Self {
            Self {
                touched: AtomicBool::new(false),
                launched: AtomicBool::new(false),
            }
        }
    }

    impl Sink for TouchSink {
        fn write_event(&self, event: &Event<'_>) {
            // Replayed first: the host gates each tap on the session's own
            // announcements in this transcript.
            SerialSink::new().write_event(event);
            if event.id.0 == AuditEvent::InputDelivered.id().0 {
                if field_str(event, "kind") == Some(DeliveredInputKind::Touch.as_str()) {
                    self.touched.store(true, Ordering::Release);
                }
            } else if event.id.0 == tairix_appload::events::APP_LOADED.0 {
                let terminal = field_str(event, "bundle").is_some_and(|bundle| {
                    names_bundle(bundle, tairix_abi::SYSTEM_APPLICATION_STORE, BAR_APP_NAME)
                });
                if terminal {
                    self.launched.store(true, Ordering::Release);
                }
            } else {
                return;
            }
            if self.touched.load(Ordering::Acquire) && self.launched.load(Ordering::Acquire) {
                qemu_exit::exit_success();
            }
        }
    }

    /// The audit observer the boot pipeline is handed.
    static AUDIT_SINK: TouchSink = TouchSink::new();

    /// Forward to the shared aarch64 panic bridge, which parks the CPU.
    #[panic_handler]
    fn tairix_touch_qemu_aarch64_panic(info: &PanicInfo<'_>) -> ! {
        handle_panic_via_serial(info)
    }

    /// Boot entry point — the symbol the arch crate's `boot.s` trampoline
    /// calls — running the production pipeline with the audit observer in
    /// place.
    #[no_mangle]
    pub extern "C" fn kernel_main(_dtb: u64) -> ! {
        let dtb = DTB_BLOB.as_ptr() as u64;
        boot_aarch64::boot(
            dtb,
            &ALLOCATOR,
            &SERIAL_SINK,
            &AUDIT_SINK,
            // The host types the passphrase once every input driver's
            // `irq_bind`, a `Debug` record, has appeared.
            tairix_log::Level::Debug,
            &tairix_kernel::hwtree_store::HW_TREE_SOURCE,
        )
    }
}

#[cfg(not(itest_aarch64))]
fn main() {}
