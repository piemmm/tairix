//! Build script: enable `fp_probe` and the per-arch `fp_probe_<arch>` cfg when
//! the fixture is built for a freestanding Tier-1 target, so `src/main.rs`
//! compiles as a real U-mode program there and an inert host stub everywhere
//! else.
//!
//! The fixture names vector/FP registers directly, which has no
//! architecture-neutral spelling, so the instruction-set decision lives here in
//! build glue exactly as `lib/abi-trap/build.rs` and `lib/crt0/build.rs` confine
//! their per-target selection — keeping `cargo xtask cfg-check` clean and the
//! choice auditable in one place.

fn main() {
    println!("cargo:rustc-check-cfg=cfg(fp_probe)");
    for arch in tairix_buildcfg::NATIVE_ARCHES {
        println!("cargo:rustc-check-cfg=cfg(fp_probe_{arch})");
    }
    if let Some(arch) = tairix_buildcfg::native_arch() {
        println!("cargo:rustc-cfg=fp_probe");
        println!("cargo:rustc-cfg=fp_probe_{arch}");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
