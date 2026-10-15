//! Enables the `freestanding` cfg on a bare-metal target, and rebuilds the
//! program when the yield count `spawn_el0_timeshare_qemu_aarch64` compiles it
//! with changes.

fn main() {
    tairix_buildcfg::emit_freestanding();
    println!("cargo:rerun-if-env-changed=TAIRIX_EL0_YIELDS");
}
