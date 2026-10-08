//! `plans/PI.md` P6c-2 QEMU integration test: boot the aarch64 (Raspberry
//! Pi 4) `tairix-kernel` pipeline on the `virt` board to
//! `AuditEvent::BootCompleted` and report success to QEMU.
//!
//! ## What this test asserts
//!
//! `kernel_core::kernel_main` emits `AuditEvent::BootCompleted`
//! (`EventId(4004)`) once every init phase (Log → Mem → Sec → Sched →
//! Irq → Syscall → Ipc) has succeeded. This binary drives the real
//! aarch64 boot pipeline — `tairix_kernel::aarch64::boot::boot` — end to
//! end on the `virt` board:
//!
//! 1. The arch crate's `boot.s` trampoline drops to EL1, establishes a
//!    stack, zeroes `.bss`, and calls `kernel_main`.
//! 2. `boot_aarch64::boot` enables the stage-1 identity MMU + EL1
//!    vectors, discovers the board from the device tree, builds the
//!    `BootMemoryMap`, installs the discovered-UART console + the `svc`
//!    dispatch callback, and hands a validated `BootInfo` to
//!    `kernel_core::kernel_main`.
//! 3. The audit sink observes `BootCompleted`, requires the ramfb
//!    framebuffer boot console to be active (the harness attaches
//!    `-device ramfb`, so the pre-MMU video bring-up must have found
//!    the tree's `fw_cfg` node and programmed the scan-out — the
//!    display path `cargo xtask run` relies on — an inactive video
//!    console is reported as FAIL), then waits for the production SMP
//!    bring-up: the run is `-smp 4`, so `kernel_main` PSCI-starts the
//!    three secondaries the embedded tree's `/cpus` declares, and each
//!    attests its arrival in the kernel dispatch loop with
//!    `AuditEvent::SecondaryCpuOnline` (`EventId(4072)`). The PASS
//!    finisher fires only once all three are online — the end-to-end
//!    proof the production boot brings every discovered core into
//!    service; a `SecondaryCpuStartFailed` (`EventId(4071)`) is an
//!    immediate FAIL.
//! 4. It also requires the production boot to have installed the fatal
//!    fault handler, without which a fault gets only the port's bare
//!    report — no registers, no backtrace.
//!
//! A regression that fails any init phase — or that loses a secondary
//! core — never reaches the finisher, so the run times out and the
//! harness reports `Outcome::Timeout` — the documented fail-loud
//! behaviour.
//!
//! ## Embedded `virt` device tree
//!
//! QEMU's `-kernel <ELF>` aarch64 path passes no DTB pointer (`x0 = 0`),
//! so the canonical `virt` device tree is dumped and embedded at build
//! time (`build.rs`) and its address handed to the boot pipeline, which
//! discovers the console / GIC / `/memory` / timer / PSCI from it exactly
//! as it would from real firmware (`plans/PI.md` P2–P5 watch-out).
//!
//! ## How it differs from a production kernel
//!
//! It reuses the entire production aarch64 boot pipeline; only the audit
//! Sink is replaced. Splitting the audit-observer behaviour into a
//! separate bin (instead of a Cargo feature on the arch crate) prevents
//! feature unification from leaking the QEMU-exit shortcut into any
//! production build (fail closed; the harness never
//! decides what the kernel does next).

#![cfg_attr(itest_aarch64, no_std)]
#![cfg_attr(itest_aarch64, no_main)]
#![deny(missing_docs)]

#[cfg(itest_aarch64)]
mod kernel;

/// The default `virt` board's tree: a GICv2.
#[cfg(itest_aarch64)]
mod tree {
    include!(concat!(env!("OUT_DIR"), "/dtb_fixture.rs"));
}

#[cfg(not(itest_aarch64))]
fn main() {}
