//! Build script: the aarch64 `virt` vertical build, its kernel reading no
//! device tree.

fn main() {
    tairix_itest_harness::aarch64_virt_guest_build_without_tree();
}
