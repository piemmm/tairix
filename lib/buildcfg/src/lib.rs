//! Build-script classification of the target a crate compiles for.
//!
//! A program crate builds two ways: as a freestanding binary on a bare-metal
//! target, and as an inert stub on the host, where clippy and the host test
//! suite compile the whole workspace. The [`FREESTANDING`] cfg names the
//! first. It is keyed off the target's operating system alone, so no source
//! gated on it ever names an instruction set.
//!
//! A crate whose code must differ per instruction set — a trap, a start
//! trampoline, a register probe — names the set in its own cfg, chosen from
//! [`native_arch_of`] here in build glue, never in source.
//!
//! The classification is `no_std`. Reading the target cargo is building and
//! printing the directives for it need `std`, so those functions are behind
//! the `std` feature a build script's dependency enables.

#![no_std]

#[cfg(feature = "std")]
extern crate std;

/// The cfg enabled when a crate compiles for a bare-metal target.
pub const FREESTANDING: &str = "freestanding";

/// The instruction sets of the native Tier-1 targets, as cargo spells them.
pub const NATIVE_ARCHES: [&str; 3] = ["x86_64", "aarch64", "riscv64"];

/// Cargo's environment key naming the target's operating system.
#[cfg(feature = "std")]
const TARGET_OS_KEY: &str = "CARGO_CFG_TARGET_OS";

/// Cargo's environment key naming the target's instruction set.
#[cfg(feature = "std")]
const TARGET_ARCH_KEY: &str = "CARGO_CFG_TARGET_ARCH";

/// Whether a target whose operating system is `os` is bare metal.
#[must_use]
pub fn is_freestanding(os: &str) -> bool {
    os == "none"
}

/// The native Tier-1 instruction set among [`NATIVE_ARCHES`] that `arch`
/// names on a target whose operating system is `os`, or `None` on a hosted
/// target or any other instruction set.
#[must_use]
pub fn native_arch_of(os: &str, arch: &str) -> Option<&'static str> {
    if !is_freestanding(os) {
        return None;
    }
    NATIVE_ARCHES.into_iter().find(|&native| native == arch)
}

/// Whether cargo is building for a bare-metal target. Call from a build
/// script.
#[cfg(feature = "std")]
#[must_use]
pub fn target_is_freestanding() -> bool {
    is_freestanding(&target(TARGET_OS_KEY))
}

/// The native Tier-1 instruction set cargo is building a bare-metal target
/// for, as [`native_arch_of`]. Call from a build script.
#[cfg(feature = "std")]
#[must_use]
pub fn native_arch() -> Option<&'static str> {
    native_arch_of(&target(TARGET_OS_KEY), &target(TARGET_ARCH_KEY))
}

/// Declare [`FREESTANDING`] to the compiler and enable it when cargo is
/// building for a bare-metal target. Call from a build script's `main`.
///
/// The classification reads only the target, so the script is rerun only
/// when it changes itself.
#[cfg(feature = "std")]
pub fn emit_freestanding() {
    std::println!("cargo:rustc-check-cfg=cfg({FREESTANDING})");
    if target_is_freestanding() {
        std::println!("cargo:rustc-cfg={FREESTANDING}");
    }
    std::println!("cargo:rerun-if-changed=build.rs");
}

/// The target fact cargo hands a build script under `key`, empty when unset.
#[cfg(feature = "std")]
fn target(key: &str) -> std::string::String {
    std::env::var(key).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_bare_metal_target_is_freestanding() {
        assert!(is_freestanding("none"));
        for hosted in ["linux", "macos", "windows", "unknown", ""] {
            assert!(!is_freestanding(hosted), "{hosted:?}");
        }
    }

    #[test]
    fn a_native_instruction_set_is_named_only_on_bare_metal() {
        for arch in NATIVE_ARCHES {
            assert_eq!(native_arch_of("none", arch), Some(arch));
            assert_eq!(native_arch_of("linux", arch), None, "{arch}");
        }
        assert_eq!(native_arch_of("unknown", "wasm32"), None);
        assert_eq!(native_arch_of("none", "wasm32"), None);
        assert_eq!(native_arch_of("none", "arm"), None);
    }
}
