//! `plans/NETWORK.md` N6b-2-β-2 QEMU integration test: boot the *production*
//! aarch64 `tairix-kernel` pipeline on the `virt` board with the planted
//! whole-disk encrypted-root image, a virtio-net device attached, and the
//! harness-side active TCP client peer on its `dgram` netdev, then prove the
//! **server-side stream-socket** (`SocketType::Stream` listener) path end to
//! end over the live two-process network — server process, network stack, and
//! virtio-net driver each in its own address space. It is the role-swapped
//! mirror of `netstack_stream_qemu_aarch64` (which runs a guest *client*
//! against a host *echo server*).
//!
//! ## What this test asserts
//!
//! The vertical's disk is the shared encrypted-root fixture whose read-only
//! `/System` volume carries the standard signed store bundles **plus** the
//! signed virtio-net driver bundle (so `devmgr` autoloads it into its own
//! process and `netstack` binds it) **plus** the test-only `tcpserve` fixture
//! bundle (`tests/integration/tcpserve_program`), all composed and signed by
//! the same `tools/xtask` composer as every store bundle and planted only on
//! this vertical's disk — no production image ever ships the fixtures.
//!
//! The runner unlocks the root at the passphrase prompt, authenticates
//! `root`/`root` at the console login, and types the bare command word
//! `tcpserve` at the shell: the store-then-`PATH` resolution finds
//! `/System/Commands/tcpserve.app/Run`, the disk-backed spawn path verifies the
//! signed bundle, and the server runs with `manifest ∩ administrator-ceiling`
//! authority (its manifest requests `CAP_NET` **and** `CAP_NET_BIND_PRIVILEGED`,
//! both enforced by the netstack socket dispatcher against the kernel-attested
//! origin). The server binds the well-known (privileged) port, listens,
//! accepts the host client peer's connection over the shared IPv6 link-local
//! wire, echoes every received byte back, and verifies the received run. The
//! peer injects bounded frame loss, so a passing run proves RFC 9293
//! retransmission carried the stream both ways across the two-process boundary
//! — not merely a clean link.
//!
//! ## Why the PASS keys on the server's exit *then* the shell's exit
//!
//! On a fully verified exchange the server prints its `TCPSERVE PASS …` line
//! and exits `0`; on any shortfall it prints the reason and parks. The run
//! passes on the login shell's audited exit after the server's
//! (`tairix_itest_witness::FixtureExit`), which the runner types only once the
//! marker appeared, and the harness also requires the client peer to report
//! the whole transfer echoed and verified, so neither side can pass alone. A
//! refused bind (missing `CAP_NET_BIND_PRIVILEGED`), a mismatched or truncated
//! stream, or a server crash times out with its diagnosis on serial.
//!
//! ## How it differs from a production kernel
//!
//! It reuses the entire production aarch64 boot pipeline and only replaces the
//! audit sink. Splitting the audit-observer behaviour into a separate bin
//! (instead of a Cargo feature on a production crate) prevents feature
//! unification from leaking the QEMU-exit shortcut into any production build
//! (fail closed; the harness never decides what the kernel does next).

#![cfg_attr(itest_aarch64, no_std)]
#![cfg_attr(itest_aarch64, no_main)]
#![deny(missing_docs)]

// --- Freestanding test bin (`aarch64-unknown-none`) ----------------

#[cfg(itest_aarch64)]
mod kernel {
    use core::panic::PanicInfo;

    use tairix_arch_aarch64::{handle_panic_via_serial, qemu_exit, SerialSink, SERIAL_SINK};
    use tairix_itest_witness::FixtureExit;
    use tairix_kalloc::{FreeListAllocator, Heap, HEAP_BYTES};
    use tairix_kernel::aarch64::boot as boot_aarch64;
    use tairix_log::{Event, EventId, Sink};
    use tairix_test_tcpserve::COMMAND;

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

    /// `EventId` emitted by the syscall dispatcher for an audited syscall
    /// that passed every check. Pinned by the audit-id test in
    /// `kernel/syscall/src/audit.rs`.
    const SYSCALL_INVOKED_EVENT_ID: EventId = EventId(5000);

    /// The run's end: the shell's exit after the server's.
    static END: FixtureExit = FixtureExit::new(&[COMMAND], SYSCALL_INVOKED_EVENT_ID);

    /// Replays every event to the serial console and ends the run at [`END`].
    struct ListenerSink;

    impl Sink for ListenerSink {
        fn write_event(&self, event: &Event<'_>) {
            SerialSink::new().write_event(event);
            if END.passed(event) {
                qemu_exit::exit_success();
            }
        }
    }

    static AUDIT_SINK: ListenerSink = ListenerSink;

    /// Forward to the shared aarch64 panic bridge. A panic before the PASS
    /// finisher parks the CPU, the run times out, and the harness reports
    /// `Outcome::Timeout` — the documented fail-loud behaviour.
    #[panic_handler]
    fn tairix_netstack_listener_qemu_aarch64_panic(info: &PanicInfo<'_>) -> ! {
        handle_panic_via_serial(info)
    }

    /// Boot entry point — the symbol the arch crate's `boot.s` trampoline
    /// calls (via `tairix_arch_aarch64_main`).
    ///
    /// QEMU hands no DTB pointer (`_dtb == 0`), so the embedded `virt` blob's
    /// address is forwarded to the production boot pipeline with the
    /// audit-observer sink in place. `SyscallInvoked` (`EventId(5000)`) is a
    /// `Debug` record, below the default `Info` filter; this observer counts
    /// it, so boot with the filter lowered.
    #[no_mangle]
    pub extern "C" fn kernel_main(_dtb: u64) -> ! {
        let dtb = DTB_BLOB.as_ptr() as u64;
        boot_aarch64::boot(
            dtb,
            &ALLOCATOR,
            &SERIAL_SINK,
            &AUDIT_SINK,
            tairix_log::Level::Debug,
            &tairix_kernel::hwtree_store::HW_TREE_SOURCE,
        )
    }
}

// --- Host stub -----------------------------------------------------
#[cfg(not(itest_aarch64))]
fn main() {}
