//! The production aarch64 boot as a display world, with an audit sink that
//! reports PASS once every witness of the desktop run has appeared.

use core::panic::PanicInfo;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use tairix_arch_aarch64::{handle_panic_via_serial, qemu_exit, SerialSink, SERIAL_SINK};
use tairix_kalloc::{FreeListAllocator, Heap, HEAP_BYTES};
use tairix_kernel::aarch64::boot as boot_aarch64;
use tairix_kernel_core::AuditEvent;
use tairix_log::{Event, Sink};
use tairix_util::fmt::format_hex_u64;

// The canonical QEMU `virt` device tree, dumped and embedded at build
// time (`build.rs`). The boot pipeline discovers the board from it
// because QEMU passes no `x0` DTB pointer at an ELF `-kernel` entry.
include!(concat!(env!("OUT_DIR"), "/dtb_fixture.rs"));

/// Static boot heap, mirroring the production aarch64 kernel binary's
/// `.bss`-resident heap (zeroed by the boot trampoline).
///
/// `static mut` because the bump allocator hands out disjoint slices via
/// an atomic cursor; the storage is otherwise never aliased.
static mut HEAP: Heap = Heap::ZERO;

/// Global allocator backed by [`HEAP`].
///
/// SAFETY: the page-aligned `HEAP` static outlives the binary and the
/// allocator is its only consumer.
#[global_allocator]
static ALLOCATOR: FreeListAllocator =
    unsafe { FreeListAllocator::new(core::ptr::addr_of!(HEAP) as *mut u8, HEAP_BYTES) };

/// Sink that replays every event through [`SERIAL_SINK`] and reports PASS
/// to QEMU once all six witnesses have appeared: the per-kind
/// first-input-delivery one-shots (`kind=key` and `kind=pointer` — the
/// autoloaded user-space virtio-input driver instances delivering), the
/// users-database load (the passphrase typed at the virtio keyboard
/// unlocked the encrypted root end to end), the reserved
/// `DISPLAY_ENDPOINT` bind (the autoloaded framebuffer display service
/// came up on its granted surface), the AW4 shell round trip (the
/// `appmgr` load of `/System/Commands/sleep.app` — the windowed terminal
/// received every typed key edge, wrote the line to its hosted shell, and
/// the shell resolved and ran the typed program), and the pty `Ctrl-C`
/// job-control round trip (the `appmgr` load of `/System/Commands/true.app`
/// after the `sleep` round trip — the recovered `true` the shell could
/// only run once `Ctrl-C` interrupted the parked foreground `sleep`,
/// `plans/PTY.md`). The guest exits only after the host has everything it
/// needs (`plans/APPWIN.md` AW3 + AW4, `plans/PTY.md`).
///
/// The file-manager stages (`plans/NEW-FILEMANAGER.md` FM9-a/-b/-c,
/// FM10, FM11) are deliberately *not* driven here: that application UI
/// logic is proven by `lib/browse`'s host unit tests, and folding a
/// long, blind pointer-injection choreography of it into this vertical
/// added only fragility (the FONT-SERVICE delivery-count drift of
/// `plans/OPEN-DEFECTS.md` D20). This vertical proves what only QEMU
/// can: driver autoload, encrypted-root unlock, display bind, and the
/// keyboard → session → terminal → pty → shell round trip.
struct AutoloadInputSink {
    key_delivered: AtomicBool,
    pointer_delivered: AtomicBool,
    users_db_loaded: AtomicBool,
    display_endpoint_bound: AtomicBool,
    /// First distinct window-event destination port (the files window);
    /// `0` until the first app-ward delivery.
    first_window_port: AtomicU64,
    /// Deliveries made to [`Self::first_window_port`]. Counted per window
    /// rather than system-wide so no other app, service or session
    /// surface can advance the files window's readiness markers.
    first_window_deliveries: AtomicU32,
    /// One-shot: [`TERMINAL_FOCUSED_MARKER`] emitted on the first delivery
    /// to the second distinct window port (the terminal gaining focus).
    terminal_focus_marked: AtomicBool,
    shell_round_trip: AtomicBool,
    ctrl_c_recovered: AtomicBool,
}

impl AutoloadInputSink {
    const fn new() -> Self {
        Self {
            key_delivered: AtomicBool::new(false),
            pointer_delivered: AtomicBool::new(false),
            users_db_loaded: AtomicBool::new(false),
            display_endpoint_bound: AtomicBool::new(false),
            first_window_port: AtomicU64::new(0),
            first_window_deliveries: AtomicU32::new(0),
            terminal_focus_marked: AtomicBool::new(false),
            shell_round_trip: AtomicBool::new(false),
            ctrl_c_recovered: AtomicBool::new(false),
        }
    }

    /// Latch the per-kind input witness from an `InputDelivered`
    /// record's `kind` field; an unrecognised value flips neither latch
    /// (fail closed — a malformed witness can never satisfy PASS).
    fn note_input_delivered(&self, event: &Event<'_>) {
        for field in event.fields {
            if field.key != "kind" {
                continue;
            }
            match field.value {
                tairix_log::FieldValue::Str("key") => {
                    self.key_delivered.store(true, Ordering::Release);
                }
                tairix_log::FieldValue::Str("pointer") => {
                    self.pointer_delivered.store(true, Ordering::Release);
                }
                _ => {}
            }
        }
    }

    /// Latch the terminal round-trip and the pty `Ctrl-C` job-control
    /// witnesses from an `appmgr` `APP_LOADED` record's `bundle` field,
    /// each attributed to the *exact bundle the shell loaded* — never a
    /// cumulative delivery count (the drift `plans/OPEN-DEFECTS.md` D20
    /// removed):
    ///
    /// * loading [`TERMINAL_ROUND_TRIP_BUNDLE`] (`sleep`) is the AW4 round
    ///   trip — the shell resolved and ran the typed command. On that
    ///   latch the guest emits [`CTRL_C_ARM_MARKER`] so the host runner
    ///   injects its `Ctrl-C` recovery step against a live, parked
    ///   foreground job (never before one exists).
    /// * loading [`CTRL_C_RECOVERY_BUNDLE`] (`true`) *after* that is the
    ///   recovered job the shell could reach only once `Ctrl-C`
    ///   interrupted the parked `sleep` (it is blocked in `wait` until
    ///   then), so it witnesses the pty cooked-mode job-control path end
    ///   to end (`plans/PTY.md`). On that latch the guest emits
    ///   [`CTRL_C_RECOVERED_MARKER`], the readiness boundary the whole FM9
    ///   file-manager stage waits on. `sleep` is loaded only by the typed
    ///   command and `true` only by the recovery, so each witness is
    ///   uniquely attributable — no overlapping `≥` threshold.
    fn note_bundle_loaded(&self, event: &Event<'_>) {
        let mut bundle = "";
        for field in event.fields {
            if field.key == "bundle" {
                if let tairix_log::FieldValue::Str(value) = field.value {
                    bundle = value;
                }
            }
        }
        if bundle == tairix_test_autoload_input_qemu_aarch64::TERMINAL_ROUND_TRIP_BUNDLE
            && !self.shell_round_trip.swap(true, Ordering::AcqRel)
        {
            // Arm the runner's Ctrl-C injection: a parked foreground job
            // (`sleep`) now exists to interrupt.
            emit_marker(tairix_test_autoload_input_qemu_aarch64::CTRL_C_ARM_MARKER);
        } else if bundle == tairix_test_autoload_input_qemu_aarch64::CTRL_C_RECOVERY_BUNDLE
            && self.shell_round_trip.load(Ordering::Acquire)
        {
            // The recovered `true` loaded: `Ctrl-C` interrupted the parked
            // `sleep` and the shell ran its next command — the pty
            // job-control PASS witness. Latch it.
            self.ctrl_c_recovered.store(true, Ordering::Release);
        }
    }

    /// Latch the display-service witness when a `CallEndpointCreated`
    /// record names the reserved `DISPLAY_ENDPOINT` — compared against
    /// the exact hex spelling the kernel/ipc audit fields render
    /// (`format_hex_u64`), so the match can neither false-positive on a
    /// different endpoint nor drift from the emitter.
    fn note_endpoint_created(&self, event: &Event<'_>) {
        let mut expected_buf = [0u8; 16];
        let expected = format_hex_u64(tairix_abi::display_ipc::DISPLAY_ENDPOINT, &mut expected_buf);
        for field in event.fields {
            if field.key != "endpoint" {
                continue;
            }
            if let tairix_log::FieldValue::Str(value) = field.value {
                if value == expected {
                    self.display_endpoint_bound.store(true, Ordering::Release);
                }
            }
        }
    }

    /// Report the scripted click-through's progress from the destination
    /// **port** of each app-ward window-event delivery, so every marker
    /// names the window it is about.
    ///
    /// The lone port sender serves the files window first and the terminal
    /// second. Deliveries to the first-seen port are therefore the files
    /// window's, and its running total keys the two AW3 markers; a
    /// delivery to any *other* port is the terminal gaining focus. A
    /// system-wide total would name no window at all, and the drift that
    /// gave once stalled this run.
    ///
    /// Only window-event mailboxes count
    /// (`tairix_abi::window_ipc::is_event_endpoint`). Other
    /// `MessageDelivered` ports — notably the Switchboard command
    /// mailbox the session's frame reports ride — must not steal the
    /// first-port slot, or the terminal-focus marker fires on the
    /// files window and the typed command never reaches the shell.
    fn note_window_delivery(&self, event: &Event<'_>) {
        for field in event.fields {
            if field.key != "port" {
                continue;
            }
            let tairix_log::FieldValue::Str(value) = field.value else {
                continue;
            };
            let Ok(port) = u64::from_str_radix(value, 16) else {
                continue;
            };
            if !tairix_abi::window_ipc::is_event_endpoint(port) {
                continue;
            }
            let first = self.first_window_port.load(Ordering::Acquire);
            if first == 0 {
                self.first_window_port.store(port, Ordering::Release);
                self.note_files_window_delivery();
            } else if port == first {
                self.note_files_window_delivery();
            } else if !self.terminal_focus_marked.swap(true, Ordering::AcqRel) {
                emit_marker(tairix_test_autoload_input_qemu_aarch64::TERMINAL_FOCUSED_MARKER);
            }
        }
    }

    /// Count one delivery to the files window and emit whichever AW3
    /// readiness marker that ordinal completes — the activating
    /// `Focus` + `Pressed` pair, then the handshake `Pressed`.
    fn note_files_window_delivery(&self) {
        let delivered = self
            .first_window_deliveries
            .fetch_add(1, Ordering::AcqRel)
            .saturating_add(1);
        if delivered == tairix_test_autoload_input_qemu_aarch64::FILES_ACTIVATION_DELIVERIES {
            emit_marker(tairix_test_autoload_input_qemu_aarch64::FILES_WINDOW_ACTIVATED_MARKER);
        } else if delivered == tairix_test_autoload_input_qemu_aarch64::FILES_HANDSHAKE_DELIVERIES {
            emit_marker(tairix_test_autoload_input_qemu_aarch64::FILES_HANDSHAKE_MARKER);
        }
    }
}

impl Sink for AutoloadInputSink {
    fn write_event(&self, event: &Event<'_>) {
        // Replay through the serial sink so the QEMU transcript records the
        // full boot + unlock + autoload + input timeline (the harness also
        // gates its mouse injection on the `kind=key` line of this replay).
        SerialSink::new().write_event(event);
        if event.id.0 == AuditEvent::InputDelivered.id().0 {
            self.note_input_delivered(event);
        } else if event.id.0 == AuditEvent::UsersDbLoaded.id().0 {
            self.users_db_loaded.store(true, Ordering::Release);
        } else if event.id.0 == tairix_kernel_ipc::AuditEvent::CallEndpointCreated.id().0 {
            self.note_endpoint_created(event);
        } else if event.id.0 == tairix_kernel_ipc::AuditEvent::MessageDelivered.id().0 {
            self.note_window_delivery(event);
        } else if event.id.0 == tairix_appload::events::APP_LOADED.0 {
            // Attributable by the loaded bundle's own name: `sleep` is
            // loaded only by the shell running the typed command (the AW4
            // round trip) and `true` only by the Ctrl-C recovery, so each
            // witness is unambiguous — no fragile delivery-count threshold.
            self.note_bundle_loaded(event);
        } else {
            return;
        }
        if self.key_delivered.load(Ordering::Acquire)
            && self.pointer_delivered.load(Ordering::Acquire)
            && self.users_db_loaded.load(Ordering::Acquire)
            && self.display_endpoint_bound.load(Ordering::Acquire)
            && self.shell_round_trip.load(Ordering::Acquire)
            && self.ctrl_c_recovered.load(Ordering::Acquire)
        {
            qemu_exit::exit_success();
        }
    }
}

/// Print one readiness marker on serial for the host runner to gate a
/// scripted step on. Carries no fields: the marker *is* the fact.
fn emit_marker(message: &'static str) {
    SerialSink::new().write_event(&Event {
        level: tairix_log::Level::Info,
        id: tairix_log::EventId(0),
        message,
        fields: &[],
    });
}

static AUDIT_SINK: AutoloadInputSink = AutoloadInputSink::new();

/// Forward to the shared aarch64 panic bridge. A panic before the PASS
/// finisher parks the CPU, the run times out, and the harness reports
/// `Outcome::Timeout` — the documented fail-loud behaviour.
#[panic_handler]
fn tairix_autoload_input_qemu_aarch64_panic(info: &PanicInfo<'_>) -> ! {
    handle_panic_via_serial(info)
}

/// Boot entry point — the symbol the arch crate's `boot.s` trampoline
/// calls (via `tairix_arch_aarch64_main`).
///
/// QEMU hands no DTB pointer (`_dtb == 0`), so the embedded `virt` blob's
/// address is forwarded to the production boot pipeline with the
/// audit-observer sink in place.
#[no_mangle]
pub extern "C" fn kernel_main(_dtb: u64) -> ! {
    let dtb = DTB_BLOB.as_ptr() as u64;
    boot_aarch64::boot(
        dtb,
        &ALLOCATOR,
        &SERIAL_SINK,
        &AUDIT_SINK,
        // `SyscallInvoked` (`EventId(5000)`) is `Debug`, below the
        // default `Info` filter; the harness waits for this record's
        // `sc=irq_bind` serial marker before injecting the key, so
        // boot with the filter lowered.
        tairix_log::Level::Debug,
        &tairix_kernel::hwtree_store::HW_TREE_SOURCE,
    )
}
