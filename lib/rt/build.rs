//! Selects the `_start` trampoline `tairix-rt` compiles in: one
//! `rt_native_<arch>` cfg, and the `rt_native` umbrella, on a native Tier-1
//! target; anywhere else only the host-testable syscall marshalling compiles.

fn main() {
    for arch in tairix_buildcfg::NATIVE_ARCHES {
        println!("cargo:rustc-check-cfg=cfg(rt_native_{arch})");
    }
    println!("cargo:rustc-check-cfg=cfg(rt_native)");

    if let Some(arch) = tairix_buildcfg::native_arch() {
        println!("cargo:rustc-cfg=rt_native_{arch}");
        println!("cargo:rustc-cfg=rt_native");
    }
}
