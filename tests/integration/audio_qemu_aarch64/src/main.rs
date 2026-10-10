//! `plans/SOUND.md` QEMU audio vertical: boot the production aarch64
//! `tairix-kernel` pipeline against a whole-disk audio-root image — whose
//! read-only `/System` volume carries the kernel-signed drivers for the sound
//! card the run attaches and the test-only `audiotone` fixture — with that
//! card behind QEMU's `wav` audio backend, and prove the whole audio path end
//! to end with an exact numeric assertion. One binary serves both cards: a
//! virtio sound device, and QEMU's USB speaker behind an xHCI controller.
//!
//! ## What this vertical asserts
//!
//! The production boot path discovers the card and `devmgr` autoloads its
//! driver into its **own user process** — for the USB speaker, the host
//! controller's driver first, then the USB Audio driver over the interface it
//! publishes. The audio driver claims a reserved `audiochan-v1` endpoint and
//! publishes its channel node, `devmgr` hands that endpoint to **`audiod`**,
//! and the scripted root shell runs `audiotone` (or `play` on the signal as a
//! WAV file), which opens an `audio-v1`
//! stream, queues a deterministic signal, starts the device and drains. Every
//! frame crosses at least two real process boundaries and two shared PCM rings
//! before it reaches the card.
//!
//! The guest's own `AUDIO PASS` witness says the stack reported success with
//! no lost frames. The **host-side** assertion says more: QEMU's `wav`
//! backend wrote what the emulated card actually received, and the harness
//! checks it holds exactly the samples the guest played. A mixer that
//! silently substituted, resampled or dropped frames would still print the
//! witness, so the run needs both.
//!
//! The run is deterministic rather than a race: the ring is sized to hold the
//! whole signal, so every frame is queued before the device is clocked and
//! the device cannot run dry however slowly the emulated machine runs.
//!
//! ## How it differs from a production kernel
//!
//! It reuses the entire production aarch64 boot pipeline unchanged. The only
//! difference is the audit-stream observer that ends the run on the scripted
//! exit; it lives in this test bin, so no QEMU-exit shortcut leaks into a
//! production build.

#![cfg_attr(itest_aarch64, no_std)]
#![cfg_attr(itest_aarch64, no_main)]
#![deny(missing_docs)]

// --- Freestanding test bin (`aarch64-unknown-none`) ----------------

#[cfg(itest_aarch64)]
mod kernel {
    use core::panic::PanicInfo;

    use tairix_arch_aarch64::{handle_panic_via_serial, qemu_exit, SerialSink, SERIAL_SINK};
    use tairix_itest_witness::field_str;
    use tairix_kalloc::{FreeListAllocator, Heap, HEAP_BYTES};
    use tairix_kernel::aarch64::boot as boot_aarch64;
    use tairix_log::{Event, EventId, Sink};
    use tairix_test_audio_wire::Finisher;

    // The canonical QEMU `virt` device tree, dumped and embedded at build
    // time (`build.rs`). The boot pipeline discovers the board from it
    // because QEMU passes no `x0` DTB pointer at an ELF `-kernel` entry.
    include!(concat!(env!("OUT_DIR"), "/dtb_fixture.rs"));

    /// `SyscallInvoked`, the audited record the finisher below counts.
    const SYSCALL_INVOKED_EVENT_ID: EventId = EventId(5000);

    /// The guest's verdict, fed every audited `exit`.
    static FINISHER: Finisher = Finisher::new();

    /// Sink that replays every event through [`SERIAL_SINK`] and ends the run
    /// once [`FINISHER`] says so.
    struct AudioSink;

    impl Sink for AudioSink {
        fn write_event(&self, event: &Event<'_>) {
            SerialSink::new().write_event(event);
            if event.id != SYSCALL_INVOKED_EVENT_ID || field_str(event, "sc") != Some("exit") {
                return;
            }
            if field_str(event, "comm").is_some_and(|comm| FINISHER.exited(comm)) {
                qemu_exit::exit_success();
            }
        }
    }

    static AUDIT_SINK: AudioSink = AudioSink;

    /// Static boot heap, mirroring the production aarch64 kernel binary's
    /// `.bss`-resident heap (zeroed by the boot trampoline).
    ///
    /// `static mut` because the free-list allocator hands out disjoint slices
    /// via an atomic cursor; the storage is otherwise never aliased.
    static mut HEAP: Heap = Heap::ZERO;

    /// Global allocator backed by [`HEAP`].
    ///
    /// SAFETY: the page-aligned `HEAP` static outlives the binary and the
    /// allocator is its only consumer.
    #[global_allocator]
    static ALLOCATOR: FreeListAllocator =
        unsafe { FreeListAllocator::new(core::ptr::addr_of!(HEAP) as *mut u8, HEAP_BYTES) };

    /// Forward to the shared aarch64 panic bridge. A panic parks the CPU; the
    /// guest never self-exits, so the run times out and the harness reports
    /// `Outcome::Timeout` — the documented fail-loud behaviour.
    #[panic_handler]
    fn tairix_audio_qemu_aarch64_panic(info: &PanicInfo<'_>) -> ! {
        handle_panic_via_serial(info)
    }

    /// Boot entry point — the symbol the arch crate's `boot.s` trampoline
    /// calls (via `tairix_arch_aarch64_main`).
    ///
    /// QEMU hands no DTB pointer (`_dtb == 0`), so the embedded `virt` blob's
    /// address is forwarded to the production boot pipeline. [`SERIAL_SINK`]
    /// carries the log stream so every boot/autoload/bind record reaches the
    /// QEMU transcript, and [`AudioSink`] observes the audit stream to finish
    /// the run once the fixture and then the shell have exited.
    #[no_mangle]
    pub extern "C" fn kernel_main(_dtb: u64) -> ! {
        let dtb = DTB_BLOB.as_ptr() as u64;
        boot_aarch64::boot(
            dtb,
            &ALLOCATOR,
            &SERIAL_SINK,
            &AUDIT_SINK,
            // `SyscallInvoked` is `Debug`, below the default filter, and
            // the finisher above counts it.
            tairix_log::Level::Debug,
            &tairix_kernel::hwtree_store::HW_TREE_SOURCE,
        )
    }
}

// --- Host stub -----------------------------------------------------
#[cfg(not(itest_aarch64))]
fn main() {}
