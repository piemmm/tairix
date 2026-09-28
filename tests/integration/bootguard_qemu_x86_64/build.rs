//! Build script: hand the x86_64 linker script to `rustc` *only* on the
//! freestanding `x86_64-tairix-none` target. On host builds we do nothing so the
//! crate still compiles for `cargo check` / IDE indexing.
//!
//! the charter forbids duplicating the linker script per test, so this
//! crate points at the same file every other x86_64 vertical uses — which
//! is also the script whose guard reservation this test exists to check.

fn main() {
    tairix_itest_harness::emit_target_cfg();
    println!("cargo:rerun-if-changed=build.rs");

    tairix_itest_harness::link_x86_64_kernel_layout();
}
