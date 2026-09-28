//! Build script: hand the kernel linker script to `rustc` *only* on the
//! freestanding `x86_64-tairix-none` target. On host builds we do
//! nothing so the crate still compiles for `cargo check`/IDE indexing.
//!
//! The linker script is the shared one under
//! `kernel/arch/x86_64/linker.ld`; a per-test copy would be the
//! duplication the charter forbids.

fn main() {
    tairix_itest_harness::emit_target_cfg();
    println!("cargo:rerun-if-changed=build.rs");

    tairix_itest_harness::link_x86_64_kernel_layout();
}
