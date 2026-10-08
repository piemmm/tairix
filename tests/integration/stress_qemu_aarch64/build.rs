//! Build script: the aarch64 `virt` vertical build.

fn main() {
    // Four CPUs, as on the Raspberry Pi 4 the workload is sized for.
    tairix_itest_harness::aarch64_virt_guest_build(4);
}
