//! Build script for the aarch64 production-boot QEMU vertical: the `virt`
//! linker script, and for each binary the four-CPU tree of the board it boots
//! — a GICv2 `virt` on the baseline model, which lacks PAN, and a GICv3 one
//! on the default. Four CPUs match the run's `-smp 4`, so the boot starts the
//! secondaries the tree's `/cpus` declares.

use tairix_itest_harness::{Board, InterruptControllers, BASELINE_CPU};

fn main() {
    tairix_itest_harness::aarch64_virt_guest_build_trees(&[
        ("dtb_fixture", Board::default().with_cpu(BASELINE_CPU), 4),
        (
            "gicv3_tree",
            Board::default().with_interrupts(InterruptControllers::Gicv3),
            4,
        ),
    ]);
}
