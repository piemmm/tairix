//! Enables the `freestanding` cfg on a bare-metal target, and rebuilds the
//! program when the grant, magic and register offset `mmio_map_qemu_aarch64`
//! compiles it with changes.

fn main() {
    tairix_buildcfg::emit_freestanding();
    println!("cargo:rerun-if-env-changed=TAIRIX_MMIO_GRANT_HANDLE");
    println!("cargo:rerun-if-env-changed=TAIRIX_MMIO_MAGIC");
    println!("cargo:rerun-if-env-changed=TAIRIX_MMIO_REG_OFFSET");
}
