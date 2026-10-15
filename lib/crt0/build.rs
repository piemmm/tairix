//! Selects the `_start` trampoline `tairix-crt0` compiles in: one
//! `crt0_native_<arch>` cfg on a native Tier-1 target; anywhere else only the
//! host-testable marshalling core compiles.

fn main() {
    for arch in tairix_buildcfg::NATIVE_ARCHES {
        println!("cargo:rustc-check-cfg=cfg(crt0_native_{arch})");
    }

    if let Some(arch) = tairix_buildcfg::native_arch() {
        println!("cargo:rustc-cfg=crt0_native_{arch}");
    }
}
