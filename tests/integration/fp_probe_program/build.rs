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
    for name in [
        "fp_probe",
        "fp_probe_x86_64",
        "fp_probe_aarch64",
        "fp_probe_riscv64",
    ] {
        println!("cargo:rustc-check-cfg=cfg({name})");
    }
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    if os == "none" && matches!(arch.as_str(), "x86_64" | "aarch64" | "riscv64") {
        println!("cargo:rustc-cfg=fp_probe");
        println!("cargo:rustc-cfg=fp_probe_{arch}");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
