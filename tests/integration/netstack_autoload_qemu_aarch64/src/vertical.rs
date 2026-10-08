//! The production aarch64 boot, over the tree of the machine a binary runs
//! on: the netstack-autoload vertical both binaries share.

use core::panic::PanicInfo;

use tairix_arch_aarch64::{handle_panic_via_serial, SERIAL_SINK};
use tairix_kalloc::{FreeListAllocator, Heap, HEAP_BYTES};
use tairix_kernel::aarch64::boot as boot_aarch64;

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

/// Forward to the shared aarch64 panic bridge. A panic parks the CPU; the
/// guest never self-exits, so the run times out and the harness reports
/// `Outcome::Timeout` — the documented fail-loud behaviour.
#[panic_handler]
fn tairix_netstack_autoload_qemu_aarch64_panic(info: &PanicInfo<'_>) -> ! {
    handle_panic_via_serial(info)
}

/// Boot the production pipeline over `tree`, the machine's device tree:
/// QEMU hands no DTB pointer at an ELF `-kernel` entry. [`SERIAL_SINK`]
/// takes both the log and the audit streams, so every boot/autoload/bind/
/// echo record reaches the QEMU transcript for diagnosis. The guest does
/// not self-exit: the harness ends the run when the host peer confirms the
/// echo round-trip (its success gate), so teardown can never precede that
/// confirmation. Boot at the default `Info` filter: keeping the noisier
/// `Debug` syscall trace off the wire stops the console-login read-retry
/// chatter from crowding the network timeline out of a failing run's serial
/// tail.
pub fn boot(tree: &'static [u8]) -> ! {
    boot_aarch64::boot(
        tree.as_ptr() as u64,
        &ALLOCATOR,
        &SERIAL_SINK,
        &SERIAL_SINK,
        tairix_log::Level::Info,
        &tairix_kernel::hwtree_store::HW_TREE_SOURCE,
    )
}
