//! Enable the `freestanding` cfg for a bare-metal target, so the `Run`
//! binary is the service there and an inert host stub everywhere else. It
//! keys only off the target's OS, never its instruction set.

fn main() {
    println!("cargo:rustc-check-cfg=cfg(freestanding)");
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "none" {
        println!("cargo:rustc-cfg=freestanding");
    }
}
