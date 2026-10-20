//! Build script for the `tairix-raster` crate.
//!
//! Sole responsibility, and it is build glue (where a target-conditional
//! decision is allowed to live; `cargo xtask cfg-check` holds the crate source
//! to none): decide which vector candidates for the compositor's per-pixel
//! kernels the crate compiles in, from the vector unit the target itself
//! carries (`tairix_buildcfg::vector_unit`). A candidate is still only
//! *selected* when the delivered `CpuFeatureSet` reports the extension and it
//! reproduces the portable reference bit for bit.

use tairix_buildcfg::VectorUnit;

fn main() {
    println!("cargo:rustc-check-cfg=cfg(raster_sse2)");
    println!("cargo:rustc-check-cfg=cfg(raster_neon)");
    println!("cargo:rustc-check-cfg=cfg(raster_lanes)");

    let candidate = match tairix_buildcfg::vector_unit() {
        Some(VectorUnit::Sse2) => "raster_sse2",
        Some(VectorUnit::Neon) => "raster_neon",
        None => return,
    };
    println!("cargo:rustc-cfg={candidate}");
    // What every vector candidate shares, which nothing else needs.
    println!("cargo:rustc-cfg=raster_lanes");
}
