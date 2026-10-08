//! Build-time fixture generator for the FIX-IO IO2 aarch64 block-transport
//! fault vertical.
//!
//! Three jobs on the freestanding `aarch64-unknown-none` target:
//!
//! 1. Hand the aarch64 `virt` linker script to the test kernel (the single
//!    per-board script the architecture port owns) and dump the canonical
//!    QEMU `virt` flattened device tree, embedding it so the test discovers
//!    the GICv2 base and the generic-timer rate from the firmware tree.
//!    QEMU's `-kernel <ELF>` aarch64 path passes no DTB pointer (`x0 = 0`),
//!    so the board tree is embedded at build time; the dump helper lives in
//!    the shared harness so no aarch64 build script re-rolls it.
//! 2. Compile the pure-Rust EL0 fixture program
//!    (`tests/integration/blkio_fault_program`) position-independent for the
//!    freestanding aarch64 target.
//! 3. Convert the linked PIE ELF to an `rxe` blob with
//!    [`tairix_itest_harness::elf2rxe::elf_to_rxe`], baking relocations for
//!    the [`tairix_itest_harness::USER_IMAGE_BIAS`] the production spawn producer maps every image at
//!    and stamping the kernel's compiled-in syscall CFI tag
//!    (`tairix_kernel_syscall::SYSCALL_TABLE_HASH`) so
//!    [`tairix_abi::rxe::LoadImage::parse`] accepts it; emit the blob and
//!    the bias as a Rust source the test `include!`s.
//!
//! On any non-aarch64 target (host `cargo build --workspace`, clippy) it
//! emits inert stubs so the crate still builds; the kernel body that
//! consumes them compiles only for the freestanding aarch64 target.
//!
//! Re-running `build.rs` produces byte-identical output, so the test is
//! deterministic.

use std::env;
use std::path::PathBuf;

use tairix_itest_harness::pie::PieArch;

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
        // One CPU: this is the single-core live-scheduler slice.

        let rxe = tairix_itest_harness::program_fixture::GuestBuild {
            manifest_dir,
            out_dir: &out_dir,
            arch: ARCH,
            package: "tairix-test-blkio-fault",
            variant: None,
            env: &[],
        }
        .program_rxe(&tairix_kernel_syscall::SYSCALL_TABLE_HASH);
        write_program_fixture(&rxe_path, &rxe);
    } else {
        // Inert stubs for host / other targets; the kernel body that uses
        // these compiles only for the freestanding aarch64 target.
        write_program_fixture(&rxe_path, &[]);
    }
}

/// Emit `PROGRAM_RXE` and `USER_BIAS` as a Rust source the test includes.
fn write_program_fixture(path: &std::path::Path, rxe: &[u8]) {
    let mut out = tairix_itest_harness::program_fixture::fixture_header();
    tairix_itest_harness::program_fixture::push_rxe_blob(
        &mut out,
        "PROGRAM_RXE",
        "the two-role fixture program",
        rxe,
    );
    tairix_itest_harness::program_fixture::write_fixture(path, &out);
}
