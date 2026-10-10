//! Build script: the `freestanding` cfg on a bare-metal target, where
//! `src/main.rs` is the driver process; it keys off the OS alone, never the
//! ISA.

fn main() {
    println!("cargo:rustc-check-cfg=cfg(freestanding)");
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "none" {
        println!("cargo:rustc-cfg=freestanding");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
