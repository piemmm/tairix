//! Build script for the `tairix-collections` crate.
//!
//! Sole responsibility, and it is build glue (which is where a
//! target-conditional decision is allowed to live; `cargo xtask cfg-check`
//! enforces that the crate source carries none): decide which control-group
//! scan candidate the hash table compiles in, from the vector unit the target
//! itself carries (`tairix_buildcfg::vector_unit`). Every other target runs the
//! portable baseline, which is always correct, and a candidate is still only
//! *selected* when the delivered `CpuFeatureSet` reports the extension present
//! and it reproduces the portable reference bit for bit.

use tairix_buildcfg::VectorUnit;

fn main() {
    println!("cargo:rustc-check-cfg=cfg(swiss_sse2)");
    println!("cargo:rustc-check-cfg=cfg(swiss_neon)");

    match tairix_buildcfg::vector_unit() {
        Some(VectorUnit::Sse2) => println!("cargo:rustc-cfg=swiss_sse2"),
        Some(VectorUnit::Neon) => println!("cargo:rustc-cfg=swiss_neon"),
        None => {}
    }
}
