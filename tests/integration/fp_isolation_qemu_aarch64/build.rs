//! Build-time fixture generator for the aarch64 floating-point isolation
//! vertical (`plans/OPEN-DEFECTS.md` D359/D360), the cross-port sibling of
//! `fp_isolation_qemu_riscv64`.
//!
//! On the freestanding `aarch64-unknown-none` target it links the `virt`
//! script, embeds the canonical `virt` device tree (for the GICv2 base and the
//! timer rate), and compiles two pure-Rust fixtures — `fp-probe` (round count
//! pinned through `TAIRIX_FP_ROUNDS`, emitted as `ROUNDS_PER_TASK`) and
//! `entry-hygiene` — position-independent, converting each to an `rxe` blob. On
//! any other target it emits inert stubs.

use std::env;
use std::fmt::Write as _;
use std::path::PathBuf;

use tairix_itest_harness::pie::PieArch;

/// Rounds each probe task fills and checks the whole register file.
const ROUNDS_PER_TASK: u32 = 4;

/// Freestanding target this vertical cross-compiles for.
const ARCH: PieArch = PieArch::Aarch64;

fn main() {
    tairix_itest_harness::aarch64_virt_guest_build(1);

    let manifest_dir = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let out_dir = env::var("OUT_DIR").expect("OUT_DIR");
    let manifest_dir = manifest_dir.trim_end_matches('/');
    let rxe_path = PathBuf::from(&out_dir).join("program_rxe.rs");

    if env::var("TARGET").unwrap_or_default() == ARCH.target_triple() {
        let probe = tairix_itest_harness::program_fixture::GuestBuild {
            manifest_dir,
            out_dir: &out_dir,
            arch: ARCH,
            package: "tairix-test-fp-probe",
            variant: None,
            env: &[("TAIRIX_FP_ROUNDS", ROUNDS_PER_TASK.to_string())],
        }
        .program_rxe(&tairix_kernel_syscall::SYSCALL_TABLE_HASH);
        let hygiene = tairix_itest_harness::program_fixture::GuestBuild {
            manifest_dir,
            out_dir: &out_dir,
            arch: ARCH,
            package: "tairix-test-entry-hygiene",
            variant: None,
            env: &[],
        }
        .program_rxe(&tairix_kernel_syscall::SYSCALL_TABLE_HASH);
        write_program_fixture(&rxe_path, &probe, &hygiene);
    } else {
        write_program_fixture(&rxe_path, &[], &[]);
    }
}

/// Emit `PROGRAM_RXE`, `HYGIENE_RXE`, `USER_BIAS`, and `ROUNDS_PER_TASK`.
fn write_program_fixture(path: &std::path::Path, probe: &[u8], hygiene: &[u8]) {
    let mut out = tairix_itest_harness::program_fixture::fixture_header();
    let _ = writeln!(
        out,
        "/// Rounds each probe task fills and checks the file (pinned by build.rs)."
    );
    let _ = writeln!(out, "pub const ROUNDS_PER_TASK: u64 = {ROUNDS_PER_TASK};");
    tairix_itest_harness::program_fixture::push_rxe_blob(
        &mut out,
        "PROGRAM_RXE",
        "the fp-probe fixture program",
        probe,
    );
    tairix_itest_harness::program_fixture::push_rxe_blob(
        &mut out,
        "HYGIENE_RXE",
        "the entry-hygiene fixture program",
        hygiene,
    );
    tairix_itest_harness::program_fixture::write_fixture(path, &out);
}
