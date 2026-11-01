//! Build script for the `tairix-crypto` crate.
//!
//! Sole responsibility, and it is build glue (a build script is build glue, so
//! confining a target-conditional decision here keeps it out of the crate
//! source): decide, per compilation target, which *hardware* backend the
//! audited SHA-256 crate (`sha2`) can take, so `backend::resolve` offers a
//! hardware candidate only where its availability record matches what
//! actually runs. The decision is `build_support`'s, which the crate's host
//! tests also cover.
//!
//! No TAIRiX target — kernel or user space — reaches one, so none gets the
//! candidate (`plans/OPEN-DEFECTS.md` D363). A hosted x86_64 build with SHA-NI,
//! or an AArch64 Linux, Android or Apple build, runs `sha2`'s own detection, so
//! the candidate is offered there, and the hardware-candidate wiring and the
//! known-answer self-test over it stay exercised.

#[path = "src/build_support.rs"]
mod build_support;

use build_support::{sha2_hardware_backend, HwSha256, Sha2Build};

fn main() {
    for cfg in [
        "crypto_hw_sha256",
        "crypto_sha256_shani",
        "crypto_sha256_armv8",
    ] {
        println!("cargo:rustc-check-cfg=cfg({cfg})");
    }

    let var = |key| std::env::var(key).unwrap_or_default();
    let (arch, os, vendor, env, features) = (
        var("CARGO_CFG_TARGET_ARCH"),
        var("CARGO_CFG_TARGET_OS"),
        var("CARGO_CFG_TARGET_VENDOR"),
        var("CARGO_CFG_TARGET_ENV"),
        var("CARGO_CFG_TARGET_FEATURE"),
    );
    let (backend, backend_256) = (
        var("CARGO_CFG_SHA2_BACKEND"),
        var("CARGO_CFG_SHA2_256_BACKEND"),
    );
    let named = |value: &str| -> Option<String> { (!value.is_empty()).then(|| value.to_owned()) };
    let (backend, backend_256) = (named(&backend), named(&backend_256));
    let build = Sha2Build {
        arch: &arch,
        os: &os,
        vendor: &vendor,
        env: &env,
        features: &features,
        miri: std::env::var_os("CARGO_CFG_MIRI").is_some(),
        backend: backend.as_deref(),
        backend_256: backend_256.as_deref(),
    };
    let cfg = match sha2_hardware_backend(&build) {
        Ok(None) => return,
        Ok(Some(HwSha256::ShaNi)) => "crypto_sha256_shani",
        Ok(Some(HwSha256::Armv8)) => "crypto_sha256_armv8",
        Err(backend) => panic!(
            "sha2 is configured with the {backend} backend, which tairix-crypto keeps no \
             availability record of"
        ),
    };
    println!("cargo:rustc-cfg=crypto_hw_sha256");
    println!("cargo:rustc-cfg={cfg}");
}
