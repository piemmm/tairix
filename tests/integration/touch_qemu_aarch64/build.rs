//! Build script for the touch QEMU vertical (`plans/POINTING.md` PO9): the
//! shared aarch64 `virt` guest build.

fn main() {
    // One CPU: PID 1, the unlock kthread, the autoloaded drivers and the
    // desktop session share it.
    tairix_itest_harness::aarch64_virt_guest_build(1);
}
