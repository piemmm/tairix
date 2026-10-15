//! The ICMP echo tools QEMU vertical (`plans/NETWORK.md` N8b-2b-β,
//! `plans/NETMAP.md` NM-S3): boot the *production* aarch64 `tairix-kernel`
//! pipeline on the `virt` board with the planted whole-disk encrypted-root
//! image, a virtio-net device attached, and the harness-side echo peer on its
//! `dgram` netdev, then prove `ping` and `traceroute` end to end over the live
//! two-process network — each tool, the network stack, and the virtio-net
//! driver in its own address space.
//!
//! The disk's `/System` volume carries the standard signed store bundles —
//! the real `ping` and `traceroute` among them, discovered from `userland/apps`
//! by the same composer as every other bundle — plus the signed virtio-net
//! driver. No test fixture is planted: both are shipping tools, run exactly as
//! a user would run them.
//!
//! The runner unlocks the root, authenticates `root`/`root`, and types
//! `ping -c 3 fe80::2`, then — once `ping`'s statistics print —
//! `traceroute -n fe80::2`. The peer answers echoes to itself and answers a
//! probe whose hop limit runs out short of it as the router it reached, so the
//! trace has three hops: the two routers, then the peer.
//!
//! ## Why the PASS keys on both tools' exits *then* the shell's exit
//!
//! A tool's exit alone proves nothing: `ping -c 3` exits whether or not a
//! reply came, and `traceroute` ends at its hop bound however far it got. The
//! runner types the shell `exit` only after the trace printed the peer's own
//! hop, having first seen `ping`'s `icmp_seq=` and each router's hop, so an
//! unanswered run never reaches it and times out fail-loud with the
//! transcript; and the harness requires the peer's verdict that it served an
//! echo and answered as both routers. This sink arms once both tools' audited
//! exits (`SyscallInvoked`, `sc=exit`) are seen and reports PASS on the next —
//! the shell's — so everything the runner waited for reached the transcript
//! before the run ended. Only the audit sink differs from a production kernel,
//! and it lives in a dedicated bin so feature unification can never leak the
//! QEMU-exit shortcut into a production build.

#![cfg_attr(itest_aarch64, no_std)]
#![cfg_attr(itest_aarch64, no_main)]
#![deny(missing_docs)]

// --- Freestanding test bin (`aarch64-unknown-none`) ----------------

#[cfg(itest_aarch64)]
mod kernel {
    use core::panic::PanicInfo;
    use core::sync::atomic::{AtomicU8, Ordering};

    use tairix_arch_aarch64::{handle_panic_via_serial, qemu_exit, SerialSink, SERIAL_SINK};
    use tairix_itest_witness::field_str;
    use tairix_kalloc::{FreeListAllocator, Heap, HEAP_BYTES};
    use tairix_kernel::aarch64::boot as boot_aarch64;
    use tairix_log::{Event, EventId, Sink};

    // The canonical QEMU `virt` device tree, dumped and embedded at build
    // time (`build.rs`). The boot pipeline discovers the board from it
    // because QEMU passes no `x0` DTB pointer at an ELF `-kernel` entry.
    include!(concat!(env!("OUT_DIR"), "/dtb_fixture.rs"));

    /// The audited `comm`s of the two tools — their `AppInfo.toml` bundle
    /// names, which the spawn path derives the process command stem from.
    /// They are shipping command apps, not test fixtures, so there is no
    /// fixture crate to import them from; they are the command words the
    /// runner types.
    const TOOLS: [&str; 2] = ["ping", "traceroute"];

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

    /// Which of [`TOOLS`] have exited, one bit each.
    static EXITED: AtomicU8 = AtomicU8::new(0);

    /// Sink that replays every event through [`SERIAL_SINK`] and reports PASS
    /// on the first audited `exit` after both tools' own.
    struct EchoToolsSink;

    impl Sink for EchoToolsSink {
        fn write_event(&self, event: &Event<'_>) {
            // Replay through the serial sink so the QEMU transcript records the
            // full boot + autoload + probe timeline.
            SerialSink::new().write_event(event);
            if event.id != SYSCALL_INVOKED_EVENT_ID || field_str(event, "sc") != Some("exit") {
                return;
            }
            let comm = field_str(event, "comm");
            match TOOLS.iter().position(|tool| comm == Some(*tool)) {
                Some(tool) => {
                    EXITED.fetch_or(1 << tool, Ordering::AcqRel);
                }
                None if EXITED.load(Ordering::Acquire) == (1 << TOOLS.len()) - 1 => {
                    qemu_exit::exit_success();
                }
                None => {}
            }
        }
    }

    static AUDIT_SINK: EchoToolsSink = EchoToolsSink;

    /// Forward to the shared aarch64 panic bridge. A panic before the PASS
    /// finisher parks the CPU, the run times out, and the harness reports
    /// `Outcome::Timeout` — the documented fail-loud behaviour.
    #[panic_handler]
    fn tairix_netstack_icmp_qemu_aarch64_panic(info: &PanicInfo<'_>) -> ! {
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
