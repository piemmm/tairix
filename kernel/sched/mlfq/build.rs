//! Build script: register the `loom` cfg, which `cargo xtask loom` sets to
//! build the run queue's models over loom's atomics (`src/loom_compat.rs`). A
//! cfg rather than a feature, as it swaps the atomics of every crate in the
//! build, which no additive feature may do.

fn main() {
    println!("cargo:rustc-check-cfg=cfg(loom)");
}
