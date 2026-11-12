//! `plans/NETWORK.md` N16b QEMU integration test: boot the *production*
//! aarch64 `tairix-kernel` pipeline on the `virt` board with the planted
//! whole-disk encrypted-root image, a virtio-net device attached, and the
//! harness-side **SYN-flood** client peer on its `dgram` netdev, then prove
//! the listener's RFC 4987 stateless-SYN-cookie defence end to end over the
//! live two-process network — server process, network stack, and virtio-net
//! driver each in its own address space.
//!
//! ## What this test asserts
//!
//! It reuses the N6b-2-β-2 listener vertical's disk and guest fixture
//! unchanged (`FsDisk::ListenRootDisk`, the `tcpserve` server): the thing
//! under test is the *stack's* behaviour under connection exhaustion, not a
//! new guest program. What differs is the peer
//! (`NetPeerMode::V6TcpFlood`), which:
//!
//! 1. fills the listener's bounded half-open backlog with SYNs from distinct
//!    source ports that it **never answers**, so every slot is occupied by a
//!    handshake that will never complete — exactly a SYN flood; then
//! 2. opens one *real* connection. Its SYN meets a full backlog, so the
//!    listener can admit it only by falling back to a stateless cookie:
//!    the server ISN is a keyed MAC over the 4-tuple, and the connection is
//!    reconstructed from the client's returning ACK with no per-connection
//!    state held meanwhile; then
//! 3. streams the whole deterministic transfer over that cookie-admitted
//!    connection and verifies the guest echoes every byte back in order.
//!
//! A stack whose backlog grew without bound, or which refused the connection
//! once the backlog filled, cannot complete step 3.
//!
//! ## Why the PASS needs all three witnesses
//!
//! * **The cookie brake must be observed engaging.** `netstack` audits it
//!   once per listener on the transition (`SYN_COOKIES_ENGAGED`), and the
//!   serial script requires that message before it will await the fixture's
//!   PASS marker. This is the witness that distinguishes a cookie-admitted
//!   connection from an ordinary one: without it a run where the flood never
//!   landed would look identical to a pass.
//! * **The exchange must be verified guest-side.** On a fully verified
//!   exchange `tcpserve` prints `TCPSERVE PASS …` and exits `0`; on any
//!   shortfall it prints the reason and parks. The run passes on the login
//!   shell's audited exit after the server's
//!   (`tairix_itest_witness::FixtureExit`), which the runner types only once
//!   the marker appeared.
//! * **The peer must agree.** Its verdict is `Ok` only if it both sent the
//!   whole flood *and* got the whole transfer echoed back verified, so a run
//!   that never filled the backlog fails even if the guest served an
//!   ordinary connection perfectly.
//!
//! No side passes alone, and the run times out fail-loud with the reason in
//! the serial transcript.
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
    use tairix_kalloc::{Arena, FreeListAllocator, HEAP_BYTES};
    use tairix_kernel::aarch64::boot as boot_aarch64;
    use tairix_log::{Event, EventId, Sink};
    use tairix_test_tcpserve::COMMAND;

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

    /// `EventId` emitted by the syscall dispatcher for an audited syscall
    /// that passed every check. Pinned by the audit-id test in
    /// `kernel/syscall/src/audit.rs`.
    const SYSCALL_INVOKED_EVENT_ID: EventId = EventId(5000);

    /// The run's end: the shell's exit after the server's.
    static END: FixtureExit = FixtureExit::new(&[COMMAND], SYSCALL_INVOKED_EVENT_ID);

    /// Replays every event to the serial console and ends the run at [`END`].
    struct SynFloodSink;

    impl Sink for SynFloodSink {
        fn write_event(&self, event: &Event<'_>) {
            SerialSink::new().write_event(event);
            if END.passed(event) {
                qemu_exit::exit_success();
            }
        }
    }

    static AUDIT_SINK: SynFloodSink = SynFloodSink;

    /// Forward to the shared aarch64 panic bridge. A panic before the PASS
    /// finisher parks the CPU, the run times out, and the harness reports
    /// `Outcome::Timeout` — the documented fail-loud behaviour.
    #[panic_handler]
    fn tairix_netstack_synflood_qemu_aarch64_panic(info: &PanicInfo<'_>) -> ! {
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
