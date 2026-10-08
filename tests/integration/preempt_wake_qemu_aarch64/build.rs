//! Build-time fixture generator for the interrupt-return-to-EL0 need-resched
//! regression vertical.
//!
//! Identical in shape to the sibling `preempt_el0_qemu_aarch64` build script
//! (the shared dump/convert helpers live in `tairix_itest_harness`, so no
//! aarch64 build script re-rolls them):
//!
//! 1. Hand the aarch64 `virt` linker script to the test kernel and dump the
//!    canonical QEMU `virt` flattened device tree, embedding it so the test
//!    discovers the GICv2 base and generic-timer rate from the firmware tree.
//! 2. Compile the pure-Rust EL0 spinner program (`tests/integration/
//!    el0_spinner_program`) position-independent for the freestanding aarch64
//!    target, pinning its busy-loop count through `TAIRIX_EL0_SPINS`.
//! 3. Convert the linked PIE ELF to an `rxe` blob stamped with the kernel's
//!    compiled-in syscall CFI tag, emitted as a Rust source the test
//!    `include!`s.
//!
//! On any non-aarch64 target it emits inert stubs so the crate still builds.
//!
//! Re-running `build.rs` produces byte-identical output, so the test is
//! deterministic.

use std::env;
use std::path::PathBuf;

use tairix_itest_harness::pie::PieArch;

/// Busy-loop iterations the spinner runs before it exits. Smaller than the
/// sibling timer vertical's count: this test proves the *single* SGI-driven
/// preemption on EL0 entry, not a multi-tick runaway, so the spinner only has
/// to run long enough to be interrupted on its first instruction and then
/// complete promptly under QEMU TCG.
const SPINS: u64 = 20_000_000;

/// Freestanding target this vertical cross-compiles for.
const ARCH: PieArch = PieArch::Aarch64;

fn main() {
    tairix_itest_harness::aarch64_virt_guest_build(1);

    let manifest_dir = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let out_dir = env::var("OUT_DIR").expect("OUT_DIR");
    let manifest_dir = manifest_dir.trim_end_matches('/');

    let rxe_path = PathBuf::from(&out_dir).join("program_rxe.rs");

    let target = env::var("TARGET").unwrap_or_default();
    if target == ARCH.target_triple() {
        // One CPU: this is a single-core preemption slice.

        let rxe = tairix_itest_harness::program_fixture::GuestBuild {
            manifest_dir,
            out_dir: &out_dir,
            arch: ARCH,
            package: "tairix-test-el0-spinner",
            variant: None,
            env: &[("TAIRIX_EL0_SPINS", SPINS.to_string())],
        }
        .program_rxe(&tairix_kernel_syscall::SYSCALL_TABLE_HASH);
        write_program_fixture(&rxe_path, &rxe);
    } else {
        write_program_fixture(&rxe_path, &[]);
    }
}

/// Emit `PROGRAM_RXE` and `USER_BIAS` as a Rust source the test includes.
fn write_program_fixture(path: &std::path::Path, rxe: &[u8]) {
    let mut out = tairix_itest_harness::program_fixture::fixture_header();
    tairix_itest_harness::program_fixture::push_rxe_blob(
        &mut out,
        "PROGRAM_RXE",
        "the el0-spinner fixture program",
        rxe,
    );
    tairix_itest_harness::program_fixture::write_fixture(path, &out);
}
