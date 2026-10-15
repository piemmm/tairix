//! Selects the barrier instruction `tairix-dma-barrier` compiles in: one
//! `dma_barrier_<arch>` cfg, and the `dma_barrier_native` umbrella, on a
//! native Tier-1 target; anywhere else the no-op host fallback compiles.

fn main() {
    for arch in tairix_buildcfg::NATIVE_ARCHES {
        println!("cargo:rustc-check-cfg=cfg(dma_barrier_{arch})");
    }
    println!("cargo:rustc-check-cfg=cfg(dma_barrier_native)");

    if let Some(arch) = tairix_buildcfg::native_arch() {
        println!("cargo:rustc-cfg=dma_barrier_{arch}");
        println!("cargo:rustc-cfg=dma_barrier_native");
    }
}
