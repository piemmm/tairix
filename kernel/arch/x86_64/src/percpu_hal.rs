//! x86_64 per-CPU storage ("per-CPU storage").
//!
//! Implements the Arch HAL [`PerCpu`](tairix_arch_api::PerCpu) surface
//! for x86_64 over the **GS base** — the `IA32_GS_BASE` MSR
//! (`0xC000_0101`). While running in the kernel the active GS base is the
//! per-CPU anchor (the syscall stub's `swapgs` makes the kernel's per-CPU
//! TLS the active base on entry; see [`crate::syscall_entry`]), so
//! reading and writing `IA32_GS_BASE` reads and writes the calling CPU's
//! per-CPU word. This slice keeps that MSR access in one place so the
//! architecture-neutral kernel reaches the per-CPU word through the one
//! HAL trait.
//!
//! This is distinct from [`crate::percpu`], which owns the per-CPU GDT /
//! IDT / IST-stack bring-up; that module's `SyscallTls` block is *what*
//! the GS base points at, while this slice is the generic read/write of
//! the base register itself.
//!
//! The stored word is opaque to this surface (see the
//! [`PerCpu`](tairix_arch_api::PerCpu) trait docs). On the host build
//! there is no GS base, so the handle backs the word with an in-handle
//! cell solely for the unit tests; it is never linked into a kernel image.

use core::sync::atomic::AtomicUsize;

use tairix_arch_api::PerCpu;

/// The `IA32_GS_BASE` MSR (Intel SDM Vol 4 §2.1): the base address the
/// `gs:` segment prefix adds while in kernel mode.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub const IA32_GS_BASE: u32 = 0xC000_0101;

/// x86_64 implementation of the Arch HAL per-CPU storage surface.
///
/// On the bare-metal target the read/write hit the `IA32_GS_BASE` MSR, so
/// the in-handle cell is unused there; on the host build it backs the
/// word so the round-trip and isolation conformance verticals run under
/// `cargo test`.
#[derive(Debug, Default)]
pub struct PerCpuStorage {
    /// Host-only backing for the per-CPU word. On the bare-metal target
    /// the GS base MSR is the source of truth and this field is never
    /// read; kept so the host and bare-metal builds share one struct
    /// shape.
    #[cfg_attr(all(target_arch = "x86_64", target_os = "none"), allow(dead_code))]
    host_base: AtomicUsize,
}

impl PerCpuStorage {
    /// Construct the x86_64 per-CPU storage handle.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            host_base: AtomicUsize::new(0),
        }
    }
}

impl PerCpu for PerCpuStorage {
    fn read_self_base(&self) -> usize {
        #[cfg(all(target_arch = "x86_64", target_os = "none"))]
        {
            // SAFETY: `IA32_GS_BASE` is unconditionally present in long mode
            // and reading it is side-effect-free; the kernel runs at CPL 0.
            let base = unsafe { crate::msr::read(IA32_GS_BASE) };
            // A pointer is 64 bits wide on x86_64, so the base converts whole.
            #[allow(clippy::cast_possible_truncation)]
            let base = base as usize;
            base
        }
        #[cfg(not(all(target_arch = "x86_64", target_os = "none")))]
        {
            self.host_base.load(core::sync::atomic::Ordering::Relaxed)
        }
    }

    unsafe fn write_self_base(&self, base: usize) {
        #[cfg(all(target_arch = "x86_64", target_os = "none"))]
        {
            // SAFETY: `IA32_GS_BASE` accepts any canonical 64-bit base; the
            // trait's safety contract requires the caller to run this on the
            // CPU whose word is being set and to pass the value the kernel's
            // per-CPU resolution expects. The write is privileged (CPL 0,
            // which the kernel holds).
            unsafe { crate::msr::write(IA32_GS_BASE, base as u64) }
        }
        #[cfg(not(all(target_arch = "x86_64", target_os = "none")))]
        {
            self.host_base
                .store(base, core::sync::atomic::Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tairix_arch_api::percpu::conformance;

    #[test]
    fn passes_per_cpu_conformance() {
        conformance::run_all(&PerCpuStorage::new());
        let dynamic: &dyn PerCpu = &PerCpuStorage::new();
        conformance::run_all(dynamic);
    }

    #[test]
    fn per_cpu_word_is_isolated_across_cpus() {
        conformance::run_isolation(&PerCpuStorage::new(), &PerCpuStorage::new());
    }
}
