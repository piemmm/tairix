//! Build script for the x86_64 real-time-clock live-boot QEMU vertical
//! (`plans/TIMESYNC.md` TS-3).
//!
//! One job on the freestanding `x86_64-tairix-none` target: hand the
//! production x86_64 kernel linker script to `rustc`. QEMU's PVH `-kernel`
//! loader enters the kernel directly and the CMOS clock has no device-tree
//! node, so unlike the aarch64 sibling there is no fixture blob to embed:
//! the clock node is synthesised on the port pair every PC-compatible
//! machine carries.

fn main() {
    tairix_itest_harness::emit_target_cfg();
    println!("cargo:rerun-if-changed=build.rs");

    tairix_itest_harness::link_x86_64_kernel_layout();
}
