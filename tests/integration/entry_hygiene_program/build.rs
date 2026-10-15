//! Build script: enable `entry_hygiene` and the per-arch `entry_hygiene_<arch>`
//! cfg when the fixture is built for a freestanding Tier-1 target, so
//! `src/main.rs` compiles as a real U-mode program there and an inert host stub
//! everywhere else. The per-arch register-capture asm has no
//! architecture-neutral spelling, so the choice lives in build glue, keeping
//! `cargo xtask cfg-check` clean.

fn main() {
    println!("cargo:rustc-check-cfg=cfg(entry_hygiene)");
    for arch in tairix_buildcfg::NATIVE_ARCHES {
        println!("cargo:rustc-check-cfg=cfg(entry_hygiene_{arch})");
    }
    if let Some(arch) = tairix_buildcfg::native_arch() {
        println!("cargo:rustc-cfg=entry_hygiene");
        println!("cargo:rustc-cfg=entry_hygiene_{arch}");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
