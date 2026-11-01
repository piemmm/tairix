//! Thin downstream boot wrapper for the riscv64 QEMU verticals.
//!
//! The riscv64 boot pipeline itself now lives in the production
//! `tairix-kernel` crate (`tairix_kernel::riscv64::boot`), so there is
//! exactly one riscv64 boot orchestration in the workspace — the same one the production `tairix-kernel` binary runs. This
//! module re-exports the production [`RiscvBinArch`] / [`BootError`] /
//! [`try_boot`] and adds the single test-only affordance the verticals
//! need on top of it: publishing the DTB pointer to the device-bring-up
//! observers ([`crate::publish`]) before delegating to the production
//! [`boot`](tairix_kernel::riscv64::boot::boot).
//!
//! Keeping the publish affordance here (not in the production pipeline)
//! is the discipline: the production kernel never carries a
//! test-observer side channel. The boot-completed QEMU bins swap only
//! the audit sink, exactly as the x86_64 / aarch64 verticals do.

use tairix_kernel_mem::DirectPhysMap;
use tairix_log::{Level, Sink};

pub use tairix_kernel::riscv64::boot::{try_boot, BootError, RiscvBinArch};

/// Boot the riscv64 `virt`-board vertical and forward to the production
/// kernel pipeline.
///
/// Publishes the DTB pointer for the virtio-MMIO / framebuffer
/// device-bring-up observers ([`crate::publish`]), then delegates to
/// [`tairix_kernel::riscv64::boot::boot`].
///
/// `heap` is the consuming vertical's own `#[global_allocator]`, which the
/// production pipeline wires the growable-heap source into.
/// `log_sink` / `audit_sink` are the `&'static` sinks the consuming
/// vertical installs; its audit sink flips the `SiFive` Test device on
/// `AuditEvent::BootCompleted`. `log_level` is the initial global log
/// filter `kernel_main` installs (`BootInfo::log_level`); a vertical
/// that must observe the `Debug`-level allow records (e.g.
/// `SyscallInvoked`, `EventId(5000)`) passes `Level::Debug`, the rest
/// pass `Level::Info`. Returns the bottom type.
///
/// # SAFETY-INVARIANT
///
/// `dtb` must be the verbatim `a1` device-tree pointer OpenSBI handed
/// the boot hart, forwarded unchanged by the arch port's `boot.s`.
pub fn boot(
    hartid: u64,
    dtb: u64,
    heap: &'static tairix_kalloc::FreeListAllocator,
    log_sink: &'static (dyn Sink + Sync),
    audit_sink: &'static (dyn Sink + Sync),
    log_level: Level,
) -> ! {
    crate::publish::publish_dtb(dtb);
    tairix_kernel::riscv64::boot::boot(hartid, dtb, heap, log_sink, audit_sink, log_level)
}

/// The identity window every root the kernel runs under maps — the board's
/// device registers and the low RAM a vertical's buffers sit in — as a
/// physical map.
#[must_use]
pub fn identity_map() -> DirectPhysMap {
    let reach = (tairix_arch_riscv64::paging::KERNEL_IDENTITY_GIGAPAGES as u64) << 30;
    // SAFETY: every root the kernel builds maps `[0, reach)` as itself and
    // none unmaps it.
    unsafe { DirectPhysMap::identity(reach) }.expect("the identity window addresses itself")
}
