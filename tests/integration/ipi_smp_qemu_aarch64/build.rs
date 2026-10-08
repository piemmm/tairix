//! Build script for the aarch64 SMP/IPI QEMU vertical: the `virt` linker
//! script, and for each binary the four-CPU tree of the board it boots — a
//! GICv2 `virt` and a GICv3 one.

use tairix_itest_harness::{Board, InterruptControllers};

fn main() {
    tairix_itest_harness::aarch64_virt_guest_build_trees(&[
        ("dtb_fixture", Board::default(), 4),
        (
            "gicv3_tree",
            Board::default().with_interrupts(InterruptControllers::Gicv3),
            4,
        ),
    ]);
}
