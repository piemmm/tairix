//! Build script for the aarch64 FIQ masked-section self-sample vertical
//! (`plans/WATCHDOG.md`, `plans/OPEN-DEFECTS.md` D13): the `virt` linker
//! script, and for each binary the one-CPU tree of the board it boots — a
//! GICv2 `virt` and a GICv3 one. The self-sample is a same-CPU property.

use tairix_itest_harness::{Board, InterruptControllers};

fn main() {
    tairix_itest_harness::aarch64_virt_guest_build_trees(&[
        ("dtb_fixture", Board::default(), 1),
        (
            "gicv3_tree",
            Board::default().with_interrupts(InterruptControllers::Gicv3),
            1,
        ),
    ]);
}
