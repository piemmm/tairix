//! Build script for the aarch64 MSI-isolation QEMU vertical: the `virt`
//! linker script and the tree of the GICv3 board it boots.

use tairix_itest_harness::{Board, InterruptControllers};

fn main() {
    tairix_itest_harness::aarch64_virt_guest_build_trees(&[(
        "gicv3_tree",
        Board::default().with_interrupts(InterruptControllers::Gicv3),
        1,
    )]);
}
