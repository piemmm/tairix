//! Build script: the aarch64 `virt` vertical build, and the pure-Rust
//! syscall-resume fixture program.

use std::env;
use std::path::{Path, PathBuf};

use tairix_itest_harness::pie::PieArch;

/// Freestanding target this vertical cross-compiles for.
const ARCH: PieArch = PieArch::Aarch64;

fn main() {
    tairix_itest_harness::aarch64_virt_guest_build(1);

    let manifest_dir = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let out_dir = env::var("OUT_DIR").expect("OUT_DIR");
    let manifest_dir = manifest_dir.trim_end_matches('/');

    let rxe_path = PathBuf::from(&out_dir).join("program_rxe.rs");
    if env::var("TARGET").unwrap_or_default() == ARCH.target_triple() {
        let rxe = tairix_itest_harness::program_fixture::GuestBuild {
            manifest_dir,
            out_dir: &out_dir,
            arch: ARCH,
            package: "tairix-test-syscall-resume-program",
            variant: None,
            env: &[],
        }
        .program_rxe(&tairix_kernel_syscall::SYSCALL_TABLE_HASH);
        write_program(&rxe_path, &rxe);
    } else {
        write_program(&rxe_path, &[]);
    }
}

fn write_program(path: &Path, bytes: &[u8]) {
    let mut out = tairix_itest_harness::program_fixture::fixture_header();
    tairix_itest_harness::program_fixture::push_rxe_blob(
        &mut out,
        "PROGRAM_RXE",
        "the syscall-resume fixture program",
        bytes,
    );
    tairix_itest_harness::program_fixture::write_fixture(path, &out);
}
