//! Build script for the `tairix-crypto` crate.
//!
//! Sole responsibility, and it is build glue (a build script is build glue, so
//! confining a target-conditional decision here keeps it out of the crate
//! source): decide, per compilation target, whether the audited SHA-256 crate
//! (`sha2`) can select a *hardware* backend at run time, so `backend::resolve`
//! offers a hardware candidate only where its availability record matches
//! what actually runs. The decision is `build_support`'s, which the crate's
//! host tests also cover.
//!
//! `sha2` gates SHA-NI on `cpufeatures`, which answers nothing on a target with
//! no operating system, so no TAIRiX target — kernel or user space — reaches
//! it, and none gets the candidate (`plans/OPEN-DEFECTS.md` D363). A hosted
//! x86_64 build, a developer's `cargo test`, does, so the hardware-candidate
//! wiring and the known-answer self-test over it stay exercised.

#[path = "src/build_support.rs"]
mod build_support;

fn main() {
    println!("cargo:rustc-check-cfg=cfg(crypto_hw_sha256)");

    let var = |key| std::env::var(key).unwrap_or_default();
    if build_support::sha2_selects_hardware_at_runtime(
        &var("CARGO_CFG_TARGET_ARCH"),
        &var("CARGO_CFG_TARGET_OS"),
        &var("CARGO_CFG_TARGET_ENV"),
    ) {
        println!("cargo:rustc-cfg=crypto_hw_sha256");
    }
}
