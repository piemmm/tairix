//! QEMU integration vertical: **boot the graphical desktop with a router-
//! advertising network peer, launch `netmap` from the program library, and
//! hold the run to the map drawing its interface, subnet and gateway tiers on
//! screen** (`plans/NETMAP.md` `NM2c`).
//!
//! # What this proves that no host test can
//!
//! The map is host-tested whole — the model, the ingest, the inference, the
//! layout, the viewport and the renderer. What only a running machine can show
//! is that `netmap` is wired to the live system: that launched from the desktop
//! it reads the real interface state, neighbour cache and routing table of a
//! booted machine — whose one NIC has autoconfigured a global address and a
//! default route against the router-advertising peer — through the capability-
//! checked System Information API, and draws them.
//!
//! So the guest boots the **production** aarch64 pipeline (`boot_aarch64::boot`)
//! against a planted encrypted root carrying the input, framebuffer and
//! virtio-net driver bundles and the complete app store, with the host
//! attaching a SLAAC router-advertisement peer to its NIC; the host drives the
//! desktop blind through the QEMU monitor: unlock, log in, start the desktop,
//! open the library, click the map's row, photograph the window once it wears
//! the title it takes on its first read, then *Quit* it. Only the audit sink is
//! swapped, for the PASS witnesses below.
//!
//! # The PASS gate
//!
//! Four latches which must land **in order**, each attributed by the kernel:
//!
//! 1. **The map launched.** An `APP_LOADED` record naming the bundle
//!    [`NETMAP_APP_NAME`] spells — the library row's own launch.
//! 2. **Its window opened.** A `WINDOW_CREATE_REPLY_LEN` reply on the reserved
//!    window endpoint after that load. `netmap` opens one window, and the
//!    desktop's own surfaces never call the window channel.
//! 3. **Its active probe reached a host.** A netmap audit record (its id in
//!    netmap's own event range) carrying a reachable count of at least one,
//!    after the window opened — the gateway answering the live ICMP-echo sweep.
//!    This is what no host test can show: the probe actually reaching the real
//!    network the guest booted onto.
//! 4. **The map exited cleanly.** A `SyscallInvoked` record naming the exit
//!    syscall from [`NETMAP_COMM`], after the window opened — the *Quit* the
//!    host injects as its final gesture, which it gates on the probe's own audit
//!    line, so the probe reaches a host before the map is quit.
//!
//! # Why the guest cannot false-pass
//!
//! The screendump the tier assertion reads is gated by the host on the window
//! on screen wearing its post-first-read title. A map that failed before
//! drawing data exits too, but no dump is taken, so the tier assertion fails
//! the run; and a map whose probing never reached a host never latches (3), so
//! the host never quits it and the run times out. A panic parks the CPU and the
//! runner reports a timeout. None is a false pass.

#![cfg_attr(itest_aarch64, no_std)]
#![cfg_attr(itest_aarch64, no_main)]
#![deny(missing_docs)]

// --- Freestanding test bin (`aarch64-unknown-none`) ----------------

#[cfg(itest_aarch64)]
mod kernel {
    use core::panic::PanicInfo;
    use core::sync::atomic::{AtomicBool, Ordering};

    use tairix_arch_aarch64::{handle_panic_via_serial, qemu_exit, SerialSink, SERIAL_SINK};
    use tairix_itest_witness::{field_str, field_u64, names_bundle};
    use tairix_kalloc::{FreeListAllocator, Heap, HEAP_BYTES};
    use tairix_kernel::aarch64::boot as boot_aarch64;
    use tairix_log::{Event, Sink};
    use tairix_test_netmap_qemu_aarch64::{EXIT_SYSCALL, NETMAP_APP_NAME, NETMAP_COMM};
    use tairix_util::fmt::format_hex_u64;

    // The canonical QEMU `virt` device tree, dumped and embedded at build time
    // (`build.rs`). The boot pipeline discovers the board from it because QEMU
    // passes no `x0` DTB pointer at an ELF `-kernel` entry.
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

    /// Audit observer that replays the whole trail to serial and latches the
    /// ordered PASS witnesses described in the module docs.
    struct NetMapSink {
        /// The map bundle was loaded.
        launched: AtomicBool,
        /// Its window was created after that.
        window_opened: AtomicBool,
        /// Its active probe reached at least one host after that.
        probe_reached: AtomicBool,
        /// It exited cleanly after that.
        exited: AtomicBool,
    }

    impl NetMapSink {
        /// A sink with no witness latched.
        const fn new() -> Self {
            Self {
                launched: AtomicBool::new(false),
                window_opened: AtomicBool::new(false),
                probe_reached: AtomicBool::new(false),
                exited: AtomicBool::new(false),
            }
        }

        /// Latch the launch witness from an `APP_LOADED` record naming the map
        /// bundle.
        fn note_bundle_loaded(&self, event: &Event<'_>) {
            if field_str(event, "bundle").is_some_and(|bundle| {
                names_bundle(
                    bundle,
                    tairix_abi::SYSTEM_APPLICATION_STORE,
                    NETMAP_APP_NAME,
                )
            }) {
                self.launched.store(true, Ordering::Release);
            }
        }

        /// Latch the window witness from a create reply on the reserved window
        /// endpoint, once the bundle has loaded. The endpoint is matched
        /// against the exact hex spelling the kernel/ipc audit field renders,
        /// and the operation by its reply's wire length.
        fn note_call_replied(&self, event: &Event<'_>) {
            if !self.launched.load(Ordering::Acquire) {
                return;
            }
            let mut endpoint_hex = [0u8; 16];
            let expected =
                format_hex_u64(tairix_abi::window_ipc::WINDOW_ENDPOINT, &mut endpoint_hex);
            if field_str(event, "endpoint") != Some(expected) {
                return;
            }
            let reply_len = field_str(event, "len")
                .and_then(tairix_util::count::parse_decimal)
                .and_then(|len| usize::try_from(len).ok())
                .unwrap_or_default();
            if reply_len == tairix_abi::window_ipc::WINDOW_CREATE_REPLY_LEN {
                self.window_opened.store(true, Ordering::Release);
            }
        }

        /// Latch the probe witness from a netmap audit record carrying a
        /// reachable-host count of at least one, once the window has opened.
        /// The record is identified by its id falling in netmap's own event
        /// range and its carrying the `live` field only `PROBE_FINISHED` has, so
        /// no other service's audit matches. Proving a host was reached — not
        /// merely that a campaign ran — is what witnesses that active probing
        /// reached the live map (the gateway answers the sweep).
        fn note_probe(&self, event: &Event<'_>) {
            if !self.window_opened.load(Ordering::Acquire) {
                return;
            }
            if field_u64(event, "live").is_some_and(|live| live >= 1) {
                self.probe_reached.store(true, Ordering::Release);
            }
        }

        /// Latch the exit witness from the map's own `exit` syscall, once its
        /// window has opened. Both the calling process and the syscall are read
        /// from the record's kernel-attested fields, so no other principal's
        /// exit matches (fail closed).
        fn note_syscall(&self, event: &Event<'_>) {
            if !self.window_opened.load(Ordering::Acquire) {
                return;
            }
            if field_str(event, "sc") == Some(EXIT_SYSCALL)
                && field_str(event, "comm") == Some(NETMAP_COMM)
            {
                self.exited.store(true, Ordering::Release);
            }
        }

        /// Whether every witness is in: launch, then window, then a probe that
        /// reached a host, then a clean exit. The host gates the quit gesture on
        /// the probe's audit line, so the probe is in before the exit.
        fn passed(&self) -> bool {
            self.probe_reached.load(Ordering::Acquire) && self.exited.load(Ordering::Acquire)
        }
    }

    impl Sink for NetMapSink {
        fn write_event(&self, event: &Event<'_>) {
            // Replay through the serial sink first, so the QEMU transcript
            // records the full boot → unlock → desktop → map timeline and the
            // host can gate its injection on it.
            SerialSink::new().write_event(event);
            if event.id.0 == tairix_appload::events::APP_LOADED.0 {
                self.note_bundle_loaded(event);
            } else if event.id.0 == tairix_kernel_ipc::AuditEvent::CallReplied.id().0 {
                self.note_call_replied(event);
            } else if event.id.0 == tairix_kernel_syscall::AuditEvent::SyscallInvoked.id().0 {
                self.note_syscall(event);
            } else if tairix_log::registry::NETMAP.contains(event.id) {
                self.note_probe(event);
            } else {
                return;
            }
            if self.passed() {
                qemu_exit::exit_success();
            }
        }
    }

    /// The audit observer the boot pipeline is handed.
    static AUDIT_SINK: NetMapSink = NetMapSink::new();

    /// Forward to the shared aarch64 panic bridge. A panic before the PASS
    /// finisher parks the CPU, the run times out, and the harness reports
    /// `Outcome::Timeout` — the documented fail-loud behaviour.
    #[panic_handler]
    fn tairix_netmap_qemu_aarch64_panic(info: &PanicInfo<'_>) -> ! {
        handle_panic_via_serial(info)
    }

    /// Boot entry point — the symbol the arch crate's `boot.s` trampoline calls
    /// (via `tairix_arch_aarch64_main`).
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
            // `SyscallInvoked` (`EventId(5000)`) is `Debug`, below the default
            // `Info` filter; it carries the exit witness and the host waits for
            // its `sc=irq_bind` marker before typing the unlock passphrase, so
            // boot with the filter lowered.
            tairix_log::Level::Debug,
            &tairix_kernel::hwtree_store::HW_TREE_SOURCE,
        )
    }
}

// --- Host stub -----------------------------------------------------
#[cfg(not(itest_aarch64))]
fn main() {}
