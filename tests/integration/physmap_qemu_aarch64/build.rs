//! Build script for the aarch64 direct-physical-map QEMU vertical
//! (`plans/OPEN-DEFECTS.md` D56).
//!
//! Two jobs on the freestanding `aarch64-unknown-none` target:
//!
//! 1. Hand the aarch64 `virt` linker script to `rustc` — the single
//!    per-board linker script the architecture port owns.
//! 2. Dump the `virt` flattened device tree and embed it, because QEMU's
//!    `-kernel <ELF>` aarch64 path passes no DTB pointer (`x0 = 0`).
//!
//! The dump asks for [`GUEST_RAM_MIB`] rather than the harness default,
//! because the boot path sizes the direct physical map from the tree's
//! `/memory` window: a tree describing the default would leave the extra
//! RAM invisible, which is the very thing under test. It must match the
//! `ram_mib` this vertical's QEMU enrolment declares.
//!
//! Re-running `build.rs` produces byte-identical output, so the test is
//! deterministic.

/// Guest RAM this vertical runs with, in mebibytes — enough that RAM spans
/// gigapages above the one holding the kernel image, so a process root that
/// carried a full-RAM identity map would be visible as one.
const GUEST_RAM_MIB: u32 = 3072;

fn main() {
    // One CPU: sizing and installing the map is a boot-CPU-only step.
    tairix_itest_harness::aarch64_virt_guest_build_with_ram(1, GUEST_RAM_MIB);
}
