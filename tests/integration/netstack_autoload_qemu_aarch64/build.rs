//! Build script: the aarch64 `virt` vertical build, a one-CPU tree for the
//! first binary and a four-CPU one for the second.

use tairix_itest_harness::Board;

fn main() {
    tairix_itest_harness::aarch64_virt_guest_build_trees(&[
        ("dtb_fixture", Board::default(), 1),
        ("smp_tree", Board::default(), 4),
    ]);
}
