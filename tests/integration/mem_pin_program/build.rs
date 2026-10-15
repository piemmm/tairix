//! Build script: enable the `freestanding` cfg when the mem-pin fixture
//! program is built for a bare-metal target (`target_os = "none"`), so
//! `src/main.rs` compiles as a freestanding pure-Rust program there and as an
//! inert host stub everywhere else (mirrors `tests/integration/wait_program`).
//!
//! The aarch64 migration register check is selected through the custom
//! `mem_pin_aarch64` cfg emitted here rather than architecture-conditional
//! source code. The role parameters (bound/within/over bytes) arrive at
//! runtime through the consuming vertical's registry argument vectors, so no
//! geometry environment variables exist here.

fn main() {
    tairix_buildcfg::emit_freestanding();
    println!("cargo:rustc-check-cfg=cfg(mem_pin_aarch64)");
    if tairix_buildcfg::native_arch() == Some("aarch64") {
        println!("cargo:rustc-cfg=mem_pin_aarch64");
    }
}
