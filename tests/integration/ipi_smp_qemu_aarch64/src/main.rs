//! WIRING Stage W6 QEMU integration test: multi-core SMP bring-up and
//! IPI delivery on the aarch64 `virt` board.
//!
//! ## What this test asserts
//!
//! `plans/WIRING.md` Stage W6 requires that the aarch64 boot core can
//! (1) start every secondary core, (2) deliver its local generic-timer PPI,
//! and (3) deliver a directed inter-processor interrupt to each — the
//! EL1/GICv2 analogue of the riscv64 vertical. This binary exercises all
//! three, end to end, on a four-core `virt` board:
//!
//! 1. The boot core (core 0) installs the shared IPI callback
//!    (`preempt::set_ipi_callback`) and the secondary-core entry
//!    (`smp::set_secondary_entry`), and brings up the GIC, a GICv2 or a
//!    GICv3 as the binary's tree says (`gic::init`).
//! 2. It starts cores 1–3 via the `SecondaryBringup` PSCI `CPU_ON` path;
//!    each runs the `smp.s` trampoline → the installed entry.
//! 3. Every secondary installs the EL1 vector table (`exceptions::init_vectors`),
//!    brings up its own GIC interface (`gic::init_secondary`), enables the timer PPI
//!    and IPI SGI, arms one local timer quantum, unmasks IRQs, publishes its
//!    `READY` bit, then idles on `wfi`.
//! 4. The boot core requires one timer callback from every secondary, then
//!    sends one IPI to each through `Aarch64Arch::send_ipi`.
//! 5. Each SGI target takes the IRQ and runs `preempt::on_ipi_interrupt` → the
//!    IPI callback. The boot core verifies all three callback CPU ids, then
//!    writes the ARM semihosting PASS finisher.
//!
//! A regression that fails to start a secondary core or to deliver a timer
//! PPI or IPI never reaches the PASS write, so the run times out and the
//! harness reports `Outcome::Timeout` — the documented fail-loud
//! behaviour.
//!
//! ## PSCI conduit (PI Stage P5)
//!
//! `smp::start_secondary` takes the PSCI conduit (`hvc`/`smc`) as a
//! parameter. This vertical proves the conduit is **discovered**, not
//! assumed: before building the arch handle it reads `/psci` `method`
//! from the canonical `virt` device tree embedded at build time
//! (`fdt::psci_method`) and fails closed if no PSCI node is found, then
//! installs *that discovered* conduit on the handle. The secondary core
//! each secondary this test starts is therefore brought up over the conduit read from
//! the tree, mirroring how the production `boot_aarch64` path installs
//! it (`plans/PI.md` P5). The board tree is embedded, not read from
//! `x0`, for the same reason as the GIC bases below: QEMU's ELF
//! `-kernel` boot hands no DTB pointer.
//!
//! ## GIC discovery (PI Stage P3)
//!
//! Before `gic::init`, the boot core **poisons** the runtime GIC base and
//! then reads the GIC from the `virt` device tree embedded at build time
//! (`gic::configure_from_fdt`), asserting the distributor base moved off the
//! poison value to `virt`'s. The GICv3 binary's tree names a GICv3, whose
//! redistributors are found from that tree too. Every later GIC access on
//! every core goes through what was read, so the delivered IPI is the
//! runtime proof the discovery works. The board tree is embedded, not read
//! from `x0`, for the same reason as the PSCI conduit above: QEMU's ELF
//! `-kernel` boot hands no DTB pointer.
//!
//! ## How it differs from a production kernel
//!
//! It links only the `tairix-arch-aarch64` port (the SMP path needs no
//! `kernel/*` subsystem) and supplies its own `kernel_main`. The
//! QEMU-exit shortcut lives in this dedicated bin, never behind a Cargo
//! feature on the arch crate (fail closed).

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
