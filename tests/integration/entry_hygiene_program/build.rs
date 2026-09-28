//! Build script: enable `entry_hygiene` and the per-arch `entry_hygiene_<arch>`
//! cfg when the fixture is built for a freestanding Tier-1 target, so
//! `src/main.rs` compiles as a real U-mode program there and an inert host stub
//! everywhere else. The per-arch register-capture asm has no
//! architecture-neutral spelling, so the choice lives in build glue, keeping
//! `cargo xtask cfg-check` clean.

fn main() {
    for name in [
        "entry_hygiene",
        "entry_hygiene_x86_64",
        "entry_hygiene_aarch64",
        "entry_hygiene_riscv64",
    ] {
        println!("cargo:rustc-check-cfg=cfg({name})");
    }
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    if os == "none" && matches!(arch.as_str(), "x86_64" | "aarch64" | "riscv64") {
        println!("cargo:rustc-cfg=entry_hygiene");
        println!("cargo:rustc-cfg=entry_hygiene_{arch}");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
