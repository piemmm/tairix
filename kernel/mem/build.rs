//! Build script: register the `loom` cfg, which `cargo xtask loom` sets to
//! build this crate's models over loom's atomics. A cfg rather than a feature,
//! as it swaps the atomics of every crate in the build, which no additive
//! feature may do. This crate's own atomics stay `core`'s: its models
//! interleave `lib/sync`'s locks.

fn main() {
    println!("cargo:rustc-check-cfg=cfg(loom)");
}
