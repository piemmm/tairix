//! Selects the syscall-trap instruction `tairix-abi-trap` compiles in: one
//! `abi_trap_<arch>` cfg, and the `abi_trap_native` umbrella, on a native
//! Tier-1 target; anywhere else the crate compiles its host trap seam.

fn main() {
    for arch in tairix_buildcfg::NATIVE_ARCHES {
        println!("cargo:rustc-check-cfg=cfg(abi_trap_{arch})");
    }
    println!("cargo:rustc-check-cfg=cfg(abi_trap_native)");

    if let Some(arch) = tairix_buildcfg::native_arch() {
        println!("cargo:rustc-cfg=abi_trap_{arch}");
        println!("cargo:rustc-cfg=abi_trap_native");
    }
}
