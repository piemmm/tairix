//! Build script: enable the `freestanding` cfg on a bare-metal target
//! (`target_os = "none"`), so the `Run` binary (`src/main.rs`) builds as a
//! freestanding program there and as an inert host stub elsewhere. Keyed off
//! the OS, never the instruction set, so `cargo xtask cfg-check` stays clean.

fn main() {
    println!("cargo:rustc-check-cfg=cfg(freestanding)");
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "none" {
        println!("cargo:rustc-cfg=freestanding");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
