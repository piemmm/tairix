//! Enables the `freestanding` cfg on a bare-metal target, and rebuilds the
//! program when the role and yield count `spawn_session_qemu_riscv64` compiles
//! it with changes.

fn main() {
    tairix_buildcfg::emit_freestanding();
    println!("cargo:rerun-if-env-changed=TAIRIX_SPAWN_ROLE");
    println!("cargo:rerun-if-env-changed=TAIRIX_SPAWN_YIELDS");
}
