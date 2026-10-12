//! Enable the `freestanding` cfg for a bare-metal build, so the `Run` binary
//! compiles as a freestanding program there and as an inert host stub
//! elsewhere. Keys only off the OS component of the target, never the
//! instruction set.

fn main() {
    println!("cargo:rustc-check-cfg=cfg(freestanding)");
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "none" {
        println!("cargo:rustc-cfg=freestanding");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
